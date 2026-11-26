//! Expressions: each root typed over the run of nodes its subtree holds,
//! children first, every parent deciding how it takes each child — the load
//! rule reading a memory view, an abstract value converted to what the
//! operation's overload asks — and then every constant among them evaluated
//! in the same order, the side of a constant short-circuit its other side
//! decides left unevaluated, as the specification requires.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::{Checker, Denotes, ExprInfo, LocalKind, Name, Stage, NONE};
use crate::diag::{error, Code, Outcome, Span, Stop};
use crate::eval::{Value, ValueId, Values};
use crate::lex::number;
use crate::predeclared::Predeclared;
use crate::syntax::{BinaryOp, ExprId, ExprKind, Ident, Literal, UnaryOp};
use crate::types::{result_members, Count, Holds, Origin, Scalar, Space, Ty, TyId};

impl Denotes {
    /// Whether it is a value or names one.
    pub(crate) const fn is_value(self) -> bool {
        !matches!(
            self,
            Self::Type
                | Self::Space(_)
                | Self::Access(_)
                | Self::Format(_)
                | Self::Function(_)
                | Self::Builtin(_)
        )
    }
}

/// The shape of a numeric value's type: scalar, vector or matrix.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Form {
    Scalar,
    Vector(u8),
    Matrix(u8, u8),
}

