use alloc::vec;
use alloc::vec::Vec;

use tairix_reclaim::PressureBand;

use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::document::{Document, Entry, Picture};
use crate::history::{Damage, Unapplied};
use crate::shape::{Point, Shape, FX};
use crate::stroke::{Blend, Layer, Stroke};
use tairix_image::Unkept;

fn document(width: u32, height: u32) -> Document {
    let canvas = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    Document::new(Picture::plain(canvas))
}

/// Draw a dab of `colour` on the showing picture and record it.
fn paint(document: &mut Document, x: i64, y: i64, colour: [u8; 4]) {
    document.reserve().expect("room");
    let mut stroke = Stroke::new(
        Layer {
            ink: Ink::Colour(colour),
            blend: Blend::Over,
        },
        None,
    );
    let canvas = document.canvas_mut().expect("a picture");
    let centre = Point::centre_of(x, y);
    stroke
        .cover(
            canvas,
            0,
            &Shape::Capsule {
                a: centre,
                b: centre,
                radius: 2 * FX,
            },
            false,
        )
        .expect("room");
    document.record_tiles(stroke.finish());
}

fn colour(document: &Document, x: u32, y: u32) -> Option<[u8; 4]> {
    document.picture()?.canvas.colour_at(x, y)
}

#[test]
fn undo_and_redo_swap_a_stroke_out_and_back() {
    let mut doc = document(100, 100);
    paint(&mut doc, 10, 10, [0, 0, 0, 255]);
    assert_eq!(colour(&doc, 10, 10), Some([0, 0, 0, 255]));
    assert!(doc.is_modified());
    let undone = doc.undo().expect("something to undo");
    assert!(matches!(undone.damage, Damage::Area(_)));
    assert_eq!(colour(&doc, 10, 10), Some([255; 4]));
    assert!(!doc.is_modified(), "back where the file was");
    doc.redo().expect("something to redo");
    assert_eq!(colour(&doc, 10, 10), Some([0, 0, 0, 255]));
    assert!(doc.is_modified());
}

#[test]
fn a_new_change_forgets_what_could_have_been_redone() {
    let mut doc = document(50, 50);
    paint(&mut doc, 5, 5, [1, 1, 1, 255]);
    doc.undo().expect("something to undo");
    paint(&mut doc, 20, 20, [2, 2, 2, 255]);
    assert_eq!(doc.can_undo_redo(), (true, false));
    assert_eq!(doc.redo(), Err(Unapplied::Nothing));
}

#[test]
fn a_save_marks_the_generation_it_was_taken_at() {
    let mut doc = document(50, 50);
    paint(&mut doc, 5, 5, [1, 1, 1, 255]);
    let generation = doc.generation();
    doc.saved(generation);
    assert!(!doc.is_modified());
    paint(&mut doc, 6, 6, [1, 1, 1, 255]);
    doc.saved(generation);
    assert!(
        doc.is_modified(),
        "a save of an older generation leaves it changed"
    );
    doc.undo().expect("something to undo");
    assert!(
        doc.is_modified(),
        "that save's state can no longer be named"
    );
}

#[test]
fn the_saved_state_undone_past_and_rewritten_is_lost() {
    let mut doc = document(50, 50);
    paint(&mut doc, 5, 5, [1, 1, 1, 255]);
    doc.saved(doc.generation());
    doc.undo().expect("something to undo");
    assert!(doc.is_modified());
    paint(&mut doc, 9, 9, [3, 3, 3, 255]);
    assert!(doc.is_modified());
    doc.undo().expect("something to undo");
    assert!(
        doc.is_modified(),
        "the saved state lay on the redo that was dropped"
    );
}

#[test]
fn pressure_trims_the_oldest_steps_first() {
    let mut doc = document(64, 64);
    for step in 0..6 {
        paint(&mut doc, 2 + step * 2, 2, [0, 0, 0, 255]);
    }
    assert_eq!(doc.history_depth(), 6);
    doc.adopt_pressure(PressureBand::Moderate);
    assert!(
        doc.history_depth() <= 2,
        "one picture's worth: a tile a step here"
    );
    doc.adopt_pressure(PressureBand::Severe);
    assert_eq!(doc.history_depth(), 0);
    assert!(doc.is_modified(), "the file's state went with the steps");
    assert_eq!(doc.undo(), Err(Unapplied::Nothing));
}

/// Under steady pressure each step recorded keeps the history within what
/// the band allows, not only the band's arrival.
#[test]
fn a_band_holds_the_history_as_steps_are_recorded() {
    let mut doc = document(64, 64);
    doc.adopt_pressure(PressureBand::Moderate);
    for step in 0..6 {
        paint(&mut doc, 2 + step * 2, 2, [0, 0, 0, 255]);
    }
    assert!(
        doc.history_depth() <= 1,
        "one picture's worth, however many steps follow"
    );
}

