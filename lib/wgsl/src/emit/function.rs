//! Functions: each variant's parameters bound — values, handles, and the
//! places its pointer parameters reach — and its body written as SPIR-V's
//! structured control flow, statement by statement.

use alloc::vec::Vec;

use tairix_spirv::enumerant::{LoopControl, SelectionControl, StorageClass};
use tairix_spirv::{Arg, Function, Id, Op};

use super::expr::Lowered;
use super::{built, Emitter, Flavour, Index, Lower, Place, Root, Shape, Step, Tables, VariantKey};
use crate::check::decl::{DeclInfo, Signature};
use crate::check::stmt::{CONTINUE, NEXT};
use crate::check::Checker;
use crate::check::Denotes;
use crate::diag::{push, reserve, Outcome, Stop};
use crate::syntax::{BinaryOp, DeclKind, ExprId, ExprKind, Run, StmtId, StmtKind};
use crate::types::{Scalar, Space, Ty, TyId};

/// A local as written: a value, a place, or a texture or sampler's handle.
#[derive(Debug)]
pub(crate) enum Local {
    Unset,
    Value(Id),
    Place(Place),
    Handle(Id),
}

/// A construct `break` and `continue` leave.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Target {
    Loop { merge: Id, continuing: Id },
    Switch { merge: Id },
}

/// How a function's parameter is bound in a variant.
#[derive(Debug)]
enum Binding {
    /// By the value of the variant's parameter at this place.
    Value(usize),
    /// A texture or sampler, by the handle at this place.
    Handle(usize),
    /// A pointer: its shape, the place of its object's pointer where the
    /// variant takes one, the first of its element indexes, and the
    /// memory it views.
    Pointer {
        shape: Shape,
        base: Option<usize>,
        elements: usize,
        space: Space,
        store: TyId,
    },
}

/// A function body being written.
pub(crate) struct Writer<'w, 'b, 'a> {
    pub(crate) c: &'w mut Checker<'a>,
    pub(crate) t: &'w mut Tables,
    pub(crate) f: &'w mut Function<'b>,
    pub(crate) first_local: u32,
    pub(crate) locals: Vec<Local>,
    pub(crate) targets: Vec<Target>,
}

impl Emitter<'_, '_> {
    /// Write the variant `key` as function `id`.
    pub(crate) fn function(&mut self, key: &VariantKey, id: Id) -> Outcome<()> {
        let (decl, shapes) = key;
        let Some(&DeclInfo::Function(signature)) = self.c.decls.get(*decl as usize) else {
            return Err(Stop::Internal);
        };
        let DeclKind::Fn(declaration) = self
            .c
            .tree
            .decls
            .get(*decl as usize)
            .ok_or(Stop::Internal)?
            .kind
        else {
            return Err(Stop::Internal);
        };
        let (types, bindings) = self.parameters(signature, shapes)?;
        let result = match signature.result {
            Some(ty) => self.lower().ty(ty, Flavour::Value)?,
            None => self.b.void().map_err(built)?,
        };
        let &(_, first_local, end_local) = self
            .c
            .fn_locals
            .iter()
            .find(|&&(d, _, _)| d == *decl)
            .ok_or(Stop::Internal)?;
        let Emitter { c, t, b } = self;
        b.define(id, result, &types, |f, values| -> Outcome<()> {
            let mut w = Writer {
                c,
                t,
                f,
                first_local,
                locals: Vec::new(),
                targets: Vec::new(),
            };
            reserve(&mut w.locals, (end_local - first_local) as usize)?;
            w.locals
                .resize_with((end_local - first_local) as usize, || Local::Unset);
            for (place, binding) in bindings.iter().enumerate() {
                let local = w.bind_param(binding, values)?;
                *w.locals.get_mut(place).ok_or(Stop::Internal)? = local;
            }
            let Some(StmtKind::Compound(body)) = w.c.tree.stmt(declaration.body).map(|s| s.kind)
            else {
                return Err(Stop::Internal);
            };
            w.block(body)?;
            if w.open() {
                if signature.result.is_some() {
                    w.terminate(Op::UNREACHABLE, &[])?;
                } else {
                    w.terminate(Op::RETURN, &[])?;
                }
            }
            Ok(())
        })
    }

