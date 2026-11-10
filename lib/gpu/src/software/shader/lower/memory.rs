//! Memory as lowering sees it: each global variable's root, every access
//! chain walked into steps or folded into a constant pointer, and each load
//! and store's plan — a run of words for memory laid out as registers hold
//! values, a scalar list for a buffer's explicit layout.

use alloc::vec::Vec;

use tairix_spirv::enumerant::{BuiltIn, Decoration, StorageClass};
use tairix_spirv::{BindingKind, Id, Member, Type, TypeId};

use super::layout::Instance;
use super::{num, push, Count, Fault, Layout, Lowering, Val, MAX_PROGRAM_INSTRUCTIONS};
use crate::software::shader::ir::{
    Builtin, Carries, Fixed, Given, Inst, Plan, Port, Reg, Root, Scalar, Step,
};

/// The most scalars the plans of one program list.
const MAX_SCALARS: usize = 1 << 22;

/// The most levels of types a program's plans are made through: four for
/// each scalar they may list.
const MAX_PLAN_VISITS: usize = 4 * MAX_SCALARS;

/// A pointer to `words` words of a lane's own memory from word `first`.
pub(crate) const fn private_pointer(first: u32, words: u32, pointee: TypeId, reg: Reg) -> Val {
    Val::Pointer {
        root: Root::Private { first, words },
        pointee,
        layout: Layout::Words,
        reg,
        descriptors: false,
    }
}

/// How an access chain's index moves its pointer: to a struct member's
/// offset, or by a stride per element.
#[derive(Copy, Clone)]
enum Stride {
    Member(u32),
    Each(u32),
}

