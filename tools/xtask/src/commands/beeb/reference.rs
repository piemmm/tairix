//! `cargo xtask beeb --reference <dir>`: the recorded corpus run against the
//! original ROMs a developer holds in `<dir>`, outside the tree, as
//! `mos.rom`, `basic.rom` and `dfs.rom`; each scenario's observation, its
//! own names put in place of the originals', rewritten as its fixture. A
//! scenario marked `ours` keeps the fixture it has.
//!
//! No fixture may carry the originals' bytes: a run of [`ROM_RUN`] bytes of
//! an original ROM in an observation's text, or in the bytes its rows write
//! in hexadecimal, refuses the whole recording, unless a clean-room ROM
//! holds that run or the clean-room ROMs show it for the same scenario.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread;

use tairix_beeb::firmware;
use tairix_beeb::scenario::{
    self, format_fixtures, own_names, parse_corpus, parse_fixtures, Observation, Roms, Scenario,
};

use crate::Context;

/// The corpus, from the workspace's root.
const CORPUS: &str = "userland/apps/beeb/tests/corpus";

/// The comment each fixtures file opens with.
const HEADER: [&str; 2] = [
    "Recorded by `cargo xtask beeb --reference` from MOS 1.20, BASIC II and DFS 1.20,",
    "but for a scenario marked `ours`, whose observation is this machine's own.",
];

/// The shortest run of an original ROM's bytes a fixture may not carry.
const ROM_RUN: usize = 12;

/// The runs of [`ROM_RUN`] bytes in a set of images.
type Runs = HashSet<[u8; ROM_RUN]>;

/// One group's scenarios, its fixtures file and what it now records.
struct Group {
    fixtures: PathBuf,
    recorded: Vec<(String, Observation)>,
}

