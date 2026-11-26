//! The parser: tokens read by recursive descent over the specification's
//! grammar for it, into a [`Tree`]. Expressions nest at most
//! [`MAX_EXPRESSION_NESTING`] deep and statements [`MAX_BRACE_NESTING`], so
//! no input recurses further than those bounds allow.

use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::diag::{error, push, reserve, Code, Message, Outcome, Span, Stop};
use crate::lex::{Kind, Token};
use crate::syntax::{
    Attr, AttrArgs, BinaryOp, Clause, Continuing, Control, Decl, DeclKind, Directive,
    DirectiveKind, Expr, ExprId, ExprKind, Field, FnDecl, Ident, Literal, OverrideDecl, Run,
    Selector, Stmt, StmtId, StmtKind, Tree, UnaryOp, ValueDecl, VarDecl,
};
use crate::{
    MAX_BRACE_NESTING, MAX_CASE_SELECTORS, MAX_EXPRESSION_NESTING, MAX_PARAMETERS,
    MAX_STRUCT_MEMBERS, MAX_SYNTAX_NODES,
};

/// The tree of `text`, whose tokens are `tokens`, the last [`Kind::End`].
///
/// # Errors
///
/// Tokens the grammar does not admit, nesting or a list past its bound, as
/// a stop naming the span; the allocator's refusal.
pub(crate) fn parse(text: &str, tokens: &[Token]) -> Outcome<Tree> {
    let mut tree = Tree::default();
    // Each expression is made at one token of these kinds, and each statement
    // at one of the others, and neither pool holds more than the bound, so
    // the pools are sized once and never moved.
    let (exprs, stmts) = tokens.iter().fold((0, 0), |(exprs, stmts), token| {
        (
            exprs + usize::from(makes_expression(token.kind)),
            stmts + usize::from(makes_statement(token.kind)),
        )
    });
    reserve(&mut tree.exprs, exprs.min(MAX_SYNTAX_NODES))?;
    reserve(&mut tree.stmts, stmts.min(MAX_SYNTAX_NODES))?;
    let mut parser = Parser {
        text,
        tokens,
        at: 0,
        tree,
        expr_scratch: Vec::new(),
        stmt_scratch: Vec::new(),
        clause_scratch: Vec::new(),
        depth: 0,
        braces: 0,
    };
    parser.module()?;
    Ok(parser.tree)
}

/// Whether a token of `kind` may be where an expression is made: a name,
/// a literal, an operator, a member's `.` or an index's `[`.
const fn makes_expression(kind: Kind) -> bool {
    use Kind as K;
    matches!(
        kind,
        K::Ident
            | K::Int
            | K::Float
            | K::True
            | K::False
            | K::And
            | K::AndAnd
            | K::Slash
            | K::Bang
            | K::BracketLeft
            | K::EqualEqual
            | K::NotEqual
            | K::Greater
            | K::GreaterEqual
            | K::ShiftRight
            | K::Less
            | K::LessEqual
            | K::ShiftLeft
            | K::Percent
            | K::Minus
            | K::Period
            | K::Plus
            | K::Or
            | K::OrOr
            | K::Star
            | K::Tilde
            | K::Xor
    )
}

/// Whether a token of `kind` may be where a statement is made: the `;` most
/// end with, a block's `{`, or a keyword that begins one of the others.
const fn makes_statement(kind: Kind) -> bool {
    use Kind as K;
    matches!(
        kind,
        K::Semicolon
            | K::BraceLeft
            | K::If
            | K::For
            | K::While
            | K::Loop
            | K::Switch
            | K::Break
            | K::Continue
            | K::Continuing
            | K::Return
            | K::Discard
            | K::Else
    )
}

struct Parser<'t> {
    text: &'t str,
    tokens: &'t [Token],
    at: usize,
    tree: Tree,
    /// Lists being gathered, each moved whole into its pool once closed, so
    /// the lists nested within it never interleave with it there.
    expr_scratch: Vec<ExprId>,
    stmt_scratch: Vec<StmtId>,
    clause_scratch: Vec<Clause>,
    /// How deep the expression being read is nested.
    depth: u32,
    /// How deep the statement being read is nested in braces.
    braces: u32,
}

/// The binary operator a token is, at any precedence.
const fn binary(kind: Kind) -> Option<BinaryOp> {
    Some(match kind {
        Kind::Plus => BinaryOp::Add,
        Kind::Minus => BinaryOp::Subtract,
        Kind::Star => BinaryOp::Multiply,
        Kind::Slash => BinaryOp::Divide,
        Kind::Percent => BinaryOp::Remainder,
        Kind::ShiftLeft => BinaryOp::ShiftLeft,
        Kind::ShiftRight => BinaryOp::ShiftRight,
        Kind::And => BinaryOp::And,
        Kind::Or => BinaryOp::Or,
        Kind::Xor => BinaryOp::Xor,
        Kind::AndAnd => BinaryOp::LogicalAnd,
        Kind::OrOr => BinaryOp::LogicalOr,
        Kind::Less => BinaryOp::Less,
        Kind::LessEqual => BinaryOp::LessEqual,
        Kind::Greater => BinaryOp::Greater,
        Kind::GreaterEqual => BinaryOp::GreaterEqual,
        Kind::EqualEqual => BinaryOp::Equal,
        Kind::NotEqual => BinaryOp::NotEqual,
        _ => return None,
    })
}

