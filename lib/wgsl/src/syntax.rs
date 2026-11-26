//! The syntax tree: what the parser read, every node held in a pool of its
//! kind and named by its index there, each with the span it was read from,
//! and each list a run of a pool.

use alloc::vec::Vec;

use crate::diag::{reserve, Outcome, Span, Stop};

/// No parent: a root expression's.
pub(crate) const NO_PARENT: u32 = u32::MAX;

/// Each expression's first descendant and parent, by index.
pub(crate) struct Shape {
    pub(crate) first: Vec<u32>,
    pub(crate) parent: Vec<u32>,
}

impl Shape {
    /// The nodes of `root`'s subtree, children before parents.
    pub(crate) fn subtree(&self, root: ExprId) -> core::ops::RangeInclusive<u32> {
        let start = self.first.get(root.0 as usize).copied().unwrap_or(root.0);
        start..=root.0
    }

    /// The parent of `id`, if it has one.
    pub(crate) fn parent(&self, id: ExprId) -> Option<ExprId> {
        self.parent
            .get(id.0 as usize)
            .copied()
            .filter(|&p| p != NO_PARENT)
            .map(ExprId)
    }
}

/// An expression, by its index among the tree's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct ExprId(pub(crate) u32);

/// A statement, by its index among the tree's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct StmtId(pub(crate) u32);

/// A run of a pool: `len` items from `start`. An empty run is no list.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Run {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

impl Run {
    /// Whether it holds nothing.
    pub(crate) const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Its items' places in their pool.
    pub(crate) fn range(self) -> core::ops::Range<usize> {
        let start = self.start as usize;
        start..start + self.len as usize
    }
}

/// A name with the template list that may elaborate it: `vec3<f32>`'s
/// `vec3` and `f32`, `foo`'s alone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Ident {
    pub(crate) name: Span,
    /// Its template arguments, into [`Tree::expr_lists`]; empty for none.
    pub(crate) template: Run,
}

/// A literal's kind; its value is read from its span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Literal {
    Int,
    Float,
    Bool(bool),
}

/// A prefix operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum UnaryOp {
    /// `-`
    Negate,
    /// `!`
    Not,
    /// `~`
    Complement,
    /// `&`
    AddressOf,
    /// `*`
    Indirection,
}

/// An infix operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    ShiftLeft,
    ShiftRight,
    And,
    Or,
    Xor,
    LogicalAnd,
    LogicalOr,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

/// What an expression is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExprKind {
    Literal(Literal),
    /// A name, perhaps elaborated: a value, a type, or an enumerant.
    Ident(Ident),
    /// A call: a function, a type's constructor, or a built-in; its
    /// arguments into [`Tree::expr_lists`].
    Call(Ident, Run),
    Unary(UnaryOp, ExprId),
    Binary(BinaryOp, ExprId, ExprId),
    /// A member or swizzle: the base and the name after its `.`.
    Member(ExprId, Span),
    Index(ExprId, ExprId),
}

/// An expression and the span it was read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) span: Span,
}

/// An attribute: its name, and its arguments or diagnostic control.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Attr {
    pub(crate) name: Span,
    pub(crate) args: AttrArgs,
    pub(crate) span: Span,
}

/// What an attribute is given.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum AttrArgs {
    /// Expressions in parentheses, into [`Tree::expr_lists`]; none at all
    /// written as an empty run.
    Exprs(Run),
    /// `@diagnostic`'s severity and rule.
    Diagnostic(Control),
}

/// A diagnostic control: a severity's name and the rule's name, which may be
/// two names joined by `.`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Control {
    pub(crate) severity: Span,
    pub(crate) rule: (Span, Option<Span>),
}

/// A directive.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectiveKind {
    /// The extensions it enables, into [`Tree::names`].
    Enable(Run),
    /// The language extensions it requires, into [`Tree::names`].
    Requires(Run),
    Diagnostic(Control),
}

/// A directive and its span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Directive {
    pub(crate) kind: DirectiveKind,
    pub(crate) span: Span,
}

