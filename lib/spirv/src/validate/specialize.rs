//! Specialization (`plans/SHADER.md` SH8): every constant of an admitted
//! module evaluated once the values a pipeline gives its specialization
//! constants are known, with each entry point's final workgroup size.
//!
//! Every operation is total over every bit pattern: a division or remainder by
//! zero is zero, the most negative value divided by minus one is itself, a
//! shift by the width or more is zero, and an undefined shuffle component is
//! zero, as an undefined value is. A composite holds its constituents by
//! reference, so one shared by many is held once. Every constant and
//! constituent evaluation keeps counts against [`MAX_SPECIALIZED`], a null
//! composite's spelled out where an insertion changes one among them, and
//! nothing is listed on the way.

use alloc::vec::Vec;

use tairix_util::mathf::{float_bits, float_of, quantized_to_half};

use super::types::MAX_VECTOR_COMPONENTS;
use super::view::{Scalar, Type, TypeId};
use super::{Admitted, WorkgroupSize, NONE};
use crate::enumerant::Decoration;
use crate::{Id, Instruction, Op, Operand, Value};

/// The most constants and composite constituents specialization holds: what
/// the decoder admits in instructions, four times over.
pub const MAX_SPECIALIZED: usize = 4 * crate::MAX_INSTRUCTIONS;

/// A value a pipeline gives one specialization constant.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpecValue {
    /// The constant's `SpecId`.
    pub id: u32,
    /// Its bits: one or zero for a Boolean, the value zero-extended for a
    /// number.
    pub bits: u64,
}

/// Why specialization was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpecError {
    /// A value for a `SpecId` no specialization constant carries.
    UnknownId(u32),
    /// Two values for one `SpecId`.
    Duplicate(u32),
    /// A value wider than its constant's type.
    Width(u32),
    /// A workgroup size with a component of zero.
    Workgroup,
    /// Past [`MAX_SPECIALIZED`].
    Budget,
    /// The memory evaluation needs could not be had.
    OutOfMemory,
    /// A constant the validator's guarantees say cannot be.
    Malformed,
}

/// An evaluated constant, by its place among a specialization's constants.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConstantId(u32);

/// What an evaluated constant is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Constant<'s> {
    /// A scalar's bits, zero-extended from its width; a Boolean is one or
    /// zero.
    Scalar(u64),
    /// A composite's constituents, in order.
    Composite(&'s [ConstantId]),
    /// Zero, of whatever type it is.
    Null,
}

#[derive(Copy, Clone, Debug)]
enum Entry {
    Scalar(u64),
    Composite { first: u32, count: u32 },
    Null,
}

/// An admitted module's constants, specialized.
#[derive(Debug)]
pub struct Specialized {
    entries: Vec<Entry>,
    parts: Vec<ConstantId>,
    /// Each definition slot's constant, or `NONE`.
    by_slot: Vec<u32>,
    workgroups: Vec<Option<[u32; 3]>>,
}

impl Specialized {
    /// The constant `id` names in `module`, if it names one.
    #[must_use]
    pub fn constant(&self, admitted: &Admitted, id: Id) -> Option<ConstantId> {
        let slot = admitted
            .module()
            .definitions
            .binary_search_by_key(&id, |&(defined, _)| defined)
            .ok()?;
        let index = *self.by_slot.get(slot)?;
        (index != NONE).then_some(ConstantId(index))
    }

    /// What `constant` is.
    #[must_use]
    pub fn value(&self, constant: ConstantId) -> Option<Constant<'_>> {
        Some(match *self.entries.get(constant.0 as usize)? {
            Entry::Scalar(bits) => Constant::Scalar(bits),
            Entry::Composite { first, count } => Constant::Composite(
                self.parts
                    .get(first as usize..(first + count) as usize)
                    .unwrap_or(&[]),
            ),
            Entry::Null => Constant::Null,
        })
    }

    /// The workgroup size of entry point `entry`, by its place in the
    /// module's entry points; `None` for one of another stage.
    #[must_use]
    pub fn workgroup_size(&self, entry: usize) -> Option<[u32; 3]> {
        self.workgroups.get(entry).copied().flatten()
    }
}

