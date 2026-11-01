//! Deterministic fuzz harness for the RISC OS application scan: the static
//! `!Boot` reading that finds the sprite files an application's icon is drawn
//! from, and the sprite names it is sought by.
//!
//! A `!Boot` comes off whatever volume is browsed, so it is hostile text. The
//! invariants driven here:
//!
//! * [`boot_sprite_files`] never panics, names at most [`MAX_SPRITE_FILES`]
//!   files, each once whatever its case, and every one lies inside the
//!   application ([`is_application_path`]), however its macros nest.
//! * [`SpriteNames::of`] never panics, and a name it accepts is two sprite
//!   names of printable lower-case ASCII, the small one the large one's `sm`
//!   form.
//!
//! The run aborting *is* the failure. A plain `cargo test` runs the smoke
//! sweep; `cargo xtask fuzz` extends the loop to a wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_icon::riscos::{
    boot_sprite_files, is_application_path, MAX_BOOT_BYTES, MAX_SPRITE_FILES,
};
use tairix_icon::SpriteNames;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Boot files of the shapes applications ship, as mutation seeds.
const SEEDS: [&str; 6] = [
    "| !Boot file for !Edit\nSet Edit$Dir <Obey$Dir>\nIconSprites <Edit$Dir>.!Sprites\n",
    "If \"<Edit$Dir>\"=\"\" Then Set Edit$Dir <Obey$Dir>\nIconSprites <Edit$Dir>.Icons",
    "SetMacro Here <Obey$Dir>\nIconSprites <Here>.!Sprites\nUnset Here",
    "Set Edit$Path <Obey$Dir>.,<Obey$Dir>.Resources.\nIconSprites Edit:!Sprites",
    "If \"<Mode>\"<>\"wide\" Then IconSprites <Obey$Dir>.Wide Else IconSprites <Obey$Dir>.Narrow",
    "%IconSprites <Obey$Dir>.Res/ources.!Sprites\r\n*iconsprites <obey$dir>.Themes.!Sprites",
];

/// The words a mutation splices in, so the scan's keywords and expansions
/// are met in orders no seed has.
const WORDS: [&str; 14] = [
    "IconSprites ",
    "Set ",
    "SetMacro ",
    "Unset ",
    "If ",
    " Then ",
    " Else ",
    "<Obey$Dir>",
    "<A>",
    "A$Path ",
    "A:",
    ".",
    "\"",
    "\n",
];

/// Assert what `boot_sprite_files` promises of `boot`, whatever it holds.
fn exercise_boot(boot: &[u8]) {
    let files = boot_sprite_files(boot);
    if boot.len() > MAX_BOOT_BYTES {
        assert!(files.is_empty(), "a boot file past its bound is read");
    }
    assert!(files.len() <= MAX_SPRITE_FILES, "{} files", files.len());
    for (at, file) in files.iter().enumerate() {
        assert!(
            !files[at + 1..]
                .iter()
                .any(|later| later.eq_ignore_ascii_case(file)),
            "{file:?} named twice"
        );
        assert!(is_application_path(file), "{file:?} leaves the application");
    }
}

/// Assert what `SpriteNames::of` promises of `name`.
fn exercise_names(name: &str) {
    let Some(names) = SpriteNames::of(name) else {
        return;
    };
    for sprite in [names.large(), names.small()] {
        assert!(!sprite.is_empty() && sprite.len() <= 12, "{sprite:?}");
        assert!(
            sprite
                .iter()
                .all(|&byte| byte.is_ascii_graphic() && !byte.is_ascii_uppercase()),
            "{sprite:?}"
        );
    }
    assert!(names.small().starts_with(b"sm"));
    let shared = names.large().len().min(names.small().len() - 2);
    assert_eq!(names.small()[2..2 + shared], names.large()[..shared]);
}

/// `seed` with words spliced in and bytes changed at random: mostly to other
/// printable text, which the scan reads, and now and then to anything, which
/// it refuses whole.
fn mutated(seed: &str, rng: &mut Prng) -> Vec<u8> {
    let mut bytes = seed.as_bytes().to_vec();
    for _ in 0..rng.at_most(4) {
        let at = rng.at_most(bytes.len());
        let word = rng.pick(&WORDS).as_bytes();
        bytes.splice(at..at, word.iter().copied());
    }
    for _ in 0..rng.at_most(6) {
        if bytes.is_empty() {
            break;
        }
        let pos = rng.below(bytes.len());
        bytes[pos] = if rng.below(8) == 0 {
            rng.next_u8()
        } else {
            b' ' + rng.next_u8() % (b'~' - b' ' + 1)
        };
    }
    bytes
}

#[test]
fn scanning_any_boot_file_names_only_files_inside_the_application() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "scanning_any_boot_file_names_only_files_inside_the_application",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = *rng.pick(&SEEDS);
        let boot = mutated(seed, &mut rng);
        exercise_boot(&boot);
        exercise_boot(&boot[..rng.at_most(boot.len())]);

        let mut noise = vec![0u8; rng.at_most(600)];
        rng.fill(&mut noise);
        exercise_boot(&noise);

        // A whole bound's worth costs a scan of it, so only now and then.
        if iteration.is_multiple_of(256) {
            let mut long = boot.repeat(MAX_BOOT_BYTES / boot.len().max(1) + 1);
            long.truncate(MAX_BOOT_BYTES + rng.at_most(1));
            exercise_boot(&long);
        }

        let name: String = (0..rng.at_most(20))
            .map(|_| char::from(rng.next_u8() & 0x7F))
            .collect();
        exercise_names(&name);
        exercise_names(&String::from_utf8_lossy(&noise[..noise.len().min(16)]));

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