/// The operator a compound assignment applies.
const fn compound(kind: Kind) -> Option<BinaryOp> {
    Some(match kind {
        Kind::PlusEqual => BinaryOp::Add,
        Kind::MinusEqual => BinaryOp::Subtract,
        Kind::StarEqual => BinaryOp::Multiply,
        Kind::SlashEqual => BinaryOp::Divide,
        Kind::PercentEqual => BinaryOp::Remainder,
        Kind::AndEqual => BinaryOp::And,
        Kind::OrEqual => BinaryOp::Or,
        Kind::XorEqual => BinaryOp::Xor,
        Kind::ShiftLeftEqual => BinaryOp::ShiftLeft,
        Kind::ShiftRightEqual => BinaryOp::ShiftRight,
        _ => return None,
    })
}

/// Whether a token is spelled as a name — an identifier, a keyword or a
/// reserved word — as a context-dependent name may be.
const fn name_shaped(kind: Kind) -> bool {
    !matches!(
        kind,
        Kind::Int
            | Kind::Float
            | Kind::And
            | Kind::AndAnd
            | Kind::Arrow
            | Kind::At
            | Kind::Slash
            | Kind::Bang
            | Kind::BracketLeft
            | Kind::BracketRight
            | Kind::BraceLeft
            | Kind::BraceRight
            | Kind::Colon
            | Kind::Comma
            | Kind::Equal
            | Kind::EqualEqual
            | Kind::NotEqual
            | Kind::Greater
            | Kind::GreaterEqual
            | Kind::ShiftRight
            | Kind::Less
            | Kind::LessEqual
            | Kind::ShiftLeft
            | Kind::Percent
            | Kind::Minus
            | Kind::MinusMinus
            | Kind::Period
            | Kind::Plus
            | Kind::PlusPlus
            | Kind::Or
            | Kind::OrOr
            | Kind::ParenLeft
            | Kind::ParenRight
            | Kind::Semicolon
            | Kind::Star
            | Kind::Tilde
            | Kind::Underscore
            | Kind::Xor
            | Kind::PlusEqual
            | Kind::MinusEqual
            | Kind::StarEqual
            | Kind::SlashEqual
            | Kind::PercentEqual
            | Kind::AndEqual
            | Kind::OrEqual
            | Kind::XorEqual
            | Kind::ShiftRightEqual
            | Kind::ShiftLeftEqual
            | Kind::TemplateStart
            | Kind::TemplateEnd
            | Kind::End
    )
}