/// The low `width` bits.
const fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// `bits`, `width` bits wide, as a signed value.
const fn signed(bits: u64, width: u32) -> i64 {
    if width >= 64 || width == 0 {
        bits.cast_signed()
    } else {
        let shift = 64 - width;
        (bits << shift).cast_signed() >> shift
    }
}

struct Evaluator<'a> {
    admitted: &'a Admitted,
    out: Specialized,
    /// The one zero every null constituent is.
    null: ConstantId,
}

/// Whether `more` constituents and the entry that holds them fit beside the
/// `kept` a specialization holds, within [`MAX_SPECIALIZED`]: compared so that
/// no sum can wrap, as a type's constituent count reaches a 32-bit target's
/// word.
const fn fits(kept: usize, more: usize) -> bool {
    more < MAX_SPECIALIZED.saturating_sub(kept)
}

impl Evaluator<'_> {
    /// The entries and constituents it holds so far.
    const fn kept(&self) -> usize {
        self.out.entries.len() + self.out.parts.len()
    }

    fn push_entry(&mut self, entry: Entry) -> Result<ConstantId, SpecError> {
        if !fits(self.kept(), 0) {
            return Err(SpecError::Budget);
        }
        let index = u32::try_from(self.out.entries.len()).map_err(|_| SpecError::Budget)?;
        self.out
            .entries
            .try_reserve(1)
            .map_err(|_| SpecError::OutOfMemory)?;
        self.out.entries.push(entry);
        Ok(ConstantId(index))
    }

    fn composite(&mut self, parts: &[ConstantId]) -> Result<ConstantId, SpecError> {
        let count = u32::try_from(parts.len()).map_err(|_| SpecError::Budget)?;
        self.composite_of(count, |_, at| {
            parts.get(at as usize).copied().ok_or(SpecError::Malformed)
        })
    }

    /// A composite of `count` constituents, constituent `at` being
    /// `part(self, at)`, written straight into the parts it is kept among.
    fn composite_of(
        &mut self,
        count: u32,
        part: impl Fn(&Self, u32) -> Result<ConstantId, SpecError>,
    ) -> Result<ConstantId, SpecError> {
        let len = count as usize;
        if !fits(self.kept(), len) {
            return Err(SpecError::Budget);
        }
        let first = u32::try_from(self.out.parts.len()).map_err(|_| SpecError::Budget)?;
        self.out
            .parts
            .try_reserve(len)
            .map_err(|_| SpecError::OutOfMemory)?;
        for at in 0..count {
            let constituent = part(self, at)?;
            self.out.parts.push(constituent);
        }
        self.push_entry(Entry::Composite { first, count })
    }

    fn entry(&self, constant: ConstantId) -> Result<Entry, SpecError> {
        self.out
            .entries
            .get(constant.0 as usize)
            .copied()
            .ok_or(SpecError::Malformed)
    }

    /// Constituent `index` of a composite constant, a null one's being null.
    fn constituent(&self, constant: ConstantId, index: u32) -> Result<ConstantId, SpecError> {
        match self.entry(constant)? {
            Entry::Composite { first, count } if index < count => self
                .out
                .parts
                .get((first + index) as usize)
                .copied()
                .ok_or(SpecError::Malformed),
            Entry::Null => Ok(self.null),
            _ => Err(SpecError::Malformed),
        }
    }

    fn scalar_bits(&self, constant: ConstantId) -> Result<u64, SpecError> {
        match self.entry(constant)? {
            Entry::Scalar(bits) => Ok(bits),
            Entry::Null => Ok(0),
            Entry::Composite { .. } => Err(SpecError::Malformed),
        }
    }

    fn constant_of(&self, id: Id) -> Result<ConstantId, SpecError> {
        self.out
            .constant(self.admitted, id)
            .ok_or(SpecError::Malformed)
    }

    fn type_of(&self, instruction: &Instruction) -> Result<Type, SpecError> {
        let ty = self
            .admitted
            .result_type(instruction)
            .ok_or(SpecError::Malformed)?;
        self.admitted.ty(ty).ok_or(SpecError::Malformed)
    }

    fn type_id_of_value(&self, id: Id) -> Result<TypeId, SpecError> {
        let instruction = self
            .admitted
            .module()
            .definition(id)
            .ok_or(SpecError::Malformed)?;
        self.admitted
            .result_type(instruction)
            .ok_or(SpecError::Malformed)
    }

    fn type_of_value(&self, id: Id) -> Result<Type, SpecError> {
        let ty = self.type_id_of_value(id)?;
        self.admitted.ty(ty).ok_or(SpecError::Malformed)
    }

    /// How many constituents a composite of `ty` has, and the type of
    /// constituent `index`: `None` for a vector's, a scalar.
    fn constituent_type(&self, ty: TypeId, index: u32) -> Result<(u32, Option<TypeId>), SpecError> {
        match self.admitted.ty(ty).ok_or(SpecError::Malformed)? {
            Type::Vector { count, .. } => Ok((count, None)),
            Type::Matrix {
                column, columns, ..
            } => Ok((columns, Some(column))),
            Type::Array { element, length } => Ok((length, Some(element))),
            Type::Struct { members } => {
                let member = self
                    .admitted
                    .member_type(ty, index)
                    .ok_or(SpecError::Malformed)?;
                Ok((members, Some(member)))
            }
            _ => Err(SpecError::Malformed),
        }
    }
}

