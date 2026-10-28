//! `GLSL.std.450`: each extended instruction typed by its specification,
//! the two that write through a pointer charged with the write, and the
//! interpolation functions — which need a capability not admitted — refused
//! by their gate.

use super::body::{Access, Args, Body};
use super::types::{Scalar, Ty};
use super::{Rule, Validator};
use crate::enumerant::StorageClass;
use crate::grammar;
use crate::Instruction;

/// How an extended instruction is typed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Rule450 {
    /// Floats of the result's type throughout.
    Float,
    /// Floats of 16 or 32 bits of the result's type throughout.
    Float32,
    /// Integers as wide and as many as the result's.
    Int,
    /// [`Rule450::Int`], 32 bits wide.
    Int32,
    Determinant,
    MatrixInverse,
    Modf,
    ModfStruct,
    Frexp,
    FrexpStruct,
    Ldexp,
    /// Packs `count` 32-bit floats into a 32-bit integer.
    Pack(u32),
    PackDouble,
    /// Unpacks a 32-bit integer into `count` 32-bit floats.
    Unpack(u32),
    UnpackDouble,
    Length,
    Distance,
    Cross,
    Refract,
}

/// The rule instruction `number` is typed by; `None` for `IMix`, which the
/// specification removed.
pub(crate) const fn rule_of(number: u32) -> Option<Rule450> {
    use Rule450 as R;
    Some(match number {
        1..=4 | 6 | 8..=10 | 31 | 32 | 37 | 40 | 43 | 46 | 48..=50 | 69..=71 | 79..=81 => R::Float,
        11..=30 => R::Float32,
        5 | 7 | 38 | 39 | 41 | 42 | 44 | 45 => R::Int,
        73..=75 => R::Int32,
        33 => R::Determinant,
        34 => R::MatrixInverse,
        35 => R::Modf,
        36 => R::ModfStruct,
        51 => R::Frexp,
        52 => R::FrexpStruct,
        53 => R::Ldexp,
        54 | 55 => R::Pack(4),
        56..=58 => R::Pack(2),
        59 => R::PackDouble,
        60..=62 => R::Unpack(2),
        63 | 64 => R::Unpack(4),
        65 => R::UnpackDouble,
        66 => R::Length,
        67 => R::Distance,
        68 => R::Cross,
        72 => R::Refract,
        _ => return None,
    })
}

