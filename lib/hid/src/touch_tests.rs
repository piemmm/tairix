extern crate std;

use alloc::vec::Vec;
use std::vec;

use tairix_abi::touch::{ContactKind, TouchFrame, TouchSurface};

use super::TouchDecoder;
use crate::descriptor::{CollectionIndex, ReportDescriptor};
use crate::test_support::items::*;
use crate::test_support::Recorder;
use crate::Decoded;

const REPORT_ID: u8 = 1;

/// One Finger collection: confidence and tip bits, a contact id, and X and Y
/// over 10.00 cm by 5.00 cm.
fn finger() -> Vec<u8> {
    join(&[
        usage_page(0x0D),
        usage(0x22),
        collection(2),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(1),
        usage(0x47),
        input(DATA_VAR),
        usage(0x42),
        input(DATA_VAR),
        report_count(6),
        input(CONSTANT),
        report_size(8),
        report_count(1),
        logical_max(0x0F),
        usage(0x51),
        input(DATA_VAR),
        usage_page(0x01),
        logical_max16(4000),
        physical_max(1000),
        unit(0x11),
        unit_exponent(0x0E),
        report_size(16),
        usage(0x30),
        input(DATA_VAR),
        logical_max16(2000),
        physical_max(500),
        usage(0x31),
        input(DATA_VAR),
        end_collection(),
    ])
}

/// A touch pad (or, with `0x04`, a touch screen) of `fingers` slots, a contact
/// count and one button.
fn digitizer(application: u8, fingers: usize) -> Vec<u8> {
    let mut parts = vec![
        usage_page(0x0D),
        usage(application),
        collection(1),
        report_id(REPORT_ID),
    ];
    parts.extend((0..fingers).map(|_| finger()));
    parts.extend([
        usage_page(0x0D),
        logical_min(0),
        logical_max(10),
        report_size(8),
        report_count(1),
        usage(0x54),
        input(DATA_VAR),
        usage_page(0x09),
        usage(0x01),
        logical_max(1),
        report_size(1),
        report_count(1),
        input(DATA_VAR),
        report_count(7),
        input(CONSTANT),
        end_collection(),
    ]);
    join(&parts)
}

/// One slot as the device sends it.
#[derive(Clone, Copy)]
struct Slot {
    confident: bool,
    touching: bool,
    id: u8,
    x: u16,
    y: u16,
}

const fn touch(id: u8, x: u16, y: u16) -> Slot {
    Slot {
        confident: true,
        touching: true,
        id,
        x,
        y,
    }
}

const EMPTY: Slot = Slot {
    confident: false,
    touching: false,
    id: 0,
    x: 0,
    y: 0,
};

fn report(slots: &[Slot], count: u8, button: bool) -> Vec<u8> {
    let mut bytes = vec![REPORT_ID];
    for slot in slots {
        bytes.push(u8::from(slot.confident) | u8::from(slot.touching) << 1);
        bytes.push(slot.id);
        bytes.extend_from_slice(&slot.x.to_le_bytes());
        bytes.extend_from_slice(&slot.y.to_le_bytes());
    }
    bytes.push(count);
    bytes.push(u8::from(button));
    bytes
}

fn pad(fingers: usize) -> (ReportDescriptor, TouchDecoder) {
    let model = ReportDescriptor::parse(&digitizer(0x05, fingers)).expect("parses");
    let decoder = TouchDecoder::new(&model, CollectionIndex::new(0).expect("index"), false, 0)
        .expect("a pad");
    (model, decoder)
}

fn feed(
    decoder: &mut TouchDecoder,
    model: &ReportDescriptor,
    report: &[u8],
) -> (Decoded, Vec<TouchFrame>) {
    let mut sink = Recorder::default();
    let outcome = decoder.decode(model, report, &mut sink).expect("delivered");
    (outcome, sink.touch)
}

fn ids(frame: &TouchFrame) -> Vec<u16> {
    frame.contacts().iter().map(|contact| contact.id).collect()
}

#[test]
fn two_contacts_in_one_report_are_one_frame_over_the_pads_size() {
    let (model, mut decoder) = pad(2);
    let (outcome, frames) = feed(
        &mut decoder,
        &model,
        &report(&[touch(3, 2000, 1000), touch(5, 4000, 0)], 2, true),
    );
    assert_eq!(outcome, Decoded::Applied);
    let [frame] = &frames[..] else {
        panic!("one frame");
    };
    assert_eq!(ids(frame), [3, 5]);
    assert_eq!(
        (frame.contacts()[0].x, frame.contacts()[0].y),
        (32767, 32767)
    );
    assert_eq!(frame.contacts()[1].x, u16::MAX);
    assert_eq!(
        frame.surface(),
        TouchSurface::Clickpad,
        "its one button is the pad"
    );
    assert_eq!(
        (frame.extent().width, frame.extent().height),
        (1000, 500),
        "10 cm by 5 cm"
    );
    assert_eq!(frame.buttons().bits(), 1);
}

