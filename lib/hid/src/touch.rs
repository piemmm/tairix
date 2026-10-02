//! The digitizer decoder: a touch pad's or touch screen's finger collections
//! read into [`TouchFrame`]s, a frame the device spreads over several reports
//! joined into one.

use alloc::vec::Vec;

use tairix_abi::touch::{
    Contact, ContactKind, ContactPhase, TouchButtons, TouchExtent, TouchFrame, TouchSurface,
    TOUCH_CONTACTS_MAX,
};
use tairix_abi::DriverError;

use crate::descriptor::{
    CollectionIndex, CollectionKind, Field, ReportDescriptor, ReportKind, Usage,
};
use crate::mouse::Slot;
use crate::usages::{
    CONFIDENCE, CONTACT_COUNT, CONTACT_IDENTIFIER, FINGER, PAD_TYPE_DEPRESSIBLE, PAGE_BUTTON,
    TIP_SWITCH, X, Y,
};
use crate::{in_application, try_push, Decoded, SeatSink};

/// One contact slot: a Finger collection's fields.
#[derive(Clone, Copy, Debug)]
struct Finger {
    tip: Slot,
    x: Slot,
    y: Slot,
    confidence: Option<Slot>,
    id: Option<Slot>,
}

/// A frame still arriving: how many slots its first report said it holds, how
/// many have come, and the contacts among them touching.
#[derive(Clone, Copy, Debug)]
struct Pending {
    expected: u16,
    received: u16,
    frame: TouchFrame,
}

/// One touch pad or touch screen application.
#[derive(Debug)]
pub struct TouchDecoder {
    device: u16,
    surface: TouchSurface,
    extent: TouchExtent,
    fingers: Vec<Finger>,
    contact_count: Option<Slot>,
    buttons: [Option<Slot>; 3],
    contact_max: Option<u16>,
    pending: Option<Pending>,
    /// Whether the last frame told the seat of a contact or a button.
    touching: bool,
}

/// The element of a variable `field` carrying `usage`.
fn find(model: &ReportDescriptor, index: usize, field: &Field, usage: Usage) -> Option<Slot> {
    if !field.flags.is_variable() {
        return None;
    }
    model.element_of(field, usage).map(|element| Slot {
        field: index,
        element,
    })
}

/// What one report's offered slots read as, before any is applied.
struct Offered {
    slots: u16,
    touching: [Option<Contact>; TOUCH_CONTACTS_MAX],
}

impl TouchDecoder {
    /// The decoder for `application`, a touch screen when `screen`, or
    /// `None` when it declares no finger the decoder can read: one with a tip
    /// switch and an absolute X and Y.
    #[must_use]
    pub fn new(
        model: &ReportDescriptor,
        application: CollectionIndex,
        screen: bool,
        device: u16,
    ) -> Option<Self> {
        let mut fingers = Vec::new();
        let mut finger_collections = Vec::new();
        for (index, collection) in model.collections().iter().enumerate() {
            let at = CollectionIndex::new(index)?;
            if collection.kind != CollectionKind::Logical
                || collection.usage != FINGER
                || !model.encloses(application, Some(at))
            {
                continue;
            }
            try_push(&mut finger_collections, at)?;
            if let Some(finger) = Self::finger(model, at) {
                try_push(&mut fingers, finger)?;
            }
        }
        if fingers.is_empty() {
            return None;
        }
        let mut contact_count = None;
        let mut buttons = [None; 3];
        for (index, field) in model.fields().iter().enumerate() {
            let in_finger = finger_collections
                .iter()
                .any(|&finger| model.encloses(finger, field.collection));
            if field.kind != ReportKind::Input
                || field.size > 32
                || !field.flags.is_variable()
                || !in_application(model, field, application)
                || in_finger
            {
                continue;
            }
            contact_count = contact_count.or(find(model, index, field, CONTACT_COUNT));
            for element in 0..field.count {
                if let Some(Usage {
                    page: PAGE_BUTTON,
                    id: id @ 1..=3,
                }) = model.element_usage(field, element)
                {
                    buttons[usize::from(id - 1)].get_or_insert(Slot {
                        field: index,
                        element,
                    });
                }
            }
        }
        let first = fingers[0];
        let extent = TouchExtent {
            width: physical_tenths_mm(&model.fields()[first.x.field]),
            height: physical_tenths_mm(&model.fields()[first.y.field]),
        };
        // A pad whose only button is the first is pressed as a whole; the
        // device's own Pad Type, when it states one, decides instead.
        let clickpad = buttons[0].is_some() && buttons[1].is_none() && buttons[2].is_none();
        let surface = match (screen, clickpad) {
            (true, _) => TouchSurface::Screen,
            (false, true) => TouchSurface::Clickpad,
            (false, false) => TouchSurface::Touchpad,
        };
        Some(Self {
            device,
            surface,
            extent,
            fingers,
            contact_count,
            buttons,
            contact_max: None,
            pending: None,
            touching: false,
        })
    }

