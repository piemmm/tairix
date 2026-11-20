//! The replay: every session genuine RISC OS 5's BASIC V and BASIC VI had
//! typed at its prompt for `cargo xtask riscos --reference`
//! (`tests/riscos5/sessions.txt`), typed into the interpreter as that harness
//! typed it, and what it printed held to what genuine BASIC printed
//! (`tests/riscos5/typed.txt`), each message told by its kind through the
//! same calibration.

mod common;

use common::{disc_file, run_hosted, DISC};
use tairix_fscopy::fixture::Tree;
use tairix_riscos_basic::corpus::session;

const SESSIONS: &str = include_str!("riscos5/sessions.txt");
const TYPED: &str = include_str!("riscos5/typed.txt");

/// What `command` prints with `keys` typed, as the harness runs it.
fn typed(command: &str, keys: &[u8]) -> Vec<u8> {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/SessKeys,fff"), keys);
    run_hosted(
        &tree,
        &format!("{command} {{ < HostFS:$.SessKeys > HostFS:$.Typed }}"),
    );
    disc_file(&tree, "Typed").unwrap_or_default()
}

#[test]
fn every_session_prints_what_genuine_basic_printed() {
    let sessions = session::sessions(SESSIONS).expect("the corpus");
    let recorded = session::recorded(TYPED).expect("the answers");
    assert!(!sessions.is_empty(), "a corpus");
    let mut differ = Vec::new();
    for (command, dialect) in [("BASIC", "v"), ("BASIC64", "vi")] {
        let messages = session::messages(&typed(command, &session::calibration_keys()))
            .expect("the calibration");
        for s in &sessions {
            let Some(answered) = recorded.iter().find(|a| a.name == s.name) else {
                differ.push(format!("{} {dialect}: not recorded", s.name));
                continue;
            };
            let genuine = if dialect == "v" {
                &answered.v
            } else {
                &answered.vi
            };
            let ours = session::told(
                &typed(command, &session::keys(s.typed.iter().map(Vec::as_slice))),
                &messages,
            );
            if &ours != genuine {
                let at = ours
                    .iter()
                    .zip(genuine)
                    .position(|(a, b)| a != b)
                    .unwrap_or(ours.len().min(genuine.len()));
                differ.push(format!(
                    "{} {dialect}, line {at}:\n  genuine {:?}\n  ours    {:?}",
                    s.name,
                    genuine.get(at),
                    ours.get(at)
                ));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} sessions printed otherwise:\n{}",
        differ.len(),
        differ.join("\n")
    );
}
