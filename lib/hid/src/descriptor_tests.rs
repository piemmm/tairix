extern crate std;

use alloc::vec::Vec;
use std::vec;

use super::{
    read_bits, sign_extend, write_bits, CollectionKind, DescriptorError, FieldFlags,
    ReportDescriptor, ReportId, ReportKind, Usage, MAX_DESCRIPTOR,
};
use crate::test_support::items::*;

/// The boot keyboard descriptor of HID 1.11 Appendix E.6.
fn boot_keyboard() -> Vec<u8> {
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
        report_count(1),
        report_size(8),
        input(CONSTANT),
        report_count(5),
        report_size(1),
        usage_page(0x08),
        usage_min(0x01),
        usage_max(0x05),
        output(DATA_VAR),
        report_count(1),
        report_size(3),
        output(CONSTANT),
        report_count(6),
        report_size(8),
        logical_min(0),
        logical_max(0x65),
        usage_page(0x07),
        usage_min(0x00),
        usage_max(0x65),
        input(DATA_ARRAY),
        end_collection(),
    ])
}

/// The boot mouse descriptor of HID 1.11 Appendix E.10.
fn boot_mouse() -> Vec<u8> {
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
        report_count(3),
        report_size(1),
        input(DATA_VAR),
        report_count(1),
        report_size(5),
        input(CONSTANT),
        usage_page(0x01),
        usage(0x30),
        usage(0x31),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(2),
        input(DATA_VAR_REL),
        end_collection(),
        end_collection(),
    ])
}

#[test]
fn the_boot_keyboard_is_modifiers_leds_and_a_key_array() {
    let model = ReportDescriptor::parse(&boot_keyboard()).expect("parses");
    let fields = model.fields();
    assert_eq!(fields.len(), 3, "padding is not a field");
    let (modifiers, leds, keys) = (&fields[0], &fields[1], &fields[2]);
    assert_eq!(
        (
            modifiers.kind,
            modifiers.offset,
            modifiers.size,
            modifiers.count
        ),
        (ReportKind::Input, 0, 1, 8)
    );
    assert!(modifiers.flags.is_variable());
    assert_eq!(
        model.element_usage(modifiers, 0),
        Some(Usage::new(0x07, 0xE0))
    );
    assert_eq!(
        model.element_usage(modifiers, 7),
        Some(Usage::new(0x07, 0xE7))
    );
    assert_eq!(
        (leds.kind, leds.offset, leds.count),
        (ReportKind::Output, 0, 5)
    );
    assert_eq!(model.element_usage(leds, 4), Some(Usage::new(0x08, 0x05)));
    assert_eq!(
        (keys.kind, keys.offset, keys.size, keys.count),
        (ReportKind::Input, 16, 8, 6)
    );
    assert!(!keys.flags.is_variable());
    assert_eq!(model.array_usage(keys, 0x04), Some(Usage::new(0x07, 0x04)));
    assert_eq!(
        model.array_usage(keys, 0x66),
        None,
        "past the logical range"
    );
    assert_eq!(
        model.report_len(ReportKind::Input, ReportId::Unprefixed),
        Some(8)
    );
    assert_eq!(
        model.report_len(ReportKind::Output, ReportId::Unprefixed),
        Some(1)
    );
    assert!(!model.uses_report_ids());
    let application = &model.collections()[0];
    assert_eq!(application.kind, CollectionKind::Application);
    assert_eq!(application.usage, Usage::new(0x01, 0x06));
}

#[test]
fn the_boot_mouse_nests_its_pointer_in_its_application() {
    let model = ReportDescriptor::parse(&boot_mouse()).expect("parses");
    let [buttons, axes] = model.fields() else {
        panic!("two fields");
    };
    assert_eq!((buttons.offset, buttons.count), (0, 3));
    assert_eq!((axes.offset, axes.size, axes.count), (8, 8, 2));
    assert!(axes.flags.is_relative());
    assert_eq!(axes.logical, (-127, 127));
    assert_eq!(model.element_of(axes, Usage::new(0x01, 0x31)), Some(1));
    let pointer = axes.collection.expect("in a collection");
    assert_eq!(
        model.collection(pointer).map(|c| c.kind),
        Some(CollectionKind::Physical)
    );
    let application = model.top_level(pointer);
    assert_eq!(
        model.collection(application).map(|c| c.usage),
        Some(Usage::new(0x01, 0x02))
    );
    assert!(model.encloses(application, Some(pointer)));
    assert!(!model.encloses(pointer, Some(application)));
    let report = [0b101, 0xFF, 0x05];
    assert_eq!(axes.value(&report, 0), Some(-1));
    assert_eq!(axes.value(&report, 1), Some(5));
    assert_eq!(buttons.value(&report, 2), Some(1));
}

