//! The validator's first pass: the module's section order, where each
//! instruction stands, what each id names, and where the functions and their
//! blocks are; then the passes that follow it, in order.

use alloc::vec::Vec;
use core::ops::Range;

use super::allow::{self, class, Class, Enabled, Section};
use super::body::{Access, Facts, Root, Typing};
use super::cfg::Scratch;
use super::declare::Deco;
use super::entry::EntryPoint;
use super::layout::Layouts;
use super::types::{Scalar, TypeInfo, TypeKey};
use super::{
    push, with_capacity, Def, Limit, Refusal, Rule, What, MAX_ENTRY_POINTS, MAX_GLOBALS,
    MAX_LOCALS, MAX_STRING, NONE,
};
use crate::enumerant::{AddressingModel, Capability, MemoryModel};
use crate::grammar::Shape;
use crate::{Id, Instruction, Module, Op, Value};

/// One function: where its definition starts and ends, how many parameters
/// it has, and its blocks.
#[derive(Clone, Debug)]
pub(crate) struct Function {
    pub(crate) id: Id,
    /// `OpFunction`'s instruction index.
    pub(crate) at: u32,
    pub(crate) parameters: u32,
    /// Its blocks, in the validator's block table.
    pub(crate) blocks: Range<u32>,
}

/// One block: its label, and its first and last instructions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Block {
    pub(crate) label: Id,
    /// `OpLabel`'s instruction index.
    pub(crate) at: u32,
    /// The terminator's instruction index.
    pub(crate) terminator: u32,
    /// The merge instruction's index, or [`NONE`].
    pub(crate) merge: u32,
}

/// Where the scan stands within the functions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Place {
    /// At module scope.
    Outside,
    /// After `OpFunction`, before its first block.
    Head,
    /// Within a block: whether it is the function's first, whether only
    /// variables or phis have been seen, and whether its merge instruction has.
    Block {
        first: bool,
        head: bool,
        merge: bool,
        open: Block,
    },
    /// After a block's terminator.
    Between,
}

impl Place {
    const fn past_head(self) -> Self {
        match self {
            Self::Block {
                first, merge, open, ..
            } => Self::Block {
                first,
                head: false,
                merge,
                open,
            },
            other => other,
        }
    }
}

/// The scan's running state.
struct Scan {
    section: Section,
    place: Place,
    previous: Op,
    memory_models: u32,
    globals: usize,
    locals: usize,
    annotations: Option<Range<u32>>,
    declarations: Option<Range<u32>>,
}

/// The module-scope state every pass after the first reads.
pub(crate) struct Validator<'m> {
    pub(crate) m: &'m Module,
    pub(crate) enabled: Enabled,
    /// What each defined id names, by its definition slot.
    pub(crate) defs: Vec<Def>,
    /// Each id's definition slot plus one, where the bound is dense enough
    /// to index by; empty otherwise.
    dense: Vec<u32>,
    pub(crate) types: Vec<TypeInfo>,
    /// Structure members and function parameters, as type indexes.
    pub(crate) lists: Vec<u32>,
    /// The first declaration of each scalar type, by index.
    pub(crate) scalars: Vec<(Scalar, u32)>,
    /// The non-aggregate, non-pointer type declarations, by key and index,
    /// for refusing a duplicate once all are declared.
    pub(crate) keys: Vec<(TypeKey, u32)>,
    /// Each type's logical class, where the module copies logically; empty
    /// otherwise.
    pub(crate) logical: Vec<u32>,
    /// Each type's buffer extent under the base and extended rules, once
    /// computed.
    pub(crate) layouts: Layouts,
    pub(crate) decorations: Vec<Deco>,
    pub(crate) functions: Vec<Function>,
    pub(crate) blocks: Vec<Block>,
    /// Each function's id and index, by ascending id.
    pub(crate) function_ids: Vec<(Id, u32)>,
    /// What each function's body does, by index.
    pub(crate) facts: Vec<Facts>,
    /// Each pointer's, and each image loaded through one's, root, by slot.
    pub(crate) roots: Vec<Root>,
    /// Whether any id is decorated `FPRoundingMode`.
    pub(crate) rounded: bool,
    /// `OpEntryPoint` instructions, by index.
    pub(crate) entry_points: Vec<u32>,
    /// `OpExecutionMode` and `OpExecutionModeId` instructions, by the id
    /// each names and by index.
    pub(crate) modes: Vec<(Id, u32)>,
    /// The constant decorated `WorkgroupSize`, found once its declarations
    /// are known.
    pub(crate) workgroup: Option<Id>,
    /// The tables each function's checks fill, kept for the next.
    pub(crate) scratch: Scratch,
    /// The annotation and declaration sections, as instruction ranges.
    pub(crate) annotations: Range<u32>,
    pub(crate) declarations: Range<u32>,
}

