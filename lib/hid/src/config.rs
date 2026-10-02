//! The features a device is configured through when it is bound: a
//! digitizer's input mode and report switches, a wheel's resolution
//! multiplier, a touch pad's pad type and contact limit.
//!
//! A feature the device refuses is left as it is: a refusal is an answer, and
//! only a device that has gone fails the configuration.

use alloc::vec::Vec;
use core::num::NonZeroU8;

use tairix_abi::DriverError;

use crate::descriptor::{CollectionIndex, Field, ReportDescriptor, ReportId, ReportKind, Usage};
use crate::mouse::{MouseDecoder, Slot};
use crate::touch::TouchDecoder;
use crate::usages::{
    BUTTON_SWITCH, CONTACT_COUNT_MAXIMUM, INPUT_MODE, PAD_TYPE, RESOLUTION_MULTIPLIER,
    SURFACE_SWITCH,
};
use crate::{descriptor, in_application, try_collect, try_push};

/// How a class driver reads and writes a device's feature reports.
pub trait HidTransport {
    /// Read the feature report `id` into `report`, answering how many bytes
    /// the device sent, its ID byte first when it has one.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] when the device has gone; any other error is
    /// the device refusing.
    fn get_feature(&mut self, id: ReportId, report: &mut [u8]) -> Result<usize, DriverError>;

    /// Write the feature report `report`, its ID byte first when it has one.
    ///
    /// # Errors
    ///
    /// As [`Self::get_feature`].
    fn set_feature(&mut self, id: ReportId, report: &[u8]) -> Result<(), DriverError>;
}

