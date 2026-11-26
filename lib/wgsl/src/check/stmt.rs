//! Function bodies: every statement checked in its scope and given its
//! behaviour — what control may leave it by — as the specification's
//! behaviour analysis has it, with the placement rules for `break`,
//! `continue`, `return` and `discard`.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::decl::{DeclInfo, Signature};
use super::{Checker, Denotes, LocalInfo, LocalKind, MemoryAccess, Root, Stage, NONE};
use crate::diag::{error, push, Code, Outcome, Span, Stop};
use crate::predeclared::Builtin;
use crate::syntax::{
    BinaryOp, Clause, DeclKind, ExprId, ExprKind, FnDecl, Run, StmtId, StmtKind, ValueDecl, VarDecl,
};
use crate::types::{Access, Scalar, Space, Ty, TyId};

/// A statement's behaviour: the ways control may leave it.
pub(crate) const NEXT: u8 = 1;
pub(crate) const RETURN: u8 = 1 << 1;
pub(crate) const BREAK: u8 = 1 << 2;
pub(crate) const CONTINUE: u8 = 1 << 3;

/// The most bytes of function-scope variables one function declares: the
/// specification's minimum limit.
pub(crate) const MAX_FUNCTION_BYTES: u64 = 8192;

/// A construct control flows within, innermost last.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Flow {
    /// A loop: where its body's locals begin, and where the first
    /// `continue` aimed at it stands.
    Loop {
        body_locals: u32,
        first_continue: u32,
    },
    Switch,
    /// A loop's `continuing` block: the run of locals its loop's body
    /// declared, and where the first `continue` aimed at that loop stands.
    Continuing {
        body_locals: (u32, u32),
        first_continue: u32,
    },
}

impl<'a> Checker<'a> {
    /// Check every function's body.
    pub(crate) fn bodies(&mut self) -> Outcome<()> {
        let tree: &'a crate::syntax::Tree = self.tree;
        for (index, decl) in tree.decls.iter().enumerate() {
            if let DeclKind::Fn(function) = decl.kind {
                let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
                self.function_body(index, function)?;
            }
        }
        Ok(())
    }

    /// One function's body: its parameters bound in the body's own scope,
    /// its behaviour what its signature requires.
    fn function_body(&mut self, decl: u32, function: FnDecl) -> Outcome<()> {
        let Some(&DeclInfo::Function(signature)) = self.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        self.current_fn = decl;
        self.function_bytes = 0;
        let first_local = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        self.locals.open()?;
        let tree: &'a crate::syntax::Tree = self.tree;
        for (place, field) in tree.field_list(function.params).iter().enumerate() {
            let place = u32::try_from(place).map_err(|_| Stop::OutOfMemory)?;
            let param = self
                .params
                .get((signature.params.0 + place) as usize)
                .copied()
                .ok_or(Stop::Internal)?;
            let root = matches!(self.types.get(param.ty), Ty::Pointer { .. })
                .then_some(Root::Param(place));
            self.declare_local(field.name, LocalKind::Param(place), param.ty, NONE, root)?;
        }
        let Some(StmtKind::Compound(body)) = self.tree.stmt(function.body).map(|s| s.kind) else {
            return Err(Stop::Internal);
        };
        let behaviour = self.block(body)?;
        self.record_behaviour(function.body, behaviour)?;
        self.locals.close();
        self.current_fn = NONE;
        let end_local = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        push(&mut self.fn_locals, (decl, first_local, end_local))?;
        let ok = match signature.result {
            Some(_) => behaviour == RETURN,
            None => behaviour != 0 && behaviour & !(NEXT | RETURN) == 0,
        };
        if !ok {
            return Err(error(Code::Behaviour, function.name, |m| {
                if signature.result.is_some() {
                    m.write_str("every path through this function returns a value")
                } else {
                    m.write_str("this function's body never finishes")
                }
            }));
        }
        Ok(())
    }

