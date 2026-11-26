//! Module-scope declarations: structures and aliases, constants and
//! overrides, variables, function signatures and assertions, each with the
//! attributes its place admits.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::{Checker, Denotes, ExprInfo, Stage, NONE};
use crate::diag::{error, push, reserve, slot, Code, Outcome, Span, Stop};
use crate::eval::{Value, ValueId};
use crate::syntax::{
    Attr, AttrArgs, Decl, DeclKind, ExprId, ExprKind, Field, FnDecl, OverrideDecl, Run, ValueDecl,
    VarDecl,
};
use crate::types::{Access, Count, Holds, Member, Origin, Scalar, Space, Ty, TyId};
use crate::OverrideKey;

/// A module-scope declaration as resolved.
#[derive(Copy, Clone, Debug)]
pub(crate) enum DeclInfo {
    /// Not yet resolved.
    Pending,
    /// A structure or an alias: the type it names.
    Type(TyId),
    Constant {
        ty: TyId,
        value: ValueId,
    },
    Override(Override),
    Global(Global),
    Function(Signature),
    Assert,
}

/// An override: its type, its initializer, the compile's value for it, and
/// its value once overrides take theirs — the compile's, else its
/// initializer's.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Override {
    pub(crate) ty: TyId,
    pub(crate) init: Option<ExprId>,
    pub(crate) given: Option<ValueId>,
    pub(crate) value: Option<ValueId>,
}

/// A module-scope variable.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Global {
    pub(crate) space: Space,
    pub(crate) access: Access,
    pub(crate) store: TyId,
    /// A resource's group and binding.
    pub(crate) binding: Option<(u32, u32)>,
    /// A private variable's initializer.
    pub(crate) init: Option<ExprId>,
}

/// An entry point's stage.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum EntryStage {
    Vertex,
    Fragment,
    Compute,
}

/// A function's signature: its parameters, into [`Checker::params`], its
/// result, and what its attributes declare.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Signature {
    pub(crate) params: (u32, u32),
    pub(crate) result: Option<TyId>,
    pub(crate) result_io: Io,
    pub(crate) stage: Option<EntryStage>,
    pub(crate) must_use: bool,
    /// A compute entry point's workgroup size: each dimension's
    /// expression, a missing one 1.
    pub(crate) workgroup: Option<[Option<ExprId>; 3]>,
}

/// How an entry point's parameter, result or structure member meets its
/// stage: as a built-in, at a location, or not at all.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Io {
    None,
    Builtin {
        value: BuiltinValue,
        invariant: bool,
        /// `frag_depth`'s depth mode, where `fragment_depth` gives one.
        depth: Option<Depth>,
    },
    Location {
        location: u32,
        /// As written; an unwritten one is assumed where it matters.
        interpolation: Option<Interpolation>,
        blend_src: Option<u32>,
    },
}

/// `frag_depth`'s depth mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Depth {
    Less,
    Greater,
}

/// A user-defined value's interpolation: its type and sampling.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Interpolation {
    pub(crate) kind: InterpolationKind,
    pub(crate) sampling: Sampling,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum InterpolationKind {
    Perspective,
    Linear,
    Flat,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sampling {
    Center,
    Centroid,
    Sample,
    First,
    Either,
}

/// A built-in value an entry point reads or writes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum BuiltinValue {
    VertexIndex,
    InstanceIndex,
    Position,
    FrontFacing,
    FragDepth,
    SampleIndex,
    SampleMask,
    LocalInvocationId,
    LocalInvocationIndex,
    GlobalInvocationId,
    GlobalInvocationIndex,
    WorkgroupId,
    WorkgroupIndex,
    NumWorkgroups,
}

/// The built-in values by name, ascending.
const BUILTIN_VALUES: [(&str, BuiltinValue); 14] = [
    ("frag_depth", BuiltinValue::FragDepth),
    ("front_facing", BuiltinValue::FrontFacing),
    ("global_invocation_id", BuiltinValue::GlobalInvocationId),
    (
        "global_invocation_index",
        BuiltinValue::GlobalInvocationIndex,
    ),
    ("instance_index", BuiltinValue::InstanceIndex),
    ("local_invocation_id", BuiltinValue::LocalInvocationId),
    ("local_invocation_index", BuiltinValue::LocalInvocationIndex),
    ("num_workgroups", BuiltinValue::NumWorkgroups),
    ("position", BuiltinValue::Position),
    ("sample_index", BuiltinValue::SampleIndex),
    ("sample_mask", BuiltinValue::SampleMask),
    ("vertex_index", BuiltinValue::VertexIndex),
    ("workgroup_id", BuiltinValue::WorkgroupId),
    ("workgroup_index", BuiltinValue::WorkgroupIndex),
];

/// The built-in values of extensions TAIRiX does not offer, each with the
/// extension.
const REFUSED_BUILTIN_VALUES: [(&str, &str); 6] = [
    ("clip_distances", "clip_distances"),
    ("num_subgroups", "subgroups"),
    ("primitive_index", "primitive_index"),
    ("subgroup_id", "subgroups"),
    ("subgroup_invocation_id", "subgroups"),
    ("subgroup_size", "subgroups"),
];

/// The attribute names, ascending.
const ATTRIBUTES: [&str; 18] = [
    "align",
    "binding",
    "blend_src",
    "builtin",
    "compute",
    "const",
    "diagnostic",
    "fragment",
    "group",
    "id",
    "interpolate",
    "invariant",
    "location",
    "must_use",
    "size",
    "subgroup_size",
    "vertex",
    "workgroup_size",
];

/// A structure member's layout and stage-interface attributes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemberAttrs {
    pub(crate) align: Option<u32>,
    pub(crate) size: Option<u32>,
    pub(crate) io: Io,
}

/// A formal parameter as resolved: its type, how it meets an entry point's
/// stage, and where it is written.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ParamInfo {
    pub(crate) ty: TyId,
    pub(crate) io: Io,
    pub(crate) span: Span,
}

