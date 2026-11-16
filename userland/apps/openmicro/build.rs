//! Enables the `freestanding` cfg on a bare-metal target, and lists the
//! recorded corpora's scenarios as tests: the Model B's and the Commodore
//! 64's.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

/// Each corpus, from the manifest's directory, and the file in `OUT_DIR` its
/// tests are listed in.
const CORPORA: [(&str, &str); 2] = [
    ("tests/corpus", "corpus.rs"),
    ("tests/corpus/c64", "c64_corpus.rs"),
];

fn main() -> Result<(), String> {
    tairix_buildcfg::emit_freestanding();
    for (corpus, listed) in CORPORA {
        corpus_tests(corpus, listed)?;
    }
    Ok(())
}

/// `listed` in `OUT_DIR`: for each of `corpus`'s `*.scenarios` files a
/// module holding its text and its fixtures' and a test for each scenario it
/// holds, and `GROUPS`, every group's two texts.
fn corpus_tests(corpus: &str, listed: &str) -> Result<(), String> {
    let manifest = env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?;
    let dir = Path::new(&manifest).join(corpus);
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut groups: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    groups.sort();
    let mut code = String::new();
    let mut groups_listed = String::new();
    for scenarios in &groups {
        let fixtures = scenarios.with_extension("fixtures");
        println!("cargo:rerun-if-changed={}", scenarios.display());
        println!("cargo:rerun-if-changed={}", fixtures.display());
        let group = scenarios
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| is_identifier(stem))
            .ok_or_else(|| format!("{}: not a group name", scenarios.display()))?;
        let text =
            fs::read_to_string(scenarios).map_err(|e| format!("{}: {e}", scenarios.display()))?;
        let [scenarios_path, fixtures_path] = [scenarios, &fixtures].map(|path| path.to_str());
        let (Some(scenarios_path), Some(fixtures_path)) = (scenarios_path, fixtures_path) else {
            return Err(format!("{}: not a UTF-8 path", scenarios.display()));
        };
        let _ = writeln!(
            code,
            "mod {group} {{\n    pub(super) const SCENARIOS: &str = include_str!({scenarios_path:?});\n    pub(super) const FIXTURES: &str = include_str!({fixtures_path:?});"
        );
        for name in text
            .lines()
            .filter_map(|line| line.strip_prefix("scenario "))
        {
            if !is_identifier(name) {
                return Err(format!(
                    "{}: {name:?} is not a test name",
                    scenarios.display()
                ));
            }
            let _ = writeln!(
                code,
                "    #[test]\n    fn {name}() {{\n        super::replay(SCENARIOS, FIXTURES, {name:?});\n    }}"
            );
        }
        code.push_str("}\n");
        let _ = write!(groups_listed, "({group}::SCENARIOS, {group}::FIXTURES), ");
    }
    let _ = writeln!(code, "const GROUPS: &[(&str, &str)] = &[{groups_listed}];");
    let out = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?).join(listed);
    fs::write(&out, code).map_err(|e| format!("{}: {e}", out.display()))
}

fn is_identifier(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_lowercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}
