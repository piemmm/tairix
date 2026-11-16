//! `cargo xtask openmicro --reference <dir>`: the recorded corpora run against
//! the original ROMs a developer holds in `<dir>`, outside the tree — the
//! Model B's as `mos.rom`, `basic.rom` and `dfs.rom`, the Commodore 64's as
//! `c64-kernal.rom`, `c64-basic.rom`, `c64-character.rom` and
//! `c1541-dos.rom` — each corpus whose originals are there; each scenario's
//! observation, its own names put in place of the originals', rewritten as
//! its fixture. A scenario marked `ours` keeps the fixture it has.
//!
//! No fixture may carry the originals' bytes: a run of
//! [`ROM_RUN`](crate::commands::rom_runs::ROM_RUN) bytes of an original ROM in
//! an observation's text, or in the bytes its rows write in hexadecimal or in
//! decimal, refuses the whole recording, unless a clean-room ROM holds that
//! run or the clean-room ROMs show it for the same scenario.

use std::path::{Path, PathBuf};
use std::thread;

use tairix_openmicro::firmware;
use tairix_openmicro::rom::asm::{AsmError, Image};
use tairix_openmicro::scenario::{
    self, c64, format_fixtures, own_names, parse_corpus, parse_fixtures, Observation, Roms,
    Scenario, ScenarioError,
};

use crate::commands::rom_runs::{runs, windows, Runs};
use crate::Context;

/// A ROM the tree builds.
type Build = fn() -> Result<Image, AsmError>;

/// One group's scenarios, its fixtures file and what it now records.
struct Group {
    fixtures: PathBuf,
    recorded: Vec<(String, Observation)>,
}

