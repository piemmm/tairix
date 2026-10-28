//! The operations that read and make values only — arithmetic, logic,
//! conversion, composites, matrices and bits — typed by their result and
//! operand types alone, so a specialization constant's operation is typed
//! exactly as the instruction is.

use super::body::{Args, Typing};
use super::types::{Scalar, Ty};
use alloc::vec::Vec;

use super::{Limit, Rule, Validator, MAX_INDEXES, NONE};
use crate::{Value, Version};

impl Validator<'_> {
    /// Type `args` as `typing`, one of the value operations.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per family keeps each family's rule beside its name"
    )]
    pub(crate) fn type_operation(&self, typing: Typing, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        let comps = |ty: u32| self.components(ty);
        let operand = |n: usize| self.arg_type(args, n);
        // The result as a scalar or vector of kind `pick`, and its count and
        // width.
        let result_of = |pick: fn(Scalar) -> bool| match comps(r) {
            Some((s, count)) if pick(s) => Ok((s, count)),
            _ => Err(Rule::ResultType),
        };
        let same = |n: usize| -> Result<(), Rule> {
            if operand(n)? == r {
                Ok(())
            } else {
                Err(args.wrong(n))
            }
        };
        let count_args = |want: usize| {
            if args.len() == want {
                Ok(())
            } else {
                Err(Rule::OperandType(args.offset))
            }
        };
        match typing {
            Typing::IntArith | Typing::IntUnary => {
                let (s, count) = result_of(Scalar::is_int)?;
                count_args(if typing == Typing::IntArith { 2 } else { 1 })?;
                for n in 0..args.len() {
                    match comps(operand(n)?) {
                        Some((o, c)) if o.is_int() && c == count && o.width() == s.width() => {}
                        _ => return Err(args.wrong(n)),
                    }
                }
                Ok(())
            }
            Typing::UnsignedArith => {
                result_of(Scalar::is_unsigned)?;
                count_args(2)?;
                same(0)?;
                same(1)
            }
            Typing::FloatArith => {
                result_of(Scalar::is_float)?;
                count_args(2)?;
                same(0)?;
                same(1)
            }
            Typing::FloatUnary => {
                result_of(Scalar::is_float)?;
                count_args(1)?;
                same(0)
            }
            Typing::Shift => {
                let (s, count) = result_of(Scalar::is_int)?;
                count_args(2)?;
                match comps(operand(0)?) {
                    Some((b, c)) if b.is_int() && c == count && b.width() == s.width() => {}
                    _ => return Err(args.wrong(0)),
                }
                match comps(operand(1)?) {
                    Some((shift, c)) if shift.is_int() && c == count => Ok(()),
                    _ => Err(args.wrong(1)),
                }
            }
            Typing::IntCompare | Typing::FloatCompare => {
                let (_, count) = result_of(Scalar::is_bool)?;
                count_args(2)?;
                let (a, b) = (operand(0)?, operand(1)?);
                let (sa, ca) = comps(a).ok_or(args.wrong(0))?;
                let (sb, cb) = comps(b).ok_or(args.wrong(1))?;
                let ok = if typing == Typing::IntCompare {
                    sa.is_int() && sb.is_int() && sa.width() == sb.width()
                } else {
                    sa.is_float() && a == b
                };
                if !ok || ca != count {
                    return Err(args.wrong(0));
                }
                if cb != count {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            Typing::Logical => {
                result_of(Scalar::is_bool)?;
                count_args(2)?;
                same(0)?;
                same(1)
            }
            Typing::LogicalNot => {
                result_of(Scalar::is_bool)?;
                count_args(1)?;
                same(0)
            }
            Typing::FloatClass => {
                let (_, count) = result_of(Scalar::is_bool)?;
                count_args(1)?;
                match comps(operand(0)?) {
                    Some((s, c)) if s.is_float() && c == count => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::AnyAll => {
                if !matches!(self.ty(r).ty, Ty::Scalar(Scalar::Bool)) {
                    return Err(Rule::ResultType);
                }
                count_args(1)?;
                match self.ty(operand(0)?).ty {
                    Ty::Vector {
                        scalar: Scalar::Bool,
                        ..
                    } => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::Select => self.select(args),
            Typing::FloatToInt { unsigned } => {
                let (_, count) = result_of(if unsigned {
                    Scalar::is_unsigned
                } else {
                    Scalar::is_int
                })?;
                count_args(1)?;
                self.counted(args, 0, Scalar::is_float, count)
            }
            Typing::IntToFloat => {
                let (_, count) = result_of(Scalar::is_float)?;
                count_args(1)?;
                self.counted(args, 0, Scalar::is_int, count)
            }
            Typing::IntWidth { unsigned } => {
                let (s, count) = result_of(if unsigned {
                    Scalar::is_unsigned
                } else {
                    Scalar::is_int
                })?;
                count_args(1)?;
                match comps(operand(0)?) {
                    Some((o, c)) if o.is_int() && c == count && o.width() != s.width() => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::FloatWidth => {
                let (s, count) = result_of(Scalar::is_float)?;
                count_args(1)?;
                match comps(operand(0)?) {
                    Some((o, c)) if o.is_float() && c == count && o.width() != s.width() => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::QuantizeToF16 | Typing::Derivative => {
                match comps(r) {
                    Some((Scalar::Float { width: 32 }, _)) => {}
                    _ => return Err(Rule::ResultType),
                }
                count_args(1)?;
                same(0)
            }
            Typing::Bitcast => self.bitcast(args),
            Typing::VectorExtractDynamic => {
                count_args(2)?;
                let Ty::Vector { scalar, .. } = self.ty(operand(0)?).ty else {
                    return Err(args.wrong(0));
                };
                if self.scalar_index(scalar) != Some(r) {
                    return Err(Rule::ResultType);
                }
                self.int_scalar(args, 1)
            }
            Typing::VectorInsertDynamic => {
                count_args(3)?;
                let Ty::Vector { scalar, .. } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                same(0)?;
                if Some(operand(1)?) != self.scalar_index(scalar) {
                    return Err(args.wrong(1));
                }
                self.int_scalar(args, 2)
            }
            Typing::VectorShuffle => self.shuffle(args),
            Typing::CompositeConstruct => self.construct(args),
            Typing::CompositeExtract => {
                if args.len() < 2 {
                    return Err(Rule::OperandType(args.offset));
                }
                let reached = self.walk(args, operand(0)?, 1)?;
                if reached != r {
                    return Err(Rule::ResultType);
                }
                Ok(())
            }
            Typing::CompositeInsert => {
                if args.len() < 3 {
                    return Err(Rule::OperandType(args.offset));
                }
                same(1)?;
                let reached = self.walk(args, r, 2)?;
                if operand(0)? != reached {
                    return Err(args.wrong(0));
                }
                Ok(())
            }
            Typing::CopyObject => {
                count_args(1)?;
                if matches!(self.ty(r).ty, Ty::Void) {
                    return Err(Rule::ResultType);
                }
                same(0)
            }
            Typing::Transpose => {
                count_args(1)?;
                let Ty::Matrix {
                    columns,
                    rows,
                    scalar,
                    ..
                } = self.ty(r).ty
                else {
                    return Err(Rule::ResultType);
                };
                match self.ty(operand(0)?).ty {
                    Ty::Matrix {
                        columns: c,
                        rows: w,
                        scalar: s,
                        ..
                    } if c == rows && w == columns && s == scalar => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::CopyLogical => {
                if self.enabled.version < (Version { major: 1, minor: 4 }) {
                    return Err(Rule::InstructionNotEnabled(args.op));
                }
                count_args(1)?;
                let from = operand(0)?;
                let class = |ty: u32| self.logical.get(ty as usize).copied();
                if from == r || class(from).is_none() || class(from) != class(r) {
                    return Err(args.wrong(0));
                }
                Ok(())
            }
            Typing::VectorTimesScalar => {
                let Ty::Vector { scalar: s, .. } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                if !s.is_float() {
                    return Err(Rule::ResultType);
                }
                count_args(2)?;
                same(0)?;
                if Some(operand(1)?) != self.scalar_index(s) {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            Typing::MatrixTimesScalar => {
                let Ty::Matrix { scalar, .. } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                count_args(2)?;
                same(0)?;
                if Some(operand(1)?) != self.scalar_index(scalar) {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            Typing::VectorTimesMatrix => {
                let Ty::Vector { scalar, count } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                count_args(2)?;
                let Ty::Matrix {
                    columns,
                    rows,
                    scalar: m,
                    ..
                } = self.ty(operand(1)?).ty
                else {
                    return Err(args.wrong(1));
                };
                if m != scalar || columns != count {
                    return Err(args.wrong(1));
                }
                match self.ty(operand(0)?).ty {
                    Ty::Vector {
                        scalar: v,
                        count: c,
                    } if v == scalar && c == rows => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            Typing::MatrixTimesVector => {
                if !matches!(
                    self.ty(r).ty,
                    Ty::Vector {
                        scalar: Scalar::Float { .. },
                        ..
                    }
                ) {
                    return Err(Rule::ResultType);
                }
                count_args(2)?;
                let Ty::Matrix {
                    column,
                    columns,
                    scalar,
                    ..
                } = self.ty(operand(0)?).ty
                else {
                    return Err(args.wrong(0));
                };
                if column != r {
                    return Err(args.wrong(0));
                }
                match self.ty(operand(1)?).ty {
                    Ty::Vector { scalar: v, count } if v == scalar && count == columns => Ok(()),
                    _ => Err(args.wrong(1)),
                }
            }
            Typing::MatrixTimesMatrix => {
                let Ty::Matrix {
                    column,
                    columns,
                    scalar,
                    ..
                } = self.ty(r).ty
                else {
                    return Err(Rule::ResultType);
                };
                count_args(2)?;
                let Ty::Matrix {
                    column: left_column,
                    columns: left_columns,
                    ..
                } = self.ty(operand(0)?).ty
                else {
                    return Err(args.wrong(0));
                };
                if left_column != column {
                    return Err(args.wrong(0));
                }
                match self.ty(operand(1)?).ty {
                    Ty::Matrix {
                        columns: c,
                        rows,
                        scalar: s,
                        ..
                    } if s == scalar && c == columns && rows == left_columns => Ok(()),
                    _ => Err(args.wrong(1)),
                }
            }
            Typing::OuterProduct => {
                let Ty::Matrix {
                    column,
                    columns,
                    scalar,
                    ..
                } = self.ty(r).ty
                else {
                    return Err(Rule::ResultType);
                };
                count_args(2)?;
                if operand(0)? != column {
                    return Err(args.wrong(0));
                }
                match self.ty(operand(1)?).ty {
                    Ty::Vector { scalar: s, count } if s == scalar && count == columns => Ok(()),
                    _ => Err(args.wrong(1)),
                }
            }
            Typing::Dot => {
                let Ty::Scalar(s @ Scalar::Float { .. }) = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                count_args(2)?;
                let a = operand(0)?;
                match self.ty(a).ty {
                    Ty::Vector { scalar, .. } if scalar == s => {}
                    _ => return Err(args.wrong(0)),
                }
                if operand(1)? != a {
                    return Err(args.wrong(1));
                }
                Ok(())
            }
            Typing::Extended { unsigned } => {
                count_args(2)?;
                let Ty::Struct { first, count: 2 } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                let members = self.list(first, 2);
                let (Some(&a), Some(&b)) = (members.first(), members.get(1)) else {
                    return Err(Rule::ResultType);
                };
                let pick: fn(Scalar) -> bool = if unsigned {
                    Scalar::is_unsigned
                } else {
                    Scalar::is_int
                };
                if a != b || !comps(a).is_some_and(|(s, _)| pick(s)) {
                    return Err(Rule::ResultType);
                }
                for n in 0..2 {
                    if operand(n)? != a {
                        return Err(args.wrong(n));
                    }
                }
                Ok(())
            }
            Typing::BitFieldInsert | Typing::BitFieldExtract | Typing::BitReverse => {
                match comps(r) {
                    Some((Scalar::Int { width: 32, .. }, _)) => {}
                    _ => return Err(Rule::ResultType),
                }
                let (values, scalars) = match typing {
                    Typing::BitFieldInsert => (2, 2),
                    Typing::BitFieldExtract => (1, 2),
                    _ => (1, 0),
                };
                count_args(values + scalars)?;
                for n in 0..values {
                    same(n)?;
                }
                for n in values..values + scalars {
                    self.int_scalar(args, n)?;
                }
                Ok(())
            }
            Typing::BitCount => {
                let (_, count) = result_of(Scalar::is_int)?;
                count_args(1)?;
                match comps(operand(0)?) {
                    Some((Scalar::Int { width: 32, .. }, c)) if c == count => Ok(()),
                    _ => Err(args.wrong(0)),
                }
            }
            _ => Err(Rule::NotAdmitted(args.op)),
        }
    }

    /// Operand `n` is a scalar or vector of kind `pick` with `count`
    /// components.
    fn counted(
        &self,
        args: &Args<'_>,
        n: usize,
        pick: fn(Scalar) -> bool,
        count: u32,
    ) -> Result<(), Rule> {
        match self.components(self.arg_type(args, n)?) {
            Some((s, c)) if pick(s) && c == count => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    /// Operand `n` is an integer scalar.
    pub(crate) fn int_scalar(&self, args: &Args<'_>, n: usize) -> Result<(), Rule> {
        match self.ty(self.arg_type(args, n)?).ty {
            Ty::Scalar(s) if s.is_int() => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    /// `OpSelect`: two objects of the result type, chosen by a Boolean or a
    /// vector of them as wide as the result.
    fn select(&self, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        if args.len() != 3 {
            return Err(Rule::OperandType(args.offset));
        }
        let info = self.ty(r);
        let composite = matches!(
            info.ty,
            Ty::Array { .. } | Ty::Struct { .. } | Ty::Matrix { .. }
        );
        let admitted = matches!(info.ty, Ty::Scalar(_) | Ty::Vector { .. })
            || (composite && self.enabled.version >= (Version { major: 1, minor: 4 }));
        if !admitted {
            return Err(Rule::ResultType);
        }
        for n in 1..3 {
            if self.arg_type(args, n)? != r {
                return Err(args.wrong(n));
            }
        }
        match self.ty(self.arg_type(args, 0)?).ty {
            Ty::Scalar(Scalar::Bool) => Ok(()),
            Ty::Vector {
                scalar: Scalar::Bool,
                count,
            } if matches!(info.ty, Ty::Vector { count: c, .. } if c == count) => Ok(()),
            _ => Err(args.wrong(0)),
        }
    }

    /// `OpBitcast` between numeric scalars and vectors of the same size,
    /// components of one width when the counts agree, or one count a
    /// multiple of the other.
    fn bitcast(&self, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        if args.len() != 1 {
            return Err(Rule::OperandType(args.offset));
        }
        let from = self.arg_type(args, 0)?;
        let numeric = |ty: u32| self.components(ty).filter(|(s, _)| !s.is_bool());
        let (Some((rs, rc)), Some((fs, fc))) = (numeric(r), numeric(from)) else {
            return Err(Rule::ResultType);
        };
        let bits = |s: Scalar, c: u32| s.width() * c;
        let shape = if rc == fc {
            rs.width() == fs.width()
        } else {
            let (large, small) = if rc > fc { (rc, fc) } else { (fc, rc) };
            large % small == 0
        };
        if from == r || bits(rs, rc) != bits(fs, fc) || !shape {
            return Err(args.wrong(0));
        }
        Ok(())
    }

    /// `OpVectorShuffle`: two vectors of the result's component type, and a
    /// literal per result component selecting from them, or `0xFFFFFFFF`.
    fn shuffle(&self, args: &Args<'_>) -> Result<(), Rule> {
        let Ty::Vector { scalar, count } = self.ty(args.result_type).ty else {
            return Err(Rule::ResultType);
        };
        if args.len() != 2 + count as usize {
            return Err(Rule::OperandType(args.offset));
        }
        let mut total = 0;
        for n in 0..2 {
            match self.ty(self.arg_type(args, n)?).ty {
                Ty::Vector {
                    scalar: s,
                    count: c,
                } if s == scalar => total += c,
                _ => return Err(args.wrong(n)),
            }
        }
        for n in 2..args.len() {
            let component = self.arg_word(args, n)?;
            if component != u32::MAX && component >= total {
                return Err(Rule::OperandValue(
                    args.offset
                        .saturating_add(u8::try_from(n).unwrap_or(u8::MAX)),
                ));
            }
        }
        Ok(())
    }

    /// `OpCompositeConstruct`: a constituent per top-level part, or for a
    /// vector, at least two scalars and vectors of its component type making
    /// up its count.
    fn construct(&self, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        let info = self.ty(r);
        if let Ty::Vector { scalar, count } = info.ty {
            if args.len() < 2 {
                return Err(Rule::OperandType(args.offset));
            }
            let mut total = 0;
            for n in 0..args.len() {
                match self.components(self.arg_type(args, n)?) {
                    Some((s, c)) if s == scalar => total += c,
                    _ => return Err(args.wrong(n)),
                }
            }
            return if total == count {
                Ok(())
            } else {
                Err(Rule::OperandType(args.offset))
            };
        }
        let parts = match info.ty {
            Ty::Matrix { .. } | Ty::Array { .. } | Ty::Struct { .. } => self.constituents(r),
            _ => None,
        }
        .ok_or(Rule::ResultType)?;
        if args.len() != parts as usize {
            return Err(Rule::OperandType(args.offset));
        }
        for n in 0..args.len() {
            let expected = self.constituent(r, u32::try_from(n).unwrap_or(NONE));
            if expected != Some(self.arg_type(args, n)?) {
                return Err(args.wrong(n));
            }
        }
        Ok(())
    }

    /// Walk the literal indexes of `args` from operand `from` on down
    /// `ty`, each in bounds, to the type it reaches.
    fn walk(&self, args: &Args<'_>, mut ty: u32, from: usize) -> Result<u32, Rule> {
        if args.len() - from > MAX_INDEXES {
            return Err(Rule::Limit(Limit::Indexes));
        }
        for n in from..args.len() {
            let Some(Value::Word(index)) = args.operands.get(n).map(|o| self.m.value(o)) else {
                return Err(args.wrong(n));
            };
            ty = self.constituent(ty, index).ok_or(Rule::OperandValue(
                args.offset
                    .saturating_add(u8::try_from(n).unwrap_or(u8::MAX)),
            ))?;
        }
        Ok(ty)
    }

    /// Each type's logical class: two types match logically, as
    /// `OpCopyLogical` needs, exactly when their classes are equal. A
    /// non-aggregate is its own class; an array's is its length and its
    /// element's class, a structure's its members' classes. Aggregates are
    /// classed a nesting level at a time, sorting each level's keys, so the
    /// cost is the members' count times its logarithm whatever the sharing.
    pub(crate) fn logical_classes(&self) -> Result<Vec<u32>, super::Refusal> {
        let types = u32::try_from(self.types.len()).unwrap_or(NONE);
        let mut class: Vec<u32> = super::collect(0..types)?;
        let aggregate =
            |info: &super::types::TypeInfo| matches!(info.ty, Ty::Array { .. } | Ty::Struct { .. });
        let mut order: Vec<u32> = super::collect((0..types).filter(|&t| aggregate(self.ty(t))))?;
        order.sort_unstable_by_key(|&t| self.ty(t).depth);
        // Keys are flattened: each type's tag, size and member classes.
        let mut keys: Vec<u32> = Vec::new();
        let mut level: Vec<(u32, core::ops::Range<usize>)> = Vec::new();
        for group in order.chunk_by(|&a, &b| self.ty(a).depth == self.ty(b).depth) {
            keys.clear();
            level.clear();
            for &index in group {
                let start = keys.len();
                match self.ty(index).ty {
                    Ty::Array { element, length } => {
                        for word in [1, length, class[element as usize]] {
                            super::push(&mut keys, word)?;
                        }
                    }
                    Ty::Struct { first, count } => {
                        super::push(&mut keys, 2)?;
                        super::push(&mut keys, count)?;
                        for &member in self.list(first, count) {
                            super::push(&mut keys, class[member as usize])?;
                        }
                    }
                    _ => continue,
                }
                super::push(&mut level, (index, start..keys.len()))?;
            }
            let key = |r: &core::ops::Range<usize>| keys.get(r.clone()).unwrap_or(&[]);
            level.sort_unstable_by(|(_, a), (_, b)| key(a).cmp(key(b)));
            let mut leader = NONE;
            let mut previous: Option<&core::ops::Range<usize>> = None;
            for (index, range) in &level {
                if previous.is_none_or(|p| key(p) != key(range)) {
                    leader = types.saturating_add(*index);
                }
                class[*index as usize] = leader;
                previous = Some(range);
            }
        }
        Ok(class)
    }
}
