//! Deterministic fuzz harness for the WGSL front end.
//!
//! Source is untrusted input — an application supplies it, and it may have
//! crossed a network — so each round mutates a corpus of valid modules:
//! bytes changed, spans deleted, duplicated and spliced from another module,
//! tokens of the language inserted, nesting deepened. The invariants are:
//!
//! 1. Nothing panics, whatever the source.
//! 2. A refusal is a shader-creation error whose span lies within the source,
//!    beginning on the line and at the column it reports — never an internal
//!    invariant's failure, and never memory refused.
//! 3. A compiled module is admitted by the validator again, and guards into a
//!    module it admits and the guard's independent check confirms.
//! 4. A compile is deterministic: one source and one set of override values
//!    write one sequence of bytes and one set of diagnostics.
//! 5. A random program over WGSL's integer arithmetic — the cases SPIR-V
//!    leaves undefined among them, under nested selections and loops — is
//!    compiled, admitted and run on the software device to exactly the values
//!    a reference evaluation of WGSL's definitions gives.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

mod support;

use std::fmt;
use std::fmt::Write as _;

use support::{admitted, with_bindings, Gpu};
use tairix_fuzzseed::Prng;
use tairix_spirv::{Admitted, GuardError, Module};
use tairix_wgsl::{compile, Code, Diagnostic, Failure, Options, OverrideKey, Severity};

/// Rounds per sweep of each property. Miri interprets every operation, so it
/// drives a handful.
const ROUNDS_PER_SWEEP: u32 = if cfg!(miri) { 2 } else { 60 };

/// Mutants compiled per round.
const MUTANTS_PER_ROUND: u32 = if cfg!(miri) { 4 } else { 30 };

/// Programs run on the software device per sweep.
const PROGRAMS_PER_SWEEP: u32 = if cfg!(miri) { 2 } else { 12 };

