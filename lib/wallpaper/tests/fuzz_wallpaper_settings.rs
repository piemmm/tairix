//! Deterministic fuzz harness for the pinboard settings registry.
//!
//! The `key = value` *grammar* is `lib/appconf`'s and is fuzzed there; what
//! this harness holds is the **registry** over it, for any bytes the desktop
//! session's published document or a pinboard-channel payload may carry:
//!
//! 1. [`merge`] — the strict reading — never panics on any input, and
//!    every document it accepts yields a total, well-formed settings value.
//! 2. [`DesktopSettings::document`] and [`merge`] are inverses: the
//!    canonical document of accepted settings re-reads equal, and its
//!    rendered text is itself within [`tairix_appconf::MAX_DOCUMENT_LEN`],
//!    so a writer can never emit a document the reader would refuse as too
//!    long.
//! 3. `DesktopSettings`' registry load — the tolerant reading — never panics and
//!    is *total*: whatever a stored document says, every field it does not
//!    accept is left at its documented default and named in the refusal
//!    list, so the two readings agree on every document the strict one
//!    accepts.
//!
//! The generator emits whole setting lines and poisons one line of one
//! document in four, so most documents are accepted and the round-trip
//! invariant is genuinely exercised. The second test hammers the reader with
//! arbitrary ASCII.
//!
//! The fixed sweep runs under plain `cargo test`; under `cargo xtask fuzz`
//! the same seeded stream keeps being drawn until the budget elapses.

use tairix_appconf::{Document, Registry, MAX_DOCUMENT_LEN};
use tairix_fuzzseed::Prng;
use tairix_wallpaper::{merge, DesktopSettings};

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 5_000;

