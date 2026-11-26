//! The checker: a module's meaning resolved as the specification states it.
//!
//! Module-scope declarations are resolved in dependency order — their
//! headers name only module-scope and predeclared names, so the order is
//! found exactly, and a cycle among them is an error — then every function
//! body, then the call graph, the entry points and the uniformity analysis.
//! Every expression is resolved over the run of nodes its subtree occupies,
//! children first, so an expression of any length recurses into nothing.

mod alias;
mod builtin;
pub(crate) mod call;
pub(crate) mod decl;
mod entry;
pub(crate) mod expr;
mod filter;
mod fold;
mod graph;
mod layout;
mod pipeline;
pub(crate) mod stmt;
mod ty;
mod uniformity;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_collections::OrderedSet;

use crate::diag::{error, push, reserve, Code, Message, Outcome, Severity, Span, Stop};
use crate::eval::{ValueId, Values};
use crate::predeclared::{self, Builtin, Predeclared};
use crate::scope::{Locals, Symbols};
use crate::syntax::{Decl, DeclKind, DirectiveKind, ExprId, ExprKind, Shape, Tree};
use crate::types::{Access, Space, TexelFormat, Ty, TyId, Types};
use crate::Options;

/// No index: no value, no local, no declaration.
pub(crate) const NONE: u32 = u32::MAX;

/// How early an expression's value is known.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Stage {
    /// When the module is made: a const-expression.
    Const,
    /// When its overrides take their values: an override-expression.
    Override,
    /// Only when it runs.
    Runtime,
}

/// What an expression denotes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Denotes {
    /// A value computed from its operands.
    Value,
    /// A type, its `ty`.
    Type,
    Space(Space),
    Access(Access),
    /// A texel format.
    Format(TexelFormat),
    /// A user function, by its declaration.
    Function(u32),
    Builtin(Builtin),
    /// A function-scope declaration's name, by its local.
    Local(u32),
    /// A module-scope variable, constant or override's name, by its
    /// declaration.
    Global(u32),
    Constant(u32),
    Override(u32),
    /// A call of a user function, by its declaration.
    CallFunction(u32),
    CallBuiltin(Builtin),
    /// A call of a type's constructor, and how it builds its value.
    Construct(call::Construct),
    /// A structure member, by its index.
    Member(u32),
    /// A swizzle: its components and how many.
    Swizzle([u8; 4], u8),
}

/// What the checker found of one expression.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ExprInfo {
    /// Its type: a value's, a memory view's, or the type a type expression
    /// denotes.
    pub(crate) ty: TyId,
    /// The type its use takes it as: after the load rule reads a memory
    /// view, and an abstract value is converted to what its use asks.
    pub(crate) converted: TyId,
    pub(crate) stage: Stage,
    /// Its value, converted, where it is known: a [`ValueId`], or
    /// [`NONE`].
    pub(crate) value: u32,
    pub(crate) denotes: Denotes,
}

impl ExprInfo {
    pub(crate) const fn value_id(&self) -> Option<ValueId> {
        if self.value == NONE {
            None
        } else {
            Some(ValueId(self.value))
        }
    }
}

/// A function-scope declaration's kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum LocalKind {
    Let,
    Var,
    Const,
    /// A formal parameter, by its place.
    Param(u32),
}

/// A function-scope declaration: its kind, its type — a variable's store
/// type — for a constant its value, and for a variable or a pointer the
/// root identifier of the memory it reaches.
#[derive(Copy, Clone, Debug)]
pub(crate) struct LocalInfo {
    pub(crate) kind: LocalKind,
    pub(crate) ty: TyId,
    pub(crate) value: u32,
    pub(crate) span: Span,
    pub(crate) root: Option<Root>,
}

/// The variable or pointer parameter a memory view is reached from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Root {
    /// A function-scope variable, by its local.
    Local(u32),
    /// A module-scope variable, by its declaration.
    Global(u32),
    /// A pointer parameter, by its place.
    Param(u32),
}

