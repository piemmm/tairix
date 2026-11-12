extern crate std;

use alloc::vec::Vec;
use std::vec;

use tairix_abi::input::{KeyEdge, PhysicalKey};

use super::KeyboardDecoder;
use crate::descriptor::{CollectionIndex, ReportDescriptor};
use crate::test_support::items::*;
use crate::test_support::Recorder;
use crate::{boot, Decoded};

fn decoder(model: &ReportDescriptor) -> KeyboardDecoder {
    KeyboardDecoder::new(model, CollectionIndex::new(0).expect("index")).expect("a keyboard")
}

fn feed(
    decoder: &mut KeyboardDecoder,
    model: &ReportDescriptor,
    report: &[u8],
) -> (Decoded, Recorder) {
    let mut sink = Recorder::default();
    let outcome = decoder.decode(model, report, &mut sink).expect("delivered");
    (outcome, sink)
}

/// The keys pressed, in order.
fn pressed(sink: &Recorder) -> Vec<PhysicalKey> {
    sink.keys
        .iter()
        .filter(|edge| edge.pressed)
        .map(|edge| edge.key)
        .collect()
}

fn down(key: PhysicalKey) -> KeyEdge {
    KeyEdge { key, pressed: true }
}

fn up(key: PhysicalKey) -> KeyEdge {
    KeyEdge {
        key,
        pressed: false,
    }
}

#[test]
fn a_shifted_key_goes_down_after_its_shift_and_comes_up_before_it() {
    // The seat reads a key under the modifiers held when it arrives, so a
    // shifted key must arrive inside its shift.
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Decoded::Applied);
    assert_eq!(
        sink.keys,
        [down(PhysicalKey::LEFT_SHIFT), down(PhysicalKey::A)]
    );
    let (_, sink) = feed(&mut keyboard, &model, &[0; 8]);
    assert_eq!(sink.keys, [up(PhysicalKey::A), up(PhysicalKey::LEFT_SHIFT)]);
}

#[test]
fn a_boot_report_names_every_modifier_and_the_key_of_a_chord() {
    // Meta and Shift held with the 3 key: every key named, the modifiers
    // first.
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (_, sink) = feed(&mut keyboard, &model, &[0x0A, 0, 0x20, 0, 0, 0, 0, 0]);
    assert_eq!(
        sink.keys,
        [
            down(PhysicalKey::LEFT_SHIFT),
            down(PhysicalKey::LEFT_GUI),
            down(PhysicalKey::DIGIT_3)
        ]
    );
    let (_, sink) = feed(&mut keyboard, &model, &[0; 8]);
    assert_eq!(sink.keys[0], up(PhysicalKey::DIGIT_3));
}

#[test]
fn a_key_named_twice_in_one_report_presses_once() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (_, sink) = feed(&mut keyboard, &model, &[0, 0, 0x04, 0x04, 0, 0, 0, 0]);
    assert_eq!(pressed(&sink), [PhysicalKey::A]);
    let (_, sink) = feed(&mut keyboard, &model, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    assert!(sink.keys.is_empty(), "still held, nothing changed");
}

#[test]
fn a_roll_over_report_is_a_phantom_and_changes_nothing() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let _ = feed(&mut keyboard, &model, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0, 0, 1, 1, 1, 1, 1, 1]);
    assert_eq!(outcome, Decoded::Applied);
    assert!(sink.keys.is_empty(), "the held key stays held");
}

#[test]
fn a_modifier_in_the_key_array_leaves_the_bitmap_in_charge() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (_, sink) = feed(&mut keyboard, &model, &[0, 0, 0xE1, 0x04, 0, 0, 0, 0]);
    assert_eq!(pressed(&sink), [PhysicalKey::A], "no shift from the array");
}

#[test]
fn a_clipped_report_carries_the_keys_that_arrived_but_must_carry_its_modifiers() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0]);
    assert_eq!(outcome, Decoded::Applied);
    assert_eq!(sink.keys, [down(PhysicalKey::LEFT_SHIFT)]);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0, 0x05]);
    assert_eq!(outcome, Decoded::Applied);
    assert_eq!(
        pressed(&sink),
        [PhysicalKey::from_usage(0x05).expect("a key")]
    );
    let (outcome, sink) = feed(&mut keyboard, &model, &[]);
    assert_eq!(outcome, Decoded::Malformed);
    assert!(sink.keys.is_empty());
}

