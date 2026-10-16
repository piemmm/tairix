use super::*;

#[test]
fn a_reserved_window_grows_to_the_widest_run_and_answers_each_its_own_span() {
    let mut window = RunWindow::default();
    assert!(window.is_empty(), "nothing is held until a run asks");
    assert_eq!(window.reserve(4096).map(|run| run.len()), Some(4096));
    window.reserve(4096).expect("held").fill(0xAA);
    assert_eq!(window.reserve(1024).map(|run| run.len()), Some(1024));
    assert_eq!(
        window.buf().map(|buf| buf.len()),
        Some(4096),
        "a narrower run keeps the wider buffer"
    );
    let grown = window.reserve(8192).expect("grown");
    assert_eq!(grown.len(), 8192);
    assert!(
        grown.iter().all(|&byte| byte == 0),
        "growing carries nothing of what the window held"
    );
}
