//! The recorded corpus replayed against the clean-room ROMs: each
//! scenario's screen and discs as its fixture holds them, which
//! `cargo xtask beeb --reference` recorded from the originals.

use std::sync::OnceLock;

use tairix_beeb::scenario::{self, parse_corpus, parse_fixtures, Roms};

/// The clean-room ROMs as the tree builds them: the OS, BASIC and the DFS.
fn clean_room() -> Roms<'static> {
    static IMAGES: OnceLock<[Vec<u8>; 3]> = OnceLock::new();
    let [os, basic, dfs] = IMAGES.get_or_init(|| {
        [
            tairix_beeb::rom::mos::build,
            tairix_beeb::rom::basic::build,
            tairix_beeb::rom::dfs::build,
        ]
        .map(|build| build().expect("each ROM assembles").bytes)
    });
    Roms { os, basic, dfs }
}

fn replay(scenarios: &str, fixtures: &str, name: &str) {
    let scenario = parse_corpus(scenarios)
        .expect("the corpus reads")
        .into_iter()
        .find(|scenario| scenario.name == name)
        .expect("the scenario is in its file");
    let (_, expected) = parse_fixtures(fixtures)
        .expect("the fixtures read")
        .into_iter()
        .find(|(fixture, _)| fixture == name)
        .unwrap_or_else(|| panic!("{name} has a fixture"));
    let observed = scenario::run(clean_room(), &scenario).expect("the scenario runs");
    assert_eq!(observed.rows, expected.rows, "{name}'s screen");
    assert_eq!(observed.discs, expected.discs, "{name}'s discs");
}

#[test]
fn each_scenario_has_one_fixture_and_each_fixture_a_scenario() {
    for (scenarios, fixtures) in GROUPS {
        let names: Vec<String> = parse_corpus(scenarios)
            .expect("the corpus reads")
            .into_iter()
            .map(|scenario| scenario.name)
            .collect();
        let fixed: Vec<String> = parse_fixtures(fixtures)
            .expect("the fixtures read")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, fixed);
    }
}

include!(concat!(env!("OUT_DIR"), "/corpus.rs"));