/// Where an attribute stands, for the attributes each place admits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Place {
    Function,
    Param,
    Result,
    Member,
    Variable,
    Override,
}

impl Place {
    fn admits(self, name: &str) -> bool {
        match self {
            Self::Function => matches!(
                name,
                "vertex" | "fragment" | "compute" | "workgroup_size" | "must_use" | "diagnostic"
            ),
            Self::Param | Self::Result => {
                matches!(name, "builtin" | "location" | "interpolate" | "invariant")
            }
            Self::Member => matches!(
                name,
                "align"
                    | "size"
                    | "builtin"
                    | "location"
                    | "interpolate"
                    | "invariant"
                    | "blend_src"
            ),
            Self::Variable => matches!(name, "group" | "binding"),
            Self::Override => name == "id",
        }
    }
}

/// The values a compile gives overrides, sorted by key, with whether an
/// override has taken each.
pub(crate) struct Given<'a> {
    values: Vec<(OverrideKey<'a>, f64)>,
    taken: Vec<bool>,
}

impl<'a> Given<'a> {
    /// The values of `overrides`, refused where one key has two.
    pub(crate) fn new(overrides: &[(OverrideKey<'a>, f64)]) -> Outcome<Self> {
        let mut values = Vec::new();
        reserve(&mut values, overrides.len())?;
        values.extend_from_slice(overrides);
        values.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        if let Some(pair) = values.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            let key = pair[0].0;
            return Err(error(Code::OverrideValue, Span::default(), |m| {
                m.write_str("the compile gives two values for ")?;
                write_key(m, key)
            }));
        }
        let mut taken = Vec::new();
        reserve(&mut taken, values.len())?;
        taken.resize(values.len(), false);
        Ok(Self { values, taken })
    }

    /// The value given for `key`, marked taken.
    fn take(&mut self, key: OverrideKey<'_>) -> Option<f64> {
        let at = self.values.binary_search_by(|(k, _)| k.cmp(&key)).ok()?;
        self.taken[at] = true;
        Some(self.values[at].1)
    }

    /// Refuse a value no override took.
    pub(crate) fn all_taken(&self) -> Outcome<()> {
        match self.taken.iter().position(|&taken| !taken) {
            None => Ok(()),
            Some(at) => {
                let key = self.values[at].0;
                Err(error(Code::OverrideValue, Span::default(), |m| {
                    m.write_str("the compile gives a value for ")?;
                    write_key(m, key)?;
                    m.write_str(", which no override is")
                }))
            }
        }
    }
}

fn write_key(m: &mut crate::diag::Message, key: OverrideKey<'_>) -> core::fmt::Result {
    match key {
        OverrideKey::Id(n) => write!(m, "@id({n})"),
        OverrideKey::Name(name) => m.quote(name),
    }
}

impl<'a> Checker<'a> {
    /// The expression information of a name resolving to module-scope
    /// declaration `decl`.
    pub(crate) fn decl_info(&mut self, decl: u32) -> Outcome<ExprInfo> {
        let info = self
            .decls
            .get(decl as usize)
            .copied()
            .ok_or(Stop::Internal)?;
        Ok(match info {
            DeclInfo::Type(ty) => Self::type_info(ty),
            DeclInfo::Constant { ty, value } => ExprInfo {
                ty,
                converted: ty,
                stage: Stage::Const,
                value: value.0,
                denotes: Denotes::Constant(decl),
            },
            DeclInfo::Override(o) => ExprInfo {
                ty: o.ty,
                converted: o.ty,
                stage: Stage::Override,
                value: o.value.map_or(NONE, |v| v.0),
                denotes: Denotes::Override(decl),
            },
            DeclInfo::Global(global) => {
                let ty = self.intern(Ty::Reference {
                    space: global.space,
                    store: global.store,
                    access: global.access,
                })?;
                ExprInfo {
                    ty,
                    converted: ty,
                    stage: Stage::Runtime,
                    value: NONE,
                    denotes: Denotes::Global(decl),
                }
            }
            DeclInfo::Function(_) => super::expr::enumerant(Denotes::Function(decl)),
            // Headers resolve in dependency order and bodies after them, so
            // no name meets a declaration not yet resolved.
            DeclInfo::Pending | DeclInfo::Assert => return Err(Stop::Internal),
        })
    }

    /// The value of a constant or override declaration an expression at
    /// `span` names.
    pub(crate) fn decl_value(&self, decl: u32, span: Span) -> Outcome<ValueId> {
        match self.decls.get(decl as usize) {
            Some(&DeclInfo::Constant { value, .. }) => Ok(value),
            Some(DeclInfo::Override(o)) => o.value.ok_or_else(|| {
                error(Code::OverrideValue, span, |m| {
                    m.quote(self.spell(span))?;
                    m.write_str(" has no initializer and the compile gives it no value")
                })
            }),
            _ => Err(Stop::Internal),
        }
    }

    /// Resolve module-scope declaration `index`'s header.
    pub(crate) fn declaration(&mut self, index: u32) -> Outcome<()> {
        let decl = *self.tree.decls.get(index as usize).ok_or(Stop::Internal)?;
        let info = match decl.kind {
            DeclKind::Struct { name, members } => {
                self.refuse_attrs(decl.attrs)?;
                DeclInfo::Type(self.structure(index, name, members)?)
            }
            DeclKind::Alias { ty, .. } => {
                self.refuse_attrs(decl.attrs)?;
                DeclInfo::Type(self.type_expr(ty)?)
            }
            DeclKind::Const(value) => {
                self.refuse_attrs(decl.attrs)?;
                let (ty, value) = self.constant_value(value)?;
                DeclInfo::Constant { ty, value }
            }
            DeclKind::Override(value) => DeclInfo::Override(self.override_decl(&decl, value)?),
            DeclKind::Var(var) => DeclInfo::Global(self.global(&decl, var)?),
            DeclKind::Fn(function) => DeclInfo::Function(self.signature(&decl, function)?),
            DeclKind::ConstAssert(expr) => {
                self.refuse_attrs(decl.attrs)?;
                self.assertion(expr)?;
                DeclInfo::Assert
            }
        };
        *self.decls.get_mut(index as usize).ok_or(Stop::Internal)? = info;
        Ok(())
    }

