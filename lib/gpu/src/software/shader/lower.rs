//! Lowering an admitted module's entry point into the program a tile runs:
//! calls inlined, blocks laid out in structured order, every pointer's root
//! resolved, values the same in every lane computed once a tile, and
//! registers allocated over live ranges.
//!
//! Lowering trusts the validator's verdict but never its own reading of it:
//! an invariant it finds broken refuses the module ([`Fault::Malformed`]),
//! and everything it builds is bounded by a fixed budget, a security bound
//! like the validator's own ([`Fault::Budget`]). Its time grows with what it
//! walks — every inlined instance's operands and the tables it spans,
//! however little a walk builds — and the budget bounds each.

mod layout;
mod memory;
mod regs;
mod select;

use alloc::vec::Vec;

use tairix_collections::RadixTree;

use tairix_spirv::enumerant::Decoration;
use tairix_spirv::{
    Admitted, Component, EntryPoint, Id, Instruction, Member, Op, Scalar, Specialized, Type,
    TypeId, Value,
};

use super::ir::{Num, Program, Reg, Root};
use crate::Error;

/// The most instructions a lowered program holds, its calls inlined.
pub(crate) const MAX_PROGRAM_INSTRUCTIONS: usize = 1 << 20;

/// The most operands lowering walks, each inlined instance's its own: what
/// its time grows with, however little a walk builds.
const MAX_OPERANDS: usize = 8 * MAX_PROGRAM_INSTRUCTIONS;

/// The most parts of constants lowering visits laying them out, each nested
/// composite's included.
const MAX_CONSTANT_PARTS: usize = 1 << 24;

/// The most words the values of a lowered program hold together before
/// registers are allocated, and the most words one value holds.
const MAX_VIRTUAL_WORDS: u32 = 1 << 28;
const MAX_VALUE_WORDS: u32 = 1 << 16;

/// The most member records lowering reads: a structure's members are read
/// once for its size and once for their offsets, and again for each plan of
/// an explicit layout through it.
const MAX_MEMBER_READS: usize = 1 << 24;

/// The most words a program's constants hold: each constant laid out once,
/// its first use, beside the runs of zeros zero values share.
const MAX_CONSTANT_WORDS: usize = 1 << 22;

/// Where virtual registers start: every register below is a constant's,
/// already where it will stay.
const VIRTUAL: u32 = 1 << 30;

/// The scratch planes' register until registers are allocated: past every
/// virtual one.
const SCRATCH: u32 = VIRTUAL + MAX_VIRTUAL_WORDS;

/// Whether `reg` is a constant's words, which stay where they are.
const fn is_constant(reg: Reg) -> bool {
    reg.is_uniform() && reg.slot() < VIRTUAL
}

/// Why a module was not lowered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    /// A guarantee of the validator's found broken.
    Malformed,
    /// Past a fixed lowering bound.
    Budget,
    /// The memory lowering needs could not be had.
    OutOfMemory,
}

impl From<Fault> for Error {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::Malformed => Self::Malformed,
            Fault::Budget => Self::TooLarge,
            Fault::OutOfMemory => Self::OutOfMemory,
        }
    }
}

/// `item` pushed onto `list`, which may grow to `bound` items.
pub(crate) fn push<T>(list: &mut Vec<T>, item: T, bound: usize) -> Result<(), Fault> {
    if list.len() >= bound {
        return Err(Fault::Budget);
    }
    list.try_reserve(1).map_err(|_| Fault::OutOfMemory)?;
    list.push(item);
    Ok(())
}

/// `len` copies of `value`.
pub(crate) fn filled<T: Clone>(len: usize, value: T) -> Result<Vec<T>, Fault> {
    let mut list = Vec::new();
    list.try_reserve_exact(len)
        .map_err(|_| Fault::OutOfMemory)?;
    list.resize(len, value);
    Ok(list)
}

/// The index the next item of `list` takes.
pub(crate) fn next_index<T>(list: &[T]) -> Result<u32, Fault> {
    u32::try_from(list.len()).map_err(|_| Fault::Budget)
}

