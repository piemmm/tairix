//! Entry points: each a wrapper the stage enters — its inputs read from the
//! stage's variables, its workgroup memory zeroed, the WGSL function called,
//! a discarded invocation ended, and its result written to the stage's
//! variables — with the execution modes its stage and attributes state.

use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, MemorySemantics, Scope,
    StorageClass,
};
use tairix_spirv::{Arg, Id, Op, MAX_BINDINGS, MAX_CALL_DEPTH, MAX_ENTRY_POINTS, MAX_LOCATIONS};

use super::function::Writer;
use super::{built, Emitter, Flavour, Index, Lower, Place, Root};
use crate::check::decl::{
    BuiltinValue, DeclInfo, Depth, EntryStage, InterpolationKind, Io, Sampling,
};
use crate::diag::{error, push, reserve, Code, Outcome, Span, Stop};
use crate::syntax::{DeclKind, StmtKind};
use crate::types::{Count, Scalar, Space, Ty, TyId};

/// A stage variable: its id, the type of the value it carries, and whether
/// it is the one-element array SPIR-V's sample mask is.
#[derive(Copy, Clone, Debug)]
struct StageVar {
    id: Id,
    ty: TyId,
    mask_array: bool,
}

/// How the wrapper makes one parameter's value.
#[derive(Debug)]
enum Input {
    Read(StageVar),
    /// A structure, from its members' inputs.
    Members(TyId, Vec<Input>),
    /// A built-in value WGSL has and SPIR-V does not, computed from ones it
    /// has.
    Linear(BuiltinValue),
}

/// What a fragment shader's result does to its fragment's depth: keeps it,
/// or replaces it, in a direction where `fragment_depth` states one.
#[derive(Copy, Clone, Debug)]
enum DepthWrite {
    Kept,
    Replaced(Option<Depth>),
}

/// Where the wrapper writes the WGSL function's result.
#[derive(Debug)]
enum Output {
    None,
    Write(StageVar),
    /// A structure's members, each to its own variable.
    Members(TyId, Vec<StageVar>),
}

/// One entry point's stage variables: each listed in its interface, a
/// built-in's declared once in each direction however often it is used.
#[derive(Default)]
struct Interface {
    variables: Vec<Id>,
    builtins: Vec<(BuiltIn, StorageClass, StageVar)>,
}

impl Emitter<'_, '_> {
    /// Every entry point, each within the bounds its module is validated
    /// against; the discard flag made first where one discards, so every
    /// storage write is masked by it.
    pub(crate) fn entry_points(&mut self) -> Outcome<()> {
        if self.c.entries.is_empty() {
            return Err(error(Code::Unsupported, Span::default(), |m| {
                m.write_str(
                    "the module declares no entry point, so it holds nothing a pipeline can run",
                )
            }));
        }
        if let Some(entry) = self.c.entries.get(MAX_ENTRY_POINTS) {
            let span = self.c.decl_name(entry.decl);
            return Err(error(Code::ModuleTooLarge, span, |m| {
                write!(
                    m,
                    "the module declares more than {MAX_ENTRY_POINTS} entry points"
                )
            }));
        }
        self.bound_call_depth()?;
        if self.discards()? {
            self.lower().discard_flag()?;
        }
        for index in 0..self.c.entries.len() {
            self.entry_point(index)?;
        }
        Ok(())
    }

    /// Refuse calls nested past the validator's bound, the wrapper's own
    /// call among them: each function's depth worked out after its
    /// callees', in the order the call graph gives.
    fn bound_call_depth(&mut self) -> Outcome<()> {
        let count = self.c.tree.decls.len();
        let mut position: Vec<u32> = Vec::new();
        reserve(&mut position, count)?;
        position.resize(count, 0);
        for (at, &function) in self.c.fn_order.iter().enumerate() {
            if let Some(slot) = position.get_mut(function as usize) {
                *slot = u32::try_from(at).map_err(|_| Stop::OutOfMemory)?;
            }
        }
        let mut sites: Vec<(u32, u32, u32)> = Vec::new();
        reserve(&mut sites, self.c.call_sites.len())?;
        for site in &self.c.call_sites {
            let at = position
                .get(site.caller as usize)
                .copied()
                .ok_or(Stop::Internal)?;
            sites.push((at, site.caller, site.callee));
        }
        sites.sort_unstable();
        let mut depth: Vec<usize> = Vec::new();
        reserve(&mut depth, count)?;
        depth.resize(count, 1);
        for &(_, caller, callee) in &sites {
            let below = depth.get(callee as usize).copied().ok_or(Stop::Internal)?;
            let slot = depth.get_mut(caller as usize).ok_or(Stop::Internal)?;
            *slot = (*slot).max(below + 1);
        }
        for entry in &self.c.entries {
            if depth.get(entry.decl as usize).copied().unwrap_or(1) + 1 > MAX_CALL_DEPTH {
                let span = self.c.decl_name(entry.decl);
                return Err(error(Code::ModuleTooLarge, span, |m| {
                    write!(
                        m,
                        "this entry point's calls nest more than {} deep",
                        MAX_CALL_DEPTH - 1
                    )
                }));
            }
        }
        Ok(())
    }