    /// Refuse any attribute on a declaration that takes none.
    fn refuse_attrs(&self, run: Run) -> Outcome<()> {
        match self.tree.attr_list(run).first() {
            None => Ok(()),
            Some(attr) => Err(error(Code::Attribute, attr.span, |m| {
                m.write_str("no attribute applies to this declaration")
            })),
        }
    }

    /// The type a type expression denotes.
    pub(crate) fn type_expr(&mut self, id: ExprId) -> Outcome<TyId> {
        let info = self.expression(id)?;
        if info.denotes != Denotes::Type {
            let span = self.span_of(id);
            return Err(error(Code::Misused, span, |m| {
                m.quote(self.spell(span))?;
                m.write_str(" is not a type")
            }));
        }
        Ok(info.ty)
    }

    /// `const_assert e`: a const-expression evaluating to `true`.
    pub(crate) fn assertion(&mut self, expr: ExprId) -> Outcome<()> {
        let info = self.expression(expr)?;
        let boolean = self.types.scalar(Scalar::Bool)?;
        self.convert_to(expr, boolean)?;
        if info.stage != Stage::Const {
            return Err(error(Code::Assertion, self.span_of(expr), |m| {
                m.write_str("a const_assert's expression is a const-expression")
            }));
        }
        self.force(expr)?;
        let value = self.value_or_stop(expr)?;
        if self.values.boolean(value) != Some(true) {
            return Err(error(Code::Assertion, self.span_of(expr), |m| {
                m.write_str("the assertion does not hold")
            }));
        }
        Ok(())
    }

    /// A constant's type and value: its initializer, a const-expression,
    /// converted to its declared type where it has one.
    pub(crate) fn constant_value(&mut self, value: ValueDecl) -> Outcome<(TyId, ValueId)> {
        let declared = value.ty.map(|ty| self.type_expr(ty)).transpose()?;
        let info = self.expression(value.init)?;
        if info.stage != Stage::Const {
            return Err(error(Code::Evaluation, self.span_of(value.init), |m| {
                m.write_str("a const's initializer is a const-expression")
            }));
        }
        let ty = match declared {
            Some(ty) => {
                self.convert_to(value.init, ty)?;
                ty
            }
            None => self.value_of(value.init)?,
        };
        if !self.types.is_constructible(ty) {
            return Err(error(Code::Type, value.name, |m| {
                m.write_str("a const's type is constructible")
            }));
        }
        self.force(value.init)?;
        Ok((ty, self.value_or_stop(value.init)?))
    }

    /// An override: a concrete scalar, its `@id` unique, its initializer an
    /// override-expression, and the compile's value for it taken — though
    /// neither that nor its initializer is evaluated until an entry point
    /// needs it.
    fn override_decl(&mut self, decl: &Decl, value: OverrideDecl) -> Outcome<Override> {
        let mut id = None;
        for attr in self.attrs_at(decl.attrs, Place::Override)? {
            let n = self.attr_integer(&attr, 0, i128::from(u16::MAX))?;
            let n = u16::try_from(n).map_err(|_| Stop::Internal)?;
            if !self
                .override_ids
                .try_insert(n)
                .map_err(|_| Stop::OutOfMemory)?
            {
                return Err(error(Code::Attribute, attr.span, |m| {
                    write!(m, "two overrides take the @id {n}")
                }));
            }
            id = Some(n);
        }
        let declared = value.ty.map(|ty| self.type_expr(ty)).transpose()?;
        let init_ty = match value.init {
            Some(init) => {
                if self.expression(init)?.stage == Stage::Runtime {
                    return Err(error(Code::Evaluation, self.span_of(init), |m| {
                        m.write_str("an override's initializer is an override-expression")
                    }));
                }
                Some(self.value_of(init)?)
            }
            None => None,
        };
        let ty = match (declared, init_ty) {
            (Some(ty), _) => ty,
            (None, Some(ty)) => self.types.concrete(ty)?,
            (None, None) => {
                return Err(error(Code::Type, value.name, |m| {
                    m.write_str("an override needs a type or an initializer")
                }))
            }
        };
        let Ty::Scalar(scalar) = self.types.get(ty) else {
            return Err(error(Code::Type, value.name, |m| {
                m.write_str("an override is a concrete scalar")
            }));
        };
        if let Some(init) = value.init {
            self.convert_to(init, ty)?;
        }
        let key = match id {
            Some(n) => OverrideKey::Id(n),
            None => OverrideKey::Name(self.spell(value.name)),
        };
        let given = match self.given.take(key) {
            Some(number) => Some(self.values.add(given_value(number, scalar, value.name)?)?),
            None => None,
        };
        Ok(Override {
            ty,
            init: value.init,
            given,
            value: None,
        })
    }