/// A function's access of memory reached from a module-scope variable or
/// a pointer parameter.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct MemoryAccess {
    pub(crate) function: u32,
    pub(crate) root: Root,
    pub(crate) write: bool,
}

/// The extensions a module enables or requires that TAIRiX offers.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Features {
    pub(crate) f16: bool,
    pub(crate) dual_source_blending: bool,
}

/// A diagnostic the checker raised that does not stop it: a warning or a
/// note.
#[derive(Clone, Debug)]
pub(crate) struct Note {
    pub(crate) severity: Severity,
    pub(crate) code: Code,
    pub(crate) span: Span,
    pub(crate) message: String,
}

/// The enable-extensions TAIRiX offers.
const ENABLES: [&str; 2] = ["dual_source_blending", "f16"];

/// The enable-extensions it refuses, each with why.
const REFUSED_ENABLES: [(&str, &str); 5] = [
    ("atomic_vec2u_min_max", "64-bit atomics are not admitted"),
    (
        "clip_distances",
        "the ClipDistance capability is not admitted",
    ),
    (
        "primitive_index",
        "PrimitiveId needs the Geometry capability, which is not admitted",
    ),
    (
        "subgroup_size_control",
        "the subgroup shuffle and quad operations are not admitted",
    ),
    (
        "subgroups",
        "the subgroup shuffle and quad operations are not admitted",
    ),
];

/// The language extensions TAIRiX offers.
const LANGUAGE: [&str; 10] = [
    "fragment_depth",
    "immediate_address_space",
    "linear_indexing",
    "packed_4x8_integer_dot_product",
    "pointer_composite_access",
    "readonly_and_readwrite_storage_textures",
    "swizzle_assignment",
    "texture_and_sampler_let",
    "texture_formats_tier1",
    "unrestricted_pointer_parameters",
];

/// The checker's state over one module.
pub(crate) struct Checker<'a> {
    pub(crate) text: &'a str,
    pub(crate) tree: &'a Tree,
    pub(crate) shape: Shape,
    pub(crate) symbols: Symbols,
    /// Each symbol's module-scope declaration, or [`NONE`].
    pub(crate) module: Vec<u32>,
    pub(crate) locals: Locals,
    pub(crate) types: Types,
    pub(crate) values: Values,
    pub(crate) exprs: Vec<ExprInfo>,
    pub(crate) decls: Vec<decl::DeclInfo>,
    pub(crate) local_list: Vec<LocalInfo>,
    /// Every function's formal parameters, each function's a run.
    pub(crate) params: Vec<decl::ParamInfo>,
    /// Each declared structure's members as `(structure, symbol, index)`,
    /// sorted.
    pub(crate) member_table: Vec<(u32, u32, u32)>,
    /// Each structure's first entry in `member_attrs`, or [`NONE`] for a
    /// built-in result.
    pub(crate) struct_attrs: Vec<u32>,
    pub(crate) member_attrs: Vec<decl::MemberAttrs>,
    pub(crate) given: decl::Given<'a>,
    pub(crate) override_ids: OrderedSet<u16>,
    /// The function whose body is being checked, or [`NONE`] at module
    /// scope.
    pub(crate) current_fn: u32,
    /// Every call of a user function.
    pub(crate) call_sites: Vec<call::CallSite>,
    /// Every module-scope declaration a function body names, as `(function,
    /// declaration)`.
    pub(crate) references: Vec<(u32, u32)>,
    /// Every use of what only some stages may use.
    pub(crate) stage_uses: Vec<StageUse>,
    /// Every access of memory the alias analysis follows.
    pub(crate) accesses: Vec<MemoryAccess>,
    /// The loops and switches control flows within.
    pub(crate) flow: Vec<stmt::Flow>,
    /// The bytes of function-scope variables the function being checked
    /// declares.
    pub(crate) function_bytes: u64,
    /// What each module-scope declaration's header names, as `(declaration,
    /// named)`, sorted.
    pub(crate) header_pairs: Vec<(u32, u32)>,
    /// The module-scope declarations, each after every one its header
    /// names.
    pub(crate) decl_order: Vec<u32>,
    /// The functions, each after every one it calls.
    pub(crate) fn_order: Vec<u32>,
    pub(crate) entries: Vec<entry::EntryPoint>,
    /// Each statement's behaviour.
    pub(crate) stmt_behaviour: Vec<u8>,
    /// Each loop's body's behaviour, by the loop statement.
    pub(crate) loop_bodies: Vec<u8>,
    /// The local each declaration statement declares, [`NONE`] for others.
    pub(crate) stmt_local: Vec<u32>,
    /// Each function's locals, as `(function, first, end)`.
    pub(crate) fn_locals: Vec<(u32, u32, u32)>,
    pub(crate) filters: Vec<filter::Filter>,
    /// Each structure's layout, once worked out.
    pub(crate) struct_layouts: Vec<Option<layout::StructLayout>>,
    pub(crate) member_offsets: Vec<u32>,
    /// The address-space constraints each structure is known to meet.
    pub(crate) struct_checked: Vec<u8>,
    pub(crate) features: Features,
    pub(crate) notes: Vec<Note>,
}