/// The scalar a scalar or vector type holds and its component count.
const fn components(ty: Type) -> Option<(Scalar, u32)> {
    match ty {
        Type::Scalar(scalar) => Some((scalar, 1)),
        Type::Vector { scalar, count } => Some((scalar, count)),
        _ => None,
    }
}

const fn width(scalar: Scalar) -> u32 {
    match scalar {
        Scalar::Bool => 1,
        Scalar::Int { width, .. } | Scalar::Float { width } => width,
    }
}

impl Admitted {
    /// Every constant evaluated, with `values` given the specialization
    /// constants their `SpecId`s name, and each entry point's workgroup size.
    ///
    /// # Errors
    ///
    /// A value for an id no specialization constant carries, two for one id,
    /// one wider than its constant, a workgroup size with a zero component,
    /// evaluation past [`MAX_SPECIALIZED`], and memory the host refuses.
    pub fn specialize(&self, values: &[SpecValue]) -> Result<Specialized, SpecError> {
        let mut given = Vec::new();
        given
            .try_reserve_exact(values.len())
            .map_err(|_| SpecError::OutOfMemory)?;
        given.extend(values.iter().map(|v| (v.id, v.bits)));
        given.sort_unstable_by_key(|&(id, _)| id);
        let repeated = given.windows(2).find_map(|pair| match pair {
            [(a, _), (b, _)] if a == b => Some(*a),
            _ => None,
        });
        if let Some(id) = repeated {
            return Err(SpecError::Duplicate(id));
        }
        let slots = self.module().definitions.len();
        let mut by_slot = Vec::new();
        by_slot
            .try_reserve_exact(slots)
            .map_err(|_| SpecError::OutOfMemory)?;
        by_slot.resize(slots, NONE);
        let mut evaluator = Evaluator {
            admitted: self,
            out: Specialized {
                entries: Vec::new(),
                parts: Vec::new(),
                by_slot,
                workgroups: Vec::new(),
            },
            null: ConstantId(0),
        };
        evaluator.null = evaluator.push_entry(Entry::Null)?;
        let mut carried = Vec::new();
        for instruction in self.module().instructions() {
            if instruction.op() == Op::FUNCTION {
                break;
            }
            let Some((id, constant)) = evaluator.declare(instruction, &given, &mut carried)? else {
                continue;
            };
            let slot = self
                .module()
                .definitions
                .binary_search_by_key(&id, |&(defined, _)| defined)
                .map_err(|_| SpecError::Malformed)?;
            if let Some(entry) = evaluator.out.by_slot.get_mut(slot) {
                *entry = constant.0;
            }
        }
        carried.sort_unstable();
        if let Some(&(unknown, _)) = given
            .iter()
            .find(|(id, _)| carried.binary_search(id).is_err())
        {
            return Err(SpecError::UnknownId(unknown));
        }
        let mut workgroups = Vec::new();
        workgroups
            .try_reserve_exact(self.entry_points().len())
            .map_err(|_| SpecError::OutOfMemory)?;
        for entry in self.entry_points() {
            let size = match entry.workgroup() {
                None => None,
                Some(WorkgroupSize::Fixed(size)) => Some(size),
                Some(WorkgroupSize::Constant(id)) => {
                    let constant = evaluator.constant_of(id)?;
                    let mut size = [0u32; 3];
                    for (axis, index) in size.iter_mut().zip(0u32..) {
                        let part = evaluator.constituent(constant, index)?;
                        *axis = u32::try_from(evaluator.scalar_bits(part)? & mask(32))
                            .map_err(|_| SpecError::Malformed)?;
                    }
                    Some(size)
                }
            };
            if size.is_some_and(|size| size.contains(&0)) {
                return Err(SpecError::Workgroup);
            }
            workgroups.push(size);
        }
        evaluator.out.workgroups = workgroups;
        Ok(evaluator.out)
    }
}

