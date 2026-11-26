//! What a WGSL compile holds at its peak in the worker, at the largest
//! sources of every shape that costs memory: the front end's own bounds keep
//! it, with the request it was handed, within what a message-sized worker
//! allows beside its baseline (`plans/SHADER.md` SH4).

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_log::DiscardSink;
use tairix_sandbox::host::ParserSandbox;
use tairix_sandbox::loopback::LoopbackLauncher;
use tairix_sandbox::proto::MAX_FRAME;
use tairix_sandbox::wgsl::{compile, CompileFailure, WgslService, WORKER_MEMORY};
use tairix_sandbox::worker::Service;
use tairix_sandbox::WorkerMemory;
use tairix_wgsl::{Code, MAX_BRACE_NESTING, MAX_PARAMETERS, MAX_SOURCE_BYTES};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// Serves as [`WgslService`] does, keeping the most it held in one request:
/// the request beside the compile's peak, its reply among it.
struct Measured(Rc<Cell<usize>>);

impl Service for Measured {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let (reply, metering) = metered(|| WgslService.handle(request));
        self.0.set(self.0.get().max(request.len() + metering.peak));
        reply
    }
}

/// What the compile worker's memory allows beside its baseline: a request
/// frame and a reply frame.
const ALLOWANCE: usize = 2 * MAX_FRAME;

/// Sources of nearly the largest length, each of a shape that costs the
/// front end most in one of its stages.
fn largest() -> Vec<(&'static str, String)> {
    let head = "@group(0) @binding(0) var<storage, read_write> out: array<u32>;\n";
    let room = MAX_SOURCE_BYTES - 512;
    let mut shapes = Vec::new();
    let mut statements =
        format!("{head}@compute @workgroup_size(1) fn main() {{\nvar x = out[0];\n");
    while statements.len() < room {
        statements.push_str("x = x * 3u + out[x % 7u];\n");
    }
    statements.push_str("out[1] = x;\n}\n");
    shapes.push(("statements", statements));
    let mut functions = String::from(head);
    let mut count = 0;
    while functions.len() < room {
        let _ = writeln!(
            functions,
            "fn f{count}(a: u32) -> u32 {{ return a * {count}u + out[a % 3u]; }}"
        );
        count += 1;
    }
    let _ = writeln!(
        functions,
        "@compute @workgroup_size(1) fn main() {{ out[0] = f0(1u) + f{}(2u); }}",
        count - 1
    );
    shapes.push(("functions", functions));
    let mut constant = format!("{head}const c = 1");
    while constant.len() < room {
        constant.push_str(" + 1");
    }
    constant.push_str(";\n@compute @workgroup_size(1) fn main() { out[0] = u32(c); }\n");
    shapes.push(("a long constant", constant));
    let mut expression =
        format!("{head}@compute @workgroup_size(1) fn main() {{\nlet a = out[0];\nout[1] = a");
    while expression.len() < room {
        expression.push_str(" ^ a");
    }
    expression.push_str(";\n}\n");
    shapes.push(("a long expression", expression));
    let mut arrays = String::from(head);
    let element = "1, ".repeat(2046);
    let mut index = 0;
    while arrays.len() + element.len() + 64 < room {
        let _ = writeln!(arrays, "const a{index} = array({element}1);");
        index += 1;
    }
    let _ = writeln!(
        arrays,
        "@compute @workgroup_size(1) fn main() {{ out[0] = u32(a0[out[1]] + a{}[2]); }}",
        index - 1
    );
    shapes.push(("constant arrays", arrays));
    let mut nested = String::from(head);
    nested.push_str("@compute @workgroup_size(1) fn main() {\nvar x = out[0];\n");
    while nested.len() < room {
        nested.push_str(
            "if x > 3u { loop { x = x / 2u; if x < 5u { break; } } } else { x = x + 1u; }\n",
        );
    }
    nested.push_str("out[1] = x;\n}\n");
    shapes.push(("control flow", nested));
    shapes.extend(joins(head, room));
    shapes.extend(dense(head, room));
    shapes
}

/// Sources of nearly the largest length spelled as densely as the grammar
/// allows, which the length alone does not hold — an operator every two
/// bytes, a statement every byte or six — and loops and selections nested to
/// the brace bound, each level joining every variable.
fn dense(head: &str, room: usize) -> Vec<(&'static str, String)> {
    let fill = |mut text: String, unit: &str, end: &str| {
        while text.len() + end.len() < room {
            text.push_str(unit);
        }
        text.push_str(end);
        text
    };
    let main = format!("{head}@compute @workgroup_size(1) fn main() {{\nvar x = out[0];\n");
    let operators = fill(format!("{main}out[1] = x"), "^x", ";\n}\n");
    let empty = fill(main.clone(), ";", "\n}\n");
    let statements = fill(main, "x=x^x;", "\nout[1] = x;\n}\n");
    let nest = |open: &str, depth: usize| {
        let mut text = format!("{head}@compute @workgroup_size(1) fn main() {{\nlet v = out[0];\n");
        for var in 0..VARS {
            let _ = writeln!(text, "var a{var} = 0u;");
        }
        text.push_str(&open.repeat(depth));
        for var in 0..VARS {
            let _ = writeln!(text, "a{var} = v;");
        }
        text.push_str(&"}\n".repeat(depth));
        text.push_str("}\n");
        text
    };
    // Within the function's own braces, and a loop's `break` within its own.
    let braces = MAX_BRACE_NESTING as usize - 1;
    Vec::from([
        ("an operator every two bytes", operators),
        ("a statement every byte", empty),
        ("a statement every six bytes", statements),
        (
            "loops nested over every variable",
            nest("loop { if v == 0u { break; }\n", braces - 1),
        ),
        (
            "selections nested over every variable",
            nest("if v == 0u {\n", braces),
        ),
    ])
}