    fn finger(model: &ReportDescriptor, collection: CollectionIndex) -> Option<Finger> {
        let (mut tip, mut x, mut y, mut confidence, mut id) = (None, None, None, None, None);
        for (index, field) in model.fields().iter().enumerate() {
            if field.kind != ReportKind::Input
                || field.size > 32
                || !model.encloses(collection, field.collection)
            {
                continue;
            }
            tip = tip.or(find(model, index, field, TIP_SWITCH));
            confidence = confidence.or(find(model, index, field, CONFIDENCE));
            id = id.or(find(model, index, field, CONTACT_IDENTIFIER));
            if !field.flags.is_relative() {
                x = x.or(find(model, index, field, X));
                y = y.or(find(model, index, field, Y));
            }
        }
        Some(Finger {
            tip: tip?,
            x: x?,
            y: y?,
            confidence,
            id,
        })
    }

    /// The surface the decoder reports.
    #[must_use]
    pub const fn surface(&self) -> TouchSurface {
        self.surface
    }

    /// Adopt the device's Pad Type: zero, a depressible pad, is a clickpad.
    pub(crate) fn adopt_pad_type(&mut self, pad_type: i64) {
        if self.surface != TouchSurface::Screen {
            self.surface = if pad_type == PAD_TYPE_DEPRESSIBLE {
                TouchSurface::Clickpad
            } else {
                TouchSurface::Touchpad
            };
        }
    }

    /// Adopt the device's Contact Count Maximum, the most contacts a frame of
    /// its may name.
    pub(crate) fn adopt_contact_max(&mut self, max: u16) {
        self.contact_max = Some(max);
    }