impl Evaluator<'_> {
    /// The constant `instruction` declares, evaluated, with its id, if it
    /// declares one; `given` holds the values by `SpecId`, sorted.
    fn declare(
        &mut self,
        instruction: &Instruction,
        given: &[(u32, u64)],
        carried: &mut Vec<u32>,
    ) -> Result<Option<(Id, ConstantId)>, SpecError> {
        let module = self.admitted.module();
        let operands = module.operands(instruction);
        let value_at = |index: usize| operands.get(index).map(|o| module.value(o));
        let op = instruction.op();
        let constant = match op {
            Op::CONSTANT_TRUE => self.push_entry(Entry::Scalar(1))?,
            Op::CONSTANT_FALSE => self.push_entry(Entry::Scalar(0))?,
            Op::CONSTANT_NULL | Op::UNDEF => self.null,
            Op::SPEC_CONSTANT_TRUE | Op::SPEC_CONSTANT_FALSE => {
                let bits = match self.given(instruction, given, carried)? {
                    Some((spec_id, bits)) if bits > 1 => return Err(SpecError::Width(spec_id)),
                    Some((_, bits)) => bits,
                    None => u64::from(op == Op::SPEC_CONSTANT_TRUE),
                };
                self.push_entry(Entry::Scalar(bits))?
            }
            Op::CONSTANT | Op::SPEC_CONSTANT => {
                let ty = self.type_of(instruction)?;
                let (scalar, _) = components(ty).ok_or(SpecError::Malformed)?;
                let Some(Value::Number(literal)) = value_at(2) else {
                    return Err(SpecError::Malformed);
                };
                let wide = mask(width(scalar));
                let given = if op == Op::SPEC_CONSTANT {
                    self.given(instruction, given, carried)?
                } else {
                    None
                };
                let bits = match given {
                    Some((spec_id, bits)) if bits & !wide != 0 => {
                        return Err(SpecError::Width(spec_id))
                    }
                    Some((_, bits)) => bits,
                    None => literal & wide,
                };
                self.push_entry(Entry::Scalar(bits))?
            }
            Op::CONSTANT_COMPOSITE | Op::SPEC_CONSTANT_COMPOSITE => {
                let count = u32::try_from(operands.len().saturating_sub(2))
                    .map_err(|_| SpecError::Budget)?;
                self.composite_of(count, |evaluator, at| match value_at(at as usize + 2) {
                    Some(Value::Id(id)) => evaluator.constant_of(id),
                    _ => Err(SpecError::Malformed),
                })?
            }
            Op::SPEC_CONSTANT_OP => self.operation(instruction)?,
            _ => return Ok(None),
        };
        let id = module.result(instruction).ok_or(SpecError::Malformed)?;
        Ok(Some((id, constant)))
    }

    /// The value `given` gives the specialization constant `instruction`
    /// declares, with its `SpecId`; the id it carries is noted in `carried`.
    fn given(
        &self,
        instruction: &Instruction,
        given: &[(u32, u64)],
        carried: &mut Vec<u32>,
    ) -> Result<Option<(u32, u64)>, SpecError> {
        let id = self
            .admitted
            .module()
            .result(instruction)
            .ok_or(SpecError::Malformed)?;
        let Some(spec_id) = self.admitted.decoration(id, None, Decoration::SPEC_ID) else {
            return Ok(None);
        };
        carried.try_reserve(1).map_err(|_| SpecError::OutOfMemory)?;
        carried.push(spec_id);
        Ok(given
            .binary_search_by_key(&spec_id, |&(id, _)| id)
            .ok()
            .and_then(|at| given.get(at))
            .map(|&(_, bits)| (spec_id, bits)))
    }

    /// An `OpSpecConstantOp`, computed.
    fn operation(&mut self, instruction: &Instruction) -> Result<ConstantId, SpecError> {
        let module = self.admitted.module();
        let operands = module.operands(instruction);
        let word = |index: usize| match operands.get(index).map(|o| module.value(o)) {
            Some(Value::Word(word)) => Ok(word),
            _ => Err(SpecError::Malformed),
        };
        let id = |index: usize| match operands.get(index).map(|o| module.value(o)) {
            Some(Value::Id(id)) => Ok(id),
            _ => Err(SpecError::Malformed),
        };
        let op = Op(u16::try_from(word(2)?).map_err(|_| SpecError::Malformed)?);
        let result_ty = self.type_of(instruction)?;
        match op {
            // A composite other than a vector is chosen whole by its one
            // Boolean, and shared as it is.
            Op::SELECT if components(result_ty).is_none() => {
                let chosen = self.scalar_bits(self.constant_of(id(3)?)?)? != 0;
                self.constant_of(id(if chosen { 4 } else { 5 })?)
            }
            Op::COMPOSITE_EXTRACT => {
                let mut at = self.constant_of(id(3)?)?;
                let mut ty = Some(self.type_id_of_value(id(3)?)?);
                for index in 4..operands.len() {
                    let index = word(index)?;
                    let (_, inner) =
                        self.constituent_type(ty.ok_or(SpecError::Malformed)?, index)?;
                    at = self.constituent(at, index)?;
                    ty = inner;
                }
                Ok(at)
            }
            Op::COMPOSITE_INSERT => {
                let object = self.constant_of(id(3)?)?;
                let into = self.constant_of(id(4)?)?;
                let ty = self.type_id_of_value(id(4)?)?;
                self.insert(into, Some(ty), operands.get(5..).unwrap_or(&[]), object)
            }
            Op::VECTOR_SHUFFLE => {
                let (first, second) = (self.constant_of(id(3)?)?, self.constant_of(id(4)?)?);
                let first_count = components(self.type_of_value(id(3)?)?)
                    .ok_or(SpecError::Malformed)?
                    .1;
                let second_count = components(self.type_of_value(id(4)?)?)
                    .ok_or(SpecError::Malformed)?
                    .1;
                let mut parts = [self.null; MAX_VECTOR_COMPONENTS];
                let parts = parts
                    .get_mut(..operands.len().saturating_sub(5))
                    .ok_or(SpecError::Malformed)?;
                for (part, index) in parts.iter_mut().zip(5..) {
                    let pick = word(index)?;
                    *part = match pick.checked_sub(first_count) {
                        None => self.constituent(first, pick)?,
                        Some(at) if at < second_count => self.constituent(second, at)?,
                        Some(_) => self.null,
                    };
                }
                self.composite(parts)
            }
            _ => self.componentwise(op, instruction, result_ty),
        }
    }

    /// `into`, a composite of `ty`, with the constituent at `path`, a list of
    /// literal indices, replaced by `object`. The deepest level is built
    /// first, so no level's constituents are held while the next is reached.
    fn insert(
        &mut self,
        into: ConstantId,
        ty: Option<TypeId>,
        path: &[Operand],
        object: ConstantId,
    ) -> Result<ConstantId, SpecError> {
        let Some((index, rest)) = path.split_first() else {
            return Ok(object);
        };
        let Value::Word(index) = self.admitted.module().value(index) else {
            return Err(SpecError::Malformed);
        };
        let (count, inner) = self.constituent_type(ty.ok_or(SpecError::Malformed)?, index)?;
        let len = match self.entry(into)? {
            Entry::Composite { count: have, .. } => have,
            Entry::Null | Entry::Scalar(_) => count,
        };
        if index >= len {
            return Err(SpecError::Malformed);
        }
        let replaced = self.insert(self.constituent(into, index)?, inner, rest, object)?;
        self.composite_of(len, |evaluator, at| {
            if at == index {
                Ok(replaced)
            } else {
                evaluator.constituent(into, at)
            }
        })
    }

    /// A scalar or component-wise vector operation.
    fn componentwise(
        &mut self,
        op: Op,
        instruction: &Instruction,
        result_ty: Type,
    ) -> Result<ConstantId, SpecError> {
        let module = self.admitted.module();
        let listed = module.operands(instruction).get(3..).unwrap_or(&[]);
        let (result_scalar, count) = components(result_ty).ok_or(SpecError::Malformed)?;
        // Each operand's constant, scalar type, and whether it is a vector;
        // a scalar operand stands for every component.
        let mut inputs = [(self.null, Scalar::Bool, false); 3];
        if listed.len() > inputs.len() {
            return Err(SpecError::Malformed);
        }
        for (input, operand) in inputs.iter_mut().zip(listed) {
            let Value::Id(id) = module.value(operand) else {
                return Err(SpecError::Malformed);
            };
            let ty = self.type_of_value(id)?;
            let (scalar, _) = components(ty).ok_or(SpecError::Malformed)?;
            *input = (
                self.constant_of(id)?,
                scalar,
                matches!(ty, Type::Vector { .. }),
            );
        }
        let mut parts = [self.null; MAX_VECTOR_COMPONENTS];
        let parts = parts
            .get_mut(..count as usize)
            .ok_or(SpecError::Malformed)?;
        for (part, component) in parts.iter_mut().zip(0u32..) {
            let mut args = [(0u64, Scalar::Bool); 3];
            for (slot, &(constant, scalar, vector)) in
                args.iter_mut().zip(&inputs).take(listed.len())
            {
                let part = if vector {
                    self.constituent(constant, component)?
                } else {
                    constant
                };
                *slot = (self.scalar_bits(part)?, scalar);
            }
            let bits = compute(op, args, result_scalar).ok_or(SpecError::Malformed)?;
            *part = self.push_entry(Entry::Scalar(bits))?;
        }
        if matches!(result_ty, Type::Vector { .. }) {
            self.composite(parts)
        } else {
            parts.first().copied().ok_or(SpecError::Malformed)
        }
    }
}