/// A construct only some stages may use — a built-in's call or a
/// `discard` — its function, the stages as a mask of vertex 1, fragment 2
/// and compute 4, and where it stands.
#[derive(Copy, Clone, Debug)]
pub(crate) struct StageUse {
    pub(crate) function: u32,
    pub(crate) stages: u8,
    pub(crate) span: Span,
}

/// What a name resolves to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Name {
    Local(u32),
    Decl(u32),
    Predeclared(Predeclared),
}

impl<'a> Checker<'a> {
    pub(crate) fn new(
        text: &'a str,
        tree: &'a Tree,
        symbols: Symbols,
        options: &'a Options<'a>,
    ) -> Outcome<Self> {
        let mut module = Vec::new();
        reserve(&mut module, symbols.len())?;
        module.resize(symbols.len(), NONE);
        let locals = Locals::new(symbols.len())?;
        let shape = tree.shape()?;
        let mut exprs = Vec::new();
        reserve(&mut exprs, tree.exprs.len())?;
        let placeholder = ExprInfo {
            ty: TyId(0),
            converted: TyId(0),
            stage: Stage::Runtime,
            value: NONE,
            denotes: Denotes::Value,
        };
        exprs.resize(tree.exprs.len(), placeholder);
        let mut decls = Vec::new();
        reserve(&mut decls, tree.decls.len())?;
        decls.resize(tree.decls.len(), decl::DeclInfo::Pending);
        let mut stmt_behaviour = Vec::new();
        reserve(&mut stmt_behaviour, tree.stmts.len())?;
        stmt_behaviour.resize(tree.stmts.len(), 0);
        let mut loop_bodies = Vec::new();
        reserve(&mut loop_bodies, tree.stmts.len())?;
        loop_bodies.resize(tree.stmts.len(), 0);
        let mut stmt_local = Vec::new();
        reserve(&mut stmt_local, tree.stmts.len())?;
        stmt_local.resize(tree.stmts.len(), NONE);
        Ok(Self {
            text,
            tree,
            shape,
            symbols,
            module,
            locals,
            types: Types::default(),
            values: Values::default(),
            exprs,
            decls,
            local_list: Vec::new(),
            params: Vec::new(),
            member_table: Vec::new(),
            struct_attrs: Vec::new(),
            member_attrs: Vec::new(),
            given: decl::Given::new(options.overrides)?,
            override_ids: OrderedSet::new(),
            current_fn: NONE,
            call_sites: Vec::new(),
            references: Vec::new(),
            stage_uses: Vec::new(),
            accesses: Vec::new(),
            flow: Vec::new(),
            function_bytes: 0,
            header_pairs: Vec::new(),
            decl_order: Vec::new(),
            fn_order: Vec::new(),
            entries: Vec::new(),
            stmt_behaviour,
            loop_bodies,
            stmt_local,
            fn_locals: Vec::new(),
            filters: Vec::new(),
            struct_layouts: Vec::new(),
            member_offsets: Vec::new(),
            struct_checked: Vec::new(),
            features: Features::default(),
            notes: Vec::new(),
        })
    }