impl<'m> Validator<'m> {
    /// Run every pass over `module`.
    pub(crate) fn run(module: &'m Module) -> Result<(Vec<Capability>, Vec<EntryPoint>), Refusal> {
        let enabled = Enabled::collect(module)?;
        if enabled.version < allow::OLDEST {
            return Err(Rule::Version.module());
        }
        let mut v = Self {
            m: module,
            enabled,
            defs: Vec::new(),
            dense: Vec::new(),
            types: Vec::new(),
            lists: Vec::new(),
            scalars: Vec::new(),
            keys: Vec::new(),
            layouts: Layouts::default(),
            logical: Vec::new(),
            decorations: Vec::new(),
            functions: Vec::new(),
            blocks: Vec::new(),
            function_ids: Vec::new(),
            facts: Vec::new(),
            roots: Vec::new(),
            rounded: false,
            entry_points: Vec::new(),
            modes: Vec::new(),
            workgroup: None,
            scratch: Scratch::default(),
            annotations: 0..0,
            declarations: 0..0,
        };
        v.scan()?;
        v.index()?;
        v.function_ids = super::collect(
            v.functions
                .iter()
                .enumerate()
                .map(|(index, f)| (f.id, u32::try_from(index).unwrap_or(NONE))),
        )?;
        v.function_ids.sort_unstable_by_key(|&(id, _)| id);
        v.facts = with_capacity(v.functions.len())?;
        v.facts.resize(v.functions.len(), Facts::default());
        v.roots = with_capacity(v.defs.len())?;
        v.roots.resize(
            v.defs.len(),
            Root {
                slot: NONE,
                forbid: Access::NONE,
            },
        );
        v.declare()?;
        if v.m
            .instructions()
            .iter()
            .any(|i| i.op() == Op::COPY_LOGICAL)
        {
            v.logical = v.logical_classes()?;
        }
        for function in 0..v.functions.len() {
            v.function(function)?;
        }
        let entry_points = v.entry_points()?;
        Ok((v.enabled.capabilities().to_vec(), entry_points))
    }

    /// The function index `id` names.
    pub(crate) fn function_of(&self, id: Id) -> Option<usize> {
        self.function_ids
            .binary_search_by_key(&id, |&(f, _)| f)
            .ok()
            .and_then(|at| self.function_ids.get(at))
            .map(|&(_, index)| index as usize)
    }

    /// The definition slot of `id`, if anything defines it.
    pub(crate) fn slot(&self, id: Id) -> Option<usize> {
        if self.dense.is_empty() {
            return self
                .m
                .definitions
                .binary_search_by_key(&id, |&(defined, _)| defined)
                .ok();
        }
        let raw = *self.dense.get(id.get() as usize)?;
        raw.checked_sub(1).map(|slot| slot as usize)
    }

    /// What the validator knows of `id`.
    pub(crate) fn def(&self, id: Id) -> Result<Def, Rule> {
        self.slot(id)
            .and_then(|slot| self.defs.get(slot).copied())
            .ok_or(Rule::Undefined(id))
    }

