//! Calls: a user function's, a value constructor's, or a built-in's; and
//! the value a constant constructor makes.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::decl::{DeclInfo, Signature};
use super::expr::{join, Form};
use super::{Checker, Denotes, ExprInfo, Name, Stage, NONE};
use crate::diag::{error, push, reserve, Code, Outcome, Span, Stop};
use crate::eval::{construct_scalar, ValueId, Values};
use crate::predeclared::{Generator, Predeclared};
use crate::syntax::{ExprId, Ident, Run};
use crate::types::{Count, Scalar, Ty, TyId};

/// How a value constructor builds its value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Construct {
    /// `T()`: the zero value.
    Zero,
    /// One scalar, vector or matrix converted component by component to
    /// the result's scalar, or copied where it is the result's type.
    Convert,
    /// A vector of one scalar repeated.
    Splat,
    /// The result's components in order: a vector's scalars and subvectors
    /// flattened, a matrix's columns, an array's elements, a structure's
    /// members.
    Parts,
    /// A matrix from its scalars, column after column.
    Elements,
}

/// A user function's call: the calling function, the called one, and the
/// call expression.
#[derive(Copy, Clone, Debug)]
pub(crate) struct CallSite {
    pub(crate) caller: u32,
    pub(crate) callee: u32,
    pub(crate) call: ExprId,
}

impl<'a> Checker<'a> {
    /// A call of `callee` with `args`, the call expression `id`.
    pub(crate) fn call(
        &mut self,
        id: ExprId,
        callee: Ident,
        args: Run,
        span: Span,
    ) -> Outcome<ExprInfo> {
        let name = self
            .resolve(callee.name)
            .ok_or_else(|| self.undeclared(callee.name))?;
        let templated = !callee.template.is_empty();
        let tree: &'a crate::syntax::Tree = self.tree;
        let args = tree.expr_list(args);
        if let Name::Decl(decl) = name {
            self.note_reference(decl)?;
        }
        match name {
            Name::Decl(decl) => match self.decls.get(decl as usize).copied() {
                Some(DeclInfo::Function(signature)) if !templated => {
                    self.user_call(id, decl, signature, args, span)
                }
                Some(DeclInfo::Type(ty)) if !templated => self.construct(ty, args, span),
                Some(DeclInfo::Function(_) | DeclInfo::Type(_)) => {
                    Err(self.no_template(callee.name))
                }
                _ => Err(self.not_callable(callee.name)),
            },
            Name::Predeclared(Predeclared::Type(named)) => {
                if templated {
                    return Err(self.no_template(callee.name));
                }
                let ty = self.named(named, callee.name)?;
                self.construct(ty, args, span)
            }
            Name::Predeclared(Predeclared::Generator(generator)) if templated => {
                let ty = self.generate(generator, callee.template, callee.name)?;
                self.held_to_features(ty, callee.name)?;
                self.construct(ty, args, span)
            }
            Name::Predeclared(Predeclared::Generator(generator)) => {
                self.inferred(generator, args, span)
            }
            Name::Predeclared(Predeclared::Builtin(builtin)) => {
                self.builtin_call(builtin, callee, args, span)
            }
            _ => Err(self.not_callable(callee.name)),
        }
    }

    fn no_template(&self, name: Span) -> Stop {
        error(Code::Type, name, |m| {
            m.quote(self.spell(name))?;
            m.write_str(" takes no template list")
        })
    }

    fn not_callable(&self, name: Span) -> Stop {
        error(Code::Misused, name, |m| {
            m.quote(self.spell(name))?;
            m.write_str(" is not a function or a type")
        })
    }

    /// A call of the user function `decl`: its arguments converted to its
    /// parameters, the call recorded for the call graph.
    fn user_call(
        &mut self,
        id: ExprId,
        decl: u32,
        signature: Signature,
        args: &[ExprId],
        span: Span,
    ) -> Outcome<ExprInfo> {
        if signature.stage.is_some() {
            return Err(error(Code::Misused, span, |m| {
                m.write_str("an entry point is not called")
            }));
        }
        let (start, end) = signature.params;
        let wanted = (end - start) as usize;
        if args.len() != wanted {
            return Err(error(Code::NoOverload, span, |m| {
                write!(
                    m,
                    "this function takes {wanted} arguments, not {}",
                    args.len()
                )
            }));
        }
        for (offset, &arg) in args.iter().enumerate() {
            let param = self
                .params
                .get(start as usize + offset)
                .copied()
                .ok_or(Stop::Internal)?;
            self.convert_to(arg, param.ty)?;
        }
        if self.current_fn != NONE {
            push(
                &mut self.call_sites,
                CallSite {
                    caller: self.current_fn,
                    callee: decl,
                    call: id,
                },
            )?;
        }
        let ty = match signature.result {
            Some(ty) => ty,
            None => self.intern(Ty::Void)?,
        };
        Ok(ExprInfo {
            ty,
            converted: ty,
            stage: Stage::Runtime,
            value: NONE,
            denotes: Denotes::CallFunction(decl),
        })
    }