pub fn run(ctx: &Context, dir: &Path) -> Result<(), String> {
    let originals = [firmware::MOS, firmware::BASIC, firmware::DFS].map(|name| {
        std::fs::read(dir.join(name))
            .map_err(|e| format!("beeb: {}: {e}", dir.join(name).display()))
    });
    let [os, basic, dfs] = originals;
    let (os, basic, dfs) = (os?, basic?, dfs?);
    let clean = [
        tairix_beeb::rom::mos::build,
        tairix_beeb::rom::basic::build,
        tairix_beeb::rom::dfs::build,
    ]
    .map(|build| build().map(|image| image.bytes));
    let [clean_os, clean_basic, clean_dfs] = clean;
    let (clean_os, clean_basic, clean_dfs) = (
        clean_os.map_err(|e| format!("beeb: the MOS does not assemble: {e:?}"))?,
        clean_basic.map_err(|e| format!("beeb: BASIC does not assemble: {e:?}"))?,
        clean_dfs.map_err(|e| format!("beeb: the DFS does not assemble: {e:?}"))?,
    );
    let theirs = Roms {
        os: &os,
        basic: &basic,
        dfs: &dfs,
    };
    let ours = Roms {
        os: &clean_os,
        basic: &clean_basic,
        dfs: &clean_dfs,
    };
    let original_runs = runs([&os[..], &basic, &dfs]);
    let clean_runs = runs([&clean_os[..], &clean_basic, &clean_dfs]);

    let mut groups = Vec::new();
    let mut refused = Vec::new();
    let mut differing = Vec::new();
    for path in group_files(ctx)? {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("beeb: {}: {e}", path.display()))?;
        let scenarios =
            parse_corpus(&text).map_err(|e| format!("beeb: {}: {e:?}", path.display()))?;
        let fixtures = path.with_extension("fixtures");
        let kept = match std::fs::read_to_string(&fixtures) {
            Ok(text) => {
                parse_fixtures(&text).map_err(|e| format!("beeb: {}: {e:?}", fixtures.display()))?
            }
            Err(_) => Vec::new(),
        };
        let mut recorded = Vec::new();
        for (scenario, observed) in scenarios.iter().zip(observe(theirs, ours, &scenarios)?) {
            let observation = match observed {
                None => kept
                    .iter()
                    .find(|(name, _)| *name == scenario.name)
                    .map(|(_, observation)| observation.clone())
                    .ok_or_else(|| {
                        format!("beeb: {} is ours but has no fixture to keep", scenario.name)
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
            "beeb: nothing written: the originals' ROM bytes would enter the fixtures of {}",
            refused.join(", ")
        ));
    }
    let mut written = 0;
    for group in &groups {
        let text = format_fixtures(&HEADER, &group.recorded);
        std::fs::write(&group.fixtures, text)
            .map_err(|e| format!("beeb: {}: {e}", group.fixtures.display()))?;
        written += group.recorded.len();
    }
    noteln!(
        "xtask: [beeb --reference] {written} fixture(s) in {} file(s)",
        groups.len()
    );
    if !differing.is_empty() {
        noteln!(
            "xtask: [beeb --reference] the clean-room ROMs differ in {}",
            differing.join(", ")
        );
    }
    Ok(())
}

/// The corpus's `*.scenarios` files, in name order.
fn group_files(ctx: &Context) -> Result<Vec<PathBuf>, String> {
    let dir = ctx.workspace_root.join(CORPUS);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("beeb: {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    files.sort();
    Ok(files)
}

/// Each scenario as the originals and the clean-room ROMs leave it, the
/// originals' names put in its own; none for one marked `ours`. The
/// scenarios run on every core.
fn observe(
    theirs: Roms<'_>,
    ours: Roms<'_>,
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
                        .map(|scenario| both(theirs, ours, scenario))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| String::from("beeb: a scenario's run panicked"))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    observed.into_iter().flatten().collect()
}

fn both(
    theirs: Roms<'_>,
    ours: Roms<'_>,
    scenario: &Scenario,
) -> Result<Option<(Observation, Observation)>, String> {
    if scenario.ours {
        return Ok(None);
    }
    let failed = |e| format!("beeb: {}: {e:?}", scenario.name);
    let mut original = scenario::run(theirs, scenario).map_err(failed)?;
    original.rows = original
        .rows
        .iter()
        .map(String::as_str)
        .map(own_names)
        .collect();
    let clean = scenario::run(ours, scenario).map_err(failed)?;
    Ok(Some((original, clean)))
}

/// Every run of [`ROM_RUN`] bytes in `images`.
fn runs<const N: usize>(images: [&[u8]; N]) -> Runs {
    images
        .iter()
        .flat_map(|image| image.windows(ROM_RUN))
        .filter_map(|run| <[u8; ROM_RUN]>::try_from(run).ok())
        .collect()
}

/// Whether `theirs` carries a run of an original ROM's bytes that neither a
/// clean-room ROM nor `ours`, the clean-room ROMs' observation, holds.
fn leaks(theirs: &Observation, ours: &Observation, originals: &Runs, clean: &Runs) -> bool {
    let shown: Runs = streams(ours)
        .iter()
        .flat_map(|stream| stream.windows(ROM_RUN))
        .filter_map(|run| <[u8; ROM_RUN]>::try_from(run).ok())
        .collect();
    streams(theirs)
        .iter()
        .flat_map(|stream| stream.windows(ROM_RUN))
        .filter_map(|run| <[u8; ROM_RUN]>::try_from(run).ok())
        .any(|run| originals.contains(&run) && !clean.contains(&run) && !shown.contains(&run))
}

/// An observation's text as bytes, its rows run together, and the bytes its
/// rows write as two-digit hexadecimal words.
fn streams(observation: &Observation) -> [Vec<u8>; 2] {
    let text = observation
        .rows
        .iter()
        .flat_map(|row| row.chars())
        .filter_map(|ch| u8::try_from(ch).ok())
        .collect();
    let words = observation
        .rows
        .iter()
        .flat_map(|row| row.split(' '))
        .filter(|word| word.len() == 2)
        .filter_map(|word| u8::from_str_radix(word, 16).ok())
        .collect();
    [text, words]
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
        let originals = runs([&original[..]]);
        let clean = runs([&b"our own ROM, nothing alike"[..]]);
        let theirs = observed(&["SECRET ROM TEX", "T HERE"]);
        assert!(
            leaks(&theirs, &observed(&[">"]), &originals, &clean),
            "across a row's end"
        );
        assert!(
            !leaks(&theirs, &theirs.clone(), &originals, &clean),
            "the clean-room ROMs show it too"
        );
        let shared = runs([&original[..]]);
        assert!(
            !leaks(&theirs, &observed(&[">"]), &originals, &shared),
            "a clean-room ROM holds it"
        );
    }

    #[test]
    fn bytes_written_in_hex_are_runs_too() {
        let original: Vec<u8> = (0x40..0x50).collect();
        let originals = runs([&original[..]]);
        let clean = runs([&b"nothing like it at all"[..]]);
        let dumped = observed(&[
            "0000 40 41 42 43 44 45 46 47 @ABCDEFG",
            "0008 48 49 4A 4B 4C 4D 4E 4F HIJKLMNO",
        ]);
        assert!(leaks(&dumped, &observed(&[">"]), &originals, &clean));
    }
}
