//! The recorded corpus: every scenario parses and has exactly one fixture,
//! every fixture a scenario and records just what its shows ask, and each
//! fixtures file is in the one format the reference harness writes.
//! Replaying the scenarios on the kernel's own SWIs is the SWI layer's.

use std::path::Path;

use tairix_riscos_kernel::corpus::{
    check_observation, format_observations, parse_observations, parse_scenarios,
};

#[test]
fn every_scenario_has_its_fixture_and_every_fixture_its_scenario() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut groups = 0;
    for entry in std::fs::read_dir(&dir).expect("the corpus") {
        let path = entry.expect("an entry").path();
        if path.extension().is_none_or(|ext| ext != "scenarios") {
            continue;
        }
        groups += 1;
        let scenarios = parse_scenarios(&std::fs::read_to_string(&path).expect("scenarios"))
            .unwrap_or_else(|e| panic!("{}:{}: {}", path.display(), e.line, e.reason));
        let fixtures_path = path.with_extension("fixtures");
        let text = std::fs::read_to_string(&fixtures_path).expect("its fixtures");
        let fixtures = parse_observations(&text)
            .unwrap_or_else(|e| panic!("{}:{}: {}", fixtures_path.display(), e.line, e.reason));
        let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
        let fixed: Vec<&str> = fixtures.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            fixed,
            "{}: one fixture per scenario, in order",
            path.display()
        );
        for (scenario, fixture) in scenarios.iter().zip(&fixtures) {
            check_observation(scenario, fixture)
                .unwrap_or_else(|e| panic!("{}: its line {}: {}", scenario.name, e.line, e.reason));
        }
        let header: Vec<&str> = text
            .lines()
            .take_while(|line| line.starts_with("# "))
            .map(|line| &line[2..])
            .collect();
        assert_eq!(
            format_observations(&header, &fixtures),
            text,
            "{}: as the harness writes it",
            fixtures_path.display()
        );
    }
    assert!(groups > 0, "a corpus");
}