    /// Whether a fragment entry point reaches a `discard`.
    fn discards(&self) -> Outcome<bool> {
        let tree = self.c.tree;
        for entry in self
            .c
            .entries
            .iter()
            .filter(|e| e.stage == EntryStage::Fragment)
        {
            for &decl in &entry.reached {
                let Some(DeclKind::Fn(function)) = tree.decls.get(decl as usize).map(|d| d.kind)
                else {
                    continue;
                };
                let mut found = false;
                self.c.each_statement(function.body, |_, stmt| {
                    found |= matches!(stmt.kind, StmtKind::Discard);
                    Ok(())
                })?;
                if found {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Entry point `index`: its stage variables, its wrapper, its
    /// declaration and its execution modes.
    fn entry_point(&mut self, index: usize) -> Outcome<()> {
        let entry = self.c.entries.get(index).ok_or(Stop::Internal)?;
        let (decl, stage, workgroup) = (entry.decl, entry.stage, entry.workgroup);
        let Some(&DeclInfo::Function(signature)) = self.c.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        self.bound_bindings(index)?;
        let mut interface = Interface::default();
        let mut inputs = Vec::new();
        for place in signature.params.0..signature.params.1 {
            let param = self
                .c
                .params
                .get(place as usize)
                .copied()
                .ok_or(Stop::Internal)?;
            push(
                &mut inputs,
                self.input(&mut interface, (param.io, param.ty, param.span), stage)?,
            )?;
        }
        let output = match signature.result {
            None => Output::None,
            Some(ty) => self.output(
                &mut interface,
                (signature.result_io, ty, self.c.decl_name(decl)),
                stage,
            )?,
        };
        let zeroed = if stage == EntryStage::Compute {
            self.workgroup_globals(index)?
        } else {
            Vec::new()
        };
        let local_index = if zeroed.is_empty() {
            None
        } else {
            let u32_ty = self.c.types.scalar(Scalar::U32)?;
            let io = Io::Builtin {
                value: BuiltinValue::LocalInvocationIndex,
                invariant: false,
                depth: None,
            };
            Some(self.stage_variable(
                &mut interface,
                (io, u32_ty, Span::default()),
                StorageClass::INPUT,
                stage,
            )?)
        };
        let callee = self.lower().variant((decl, Vec::new()))?;
        let wrapper = self.b.id().map_err(built)?;
        let void = self.b.void().map_err(built)?;
        let Emitter { c, t, b } = self;
        b.define(wrapper, void, &[], |f, _| -> Outcome<()> {
            let mut w = Writer {
                c,
                t,
                f,
                first_local: 0,
                locals: Vec::new(),
                targets: Vec::new(),
            };
            let mut args = Vec::new();
            reserve(&mut args, inputs.len() + 1)?;
            args.push(callee);
            for input in &inputs {
                args.push(w.read_input(input, &interface, workgroup)?);
            }
            if let Some(local_index) = local_index {
                w.zero_workgroup(&zeroed, local_index, workgroup)?;
            }
            let result_ty = match signature.result {
                Some(ty) => w.value_type(ty)?,
                None => void,
            };
            let result = w.f.op(Op::FUNCTION_CALL, result_ty, &args).map_err(built)?;
            if stage == EntryStage::Fragment {
                w.end_if_discarded()?;
            }
            w.write_output(&output, result)?;
            w.terminate(Op::RETURN, &[])
        })?;
        let name = self.c.spell(self.c.decl_name(decl));
        let model = match stage {
            EntryStage::Vertex => ExecutionModel::VERTEX,
            EntryStage::Fragment => ExecutionModel::FRAGMENT,
            EntryStage::Compute => ExecutionModel::GL_COMPUTE,
        };
        self.b
            .entry_point(model, wrapper, name, &interface.variables)
            .map_err(built)?;
        self.execution_modes(wrapper, (stage, workgroup), (signature.result_io, &output))
    }

    /// The execution modes of the entry point `wrapper` of `stage`: a
    /// fragment shader's origin and the depth its result, written as
    /// `result_io` states, replaces; a compute shader's workgroup.
    fn execution_modes(
        &mut self,
        wrapper: Id,
        (stage, workgroup): (EntryStage, [u32; 3]),
        (result_io, output): (Io, &Output),
    ) -> Outcome<()> {
        match stage {
            EntryStage::Vertex => {}
            EntryStage::Fragment => {
                self.b
                    .execution_mode(wrapper, ExecutionMode::ORIGIN_UPPER_LEFT, &[])
                    .map_err(built)?;
                if let DepthWrite::Replaced(depth) = self.depth_write(result_io, output)? {
                    self.b
                        .execution_mode(wrapper, ExecutionMode::DEPTH_REPLACING, &[])
                        .map_err(built)?;
                    if let Some(depth) = depth {
                        let mode = match depth {
                            Depth::Greater => ExecutionMode::DEPTH_GREATER,
                            Depth::Less => ExecutionMode::DEPTH_LESS,
                        };
                        self.b.execution_mode(wrapper, mode, &[]).map_err(built)?;
                    }
                }
            }
            EntryStage::Compute => {
                self.b
                    .execution_mode(wrapper, ExecutionMode::LOCAL_SIZE, &workgroup)
                    .map_err(built)?;
            }
        }
        Ok(())
    }

    /// Refuse an entry point using more resource bindings than the
    /// validator admits.
    fn bound_bindings(&self, index: usize) -> Outcome<()> {
        let entry = self.c.entries.get(index).ok_or(Stop::Internal)?;
        let bound = entry
            .reached
            .iter()
            .filter(|&&decl| {
                matches!(self.c.decls.get(decl as usize), Some(DeclInfo::Global(global)) if global.binding.is_some())
            })
            .count();
        if bound > MAX_BINDINGS {
            let span = self.c.decl_name(entry.decl);
            return Err(error(Code::ModuleTooLarge, span, |m| {
                write!(
                    m,
                    "this entry point uses more than {MAX_BINDINGS} resource bindings"
                )
            }));
        }
        Ok(())
    }

    /// The workgroup variables entry point `index` reaches.
    fn workgroup_globals(&self, index: usize) -> Outcome<Vec<u32>> {
        let entry = self.c.entries.get(index).ok_or(Stop::Internal)?;
        let mut out = Vec::new();
        for &decl in &entry.reached {
            if let Some(DeclInfo::Global(global)) = self.c.decls.get(decl as usize) {
                if global.space == Space::Workgroup {
                    push(&mut out, decl)?;
                }
            }
        }
        Ok(out)
    }

    /// What the result, written as `result_io` states, does to the depth.
    fn depth_write(&self, result_io: Io, output: &Output) -> Outcome<DepthWrite> {
        let depth_of = |io: Io| match io {
            Io::Builtin {
                value: BuiltinValue::FragDepth,
                depth,
                ..
            } => Some(DepthWrite::Replaced(depth)),
            _ => None,
        };
        let written = match output {
            Output::None => None,
            Output::Write(_) => depth_of(result_io),
            Output::Members(ty, _) => {
                let Ty::Struct(structure) = self.c.types.get(*ty) else {
                    return Err(Stop::Internal);
                };
                let count = self.c.types.members_of(structure).len();
                (0..count).find_map(|at| self.member_io(structure, at).and_then(depth_of))
            }
        };
        Ok(written.unwrap_or(DepthWrite::Kept))
    }

    /// The stage-interface attributes of member `at` of `structure`.
    fn member_io(&self, structure: u32, at: usize) -> Option<Io> {
        let first = *self.c.struct_attrs.get(structure as usize)?;
        let attrs = self.c.member_attrs.get(first as usize + at)?;
        Some(attrs.io)
    }

    /// How the wrapper makes a parameter's value: read from its variable,
    /// built from its members', or computed.
    fn input(
        &mut self,
        interface: &mut Interface,
        (io, ty, span): (Io, TyId, Span),
        stage: EntryStage,
    ) -> Outcome<Input> {
        if let Io::Builtin {
            value: value @ (BuiltinValue::GlobalInvocationIndex | BuiltinValue::WorkgroupIndex),
            ..
        } = io
        {
            let vector = self.c.types.intern(Ty::Vector(3, Scalar::U32))?;
            let needed = [
                if value == BuiltinValue::WorkgroupIndex {
                    BuiltinValue::WorkgroupId
                } else {
                    BuiltinValue::GlobalInvocationId
                },
                BuiltinValue::NumWorkgroups,
            ];
            for value in needed {
                let io = Io::Builtin {
                    value,
                    invariant: false,
                    depth: None,
                };
                self.stage_variable(interface, (io, vector, span), StorageClass::INPUT, stage)?;
            }
            return Ok(Input::Linear(value));
        }
        if io != Io::None {
            return self
                .stage_variable(interface, (io, ty, span), StorageClass::INPUT, stage)
                .map(Input::Read);
        }
        let Ty::Struct(structure) = self.c.types.get(ty) else {
            return Err(Stop::Internal);
        };
        let count = self.c.types.members_of(structure).len();
        let mut members = Vec::new();
        reserve(&mut members, count)?;
        for at in 0..count {
            let member = self.c.types.members_of(structure)[at];
            let io = self.member_io(structure, at).ok_or(Stop::Internal)?;
            members.push(self.input(interface, (io, member.ty, member.name), stage)?);
        }
        Ok(Input::Members(ty, members))
    }

    /// Where the wrapper writes the result: its variable, or each member's.
    fn output(
        &mut self,
        interface: &mut Interface,
        (io, ty, span): (Io, TyId, Span),
        stage: EntryStage,
    ) -> Outcome<Output> {
        if io != Io::None {
            return self
                .stage_variable(interface, (io, ty, span), StorageClass::OUTPUT, stage)
                .map(Output::Write);
        }
        let Ty::Struct(structure) = self.c.types.get(ty) else {
            return Err(Stop::Internal);
        };
        let count = self.c.types.members_of(structure).len();
        let mut members = Vec::new();
        reserve(&mut members, count)?;
        for at in 0..count {
            let member = self.c.types.members_of(structure)[at];
            let io = self.member_io(structure, at).ok_or(Stop::Internal)?;
            members.push(self.stage_variable(
                interface,
                (io, member.ty, member.name),
                StorageClass::OUTPUT,
                stage,
            )?);
        }
        Ok(Output::Members(ty, members))
    }

    /// The variable carrying one stage datum, with its built-in or location
    /// and its interpolation; a built-in's made once per entry point.
    fn stage_variable(
        &mut self,
        interface: &mut Interface,
        (io, ty, span): (Io, TyId, Span),
        class: StorageClass,
        stage: EntryStage,
    ) -> Outcome<StageVar> {
        let mut lower = self.lower();
        let value_ty = lower.ty(ty, Flavour::Plain)?;
        if lower.holds_f16(ty) {
            lower
                .b
                .capability(Capability::STORAGE_INPUT_OUTPUT16)
                .map_err(built)?;
        }
        match io {
            Io::Builtin {
                value, invariant, ..
            } => {
                let builtin = spirv_builtin(value, stage).ok_or(Stop::Internal)?;
                if let Some(&(_, _, existing)) = interface
                    .builtins
                    .iter()
                    .find(|&&(b, c, _)| b == builtin && c == class)
                {
                    return Ok(existing);
                }
                let mask_array = value == BuiltinValue::SampleMask;
                let carried = if mask_array {
                    let one = lower.uint(1)?;
                    lower.b.array(value_ty, one).map_err(built)?
                } else {
                    value_ty
                };
                if value == BuiltinValue::SampleIndex {
                    lower
                        .b
                        .capability(Capability::SAMPLE_RATE_SHADING)
                        .map_err(built)?;
                }
                let pointer = lower.pointer(class, carried)?;
                let id = lower.b.variable(pointer, class).map_err(built)?;
                lower
                    .b
                    .decorate(id, Decoration::BUILT_IN, &[builtin.0])
                    .map_err(built)?;
                if invariant {
                    lower
                        .b
                        .decorate(id, Decoration::INVARIANT, &[])
                        .map_err(built)?;
                }
                let var = StageVar { id, ty, mask_array };
                push(&mut interface.builtins, (builtin, class, var))?;
                push(&mut interface.variables, id)?;
                Ok(var)
            }
            Io::Location {
                location,
                interpolation,
                blend_src,
            } => {
                if location >= MAX_LOCATIONS {
                    return Err(error(Code::ModuleTooLarge, span, |m| {
                        write!(m, "locations run from 0 to {}", MAX_LOCATIONS - 1)
                    }));
                }
                let pointer = lower.pointer(class, value_ty)?;
                let id = lower.b.variable(pointer, class).map_err(built)?;
                lower
                    .b
                    .decorate(id, Decoration::LOCATION, &[location])
                    .map_err(built)?;
                if let Some(index) = blend_src {
                    lower
                        .b
                        .decorate(id, Decoration::INDEX, &[index])
                        .map_err(built)?;
                }
                let interpolated = matches!(
                    (stage, class),
                    (EntryStage::Fragment, StorageClass::INPUT)
                        | (EntryStage::Vertex, StorageClass::OUTPUT)
                );
                if interpolated {
                    interpolate(&mut lower, id, interpolation, ty)?;
                }
                push(&mut interface.variables, id)?;
                Ok(StageVar {
                    id,
                    ty,
                    mask_array: false,
                })
            }
            Io::None => Err(Stop::Internal),
        }
    }
}

/// The decorations a datum's interpolation takes: flat for an integer
/// whatever is written, which the stage interface requires.
fn interpolate(
    lower: &mut Lower<'_, '_>,
    id: Id,
    interpolation: Option<crate::check::decl::Interpolation>,
    ty: TyId,
) -> Outcome<()> {
    let integer = lower.c.types.scalar_of(ty).is_some_and(|s| !s.is_float());
    let kind = interpolation.map(|i| i.kind);
    let flat = integer || kind == Some(InterpolationKind::Flat);
    if flat {
        return lower.b.decorate(id, Decoration::FLAT, &[]).map_err(built);
    }
    if kind == Some(InterpolationKind::Linear) {
        lower
            .b
            .decorate(id, Decoration::NO_PERSPECTIVE, &[])
            .map_err(built)?;
    }
    match interpolation.map(|i| i.sampling) {
        Some(Sampling::Centroid) => lower
            .b
            .decorate(id, Decoration::CENTROID, &[])
            .map_err(built),
        Some(Sampling::Sample) => {
            lower
                .b
                .capability(Capability::SAMPLE_RATE_SHADING)
                .map_err(built)?;
            lower.b.decorate(id, Decoration::SAMPLE, &[]).map_err(built)
        }
        _ => Ok(()),
    }
}

/// The SPIR-V built-in a WGSL built-in value is in `stage`; none for the
/// linear indexes, which are computed.
const fn spirv_builtin(value: BuiltinValue, stage: EntryStage) -> Option<BuiltIn> {
    use BuiltinValue as V;
    Some(match value {
        V::VertexIndex => BuiltIn::VERTEX_INDEX,
        V::InstanceIndex => BuiltIn::INSTANCE_INDEX,
        V::Position if matches!(stage, EntryStage::Fragment) => BuiltIn::FRAG_COORD,
        V::Position => BuiltIn::POSITION,
        V::FrontFacing => BuiltIn::FRONT_FACING,
        V::FragDepth => BuiltIn::FRAG_DEPTH,
        V::SampleIndex => BuiltIn::SAMPLE_ID,
        V::SampleMask => BuiltIn::SAMPLE_MASK,
        V::LocalInvocationId => BuiltIn::LOCAL_INVOCATION_ID,
        V::LocalInvocationIndex => BuiltIn::LOCAL_INVOCATION_INDEX,
        V::GlobalInvocationId => BuiltIn::GLOBAL_INVOCATION_ID,
        V::WorkgroupId => BuiltIn::WORKGROUP_ID,
        V::NumWorkgroups => BuiltIn::NUM_WORKGROUPS,
        V::GlobalInvocationIndex | V::WorkgroupIndex => return None,
    })
}

impl Writer<'_, '_, '_> {
    /// The value an input gives.
    fn read_input(
        &mut self,
        input: &Input,
        interface: &Interface,
        workgroup: [u32; 3],
    ) -> Outcome<Id> {
        match input {
            Input::Read(var) => self.read_stage(*var),
            Input::Members(ty, members) => {
                let mut parts = Vec::new();
                reserve(&mut parts, members.len())?;
                for member in members {
                    parts.push(self.read_input(member, interface, workgroup)?);
                }
                let ty = self.value_type(*ty)?;
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, ty, &parts)
                    .map_err(built)
            }
            Input::Linear(value) => self.linear_index(*value, interface, workgroup),
        }
    }

    /// The value one stage variable carries.
    fn read_stage(&mut self, var: StageVar) -> Outcome<Id> {
        let ty = self.lower().ty(var.ty, Flavour::Plain)?;
        let pointer = if var.mask_array {
            let element = self.lower().pointer(StorageClass::INPUT, ty)?;
            let zero = self.lower().uint(0)?;
            self.f
                .op(Op::ACCESS_CHAIN, element, &[var.id, zero])
                .map_err(built)?
        } else {
            var.id
        };
        self.f.op(Op::LOAD, ty, &[pointer]).map_err(built)
    }

    /// `global_invocation_index` or `workgroup_index`: the invocation's or
    /// workgroup's id laid out row by row, plane by plane.
    fn linear_index(
        &mut self,
        value: BuiltinValue,
        interface: &Interface,
        workgroup: [u32; 3],
    ) -> Outcome<Id> {
        let find = |builtin: BuiltIn| {
            interface
                .builtins
                .iter()
                .find(|&&(b, c, _)| b == builtin && c == StorageClass::INPUT)
                .map(|&(_, _, var)| var)
                .ok_or(Stop::Internal)
        };
        let by_workgroup = value == BuiltinValue::WorkgroupIndex;
        let id = find(if by_workgroup {
            BuiltIn::WORKGROUP_ID
        } else {
            BuiltIn::GLOBAL_INVOCATION_ID
        })?;
        let id = self.read_stage(id)?;
        let counts = find(BuiltIn::NUM_WORKGROUPS)?;
        let counts = self.read_stage(counts)?;
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        let lane = |w: &mut Self, vector: Id, at: u32| {
            w.f.op_args(
                Op::COMPOSITE_EXTRACT,
                u32_ty,
                &[Arg::Id(vector), Arg::Word(at)],
            )
            .map_err(built)
        };
        let (x, y, z) = (lane(self, id, 0)?, lane(self, id, 1)?, lane(self, id, 2)?);
        let mut row = lane(self, counts, 0)?;
        let mut plane = lane(self, counts, 1)?;
        if !by_workgroup {
            let (wide, high) = (
                self.lower().uint(workgroup[0])?,
                self.lower().uint(workgroup[1])?,
            );
            row = self.f.op(Op::I_MUL, u32_ty, &[row, wide]).map_err(built)?;
            plane = self
                .f
                .op(Op::I_MUL, u32_ty, &[plane, high])
                .map_err(built)?;
        }
        let area = self.f.op(Op::I_MUL, u32_ty, &[row, plane]).map_err(built)?;
        let down = self.f.op(Op::I_MUL, u32_ty, &[y, row]).map_err(built)?;
        let deep = self.f.op(Op::I_MUL, u32_ty, &[z, area]).map_err(built)?;
        let partial = self.f.op(Op::I_ADD, u32_ty, &[x, down]).map_err(built)?;
        self.f
            .op(Op::I_ADD, u32_ty, &[partial, deep])
            .map_err(built)
    }

    /// Zero every workgroup variable the entry point reaches before any
    /// invocation reads one: an array's elements shared out among the
    /// invocations, anything else written by the first, then a barrier.
    fn zero_workgroup(
        &mut self,
        globals: &[u32],
        local_index: StageVar,
        workgroup: [u32; 3],
    ) -> Outcome<()> {
        let local = self.read_stage(local_index)?;
        let total = workgroup
            .iter()
            .try_fold(1u32, |product, &n| product.checked_mul(n))
            .unwrap_or(u32::MAX);
        let bool_ty = self.lower().scalar(Scalar::Bool)?;
        for &decl in globals {
            let Some(&DeclInfo::Global(global)) = self.c.decls.get(decl as usize) else {
                return Err(Stop::Internal);
            };
            let written = self.lower().global(decl)?;
            let place = Place {
                base: written.id,
                root: Root::Global(decl),
                space: Space::Workgroup,
                wrapped: false,
                indexes: Vec::new(),
                ty: global.store,
            };
            match self.c.types.get(global.store) {
                Ty::Array { element, count } if !matches!(count, Count::Runtime) => {
                    let n = self.c.count_value(count).ok_or(Stop::Internal)?;
                    let zero = self.zero_of(element)?;
                    let limit = self.lower().uint(n)?;
                    if total >= n {
                        let within = self
                            .f
                            .op(Op::U_LESS_THAN, bool_ty, &[local, limit])
                            .map_err(built)?;
                        self.when(within, |w| {
                            w.store(&place.step(Index::Dynamic(local), element)?, zero)
                        })?;
                    } else {
                        let step = self.lower().uint(total)?;
                        self.count((local, limit, step), |w, at| {
                            w.store(&place.step(Index::Dynamic(at), element)?, zero)
                        })?;
                    }
                }
                _ => {
                    let zero = self.zero_of(global.store)?;
                    let first = self.lower().uint(0)?;
                    let leads = self
                        .f
                        .op(Op::I_EQUAL, bool_ty, &[local, first])
                        .map_err(built)?;
                    self.when(leads, |w| w.store(&place, zero))?;
                }
            }
        }
        let workgroup_scope = self.lower().uint(Scope::WORKGROUP.0)?;
        let semantics = self
            .lower()
            .uint(MemorySemantics::ACQUIRE_RELEASE.0 | MemorySemantics::WORKGROUP_MEMORY.0)?;
        self.f
            .emit(
                Op::CONTROL_BARRIER,
                &[workgroup_scope, workgroup_scope, semantics],
            )
            .map_err(built)
    }

    /// The zero value of `ty`, in its values' form.
    fn zero_of(&mut self, ty: TyId) -> Outcome<Id> {
        let spirv = self.value_type(ty)?;
        self.f.module().constant_null(spirv).map_err(built)
    }

    /// End an invocation that discarded where it returns, so it writes no
    /// output.
    fn end_if_discarded(&mut self) -> Outcome<()> {
        let Some(flag) = self.t.discarded else {
            return Ok(());
        };
        self.f
            .module()
            .extension("SPV_KHR_terminate_invocation")
            .map_err(built)?;
        let bool_ty = self.lower().scalar(Scalar::Bool)?;
        let discarded = self.f.op(Op::LOAD, bool_ty, &[flag]).map_err(built)?;
        let (end, go) = (self.label()?, self.label()?);
        self.select_merge(go)?;
        self.terminate(Op::BRANCH_CONDITIONAL, &[discarded, end, go])?;
        self.begin(end)?;
        self.terminate(Op::TERMINATE_INVOCATION, &[])?;
        self.begin(go)
    }

    /// Write the result to its stage variables.
    fn write_output(&mut self, output: &Output, result: Id) -> Outcome<()> {
        match output {
            Output::None => Ok(()),
            Output::Write(var) => self.write_stage(*var, result),
            Output::Members(ty, members) => {
                let flavour = self.lower().value_flavour(*ty)?;
                for (at, var) in (0u32..).zip(members) {
                    let part = self.extract(result, (var.ty, flavour), &[at])?;
                    self.write_stage(*var, part)?;
                }
                Ok(())
            }
        }
    }

    fn write_stage(&mut self, var: StageVar, value: Id) -> Outcome<()> {
        let pointer = if var.mask_array {
            let ty = self.lower().ty(var.ty, Flavour::Plain)?;
            let element = self.lower().pointer(StorageClass::OUTPUT, ty)?;
            let zero = self.lower().uint(0)?;
            self.f
                .op(Op::ACCESS_CHAIN, element, &[var.id, zero])
                .map_err(built)?
        } else {
            var.id
        };
        self.f.emit(Op::STORE, &[pointer, value]).map_err(built)
    }
}
