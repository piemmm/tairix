//! Expressions: each root walked with an explicit stack, children first, a
//! child's result held on a second stack until its parent takes it; a
//! const- or override-expression written as its constant without its
//! subtree; a memory view kept as a place until the load rule reads it; a
//! short-circuit whose right side has effects written as a branch.

use alloc::vec::Vec;

use tairix_spirv::enumerant::StorageClass;
use tairix_spirv::{Arg, GlslStd450, Id, Op};

use super::function::{Local, Writer};
use super::{built, Flavour, Index, Place, Root, Shape, Step};
use crate::check::call::Construct;
use crate::check::decl::DeclInfo;
use crate::check::expr::Form;
use crate::check::{Denotes, Stage};
use crate::diag::{push, reserve, Outcome, Stop};
use crate::predeclared::Builtin;
use crate::syntax::{BinaryOp, ExprId, ExprKind, UnaryOp};
use crate::types::{Scalar, Space, Ty, TyId};

/// An expression as written: a value, a memory view, a texture or
/// sampler's handle — the pointer it is loaded through where it is used —
/// or the nothing a call with no result gives.
#[derive(Debug)]
pub(crate) enum Lowered {
    Value(Id),
    Place(Place),
    Handle(Id),
    Void,
}

impl Lowered {
    pub(crate) fn value(&self) -> Outcome<Id> {
        match *self {
            Self::Value(value) => Ok(value),
            Self::Place(_) | Self::Handle(_) | Self::Void => Err(Stop::Internal),
        }
    }

    pub(crate) fn place(&self) -> Outcome<&Place> {
        match self {
            Self::Place(place) => Ok(place),
            Self::Value(_) | Self::Handle(_) | Self::Void => Err(Stop::Internal),
        }
    }

    /// The pointer a texture or sampler is loaded through.
    pub(crate) fn handle(&self) -> Outcome<Id> {
        match self {
            Self::Handle(handle) => Ok(*handle),
            Self::Place(place) if place.space == Space::Handle => Ok(place.base),
            Self::Place(_) | Self::Value(_) | Self::Void => Err(Stop::Internal),
        }
    }
}

/// The bits of `x` as a float of `scalar`.
pub(crate) fn float_bits(x: f32, scalar: Scalar) -> u32 {
    if scalar == Scalar::F16 {
        u32::from(tairix_util::mathf::to_half(x))
    } else {
        x.to_bits()
    }
}

#[derive(Copy, Clone, Debug)]
enum Task {
    Enter(ExprId),
    /// A short-circuit's left side is written: branch on it.
    Decide(ExprId),
    Exit(ExprId),
}

/// A short-circuit written as a branch: its node, the block its left side
/// ended in, and its merge.
#[derive(Copy, Clone, Debug)]
struct Circuit {
    id: ExprId,
    left_block: Id,
    merge: Id,
}

/// The walk over one root: what is still to visit, the results waiting for
/// their parents, the branches open, and — once a short-circuit asks — how
/// many effectful nodes precede each node of the root's subtree.
struct Walk {
    tasks: Vec<Task>,
    results: Vec<Lowered>,
    circuits: Vec<Circuit>,
    base: u32,
    root: u32,
    effects: Option<Vec<u32>>,
}

