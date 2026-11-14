use super::*;
use alloc::string::String;
use alloc::vec;

fn waiting() -> (LastScreen, Suspended) {
    (
        LastScreen {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        },
        Suspended {
            kept: vec![7],
            at: Some(9),
        },
    )
}

/// A suspended machine's window is handed back only when its suspend ends —
/// never as its images turn durable — so it stays open while the state is on
/// its way to the store.
#[test]
fn a_window_is_held_until_its_machines_state_is_kept() {
    let mut keeping = Suspends::new();
    assert!(keeping.reserve());
    keeping.suspend((3, String::from("PC")), waiting(), 40u8);
    assert!(keeping.holds(3) && !keeping.is_empty() && keeping.len() == 1);

    let (name, screen, snapshot) = keeping.synced(3, true).expect("waiting");
    assert_eq!((name, screen.width), ("PC", 1));
    assert_eq!(snapshot.map(|kept| kept.kept), Some(vec![7]));
    assert!(
        keeping.synced(3, true).is_none(),
        "it goes to be written once"
    );
    assert!(keeping.holds(3), "the window is still held");

    assert_eq!(
        keeping.end(3),
        Some(Ended {
            name: String::from("PC"),
            with_snapshot: true,
            window: 40,
        })
    );
    assert!(keeping.is_empty() && keeping.end(3).is_none());
}

/// Images that could not be made durable keep only the last screen, and the
/// ended suspend says no snapshot went with it.
#[test]
fn a_suspend_whose_images_are_not_durable_keeps_no_snapshot() {
    let mut keeping = Suspends::new();
    keeping.suspend((3, String::from("PC")), waiting(), 40u8);
    let (_, _, snapshot) = keeping.synced(3, false).expect("waiting");
    assert!(snapshot.is_none());
    assert!(!keeping.end(3).expect("held").with_snapshot);
    assert!(keeping.synced(4, true).is_none() && keeping.end(4).is_none());
}
