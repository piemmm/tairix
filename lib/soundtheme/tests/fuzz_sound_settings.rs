//! Deterministic fuzz harness for the sound settings registry: the machine's
//! document and the session's store are read tolerantly, and the document a
//! session pushes to the cue authority strictly, and for any bytes
//!
//! 1. no reading panics,
//! 2. an accepted push's canonical render re-reads equal, both ways,
//! 3. the tolerant reading leaves every key it refuses at its default, and
//! 4. whatever a session's store holds, what it pushes is a document the cue
//!    authority accepts, meaning exactly what the session shows.
//!
//! The fixed sweep runs under plain `cargo test`; under `cargo xtask fuzz` the
//! same seeded stream keeps being drawn until the budget elapses.

use tairix_appconf::{Document, Registry, MAX_DOCUMENT_LEN};
use tairix_fuzzseed::registry::{registry_document, RegistryRow};
use tairix_fuzzseed::Prng;
use tairix_soundtheme::{merge_pushed, pushable, SoundKey, SoundSettings, UserChoices, PUSHABLE};

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 5_000;

const BOOLS: &[&str] = &["true", "false", "on", "off"];
const NOT_BOOLS: &[&str] = &["yes", "1", "True"];
const THEMES: &[&str] = &["TAIRiX", "High Contrast", "\"Quiet Hours\""];
const NOT_THEMES: &[&str] = &["..", "a/b", "\"\""];
const GAINS: &[&str] = &["0dB", "-6.5dB", "-12.25dB", "-90dB"];
const NOT_GAINS: &[&str] = &["3dB", "-0dB", "-6", "-06dB", "+1dB"];
const FILES: &[&str] = &["\"\"", "/Users/ada/UserFiles/Music/bell.flac", "/a"];
const NOT_FILES: &[&str] = &["relative.flac", "Music:/bell.flac", "/"];
const BAD_TOKENS: &[&str] = &["", " ", "has space", "bogus"];

/// `key` with the values it accepts and those it refuses.
fn row(key: SoundKey) -> RegistryRow {
    let (accepted, refused) = match key {
        SoundKey::Enabled | SoundKey::EventEnabled(_) => (BOOLS, NOT_BOOLS),
        SoundKey::Theme => (THEMES, NOT_THEMES),
        SoundKey::EventGain(_) => (GAINS, NOT_GAINS),
        SoundKey::EventFile(_) => (FILES, NOT_FILES),
    };
    (SoundSettings::name(key), accepted, refused)
}

/// Every key, built from the registry itself so no key goes unfuzzed.
fn keys() -> Vec<RegistryRow> {
    SoundSettings::KEYS.iter().copied().map(row).collect()
}

/// The keys a session may push, which is what the strict reading accepts.
fn pushed_keys() -> Vec<RegistryRow> {
    PUSHABLE.iter().copied().map(row).collect()
}

/// Read `doc` every way and hold every invariant the readings owe each other,
/// answering whether the strict reading accepted it.
fn check(doc: &str) -> bool {
    let defaults = SoundSettings::default();
    if let Ok(parsed) = Document::parse(doc) {
        let (lenient, refused) = SoundSettings::load(&parsed);
        let mut mine = String::new();
        let mut theirs = String::new();
        for key in refused {
            mine.clear();
            theirs.clear();
            let had = lenient.spell(key, &mut mine);
            let default = defaults.spell(key, &mut theirs);
            assert_eq!((had, &mine), (default, &theirs), "{key} kept its default");
        }
        let (choices, _) = UserChoices::load(&parsed);
        let pushed = choices.pushed();
        assert_eq!(
            merge_pushed(&lenient, &pushed),
            Ok(choices.over(&lenient)),
            "a session pushes what it shows"
        );
        assert_eq!(UserChoices::load(&choices.document()).0, choices);
    }
    let Ok(settings) = merge_pushed(&defaults, doc) else {
        return false;
    };
    let rendered = settings.document_of(&PUSHABLE).render();
    assert!(rendered.len() <= MAX_DOCUMENT_LEN);
    let reread = merge_pushed(&defaults, &rendered).expect("a rendered document re-reads");
    assert_eq!(settings, reread, "render and merge are not a round trip");
    let (lenient, refused) =
        SoundSettings::load(&Document::parse(&rendered).expect("a rendered document parses"));
    assert!(refused.is_empty(), "a canonical document refused a key");
    assert_eq!(settings, lenient, "the two readings disagree");
    for key in SoundSettings::KEYS
        .iter()
        .copied()
        .filter(|key| !pushable(*key))
    {
        let (mut ours, mut defaults_spelled) = (String::new(), String::new());
        let ours = (settings.spell(key, &mut ours), ours);
        let theirs = (defaults.spell(key, &mut defaults_spelled), defaults_spelled);
        assert_eq!(ours, theirs, "{key} is no push's to change");
    }
    let mut choices = UserChoices::default();
    choices
        .apply(doc)
        .expect("an accepted push is a pane's to apply");
    assert_eq!(choices.over(&defaults), settings);
    true
}

#[test]
fn generated_documents_round_trip_through_the_canonical_render() {
    let (keys, pushed) = (keys(), pushed_keys());
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "generated_documents_round_trip_through_the_canonical_render",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for draw in 0..SMOKE_ITERATIONS {
            let rows = if draw % 2 == 0 { &keys } else { &pushed };
            check(&registry_document(&mut rng, rows, BAD_TOKENS));
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn the_generator_produces_accepted_documents() {
    const DRAWS: u64 = 2_000;
    let keys = pushed_keys();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_generator_produces_accepted_documents",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let accepted = (0..DRAWS)
        .filter(|_| check(&registry_document(&mut rng, &keys, BAD_TOKENS)))
        .count();
    assert!(
        u64::try_from(accepted).expect("count fits") * 2 >= DRAWS,
        "only {accepted} of {DRAWS} generated documents parsed; the corpus is degenerate"
    );
}

#[test]
fn arbitrary_ascii_never_panics() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "arbitrary_ascii_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut buf = String::new();
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            buf.clear();
            for _ in 0..rng.below(256) {
                buf.push(char::from(u8::try_from(rng.below(128)).expect("byte fits")));
            }
            check(&buf);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
