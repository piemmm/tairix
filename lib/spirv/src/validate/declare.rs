//! Decorations, and the module-scope declarations: constants, global
//! variables and undefined values, each checked as it is met.
//!
//! Every operand of a module-scope declaration is defined before it, so one
//! pass in module order suffices; decorations come earlier still and are
//! collected first, which is what lets a type know its layout as it is
//! declared.

use super::allow::{class, word, Class};
use super::body::Args;
use super::types::{Holds, Scalar, Ty};
use super::{push, Refusal, Rule, Validator, What, NONE};
use crate::enumerant::{BuiltIn, Capability, Decoration, FPRoundingMode, Scope, StorageClass};
use crate::{Id, Instruction, Op, Value, Version};

/// The decoration marking a memory interface block.
pub(crate) const BLOCK: Decoration = Decoration::BLOCK;

/// One decoration: its target, the member it decorates or [`NONE`], and
/// where its parameters start.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Deco {
    pub(crate) target: Id,
    pub(crate) member: u32,
    pub(crate) decoration: Decoration,
    /// The instruction's index.
    pub(crate) at: u32,
    /// The operand index of its first parameter, or [`NONE`].
    pub(crate) param: u32,
}

impl Deco {
    const fn key(&self) -> (Id, u32, u32) {
        (self.target, self.member, self.decoration.0)
    }
}