    /// The types of the parameters a variant of `signature` takes, its
    /// pointers specialised to `shapes`, and how each is bound.
    fn parameters(
        &mut self,
        signature: Signature,
        shapes: &[Shape],
    ) -> Outcome<(Vec<Id>, Vec<Binding>)> {
        let mut types: Vec<Id> = Vec::new();
        let mut bindings: Vec<Binding> = Vec::new();
        let mut shape_at = 0usize;
        for place in signature.params.0..signature.params.1 {
            let param = self
                .c
                .params
                .get(place as usize)
                .copied()
                .ok_or(Stop::Internal)?;
            let mut lower = self.lower();
            match lower.c.types.get(param.ty) {
                Ty::Pointer { space, store, .. } => {
                    let shape = shapes.get(shape_at).ok_or(Stop::Internal)?.duplicate()?;
                    shape_at += 1;
                    let base = match shape.root {
                        Root::Global(_) => None,
                        Root::Object {
                            ty,
                            space: object_space,
                        } => {
                            let flavour = lower.memory_flavour(object_space, ty)?;
                            let pointee = lower.ty(ty, flavour)?;
                            push(
                                &mut types,
                                lower.pointer(Lower::class(object_space), pointee)?,
                            )?;
                            Some(types.len() - 1)
                        }
                    };
                    let elements = types.len();
                    let u32_ty = lower.scalar(Scalar::U32)?;
                    for step in &shape.steps {
                        if *step == Step::Element {
                            push(&mut types, u32_ty)?;
                        }
                    }
                    push(
                        &mut bindings,
                        Binding::Pointer {
                            shape,
                            base,
                            elements,
                            space,
                            store,
                        },
                    )?;
                }
                Ty::Texture(_) | Ty::Sampler { .. } => {
                    let handle = lower.ty(param.ty, Flavour::Plain)?;
                    push(
                        &mut types,
                        lower.pointer(StorageClass::UNIFORM_CONSTANT, handle)?,
                    )?;
                    push(&mut bindings, Binding::Handle(types.len() - 1))?;
                }
                _ => {
                    push(&mut types, lower.ty(param.ty, Flavour::Value)?)?;
                    push(&mut bindings, Binding::Value(types.len() - 1))?;
                }
            }
        }
        Ok((types, bindings))
    }
}

impl<'a> Writer<'_, '_, 'a> {
    /// The lowering view over the function's module.
    pub(crate) fn lower(&mut self) -> Lower<'_, 'a> {
        Lower {
            c: self.c,
            t: self.t,
            b: self.f.module(),
        }
    }

    /// The local a variant's parameter is bound as.
    fn bind_param(&mut self, binding: &Binding, values: &[Id]) -> Outcome<Local> {
        let value = |at: usize| values.get(at).copied().ok_or(Stop::Internal);
        Ok(match binding {
            Binding::Value(at) => Local::Value(value(*at)?),
            Binding::Handle(at) => Local::Handle(value(*at)?),
            Binding::Pointer {
                shape,
                base,
                elements,
                space,
                store,
            } => {
                let (base, wrapped, root_space) = match shape.root {
                    Root::Global(decl) => {
                        let global = self.lower().global(decl)?;
                        (global.id, global.wrapped, *space)
                    }
                    Root::Object {
                        space: object_space,
                        ..
                    } => (value(base.ok_or(Stop::Internal)?)?, false, object_space),
                };
                let mut indexes = Vec::new();
                reserve(&mut indexes, shape.steps.len())?;
                let mut element = *elements;
                for step in &shape.steps {
                    indexes.push(match *step {
                        Step::Member(n) => Index::Const(n),
                        Step::Element => {
                            let id = value(element)?;
                            element += 1;
                            Index::Dynamic(id)
                        }
                    });
                }
                Local::Place(Place {
                    base,
                    root: shape.root,
                    space: root_space,
                    wrapped,
                    indexes,
                    ty: *store,
                })
            }
        })
    }

    /// Begin block `label`.
    pub(crate) fn begin(&mut self, label: Id) -> Outcome<()> {
        self.f.begin(label).map_err(built)
    }

    /// Whether a block is open.
    pub(crate) fn open(&self) -> bool {
        self.f.block().is_some()
    }