    /// A structure: its members' types plain, concrete and of fixed size, a
    /// runtime-sized array only last, nested no deeper than the bound, and
    /// each member's attributes.
    fn structure(&mut self, index: u32, name: Span, members: Run) -> Outcome<TyId> {
        let fields = self.tree.field_list(members);
        let mut built: Vec<Member> = Vec::new();
        let mut attrs: Vec<MemberAttrs> = Vec::new();
        reserve(&mut built, fields.len())?;
        reserve(&mut attrs, fields.len())?;
        for (at, field) in fields.iter().enumerate() {
            let ty = self.type_expr(field.ty)?;
            let last = at + 1 == fields.len();
            let plain = matches!(
                self.types.get(ty),
                Ty::Scalar(_)
                    | Ty::Vector(..)
                    | Ty::Matrix { .. }
                    | Ty::Atomic(_)
                    | Ty::Array { .. }
                    | Ty::Struct(_)
            );
            let runtime = matches!(
                self.types.get(ty),
                Ty::Array {
                    count: Count::Runtime,
                    ..
                }
            );
            let sized = self.types.has_creation_fixed_footprint(ty) || (runtime && last);
            if !plain || !sized || self.types.is_abstract(ty) {
                return Err(error(Code::Type, field.span, |m| {
                    m.write_str(
                        "a member is a plain type of fixed size, a runtime-sized array only last",
                    )
                }));
            }
            built.push(Member {
                name: field.name,
                ty,
            });
            attrs.push(self.member_attrs(field, ty)?);
        }
        let ty = self
            .types
            .structure(Origin::Declared(index), name, &built)?;
        if self.types.depth(ty) > super::ty::MAX_TYPE_NESTING {
            return Err(error(Code::NestingTooDeep, name, |m| {
                write!(
                    m,
                    "composite types nest more than {} deep",
                    super::ty::MAX_TYPE_NESTING
                )
            }));
        }
        let Ty::Struct(structure) = self.types.get(ty) else {
            return Err(Stop::Internal);
        };
        self.record_members(structure, fields, &attrs)?;
        self.struct_layout(structure)?;
        self.member_alignments(ty, structure, fields, &attrs)?;
        Self::member_locations(fields, &built, &attrs)?;
        Ok(ty)
    }

    /// Refuse an `@align` that is no multiple of its member type's own
    /// alignment, where the structure may be a variable's store type in a
    /// space laid out as storage is.
    fn member_alignments(
        &mut self,
        ty: TyId,
        structure: u32,
        fields: &[Field],
        attrs: &[MemberAttrs],
    ) -> Outcome<()> {
        let held = self.types.is_constructible(ty)
            || self.types.has_fixed_footprint(ty)
            || self.types.is_host_shareable(ty);
        if !held {
            return Ok(());
        }
        for (index, (field, attr)) in fields.iter().zip(attrs).enumerate() {
            let Some(align) = attr.align else {
                continue;
            };
            let member = self
                .types
                .members_of(structure)
                .get(index)
                .map(|m| m.ty)
                .ok_or(Stop::Internal)?;
            let natural = self.layout(member)?.align;
            if align % natural != 0 {
                return Err(error(Code::Layout, field.span, |m| {
                    write!(
                        m,
                        "@align({align}) is no multiple of its type's alignment, {natural}"
                    )
                }));
            }
        }
        Ok(())
    }

    /// Refuse two members of one structure at one location, and blend
    /// sources but as the one pair at location 0 of one type.
    fn member_locations(
        fields: &[Field],
        members: &[Member],
        attrs: &[MemberAttrs],
    ) -> Outcome<()> {
        let mut located: Vec<(u32, Option<u32>, TyId, Span)> = Vec::new();
        for ((field, member), attr) in fields.iter().zip(members).zip(attrs) {
            if let Io::Location {
                location,
                blend_src,
                ..
            } = attr.io
            {
                push(&mut located, (location, blend_src, member.ty, field.span))?;
            }
        }
        if located.iter().any(|l| l.1.is_some()) {
            let pair = matches!(
                &located[..],
                [(0, Some(a), ta, _), (0, Some(b), tb, _)] if ta == tb && (*a, *b) != (0, 0) && a != b
            );
            if !pair {
                let span = located.first().map_or_else(Span::default, |l| l.3);
                return Err(error(Code::Interface, span, |m| {
                    m.write_str("blend sources are two members of one type, both @location(0), @blend_src(0) and @blend_src(1)")
                }));
            }
            return Ok(());
        }
        located.sort_unstable_by_key(|l| l.0);
        if let Some(pair) = located.windows(2).find(|p| p[0].0 == p[1].0) {
            return Err(error(Code::Interface, pair[1].3, |m| {
                write!(m, "two members are at @location({})", pair[1].0)
            }));
        }
        Ok(())
    }

    /// Record a declared structure's members: their attributes, and each
    /// name's symbol beside its index, sorted, which refuses a name given
    /// twice and is what a member access searches.
    fn record_members(
        &mut self,
        structure: u32,
        fields: &[Field],
        attrs: &[MemberAttrs],
    ) -> Outcome<()> {
        let start = u32::try_from(self.member_attrs.len()).map_err(|_| Stop::OutOfMemory)?;
        *slot(&mut self.struct_attrs, structure as usize, NONE)? = start;
        reserve(&mut self.member_attrs, attrs.len())?;
        self.member_attrs.extend_from_slice(attrs);
        let first = self.member_table.len();
        reserve(&mut self.member_table, fields.len())?;
        for (index, field) in fields.iter().enumerate() {
            let symbol = self.symbols.of(field.name).ok_or(Stop::Internal)?;
            let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            self.member_table.push((structure, symbol.0, index));
        }
        let table = &mut self.member_table[first..];
        table.sort_unstable();
        if let Some(pair) = table.windows(2).find(|pair| pair[0].1 == pair[1].1) {
            let repeated = fields
                .get(pair[1].2 as usize)
                .map_or_else(Span::default, |f| f.name);
            return Err(error(Code::Redeclared, repeated, |m| {
                m.write_str("two members are named ")?;
                m.quote(self.spell(repeated))
            }));
        }
        Ok(())
    }

    /// The attributes of a declared structure's member `index`.
    pub(crate) fn member_attrs_of(&self, structure: u32, index: u32) -> Option<MemberAttrs> {
        let start = *self.struct_attrs.get(structure as usize)?;
        if start == NONE {
            return None;
        }
        self.member_attrs.get((start + index) as usize).copied()
    }