/// A refusal, unless the device has gone.
fn refusal(error: DriverError) -> Result<(), DriverError> {
    match error {
        DriverError::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// A feature report of `model`'s, read from the device or zeroed when it
/// will not say.
fn current(
    model: &ReportDescriptor,
    transport: &mut dyn HidTransport,
    id: ReportId,
) -> Result<Option<Vec<u8>>, DriverError> {
    let Some(len) = model.report_len(ReportKind::Feature, id) else {
        return Ok(None);
    };
    let mut report = Vec::new();
    if report.try_reserve_exact(len).is_err() {
        return Ok(None);
    }
    report.resize(len, 0);
    match transport.get_feature(id, &mut report) {
        Ok(got) if got == len && id.matches(&report) => {}
        Ok(_) => report.fill(0),
        Err(error) => {
            refusal(error)?;
            report.fill(0);
        }
    }
    if let Some(id) = id.id() {
        report[0] = id;
    }
    Ok(Some(report))
}

/// Set element `element` of `field` to `value` in its feature report.
fn set_element(report: &mut [u8], field: &Field, element: u16, value: u32) -> Option<()> {
    let size = u32::from(field.size);
    let offset = field
        .offset
        .checked_add(u32::from(element).checked_mul(size)?)?;
    descriptor::write_bits(report, offset, size, value)
}

/// The feature fields under `application` carrying `usage`, by report.
fn feature_slots(
    model: &ReportDescriptor,
    application: CollectionIndex,
    usage: Usage,
) -> impl Iterator<Item = Slot> + '_ {
    model
        .fields()
        .iter()
        .enumerate()
        .filter_map(move |(index, field)| {
            let element = (field.kind == ReportKind::Feature
                && field.flags.is_variable()
                && field.size <= 32
                && in_application(model, field, application))
            .then(|| model.element_of(field, usage))
            .flatten()?;
            Some(Slot {
                field: index,
                element,
            })
        })
}

/// Switch every Device Configuration application to report contacts as
/// `mode`, with its surface and button switches on.
pub(crate) fn input_mode(
    model: &ReportDescriptor,
    transport: &mut dyn HidTransport,
    configurations: &[CollectionIndex],
    mode: u32,
) -> Result<(), DriverError> {
    for &application in configurations {
        let wanted = [(INPUT_MODE, mode), (SURFACE_SWITCH, 1), (BUTTON_SWITCH, 1)];
        let mut reports: Vec<ReportId> = Vec::new();
        for (usage, _) in wanted {
            for slot in feature_slots(model, application, usage) {
                let id = model.fields()[slot.field].report;
                if !reports.contains(&id) && try_push(&mut reports, id).is_none() {
                    return Ok(());
                }
            }
        }
        for id in reports {
            let Some(mut report) = current(model, transport, id)? else {
                continue;
            };
            for (usage, value) in wanted {
                for slot in feature_slots(model, application, usage) {
                    let field = &model.fields()[slot.field];
                    if field.report == id {
                        let _ = set_element(&mut report, field, slot.element, value);
                    }
                }
            }
            if let Err(error) = transport.set_feature(id, &report) {
                refusal(error)?;
            }
        }
    }
    Ok(())
}

/// Read a touch application's Pad Type and Contact Count Maximum into its
/// decoder.
pub(crate) fn touch_limits(
    model: &ReportDescriptor,
    transport: &mut dyn HidTransport,
    application: CollectionIndex,
    decoder: &mut TouchDecoder,
) -> Result<(), DriverError> {
    if let Some(value) = feature_value(model, transport, application, PAD_TYPE)? {
        decoder.adopt_pad_type(value);
    }
    if let Some(value) = feature_value(model, transport, application, CONTACT_COUNT_MAXIMUM)? {
        if let Ok(max) = u16::try_from(value) {
            if max > 0 {
                decoder.adopt_contact_max(max);
            }
        }
    }
    Ok(())
}

/// The value the device states for the first feature under `application`
/// carrying `usage`, when it answers.
fn feature_value(
    model: &ReportDescriptor,
    transport: &mut dyn HidTransport,
    application: CollectionIndex,
    usage: Usage,
) -> Result<Option<i64>, DriverError> {
    let Some(slot) = feature_slots(model, application, usage).next() else {
        return Ok(None);
    };
    let field = &model.fields()[slot.field];
    let Some(len) = model.report_len(ReportKind::Feature, field.report) else {
        return Ok(None);
    };
    let mut report = Vec::new();
    if report.try_reserve_exact(len).is_err() {
        return Ok(None);
    }
    report.resize(len, 0);
    match transport.get_feature(field.report, &mut report) {
        Ok(got) if got == len => Ok(field
            .value(&report, slot.element)
            .filter(|&value| field.in_range(value))),
        Ok(_) => Ok(None),
        Err(error) => refusal(error).map(|()| None),
    }
}

/// Raise each of `mouse`'s wheels to the finest resolution its multiplier
/// allows: every multiplier in a feature report raised at once, written, and
/// read back, the device's answer adopted. A write the device accepts but
/// will not read back is taken to hold.
pub(crate) fn wheel_resolution(
    model: &ReportDescriptor,
    transport: &mut dyn HidTransport,
    mouse: &mut MouseDecoder,
) -> Result<(), DriverError> {
    let (wheel, pan) = mouse.wheels();
    let governs = |multiplier: &Field, axis: Option<Slot>| {
        axis.is_some_and(|axis| {
            multiplier
                .collection
                .is_some_and(|outer| model.encloses(outer, model.fields()[axis.field].collection))
        })
    };
    let Some(multipliers) = try_collect(
        model
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, field)| {
                field.kind == ReportKind::Feature && field.flags.is_variable() && field.size <= 32
            })
            .filter_map(|(index, field)| {
                let element = model.element_of(field, RESOLUTION_MULTIPLIER)?;
                let usable = (governs(field, wheel) || governs(field, pan))
                    && multiplier_range(field).is_some();
                usable.then_some(Slot {
                    field: index,
                    element,
                })
            }),
    ) else {
        return Ok(());
    };
    let mut done: Vec<ReportId> = Vec::new();
    for slot in &multipliers {
        let id = model.fields()[slot.field].report;
        if done.contains(&id) {
            continue;
        }
        if try_push(&mut done, id).is_none() {
            return Ok(());
        }
        let Some(mut report) = current(model, transport, id)? else {
            continue;
        };
        for slot in multipliers
            .iter()
            .filter(|slot| model.fields()[slot.field].report == id)
        {
            let field = &model.fields()[slot.field];
            if let Ok(finest) = u32::try_from(field.logical.1) {
                let _ = set_element(&mut report, field, slot.element, finest);
            }
        }
        if let Err(error) = transport.set_feature(id, &report) {
            refusal(error)?;
            continue;
        }
        let Some(mut confirmed) = try_collect(report.iter().copied()) else {
            continue;
        };
        let answered = match transport.get_feature(id, &mut confirmed) {
            Ok(got) => got == confirmed.len() && id.matches(&confirmed),
            Err(error) => {
                refusal(error)?;
                false
            }
        };
        let settled = if answered { &confirmed } else { &report };
        for slot in multipliers
            .iter()
            .filter(|slot| model.fields()[slot.field].report == id)
        {
            let field = &model.fields()[slot.field];
            let per_detent = field
                .value(settled, slot.element)
                .and_then(|setting| counts_per_detent(field, setting));
            mouse.adopt(
                per_detent.filter(|_| governs(field, wheel)),
                per_detent.filter(|_| governs(field, pan)),
            );
        }
    }
    Ok(())
}

/// A multiplier field whose settings are whole, non-negative counts it can
/// be set to: its logical and physical ranges.
fn multiplier_range(field: &Field) -> Option<((i64, i64), (i64, i64))> {
    let (low, high) = field.logical;
    let (least, most) = field.physical;
    let width = u32::from(field.size);
    let representable =
        (1..=32).contains(&width) && low >= 0 && high > low && high < (1i64 << width);
    (representable && least >= 1 && most >= least && most <= i64::from(u8::MAX))
        .then_some((field.logical, field.physical))
}

/// The counts a detent `setting` makes, through the physical range; `None`
/// for a setting outside the logical range.
fn counts_per_detent(field: &Field, setting: i64) -> Option<NonZeroU8> {
    let ((low, high), (least, most)) = multiplier_range(field)?;
    if !(low..=high).contains(&setting) {
        return None;
    }
    let counts = least + (setting - low) * (most - least) / (high - low);
    NonZeroU8::new(u8::try_from(counts).ok()?)
}
