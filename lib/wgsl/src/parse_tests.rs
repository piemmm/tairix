//! The parser held to the grammar: precedence as it forms it and the mixing
//! it refuses, each statement and declaration, directives and attributes,
//! and every bound.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::parse;
use crate::diag::{Code, Stop};
use crate::lex::tokens;
use crate::syntax::{
    AttrArgs, BinaryOp, Clause, DeclKind, DirectiveKind, ExprId, ExprKind, Literal, StmtId,
    StmtKind, Tree, UnaryOp,
};
use crate::{MAX_BRACE_NESTING, MAX_CASE_SELECTORS, MAX_PARAMETERS, MAX_STRUCT_MEMBERS};

fn tree(text: &str) -> Tree {
    parse(text, &tokens(text).expect("lexed")).expect("parsed")
}

fn refused(text: &str) -> Code {
    match tokens(text).and_then(|t| parse(text, &t)) {
        Err(Stop::Error(code, ..)) => code,
        other => panic!("{text:?}: {other:?}"),
    }
}

fn spell(text: &str, span: crate::diag::Span) -> &str {
    &text[span.start as usize..span.end as usize]
}

/// `id` as an s-expression.
fn sexpr(tree: &Tree, text: &str, id: ExprId) -> String {
    let expr = tree.expr(id).expect("an expression");
    match expr.kind {
        ExprKind::Literal(Literal::Bool(value)) => format!("{value}"),
        ExprKind::Literal(_) => String::from(spell(text, expr.span)),
        ExprKind::Ident(ident) => {
            let name = spell(text, ident.name);
            if ident.template.is_empty() {
                String::from(name)
            } else {
                let args: Vec<String> = tree
                    .expr_list(ident.template)
                    .iter()
                    .map(|&a| sexpr(tree, text, a))
                    .collect();
                format!("{name}<{}>", args.join(" "))
            }
        }
        ExprKind::Call(ident, args) => {
            let callee = if ident.template.is_empty() {
                String::from(spell(text, ident.name))
            } else {
                let template: Vec<String> = tree
                    .expr_list(ident.template)
                    .iter()
                    .map(|&a| sexpr(tree, text, a))
                    .collect();
                format!("{}<{}>", spell(text, ident.name), template.join(" "))
            };
            let args: Vec<String> = tree
                .expr_list(args)
                .iter()
                .map(|&a| sexpr(tree, text, a))
                .collect();
            format!("(call {callee} {})", args.join(" "))
        }
        ExprKind::Unary(op, operand) => {
            let op = match op {
                UnaryOp::Negate => "-",
                UnaryOp::Not => "!",
                UnaryOp::Complement => "~",
                UnaryOp::AddressOf => "&",
                UnaryOp::Indirection => "*",
            };
            format!("({op} {})", sexpr(tree, text, operand))
        }
        ExprKind::Binary(op, lhs, rhs) => {
            let op = match op {
                BinaryOp::Add => "+",
                BinaryOp::Subtract => "-",
                BinaryOp::Multiply => "*",
                BinaryOp::Divide => "/",
                BinaryOp::Remainder => "%",
                BinaryOp::ShiftLeft => "<<",
                BinaryOp::ShiftRight => ">>",
                BinaryOp::And => "&",
                BinaryOp::Or => "|",
                BinaryOp::Xor => "^",
                BinaryOp::LogicalAnd => "&&",
                BinaryOp::LogicalOr => "||",
                BinaryOp::Less => "<",
                BinaryOp::LessEqual => "<=",
                BinaryOp::Greater => ">",
                BinaryOp::GreaterEqual => ">=",
                BinaryOp::Equal => "==",
                BinaryOp::NotEqual => "!=",
            };
            format!(
                "({op} {} {})",
                sexpr(tree, text, lhs),
                sexpr(tree, text, rhs)
            )
        }
        ExprKind::Member(base, member) => {
            format!("(. {} {})", sexpr(tree, text, base), spell(text, member))
        }
        ExprKind::Index(base, index) => format!(
            "([] {} {})",
            sexpr(tree, text, base),
            sexpr(tree, text, index)
        ),
    }
}

/// The initializer of `const x = <expression>;` as an s-expression.
fn expression(source: &str) -> String {
    let text = format!("const x = {source};");
    let tree = tree(&text);
    let DeclKind::Const(value) = tree.decls[0].kind else {
        panic!("a const");
    };
    sexpr(&tree, &text, value.init)
}

/// What `const x = <expression>;` is refused with.
fn refused_expression(source: &str) -> Code {
    refused(&format!("const x = {source};"))
}