/// The variables the shapes whose joins could multiply them assign: near
/// the most a function's memory holds.
const VARS: usize = 2000;

/// Sources of nearly the largest length whose every exit or branch could
/// join every variable: a chain of branches, switches of clauses, a loop of
/// exits, a function of returns past every pointer it may take, and calls
/// of a function whose every pointer takes every later one's contents.
fn joins(head: &str, room: usize) -> Vec<(&'static str, String)> {
    let mut declared = format!("{head}@compute @workgroup_size(1) fn main() {{\nlet v = out[0];\n");
    for var in 0..VARS {
        let _ = writeln!(declared, "var a{var} = 0u;");
    }
    let mut chain = declared.clone();
    let mut link = 0;
    while chain.len() < room {
        let _ = write!(chain, "if v == {link}u {{ a{} = v; }} else ", link % VARS);
        link += 1;
    }
    chain.push_str("{ }\n}\n");
    let mut switch = String::from("switch v {\n");
    for clause in 0..VARS / 2 {
        let _ = writeln!(
            switch,
            "case {clause}u: {{ a{} = v; a{} = v; }}",
            2 * clause,
            2 * clause + 1
        );
    }
    switch.push_str("default: { }\n}\n");
    let mut switches = declared.clone();
    while switches.len() + switch.len() < room {
        switches.push_str(&switch);
    }
    switches.push_str("}\n");
    let mut exits = declared;
    exits.push_str("loop {\n");
    for var in 0..VARS {
        let _ = writeln!(exits, "a{var} = v;");
    }
    while exits.len() < room {
        exits.push_str("if v == 0u { break; }\nif v == 1u { continue; }\n");
    }
    exits.push_str("}\n}\n");
    // Every pointer parameter a function may take, beside one value.
    let pointers = MAX_PARAMETERS - 1;
    let mut call = String::from("@compute @workgroup_size(1) fn main() {\n");
    let mut args = String::new();
    let mut returns = format!("{head}fn f(");
    for p in 0..pointers {
        let _ = write!(returns, "p{p}: ptr<function, u32>, ");
        let _ = writeln!(call, "var b{p} = 0u;");
        let _ = write!(args, "&b{p}, ");
    }
    let _ = writeln!(call, "f({args}out[0]);\n}}");
    returns.push_str("c: u32) {\n");
    while returns.len() + call.len() < room {
        returns.push_str("if c == 0u { return; }\n");
    }
    returns.push_str("}\n");
    returns.push_str(&call);
    let mut params = String::new();
    let mut args = String::new();
    let mut fed = format!("{head}fn g(");
    let mut vars = String::new();
    for p in 0..MAX_PARAMETERS {
        let _ = write!(params, "p{p}: ptr<function, u32>, ");
        let _ = writeln!(vars, "var b{p} = out[{p}];");
        let _ = write!(args, "&b{p}, ");
    }
    let _ = writeln!(fed, "{params}) {{");
    for p in (0..MAX_PARAMETERS - 1).rev() {
        let _ = writeln!(fed, "*p{p} = *p{p} + *p{};", p + 1);
    }
    let _ = writeln!(fed, "}}\n@compute @workgroup_size(1) fn main() {{\n{vars}");
    let call = format!("g({args});\n");
    while fed.len() + call.len() < room {
        fed.push_str(&call);
    }
    fed.push_str("}\n");
    Vec::from([
        ("a chain of branches", chain),
        ("switches of clauses", switches),
        ("a loop of exits", exits),
        ("a function of returns", returns),
        ("calls of pointers fed by one another", fed),
    ])
}

#[test]
fn a_compile_at_the_largest_source_stays_within_a_message_worker() {
    assert_eq!(
        WORKER_MEMORY,
        WorkerMemory::Messages,
        "the allowance is a message worker's"
    );
    for (shape, source) in largest() {
        assert!(
            source.len() <= MAX_SOURCE_BYTES,
            "{shape} is a source the service takes"
        );
        let held = Rc::new(Cell::new(0));
        let worker = Rc::clone(&held);
        let mut sandbox = ParserSandbox::new(
            LoopbackLauncher::new(move || Measured(Rc::clone(&worker))),
            DiscardSink,
        );
        match compile(&mut sandbox, &source, &[]) {
            Ok(_) => {}
            Err(CompileFailure::Source(error)) => {
                assert_eq!(
                    error.code,
                    Code::ModuleTooLarge,
                    "{shape}: {}",
                    error.message
                );
            }
            Err(other) => panic!("{shape}: {other:?}"),
        }
        assert!(
            held.get() <= ALLOWANCE,
            "{shape} held {} KiB in the worker, past {} KiB",
            held.get() >> 10,
            ALLOWANCE >> 10
        );
    }
}
