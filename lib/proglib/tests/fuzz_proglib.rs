//! Deterministic fuzz harness for the program-library catalog registry.
//!
//! The `key = value` grammar is `lib/appconf`'s and is fuzzed there; what
//! this harness holds is the **registry** over it, for any bytes an
//! untrusted catalog document may carry:
//!
//! 1. [`load`] never panics on any document, and an accepted one never
//!    yields more than [`MAX_ENTRIES`] records.
//! 2. [`document`] and [`load`] are inverses: an accepted catalog re-renders
//!    to a document that reads back equal, and the rendered text is itself
//!    one the engine will parse, so a writer can never emit a store the
//!    reader would refuse.
//! 3. [`merge`] never panics for any pair of accepted catalogs, resolves
//!    every patch, stays within the record bound, and resolves visibility
//!    with the overlay's verdict last: an identifier survives exactly when
//!    the last word on it — the user's patch, else the machine's, else its
//!    own declaration — shows it.
//!
//! 4. The account's own record ([`Personal`]) never panics on any stored
//!    value, re-reads what it renders, and stays bounded under any run of
//!    edits, changing only the records each edit reports.
//!
//! The generator emits *whole records* and mutates them at a low rate, so
//! most documents are accepted and the round-trip and merge invariants are
//! genuinely exercised rather than being refused at the first line. The
//! third test then hammers the parser with arbitrary ASCII, where nothing
//! but "does not panic" is expected.
//!
//! The fixed sweep runs under plain `cargo test`; under `cargo xtask fuzz`
//! the same seeded stream keeps being drawn until the budget elapses.

use tairix_appconf::Document;
use tairix_fuzzseed::Prng;
use tairix_proglib::{
    document as catalog_document, load, merge, personal, Catalog, CatalogError, Edit, EntryId,
    EntryPatch, Personal, Record, MAX_ENTRIES,
};

/// Read `text` the way a store's reader does: through the format engine,
/// then the registry. A document the engine itself refuses is reported as a
/// refusal too, so the harness walks one accept/reject boundary rather than
/// two.
fn read(text: &str) -> Result<Catalog, Option<CatalogError>> {
    let parsed = Document::parse(text).map_err(|_| None)?;
    load(&parsed).map_err(Some)
}

/// The canonical rendered text of `catalog`.
fn rendered(catalog: &Catalog) -> String {
    catalog_document(catalog).render()
}

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 5_000;

/// Identifiers a record may be filed under. Drawn without replacement per
/// document so a duplicate key is a *mutation*, not the common case.
const IDS: &[&str] = &[
    "editor",
    "com.example.editor",
    "term-1",
    "a",
    "chess",
    "net.tairix.files",
];
const NAMES: &[&str] = &["Text Editor", "Chess", "F", "Files & Folders"];
const BUNDLES: &[&str] = &[
    "/System/Applications/Editor.app",
    "/System/Applications/games/Chess.app",
    "/Users/ada/Applications/Editor.app",
    "/System/Commands/ls.app",
];
/// Folder identifiers spelled exactly as the closed taxonomy renders them:
/// the decode is case-sensitive, so only these spellings are accepted.
const CATEGORIES: &[&str] = &[
    "Accessories",
    "Graphics",
    "Internet",
    "Multimedia",
    "Office",
    "Programming",
    "Games",
    "SystemTools",
    "Utilities",
    "Other",
];
const ICONS: &[&str] = &["icon.svg", "art/icon.svg", "chess.svg"];
/// Tokens that should be refused wherever they appear.
const BAD_TOKENS: &[&str] = &[
    "",
    " ",
    "has space",
    ".leading",
    "trailing.",
    "utilities",
    "Development",
    "/System/Applications/Editor",
    "System/Applications/Editor.app",
    "/System/Applications//Editor.app",
    "../escape.svg",
    "/absolute.svg",
    "yes",
    "Name",
    "bogus",
];

/// One `<id>.<field> <value>` line, with `value` mutated to a refused token
/// at a low rate so the accept/reject boundary is walked from both sides.
///
/// The rates are per *line*, and a document carries several lines across
/// several records, so they compound: they are kept low enough that the
/// whole document is accepted far more often than not, which is what keeps
/// the round-trip and merge invariants genuinely exercised.
fn line(rng: &mut Prng, id: &str, field: &str, value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let field = if rng.below(64) == 0 {
        rng.pick(BAD_TOKENS)
    } else {
        field
    };
    let value = if rng.below(48) == 0 {
        rng.pick(BAD_TOKENS)
    } else {
        value
    };
    let _ = write!(out, "{id}.{field} = {value}");
    out
}