#[test]
fn a_frame_spread_over_reports_is_delivered_once_whole() {
    let (model, mut decoder) = pad(2);
    let (_, frames) = feed(
        &mut decoder,
        &model,
        &report(&[touch(1, 10, 10), touch(2, 20, 20)], 3, false),
    );
    assert!(frames.is_empty(), "two of three slots have come");
    let (_, frames) = feed(
        &mut decoder,
        &model,
        &report(&[touch(4, 30, 30), EMPTY], 0, false),
    );
    let [frame] = &frames[..] else {
        panic!("one frame");
    };
    assert_eq!(ids(frame), [1, 2, 4]);
}

#[test]
fn a_frame_a_new_one_interrupts_is_dropped_not_delivered_short() {
    let (model, mut decoder) = pad(2);
    let _ = feed(
        &mut decoder,
        &model,
        &report(&[touch(1, 10, 10), touch(2, 20, 20)], 3, false),
    );
    let (_, frames) = feed(
        &mut decoder,
        &model,
        &report(&[touch(7, 30, 30), EMPTY], 1, false),
    );
    let [frame] = &frames[..] else {
        panic!("one frame");
    };
    assert_eq!(ids(frame), [7]);
}

#[test]
fn a_lifting_contact_leaves_the_frame_and_a_palm_is_marked() {
    let (model, mut decoder) = pad(2);
    let lifting = Slot {
        touching: false,
        ..touch(1, 10, 10)
    };
    let palm = Slot {
        confident: false,
        ..touch(2, 20, 20)
    };
    let (_, frames) = feed(&mut decoder, &model, &report(&[lifting, palm], 2, false));
    assert_eq!(ids(&frames[0]), [2]);
    assert_eq!(frames[0].contacts()[0].kind, ContactKind::Palm);
}

#[test]
fn a_frame_naming_more_contacts_than_the_device_allows_is_refused() {
    let (model, mut decoder) = pad(2);
    decoder.adopt_contact_max(2);
    let (outcome, frames) = feed(
        &mut decoder,
        &model,
        &report(&[touch(1, 0, 0), touch(2, 0, 0)], 3, false),
    );
    assert_eq!(outcome, Decoded::Malformed);
    assert!(frames.is_empty());
}

#[test]
fn a_count_of_zero_with_nothing_arriving_is_an_empty_frame() {
    let (model, mut decoder) = pad(2);
    let _ = feed(
        &mut decoder,
        &model,
        &report(&[touch(1, 0, 0), EMPTY], 1, false),
    );
    let (_, frames) = feed(&mut decoder, &model, &report(&[EMPTY, EMPTY], 0, false));
    assert!(frames[0].contacts().is_empty(), "every contact lifted");
}

#[test]
fn a_cut_report_changes_nothing() {
    let (model, mut decoder) = pad(2);
    let mut cut = report(&[touch(1, 0, 0), touch(2, 0, 0)], 2, false);
    cut.truncate(9);
    let (outcome, frames) = feed(&mut decoder, &model, &cut);
    assert_eq!(outcome, Decoded::Malformed);
    assert!(frames.is_empty());
}

#[test]
fn letting_go_lifts_what_the_last_frame_held_once() {
    let (model, mut decoder) = pad(2);
    let _ = feed(
        &mut decoder,
        &model,
        &report(&[touch(1, 0, 0), EMPTY], 1, true),
    );
    let mut sink = Recorder::default();
    decoder.release(&mut sink).expect("released");
    assert!(sink.touch[0].contacts().is_empty() && sink.touch[0].buttons().bits() == 0);
    let mut sink = Recorder::default();
    decoder.release(&mut sink).expect("released");
    assert!(sink.touch.is_empty());
}

#[test]
fn a_screen_is_direct_and_a_stated_pad_type_decides_a_pad() {
    let model = ReportDescriptor::parse(&digitizer(0x04, 1)).expect("parses");
    let screen = TouchDecoder::new(&model, CollectionIndex::new(0).expect("index"), true, 0)
        .expect("a screen");
    assert_eq!(screen.surface(), TouchSurface::Screen);
    let (_, mut pad) = pad(1);
    pad.adopt_pad_type(2);
    assert_eq!(pad.surface(), TouchSurface::Touchpad);
    pad.adopt_pad_type(0);
    assert_eq!(pad.surface(), TouchSurface::Clickpad);
}

#[test]
fn a_finger_with_no_absolute_position_is_no_contact_slot() {
    let relative = join(&[
        usage_page(0x0D),
        usage(0x05),
        collection(1),
        usage(0x22),
        collection(2),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(1),
        usage(0x42),
        input(DATA_VAR),
        report_count(7),
        input(CONSTANT),
        usage_page(0x01),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(2),
        usage(0x30),
        usage(0x31),
        input(DATA_VAR_REL),
        end_collection(),
        end_collection(),
    ]);
    let model = ReportDescriptor::parse(&relative).expect("parses");
    assert!(TouchDecoder::new(&model, CollectionIndex::new(0).expect("index"), false, 0).is_none());
}