/// Every key of the registry with values it accepts and values it refuses,
/// so no `set_field`/`field_value` arm is left unfuzzed. The names are
/// asserted against `SettingsKey::ALL` below, so a key added without a row here
/// fails rather than silently going uncovered.
const KEYS: &[(&str, &[&str], &[&str])] = &[
    (
        "wallpaper",
        &[
            "none",
            "/System/Graphics/Wallpapers/TAIRiX/tairix-dark.jpg",
            "/Users/ada/Documents/sunset.png",
            // The format engine quotes a `#`, so a name holding one round-trips.
            "/Users/ada/Documents/sunset#2.png",
        ],
        &[],
    ),
    ("fit", &["fill", "fit", "stretch", "centre", "tile"], &[]),
    ("backdrop", &["theme", "112233", "ffffff", "000000"], &[]),
    ("icons", &["leading", "trailing"], &[]),
    ("sort", &["name", "kind", "size", "date"], &[]),
    ("appearance", &["dark", "light"], &[]),
    ("contrast", &["normal", "high", "monochrome"], &[]),
    ("density", &["compact", "normal", "comfortable"], &[]),
    ("motion", &["full", "reduced"], &[]),
    (
        "scale",
        &["100", "150", "300"],
        &["24", "801", "-100", "1e3"],
    ),
    (
        "font.family",
        &["theme", "inter", "noto-serif", "gone-family"],
        &["Inter", "../fonts", ""],
    ),
    (
        "font.size",
        &["theme", "6", "10", "48"],
        &["5", "49", "+10", "10pt"],
    ),
    (
        "cursor.set",
        &["Standard", "High Visibility", "Gone Away"],
        &[".."],
    ),
    (
        "cursor.size",
        &["normal", "large", "larger", "largest"],
        &[],
    ),
    ("cursor.shake", &["true", "false", "on", "off"], &["shaken"]),
    (
        "cursor.trail",
        &["off", "short", "medium", "long"],
        &["endless", "0"],
    ),
    ("cursor.locate", &["true", "false", "on", "off"], &["ctrl"]),
    ("cursor.shadow", &["true", "false", "on", "off"], &["soft"]),
    (
        "notify.enabled",
        &["true", "false", "on", "off"],
        &["maybe"],
    ),
    (
        "notify.sources",
        &[
            "\"\"",
            "com.example.chat:none",
            "os.tairix.netstack:critical com.example.chat:warning",
        ],
        &[
            "com.example.chat:all",
            "com.example.chat:none com.example.chat:critical",
            "Upper.Case:none",
        ],
    ),
    ("pointer.primary", &["left", "right"], &["middle"]),
    (
        "pointer.double_click_ms",
        &["100", "250", "500", "750", "2000"],
        &["99", "2001", "-1"],
    ),
    (
        "pointer.speed",
        &["25", "50", "100", "150", "400"],
        &["24", "401"],
    ),
    ("touchpad.tap", &["true", "false"], &["on", "yes"]),
    (
        "touchpad.natural_scroll",
        &["true", "false"],
        &["1", "natural"],
    ),
    (
        "touchpad.speed",
        &["25", "75", "100", "250", "400"],
        &["24", "401"],
    ),
    (
        "key.repeat_delay_ms",
        &["100", "250", "500", "1000", "2000"],
        &["99", "2001"],
    ),
    (
        "key.repeat_rate",
        &["off", "1", "10", "30", "60"],
        &["0", "61"],
    ),
    (
        "screensaver.after_min",
        &["never", "1", "10", "1440"],
        &["0", "1441"],
    ),
    (
        "screensaver.kind",
        &[
            "blank",
            "dim",
            "slideshow",
            "clock",
            "ribbon",
            "starfield",
            "life",
            "raytrace",
            "retro_games",
            "system_monitor",
        ],
        &[
            "fireworks",
            "Raytrace",
            "horizon",
            "RetroGames",
            "retro-games",
            "monitor",
            "SystemMonitor",
        ],
    ),
    (
        "screensaver.display_off_min",
        &["never", "0", "10", "1440"],
        &["1441", "-1"],
    ),
    (
        "screensaver.slideshow.interval_s",
        &["5", "30", "600", "3600"],
        &["4", "3601", "30s"],
    ),
    (
        "screensaver.slideshow.order",
        &["sequential", "shuffled"],
        &["random"],
    ),
    (
        "screensaver.slideshow.category",
        // The empty value is every category; a quoted name round-trips too.
        &["\"\"", "Nature", "TAIRiX", "\"Gone Away\""],
        &["a/b", ".."],
    ),
    (
        "screensaver.clock.date",
        &["true", "false", "on", "off"],
        &["maybe"],
    ),
    ("screensaver.clock.identity", &["true", "false"], &["1"]),
    ("screensaver.ribbon.date", &["true", "false"], &["someday"]),
    (
        "screensaver.starfield.stars",
        &["sparse", "normal", "dense"],
        &["thousands"],
    ),
    ("screensaver.starfield.warp", &["true", "false"], &["fast"]),
    (
        "screensaver.life.cells",
        &["small", "medium", "large"],
        &["huge"],
    ),
    (
        "screensaver.life.speed",
        &["slow", "normal", "fast"],
        &["ludicrous"],
    ),
    (
        "screensaver.raytrace.cpu",
        &["idle", "performance"],
        &["turbo", "Idle"],
    ),
    (
        "screensaver.raytrace.save",
        &["true", "false", "on", "off"],
        &["yes"],
    ),
    (
        "screensaver.raytrace.detail",
        &["simple", "maximum"],
        &["fine", "Maximum"],
    ),
    (
        "screensaver.retro_games.speed",
        &["slow", "normal", "fast"],
        &["warp"],
    ),
    (
        "screensaver.system_monitor.tasks",
        &["true", "false", "on", "off"],
        &["named"],
    ),
    (
        "lock.after_min",
        &["never", "1", "15", "1440"],
        &["0", "later"],
    ),
    (
        "audio.output",
        &["\"\"", "0000000000000051.0", "ffffffffffffffff.31"],
        &["speakers", "51.0", "0000000000000051.32"],
    ),
    (
        "audio.input",
        &["\"\"", "00a1b2c3d4e5f607.1"],
        &["0000000000000000.0", "00A1B2C3D4E5F607.1"],
    ),
    (
        "audio.levels",
        &[
            "\"\"",
            "0000000000000051.0:-6.5dB",
            "0000000000000051.0:-3dB 0000000000000052.1:-12.25dB",
        ],
        &[
            "0000000000000051.0:3dB",
            "0000000000000051.0:-6",
            "0000000000000051.0:-1dB 0000000000000051.0:-2dB",
        ],
    ),
    (
        "audio.muted",
        &[
            "\"\"",
            "0000000000000051.0",
            "0000000000000051.0 0000000000000052.0",
        ],
        &["0000000000000051.0 0000000000000051.0", "muted"],
    ),
];
const BAD_TOKENS: &[&str] = &["", " ", "has space", "bogus", "relative/path.png"];

/// One document in this many carries a value drawn to be refused.
const POISONED_ONE_IN: usize = 4;