/// A corpus: where it lies from the workspace's root, the comment its
/// fixtures open with, each original's file with the clean-room ROM it is
/// checked against, how a scenario runs on a set of images, and the
/// clean-room ROMs' names put in the originals'.
struct Corpus<const N: usize> {
    dir: &'static str,
    header: [&'static str; 2],
    roms: [(&'static str, Build); N],
    run: fn(&[Vec<u8>; N], &Scenario) -> Result<Observation, ScenarioError>,
    own_names: fn(&str) -> String,
}

const MODEL_B: Corpus<3> = Corpus {
    dir: "userland/apps/openmicro/tests/corpus",
    header: [
        "Recorded by `cargo xtask openmicro --reference` from MOS 1.20, BASIC II and DFS 1.20,",
        "but for a scenario marked `ours`, whose observation is this machine's own.",
    ],
    roms: [
        (firmware::MOS, tairix_openmicro::rom::mos::model_b),
        (firmware::BASIC, tairix_openmicro::rom::basic::build),
        (firmware::DFS, tairix_openmicro::rom::dfs::build),
    ],
    run: |[os, basic, dfs], scenario| scenario::run(Roms { os, basic, dfs }, scenario),
    own_names,
};

const COMMODORE_64: Corpus<4> = Corpus {
    dir: "userland/apps/openmicro/tests/corpus/c64",
    header: [
        "Recorded by `cargo xtask openmicro --reference` from KERNAL 3, BASIC V2 and 1541 DOS 2.6,",
        "but for a scenario marked `ours`, whose observation is this machine's own.",
    ],
    roms: [
        (
            firmware::C64_KERNAL,
            tairix_openmicro::rom::c64::kernal_image,
        ),
        (firmware::C64_BASIC, tairix_openmicro::rom::c64::basic_image),
        (
            firmware::C64_CHARACTER,
            tairix_openmicro::rom::c64::charset::build,
        ),
        (firmware::C1541_DOS, tairix_openmicro::rom::c1541::build),
    ],
    run: |[kernal, basic, character, dos], scenario| {
        c64::run(
            c64::Roms {
                kernal,
                basic,
                character,
                dos,
            },
            scenario,
        )
    },
    own_names: c64::own_names,
};

pub fn run(ctx: &Context, dir: &Path) -> Result<(), String> {
    let model_b = record(ctx, dir, &MODEL_B)?;
    let commodore = record(ctx, dir, &COMMODORE_64)?;
    if model_b || commodore {
        Ok(())
    } else {
        Err(format!(
            "OpenMicro: {} holds no corpus's original ROMs",
            dir.display()
        ))
    }
}

/// `corpus` recorded from the originals in `dir`: whether they are there,
/// refused where only some are.
fn record<const N: usize>(ctx: &Context, dir: &Path, corpus: &Corpus<N>) -> Result<bool, String> {
    let present = corpus.roms.map(|(name, _)| dir.join(name).exists());
    if present.iter().all(|&held| !held) {
        return Ok(false);
    }
    let mut originals: [Vec<u8>; N] = core::array::from_fn(|_| Vec::new());
    let mut clean: [Vec<u8>; N] = core::array::from_fn(|_| Vec::new());
    for ((original, own), &(name, build)) in originals.iter_mut().zip(&mut clean).zip(&corpus.roms)
    {
        *original = std::fs::read(dir.join(name))
            .map_err(|e| format!("OpenMicro: {}: {e}", dir.join(name).display()))?;
        *own = build()
            .map_err(|e| format!("OpenMicro: {name} does not assemble: {e:?}"))?
            .bytes;
    }
    let original_runs = runs(&originals);
    let clean_runs = runs(&clean);

    let mut groups = Vec::new();
    let mut refused = Vec::new();
    let mut differing = Vec::new();
    for path in group_files(ctx, corpus.dir)? {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("OpenMicro: {}: {e}", path.display()))?;
        let scenarios =
            parse_corpus(&text).map_err(|e| format!("OpenMicro: {}: {e:?}", path.display()))?;
        let fixtures = path.with_extension("fixtures");
        let kept = match std::fs::read_to_string(&fixtures) {
            Ok(text) => parse_fixtures(&text)
                .map_err(|e| format!("OpenMicro: {}: {e:?}", fixtures.display()))?,
            Err(_) => Vec::new(),
        };
        let mut recorded = Vec::new();
        let observed = observe(corpus, (&originals, &clean), &scenarios)?;
        for (scenario, observed) in scenarios.iter().zip(observed) {
            let observation = match observed {
                None => kept
                    .iter()
                    .find(|(name, _)| *name == scenario.name)
                    .map(|(_, observation)| observation.clone())
                    .ok_or_else(|| {
                        format!(
                            "OpenMicro: {} is ours but has no fixture to keep",
                            scenario.name
                        )
                    })?,
                Some((theirs, ours)) => {
                    if leaks(&theirs, &ours, &original_runs, &clean_runs) {
                        refused.push(scenario.name.clone());
                    }
                    if theirs != ours {
                        differing.push(scenario.name.clone());
                    }
                    theirs
                }
            };
            recorded.push((scenario.name.clone(), observation));
        }
        groups.push(Group { fixtures, recorded });
    }
    if !refused.is_empty() {
        return Err(format!(
            "OpenMicro: nothing written: the originals' ROM bytes would enter the fixtures of {}",
            refused.join(", ")
        ));
    }
    let mut written = 0;
    for group in &groups {
        let text = format_fixtures(&corpus.header, &group.recorded);
        std::fs::write(&group.fixtures, text)
            .map_err(|e| format!("OpenMicro: {}: {e}", group.fixtures.display()))?;
        written += group.recorded.len();
    }
    noteln!(
        "xtask: [openmicro --reference] {written} fixture(s) in {} file(s) of {}",
        groups.len(),
        corpus.dir
    );
    if !differing.is_empty() {
        noteln!(
            "xtask: [openmicro --reference] the clean-room ROMs differ in {}",
            differing.join(", ")
        );
    }
    Ok(true)
}

/// A corpus's `*.scenarios` files, in name order.
fn group_files(ctx: &Context, corpus: &str) -> Result<Vec<PathBuf>, String> {
    let dir = ctx.workspace_root.join(corpus);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("OpenMicro: {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    files.sort();
    Ok(files)
}

/// Each scenario as the originals and the clean-room ROMs leave it, the
/// originals' names put in its own; none for one marked `ours`. The
/// scenarios run on every core.
fn observe<const N: usize>(
    corpus: &Corpus<N>,
    (theirs, ours): (&[Vec<u8>; N], &[Vec<u8>; N]),
    scenarios: &[Scenario],
) -> Result<Vec<Option<(Observation, Observation)>>, String> {
    let cores = thread::available_parallelism().map_or(1, usize::from);
    let share = scenarios.len().div_ceil(cores).max(1);
    let observed = thread::scope(|scope| {
        let workers: Vec<_> = scenarios
            .chunks(share)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|scenario| both(corpus, (theirs, ours), scenario))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| String::from("OpenMicro: a scenario's run panicked"))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    observed.into_iter().flatten().collect()
}

fn both<const N: usize>(
    corpus: &Corpus<N>,
    (theirs, ours): (&[Vec<u8>; N], &[Vec<u8>; N]),
    scenario: &Scenario,
) -> Result<Option<(Observation, Observation)>, String> {
    if scenario.ours {
        return Ok(None);
    }
    let failed = |e| format!("OpenMicro: {}: {e:?}", scenario.name);
    let mut original = (corpus.run)(theirs, scenario).map_err(failed)?;
    original.rows = original
        .rows
        .iter()
        .map(|row| (corpus.own_names)(row))
        .collect();
    let clean = (corpus.run)(ours, scenario).map_err(failed)?;
    Ok(Some((original, clean)))
}

/// Whether `theirs` carries a run of an original ROM's bytes that neither a
/// clean-room ROM nor `ours`, the clean-room ROMs' observation, holds.
fn leaks(theirs: &Observation, ours: &Observation, originals: &Runs, clean: &Runs) -> bool {
    let shown = runs(&streams(ours));
    streams(theirs)
        .iter()
        .flat_map(|stream| windows(stream))
        .any(|run| originals.contains(&run) && !clean.contains(&run) && !shown.contains(&run))
}

/// An observation's text as bytes, its rows run together, and the bytes its
/// rows write as words: two-digit hexadecimal, as a monitor dumps them, and
/// decimal of up to three digits, as BASIC V2 prints a PEEK.
fn streams(observation: &Observation) -> [Vec<u8>; 3] {
    let text = observation
        .rows
        .iter()
        .flat_map(|row| row.chars())
        .filter_map(|ch| u8::try_from(ch).ok())
        .collect();
    let words = |byte: fn(&str) -> Option<u8>| {
        observation
            .rows
            .iter()
            .flat_map(|row| row.split(' '))
            .filter_map(byte)
            .collect()
    };
    let hexadecimal = words(|word| {
        (word.len() == 2)
            .then(|| u8::from_str_radix(word, 16).ok())
            .flatten()
    });
    let decimal = words(|word| {
        (1..=3)
            .contains(&word.len())
            .then(|| word.parse().ok())
            .flatten()
    });
    [text, hexadecimal, decimal]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(rows: &[&str]) -> Observation {
        Observation {
            rows: rows.iter().map(|row| String::from(*row)).collect(),
            discs: Vec::new(),
        }
    }

    #[test]
    fn a_run_of_an_original_rom_only_its_screen_shows_is_a_leak() {
        let original = b"\x00SECRET ROM TEXT HERE\x00";
        let originals = runs(&[&original[..]]);
        let clean = runs(&[&b"our own ROM, nothing alike"[..]]);
        let theirs = observed(&["SECRET ROM TEX", "T HERE"]);
        assert!(
            leaks(&theirs, &observed(&[">"]), &originals, &clean),
            "across a row's end"
        );
        assert!(
            !leaks(&theirs, &theirs.clone(), &originals, &clean),
            "the clean-room ROMs show it too"
        );
        let shared = runs(&[&original[..]]);
        assert!(
            !leaks(&theirs, &observed(&[">"]), &originals, &shared),
            "a clean-room ROM holds it"
        );
    }

    #[test]
    fn bytes_written_in_hex_are_runs_too() {
        let original: Vec<u8> = (0x40..0x50).collect();
        let originals = runs(&[&original[..]]);
        let clean = runs(&[&b"nothing like it at all"[..]]);
        let dumped = observed(&[
            "0000 40 41 42 43 44 45 46 47 @ABCDEFG",
            "0008 48 49 4A 4B 4C 4D 4E 4F HIJKLMNO",
        ]);
        assert!(leaks(&dumped, &observed(&[">"]), &originals, &clean));
    }

    #[test]
    fn bytes_printed_in_decimal_are_runs_too() {
        let original: Vec<u8> = (0xA0..0xB0).collect();
        let originals = runs(&[&original[..]]);
        let clean = runs(&[&b"nothing like it at all"[..]]);
        let peeked: Vec<String> = original.iter().map(|byte| format!(" {byte}")).collect();
        let peeked: Vec<&str> = peeked.iter().map(String::as_str).collect();
        assert!(
            leaks(&observed(&peeked), &observed(&[">"]), &originals, &clean),
            "a PEEK of each byte in turn"
        );
    }
}