/// What a module's id holds once lowered.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Val {
    /// A value in registers.
    Data { reg: Reg, ty: TypeId },
    /// A pointer: its root, what it points to and how that is laid out, the
    /// register holding its descriptor and byte offset, and whether it
    /// points to a binding's whole array of descriptors.
    Pointer {
        root: Root,
        pointee: TypeId,
        layout: Layout,
        reg: Reg,
        descriptors: bool,
    },
    /// An image, a sampler or both: their bindings, and the register
    /// holding the descriptor of each.
    Handle {
        image: Option<u32>,
        sampler: Option<u32>,
        ty: TypeId,
        reg: Reg,
    },
}

impl Val {
    fn reg(self) -> Reg {
        match self {
            Self::Data { reg, .. } | Self::Pointer { reg, .. } | Self::Handle { reg, .. } => reg,
        }
    }
}

/// How memory a pointer reaches is laid out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Layout {
    /// As registers hold values.
    Words,
    /// As a buffer's explicit layout places it: the matrices it holds of
    /// `matrix` stride and majorness, and for a column of a row-major matrix
    /// the stride between its components.
    Explicit {
        matrix: Option<(u32, bool)>,
        column: Option<u32>,
    },
}

/// One function: the positions its instructions span, its parameters, and
/// its blocks — each its label's position and its terminator's.
pub(crate) struct Function {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) parameters: Vec<Id>,
    pub(crate) blocks: Vec<(u32, u32)>,
    /// Whether a branch from the entry reaches each block: the rest can
    /// never run, and are not lowered.
    pub(crate) reachable: Vec<bool>,
    /// How many reached blocks branch to each other than round a loop.
    pub(crate) entering: Vec<u32>,
    /// Each phi's incoming value as `(block, parent, phi, value)`, sorted.
    pub(crate) phis: Vec<(u32, u32, u32, Id)>,
    /// Whether `reachable`, `entering` and `phis` are made: only once the
    /// function is first instanced, as an entry point calls few of a module's.
    pub(crate) shaped: bool,
}

/// A lowering in progress.
pub(crate) struct Lowering<'a> {
    pub(crate) admitted: &'a Admitted,
    pub(crate) specialized: &'a Specialized,
    pub(crate) entry: &'a EntryPoint,
    pub(crate) program: Program,
    /// Each function of the module, in order.
    pub(crate) functions: Vec<Function>,
    /// Each global id's value, by the position of its declaration: a
    /// variable's from the start, a constant's once first used.
    pub(crate) globals: Vec<Option<Val>>,
    /// Each virtual register's first word, words and class, in order.
    pub(crate) virtuals: Vec<(u32, u32, bool)>,
    pub(crate) virtual_words: u32,
    /// The words of each type, by its index.
    pub(crate) sizes: Vec<u32>,
    /// The words before each member of a structure, each structure's run
    /// found from its type index once it is laid out.
    pub(crate) offsets: Vec<u32>,
    pub(crate) offsets_at: Vec<u32>,
    /// The member records read so far.
    pub(crate) member_reads: usize,
    /// The longest run of zero constants laid out, which every zero value
    /// no longer than it shares: its first word and length.
    pub(crate) zero_run: Option<(u32, u32)>,
    /// The parts of constants visited so far.
    pub(crate) constant_parts: usize,
    /// What lowering has built and walked so far, by [`Count`].
    pub(crate) counted: [usize; 4],
    /// Each plan made, by the type and layout it was made from, so each is
    /// made once.
    pub(crate) plans_of: RadixTree<u32>,
    /// The levels of types plans were made through so far.
    pub(crate) plan_visits: usize,
    /// Each pointer a chain folded to a constant, by its element above its
    /// offset, at the constant slot laid out for it.
    pub(crate) folded: RadixTree<u32>,
    /// Whether every workgroup has a tile of its own.
    pub(crate) whole_tiles: bool,
    /// The scratch planes an edge's copies break cycles through.
    pub(crate) scratch_words: u32,
    /// The instructions every instance's tables span, its calls inlined: a
    /// call of a function that makes nothing still costs its tables, so they
    /// are held to the program's own bound.
    pub(crate) instanced: usize,
}