    /// The attributes of `run` at `place`: each a known name admitted there
    /// and given once, diagnostic filters aside, which a function takes.
    fn attrs_at(&self, run: Run, place: Place) -> Outcome<Vec<Attr>> {
        let mut kept: Vec<Attr> = Vec::new();
        for &attr in self.tree.attr_list(run) {
            let name = self.spell(attr.name);
            if ATTRIBUTES.binary_search(&name).is_err() {
                return Err(error(Code::Attribute, attr.name, |m| {
                    m.quote(name)?;
                    m.write_str(" is not an attribute")
                }));
            }
            match name {
                "subgroup_size" => {
                    return Err(error(Code::UnsupportedExtension, attr.span, |m| {
                        m.write_str(
                            "@subgroup_size needs `subgroup_size_control`, which is not offered",
                        )
                    }))
                }
                "const" => {
                    return Err(error(Code::Attribute, attr.span, |m| {
                        m.write_str("@const marks built-in functions alone")
                    }))
                }
                _ => {}
            }
            if !place.admits(name) {
                return Err(error(Code::Attribute, attr.span, |m| {
                    write!(m, "@{name} does not apply here")
                }));
            }
            if name == "diagnostic" {
                continue;
            }
            if kept.iter().any(|k| self.spell(k.name) == name) {
                return Err(error(Code::Attribute, attr.span, |m| {
                    write!(m, "@{name} is given twice")
                }));
            }
            push(&mut kept, attr)?;
        }
        Ok(kept)
    }

    /// An attribute's arguments, exactly `count` of them.
    fn attr_args(&self, attr: &Attr, count: usize) -> Outcome<&'a [ExprId]> {
        let AttrArgs::Exprs(run) = attr.args else {
            return Err(Stop::Internal);
        };
        let tree: &'a crate::syntax::Tree = self.tree;
        let args = tree.expr_list(run);
        if args.len() != count {
            return Err(error(Code::Attribute, attr.span, |m| {
                write!(m, "@{} takes {count} arguments", self.spell(attr.name))
            }));
        }
        Ok(args)
    }

    /// An attribute's one argument: a const-expression of type `i32` or
    /// `u32` from `least` to `most`.
    pub(crate) fn attr_integer(&mut self, attr: &Attr, least: i128, most: i128) -> Outcome<u32> {
        let arg = self.attr_args(attr, 1)?[0];
        self.expression(arg)?;
        let ty = self.integer_type(arg)?;
        self.convert_to(arg, ty)?;
        if self.info(arg).stage != Stage::Const {
            return Err(error(Code::Attribute, self.span_of(arg), |m| {
                m.write_str("this argument is a const-expression")
            }));
        }
        self.force(arg)?;
        let n = self.integer_value(arg)?;
        Self::in_range(n, (least, most), self.span_of(arg))
    }

    /// The type an attribute's integer argument takes: its own where it is
    /// `i32` or `u32`, `i32` where it is abstract.
    fn integer_type(&mut self, arg: ExprId) -> Outcome<TyId> {
        let ty = self.value_of(arg)?;
        match self.types.get(ty) {
            Ty::Scalar(Scalar::AbstractInt) => self.types.scalar(Scalar::I32),
            Ty::Scalar(Scalar::I32 | Scalar::U32) => Ok(ty),
            _ => Err(error(Code::Attribute, self.span_of(arg), |m| {
                m.write_str("this argument is an i32 or a u32")
            })),
        }
    }

    /// The integer value of the evaluated `arg`.
    pub(crate) fn integer_value(&self, arg: ExprId) -> Outcome<i128> {
        let value = self.value_or_stop(arg)?;
        self.values.integer(value).ok_or(Stop::Internal)
    }

    /// `n` refused outside `least` to `most`, at `span`.
    pub(crate) fn in_range(n: i128, (least, most): (i128, i128), span: Span) -> Outcome<u32> {
        if n < least || n > most {
            return Err(error(Code::Attribute, span, |m| {
                write!(m, "{n} is outside {least} to {most}")
            }));
        }
        u32::try_from(n).map_err(|_| Stop::Internal)
    }

    /// A member's layout and stage-interface attributes.
    fn member_attrs(&mut self, field: &Field, ty: TyId) -> Outcome<MemberAttrs> {
        let attrs = self.attrs_at(field.attrs, Place::Member)?;
        let mut align = None;
        let mut size = None;
        let mut io_attrs: Vec<Attr> = Vec::new();
        for attr in attrs {
            match self.spell(attr.name) {
                "align" => {
                    let n = self.attr_integer(&attr, 1, i128::from(u32::MAX))?;
                    if !n.is_power_of_two() {
                        return Err(error(Code::Attribute, attr.span, |m| {
                            write!(m, "@align({n}) is not a power of two")
                        }));
                    }
                    align = Some(n);
                }
                "size" => {
                    if !self.types.has_creation_fixed_footprint(ty) {
                        return Err(error(Code::Attribute, attr.span, |m| {
                            m.write_str("@size applies only to a member of fixed size")
                        }));
                    }
                    size = Some(self.attr_integer(&attr, 1, i128::from(u32::MAX))?);
                }
                _ => push(&mut io_attrs, attr)?,
            }
        }
        let io = self.io(&io_attrs, ty, field.span)?;
        Ok(MemberAttrs { align, size, io })
    }

