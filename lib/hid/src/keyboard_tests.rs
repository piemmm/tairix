extern crate std;

use alloc::vec::Vec;
use std::vec;

use tairix_abi::input::{KeyInput, KeyValue};

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

fn pressed_chars(sink: &Recorder) -> Vec<char> {
    sink.keys
        .iter()
        .filter_map(|record| match record {
            KeyInput::Pressed {
                key: KeyValue::Char(c),
                ..
            } => Some(*c),
            _ => None,
        })
        .collect()
}

#[test]
fn a_shifted_key_resolves_under_the_shift_typed_with_it() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Decoded::Applied);
    assert!(matches!(sink.keys[0], KeyInput::ModifiersChanged { modifiers } if modifiers.shift));
    assert_eq!(pressed_chars(&sink), ['A']);
    let (_, sink) = feed(&mut keyboard, &model, &[0; 8]);
    assert!(matches!(
        sink.keys[0],
        KeyInput::Released {
            key: KeyValue::Char('A'),
            ..
        }
    ));
    assert!(matches!(sink.keys[1], KeyInput::ModifiersChanged { modifiers } if !modifiers.shift));
}

#[test]
fn a_key_named_twice_in_one_report_presses_once() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (_, sink) = feed(&mut keyboard, &model, &[0, 0, 0x04, 0x04, 0, 0, 0, 0]);
    assert_eq!(pressed_chars(&sink), ['a']);
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
    assert_eq!(pressed_chars(&sink), ['a'], "no shift from the array");
}

#[test]
fn a_clipped_report_carries_the_keys_that_arrived_but_must_carry_its_modifiers() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0]);
    assert_eq!(outcome, Decoded::Applied);
    assert!(matches!(sink.keys[..], [KeyInput::ModifiersChanged { modifiers }] if modifiers.shift));
    let (outcome, sink) = feed(&mut keyboard, &model, &[0x02, 0, 0x05]);
    assert_eq!(outcome, Decoded::Applied);
    assert_eq!(pressed_chars(&sink), ['B']);
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
    assert_eq!(pressed_chars(&sink), ['a', 'b']);
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
    assert_eq!(pressed_chars(&sink), ['a']);
}

#[test]
fn letting_go_releases_every_key_held() {
    let model = boot::keyboard().expect("boot layout");
    let mut keyboard = decoder(&model);
    let _ = feed(&mut keyboard, &model, &[0x01, 0, 0x04, 0x05, 0, 0, 0, 0]);
    let mut sink = Recorder::default();
    keyboard.release(&mut sink).expect("released");
    let released = sink
        .keys
        .iter()
        .filter(|record| matches!(record, KeyInput::Released { .. }))
        .count();
    assert_eq!(released, 2);
    assert!(
        matches!(sink.keys.last(), Some(KeyInput::ModifiersChanged { modifiers }) if !modifiers.ctrl)
    );
}

#[test]
fn an_application_with_no_key_field_is_no_keyboard() {
    let model = boot::mouse().expect("boot layout");
    assert!(KeyboardDecoder::new(&model, CollectionIndex::new(0).expect("index")).is_none());
}