    /// The latest stage among `args`, `Const` for none.
    fn stage_of(&self, args: &[ExprId]) -> Stage {
        args.iter()
            .map(|&a| self.info(a).stage)
            .max()
            .unwrap_or(Stage::Const)
    }

    /// A constructor's information: its type, stage and form.
    fn constructed(ty: TyId, stage: Stage, how: Construct) -> ExprInfo {
        ExprInfo {
            ty,
            converted: ty,
            stage,
            value: NONE,
            denotes: Denotes::Construct(how),
        }
    }

    fn no_constructor(&self, ty: TyId, span: Span) -> Stop {
        error(Code::NoOverload, span, |m| {
            m.write_str("no constructor of ")?;
            self.write_type(m, ty)?;
            m.write_str(" takes these arguments")
        })
    }

    /// A constructor of the written type `ty`.
    fn construct(&mut self, ty: TyId, args: &[ExprId], span: Span) -> Outcome<ExprInfo> {
        if !self.types.is_constructible(ty) {
            return Err(error(Code::Type, span, |m| {
                self.write_type(m, ty)?;
                m.write_str(" is not constructible")
            }));
        }
        let stage = self.stage_of(args);
        if args.is_empty() {
            return Ok(Self::constructed(ty, Stage::Const, Construct::Zero));
        }
        let how = match self.types.get(ty) {
            Ty::Scalar(_) => {
                let [arg] = args else {
                    return Err(self.no_constructor(ty, span));
                };
                let from = self.value_of(*arg)?;
                if !matches!(self.types.get(from), Ty::Scalar(_)) {
                    return Err(self.no_constructor(ty, span));
                }
                Construct::Convert
            }
            Ty::Vector(n, scalar) => self.vector_parts(ty, (n, scalar), args, span)?,
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => self.matrix_parts(ty, (columns, rows, scalar), args, span)?,
            Ty::Array {
                element,
                count: Count::Fixed(n),
            } => {
                if args.len() != n as usize {
                    return Err(error(Code::NoOverload, span, |m| {
                        write!(m, "this array takes {n} elements, not {}", args.len())
                    }));
                }
                for &arg in args {
                    self.convert_to(arg, element)?;
                }
                Construct::Parts
            }
            Ty::Struct(structure) => {
                let count = self.types.members_of(structure).len();
                if args.len() != count {
                    return Err(error(Code::NoOverload, span, |m| {
                        write!(
                            m,
                            "this structure takes {count} members, not {}",
                            args.len()
                        )
                    }));
                }
                for (index, &arg) in args.iter().enumerate() {
                    let member = self
                        .types
                        .members_of(structure)
                        .get(index)
                        .map(|m| m.ty)
                        .ok_or(Stop::Internal)?;
                    self.convert_to(arg, member)?;
                }
                Construct::Parts
            }
            _ => return Err(self.no_constructor(ty, span)),
        };
        Ok(Self::constructed(ty, stage, how))
    }

    /// A vector of `n` `scalar`s from `args`: one scalar repeated, one
    /// vector of its count converted, or scalars and subvectors whose
    /// components number `n`.
    fn vector_parts(
        &mut self,
        ty: TyId,
        (n, scalar): (u8, Scalar),
        args: &[ExprId],
        span: Span,
    ) -> Outcome<Construct> {
        if let [arg] = args {
            let from = self.value_of(*arg)?;
            match self.form(from) {
                Some((Form::Vector(m), _)) if m == n => return Ok(Construct::Convert),
                Some((Form::Scalar, _)) => {
                    let element = self.types.scalar(scalar)?;
                    self.convert_to(*arg, element)?;
                    return Ok(Construct::Splat);
                }
                _ => return Err(self.no_constructor(ty, span)),
            }
        }
        let mut total = 0u32;
        for &arg in args {
            let from = self.value_of(arg)?;
            let (form, width) = match self.form(from) {
                Some((Form::Scalar, _)) => (Form::Scalar, 1),
                Some((Form::Vector(m), _)) => (Form::Vector(m), u32::from(m)),
                _ => return Err(self.no_constructor(ty, span)),
            };
            let to = self.of_form(form, scalar)?;
            self.convert_to(arg, to)?;
            total += width;
        }
        if total != u32::from(n) {
            return Err(error(Code::NoOverload, span, |m| {
                write!(m, "a vec{n} takes {n} components, not {total}")
            }));
        }
        Ok(Construct::Parts)
    }

