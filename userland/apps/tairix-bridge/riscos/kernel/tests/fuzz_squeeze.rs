//! Deterministic fuzz harness for the squeezed-module expander: the corpus's
//! squeezed modules mutated, and noise carrying a squeezed length, expanded
//! under random bounds.
//!
//! The invariants, checked for every image:
//!
//! * nothing panics, and every refusal is a value;
//! * a module expands to whole pairs of words, no more than its trailer's
//!   size or the bound allows;
//! * the same image always expands the same way.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use std::path::Path;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::corpus::{parse_scenarios, Step, Value};
use tairix_riscos_kernel::module::squeeze::{is_squeezed, unsqueeze};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 4000;
/// The block each squeeze scenario writes its module into.
const BLOCK: &str = "mod";

/// Every squeezed module the corpus inserts.
fn seeds() -> Vec<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/squeeze.scenarios");
    let text = std::fs::read_to_string(path).expect("the squeeze scenarios");
    let mut seeds = Vec::new();
    for scenario in parse_scenarios(&text).expect("parsed") {
        let mut image = Vec::new();
        for step in &scenario.steps {
            if let Step::Write {
                at: Value::Kept { name, offset },
                bytes,
            } = step
            {
                assert_eq!(name, BLOCK, "{}: a module's block", scenario.name);
                let at = usize::try_from(*offset).expect("a forward offset");
                image.resize(image.len().max(at + bytes.len()), 0);
                image[at..at + bytes.len()].copy_from_slice(bytes);
            }
        }
        if is_squeezed(&image) {
            seeds.push(image);
        }
    }
    assert!(!seeds.is_empty(), "squeezed modules to start from");
    seeds
}

/// Noise as long as its own squeezed length says.
fn noise(rng: &mut Prng) -> Vec<u8> {
    let length = 4 * (8 + rng.below(64));
    let mut image = rng.noise(length);
    let word = u32::try_from(length).expect("short") | 1 << 31;
    image[4..8].copy_from_slice(&word.to_le_bytes());
    image
}

/// The trailer's expanded size, where the image has a trailer to read.
fn stated_size(image: &[u8]) -> Option<usize> {
    let length = u32::from_le_bytes(image.get(4..8)?.try_into().ok()?) & !(1 << 31);
    let at = usize::try_from(length).ok()?.checked_sub(20)?;
    let size = u32::from_le_bytes(image.get(at..at + 4)?.try_into().ok()?);
    usize::try_from(size).ok()
}

fn check(image: &[u8], most: usize) {
    let expanded = unsqueeze(image, most);
    assert_eq!(unsqueeze(image, most), expanded, "the same each time");
    let Ok(module) = expanded else {
        return;
    };
    assert!(module.len().is_multiple_of(8), "whole pairs of words");
    assert!(module.len() <= most, "within the bound");
    assert!(
        stated_size(image).is_some_and(|size| module.len() <= size),
        "within the size its trailer states"
    );
}

#[test]
fn any_squeezed_image_expands_within_its_bounds_or_is_refused() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_squeezed_image_expands_within_its_bounds_or_is_refused",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds = seeds();
    let expanded = seeds
        .iter()
        .filter(|seed| {
            check(seed, 1 << 20);
            unsqueeze(seed, 1 << 20).is_ok()
        })
        .count();
    assert!(
        expanded > 0 && expanded < seeds.len(),
        "the seeds reach both an expansion and a refusal"
    );
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let image = if rng.below(4) == 0 {
            noise(&mut rng)
        } else {
            let pristine = rng.pick(&seeds).clone();
            rng.mutated(&pristine)
        };
        let most = *rng.pick(&[0, 8, 64, 256, 1 << 20]);
        check(&image, most);
        runs += 1;
    }
}
