//! Pipeline creation: the module's overrides take their values — the
//! compile's, else their initializers' — and every override-expression an
//! entry point reaches is evaluated, with the checks that wait on those
//! values, exactly as the specification evaluates them at pipeline creation:
//! an initializer the compile overrides, and code no entry point reaches,
//! are never evaluated.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::decl::DeclInfo;
use super::entry::MAX_WORKGROUP_BYTES;
use super::{header_roots, Checker, Denotes, Stage};
use crate::diag::{error, push, reserve, Code, Outcome, Stop};
use crate::syntax::{BinaryOp, DeclKind, ExprId, ExprKind, Stmt, StmtId, StmtKind};
use crate::types::{Count, Space, Ty};

impl<'a> Checker<'a> {
    /// Give every override an entry point reaches its value, evaluate
    /// every override-expression the entry points reach, and settle each
    /// entry point's workgroup size and workgroup memory.
    pub(crate) fn pipeline(&mut self) -> Outcome<()> {
        let count = self.tree.decls.len();
        let mut live: Vec<bool> = Vec::new();
        reserve(&mut live, count)?;
        live.resize(count, false);
        for entry in &self.entries {
            for &decl in &entry.reached {
                if let Some(mark) = live.get_mut(decl as usize) {
                    *mark = true;
                }
            }
        }
        let tree: &'a crate::syntax::Tree = self.tree;
        let mut roots: Vec<ExprId> = Vec::new();
        for at in 0..self.decl_order.len() {
            let decl = self.decl_order[at];
            if !live.get(decl as usize).copied().unwrap_or(false) {
                continue;
            }
            let declaration = tree.decls.get(decl as usize).ok_or(Stop::Internal)?;
            if let DeclKind::Override(value) = declaration.kind {
                self.override_value(decl, value.name)?;
                continue;
            }
            roots.clear();
            header_roots(self.text, tree, declaration, &mut roots)?;
            for &root in &roots {
                self.at_pipeline(root)?;
            }
        }
        for (decl, declaration) in tree.decls.iter().enumerate() {
            let DeclKind::Fn(function) = declaration.kind else {
                continue;
            };
            if !live.get(decl).copied().unwrap_or(false) {
                continue;
            }
            roots.clear();
            self.body_roots(function.body, &mut roots)?;
            for &root in &roots {
                self.at_pipeline(root)?;
            }
        }
        for index in 0..self.entries.len() {
            self.settle_entry(index)?;
        }
        Ok(())
    }

    /// The value override `decl`, named at `name`, takes: the compile's, or
    /// else its initializer's, which only then is evaluated.
    fn override_value(&mut self, decl: u32, name: crate::diag::Span) -> Outcome<()> {
        let Some(&DeclInfo::Override(mut o)) = self.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        o.value = match (o.given, o.init) {
            (Some(given), _) => Some(given),
            (None, Some(init)) => {
                self.at_pipeline(init)?;
                Some(self.value_or_stop(init)?)
            }
            (None, None) => {
                return Err(error(Code::OverrideValue, name, |m| {
                    m.quote(self.spell(name))?;
                    m.write_str(" has no initializer and the compile gives it no value")
                }))
            }
        };
        *self.decls.get_mut(decl as usize).ok_or(Stop::Internal)? = DeclInfo::Override(o);
        Ok(())
    }

    /// Evaluate `root`'s override-expressions, then every check in its
    /// subtree that waited on their values — each check's operand
    /// evaluated even where a short-circuit skips the rest.
    fn at_pipeline(&mut self, root: ExprId) -> Outcome<()> {
        self.evaluate_range(root, Stage::Override)?;
        for raw in self.shape.subtree(root) {
            self.settle(ExprId(raw))?;
        }
        Ok(())
    }

    /// Evaluate `id`'s subtree as pipeline creation does.
    fn force_override(&mut self, id: ExprId) -> Outcome<()> {
        if self.info(id).value_id().is_none() {
            self.evaluate_range(id, Stage::Override)?;
        }
        Ok(())
    }

