//! The replay: every program genuine RISC OS 5's BASIC V and BASIC VI ran
//! for `cargo xtask riscos --reference` (`tests/riscos5/programs.txt`), run
//! by the interpreter as that harness ran it, its output and input
//! redirected to files of a HostFS disc, and what it printed held to what
//! genuine BASIC printed (`tests/riscos5/printed.txt`), byte for byte.

mod common;

use std::collections::HashMap;

use common::{disc_file, run_hosted, unquote, DISC};
use tairix_fscopy::fixture::Tree;
use tairix_riscos_basic::corpus::language::Program;

const PROGRAMS: &str = include_str!("riscos5/programs.txt");
const PRINTED: &str = include_str!("riscos5/printed.txt");

/// Each dialect's command and its name in the recording.
const DIALECTS: [(&str, &str); 2] = [("BASIC", "v"), ("BASIC64", "vi")];

/// The programs of the corpus.
fn programs() -> Vec<Program> {
    tairix_riscos_basic::corpus::language::programs(PROGRAMS).expect("the corpus")
}

/// What each dialect printed for each program, by the program's name and
/// the dialect's.
fn printed() -> HashMap<(String, String), Vec<u8>> {
    let mut printed: HashMap<(String, String), Vec<u8>> = HashMap::new();
    let mut program = String::new();
    for line in PRINTED.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("program ") {
            program = name.into();
            continue;
        }
        let (dialect, text) = line.split_once(' ').expect("a dialect and its text");
        printed
            .entry((program.clone(), dialect.to_string()))
            .or_default()
            .extend(unquote(text));
    }
    printed
}

/// What `program` prints run as the harness ran it under `command`.
fn run(command: &str, program: &Program) -> Vec<u8> {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/Lang,fff"), &program.text)
        .file(&format!("{DISC}/Keys,fff"), &program.input);
    let input = if program.input.is_empty() {
        ""
    } else {
        "< HostFS:$.Keys "
    };
    run_hosted(
        &tree,
        &format!("{command} -quit HostFS:$.Lang {{ {input}> HostFS:$.Printed }}"),
    );
    disc_file(&tree, "Printed").unwrap_or_default()
}

#[test]
fn every_program_prints_what_genuine_basic_printed() {
    let printed = printed();
    let programs = programs();
    assert!(!programs.is_empty(), "a corpus");
    let mut differ = Vec::new();
    for program in &programs {
        for (command, dialect) in DIALECTS {
            let Some(genuine) = printed.get(&(program.name.clone(), dialect.to_string())) else {
                differ.push(format!("{} {dialect}: not recorded", program.name));
                continue;
            };
            let ours = run(command, program);
            if &ours != genuine {
                differ.push(format!(
                    "{} {dialect}:\n  genuine {:?}\n  ours    {:?}",
                    program.name,
                    String::from_utf8_lossy(genuine),
                    String::from_utf8_lossy(&ours)
                ));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} of {} runs printed otherwise:\n{}",
        differ.len(),
        2 * programs.len(),
        differ.join("\n")
    );
}