/// Valid modules the mutants are drawn from: a render pipeline, compute with
/// workgroup memory and atomics, pointers and overrides, textures, half
/// floats with a name outside ASCII, and control flow nested — a switch
/// within a switch, a chain whose conditions write through pointers, a
/// loop's exits and a function's early returns.
const CORPUS: [&str; 6] = [
    "
    struct Out {
        @builtin(position) position: vec4f,
        @location(0) colour: vec3f,
        @location(1) @interpolate(flat) id: u32,
    }
    struct Frame { scale: vec2f, tint: vec4f }
    @group(0) @binding(0) var<uniform> frame: Frame;
    @group(0) @binding(1) var picture: texture_2d<f32>;
    @group(0) @binding(2) var smooth_sampler: sampler;
    @vertex
    fn vs(@builtin(vertex_index) index: u32) -> Out {
        var corners = array(vec2f(0.0, 0.5), vec2f(-0.5, -0.5), vec2f(0.5, -0.5));
        var out: Out;
        out.position = vec4f(corners[index] * frame.scale, 0.0, 1.0);
        out.colour = vec3f(f32(index) / 2.0);
        out.id = index;
        return out;
    }
    @fragment
    fn fs(in: Out) -> @location(0) vec4f {
        if in.colour.x > 2.0 { discard; }
        let sampled = textureSample(picture, smooth_sampler, in.position.xy);
        return sampled * frame.tint + vec4f(in.colour, f32(in.id)) + vec4f(dpdx(in.position.x));
    }
    ",
    "
    var<workgroup> tile: array<f32, 64>;
    var<workgroup> total: atomic<u32>;
    @group(0) @binding(0) var<storage, read_write> out: array<f32>;
    @compute @workgroup_size(8, 8)
    fn main(@builtin(local_invocation_index) lane: u32, @builtin(global_invocation_id) id: vec3u) {
        tile[lane] = f32(lane) * 0.5;
        atomicAdd(&total, 1u);
        workgroupBarrier();
        var sum = 0.0;
        for (var i = 0u; i < 8u; i++) {
            if i == lane % 8u { continue; }
            sum += tile[(lane + i) % 64u];
        }
        out[id.x + id.y * 64u] = sum + f32(workgroupUniformLoad(&total));
    }
    ",
    "
    struct Item { weight: f32, tags: array<u32, 4> }
    struct Store { count: u32, items: array<Item> }
    @group(0) @binding(0) var<storage, read_write> store: Store;
    override width: u32 = 16;
    @id(3) override scale: f32 = 2.0;
    var<private> scratch: array<u32, 8>;
    fn bump(p: ptr<function, u32>) -> u32 { *p += 1u; return *p; }
    fn weigh(item: ptr<storage, Item, read_write>, by: f32) { (*item).weight *= by; item.tags[1] = 7u; }
    @compute @workgroup_size(width)
    fn main(@builtin(local_invocation_index) lane: u32) {
        var n = lane;
        let k = bump(&n);
        weigh(&store.items[lane], scale);
        scratch[lane % 8u] = k << (lane % 32u);
        switch lane % 4u {
            case 0u: { store.count = arrayLength(&store.items); }
            case 1u, 2u: { store.items[0].tags[3] = scratch[1] / n; }
            default: { store.items[1].weight = f32(i32(scale) % 3); }
        }
    }
    ",
    "
    @group(0) @binding(0) var colour: texture_2d<f32>;
    @group(0) @binding(1) var depth: texture_depth_2d;
    @group(0) @binding(2) var grid: texture_storage_2d<r32uint, read_write>;
    @group(0) @binding(3) var smooth_sampler: sampler;
    @group(0) @binding(4) var shadow: sampler_comparison;
    @fragment
    fn main(@location(0) uv: vec2f) -> @location(0) vec4f {
        var c = textureSample(colour, smooth_sampler, uv, vec2i(1, -1));
        c += textureGather(2, colour, smooth_sampler, uv);
        let d = textureSampleCompare(depth, shadow, uv, 0.5) + textureLoad(depth, vec2i(0), 0);
        let size = textureDimensions(colour, 0);
        textureStore(grid, vec2u(size), vec4u(u32(d)));
        return c + vec4f(f32(textureLoad(grid, vec2i(1)).x));
    }
    ",
    "
    enable f16;
    requires swizzle_assignment;
    const été = 3.5h;
    @group(0) @binding(0) var<storage, read_write> halves: array<vec4h>;
    @compute @workgroup_size(1)
    fn main() {
        var h = halves[0];
        h.xz = vec2h(été, -été);
        h.yw *= 2.0h;
        halves[1] = h + vec4h(f16(pack2x16float(vec2f(h.xy)) == 0u));
        halves[2] = vec4h(f16(countOneBits(bitcast<u32>(h.xy))));
    }
    ",
    "
    @group(0) @binding(0) var<storage, read_write> out: array<u32>;
    fn take(p: ptr<function, u32>, v: u32) -> bool { *p = v; return v > 2u; }
    fn settle(p: ptr<function, u32>, q: ptr<function, u32>, c: u32) {
        if c == 0u { *p = 1u; return; }
        if c == 1u { return; }
        *q = c;
    }
    @compute @workgroup_size(4)
    fn main(@builtin(local_invocation_index) lane: u32, @builtin(workgroup_id) group: vec3u) {
        var a = 0u;
        var b = lane;
        if group.x == 0u { a = 1u; } else if take(&b, group.y) { a = b; } else if take(&a, 3u) { } else { b = 2u; }
        loop {
            if a > 8u { break; }
            a += 1u;
            if a == 3u { continue; }
            switch a % 3u {
                case 0u: {
                    switch b {
                        case 0u: { b = 1u; }
                        default: { break; }
                    }
                }
                default: { settle(&a, &b, group.z); }
            }
            continuing { break if a > 6u; }
        }
        workgroupBarrier();
        out[lane] = a + b;
    }
    ",
];

