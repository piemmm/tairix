//! Deterministic fuzz harness for the service enrolment override parser.
//!
//! `/System/Settings/Services/overrides` is read by PID 1 as it decides which
//! services to bring up, so its bytes are outside the reader's trust boundary:
//! a hostile or corrupted document must be refused whole, never partly
//! applied. Three invariants are held against arbitrary text:
//!
//! * [`EnrolmentOverride::parse`] never panics, whatever it is given;
//! * a document that parses renders to text that parses back equal;
//! * over any image layer, a layer rebuilt by [`EnrolmentOverride::set`]ting
//!   each service to the enrolment a parsed one gives it reproduces that
//!   enrolment exactly, holding an entry only where it differs from the
//!   image.
//!
//! A plain `cargo test` runs the fixed [`SMOKE_ITERATIONS`] sweep;
//! `cargo xtask fuzz` exports `TAIRIX_FUZZ_BUDGET_SECS` to extend it.

use tairix_abi::ServiceEnrolment;
use tairix_enrolment::{enabled, Enrolment, EnrolmentOverride};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Largest arbitrary string fed straight to the parser.
const MAX_NOISE: usize = 1024;

/// Bytes the noise is drawn from: the format's own alphabet, so mutations
/// reach past the first name check rather than bouncing off it.
const ALPHABET: &[u8] = b"abcdefimnorstxz0123456789._-# \t\nenableddisabledENABLED/";

/// The image layers the inverse is checked over.
const VENDORS: &[&[&str]] = &[&[], &["netstack"], &["netstack", "timed", "fontd"]];

/// Well-formed documents, rendered through the engine itself so the harness
/// holds no second copy of the format.
fn templates() -> Vec<String> {
    VENDORS
        .iter()
        .map(|vendor| {
            let vendor = Enrolment::of(vendor.iter().copied()).expect("valid names");
            let mut overrides = EnrolmentOverride::empty();
            for name in ["discoveryd", "netstack", "timed", "fontd"] {
                let wanted = if matches!(name, "discoveryd" | "netstack") {
                    ServiceEnrolment::Enabled
                } else {
                    ServiceEnrolment::Disabled
                };
                let _ = overrides.set(&vendor, name, wanted).expect("set");
            }
            overrides
                .to_store_text()
                .expect("a two-entry document fits")
        })
        .chain([
            String::from("# nothing changed\n\n"),
            String::from("timed disabled # why\nfontd enabled\n"),
        ])
        .collect()
}

/// Parse `text`; on success, the round trip and the inverse must hold.
fn exercise(text: &str) {
    let Ok(overrides) = EnrolmentOverride::parse(text) else {
        return;
    };
    let rendered = overrides
        .to_store_text()
        .expect("a parsed document of at most MAX_NOISE bytes renders within the bound");
    assert_eq!(
        EnrolmentOverride::parse(&rendered).as_ref(),
        Ok(&overrides),
        "round trip changed the document"
    );
    for vendor in VENDORS {
        let image = Enrolment::of(vendor.iter().copied()).expect("valid names");
        let named = || {
            vendor
                .iter()
                .copied()
                .chain(overrides.entries().iter().map(|(name, _)| name.as_str()))
        };
        let mut rebuilt = EnrolmentOverride::empty();
        for name in named() {
            let wanted = if enabled(&image, &overrides, name) {
                ServiceEnrolment::Enabled
            } else {
                ServiceEnrolment::Disabled
            };
            let _ = rebuilt.set(&image, name, wanted).expect("set");
        }
        for name in named() {
            assert_eq!(
                enabled(&image, &rebuilt, name),
                enabled(&image, &overrides, name),
                "the rebuilt layer does not reproduce what it was made for"
            );
        }
        assert!(
            rebuilt
                .entries()
                .iter()
                .all(|(name, disposition)| image.is_enabled(name) != disposition.is_enabled()),
            "the rebuilt layer holds an entry the image already says"
        );
    }
}

#[test]
fn parsing_any_override_document_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let corpus = templates();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "parsing_any_override_document_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let template = rng.pick(&corpus);
        let mut mutated = template.clone().into_bytes();
        for _ in 0..rng.at_most(8) {
            if mutated.is_empty() {
                break;
            }
            let at = rng.below(mutated.len());
            mutated[at] = *rng.pick(ALPHABET);
        }
        if let Ok(text) = core::str::from_utf8(&mutated) {
            exercise(text);
        }
        if let Some(prefix) = template.get(..rng.at_most(template.len())) {
            exercise(prefix);
        }
        let mut noise = String::new();
        for _ in 0..rng.at_most(MAX_NOISE) {
            noise.push(char::from(*rng.pick(ALPHABET)));
        }
        exercise(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
