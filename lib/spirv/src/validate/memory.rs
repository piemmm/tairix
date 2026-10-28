//! Memory: function variables, loads, stores and copies, access chains,
//! array lengths, texel pointers, atomics and barriers; with each pointer's
//! root, so every access is charged to the memory object it reaches and
//! refused where the object or the path to it forbids it.

use super::body::{Access, Args, Body, Needs, Root, Typing};
use super::types::{Holds, Scalar, Ty};
use super::{Limit, Rule, Validator, What, MAX_INDEXES, NONE};
use crate::enumerant::{
    BuiltIn, Decoration, Dim, ImageFormat, MemoryAccess, MemorySemantics, Scope, StorageClass,
};
use crate::{Id, Instruction, Kind, Value, Version};

/// How a scope operand is used.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ScopeUse {
    Execution,
    Memory,
}

impl Validator<'_> {
    /// The root a global or local variable of `class` starts its pointers
    /// at: what the class and the variable's decorations forbid.
    pub(crate) fn variable_root(&self, id: Id, class: StorageClass) -> Root {
        let slot = self
            .slot(id)
            .map_or(NONE, |s| u32::try_from(s).unwrap_or(NONE));
        let mut forbid = match class {
            StorageClass::UNIFORM_CONSTANT
            | StorageClass::INPUT
            | StorageClass::UNIFORM
            | StorageClass::PUSH_CONSTANT => Access::WRITE,
            _ => Access::NONE,
        };
        if self.decorated(id, Decoration::NON_WRITABLE) {
            forbid = forbid.with(Access::WRITE);
        }
        if self.decorated(id, Decoration::NON_READABLE) {
            forbid = forbid.with(Access::READ);
        }
        Root { slot, forbid }
    }

    /// What the image variable in `slot` forbids of its texels: its
    /// `NonWritable` and `NonReadable` decorations, the class aside — an
    /// image's variable is never stored to, but its texels may be.
    pub(crate) fn texel_root(&self, slot: u32) -> Root {
        let mut forbid = Access::NONE;
        if let Some(&(id, _)) = self.m.definitions.get(slot as usize) {
            if self.decorated(id, Decoration::NON_WRITABLE) {
                forbid = forbid.with(Access::WRITE);
            }
            if self.decorated(id, Decoration::NON_READABLE) {
                forbid = forbid.with(Access::READ);
            }
        }
        Root { slot, forbid }
    }

    /// A `Function` variable at the head of its function's first block.
    pub(crate) fn local_variable(&mut self, instruction: &Instruction) -> Result<(), Rule> {
        let (id, _, class, pointee) = self.variable_parts(instruction)?;
        if class != StorageClass::FUNCTION {
            return Err(Rule::StorageClass(class));
        }
        self.variable_contents(class, pointee)?;
        if let Some(initializer) = self.initializer(instruction) {
            let def = self.def(initializer)?;
            if !matches!(def.what, What::Constant | What::SpecConstant)
                || self.value_type(initializer)? != pointee
            {
                return Err(Rule::Initializer);
            }
        }
        let root = self.variable_root(id, class);
        self.set_root(id, root);
        Ok(())
    }

    /// The pointer operand `n` of `args`: its class and pointee, and its
    /// root. A texel pointer is only an atomic's.
    fn pointer(
        &self,
        args: &Args<'_>,
        n: usize,
        atomic: bool,
    ) -> Result<(StorageClass, u32, Root), Rule> {
        let id = self.arg_id(args, n)?;
        let ty = self.value_type(id).map_err(|_| args.wrong(n))?;
        let Ty::Pointer { class, pointee } = self.ty(ty).ty else {
            return Err(args.wrong(n));
        };
        if class == StorageClass::IMAGE && !atomic {
            return Err(Rule::LogicalPointer);
        }
        let root = self.root(id).ok_or(Rule::LogicalPointer)?;
        Ok((class, pointee, root))
    }

    /// Check one memory, atomic or barrier instruction.
    pub(crate) fn memory(
        &mut self,
        body: &mut Body<'_>,
        typing: Typing,
        args: &Args<'_>,
    ) -> Result<(), Rule> {
        let r = args.result_type;
        match typing {
            Typing::Load => {
                let (_, pointee, root) = self.pointer(args, 0, false)?;
                if pointee != r || self.ty(r).holds.any(Holds::RUNTIME_ARRAY) {
                    return Err(args.wrong(0));
                }
                self.whole_access(pointee)?;
                self.memory_operands(args, 1, 1)?;
                // Loading an image or sampler reads its descriptor, not its
                // texels: those accesses are the image instructions' to charge.
                if self.ty(r).holds.any(Holds::OPAQUE) {
                    if let Some(result) = args.result {
                        self.set_root(result, root);
                    }
                    return Ok(());
                }
                self.access(body, root, Access::READ)
            }
            Typing::Store => {
                let (_, pointee, root) = self.pointer(args, 0, false)?;
                if self.arg_type(args, 1)? != pointee {
                    return Err(args.wrong(1));
                }
                self.whole_access(pointee)?;
                self.rounding_store(args, pointee)?;
                self.memory_operands(args, 2, 1)?;
                self.access(body, root, Access::WRITE)
            }
            Typing::CopyMemory => {
                let (_, target, to) = self.pointer(args, 0, false)?;
                let (_, source, from) = self.pointer(args, 1, false)?;
                if target != source || self.ty(target).holds.any(Holds::RUNTIME_ARRAY) {
                    return Err(args.wrong(1));
                }
                self.whole_access(target)?;
                let masks = if self.enabled.version >= (Version { major: 1, minor: 4 }) {
                    2
                } else {
                    1
                };
                self.memory_operands(args, 2, masks)?;
                self.access(body, to, Access::WRITE)?;
                self.access(body, from, Access::READ)
            }
            Typing::AccessChain => self.access_chain(args),
            Typing::ArrayLength => {
                match self.ty(r).ty {
                    Ty::Scalar(Scalar::Int {
                        width: 32 | 64,
                        signed: false,
                    }) => {}
                    _ => return Err(Rule::ResultType),
                }
                let (class, pointee, _) = self.pointer(args, 0, false)?;
                let Ty::Struct { first, count } = self.ty(pointee).ty else {
                    return Err(args.wrong(0));
                };
                let member = self.arg_word(args, 1)?;
                let last = self.list(first, count).last().copied();
                let runtime =
                    last.is_some_and(|m| matches!(self.ty(m).ty, Ty::RuntimeArray { .. }));
                if class != StorageClass::STORAGE_BUFFER
                    || !runtime
                    || member.checked_add(1) != Some(count)
                {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            Typing::ImageTexelPointer => self.texel_pointer(args),
            Typing::AtomicLoad
            | Typing::AtomicStore
            | Typing::AtomicExchange
            | Typing::AtomicCompareExchange
            | Typing::AtomicStep
            | Typing::AtomicArith => self.atomic(body, typing, args),
            Typing::ControlBarrier => {
                let execution = self.scope(args, 0, ScopeUse::Execution, body)?;
                self.scope(args, 1, ScopeUse::Memory, body)?;
                let semantics = self.semantics(args, 2)?;
                if execution == Scope::WORKGROUP {
                    body.facts.needs = body.facts.needs.with(Needs::COMPUTE);
                }
                order(semantics).map_err(|()| args.wrong(2))
            }
            Typing::MemoryBarrier => {
                self.scope(args, 0, ScopeUse::Memory, body)?;
                let semantics = self.semantics(args, 1)?;
                order(semantics).map_err(|()| args.wrong(1))?;
                if strength(semantics) == 0 {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            _ => Err(Rule::NotAdmitted(args.op)),
        }
    }

    /// Refuse reading or writing all of a `ty` that holds a latent built-in.
    fn whole_access(&self, ty: u32) -> Result<(), Rule> {
        if !self.ty(ty).holds.contains(Holds::LATENT) {
            return Ok(());
        }
        let block = self.ty(self.innermost(ty)).id;
        let latent = block.and_then(|id| {
            self.decorations_of(id)
                .iter()
                .filter(|d| d.decoration == Decoration::BUILT_IN && d.member != NONE)
                .map(|d| self.parameter(d, 0))
                .find(|&b| self.enabled.latent(BuiltIn(b)))
        });
        Err(Rule::EnumerantNotEnabled(
            Kind::BuiltIn,
            latent.unwrap_or(BuiltIn::CLIP_DISTANCE.0),
        ))
    }

    /// The memory-access operands from operand `from`: up to `masks` masks,
    /// each with the parameters its bits take — an alignment that is a power
    /// of two.
    fn memory_operands(&self, args: &Args<'_>, from: usize, masks: usize) -> Result<(), Rule> {
        let mut n = from;
        for _ in 0..masks {
            let Some(operand) = args.operands.get(n) else {
                return Ok(());
            };
            let Value::Word(mask) = self.m.value(operand) else {
                return Err(args.wrong(n));
            };
            n += 1;
            if mask & MemoryAccess::ALIGNED.0 != 0 {
                let alignment = self.arg_word(args, n)?;
                if !alignment.is_power_of_two() {
                    return Err(args.wrong(n));
                }
                n += 1;
            }
        }
        if n == args.len() {
            Ok(())
        } else {
            Err(args.wrong(n))
        }
    }

    /// `OpAccessChain`: indexes walking the base's pointee, a structure's by
    /// an in-range constant and anything else's by an integer, to the result
    /// pointer's pointee in the base's class; a constant index past a fixed
    /// length refused; an array of descriptors indexed by a constant, however
    /// the base was reached.
    fn access_chain(&mut self, args: &Args<'_>) -> Result<(), Rule> {
        let (class, mut ty, root) = self.pointer(args, 0, false)?;
        let Ty::Pointer {
            class: result_class,
            pointee: result,
        } = self.ty(args.result_type).ty
        else {
            return Err(Rule::ResultType);
        };
        if result_class != class {
            return Err(Rule::ResultType);
        }
        let indexes = args.len() - 1;
        if indexes > MAX_INDEXES {
            return Err(Rule::Limit(Limit::Indexes));
        }
        let buffers = matches!(class, StorageClass::UNIFORM | StorageClass::STORAGE_BUFFER);
        let mut forbid = root.forbid;
        for n in 1..args.len() {
            let index = self.arg_id(args, n)?;
            let index_type = self.value_type(index).map_err(|_| args.wrong(n))?;
            if !matches!(self.ty(index_type).ty, Ty::Scalar(s) if s.is_int()) {
                return Err(args.wrong(n));
            }
            let constant = self.integer_constant(index);
            if let Some((
                value,
                Scalar::Int {
                    signed: true,
                    width,
                },
            )) = constant
            {
                if value >> (width - 1) & 1 == 1 {
                    return Err(Rule::AccessChain);
                }
            }
            let value = constant.map(|(v, _)| v);
            let info = *self.ty(ty);
            // Every array an image or sampler variable holds is of descriptors,
            // and so is an array of blocks: no block nests in another.
            let descriptors = matches!(info.ty, Ty::Array { .. } | Ty::RuntimeArray { .. })
                && (class == StorageClass::UNIFORM_CONSTANT
                    || buffers && info.holds.contains(Holds::BLOCK));
            if descriptors && value.is_none() {
                return Err(Rule::AccessChain);
            }
            ty = match info.ty {
                Ty::Struct { first, count } => {
                    let member = value
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|&m| m < count)
                        .ok_or(Rule::AccessChain)?;
                    forbid = forbid.with(self.member_forbids(info, member)?);
                    self.list(first, count)
                        .get(member as usize)
                        .copied()
                        .ok_or(Rule::AccessChain)?
                }
                Ty::RuntimeArray { element } => element,
                Ty::Array { element, length } => {
                    bounded(value, length)?;
                    element
                }
                Ty::Vector { scalar, count } => {
                    bounded(value, count)?;
                    self.scalar_index(scalar).ok_or(Rule::AccessChain)?
                }
                Ty::Matrix {
                    column, columns, ..
                } => {
                    bounded(value, columns)?;
                    column
                }
                _ => return Err(Rule::AccessChain),
            };
        }
        if ty != result {
            return Err(Rule::ResultType);
        }
        if let Some(id) = args.result {
            self.set_root(
                id,
                Root {
                    slot: root.slot,
                    forbid,
                },
            );
        }
        Ok(())
    }

    /// What member `member` of the structure `info` forbids of an access
    /// through it, refusing a latent built-in outright.
    fn member_forbids(&self, info: super::types::TypeInfo, member: u32) -> Result<Access, Rule> {
        let Some(id) = info.id else {
            return Ok(Access::NONE);
        };
        if info.holds.contains(Holds::LATENT) {
            if let Some(built_in) = self.decoration_value_of(id, member, Decoration::BUILT_IN) {
                if self.enabled.latent(BuiltIn(built_in)) {
                    return Err(Rule::EnumerantNotEnabled(Kind::BuiltIn, built_in));
                }
            }
        }
        let mut forbid = Access::NONE;
        if self
            .decoration_of(id, member, Decoration::NON_WRITABLE)
            .is_some()
        {
            forbid = forbid.with(Access::WRITE);
        }
        if self
            .decoration_of(id, member, Decoration::NON_READABLE)
            .is_some()
        {
            forbid = forbid.with(Access::READ);
        }
        Ok(forbid)
    }

    /// `OpImageTexelPointer`: a pointer, in the `Image` class, to one texel
    /// of a 32-bit integer storage image an atomic will operate on.
    fn texel_pointer(&mut self, args: &Args<'_>) -> Result<(), Rule> {
        let Ty::Pointer {
            class: StorageClass::IMAGE,
            pointee,
        } = self.ty(args.result_type).ty
        else {
            return Err(Rule::ResultType);
        };
        let (class, image, root) = self.pointer(args, 0, false)?;
        let Ty::Image(image) = self.ty(image).ty else {
            return Err(args.wrong(0));
        };
        let format_ok = matches!(image.format, ImageFormat::R32I | ImageFormat::R32UI);
        if class != StorageClass::UNIFORM_CONSTANT
            || image.sampled != 2
            || image.dim == Dim::SUBPASS_DATA
            || !format_ok
            || pointee != image.sampled_type
        {
            return Err(args.wrong(0));
        }
        let count = image.coordinates() + u32::from(image.arrayed);
        match self.components(self.arg_type(args, 1)?) {
            Some((s, c)) if s.is_int() && c == count => {}
            _ => return Err(args.wrong(1)),
        }
        let sample = self.arg_id(args, 2)?;
        self.int_scalar(args, 2)?;
        if !image.multisampled && self.integer_constant(sample).map(|(v, _)| v) != Some(0) {
            return Err(args.wrong(2));
        }
        if let Some(id) = args.result {
            self.set_root(id, self.texel_root(root.slot));
        }
        Ok(())
    }

    /// An atomic: a pointer to a 32-bit integer in a storage buffer,
    /// workgroup memory or a storage image's texel, of the result's or
    /// value's type; a memory scope and semantics fit for it.
    fn atomic(&mut self, body: &mut Body<'_>, typing: Typing, args: &Args<'_>) -> Result<(), Rule> {
        let (class, pointee, root) = self.pointer(args, 0, true)?;
        if !matches!(
            class,
            StorageClass::STORAGE_BUFFER | StorageClass::WORKGROUP | StorageClass::IMAGE
        ) {
            return Err(args.wrong(0));
        }
        if !matches!(
            self.ty(pointee).ty,
            Ty::Scalar(Scalar::Int { width: 32, .. })
        ) {
            return Err(args.wrong(0));
        }
        let scope = self.scope(args, 1, ScopeUse::Memory, body)?;
        let semantics = self.semantics(args, 2)?;
        order(semantics).map_err(|()| args.wrong(2))?;
        if scope == Scope::INVOCATION && strength(semantics) != 0 {
            return Err(args.wrong(2));
        }
        let order_bits = semantics.0 & ORDERS;
        let (access, value) = match typing {
            Typing::AtomicLoad => {
                if order_bits & (MemorySemantics::RELEASE.0 | MemorySemantics::ACQUIRE_RELEASE.0)
                    != 0
                {
                    return Err(args.wrong(2));
                }
                (Access::READ, None)
            }
            Typing::AtomicStore => {
                if order_bits & (MemorySemantics::ACQUIRE.0 | MemorySemantics::ACQUIRE_RELEASE.0)
                    != 0
                {
                    return Err(args.wrong(2));
                }
                (Access::WRITE, Some(3))
            }
            Typing::AtomicCompareExchange => {
                let unequal = self.semantics(args, 3)?;
                order(unequal).map_err(|()| args.wrong(3))?;
                let unequal_order = unequal.0 & ORDERS;
                let weaker = unequal_order == 0
                    || unequal_order == order_bits
                    || order_bits == MemorySemantics::ACQUIRE_RELEASE.0;
                let storage_within = (unequal.0 & STORAGE) & !(semantics.0 & STORAGE) == 0;
                if unequal_order & (MemorySemantics::RELEASE.0 | MemorySemantics::ACQUIRE_RELEASE.0)
                    != 0
                    || !weaker
                    || !storage_within
                {
                    return Err(args.wrong(3));
                }
                if self.arg_type(args, 5)? != pointee {
                    return Err(args.wrong(5));
                }
                (Access::READ.with(Access::WRITE), Some(4))
            }
            Typing::AtomicStep => (Access::READ.with(Access::WRITE), None),
            _ => (Access::READ.with(Access::WRITE), Some(3)),
        };
        if typing == Typing::AtomicStore {
            if args.result_type != NONE {
                return Err(Rule::ResultType);
            }
        } else if args.result_type != pointee {
            return Err(Rule::ResultType);
        }
        if let Some(n) = value {
            if self.arg_type(args, n)? != pointee {
                return Err(args.wrong(n));
            }
        }
        self.access(body, root, access)
    }

    /// A scope operand: a 32-bit integer `OpConstant` naming a scope the
    /// Vulkan environment admits for the use; a workgroup scope needs a
    /// compute shader.
    fn scope(
        &self,
        args: &Args<'_>,
        n: usize,
        used: ScopeUse,
        body: &mut Body<'_>,
    ) -> Result<Scope, Rule> {
        let id = self.arg_id(args, n)?;
        let scope = self.constant_word(id).map(Scope).ok_or(args.wrong(n))?;
        let admitted = match used {
            ScopeUse::Execution => matches!(scope, Scope::WORKGROUP | Scope::SUBGROUP),
            ScopeUse::Memory => matches!(
                scope,
                Scope::DEVICE | Scope::WORKGROUP | Scope::SUBGROUP | Scope::INVOCATION
            ),
        };
        if !admitted {
            return Err(Rule::Semantics);
        }
        if scope == Scope::WORKGROUP {
            body.facts.needs = body.facts.needs.with(Needs::COMPUTE);
        }
        Ok(scope)
    }

    /// A memory-semantics operand: a 32-bit integer `OpConstant` setting only
    /// bits the Vulkan environment admits.
    fn semantics(&self, args: &Args<'_>, n: usize) -> Result<MemorySemantics, Rule> {
        let id = self.arg_id(args, n)?;
        let value = self.constant_word(id).ok_or(args.wrong(n))?;
        if value & !(ORDERS | STORAGE) != 0 {
            return Err(Rule::Semantics);
        }
        Ok(MemorySemantics(value))
    }

    /// The value of a 32-bit integer `OpConstant`.
    pub(crate) fn constant_word(&self, id: Id) -> Option<u32> {
        match self.integer_constant(id)? {
            (value, Scalar::Int { width: 32, .. }) => u32::try_from(value).ok(),
            _ => None,
        }
    }

    /// A value decorated `FPRoundingMode` is a width-only conversion to a
    /// 16-bit float stored into a buffer or the output interface, and only
    /// stored.
    fn rounding_store(&self, args: &Args<'_>, pointee: u32) -> Result<(), Rule> {
        let object = self.arg_id(args, 1)?;
        if !self.decorated(object, Decoration::FP_ROUNDING_MODE) {
            return Ok(());
        }
        let (class, _, _) = self.pointer(args, 0, false)?;
        let narrow = matches!(
            self.components(pointee),
            Some((Scalar::Float { width: 16 }, _))
        );
        let stored = matches!(
            class,
            StorageClass::STORAGE_BUFFER | StorageClass::UNIFORM | StorageClass::OUTPUT
        );
        if narrow && stored {
            Ok(())
        } else {
            Err(Rule::DecorationTarget(Decoration::FP_ROUNDING_MODE))
        }
    }
}

/// The memory-order bits.
const ORDERS: u32 = MemorySemantics::ACQUIRE.0
    | MemorySemantics::RELEASE.0
    | MemorySemantics::ACQUIRE_RELEASE.0
    | MemorySemantics::SEQUENTIALLY_CONSISTENT.0;

/// The storage-class bits the Vulkan environment gives meaning.
const STORAGE: u32 = MemorySemantics::UNIFORM_MEMORY.0
    | MemorySemantics::WORKGROUP_MEMORY.0
    | MemorySemantics::IMAGE_MEMORY.0;

/// Refuse semantics with two orders, sequential consistency, an order with
/// no storage class, or a storage class with no order.
fn order(semantics: MemorySemantics) -> Result<(), ()> {
    let orders = semantics.0 & ORDERS;
    let storage = semantics.0 & STORAGE;
    if orders.count_ones() > 1
        || orders == MemorySemantics::SEQUENTIALLY_CONSISTENT.0
        || (orders != 0) != (storage != 0)
    {
        return Err(());
    }
    Ok(())
}

/// How strong an order is: relaxed, then acquire or release, then both.
const fn strength(semantics: MemorySemantics) -> u32 {
    let orders = semantics.0 & ORDERS;
    if orders == 0 {
        0
    } else if orders == MemorySemantics::ACQUIRE_RELEASE.0 {
        2
    } else {
        1
    }
}

/// Refuse a constant index at or past `length`.
fn bounded(index: Option<u64>, length: u32) -> Result<(), Rule> {
    match index {
        Some(index) if index >= u64::from(length) => Err(Rule::AccessChain),
        _ => Ok(()),
    }
}