/// One record: a declared entry (`name` + `bundle`) or a patch (no
/// `bundle`, optionally `hidden`), with the odd stray comment or blank line
/// and the occasional malformed identifier.
fn record(rng: &mut Prng, id: &str, out: &mut String) {
    let id = if rng.below(64) == 0 {
        rng.pick(BAD_TOKENS)
    } else {
        id
    };
    let declared = rng.below(3) != 0;

    if rng.below(6) == 0 {
        out.push_str("# a comment\n");
    }
    let name = rng.pick(NAMES);
    out.push_str(&line(rng, id, "name", name));
    out.push('\n');
    if declared {
        let bundle = rng.pick(BUNDLES);
        out.push_str(&line(rng, id, "bundle", bundle));
        out.push('\n');
    }
    if rng.below(2) == 0 {
        let category = rng.pick(CATEGORIES);
        out.push_str(&line(rng, id, "category", category));
        out.push('\n');
    }
    if rng.below(3) == 0 {
        let icon = rng.pick(ICONS);
        out.push_str(&line(rng, id, "icon", icon));
        out.push('\n');
    }
    // `hidden` is legal on any record — a declaration may suppress itself,
    // a patch carries the overlay's verdict — so emit it on both, walking
    // the flag grammar's accept/reject boundary with the odd bad spelling.
    if rng.below(3) == 0 {
        let flag = if rng.below(8) == 0 {
            "yes"
        } else if rng.below(2) == 0 {
            "false"
        } else {
            "true"
        };
        out.push_str(&line(rng, id, "hidden", flag));
        out.push('\n');
    }
    if rng.below(8) == 0 {
        out.push('\n');
    }
}

fn document(rng: &mut Prng) -> String {
    let mut out = String::new();
    let mut pool: Vec<&str> = IDS.to_vec();
    let records = rng.below(IDS.len() + 1);
    for _ in 0..records {
        // Draw without replacement: a repeated identifier would be a
        // duplicate-key refusal in almost every document otherwise.
        let index = rng.below(pool.len().max(1));
        let Some(id) = pool.get(index).copied() else {
            break;
        };
        pool.swap_remove(index);
        record(rng, id, &mut out);
    }
    out
}

/// The round-trip invariant. Returns whether the document was accepted, so
/// the corpus-quality test can measure the generator.
fn check_round_trip(doc: &str) -> bool {
    let Ok(catalog) = read(doc) else {
        return false;
    };
    assert!(catalog.len() <= MAX_ENTRIES);

    let reread = read(&rendered(&catalog)).expect("a rendered catalog re-reads");
    assert_eq!(catalog, reread, "render/read is not a round trip");
    true
}

fn check_merge(machine: &str, overlay: &str) {
    let (Ok(machine), Ok(overlay)) = (read(machine), read(overlay)) else {
        return;
    };

    let resolved = merge(&machine, &overlay);
    assert!(resolved.len() <= MAX_ENTRIES);
    for (_, record) in resolved.records() {
        assert!(
            matches!(record, Record::Entry(_)),
            "merge left a patch unresolved"
        );
    }

    // The visibility oracle: an identifier resolves to an entry exactly
    // when some document declares it and the last verdict on it — the
    // overlay's patch, else the machine's, else the declaration's own
    // flag — shows it.
    for (id, _) in machine.records().chain(overlay.records()) {
        let declared = overlay.entry(id).or_else(|| machine.entry(id));
        let shown = declared.map(|entry| {
            !overlay
                .entry_patch(id)
                .and_then(EntryPatch::hidden)
                .or_else(|| machine.entry_patch(id).and_then(EntryPatch::hidden))
                .unwrap_or(entry.hidden())
        });
        assert_eq!(
            resolved.entry(id).is_some(),
            shown.unwrap_or(false),
            "merge and the visibility oracle disagree for {id}"
        );
    }

    let rendered = rendered(&resolved);
    let reparsed = read(&rendered).expect("a rendered resolved catalog re-reads");
    assert_eq!(resolved, reparsed);
}