/// Tokens of the language a mutation inserts, the boundaries of its
/// literals among them.
const TOKENS: [&str; 72] = [
    "fn",
    "let",
    "var",
    "const",
    "override",
    "struct",
    "if",
    "else",
    "loop",
    "for",
    "while",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "continuing",
    "return",
    "discard",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "<",
    ">",
    ";",
    ",",
    ".",
    ":",
    "=",
    "+=",
    "&&",
    "||",
    "!",
    "~",
    "&",
    "*",
    "->",
    "@group(0)",
    "@binding(1)",
    "@builtin(position)",
    "@location(0)",
    "@workgroup_size(64)",
    "@compute",
    "array<",
    "vec4f",
    "mat4x4f",
    "ptr<function,",
    "u32",
    "i32",
    "f32",
    "f16",
    "atomic<u32>",
    "texture_2d<f32>",
    "0x7fffffff",
    "4294967295u",
    "-2147483648",
    "1e38",
    "3.4e38f",
    "65504h",
    "1e-45",
    "true",
    "bitcast<u32>",
    "select",
    "textureSample",
    "dpdx",
    "workgroupBarrier()",
    "_",
    "__a",
    "diagnostic(off, derivative_uniformity)",
];

/// A token class a substitution keeps: what replaces a token is of its
/// class, so most substitutions leave the syntax whole and reach the
/// checker and the emitter.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Class {
    Name,
    Number,
    Operator,
}

/// The tokens of `source` a substitution may replace, as their spans and
/// classes.
fn tokens(source: &[u8]) -> Vec<(usize, usize, Class)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let byte = source[at];
        let start = at;
        let class = if byte.is_ascii_alphabetic() || byte == b'_' {
            while at < source.len() && (source[at].is_ascii_alphanumeric() || source[at] == b'_') {
                at += 1;
            }
            Class::Name
        } else if byte.is_ascii_digit() {
            while at < source.len() && (source[at].is_ascii_alphanumeric() || source[at] == b'.') {
                at += 1;
            }
            Class::Number
        } else if b"+-*/%&|^<>=!".contains(&byte) {
            while at < source.len() && b"+-*/%&|^<>=!".contains(&source[at]) {
                at += 1;
            }
            Class::Operator
        } else {
            at += 1;
            continue;
        };
        out.push((start, at, class));
    }
    out
}

/// Numbers a substitution puts in place of one: the boundaries of each
/// literal's type, and the values arithmetic is undefined or saturates at.
const NUMBERS: [&str; 20] = [
    "0",
    "1",
    "2",
    "3",
    "7",
    "31",
    "32",
    "64",
    "255",
    "256",
    "1000",
    "65535",
    "2147483647",
    "4294967295u",
    "0u",
    "0.5",
    "1.5",
    "1e10",
    "3.4e38",
    "0x7f",
];

/// Operators a substitution puts in place of one.
const OPERATORS: [&str; 18] = [
    "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", "<", ">", "<=", ">=", "==", "!=", "&&",
    "||",
];