    /// The checks on node `id` that wait for override values: an integer
    /// divisor of zero, a shift past the width, an index out of bounds, a
    /// built-in's bound arguments, an override-sized count not positive.
    fn settle(&mut self, id: ExprId) -> Outcome<()> {
        let kind = self.tree.expr(id).ok_or(Stop::Internal)?.kind;
        let waits = |this: &Self, operand: ExprId| this.info(operand).stage == Stage::Override;
        match kind {
            ExprKind::Binary(BinaryOp::Divide | BinaryOp::Remainder, _, rhs)
                if waits(self, rhs) =>
            {
                let integer = self
                    .types
                    .scalar_of(self.info(rhs).converted)
                    .is_some_and(crate::types::Scalar::is_integer);
                if integer {
                    self.force_override(rhs)?;
                    self.zero_divisor(rhs)?;
                }
            }
            ExprKind::Binary(BinaryOp::ShiftLeft | BinaryOp::ShiftRight, lhs, rhs)
                if waits(self, rhs) =>
            {
                let concrete = self
                    .types
                    .scalar_of(self.info(lhs).converted)
                    .is_some_and(|s| !s.is_abstract());
                if concrete {
                    self.force_override(rhs)?;
                    self.shift_amount(rhs)?;
                }
            }
            ExprKind::Index(base, index) => {
                let store = self
                    .types
                    .store_of(self.info(base).ty)
                    .unwrap_or(self.info(base).ty);
                let counted = match self.types.get(store) {
                    Ty::Array { count, .. } => !matches!(count, Count::Fixed(_) | Count::Runtime),
                    _ => false,
                };
                let stage = self.info(index).stage;
                if stage == Stage::Override || (stage == Stage::Const && counted) {
                    self.force_override(index)?;
                    let bound = match self.types.get(store) {
                        Ty::Array { count, .. } => self.count_value(count),
                        Ty::Vector(n, _) => Some(u32::from(n)),
                        Ty::Matrix { columns, .. } => Some(u32::from(columns)),
                        _ => None,
                    };
                    self.index_bound(index, bound)?;
                }
            }
            ExprKind::Call(_, args) if matches!(self.info(id).denotes, Denotes::CallBuiltin(_)) => {
                let Denotes::CallBuiltin(builtin) = self.info(id).denotes else {
                    return Err(Stop::Internal);
                };
                let tree: &'a crate::syntax::Tree = self.tree;
                self.argument_rules(builtin, tree.expr_list(args), Stage::Override)?;
            }
            ExprKind::Ident(_) if self.info(id).denotes == Denotes::Type => {
                if let Ty::Array { count, .. } = self.types.get(self.info(id).ty) {
                    let value = match count {
                        Count::Expression(expr) => {
                            self.force_override(ExprId(expr))?;
                            self.integer_value(ExprId(expr))?
                        }
                        Count::Override(decl) => {
                            let value = self.decl_value(decl, self.span_of(id))?;
                            self.values.integer(value).ok_or(Stop::Internal)?
                        }
                        Count::Fixed(_) | Count::Runtime => return Ok(()),
                    };
                    if value <= 0 || value > i128::from(u32::MAX) {
                        return Err(error(Code::Type, self.span_of(id), |m| {
                            write!(m, "an array's element count is positive, not {value}")
                        }));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// An entry point's workgroup size, each dimension positive, and the
    /// workgroup memory it reaches within its limit.
    fn settle_entry(&mut self, index: usize) -> Outcome<()> {
        let entry = self.entries.get(index).ok_or(Stop::Internal)?;
        let decl = entry.decl;
        let Some(&DeclInfo::Function(signature)) = self.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        let mut size = [1u32; 3];
        if let Some(dimensions) = signature.workgroup {
            for (slot, dimension) in size.iter_mut().zip(dimensions) {
                let Some(arg) = dimension else {
                    continue;
                };
                self.force_override(arg)?;
                let n = self.integer_value(arg)?;
                *slot = Self::in_range(n, (1, i128::from(u32::MAX)), self.span_of(arg))?;
            }
        }
        let mut workgroup_bytes = 0u64;
        let reached_len = self.entries.get(index).map_or(0, |e| e.reached.len());
        for at in 0..reached_len {
            let decl = self.entries[index].reached[at];
            let Some(&DeclInfo::Global(global)) = self.decls.get(decl as usize) else {
                continue;
            };
            if global.space == Space::Workgroup {
                let layout = self.layout(global.store)?;
                workgroup_bytes = workgroup_bytes.saturating_add(layout.size);
            }
        }
        if workgroup_bytes > MAX_WORKGROUP_BYTES {
            let name = self.decl_name(decl);
            return Err(error(Code::MemoryLimit, name, |m| {
                write!(
                    m,
                    "this entry point's workgroup variables take {workgroup_bytes} bytes, past {MAX_WORKGROUP_BYTES}"
                )
            }));
        }
        if let Some(entry) = self.entries.get_mut(index) {
            entry.workgroup = size;
        }
        Ok(())
    }

    /// Every expression root of a function body, walked without recursion.
    pub(crate) fn body_roots(&self, body: StmtId, out: &mut Vec<ExprId>) -> Outcome<()> {
        self.each_statement(body, |_, stmt| self.statement_roots(stmt, out))
    }

    /// Call `visit` with each statement of `root`'s subtree, itself
    /// included, walked without recursion.
    pub(crate) fn each_statement(
        &self,
        root: StmtId,
        mut visit: impl FnMut(StmtId, &Stmt) -> Outcome<()>,
    ) -> Outcome<()> {
        let tree = self.tree;
        let mut pending: Vec<StmtId> = Vec::new();
        push(&mut pending, root)?;
        while let Some(id) = pending.pop() {
            let stmt = tree.stmt(id).ok_or(Stop::Internal)?;
            visit(id, stmt)?;
            let mut child = |child: StmtId| push(&mut pending, child);
            match stmt.kind {
                StmtKind::Compound(run) => {
                    for &s in tree.stmt_list(run) {
                        child(s)?;
                    }
                }
                StmtKind::If { accept, reject, .. } => {
                    child(accept)?;
                    if let Some(reject) = reject {
                        child(reject)?;
                    }
                }
                StmtKind::Switch { clauses, .. } => {
                    for clause in tree.clause_list(clauses) {
                        child(clause.body)?;
                    }
                }
                StmtKind::Loop {
                    body, continuing, ..
                } => {
                    for &s in tree.stmt_list(body) {
                        child(s)?;
                    }
                    if let Some(index) = continuing {
                        let continuing =
                            tree.continuings.get(index as usize).ok_or(Stop::Internal)?;
                        for &s in tree.stmt_list(continuing.body) {
                            child(s)?;
                        }
                    }
                }
                StmtKind::For {
                    init, update, body, ..
                } => {
                    for s in [init, update, Some(body)].into_iter().flatten() {
                        child(s)?;
                    }
                }
                StmtKind::While { body, .. } => child(body)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// The expression roots statement `stmt` holds itself, its nested
    /// statements' aside.
    pub(crate) fn statement_roots(&self, stmt: &Stmt, out: &mut Vec<ExprId>) -> Outcome<()> {
        let tree = self.tree;
        let mut root = |expr: Option<ExprId>| match expr {
            Some(expr) => push(out, expr),
            None => Ok(()),
        };
        match stmt.kind {
            StmtKind::Assign { lhs, rhs, .. } => {
                root(Some(lhs))?;
                root(Some(rhs))?;
            }
            StmtKind::Phony(e)
            | StmtKind::Increment(e)
            | StmtKind::Decrement(e)
            | StmtKind::Call(e)
            | StmtKind::ConstAssert(e) => root(Some(e))?,
            StmtKind::If { condition, .. } | StmtKind::While { condition, .. } => {
                root(Some(condition))?;
            }
            StmtKind::Switch {
                selector, clauses, ..
            } => {
                root(Some(selector))?;
                for clause in tree.clause_list(clauses) {
                    for selector in tree.selector_list(clause.selectors) {
                        root(selector.expr)?;
                    }
                }
            }
            StmtKind::Loop { continuing, .. } => {
                if let Some(index) = continuing {
                    let continuing = tree.continuings.get(index as usize).ok_or(Stop::Internal)?;
                    root(continuing.break_if)?;
                }
            }
            StmtKind::For { condition, .. } => root(condition)?,
            StmtKind::Return(value) => root(value)?,
            StmtKind::Let(value) | StmtKind::Const(value) => {
                root(value.ty)?;
                root(Some(value.init))?;
            }
            StmtKind::Var(var) => {
                for &arg in tree.expr_list(var.template) {
                    root(Some(arg))?;
                }
                root(var.ty)?;
                root(var.init)?;
            }
            StmtKind::Compound(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Discard
            | StmtKind::Empty => {}
        }
        Ok(())
    }
}