    /// The text `span` covers.
    pub(crate) fn spell(&self, span: Span) -> &'a str {
        self.text
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
    }

    /// What the name at `span` resolves to: the nearest function-scope
    /// declaration, else the module's, else the language's.
    pub(crate) fn resolve(&self, span: Span) -> Option<Name> {
        if let Some(symbol) = self.symbols.of(span) {
            if let Some(local) = self.locals.get(symbol) {
                return Some(Name::Local(local));
            }
            if let Some(&decl) = self.module.get(symbol.0 as usize) {
                if decl != NONE {
                    return Some(Name::Decl(decl));
                }
            }
        }
        predeclared::lookup(self.spell(span)).map(Name::Predeclared)
    }

    /// The stop for a name no declaration in scope gives.
    pub(crate) fn undeclared(&self, span: Span) -> Stop {
        error(Code::Undeclared, span, |m| {
            m.quote(self.spell(span))?;
            m.write_str(" is not declared")
        })
    }

    pub(crate) fn info(&self, id: ExprId) -> ExprInfo {
        self.exprs.get(id.0 as usize).copied().unwrap_or(ExprInfo {
            ty: TyId(0),
            converted: TyId(0),
            stage: Stage::Runtime,
            value: NONE,
            denotes: Denotes::Value,
        })
    }

    pub(crate) fn set(&mut self, id: ExprId, info: ExprInfo) {
        if let Some(slot) = self.exprs.get_mut(id.0 as usize) {
            *slot = info;
        }
    }

    pub(crate) fn span_of(&self, id: ExprId) -> Span {
        self.tree.expr(id).map_or_else(Span::default, |e| e.span)
    }

    /// Write `ty` into `m` as WGSL spells it.
    pub(crate) fn write_type(&self, m: &mut Message, ty: TyId) -> core::fmt::Result {
        self.types.write(m, self.text, ty)
    }

    /// The stop for `found` where `expected` was needed, at `span`.
    pub(crate) fn mismatch(&self, span: Span, expected: TyId, found: TyId) -> Stop {
        error(Code::Mismatch, span, |m| {
            m.write_str("expected ")?;
            self.write_type(m, expected)?;
            m.write_str(", found ")?;
            self.write_type(m, found)
        })
    }

    /// Raise a warning or note, unless the bound on them is reached.
    pub(crate) fn note(
        &mut self,
        severity: Severity,
        code: Code,
        span: Span,
        write: impl FnOnce(&mut Message) -> core::fmt::Result,
    ) -> Outcome<()> {
        if self.notes.len() >= crate::MAX_DIAGNOSTICS {
            return Ok(());
        }
        let mut message = Message::default();
        if write(&mut message).is_err() {
            return Err(Stop::OutOfMemory);
        }
        push(
            &mut self.notes,
            Note {
                severity,
                code,
                span,
                message: message.into_text(),
            },
        )
    }

    /// The directives: each enabled extension one TAIRiX offers, each
    /// required language extension one it supports.
    pub(crate) fn directives(&mut self) -> Outcome<()> {
        for directive in &self.tree.directives {
            match directive.kind {
                DirectiveKind::Enable(names) => {
                    for &name in self.tree.name_list(names) {
                        let spelled = self.spell(name);
                        if let Some((_, why)) = REFUSED_ENABLES.iter().find(|(n, _)| *n == spelled)
                        {
                            return Err(error(Code::UnsupportedExtension, name, |m| {
                                m.quote(spelled)?;
                                write!(m, " is not offered: {why}")
                            }));
                        }
                        if ENABLES.binary_search(&spelled).is_err() {
                            return Err(error(Code::UnsupportedExtension, name, |m| {
                                m.quote(spelled)?;
                                m.write_str(" is not an enable-extension")
                            }));
                        }
                        match spelled {
                            "f16" => self.features.f16 = true,
                            _ => self.features.dual_source_blending = true,
                        }
                    }
                }
                DirectiveKind::Requires(names) => {
                    for &name in self.tree.name_list(names) {
                        let spelled = self.spell(name);
                        if LANGUAGE.binary_search(&spelled).is_err() {
                            return Err(error(Code::UnsupportedExtension, name, |m| {
                                m.quote(spelled)?;
                                m.write_str(" is not a language extension TAIRiX supports")
                            }));
                        }
                    }
                }
                DirectiveKind::Diagnostic(_) => {}
            }
        }
        Ok(())
    }

    /// The module-scope names: each declaration's name bound once.
    pub(crate) fn declare_module(&mut self) -> Outcome<()> {
        for (index, decl) in self.tree.decls.iter().enumerate() {
            let Some(name) = declared_name(decl) else {
                continue;
            };
            let Some(symbol) = self.symbols.of(name) else {
                continue;
            };
            let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            let slot = self
                .module
                .get_mut(symbol.0 as usize)
                .ok_or(Stop::Internal)?;
            if *slot != NONE {
                return Err(error(Code::Redeclared, name, |m| {
                    m.quote(
                        self.text
                            .get(name.start as usize..name.end as usize)
                            .unwrap_or(""),
                    )?;
                    m.write_str(" is declared twice at module scope")
                }));
            }
            *slot = index;
        }
        Ok(())
    }

    /// The module-scope declarations in an order that puts each after
    /// every one its header names, refusing a cycle; what each header names
    /// kept for what the entry points reach.
    pub(crate) fn order(&mut self) -> Outcome<Vec<u32>> {
        let tree: &'a Tree = self.tree;
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut roots: Vec<ExprId> = Vec::new();
        for (index, decl) in tree.decls.iter().enumerate() {
            let from = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            roots.clear();
            header_roots(self.text, tree, decl, &mut roots)?;
            for &root in &roots {
                for id in self.shape.subtree(root) {
                    let name = match tree.expr(ExprId(id)).map(|e| e.kind) {
                        Some(ExprKind::Ident(ident) | ExprKind::Call(ident, _)) => ident.name,
                        _ => continue,
                    };
                    if let Some(Name::Decl(target)) = self.resolve(name) {
                        push(&mut pairs, (from, target))?;
                    }
                }
            }
        }
        let graph = graph::Graph::from_edges(tree.decls.len(), &mut pairs)?;
        let order = graph.order()?.map_err(|node| self.cycle(node))?;
        self.header_pairs = pairs;
        Ok(order)
    }

    fn cycle(&self, decl: u32) -> Stop {
        let tree_decl = self.tree.decls.get(decl as usize);
        let span = tree_decl
            .and_then(declared_name)
            .or_else(|| tree_decl.map(|d| d.span))
            .unwrap_or_default();
        error(Code::Cycle, span, |m| {
            m.quote(self.spell(span))?;
            m.write_str(" depends on itself")
        })
    }

    /// A type's index for `ty`, interned.
    pub(crate) fn intern(&mut self, ty: Ty) -> Outcome<TyId> {
        self.types.intern(ty)
    }

    /// Record that the function being checked names module-scope
    /// declaration `decl`, which an entry point reaching it then needs.
    pub(crate) fn note_reference(&mut self, decl: u32) -> Outcome<()> {
        if self.current_fn == NONE {
            return Ok(());
        }
        push(&mut self.references, (self.current_fn, decl))
    }
}