/// One mutation of `source`: most keep its syntax — a line deleted,
/// duplicated or swapped, a name, number or operator replaced by another —
/// and the rest break it the ways a hostile source would.
fn mutate(rng: &mut Prng, source: &mut Vec<u8>, other: &[u8]) {
    let at = rng.at_most(source.len());
    let lines: Vec<(usize, usize)> = {
        let mut spans = Vec::new();
        let mut start = 0;
        for (index, &byte) in source.iter().enumerate() {
            if byte == b'\n' {
                spans.push((start, index + 1));
                start = index + 1;
            }
        }
        spans
    };
    match rng.below(16) {
        0 if !source.is_empty() => {
            let at = rng.below(source.len());
            let bytes = b"{}()<>[];,.:=+-*/&|!~@ \nax0_";
            source[at] = bytes[rng.below(bytes.len())];
        }
        1 if !source.is_empty() => {
            let start = rng.below(source.len());
            let end = (start + 1 + rng.below(16)).min(source.len());
            source.drain(start..end);
        }
        2 => {
            let token = TOKENS[rng.below(TOKENS.len())];
            let spaced: Vec<u8> = [b" ", token.as_bytes(), b" "].concat();
            source.splice(at..at, spaced);
        }
        3 if !other.is_empty() => {
            let from = rng.below(other.len());
            let end = (from + 1 + rng.below(64)).min(other.len());
            source.splice(at..at, other[from..end].iter().copied());
        }
        4 => {
            let depth = 1 + rng.below(40);
            let open: Vec<u8> = core::iter::repeat_n(b'(', depth).collect();
            source.splice(at..at, open);
        }
        5 | 6 if !lines.is_empty() => {
            let (start, end) = lines[rng.below(lines.len())];
            source.drain(start..end);
        }
        7 | 8 if !lines.is_empty() => {
            let (start, end) = lines[rng.below(lines.len())];
            let line: Vec<u8> = source[start..end].to_vec();
            let (into, _) = lines[rng.below(lines.len())];
            source.splice(into..into, line);
        }
        9 if lines.len() > 1 => {
            let first = rng.below(lines.len() - 1);
            let second = first + 1 + rng.below(lines.len() - first - 1);
            let (a, b) = (lines[first], lines[second]);
            let swapped: Vec<u8> =
                [&source[b.0..b.1], &source[a.1..b.0], &source[a.0..a.1]].concat();
            source.splice(a.0..b.1, swapped);
        }
        _ => {
            let spans = tokens(source);
            if spans.is_empty() {
                return;
            }
            let (start, end, class) = spans[rng.below(spans.len())];
            let replacement: Vec<u8> = match class {
                Class::Name => {
                    let names: Vec<&(usize, usize, Class)> =
                        spans.iter().filter(|t| t.2 == Class::Name).collect();
                    let &&(from, to, _) = &names[rng.below(names.len())];
                    source[from..to].to_vec()
                }
                Class::Number => NUMBERS[rng.below(NUMBERS.len())].as_bytes().to_vec(),
                Class::Operator => OPERATORS[rng.below(OPERATORS.len())].as_bytes().to_vec(),
            };
            source.splice(start..end, replacement);
        }
    }
}

/// The line and column, each from 1, byte `at` of `text` begins: lines end
/// at each line break WGSL names, columns count code points.
fn place(text: &str, at: usize) -> (u32, u32) {
    let (mut line, mut column) = (1u32, 1u32);
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if index >= at {
            break;
        }
        let breaks = match ch {
            '\r' => chars.peek().is_none_or(|&(_, next)| next != '\n'),
            '\n' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => true,
            _ => false,
        };
        if breaks {
            line += 1;
            column = 1;
        } else if ch != '\r' {
            column += 1;
        }
    }
    (line, column)
}

/// Hold one compile of `source` to invariants 1 to 4.
fn check(source: &str, options: &Options<'_>) {
    let first = compile(source, options);
    let again = compile(source, options);
    match (&first, &again) {
        (Ok(one), Ok(two)) => {
            assert_eq!(
                one.module, two.module,
                "a compile is deterministic\n{source}"
            );
            assert_eq!(
                one.diagnostics, two.diagnostics,
                "its diagnostics too\n{source}"
            );
            let module = Module::decode(&one.module).expect("a module the decoder reads");
            let admitted = Admitted::new(module).unwrap_or_else(|refusal| {
                panic!("a compiled module was refused again: {refusal:?}\n{source}")
            });
            match admitted.guarded() {
                Ok(_) | Err(GuardError::TooLarge) => {}
                Err(refused) => panic!("a compiled module guards: {refused:?}\n{source}"),
            }
            for diagnostic in &one.diagnostics {
                assert_ne!(diagnostic.severity, Severity::Error, "{diagnostic:?}");
                held_within(source, diagnostic);
            }
        }
        (Err(Failure::Error(one)), Err(Failure::Error(two))) => {
            assert_eq!(one, two, "a refusal is deterministic\n{source}");
            assert_eq!(one.severity, Severity::Error);
            assert!(Code::ALL.contains(&one.code));
            held_within(source, one);
        }
        (Err(Failure::Internal), _) | (_, Err(Failure::Internal)) => {
            panic!("an invariant of the front end failed\n{source}")
        }
        (Err(Failure::OutOfMemory), _) | (_, Err(Failure::OutOfMemory)) => {
            panic!("memory was refused on a small source\n{source}")
        }
        _ => panic!("a compile answered two ways\n{source}"),
    }
}