    /// A `columns`×`rows` matrix of `scalar` from `args`: one matrix of its
    /// shape converted, its columns, or its scalars.
    fn matrix_parts(
        &mut self,
        ty: TyId,
        (columns, rows, scalar): (u8, u8, Scalar),
        args: &[ExprId],
        span: Span,
    ) -> Outcome<Construct> {
        if let [arg] = args {
            let from = self.value_of(*arg)?;
            return match self.form(from) {
                Some((Form::Matrix(c, r), s)) if c == columns && r == rows && s.is_float() => {
                    Ok(Construct::Convert)
                }
                _ => Err(self.no_constructor(ty, span)),
            };
        }
        let (part, how) = if args.len() == usize::from(columns) {
            (self.intern(Ty::Vector(rows, scalar))?, Construct::Parts)
        } else if args.len() == usize::from(columns) * usize::from(rows) {
            (self.types.scalar(scalar)?, Construct::Elements)
        } else {
            return Err(self.no_constructor(ty, span));
        };
        for &arg in args {
            self.convert_to(arg, part)?;
        }
        Ok(how)
    }

    /// A constructor of a generator's type with its template list left to
    /// inference from `args`.
    fn inferred(&mut self, generator: Generator, args: &[ExprId], span: Span) -> Outcome<ExprInfo> {
        let stage = self.stage_of(args);
        match generator {
            Generator::Vector(n) if args.is_empty() => {
                let ty = self.intern(Ty::Vector(n, Scalar::AbstractInt))?;
                Ok(Self::constructed(ty, Stage::Const, Construct::Zero))
            }
            Generator::Vector(n) => {
                let scalar = self.common_scalar(args, span)?;
                let ty = self.intern(Ty::Vector(n, scalar))?;
                let how = self.vector_parts(ty, (n, scalar), args, span)?;
                Ok(Self::constructed(ty, stage, how))
            }
            Generator::Matrix(columns, rows) if !args.is_empty() => {
                let scalar = match self.common_scalar(args, span)? {
                    Scalar::AbstractInt => Scalar::AbstractFloat,
                    other => other,
                };
                let ty = self.intern(Ty::Matrix {
                    columns,
                    rows,
                    scalar,
                })?;
                if !scalar.is_float() {
                    return Err(self.no_constructor(ty, span));
                }
                let how = self.matrix_parts(ty, (columns, rows, scalar), args, span)?;
                Ok(Self::constructed(ty, stage, how))
            }
            Generator::Array if !args.is_empty() => {
                let mut element = self.value_of(args[0])?;
                for &arg in &args[1..] {
                    let next = self.value_of(arg)?;
                    element = self.common_type(element, next).ok_or_else(|| {
                        error(Code::Mismatch, self.span_of(arg), |m| {
                            m.write_str("an array's elements share one type, not ")?;
                            self.write_type(m, element)?;
                            m.write_str(" and ")?;
                            self.write_type(m, next)
                        })
                    })?;
                }
                if !self.types.is_constructible(element)
                    || self.types.depth(element) + 1 > super::ty::MAX_TYPE_NESTING
                {
                    return Err(error(Code::Type, span, |m| {
                        m.write_str(
                            "an array is of a constructible element nested within the bound",
                        )
                    }));
                }
                let n = u32::try_from(args.len()).map_err(|_| Stop::OutOfMemory)?;
                let ty = self.intern(Ty::Array {
                    element,
                    count: Count::Fixed(n),
                })?;
                for &arg in args {
                    self.convert_to(arg, element)?;
                }
                Ok(Self::constructed(ty, stage, Construct::Parts))
            }
            _ => Err(error(Code::NoOverload, span, |m| {
                m.write_str("this constructor needs its template list")
            })),
        }
    }

    /// The scalar every argument's components convert to with the least
    /// rank.
    fn common_scalar(&mut self, args: &[ExprId], span: Span) -> Outcome<Scalar> {
        let mut common: Option<Scalar> = None;
        for &arg in args {
            let ty = self.value_of(arg)?;
            let Some((_, scalar)) = self.form(ty) else {
                return Err(error(Code::NoOverload, self.span_of(arg), |m| {
                    m.write_str("a component is a scalar, vector or matrix")
                }));
            };
            common = Some(match common {
                None => scalar,
                Some(seen) => join(seen, scalar).ok_or_else(|| {
                    error(Code::Mismatch, span, |m| {
                        write!(m, "{} and {} share no type", seen.name(), scalar.name())
                    })
                })?,
            });
        }
        common.ok_or(Stop::Internal)
    }