/// Check a module's meaning: its directives, every declaration in
/// dependency order, every function body, the call graph, each entry
/// point's interface, and at last its overrides' values and the
/// override-expressions its entry points reach.
pub(crate) fn check<'a>(
    text: &'a str,
    tree: &'a Tree,
    symbols: Symbols,
    options: &'a Options<'a>,
) -> Outcome<Checker<'a>> {
    let mut checker = Checker::new(text, tree, symbols, options)?;
    checker.directives()?;
    checker.filters()?;
    checker.declare_module()?;
    checker.decl_order = checker.order()?;
    for at in 0..checker.decl_order.len() {
        let decl = checker.decl_order[at];
        checker.declaration(decl)?;
    }
    checker.given.all_taken()?;
    checker.bodies()?;
    checker.call_graph()?;
    checker.entry_points()?;
    checker.alias_analysis()?;
    checker.uniformity()?;
    checker.pipeline()?;
    Ok(checker)
}

impl Checker<'_> {
    /// The functions in an order that puts each after every one it calls,
    /// recursion refused.
    fn call_graph(&mut self) -> Outcome<()> {
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        reserve(&mut pairs, self.call_sites.len())?;
        pairs.extend(
            self.call_sites
                .iter()
                .map(|site| (site.caller, site.callee)),
        );
        let calls = graph::Graph::from_edges(self.tree.decls.len(), &mut pairs)?;
        let order = calls.order()?.map_err(|node| {
            let span = self.decl_name(node);
            error(Code::Cycle, span, |m| {
                m.quote(self.spell(span))?;
                m.write_str(" calls itself")
            })
        })?;
        let mut functions = Vec::new();
        for decl in order {
            if matches!(
                self.decls.get(decl as usize),
                Some(decl::DeclInfo::Function(_))
            ) {
                push(&mut functions, decl)?;
            }
        }
        self.fn_order = functions;
        Ok(())
    }
}

