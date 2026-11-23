//! The replay: every case genuine RISC OS 5's BASIC V ran through the Draw
//! module for `cargo xtask riscos --reference` (`tests/riscos5/screens.txt`),
//! framed and run in order in one session as that harness ran them, and
//! what each read and drew held to what genuine RISC OS recorded
//! (`tests/riscos5/drawn.txt`), every reading and every byte of the screen.

#[path = "../../vdu/tests/replay/mod.rs"]
mod replay;

const CASES: &str = include_str!("riscos5/screens.txt");
const DRAWN: &str = include_str!("riscos5/drawn.txt");

#[test]
fn every_draw_screen_is_read_and_drawn_as_genuine_risc_os_did() {
    let wrong = replay::differences(CASES, DRAWN);
    assert!(
        wrong.is_empty(),
        "{} cases differ from genuine RISC OS:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
