//! A multi-touch device: contacts reported by the slot protocol (Linux
//! `Documentation/input/multi-touch-protocol.rst`, type B), each
//! `SYN_REPORT` closing one [`TouchFrame`] of every contact still down.

use tairix_abi::touch::{
    Contact, ContactKind, ContactPhase, TouchButtons, TouchExtent, TouchFrame, TouchSurface,
    TOUCH_CONTACTS_MAX,
};
use tairix_virtio::Transport;

use crate::{event_bits, reports, wire};

/// One axis's reported range: its least value and how far it reaches past it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range {
    min: i64,
    span: i64,
}

impl Range {
    /// `value` across the range, normalised so its two ends are 0 and
    /// `u16::MAX`; a value outside it is held to the nearer end.
    fn normalised(self, value: i32) -> u16 {
        let offset = (i64::from(value) - self.min).clamp(0, self.span);
        u16::try_from(offset * i64::from(u16::MAX) / self.span).unwrap_or(u16::MAX)
    }
}

/// One slot's contact.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Slot {
    tracking: Option<u16>,
    x: i32,
    y: i32,
    palm: bool,
}

/// A multi-touch device's decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MultiTouch {
    surface: TouchSurface,
    extent: TouchExtent,
    across: Range,
    down: Range,
    slots: [Slot; TOUCH_CONTACTS_MAX],
    /// The slot the next contact event addresses; `None` past the slots a
    /// frame can carry, whose contacts are not followed.
    current: Option<usize>,
    buttons: u8,
    /// The device lost events: everything up to the end of the frame is
    /// discarded, and then every contact lifts.
    dropped: bool,
}

impl MultiTouch {
    /// The decoder for a device that reports slotted contacts on two axes it
    /// states a range for, or `None` for one that does not.
    pub(crate) fn reported_by<T: Transport>(transport: &mut T) -> Option<Self> {
        let mut bitmap = [0u8; wire::CFG_ANSWER_LEN];
        let answered = event_bits(transport, wire::EV_ABS, &mut bitmap);
        let absolute = &bitmap[..answered];
        let slotted = [
            wire::ABS_MT_POSITION_X,
            wire::ABS_MT_POSITION_Y,
            wire::ABS_MT_TRACKING_ID,
        ]
        .iter()
        .all(|&code| reports(absolute, code));
        if !slotted {
            return None;
        }
        let (across, width) = axis(transport, wire::ABS_MT_POSITION_X)?;
        let (down, height) = axis(transport, wire::ABS_MT_POSITION_Y)?;
        Some(Self {
            surface: surface(property_bits(transport)),
            extent: TouchExtent { width, height },
            across,
            down,
            slots: [Slot::default(); TOUCH_CONTACTS_MAX],
            current: Some(0),
            buttons: 0,
            dropped: false,
        })
    }

    /// Read one event, answering the frame a `SYN_REPORT` closes.
    pub(crate) fn decode(&mut self, etype: u16, code: u16, value: i32) -> Option<TouchFrame> {
        match (etype, code) {
            (wire::EV_SYN, wire::SYN_REPORT) if self.dropped => {
                self.dropped = false;
                self.slots = [Slot::default(); TOUCH_CONTACTS_MAX];
                self.buttons = 0;
                Some(self.lifted())
            }
            (wire::EV_SYN, wire::SYN_REPORT) => Some(self.frame()),
            (wire::EV_SYN, wire::SYN_DROPPED) => {
                self.dropped = true;
                None
            }
            _ if self.dropped => None,
            (wire::EV_ABS, _) => {
                self.absolute(code, value);
                None
            }
            (wire::EV_KEY, _) => {
                self.key(code, value);
                None
            }
            _ => None,
        }
    }

    fn absolute(&mut self, code: u16, value: i32) {
        if code == wire::ABS_MT_SLOT {
            self.current = usize::try_from(value)
                .ok()
                .filter(|&slot| slot < TOUCH_CONTACTS_MAX);
            return;
        }
        let Some(slot) = self.current.and_then(|slot| self.slots.get_mut(slot)) else {
            return;
        };
        match code {
            // A negative id lifts the slot's contact; another begins a new
            // one, whose low half is unique among the few down at once.
            wire::ABS_MT_TRACKING_ID => {
                slot.tracking = if value < 0 {
                    None
                } else {
                    u16::try_from(value & 0xFFFF).ok()
                };
                slot.palm = false;
            }
            wire::ABS_MT_POSITION_X => slot.x = value,
            wire::ABS_MT_POSITION_Y => slot.y = value,
            wire::ABS_MT_TOOL_TYPE => slot.palm = value == wire::MT_TOOL_PALM,
            _ => {}
        }
    }