impl Lowering<'_> {
    /// The value a global variable the entry point reaches, of `class` and
    /// holding `pointee`, has.
    pub(crate) fn global_variable(
        &mut self,
        id: Id,
        class: StorageClass,
        pointee: TypeId,
    ) -> Result<Val, Fault> {
        let origin = self.zeros(2)?;
        let rooted = |root: Root, layout: Layout, descriptors: bool| Val::Pointer {
            root,
            pointee,
            layout,
            reg: origin,
            descriptors,
        };
        let explicit = Layout::Explicit {
            matrix: None,
            column: None,
        };
        let val = match class {
            StorageClass::INPUT | StorageClass::OUTPUT => {
                let root = self.interface_variable(id, pointee, class == StorageClass::INPUT)?;
                rooted(root, Layout::Words, false)
            }
            StorageClass::UNIFORM
            | StorageClass::STORAGE_BUFFER
            | StorageClass::UNIFORM_CONSTANT => {
                let set = self
                    .decoration(id, Decoration::DESCRIPTOR_SET)
                    .ok_or(Fault::Malformed)?;
                let binding = self
                    .decoration(id, Decoration::BINDING)
                    .ok_or(Fault::Malformed)?;
                // In the entry point's order, by set and binding.
                let index = self
                    .program
                    .bindings
                    .binary_search_by_key(&(set, binding), |b| (b.set, b.number))
                    .map_err(|_| Fault::Malformed)?;
                let index = u32::try_from(index).map_err(|_| Fault::Budget)?;
                let descriptors = matches!(
                    self.ty(pointee)?,
                    Type::Array { .. } | Type::RuntimeArray { .. }
                ) && self.holds_descriptors(pointee, class)?;
                let layout = if class == StorageClass::UNIFORM_CONSTANT {
                    Layout::Words
                } else {
                    explicit
                };
                rooted(Root::Binding(index), layout, descriptors)
            }
            StorageClass::PUSH_CONSTANT => rooted(Root::PushConstants, explicit, false),
            StorageClass::PRIVATE => {
                let words = self.words(pointee)?;
                let first = self.private(words)?;
                rooted(Root::Private { first, words }, Layout::Words, false)
            }
            StorageClass::WORKGROUP => {
                let words = self.words(pointee)?;
                let first = self.program.workgroup_words;
                self.program.workgroup_words = first.checked_add(words).ok_or(Fault::Budget)?;
                rooted(Root::Workgroup { first, words }, Layout::Words, false)
            }
            _ => return Err(Fault::Malformed),
        };
        Ok(val)
    }

    /// `words` more words of each lane's private memory: the first of them.
    fn private(&mut self, words: u32) -> Result<u32, Fault> {
        let first = self.program.private_words;
        self.program.private_words = first.checked_add(words).ok_or(Fault::Budget)?;
        Ok(first)
    }

    /// The root of an input or output variable. A built-in the tile gives
    /// one value apiece is the tile's; any other is each lane's own words,
    /// every built-in in it given by the tile or a port of the stage's fixed
    /// function, and every location it holds a port.
    fn interface_variable(&mut self, id: Id, pointee: TypeId, input: bool) -> Result<Root, Fault> {
        let words = self.words(pointee)?;
        let built_in = self
            .admitted
            .decoration(id, None, Decoration::BUILT_IN)
            .map(BuiltIn);
        if let Some(which) = built_in.filter(|_| input).and_then(builtin) {
            if self.shared_builtin(which) {
                let first = self.program.shared_words;
                self.program.shared_words = first.checked_add(words).ok_or(Fault::Budget)?;
                let root = Root::Shared { first, words };
                push(
                    &mut self.program.given,
                    Given { which, root },
                    MAX_PROGRAM_INSTRUCTIONS,
                )?;
                return Ok(root);
            }
        }
        let first = self.private(words)?;
        if let Some(built_in) = built_in {
            self.built_in_at(built_in, (first, words), input)?;
        } else if let Some(members) = self.built_in_block(pointee)? {
            for member in 0..members {
                let Member { ty, built_in, .. } = self.member(pointee, member)?;
                let at = first
                    .checked_add(self.words_before(pointee, members, member)?)
                    .ok_or(Fault::Budget)?;
                match built_in {
                    Some(BuiltIn::CLIP_DISTANCE | BuiltIn::CULL_DISTANCE) => {}
                    Some(built_in) => {
                        let words = self.words(ty)?;
                        self.built_in_at(built_in, (at, words), input)?;
                    }
                    None => return Err(Fault::Malformed),
                }
            }
        } else {
            self.located(id, (first, words), input)?;
        }
        Ok(Root::Private { first, words })
    }

    /// The members of `ty` where it is a block of built-ins.
    fn built_in_block(&mut self, ty: TypeId) -> Result<Option<u32>, Fault> {
        let Type::Struct { members } = self.ty(ty)? else {
            return Ok(None);
        };
        if members == 0 {
            return Ok(None);
        }
        Ok(self.member(ty, 0)?.built_in.map(|_| members))
    }

    /// Built-in `built_in` in `words` words from private word `first`: given
    /// by the tile where it is an input the tile knows, a port otherwise.
    fn built_in_at(
        &mut self,
        built_in: BuiltIn,
        (first, words): (u32, u32),
        input: bool,
    ) -> Result<(), Fault> {
        if let Some(which) = builtin(built_in).filter(|_| input) {
            let root = Root::Private { first, words };
            return push(
                &mut self.program.given,
                Given { which, root },
                MAX_PROGRAM_INSTRUCTIONS,
            );
        }
        let fixed = fixed(built_in).ok_or(Fault::Malformed)?;
        self.port(Carries::Fixed(fixed), (first, words), input)
    }

    /// A port for each location the entry point's interface lists in the
    /// variable `id`, whose `words` words start at private word `first`.
    fn located(&mut self, id: Id, (first, words): (u32, u32), input: bool) -> Result<(), Fault> {
        let entry = self.entry;
        let varyings = if input {
            entry.inputs()
        } else {
            entry.outputs()
        };
        let mut found = false;
        for varying in varyings.iter().filter(|v| v.variable == id) {
            let end = varying.offset.checked_add(varying.components);
            if end.is_none_or(|end| end > words) {
                return Err(Fault::Malformed);
            }
            let carries = Carries::Located {
                location: varying.location,
                component: varying.component,
                index: varying.index,
            };
            self.port(carries, (first + varying.offset, varying.components), input)?;
            found = true;
        }
        if found {
            Ok(())
        } else {
            Err(Fault::Malformed)
        }
    }

    fn port(
        &mut self,
        carries: Carries,
        (first, words): (u32, u32),
        input: bool,
    ) -> Result<(), Fault> {
        let ports = if input {
            &mut self.program.inputs
        } else {
            &mut self.program.outputs
        };
        push(
            ports,
            Port {
                carries,
                first,
                words,
            },
            MAX_PROGRAM_INSTRUCTIONS,
        )
    }

    /// Whether an array of `class` is an array of descriptors: of images,
    /// samplers, or blocks.
    fn holds_descriptors(&self, array: TypeId, class: StorageClass) -> Result<bool, Fault> {
        let (Type::Array { element, .. } | Type::RuntimeArray { element }) = self.ty(array)? else {
            return Ok(false);
        };
        Ok(match self.ty(element)? {
            Type::Image(_) | Type::Sampler | Type::SampledImage { .. } => true,
            Type::Struct { .. } => class != StorageClass::UNIFORM_CONSTANT,
            _ => false,
        })
    }

    /// Whether a built-in holds one value for a whole tile.
    fn shared_builtin(&self, which: Builtin) -> bool {
        match which {
            Builtin::NumWorkgroups
            | Builtin::WorkgroupSize
            | Builtin::SubgroupSize
            | Builtin::NumSubgroups => true,
            Builtin::WorkgroupId => self.whole_tiles,
            _ => false,
        }
    }

    /// The binding kind of the binding at `index` among the program's.
    pub(crate) fn binding_kind(&self, index: u32) -> Option<BindingKind> {
        self.entry.bindings().get(index as usize).map(|b| b.kind)
    }

    /// Whether a load through a pointer to `root` reads one value for the
    /// whole tile when the pointer is the tile's: memory no invocation of
    /// the dispatch writes.
    pub(crate) fn reads_shared(&self, root: Root) -> bool {
        match root {
            Root::PushConstants | Root::Shared { .. } => true,
            Root::Binding(index) => self.binding_kind(index) == Some(BindingKind::UniformBuffer),
            Root::Private { .. } | Root::Workgroup { .. } => false,
        }
    }

    /// Store `initial`, or zeros, through the variable pointer `pointer`.
    pub(crate) fn initialize(
        &mut self,
        insts: &mut Vec<Inst>,
        pointer: Val,
        initial: Option<Val>,
    ) -> Result<(), Fault> {
        let Val::Pointer {
            root,
            pointee,
            layout,
            reg,
            ..
        } = pointer
        else {
            return Err(Fault::Malformed);
        };
        let words = self.words(pointee)?;
        let src = match initial {
            Some(Val::Data { reg, .. }) => reg,
            Some(_) => return Err(Fault::Malformed),
            None => self.zeros(words)?,
        };
        let plan = self.plan(pointee, layout)?;
        self.count(Count::Instructions, 1)?;
        push(
            insts,
            Inst::Store {
                src,
                root,
                ptr: reg,
                plan,
            },
            MAX_PROGRAM_INSTRUCTIONS,
        )
    }

    /// The plan a load or store of `pointee` laid out as `layout` takes,
    /// made once for each type and layout however often it is used.
    pub(crate) fn plan(&mut self, pointee: TypeId, layout: Layout) -> Result<u32, Fault> {
        let key = plan_key(pointee, layout);
        if let Some(&plan) = key.and_then(|key| self.plans_of.get(key)) {
            return Ok(plan);
        }
        let plan = match layout {
            Layout::Words => Plan::Words(self.words(pointee)?),
            Layout::Explicit { matrix, column } => {
                let first = u32::try_from(self.program.scalars.len()).map_err(|_| Fault::Budget)?;
                self.scalars(pointee, (0, 0), (matrix, column))?;
                let last = u32::try_from(self.program.scalars.len()).map_err(|_| Fault::Budget)?;
                Plan::Scalars(first, last)
            }
        };
        let index = u32::try_from(self.program.plans.len()).map_err(|_| Fault::Budget)?;
        push(&mut self.program.plans, plan, MAX_PROGRAM_INSTRUCTIONS)?;
        if let Some(key) = key {
            self.plans_of
                .try_insert(key, index)
                .map_err(|_| Fault::OutOfMemory)?;
        }
        Ok(index)
    }

    /// The scalars of `ty` at word `word` of a value and byte `byte` of
    /// memory, as an explicit layout places them.
    fn scalars(
        &mut self,
        ty: TypeId,
        (word, byte): (u32, u32),
        (matrix, column): (Option<(u32, bool)>, Option<u32>),
    ) -> Result<(), Fault> {
        let scalar = |this: &mut Self, word: u32, byte: u32, bits: u32| {
            push(
                &mut this.program.scalars,
                Scalar {
                    word,
                    byte,
                    bits: u8::try_from(bits).map_err(|_| Fault::Malformed)?,
                },
                MAX_SCALARS,
            )
        };
        self.plan_visits += 1;
        if self.plan_visits > MAX_PLAN_VISITS {
            return Err(Fault::Budget);
        }
        // A value of no words lists nothing, however many elements it has.
        if self.words(ty)? == 0 {
            return Ok(());
        }
        match self.ty(ty)? {
            Type::Scalar(s) => scalar(self, word, byte, num(s).bits()),
            Type::Vector { scalar: s, count } => {
                let n = num(s);
                let stride = column.unwrap_or(n.bits() / 8);
                for i in 0..count {
                    scalar(self, word + i * n.words(), at(byte, i, stride)?, n.bits())?;
                }
                Ok(())
            }
            Type::Matrix {
                columns,
                rows,
                scalar: s,
                ..
            } => {
                let (stride, row_major) = matrix.ok_or(Fault::Malformed)?;
                let n = num(s);
                let bytes = n.bits() / 8;
                for c in 0..columns {
                    for r in 0..rows {
                        let offset = if row_major {
                            at(at(byte, r, stride)?, c, bytes)?
                        } else {
                            at(at(byte, c, stride)?, r, bytes)?
                        };
                        scalar(self, word + (c * rows + r) * n.words(), offset, n.bits())?;
                    }
                }
                Ok(())
            }
            Type::Array { element, length } => {
                let stride = self.admitted.array_stride(ty).ok_or(Fault::Malformed)?;
                let words = self.words(element)?;
                for i in 0..length {
                    self.scalars(
                        element,
                        (word + i * words, at(byte, i, stride)?),
                        (matrix, None),
                    )?;
                }
                Ok(())
            }
            Type::Struct { members } => {
                let mut before = 0u32;
                for index in 0..members {
                    let m = self.member(ty, index)?;
                    let offset = m.offset.ok_or(Fault::Malformed)?;
                    let inner = m
                        .matrix_stride
                        .map(|stride| (stride, m.row_major))
                        .or(matrix);
                    self.scalars(
                        m.ty,
                        (
                            word + before,
                            byte.checked_add(offset).ok_or(Fault::Malformed)?,
                        ),
                        (inner, None),
                    )?;
                    before += self.words(m.ty)?;
                }
                Ok(())
            }
            _ => Err(Fault::Malformed),
        }
    }

    /// An access chain from the pointer `base` through `indexes`: its root,
    /// the steps it takes, and the layout and type it reaches.
    pub(crate) fn access_chain(
        &mut self,
        insts: &mut Vec<Inst>,
        instances: &[Instance],
        instance: u32,
        (base, indexes): (Id, &[Id]),
        result_pointee: TypeId,
    ) -> Result<Val, Fault> {
        let Val::Pointer {
            root,
            pointee,
            mut layout,
            reg,
            descriptors,
        } = self.value(instances, instance, base)?
        else {
            return Err(Fault::Malformed);
        };
        let mut uniform = reg.is_uniform();
        let mut ty = pointee;
        // Steps go straight into the program; the constant bytes a chain
        // begins with are folded into its base where that is a constant.
        let first_step = self.program.steps.len();
        let mut base = reg;
        let mut leading = true;
        for (position, &index_id) in indexes.iter().enumerate() {
            let Val::Data {
                reg: index_reg,
                ty: index_ty,
            } = self.value(instances, instance, index_id)?
            else {
                return Err(Fault::Malformed);
            };
            uniform &= index_reg.is_uniform();
            let (index_num, _) = self.components(index_ty)?;
            if position == 0 && descriptors {
                leading = false;
                push(
                    &mut self.program.steps,
                    Step::Element {
                        index: index_reg,
                        num: index_num,
                    },
                    MAX_PROGRAM_INSTRUCTIONS,
                )?;
                ty = match self.ty(ty)? {
                    Type::Array { element, .. } | Type::RuntimeArray { element } => element,
                    _ => return Err(Fault::Malformed),
                };
                continue;
            }
            let constant = self.constant_index(index_reg, index_num);
            let (step, next) = self.index_step(ty, &mut layout, constant)?;
            let step = match step {
                Stride::Member(offset) => Step::Bytes(offset),
                Stride::Each(stride) => match constant
                    .and_then(|c| u64::try_from(c).ok())
                    .and_then(|c| c.checked_mul(u64::from(stride)))
                    .and_then(|bytes| u32::try_from(bytes).ok())
                {
                    Some(bytes) => Step::Bytes(bytes),
                    None => Step::Index {
                        index: index_reg,
                        num: index_num,
                        stride,
                    },
                },
            };
            if let Step::Bytes(bytes) = step {
                self.push_bytes(first_step, bytes)?;
            } else {
                if leading {
                    base = self.fold_leading(base, first_step)?;
                    leading = false;
                }
                push(&mut self.program.steps, step, MAX_PROGRAM_INSTRUCTIONS)?;
            }
            if let Some(next) = next {
                ty = next;
            }
        }
        if leading {
            base = self.fold_leading(base, first_step)?;
        }
        let reg = if self.program.steps.len() == first_step {
            base
        } else {
            let dst = self.fresh(2, uniform)?;
            let first = u32::try_from(first_step).map_err(|_| Fault::Budget)?;
            let last = u32::try_from(self.program.steps.len()).map_err(|_| Fault::Budget)?;
            self.count(Count::Instructions, 1)?;
            push(
                insts,
                Inst::Chain {
                    dst,
                    base,
                    steps: (first, last),
                },
                MAX_PROGRAM_INSTRUCTIONS,
            )?;
            dst
        };
        Ok(Val::Pointer {
            root,
            pointee: result_pointee,
            layout,
            reg,
            descriptors: descriptors && indexes.is_empty(),
        })
    }

    /// How an index steps from a value of `ty` laid out as `layout`, a
    /// struct's by its `constant` member: the stride it scales or the
    /// member's offset, and the type it reaches, a vector's component
    /// reaching none the chain goes on from.
    fn index_step(
        &mut self,
        ty: TypeId,
        layout: &mut Layout,
        constant: Option<i64>,
    ) -> Result<(Stride, Option<TypeId>), Fault> {
        Ok(match (self.ty(ty)?, *layout) {
            (Type::Struct { members }, _) => {
                let member = u32::try_from(constant.ok_or(Fault::Malformed)?)
                    .map_err(|_| Fault::Malformed)?;
                if member >= members {
                    return Err(Fault::Malformed);
                }
                let declared = self.member(ty, member)?;
                let offset = match layout {
                    Layout::Words => self
                        .words_before(ty, members, member)?
                        .checked_mul(4)
                        .ok_or(Fault::Budget)?,
                    Layout::Explicit { .. } => declared.offset.ok_or(Fault::Malformed)?,
                };
                if let Layout::Explicit { matrix, column } = layout {
                    *matrix = declared
                        .matrix_stride
                        .map(|s| (s, declared.row_major))
                        .or(*matrix);
                    *column = None;
                }
                (Stride::Member(offset), Some(declared.ty))
            }
            (Type::Array { element, .. } | Type::RuntimeArray { element }, Layout::Words) => (
                Stride::Each(self.words(element)?.checked_mul(4).ok_or(Fault::Budget)?),
                Some(element),
            ),
            (
                Type::Array { element, .. } | Type::RuntimeArray { element },
                Layout::Explicit { matrix, .. },
            ) => {
                *layout = Layout::Explicit {
                    matrix,
                    column: None,
                };
                (
                    Stride::Each(self.admitted.array_stride(ty).ok_or(Fault::Malformed)?),
                    Some(element),
                )
            }
            (Type::Matrix { column, .. }, Layout::Words) => (
                Stride::Each(self.words(column)?.checked_mul(4).ok_or(Fault::Budget)?),
                Some(column),
            ),
            (Type::Matrix { column, scalar, .. }, Layout::Explicit { matrix, .. }) => {
                let (stride, row_major) = matrix.ok_or(Fault::Malformed)?;
                // A row-major matrix's column starts a scalar further on
                // and steps a row's stride between its components.
                *layout = Layout::Explicit {
                    matrix,
                    column: row_major.then_some(stride),
                };
                let each = if row_major {
                    num(scalar).bits() / 8
                } else {
                    stride
                };
                (Stride::Each(each), Some(column))
            }
            (Type::Vector { scalar, .. }, Layout::Words) => {
                (Stride::Each(num(scalar).words() * 4), None)
            }
            (Type::Vector { scalar, .. }, Layout::Explicit { column, .. }) => {
                (Stride::Each(column.unwrap_or(num(scalar).bits() / 8)), None)
            }
            _ => return Err(Fault::Malformed),
        })
    }

    /// A constant index's value, signed, where `reg` is a constant's.
    fn constant_index(&self, reg: Reg, index: crate::software::shader::ir::Num) -> Option<i64> {
        if !super::is_constant(reg) {
            return None;
        }
        let at = reg.slot() as usize;
        let low = u64::from(*self.program.constants.get(at)?);
        let high = if index.words() == 2 {
            u64::from(*self.program.constants.get(at + 1)?)
        } else {
            0
        };
        Some(crate::software::shader::ops::signed(
            low | high << 32,
            index.bits(),
        ))
    }

    /// The constant pointer `bytes` past the constant pointer `base`, or
    /// `None` where `base` is not a constant or the offset leaves its range.
    fn folded_pointer(&mut self, base: Reg, bytes: u64) -> Result<Option<Reg>, Fault> {
        if !super::is_constant(base) {
            return Ok(None);
        }
        let at = base.slot() as usize;
        let (Some(&element), Some(&offset)) = (
            self.program.constants.get(at),
            self.program.constants.get(at + 1),
        ) else {
            return Err(Fault::Malformed);
        };
        let Some(moved) = u64::from(offset)
            .checked_add(bytes)
            .and_then(|v| u32::try_from(v).ok())
        else {
            return Ok(None);
        };
        if moved == u32::MAX {
            return Ok(None);
        }
        // Constants are never written, so equal pointers share one.
        let key = u64::from(element) << 32 | u64::from(moved);
        if let Some(&slot) = self.folded.get(key) {
            return Ok(Some(Reg::uniform(slot)));
        }
        let reg = self.constant_words(&[element, moved])?;
        self.folded
            .try_insert(key, reg.slot())
            .map_err(|_| Fault::OutOfMemory)?;
        Ok(Some(reg))
    }

    /// A step of `bytes` added to the chain whose steps start at `first`:
    /// none for none, and one with the step before where that moves bytes
    /// too, as moving by two counts is moving by their sum.
    fn push_bytes(&mut self, first: usize, bytes: u32) -> Result<(), Fault> {
        if bytes == 0 {
            return Ok(());
        }
        let steps = &mut self.program.steps;
        if steps.len() > first {
            if let Some(Step::Bytes(before)) = steps.last_mut() {
                if let Some(sum) = before.checked_add(bytes) {
                    *before = sum;
                    return Ok(());
                }
            }
        }
        push(steps, Step::Bytes(bytes), MAX_PROGRAM_INSTRUCTIONS)
    }

    /// `base` moved by the chain's steps from `first` where they are one run
    /// of constant bytes and it is a constant: folded into a constant, the
    /// steps taken back.
    fn fold_leading(&mut self, base: Reg, first: usize) -> Result<Reg, Fault> {
        let run = match self.program.steps.get(first..) {
            Some([Step::Bytes(bytes)]) => u64::from(*bytes),
            _ => return Ok(base),
        };
        let Some(folded) = self.folded_pointer(base, run)? else {
            return Ok(base);
        };
        self.program.steps.truncate(first);
        Ok(folded)
    }
}