    /// End the open block with the terminator `op`.
    pub(crate) fn terminate(&mut self, op: Op, operands: &[Id]) -> Outcome<()> {
        self.f.emit(op, operands).map_err(built)
    }

    /// Branch to `target` where a block is open.
    pub(crate) fn branch(&mut self, target: Id) -> Outcome<()> {
        if self.open() {
            self.terminate(Op::BRANCH, &[target])?;
        }
        Ok(())
    }

    pub(crate) fn label(&mut self) -> Outcome<Id> {
        self.f.label().map_err(built)
    }

    pub(crate) fn select_merge(&mut self, merge: Id) -> Outcome<()> {
        self.f
            .emit_args(
                Op::SELECTION_MERGE,
                &[Arg::Id(merge), Arg::Word(SelectionControl::NONE.0)],
            )
            .map_err(built)
    }

    fn loop_merge(&mut self, merge: Id, continuing: Id) -> Outcome<()> {
        self.f
            .emit_args(
                Op::LOOP_MERGE,
                &[
                    Arg::Id(merge),
                    Arg::Id(continuing),
                    Arg::Word(LoopControl::NONE.0),
                ],
            )
            .map_err(built)
    }

    fn behaviour(&self, id: StmtId) -> u8 {
        self.c
            .stmt_behaviour
            .get(id.0 as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Statements in sequence: one that does not finish normally ends the
    /// block, for nothing after it runs.
    pub(crate) fn block(&mut self, run: Run) -> Outcome<()> {
        let tree = self.c.tree;
        for &stmt in tree.stmt_list(run) {
            self.statement(stmt)?;
            super::within_output(self.f.module())?;
            if self.behaviour(stmt) & NEXT == 0 {
                break;
            }
        }
        Ok(())
    }

    #[expect(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn statement(&mut self, id: StmtId) -> Outcome<()> {
        let tree = self.c.tree;
        let stmt = *tree.stmt(id).ok_or(Stop::Internal)?;
        match stmt.kind {
            StmtKind::Compound(run) => self.block(run),
            StmtKind::Assign { lhs, op, rhs } => self.assign(lhs, op, rhs),
            StmtKind::Increment(target) | StmtKind::Decrement(target) => {
                let op = if matches!(stmt.kind, StmtKind::Increment(_)) {
                    BinaryOp::Add
                } else {
                    BinaryOp::Subtract
                };
                self.step_by_one(target, op)
            }
            StmtKind::Phony(e) | StmtKind::Call(e) => {
                self.expression(e)?;
                Ok(())
            }
            StmtKind::If { .. } => self.if_chain(id),
            StmtKind::Switch {
                selector, clauses, ..
            } => self.switch(id, selector, clauses),
            StmtKind::Loop {
                body, continuing, ..
            } => {
                let continuing = continuing
                    .map(|c| {
                        tree.continuings
                            .get(c as usize)
                            .copied()
                            .ok_or(Stop::Internal)
                    })
                    .transpose()?;
                self.looping(
                    id,
                    |w| w.block(body),
                    |w, merge, header| {
                        if let Some(c) = continuing {
                            w.block(c.body)?;
                            if let Some(condition) = c.break_if {
                                let holds = w.value(condition)?;
                                if w.open() {
                                    w.terminate(Op::BRANCH_CONDITIONAL, &[holds, merge, header])?;
                                }
                            }
                        }
                        w.branch(header)
                    },
                )
            }
            StmtKind::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(init) = init {
                    self.statement(init)?;
                }
                self.looping(
                    id,
                    |w| {
                        if let Some(condition) = condition {
                            w.guard(condition)?;
                        }
                        w.statement(body)
                    },
                    |w, _, header| {
                        if let Some(update) = update {
                            w.statement(update)?;
                        }
                        w.branch(header)
                    },
                )
            }
            StmtKind::While { condition, body } => self.looping(
                id,
                |w| {
                    w.guard(condition)?;
                    w.statement(body)
                },
                |w, _, header| w.branch(header),
            ),
            StmtKind::Break => {
                let merge = match self.targets.last() {
                    Some(Target::Loop { merge, .. } | Target::Switch { merge }) => *merge,
                    None => return Err(Stop::Internal),
                };
                self.branch(merge)
            }
            StmtKind::Continue => {
                let continuing = self
                    .targets
                    .iter()
                    .rev()
                    .find_map(|t| match t {
                        Target::Loop { continuing, .. } => Some(*continuing),
                        Target::Switch { .. } => None,
                    })
                    .ok_or(Stop::Internal)?;
                self.branch(continuing)
            }
            StmtKind::Discard => {
                let flag = self.lower().discard_flag()?;
                let yes = self.lower().boolean(true)?;
                self.f.emit(Op::STORE, &[flag, yes]).map_err(built)
            }
            StmtKind::Return(value) => match value {
                Some(e) => {
                    let v = self.value(e)?;
                    self.terminate(Op::RETURN_VALUE, &[v])
                }
                None => self.terminate(Op::RETURN, &[]),
            },
            StmtKind::Let(value) => {
                let local = self.declared(id)?;
                let bound = match self.expression(value.init)? {
                    Lowered::Value(v) => Local::Value(v),
                    Lowered::Place(p) => Local::Place(p),
                    Lowered::Handle(h) => Local::Handle(h),
                    Lowered::Void => return Err(Stop::Internal),
                };
                self.set_local(local, bound)
            }
            StmtKind::Var(var) => {
                let local = self.declared(id)?;
                let store = self
                    .c
                    .local_list
                    .get(local as usize)
                    .map(|l| l.ty)
                    .ok_or(Stop::Internal)?;
                let pointer = self.pointer_type(Space::Function, store)?;
                let variable = self.f.local(pointer).map_err(built)?;
                let place = Place {
                    base: variable,
                    root: Root::Object {
                        ty: store,
                        space: Space::Function,
                    },
                    space: Space::Function,
                    wrapped: false,
                    indexes: Vec::new(),
                    ty: store,
                };
                let initial = if let Some(init) = var.init {
                    self.value(init)?
                } else {
                    let ty = self.lower().ty(store, Flavour::Value)?;
                    self.f.module().constant_null(ty).map_err(built)?
                };
                self.f
                    .emit(Op::STORE, &[variable, initial])
                    .map_err(built)?;
                self.set_local(local, Local::Place(place))
            }
            StmtKind::Const(_) | StmtKind::ConstAssert(_) | StmtKind::Empty => Ok(()),
        }
    }

