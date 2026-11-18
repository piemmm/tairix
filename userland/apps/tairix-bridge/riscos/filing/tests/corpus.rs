//! The filing crate's recorded corpus: every scenario has its fixture and
//! every fixture its scenario, in the one format the reference harness
//! writes, and every scenario replayed on a session holding FileSwitch leaves
//! what genuine RISC OS 5 was recorded leaving.

mod common;

use std::path::{Path, PathBuf};

use tairix_riscos_kernel::corpus::{check_group, differences, replay};

use common::kernel;

/// The corpus's groups: each scenarios file and its fixtures.
fn groups() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the corpus")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    paths.sort();
    paths
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_scenario_has_its_fixture_and_every_fixture_its_scenario() {
    let paths = groups();
    assert!(!paths.is_empty(), "a corpus");
    for path in paths {
        let fixtures = path.with_extension("fixtures");
        check_group(&read(&path), &read(&fixtures))
            .unwrap_or_else(|reason| panic!("{}: {reason}", path.display()));
    }
}

#[test]
fn every_scenario_replayed_with_fileswitch_leaves_what_its_fixture_records() {
    let mut differ = Vec::new();
    for path in groups() {
        let group = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let found = differences(
            &group,
            &read(&path),
            &read(&path.with_extension("fixtures")),
            |scenario| replay(&mut kernel(), scenario),
        )
        .unwrap_or_else(|reason| panic!("{reason}"));
        differ.extend(found);
    }
    assert!(
        differ.is_empty(),
        "{} lines differ from genuine RISC OS:\n{}",
        differ.len(),
        differ.join("\n")
    );
}