    /// Bind a function-scope declaration of `name`, refused where its
    /// scope already declares the name.
    fn declare_local(
        &mut self,
        name: Span,
        kind: LocalKind,
        ty: TyId,
        value: u32,
        root: Option<Root>,
    ) -> Outcome<u32> {
        let symbol = self.symbols.of(name).ok_or(Stop::Internal)?;
        if self.locals.declared_here(symbol) {
            return Err(error(Code::Redeclared, name, |m| {
                m.quote(self.spell(name))?;
                m.write_str(" is declared twice in one scope")
            }));
        }
        let local = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        push(
            &mut self.local_list,
            LocalInfo {
                kind,
                ty,
                value,
                span: name,
                root,
            },
        )?;
        self.locals.bind(symbol, local)?;
        Ok(local)
    }

    /// The statements of `run` in sequence, in the current scope: control
    /// reaches each only through the one before finishing.
    fn block(&mut self, run: Run) -> Outcome<u8> {
        let tree: &'a crate::syntax::Tree = self.tree;
        let mut behaviour = NEXT;
        for &stmt in tree.stmt_list(run) {
            let this = self.statement(stmt)?;
            if behaviour & NEXT != 0 {
                behaviour = (behaviour & !NEXT) | this;
            }
        }
        Ok(behaviour)
    }

    /// A compound statement: its own scope.
    fn compound(&mut self, id: StmtId) -> Outcome<u8> {
        let stmt = *self.tree.stmt(id).ok_or(Stop::Internal)?;
        self.statement_attrs(stmt.attrs)?;
        let StmtKind::Compound(run) = stmt.kind else {
            return Err(Stop::Internal);
        };
        self.locals.open()?;
        let behaviour = self.block(run)?;
        self.locals.close();
        self.record_behaviour(id, behaviour)
    }

    /// Keep `local` as the one declaration statement `id` makes.
    fn record_local(&mut self, id: StmtId, local: u32) -> Outcome<()> {
        *self
            .stmt_local
            .get_mut(id.0 as usize)
            .ok_or(Stop::Internal)? = local;
        Ok(())
    }

    /// Keep `behaviour` as statement `id`'s, for the uniformity analysis.
    fn record_behaviour(&mut self, id: StmtId, behaviour: u8) -> Outcome<u8> {
        *self
            .stmt_behaviour
            .get_mut(id.0 as usize)
            .ok_or(Stop::Internal)? = behaviour;
        Ok(behaviour)
    }

