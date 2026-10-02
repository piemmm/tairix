//! Input driver class (`drivers/input/*`).
//!
//! Input drivers report user-generated events: keyboard, pointer, and
//! scroll. The shipped drivers are `ps2`, `usb_hid`, and the virtio-input
//! pair.

use super::DriverError;

/// Discriminant for an [`InputEvent`].
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum InputEventKind {
    /// Keyboard key press or release; `value == 1` is press, `0` is
    /// release; `code` is the platform-neutral keycode.
    Key = 1,
    /// Pointer motion along an axis; `code` selects the axis
    /// (`0 = X`, `1 = Y`), `value` carries the signed delta.
    Pointer = 2,
    /// Scroll wheel along an axis, in scroll units
    /// ([`SCROLL_UNITS_PER_DETENT`] to a detent); `code` selects the axis as
    /// for `Pointer`. A positive `Y` value is toward the user and a positive
    /// `X` value toward the logical end, both scrolling toward the end.
    Scroll = 3,
}

/// How many scroll units one wheel detent is worth.
///
/// The one unit every layer carries a scroll in — the device decode, the seat
/// channel, and the window event — so a fine wheel reports its own fraction
/// of a detent and nothing between the device and the viewport rounds a slow
/// turn away. It is the evdev hi-res and Windows `WHEEL_DELTA` convention.
pub const SCROLL_UNITS_PER_DETENT: i32 = 120;

impl InputEventKind {
    /// Raw on-wire value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Construct an [`InputEventKind`] from its raw byte.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::OutOfRange`] if `raw` is not a defined
    /// variant.
    ///
    /// # Capabilities
    ///
    /// None.
    pub const fn from_u8(raw: u8) -> Result<Self, DriverError> {
        match raw {
            1 => Ok(Self::Key),
            2 => Ok(Self::Pointer),
            3 => Ok(Self::Scroll),
            _ => Err(DriverError::OutOfRange),
        }
    }
}

/// `code` of the X axis in an [`InputEventKind::Pointer`] /
/// [`InputEventKind::Scroll`] event — the platform-neutral axis encoding
/// every input decoder (`lib/hid`, `lib/virtio_input`) reports in. One
/// definition here; decoders import it rather than carrying a private copy.
pub const AXIS_X: u16 = 0;
/// `code` of the Y axis (see [`AXIS_X`]).
pub const AXIS_Y: u16 = 1;

/// `code` of pointer button `n` in an [`InputEventKind::Key`] event:
/// `0x110` primary/left, `0x111` secondary/right, `0x112` middle — the
/// `evdev` `BTN_LEFT`/`BTN_RIGHT`/`BTN_MIDDLE` codes a virtio pointer
/// device delivers, mirrored by the USB HID boot-mouse decode, so every
/// pointer producer sees one button vocabulary.
pub const POINTER_BUTTON_CODE_BASE: u16 = 0x110;
/// Pointer buttons the platform-neutral vocabulary models (primary,
/// secondary, middle), starting at [`POINTER_BUTTON_CODE_BASE`].
pub const POINTER_BUTTON_COUNT: u16 = 3;

/// A single input event.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InputEvent {
    /// What kind of event this is.
    pub kind: InputEventKind,
    /// Reserved; must be zero in `abi-v1`.
    pub reserved0: u8,
    /// Event-kind-specific identifier (keycode, axis index).
    pub code: u16,
    /// Event-kind-specific value (press/release, axis delta).
    pub value: i32,
}

/// Trait every input driver implements.
///
/// # Capabilities
///
/// Methods are gated by ownership of the
/// [`DriverHandle`](crate::driver::DriverHandle) (load-time
/// [`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD)). Input
/// drivers do not require an additional per-method capability — the
/// dispatcher routes events exclusively to the focused session, which
/// is itself a capability-checked object owned by `userland/gui/wm`.
pub trait Input {
    /// Drain pending events into `events`, returning the number of
    /// entries written.
    ///
    /// Returns `Ok(0)` when no events are pending.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BufferTooSmall`] if `events.is_empty()`.
    /// * [`DriverError::DeviceFault`] if the underlying transport
    ///   reported an unrecoverable error.
    ///
    /// # Capabilities
    ///
    /// Caller must present the driver's [`DriverHandle`].
    ///
    /// [`DriverHandle`]: crate::driver::DriverHandle
    fn poll(&mut self, events: &mut [InputEvent]) -> Result<usize, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trip() {
        assert_eq!(InputEventKind::from_u8(1), Ok(InputEventKind::Key));
        assert_eq!(InputEventKind::from_u8(2), Ok(InputEventKind::Pointer));
        assert_eq!(InputEventKind::from_u8(3), Ok(InputEventKind::Scroll));
        assert_eq!(InputEventKind::from_u8(0), Err(DriverError::OutOfRange));
    }

    struct MockInput {
        queue: [InputEvent; 4],
        pending: usize,
    }

    impl Input for MockInput {
        fn poll(&mut self, events: &mut [InputEvent]) -> Result<usize, DriverError> {
            if events.is_empty() {
                return Err(DriverError::BufferTooSmall);
            }
            let n = self.pending.min(events.len());
            events[..n].copy_from_slice(&self.queue[..n]);
            self.pending -= n;
            // Shift the queue down so subsequent polls see the rest.
            self.queue.copy_within(n..n + self.pending, 0);
            Ok(n)
        }
    }

    #[test]
    fn poll_drains_queue() {
        let zero = InputEvent {
            kind: InputEventKind::Key,
            reserved0: 0,
            code: 0,
            value: 0,
        };
        let mut dev = MockInput {
            queue: [
                InputEvent {
                    kind: InputEventKind::Key,
                    reserved0: 0,
                    code: 30,
                    value: 1,
                },
                InputEvent {
                    kind: InputEventKind::Key,
                    reserved0: 0,
                    code: 30,
                    value: 0,
                },
                zero,
                zero,
            ],
            pending: 2,
        };
        let mut out = [zero; 1];
        assert_eq!(dev.poll(&mut out), Ok(1));
        assert_eq!(out[0].code, 30);
        assert_eq!(out[0].value, 1);
        assert_eq!(dev.poll(&mut out), Ok(1));
        assert_eq!(out[0].value, 0);
        assert_eq!(dev.poll(&mut out), Ok(0));
    }

    #[test]
    fn poll_rejects_empty_buffer() {
        let zero = InputEvent {
            kind: InputEventKind::Key,
            reserved0: 0,
            code: 0,
            value: 0,
        };
        let mut dev = MockInput {
            queue: [zero; 4],
            pending: 0,
        };
        let mut empty: [InputEvent; 0] = [];
        assert_eq!(dev.poll(&mut empty), Err(DriverError::BufferTooSmall));
    }
}