impl Validator<'_> {
    /// Collect the decorations, then check every declaration in order.
    pub(crate) fn declare(&mut self) -> Result<(), Refusal> {
        self.collect_decorations()?;
        for index in self.declarations.clone() {
            let instruction = self.instruction(index);
            self.earlier_operands(index, instruction)?;
            match class(instruction.op()) {
                Some(Class::Type) => self.declare_type(index, instruction)?,
                Some(Class::Constant) => self.declare_constant(instruction)?,
                Some(Class::Variable) => self.declare_global(instruction)?,
                Some(Class::Undef) => self.declare_undef(instruction)?,
                Some(Class::Line) => self.line(instruction)?,
                _ => return Err(Rule::Layout.at(instruction)),
            }
        }
        self.duplicate_types()?;
        self.debug_references()?;
        self.assign_local_types()
    }

    /// Refuse a module-scope declaration naming an id defined after it.
    fn earlier_operands(&self, index: u32, instruction: &Instruction) -> Result<(), Refusal> {
        let result = self.m.result(instruction);
        for operand in self.m.operands(instruction) {
            if let Value::Id(id) = self.m.value(operand) {
                if Some(id) == result {
                    continue;
                }
                let defined = self.defining_index(id).map_err(|r| r.at(instruction))?;
                if defined >= index {
                    return Err(Rule::ForwardReference(id).at(instruction));
                }
            }
        }
        Ok(())
    }

    /// Give each function-local id its type, so a use met before its
    /// definition — an `OpPhi` operand — can be typed.
    fn assign_local_types(&mut self) -> Result<(), Refusal> {
        for slot in 0..self.defs.len() {
            let Some(def) = self.defs.get(slot).copied() else {
                continue;
            };
            if def.scope == NONE || def.ty != NONE {
                continue;
            }
            let Some(&(_, index)) = self.m.definitions.get(slot) else {
                continue;
            };
            let instruction = self.instruction(index);
            let Some(ty) = self.m.result_type(instruction) else {
                continue;
            };
            let ty = self.type_index(ty).map_err(|r| r.at(instruction))?;
            if let Some(def) = self.defs.get_mut(slot) {
                def.ty = ty;
            }
        }
        Ok(())
    }

    fn set_type(&mut self, id: Id, ty: u32) {
        if let Some(slot) = self.slot(id) {
            if let Some(def) = self.defs.get_mut(slot) {
                def.ty = ty;
            }
        }
    }

    /// Collect, sort and sanity-check the annotation section.
    fn collect_decorations(&mut self) -> Result<(), Refusal> {
        let m = self.m;
        for index in self.annotations.clone() {
            let instruction = self.instruction(index);
            let operands = m.operands(instruction);
            let id = |position: usize| match operands.get(position).map(|o| m.value(o)) {
                Some(Value::Id(id)) => Ok(id),
                _ => Err(Rule::Layout.at(instruction)),
            };
            let (target, member, at) = match instruction.op() {
                Op::DECORATE | Op::DECORATE_ID => (id(0)?, NONE, 1),
                Op::MEMBER_DECORATE => (id(0)?, word(m, instruction, 1), 2),
                _ => return Err(Rule::Layout.at(instruction)),
            };
            if member == NONE && instruction.op() == Op::MEMBER_DECORATE {
                return Err(Rule::DecorationTarget(Decoration(NONE)).at(instruction));
            }
            let decoration = Decoration(word(m, instruction, at));
            let param = if operands.len() > at + 1 {
                u32::try_from(at + 1).unwrap_or(NONE)
            } else {
                NONE
            };
            let deco = Deco {
                target,
                member,
                decoration,
                at: index,
                param,
            };
            self.decoration_value(instruction, &deco)?;
            push(&mut self.decorations, deco)?;
        }
        self.decorations.sort_unstable_by_key(Deco::key);
        self.rounded = self
            .decorations
            .iter()
            .any(|d| d.decoration == Decoration::FP_ROUNDING_MODE);
        for pair in self.decorations.windows(2) {
            if let [a, b] = pair {
                if a.key() == b.key() {
                    return Err(Rule::DuplicateDecoration(b.decoration).at(self.instruction(b.at)));
                }
            }
        }
        Ok(())
    }

    /// Refuse a decoration the environment does not admit, or a value it may
    /// not take, before anything it decorates is known.
    fn decoration_value(&self, instruction: &Instruction, deco: &Deco) -> Result<(), Refusal> {
        let refused = Rule::DecorationTarget(deco.decoration).at(instruction);
        let value = self.parameter(deco, 0);
        let takes_id = deco.decoration == Decoration::UNIFORM_ID;
        if takes_id != (instruction.op() == Op::DECORATE_ID) {
            return Err(refused);
        }
        let ok = match deco.decoration {
            // Deprecated, or not the Vulkan environment's.
            Decoration::BUFFER_BLOCK | Decoration::GLSL_SHARED | Decoration::GLSL_PACKED => false,
            Decoration::BUILT_IN => {
                !matches!(BuiltIn(value), BuiltIn::VERTEX_ID | BuiltIn::INSTANCE_ID)
            }
            Decoration::COMPONENT => value <= 3,
            Decoration::ARRAY_STRIDE | Decoration::MATRIX_STRIDE => value > 0,
            Decoration::FP_ROUNDING_MODE => matches!(
                FPRoundingMode(value),
                FPRoundingMode::RTE | FPRoundingMode::RTZ
            ),
            Decoration::UNIFORM_ID => self.decoration_scope(instruction, deco),
            _ => true,
        };
        if ok {
            Ok(())
        } else {
            Err(refused)
        }
    }

    /// Whether `UniformId`'s scope is a constant before its target naming a
    /// subgroup or workgroup.
    fn decoration_scope(&self, instruction: &Instruction, deco: &Deco) -> bool {
        let Some(Value::Id(scope)) = self
            .m
            .operands(instruction)
            .get(deco.param as usize)
            .map(|o| self.m.value(o))
        else {
            return false;
        };
        let before = match (self.defining_index(scope), self.defining_index(deco.target)) {
            (Ok(scope), Ok(target)) => scope < target,
            _ => false,
        };
        before
            && self.integer_constant(scope).is_some_and(|(value, _)| {
                matches!(
                    Scope(u32::try_from(value).unwrap_or(NONE)),
                    Scope::SUBGROUP | Scope::WORKGROUP
                )
            })
    }

    /// Parameter `n` of `deco`'s literal parameters.
    pub(crate) fn parameter(&self, deco: &Deco, n: u32) -> u32 {
        if deco.param == NONE {
            return 0;
        }
        word(self.m, self.instruction(deco.at), (deco.param + n) as usize)
    }

    /// The decoration `decoration` of `id`'s member `member` ([`NONE`] for
    /// `id` itself).
    pub(crate) fn decoration_of(
        &self,
        id: Id,
        member: u32,
        decoration: Decoration,
    ) -> Option<&Deco> {
        let key = (id, member, decoration.0);
        self.decorations
            .binary_search_by_key(&key, Deco::key)
            .ok()
            .and_then(|at| self.decorations.get(at))
    }

    pub(crate) fn decorated(&self, id: Id, decoration: Decoration) -> bool {
        self.decoration_of(id, NONE, decoration).is_some()
    }

    /// The literal `decoration` of `id` carries, if it is decorated so.
    pub(crate) fn decoration_value_of(
        &self,
        id: Id,
        member: u32,
        decoration: Decoration,
    ) -> Option<u32> {
        self.decoration_of(id, member, decoration)
            .map(|d| self.parameter(d, 0))
    }

    /// Every decoration of `id` and its members.
    pub(crate) fn decorations_of(&self, id: Id) -> &[Deco] {
        let start = self.decorations.partition_point(|d| d.target < id);
        let end = self.decorations.partition_point(|d| d.target <= id);
        self.decorations.get(start..end).unwrap_or(&[])
    }

    /// Whether `id`, a type, carries explicit layout itself: an array stride,
    /// or a member offset or matrix stride.
    pub(crate) fn decorated_layout(&self, id: Id) -> Holds {
        let layout = self.decorations_of(id).iter().any(|d| {
            matches!(
                d.decoration,
                Decoration::ARRAY_STRIDE | Decoration::OFFSET | Decoration::MATRIX_STRIDE
            )
        });
        if layout {
            Holds::LAYOUT
        } else {
            Holds::NONE
        }
    }

    /// Each debug instruction's references: a name to a defined id, a
    /// member name to a member its structure has, a source to its file.
    fn debug_references(&self) -> Result<(), Refusal> {
        let end = self.annotations.start.min(self.declarations.start) as usize;
        let debug = self.m.instructions().get(..end).unwrap_or(&[]);
        for instruction in debug {
            let operands = self.m.operands(instruction);
            let first = operands.first().map(|o| self.m.value(o));
            match (instruction.op(), first) {
                (Op::NAME, Some(Value::Id(target))) => {
                    self.def(target).map_err(|r| r.at(instruction))?;
                }
                (Op::MEMBER_NAME, Some(Value::Id(target))) => {
                    let ty = self.type_index(target).map_err(|r| r.at(instruction))?;
                    let member = word(self.m, instruction, 1);
                    if !matches!(self.ty(ty).ty, Ty::Struct { count, .. } if member < count) {
                        return Err(Rule::WrongKind(target).at(instruction));
                    }
                }
                (Op::SOURCE, _) => {
                    if let Some(Value::Id(file)) = operands.get(2).map(|o| self.m.value(o)) {
                        self.string(file).map_err(|r| r.at(instruction))?;
                    }
                }
                (Op::NAME | Op::MEMBER_NAME, _) => return Err(Rule::Layout.at(instruction)),
                _ => {}
            }
        }
        Ok(())
    }

    /// Refuse `id` unless it names an `OpString`.
    fn string(&self, id: Id) -> Result<(), Rule> {
        if self.def(id)?.what == What::String {
            Ok(())
        } else {
            Err(Rule::WrongKind(id))
        }
    }

    /// `OpLine`'s file must be an `OpString`.
    pub(crate) fn line(&self, instruction: &Instruction) -> Result<(), Refusal> {
        if instruction.op() == Op::LINE {
            if let Some(Value::Id(file)) = self
                .m
                .operands(instruction)
                .first()
                .map(|o| self.m.value(o))
            {
                self.string(file).map_err(|r| r.at(instruction))?;
            }
        }
        Ok(())
    }

    /// Whether `ty` holds no 16-bit type the module may not compute with.
    pub(crate) fn narrow_computable(&self, ty: u32) -> bool {
        let holds = self.ty(ty).holds;
        (!holds.contains(Holds::INT16) || self.enabled.has(Capability::INT16))
            && (!holds.contains(Holds::FLOAT16) || self.enabled.has(Capability::FLOAT16))
    }

    /// Whether 16-bit data may be held in `class` memory: a buffer, push
    /// constant or interface by its storage capability, other memory by the
    /// arithmetic capability for each width it holds.
    pub(crate) fn narrow_storable(&self, holds: Holds, class: StorageClass) -> bool {
        if !holds.any(Holds::NARROW) {
            return true;
        }
        let has = |c| self.enabled.has(c);
        match class {
            StorageClass::STORAGE_BUFFER => has(Capability::STORAGE_BUFFER16_BIT_ACCESS),
            StorageClass::UNIFORM => has(Capability::UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS),
            StorageClass::PUSH_CONSTANT => has(Capability::STORAGE_PUSH_CONSTANT16),
            StorageClass::INPUT | StorageClass::OUTPUT => has(Capability::STORAGE_INPUT_OUTPUT16),
            _ => {
                (!holds.contains(Holds::INT16) || has(Capability::INT16))
                    && (!holds.contains(Holds::FLOAT16) || has(Capability::FLOAT16))
            }
        }
    }

    /// A constant or specialization constant, checked and typed.
    fn declare_constant(&mut self, instruction: &Instruction) -> Result<(), Refusal> {
        let m = self.m;
        let at = |rule: Rule| rule.at(instruction);
        let id = m.result(instruction).ok_or(at(Rule::Constant))?;
        let result_type = m.result_type(instruction).ok_or(at(Rule::Constant))?;
        let ty = self.type_index(result_type).map_err(at)?;
        let info = *self.ty(ty);
        let op = instruction.op();
        let spec = super::scan::is_spec(op);
        if !self.narrow_computable(ty) {
            return Err(at(Rule::Contents));
        }
        let ok = match op {
            Op::CONSTANT_TRUE
            | Op::CONSTANT_FALSE
            | Op::SPEC_CONSTANT_TRUE
            | Op::SPEC_CONSTANT_FALSE => {
                matches!(info.ty, Ty::Scalar(Scalar::Bool))
            }
            Op::CONSTANT | Op::SPEC_CONSTANT => match info.ty {
                Ty::Scalar(scalar @ (Scalar::Int { .. } | Scalar::Float { .. })) => {
                    self.literal_fits(instruction, scalar)
                }
                _ => false,
            },
            Op::CONSTANT_COMPOSITE | Op::SPEC_CONSTANT_COMPOSITE => {
                // A specialization composite is a vector; a constant one any
                // composite of fixed size.
                let shape = if spec {
                    matches!(info.ty, Ty::Vector { .. })
                } else {
                    matches!(
                        info.ty,
                        Ty::Vector { .. }
                            | Ty::Matrix { .. }
                            | Ty::Array { .. }
                            | Ty::Struct { .. }
                    ) && !info.holds.any(Holds::RUNTIME_ARRAY.with(Holds::OPAQUE))
                };
                shape && self.constituents_match(instruction, ty, spec).map_err(at)?
            }
            Op::CONSTANT_NULL => {
                matches!(
                    info.ty,
                    Ty::Scalar(_)
                        | Ty::Vector { .. }
                        | Ty::Matrix { .. }
                        | Ty::Array { .. }
                        | Ty::Struct { .. }
                ) && !info.holds.any(
                    Holds::RUNTIME_ARRAY
                        .with(Holds::OPAQUE)
                        .with(Holds::POINTER),
                )
            }
            Op::SPEC_CONSTANT_OP => {
                self.spec_constant_op(instruction, ty).map_err(at)?;
                true
            }
            _ => false,
        };
        if !ok {
            return Err(at(Rule::Constant));
        }
        self.set_type(id, ty);
        Ok(())
    }

    /// Whether a constant's literal holds a value of `scalar`: a type
    /// narrower than its word zero-extended, or sign-extended if signed.
    fn literal_fits(&self, instruction: &Instruction, scalar: Scalar) -> bool {
        let Some(Value::Number(value)) =
            self.m.operands(instruction).get(2).map(|o| self.m.value(o))
        else {
            return false;
        };
        match scalar {
            Scalar::Int {
                width: 16,
                signed: true,
            } => {
                let low = (value & 0xffff) as u16;
                value == u64::from(i32::from(low.cast_signed()).cast_unsigned())
            }
            Scalar::Int { width: 16, .. } | Scalar::Float { width: 16 } => value <= 0xffff,
            _ => true,
        }
    }

    /// Whether a composite constant's constituents are, in order, constants
    /// of its constituent types: non-specialization ones for a constant.
    fn constituents_match(
        &self,
        instruction: &Instruction,
        ty: u32,
        spec: bool,
    ) -> Result<bool, Rule> {
        let operands = self.m.operands(instruction).get(2..).unwrap_or(&[]);
        let Some(count) = self.constituents(ty) else {
            return Ok(false);
        };
        if operands.len() != count as usize {
            return Ok(false);
        }
        for (index, operand) in operands.iter().enumerate() {
            let Value::Id(constituent) = self.m.value(operand) else {
                return Ok(false);
            };
            let what = self.def(constituent)?.what;
            let constant = matches!(what, What::Constant | What::Undef)
                || (spec && what == What::SpecConstant);
            let expected = self.constituent(ty, u32::try_from(index).unwrap_or(NONE));
            if !constant || expected != Some(self.value_type(constituent)?) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// An `OpSpecConstantOp`: an operation the Shader capability admits over
    /// constants, typed as that instruction would be.
    fn spec_constant_op(&self, instruction: &Instruction, ty: u32) -> Result<(), Rule> {
        let operands = self.m.operands(instruction);
        let code = match operands.get(2).map(|o| self.m.value(o)) {
            Some(Value::Word(code)) => u16::try_from(code).map_err(|_| Rule::SpecConstantOp)?,
            _ => return Err(Rule::SpecConstantOp),
        };
        let op = crate::Op(code);
        let admitted = match op {
            Op::U_CONVERT => self.enabled.version >= Version { major: 1, minor: 4 },
            Op::S_CONVERT
            | Op::F_CONVERT
            | Op::S_NEGATE
            | Op::NOT
            | Op::I_ADD
            | Op::I_SUB
            | Op::I_MUL
            | Op::U_DIV
            | Op::S_DIV
            | Op::U_MOD
            | Op::S_REM
            | Op::S_MOD
            | Op::SHIFT_RIGHT_LOGICAL
            | Op::SHIFT_RIGHT_ARITHMETIC
            | Op::SHIFT_LEFT_LOGICAL
            | Op::BITWISE_OR
            | Op::BITWISE_XOR
            | Op::BITWISE_AND
            | Op::VECTOR_SHUFFLE
            | Op::COMPOSITE_EXTRACT
            | Op::COMPOSITE_INSERT
            | Op::LOGICAL_OR
            | Op::LOGICAL_AND
            | Op::LOGICAL_NOT
            | Op::LOGICAL_EQUAL
            | Op::LOGICAL_NOT_EQUAL
            | Op::SELECT
            | Op::I_EQUAL
            | Op::I_NOT_EQUAL
            | Op::U_LESS_THAN
            | Op::S_LESS_THAN
            | Op::U_GREATER_THAN
            | Op::S_GREATER_THAN
            | Op::U_LESS_THAN_EQUAL
            | Op::S_LESS_THAN_EQUAL
            | Op::U_GREATER_THAN_EQUAL
            | Op::S_GREATER_THAN_EQUAL
            | Op::QUANTIZE_TO_F16 => true,
            _ => false,
        };
        let typing = match class(op) {
            Some(Class::Body(typing)) if admitted => typing,
            _ => return Err(Rule::SpecConstantOp),
        };
        let nested = operands.get(3..).unwrap_or(&[]);
        for operand in nested {
            if let Value::Id(id) = self.m.value(operand) {
                let what = self.def(id)?.what;
                if !matches!(what, What::Constant | What::SpecConstant | What::Undef) {
                    return Err(Rule::SpecConstantOp);
                }
            }
        }
        let args = Args {
            op,
            result_type: ty,
            result: self.m.result(instruction),
            operands: nested,
            offset: 3,
        };
        self.type_operation(typing, &args)
            .map_err(|_| Rule::SpecConstantOp)
    }

    /// A global variable: its storage class, its type's contents and layout,
    /// its resource decorations, and its initializer.
    fn declare_global(&mut self, instruction: &Instruction) -> Result<(), Refusal> {
        let at = |rule: Rule| rule.at(instruction);
        let (id, pointer_type, class, pointee) = self.variable_parts(instruction).map_err(at)?;
        if matches!(class, StorageClass::FUNCTION | StorageClass::IMAGE) {
            return Err(at(Rule::StorageClass(class)));
        }
        self.variable_contents(class, pointee).map_err(at)?;
        if let Some(initializer) = self.initializer(instruction) {
            let admitted = matches!(class, StorageClass::OUTPUT | StorageClass::PRIVATE);
            let what = self.def(initializer).map_err(at)?.what;
            if !admitted
                || !matches!(what, What::Constant | What::SpecConstant)
                || self.value_type(initializer).map_err(at)? != pointee
            {
                return Err(at(Rule::Initializer));
            }
        }
        if matches!(
            class,
            StorageClass::UNIFORM_CONSTANT | StorageClass::UNIFORM | StorageClass::STORAGE_BUFFER
        ) && !(self.decorated(id, Decoration::DESCRIPTOR_SET)
            && self.decorated(id, Decoration::BINDING))
        {
            return Err(at(Rule::Resource));
        }
        self.set_type(id, pointer_type);
        let root = self.variable_root(id, class);
        self.set_root(id, root);
        Ok(())
    }

    /// A variable's id, pointer type, storage class and pointee, checked to
    /// agree with each other.
    pub(crate) fn variable_parts(
        &self,
        instruction: &Instruction,
    ) -> Result<(Id, u32, StorageClass, u32), Rule> {
        let id = self.m.result(instruction).ok_or(Rule::VariableType)?;
        let pointer = self
            .m
            .result_type(instruction)
            .ok_or(Rule::VariableType)
            .and_then(|t| self.type_index(t))?;
        let class = StorageClass(word(self.m, instruction, 2));
        let Ty::Pointer {
            class: declared,
            pointee,
        } = self.ty(pointer).ty
        else {
            return Err(Rule::VariableType);
        };
        if declared != class {
            return Err(Rule::StorageClass(class));
        }
        Ok((id, pointer, class, pointee))
    }

    /// The initializer `instruction`, an `OpVariable`, names, if any.
    pub(crate) fn initializer(&self, instruction: &Instruction) -> Option<Id> {
        match self.m.operands(instruction).get(3).map(|o| self.m.value(o)) {
            Some(Value::Id(id)) => Some(id),
            _ => None,
        }
    }

    /// Refuse a variable of `class` whose pointee its class does not hold.
    pub(crate) fn variable_contents(
        &mut self,
        class: StorageClass,
        pointee: u32,
    ) -> Result<(), Rule> {
        let info = *self.ty(pointee);
        let holds = info.holds;
        if holds.contains(Holds::POINTER) || matches!(info.ty, Ty::Void | Ty::Function { .. }) {
            return Err(Rule::LogicalPointer);
        }
        if !self.narrow_storable(holds, class) {
            return Err(Rule::Contents);
        }
        let layout_allowed = matches!(
            class,
            StorageClass::UNIFORM | StorageClass::STORAGE_BUFFER | StorageClass::PUSH_CONSTANT
        ) || (matches!(class, StorageClass::PRIVATE | StorageClass::FUNCTION)
            && self.enabled.version <= Version { major: 1, minor: 4 });
        if holds.contains(Holds::LAYOUT) && !layout_allowed {
            return Err(Rule::ExplicitLayout);
        }
        if holds.contains(Holds::QUALIFIED) && class != StorageClass::STORAGE_BUFFER {
            return Err(Rule::VariableType);
        }
        match class {
            StorageClass::UNIFORM_CONSTANT => {
                let element = match info.ty {
                    Ty::Array { element, .. } => element,
                    _ => pointee,
                };
                if !matches!(
                    self.ty(element).ty,
                    Ty::Image(_) | Ty::Sampler | Ty::SampledImage { .. }
                ) {
                    return Err(Rule::VariableType);
                }
            }
            StorageClass::UNIFORM | StorageClass::STORAGE_BUFFER | StorageClass::PUSH_CONSTANT => {
                let block = match info.ty {
                    Ty::Array { element, .. } if class != StorageClass::PUSH_CONSTANT => {
                        if self
                            .ty(pointee)
                            .id
                            .is_some_and(|t| self.decorated(t, Decoration::ARRAY_STRIDE))
                        {
                            return Err(Rule::ExplicitLayout);
                        }
                        element
                    }
                    _ => pointee,
                };
                let block_info = *self.ty(block);
                let is_block = matches!(block_info.ty, Ty::Struct { .. })
                    && block_info.id.is_some_and(|t| self.decorated(t, BLOCK));
                if !is_block || holds.contains(Holds::BOOL) {
                    return Err(Rule::VariableType);
                }
                let runtime = block_info.holds.contains(Holds::RUNTIME_ARRAY);
                if runtime && class != StorageClass::STORAGE_BUFFER {
                    return Err(Rule::RuntimeArray);
                }
                self.buffer_layout(block, class)?;
            }
            StorageClass::INPUT | StorageClass::OUTPUT => {
                if holds.any(Holds::OPAQUE.with(Holds::RUNTIME_ARRAY)) {
                    return Err(Rule::VariableType);
                }
            }
            StorageClass::WORKGROUP | StorageClass::PRIVATE | StorageClass::FUNCTION => {
                if holds.any(Holds::OPAQUE.with(Holds::RUNTIME_ARRAY).with(Holds::BLOCK)) {
                    return Err(Rule::VariableType);
                }
            }
            _ => return Err(Rule::StorageClass(class)),
        }
        Ok(())
    }

    /// A module-scope `OpUndef`: of any type a value may have but a pointer,
    /// an opaque type, or one of no fixed size.
    fn declare_undef(&mut self, instruction: &Instruction) -> Result<(), Refusal> {
        let ty = self
            .undef_type(instruction)
            .map_err(|r| r.at(instruction))?;
        if let Some(id) = self.m.result(instruction) {
            self.set_type(id, ty);
        }
        Ok(())
    }

    pub(crate) fn undef_type(&self, instruction: &Instruction) -> Result<u32, Rule> {
        let ty = self
            .m
            .result_type(instruction)
            .ok_or(Rule::ResultType)
            .and_then(|t| self.type_index(t))?;
        let info = self.ty(ty);
        if matches!(info.ty, Ty::Void | Ty::Function { .. } | Ty::Pointer { .. })
            || info.holds.any(
                Holds::OPAQUE
                    .with(Holds::RUNTIME_ARRAY)
                    .with(Holds::POINTER),
            )
            || !self.narrow_computable(ty)
        {
            return Err(Rule::ResultType);
        }
        Ok(ty)
    }
}