#[test]
fn operators_bind_as_the_grammar_forms_them() {
    let cases = [
        ("a + b * c - d", "(- (+ a (* b c)) d)"),
        ("a * b / c % d", "(% (/ (* a b) c) d)"),
        ("a << b", "(<< a b)"),
        ("a + b < c * d", "(< (+ a b) (* c d))"),
        ("a < b && c > d && e", "(&& (&& (< a b) (> c d)) e)"),
        ("a || b || c", "(|| (|| a b) c)"),
        ("a & b & c", "(& (& a b) c)"),
        ("a ^ b", "(^ a b)"),
        ("-a.b[c]", "(- ([] (. a b) c))"),
        ("!~x", "(! (~ x))"),
        ("*&p", "(* (& p))"),
        ("(a + b) * c", "(* (+ a b) c)"),
        ("vec3<f32>(1.0, 2, true)", "(call vec3<f32> 1.0 2 true)"),
        ("array<vec4<f32>, 2>()", "(call array<vec4<f32> 2> )"),
        ("f(g(x),)", "(call f (call g x))"),
        ("a[b < c]", "([] a (< b c))"),
    ];
    for (source, expected) in cases {
        assert_eq!(expression(source), expected, "{source}");
    }
}

#[test]
fn mixing_operators_the_grammar_keeps_apart_is_refused() {
    for source in [
        "a & b | c",
        "a < b < c",
        "a && b || c",
        "a << b << c",
        "a << b + c",
        "a + b << c",
        "a & b == c",
        "--a",
    ] {
        assert_eq!(refused_expression(source), Code::Syntax, "{source}");
    }
}

#[test]
fn a_long_chain_of_operators_or_accesses_is_read_without_recursing() {
    let sum = alloc::vec!["a"; 20_000].join(" + ");
    let text = format!("const x = {sum};");
    tree(&text);
    let indexes = "[0]".repeat(20_000);
    tree(&format!("const x = a{indexes};"));
}

#[test]
fn expressions_nest_to_the_bound_and_no_further() {
    let nested = |depth: usize| format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
    tree(&format!("const x = {};", nested(200)));
    assert_eq!(refused_expression(&nested(300)), Code::NestingTooDeep);
    let negated = "- ".repeat(300);
    assert_eq!(
        refused_expression(&format!("{negated}a")),
        Code::NestingTooDeep
    );
    let types = |depth: usize| format!("{}f32{}", "array<".repeat(depth), ">".repeat(depth));
    tree(&format!("alias t = {};", types(100)));
    assert_eq!(
        refused(&format!("alias t = {};", types(300))),
        Code::NestingTooDeep
    );
}

/// The statements of `fn f() { <body> }`.
fn body(text: &str) -> (Tree, Vec<StmtId>) {
    let tree = tree(text);
    let DeclKind::Fn(function) = tree.decls[0].kind else {
        panic!("a function");
    };
    let StmtKind::Compound(run) = tree.stmt(function.body).expect("a body").kind else {
        panic!("a compound body");
    };
    let statements = tree.stmt_list(run).to_vec();
    (tree, statements)
}

/// The word naming a statement's kind.
fn kind_word(kind: &StmtKind) -> &'static str {
    match kind {
        StmtKind::Let(_) => "let",
        StmtKind::Const(_) => "const",
        StmtKind::Var(_) => "var",
        StmtKind::Assign { op: None, .. } => "=",
        StmtKind::Assign { op: Some(_), .. } => "op=",
        StmtKind::Increment(_) => "++",
        StmtKind::Decrement(_) => "--",
        StmtKind::Phony(_) => "_",
        StmtKind::Call(_) => "call",
        StmtKind::If { .. } => "if",
        StmtKind::Switch { .. } => "switch",
        StmtKind::Loop { .. } => "loop",
        StmtKind::For { .. } => "for",
        StmtKind::While { .. } => "while",
        StmtKind::Compound(_) => "{}",
        StmtKind::ConstAssert(_) => "const_assert",
        StmtKind::Discard => "discard",
        StmtKind::Return(_) => "return",
        StmtKind::Break => "break",
        StmtKind::Continue => "continue",
        StmtKind::Empty => ";",
    }
}