    /// How a parameter, result or member written with `attrs`, of type
    /// `ty`, meets a stage.
    fn io(&mut self, attrs: &[Attr], ty: TyId, span: Span) -> Outcome<Io> {
        let named = |name: &str| attrs.iter().find(|a| self.spell(a.name) == name).copied();
        let builtin = named("builtin");
        let location = named("location");
        let interpolate = named("interpolate");
        let invariant = named("invariant");
        let blend_src = named("blend_src");
        if let Some(attr) = builtin {
            if location.is_some() || interpolate.is_some() || blend_src.is_some() {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("a built-in takes no location, interpolation or blend source")
                }));
            }
            let (value, depth) = self.builtin_value(&attr)?;
            if invariant.is_some() && value != BuiltinValue::Position {
                return Err(error(Code::Attribute, span, |m| {
                    m.write_str("@invariant applies to the position built-in alone")
                }));
            }
            return Ok(Io::Builtin {
                value,
                invariant: invariant.is_some(),
                depth,
            });
        }
        if let Some(attr) = invariant {
            return Err(error(Code::Attribute, attr.span, |m| {
                m.write_str("@invariant applies to the position built-in alone")
            }));
        }
        let Some(location_attr) = location else {
            if let Some(attr) = interpolate.or(blend_src) {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("this attribute applies only beside @location")
                }));
            }
            return Ok(Io::None);
        };
        let location = self.attr_integer(&location_attr, 0, i128::from(u32::MAX))?;
        let numeric = match self.types.get(ty) {
            Ty::Scalar(s) | Ty::Vector(_, s) => s.is_numeric() && !s.is_abstract(),
            _ => false,
        };
        if !numeric {
            return Err(error(Code::Attribute, span, |m| {
                m.write_str("a value at a location is a numeric scalar or vector")
            }));
        }
        let blend_src = match blend_src {
            Some(attr) => {
                if !self.features.dual_source_blending {
                    return Err(error(Code::Attribute, attr.span, |m| {
                        m.write_str("@blend_src needs `enable dual_source_blending;`")
                    }));
                }
                Some(self.attr_integer(&attr, 0, 1)?)
            }
            None => None,
        };
        let interpolation = interpolate
            .map(|attr| self.interpolation(&attr))
            .transpose()?;
        if let Some(i) = interpolation {
            let integer = self.types.scalar_of(ty).is_some_and(Scalar::is_integer);
            if integer && i.kind != InterpolationKind::Flat {
                return Err(error(Code::Attribute, span, |m| {
                    m.write_str("an integer value is interpolated flat")
                }));
            }
        }
        Ok(Io::Location {
            location,
            interpolation,
            blend_src,
        })
    }

    /// The names an attribute takes in place of expressions, `@builtin`'s
    /// and `@interpolate`'s, and how many it was given.
    fn attr_names(
        &self,
        attr: &Attr,
        (least, most): (usize, usize),
    ) -> Outcome<([&'a str; 2], usize)> {
        let AttrArgs::Exprs(run) = attr.args else {
            return Err(Stop::Internal);
        };
        let args = self.tree.expr_list(run);
        if args.len() < least || args.len() > most {
            return Err(error(Code::Attribute, attr.span, |m| {
                write!(
                    m,
                    "@{} takes {least} to {most} names",
                    self.spell(attr.name)
                )
            }));
        }
        let mut names = [""; 2];
        for (slot, &arg) in names.iter_mut().zip(args) {
            let bare = matches!(
                self.tree.expr(arg).map(|e| e.kind),
                Some(ExprKind::Ident(ident)) if ident.template.is_empty()
            );
            if !bare {
                return Err(error(Code::Attribute, self.span_of(arg), |m| {
                    m.write_str("a name is needed here")
                }));
            }
            *slot = self.spell(self.span_of(arg));
        }
        Ok((names, args.len()))
    }

    /// `@builtin(name)`, with `frag_depth`'s depth mode where given.
    fn builtin_value(&self, attr: &Attr) -> Outcome<(BuiltinValue, Option<Depth>)> {
        let ([spelled, mode], count) = self.attr_names(attr, (1, 2))?;
        if let Some((_, extension)) = REFUSED_BUILTIN_VALUES.iter().find(|(n, _)| *n == spelled) {
            return Err(error(Code::UnsupportedExtension, attr.span, |m| {
                m.quote(spelled)?;
                write!(m, " needs `{extension}`, which is not offered")
            }));
        }
        let value = BUILTIN_VALUES
            .binary_search_by(|(n, _)| n.cmp(&spelled))
            .map(|at| BUILTIN_VALUES[at].1)
            .map_err(|_| {
                error(Code::Attribute, attr.span, |m| {
                    m.quote(spelled)?;
                    m.write_str(" is not a built-in value")
                })
            })?;
        let depth = match (count, value) {
            (1, _) => None,
            (_, BuiltinValue::FragDepth) => Some(match mode {
                "less" => Depth::Less,
                "greater" => Depth::Greater,
                _ => {
                    return Err(error(Code::Attribute, attr.span, |m| {
                        m.write_str("a depth mode is `less` or `greater`")
                    }))
                }
            }),
            _ => {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("only frag_depth takes a depth mode")
                }))
            }
        };
        Ok((value, depth))
    }

    /// `@interpolate(type[, sampling])`, each type's sampling defaulted.
    fn interpolation(&self, attr: &Attr) -> Outcome<Interpolation> {
        let ([kind, sampling], count) = self.attr_names(attr, (1, 2))?;
        let refuse = |what: &str| Err(error(Code::Attribute, attr.span, |m| m.write_str(what)));
        let kind = match kind {
            "perspective" => InterpolationKind::Perspective,
            "linear" => InterpolationKind::Linear,
            "flat" => InterpolationKind::Flat,
            _ => return refuse("an interpolation type is perspective, linear or flat"),
        };
        let flat = kind == InterpolationKind::Flat;
        let sampling = match (count, sampling) {
            (1, _) if flat => Sampling::First,
            (1, _) => Sampling::Center,
            (_, "first") if flat => Sampling::First,
            (_, "either") if flat => Sampling::Either,
            (_, "center") if !flat => Sampling::Center,
            (_, "centroid") if !flat => Sampling::Centroid,
            (_, "sample") if !flat => Sampling::Sample,
            _ => return refuse("this sampling does not go with this interpolation type"),
        };
        Ok(Interpolation { kind, sampling })
    }

    /// A module-scope variable: its address space and access mode, its
    /// store type as the space holds it, a resource's group and binding, a
    /// private variable's initializer.
    fn global(&mut self, decl: &Decl, var: VarDecl) -> Outcome<Global> {
        let tree: &'a crate::syntax::Tree = self.tree;
        let template = tree.expr_list(var.template);
        for &arg in template {
            self.expression(arg)?;
        }
        if template.len() > 2 {
            return Err(error(Code::AddressSpace, decl.span, |m| {
                m.write_str("a variable takes an address space and an access mode")
            }));
        }
        let space = match template.first() {
            Some(&arg) => match self.info(arg).denotes {
                Denotes::Space(Space::Function) => {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        m.write_str("a function variable is declared in a function")
                    }))
                }
                Denotes::Space(space) => space,
                _ => {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        m.write_str("an address space is needed here")
                    }))
                }
            },
            None => Space::Handle,
        };
        let access = match template.get(1) {
            Some(&arg) => {
                let Denotes::Access(access) = self.info(arg).denotes else {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        m.write_str("an access mode is needed here")
                    }));
                };
                if space != Space::Storage || access == Access::Write {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        m.write_str("only storage names its access mode, read or read_write")
                    }));
                }
                access
            }
            None => space.default_access(),
        };
        let declared = var.ty.map(|ty| self.type_expr(ty)).transpose()?;
        let init_stage = match var.init {
            Some(init) => Some(self.expression(init)?.stage),
            None => None,
        };
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
        let handle = matches!(self.types.get(store), Ty::Texture(_) | Ty::Sampler { .. });
        if handle != (space == Space::Handle) {
            return Err(error(Code::AddressSpace, var.name, |m| {
                if handle {
                    m.write_str("a texture or sampler variable names no address space")
                } else {
                    m.write_str("a module-scope variable names its address space")
                }
            }));
        }
        self.store_in(store, space, access, var.name)?;
        if let (Some(init), Some(stage)) = (var.init, init_stage) {
            if space != Space::Private {
                return Err(error(Code::AddressSpace, self.span_of(init), |m| {
                    write!(m, "a {} variable takes no initializer", space.name())
                }));
            }
            if stage == Stage::Runtime {
                return Err(error(Code::Evaluation, self.span_of(init), |m| {
                    m.write_str("a module-scope initializer is a const- or override-expression")
                }));
            }
        }
        let binding = self.binding(decl, space)?;
        Ok(Global {
            space,
            access,
            store,
            binding,
            init: var.init,
        })
    }

    /// A variable's `@group` and `@binding`: both on a resource, neither on
    /// anything else.
    fn binding(&mut self, decl: &Decl, space: Space) -> Outcome<Option<(u32, u32)>> {
        let mut group = None;
        let mut binding = None;
        for attr in self.attrs_at(decl.attrs, Place::Variable)? {
            let n = self.attr_integer(&attr, 0, i128::from(u32::MAX))?;
            if self.spell(attr.name) == "group" {
                group = Some(n);
            } else {
                binding = Some(n);
            }
        }
        let resource = matches!(space, Space::Uniform | Space::Storage | Space::Handle);
        match (group, binding) {
            (Some(g), Some(b)) if resource => Ok(Some((g, b))),
            (None, None) if !resource => Ok(None),
            _ => Err(error(Code::Attribute, decl.span, |m| {
                if resource {
                    m.write_str("a resource takes both @group and @binding")
                } else {
                    m.write_str("only a resource takes @group and @binding")
                }
            })),
        }
    }

    /// Refuse a store type its address space does not hold, at `span`.
    pub(crate) fn store_in(
        &mut self,
        store: TyId,
        space: Space,
        access: Access,
        span: Span,
    ) -> Outcome<()> {
        let types = &self.types;
        let refuse = |what: &str| {
            Err(error(Code::AddressSpace, span, |m| {
                write!(m, "a {} variable holds {what}", space.name())
            }))
        };
        match space {
            Space::Function | Space::Private if !types.is_constructible(store) => {
                return refuse("a constructible type")
            }
            Space::Workgroup if !types.has_fixed_footprint(store) => {
                return refuse("a plain type of fixed size")
            }
            Space::Uniform if !types.is_constructible(store) || !types.is_host_shareable(store) => {
                return refuse("a constructible, host-shareable type")
            }
            Space::Storage if !types.is_host_shareable(store) => {
                return refuse("a host-shareable type")
            }
            Space::Storage if access == Access::Read && types.holds(store, Holds::ATOMIC) => {
                return refuse("an atomic only with read_write access")
            }
            Space::Immediate
                if !types.is_constructible(store)
                    || !types.is_host_shareable(store)
                    || types.holds(store, Holds::ARRAY) =>
            {
                return refuse("a constructible, host-shareable type holding no array")
            }
            _ => {}
        }
        self.held_to_space(store, space, span)?;
        self.laid_out(store, space, span)
    }

    /// A function's signature.
    fn signature(&mut self, decl: &Decl, function: FnDecl) -> Outcome<Signature> {
        let mut stage = None;
        let mut must_use = false;
        let mut workgroup_attr = None;
        for attr in self.attrs_at(decl.attrs, Place::Function)? {
            let name = self.spell(attr.name);
            let this = match name {
                "vertex" => Some(EntryStage::Vertex),
                "fragment" => Some(EntryStage::Fragment),
                "compute" => Some(EntryStage::Compute),
                _ => None,
            };
            if let Some(this) = this {
                self.attr_args(&attr, 0)?;
                if stage.is_some() {
                    return Err(error(Code::Attribute, attr.span, |m| {
                        m.write_str("a function is the entry point of at most one stage")
                    }));
                }
                stage = Some(this);
            } else if name == "must_use" {
                self.attr_args(&attr, 0)?;
                must_use = true;
            } else if name == "workgroup_size" {
                workgroup_attr = Some(attr);
            }
        }
        let workgroup = match (stage, workgroup_attr) {
            (Some(EntryStage::Compute), Some(attr)) => Some(self.workgroup_size(&attr)?),
            (Some(EntryStage::Compute), None) => {
                return Err(error(Code::Attribute, function.name, |m| {
                    m.write_str("a compute entry point declares its @workgroup_size")
                }))
            }
            (_, Some(attr)) => {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("@workgroup_size applies to a compute entry point alone")
                }))
            }
            (_, None) => None,
        };
        let params = self.parameters(function, stage)?;
        let (result, result_io) = match function.result {
            Some((attrs, ty_expr)) => {
                let ty = self.type_expr(ty_expr)?;
                let span = self.span_of(ty_expr);
                if !self.types.is_constructible(ty) || self.types.is_abstract(ty) {
                    return Err(error(Code::Type, span, |m| {
                        m.write_str("a function returns a constructible type")
                    }));
                }
                let io = self.stage_io(attrs, ty, span, stage, Place::Result)?;
                (Some(ty), io)
            }
            None => (None, Io::None),
        };
        if must_use && result.is_none() {
            return Err(error(Code::Attribute, function.name, |m| {
                m.write_str("@must_use applies to a function that returns a value")
            }));
        }
        Ok(Signature {
            params,
            result,
            result_io,
            stage,
            must_use,
            workgroup,
        })
    }

    /// A function's formal parameters, recorded as a run of
    /// [`Checker::params`].
    fn parameters(&mut self, function: FnDecl, stage: Option<EntryStage>) -> Outcome<(u32, u32)> {
        let start = u32::try_from(self.params.len()).map_err(|_| Stop::OutOfMemory)?;
        let tree: &'a crate::syntax::Tree = self.tree;
        for field in tree.field_list(function.params) {
            let ty = self.type_expr(field.ty)?;
            let pointer_space = match self.types.get(ty) {
                Ty::Pointer { space, .. } => Some(space),
                _ => None,
            };
            let allowed = self.types.is_constructible(ty)
                || matches!(self.types.get(ty), Ty::Texture(_) | Ty::Sampler { .. })
                || pointer_space.is_some_and(|space| {
                    matches!(
                        space,
                        Space::Function
                            | Space::Private
                            | Space::Storage
                            | Space::Uniform
                            | Space::Workgroup
                    )
                });
            if !allowed || self.types.is_abstract(ty) {
                return Err(error(Code::Type, field.span, |m| {
                    m.write_str(
                        "a parameter is constructible, a texture, a sampler or a pointer to memory",
                    )
                }));
            }
            let io = self.stage_io(field.attrs, ty, field.span, stage, Place::Param)?;
            push(
                &mut self.params,
                ParamInfo {
                    ty,
                    io,
                    span: field.span,
                },
            )?;
        }
        let end = u32::try_from(self.params.len()).map_err(|_| Stop::OutOfMemory)?;
        Ok((start, end))
    }

    /// The stage IO of a parameter or result; a function that is no entry
    /// point takes none.
    fn stage_io(
        &mut self,
        run: Run,
        ty: TyId,
        span: Span,
        stage: Option<EntryStage>,
        place: Place,
    ) -> Outcome<Io> {
        let attrs = self.attrs_at(run, place)?;
        match (attrs.first(), stage) {
            (None, _) => Ok(Io::None),
            (Some(attr), None) => Err(error(Code::Attribute, attr.span, |m| {
                m.write_str("only an entry point's parameters and result meet a stage")
            })),
            (Some(_), Some(_)) => self.io(&attrs, ty, span),
        }
    }

    /// `@workgroup_size(x[, y[, z]])`: const- or override-expressions all
    /// of one integer type, an abstract one taking the others', and
    /// positive — a constant checked now, the rest once overrides take
    /// their values.
    fn workgroup_size(&mut self, attr: &Attr) -> Outcome<[Option<ExprId>; 3]> {
        let AttrArgs::Exprs(run) = attr.args else {
            return Err(Stop::Internal);
        };
        let tree: &'a crate::syntax::Tree = self.tree;
        let args = tree.expr_list(run);
        if args.is_empty() || args.len() > 3 {
            return Err(error(Code::Attribute, attr.span, |m| {
                m.write_str("@workgroup_size takes one to three dimensions")
            }));
        }
        let mut common: Option<Scalar> = None;
        for &arg in args {
            if self.expression(arg)?.stage == Stage::Runtime {
                return Err(error(Code::Attribute, self.span_of(arg), |m| {
                    m.write_str("a workgroup dimension is a const- or override-expression")
                }));
            }
            let own = self.value_of(arg)?;
            self.integer_type(arg)?;
            let Some(scalar) = self.types.scalar_of(own).filter(|s| !s.is_abstract()) else {
                continue;
            };
            if common.is_some_and(|seen| seen != scalar) {
                return Err(error(Code::Attribute, attr.span, |m| {
                    m.write_str("a workgroup size's dimensions are all i32 or all u32")
                }));
            }
            common = Some(scalar);
        }
        let ty = self.types.scalar(common.unwrap_or(Scalar::I32))?;
        let mut size = [None; 3];
        for (slot, &arg) in size.iter_mut().zip(args) {
            self.convert_to(arg, ty)?;
            if self.info(arg).stage == Stage::Const {
                self.force(arg)?;
                let n = self.integer_value(arg)?;
                Self::in_range(n, (1, i128::from(u32::MAX)), self.span_of(arg))?;
            }
            *slot = Some(arg);
        }
        Ok(size)
    }
}

