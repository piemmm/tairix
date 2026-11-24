//! The kernel's recorded corpus: every scenario has its fixture and every
//! fixture its scenario, in the one format the reference harness writes, and
//! every scenario replayed on the kernel's own SWIs leaves what its fixture
//! records.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tairix_riscos_kernel::corpus::{check_group, differences, replay, StillHost};
use tairix_riscos_kernel::memory::layout::RISC_OS_5;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::os::{Config, Kernel};

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

/// The zone the reference machine's UK territory keeps.
fn uk_time() -> tairix_tz::Zone<'static> {
    static STORE: OnceLock<Vec<u8>> = OnceLock::new();
    static INFO: OnceLock<tairix_tz::ZoneInfo<'static>> = OnceLock::new();
    let bytes = STORE.get_or_init(|| tairix_tzcompile::compile().expect("the store compiles"));
    let info = INFO.get_or_init(|| tairix_tz::ZoneInfo::parse(bytes).expect("the store"));
    info.zone("Europe/London").expect("in the store")
}

/// A kernel booted afresh, as the reference machine's kernel stands.
fn kernel() -> Kernel {
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(StillHost),
        vdu: None,
        modules: &[],
        zone: uk_time(),
        desktop_font: tairix_riscos_kernel::desktop::DesktopFont::SHIPPED,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
        cache_key: tairix_hash::HashSeed::UNKEYED,
    };
    Kernel::new(config).expect("a kernel")
}

#[test]
fn every_scenario_replayed_on_the_kernel_leaves_what_its_fixture_records() {
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