    /// Decode `report`, delivering each frame it completes to `sink`.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn decode(
        &mut self,
        model: &ReportDescriptor,
        report: &[u8],
        sink: &mut dyn SeatSink,
    ) -> Result<Decoded, DriverError> {
        let fields = model.fields();
        let carried = |slot: &Slot| fields[slot.field].report.matches(report);
        if !self.fingers.iter().any(|finger| carried(&finger.tip)) {
            return Ok(Decoded::NotMine);
        }
        let read = |slot: &Slot| fields[slot.field].value(report, slot.element);
        let optional = |slot: &Option<Slot>| slot.as_ref().filter(|slot| carried(slot)).map(read);
        let count = match optional(&self.contact_count) {
            Some(Some(count)) => Some(u16::try_from(count).unwrap_or(u16::MAX)),
            Some(None) => return Ok(Decoded::Malformed),
            None => None,
        };
        if count
            .zip(self.contact_max)
            .is_some_and(|(count, max)| count > max)
        {
            return Ok(Decoded::Malformed);
        }
        let mut buttons = 0u8;
        for (bit, slot) in self.buttons.iter().enumerate() {
            match optional(slot) {
                Some(Some(0)) | None => {}
                Some(Some(_)) => buttons |= 1 << bit,
                Some(None) => return Ok(Decoded::Malformed),
            }
        }
        let continuing = count == Some(0) && self.pending.is_some();
        let limit = match (count, self.pending) {
            (Some(0), Some(pending)) => pending.expected - pending.received,
            (Some(0) | None, _) => u16::MAX,
            (Some(count), _) => count,
        };
        let Some(offered) = self.offered(fields, report, limit) else {
            return Ok(Decoded::Malformed);
        };
        // A frame a new one interrupts is dropped rather than delivered
        // short: contacts missing from a frame read as lifted.
        let (mut frame, received, expected) = match self.pending {
            Some(pending) if continuing => (
                pending.frame,
                pending.received.saturating_add(offered.slots),
                Some(pending.expected),
            ),
            _ => (self.frame(buttons), offered.slots, count),
        };
        for contact in offered.touching.into_iter().flatten() {
            // A contact past the most a frame holds, or repeating an id it
            // already holds, is not followed.
            let _ = frame.push(contact);
        }
        match expected {
            Some(expected) if received < expected => {
                self.pending = Some(Pending {
                    expected,
                    received,
                    frame,
                });
            }
            _ => {
                self.pending = None;
                self.deliver(&frame, sink)?;
            }
        }
        Ok(Decoded::Applied)
    }

    /// Read up to `limit` of the slots `report` carries, in report order;
    /// `None` when one of them is cut short.
    fn offered(&self, fields: &[Field], report: &[u8], limit: u16) -> Option<Offered> {
        let read = |slot: &Slot| fields[slot.field].value(report, slot.element);
        let mut offered = Offered {
            slots: 0,
            touching: [None; TOUCH_CONTACTS_MAX],
        };
        let mut touching = 0;
        for (index, finger) in self
            .fingers
            .iter()
            .enumerate()
            .filter(|(_, finger)| fields[finger.tip.field].report.matches(report))
        {
            if offered.slots >= limit {
                break;
            }
            let (tip, x, y) = (read(&finger.tip)?, read(&finger.x)?, read(&finger.y)?);
            let confidence = finger
                .confidence
                .as_ref()
                .map(read)
                .map_or(Some(None), |value| value.map(Some))?;
            let id = finger
                .id
                .as_ref()
                .map(read)
                .map_or(Some(None), |value| value.map(Some))?;
            offered.slots += 1;
            if tip == 0 || touching == TOUCH_CONTACTS_MAX {
                continue;
            }
            let id = id.map_or_else(
                || u16::try_from(index).unwrap_or(u16::MAX),
                |id| u16::try_from(id.rem_euclid(1 << 16)).unwrap_or(0),
            );
            offered.touching[touching] = Some(Contact {
                id,
                phase: ContactPhase::Down,
                kind: if confidence == Some(0) {
                    ContactKind::Palm
                } else {
                    ContactKind::Finger
                },
                x: normalised(&fields[finger.x.field], x),
                y: normalised(&fields[finger.y.field], y),
            });
            touching += 1;
        }
        Some(offered)
    }

    fn frame(&self, buttons: u8) -> TouchFrame {
        TouchFrame::new(
            self.device,
            self.surface,
            TouchButtons::from_bits(buttons).unwrap_or_default(),
            self.extent,
        )
    }

    fn deliver(&mut self, frame: &TouchFrame, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        self.touching = !frame.contacts().is_empty() || frame.buttons().bits() != 0;
        sink.touch(frame)
    }

    /// Lift every contact and button the last frame held.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn release(&mut self, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        self.pending = None;
        if self.touching {
            let lifted = self.frame(0);
            self.deliver(&lifted, sink)?;
        }
        Ok(())
    }
}

/// `value` across `field`'s logical range, 0 to `u16::MAX`, held to it.
fn normalised(field: &Field, value: i64) -> u16 {
    let (min, max) = field.logical;
    let span = max - min;
    if span <= 0 {
        return 0;
    }
    let offset = (value - min).clamp(0, span);
    u16::try_from(offset * i64::from(u16::MAX) / span).unwrap_or(u16::MAX)
}

/// HID unit codes for a length: SI centimetres and English inches, each to
/// the first power.
const UNIT_CENTIMETRE: u32 = 0x11;
const UNIT_INCH: u32 = 0x13;

/// `field`'s physical extent in tenths of a millimetre, zero when it states
/// no length unit.
fn physical_tenths_mm(field: &Field) -> u16 {
    let tenths_per_unit: i64 = match field.unit {
        UNIT_CENTIMETRE => 100,
        UNIT_INCH => 254,
        _ => return 0,
    };
    let span = field.physical.1 - field.physical.0;
    if span <= 0 {
        return 0;
    }
    let mut tenths = span.saturating_mul(tenths_per_unit);
    let exponent = field.unit_exponent;
    for _ in 0..exponent.unsigned_abs() {
        tenths = if exponent > 0 {
            tenths.saturating_mul(10)
        } else {
            tenths / 10
        };
    }
    u16::try_from(tenths).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[path = "touch_tests.rs"]
mod tests;