/// The compile's `number` for an override of `scalar` named at `name`,
/// converted as the WebGPU API converts a pipeline constant: a bool false
/// for zero or NaN, an integer truncated toward zero and held to its range,
/// a float rounded once to nearest and finite.
fn given_value(number: f64, scalar: Scalar, name: Span) -> Outcome<Value> {
    let refuse = || {
        error(Code::OverrideValue, name, |m| {
            write!(
                m,
                "{number} is not a value of the override's type, {}",
                scalar.name()
            )
        })
    };
    let integral = tairix_util::mathf::trunc(number);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "each cast is of an integral value its guard holds in range"
    )]
    let value = match scalar {
        Scalar::Bool => Value::Bool(number != 0.0 && !number.is_nan()),
        Scalar::I32 if (-2_147_483_648.0..=2_147_483_647.0).contains(&integral) => {
            Value::I32(integral as i32)
        }
        Scalar::U32 if (0.0..=4_294_967_295.0).contains(&integral) => Value::U32(integral as u32),
        Scalar::F32 if tairix_util::mathf::single(number).is_finite() => {
            Value::F32(tairix_util::mathf::single(number))
        }
        Scalar::F16 if number.is_finite() => {
            let bits = crate::eval::half(number);
            if !tairix_util::mathf::from_half(bits).is_finite() {
                return Err(refuse());
            }
            Value::F16(bits)
        }
        _ => return Err(refuse()),
    };
    Ok(value)
}
