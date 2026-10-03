//! Deterministic fuzz harness for the XML element scanner, the reader under
//! the SVG decoder and OpenRaster's layer stack: no input, however broken,
//! panics it. A plain `cargo test` runs a fixed sweep from a fresh, logged
//! seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_xml::parse;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 50_000;

/// Largest arbitrary byte string fed straight to the scanner.
const MAX_NOISE: usize = 2048;

/// Real documents the harness mutates.
const TEMPLATES: &[&[u8]] = &[
    br#"<?xml version="1.0" encoding="UTF-8"?><image version="0.0.3" w="64" h="48"><stack><layer name="Layer 1" src="data/layer0.png" x="0" y="0" opacity="1.000" visibility="visible"/></stack></image>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:x="urn:x"><x:g a='1'><![CDATA[<raw>]]>&amp;&#65;&#x42;</x:g><!-- c --></svg>"#,
];

/// Characters that move the scanner between its states.
const DELIMITERS: &[u8] = b"<>\"'/=&;!?-[]:";

#[test]
fn the_scanner_never_panics_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_scanner_never_panics_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut mutated = rng.pick(TEMPLATES).to_vec();
        for _ in 0..rng.at_most(8) {
            let at = rng.below(mutated.len().max(1));
            match rng.below(3) {
                0 if at < mutated.len() => mutated[at] = rng.next_u8(),
                1 => mutated.insert(at.min(mutated.len()), *rng.pick(DELIMITERS)),
                _ if at < mutated.len() => {
                    mutated.remove(at);
                }
                _ => {}
            }
        }
        if let Ok(text) = core::str::from_utf8(&mutated) {
            let _ = parse(text, "urn:x");
        }
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        for byte in &mut noise {
            *byte = DELIMITERS[usize::from(*byte) % DELIMITERS.len()];
        }
        if let Ok(text) = core::str::from_utf8(&noise) {
            let _ = parse(text, "urn:x");
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