impl Writer<'_, '_, '_> {
    /// The value of expression `root`.
    pub(crate) fn value(&mut self, root: ExprId) -> Outcome<Id> {
        self.expression(root)?.value()
    }

    /// Expression `root` as its use takes it.
    pub(crate) fn expression(&mut self, root: ExprId) -> Outcome<Lowered> {
        self.walk(root, true)
    }

    /// The memory view expression `root` is, which its use writes and so
    /// does not read whole.
    pub(crate) fn reference(&mut self, root: ExprId) -> Outcome<Place> {
        match self.walk(root, false)? {
            Lowered::Place(place) => Ok(place),
            _ => Err(Stop::Internal),
        }
    }

    /// Expression `root`, read by the load rule at its root only where
    /// `load_root`.
    fn walk(&mut self, root: ExprId, load_root: bool) -> Outcome<Lowered> {
        let mut walk = Walk {
            tasks: Vec::new(),
            results: Vec::new(),
            circuits: Vec::new(),
            base: *self.c.shape.subtree(root).start(),
            root: root.0,
            effects: None,
        };
        push(&mut walk.tasks, Task::Enter(root))?;
        while let Some(task) = walk.tasks.pop() {
            match task {
                Task::Enter(id) => self.enter(id, &mut walk)?,
                Task::Decide(id) => self.decide(id, &mut walk)?,
                Task::Exit(id) => {
                    let mut lowered = self.exit(id, &mut walk)?;
                    if load_root || id != root {
                        lowered = self.apply_load(id, lowered)?;
                    }
                    push(&mut walk.results, lowered)?;
                }
            }
        }
        let lowered = walk.results.pop().ok_or(Stop::Internal)?;
        if walk.results.is_empty() {
            Ok(lowered)
        } else {
            Err(Stop::Internal)
        }
    }

    fn enter(&mut self, id: ExprId, walk: &mut Walk) -> Outcome<()> {
        if let Some(value) = self.constant_of(id)? {
            return push(&mut walk.results, Lowered::Value(value));
        }
        let tree = self.c.tree;
        let kind = tree.expr(id).ok_or(Stop::Internal)?.kind;
        if let ExprKind::Binary(BinaryOp::LogicalAnd | BinaryOp::LogicalOr, lhs, rhs) = kind {
            if self.has_effects(rhs, walk)? {
                push(&mut walk.tasks, Task::Exit(id))?;
                push(&mut walk.tasks, Task::Enter(rhs))?;
                push(&mut walk.tasks, Task::Decide(id))?;
                return push(&mut walk.tasks, Task::Enter(lhs));
            }
        }
        push(&mut walk.tasks, Task::Exit(id))?;
        // Children in reverse, so the first is written first; a call's
        // template arguments are types and write nothing.
        match kind {
            ExprKind::Literal(_) | ExprKind::Ident(_) => Ok(()),
            ExprKind::Call(_, args) => {
                for &arg in tree.expr_list(args).iter().rev() {
                    push(&mut walk.tasks, Task::Enter(arg))?;
                }
                Ok(())
            }
            ExprKind::Unary(_, operand) | ExprKind::Member(operand, _) => {
                push(&mut walk.tasks, Task::Enter(operand))
            }
            ExprKind::Binary(_, first, second) | ExprKind::Index(first, second) => {
                push(&mut walk.tasks, Task::Enter(second))?;
                push(&mut walk.tasks, Task::Enter(first))
            }
        }
    }

    /// Branch on a short-circuit's left side, its right side written only
    /// where the left does not decide.
    fn decide(&mut self, id: ExprId, walk: &mut Walk) -> Outcome<()> {
        let Some(ExprKind::Binary(op, _, _)) = self.c.tree.expr(id).map(|e| e.kind) else {
            return Err(Stop::Internal);
        };
        let left = walk.results.last().ok_or(Stop::Internal)?.value()?;
        let left_block = self.f.block().ok_or(Stop::Internal)?;
        let (right, merge) = (self.label()?, self.label()?);
        self.select_merge(merge)?;
        let (yes, no) = if op == BinaryOp::LogicalAnd {
            (right, merge)
        } else {
            (merge, right)
        };
        self.terminate(Op::BRANCH_CONDITIONAL, &[left, yes, no])?;
        self.begin(right)?;
        push(
            &mut walk.circuits,
            Circuit {
                id,
                left_block,
                merge,
            },
        )
    }

    /// The constant node `id` is, where its value is known.
    fn constant_of(&mut self, id: ExprId) -> Outcome<Option<Id>> {
        let info = self.c.info(id);
        match info.value_id() {
            Some(value) if info.stage != Stage::Runtime => {
                self.lower().constant(value, info.converted).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Whether evaluating `root` may do more than give its value: call a
    /// user function, or a built-in that writes or synchronises.
    fn has_effects(&self, root: ExprId, walk: &mut Walk) -> Outcome<bool> {
        let start = *self.c.shape.subtree(root).start();
        let end = root.0 + 1;
        if walk.effects.is_none() {
            let mut counts: Vec<u32> = Vec::new();
            reserve(&mut counts, (walk.root - walk.base) as usize + 2)?;
            counts.push(0);
            let mut total = 0u32;
            for raw in walk.base..=walk.root {
                if self.effectful(ExprId(raw)) {
                    total += 1;
                }
                counts.push(total);
            }
            walk.effects = Some(counts);
        }
        let counts = walk.effects.as_deref().ok_or(Stop::Internal)?;
        let at = |raw: u32| -> Outcome<u32> {
            counts
                .get(raw.checked_sub(walk.base).ok_or(Stop::Internal)? as usize)
                .copied()
                .ok_or(Stop::Internal)
        };
        Ok(at(end)? != at(start)?)
    }

    fn effectful(&self, id: ExprId) -> bool {
        match self.c.info(id).denotes {
            Denotes::CallFunction(_) => true,
            Denotes::CallBuiltin(builtin) => effectful(builtin),
            _ => false,
        }
    }

    /// A memory view its use reads, read: a texture's or sampler's taken as
    /// its handle.
    fn apply_load(&mut self, id: ExprId, lowered: Lowered) -> Outcome<Lowered> {
        let info = self.c.info(id);
        let Ty::Reference { space, .. } = self.c.types.get(info.ty) else {
            return Ok(lowered);
        };
        if info.converted == info.ty {
            return Ok(lowered);
        }
        let Lowered::Place(place) = lowered else {
            return Ok(lowered);
        };
        if space == Space::Handle {
            return Ok(Lowered::Handle(place.base));
        }
        self.load(&place).map(Lowered::Value)
    }

    /// Node `id` from its children's results, taken from the top of the
    /// result stack.
    fn exit(&mut self, id: ExprId, walk: &mut Walk) -> Outcome<Lowered> {
        let info = self.c.info(id);
        let tree = self.c.tree;
        let kind = tree.expr(id).ok_or(Stop::Internal)?.kind;
        let results = &mut walk.results;
        Ok(match kind {
            ExprKind::Literal(_) => return Err(Stop::Internal),
            ExprKind::Ident(_) => self.ident(id)?,
            ExprKind::Unary(op, _) => {
                let operand = results.pop().ok_or(Stop::Internal)?;
                match op {
                    UnaryOp::AddressOf | UnaryOp::Indirection => operand,
                    UnaryOp::Negate => {
                        let ty = self.value_type(info.ty)?;
                        let negate = if self.float_typed(info.ty) {
                            Op::F_NEGATE
                        } else {
                            Op::S_NEGATE
                        };
                        Lowered::Value(self.f.op(negate, ty, &[operand.value()?]).map_err(built)?)
                    }
                    UnaryOp::Not | UnaryOp::Complement => {
                        let ty = self.value_type(info.ty)?;
                        let op = if op == UnaryOp::Not {
                            Op::LOGICAL_NOT
                        } else {
                            Op::NOT
                        };
                        Lowered::Value(self.f.op(op, ty, &[operand.value()?]).map_err(built)?)
                    }
                }
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let right = results.pop().ok_or(Stop::Internal)?.value()?;
                let left = results.pop().ok_or(Stop::Internal)?.value()?;
                let circuit = walk.circuits.last().copied().filter(|c| c.id == id);
                match (op, circuit) {
                    (BinaryOp::LogicalAnd | BinaryOp::LogicalOr, Some(circuit)) => {
                        walk.circuits.pop();
                        let bool_ty = self.lower().scalar(Scalar::Bool)?;
                        let right_block = self.f.block().ok_or(Stop::Internal)?;
                        self.branch(circuit.merge)?;
                        self.begin(circuit.merge)?;
                        let short = self.lower().boolean(op == BinaryOp::LogicalOr)?;
                        Lowered::Value(
                            self.f
                                .op(
                                    Op::PHI,
                                    bool_ty,
                                    &[short, circuit.left_block, right, right_block],
                                )
                                .map_err(built)?,
                        )
                    }
                    (BinaryOp::LogicalAnd | BinaryOp::LogicalOr, None) => {
                        let bool_ty = self.lower().scalar(Scalar::Bool)?;
                        let op = if op == BinaryOp::LogicalAnd {
                            Op::LOGICAL_AND
                        } else {
                            Op::LOGICAL_OR
                        };
                        Lowered::Value(self.f.op(op, bool_ty, &[left, right]).map_err(built)?)
                    }
                    _ => {
                        let left = (left, self.c.info(lhs).converted);
                        let right = (right, self.c.info(rhs).converted, self.safe_divisor(rhs));
                        Lowered::Value(self.binary(op, left, right, info.ty)?)
                    }
                }
            }
            ExprKind::Member(object, _) => {
                let lowered = results.pop().ok_or(Stop::Internal)?;
                self.member(id, &lowered, object)?
            }
            ExprKind::Index(object, index) => {
                let at = results.pop().ok_or(Stop::Internal)?;
                let lowered = results.pop().ok_or(Stop::Internal)?;
                let at = self.index_of(index, &at)?;
                self.indexed(id, &lowered, object, at)?
            }
            ExprKind::Call(_, args) => {
                let exprs = tree.expr_list(args);
                let at = results
                    .len()
                    .checked_sub(exprs.len())
                    .ok_or(Stop::Internal)?;
                let lowered = self.call(id, exprs, results.get(at..).ok_or(Stop::Internal)?)?;
                results.truncate(at);
                lowered
            }
        })
    }

    /// Whether `divisor` is a constant integer no division is undefined by:
    /// never zero, nor -1 where it is signed.
    pub(crate) fn safe_divisor(&self, divisor: ExprId) -> bool {
        let info = self.c.info(divisor);
        let Some(value) = info.value_id().filter(|_| info.stage != Stage::Runtime) else {
            return false;
        };
        self.c
            .values
            .integer(value)
            .is_some_and(|n| n != 0 && n != -1)
    }

    /// The SPIR-V type a value of `ty` has.
    pub(crate) fn value_type(&mut self, ty: TyId) -> Outcome<Id> {
        self.lower().ty(ty, Flavour::Value)
    }

    pub(crate) fn float_typed(&self, ty: TyId) -> bool {
        self.c.types.scalar_of(ty).is_some_and(Scalar::is_float)
    }

    /// An identifier: a local as bound, or a module-scope variable's place
    /// or handle.
    fn ident(&mut self, id: ExprId) -> Outcome<Lowered> {
        match self.c.info(id).denotes {
            Denotes::Local(local) => Ok(match self.local(local)? {
                Local::Value(v) => Lowered::Value(*v),
                Local::Place(place) => Lowered::Place(place.duplicate()?),
                Local::Handle(handle) => Lowered::Handle(*handle),
                Local::Unset => return Err(Stop::Internal),
            }),
            Denotes::Global(decl) => {
                let Some(&DeclInfo::Global(global)) = self.c.decls.get(decl as usize) else {
                    return Err(Stop::Internal);
                };
                let written = self.lower().global(decl)?;
                Ok(Lowered::Place(Place {
                    base: written.id,
                    root: Root::Global(decl),
                    space: global.space,
                    wrapped: written.wrapped,
                    indexes: Vec::new(),
                    ty: global.store,
                }))
            }
            _ => Err(Stop::Internal),
        }
    }

    /// A member or swizzle of a value or a memory view.
    fn member(&mut self, id: ExprId, object: &Lowered, object_expr: ExprId) -> Outcome<Lowered> {
        let info = self.c.info(id);
        let result = self.c.types.store_of(info.ty).unwrap_or(info.ty);
        match (info.denotes, object) {
            (Denotes::Member(index), Lowered::Place(place)) => {
                Ok(Lowered::Place(place.step(Index::Const(index), result)?))
            }
            (Denotes::Member(index), Lowered::Value(value)) => {
                let object_ty = self.c.info(object_expr).converted;
                let member = self.component_type(object_ty, index)?;
                let flavour = self.lower().value_flavour(object_ty)?;
                Ok(Lowered::Value(self.extract(
                    *value,
                    (member, flavour),
                    &[index],
                )?))
            }
            (Denotes::Swizzle(lanes, 1), Lowered::Place(place)) => Ok(Lowered::Place(
                place.step(Index::Const(u32::from(lanes[0])), result)?,
            )),
            (Denotes::Swizzle(lanes, count), object) => {
                let vector = match object {
                    Lowered::Place(place) => self.load(place)?,
                    other => other.value()?,
                };
                let ty = self.value_type(result)?;
                self.shuffle(
                    vector,
                    ty,
                    lanes.get(..usize::from(count)).ok_or(Stop::Internal)?,
                )
                .map(Lowered::Value)
            }
            _ => Err(Stop::Internal),
        }
    }

    /// The lanes `lanes` of `vector`, as one component or a vector of `ty`.
    pub(crate) fn shuffle(&mut self, vector: Id, ty: Id, lanes: &[u8]) -> Outcome<Id> {
        if let [lane] = lanes {
            return self
                .f
                .op_args(
                    Op::COMPOSITE_EXTRACT,
                    ty,
                    &[Arg::Id(vector), Arg::Word(u32::from(*lane))],
                )
                .map_err(built);
        }
        let mut args: Vec<Arg<'_>> = Vec::new();
        reserve(&mut args, 2 + lanes.len())?;
        args.push(Arg::Id(vector));
        args.push(Arg::Id(vector));
        args.extend(lanes.iter().map(|&l| Arg::Word(u32::from(l))));
        self.f.op_args(Op::VECTOR_SHUFFLE, ty, &args).map_err(built)
    }

    /// An index's place in an access: constant where its value is known,
    /// else its value as a `u32`.
    fn index_of(&mut self, index: ExprId, lowered: &Lowered) -> Outcome<Index> {
        let info = self.c.info(index);
        if let Some(value) = info.value_id().filter(|_| info.stage != Stage::Runtime) {
            let n = self.c.values.integer(value).ok_or(Stop::Internal)?;
            return Ok(Index::Const(u32::try_from(n).map_err(|_| Stop::Internal)?));
        }
        Ok(Index::Dynamic(
            self.as_unsigned(lowered.value()?, info.converted)?,
        ))
    }

    /// An `i32` or `u32` value as a `u32`.
    pub(crate) fn as_unsigned(&mut self, value: Id, ty: TyId) -> Outcome<Id> {
        if self.c.types.scalar_of(ty) == Some(Scalar::U32) {
            return Ok(value);
        }
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        self.f.op(Op::BITCAST, u32_ty, &[value]).map_err(built)
    }

    /// An element of a value or a memory view.
    fn indexed(
        &mut self,
        id: ExprId,
        object: &Lowered,
        object_expr: ExprId,
        at: Index,
    ) -> Outcome<Lowered> {
        let info = self.c.info(id);
        let result = self.c.types.store_of(info.ty).unwrap_or(info.ty);
        let value = match object {
            Lowered::Place(place) => return Ok(Lowered::Place(place.step(at, result)?)),
            other => other.value()?,
        };
        let object_ty = self.c.info(object_expr).converted;
        let flavour = self.lower().value_flavour(object_ty)?;
        let element = self.lower().ty(result, flavour)?;
        Ok(Lowered::Value(match at {
            Index::Const(n) => self
                .f
                .op_args(
                    Op::COMPOSITE_EXTRACT,
                    element,
                    &[Arg::Id(value), Arg::Word(n)],
                )
                .map_err(built)?,
            Index::Dynamic(n) if matches!(self.c.types.get(object_ty), Ty::Vector(..)) => self
                .f
                .op(Op::VECTOR_EXTRACT_DYNAMIC, element, &[value, n])
                .map_err(built)?,
            Index::Dynamic(n) => {
                // Logical addressing reaches a composite's element by a value
                // only through memory.
                let whole = self.lower().ty(object_ty, flavour)?;
                let pointer = self.lower().pointer(StorageClass::FUNCTION, whole)?;
                let temporary = self.f.local(pointer).map_err(built)?;
                self.f.emit(Op::STORE, &[temporary, value]).map_err(built)?;
                let element_pointer = self.lower().pointer(StorageClass::FUNCTION, element)?;
                let at = self
                    .f
                    .op(Op::ACCESS_CHAIN, element_pointer, &[temporary, n])
                    .map_err(built)?;
                self.f.op(Op::LOAD, element, &[at]).map_err(built)?
            }
        }))
    }

    /// A call: a user function's variant, a built-in, or a constructor.
    fn call(&mut self, id: ExprId, exprs: &[ExprId], args: &[Lowered]) -> Outcome<Lowered> {
        match self.c.info(id).denotes {
            Denotes::CallFunction(decl) => self.user_call(id, decl, args),
            Denotes::CallBuiltin(builtin) => self.builtin(id, builtin, exprs, args),
            Denotes::Construct(how) => {
                let mut values: Vec<(Id, TyId)> = Vec::new();
                reserve(&mut values, exprs.len())?;
                for (&arg, lowered) in exprs.iter().zip(args) {
                    values.push((lowered.value()?, self.c.info(arg).converted));
                }
                self.construct(self.c.info(id).ty, how, &values)
                    .map(Lowered::Value)
            }
            _ => Err(Stop::Internal),
        }
    }

    /// A user function's call: its variant for the shapes of the pointers
    /// its arguments carry, each pointer passed as its object and the
    /// element indexes into it.
    fn user_call(&mut self, id: ExprId, decl: u32, args: &[Lowered]) -> Outcome<Lowered> {
        let Some(&DeclInfo::Function(signature)) = self.c.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        let mut shapes: Vec<Shape> = Vec::new();
        let mut operands: Vec<Id> = Vec::new();
        for (offset, lowered) in args.iter().enumerate() {
            let param = self
                .c
                .params
                .get(signature.params.0 as usize + offset)
                .copied()
                .ok_or(Stop::Internal)?;
            match self.c.types.get(param.ty) {
                Ty::Pointer { .. } => {
                    let place = lowered.place()?;
                    let (shape, elements) = self.shape_of(place)?;
                    if matches!(shape.root, Root::Object { .. }) {
                        push(&mut operands, place.base)?;
                    }
                    for element in elements {
                        push(&mut operands, element)?;
                    }
                    push(&mut shapes, shape)?;
                }
                Ty::Texture(_) | Ty::Sampler { .. } => push(&mut operands, lowered.handle()?)?,
                _ => push(&mut operands, lowered.value()?)?,
            }
        }
        let variant = self.lower().variant((decl, shapes))?;
        let result = self.value_type(self.c.info(id).ty)?;
        let mut call: Vec<Id> = Vec::new();
        reserve(&mut call, operands.len() + 1)?;
        call.push(variant);
        call.extend_from_slice(&operands);
        let value = self.f.op(Op::FUNCTION_CALL, result, &call).map_err(built)?;
        Ok(match signature.result {
            Some(_) => Lowered::Value(value),
            None => Lowered::Void,
        })
    }

    /// The shape a place has as a pointer argument — its root, and each
    /// step a structure's member or an element — and its element indexes as
    /// `u32` values.
    fn shape_of(&mut self, place: &Place) -> Outcome<(Shape, Vec<Id>)> {
        let mut ty = match place.root {
            Root::Global(decl) => match self.c.decls.get(decl as usize) {
                Some(DeclInfo::Global(global)) => global.store,
                _ => return Err(Stop::Internal),
            },
            Root::Object { ty, .. } => ty,
        };
        let mut steps = Vec::new();
        let mut elements = Vec::new();
        reserve(&mut steps, place.indexes.len())?;
        for &index in &place.indexes {
            let is_struct = matches!(self.c.types.get(ty), Ty::Struct(_));
            let next = match (is_struct, index) {
                (true, Index::Const(n)) => {
                    steps.push(Step::Member(n));
                    n
                }
                (true, Index::Dynamic(_)) => return Err(Stop::Internal),
                (false, Index::Const(n)) => {
                    steps.push(Step::Element);
                    push(&mut elements, self.lower().uint(n)?)?;
                    n
                }
                (false, Index::Dynamic(value)) => {
                    steps.push(Step::Element);
                    push(&mut elements, value)?;
                    0
                }
            };
            ty = self.component_type(ty, next)?;
        }
        Ok((
            Shape {
                root: place.root,
                steps,
            },
            elements,
        ))
    }

    /// `op` over two values of the types its overload took them as, giving
    /// a value of `result`: a scalar beside a vector splat or scaling it,
    /// matrices multiplied as linear algebra and added column by column, and
    /// integer division, remainder and shifts as WGSL defines them where
    /// SPIR-V leaves them undefined.
    pub(crate) fn binary(
        &mut self,
        op: BinaryOp,
        (a, a_ty): (Id, TyId),
        (b, b_ty, safe_divisor): (Id, TyId, bool),
        result: TyId,
    ) -> Outcome<Id> {
        let result_type = self.value_type(result)?;
        let (fa, scalar) = self.c.form(a_ty).ok_or(Stop::Internal)?;
        let (fb, _) = self.c.form(b_ty).ok_or(Stop::Internal)?;
        let float = scalar.is_float();
        if let Some((linear, swapped)) = linear_algebra(op, (fa, fb), float) {
            let operands = if swapped { [b, a] } else { [a, b] };
            return self.f.op(linear, result_type, &operands).map_err(built);
        }
        if let (BinaryOp::Add | BinaryOp::Subtract, Form::Matrix(columns, rows)) = (op, fa) {
            return self.matrix_columns(op, (a, b), (columns, rows, scalar), result_type);
        }
        let lanes = match (fa, fb) {
            (Form::Vector(n), _) | (_, Form::Vector(n)) => Some(n),
            _ => None,
        };
        let (a, b) = match (lanes, fa, fb) {
            (Some(n), Form::Scalar, _) => (self.splat(a, n, scalar)?, b),
            (Some(n), _, Form::Scalar) => (a, self.splat(b, n, scalar)?),
            _ => (a, b),
        };
        let unsigned = scalar == Scalar::U32;
        let boolean = scalar == Scalar::Bool;
        let simple = match op {
            BinaryOp::Add if float => Op::F_ADD,
            BinaryOp::Add => Op::I_ADD,
            BinaryOp::Subtract if float => Op::F_SUB,
            BinaryOp::Subtract => Op::I_SUB,
            BinaryOp::Multiply if float => Op::F_MUL,
            BinaryOp::Multiply => Op::I_MUL,
            BinaryOp::Divide if float => Op::F_DIV,
            // The remainder's sign is the dividend's.
            BinaryOp::Remainder if float => Op::F_REM,
            BinaryOp::Divide if safe_divisor && scalar == Scalar::I32 => Op::S_DIV,
            BinaryOp::Divide if safe_divisor => Op::U_DIV,
            BinaryOp::Remainder if safe_divisor && scalar == Scalar::I32 => Op::S_REM,
            BinaryOp::Remainder if safe_divisor => Op::U_MOD,
            BinaryOp::Divide | BinaryOp::Remainder => {
                return self.integer_division(op, (a, b), (scalar, lanes), result_type)
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                let mask = self.splat_constant(31, lanes, Scalar::U32)?;
                let amount_ty = self.value_type(b_ty)?;
                let amount = self
                    .f
                    .op(Op::BITWISE_AND, amount_ty, &[b, mask])
                    .map_err(built)?;
                let shift = match (op, scalar) {
                    (BinaryOp::ShiftLeft, _) => Op::SHIFT_LEFT_LOGICAL,
                    (_, Scalar::I32) => Op::SHIFT_RIGHT_ARITHMETIC,
                    _ => Op::SHIFT_RIGHT_LOGICAL,
                };
                return self.f.op(shift, result_type, &[a, amount]).map_err(built);
            }
            BinaryOp::And if boolean => Op::LOGICAL_AND,
            BinaryOp::Or if boolean => Op::LOGICAL_OR,
            BinaryOp::And => Op::BITWISE_AND,
            BinaryOp::Or => Op::BITWISE_OR,
            BinaryOp::Xor => Op::BITWISE_XOR,
            BinaryOp::Equal if float => Op::F_ORD_EQUAL,
            BinaryOp::NotEqual if float => Op::F_UNORD_NOT_EQUAL,
            BinaryOp::Less if float => Op::F_ORD_LESS_THAN,
            BinaryOp::LessEqual if float => Op::F_ORD_LESS_THAN_EQUAL,
            BinaryOp::Greater if float => Op::F_ORD_GREATER_THAN,
            BinaryOp::GreaterEqual if float => Op::F_ORD_GREATER_THAN_EQUAL,
            BinaryOp::Equal if boolean => Op::LOGICAL_EQUAL,
            BinaryOp::NotEqual if boolean => Op::LOGICAL_NOT_EQUAL,
            BinaryOp::Equal => Op::I_EQUAL,
            BinaryOp::NotEqual => Op::I_NOT_EQUAL,
            BinaryOp::Less if unsigned => Op::U_LESS_THAN,
            BinaryOp::Less => Op::S_LESS_THAN,
            BinaryOp::LessEqual if unsigned => Op::U_LESS_THAN_EQUAL,
            BinaryOp::LessEqual => Op::S_LESS_THAN_EQUAL,
            BinaryOp::Greater if unsigned => Op::U_GREATER_THAN,
            BinaryOp::Greater => Op::S_GREATER_THAN,
            BinaryOp::GreaterEqual if unsigned => Op::U_GREATER_THAN_EQUAL,
            BinaryOp::GreaterEqual => Op::S_GREATER_THAN_EQUAL,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => return Err(Stop::Internal),
        };
        self.f.op(simple, result_type, &[a, b]).map_err(built)
    }

    /// Matrices added or subtracted column by column.
    fn matrix_columns(
        &mut self,
        op: BinaryOp,
        (a, b): (Id, Id),
        (columns, rows, scalar): (u8, u8, Scalar),
        result: Id,
    ) -> Outcome<Id> {
        let column = self.lower().vector_of(rows, scalar)?;
        let operation = if op == BinaryOp::Add {
            Op::F_ADD
        } else {
            Op::F_SUB
        };
        let mut parts = Vec::new();
        reserve(&mut parts, usize::from(columns))?;
        for index in 0..u32::from(columns) {
            let x = self
                .f
                .op_args(
                    Op::COMPOSITE_EXTRACT,
                    column,
                    &[Arg::Id(a), Arg::Word(index)],
                )
                .map_err(built)?;
            let y = self
                .f
                .op_args(
                    Op::COMPOSITE_EXTRACT,
                    column,
                    &[Arg::Id(b), Arg::Word(index)],
                )
                .map_err(built)?;
            parts.push(self.f.op(operation, column, &[x, y]).map_err(built)?);
        }
        self.f
            .op(Op::COMPOSITE_CONSTRUCT, result, &parts)
            .map_err(built)
    }

    /// Integer division and remainder as WGSL defines them: a divisor of
    /// zero divides as one, so a quotient is the dividend and a remainder
    /// zero; the most negative `i32` over -1 likewise.
    fn integer_division(
        &mut self,
        op: BinaryOp,
        (a, b): (Id, Id),
        (scalar, lanes): (Scalar, Option<u8>),
        result: Id,
    ) -> Outcome<Id> {
        let bool_ty = self.bool_type(lanes)?;
        let zero = self.splat_constant(0, lanes, scalar)?;
        let one = self.splat_constant(1, lanes, scalar)?;
        let mut unsafe_divisor = self.f.op(Op::I_EQUAL, bool_ty, &[b, zero]).map_err(built)?;
        if scalar == Scalar::I32 {
            let most_negative = self.splat_constant(i32::MIN.cast_unsigned(), lanes, scalar)?;
            let minus_one = self.splat_constant(u32::MAX, lanes, scalar)?;
            let lowest = self
                .f
                .op(Op::I_EQUAL, bool_ty, &[a, most_negative])
                .map_err(built)?;
            let negative_one = self
                .f
                .op(Op::I_EQUAL, bool_ty, &[b, minus_one])
                .map_err(built)?;
            let overflow = self
                .f
                .op(Op::LOGICAL_AND, bool_ty, &[lowest, negative_one])
                .map_err(built)?;
            unsafe_divisor = self
                .f
                .op(Op::LOGICAL_OR, bool_ty, &[unsafe_divisor, overflow])
                .map_err(built)?;
        }
        let divisor = self
            .f
            .op(Op::SELECT, result, &[unsafe_divisor, one, b])
            .map_err(built)?;
        let operation = match (op, scalar) {
            (BinaryOp::Divide, Scalar::I32) => Op::S_DIV,
            (BinaryOp::Divide, _) => Op::U_DIV,
            (_, Scalar::I32) => Op::S_REM,
            _ => Op::U_MOD,
        };
        self.f.op(operation, result, &[a, divisor]).map_err(built)
    }

    /// The `bool` type, or the vector of `n` of them.
    pub(crate) fn bool_type(&mut self, lanes: Option<u8>) -> Outcome<Id> {
        match lanes {
            Some(n) => self.lower().vector_of(n, Scalar::Bool),
            None => self.lower().scalar(Scalar::Bool),
        }
    }

    /// `value`, a `scalar`, repeated across a vector of `n`.
    pub(crate) fn splat(&mut self, value: Id, n: u8, scalar: Scalar) -> Outcome<Id> {
        let ty = self.lower().vector_of(n, scalar)?;
        let mut parts = Vec::new();
        reserve(&mut parts, usize::from(n))?;
        parts.resize(usize::from(n), value);
        self.f
            .op(Op::COMPOSITE_CONSTRUCT, ty, &parts)
            .map_err(built)
    }

    /// The constant of `scalar` whose bits are `bits`, across `lanes` where
    /// a vector's.
    pub(crate) fn splat_constant(
        &mut self,
        bits: u32,
        lanes: Option<u8>,
        scalar: Scalar,
    ) -> Outcome<Id> {
        let ty = self.lower().scalar(scalar)?;
        let one = self.f.module().constant(ty, bits).map_err(built)?;
        let Some(n) = lanes else {
            return Ok(one);
        };
        let vector = self.lower().vector_of(n, scalar)?;
        let mut parts = Vec::new();
        reserve(&mut parts, usize::from(n))?;
        parts.resize(usize::from(n), one);
        self.f
            .module()
            .constant_composite(vector, &parts)
            .map_err(built)
    }

    /// A constructor's value of `ty`, built `how` from `args`.
    pub(crate) fn construct(
        &mut self,
        ty: TyId,
        how: Construct,
        args: &[(Id, TyId)],
    ) -> Outcome<Id> {
        let target = self.value_type(ty)?;
        match how {
            Construct::Zero => self.f.module().constant_null(target).map_err(built),
            Construct::Convert => {
                let &(value, from) = args.first().ok_or(Stop::Internal)?;
                self.convert(value, from, ty)
            }
            Construct::Splat => {
                let &(value, _) = args.first().ok_or(Stop::Internal)?;
                let (Form::Vector(n), scalar) = self.c.form(ty).ok_or(Stop::Internal)? else {
                    return Err(Stop::Internal);
                };
                self.splat(value, n, scalar)
            }
            Construct::Parts => {
                let mut parts = Vec::new();
                reserve(&mut parts, args.len())?;
                parts.extend(args.iter().map(|&(value, _)| value));
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, target, &parts)
                    .map_err(built)
            }
            Construct::Elements => {
                let Ty::Matrix {
                    columns,
                    rows,
                    scalar,
                } = self.c.types.get(ty)
                else {
                    return Err(Stop::Internal);
                };
                let column = self.lower().vector_of(rows, scalar)?;
                let mut built_columns = Vec::new();
                reserve(&mut built_columns, usize::from(columns))?;
                let mut cells = Vec::new();
                reserve(&mut cells, usize::from(rows))?;
                for chunk in args.chunks(usize::from(rows.max(1))) {
                    cells.clear();
                    cells.extend(chunk.iter().map(|&(value, _)| value));
                    built_columns.push(
                        self.f
                            .op(Op::COMPOSITE_CONSTRUCT, column, &cells)
                            .map_err(built)?,
                    );
                }
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, target, &built_columns)
                    .map_err(built)
            }
        }
    }

    /// `value` of type `from` converted explicitly to `to`, as WGSL's
    /// conversions define it: a float to an integer saturating, NaN to zero.
    pub(crate) fn convert(&mut self, value: Id, from: TyId, to: TyId) -> Outcome<Id> {
        if from == to {
            return Ok(value);
        }
        let target = self.value_type(to)?;
        let source = self.c.types.scalar_of(from).ok_or(Stop::Internal)?;
        let dest = self.c.types.scalar_of(to).ok_or(Stop::Internal)?;
        let lanes = match self.c.types.get(to) {
            Ty::Matrix { columns, rows, .. } => {
                let column_from = self.c.types.intern(Ty::Vector(rows, source))?;
                let column_to = self.c.types.intern(Ty::Vector(rows, dest))?;
                let from_ty = self.value_type(column_from)?;
                let mut parts = Vec::new();
                reserve(&mut parts, usize::from(columns))?;
                for index in 0..u32::from(columns) {
                    let part = self
                        .f
                        .op_args(
                            Op::COMPOSITE_EXTRACT,
                            from_ty,
                            &[Arg::Id(value), Arg::Word(index)],
                        )
                        .map_err(built)?;
                    parts.push(self.convert(part, column_from, column_to)?);
                }
                return self
                    .f
                    .op(Op::COMPOSITE_CONSTRUCT, target, &parts)
                    .map_err(built);
            }
            Ty::Vector(n, _) => Some(n),
            _ => None,
        };
        let op = match (source, dest) {
            (Scalar::Bool, _) => {
                let one = if dest.is_float() {
                    float_bits(1.0, dest)
                } else {
                    1
                };
                let one = self.splat_constant(one, lanes, dest)?;
                let zero = self.splat_constant(0, lanes, dest)?;
                return self
                    .f
                    .op(Op::SELECT, target, &[value, one, zero])
                    .map_err(built);
            }
            (_, Scalar::Bool) => {
                let zero = self.splat_constant(0, lanes, source)?;
                // NaN is not zero, so it converts to true.
                let compare = if source.is_float() {
                    Op::F_UNORD_NOT_EQUAL
                } else {
                    Op::I_NOT_EQUAL
                };
                return self.f.op(compare, target, &[value, zero]).map_err(built);
            }
            (s, d) if s.is_float() && d.is_float() => Op::F_CONVERT,
            (Scalar::I32, d) if d.is_float() => Op::CONVERT_S_TO_F,
            (Scalar::U32, d) if d.is_float() => Op::CONVERT_U_TO_F,
            (s, d) if s.is_float() => {
                return self.float_to_integer(value, (from, s), (d, lanes), target)
            }
            _ => Op::BITCAST,
        };
        self.f.op(op, target, &[value]).map_err(built)
    }

    /// A float converted to an integer: clamped to the nearest values the
    /// source float holds within the integer's range, truncated toward
    /// zero, a NaN zero.
    fn float_to_integer(
        &mut self,
        value: Id,
        (from, source): (TyId, Scalar),
        (dest, lanes): (Scalar, Option<u8>),
        target: Id,
    ) -> Outcome<Id> {
        let (low, high) = match (dest, source) {
            (Scalar::I32, Scalar::F16) => (-65504.0f32, 65504.0f32),
            (_, Scalar::F16) => (0.0, 65504.0),
            (Scalar::I32, _) => (-2_147_483_648.0, 2_147_483_520.0),
            (_, _) => (0.0, 4_294_967_040.0),
        };
        let float_ty = self.value_type(from)?;
        let low = self.splat_constant(float_bits(low, source), lanes, source)?;
        let high = self.splat_constant(float_bits(high, source), lanes, source)?;
        let clamped = self
            .f
            .glsl(GlslStd450::N_CLAMP, float_ty, &[value, low, high])
            .map_err(built)?;
        let op = if dest == Scalar::I32 {
            Op::CONVERT_F_TO_S
        } else {
            Op::CONVERT_F_TO_U
        };
        let converted = self.f.op(op, target, &[clamped]).map_err(built)?;
        let bool_ty = self.bool_type(lanes)?;
        let nan = self.f.op(Op::IS_NAN, bool_ty, &[value]).map_err(built)?;
        let zero = self.splat_constant(0, lanes, dest)?;
        self.f
            .op(Op::SELECT, target, &[nan, zero, converted])
            .map_err(built)
    }
}