    /// The local a declaration statement declares.
    fn declared(&self, id: StmtId) -> Outcome<u32> {
        self.c
            .stmt_local
            .get(id.0 as usize)
            .copied()
            .ok_or(Stop::Internal)
    }

    pub(crate) fn set_local(&mut self, local: u32, bound: Local) -> Outcome<()> {
        let slot = local.checked_sub(self.first_local).ok_or(Stop::Internal)?;
        *self.locals.get_mut(slot as usize).ok_or(Stop::Internal)? = bound;
        Ok(())
    }

    pub(crate) fn local(&self, local: u32) -> Outcome<&Local> {
        let slot = local.checked_sub(self.first_local).ok_or(Stop::Internal)?;
        self.locals.get(slot as usize).ok_or(Stop::Internal)
    }

    /// `if !(condition) { break; }`: branch to the loop's merge where the
    /// condition fails.
    fn guard(&mut self, condition: ExprId) -> Outcome<()> {
        let holds = self.value(condition)?;
        let Some(&Target::Loop { merge, .. }) = self.targets.last() else {
            return Err(Stop::Internal);
        };
        let body = self.label()?;
        self.terminate(Op::BRANCH_CONDITIONAL, &[holds, body, merge])?;
        self.begin(body)
    }

    /// A loop: its header, its body, its continuing block reached where the
    /// body may go round again, its merge.
    fn looping(
        &mut self,
        id: StmtId,
        body: impl FnOnce(&mut Self) -> Outcome<()>,
        continuing: impl FnOnce(&mut Self, Id, Id) -> Outcome<()>,
    ) -> Outcome<()> {
        let (header, inside, back, merge) =
            (self.label()?, self.label()?, self.label()?, self.label()?);
        self.branch(header)?;
        self.begin(header)?;
        self.loop_merge(merge, back)?;
        self.terminate(Op::BRANCH, &[inside])?;
        self.begin(inside)?;
        push(
            &mut self.targets,
            Target::Loop {
                merge,
                continuing: back,
            },
        )?;
        body(self)?;
        self.branch(back)?;
        self.begin(back)?;
        let repeats =
            self.c.loop_bodies.get(id.0 as usize).copied().unwrap_or(0) & (NEXT | CONTINUE) != 0;
        if repeats {
            continuing(self, merge, header)?;
        } else {
            self.terminate(Op::BRANCH, &[header])?;
        }
        self.targets.pop();
        self.begin(merge)?;
        if self.behaviour(id) & NEXT == 0 {
            self.terminate(Op::UNREACHABLE, &[])?;
        }
        Ok(())
    }