/// A `var` declaration: its template arguments — the address space and
/// access mode — its name, type and initializer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct VarDecl {
    pub(crate) template: Run,
    pub(crate) name: Span,
    pub(crate) ty: Option<ExprId>,
    pub(crate) init: Option<ExprId>,
}

/// A `let` or `const` declaration: its name, type and initializer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValueDecl {
    pub(crate) name: Span,
    pub(crate) ty: Option<ExprId>,
    pub(crate) init: ExprId,
}

/// An `override` declaration: its name, type and initializer, which it may
/// do without.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverrideDecl {
    pub(crate) name: Span,
    pub(crate) ty: Option<ExprId>,
    pub(crate) init: Option<ExprId>,
}

/// A loop's `continuing` statement: its attributes and body, the `break if`
/// that may end it, and its span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuing {
    pub(crate) attrs: Run,
    pub(crate) body: Run,
    pub(crate) break_if: Option<ExprId>,
    pub(crate) span: Span,
}

/// One case selector: an expression, or `default`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selector {
    pub(crate) expr: Option<ExprId>,
    pub(crate) span: Span,
}

/// A switch clause: its selectors, into [`Tree::selectors`], and its body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Clause {
    pub(crate) selectors: Run,
    pub(crate) body: StmtId,
    pub(crate) span: Span,
}

/// What a statement is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum StmtKind {
    /// Its statements, into [`Tree::stmt_lists`].
    Compound(Run),
    /// An assignment: `=` where `op` is none, else the compound operator.
    Assign {
        lhs: ExprId,
        op: Option<BinaryOp>,
        rhs: ExprId,
    },
    /// `_ = e;`
    Phony(ExprId),
    Increment(ExprId),
    Decrement(ExprId),
    /// An `if`, its `else if` chain an `if` statement in `reject`.
    If {
        condition: ExprId,
        accept: StmtId,
        reject: Option<StmtId>,
    },
    /// A `switch`: its selector, the attributes on its body, and its clauses
    /// into [`Tree::clauses`].
    Switch {
        selector: ExprId,
        body_attrs: Run,
        clauses: Run,
    },
    /// A `loop`: the attributes on its body, its statements, and its
    /// `continuing`, into [`Tree::continuings`].
    Loop {
        body_attrs: Run,
        body: Run,
        continuing: Option<u32>,
    },
    For {
        init: Option<StmtId>,
        condition: Option<ExprId>,
        update: Option<StmtId>,
        body: StmtId,
    },
    While {
        condition: ExprId,
        body: StmtId,
    },
    Break,
    Continue,
    Discard,
    Return(Option<ExprId>),
    /// A function call statement: the call.
    Call(ExprId),
    Let(ValueDecl),
    Const(ValueDecl),
    Var(VarDecl),
    ConstAssert(ExprId),
    /// `;` alone.
    Empty,
}

/// A statement, its span, and its attributes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Stmt {
    pub(crate) kind: StmtKind,
    pub(crate) span: Span,
    pub(crate) attrs: Run,
}

/// A function's formal parameter or a structure's member: its attributes,
/// name, type and span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Field {
    pub(crate) attrs: Run,
    pub(crate) name: Span,
    pub(crate) ty: ExprId,
    pub(crate) span: Span,
}

/// A function declaration.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FnDecl {
    pub(crate) name: Span,
    /// Its formal parameters, into [`Tree::fields`].
    pub(crate) params: Run,
    /// Its return type with that type's attributes.
    pub(crate) result: Option<(Run, ExprId)>,
    pub(crate) body: StmtId,
}

/// What a module-scope declaration is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum DeclKind {
    Fn(FnDecl),
    Var(VarDecl),
    Override(OverrideDecl),
    Const(ValueDecl),
    Alias {
        name: Span,
        ty: ExprId,
    },
    /// A structure: its name and its members, into [`Tree::fields`].
    Struct {
        name: Span,
        members: Run,
    },
    ConstAssert(ExprId),
}

