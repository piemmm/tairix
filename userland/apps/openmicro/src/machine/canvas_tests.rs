use alloc::vec::Vec;

use super::*;
use crate::snapshot;

const WHITE: u8 = encode(7, 0, 3);

fn saved(canvas: &Canvas) -> Vec<u8> {
    let mut w = Writer::new();
    w.put(canvas);
    w.finish().unwrap()
}

fn restored(saved: &[u8], clock: u64) -> Result<Canvas, SnapshotError> {
    let body = snapshot::open(saved)?;
    let mut r = Reader::new(&body);
    r.set_clock(clock)?;
    r.get()
}

/// The regression: every frame compared and copied the whole canvas, however
/// little was drawn into it.
#[test]
fn a_frame_touches_only_the_rows_painted_differently() {
    let mut canvas = Canvas::new();
    canvas.paint(5, 0, &[BLACK; 8]);
    assert_eq!(
        canvas.touched.rows().count(),
        0,
        "a still picture touches nothing"
    );
    canvas.paint(5, 16, &[WHITE; 8]);
    canvas.paint(7, 0, &[WHITE; 8]);
    canvas.finish();
    assert_eq!(canvas.frames(), 1);
    assert_eq!(canvas.take_dirty().rows().collect::<Vec<_>>(), [5, 7]);
    assert!(canvas.take_dirty().rows().next().is_none(), "taken once");
    canvas.paint(5, 16, &[WHITE; 8]);
    canvas.finish();
    assert!(
        canvas.take_dirty().rows().next().is_none(),
        "painted as shown is no change"
    );
    assert_eq!(canvas.shown()[5 * CANVAS_WIDTH + 16], WHITE);
}

#[test]
fn what_lies_past_the_canvas_is_dropped() {
    let mut canvas = Canvas::new();
    canvas.paint(0, CANVAS_WIDTH - 4, &[WHITE; 8]);
    canvas.paint(CANVAS_HEIGHT, 0, &[WHITE; 8]);
    canvas.paint(1, CANVAS_WIDTH, &[WHITE; 8]);
    canvas.finish();
    assert_eq!(canvas.take_dirty().rows().collect::<Vec<_>>(), [0]);
    assert_eq!(
        &canvas.shown()[CANVAS_WIDTH - 5..=CANVAS_WIDTH],
        [BLACK, WHITE, WHITE, WHITE, WHITE, BLACK],
        "the row's end painted and the next row's start left"
    );
}

/// The regression: the count of frames was restored unchecked, so one at the
/// top of its range overflowed at the next frame.
#[test]
fn a_restored_canvas_is_all_new_and_no_more_frames_than_cycles_are_taken() {
    let mut canvas = Canvas::new();
    canvas.paint(3, 8, &[WHITE; 8]);
    canvas.finish();
    canvas.take_dirty();
    let mut copy = restored(&saved(&canvas), 100).unwrap();
    assert_eq!(copy.shown(), canvas.shown());
    assert_eq!(copy.take_dirty().rows().count(), CANVAS_HEIGHT);
    canvas.frames = u64::MAX;
    assert_eq!(
        restored(&saved(&canvas), 100).err(),
        Some(SnapshotError::Malformed)
    );
}
