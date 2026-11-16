//! The Commodore 64's recorded corpus replayed against the clean-room ROMs:
//! each scenario's screen and disks as its fixture holds them, which
//! `cargo xtask openmicro --reference` recorded from the originals.

use std::sync::OnceLock;

use tairix_openmicro::rom::{c1541, c64 as roms};
use tairix_openmicro::scenario::c64::{self, Roms};
use tairix_openmicro::scenario::{parse_corpus, parse_fixtures};

/// The clean-room ROMs as the tree builds them: the KERNAL, BASIC, the
/// character generator and the 1541's DOS.
fn clean_room() -> Roms<'static> {
    static IMAGES: OnceLock<[Vec<u8>; 4]> = OnceLock::new();
    let [kernal, basic, character, dos] = IMAGES.get_or_init(|| {
        [
            roms::kernal_image,
            roms::basic_image,
            roms::charset::build,
            c1541::build,
        ]
        .map(|build| build().expect("each ROM assembles").bytes)
    });
    Roms {
        kernal,
        basic,
        character,
        dos,
    }
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
    let observed = c64::run(clean_room(), &scenario).expect("the scenario runs");
    assert_eq!(observed.rows, expected.rows, "{name}'s screen");
    assert_eq!(observed.discs, expected.discs, "{name}'s disks");
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

include!(concat!(env!("OUT_DIR"), "/c64_corpus.rs"));