#[test]
fn a_prefixed_report_starts_after_its_id_and_reads_only_its_own_reports() {
    let bytes = join(&[
        usage_page(0x01),
        usage(0x02),
        collection(1),
        report_id(2),
        usage(0x30),
        logical_min(-127),
        logical_max(127),
        report_size(8),
        report_count(1),
        input(DATA_VAR_REL),
        end_collection(),
        usage_page(0x01),
        usage(0x06),
        collection(1),
        report_id(1),
        usage_page(0x07),
        usage_min(0xE0),
        usage_max(0xE7),
        logical_min(0),
        logical_max(1),
        report_size(1),
        report_count(8),
        input(DATA_VAR),
        end_collection(),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    assert!(model.uses_report_ids());
    let [x, modifiers] = model.fields() else {
        panic!("two fields");
    };
    assert_eq!((x.report.id(), x.offset), (Some(2), 8));
    assert_eq!((modifiers.report.id(), modifiers.offset), (Some(1), 8));
    assert_eq!(x.value(&[2, 0xFE], 0), Some(-2));
    assert_eq!(x.value(&[1, 0xFE], 0), None, "another report's bytes");
    assert_eq!(model.report_len(ReportKind::Input, x.report), Some(2));
}

#[test]
fn a_report_id_of_zero_is_refused() {
    let bytes = join(&[
        report_id(0),
        usage(0x30),
        report_size(8),
        report_count(1),
        input(DATA_VAR),
    ]);
    assert_eq!(
        ReportDescriptor::parse(&bytes),
        Err(DescriptorError::ReportIdZero)
    );
}

#[test]
fn a_data_field_outside_every_report_id_is_refused_once_ids_appear() {
    let bytes = join(&[
        usage_page(0x01),
        usage(0x30),
        report_size(8),
        report_count(1),
        input(DATA_VAR),
        report_id(1),
        usage(0x31),
        input(DATA_VAR),
    ]);
    assert_eq!(
        ReportDescriptor::parse(&bytes),
        Err(DescriptorError::Undemuxable)
    );
}

#[test]
fn padding_outside_every_report_id_is_not_data() {
    let bytes = join(&[
        report_size(8),
        report_count(1),
        input(CONSTANT),
        report_id(1),
        usage_page(0x01),
        usage(0x30),
        input(DATA_VAR),
    ]);
    assert!(ReportDescriptor::parse(&bytes).is_ok());
}

#[test]
fn an_unbalanced_collection_is_refused_either_way() {
    let open = join(&[usage(1), collection(1)]);
    assert_eq!(
        ReportDescriptor::parse(&open),
        Err(DescriptorError::Unbalanced)
    );
    assert_eq!(
        ReportDescriptor::parse(&end_collection()),
        Err(DescriptorError::Unbalanced)
    );
}

#[test]
fn a_usage_page_applies_at_the_main_item_and_an_extended_usage_keeps_its_own() {
    let bytes = join(&[
        usage(0x30),
        item(0x08, &[0x38, 0x02, 0x0C, 0x00]),
        usage_page(0x01),
        report_size(8),
        report_count(2),
        input(DATA_VAR_REL),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    let field = &model.fields()[0];
    assert_eq!(model.element_usage(field, 0), Some(Usage::new(0x01, 0x30)));
    assert_eq!(model.element_usage(field, 1), Some(Usage::new(0x0C, 0x238)));
}

#[test]
fn a_variable_fields_last_usage_stands_for_every_element_past_the_list() {
    let bytes = join(&[
        usage_page(0x01),
        usage(0x30),
        usage(0x31),
        report_size(8),
        report_count(4),
        input(DATA_VAR),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    let field = &model.fields()[0];
    assert_eq!(model.element_usage(field, 3), Some(Usage::new(0x01, 0x31)));
    assert_eq!(model.element_usage(field, 4), None, "past the count");
}

#[test]
fn an_array_offsets_its_usages_by_the_logical_minimum() {
    let bytes = join(&[
        usage_page(0x07),
        usage_min(0x04),
        usage_max(0x06),
        logical_min(1),
        logical_max(3),
        report_size(8),
        report_count(1),
        input(DATA_ARRAY),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    let field = &model.fields()[0];
    assert_eq!(model.array_usage(field, 1), Some(Usage::new(0x07, 0x04)));
    assert_eq!(model.array_usage(field, 3), Some(Usage::new(0x07, 0x06)));
    assert_eq!(model.array_usage(field, 0), None, "no key");
}

#[test]
fn a_usage_range_that_runs_backwards_or_lacks_its_minimum_is_refused() {
    let backwards = join(&[
        usage_min(5),
        usage_max(4),
        report_size(1),
        report_count(1),
        input(DATA_VAR),
    ]);
    assert_eq!(
        ReportDescriptor::parse(&backwards),
        Err(DescriptorError::BadUsageRange)
    );
    assert_eq!(
        ReportDescriptor::parse(&usage_max(4)),
        Err(DescriptorError::BadUsageRange)
    );
}

#[test]
fn a_long_item_is_skipped_and_a_cut_item_refused() {
    let mut bytes = vec![0xFE, 2, 0x10, 0xAA, 0xBB];
    bytes.extend(boot_mouse());
    assert_eq!(
        ReportDescriptor::parse(&bytes).map(|model| model.fields().len()),
        Ok(2)
    );
    assert_eq!(
        ReportDescriptor::parse(&[0xFE, 4, 0x10, 0xAA]),
        Err(DescriptorError::Truncated)
    );
    assert_eq!(
        ReportDescriptor::parse(&[0x26, 0xFF]),
        Err(DescriptorError::Truncated)
    );
}

#[test]
fn the_length_bounds_refuse_an_empty_or_oversize_descriptor() {
    assert_eq!(ReportDescriptor::parse(&[]), Err(DescriptorError::Length));
    let oversize = vec![0u8; MAX_DESCRIPTOR + 1];
    assert_eq!(
        ReportDescriptor::parse(&oversize),
        Err(DescriptorError::Length)
    );
}

#[test]
fn nesting_and_collection_counts_are_bounded() {
    let deep: Vec<u8> = (0..17).flat_map(|_| collection(0)).collect();
    assert_eq!(
        ReportDescriptor::parse(&deep),
        Err(DescriptorError::TooMany)
    );
    let many: Vec<u8> = (0..129)
        .flat_map(|_| join(&[collection(0), end_collection()]))
        .collect();
    assert_eq!(
        ReportDescriptor::parse(&many),
        Err(DescriptorError::TooMany)
    );
}

#[test]
fn push_and_pop_restore_the_report_id_and_an_empty_pop_is_refused() {
    let bytes = join(&[
        report_id(1),
        item(0xA4, &[]),
        report_id(2),
        item(0xB4, &[]),
        usage_page(0x01),
        usage(0x30),
        report_size(8),
        report_count(1),
        input(DATA_VAR),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    assert_eq!(model.fields()[0].report.id(), Some(1));
    assert_eq!(
        ReportDescriptor::parse(&item(0xB4, &[])),
        Err(DescriptorError::StackUnderflow)
    );
    let pushes: Vec<u8> = (0..9).flat_map(|_| item(0xA4, &[])).collect();
    assert_eq!(
        ReportDescriptor::parse(&pushes),
        Err(DescriptorError::TooMany)
    );
}

#[test]
fn a_unit_exponent_reads_as_a_nibble_or_a_signed_byte() {
    let field_with = |exponent: u8| {
        let bytes = join(&[
            item(0x54, &[exponent]),
            usage(0x30),
            report_size(8),
            report_count(1),
            input(DATA_VAR),
        ]);
        ReportDescriptor::parse(&bytes).expect("parses").fields()[0].unit_exponent
    };
    assert_eq!(field_with(0x0E), -2);
    assert_eq!(field_with(0x03), 3);
    assert_eq!(field_with(0xFE), -2);
}

#[test]
fn a_physical_range_defaults_to_the_logical_one_and_a_maximum_reads_unsigned() {
    let bytes = join(&[
        usage(0x30),
        logical_min(0),
        logical_max(0xFF),
        report_size(8),
        report_count(1),
        input(DATA_VAR),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    let field = &model.fields()[0];
    assert_eq!(field.logical, (0, 255));
    assert_eq!(field.physical, (0, 255));
    assert_eq!(field.value(&[0xFF], 0), Some(255));
    assert!(field.in_range(255) && !field.in_range(256));
}

#[test]
fn only_the_first_usage_of_a_delimiter_set_is_taken() {
    let bytes = join(&[
        item(0xA8, &[1]),
        usage(0x30),
        usage(0x31),
        item(0xA8, &[0]),
        report_size(8),
        report_count(1),
        input(DATA_VAR),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    assert_eq!(
        model.element_usage(&model.fields()[0], 0),
        Some(Usage::new(0, 0x30))
    );
    let unclosed = join(&[
        item(0xA8, &[1]),
        usage(1),
        report_size(1),
        report_count(1),
        input(DATA_VAR),
    ]);
    assert_eq!(
        ReportDescriptor::parse(&unclosed),
        Err(DescriptorError::BadDelimiter)
    );
    assert_eq!(
        ReportDescriptor::parse(&item(0xA8, &[0])),
        Err(DescriptorError::BadDelimiter)
    );
}

#[test]
fn a_report_longer_than_the_bound_is_refused() {
    let bytes = join(&[
        usage(1),
        report_size(32),
        item(0x94, &[0x01, 0x04]),
        input(DATA_VAR),
    ]);
    assert_eq!(
        ReportDescriptor::parse(&bytes),
        Err(DescriptorError::ReportTooLong)
    );
}

#[test]
fn feature_and_output_reports_keep_offsets_of_their_own() {
    let bytes = join(&[
        report_id(3),
        usage(0x30),
        report_size(8),
        report_count(2),
        input(DATA_VAR),
        usage(0x48),
        report_size(4),
        report_count(1),
        feature(DATA_VAR),
        usage(0x48),
        feature(DATA_VAR),
    ]);
    let model = ReportDescriptor::parse(&bytes).expect("parses");
    let offsets: Vec<_> = model
        .fields()
        .iter()
        .map(|field| (field.kind, field.offset))
        .collect();
    assert_eq!(
        offsets,
        [
            (ReportKind::Input, 8),
            (ReportKind::Feature, 8),
            (ReportKind::Feature, 12)
        ]
    );
    assert_eq!(
        model.report_len(ReportKind::Feature, model.fields()[1].report),
        Some(2)
    );
    assert_eq!(model.longest_report(ReportKind::Input), 3);
}

#[test]
fn bits_read_and_write_across_byte_boundaries() {
    let mut bytes = [0u8; 4];
    write_bits(&mut bytes, 5, 12, 0xABC).expect("fits");
    assert_eq!(read_bits(&bytes, 5, 12), Some(0xABC));
    assert_eq!(read_bits(&bytes, 0, 5), Some(0), "neighbours untouched");
    assert_eq!(read_bits(&bytes, 20, 13), None, "past the end");
    assert_eq!(read_bits(&bytes, 0, 33), None);
    assert_eq!(read_bits(&[0xFF; 4], 0, 32), Some(u32::MAX));
    assert_eq!(sign_extend(0xFF, 8), -1);
    assert_eq!(sign_extend(0x7F, 8), 127);
    assert_eq!(sign_extend(0x8, 4), -8);
}

#[test]
fn field_flags_read_back_what_the_item_set() {
    let flags = FieldFlags::from_bits(0x46);
    assert!(flags.is_variable() && flags.is_relative() && flags.contains(FieldFlags::NULL_STATE));
    assert!(!flags.contains(FieldFlags::CONSTANT));
}