/// A diagnostic's span within `source`, beginning where it says.
fn held_within(source: &str, diagnostic: &Diagnostic) {
    let span = diagnostic.span;
    assert!(span.start <= span.end, "{diagnostic:?}");
    assert!(
        span.end as usize <= source.len(),
        "{diagnostic:?} lies within the source"
    );
    assert_eq!(
        (diagnostic.line, diagnostic.column),
        place(source, span.start as usize),
        "{diagnostic:?} begins where its span does\n{source}"
    );
    assert!(diagnostic.message.len() <= tairix_wgsl::MAX_MESSAGE_BYTES);
}

#[test]
fn the_front_end_is_total_over_mutated_sources() {
    for source in CORPUS {
        check(source, &Options::default());
        assert!(
            compile(source, &Options::default()).is_ok(),
            "the corpus compiles\n{source}"
        );
    }
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_front_end_is_total_over_mutated_sources",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..ROUNDS_PER_SWEEP {
            let base = CORPUS[rng.below(CORPUS.len())].as_bytes();
            let other = CORPUS[rng.below(CORPUS.len())].as_bytes();
            for _ in 0..MUTANTS_PER_ROUND {
                let mut source = base.to_vec();
                for _ in 0..=rng.below(4) {
                    mutate(&mut rng, &mut source, other);
                }
                let text = String::from_utf8_lossy(&source).into_owned();
                let width = f64::from(rng.next_u32() % 300);
                let overrides = [
                    (OverrideKey::Name("width"), width),
                    (OverrideKey::Id(3), -1.5),
                ];
                let options = Options {
                    overrides: if rng.below(4) == 0 { &overrides } else { &[] },
                };
                check(&text, &options);
            }
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

/// The variables a generated program computes in.
const VARIABLES: usize = 8;

/// The deepest a generated expression or statement nests.
const DEPTH: u32 = 4;

/// An integer operation of a generated program over `u32` words, `i32` ones
/// reached through `bitcast`.
#[derive(Copy, Clone, Debug)]
enum Op {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    And,
    Or,
    Xor,
    Left,
    Right,
    Min,
    Max,
    SignedDivide,
    SignedRemainder,
    SignedRight,
    SignedMin,
    SignedMax,
}

const OPS: [Op; 17] = [
    Op::Add,
    Op::Subtract,
    Op::Multiply,
    Op::Divide,
    Op::Remainder,
    Op::And,
    Op::Or,
    Op::Xor,
    Op::Left,
    Op::Right,
    Op::Min,
    Op::Max,
    Op::SignedDivide,
    Op::SignedRemainder,
    Op::SignedRight,
    Op::SignedMin,
    Op::SignedMax,
];

/// A bit built-in of one word.
#[derive(Copy, Clone, Debug)]
enum Bits {
    Not,
    Ones,
    Reverse,
    LeadingZeros,
    TrailingZeros,
    LeadingBit,
    TrailingBit,
    SignedLeadingBit,
}

const BITS: [Bits; 8] = [
    Bits::Not,
    Bits::Ones,
    Bits::Reverse,
    Bits::LeadingZeros,
    Bits::TrailingZeros,
    Bits::LeadingBit,
    Bits::TrailingBit,
    Bits::SignedLeadingBit,
];

/// An expression of a generated program: every leaf a variable, so nothing
/// is a const-expression the checker would evaluate instead.
#[derive(Clone, Debug)]
enum Expr {
    Var(usize),
    Unary(Bits, Box<Expr>),
    Binary(Op, Box<Expr>, Box<Expr>),
    /// `select(a, b, c < d)`.
    Pick([Box<Expr>; 4]),
    /// `extractBits(a, b, c)`, signed where it says.
    Extract(bool, [Box<Expr>; 3]),
    /// `insertBits(a, b, c, d)`.
    Insert([Box<Expr>; 4]),
}

#[derive(Clone, Debug)]
enum Stmt {
    Assign(usize, Expr),
    /// `if a < b { .. } else { .. }`.
    If(Expr, Expr, Vec<Stmt>, Vec<Stmt>),
    /// `for (var i = 0u; i < (v % 4u); i++) { .. }`: at most three rounds.
    Repeat(usize, Vec<Stmt>),
}

fn expr(rng: &mut Prng, depth: u32) -> Expr {
    let next = |rng: &mut Prng| Box::new(expr(rng, depth.saturating_sub(1)));
    if depth == 0 || rng.below(4) == 0 {
        return Expr::Var(rng.below(VARIABLES));
    }
    match rng.below(10) {
        0 | 1 => Expr::Unary(BITS[rng.below(BITS.len())], next(rng)),
        2 => Expr::Pick([next(rng), next(rng), next(rng), next(rng)]),
        3 => Expr::Extract(rng.below(2) == 0, [next(rng), next(rng), next(rng)]),
        4 => Expr::Insert([next(rng), next(rng), next(rng), next(rng)]),
        _ => Expr::Binary(OPS[rng.below(OPS.len())], next(rng), next(rng)),
    }
}

fn block(rng: &mut Prng, depth: u32, out: &mut Vec<Stmt>) {
    for _ in 0..=rng.below(4) {
        let target = rng.below(VARIABLES);
        out.push(match rng.below(if depth == 0 { 1 } else { 5 }) {
            0..=2 => Stmt::Assign(target, expr(rng, DEPTH)),
            3 => {
                let (mut then, mut otherwise) = (Vec::new(), Vec::new());
                block(rng, depth - 1, &mut then);
                block(rng, depth - 1, &mut otherwise);
                Stmt::If(expr(rng, 2), expr(rng, 2), then, otherwise)
            }
            _ => {
                let mut body = Vec::new();
                block(rng, depth - 1, &mut body);
                Stmt::Repeat(rng.below(VARIABLES), body)
            }
        });
    }
}

impl Op {
    /// The operator as WGSL spells it, a built-in's name where it is one,
    /// and whether it reads its operands as `i32`.
    const fn spelling(self) -> (&'static str, bool) {
        match self {
            Self::Add => ("+", false),
            Self::Subtract => ("-", false),
            Self::Multiply => ("*", false),
            Self::Divide => ("/", false),
            Self::Remainder => ("%", false),
            Self::And => ("&", false),
            Self::Or => ("|", false),
            Self::Xor => ("^", false),
            Self::Left => ("<<", false),
            Self::Right => (">>", false),
            Self::Min => ("min", false),
            Self::Max => ("max", false),
            Self::SignedDivide => ("/", true),
            Self::SignedRemainder => ("%", true),
            Self::SignedRight => (">>", true),
            Self::SignedMin => ("min", true),
            Self::SignedMax => ("max", true),
        }
    }

    /// `left` and `right` under the operator, as WGSL defines it.
    fn apply(self, left: u32, right: u32) -> u32 {
        let (signed_left, signed_right) = (left.cast_signed(), right.cast_signed());
        match self {
            Self::Add => left.wrapping_add(right),
            Self::Subtract => left.wrapping_sub(right),
            Self::Multiply => left.wrapping_mul(right),
            Self::Divide => left.checked_div(right).unwrap_or(left),
            Self::Remainder => left.checked_rem(right).unwrap_or(0),
            Self::And => left & right,
            Self::Or => left | right,
            Self::Xor => left ^ right,
            Self::Left => left << (right & 31),
            Self::Right => left >> (right & 31),
            Self::Min => left.min(right),
            Self::Max => left.max(right),
            Self::SignedDivide => signed_left
                .checked_div(signed_right)
                .unwrap_or(signed_left)
                .cast_unsigned(),
            Self::SignedRemainder => signed_left
                .checked_rem(signed_right)
                .unwrap_or(0)
                .cast_unsigned(),
            Self::SignedRight => (signed_left >> (right & 31)).cast_unsigned(),
            Self::SignedMin => signed_left.min(signed_right).cast_unsigned(),
            Self::SignedMax => signed_left.max(signed_right).cast_unsigned(),
        }
    }
}

impl Bits {
    /// The built-in's result for `word`, as WGSL defines it.
    fn apply(self, word: u32) -> u32 {
        match self {
            Self::Not => !word,
            Self::Ones => word.count_ones(),
            Self::Reverse => word.reverse_bits(),
            Self::LeadingZeros => word.leading_zeros(),
            Self::LeadingBit => word.checked_ilog2().unwrap_or(u32::MAX),
            Self::TrailingBit if word == 0 => u32::MAX,
            Self::TrailingZeros | Self::TrailingBit => word.trailing_zeros(),
            Self::SignedLeadingBit => {
                let magnitude = if word.cast_signed() < 0 { !word } else { word };
                magnitude.checked_ilog2().unwrap_or(u32::MAX)
            }
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Not => "~",
            Self::Ones => "countOneBits",
            Self::Reverse => "reverseBits",
            Self::LeadingZeros => "countLeadingZeros",
            Self::TrailingZeros => "countTrailingZeros",
            Self::LeadingBit | Self::SignedLeadingBit => "firstLeadingBit",
            Self::TrailingBit => "firstTrailingBit",
        }
    }
}

/// An operand as an operation reads it: as an `i32` where it is signed.
struct Operand<'e>(&'e Expr, bool);