impl Parser<'_> {
    fn token(&self) -> Token {
        // The last token is the end, and nothing advances past it.
        self.tokens
            .get(self.at)
            .or_else(|| self.tokens.last())
            .copied()
            .unwrap_or(Token {
                kind: Kind::End,
                span: Span::default(),
            })
    }

    fn peek(&self) -> Kind {
        self.token().kind
    }

    fn peek_second(&self) -> Kind {
        self.tokens.get(self.at + 1).map_or(Kind::End, |t| t.kind)
    }

    fn advance(&mut self) -> Token {
        let token = self.token();
        if token.kind != Kind::End {
            self.at += 1;
        }
        token
    }

    fn eat(&mut self, kind: Kind) -> bool {
        let found = self.peek() == kind;
        if found {
            self.advance();
        }
        found
    }

    fn spell(&self, span: Span) -> &str {
        self.text
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
    }

    /// The stop for the current token where `expected` was.
    fn unexpected(&self, expected: &str) -> Stop {
        let token = self.token();
        error(Code::Syntax, token.span, |m| {
            write!(m, "expected {expected}, found ")?;
            self.describe(m, token)
        })
    }

    fn describe(&self, m: &mut Message, token: Token) -> core::fmt::Result {
        match token.kind {
            Kind::End => m.write_str("the end of the module"),
            Kind::Ident => {
                m.write_str("the name ")?;
                m.quote(self.spell(token.span))
            }
            Kind::Int | Kind::Float => {
                m.write_str("the literal ")?;
                m.quote(self.spell(token.span))
            }
            Kind::Reserved => {
                m.write_str("the reserved word ")?;
                m.quote(self.spell(token.span))
            }
            Kind::TemplateStart => m.write_str("the `<` opening a template list"),
            Kind::TemplateEnd => m.write_str("the `>` closing a template list"),
            _ => m.quote(self.spell(token.span)),
        }
    }

    fn expect(&mut self, kind: Kind, expected: &str) -> Outcome<Token> {
        if self.peek() == kind {
            Ok(self.advance())
        } else {
            Err(self.unexpected(expected))
        }
    }

    /// A name declared or named: an identifier, never a keyword or a
    /// reserved word.
    fn ident(&mut self) -> Outcome<Span> {
        match self.peek() {
            Kind::Ident => Ok(self.advance().span),
            Kind::Reserved => Err(self.reserved()),
            _ => Err(self.unexpected("a name")),
        }
    }

    /// The stop for the reserved word now current.
    fn reserved(&self) -> Stop {
        let token = self.token();
        error(Code::ReservedWord, token.span, |m| {
            m.quote(self.spell(token.span))?;
            m.write_str(" is reserved and names nothing")
        })
    }

    /// A context-dependent name: any token spelled as a name.
    fn spelled_name(&mut self, expected: &str) -> Outcome<Span> {
        if name_shaped(self.peek()) {
            Ok(self.advance().span)
        } else {
            Err(self.unexpected(expected))
        }
    }

    fn node(&mut self, kind: ExprKind, span: Span) -> Outcome<ExprId> {
        self.room_for_another(span)?;
        let id = ExprId(u32::try_from(self.tree.exprs.len()).map_err(|_| Stop::OutOfMemory)?);
        push(&mut self.tree.exprs, Expr { kind, span })?;
        Ok(id)
    }

    fn statement_node(&mut self, kind: StmtKind, span: Span, attrs: Run) -> Outcome<StmtId> {
        self.room_for_another(span)?;
        let id = StmtId(u32::try_from(self.tree.stmts.len()).map_err(|_| Stop::OutOfMemory)?);
        push(&mut self.tree.stmts, Stmt { kind, span, attrs })?;
        Ok(id)
    }

    /// Refuse, at `span`, the expression or statement past the bound.
    fn room_for_another(&self, span: Span) -> Outcome<()> {
        if self.tree.exprs.len() + self.tree.stmts.len() < MAX_SYNTAX_NODES {
            return Ok(());
        }
        Err(error(Code::ModuleTooLarge, span, |m| {
            write!(
                m,
                "a module holds at most {MAX_SYNTAX_NODES} expressions and statements"
            )
        }))
    }

    /// The span from `start` to the last token read.
    fn since(&self, start: Span) -> Span {
        let last = self
            .at
            .checked_sub(1)
            .and_then(|at| self.tokens.get(at))
            .map_or(start, |t| t.span);
        start.to(last)
    }

    /// Move the expressions gathered since `mark` into their pool.
    fn close_exprs(&mut self, mark: usize) -> Outcome<Run> {
        close(&mut self.tree.expr_lists, &mut self.expr_scratch, mark)
    }

    fn close_stmts(&mut self, mark: usize) -> Outcome<Run> {
        close(&mut self.tree.stmt_lists, &mut self.stmt_scratch, mark)
    }

    fn enter(&mut self) -> Outcome<()> {
        if self.depth >= MAX_EXPRESSION_NESTING {
            let span = self.token().span;
            return Err(error(Code::NestingTooDeep, span, |m| {
                write!(
                    m,
                    "expressions nest more than {MAX_EXPRESSION_NESTING} deep"
                )
            }));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn module(&mut self) -> Outcome<()> {
        while matches!(
            self.peek(),
            Kind::Enable | Kind::Requires | Kind::Diagnostic
        ) {
            self.directive()?;
        }
        loop {
            match self.peek() {
                Kind::End => return Ok(()),
                Kind::Semicolon => {
                    self.advance();
                }
                Kind::Enable | Kind::Requires | Kind::Diagnostic => {
                    let span = self.token().span;
                    return Err(error(Code::Directive, span, |m| {
                        m.write_str("a directive must precede every declaration")
                    }));
                }
                Kind::ConstAssert => {
                    let start = self.advance().span;
                    let expr = self.expression()?;
                    self.expect(Kind::Semicolon, "`;`")?;
                    self.declare(DeclKind::ConstAssert(expr), start, Run::default())?;
                }
                _ => self.global_decl()?,
            }
        }
    }

    fn declare(&mut self, kind: DeclKind, start: Span, attrs: Run) -> Outcome<()> {
        let span = self.since(start);
        push(&mut self.tree.decls, Decl { kind, span, attrs })
    }

    fn directive(&mut self) -> Outcome<()> {
        let keyword = self.advance();
        let kind = if keyword.kind == Kind::Diagnostic {
            let control = self.control()?;
            DirectiveKind::Diagnostic(control)
        } else {
            let start = self.tree.names.len();
            loop {
                let name = self.spelled_name("an extension's name")?;
                push(&mut self.tree.names, name)?;
                if !self.eat(Kind::Comma) || self.peek() == Kind::Semicolon {
                    break;
                }
            }
            let names = run_from(start, self.tree.names.len())?;
            if keyword.kind == Kind::Enable {
                DirectiveKind::Enable(names)
            } else {
                DirectiveKind::Requires(names)
            }
        };
        self.expect(Kind::Semicolon, "`;`")?;
        let span = self.since(keyword.span);
        push(&mut self.tree.directives, Directive { kind, span })
    }

    /// `( severity , rule )`, a rule's name one name or two joined by `.`.
    fn control(&mut self) -> Outcome<Control> {
        self.expect(Kind::ParenLeft, "`(`")?;
        let severity = self.spelled_name("a severity")?;
        self.expect(Kind::Comma, "`,`")?;
        let first = self.spelled_name("a diagnostic rule's name")?;
        let second = if self.eat(Kind::Period) {
            Some(self.spelled_name("a diagnostic rule's name")?)
        } else {
            None
        };
        self.eat(Kind::Comma);
        self.expect(Kind::ParenRight, "`)`")?;
        Ok(Control {
            severity,
            rule: (first, second),
        })
    }

    fn attributes(&mut self) -> Outcome<Run> {
        let start = self.tree.attrs.len();
        while self.peek() == Kind::At {
            let at = self.advance().span;
            let name = match self.peek() {
                Kind::Ident | Kind::Const | Kind::Diagnostic => self.advance(),
                _ => return Err(self.unexpected("an attribute's name")),
            };
            let args = if name.kind == Kind::Diagnostic {
                AttrArgs::Diagnostic(self.control()?)
            } else if self.peek() == Kind::ParenLeft {
                AttrArgs::Exprs(self.arguments()?)
            } else {
                AttrArgs::Exprs(Run::default())
            };
            let span = self.since(at);
            push(
                &mut self.tree.attrs,
                Attr {
                    name: name.span,
                    args,
                    span,
                },
            )?;
        }
        run_from(start, self.tree.attrs.len())
    }

    /// `( expression , … )`, a comma after the last allowed.
    fn arguments(&mut self) -> Outcome<Run> {
        self.expect(Kind::ParenLeft, "`(`")?;
        let mark = self.expr_scratch.len();
        while self.peek() != Kind::ParenRight {
            let argument = self.expression()?;
            push(&mut self.expr_scratch, argument)?;
            if !self.eat(Kind::Comma) {
                break;
            }
        }
        self.expect(Kind::ParenRight, "`)`")?;
        self.close_exprs(mark)
    }

    /// A template list's arguments, its delimiters read.
    fn template(&mut self) -> Outcome<Run> {
        self.expect(Kind::TemplateStart, "`<`")?;
        let mark = self.expr_scratch.len();
        loop {
            let argument = self.expression()?;
            push(&mut self.expr_scratch, argument)?;
            if !self.eat(Kind::Comma) || self.peek() == Kind::TemplateEnd {
                break;
            }
        }
        self.expect(Kind::TemplateEnd, "`>` or `,`")?;
        self.close_exprs(mark)
    }

    /// A name and the template list elaborating it, if one does.
    fn elaborated(&mut self) -> Outcome<Ident> {
        let name = self.ident()?;
        let template = if self.peek() == Kind::TemplateStart {
            self.enter()?;
            let template = self.template();
            self.leave();
            template?
        } else {
            Run::default()
        };
        Ok(Ident { name, template })
    }

    /// A type: a name, perhaps elaborated.
    fn type_specifier(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        let ident = self.elaborated()?;
        let span = self.since(start);
        self.node(ExprKind::Ident(ident), span)
    }

    /// `name` then `: type` if a colon follows.
    fn typed_name(&mut self) -> Outcome<(Span, Option<ExprId>)> {
        let name = self.ident()?;
        let ty = if self.eat(Kind::Colon) {
            Some(self.type_specifier()?)
        } else {
            None
        };
        Ok((name, ty))
    }

    fn global_decl(&mut self) -> Outcome<()> {
        let start = self.token().span;
        let attrs = self.attributes()?;
        match self.peek() {
            Kind::Fn => self.function(start, attrs),
            Kind::Var => {
                let var = self.variable()?;
                self.expect(Kind::Semicolon, "`;`")?;
                self.declare(DeclKind::Var(var), start, attrs)
            }
            Kind::Override => {
                self.advance();
                let (name, ty) = self.typed_name()?;
                let init = if self.eat(Kind::Equal) {
                    Some(self.expression()?)
                } else {
                    None
                };
                self.expect(Kind::Semicolon, "`;`")?;
                self.declare(
                    DeclKind::Override(OverrideDecl { name, ty, init }),
                    start,
                    attrs,
                )
            }
            Kind::Const | Kind::Alias | Kind::Struct if !attrs.is_empty() => {
                Err(self.unexpected("`fn`, `var` or `override` after attributes"))
            }
            Kind::Const => {
                self.advance();
                let (name, ty) = self.typed_name()?;
                self.expect(Kind::Equal, "`=`")?;
                let init = self.expression()?;
                self.expect(Kind::Semicolon, "`;`")?;
                self.declare(DeclKind::Const(ValueDecl { name, ty, init }), start, attrs)
            }
            Kind::Alias => {
                self.advance();
                let name = self.ident()?;
                self.expect(Kind::Equal, "`=`")?;
                let ty = self.type_specifier()?;
                self.expect(Kind::Semicolon, "`;`")?;
                self.declare(DeclKind::Alias { name, ty }, start, attrs)
            }
            Kind::Struct => self.structure(start),
            _ => Err(self.unexpected("a declaration")),
        }
    }

    fn function(&mut self, start: Span, attrs: Run) -> Outcome<()> {
        self.advance();
        let name = self.ident()?;
        self.expect(Kind::ParenLeft, "`(`")?;
        let first = self.tree.fields.len();
        while self.peek() != Kind::ParenRight {
            let field = self.field()?;
            if self.tree.fields.len() - first >= MAX_PARAMETERS {
                return Err(error(Code::TooMany, field.span, |m| {
                    write!(m, "a function takes at most {MAX_PARAMETERS} parameters")
                }));
            }
            push(&mut self.tree.fields, field)?;
            if !self.eat(Kind::Comma) {
                break;
            }
        }
        self.expect(Kind::ParenRight, "`)`")?;
        let params = run_from(first, self.tree.fields.len())?;
        let result = if self.eat(Kind::Arrow) {
            let attrs = self.attributes()?;
            Some((attrs, self.type_specifier()?))
        } else {
            None
        };
        let body = self.attributed_compound()?;
        let decl = FnDecl {
            name,
            params,
            result,
            body,
        };
        self.declare(DeclKind::Fn(decl), start, attrs)
    }

    /// A parameter or member: attributes, a name, `:` and a type.
    fn field(&mut self) -> Outcome<Field> {
        let start = self.token().span;
        let attrs = self.attributes()?;
        let name = self.ident()?;
        self.expect(Kind::Colon, "`:`")?;
        let ty = self.type_specifier()?;
        Ok(Field {
            attrs,
            name,
            ty,
            span: self.since(start),
        })
    }

    fn structure(&mut self, start: Span) -> Outcome<()> {
        self.advance();
        let name = self.ident()?;
        self.expect(Kind::BraceLeft, "`{`")?;
        let first = self.tree.fields.len();
        loop {
            let member = self.field()?;
            if self.tree.fields.len() - first >= MAX_STRUCT_MEMBERS {
                return Err(error(Code::TooMany, member.span, |m| {
                    write!(m, "a structure holds at most {MAX_STRUCT_MEMBERS} members")
                }));
            }
            push(&mut self.tree.fields, member)?;
            if !self.eat(Kind::Comma) || self.peek() == Kind::BraceRight {
                break;
            }
        }
        self.expect(Kind::BraceRight, "`}` or `,`")?;
        let members = run_from(first, self.tree.fields.len())?;
        self.declare(DeclKind::Struct { name, members }, start, Run::default())
    }

    /// `var`, its template list, and its typed name and initializer.
    fn variable(&mut self) -> Outcome<VarDecl> {
        self.advance();
        let template = if self.peek() == Kind::TemplateStart {
            self.template()?
        } else {
            Run::default()
        };
        let (name, ty) = self.typed_name()?;
        let init = if self.eat(Kind::Equal) {
            Some(self.expression()?)
        } else {
            None
        };
        Ok(VarDecl {
            template,
            name,
            ty,
            init,
        })
    }

    /// A compound statement and the attributes it begins with.
    fn attributed_compound(&mut self) -> Outcome<StmtId> {
        let start = self.token().span;
        let attrs = self.attributes()?;
        self.compound(start, attrs)
    }

    /// `{ statements }`, its attributes `attrs` already read.
    fn compound(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        self.expect(Kind::BraceLeft, "`{`")?;
        self.nest()?;
        let body = self.statements(&[Kind::BraceRight]);
        self.braces -= 1;
        let body = body?;
        self.expect(Kind::BraceRight, "`}`")?;
        let span = self.since(start);
        self.statement_node(StmtKind::Compound(body), span, attrs)
    }

    fn nest(&mut self) -> Outcome<()> {
        if self.braces >= MAX_BRACE_NESTING {
            let span = self.token().span;
            return Err(error(Code::NestingTooDeep, span, |m| {
                write!(m, "statements nest more than {MAX_BRACE_NESTING} deep")
            }));
        }
        self.braces += 1;
        Ok(())
    }

    /// The statements up to the first token of `until`, left unread.
    fn statements(&mut self, until: &[Kind]) -> Outcome<Run> {
        let mark = self.stmt_scratch.len();
        while !until.contains(&self.peek()) && self.peek() != Kind::End {
            let statement = self.statement()?;
            push(&mut self.stmt_scratch, statement)?;
        }
        self.close_stmts(mark)
    }

    fn statement(&mut self) -> Outcome<StmtId> {
        let start = self.token().span;
        let attrs = self.attributes()?;
        match self.peek() {
            Kind::For => return self.for_loop(start, attrs),
            Kind::If => return self.conditional(start, attrs),
            Kind::Loop => return self.endless(start, attrs),
            Kind::Switch => return self.switch(start, attrs),
            Kind::While => return self.while_loop(start, attrs),
            Kind::BraceLeft => return self.compound(start, attrs),
            _ if !attrs.is_empty() => return Err(self.unexpected(
                "a compound, `for`, `if`, `loop`, `switch` or `while` statement after attributes",
            )),
            _ => {}
        }
        let kind = match self.peek() {
            Kind::Semicolon => {
                self.advance();
                return self.statement_node(StmtKind::Empty, start, attrs);
            }
            Kind::Discard => {
                self.advance();
                StmtKind::Discard
            }
            Kind::Return => {
                self.advance();
                let value = if self.peek() == Kind::Semicolon {
                    None
                } else {
                    Some(self.expression()?)
                };
                StmtKind::Return(value)
            }
            Kind::Break => {
                self.advance();
                if self.peek() == Kind::If {
                    return Err(error(Code::Syntax, self.token().span, |m| {
                        m.write_str("`break if` ends only a `continuing` block")
                    }));
                }
                StmtKind::Break
            }
            Kind::Continue => {
                self.advance();
                StmtKind::Continue
            }
            Kind::ConstAssert => {
                self.advance();
                StmtKind::ConstAssert(self.expression()?)
            }
            Kind::Ident if matches!(self.peek_second(), Kind::ParenLeft | Kind::TemplateStart) => {
                StmtKind::Call(self.call()?)
            }
            _ => self.simple()?,
        };
        self.expect(Kind::Semicolon, "`;`")?;
        let span = self.since(start);
        self.statement_node(kind, span, attrs)
    }

    /// A declaration or an update, as each may begin a `for` or stand alone,
    /// before its `;`.
    fn simple(&mut self) -> Outcome<StmtKind> {
        match self.peek() {
            Kind::Var => Ok(StmtKind::Var(self.variable()?)),
            Kind::Let | Kind::Const => {
                let constant = self.advance().kind == Kind::Const;
                let (name, ty) = self.typed_name()?;
                self.expect(Kind::Equal, "`=`")?;
                let value = ValueDecl {
                    name,
                    ty,
                    init: self.expression()?,
                };
                Ok(if constant {
                    StmtKind::Const(value)
                } else {
                    StmtKind::Let(value)
                })
            }
            _ => self.update(),
        }
    }

    /// An assignment, increment or decrement, or a phony assignment.
    fn update(&mut self) -> Outcome<StmtKind> {
        if self.eat(Kind::Underscore) {
            self.expect(Kind::Equal, "`=`")?;
            return Ok(StmtKind::Phony(self.expression()?));
        }
        let lhs = self.lhs()?;
        let next = self.peek();
        if next == Kind::PlusPlus || next == Kind::MinusMinus {
            self.advance();
            return Ok(if next == Kind::PlusPlus {
                StmtKind::Increment(lhs)
            } else {
                StmtKind::Decrement(lhs)
            });
        }
        let op = if next == Kind::Equal {
            None
        } else if let Some(op) = compound(next) {
            Some(op)
        } else {
            return Err(self.unexpected("`=`, a compound assignment, `++` or `--`"));
        };
        self.advance();
        let rhs = self.expression()?;
        Ok(StmtKind::Assign { lhs, op, rhs })
    }

    /// The left side of an assignment: a name or a parenthesised one, its
    /// members and indexes, or one dereferenced or taken the address of.
    fn lhs(&mut self) -> Outcome<ExprId> {
        self.enter()?;
        let parsed = self.lhs_within();
        self.leave();
        parsed
    }

    fn lhs_within(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        let op = match self.peek() {
            Kind::And => Some(UnaryOp::AddressOf),
            Kind::Star => Some(UnaryOp::Indirection),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let inner = self.lhs()?;
            let span = self.since(start);
            return self.node(ExprKind::Unary(op, inner), span);
        }
        let core = if self.eat(Kind::ParenLeft) {
            let inner = self.lhs()?;
            self.expect(Kind::ParenRight, "`)`")?;
            inner
        } else {
            let name = self.ident()?;
            self.node(
                ExprKind::Ident(Ident {
                    name,
                    template: Run::default(),
                }),
                name,
            )?
        };
        self.postfix(core, start)
    }

    fn call(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        let callee = self.elaborated()?;
        let args = self.arguments()?;
        let span = self.since(start);
        self.node(ExprKind::Call(callee, args), span)
    }

    fn conditional(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        // An `else if` chain is linked from its end, so its length is never
        // recursion.
        let mut arms: Vec<(Span, ExprId, StmtId)> = Vec::new();
        let mut arm_start = start;
        let mut reject = None;
        loop {
            self.expect(Kind::If, "`if`")?;
            let condition = self.expression()?;
            let accept = self.attributed_compound()?;
            push(&mut arms, (arm_start, condition, accept))?;
            if !self.eat(Kind::Else) {
                break;
            }
            if self.peek() == Kind::If {
                arm_start = self.token().span;
                continue;
            }
            reject = Some(self.attributed_compound()?);
            break;
        }
        let end = self.since(start);
        let mut next = reject;
        let mut last = None;
        for (index, &(arm, condition, accept)) in arms.iter().enumerate().rev() {
            let arm_attrs = if index == 0 { attrs } else { Run::default() };
            let span = Span::new(arm.start, end.end);
            let id = self.statement_node(
                StmtKind::If {
                    condition,
                    accept,
                    reject: next,
                },
                span,
                arm_attrs,
            )?;
            next = Some(id);
            last = Some(id);
        }
        last.ok_or_else(|| self.unexpected("`if`"))
    }

    fn switch(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        self.advance();
        let selector = self.expression()?;
        let body_attrs = self.attributes()?;
        self.expect(Kind::BraceLeft, "`{`")?;
        self.nest()?;
        let clauses = self.clauses();
        self.braces -= 1;
        let clauses = clauses?;
        self.expect(Kind::BraceRight, "`}`")?;
        let span = self.since(start);
        self.statement_node(
            StmtKind::Switch {
                selector,
                body_attrs,
                clauses,
            },
            span,
            attrs,
        )
    }

    fn clauses(&mut self) -> Outcome<Run> {
        let mark = self.clause_scratch.len();
        let mut selectors = 0usize;
        while matches!(self.peek(), Kind::Case | Kind::Default) {
            let start = self.token().span;
            let first = self.tree.selectors.len();
            if self.eat(Kind::Default) {
                push(
                    &mut self.tree.selectors,
                    Selector {
                        expr: None,
                        span: start,
                    },
                )?;
            } else {
                self.advance();
                loop {
                    let span = self.token().span;
                    let expr = if self.eat(Kind::Default) {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    let span = self.since(span);
                    push(&mut self.tree.selectors, Selector { expr, span })?;
                    if !self.eat(Kind::Comma)
                        || matches!(self.peek(), Kind::Colon | Kind::BraceLeft)
                    {
                        break;
                    }
                }
            }
            selectors += self.tree.selectors.len() - first;
            if selectors > MAX_CASE_SELECTORS {
                return Err(error(Code::TooMany, start, |m| {
                    write!(
                        m,
                        "a switch takes at most {MAX_CASE_SELECTORS} case selectors"
                    )
                }));
            }
            self.eat(Kind::Colon);
            // Closed before the body, whose own switches add selectors.
            let cases = run_from(first, self.tree.selectors.len())?;
            let body = self.attributed_compound()?;
            let span = self.since(start);
            let clause = Clause {
                selectors: cases,
                body,
                span,
            };
            push(&mut self.clause_scratch, clause)?;
        }
        close(&mut self.tree.clauses, &mut self.clause_scratch, mark)
    }

    fn endless(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        self.advance();
        let body_attrs = self.attributes()?;
        self.expect(Kind::BraceLeft, "`{`")?;
        self.nest()?;
        let parsed = self.loop_body();
        self.braces -= 1;
        let (body, continuing) = parsed?;
        self.expect(Kind::BraceRight, "`}`")?;
        let span = self.since(start);
        self.statement_node(
            StmtKind::Loop {
                body_attrs,
                body,
                continuing,
            },
            span,
            attrs,
        )
    }

    /// A loop's statements, and its `continuing` where it ends with one.
    fn loop_body(&mut self) -> Outcome<(Run, Option<u32>)> {
        let body = self.statements(&[Kind::BraceRight, Kind::Continuing])?;
        if self.peek() != Kind::Continuing {
            return Ok((body, None));
        }
        let start = self.advance().span;
        let attrs = self.attributes()?;
        self.expect(Kind::BraceLeft, "`{`")?;
        self.nest()?;
        let parsed = self.continuing_body();
        self.braces -= 1;
        let (statements, break_if) = parsed?;
        self.expect(Kind::BraceRight, "`}`")?;
        let continuing = Continuing {
            attrs,
            body: statements,
            break_if,
            span: self.since(start),
        };
        let index = u32::try_from(self.tree.continuings.len()).map_err(|_| Stop::OutOfMemory)?;
        push(&mut self.tree.continuings, continuing)?;
        Ok((body, Some(index)))
    }

    /// A `continuing` block's statements and the `break if` ending it.
    fn continuing_body(&mut self) -> Outcome<(Run, Option<ExprId>)> {
        let mark = self.stmt_scratch.len();
        let mut break_if = None;
        while !matches!(self.peek(), Kind::BraceRight | Kind::End) {
            if self.peek() == Kind::Break && self.peek_second() == Kind::If {
                self.advance();
                self.advance();
                break_if = Some(self.expression()?);
                self.expect(Kind::Semicolon, "`;`")?;
                if self.peek() != Kind::BraceRight {
                    return Err(self.unexpected("`}` after `break if`"));
                }
                break;
            }
            let statement = self.statement()?;
            push(&mut self.stmt_scratch, statement)?;
        }
        Ok((self.close_stmts(mark)?, break_if))
    }

    fn for_loop(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        self.advance();
        self.expect(Kind::ParenLeft, "`(`")?;
        let init = if self.peek() == Kind::Semicolon {
            None
        } else {
            Some(self.for_part()?)
        };
        self.expect(Kind::Semicolon, "`;`")?;
        let condition = if self.peek() == Kind::Semicolon {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(Kind::Semicolon, "`;`")?;
        let update = if self.peek() == Kind::ParenRight {
            None
        } else {
            if matches!(self.peek(), Kind::Var | Kind::Let | Kind::Const) {
                return Err(self.unexpected("an update"));
            }
            Some(self.for_part()?)
        };
        self.expect(Kind::ParenRight, "`)`")?;
        let body = self.attributed_compound()?;
        let span = self.since(start);
        self.statement_node(
            StmtKind::For {
                init,
                condition,
                update,
                body,
            },
            span,
            attrs,
        )
    }

    /// A `for` loop's initializer or update: a call, a declaration, or an
    /// update, with no `;`.
    fn for_part(&mut self) -> Outcome<StmtId> {
        let start = self.token().span;
        let kind = if self.peek() == Kind::Ident
            && matches!(self.peek_second(), Kind::ParenLeft | Kind::TemplateStart)
        {
            StmtKind::Call(self.call()?)
        } else {
            self.simple()?
        };
        let span = self.since(start);
        self.statement_node(kind, span, Run::default())
    }

    fn while_loop(&mut self, start: Span, attrs: Run) -> Outcome<StmtId> {
        self.advance();
        let condition = self.expression()?;
        let body = self.attributed_compound()?;
        let span = self.since(start);
        self.statement_node(StmtKind::While { condition, body }, span, attrs)
    }

    /// An expression, as the grammar's precedence forms it: a chain of one
    /// bitwise operator, or a comparison of shifts or sums, perhaps chained
    /// by one short-circuiting operator.
    fn expression(&mut self) -> Outcome<ExprId> {
        self.enter()?;
        let parsed = self.expression_within();
        self.leave();
        parsed
    }

    fn expression_within(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        let first = self.unary()?;
        if matches!(self.peek(), Kind::And | Kind::Or | Kind::Xor) {
            return self.chain(first, start, Self::unary);
        }
        let relation = self.relational(first, start)?;
        if matches!(self.peek(), Kind::AndAnd | Kind::OrOr) {
            let operator = self.peek();
            let mut lhs = relation;
            while self.peek() == operator {
                let op = binary(self.advance().kind).ok_or_else(|| self.unexpected("`&&`"))?;
                let next_start = self.token().span;
                let unary = self.unary()?;
                let rhs = self.relational(unary, next_start)?;
                let span = self.since(start);
                lhs = self.node(ExprKind::Binary(op, lhs, rhs), span)?;
            }
            return Ok(lhs);
        }
        Ok(relation)
    }

    /// `first` then a run of the operator now current, each joining the
    /// operand `operand` reads.
    fn chain(
        &mut self,
        first: ExprId,
        start: Span,
        operand: fn(&mut Self) -> Outcome<ExprId>,
    ) -> Outcome<ExprId> {
        let operator = self.peek();
        let mut lhs = first;
        while self.peek() == operator {
            let op = binary(self.advance().kind).ok_or_else(|| self.unexpected("an operator"))?;
            let rhs = operand(self)?;
            let span = self.since(start);
            lhs = self.node(ExprKind::Binary(op, lhs, rhs), span)?;
        }
        Ok(lhs)
    }

    /// A shift or sum beginning `first`, compared with another where a
    /// comparison follows: comparisons do not chain.
    fn relational(&mut self, first: ExprId, start: Span) -> Outcome<ExprId> {
        let lhs = self.shift(first, start)?;
        let op = match self.peek() {
            Kind::Less
            | Kind::LessEqual
            | Kind::Greater
            | Kind::GreaterEqual
            | Kind::EqualEqual
            | Kind::NotEqual => binary(self.advance().kind),
            _ => return Ok(lhs),
        };
        let op = op.ok_or_else(|| self.unexpected("a comparison"))?;
        let rhs_start = self.token().span;
        let unary = self.unary()?;
        let rhs = self.shift(unary, rhs_start)?;
        let span = self.since(start);
        self.node(ExprKind::Binary(op, lhs, rhs), span)
    }

    /// A shift of two unary operands, or a sum of products, beginning
    /// `first`.
    fn shift(&mut self, first: ExprId, start: Span) -> Outcome<ExprId> {
        if matches!(self.peek(), Kind::ShiftLeft | Kind::ShiftRight) {
            let op = binary(self.advance().kind).ok_or_else(|| self.unexpected("a shift"))?;
            let rhs = self.unary()?;
            let span = self.since(start);
            return self.node(ExprKind::Binary(op, first, rhs), span);
        }
        let mut lhs = self.product(first, start)?;
        while matches!(self.peek(), Kind::Plus | Kind::Minus) {
            let op = binary(self.advance().kind).ok_or_else(|| self.unexpected("`+`"))?;
            let rhs_start = self.token().span;
            let unary = self.unary()?;
            let rhs = self.product(unary, rhs_start)?;
            let span = self.since(start);
            lhs = self.node(ExprKind::Binary(op, lhs, rhs), span)?;
        }
        Ok(lhs)
    }

    /// `first` then a run of multiplicative operators and unary operands.
    fn product(&mut self, first: ExprId, start: Span) -> Outcome<ExprId> {
        let mut lhs = first;
        while matches!(self.peek(), Kind::Star | Kind::Slash | Kind::Percent) {
            let op = binary(self.advance().kind).ok_or_else(|| self.unexpected("`*`"))?;
            let rhs = self.unary()?;
            let span = self.since(start);
            lhs = self.node(ExprKind::Binary(op, lhs, rhs), span)?;
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        let op = match self.peek() {
            Kind::Minus => UnaryOp::Negate,
            Kind::Bang => UnaryOp::Not,
            Kind::Tilde => UnaryOp::Complement,
            Kind::And => UnaryOp::AddressOf,
            Kind::Star => UnaryOp::Indirection,
            _ => {
                let primary = self.primary()?;
                return self.postfix(primary, start);
            }
        };
        self.advance();
        self.enter()?;
        let operand = self.unary();
        self.leave();
        let operand = operand?;
        let span = self.since(start);
        self.node(ExprKind::Unary(op, operand), span)
    }

    fn primary(&mut self) -> Outcome<ExprId> {
        let start = self.token().span;
        match self.peek() {
            Kind::Ident => {
                let ident = self.elaborated()?;
                if self.peek() == Kind::ParenLeft {
                    let args = self.arguments()?;
                    let span = self.since(start);
                    self.node(ExprKind::Call(ident, args), span)
                } else {
                    let span = self.since(start);
                    self.node(ExprKind::Ident(ident), span)
                }
            }
            Kind::Int => {
                self.advance();
                self.node(ExprKind::Literal(Literal::Int), start)
            }
            Kind::Float => {
                self.advance();
                self.node(ExprKind::Literal(Literal::Float), start)
            }
            Kind::True | Kind::False => {
                let value = self.advance().kind == Kind::True;
                self.node(ExprKind::Literal(Literal::Bool(value)), start)
            }
            Kind::ParenLeft => {
                self.advance();
                let inner = self.expression()?;
                self.expect(Kind::ParenRight, "`)`")?;
                Ok(inner)
            }
            Kind::Reserved => Err(self.reserved()),
            _ => Err(self.unexpected("an expression")),
        }
    }

    /// `base` then its members, swizzles and indexes.
    fn postfix(&mut self, base: ExprId, start: Span) -> Outcome<ExprId> {
        let mut base = base;
        loop {
            match self.peek() {
                Kind::Period => {
                    self.advance();
                    let member = self.ident()?;
                    let span = self.since(start);
                    base = self.node(ExprKind::Member(base, member), span)?;
                }
                Kind::BracketLeft => {
                    self.advance();
                    let index = self.expression()?;
                    self.expect(Kind::BracketRight, "`]`")?;
                    let span = self.since(start);
                    base = self.node(ExprKind::Index(base, index), span)?;
                }
                _ => return Ok(base),
            }
        }
    }
}

/// The run of a pool's items from `start` to `end`.
fn run_from(start: usize, end: usize) -> Outcome<Run> {
    let start32 = u32::try_from(start).map_err(|_| Stop::OutOfMemory)?;
    let len = u32::try_from(end.saturating_sub(start)).map_err(|_| Stop::OutOfMemory)?;
    Ok(Run {
        start: start32,
        len,
    })
}

/// Move `scratch`'s items from `mark` into `pool` as one run.
fn close<T: Copy>(pool: &mut Vec<T>, scratch: &mut Vec<T>, mark: usize) -> Outcome<Run> {
    let items = scratch.get(mark..).unwrap_or(&[]);
    let start = pool.len();
    reserve(pool, items.len())?;
    pool.extend_from_slice(items);
    scratch.truncate(mark);
    run_from(start, pool.len())
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