/// What a plan of `pointee` laid out as `layout` is made from, as one key:
/// the type, a matrix's stride and whether it is row-major and a column of
/// it. `None` where those do not fit, and the plan is made afresh.
fn plan_key(pointee: TypeId, layout: Layout) -> Option<u64> {
    let ty = u64::from(pointee.index());
    if ty >= 1 << 24 {
        return None;
    }
    let (stride, flags) = match layout {
        Layout::Words => (0, 0),
        Layout::Explicit {
            matrix: None,
            column: None,
        } => (0, 1),
        Layout::Explicit {
            matrix: Some((stride, row_major)),
            column,
        } => {
            // A column steps the matrix's own stride, so its presence is all
            // it adds.
            let column = match column {
                None => 0,
                Some(step) if step == stride => 8,
                Some(_) => return None,
            };
            (stride, 1 | 2 | u64::from(row_major) << 2 | column)
        }
        Layout::Explicit {
            matrix: None,
            column: Some(_),
        } => return None,
    };
    Some(ty << 40 | u64::from(stride) << 8 | flags)
}

/// `byte` plus `index` strides of `stride` bytes.
fn at(byte: u32, index: u32, stride: u32) -> Result<u32, Fault> {
    index
        .checked_mul(stride)
        .and_then(|bytes| byte.checked_add(bytes))
        .ok_or(Fault::Malformed)
}