#[test]
fn every_statement_reads_as_its_kind() {
    let text = "fn f() {
        let a = 1;
        const b = 2;
        var c: i32;
        var<function> d = 3;
        c = a;
        c += b;
        c >>= 1;
        c++;
        d--;
        _ = c;
        g(c);
        h<i32>(c);
        if a < b { } else if a > b { } else { }
        switch c { case 1, 2, { } case default: { } }
        loop { continue; continuing { break if c > 3; } }
        for (var i = 0; i < 4; i++) { break; }
        for (;;) { }
        while c < 9 { }
        { ; }
        const_assert true;
        discard;
        return;
    }";
    let (tree, statements) = body(text);
    let kinds: Vec<&str> = statements
        .iter()
        .map(|&s| kind_word(&tree.stmt(s).expect("a statement").kind))
        .collect();
    assert_eq!(
        kinds,
        [
            "let",
            "const",
            "var",
            "var",
            "=",
            "op=",
            "op=",
            "++",
            "--",
            "_",
            "call",
            "call",
            "if",
            "switch",
            "loop",
            "for",
            "for",
            "while",
            "{}",
            "const_assert",
            "discard",
            "return"
        ]
    );
    let StmtKind::If { reject, .. } = tree.stmt(statements[12]).expect("if").kind else {
        panic!("if");
    };
    let chained = tree.stmt(reject.expect("an else")).expect("else if");
    assert!(
        matches!(
            chained.kind,
            StmtKind::If {
                reject: Some(_),
                ..
            }
        ),
        "else if then else"
    );
    let StmtKind::Switch { clauses, .. } = tree.stmt(statements[13]).expect("switch").kind else {
        panic!("switch");
    };
    let clauses = tree.clause_list(clauses);
    assert_eq!(clauses.len(), 2);
    assert_eq!(tree.selector_list(clauses[0].selectors).len(), 2, "1, 2");
    assert!(
        tree.selector_list(clauses[1].selectors)[0].expr.is_none(),
        "default"
    );
    let StmtKind::Loop { continuing, .. } = tree.stmt(statements[14]).expect("loop").kind else {
        panic!("loop");
    };
    let continuing = tree.continuings[continuing.expect("continuing") as usize];
    assert!(continuing.break_if.is_some());
}

#[test]
fn a_switch_within_a_clause_keeps_its_selectors_apart() {
    let text =
        "fn f() { switch c { case 1: { switch d { case 2, 3: { } default: { } } } default: { } } }";
    let (tree, statements) = body(text);
    let clauses = |id: StmtId| {
        let StmtKind::Switch { clauses, .. } = tree.stmt(id).expect("a switch").kind else {
            panic!("a switch");
        };
        tree.clause_list(clauses)
    };
    let spelled = |clauses: &[Clause]| -> Vec<Vec<&str>> {
        clauses
            .iter()
            .map(|clause| {
                tree.selector_list(clause.selectors)
                    .iter()
                    .map(|selector| spell(text, selector.span))
                    .collect()
            })
            .collect()
    };
    let outer = clauses(statements[0]);
    assert_eq!(spelled(outer), [alloc::vec!["1"], alloc::vec!["default"]]);
    let StmtKind::Compound(run) = tree.stmt(outer[0].body).expect("a body").kind else {
        panic!("a compound body");
    };
    let inner = clauses(tree.stmt_list(run)[0]);
    assert_eq!(
        spelled(inner),
        [alloc::vec!["2", "3"], alloc::vec!["default"]]
    );
}

#[test]
fn a_long_else_if_chain_is_linked_without_recursing() {
    let chain = "if a { } else ".repeat(20_000);
    let (tree, statements) = body(&format!("fn f() {{ {chain} {{ }} }}"));
    let mut next = statements.first().copied();
    let mut arms = 0;
    while let Some(id) = next {
        match tree.stmt(id).expect("a statement").kind {
            StmtKind::If { reject, .. } => {
                arms += 1;
                next = reject;
            }
            _ => break,
        }
    }
    assert_eq!(arms, 20_000);
}

#[test]
fn statements_nest_to_the_bound_and_no_further() {
    let nested = |depth: u32| {
        let depth = depth as usize;
        format!("fn f() {}{}", "{".repeat(depth), "}".repeat(depth))
    };
    tree(&nested(MAX_BRACE_NESTING));
    assert_eq!(
        refused(&nested(MAX_BRACE_NESTING + 1)),
        Code::NestingTooDeep
    );
}

#[test]
fn lists_hold_to_their_bounds() {
    let params = |count: usize| {
        let list: Vec<String> = (0..count).map(|i| format!("p{i}: i32")).collect();
        format!("fn f({}) {{ }}", list.join(", "))
    };
    tree(&params(MAX_PARAMETERS));
    assert_eq!(refused(&params(MAX_PARAMETERS + 1)), Code::TooMany);
    let members = |count: usize| {
        let list: Vec<String> = (0..count).map(|i| format!("m{i}: i32")).collect();
        format!("struct s {{ {} }}", list.join(", "))
    };
    tree(&members(MAX_STRUCT_MEMBERS));
    assert_eq!(refused(&members(MAX_STRUCT_MEMBERS + 1)), Code::TooMany);
    let selectors = |count: usize| {
        let list: Vec<String> = (0..count).map(|i| format!("{i}")).collect();
        format!(
            "fn f() {{ switch x {{ case {} {{ }} default {{ }} }} }}",
            list.join(", ")
        )
    };
    tree(&selectors(MAX_CASE_SELECTORS - 1));
    assert_eq!(
        refused(&selectors(MAX_CASE_SELECTORS)),
        Code::TooMany,
        "default counts"
    );
}