/// A value `key` accepts or, to `refuse`, one it should refuse: its own, or a
/// token few keys take.
fn value_for(rng: &mut Prng, key: &str, refuse: bool) -> &'static str {
    let Some(&(_, accepted, refused)) = KEYS.iter().find(|(name, _, _)| *name == key) else {
        unreachable!("every generated key comes from the table");
    };
    if !refuse {
        return rng.pick(accepted);
    }
    if refused.is_empty() || rng.below(2) == 0 {
        return rng.pick(BAD_TOKENS);
    }
    rng.pick(refused)
}

/// A document of distinct keys, one line of which is poisoned in one document
/// of every [`POISONED_ONE_IN`]. Poisoning a document rather than each line
/// keeps the share accepted whole, and with it the round trip's coverage,
/// from falling as the registry grows.
fn document(rng: &mut Prng) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let mut keys: Vec<&str> = KEYS.iter().map(|(name, _, _)| *name).collect();
    let n = rng.below(keys.len() + 1);
    let poisoned = (n > 0 && rng.below(POISONED_ONE_IN) == 0).then(|| rng.below(n));
    for line in 0..n {
        let index = rng.below(keys.len());
        let key = keys.swap_remove(index);

        if rng.below(10) == 0 {
            out.push_str("# comment\n");
        }
        let value = value_for(rng, key, poisoned == Some(line));
        let _ = writeln!(out, "{key} = {value}");
    }
    out
}

/// Read `doc` both ways and hold every invariant the two readings owe each
/// other. Answers whether the strict reading accepted it.
fn check_round_trip(doc: &str) -> bool {
    // The tolerant reading is total for *every* document the engine can
    // parse, accepted or not, so it is exercised on both branches.
    if let Ok(parsed) = Document::parse(doc) {
        let (lenient, refused) = DesktopSettings::load(&parsed);
        assert!(
            refused.len() <= KEYS.len(),
            "a refusal list longer than the registry"
        );
        // A refused key left its field at the documented default.
        let defaults = DesktopSettings::default();
        for key in refused {
            assert_eq!(
                key.value_of(&lenient),
                key.value_of(&defaults),
                "a refused key did not keep its default"
            );
        }
    }

    let Ok(settings) = merge(&DesktopSettings::default(), doc) else {
        return false;
    };

    let rendered = settings.document().render();
    assert!(
        rendered.len() <= MAX_DOCUMENT_LEN,
        "a rendered document exceeded the document bound"
    );
    let reread =
        merge(&DesktopSettings::default(), &rendered).expect("a rendered document re-reads");
    assert_eq!(settings, reread, "render/merge is not a round trip");
    // The two readings agree on every document the strict one accepts.
    let (lenient, refused) = DesktopSettings::load(&settings.document());
    assert!(refused.is_empty(), "a canonical document refused a key");
    assert_eq!(settings, lenient, "the two readings disagree");
    true
}

/// A key added to the registry without a value table here would be
/// generated by nothing and so fuzzed by nothing, which looks exactly like
/// passing coverage.
#[test]
fn the_generator_names_every_registry_key() {
    let registry: Vec<&str> = tairix_wallpaper::SettingsKey::ALL
        .iter()
        .map(|key| key.name())
        .collect();
    let generated: Vec<&str> = KEYS.iter().map(|(name, _, _)| *name).collect();
    assert_eq!(
        registry, generated,
        "the generator and the registry disagree"
    );
}

#[test]
fn generated_documents_round_trip_through_the_canonical_render() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "generated_documents_round_trip_through_the_canonical_render",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            check_round_trip(&document(&mut rng));
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
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
            let len = rng.below(256);
            for _ in 0..len {
                buf.push(char::from(u8::try_from(rng.below(128)).expect("byte fits")));
            }
            let _ = merge(&DesktopSettings::default(), &buf);
            if let Ok(parsed) = Document::parse(&buf) {
                let _ = DesktopSettings::load(&parsed);
            }
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn the_generator_produces_accepted_documents() {
    const DRAWS: u64 = 2_000;
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_generator_produces_accepted_documents",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let accepted = (0..DRAWS)
        .filter(|_| check_round_trip(&document(&mut rng)))
        .count();
    assert!(
        u64::try_from(accepted).expect("count fits") * 2 >= DRAWS,
        "only {accepted} of {DRAWS} generated documents parsed; the corpus is degenerate"
    );
}