#[test]
fn generated_documents_round_trip_through_the_canonical_render() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "generated_documents_round_trip_through_render",
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
fn merging_resolves_every_patch_and_honours_every_hide() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "merging_resolves_every_patch_and_honours_every_hide",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let machine = document(&mut rng);
            let overlay = document(&mut rng);
            check_merge(&machine, &overlay);
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
                // ASCII — control characters, `.`, `/`, digits, letters —
                // is always valid UTF-8, so `parse` sees a legal `&str`.
                buf.push(char::from(u8::try_from(rng.below(128)).expect("byte fits")));
            }
            let _ = read(&buf);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

/// A generator that almost never produced an accepted document would leave
/// the round-trip and merge invariants untested while still passing, so the
/// corpus itself is asserted on.
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
        u64::try_from(accepted).expect("count fits") * 4 >= DRAWS,
        "only {accepted} of {DRAWS} generated documents parsed; the corpus is degenerate"
    );
}

/// Value tokens a generated personal record is built from: mostly well-formed
/// so the round trip is genuinely exercised, with the grammar's edges mixed in.
const PERSONAL_TOKENS: &[&str] = &[
    "favourite=1",
    "favourite=7",
    "favourite=0",
    "favourite=01",
    "weight=1000 last=5",
    "weight=1000000 last=-9223372036854775808",
    "weight=0 last=1",
    "weight=5",
    "last=5",
    "none",
    "colour=red",
    "",
];

/// One generated stored value: up to three tokens joined by single spaces, now
/// and then by a double one.
fn personal_value(rng: &mut Prng) -> String {
    let mut value = String::new();
    for index in 0..=rng.below(3) {
        if index > 0 {
            value.push_str(if rng.below(16) == 0 { "  " } else { " " });
        }
        value.push_str(rng.pick(PERSONAL_TOKENS));
    }
    value
}

#[test]
fn personal_records_never_panic_and_accepted_ones_round_trip() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "personal_records_never_panic_and_accepted_ones_round_trip",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let stored: Vec<(String, String)> = (0..rng.below(6))
                .map(|_| {
                    let key = format!("library.{}", rng.pick(IDS));
                    (key, personal_value(&mut rng))
                })
                .collect();
            let (personal, _) = Personal::load(
                stored
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            assert!(personal.len() <= personal::MAX_RECORDS);
            let rendered: Vec<(String, String)> = personal
                .keeps()
                .filter_map(|(id, _)| Some((personal::key_of(id), personal.stored(id)?)))
                .collect();
            let (reread, refused) = Personal::load(
                rendered
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            assert!(refused.is_empty(), "a rendered record was refused");
            assert_eq!(reread, personal, "render/read is not a round trip");
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn personal_edits_keep_the_record_bounded_and_readable() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "personal_edits_keep_the_record_bounded_and_readable",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let entries: Vec<EntryId> = (0..personal::MAX_RECORDS + 8)
        .map(|n| EntryId::new(&format!("e{n}")).expect("valid"))
        .collect();
    loop {
        let mut personal = Personal::new();
        for _ in 0..SMOKE_ITERATIONS {
            let entry = rng.pick(&entries).clone();
            let now = i64::from(rng.next_u32()) - i64::from(u32::MAX / 2);
            let edit = match rng.below(8) {
                0..=2 => Edit::Launched(entry, (rng.below(8) != 0).then_some(now)),
                3 => Edit::Favourite(entry),
                4 => Edit::Unfavourite(entry),
                5 => Edit::MoveEarlier(entry),
                6 => Edit::MoveLater(entry),
                _ if rng.below(64) == 0 => Edit::ForgetAll,
                _ => Edit::Forget(entry),
            };
            let before = personal.clone();
            match personal.apply(&edit) {
                Ok(changed) => {
                    // Exactly the reported records may differ.
                    for (id, keep) in before.keeps() {
                        if !changed.contains(id) {
                            assert_eq!(personal.keep(id), keep, "{id} changed unreported");
                        }
                    }
                }
                Err(_) => assert_eq!(personal, before, "a refused edit changed the record"),
            }
            assert!(personal.len() <= personal::MAX_RECORDS);
            let shelf = personal.shelf(Some(now));
            assert!(shelf
                .frequent
                .iter()
                .all(|id| !shelf.favourites.contains(id)));
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