    /// An `if` and its `else if` chain: each link a selection nested in the
    /// one before's else, closed from the innermost out.
    fn if_chain(&mut self, first: StmtId) -> Outcome<()> {
        let tree = self.c.tree;
        let mut merges: Vec<(Id, StmtId)> = Vec::new();
        let mut at = first;
        loop {
            let StmtKind::If {
                condition,
                accept,
                reject,
            } = tree.stmt(at).ok_or(Stop::Internal)?.kind
            else {
                return Err(Stop::Internal);
            };
            let holds = self.value(condition)?;
            let (then, otherwise, merge) = (self.label()?, self.label()?, self.label()?);
            self.select_merge(merge)?;
            let other = if reject.is_some() { otherwise } else { merge };
            self.terminate(Op::BRANCH_CONDITIONAL, &[holds, then, other])?;
            self.begin(then)?;
            self.statement(accept)?;
            self.branch(merge)?;
            push(&mut merges, (merge, at))?;
            match reject {
                None => break,
                Some(next) => {
                    self.begin(otherwise)?;
                    if matches!(tree.stmt(next).map(|s| s.kind), Some(StmtKind::If { .. })) {
                        at = next;
                        continue;
                    }
                    self.statement(next)?;
                    self.branch(merge)?;
                    break;
                }
            }
        }
        // Each merge ends the else of the link before it.
        for index in (0..merges.len()).rev() {
            let (merge, link) = merges[index];
            if index + 1 < merges.len() {
                self.branch(merge)?;
            }
            self.begin(merge)?;
            if self.behaviour(link) & NEXT == 0 {
                self.terminate(Op::UNREACHABLE, &[])?;
            }
        }
        Ok(())
    }

    /// A `switch`: a selection over its cases' literals, each clause its own
    /// block, a break leaving to its merge.
    fn switch(&mut self, id: StmtId, selector: ExprId, clauses: Run) -> Outcome<()> {
        let tree = self.c.tree;
        let value = self.value(selector)?;
        let merge = self.label()?;
        let clause_list = tree.clause_list(clauses);
        let mut labels: Vec<Id> = Vec::new();
        reserve(&mut labels, clause_list.len())?;
        let mut default = None;
        let mut cases: Vec<(u32, Id)> = Vec::new();
        for clause in clause_list {
            let label = self.label()?;
            labels.push(label);
            for selector_case in tree.selector_list(clause.selectors) {
                match selector_case.expr {
                    None => default = Some(label),
                    Some(case) => {
                        let n = self.c.integer_value(case)?;
                        let word = u32::try_from(n)
                            .or_else(|_| i32::try_from(n).map(i32::cast_unsigned))
                            .map_err(|_| Stop::Internal)?;
                        push(&mut cases, (word, label))?;
                    }
                }
            }
        }
        let default = default.ok_or(Stop::Internal)?;
        self.select_merge(merge)?;
        let mut args: Vec<Arg<'_>> = Vec::new();
        reserve(&mut args, 2 + cases.len() * 2)?;
        args.push(Arg::Id(value));
        args.push(Arg::Id(default));
        for &(word, label) in &cases {
            args.push(Arg::Word(word));
            args.push(Arg::Id(label));
        }
        self.f.emit_args(Op::SWITCH, &args).map_err(built)?;
        push(&mut self.targets, Target::Switch { merge })?;
        for (clause, &label) in clause_list.iter().zip(&labels) {
            self.begin(label)?;
            self.statement(clause.body)?;
            self.branch(merge)?;
        }
        self.targets.pop();
        self.begin(merge)?;
        if self.behaviour(id) & NEXT == 0 {
            self.terminate(Op::UNREACHABLE, &[])?;
        }
        Ok(())
    }