impl fmt::Display for Operand<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.1 {
            write!(f, "bitcast<i32>({})", self.0)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Var(at) => write!(f, "v{at}"),
            Self::Unary(Bits::SignedLeadingBit, operand) => {
                write!(
                    f,
                    "bitcast<u32>(firstLeadingBit({}))",
                    Operand(operand, true)
                )
            }
            Self::Unary(bits, operand) => write!(f, "{}({operand})", bits.name()),
            Self::Binary(op, left, right) => {
                let (spelling, signed) = op.spelling();
                // A shift's amount is a u32 whatever it shifts.
                let (left, right) = (
                    Operand(left, signed),
                    Operand(right, signed && !matches!(op, Op::SignedRight)),
                );
                let (open, close) = if signed {
                    ("bitcast<u32>(", ")")
                } else {
                    ("", "")
                };
                if spelling.starts_with(char::is_alphabetic) {
                    write!(f, "{open}{spelling}({left}, {right}){close}")
                } else {
                    write!(f, "{open}({left} {spelling} {right}){close}")
                }
            }
            Self::Pick([kept, chosen, left, right]) => {
                write!(f, "select({kept}, {chosen}, {left} < {right})")
            }
            Self::Extract(false, [base, offset, count]) => {
                write!(f, "extractBits({base}, {offset}, {count})")
            }
            Self::Extract(true, [base, offset, count]) => write!(
                f,
                "bitcast<u32>(extractBits({}, {offset}, {count}))",
                Operand(base, true)
            ),
            Self::Insert([base, inserted, offset, count]) => {
                write!(f, "insertBits({base}, {inserted}, {offset}, {count})")
            }
        }
    }
}

