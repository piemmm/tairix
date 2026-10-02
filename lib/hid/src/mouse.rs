//! The mouse decoder: buttons, relative motion, and both wheels in scroll
//! units at the resolution the device confirmed (`plans/POINTING.md` PO2).

use core::num::NonZeroU8;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::input::{PointerButtonCode, PointerInput};
use tairix_abi::DriverError;

use crate::descriptor::{CollectionIndex, Field, ReportDescriptor, ReportKind, Usage};
use crate::usages::{AC_PAN, PAGE_BUTTON, WHEEL, X, Y};
use crate::{in_application, Decoded, SeatSink};

/// Where one value sits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Slot {
    pub(crate) field: usize,
    pub(crate) element: u16,
}

/// The buttons the seat names: primary, secondary, middle.
const BUTTONS: [PointerButtonCode; 3] = [
    PointerButtonCode::Primary,
    PointerButtonCode::Secondary,
    PointerButtonCode::Middle,
];

/// One mouse application.
#[derive(Debug)]
pub struct MouseDecoder {
    buttons: [Option<Slot>; 3],
    x: Slot,
    y: Slot,
    wheel: Option<Slot>,
    pan: Option<Slot>,
    wheel_per_detent: NonZeroU8,
    pan_per_detent: NonZeroU8,
    held: [bool; 3],
    wheel_carry: i64,
    pan_carry: i64,
}

/// How a field carries one of a pointer's values.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Carriage {
    Absent,
    Relative(Slot),
    Absolute,
}

impl Carriage {
    fn of(model: &ReportDescriptor, index: usize, field: &Field, usage: Usage) -> Self {
        if !field.flags.is_variable() {
            return Self::Absent;
        }
        match model.element_of(field, usage) {
            Some(element) if field.flags.is_relative() => Self::Relative(Slot {
                field: index,
                element,
            }),
            Some(_) => Self::Absolute,
            None => Self::Absent,
        }
    }

    const fn relative(self) -> Option<Slot> {
        match self {
            Self::Relative(slot) => Some(slot),
            Self::Absent | Self::Absolute => None,
        }
    }
}

impl MouseDecoder {
    /// The decoder for `application`, or `None` when it is not a relative
    /// pointer: it moves no X and Y, or places them absolutely.
    #[must_use]
    pub fn new(model: &ReportDescriptor, application: CollectionIndex) -> Option<Self> {
        let (mut x, mut y, mut wheel, mut pan) = (None, None, None, None);
        let mut buttons = [None; 3];
        for (index, field) in model.fields().iter().enumerate() {
            if field.kind != ReportKind::Input
                || field.size > 32
                || !in_application(model, field, application)
            {
                continue;
            }
            if field.flags.is_variable() {
                for element in 0..field.count {
                    let Some(usage) = model.element_usage(field, element) else {
                        continue;
                    };
                    let button = usage.id.checked_sub(1).map(usize::from);
                    if let (PAGE_BUTTON, Some(slot @ None)) =
                        (usage.page, button.and_then(|at| buttons.get_mut(at)))
                    {
                        *slot = Some(Slot {
                            field: index,
                            element,
                        });
                    }
                }
            }
            let (across, down) = (
                Carriage::of(model, index, field, X),
                Carriage::of(model, index, field, Y),
            );
            if across == Carriage::Absolute || down == Carriage::Absolute {
                return None;
            }
            x = x.or(across.relative());
            y = y.or(down.relative());
            wheel = wheel.or(Carriage::of(model, index, field, WHEEL).relative());
            pan = pan.or(Carriage::of(model, index, field, AC_PAN).relative());
        }
        Some(Self {
            buttons,
            x: x?,
            y: y?,
            wheel,
            pan,
            wheel_per_detent: NonZeroU8::MIN,
            pan_per_detent: NonZeroU8::MIN,
            held: [false; 3],
            wheel_carry: 0,
            pan_carry: 0,
        })
    }

    /// The field the wheel is read from, and the AC Pan's.
    pub(crate) const fn wheels(&self) -> (Option<Slot>, Option<Slot>) {
        (self.wheel, self.pan)
    }

    /// Count the wheel, or the pan, at `per_detent` counts a detent from now
    /// on: the resolution the device confirmed.
    pub(crate) fn adopt(&mut self, wheel: Option<NonZeroU8>, pan: Option<NonZeroU8>) {
        if let Some(per_detent) = wheel {
            self.wheel_per_detent = per_detent;
        }
        if let Some(per_detent) = pan {
            self.pan_per_detent = per_detent;
        }
    }

    /// Decode `report` into pointer records on `sink`.
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
        let carried = |slot: Slot| fields[slot.field].report.matches(report);
        let mine = carried(self.x)
            || carried(self.y)
            || self.buttons.iter().flatten().any(|&slot| carried(slot));
        if !mine {
            return Ok(Decoded::NotMine);
        }
        let read = |slot: Slot| fields[slot.field].value(report, slot.element);
        let mandatory = |slot: Slot| {
            if carried(slot) {
                read(slot).map(Some)
            } else {
                Some(None)
            }
        };
        let (Some(dx), Some(dy)) = (mandatory(self.x), mandatory(self.y)) else {
            return Ok(Decoded::Malformed);
        };
        let mut buttons = self.held;
        for (held, slot) in buttons.iter_mut().zip(self.buttons) {
            match slot.map(mandatory) {
                Some(Some(Some(value))) => *held = value != 0,
                Some(None) => return Ok(Decoded::Malformed),
                Some(Some(None)) | None => {}
            }
        }
        // A wheel past a short report did not turn: a boot mouse sends three
        // bytes or four.
        let optional = |slot: Option<Slot>| {
            slot.filter(|&slot| carried(slot))
                .and_then(read)
                .unwrap_or(0)
        };
        let (wheel, pan) = (optional(self.wheel), optional(self.pan));

        for (code, (was, is)) in BUTTONS.into_iter().zip(self.held.into_iter().zip(buttons)) {
            if was != is {
                sink.pointer(&if is {
                    PointerInput::Pressed(code)
                } else {
                    PointerInput::Released(code)
                })?;
            }
        }
        self.held = buttons;
        let (dx, dy) = (clamp(dx.unwrap_or(0)), clamp(dy.unwrap_or(0)));
        if dx != 0 || dy != 0 {
            sink.pointer(&PointerInput::MovedBy { dx, dy })?;
        }
        let down =
            scroll_units(wheel, self.wheel_per_detent, &mut self.wheel_carry).saturating_neg();
        let right = scroll_units(pan, self.pan_per_detent, &mut self.pan_carry);
        if down != 0 || right != 0 {
            sink.pointer(&PointerInput::Scrolled {
                dx: right,
                dy: down,
            })?;
        }
        Ok(Decoded::Applied)
    }

    /// Release every button held.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn release(&mut self, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        for (code, held) in BUTTONS.into_iter().zip(self.held) {
            if held {
                sink.pointer(&PointerInput::Released(code))?;
            }
        }
        self.held = [false; 3];
        Ok(())
    }
}

fn clamp(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// The scroll units `counts` counted at `per_detent` a detent are worth,
/// carrying the remainder so a fine wheel reports exactly a detent's units for
/// every detent it turns.
fn scroll_units(counts: i64, per_detent: NonZeroU8, carry: &mut i64) -> i32 {
    let total = counts
        .saturating_mul(i64::from(SCROLL_UNITS_PER_DETENT))
        .saturating_add(*carry);
    let per_detent = i64::from(per_detent.get());
    let units = total / per_detent;
    *carry = total - units * per_detent;
    clamp(units)
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