    /// A loop counting a `u32` from `start` while below `limit`, by `step`,
    /// `body` given each count.
    pub(crate) fn count(
        &mut self,
        (start, limit, step): (Id, Id, Id),
        body: impl FnOnce(&mut Self, Id) -> Outcome<()>,
    ) -> Outcome<()> {
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        let bool_ty = self.lower().scalar(Scalar::Bool)?;
        let pointer = self.lower().pointer(StorageClass::FUNCTION, u32_ty)?;
        let counter = self.f.local(pointer).map_err(built)?;
        self.f.emit(Op::STORE, &[counter, start]).map_err(built)?;
        let (header, check, inside, back, merge) = (
            self.label()?,
            self.label()?,
            self.label()?,
            self.label()?,
            self.label()?,
        );
        self.branch(header)?;
        self.begin(header)?;
        self.loop_merge(merge, back)?;
        self.terminate(Op::BRANCH, &[check])?;
        self.begin(check)?;
        let count = self.f.op(Op::LOAD, u32_ty, &[counter]).map_err(built)?;
        let within = self
            .f
            .op(Op::U_LESS_THAN, bool_ty, &[count, limit])
            .map_err(built)?;
        self.terminate(Op::BRANCH_CONDITIONAL, &[within, inside, merge])?;
        self.begin(inside)?;
        body(self, count)?;
        self.branch(back)?;
        self.begin(back)?;
        let next = self
            .f
            .op(Op::I_ADD, u32_ty, &[count, step])
            .map_err(built)?;
        self.f.emit(Op::STORE, &[counter, next]).map_err(built)?;
        self.terminate(Op::BRANCH, &[header])?;
        self.begin(merge)
    }

    /// Run `body` where `condition` holds.
    pub(crate) fn when(
        &mut self,
        condition: Id,
        body: impl FnOnce(&mut Self) -> Outcome<()>,
    ) -> Outcome<()> {
        let (taken, merge) = (self.label()?, self.label()?);
        self.select_merge(merge)?;
        self.terminate(Op::BRANCH_CONDITIONAL, &[condition, taken, merge])?;
        self.begin(taken)?;
        body(self)?;
        self.branch(merge)?;
        self.begin(merge)
    }

    /// Run `body`, where `masked`, only while the invocation has not
    /// discarded: in a module that discards, under a selection on the flag.
    pub(crate) fn unless_discarded(
        &mut self,
        masked: bool,
        body: impl FnOnce(&mut Self) -> Outcome<()>,
    ) -> Outcome<()> {
        let Some(flag) = self.t.discarded.filter(|_| masked) else {
            return body(self);
        };
        let (keep, merge) = self.branch_unless(flag)?;
        self.begin(keep)?;
        body(self)?;
        self.branch(merge)?;
        self.begin(merge)
    }

    /// [`Self::unless_discarded`] for a `body` with a value of `ty`: zero
    /// where the invocation has discarded.
    pub(crate) fn discarded_or(
        &mut self,
        masked: bool,
        ty: Id,
        body: impl FnOnce(&mut Self) -> Outcome<Id>,
    ) -> Outcome<Id> {
        let Some(flag) = self.t.discarded.filter(|_| masked) else {
            return body(self);
        };
        let header = self.f.block().ok_or(Stop::Internal)?;
        let (keep, merge) = self.branch_unless(flag)?;
        self.begin(keep)?;
        let value = body(self)?;
        let end = self.f.block().ok_or(Stop::Internal)?;
        self.branch(merge)?;
        self.begin(merge)?;
        let zero = self.f.module().constant_null(ty).map_err(built)?;
        self.f
            .op(Op::PHI, ty, &[zero, header, value, end])
            .map_err(built)
    }