    /// Refuse a statement attribute but `@diagnostic`, which the
    /// statements that take attributes may carry.
    fn statement_attrs(&self, run: Run) -> Outcome<()> {
        for attr in self.tree.attr_list(run) {
            if self.spell(attr.name) != "diagnostic" {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("a statement takes no attribute but @diagnostic")
                }));
            }
        }
        Ok(())
    }

    /// One statement and its behaviour.
    fn statement(&mut self, id: StmtId) -> Outcome<u8> {
        let behaviour = self.statement_kind(id)?;
        self.record_behaviour(id, behaviour)
    }

    #[expect(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn statement_kind(&mut self, id: StmtId) -> Outcome<u8> {
        let stmt = *self.tree.stmt(id).ok_or(Stop::Internal)?;
        let takes_attrs = matches!(
            stmt.kind,
            StmtKind::Compound(_)
                | StmtKind::If { .. }
                | StmtKind::Switch { .. }
                | StmtKind::Loop { .. }
                | StmtKind::For { .. }
                | StmtKind::While { .. }
        );
        if !takes_attrs {
            if let Some(attr) = self.tree.attr_list(stmt.attrs).first() {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("this statement takes no attribute")
                }));
            }
        }
        match stmt.kind {
            StmtKind::Compound(_) => self.compound(id),
            StmtKind::Assign { lhs, op, rhs } => {
                self.assignment(lhs, op, rhs, stmt.span)?;
                Ok(NEXT)
            }
            StmtKind::Phony(expr) => {
                self.expression(expr)?;
                let ty = self.value_of(expr)?;
                let usable = self.types.is_constructible(ty)
                    || matches!(
                        self.types.get(ty),
                        Ty::Pointer { .. } | Ty::Texture(_) | Ty::Sampler { .. }
                    );
                if !usable {
                    return Err(error(Code::Type, self.span_of(expr), |m| {
                        m.write_str("a phony assignment takes a constructible value, a pointer, a texture or a sampler")
                    }));
                }
                self.concretize(expr)?;
                Ok(NEXT)
            }
            StmtKind::Increment(target) | StmtKind::Decrement(target) => {
                self.expression(target)?;
                let store = self.written(target, true)?;
                if !matches!(self.types.get(store), Ty::Scalar(Scalar::I32 | Scalar::U32)) {
                    return Err(error(Code::Mismatch, self.span_of(target), |m| {
                        m.write_str("an increment or decrement takes an i32 or u32 in memory")
                    }));
                }
                Ok(NEXT)
            }
            StmtKind::If { .. } => self.if_chain(id),
            StmtKind::Switch {
                selector,
                body_attrs,
                clauses,
            } => {
                self.statement_attrs(stmt.attrs)?;
                self.statement_attrs(body_attrs)?;
                self.switch(selector, clauses)
            }
            StmtKind::Loop {
                body_attrs,
                body,
                continuing,
            } => {
                self.statement_attrs(stmt.attrs)?;
                self.statement_attrs(body_attrs)?;
                self.loop_statement(id, (body, continuing), stmt.span)
            }
            StmtKind::For {
                init,
                condition,
                update,
                body,
            } => {
                self.statement_attrs(stmt.attrs)?;
                self.for_statement(id, (init, condition, update), body)
            }
            StmtKind::While { condition, body } => {
                self.statement_attrs(stmt.attrs)?;
                self.condition(condition)?;
                self.looping(id, |this| {
                    let body = this.compound(body)?;
                    Ok((sequence(BREAK | NEXT, body), NEXT))
                })
            }
            StmtKind::Break => {
                self.break_target(stmt.span)?;
                Ok(BREAK)
            }
            StmtKind::Continue => {
                self.continue_target(stmt.span)?;
                Ok(CONTINUE)
            }
            StmtKind::Discard => {
                push(
                    &mut self.stage_uses,
                    super::StageUse {
                        function: self.current_fn,
                        stages: 2,
                        span: stmt.span,
                    },
                )?;
                Ok(NEXT)
            }
            StmtKind::Return(value) => {
                self.return_statement(value, stmt.span)?;
                Ok(RETURN)
            }
            StmtKind::Call(call) => {
                self.call_statement(call)?;
                Ok(NEXT)
            }
            StmtKind::Let(value) => {
                let local = self.let_statement(value)?;
                self.record_local(id, local)?;
                Ok(NEXT)
            }
            StmtKind::Const(value) => {
                let (ty, value_id) = self.constant_value(value)?;
                let local =
                    self.declare_local(value.name, LocalKind::Const, ty, value_id.0, None)?;
                self.record_local(id, local)?;
                Ok(NEXT)
            }
            StmtKind::Var(var) => {
                let local = self.var_statement(var)?;
                self.record_local(id, local)?;
                Ok(NEXT)
            }
            StmtKind::ConstAssert(expr) => {
                self.assertion(expr)?;
                Ok(NEXT)
            }
            StmtKind::Empty => Ok(NEXT),
        }
    }

    /// A `bool` condition.
    fn condition(&mut self, expr: ExprId) -> Outcome<()> {
        self.expression(expr)?;
        let boolean = self.types.scalar(Scalar::Bool)?;
        self.convert_to(expr, boolean)
    }

    /// An `if` and its `else if` chain, taken link by link: a missing
    /// `else` finishes normally.
    fn if_chain(&mut self, first: StmtId) -> Outcome<u8> {
        // Each link and its branch's behaviour.
        let mut links: Vec<(StmtId, u8)> = Vec::new();
        let mut at = first;
        let tail = loop {
            let stmt = *self.tree.stmt(at).ok_or(Stop::Internal)?;
            let StmtKind::If {
                condition,
                accept,
                reject,
            } = stmt.kind
            else {
                return Err(Stop::Internal);
            };
            self.statement_attrs(stmt.attrs)?;
            self.condition(condition)?;
            let accepted = self.compound(accept)?;
            push(&mut links, (at, accepted))?;
            match reject.map(|r| (r, self.tree.stmt(r).map(|s| s.kind))) {
                None => break NEXT,
                Some((link, Some(StmtKind::If { .. }))) => at = link,
                Some((other, Some(StmtKind::Compound(_)))) => break self.compound(other)?,
                Some(_) => return Err(Stop::Internal),
            }
        };
        // A link behaves as its branch and the rest of the chain together.
        let mut rest = tail;
        for &(link, accepted) in links.iter().rev() {
            rest |= accepted;
            self.record_behaviour(link, rest)?;
        }
        Ok(rest)
    }

    /// A `switch`: its selector and every case value of one concrete
    /// integer type, each case a distinct const-expression, one default.
    fn switch(&mut self, selector: ExprId, clauses: Run) -> Outcome<u8> {
        self.expression(selector)?;
        let selector_ty = self.value_of(selector)?;
        let tree: &'a crate::syntax::Tree = self.tree;
        let clause_list: &[Clause] = tree.clause_list(clauses);
        let mut common = match self.types.get(selector_ty) {
            Ty::Scalar(s @ (Scalar::I32 | Scalar::U32)) => Some(s),
            Ty::Scalar(Scalar::AbstractInt) => None,
            _ => {
                return Err(error(Code::Mismatch, self.span_of(selector), |m| {
                    m.write_str("a switch selects on an i32 or a u32")
                }))
            }
        };
        let mut defaults = 0usize;
        for clause in clause_list {
            for selector_case in tree.selector_list(clause.selectors) {
                let Some(case) = selector_case.expr else {
                    defaults += 1;
                    continue;
                };
                if self.expression(case)?.stage != Stage::Const {
                    return Err(error(Code::Evaluation, self.span_of(case), |m| {
                        m.write_str("a case value is a const-expression")
                    }));
                }
                let ty = self.value_of(case)?;
                match (self.types.get(ty), common) {
                    (Ty::Scalar(Scalar::AbstractInt), _) => {}
                    (Ty::Scalar(s @ (Scalar::I32 | Scalar::U32)), None) => common = Some(s),
                    (Ty::Scalar(s), Some(c)) if s == c => {}
                    _ => {
                        return Err(error(Code::Mismatch, self.span_of(case), |m| {
                            m.write_str("a case value is of the selector's integer type")
                        }))
                    }
                }
            }
        }
        if defaults != 1 {
            return Err(error(Code::Syntax, self.span_of(selector), |m| {
                m.write_str("a switch has exactly one default")
            }));
        }
        let ty = self.types.scalar(common.unwrap_or(Scalar::I32))?;
        self.convert_to(selector, ty)?;
        let mut seen: Vec<(i128, Span)> = Vec::new();
        for clause in clause_list {
            for selector_case in tree.selector_list(clause.selectors) {
                let Some(case) = selector_case.expr else {
                    continue;
                };
                self.convert_to(case, ty)?;
                self.force(case)?;
                let value = self.integer_value(case)?;
                push(&mut seen, (value, self.span_of(case)))?;
            }
        }
        seen.sort_unstable_by_key(|&(value, _)| value);
        if let Some(pair) = seen.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            let (value, span) = pair[1];
            return Err(error(Code::Evaluation, span, |m| {
                write!(m, "the case value {value} is given twice")
            }));
        }
        push(&mut self.flow, Flow::Switch)?;
        let mut behaviour = 0u8;
        for clause in clause_list {
            behaviour |= self.compound(clause.body)?;
        }
        self.flow.pop();
        Ok(if behaviour & BREAK != 0 {
            (behaviour | NEXT) & !BREAK
        } else {
            behaviour
        })
    }

    /// A loop whose body `check` checks, giving the body's behaviour and
    /// its continuing's: the loop's behaviour by the specification's rule,
    /// refused where it is empty — a loop no path leaves.
    fn looping(
        &mut self,
        id: StmtId,
        check: impl FnOnce(&mut Self) -> Outcome<(u8, u8)>,
    ) -> Outcome<u8> {
        let body_locals = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        push(
            &mut self.flow,
            Flow::Loop {
                body_locals,
                first_continue: u32::MAX,
            },
        )?;
        let (body, continuing) = check(self)?;
        self.flow.pop();
        *self
            .loop_bodies
            .get_mut(id.0 as usize)
            .ok_or(Stop::Internal)? = body;
        let both = body | continuing;
        let behaviour = if body == RETURN {
            RETURN
        } else if both & BREAK == 0 {
            both & !(CONTINUE | NEXT)
        } else {
            (both | NEXT) & !(BREAK | CONTINUE)
        };
        Ok(behaviour)
    }

    /// `loop { body continuing { … break if e; } }`.
    fn loop_statement(
        &mut self,
        id: StmtId,
        (body, continuing): (Run, Option<u32>),
        span: Span,
    ) -> Outcome<u8> {
        let behaviour = self.looping(id, |this| {
            this.locals.open()?;
            let checked = this.loop_body(body, continuing)?;
            this.locals.close();
            Ok(checked)
        })?;
        if behaviour == 0 {
            return Err(error(Code::Behaviour, span, |m| {
                m.write_str("no path leaves this loop")
            }));
        }
        Ok(behaviour)
    }

    fn loop_body(&mut self, body: Run, continuing: Option<u32>) -> Outcome<(u8, u8)> {
        let body_behaviour = self.block(body)?;
        let Some(index) = continuing else {
            return Ok((body_behaviour, NEXT));
        };
        let tree: &'a crate::syntax::Tree = self.tree;
        let continuing = *tree.continuings.get(index as usize).ok_or(Stop::Internal)?;
        self.statement_attrs(continuing.attrs)?;
        let Some(&Flow::Loop {
            body_locals,
            first_continue,
        }) = self.flow.last()
        else {
            return Err(Stop::Internal);
        };
        let declared = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        push(
            &mut self.flow,
            Flow::Continuing {
                body_locals: (body_locals, declared),
                first_continue,
            },
        )?;
        self.locals.open()?;
        let continuing_behaviour = self.continuing_block(continuing.body, continuing.break_if)?;
        self.locals.close();
        self.flow.pop();
        Ok((body_behaviour, continuing_behaviour))
    }

    fn continuing_block(&mut self, body: Run, break_if: Option<ExprId>) -> Outcome<u8> {
        let mut behaviour = self.block(body)?;
        if let Some(condition) = break_if {
            self.condition(condition)?;
            behaviour = sequence(behaviour, BREAK | NEXT);
        }
        Ok(behaviour)
    }

    /// `for (init; condition; update) body`, as the loop it stands for.
    fn for_statement(
        &mut self,
        id: StmtId,
        header: (Option<StmtId>, Option<ExprId>, Option<StmtId>),
        body: StmtId,
    ) -> Outcome<u8> {
        self.locals.open()?;
        let behaviour = self.for_parts(id, header, body)?;
        self.locals.close();
        Ok(behaviour)
    }

    fn for_parts(
        &mut self,
        id: StmtId,
        (init, condition, update): (Option<StmtId>, Option<ExprId>, Option<StmtId>),
        body: StmtId,
    ) -> Outcome<u8> {
        if let Some(init) = init {
            self.statement(init)?;
        }
        if let Some(condition) = condition {
            self.condition(condition)?;
        }
        let guard = if condition.is_some() {
            BREAK | NEXT
        } else {
            NEXT
        };
        let behaviour = self.looping(id, |this| {
            let body = this.compound(body)?;
            if let Some(update) = update {
                let declared =
                    u32::try_from(this.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
                push(
                    &mut this.flow,
                    Flow::Continuing {
                        body_locals: (declared, declared),
                        first_continue: u32::MAX,
                    },
                )?;
                this.statement(update)?;
                this.flow.pop();
            }
            Ok((sequence(guard, body), NEXT))
        })?;
        if behaviour == 0 {
            return Err(error(Code::Behaviour, self.span_of_stmt(body), |m| {
                m.write_str("no path leaves this loop")
            }));
        }
        Ok(behaviour)
    }

    fn span_of_stmt(&self, id: StmtId) -> Span {
        self.tree.stmt(id).map_or_else(Span::default, |s| s.span)
    }

    /// Refuse a `break` that no loop or switch encloses, or that would
    /// leave a continuing block by aiming at its own loop.
    fn break_target(&self, span: Span) -> Outcome<()> {
        match self.flow.last() {
            Some(Flow::Loop { .. } | Flow::Switch) => Ok(()),
            Some(Flow::Continuing { .. }) => Err(error(Code::Behaviour, span, |m| {
                m.write_str("a break leaves a continuing block only as `break if`")
            })),
            None => Err(error(Code::Behaviour, span, |m| {
                m.write_str("a break stands in a loop or switch")
            })),
        }
    }

    /// Refuse a `continue` that no loop encloses, or that would enter a
    /// continuing block; record where its loop's first one stands.
    fn continue_target(&mut self, span: Span) -> Outcome<()> {
        for flow in self.flow.iter_mut().rev() {
            match flow {
                Flow::Switch => {}
                Flow::Loop { first_continue, .. } => {
                    *first_continue = (*first_continue).min(span.start);
                    return Ok(());
                }
                Flow::Continuing { .. } => {
                    return Err(error(Code::Behaviour, span, |m| {
                        m.write_str("a continue does not stand in a continuing block")
                    }))
                }
            }
        }
        Err(error(Code::Behaviour, span, |m| {
            m.write_str("a continue stands in a loop")
        }))
    }

    /// Refuse a local a continuing block names that its loop's body
    /// declared after a `continue` aimed at that loop: control could reach
    /// the continuing block without passing the declaration.
    pub(crate) fn continuing_may_name(&self, local: u32, span: Span) -> Outcome<()> {
        for flow in &self.flow {
            let Flow::Continuing {
                body_locals: (start, end),
                first_continue,
            } = *flow
            else {
                continue;
            };
            let declared_at = self
                .local_list
                .get(local as usize)
                .map_or(0, |l| l.span.start);
            if (start..end).contains(&local) && declared_at > first_continue {
                return Err(error(Code::Behaviour, span, |m| {
                    m.quote(self.spell(span))?;
                    m.write_str(" is declared after a continue that skips it")
                }));
            }
        }
        Ok(())
    }

    /// `return [e]`: a value exactly where the function returns one, and
    /// never within a continuing block.
    fn return_statement(&mut self, value: Option<ExprId>, span: Span) -> Outcome<()> {
        if self
            .flow
            .iter()
            .any(|f| matches!(f, Flow::Continuing { .. }))
        {
            return Err(error(Code::Behaviour, span, |m| {
                m.write_str("a continuing block does not return")
            }));
        }
        let Some(&DeclInfo::Function(signature)) = self.decls.get(self.current_fn as usize) else {
            return Err(Stop::Internal);
        };
        match (value, signature.result) {
            (None, None) => Ok(()),
            (Some(expr), Some(ty)) => {
                self.expression(expr)?;
                self.convert_to(expr, ty)
            }
            (None, Some(_)) => Err(error(Code::Behaviour, span, |m| {
                m.write_str("this function returns a value")
            })),
            (Some(expr), None) => Err(error(Code::Behaviour, self.span_of(expr), |m| {
                m.write_str("this function returns no value")
            })),
        }
    }

    /// A call standing alone: refused where its callee's value must be
    /// used.
    fn call_statement(&mut self, call: ExprId) -> Outcome<()> {
        let info = self.expression(call)?;
        let must_use = match info.denotes {
            Denotes::CallFunction(decl) => matches!(
                self.decls.get(decl as usize),
                Some(DeclInfo::Function(Signature { must_use: true, .. }))
            ),
            Denotes::CallBuiltin(builtin) => super::builtin::must_use(builtin),
            Denotes::Construct(_) => true,
            // The parser makes a call statement of a call alone.
            _ => return Err(Stop::Internal),
        };
        if must_use {
            return Err(error(Code::Misused, self.span_of(call), |m| {
                m.write_str("this call's value is used; assign it to `_` to discard it")
            }));
        }
        Ok(())
    }

    /// `let name [: T] = e`: a concrete constructible value, a pointer, a
    /// texture or a sampler.
    fn let_statement(&mut self, value: ValueDecl) -> Outcome<u32> {
        let declared = value.ty.map(|ty| self.type_expr(ty)).transpose()?;
        self.expression(value.init)?;
        let ty = match declared {
            Some(ty) => {
                self.convert_to(value.init, ty)?;
                ty
            }
            None => self.concretize(value.init)?,
        };
        let usable = (self.types.is_constructible(ty) && !self.types.is_abstract(ty))
            || matches!(
                self.types.get(ty),
                Ty::Pointer { .. } | Ty::Texture(_) | Ty::Sampler { .. }
            );
        if !usable {
            return Err(error(Code::Type, value.name, |m| {
                m.write_str("a let holds a constructible value, a pointer, a texture or a sampler")
            }));
        }
        let root = matches!(self.types.get(ty), Ty::Pointer { .. })
            .then(|| self.root_of(value.init))
            .flatten();
        self.declare_local(value.name, LocalKind::Let, ty, NONE, root)
    }

    /// A function-scope `var`: in the function address space, of a
    /// constructible store type, its bytes counted against the function's
    /// limit.
    fn var_statement(&mut self, var: VarDecl) -> Outcome<u32> {
        let tree: &'a crate::syntax::Tree = self.tree;
        let template = tree.expr_list(var.template);
        for &arg in template {
            self.expression(arg)?;
        }
        match template {
            [] => {}
            [arg] if self.info(*arg).denotes == Denotes::Space(Space::Function) => {}
            _ => {
                return Err(error(Code::AddressSpace, var.name, |m| {
                    m.write_str("a variable in a function is in the function address space")
                }))
            }
        }
        let declared = var.ty.map(|ty| self.type_expr(ty)).transpose()?;
        if let Some(init) = var.init {
            self.expression(init)?;
        }
        let store = match (declared, var.init) {
            (Some(ty), Some(init)) => {
                self.convert_to(init, ty)?;
                ty
            }
            (Some(ty), None) => ty,
            (None, Some(init)) => self.concretize(init)?,
            (None, None) => {
                return Err(error(Code::Type, var.name, |m| {
                    m.write_str("a variable needs a type or an initializer")
                }))
            }
        };
        self.store_in(store, Space::Function, Access::ReadWrite, var.name)?;
        let size = self.layout(store)?.size;
        self.function_bytes = self.function_bytes.saturating_add(size);
        if self.function_bytes > MAX_FUNCTION_BYTES {
            return Err(error(Code::MemoryLimit, var.name, |m| {
                write!(
                    m,
                    "this function's variables take {} bytes, past {MAX_FUNCTION_BYTES}",
                    self.function_bytes
                )
            }));
        }
        let local = u32::try_from(self.local_list.len()).map_err(|_| Stop::OutOfMemory)?;
        self.declare_local(
            var.name,
            LocalKind::Var,
            store,
            NONE,
            Some(Root::Local(local)),
        )
    }

    /// An assignment: `=`, or a compound operator taking the target's
    /// value as its left side.
    fn assignment(
        &mut self,
        lhs: ExprId,
        op: Option<BinaryOp>,
        rhs: ExprId,
        span: Span,
    ) -> Outcome<()> {
        self.expression(lhs)?;
        self.expression(rhs)?;
        let store = self.written(lhs, op.is_some())?;
        match op {
            None => self.convert_to(rhs, store),
            Some(op) => {
                let result = self.binary(op, (lhs, rhs), span)?.ty;
                if result != store {
                    return Err(self.mismatch(span, store, result));
                }
                Ok(())
            }
        }
    }

    /// The store type an assignment writes through `target` — a memory
    /// view, or a swizzle of a vector one with distinct components —
    /// refused where its access mode does not write, or where `reads` and
    /// it does not read. The access is recorded for the alias analysis.
    fn written(&mut self, target: ExprId, reads: bool) -> Outcome<TyId> {
        let info = self.info(target);
        let span = self.span_of(target);
        let (store, access) = match self.types.get(info.ty) {
            Ty::Reference { store, access, .. } => (store, access),
            _ => match self.swizzle_view(target)? {
                Some(view) => view,
                None => {
                    return Err(error(Code::Pointer, span, |m| {
                        m.write_str("an assignment writes to memory")
                    }))
                }
            },
        };
        if !access.writes() || (reads && !access.reads()) {
            return Err(error(Code::Pointer, span, |m| {
                write!(m, "this memory's access mode is {}", access.name())
            }));
        }
        if let Some(root) = self.root_of(target) {
            self.note_access(root, true)?;
            if reads {
                self.note_access(root, false)?;
            }
        }
        Ok(store)
    }

    /// The vector type and access mode of a swizzle of more than one
    /// distinct component of a vector memory view.
    fn swizzle_view(&mut self, target: ExprId) -> Outcome<Option<(TyId, Access)>> {
        let Some(ExprKind::Member(base, _)) = self.tree.expr(target).map(|e| e.kind) else {
            return Ok(None);
        };
        let Denotes::Swizzle(lanes, count) = self.info(target).denotes else {
            return Ok(None);
        };
        let (Ty::Reference { store, access, .. } | Ty::Pointer { store, access, .. }) =
            self.types.get(self.info(base).ty)
        else {
            return Ok(None);
        };
        let Ty::Vector(_, scalar) = self.types.get(store) else {
            return Ok(None);
        };
        let lanes = &lanes[..usize::from(count)];
        let distinct = lanes
            .iter()
            .enumerate()
            .all(|(i, lane)| !lanes[..i].contains(lane));
        if !distinct {
            return Err(error(Code::Pointer, self.span_of(target), |m| {
                m.write_str("a swizzle written to names each component once")
            }));
        }
        let ty = self.intern(Ty::Vector(count, scalar))?;
        Ok(Some((ty, access)))
    }

    /// The root identifier of the memory view or pointer `id`, if it has
    /// one: the variable or pointer parameter it is reached from.
    pub(crate) fn root_of(&self, id: ExprId) -> Option<Root> {
        let mut at = id;
        loop {
            match self.tree.expr(at)?.kind {
                ExprKind::Unary(_, inner)
                | ExprKind::Index(inner, _)
                | ExprKind::Member(inner, _) => {
                    at = inner;
                }
                ExprKind::Ident(_) => {
                    return match self.info(at).denotes {
                        Denotes::Global(decl) => Some(Root::Global(decl)),
                        Denotes::Local(local) => self.local_list.get(local as usize)?.root,
                        _ => None,
                    };
                }
                _ => return None,
            }
        }
    }

    /// Record an access of memory reached from `root`, where it matters
    /// to the alias analysis: a module-scope variable's or a pointer
    /// parameter's.
    pub(crate) fn note_access(&mut self, root: Root, write: bool) -> Outcome<()> {
        if self.current_fn == NONE || matches!(root, Root::Local(_)) {
            return Ok(());
        }
        push(
            &mut self.accesses,
            MemoryAccess {
                function: self.current_fn,
                root,
                write,
            },
        )
    }

    /// The pointer access a built-in call makes, recorded for the alias
    /// analysis.
    pub(crate) fn note_builtin_access(&mut self, builtin: Builtin, args: &[ExprId]) -> Outcome<()> {
        use Builtin as B;
        let (reads, writes) = match builtin {
            B::AtomicLoad | B::WorkgroupUniformLoad => (true, false),
            B::AtomicStore => (false, true),
            B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak => (true, true),
            _ => return Ok(()),
        };
        let Some(root) = args.first().and_then(|&p| self.root_of(p)) else {
            return Ok(());
        };
        if reads {
            self.note_access(root, false)?;
        }
        if writes {
            self.note_access(root, true)?;
        }
        Ok(())
    }
}

/// The behaviour of `first` then `second`.
const fn sequence(first: u8, second: u8) -> u8 {
    if first & NEXT != 0 {
        (first & !NEXT) | second
    } else {
        first
    }
}