impl<'a> Lowering<'a> {
    pub(crate) fn module(&self) -> &'a tairix_spirv::Module {
        self.admitted.module()
    }

    /// The instruction at `position`.
    pub(crate) fn at(&self, position: u32) -> Result<&'a Instruction, Fault> {
        self.module()
            .instructions()
            .get(position as usize)
            .ok_or(Fault::Malformed)
    }

    /// Operand `index` of `instruction` as an id.
    pub(crate) fn id(&self, instruction: &Instruction, index: usize) -> Result<Id, Fault> {
        match self.operand(instruction, index)? {
            Value::Id(id) => Ok(id),
            _ => Err(Fault::Malformed),
        }
    }

    /// Operand `index` of `instruction` as a word.
    pub(crate) fn word(&self, instruction: &Instruction, index: usize) -> Result<u32, Fault> {
        match self.operand(instruction, index)? {
            Value::Word(word) => Ok(word),
            Value::Number(n) => u32::try_from(n).map_err(|_| Fault::Malformed),
            _ => Err(Fault::Malformed),
        }
    }

    pub(crate) fn operand(
        &self,
        instruction: &Instruction,
        index: usize,
    ) -> Result<Value<'a>, Fault> {
        let module = self.module();
        let operand = module
            .operands(instruction)
            .get(index)
            .ok_or(Fault::Malformed)?;
        Ok(module.value(operand))
    }

    pub(crate) fn operands(&self, instruction: &Instruction) -> usize {
        self.module().operands(instruction).len()
    }

    /// What `ty` is.
    pub(crate) fn ty(&self, ty: TypeId) -> Result<Type, Fault> {
        self.admitted.ty(ty).ok_or(Fault::Malformed)
    }

    /// The result type of `instruction`.
    pub(crate) fn result_type(&self, instruction: &Instruction) -> Result<TypeId, Fault> {
        self.admitted
            .result_type(instruction)
            .ok_or(Fault::Malformed)
    }

    /// How many words a value of `ty` takes in registers.
    pub(crate) fn words(&mut self, ty: TypeId) -> Result<u32, Fault> {
        let index = ty.index() as usize;
        if let Some(&words) = self.sizes.get(index) {
            if words != u32::MAX {
                return Ok(words);
            }
        }
        let words = match self.ty(ty)? {
            Type::Void => 0,
            Type::Scalar(scalar) => num(scalar).words(),
            Type::Vector { scalar, count } => num(scalar).words() * count,
            Type::Matrix {
                column, columns, ..
            } => self.words(column)?.saturating_mul(columns),
            Type::Array { element, length } => self.words(element)?.saturating_mul(length),
            Type::Struct { members } => {
                let mut total = 0u32;
                for member in 0..members {
                    let member = self.member_type(ty, member)?;
                    total = total.saturating_add(self.words(member)?);
                }
                total
            }
            Type::Pointer { .. } | Type::SampledImage { .. } => 2,
            Type::Image(_) | Type::Sampler => 1,
            Type::RuntimeArray { .. } | Type::Function { .. } => return Err(Fault::Malformed),
        };
        if words > MAX_VALUE_WORDS {
            return Err(Fault::Budget);
        }
        if self.sizes.len() <= index {
            self.sizes
                .try_reserve(index + 1 - self.sizes.len())
                .map_err(|_| Fault::OutOfMemory)?;
            self.sizes.resize(index + 1, u32::MAX);
        }
        if let Some(slot) = self.sizes.get_mut(index) {
            *slot = words;
        }
        Ok(words)
    }

    /// The scalar and component count of a scalar or vector type.
    pub(crate) fn components(&self, ty: TypeId) -> Result<(Num, u32), Fault> {
        match self.ty(ty)? {
            Type::Scalar(scalar) => Ok((num(scalar), 1)),
            Type::Vector { scalar, count } => Ok((num(scalar), count)),
            _ => Err(Fault::Malformed),
        }
    }

    /// A fresh virtual register of `words` words, shared by the tile's
    /// lanes where `uniform`.
    pub(crate) fn fresh(&mut self, words: u32, uniform: bool) -> Result<Reg, Fault> {
        let first = self.virtual_words;
        let end = first.checked_add(words.max(1)).ok_or(Fault::Budget)?;
        if end > MAX_VIRTUAL_WORDS {
            return Err(Fault::Budget);
        }
        push(
            &mut self.virtuals,
            (first, words.max(1), uniform),
            MAX_PROGRAM_INSTRUCTIONS * 4,
        )?;
        self.virtual_words = end;
        let slot = VIRTUAL + first;
        Ok(if uniform {
            Reg::uniform(slot)
        } else {
            Reg::varying(slot)
        })
    }

    /// A constant's words in the uniform registers, where they will stay.
    pub(crate) fn constant_words(&mut self, words: &[u32]) -> Result<Reg, Fault> {
        let first = next_index(&self.program.constants)?;
        if first as usize + words.len() > MAX_CONSTANT_WORDS {
            return Err(Fault::Budget);
        }
        self.program
            .constants
            .try_reserve(words.len())
            .map_err(|_| Fault::OutOfMemory)?;
        self.program.constants.extend_from_slice(words);
        Ok(Reg::uniform(first))
    }

    /// `words` zero words: the longest run laid out where it holds them,
    /// else a new one of `words` or twice the longest, whichever is more, so
    /// all the runs together are less than twice the longest.
    pub(crate) fn zeros(&mut self, words: u32) -> Result<Reg, Fault> {
        let held = match self.zero_run {
            Some((first, held)) if held >= words => return Ok(Reg::uniform(first)),
            Some((_, held)) => held,
            None => 0,
        };
        let grown = words.max(held.saturating_mul(2)).max(1);
        let reg = self.constant_words(&filled(grown as usize, 0)?)?;
        self.zero_run = Some((reg.slot(), grown));
        Ok(reg)
    }

    /// The constant `id` names, laid out in words, or `None` for an id that
    /// is not one. A zero value of any type is a shared run of zeros.
    pub(crate) fn constant(&mut self, id: Id) -> Result<Option<Val>, Fault> {
        let Some(constant) = self.specialized.constant(self.admitted, id) else {
            return Ok(None);
        };
        let ty = {
            let instruction = self.module().definition(id).ok_or(Fault::Malformed)?;
            self.result_type(instruction)?
        };
        if matches!(
            self.specialized.value(constant),
            Some(tairix_spirv::Constant::Null)
        ) {
            let words = self.words(ty)?;
            let reg = self.zeros(words)?;
            return Ok(Some(Val::Data { reg, ty }));
        }
        let mut words = Vec::new();
        self.flatten_constant(constant, ty, &mut words)?;
        let reg = self.constant_words(&words)?;
        Ok(Some(Val::Data { reg, ty }))
    }

    /// A constant of `ty` appended to `out` as registers hold it: a nested
    /// composite is walked with a stack, so its depth costs no recursion, and
    /// every part visited is counted, as one module's constants may share
    /// parts its own words never count.
    fn flatten_constant(
        &mut self,
        constant: tairix_spirv::ConstantId,
        ty: TypeId,
        out: &mut Vec<u32>,
    ) -> Result<(), Fault> {
        let mut pending = Vec::new();
        push(
            &mut pending,
            (constant, Part::Typed(ty)),
            MAX_VALUE_WORDS as usize,
        )?;
        while let Some((constant, part)) = pending.pop() {
            self.constant_parts += 1;
            if self.constant_parts > MAX_CONSTANT_PARTS {
                return Err(Fault::Budget);
            }
            let words = self.part_words(part)?;
            match self.specialized.value(constant).ok_or(Fault::Malformed)? {
                tairix_spirv::Constant::Null => push_words(out, words, |_| 0)?,
                tairix_spirv::Constant::Scalar(bits) => {
                    push_words(out, words, |w| scalar_word(bits, w))?;
                }
                tairix_spirv::Constant::Composite(parts) => {
                    let Part::Typed(ty) = part else {
                        return Err(Fault::Malformed);
                    };
                    // Pushed last to first, so the first is laid out first; a
                    // part of no words lays out nothing, nor anything in it.
                    for (index, &child) in parts.iter().enumerate().rev() {
                        let index = u32::try_from(index).map_err(|_| Fault::Malformed)?;
                        let (_, inner) = self.part_of(ty, index)?;
                        if self.part_words(inner)? > 0 {
                            push(&mut pending, (child, inner), MAX_VALUE_WORDS as usize * 16)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Count `n` more of `what`, refused past its bound: what is built is
    /// counted as it is built, so nothing is built past what the program
    /// could take.
    pub(crate) fn count(&mut self, what: Count, n: usize) -> Result<(), Fault> {
        let at = what as usize;
        let total = self.counted.get(at).copied().unwrap_or(0).saturating_add(n);
        if total > what.bound() {
            return Err(Fault::Budget);
        }
        if let Some(slot) = self.counted.get_mut(at) {
            *slot = total;
        }
        Ok(())
    }

    /// Constituent `index` of a composite of `ty`: its first word within
    /// the composite, and its type, or its words where it is a vector's
    /// scalar.
    pub(crate) fn part_of(&mut self, ty: TypeId, index: u32) -> Result<(u32, Part), Fault> {
        match self.ty(ty)? {
            Type::Vector { scalar, count } if index < count => {
                let words = num(scalar).words();
                Ok((words * index, Part::Scalar(words)))
            }
            Type::Matrix {
                column, columns, ..
            } if index < columns => Ok((
                self.words(column)?.saturating_mul(index),
                Part::Typed(column),
            )),
            Type::Array { element, length } if index < length => Ok((
                self.words(element)?.saturating_mul(index),
                Part::Typed(element),
            )),
            Type::Struct { members } if index < members => {
                let before = self.words_before(ty, members, index)?;
                Ok((before, Part::Typed(self.member_type(ty, index)?)))
            }
            _ => Err(Fault::Malformed),
        }
    }

    /// Member `index` of the structure `ty`, each read charged.
    pub(crate) fn member(&mut self, ty: TypeId, index: u32) -> Result<Member, Fault> {
        self.charge_member_read()?;
        self.admitted.member(ty, index).ok_or(Fault::Malformed)
    }

    /// The type of member `index` of the structure `ty`, charged as a read.
    fn member_type(&mut self, ty: TypeId, index: u32) -> Result<TypeId, Fault> {
        self.charge_member_read()?;
        self.admitted.member_type(ty, index).ok_or(Fault::Malformed)
    }

    fn charge_member_read(&mut self) -> Result<(), Fault> {
        self.member_reads += 1;
        if self.member_reads > MAX_MEMBER_READS {
            return Err(Fault::Budget);
        }
        Ok(())
    }

    /// The words before member `index` of the structure `ty` of `members`
    /// members: every member's found in one walk, the first time one is
    /// asked for.
    fn words_before(&mut self, ty: TypeId, members: u32, index: u32) -> Result<u32, Fault> {
        let at = ty.index() as usize;
        let start = match self.offsets_at.get(at) {
            Some(&start) if start != u32::MAX => start,
            _ => {
                let start = next_index(&self.offsets)?;
                let mut before = 0u32;
                for member in 0..members {
                    push(&mut self.offsets, before, MAX_MEMBER_READS)?;
                    let member = self.member_type(ty, member)?;
                    before = before.saturating_add(self.words(member)?);
                }
                if self.offsets_at.len() <= at {
                    self.offsets_at
                        .try_reserve(at + 1 - self.offsets_at.len())
                        .map_err(|_| Fault::OutOfMemory)?;
                    self.offsets_at.resize(at + 1, u32::MAX);
                }
                if let Some(slot) = self.offsets_at.get_mut(at) {
                    *slot = start;
                }
                start
            }
        };
        self.offsets
            .get((start + index) as usize)
            .copied()
            .ok_or(Fault::Malformed)
    }

    /// The words a part takes.
    pub(crate) fn part_words(&mut self, part: Part) -> Result<u32, Fault> {
        match part {
            Part::Typed(ty) => self.words(ty),
            Part::Scalar(words) => Ok(words),
        }
    }

    /// The value of the global id defined at `position`: a variable's, or a
    /// constant's laid out the first time it is asked for.
    pub(crate) fn global_at(&mut self, id: Id, position: usize) -> Result<Option<Val>, Fault> {
        if let Some(Some(val)) = self.globals.get(position) {
            return Ok(Some(*val));
        }
        if position >= self.globals.len() {
            return Ok(None);
        }
        let val = self.constant(id)?;
        if let (Some(slot), Some(val)) = (self.globals.get_mut(position), val) {
            *slot = Some(val);
        }
        Ok(val)
    }

    /// The first parameter of `decoration` on `target`.
    pub(crate) fn decoration(&self, target: Id, decoration: Decoration) -> Option<u32> {
        self.admitted.decoration(target, None, decoration)
    }
}

/// What lowering counts as it builds, each against its own bound.
#[derive(Copy, Clone)]
pub(crate) enum Count {
    /// Instructions made, calls inlined.
    Instructions,
    /// The copies edges make.
    Copies,
    /// Switches' cases.
    Cases,
    /// Operands walked.
    Operands,
}

impl Count {
    const fn bound(self) -> usize {
        match self {
            Self::Instructions | Self::Copies | Self::Cases => MAX_PROGRAM_INSTRUCTIONS,
            Self::Operands => MAX_OPERANDS,
        }
    }
}

/// A constituent of a composite: a type the module declares, or a vector's
/// scalar of so many words.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Part {
    Typed(TypeId),
    Scalar(u32),
}

/// Word `word` of a scalar constant's bits.
const fn scalar_word(bits: u64, word: u32) -> u32 {
    if word == 0 {
        super::ops::low32(bits)
    } else {
        super::ops::high32(bits)
    }
}

/// `words` words of `f` pushed onto `out`.
fn push_words(out: &mut Vec<u32>, words: u32, f: impl Fn(u32) -> u32) -> Result<(), Fault> {
    if out.len() + words as usize > MAX_VALUE_WORDS as usize * 16 {
        return Err(Fault::Budget);
    }
    out.try_reserve(words as usize)
        .map_err(|_| Fault::OutOfMemory)?;
    out.extend((0..words).map(f));
    Ok(())
}

/// How a lane holds a scalar.
pub(crate) const fn num(scalar: Scalar) -> Num {
    match scalar {
        Scalar::Bool => Num::Bool,
        Scalar::Int { width: 16, .. } => Num::I16,
        Scalar::Int { width: 64, .. } => Num::I64,
        Scalar::Int { .. } => Num::I32,
        Scalar::Float { width: 16 } => Num::F16,
        Scalar::Float { width: 64 } => Num::F64,
        Scalar::Float { .. } => Num::F32,
    }
}

/// What a texel of an image whose texels read as `scalar` holds.
pub(crate) const fn component(scalar: Scalar) -> Component {
    match scalar {
        Scalar::Int { signed: true, .. } => Component::Sint,
        Scalar::Int { .. } | Scalar::Bool => Component::Uint,
        Scalar::Float { .. } => Component::Float,
    }
}

/// The program entry point `entry` of `admitted` lowers to, under
/// `specialized`'s constants, run in workgroups of `workgroup`: a vertex or
/// fragment stage's invocations are workgroups of one.
pub(crate) fn lower(
    admitted: &Admitted,
    entry: &EntryPoint,
    specialized: &Specialized,
    workgroup: [u32; 3],
) -> Result<Program, Fault> {
    let invocations = workgroup.iter().fold(1u32, |n, &s| n.saturating_mul(s));
    let invocations = usize::try_from(invocations).unwrap_or(usize::MAX);
    let mut lowering = Lowering {
        admitted,
        specialized,
        entry,
        program: Program::new(workgroup, Reg::varying(SCRATCH)),
        functions: Vec::new(),
        globals: Vec::new(),
        virtuals: Vec::new(),
        virtual_words: 0,
        sizes: Vec::new(),
        offsets: Vec::new(),
        offsets_at: Vec::new(),
        member_reads: 0,
        zero_run: None,
        constant_parts: 0,
        counted: [0; 4],
        plans_of: RadixTree::new(),
        plan_visits: 0,
        folded: RadixTree::new(),
        whole_tiles: super::ir::owns_tiles(invocations),
        scratch_words: 0,
        instanced: 0,
    };
    lowering.index()?;
    lowering.declare_globals()?;
    let blocks = lowering.lay_out()?;
    lowering.allocate(blocks)?;
    let mut program = lowering.program;
    program.barriers = program
        .blocks
        .iter()
        .any(|block| matches!(block.term, super::ir::Term::Barrier(_)));
    program.trim();
    Ok(program)
}

/// Whether `op` ends a block.
pub(crate) const fn terminates(op: Op) -> bool {
    matches!(
        op,
        Op::BRANCH
            | Op::BRANCH_CONDITIONAL
            | Op::SWITCH
            | Op::RETURN
            | Op::RETURN_VALUE
            | Op::KILL
            | Op::UNREACHABLE
            | Op::TERMINATE_INVOCATION
    )
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;
