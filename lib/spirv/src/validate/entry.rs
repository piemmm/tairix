//! Entry points: each one's function, model, name and execution modes; the
//! call graph — no recursion, bounded depth, an entry point's function never
//! called; what each entry point statically uses, its interface complete;
//! and what a pipeline must provide it.

use alloc::string::String;
use alloc::vec::Vec;

use super::binding::Binding;
use super::body::{fold, Access, Needs};
use super::types::{Scalar, Ty};
use super::{
    collect, push, with_capacity, Limit, Refusal, Rule, Validator, What, MAX_CALL_DEPTH, MAX_MODES,
    NONE,
};
use crate::enumerant::{BuiltIn, Decoration, ExecutionMode, ExecutionModel, StorageClass};
use crate::{Id, Instruction, Op, Value, Version};

/// The pipeline stage an entry point is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stage {
    /// A vertex shader.
    Vertex,
    /// A fragment shader.
    Fragment,
    /// A compute shader.
    Compute,
}

/// How a compute entry point's workgroup is sized.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WorkgroupSize {
    /// By its `LocalSize` execution mode.
    Fixed([u32; 3]),
    /// By the constant decorated `WorkgroupSize`, which a specialization
    /// may change.
    Constant(Id),
}

/// One location an entry point's input or output occupies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Varying {
    /// The location.
    pub location: u32,
    /// The first 32-bit component within it.
    pub component: u32,
    /// How many 32-bit components it takes.
    pub components: u32,
    /// What each holds, and how wide it is.
    pub scalar: super::binding::Component,
    /// The width of each value, in bits.
    pub width: u32,
    /// A fragment output's blend index; zero otherwise.
    pub index: u32,
}

/// An admitted entry point: what it is, and what a pipeline must provide
/// it.
#[derive(Clone, Debug)]
pub struct EntryPoint {
    pub(crate) name: String,
    pub(crate) stage: Stage,
    pub(crate) function: Id,
    pub(crate) workgroup: Option<WorkgroupSize>,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) push_constants: Option<u32>,
    pub(crate) inputs: Vec<Varying>,
    pub(crate) outputs: Vec<Varying>,
}

impl EntryPoint {
    /// Its name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its stage.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Its function.
    #[must_use]
    pub const fn function(&self) -> Id {
        self.function
    }

    /// A compute entry point's workgroup size; `None` for any other.
    #[must_use]
    pub const fn workgroup(&self) -> Option<WorkgroupSize> {
        self.workgroup
    }

    /// The resource bindings it statically uses, by ascending set and
    /// binding.
    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// How many bytes of push constants it reads, if it reads any.
    #[must_use]
    pub const fn push_constants(&self) -> Option<u32> {
        self.push_constants
    }

    /// The locations its user-defined inputs occupy, by ascending location
    /// and component.
    #[must_use]
    pub fn inputs(&self) -> &[Varying] {
        &self.inputs
    }

    /// The locations its user-defined outputs occupy, by ascending index,
    /// location and component.
    #[must_use]
    pub fn outputs(&self) -> &[Varying] {
        &self.outputs
    }
}

/// What one function does with its callees folded in: the globals it
/// reaches and how, and how it accesses through each pointer parameter.
#[derive(Clone, Debug, Default)]
struct Effective {
    globals: Vec<(u32, Access)>,
    parameters: Vec<Access>,
}

/// What an entry point reaches: its functions, the globals they name and
/// how, and what they need of the model.
pub(crate) struct Reach {
    pub(crate) globals: Vec<(u32, Access)>,
    pub(crate) needs: Needs,
}