impl Checker<'_> {
    /// Resolve expression `root` and everything within it, evaluating its
    /// const-expressions; its override-expressions wait for their values.
    pub(crate) fn expression(&mut self, root: ExprId) -> Outcome<ExprInfo> {
        for raw in self.shape.subtree(root) {
            let id = ExprId(raw);
            let info = self.node(id)?;
            self.set(id, info);
        }
        self.evaluate_range(root, Stage::Const)?;
        Ok(self.info(root))
    }

    /// Evaluate every value of `root`'s subtree known by `upto` and not yet
    /// evaluated, children first, skipping the right side of a `&&` or `||`
    /// whose left side's value decides it.
    pub(crate) fn evaluate_range(&mut self, root: ExprId, upto: Stage) -> Outcome<()> {
        let mut skip_below = 0u32;
        for raw in self.shape.subtree(root) {
            if raw < skip_below {
                continue;
            }
            let id = ExprId(raw);
            let info = self.info(id);
            if info.value == NONE && info.stage <= upto && info.denotes.is_value() {
                self.evaluate(id)?;
            }
            if let Some(parent) = self.decided_parent(id) {
                skip_below = skip_below.max(parent.0);
            }
        }
        Ok(())
    }

    /// The short-circuit `id` is the left side of, where `id`'s value
    /// decides it.
    fn decided_parent(&self, id: ExprId) -> Option<ExprId> {
        let parent = self.shape.parent(id)?;
        let Some(ExprKind::Binary(op, lhs, _)) = self.tree.expr(parent).map(|e| e.kind) else {
            return None;
        };
        if lhs != id {
            return None;
        }
        let value = self.values.boolean(self.info(id).value_id()?)?;
        match op {
            BinaryOp::LogicalAnd if !value => Some(parent),
            BinaryOp::LogicalOr if value => Some(parent),
            _ => None,
        }
    }

    /// Evaluate the const-expression `id` now, where its value decides a
    /// type or a shader-creation error: its nodes are typed already.
    pub(crate) fn force(&mut self, id: ExprId) -> Outcome<()> {
        if self.info(id).value == NONE {
            self.evaluate_range(id, Stage::Const)?;
        }
        Ok(())
    }

    /// The value of a constant `id` whose children are evaluated, converted
    /// to the type its use takes it as.
    fn evaluate(&mut self, id: ExprId) -> Outcome<()> {
        let info = self.info(id);
        let span = self.span_of(id);
        let Some(expr) = self.tree.expr(id) else {
            return Ok(());
        };
        let raw = match expr.kind {
            ExprKind::Literal(_) => return Ok(()),
            ExprKind::Ident(ident) => self.named_value(ident, span)?,
            ExprKind::Unary(op, operand) => {
                let operand_value = self.value_or_stop(operand)?;
                let operand_ty = self.info(operand).converted;
                self.values
                    .unary(&mut self.types, op, (operand_value, operand_ty), span)?
            }
            ExprKind::Binary(op, lhs, rhs) => {
                self.evaluate_binary(op, (lhs, rhs), info.ty, span)?
            }
            ExprKind::Member(base, _) => self.evaluate_member(base, info, span)?,
            ExprKind::Index(base, index) => {
                let base_value = self.value_or_stop(base)?;
                let base_ty = self.info(base).converted;
                let at = self.value_or_stop(index)?;
                let at = self
                    .values
                    .integer(at)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or(Stop::Internal)?;
                self.values.charge(1, span)?;
                self.values
                    .component(&mut self.types, (base_value, base_ty), at)?
            }
            ExprKind::Call(_, args) => self.evaluate_call(id, args, span)?,
        };
        let value = if info.converted == info.ty {
            raw
        } else {
            self.values
                .convert(&mut self.types, (raw, info.ty), info.converted, span)?
        };
        let mut updated = info;
        updated.value = value.0;
        self.set(id, updated);
        Ok(())
    }

    /// The value of child `id`, which a constant parent needs evaluated.
    pub(crate) fn value_or_stop(&self, id: ExprId) -> Outcome<ValueId> {
        self.info(id).value_id().ok_or_else(|| {
            error(Code::Evaluation, self.span_of(id), |m| {
                m.write_str("this has no value when the module is made")
            })
        })
    }

    fn named_value(&self, ident: Ident, span: Span) -> Outcome<ValueId> {
        match self.resolve(ident.name) {
            Some(Name::Local(local)) => self
                .local_list
                .get(local as usize)
                .filter(|l| l.value != NONE)
                .map(|l| ValueId(l.value))
                .ok_or_else(|| self.undeclared(span)),
            Some(Name::Decl(decl)) => self.decl_value(decl, ident.name),
            _ => Err(self.undeclared(span)),
        }
    }

    fn evaluate_binary(
        &mut self,
        op: BinaryOp,
        (lhs, rhs): (ExprId, ExprId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let left = self.value_or_stop(lhs)?;
        if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            let decided = self.values.boolean(left) == Some(op == BinaryOp::LogicalOr);
            if decided {
                return Ok(left);
            }
            return self.value_or_stop(rhs);
        }
        let right = self.value_or_stop(rhs)?;
        let (lhs_ty, rhs_ty) = (self.info(lhs).converted, self.info(rhs).converted);
        self.values.binary(
            &mut self.types,
            op,
            (left, lhs_ty),
            (right, rhs_ty),
            result,
            span,
        )
    }

    fn evaluate_member(&mut self, base: ExprId, info: ExprInfo, span: Span) -> Outcome<ValueId> {
        let base_value = self.value_or_stop(base)?;
        let base_ty = self.info(base).converted;
        match info.denotes {
            Denotes::Member(index) => {
                self.values.charge(1, span)?;
                self.values
                    .component(&mut self.types, (base_value, base_ty), index)
            }
            Denotes::Swizzle(lanes, count) => {
                self.values.charge(u64::from(count), span)?;
                let mut parts = Vec::new();
                for &lane in lanes.iter().take(usize::from(count)) {
                    let part = self.values.component(
                        &mut self.types,
                        (base_value, base_ty),
                        u32::from(lane),
                    )?;
                    crate::diag::push(&mut parts, part)?;
                }
                if count == 1 {
                    parts.first().copied().ok_or(Stop::Internal)
                } else {
                    self.values.composite(info.ty, &parts)
                }
            }
            _ => Err(Stop::Internal),
        }
    }

    /// The type a value of `ty` is read as: a memory view's store type,
    /// anything else itself.
    pub(crate) fn load_type(&self, ty: TyId) -> TyId {
        match self.types.get(ty) {
            Ty::Reference { store, .. } => store,
            _ => ty,
        }
    }

    /// Take `id` as a value, the load rule reading it where it is a memory
    /// view: its value type.
    pub(crate) fn value_of(&mut self, id: ExprId) -> Outcome<TyId> {
        let info = self.info(id);
        let span = self.span_of(id);
        if !info.denotes.is_value() {
            return Err(error(Code::Misused, span, |m| match info.denotes {
                Denotes::Type => m.write_str("a type is not a value"),
                Denotes::Function(_) | Denotes::Builtin(_) => {
                    m.write_str("a function is not a value; call it")
                }
                _ => m.write_str("an enumerant is not a value"),
            }));
        }
        if self.types.get(info.ty) == Ty::Void {
            return Err(error(Code::Misused, span, |m| {
                m.write_str("this call returns no value")
            }));
        }
        let ty = self.load_type(info.ty);
        if ty != info.ty {
            if self.types.holds(ty, Holds::ATOMIC) {
                return Err(error(Code::Type, span, |m| {
                    m.write_str("an atomic is reached only through the atomic built-ins")
                }));
            }
            if let Some(root) = self.root_of(id) {
                self.note_access(root, false)?;
            }
            if self.types.holds(ty, Holds::RUNTIME_ARRAY) {
                return Err(error(Code::Type, span, |m| {
                    m.write_str("a runtime-sized array cannot be read whole")
                }));
            }
            let mut loaded = info;
            loaded.converted = ty;
            self.set(id, loaded);
        }
        Ok(ty)
    }

    /// Take `id` as a value of `to`, converted automatically where its own
    /// type converts.
    pub(crate) fn convert_to(&mut self, id: ExprId, to: TyId) -> Outcome<()> {
        let from = self.value_of(id)?;
        if from == to {
            return Ok(());
        }
        if self.types.rank(from, to).is_none() {
            return Err(self.mismatch(self.span_of(id), to, from));
        }
        let mut info = self.info(id);
        info.converted = to;
        if let Some(value) = info.value_id() {
            let span = self.span_of(id);
            info.value = self
                .values
                .convert(&mut self.types, (value, from), to, span)?
                .0;
        }
        self.set(id, info);
        Ok(())
    }

    /// Take `id` as a concrete value: an abstract one converted as nothing
    /// asks otherwise.
    pub(crate) fn concretize(&mut self, id: ExprId) -> Outcome<TyId> {
        let from = self.value_of(id)?;
        let to = self.types.concrete(from)?;
        self.convert_to(id, to)?;
        Ok(to)
    }

    /// The form and scalar of a numeric value type.
    pub(crate) fn form(&self, ty: TyId) -> Option<(Form, Scalar)> {
        match self.types.get(ty) {
            Ty::Scalar(s) => Some((Form::Scalar, s)),
            Ty::Vector(n, s) => Some((Form::Vector(n), s)),
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => Some((Form::Matrix(columns, rows), scalar)),
            _ => None,
        }
    }

    /// The type of `form` over `scalar`.
    pub(crate) fn of_form(&mut self, form: Form, scalar: Scalar) -> Outcome<TyId> {
        match form {
            Form::Scalar => self.types.scalar(scalar),
            Form::Vector(n) => self.intern(Ty::Vector(n, scalar)),
            Form::Matrix(columns, rows) => self.intern(Ty::Matrix {
                columns,
                rows,
                scalar,
            }),
        }
    }

    /// The type an expression's node has, its children resolved.
    fn node(&mut self, id: ExprId) -> Outcome<ExprInfo> {
        let span = self.span_of(id);
        let Some(expr) = self.tree.expr(id).copied() else {
            return Err(Stop::Internal);
        };
        match expr.kind {
            ExprKind::Literal(literal) => self.literal(literal, span),
            ExprKind::Ident(ident) => self.ident(ident, span),
            ExprKind::Unary(op, operand) => self.unary(op, operand, span),
            ExprKind::Binary(op, lhs, rhs) => self.binary(op, (lhs, rhs), span),
            ExprKind::Member(base, name) => self.member(base, name, span),
            ExprKind::Index(base, index) => self.index(base, index, span),
            ExprKind::Call(callee, args) => self.call(id, callee, args, span),
        }
    }

    fn literal(&mut self, literal: Literal, span: Span) -> Outcome<ExprInfo> {
        let value = match literal {
            Literal::Bool(b) => Value::Bool(b),
            Literal::Int | Literal::Float => {
                let text = self.spell(span);
                let (_, shape) = number(text.as_bytes(), 0).ok_or(Stop::Internal)?;
                crate::literal::value(text, shape, span, self.features.f16)?
            }
        };
        let scalar = match value {
            Value::Bool(_) => Scalar::Bool,
            Value::AbstractInt(_) => Scalar::AbstractInt,
            Value::AbstractFloat(_) => Scalar::AbstractFloat,
            Value::I32(_) => Scalar::I32,
            Value::U32(_) => Scalar::U32,
            Value::F32(_) => Scalar::F32,
            Value::F16(_) => Scalar::F16,
            Value::Composite { .. } | Value::Zero(_) => return Err(Stop::Internal),
        };
        let ty = self.types.scalar(scalar)?;
        let value = self.values.add(value)?;
        Ok(ExprInfo {
            ty,
            converted: ty,
            stage: Stage::Const,
            value: value.0,
            denotes: Denotes::Value,
        })
    }

    /// A name: what it resolves to, a type generator completed by its
    /// template list.
    fn ident(&mut self, ident: Ident, span: Span) -> Outcome<ExprInfo> {
        let name = self
            .resolve(ident.name)
            .ok_or_else(|| self.undeclared(ident.name))?;
        let templated = !ident.template.is_empty();
        let refuse_template = |this: &Self| {
            Err(error(Code::Type, span, |m| {
                m.quote(this.spell(ident.name))?;
                m.write_str(" takes no template list")
            }))
        };
        let info = match name {
            Name::Predeclared(Predeclared::Generator(generator)) => {
                if !templated {
                    return Err(error(Code::Type, span, |m| {
                        m.quote(self.spell(ident.name))?;
                        m.write_str(" needs a template list")
                    }));
                }
                let ty = self.generate(generator, ident.template, span)?;
                self.held_to_features(ty, span)?;
                Self::type_info(ty)
            }
            _ if templated => return refuse_template(self),
            Name::Predeclared(Predeclared::Type(named)) => {
                Self::type_info(self.named(named, ident.name)?)
            }
            Name::Predeclared(Predeclared::Space(space)) => enumerant(Denotes::Space(space)),
            Name::Predeclared(Predeclared::Access(access)) => enumerant(Denotes::Access(access)),
            Name::Predeclared(Predeclared::Format(format, _)) => enumerant(Denotes::Format(format)),
            Name::Predeclared(Predeclared::Builtin(builtin)) => {
                enumerant(Denotes::Builtin(builtin))
            }
            Name::Local(local) => self.local_info(local, span)?,
            Name::Decl(decl) => {
                self.note_reference(decl)?;
                self.decl_info(decl)?
            }
        };
        Ok(info)
    }

    fn local_info(&mut self, local: u32, span: Span) -> Outcome<ExprInfo> {
        let info = *self.local_list.get(local as usize).ok_or(Stop::Internal)?;
        self.continuing_may_name(local, span)?;
        Ok(match info.kind {
            LocalKind::Var => {
                let ty = self.intern(Ty::Reference {
                    space: Space::Function,
                    store: info.ty,
                    access: crate::types::Access::ReadWrite,
                })?;
                ExprInfo {
                    ty,
                    converted: ty,
                    stage: Stage::Runtime,
                    value: NONE,
                    denotes: Denotes::Local(local),
                }
            }
            LocalKind::Const => ExprInfo {
                ty: info.ty,
                converted: info.ty,
                stage: Stage::Const,
                value: info.value,
                denotes: Denotes::Local(local),
            },
            LocalKind::Let | LocalKind::Param(_) => ExprInfo {
                ty: info.ty,
                converted: info.ty,
                stage: Stage::Runtime,
                value: NONE,
                denotes: Denotes::Local(local),
            },
        })
    }

    fn unary(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> Outcome<ExprInfo> {
        let child = self.info(operand);
        match op {
            UnaryOp::AddressOf => {
                let Ty::Reference {
                    space,
                    store,
                    access,
                } = self.types.get(child.ty)
                else {
                    return Err(error(Code::Pointer, span, |m| {
                        m.write_str("`&` takes the address of memory, a variable or part of one")
                    }));
                };
                if space == Space::Handle {
                    return Err(error(Code::Pointer, span, |m| {
                        m.write_str("a texture or sampler has no address")
                    }));
                }
                if self.is_vector_component(operand) {
                    return Err(error(Code::Pointer, span, |m| {
                        m.write_str("a vector's component has no address")
                    }));
                }
                let ty = self.intern(Ty::Pointer {
                    space,
                    store,
                    access,
                })?;
                Ok(runtime(ty, Denotes::Value))
            }
            UnaryOp::Indirection => {
                let Ty::Pointer {
                    space,
                    store,
                    access,
                } = self.types.get(child.ty)
                else {
                    return Err(error(Code::Pointer, span, |m| {
                        m.write_str("`*` follows a pointer")
                    }));
                };
                let ty = self.intern(Ty::Reference {
                    space,
                    store,
                    access,
                })?;
                Ok(runtime(ty, Denotes::Value))
            }
            UnaryOp::Negate | UnaryOp::Not | UnaryOp::Complement => {
                let ty = self.value_of(operand)?;
                let allowed = self.form(ty).is_some_and(|(form, scalar)| {
                    !matches!(form, Form::Matrix(..))
                        && match op {
                            UnaryOp::Not => scalar == Scalar::Bool,
                            UnaryOp::Complement => scalar.is_integer(),
                            _ => scalar.is_numeric() && scalar.is_signed(),
                        }
                });
                if !allowed {
                    return Err(error(Code::Mismatch, span, |m| {
                        m.write_str("this operator does not apply to ")?;
                        self.write_type(m, ty)
                    }));
                }
                Ok(ExprInfo {
                    ty,
                    converted: ty,
                    stage: self.info(operand).stage,
                    value: NONE,
                    denotes: Denotes::Value,
                })
            }
        }
    }

    /// Whether `id` names one component of a vector in memory: a swizzle of
    /// one letter, or an index, into a vector memory view.
    fn is_vector_component(&self, id: ExprId) -> bool {
        let Some(ExprKind::Member(base, _) | ExprKind::Index(base, _)) =
            self.tree.expr(id).map(|e| e.kind)
        else {
            return false;
        };
        let store = self
            .types
            .store_of(self.info(base).ty)
            .unwrap_or_else(|| self.info(base).ty);
        matches!(self.types.get(store), Ty::Vector(..))
    }

    pub(crate) fn binary(
        &mut self,
        op: BinaryOp,
        (lhs, rhs): (ExprId, ExprId),
        span: Span,
    ) -> Outcome<ExprInfo> {
        let stage = self.info(lhs).stage.max(self.info(rhs).stage);
        if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            let boolean = self.types.scalar(Scalar::Bool)?;
            self.convert_to(lhs, boolean)?;
            self.convert_to(rhs, boolean)?;
            return Ok(value_info(boolean, stage));
        }
        let left = self.value_of(lhs)?;
        let right = self.value_of(rhs)?;
        let refuse = |this: &Self| {
            error(Code::Mismatch, span, |m| {
                m.write_str("this operator does not apply to ")?;
                this.write_type(m, left)?;
                m.write_str(" and ")?;
                this.write_type(m, right)
            })
        };
        let (Some((left_form, a)), Some((right_form, b))) = (self.form(left), self.form(right))
        else {
            return Err(refuse(self));
        };
        if matches!(op, BinaryOp::ShiftLeft | BinaryOp::ShiftRight) {
            return self.shift((lhs, rhs), (left_form, a), (right_form, b), stage, span);
        }
        let scalar = join(a, b).ok_or_else(|| refuse(self))?;
        let (lhs_ty, rhs_ty, result) = match op {
            BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Remainder => {
                if scalar == Scalar::Bool {
                    return Err(refuse(self));
                }
                self.arithmetic(op, (left_form, right_form), scalar)?
                    .ok_or_else(|| refuse(self))?
            }
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                let fits = left_form == right_form
                    && !matches!(left_form, Form::Matrix(..))
                    && (scalar.is_integer() || (scalar == Scalar::Bool && op != BinaryOp::Xor));
                if !fits {
                    return Err(refuse(self));
                }
                let ty = self.of_form(left_form, scalar)?;
                (ty, ty, ty)
            }
            _ => {
                let ordered = !matches!(op, BinaryOp::Equal | BinaryOp::NotEqual);
                let fits = left_form == right_form
                    && !matches!(left_form, Form::Matrix(..))
                    && (!ordered || scalar != Scalar::Bool);
                if !fits {
                    return Err(refuse(self));
                }
                let ty = self.of_form(left_form, scalar)?;
                let result = self.of_form(left_form, Scalar::Bool)?;
                (ty, ty, result)
            }
        };
        self.convert_to(lhs, lhs_ty)?;
        self.convert_to(rhs, rhs_ty)?;
        if matches!(op, BinaryOp::Divide | BinaryOp::Remainder) && scalar.is_integer() {
            self.refuse_zero_divisor(rhs)?;
        }
        Ok(value_info(result, stage))
    }

    /// The operand and result types of an arithmetic operator over forms
    /// `forms` of `scalar`: a scalar operand beside a vector or a matrix is
    /// taken as that scalar, and matrices multiply as linear algebra.
    fn arithmetic(
        &mut self,
        op: BinaryOp,
        forms: (Form, Form),
        scalar: Scalar,
    ) -> Outcome<Option<(TyId, TyId, TyId)>> {
        let mut ty = |form| self.of_form(form, scalar);
        Ok(Some(match forms {
            (same, other) if same == other && !matches!(same, Form::Matrix(..)) => {
                let both = ty(same)?;
                (both, both, both)
            }
            (Form::Vector(lanes), Form::Scalar) => {
                let vector = ty(Form::Vector(lanes))?;
                (vector, ty(Form::Scalar)?, vector)
            }
            (Form::Scalar, Form::Vector(lanes)) => {
                let vector = ty(Form::Vector(lanes))?;
                (ty(Form::Scalar)?, vector, vector)
            }
            _ if !scalar.is_float() => return Ok(None),
            (Form::Matrix(columns, rows), Form::Matrix(other_columns, other_rows))
                if columns == other_columns
                    && rows == other_rows
                    && matches!(op, BinaryOp::Add | BinaryOp::Subtract) =>
            {
                let matrix = ty(Form::Matrix(columns, rows))?;
                (matrix, matrix, matrix)
            }
            _ if op != BinaryOp::Multiply => return Ok(None),
            (Form::Matrix(columns, rows), Form::Scalar) => {
                let matrix = ty(Form::Matrix(columns, rows))?;
                (matrix, ty(Form::Scalar)?, matrix)
            }
            (Form::Scalar, Form::Matrix(columns, rows)) => {
                let matrix = ty(Form::Matrix(columns, rows))?;
                (ty(Form::Scalar)?, matrix, matrix)
            }
            (Form::Matrix(columns, rows), Form::Vector(lanes)) if lanes == columns => (
                ty(Form::Matrix(columns, rows))?,
                ty(Form::Vector(columns))?,
                ty(Form::Vector(rows))?,
            ),
            (Form::Vector(lanes), Form::Matrix(columns, rows)) if lanes == rows => (
                ty(Form::Vector(rows))?,
                ty(Form::Matrix(columns, rows))?,
                ty(Form::Vector(columns))?,
            ),
            (Form::Matrix(inner, rows), Form::Matrix(columns, depth)) if inner == depth => (
                ty(Form::Matrix(inner, rows))?,
                ty(Form::Matrix(columns, inner))?,
                ty(Form::Matrix(columns, rows))?,
            ),
            _ => return Ok(None),
        }))
    }

    /// Refuse an integer division or remainder whose divisor is a
    /// const-expression holding a zero, whatever the dividend.
    fn refuse_zero_divisor(&mut self, divisor: ExprId) -> Outcome<()> {
        if self.info(divisor).stage != Stage::Const {
            return Ok(());
        }
        self.force(divisor)?;
        self.zero_divisor(divisor)
    }

    /// Refuse the evaluated integer divisor `divisor` where a component of
    /// it is zero.
    pub(crate) fn zero_divisor(&mut self, divisor: ExprId) -> Outcome<()> {
        let value = self.value_or_stop(divisor)?;
        let ty = self.info(divisor).converted;
        if self.any_component(value, ty, |n| n == 0)? {
            return Err(error(Code::Evaluation, self.span_of(divisor), |m| {
                m.write_str("an integer division or remainder by zero")
            }));
        }
        Ok(())
    }

    /// Whether any integer component of the value `id` of type `ty` is one
    /// `pick` accepts.
    pub(crate) fn any_component(
        &mut self,
        id: ValueId,
        ty: TyId,
        pick: fn(i128) -> bool,
    ) -> Outcome<bool> {
        if let Some(n) = self.values.integer(id) {
            return Ok(pick(n));
        }
        let mut parts = Vec::new();
        self.values
            .components(&mut self.types, (id, ty), &mut parts)?;
        Ok(parts
            .iter()
            .any(|&p| self.values.integer(p).is_some_and(pick)))
    }

    /// A shift: an integer of any width shifted by `u32`s of its form, a
    /// constant amount at or past a concrete width refused.
    fn shift(
        &mut self,
        (lhs, rhs): (ExprId, ExprId),
        (left_form, a): (Form, Scalar),
        (right_form, b): (Form, Scalar),
        stage: Stage,
        span: Span,
    ) -> Outcome<ExprInfo> {
        let fits = left_form == right_form
            && !matches!(left_form, Form::Matrix(..))
            && a.is_integer()
            && matches!(b, Scalar::AbstractInt | Scalar::U32);
        if !fits {
            return Err(error(Code::Mismatch, span, |m| {
                m.write_str("a shift takes an integer and u32 amounts of its form")
            }));
        }
        // An abstract operand shifted by a run-time amount is made concrete.
        let shifted = if a.is_abstract() && self.info(rhs).stage == Stage::Runtime {
            self.concretize(lhs)?
        } else {
            self.value_of(lhs)?
        };
        let amount = self.of_form(right_form, Scalar::U32)?;
        self.convert_to(rhs, amount)?;
        if !a.is_abstract() && self.info(rhs).stage == Stage::Const {
            self.force(rhs)?;
            self.shift_amount(rhs)?;
        }
        Ok(value_info(shifted, stage.max(self.info(lhs).stage)))
    }

    /// Refuse the evaluated amount `rhs` of a concrete shift where a
    /// component of it is not less than the width.
    pub(crate) fn shift_amount(&mut self, rhs: ExprId) -> Outcome<()> {
        let value = self.value_or_stop(rhs)?;
        let ty = self.info(rhs).converted;
        if self.any_component(value, ty, |n| n >= 32)? {
            return Err(error(Code::Evaluation, self.span_of(rhs), |m| {
                m.write_str("a shift amount is not less than the width, 32")
            }));
        }
        Ok(())
    }

    /// A member of a structure, or a swizzle of a vector: of a value, or of
    /// a memory view, which a pointer is taken as.
    fn member(&mut self, base: ExprId, name: Span, span: Span) -> Outcome<ExprInfo> {
        let base_info = self.info(base);
        let (view, store) = match self.types.get(base_info.ty) {
            Ty::Reference {
                space,
                store,
                access,
            }
            | Ty::Pointer {
                space,
                store,
                access,
            } => (Some((space, access)), store),
            _ => {
                if !base_info.denotes.is_value() {
                    return Err(error(Code::Misused, span, |m| {
                        m.write_str("only a value has members")
                    }));
                }
                (None, base_info.ty)
            }
        };
        match self.types.get(store) {
            Ty::Struct(structure) => {
                let index = self.member_index(structure, name).ok_or_else(|| {
                    error(Code::Undeclared, name, |m| {
                        m.write_str("no member ")?;
                        m.quote(self.spell(name))?;
                        m.write_str(" in ")?;
                        self.write_type(m, store)
                    })
                })?;
                let member_ty = self
                    .types
                    .members_of(structure)
                    .get(index as usize)
                    .map(|m| m.ty)
                    .ok_or(Stop::Internal)?;
                let ty = match view {
                    Some((space, access)) => self.intern(Ty::Reference {
                        space,
                        store: member_ty,
                        access,
                    })?,
                    None => member_ty,
                };
                let stage = if view.is_some() {
                    Stage::Runtime
                } else {
                    base_info.stage
                };
                Ok(ExprInfo {
                    ty,
                    converted: ty,
                    stage,
                    value: NONE,
                    denotes: Denotes::Member(index),
                })
            }
            Ty::Vector(n, scalar) => {
                let (lanes, count) = swizzle(self.spell(name), n).ok_or_else(|| {
                    error(Code::Undeclared, name, |m| {
                        m.quote(self.spell(name))?;
                        write!(m, " is not a swizzle of a vec{n}")
                    })
                })?;
                if view.is_none() {
                    self.value_of(base)?;
                }
                let component = if count == 1 {
                    self.types.scalar(scalar)?
                } else {
                    self.intern(Ty::Vector(count, scalar))?
                };
                let (ty, stage) = match view {
                    // One component of a memory view is a view of it; more
                    // are read as a vector.
                    Some((space, access)) if count == 1 => (
                        self.intern(Ty::Reference {
                            space,
                            store: component,
                            access,
                        })?,
                        Stage::Runtime,
                    ),
                    Some(_) => (component, Stage::Runtime),
                    None => (component, base_info.stage),
                };
                Ok(ExprInfo {
                    ty,
                    converted: ty,
                    stage,
                    value: NONE,
                    denotes: Denotes::Swizzle(lanes, count),
                })
            }
            _ => Err(error(Code::Mismatch, span, |m| {
                m.write_str("no members in ")?;
                self.write_type(m, store)
            })),
        }
    }

    /// The index of the member `name` names in `structure`: a declared
    /// structure's by its sorted symbols, a built-in result's by its fixed
    /// names.
    fn member_index(&self, structure: u32, name: Span) -> Option<u32> {
        match self.types.structs.get(structure as usize)?.origin {
            Origin::Declared(_) => {
                let symbol = self.symbols.of(name)?.0;
                let at = self
                    .member_table
                    .binary_search_by(|&(s, sym, _)| (s, sym).cmp(&(structure, symbol)))
                    .ok()?;
                Some(self.member_table[at].2)
            }
            origin => {
                let wanted = self.spell(name);
                let at = result_members(origin)?.iter().position(|n| *n == wanted)?;
                u32::try_from(at).ok()
            }
        }
    }

    /// An index into an array, vector or matrix: a value's element, or a
    /// view of a memory view's.
    fn index(&mut self, base: ExprId, index: ExprId, span: Span) -> Outcome<ExprInfo> {
        let base_info = self.info(base);
        let index_ty = self.value_of(index)?;
        let index_ty = match self.types.get(index_ty) {
            Ty::Scalar(Scalar::AbstractInt) => self.types.scalar(Scalar::I32)?,
            Ty::Scalar(Scalar::I32 | Scalar::U32) => index_ty,
            _ => {
                return Err(error(Code::Mismatch, self.span_of(index), |m| {
                    m.write_str("an index is an i32 or a u32")
                }))
            }
        };
        self.convert_to(index, index_ty)?;
        let (view, store) = match self.types.get(base_info.ty) {
            Ty::Reference {
                space,
                store,
                access,
            }
            | Ty::Pointer {
                space,
                store,
                access,
            } => (Some((space, access)), store),
            _ => (None, base_info.ty),
        };
        let (element, length) = match self.types.get(store) {
            Ty::Array { element, count } => (element, count),
            Ty::Vector(n, scalar) => (self.types.scalar(scalar)?, Count::Fixed(u32::from(n))),
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => (
                self.intern(Ty::Vector(rows, scalar))?,
                Count::Fixed(u32::from(columns)),
            ),
            _ => {
                return Err(error(Code::Mismatch, span, |m| {
                    m.write_str("only an array, a vector or a matrix is indexed, not ")?;
                    self.write_type(m, store)
                }))
            }
        };
        let index_stage = self.info(index).stage;
        if index_stage == Stage::Const {
            self.force(index)?;
            // An override-sized count is known once overrides take values.
            let bound = match length {
                Count::Fixed(n) => Some(n),
                _ => None,
            };
            self.index_bound(index, bound)?;
        }
        if let Some((space, access)) = view {
            let reference = self.intern(Ty::Reference {
                space,
                store: element,
                access,
            })?;
            return Ok(value_info(reference, Stage::Runtime));
        }
        if !base_info.denotes.is_value() {
            return Err(error(Code::Misused, span, |m| {
                m.write_str("only a value is indexed")
            }));
        }
        // An abstract composite indexed at run time is made concrete first.
        if index_stage == Stage::Runtime && self.types.is_abstract(store) {
            let concrete = self.concretize(base)?;
            let element = Values::component_type(&mut self.types, concrete, 0)?;
            return Ok(value_info(element, Stage::Runtime));
        }
        self.value_of(base)?;
        Ok(value_info(element, base_info.stage.max(index_stage)))
    }

    /// Refuse the evaluated index `index` where it is negative or, for a
    /// known `bound`, not below it.
    pub(crate) fn index_bound(&self, index: ExprId, bound: Option<u32>) -> Outcome<()> {
        let at = self.integer_value(index)?;
        if at < 0 || bound.is_some_and(|n| at >= i128::from(n)) {
            return Err(error(Code::Evaluation, self.span_of(index), |m| {
                write!(m, "the index {at} is out of bounds")
            }));
        }
        Ok(())
    }
}

