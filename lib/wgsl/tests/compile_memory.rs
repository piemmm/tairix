//! What compiling holds at its peak where a program's shape could multiply
//! it — a chain of branches, a switch of clauses, a loop of exits, a
//! function of returns and calls of pointers fed by one another, each over
//! as many variables or pointers as a function holds — held to twice what
//! the same statements hold without that shape.

use std::fmt::Write as _;

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_wgsl::{compile, Code, Failure, Options, MAX_PARAMETERS, MAX_SYNTAX_NODES};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// The variables each entry point declares, near the most its function
/// memory holds.
const VARS: usize = 2000;

const BINDING: &str = "@group(0) @binding(0) var<storage, read_write> out: array<u32>;\n";

/// The most compiling `source` holds, which it must compile.
fn peak(source: &str) -> usize {
    let (compiled, metering) = metered(|| compile(source, &Options::default()));
    if let Err(failure) = compiled {
        panic!("{failure:?}");
    }
    metering.peak
}

/// `shaped` holds at most twice what `plain` does.
fn held_alike(shaped: &str, plain: &str) {
    let (shaped, plain) = (peak(shaped), peak(plain));
    assert!(
        shaped <= 2 * plain,
        "{shaped} bytes held, against {plain} without the shape"
    );
}

/// An entry point declaring the variables, then running `body`.
fn entry(body: &str) -> String {
    let mut text = format!("{BINDING}@compute @workgroup_size(1) fn main() {{\nlet v = out[0];\n");
    for var in 0..VARS {
        let _ = writeln!(text, "var a{var} = 0u;");
    }
    text.push_str(body);
    text.push_str("}\n");
    text
}

/// The assignments of the `k`th branch: two variables of its own.
fn pair(k: usize) -> String {
    format!("a{} = 1u; a{} = v;", 2 * k, 2 * k + 1)
}

#[test]
fn a_chain_of_branches_holds_what_its_branches_assign() {
    const LINKS: usize = VARS / 2;
    let mut chained = String::new();
    let mut separate = String::new();
    for k in 0..LINKS {
        let _ = write!(chained, "if v == {k}u {{ {} }} else ", pair(k));
        let _ = writeln!(separate, "if v == {k}u {{ {} }}", pair(k));
    }
    chained.push_str("{ }\n");
    held_alike(&entry(&chained), &entry(&separate));
}

#[test]
fn a_switch_of_clauses_holds_what_its_clauses_assign() {
    const CLAUSES: usize = VARS / 2;
    let mut switch = String::from("switch v {\n");
    let mut separate = String::new();
    for k in 0..CLAUSES {
        let _ = writeln!(switch, "case {k}u: {{ {} }}", pair(k));
        let _ = writeln!(separate, "if v == {k}u {{ {} }}", pair(k));
    }
    switch.push_str("default: { }\n}\n");
    held_alike(&entry(&switch), &entry(&separate));
}

#[test]
fn a_loop_of_exits_holds_what_its_assignments_do() {
    const EXITS: usize = 4000;
    let mut assigned = String::new();
    for var in 0..VARS {
        let _ = writeln!(assigned, "a{var} = v;");
    }
    let exits = "if v == 0u { break; }\nif v == 1u { continue; }\n".repeat(EXITS / 2);
    let plain = "if v == 0u { out[1] = v; }\n".repeat(EXITS - 1) + "if v == 0u { break; }\n";
    held_alike(
        &entry(&format!("loop {{\n{assigned}{exits}}}\n")),
        &entry(&format!("loop {{\n{assigned}{plain}}}\n")),
    );
}

#[test]
fn a_function_of_returns_holds_what_its_statements_do() {
    // Every pointer parameter a function may take, beside one value.
    const POINTERS: usize = MAX_PARAMETERS - 1;
    const RETURNS: usize = 4000;
    let program = |body: &str| {
        let mut params = String::new();
        let mut vars = String::new();
        let mut args = String::new();
        for p in 0..POINTERS {
            let _ = write!(params, "p{p}: ptr<function, u32>, ");
            let _ = writeln!(vars, "var b{p} = 0u;");
            let _ = write!(args, "&b{p}, ");
        }
        format!(
            "{BINDING}fn f({params}c: u32) {{\n{body}}}\n\
             @compute @workgroup_size(1) fn main() {{\n{vars}f({args}out[0]);\n}}\n"
        )
    };
    held_alike(
        &program(&"if c == 0u { return; }\n".repeat(RETURNS)),
        &program(&"if c == 0u { out[1] = c; }\n".repeat(RETURNS)),
    );
}

#[test]
fn calls_of_pointers_fed_by_one_another_hold_what_their_arguments_do() {
    const CALLS: usize = 100;
    // Each pointer adds the next's contents to its own: last first, each
    // pointer takes every later one's; first first, only its neighbour's.
    let program = |last_first: bool| {
        let mut params = String::new();
        let mut vars = String::new();
        let mut args = String::new();
        for p in 0..MAX_PARAMETERS {
            let _ = write!(params, "p{p}: ptr<function, u32>, ");
            let _ = writeln!(vars, "var b{p} = out[{p}];");
            let _ = write!(args, "&b{p}, ");
        }
        let mut text = format!("{BINDING}fn g({params}) {{\n");
        let mut order: Vec<usize> = (0..MAX_PARAMETERS - 1).collect();
        if last_first {
            order.reverse();
        }
        for p in order {
            let _ = writeln!(text, "*p{p} = *p{p} + *p{};", p + 1);
        }
        let _ = writeln!(text, "}}\n@compute @workgroup_size(1) fn main() {{\n{vars}");
        for _ in 0..CALLS {
            let _ = writeln!(text, "g({args});");
        }
        text.push_str("}\n");
        text
    };
    held_alike(&program(true), &program(false));
}

/// The code and message `source` is refused with.
fn refusal(source: &str) -> (Code, String) {
    match compile(source, &Options::default()) {
        Err(Failure::Error(diagnostic)) => (diagnostic.code, diagnostic.message),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_module_past_its_expressions_and_statements_is_refused() {
    // The workgroup size is an expression and the body a statement; each `;`
    // is another.
    let module = |empty: usize| {
        format!(
            "@compute @workgroup_size(1) fn main() {{ {} }}",
            ";".repeat(empty)
        )
    };
    let fits = MAX_SYNTAX_NODES - 2;
    assert!(compile(&module(fits), &Options::default()).is_ok());
    let (code, message) = refusal(&module(fits + 1));
    assert_eq!(code, Code::ModuleTooLarge, "{message}");
}

#[test]
fn a_function_whose_analysis_passes_its_bound_is_refused() {
    // Each loop joins every variable assigned within it.
    let nest = |depth: usize| {
        let mut body = String::new();
        for _ in 0..depth {
            body.push_str("loop { if v == 0u { break; }\n");
        }
        for var in 0..VARS {
            let _ = writeln!(body, "a{var} = v;");
        }
        body.push_str(&"}\n".repeat(depth));
        entry(&body)
    };
    assert!(compile(&nest(16), &Options::default()).is_ok());
    let source = nest(125);
    let (code, message) = refusal(&source);
    assert_eq!(code, Code::ModuleTooLarge, "{message}");
    assert!(message.contains("uniformity"), "{message}");
}