    fn key(&mut self, code: u16, value: i32) {
        let bit = match code {
            wire::BTN_LEFT => TouchButtons::PRIMARY,
            wire::BTN_RIGHT => TouchButtons::SECONDARY,
            wire::BTN_MIDDLE => TouchButtons::MIDDLE,
            _ => return,
        };
        if value == 0 {
            self.buttons &= !bit;
        } else {
            self.buttons |= bit;
        }
    }

    /// Every contact down, as a frame. A tracking id the device gave two
    /// slots is one contact, taken from the first.
    fn frame(&self) -> TouchFrame {
        let mut frame = TouchFrame::new(
            0,
            self.surface,
            TouchButtons::from_bits(self.buttons).unwrap_or_default(),
            self.extent,
        );
        for slot in &self.slots {
            let Some(id) = slot.tracking else {
                continue;
            };
            let _ = frame.push(Contact {
                id,
                phase: ContactPhase::Down,
                kind: if slot.palm {
                    ContactKind::Palm
                } else {
                    ContactKind::Finger
                },
                x: self.across.normalised(slot.x),
                y: self.down.normalised(slot.y),
            });
        }
        frame
    }

    /// A frame with every contact lifted and no button held: what the seat
    /// is told when the device stops reporting.
    pub(crate) const fn lifted(&self) -> TouchFrame {
        TouchFrame::new(0, self.surface, TouchButtons::NONE, self.extent)
    }
}

/// An axis's range, and its length in tenths of a millimetre where the device
/// states a resolution; `None` for an axis with no range to normalise over.
fn axis<T: Transport>(transport: &mut T, code: u16) -> Option<(Range, u16)> {
    let subsel = u8::try_from(code).ok()?;
    transport.write_config(wire::CFG_SELECT, &[wire::CFG_ABS_INFO, subsel]);
    let mut size = [0u8];
    transport.read_config(wire::CFG_SIZE, &mut size);
    if usize::from(size[0]) < wire::ABS_INFO_LEN {
        return None;
    }
    let mut info = [0u8; wire::ABS_INFO_LEN];
    transport.read_config(wire::CFG_ANSWER, &mut info);
    let field = |at: usize| [info[at], info[at + 1], info[at + 2], info[at + 3]];
    let min = i64::from(i32::from_le_bytes(field(0)));
    let max = i64::from(i32::from_le_bytes(field(4)));
    let resolution = i64::from(u32::from_le_bytes(field(16)));
    if max <= min {
        return None;
    }
    let span = max - min;
    let tenths = if resolution == 0 {
        0
    } else {
        u16::try_from(span * 10 / resolution).unwrap_or(u16::MAX)
    };
    Some((Range { min, span }, tenths))
}

/// The device's input property bits, or none where it states none.
fn property_bits<T: Transport>(transport: &mut T) -> u8 {
    transport.write_config(wire::CFG_SELECT, &[wire::CFG_PROP_BITS, 0]);
    let mut size = [0u8];
    transport.read_config(wire::CFG_SIZE, &mut size);
    if size[0] == 0 {
        return 0;
    }
    let mut bits = [0u8];
    transport.read_config(wire::CFG_ANSWER, &mut bits);
    bits[0]
}

/// What kind of surface the property bits name. A device that names neither
/// a pointer nor a direct surface reports contacts on a display, as a
/// touchscreen's are.
fn surface(properties: u8) -> TouchSurface {
    let has = |property: u8| properties & (1 << property) != 0;
    if has(wire::INPUT_PROP_DIRECT) || !has(wire::INPUT_PROP_POINTER) {
        TouchSurface::Screen
    } else if has(wire::INPUT_PROP_BUTTONPAD) {
        TouchSurface::Clickpad
    } else {
        TouchSurface::Touchpad
    }
}