    /// A selection on `flag`: its merge where it is set, a block of its
    /// own, returned with the merge, where it is not.
    fn branch_unless(&mut self, flag: Id) -> Outcome<(Id, Id)> {
        let bool_ty = self.lower().scalar(Scalar::Bool)?;
        let set = self.f.op(Op::LOAD, bool_ty, &[flag]).map_err(built)?;
        let (keep, merge) = (self.label()?, self.label()?);
        self.select_merge(merge)?;
        self.terminate(Op::BRANCH_CONDITIONAL, &[set, merge, keep])?;
        Ok((keep, merge))
    }

    /// An assignment: the memory written found first, then the value, and
    /// a compound one's old value read once.
    fn assign(&mut self, lhs: ExprId, op: Option<BinaryOp>, rhs: ExprId) -> Outcome<()> {
        let target = self.written(lhs)?;
        let value = self.value(rhs)?;
        let value = match op {
            None => value,
            Some(op) => {
                let (old, ty) = self.read_written(&target)?;
                let right = (value, self.c.info(rhs).converted, self.safe_divisor(rhs));
                self.binary(op, (old, ty), right, ty)?
            }
        };
        self.write(&target, value)
    }

    /// `++` and `--`: the integer in memory stepped by one, wrapping.
    fn step_by_one(&mut self, target: ExprId, op: BinaryOp) -> Outcome<()> {
        let place = self.reference(target)?;
        let old = self.load(&place)?;
        let scalar = self.c.types.scalar_of(place.ty).ok_or(Stop::Internal)?;
        let one = self.splat_constant(1, None, scalar)?;
        let stepped = self.binary(op, (old, place.ty), (one, place.ty, false), place.ty)?;
        self.store(&place, stepped)
    }

    /// What an assignment to `lhs` writes: a memory view, or the distinct
    /// lanes a swizzle names of a vector one, each lane its own location.
    fn written(&mut self, lhs: ExprId) -> Outcome<Written> {
        let kind = self.c.tree.expr(lhs).map(|e| e.kind);
        if let (Some(ExprKind::Member(base, _)), Denotes::Swizzle(lanes, count)) =
            (kind, self.c.info(lhs).denotes)
        {
            if count > 1 {
                return Ok(Written::Lanes(self.reference(base)?, lanes, count));
            }
        }
        Ok(Written::Whole(self.reference(lhs)?))
    }

    /// The value `target` holds, and its type.
    fn read_written(&mut self, target: &Written) -> Outcome<(Id, TyId)> {
        match target {
            Written::Whole(place) => Ok((self.load(place)?, place.ty)),
            Written::Lanes(vector, lanes, count) => {
                let scalar = self.c.types.scalar_of(vector.ty).ok_or(Stop::Internal)?;
                let lane_ty = self.c.types.scalar(scalar)?;
                let ty = self.c.types.intern(Ty::Vector(*count, scalar))?;
                let mut parts = [vector.base; 4];
                for (part, &lane) in parts.iter_mut().zip(lanes.iter().take(usize::from(*count))) {
                    *part = self.load(&vector.step(Index::Const(u32::from(lane)), lane_ty)?)?;
                }
                let vector_ty = self.value_type(ty)?;
                let parts = parts.get(..usize::from(*count)).ok_or(Stop::Internal)?;
                Ok((
                    self.f
                        .op(Op::COMPOSITE_CONSTRUCT, vector_ty, parts)
                        .map_err(built)?,
                    ty,
                ))
            }
        }
    }

    fn write(&mut self, target: &Written, value: Id) -> Outcome<()> {
        match target {
            Written::Whole(place) => self.store(place, value),
            Written::Lanes(vector, lanes, count) => {
                let scalar = self.c.types.scalar_of(vector.ty).ok_or(Stop::Internal)?;
                let lane_ty = self.c.types.scalar(scalar)?;
                let component = self.lower().scalar(scalar)?;
                for (at, &lane) in (0u32..).zip(lanes.iter().take(usize::from(*count))) {
                    let part = self
                        .f
                        .op_args(
                            Op::COMPOSITE_EXTRACT,
                            component,
                            &[Arg::Id(value), Arg::Word(at)],
                        )
                        .map_err(built)?;
                    self.store(&vector.step(Index::Const(u32::from(lane)), lane_ty)?, part)?;
                }
                Ok(())
            }
        }
    }
}

/// The memory an assignment writes.
enum Written {
    Whole(Place),
    Lanes(Place, [u8; 4], u8),
}