/// A step charged nothing while a snapshot shared its tile is charged in
/// full once the snapshot lets go, so pressure still reaches it.
#[test]
fn a_steps_charge_follows_what_it_alone_holds() {
    let mut doc = document(64, 64);
    let snapshot = doc.snapshot().expect("room");
    paint(&mut doc, 2, 2, [0, 0, 0, 255]);
    drop(snapshot);
    doc.adopt_pressure(PressureBand::Severe);
    assert_eq!(doc.history_depth(), 0);
}

#[test]
fn normal_pressure_trims_nothing() {
    let mut doc = document(64, 64);
    for step in 0..4 {
        paint(&mut doc, 2 + step * 2, 2, [0, 0, 0, 255]);
    }
    doc.adopt_pressure(PressureBand::Normal);
    assert_eq!(doc.history_depth(), 4);
}

fn sprite_document(count: usize) -> Document {
    let entries: Vec<Entry> = (0..count)
        .map(|shade| {
            let shade = u8::try_from(shade).expect("small");
            let canvas =
                Canvas::new(4, 4, Kind::Rgba, Sample::Rgba([shade, 0, 0, 255])).expect("fits");
            Entry::Picture(Picture::plain(canvas))
        })
        .collect();
    Document::of(entries, crate::document::Origin::New, Unkept::default()).expect("entries")
}

#[test]
fn removing_inserting_and_moving_entries_undo_in_turn() {
    let mut doc = sprite_document(3);
    doc.remove(1).expect("removes");
    assert_eq!(doc.entries().len(), 2);
    assert_eq!(
        colour(&doc, 0, 0),
        Some([2, 0, 0, 255]),
        "the next takes its place"
    );
    doc.move_entry(1, 0).expect("moves");
    assert_eq!(doc.current(), 0);
    let blank = Canvas::new(2, 2, Kind::Rgba, Sample::Rgba([9; 4])).expect("fits");
    doc.insert(2, Entry::Picture(Picture::plain(blank)))
        .expect("inserts");
    assert_eq!(doc.entries().len(), 3);
    doc.undo().expect("insert undone");
    doc.undo().expect("move undone");
    doc.undo().expect("remove undone");
    let shades: Vec<Option<[u8; 4]>> = doc
        .entries()
        .iter()
        .map(|entry| entry.picture().and_then(|p| p.canvas.colour_at(0, 0)))
        .collect();
    assert_eq!(
        shades,
        vec![
            Some([0, 0, 0, 255]),
            Some([1, 0, 0, 255]),
            Some([2, 0, 0, 255])
        ]
    );
    assert!(!doc.is_modified());
    assert_eq!(
        doc.remove(9),
        Err(crate::document::ListRefusal::NoSuchEntry)
    );
}

#[test]
fn the_last_entry_cannot_be_removed() {
    let mut doc = sprite_document(1);
    assert_eq!(doc.remove(0), Err(crate::document::ListRefusal::LastEntry));
}

#[test]
fn undoing_a_change_to_another_entry_shows_that_entry() {
    let mut doc = sprite_document(2);
    paint(&mut doc, 1, 1, [9, 9, 9, 255]);
    assert!(doc.select(1));
    let applied = doc.undo().expect("undone");
    assert_eq!(doc.current(), 0);
    assert_eq!(applied.damage, Damage::List);
}

#[test]
fn a_replaced_picture_comes_back_whole() {
    let mut doc = document(10, 10);
    let smaller = Canvas::new(3, 3, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    doc.replace_picture(Picture::plain(smaller)).expect("room");
    assert_eq!(doc.picture().map(|p| p.canvas.width()), Some(3));
    let applied = doc.undo().expect("undone");
    assert_eq!(applied.damage, Damage::Whole);
    assert_eq!(doc.picture().map(|p| p.canvas.width()), Some(10));
}

/// A step whose tiles no longer fit their slots is refused whole, every tile
/// checked before any is put in, and the picture keeps every pixel.
#[test]
fn a_step_that_no_longer_fits_changes_nothing() {
    let mut doc = document(40, 40);
    let before = colour(&doc, 39, 39);
    let wider = Canvas::new(70, 70, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    doc.reserve().expect("room");
    doc.record_tiles(vec![(0, alloc::sync::Arc::clone(wider.tile(0)))]);
    assert_eq!(doc.undo(), Err(Unapplied::Stale));
    assert_eq!(colour(&doc, 39, 39), before, "nothing was put in");
}

/// Steps that share what they hold are charged for it between them, never
/// not at all: two removed sprites holding the same pixels still go at the
/// pressure that keeps nothing.
#[test]
fn steps_sharing_pixels_are_charged_for_them_and_go_under_pressure() {
    let mut doc = document(64, 64);
    paint(&mut doc, 10, 10, [0, 0, 0, 255]);
    let copy = doc.entry().try_clone().expect("room");
    doc.insert(1, copy).expect("room");
    doc.remove(0).expect("one is left");
    let copy = doc.entry().try_clone().expect("room");
    doc.insert(1, copy).expect("room");
    doc.remove(0).expect("one is left");
    doc.adopt_pressure(PressureBand::Severe);
    assert_eq!(doc.undo(), Err(Unapplied::Nothing), "nothing kept");
}
