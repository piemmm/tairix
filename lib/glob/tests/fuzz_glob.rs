//! Deterministic fuzz harness for the `lib/glob` matcher (its untrusted-pattern
//! compiler and match engine).
//!
//! The invariants:
//!
//! * compiling any text, under any [`Options`] and as marked text, never
//!   panics — it yields a `Pattern` or a typed `GlobError` (fail closed);
//! * matching any compiled pattern against any candidate never panics and
//!   always terminates, including extended patterns against candidates at
//!   and past [`tairix_glob::MAX_EXTENDED_LEN`];
//! * the convenience match and a reused [`Scratch`] agree, so no state leaks
//!   from one candidate into the next.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` draws
//! pseudo-random patterns, mutates real templates, and matches them against
//! mutated candidates. A plain `cargo test` runs the [`SMOKE_ITERATIONS`]
//! sweep once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend it to a wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_glob::{Mark, Options, Pattern, Scratch, MAX_EXTENDED_LEN};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Largest arbitrary byte string turned into a pattern or candidate.
const MAX_NOISE: usize = 512;

/// Real templates the harness mutates, each exercising a different compile
/// or match path, malformed forms included.
const TEMPLATES: &[&str] = &[
    "*.rs",
    "img_[0-9][0-9].???",
    "[!a-z]*[A-Z]",
    r"a\*b\?c",
    "[]-]*[^]]",
    "**/*.tar.gz",
    "[abc",
    "[z-a]",
    r"trailing\",
    "?*?*?*?*?*",
    "[[:alpha:][:digit:]_]*",
    "[[.space.][=e=]]x",
    "[[:nope:]]",
    "!(*.o|*.a)",
    "*.@(jpg|png|gif)",
    "+([0-9])?(.[0-9])",
    "*(a|b*(c|!(d)))",
    "@(a(b)c|[|)]x)",
    "@(unclosed|",
    "src/*/[!.]*.rs",
];

/// Candidate strings compiled patterns are matched against.
const CANDIDATES: &[&str] = &[
    "",
    "lib.rs",
    "img_04.png",
    "café/photo_99.jpeg",
    "a/b/c/d.tar.gz",
    "]-]",
    ".hidden",
    "ZZZ",
    "a(b)c",
];

/// Every combination of the four option flags.
fn options(bits: u8) -> Options {
    Options::empty()
        .with(Options::EXTENDED, bits & 1 != 0)
        .with(Options::CASE_INSENSITIVE, bits & 2 != 0)
        .with(Options::PERIOD, bits & 4 != 0)
        .with(Options::PATHNAME, bits & 8 != 0)
}

/// Compile `pattern` (must not panic) under `options`, and when it compiles
/// match it against every fixed candidate plus `extra` (must not panic or
/// hang), checking the reused scratch agrees with a fresh match.
fn exercise(pattern: &str, options: Options, extra: &str, scratch: &mut Scratch) {
    if let Ok(pat) = Pattern::with_options(pattern, options) {
        for candidate in CANDIDATES.iter().copied().chain([extra]) {
            assert_eq!(
                pat.matches_with(candidate, scratch),
                pat.matches(candidate),
                "{pattern:?} against {candidate:?}"
            );
        }
    }
}

#[test]
fn compile_and_match_never_panic_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);

    // The seed is drawn and logged by `tairix_fuzzseed::start`: fresh per
    // run, reproducible from the logged value via `TAIRIX_FUZZ_SEED`.
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "compile_and_match_never_panic_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut scratch = Scratch::new();

    let mut iteration: u64 = 0;
    loop {
        let flags = options(rng.next_u8() & 0x0F);

        // 1. A real template with a handful of bytes flipped at random.
        let template = *rng.pick(TEMPLATES);
        let mut mutated: Vec<u8> = template.as_bytes().to_vec();
        for _ in 0..rng.at_most(6) {
            if mutated.is_empty() {
                break;
            }
            let pos = rng.below(mutated.len());
            mutated[pos] ^= rng.next_u8();
        }
        let candidate = *rng.pick(CANDIDATES);
        exercise(
            &String::from_utf8_lossy(&mutated),
            flags,
            candidate,
            &mut scratch,
        );

        // 2. A structured-but-hostile pattern: metacharacters, group
        //    operators and bracket syntax spliced together.
        let mut spliced = String::new();
        for _ in 0..rng.at_most(48) {
            const PIECES: &[&str] = &[
                "*", "?", "[", "]", "-", "\\", "!", "^", "(", ")", "|", "@(", "*(", "+(", "!(",
                "[:", ":]", "[.", ".]", "[=", "=]", "/", ".",
            ];
            if rng.at_most(3) == 0 {
                spliced.push(char::from(b'a' + rng.next_u8() % 26));
            } else {
                spliced.push_str(rng.pick(PIECES));
            }
        }
        exercise(&spliced, flags, candidate, &mut scratch);

        // 3. The same text as marked input, with random characters literal.
        let marked: Vec<Mark> = spliced
            .chars()
            .map(|ch| Mark {
                ch,
                literal: rng.at_most(3) == 0,
            })
            .collect();
        if let Ok(pat) = Pattern::from_marked(&marked, flags) {
            let _ = pat.matches_with(candidate, &mut scratch);
        }

        // 4. An extended pattern against candidates at and past the bound.
        if let Ok(pat) = Pattern::with_options("*(a|b*(a))!(c)", options(1)) {
            let len = MAX_EXTENDED_LEN - 1 + rng.at_most(2);
            let long: String = (0..len).map(|_| ['a', 'b', 'c'][rng.below(3)]).collect();
            let _ = pat.matches_with(&long, &mut scratch);
        }

        // 5. Pure noise as the pattern (lossy UTF-8), and as the candidate.
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        let noise = String::from_utf8_lossy(&noise);
        exercise(&noise, flags, &noise, &mut scratch);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