    /// The instruction defining `id`.
    pub(crate) fn defining(&self, id: Id) -> Result<&'m Instruction, Rule> {
        self.defining_index(id).map(|index| self.instruction(index))
    }

    /// The instruction index defining `id`.
    pub(crate) fn defining_index(&self, id: Id) -> Result<u32, Rule> {
        self.slot(id)
            .and_then(|slot| self.m.definitions.get(slot))
            .map(|&(_, index)| index)
            .ok_or(Rule::Undefined(id))
    }

    pub(crate) fn instruction(&self, index: u32) -> &'m Instruction {
        const EMPTY: Instruction = Instruction {
            op: Op::NOP,
            words: 0,
            at: 0,
            first: 0,
            count: 0,
        };
        self.m.instructions().get(index as usize).unwrap_or(&EMPTY)
    }

    /// The module's sections in order, where every instruction stands, and
    /// the gate each one passes.
    fn scan(&mut self) -> Result<(), Refusal> {
        let m = self.m;
        let mut state = Scan {
            section: Section::Capability,
            place: Place::Outside,
            previous: Op::NOP,
            memory_models: 0,
            globals: 0,
            locals: 0,
            annotations: None,
            declarations: None,
        };
        for (index, instruction) in m.instructions().iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| Rule::Layout.module())?;
            let op = instruction.op();
            let class = class(op).ok_or_else(|| Rule::NotAdmitted(op).at(instruction))?;
            self.enabled.check(m, instruction)?;
            for operand in m
                .operands(instruction)
                .iter()
                .filter(|o| o.kind().shape() == Shape::Text)
            {
                // A character takes a byte at least, so only a long string needs counting.
                if let Value::Text(text) = m.value(operand) {
                    if text.len() > MAX_STRING && text.chars().count() > MAX_STRING {
                        return Err(Rule::Limit(Limit::String).at(instruction));
                    }
                }
            }
            self.section(&mut state, class, index, instruction)?;
            self.place(&mut state, class, index, instruction)?;
            state.previous = op;
        }
        if state.place != Place::Outside {
            return Err(Rule::Block.module());
        }
        if state.memory_models != 1 {
            return Err(Rule::Layout.module());
        }
        if self.entry_points.is_empty() {
            return Err(Rule::NoEntryPoint.module());
        }
        let end = len_u32(&self.m.instructions);
        self.annotations = state.annotations.unwrap_or(end..end);
        self.declarations = state.declarations.unwrap_or(end..end);
        Ok(())
    }

    /// Advance the section `instruction` stands in, refusing it out of order.
    fn section(
        &mut self,
        state: &mut Scan,
        class: Class,
        index: u32,
        instruction: &Instruction,
    ) -> Result<(), Refusal> {
        let layout = Rule::Layout.at(instruction);
        let outside = state.place == Place::Outside;
        let section = match class {
            Class::Section(section) => Some(section),
            Class::Line => None,
            Class::Type | Class::Constant => Some(Section::Declaration),
            Class::Variable | Class::Undef if outside => Some(Section::Declaration),
            _ => Some(Section::Function),
        };
        match section {
            Some(section) if section < state.section => return Err(layout),
            Some(section) => state.section = section,
            None if state.section < Section::Declaration => return Err(layout),
            None => {}
        }
        if instruction.op() == Op::SOURCE_CONTINUED
            && !matches!(state.previous, Op::SOURCE | Op::SOURCE_CONTINUED)
        {
            return Err(layout);
        }
        match state.section {
            Section::Annotation => extend(&mut state.annotations, index),
            Section::Declaration => extend(&mut state.declarations, index),
            _ => {}
        }
        match class {
            Class::Section(Section::MemoryModel) => {
                state.memory_models += 1;
                self.memory_model(instruction)?;
            }
            Class::Section(Section::EntryPoint) => {
                if self.entry_points.len() == MAX_ENTRY_POINTS {
                    return Err(Rule::Limit(Limit::EntryPoints).at(instruction));
                }
                push(&mut self.entry_points, index)?;
            }
            Class::Section(Section::ExecutionMode) => {
                let Some(Value::Id(target)) = self
                    .m
                    .operands(instruction)
                    .first()
                    .map(|o| self.m.value(o))
                else {
                    return Err(Rule::Layout.at(instruction));
                };
                push(&mut self.modes, (target, index))?;
            }
            Class::Variable if outside => {
                state.globals += 1;
                if state.globals > MAX_GLOBALS {
                    return Err(Rule::Limit(Limit::Globals).at(instruction));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Track functions and blocks, refusing an instruction outside the place
    /// it belongs.
    fn place(
        &mut self,
        state: &mut Scan,
        class: Class,
        index: u32,
        instruction: &Instruction,
    ) -> Result<(), Refusal> {
        let m = self.m;
        let block_rule = Rule::Block.at(instruction);
        match (state.place, class) {
            (_, Class::Line) => {
                if let Place::Block { merge, .. } = state.place {
                    // The merge instruction is its block's second-to-last.
                    if merge {
                        return Err(Rule::Merge.at(instruction));
                    }
                }
            }
            (Place::Outside, Class::Function) => {
                self.open_function(index, instruction)?;
                state.place = Place::Head;
            }
            (
                Place::Outside,
                Class::Variable | Class::Undef | Class::Type | Class::Constant | Class::Section(_),
            ) => {}
            (Place::Outside, _) => return Err(Rule::Layout.at(instruction)),
            (Place::Head, Class::Parameter)
                if matches!(state.previous, Op::FUNCTION | Op::FUNCTION_PARAMETER) =>
            {
                if let Some(f) = self.functions.last_mut() {
                    f.parameters += 1;
                }
            }
            (Place::Head | Place::Between, Class::Label) => {
                let first = state.place == Place::Head;
                let label = m.result(instruction).ok_or(block_rule)?;
                state.place = Place::Block {
                    first,
                    head: true,
                    merge: false,
                    open: Block {
                        label,
                        at: index,
                        terminator: NONE,
                        merge: NONE,
                    },
                };
            }
            (Place::Between, Class::FunctionEnd) => state.place = Place::Outside,
            (Place::Block { first, head, .. }, Class::Variable) => {
                state.locals += 1;
                if state.locals > MAX_LOCALS {
                    return Err(Rule::Limit(Limit::Locals).at(instruction));
                }
                if !first || !head {
                    return Err(Rule::Layout.at(instruction));
                }
            }
            (Place::Block { .. }, Class::Undef) => state.place = state.place.past_head(),
            (
                Place::Block {
                    first,
                    head,
                    merge,
                    mut open,
                },
                Class::Body(typing),
            ) => {
                if merge && !typing.terminates() {
                    return Err(Rule::Merge.at(instruction));
                }
                if typing == Typing::Phi {
                    if first || !head {
                        return Err(Rule::Phi.at(instruction));
                    }
                } else if typing.terminates() {
                    open.terminator = index;
                    push(&mut self.blocks, open)?;
                    if let Some(f) = self.functions.last_mut() {
                        f.blocks.end = len_u32(&self.blocks);
                    }
                    state.place = Place::Between;
                } else {
                    let merge = matches!(typing, Typing::LoopMerge | Typing::SelectionMerge);
                    if merge {
                        open.merge = index;
                    }
                    state.place = Place::Block {
                        first,
                        head: false,
                        merge,
                        open,
                    };
                }
            }
            _ => return Err(block_rule),
        }
        Ok(())
    }

    /// Record the function `OpFunction` at `index` opens.
    fn open_function(&mut self, index: u32, instruction: &Instruction) -> Result<(), Refusal> {
        let id = self
            .m
            .result(instruction)
            .ok_or(Rule::Block.at(instruction))?;
        let blocks = len_u32(&self.blocks);
        push(
            &mut self.functions,
            Function {
                id,
                at: index,
                parameters: 0,
                blocks: blocks..blocks,
            },
        )
    }

    /// Refuse every addressing model but `Logical` and every memory model but
    /// `GLSL450`.
    fn memory_model(&self, instruction: &Instruction) -> Result<(), Refusal> {
        if AddressingModel(allow::word(self.m, instruction, 0)) != AddressingModel::LOGICAL {
            return Err(Rule::AddressingModel.at(instruction));
        }
        if MemoryModel(allow::word(self.m, instruction, 1)) != MemoryModel::GLSL450 {
            return Err(Rule::MemoryModel.at(instruction));
        }
        Ok(())
    }

    /// Record what each defined id names and the function it is local to,
    /// and build the dense id index where the bound allows one.
    fn index(&mut self) -> Result<(), Refusal> {
        let m = self.m;
        let count = m.definitions.len();
        self.defs = with_capacity(count)?;
        let mut function = NONE;
        let mut next = 0usize;
        let mut scopes: Vec<u32> = with_capacity(m.instructions().len())?;
        for instruction in m.instructions() {
            match instruction.op() {
                Op::FUNCTION => {
                    function = u32::try_from(next).unwrap_or(NONE);
                    next += 1;
                    scopes.push(NONE);
                }
                Op::FUNCTION_END => {
                    scopes.push(function);
                    function = NONE;
                }
                _ => scopes.push(function),
            }
        }
        for &(_, index) in &m.definitions {
            let instruction = self.instruction(index);
            let what = match class(instruction.op()) {
                Some(Class::Type) => What::Type,
                Some(Class::Constant) if is_spec(instruction.op()) => What::SpecConstant,
                Some(Class::Constant) => What::Constant,
                Some(Class::Variable) => What::Variable,
                Some(Class::Undef) => What::Undef,
                Some(Class::Function) => What::Function,
                Some(Class::Parameter) => What::Parameter,
                Some(Class::Label) => What::Label,
                Some(Class::Section(Section::Import)) => What::Import,
                Some(Class::Section(Section::Source)) => What::String,
                _ => What::Value,
            };
            self.defs.push(Def {
                what,
                ty: NONE,
                scope: scopes.get(index as usize).copied().unwrap_or(NONE),
            });
        }
        let bound = m.bound() as usize;
        if bound <= count.saturating_mul(2).saturating_add(1024) {
            self.dense = with_capacity(bound)?;
            self.dense.resize(bound, 0);
            for (slot, &(id, _)) in m.definitions.iter().enumerate() {
                if let Some(entry) = self.dense.get_mut(id.get() as usize) {
                    *entry = u32::try_from(slot + 1).unwrap_or(0);
                }
            }
        }
        Ok(())
    }
}

/// Whether `op` declares a specialization constant.
pub(crate) const fn is_spec(op: Op) -> bool {
    matches!(
        op,
        Op::SPEC_CONSTANT_TRUE
            | Op::SPEC_CONSTANT_FALSE
            | Op::SPEC_CONSTANT
            | Op::SPEC_CONSTANT_COMPOSITE
            | Op::SPEC_CONSTANT_OP
    )
}

fn extend(range: &mut Option<Range<u32>>, index: u32) {
    match range {
        Some(r) => r.end = index + 1,
        None => *range = Some(index..index + 1),
    }
}

pub(crate) fn len_u32<T>(list: &[T]) -> u32 {
    u32::try_from(list.len()).unwrap_or(NONE)
}