/// Statements at a depth of loops, which names each loop's counter.
struct Block<'s>(&'s [Stmt], usize);

impl fmt::Display for Block<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(statements, depth) = *self;
        for statement in statements {
            match statement {
                Stmt::Assign(target, value) => writeln!(f, "v{target} = {value};")?,
                Stmt::If(left, right, then, otherwise) => writeln!(
                    f,
                    "if {left} < {right} {{\n{}}} else {{\n{}}}",
                    Block(then, depth + 1),
                    Block(otherwise, depth + 1)
                )?,
                Stmt::Repeat(count, body) => writeln!(
                    f,
                    "for (var i{depth} = 0u; i{depth} < v{count} % 4u; i{depth}++) {{\n{}}}",
                    Block(body, depth + 1)
                )?,
            }
        }
        Ok(())
    }
}

/// The offset and count a bit field takes, as WGSL clamps the two.
fn field(offset: u32, count: u32) -> (u32, u32) {
    let offset = offset.min(32);
    (offset, count.min(32 - offset))
}

fn eval(expr: &Expr, vars: &[u32; VARIABLES]) -> u32 {
    match expr {
        Expr::Var(at) => vars[*at],
        Expr::Unary(bits, operand) => bits.apply(eval(operand, vars)),
        Expr::Binary(op, left, right) => op.apply(eval(left, vars), eval(right, vars)),
        Expr::Pick([kept, chosen, left, right]) => {
            if eval(left, vars) < eval(right, vars) {
                eval(chosen, vars)
            } else {
                eval(kept, vars)
            }
        }
        Expr::Extract(signed, [base, offset, count]) => {
            let (offset, count) = field(eval(offset, vars), eval(count, vars));
            if count == 0 {
                return 0;
            }
            let mask = (1u64 << count) - 1;
            let bits = (u64::from(eval(base, vars)) >> offset) & mask;
            let negative = *signed && bits >> (count - 1) & 1 == 1;
            let extended = if negative { bits | !mask } else { bits };
            u32::try_from(extended & u64::from(u32::MAX)).expect("a word")
        }
        Expr::Insert([base, inserted, offset, count]) => {
            let (offset, count) = field(eval(offset, vars), eval(count, vars));
            let mask = ((1u64 << count) - 1) << offset;
            let (base, inserted) = (u64::from(eval(base, vars)), u64::from(eval(inserted, vars)));
            u32::try_from(((base & !mask) | ((inserted << offset) & mask)) & u64::from(u32::MAX))
                .expect("a word")
        }
    }
}