#[test]
fn declarations_directives_and_attributes_read_as_written() {
    let text = "enable f16;
        requires readonly_and_readwrite_storage_textures, pointer_composite_access,;
        diagnostic(off, derivative_uniformity);
        alias v = vec4<f32>;
        struct s { @align(16) a: f32, @size(8) b: i32, }
        @group(0) @binding(1) var<storage, read_write> buf: array<u32>;
        @id(3) override o: u32 = 4;
        const c = 1;
        const_assert c == 1;
        @vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
            return vec4<f32>();
        }
        @diagnostic(warning, chromium.unreachable_code) fn g() { }";
    let tree = tree(text);
    assert_eq!(tree.directives.len(), 3);
    let DirectiveKind::Requires(names) = tree.directives[1].kind else {
        panic!("requires");
    };
    assert_eq!(tree.name_list(names).len(), 2, "a trailing comma");
    let kinds: Vec<&str> = tree
        .decls
        .iter()
        .map(|d| match d.kind {
            DeclKind::Alias { .. } => "alias",
            DeclKind::Struct { .. } => "struct",
            DeclKind::Var(_) => "var",
            DeclKind::Override(_) => "override",
            DeclKind::Const(_) => "const",
            DeclKind::ConstAssert(_) => "const_assert",
            DeclKind::Fn(_) => "fn",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "alias",
            "struct",
            "var",
            "override",
            "const",
            "const_assert",
            "fn",
            "fn"
        ]
    );
    let DeclKind::Fn(main) = tree.decls[6].kind else {
        panic!("main");
    };
    assert_eq!(tree.attr_list(tree.decls[6].attrs).len(), 1, "@vertex");
    assert_eq!(tree.field_list(main.params).len(), 1);
    assert!(main.result.is_some_and(|(attrs, _)| attrs.len == 1));
    let diagnostic = tree.attr_list(tree.decls[7].attrs)[0];
    let AttrArgs::Diagnostic(control) = diagnostic.args else {
        panic!("a diagnostic control");
    };
    assert_eq!(spell(text, control.severity), "warning");
    assert_eq!(spell(text, control.rule.0), "chromium");
    assert_eq!(
        control.rule.1.map(|s| spell(text, s)),
        Some("unreachable_code")
    );
}

#[test]
fn what_the_grammar_does_not_admit_is_refused() {
    let cases: [(&str, Code); 12] = [
        ("const x = 1; enable f16;", Code::Directive),
        ("fn f() { break if true; }", Code::Syntax),
        ("@vertex const x = 1;", Code::Syntax),
        ("struct s { }", Code::Syntax),
        ("fn f() { @diagnostic(off, x) let a = 1; }", Code::Syntax),
        ("var asm: i32;", Code::ReservedWord),
        ("fn f() { x = ; }", Code::Syntax),
        ("fn f() { a b; }", Code::Syntax),
        ("fn f() { for (; ; var i = 0) { } }", Code::Syntax),
        (
            "fn f() { loop { continuing { break if a; x = 1; } } }",
            Code::Syntax,
        ),
        ("fn f() { return 1 }", Code::Syntax),
        ("const x = 1", Code::Syntax),
    ];
    for (text, code) in cases {
        assert_eq!(refused(text), code, "{text}");
    }
}

/// The descendants of `id`, itself included, found by walking down.
fn descendants(tree: &Tree, id: ExprId, out: &mut Vec<u32>) {
    out.push(id.0);
    tree.children(id, |child| descendants(tree, child, out));
}

#[test]
fn each_subtree_is_the_run_of_nodes_from_its_first_descendant_to_itself() {
    let text = "fn f() {
        let a = vec3<f32>(1.0, g(h(2), i[3].x), -j * k + l[m[n]]);
        let b = array<array<f32, 2 + 2>, 4>();
        x[y.z].w = (p && q) || !r;
    }";
    let tree = tree(text);
    let shape = tree.shape().expect("shaped");
    for index in 0..tree.exprs.len() {
        let id = ExprId(u32::try_from(index).expect("small"));
        let mut found = Vec::new();
        descendants(&tree, id, &mut found);
        found.sort_unstable();
        let run: Vec<u32> = shape.subtree(id).collect();
        assert_eq!(found, run, "node {index}");
        tree.children(id, |child| assert_eq!(shape.parent(child), Some(id)));
    }
}