/// The built-in a compute invocation reads `built_in` as.
const fn builtin(built_in: BuiltIn) -> Option<Builtin> {
    Some(match built_in {
        BuiltIn::LOCAL_INVOCATION_ID => Builtin::LocalInvocationId,
        BuiltIn::GLOBAL_INVOCATION_ID => Builtin::GlobalInvocationId,
        BuiltIn::WORKGROUP_ID => Builtin::WorkgroupId,
        BuiltIn::NUM_WORKGROUPS => Builtin::NumWorkgroups,
        BuiltIn::LOCAL_INVOCATION_INDEX => Builtin::LocalInvocationIndex,
        BuiltIn::WORKGROUP_SIZE => Builtin::WorkgroupSize,
        BuiltIn::SUBGROUP_SIZE => Builtin::SubgroupSize,
        BuiltIn::SUBGROUP_LOCAL_INVOCATION_ID => Builtin::SubgroupLocalInvocationId,
        BuiltIn::NUM_SUBGROUPS => Builtin::NumSubgroups,
        BuiltIn::SUBGROUP_ID => Builtin::SubgroupId,
        BuiltIn::SUBGROUP_EQ_MASK => Builtin::SubgroupEqMask,
        BuiltIn::SUBGROUP_GE_MASK => Builtin::SubgroupGeMask,
        BuiltIn::SUBGROUP_GT_MASK => Builtin::SubgroupGtMask,
        BuiltIn::SUBGROUP_LE_MASK => Builtin::SubgroupLeMask,
        BuiltIn::SUBGROUP_LT_MASK => Builtin::SubgroupLtMask,
        BuiltIn::HELPER_INVOCATION => Builtin::HelperInvocation,
        _ => return None,
    })
}

/// The fixed-function built-in `built_in` is.
const fn fixed(built_in: BuiltIn) -> Option<Fixed> {
    Some(match built_in {
        BuiltIn::VERTEX_INDEX => Fixed::VertexIndex,
        BuiltIn::INSTANCE_INDEX => Fixed::InstanceIndex,
        BuiltIn::POSITION => Fixed::Position,
        BuiltIn::POINT_SIZE => Fixed::PointSize,
        BuiltIn::FRAG_COORD => Fixed::FragCoord,
        BuiltIn::FRONT_FACING => Fixed::FrontFacing,
        BuiltIn::POINT_COORD => Fixed::PointCoord,
        BuiltIn::SAMPLE_MASK => Fixed::SampleMask,
        BuiltIn::FRAG_DEPTH => Fixed::FragDepth,
        _ => return None,
    })
}