    /// The type values of `a` and `b` both convert to with the least rank.
    pub(crate) fn common_type(&self, a: TyId, b: TyId) -> Option<TyId> {
        if self.types.rank(a, b).is_some() {
            Some(b)
        } else if self.types.rank(b, a).is_some() {
            Some(a)
        } else {
            None
        }
    }

    /// The value of the constant constructor or built-in call `id`.
    pub(crate) fn evaluate_call(&mut self, id: ExprId, args: Run, span: Span) -> Outcome<ValueId> {
        let info = self.info(id);
        let tree: &'a crate::syntax::Tree = self.tree;
        let args = tree.expr_list(args);
        match info.denotes {
            Denotes::Construct(how) => self.evaluate_construct(info.ty, how, args, span),
            Denotes::CallBuiltin(builtin) => self.evaluate_builtin(id, builtin, args, span),
            _ => Err(Stop::Internal),
        }
    }

    /// The value a constructor of `ty` builds `how` from `args`, each
    /// evaluated and converted already.
    fn evaluate_construct(
        &mut self,
        ty: TyId,
        how: Construct,
        args: &[ExprId],
        span: Span,
    ) -> Outcome<ValueId> {
        let mut values: Vec<(ValueId, TyId)> = Vec::new();
        reserve(&mut values, args.len())?;
        for &arg in args {
            values.push((self.value_or_stop(arg)?, self.info(arg).converted));
        }
        match how {
            Construct::Zero => self.values.zero(&self.types, ty),
            Construct::Convert => {
                self.convert_explicitly(values.first().copied().ok_or(Stop::Internal)?, ty, span)
            }
            Construct::Splat => {
                let (value, _) = values.first().copied().ok_or(Stop::Internal)?;
                let n = Values::arity(&self.types, ty);
                self.values.charge(u64::from(n), span)?;
                let mut parts = Vec::new();
                reserve(&mut parts, n as usize)?;
                parts.resize(n as usize, value);
                self.values.composite(ty, &parts)
            }
            Construct::Parts => {
                let mut parts = Vec::new();
                if matches!(self.types.get(ty), Ty::Vector(..)) {
                    for &(value, part_ty) in &values {
                        if matches!(self.types.get(part_ty), Ty::Vector(..)) {
                            self.values.components(
                                &mut self.types,
                                (value, part_ty),
                                &mut parts,
                            )?;
                        } else {
                            push(&mut parts, value)?;
                        }
                    }
                } else {
                    reserve(&mut parts, values.len())?;
                    parts.extend(values.iter().map(|&(value, _)| value));
                }
                self.values.charge(parts.len() as u64, span)?;
                self.values.composite(ty, &parts)
            }
            Construct::Elements => {
                let Ty::Matrix { rows, scalar, .. } = self.types.get(ty) else {
                    return Err(Stop::Internal);
                };
                let column = self.intern(Ty::Vector(rows, scalar))?;
                let mut columns = Vec::new();
                reserve(&mut columns, values.len() / usize::from(rows.max(1)))?;
                let mut cells = Vec::new();
                for chunk in values.chunks(usize::from(rows)) {
                    cells.clear();
                    reserve(&mut cells, chunk.len())?;
                    cells.extend(chunk.iter().map(|&(value, _)| value));
                    columns.push(self.values.composite(column, &cells)?);
                }
                self.values.charge(values.len() as u64, span)?;
                self.values.composite(ty, &columns)
            }
        }
    }

    /// `value` of its type converted explicitly to `to`, component by
    /// component: a scalar's conversion, lifted through vectors and
    /// matrices of one shape.
    fn convert_explicitly(
        &mut self,
        (value, from): (ValueId, TyId),
        to: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        if from == to {
            return Ok(value);
        }
        if let Ty::Scalar(target) = self.types.get(to) {
            self.values.charge(1, span)?;
            let converted = construct_scalar(self.values.get(value), target, span)?;
            return self.values.add(converted);
        }
        let mut parts = Vec::new();
        self.values
            .components(&mut self.types, (value, from), &mut parts)?;
        let part_from = Values::component_type(&mut self.types, from, 0)?;
        let part_to = Values::component_type(&mut self.types, to, 0)?;
        for part in &mut parts {
            *part = self.convert_explicitly((*part, part_from), part_to, span)?;
        }
        self.values.composite(to, &parts)
    }
}
