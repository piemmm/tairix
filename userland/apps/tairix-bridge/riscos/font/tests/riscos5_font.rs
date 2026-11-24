//! The replay: every case genuine RISC OS 5's BASIC V ran through the Font
//! Manager for `cargo xtask riscos --reference` (`tests/riscos5/screens.txt`),
//! framed and run in order in one session as that harness ran them, the
//! first-party font library on the disc where it was staged, and what each
//! read and painted held to what genuine RISC OS recorded
//! (`tests/riscos5/drawn.txt`).

#[path = "../../vdu/tests/replay/mod.rs"]
mod replay;

use std::sync::LazyLock;

use tairix_riscos_font::build::{corpus_library, CORPUS_LIBRARY};
use tairix_riscos_kernel::module::Builtin;

const CASES: &str = include_str!("riscos5/screens.txt");
const DRAWN: &str = include_str!("riscos5/drawn.txt");

/// The screen replay's modules and the Font Manager's.
static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    let mut modules = replay::MODULES.clone();
    modules.extend(tairix_riscos_font::MODULES);
    modules
});

#[test]
fn every_font_screen_is_read_and_painted_as_genuine_risc_os_did() {
    let library: Vec<_> = corpus_library()
        .into_iter()
        .map(|(path, bytes)| (format!("{CORPUS_LIBRARY}/{path}"), bytes))
        .collect();
    let wrong = replay::differences(CASES, DRAWN, MODULES.as_slice(), &library);
    assert!(
        wrong.is_empty(),
        "{} cases differ from genuine RISC OS:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