fn run_block(statements: &[Stmt], vars: &mut [u32; VARIABLES]) {
    for statement in statements {
        match statement {
            Stmt::Assign(target, value) => vars[*target] = eval(value, vars),
            Stmt::If(left, right, then, otherwise) => {
                if eval(left, vars) < eval(right, vars) {
                    run_block(then, vars);
                } else {
                    run_block(otherwise, vars);
                }
            }
            Stmt::Repeat(count, body) => {
                let mut round = 0;
                while round < vars[*count] % 4 {
                    run_block(body, vars);
                    round += 1;
                }
            }
        }
    }
}

/// A word a generated program's variable starts as: a boundary of the
/// arithmetic more often than not.
fn start_word(rng: &mut Prng) -> u32 {
    const EDGES: [u32; 9] = [0, 1, 2, 31, 32, 33, 0x7FFF_FFFF, 0x8000_0000, u32::MAX];
    if rng.below(3) == 0 {
        rng.next_u32()
    } else {
        EDGES[rng.below(EDGES.len())]
    }
}

#[test]
fn generated_integer_programs_run_to_wgsls_values() {
    let gpu = Gpu::new();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "generated_integer_programs_run_to_wgsls_values",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..PROGRAMS_PER_SWEEP {
            let mut program = Vec::new();
            block(&mut rng, 3, &mut program);
            let mut body = String::from("@compute @workgroup_size(1)\nfn main() {\n");
            for at in 0..VARIABLES {
                let _ = writeln!(body, "var v{at} = input[{at}];");
            }
            let _ = write!(body, "{}", Block(&program, 0));
            for at in 0..VARIABLES {
                let _ = writeln!(body, "out[{at}] = v{at};");
            }
            body.push_str("}\n");
            let mut values = [0u32; VARIABLES];
            for value in &mut values {
                *value = start_word(&mut rng);
            }
            let source = with_bindings(&body);
            let ran = gpu.compute(
                admitted(&source, &Options::default()),
                &values,
                1,
                VARIABLES,
            );
            run_block(&program, &mut values);
            assert_eq!(
                ran, values,
                "the device and WGSL's definitions disagree\n{source}"
            );
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