/// `op` over scalar operands of their own scalar types, giving a result of
/// `result`: `None` for an operation no specialization constant takes.
fn compute(op: Op, args: [(u64, Scalar); 3], result: Scalar) -> Option<u64> {
    let [(x, x_type), (y, _), (z, _)] = args;
    let wide = width(result);
    let operand = width(x_type);
    let (low_x, low_y) = (x & mask(operand), y & mask(operand));
    let (signed_x, signed_y) = (signed(x, operand), signed(y, operand));
    let shift =
        |amount: u64| (amount < u64::from(wide)).then(|| u32::try_from(amount).unwrap_or(0));
    let truth = u64::from;
    let bits = match op {
        Op::S_CONVERT => signed_x.cast_unsigned(),
        Op::U_CONVERT => x,
        Op::F_CONVERT => float_bits(wide, float_of(operand, x)),
        Op::S_NEGATE => signed_x.wrapping_neg().cast_unsigned(),
        Op::NOT => !x,
        Op::I_ADD => x.wrapping_add(y),
        Op::I_SUB => x.wrapping_sub(y),
        Op::I_MUL => x.wrapping_mul(y),
        Op::U_DIV => low_x.checked_div(low_y).unwrap_or(0),
        Op::S_DIV => {
            if signed_y == 0 {
                0
            } else {
                signed_x.wrapping_div(signed_y).cast_unsigned()
            }
        }
        Op::U_MOD => low_x.checked_rem(low_y).unwrap_or(0),
        Op::S_REM => {
            if signed_y == 0 {
                0
            } else {
                signed_x.wrapping_rem(signed_y).cast_unsigned()
            }
        }
        Op::S_MOD => {
            if signed_y == 0 {
                0
            } else {
                let remainder = signed_x.wrapping_rem(signed_y);
                if remainder != 0 && (remainder < 0) != (signed_y < 0) {
                    remainder.wrapping_add(signed_y).cast_unsigned()
                } else {
                    remainder.cast_unsigned()
                }
            }
        }
        Op::SHIFT_LEFT_LOGICAL => shift(y).map_or(0, |by| x << by),
        Op::SHIFT_RIGHT_LOGICAL => shift(y).map_or(0, |by| (x & mask(wide)) >> by),
        Op::SHIFT_RIGHT_ARITHMETIC => {
            shift(y).map_or(0, |by| (signed(x, wide) >> by).cast_unsigned())
        }
        Op::BITWISE_OR => x | y,
        Op::BITWISE_XOR => x ^ y,
        Op::BITWISE_AND => x & y,
        Op::LOGICAL_OR => truth(x != 0 || y != 0),
        Op::LOGICAL_AND => truth(x != 0 && y != 0),
        Op::LOGICAL_NOT => truth(x == 0),
        Op::LOGICAL_EQUAL => truth((x != 0) == (y != 0)),
        Op::LOGICAL_NOT_EQUAL => truth((x != 0) != (y != 0)),
        Op::SELECT => {
            if x != 0 {
                y
            } else {
                z
            }
        }
        Op::I_EQUAL => truth(low_x == low_y),
        Op::I_NOT_EQUAL => truth(low_x != low_y),
        Op::U_LESS_THAN => truth(low_x < low_y),
        Op::U_GREATER_THAN => truth(low_x > low_y),
        Op::U_LESS_THAN_EQUAL => truth(low_x <= low_y),
        Op::U_GREATER_THAN_EQUAL => truth(low_x >= low_y),
        Op::S_LESS_THAN => truth(signed_x < signed_y),
        Op::S_GREATER_THAN => truth(signed_x > signed_y),
        Op::S_LESS_THAN_EQUAL => truth(signed_x <= signed_y),
        Op::S_GREATER_THAN_EQUAL => truth(signed_x >= signed_y),
        Op::QUANTIZE_TO_F16 => {
            u64::from(quantized_to_half(u32::try_from(x & mask(32)).unwrap_or(0)))
        }
        _ => return None,
    };
    Some(bits & mask(wide))
}

#[cfg(test)]
#[path = "specialize_tests.rs"]
mod tests;