/// A value of type `ty` computed at `stage`.
pub(crate) const fn value_info(ty: TyId, stage: Stage) -> ExprInfo {
    ExprInfo {
        ty,
        converted: ty,
        stage,
        value: NONE,
        denotes: Denotes::Value,
    }
}

/// A run-time value of type `ty` that `denotes` names.
const fn runtime(ty: TyId, denotes: Denotes) -> ExprInfo {
    ExprInfo {
        ty,
        converted: ty,
        stage: Stage::Runtime,
        value: NONE,
        denotes,
    }
}

/// An enumerant or other name that is no value.
pub(crate) const fn enumerant(denotes: Denotes) -> ExprInfo {
    ExprInfo {
        ty: TyId(0),
        converted: TyId(0),
        stage: Stage::Const,
        value: NONE,
        denotes,
    }
}

/// The scalar two operands meet at: one, or the concrete one an abstract
/// other converts to, or for two abstract ones the float where either is.
pub(crate) const fn join(a: Scalar, b: Scalar) -> Option<Scalar> {
    if a as u8 == b as u8 {
        return Some(a);
    }
    match (a.is_abstract(), b.is_abstract()) {
        (true, false) => match a.rank_to(b) {
            Some(_) => Some(b),
            None => None,
        },
        (false, true) => match b.rank_to(a) {
            Some(_) => Some(a),
            None => None,
        },
        (true, true) => Some(Scalar::AbstractFloat),
        (false, false) => None,
    }
}

/// A swizzle's components of a vector of `n`: one to four letters, all of
/// `xyzw` or all of `rgba`, each within the vector.
pub(crate) fn swizzle(name: &str, n: u8) -> Option<([u8; 4], u8)> {
    let mut lanes = [0u8; 4];
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let set: &[u8; 4] = if b"xyzw".contains(&bytes[0]) {
        b"xyzw"
    } else {
        b"rgba"
    };
    for (lane, byte) in lanes.iter_mut().zip(bytes) {
        let at = set.iter().position(|c| c == byte)?;
        let at = u8::try_from(at).ok()?;
        if at >= n {
            return None;
        }
        *lane = at;
    }
    Some((lanes, u8::try_from(bytes.len()).ok()?))
}