/// The name a module-scope declaration declares, if it declares one.
pub(crate) fn declared_name(decl: &Decl) -> Option<Span> {
    match decl.kind {
        DeclKind::Fn(function) => Some(function.name),
        DeclKind::Var(var) => Some(var.name),
        DeclKind::Override(value) => Some(value.name),
        DeclKind::Const(value) => Some(value.name),
        DeclKind::Alias { name, .. } | DeclKind::Struct { name, .. } => Some(name),
        DeclKind::ConstAssert(_) => None,
    }
}

/// The expressions of a declaration's header — everything but a function's
/// body — whose names its resolution depends on: not `@builtin`'s and
/// `@interpolate`'s, which name what no declaration can.
pub(crate) fn header_roots(
    text: &str,
    tree: &Tree,
    decl: &Decl,
    roots: &mut Vec<ExprId>,
) -> Outcome<()> {
    let attrs = |run, roots: &mut Vec<ExprId>| -> Outcome<()> {
        for attr in tree.attr_list(run) {
            let name = text.get(attr.name.start as usize..attr.name.end as usize);
            if matches!(name, Some("builtin" | "interpolate")) {
                continue;
            }
            if let crate::syntax::AttrArgs::Exprs(args) = attr.args {
                for &arg in tree.expr_list(args) {
                    push(roots, arg)?;
                }
            }
        }
        Ok(())
    };
    attrs(decl.attrs, roots)?;
    match decl.kind {
        DeclKind::Fn(function) => {
            for field in tree.field_list(function.params) {
                attrs(field.attrs, roots)?;
                push(roots, field.ty)?;
            }
            if let Some((result_attrs, ty)) = function.result {
                attrs(result_attrs, roots)?;
                push(roots, ty)?;
            }
        }
        DeclKind::Var(var) => {
            for &arg in tree.expr_list(var.template) {
                push(roots, arg)?;
            }
            for expr in [var.ty, var.init].into_iter().flatten() {
                push(roots, expr)?;
            }
        }
        DeclKind::Override(value) => {
            for expr in [value.ty, value.init].into_iter().flatten() {
                push(roots, expr)?;
            }
        }
        DeclKind::Const(value) => {
            for expr in [value.ty, Some(value.init)].into_iter().flatten() {
                push(roots, expr)?;
            }
        }
        DeclKind::Alias { ty, .. } => push(roots, ty)?,
        DeclKind::Struct { members, .. } => {
            for field in tree.field_list(members) {
                attrs(field.attrs, roots)?;
                push(roots, field.ty)?;
            }
        }
        DeclKind::ConstAssert(expr) => push(roots, expr)?,
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