/// The linear-algebra instruction `op` is over values of the forms `fa` and
/// `fb`, and whether it takes them the other way round: a scalar scaling a
/// matrix or a float vector, and the products of matrices and vectors.
const fn linear_algebra(op: BinaryOp, (fa, fb): (Form, Form), float: bool) -> Option<(Op, bool)> {
    if !matches!(op, BinaryOp::Multiply) {
        return None;
    }
    match (fa, fb) {
        (Form::Matrix(..), Form::Scalar) => Some((Op::MATRIX_TIMES_SCALAR, false)),
        (Form::Scalar, Form::Matrix(..)) => Some((Op::MATRIX_TIMES_SCALAR, true)),
        (Form::Matrix(..), Form::Vector(_)) => Some((Op::MATRIX_TIMES_VECTOR, false)),
        (Form::Vector(_), Form::Matrix(..)) => Some((Op::VECTOR_TIMES_MATRIX, false)),
        (Form::Matrix(..), Form::Matrix(..)) => Some((Op::MATRIX_TIMES_MATRIX, false)),
        (Form::Vector(_), Form::Scalar) if float => Some((Op::VECTOR_TIMES_SCALAR, false)),
        (Form::Scalar, Form::Vector(_)) if float => Some((Op::VECTOR_TIMES_SCALAR, true)),
        _ => None,
    }
}

/// Whether a built-in writes memory or synchronises, so a short-circuit
/// must not run it where its left side decides.
pub(crate) const fn effectful(builtin: Builtin) -> bool {
    use Builtin as B;
    matches!(
        builtin,
        B::AtomicLoad
            | B::AtomicStore
            | B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak
            | B::StorageBarrier
            | B::TextureBarrier
            | B::WorkgroupBarrier
            | B::WorkgroupUniformLoad
            | B::TextureStore
    )
}