impl Validator<'_> {
    /// Check every entry point and answer what each needs.
    pub(crate) fn entry_points(&mut self) -> Result<Vec<EntryPoint>, Refusal> {
        self.workgroup = self.declared_workgroup();
        let order = self.call_order()?;
        let effective = self.effective_accesses(&order)?;
        self.decorations_checked()?;
        self.modes_grouped()?;
        let called = self.called()?;
        // Each entry point's walk marks the functions it reaches with its own
        // stamp, so one table serves them all.
        let mut stamps: Vec<u32> = with_capacity(self.functions.len())?;
        stamps.resize(self.functions.len(), 0);
        let mut entries: Vec<EntryPoint> = with_capacity(self.entry_points.len())?;
        let mut names: Vec<(u32, &str)> = with_capacity(self.entry_points.len())?;
        for (&at, stamp) in self.entry_points.clone().iter().zip(1u32..) {
            let instruction = self.instruction(at);
            let refuse = |rule: Rule| rule.at(instruction);
            let model = ExecutionModel(super::allow::word(self.m, instruction, 0));
            let stage = match model {
                ExecutionModel::VERTEX => Stage::Vertex,
                ExecutionModel::FRAGMENT => Stage::Fragment,
                ExecutionModel::GL_COMPUTE => Stage::Compute,
                _ => return Err(refuse(Rule::EntryPoint)),
            };
            let name = super::allow::text(self.m, instruction, 2);
            names.push((model.0, name));
            let Some(Value::Id(function)) =
                self.m.operands(instruction).get(1).map(|o| self.m.value(o))
            else {
                return Err(refuse(Rule::EntryPoint));
            };
            let index = self.function_of(function).ok_or(refuse(Rule::EntryPoint))?;
            self.entry_function(index, &called).map_err(refuse)?;
            let reach = self
                .reach(index, &effective, &mut stamps, stamp)
                .map_err(refuse)?;
            let interface = self.interface_list(instruction).map_err(refuse)?;
            self.complete(&reach, &interface).map_err(refuse)?;
            self.model_needs(stage, &reach).map_err(refuse)?;
            let (inputs, outputs) = self.stage_interface(stage, &interface).map_err(refuse)?;
            let workgroup = self.modes_of(function, stage, &reach).map_err(refuse)?;
            let (bindings, push_constants) = self.resources(&reach).map_err(refuse)?;
            let mut owned = String::new();
            owned
                .try_reserve_exact(name.len())
                .map_err(|_| Rule::OutOfMemory.module())?;
            owned.push_str(name);
            push(
                &mut entries,
                EntryPoint {
                    name: owned,
                    stage,
                    function,
                    workgroup,
                    bindings,
                    push_constants,
                    inputs,
                    outputs,
                },
            )?;
        }
        names.sort_unstable();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return Err(Rule::EntryPoint.module());
        }
        Ok(entries)
    }

    /// The functions in an order that puts every callee before its callers,
    /// refusing recursion and a chain of calls past the bound.
    fn call_order(&self) -> Result<Vec<u32>, Refusal> {
        let n = self.functions.len();
        // 0: unvisited, 1: on the walk, 2: done.
        let mut state: Vec<u8> = with_capacity(n)?;
        state.resize(n, 0);
        let mut depth: Vec<u32> = with_capacity(n)?;
        depth.resize(n, 0);
        let mut order: Vec<u32> = with_capacity(n)?;
        let mut stack: Vec<(u32, usize)> = with_capacity(n)?;
        for start in 0..super::scan::len_u32(&self.functions) {
            if state[start as usize] != 0 {
                continue;
            }
            state[start as usize] = 1;
            stack.push((start, 0));
            while let Some(&(f, next)) = stack.last() {
                let calls = self
                    .facts
                    .get(f as usize)
                    .map_or(&[][..], |facts| &facts.calls);
                if let Some(&callee) = calls.get(next) {
                    if let Some(top) = stack.last_mut() {
                        top.1 += 1;
                    }
                    match state.get(callee as usize) {
                        Some(1) => {
                            return Err(
                                Rule::Recursion.at(self.instruction(self.functions[f as usize].at))
                            )
                        }
                        Some(0) => {
                            state[callee as usize] = 1;
                            stack.push((callee, 0));
                        }
                        Some(_) => {}
                        None => {
                            return Err(
                                Rule::Call.at(self.instruction(self.functions[f as usize].at))
                            )
                        }
                    }
                    continue;
                }
                let deepest = calls
                    .iter()
                    .map(|&c| depth[c as usize])
                    .max()
                    .map_or(1, |d| d + 1);
                if deepest as usize > MAX_CALL_DEPTH {
                    return Err(Rule::Limit(Limit::CallDepth)
                        .at(self.instruction(self.functions[f as usize].at)));
                }
                depth[f as usize] = deepest;
                state[f as usize] = 2;
                order.push(f);
                stack.pop();
            }
        }
        Ok(order)
    }

    /// Each function's accesses with its callees' folded in: what a callee
    /// does through a pointer parameter is charged to the argument's root,
    /// and refused where that root forbids it.
    fn effective_accesses(&self, order: &[u32]) -> Result<Vec<Effective>, Refusal> {
        let mut effective: Vec<Effective> = with_capacity(self.functions.len())?;
        effective.resize(self.functions.len(), Effective::default());
        for &f in order {
            let Some(facts) = self.facts.get(f as usize) else {
                continue;
            };
            let mut globals = collect(facts.globals.iter().copied())?;
            let mut parameters = collect(facts.parameters.iter().copied())?;
            for argument in &facts.arguments {
                let through = effective
                    .get(argument.callee as usize)
                    .and_then(|e| e.parameters.get(argument.parameter as usize))
                    .copied()
                    .unwrap_or(Access::NONE);
                let forbid = if argument.texels {
                    self.texel_root(argument.root.slot).forbid
                } else {
                    argument.root.forbid
                };
                let refused = Access::from_bits(through.bits() & forbid.bits());
                if refused != Access::NONE {
                    let rule = if refused.contains(Access::WRITE) {
                        Rule::NotWritable
                    } else {
                        Rule::NotReadable
                    };
                    return Err(Refusal {
                        word: argument.word,
                        rule,
                    });
                }
                let root = argument.root.slot;
                let def = self.defs.get(root as usize).copied();
                match def.map(|d| (d.what, d.scope)) {
                    Some((What::Variable, NONE)) => push(&mut globals, (root, through))?,
                    Some((What::Parameter, _)) => {
                        let index =
                            self.m.definitions.get(root as usize).and_then(|&(_, i)| {
                                i.checked_sub(self.functions[f as usize].at + 1)
                            });
                        if let Some(slot) = index.and_then(|i| parameters.get_mut(i as usize)) {
                            *slot = slot.with(through);
                        }
                    }
                    _ => {}
                }
            }
            globals.sort_unstable_by_key(|&(slot, _)| slot);
            fold(&mut globals);
            if let Some(slot) = effective.get_mut(f as usize) {
                *slot = Effective {
                    globals,
                    parameters,
                };
            }
        }
        Ok(effective)
    }

    /// Which functions some call names, by index.
    fn called(&self) -> Result<Vec<bool>, Refusal> {
        let mut called: Vec<bool> = with_capacity(self.functions.len())?;
        called.resize(self.functions.len(), false);
        for &callee in self.facts.iter().flat_map(|facts| &facts.calls) {
            if let Some(slot) = called.get_mut(callee as usize) {
                *slot = true;
            }
        }
        Ok(called)
    }

    /// An entry point's function: no parameters, no result, and called by
    /// nothing.
    fn entry_function(&self, index: usize, called: &[bool]) -> Result<(), Rule> {
        let f = self.functions.get(index).ok_or(Rule::EntryPoint)?;
        let returns = self
            .m
            .result_type(self.instruction(f.at))
            .ok_or(Rule::EntryPoint)
            .and_then(|t| self.type_index(t))?;
        if f.parameters != 0 || !matches!(self.ty(returns).ty, Ty::Void) {
            return Err(Rule::EntryPoint);
        }
        if called.get(index).copied().unwrap_or(true) {
            return Err(Rule::EntryPoint);
        }
        Ok(())
    }

    /// What the call tree from function `index` names and needs, the
    /// functions it reaches marked `stamp` in `stamps`.
    fn reach(
        &self,
        index: usize,
        effective: &[Effective],
        stamps: &mut [u32],
        stamp: u32,
    ) -> Result<Reach, Rule> {
        let mut pending: Vec<u32> = Vec::new();
        push(&mut pending, u32::try_from(index).unwrap_or(NONE)).map_err(|r| r.rule)?;
        let mut globals: Vec<(u32, Access)> = Vec::new();
        let mut needs = Needs::NONE;
        while let Some(f) = pending.pop() {
            if core::mem::replace(stamps.get_mut(f as usize).ok_or(Rule::Call)?, stamp) == stamp {
                continue;
            }
            if let Some(own) = effective.get(f as usize) {
                for &entry in &own.globals {
                    push(&mut globals, entry).map_err(|r| r.rule)?;
                }
            }
            if let Some(facts) = self.facts.get(f as usize) {
                needs = needs.with(facts.needs);
                for &callee in &facts.calls {
                    push(&mut pending, callee).map_err(|r| r.rule)?;
                }
            }
        }
        globals.sort_unstable_by_key(|&(slot, _)| slot);
        fold(&mut globals);
        Ok(Reach { globals, needs })
    }

    /// The global variables an entry point's interface lists, by slot and
    /// ascending: before 1.4 only inputs and outputs, from 1.4 none twice.
    fn interface_list(&self, instruction: &Instruction) -> Result<Vec<u32>, Rule> {
        let operands = self.m.operands(instruction).get(3..).unwrap_or(&[]);
        let mut list: Vec<u32> = with_capacity(operands.len()).map_err(|r| r.rule)?;
        let old = self.enabled.version < (Version { major: 1, minor: 4 });
        for operand in operands {
            let Value::Id(id) = self.m.value(operand) else {
                return Err(Rule::Interface);
            };
            let def = self.def(id)?;
            if def.what != What::Variable || def.scope != NONE {
                return Err(Rule::Interface);
            }
            let class = self.class_of(id)?;
            if old && !matches!(class, StorageClass::INPUT | StorageClass::OUTPUT) {
                return Err(Rule::Interface);
            }
            list.push(
                self.slot(id)
                    .map_or(NONE, |s| u32::try_from(s).unwrap_or(NONE)),
            );
        }
        list.sort_unstable();
        let before = list.len();
        list.dedup();
        if !old && list.len() != before {
            return Err(Rule::Interface);
        }
        Ok(list)
    }

    /// The storage class of the variable `id`.
    pub(crate) fn class_of(&self, id: Id) -> Result<StorageClass, Rule> {
        match self.ty(self.value_type(id)?).ty {
            Ty::Pointer { class, .. } => Ok(class),
            _ => Err(Rule::WrongKind(id)),
        }
    }

    /// Every global the call tree names is listed: from 1.4 every one,
    /// before it every input and output.
    fn complete(&self, reach: &Reach, interface: &[u32]) -> Result<(), Rule> {
        let old = self.enabled.version < (Version { major: 1, minor: 4 });
        for &(slot, _) in &reach.globals {
            let Some(&(id, _)) = self.m.definitions.get(slot as usize) else {
                continue;
            };
            let class = self.class_of(id)?;
            let covered = !old || matches!(class, StorageClass::INPUT | StorageClass::OUTPUT);
            if covered && interface.binary_search(&slot).is_err() {
                return Err(Rule::Interface);
            }
        }
        Ok(())
    }

    /// What the call tree does that only some models may: a fragment's
    /// derivatives and discards, a compute shader's workgroup memory and
    /// barriers, no output from a compute shader.
    fn model_needs(&self, stage: Stage, reach: &Reach) -> Result<(), Rule> {
        if reach.needs.contains(Needs::FRAGMENT) && stage != Stage::Fragment {
            return Err(Rule::ExecutionModel);
        }
        if reach.needs.contains(Needs::COMPUTE) && stage != Stage::Compute {
            return Err(Rule::ExecutionModel);
        }
        for &(slot, _) in &reach.globals {
            let Some(&(id, _)) = self.m.definitions.get(slot as usize) else {
                continue;
            };
            let class = self.class_of(id)?;
            let refused = match class {
                StorageClass::WORKGROUP => stage != Stage::Compute,
                StorageClass::OUTPUT => stage == Stage::Compute,
                StorageClass::INPUT => stage == Stage::Compute && !self.built_in_variable(id),
                _ => false,
            };
            if refused {
                return Err(Rule::ExecutionModel);
            }
        }
        Ok(())
    }

    /// Whether the variable `id` is a built-in or a block of them.
    pub(crate) fn built_in_variable(&self, id: Id) -> bool {
        self.decorated(id, Decoration::BUILT_IN)
            || self
                .value_type(id)
                .ok()
                .and_then(|p| match self.ty(p).ty {
                    Ty::Pointer { pointee, .. } => Some(pointee),
                    _ => None,
                })
                .is_some_and(|t| {
                    self.ty(self.innermost(t))
                        .holds
                        .contains(super::types::Holds::BUILT_IN)
                })
    }

    /// Refuse an execution mode naming anything but an entry point's
    /// function, then order the modes by the function each names.
    fn modes_grouped(&mut self) -> Result<(), Refusal> {
        let mut functions: Vec<Id> = collect(self.entry_points.iter().filter_map(|&at| {
            match self
                .m
                .operands(self.instruction(at))
                .get(1)
                .map(|o| self.m.value(o))
            {
                Some(Value::Id(function)) => Some(function),
                _ => None,
            }
        }))?;
        functions.sort_unstable();
        for &(target, at) in &self.modes {
            if functions.binary_search(&target).is_err() {
                let instruction = self.instruction(at);
                let mode = ExecutionMode(super::allow::word(self.m, instruction, 1));
                return Err(Rule::ExecutionMode(mode).at(instruction));
            }
        }
        self.modes.sort_unstable();
        Ok(())
    }

    /// The execution modes set for `function`, each admitted for `stage`
    /// and set once, those the model requires present; a compute entry
    /// point's workgroup size.
    fn modes_of(
        &self,
        function: Id,
        stage: Stage,
        reach: &Reach,
    ) -> Result<Option<WorkgroupSize>, Rule> {
        let start = self.modes.partition_point(|&(target, _)| target < function);
        let own = self.modes.get(start..).unwrap_or(&[]);
        let own = own
            .get(..own.partition_point(|&(target, _)| target == function))
            .unwrap_or(&[]);
        if own.len() > MAX_MODES {
            return Err(Rule::Limit(Limit::Modes));
        }
        let mut set: Vec<ExecutionMode> = with_capacity(own.len()).map_err(|r| r.rule)?;
        let mut workgroup = None;
        for &(_, at) in own {
            let instruction = self.instruction(at);
            let mode = ExecutionMode(super::allow::word(self.m, instruction, 1));
            if instruction.op() == Op::EXECUTION_MODE_ID {
                return Err(Rule::ExecutionMode(mode));
            }
            let admitted = match stage {
                Stage::Fragment => matches!(
                    mode,
                    ExecutionMode::ORIGIN_UPPER_LEFT
                        | ExecutionMode::EARLY_FRAGMENT_TESTS
                        | ExecutionMode::DEPTH_REPLACING
                        | ExecutionMode::DEPTH_GREATER
                        | ExecutionMode::DEPTH_LESS
                        | ExecutionMode::DEPTH_UNCHANGED
                ),
                Stage::Compute => mode == ExecutionMode::LOCAL_SIZE,
                Stage::Vertex => false,
            };
            if !admitted {
                return Err(Rule::ExecutionMode(mode));
            }
            if mode == ExecutionMode::LOCAL_SIZE {
                let size = [2, 3, 4].map(|n| super::allow::word(self.m, instruction, n));
                if size.contains(&0) {
                    return Err(Rule::ExecutionMode(mode));
                }
                workgroup = Some(WorkgroupSize::Fixed(size));
            }
            push(&mut set, mode).map_err(|r| r.rule)?;
        }
        set.sort_unstable();
        if let Some(pair) = set.windows(2).find(|w| w[0] == w[1]) {
            return Err(Rule::ExecutionMode(pair[0]));
        }
        let has = |mode| set.binary_search(&mode).is_ok();
        match stage {
            Stage::Fragment => {
                if !has(ExecutionMode::ORIGIN_UPPER_LEFT) {
                    return Err(Rule::ExecutionMode(ExecutionMode::ORIGIN_UPPER_LEFT));
                }
                let depth = [
                    ExecutionMode::DEPTH_GREATER,
                    ExecutionMode::DEPTH_LESS,
                    ExecutionMode::DEPTH_UNCHANGED,
                ];
                if depth.into_iter().filter(|&m| has(m)).count() > 1 {
                    return Err(Rule::ExecutionMode(ExecutionMode::DEPTH_UNCHANGED));
                }
                if self.writes_built_in(reach, BuiltIn::FRAG_DEPTH)
                    && !has(ExecutionMode::DEPTH_REPLACING)
                {
                    return Err(Rule::ExecutionMode(ExecutionMode::DEPTH_REPLACING));
                }
                Ok(None)
            }
            Stage::Compute => {
                if let Some(constant) = self.workgroup_constant() {
                    return Ok(Some(WorkgroupSize::Constant(constant)));
                }
                workgroup
                    .map(Some)
                    .ok_or(Rule::ExecutionMode(ExecutionMode::LOCAL_SIZE))
            }
            Stage::Vertex => Ok(None),
        }
    }

    /// The constant decorated `WorkgroupSize`.
    pub(crate) const fn workgroup_constant(&self) -> Option<Id> {
        self.workgroup
    }

    /// The first constant the decorations name `WorkgroupSize`; a second is
    /// refused with the decorations.
    fn declared_workgroup(&self) -> Option<Id> {
        self.decorations
            .iter()
            .find(|d| {
                d.decoration == Decoration::BUILT_IN
                    && d.member == NONE
                    && BuiltIn(self.parameter(d, 0)) == BuiltIn::WORKGROUP_SIZE
            })
            .map(|d| d.target)
    }

    /// Whether the call tree writes the output built-in `built_in`.
    fn writes_built_in(&self, reach: &Reach, built_in: BuiltIn) -> bool {
        reach.globals.iter().any(|&(slot, access)| {
            access.contains(Access::WRITE)
                && self
                    .m
                    .definitions
                    .get(slot as usize)
                    .is_some_and(|&(id, _)| {
                        self.class_of(id) == Ok(StorageClass::OUTPUT)
                            && (self.decoration_value_of(id, NONE, Decoration::BUILT_IN)
                                == Some(built_in.0)
                                || self.struct_built_in(id, built_in))
                    })
        })
    }

    /// Whether the variable `id` holds a block with a member that is
    /// `built_in`.
    fn struct_built_in(&self, id: Id, built_in: BuiltIn) -> bool {
        let Some(block) = self
            .value_type(id)
            .ok()
            .and_then(|p| match self.ty(p).ty {
                Ty::Pointer { pointee, .. } => Some(self.innermost(pointee)),
                _ => None,
            })
            .filter(|&t| self.ty(t).holds.contains(super::types::Holds::BUILT_IN))
            .and_then(|t| self.ty(t).id)
        else {
            return false;
        };
        self.decorations_of(block).iter().any(|d| {
            d.decoration == Decoration::BUILT_IN
                && d.member != NONE
                && self.parameter(d, 0) == built_in.0
        })
    }

    /// Whether `ty` is a three-component 32-bit integer vector.
    pub(crate) fn uvec3_like(&self, ty: u32) -> bool {
        matches!(
            self.ty(ty).ty,
            Ty::Vector {
                scalar: Scalar::Int { width: 32, .. },
                count: 3
            }
        )
    }
}