/// A report-protocol keyboard whose keys are a bitmap, one bit per usage.
fn nkro() -> Vec<u8> {
    join(&[
        usage_page(0x01),
        usage(0x06),
        collection(1),
        usage_page(0x07),
        usage_min(0xE0),
        usage_max(0xE7),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(8),
        input(DATA_VAR),
        usage_min(0x00),
        usage_max(0x67),
        report_count(0x68),
        input(DATA_VAR),
        end_collection(),
    ])
}

#[test]
fn a_bitmap_keyboard_holds_every_key_its_bits_name() {
    let model = ReportDescriptor::parse(&nkro()).expect("parses");
    let mut keyboard = decoder(&model);
    let mut report = vec![0u8; 14];
    report[1] = 1 << 4 | 1 << 5;
    let (_, sink) = feed(&mut keyboard, &model, &report);
    assert_eq!(
        pressed(&sink),
        [
            PhysicalKey::A,
            PhysicalKey::from_usage(0x05).expect("a key")
        ]
    );
}

#[test]
fn a_bitmap_report_names_each_key_by_its_usage() {
    let model = ReportDescriptor::parse(&nkro()).expect("parses");
    let mut keyboard = decoder(&model);
    let mut report = vec![0u8; 14];
    // Usage `n` is bit `n` of the bitmap after the modifier byte: `A` (0x04)
    // and the space bar (0x2C).
    report[1] = 1 << 4;
    report[6] = 1 << 4;
    let (_, sink) = feed(&mut keyboard, &model, &report);
    assert_eq!(pressed(&sink), [PhysicalKey::A, PhysicalKey::SPACE]);
}

/// A keyboard reporting under ID 6 beside a vendor collection under ID 1: the
/// composite keyboard whose keys were all lost when only the first keyboard
/// ID was read.
fn behind_a_vendor_collection() -> Vec<u8> {
    join(&[
        item(0x06, &[0x00, 0xFF]),
        usage(0x01),
        collection(1),
        report_id(1),
        usage(0x02),
        logical_min(0),
        logical_max(0xFF),
        report_size(8),
        report_count(4),
        input(DATA_VAR),
        end_collection(),
        usage_page(0x01),
        usage(0x06),
        collection(1),
        report_id(6),
        usage_page(0x07),
        usage_min(0xE0),
        usage_max(0xE7),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(8),
        input(DATA_VAR),
        report_count(6),
        report_size(8),
        logical_max(0x65),
        usage_min(0x00),
        usage_max(0x65),
        input(DATA_ARRAY),
        end_collection(),
    ])
}

#[test]
fn a_keyboard_reads_only_its_own_report_id() {
    let model = ReportDescriptor::parse(&behind_a_vendor_collection()).expect("parses");
    let mut keyboard =
        KeyboardDecoder::new(&model, CollectionIndex::new(1).expect("index")).expect("a keyboard");
    let (outcome, sink) = feed(&mut keyboard, &model, &[1, 0x02, 0x04, 0, 0]);
    assert_eq!(outcome, Decoded::NotMine);
    assert!(sink.keys.is_empty(), "a vendor report is no keystroke");
    let (outcome, sink) = feed(&mut keyboard, &model, &[6, 0, 0x04, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Decoded::Applied);
    assert_eq!(pressed(&sink), [PhysicalKey::A]);
}

#[test]
fn letting_go_releases_every_key_held() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let _ = feed(&mut keyboard, &model, &[0x01, 0, 0x04, 0x05, 0, 0, 0, 0]);
    let mut sink = Recorder::default();
    keyboard.release(&mut sink).expect("released");
    assert_eq!(
        sink.keys,
        [
            up(PhysicalKey::A),
            up(PhysicalKey::from_usage(0x05).expect("a key")),
            up(PhysicalKey::LEFT_CTRL)
        ],
        "the keys, then the modifier"
    );
}

#[test]
fn an_application_with_no_key_field_is_no_keyboard() {
    let model = boot::mouse().expect("boot layout");
    assert!(KeyboardDecoder::new(&model, CollectionIndex::new(0).expect("index")).is_none());
}
