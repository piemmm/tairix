extern crate std;

use alloc::vec::Vec;
use core::num::NonZeroU8;

use tairix_abi::input::{PointerButtonCode, PointerInput};

use super::MouseDecoder;
use crate::descriptor::{CollectionIndex, ReportDescriptor};
use crate::test_support::items::*;
use crate::test_support::Recorder;
use crate::{boot, Decoded};

fn application() -> CollectionIndex {
    CollectionIndex::new(0).expect("index")
}

fn feed(
    mouse: &mut MouseDecoder,
    model: &ReportDescriptor,
    report: &[u8],
) -> (Decoded, Vec<PointerInput>) {
    let mut sink = Recorder::default();
    let decoded = mouse.decode(model, report, &mut sink).expect("delivered");
    (decoded, sink.pointer)
}

#[test]
fn a_three_byte_boot_report_presses_and_moves_with_no_wheel() {
    let model = boot::mouse().expect("boot layout");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let (decoded, records) = feed(&mut mouse, &model, &[0x01, 0x05, 0xFB]);
    assert_eq!(decoded, Decoded::Applied);
    assert_eq!(
        records,
        [
            PointerInput::Pressed(PointerButtonCode::Primary),
            PointerInput::MovedBy { dx: 5, dy: -5 },
        ]
    );
    let (_, records) = feed(&mut mouse, &model, &[0x00, 0, 0, 0x01]);
    assert_eq!(
        records,
        [
            PointerInput::Released(PointerButtonCode::Primary),
            PointerInput::Scrolled { dx: 0, dy: -120 },
        ],
        "a wheel turned away scrolls toward the start"
    );
}

#[test]
fn a_report_missing_its_axes_is_refused_and_changes_nothing() {
    let model = boot::mouse().expect("boot layout");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let (decoded, records) = feed(&mut mouse, &model, &[0x01, 0x05]);
    assert_eq!(decoded, Decoded::Malformed);
    assert!(records.is_empty());
    let (_, records) = feed(&mut mouse, &model, &[0x01, 0, 0]);
    assert_eq!(
        records,
        [PointerInput::Pressed(PointerButtonCode::Primary)],
        "the press was not taken before"
    );
}

#[test]
fn buttons_past_the_three_the_seat_names_are_not_read() {
    let model = boot::mouse().expect("boot layout");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let (_, records) = feed(&mut mouse, &model, &[0b1100_0100, 0, 0]);
    assert_eq!(records, [PointerInput::Pressed(PointerButtonCode::Middle)]);
}

/// A report-protocol mouse: three buttons, 16-bit X and Y, a wheel and AC Pan.
fn fine_mouse() -> Vec<u8> {
    join(&[
        usage_page(0x01),
        usage(0x02),
        collection(1),
        usage(0x01),
        collection(0),
        usage_page(0x09),
        usage_min(1),
        usage_max(3),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(3),
        input(DATA_VAR),
        report_count(5),
        input(CONSTANT),
        usage_page(0x01),
        usage(0x30),
        usage(0x31),
        item(0x16, &[0x01, 0x80]),
        item(0x26, &[0xFF, 0x7F]),
        report_size(16),
        report_count(2),
        input(DATA_VAR_REL),
        usage(0x38),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(1),
        input(DATA_VAR_REL),
        usage_page(0x0C),
        item(0x0A, &[0x38, 0x02]),
        input(DATA_VAR_REL),
        end_collection(),
        end_collection(),
    ])
}

#[test]
fn motion_keeps_sixteen_bits_and_a_fine_wheel_reports_a_detent_per_detent() {
    let model = ReportDescriptor::parse(&fine_mouse()).expect("parses");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let (_, records) = feed(&mut mouse, &model, &[0, 0x00, 0x10, 0x00, 0xF0, 0, 1]);
    assert_eq!(
        records,
        [
            PointerInput::MovedBy {
                dx: 4096,
                dy: -4096
            },
            PointerInput::Scrolled { dx: 120, dy: 0 },
        ]
    );
    mouse.adopt(NonZeroU8::new(8), None);
    let mut down = 0;
    for _ in 0..8 {
        let (_, records) = feed(&mut mouse, &model, &[0, 0, 0, 0, 0, 0xFF, 0]);
        for record in records {
            if let PointerInput::Scrolled { dy, .. } = record {
                down += dy;
            }
        }
    }
    assert_eq!(
        down, 120,
        "eight counts at eight a detent are one detent toward the end"
    );
}

#[test]
fn a_wheel_turned_as_far_as_its_field_holds_scrolls_as_far_as_a_record_can() {
    let wide_wheel = join(&[
        usage_page(0x01),
        usage(0x02),
        collection(1),
        usage(0x30),
        usage(0x31),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(2),
        input(DATA_VAR_REL),
        usage(0x38),
        item(0x14, &i32::MIN.to_le_bytes()),
        item(0x24, &i32::MAX.to_le_bytes()),
        report_size(32),
        report_count(1),
        input(DATA_VAR_REL),
        end_collection(),
    ]);
    let model = ReportDescriptor::parse(&wide_wheel).expect("parses");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let (_, records) = feed(&mut mouse, &model, &[0, 0, 0x00, 0x00, 0x00, 0x80]);
    assert_eq!(
        records,
        [PointerInput::Scrolled {
            dx: 0,
            dy: i32::MAX
        }]
    );
}

#[test]
fn an_absolute_pointer_is_no_mouse() {
    let tablet = join(&[
        usage_page(0x01),
        usage(0x02),
        collection(1),
        usage(0x30),
        usage(0x31),
        logical_min(0),
        logical_max(0x7F),
        report_size(8),
        report_count(2),
        input(DATA_VAR),
        end_collection(),
    ]);
    let model = ReportDescriptor::parse(&tablet).expect("parses");
    assert!(MouseDecoder::new(&model, application()).is_none());
}

#[test]
fn letting_go_releases_every_button_held() {
    let model = boot::mouse().expect("boot layout");
    let mut mouse = MouseDecoder::new(&model, application()).expect("a mouse");
    let _ = feed(&mut mouse, &model, &[0b011, 0, 0]);
    let mut sink = Recorder::default();
    mouse.release(&mut sink).expect("released");
    assert_eq!(
        sink.pointer,
        [
            PointerInput::Released(PointerButtonCode::Primary),
            PointerInput::Released(PointerButtonCode::Secondary),
        ]
    );
    let mut sink = Recorder::default();
    mouse.release(&mut sink).expect("released");
    assert!(sink.pointer.is_empty(), "nothing is released twice");
}