impl Validator<'_> {
    /// An `OpExtInst` of `GLSL.std.450`.
    pub(crate) fn ext_inst(
        &mut self,
        body: &mut Body<'_>,
        instruction: &Instruction,
        args: &Args<'_>,
    ) -> Result<(), Rule> {
        let number = self.arg_word(args, 1)?;
        let enabled = grammar::glsl_std_450(number).is_some_and(|inst| {
            inst.capabilities.is_empty() || inst.capabilities.iter().any(|&c| self.enabled.has(c))
        });
        let rule = rule_of(number)
            .filter(|_| enabled)
            .ok_or(Rule::NotAdmitted(instruction.op()))?;
        // The instruction's own operands follow the set and the number.
        let own = Args {
            operands: args.operands.get(2..).unwrap_or(&[]),
            offset: args.offset.saturating_add(2),
            ..*args
        };
        let takes_pointer = matches!(rule, Rule450::Modf | Rule450::Frexp);
        for n in 0..own.len() {
            let pointer = matches!(self.ty(self.arg_type(&own, n)?).ty, Ty::Pointer { .. });
            if pointer != (takes_pointer && n == 1) {
                return Err(Rule::LogicalPointer);
            }
        }
        self.type_450(body, rule, &own)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per extended-instruction family"
    )]
    fn type_450(&self, body: &mut Body<'_>, rule: Rule450, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        let operand = |n: usize| self.arg_type(args, n);
        let float_sv = |ty: u32| matches!(self.components(ty), Some((s, _)) if s.is_float());
        let all_result = || -> Result<(), Rule> {
            for n in 0..args.len() {
                if operand(n)? != r {
                    return Err(args.wrong(n));
                }
            }
            Ok(())
        };
        match rule {
            Rule450::Float => {
                if !float_sv(r) {
                    return Err(Rule::ResultType);
                }
                all_result()
            }
            Rule450::Float32 => {
                if !matches!(
                    self.components(r),
                    Some((Scalar::Float { width: 16 | 32 }, _))
                ) {
                    return Err(Rule::ResultType);
                }
                all_result()
            }
            Rule450::Int | Rule450::Int32 => {
                let Some((s, count)) = self.components(r).filter(|(s, _)| s.is_int()) else {
                    return Err(Rule::ResultType);
                };
                if rule == Rule450::Int32 && s.width() != 32 {
                    return Err(Rule::ResultType);
                }
                for n in 0..args.len() {
                    match self.components(operand(n)?) {
                        Some((o, c)) if o.is_int() && c == count && o.width() == s.width() => {}
                        _ => return Err(args.wrong(n)),
                    }
                }
                Ok(())
            }
            Rule450::Determinant | Rule450::MatrixInverse => {
                let x = operand(0)?;
                let Ty::Matrix {
                    columns,
                    rows,
                    scalar,
                    ..
                } = self.ty(x).ty
                else {
                    return Err(args.wrong(0));
                };
                if columns != rows {
                    return Err(args.wrong(0));
                }
                let expected = if rule == Rule450::Determinant {
                    self.scalar_index(scalar)
                } else {
                    Some(x)
                };
                if Some(r) != expected {
                    return Err(Rule::ResultType);
                }
                Ok(())
            }
            Rule450::Modf | Rule450::Frexp => {
                if !float_sv(r) || operand(0)? != r {
                    return Err(args.wrong(0));
                }
                let pointer = self.arg_id(args, 1)?;
                let Ty::Pointer { class, pointee } = self.ty(operand(1)?).ty else {
                    return Err(args.wrong(1));
                };
                let pointee_ok = if rule == Rule450::Modf {
                    pointee == r
                } else {
                    let count = self.components(r).map(|(_, c)| c);
                    matches!(self.components(pointee), Some((Scalar::Int { width: 32, .. }, c)) if Some(c) == count)
                };
                if !pointee_ok || class == StorageClass::IMAGE {
                    return Err(args.wrong(1));
                }
                let root = self.root(pointer).ok_or(Rule::LogicalPointer)?;
                self.access(body, root, Access::WRITE)
            }
            Rule450::ModfStruct | Rule450::FrexpStruct => {
                let x = operand(0)?;
                if !float_sv(x) {
                    return Err(args.wrong(0));
                }
                let Ty::Struct { first, count: 2 } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                let members = self.list(first, 2);
                let (Some(&fraction), Some(&whole)) = (members.first(), members.get(1)) else {
                    return Err(Rule::ResultType);
                };
                let second = if rule == Rule450::ModfStruct {
                    whole == x
                } else {
                    let count = self.components(x).map(|(_, c)| c);
                    matches!(self.components(whole), Some((Scalar::Int { width: 32, .. }, c)) if Some(c) == count)
                };
                if fraction != x || !second {
                    return Err(Rule::ResultType);
                }
                Ok(())
            }
            Rule450::Ldexp => {
                if !float_sv(r) || operand(0)? != r {
                    return Err(args.wrong(0));
                }
                let count = self.components(r).map(|(_, c)| c);
                match self.components(operand(1)?) {
                    Some((s, c)) if s.is_int() && Some(c) == count => Ok(()),
                    _ => Err(args.wrong(1)),
                }
            }
            Rule450::Pack(count) => {
                if !matches!(self.ty(r).ty, Ty::Scalar(Scalar::Int { width: 32, .. })) {
                    return Err(Rule::ResultType);
                }
                match self.components(operand(0)?) {
                    Some((Scalar::Float { width: 32 }, c)) if c == count => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Rule450::PackDouble => {
                if !matches!(self.ty(r).ty, Ty::Scalar(Scalar::Float { width: 64 })) {
                    return Err(Rule::ResultType);
                }
                match self.components(operand(0)?) {
                    Some((Scalar::Int { width: 32, .. }, 2)) => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Rule450::Unpack(count) => {
                match self.components(r) {
                    Some((Scalar::Float { width: 32 }, c)) if c == count => {}
                    _ => return Err(Rule::ResultType),
                }
                match self.ty(operand(0)?).ty {
                    Ty::Scalar(Scalar::Int { width: 32, .. }) => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Rule450::UnpackDouble => {
                match self.components(r) {
                    Some((Scalar::Int { width: 32, .. }, 2)) => {}
                    _ => return Err(Rule::ResultType),
                }
                match self.ty(operand(0)?).ty {
                    Ty::Scalar(Scalar::Float { width: 64 }) => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Rule450::Length | Rule450::Distance => {
                let x = operand(0)?;
                let Some((s, _)) = self.components(x).filter(|(s, _)| s.is_float()) else {
                    return Err(args.wrong(0));
                };
                if rule == Rule450::Distance && operand(1)? != x {
                    return Err(args.wrong(1));
                }
                if Some(r) != self.scalar_index(s) {
                    return Err(Rule::ResultType);
                }
                Ok(())
            }
            Rule450::Cross => {
                if !matches!(self.components(r), Some((s, 3)) if s.is_float()) {
                    return Err(Rule::ResultType);
                }
                all_result()
            }
            Rule450::Refract => {
                if !float_sv(r) {
                    return Err(Rule::ResultType);
                }
                for n in 0..2 {
                    if operand(n)? != r {
                        return Err(args.wrong(n));
                    }
                }
                let component = self.components(r).map(|(s, _)| s);
                match self.ty(operand(2)?).ty {
                    Ty::Scalar(s) if Some(s) == component => Ok(()),
                    _ => Err(args.wrong(2)),
                }
            }
        }
    }
}