/// A module-scope declaration, its span, and its attributes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Decl {
    pub(crate) kind: DeclKind,
    pub(crate) span: Span,
    pub(crate) attrs: Run,
}

/// The tree of one module: its directives and declarations in source order,
/// and the pools their nodes and lists are held in.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    pub(crate) directives: Vec<Directive>,
    pub(crate) decls: Vec<Decl>,
    pub(crate) exprs: Vec<Expr>,
    pub(crate) stmts: Vec<Stmt>,
    pub(crate) expr_lists: Vec<ExprId>,
    pub(crate) stmt_lists: Vec<StmtId>,
    pub(crate) attrs: Vec<Attr>,
    pub(crate) fields: Vec<Field>,
    pub(crate) clauses: Vec<Clause>,
    pub(crate) selectors: Vec<Selector>,
    pub(crate) continuings: Vec<Continuing>,
    pub(crate) names: Vec<Span>,
}

impl Tree {
    /// The expression `id` names.
    pub(crate) fn expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.0 as usize)
    }

    /// The statement `id` names.
    pub(crate) fn stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.0 as usize)
    }

    /// The expressions of `run`.
    pub(crate) fn expr_list(&self, run: Run) -> &[ExprId] {
        self.expr_lists.get(run.range()).unwrap_or(&[])
    }

    /// The statements of `run`.
    pub(crate) fn stmt_list(&self, run: Run) -> &[StmtId] {
        self.stmt_lists.get(run.range()).unwrap_or(&[])
    }

    /// The attributes of `run`.
    pub(crate) fn attr_list(&self, run: Run) -> &[Attr] {
        self.attrs.get(run.range()).unwrap_or(&[])
    }

    /// The parameters or members of `run`.
    pub(crate) fn field_list(&self, run: Run) -> &[Field] {
        self.fields.get(run.range()).unwrap_or(&[])
    }

    /// The clauses of `run`.
    pub(crate) fn clause_list(&self, run: Run) -> &[Clause] {
        self.clauses.get(run.range()).unwrap_or(&[])
    }

    /// The selectors of `run`.
    pub(crate) fn selector_list(&self, run: Run) -> &[Selector] {
        self.selectors.get(run.range()).unwrap_or(&[])
    }

    /// The names of `run`.
    pub(crate) fn name_list(&self, run: Run) -> &[Span] {
        self.names.get(run.range()).unwrap_or(&[])
    }

    /// Call `visit` with each of expression `id`'s children, in the order
    /// they were read.
    pub(crate) fn children(&self, id: ExprId, mut visit: impl FnMut(ExprId)) {
        let Some(expr) = self.expr(id) else {
            return;
        };
        match expr.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Ident(ident) => self
                .expr_list(ident.template)
                .iter()
                .copied()
                .for_each(visit),
            ExprKind::Call(ident, args) => {
                for &child in self.expr_list(ident.template) {
                    visit(child);
                }
                for &child in self.expr_list(args) {
                    visit(child);
                }
            }
            ExprKind::Unary(_, operand) | ExprKind::Member(operand, _) => visit(operand),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                visit(a);
                visit(b);
            }
        }
    }

    /// Each expression's first descendant and parent. The parser makes a
    /// node after its children, so an expression's subtree is exactly the
    /// run of nodes from its first descendant to itself: a pass over that
    /// run in order meets every child before its parent and recurses into
    /// nothing.
    pub(crate) fn shape(&self) -> Outcome<Shape> {
        let count = self.exprs.len();
        let mut first = Vec::new();
        let mut parent = Vec::new();
        reserve(&mut first, count)?;
        reserve(&mut parent, count)?;
        parent.resize(count, NO_PARENT);
        for index in 0..count {
            let id = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            let mut least = id;
            self.children(ExprId(id), |child| {
                if let Some(&start) = first.get(child.0 as usize) {
                    least = least.min(start);
                }
                if let Some(slot) = parent.get_mut(child.0 as usize) {
                    *slot = id;
                }
            });
            first.push(least);
        }
        Ok(Shape { first, parent })
    }
}
