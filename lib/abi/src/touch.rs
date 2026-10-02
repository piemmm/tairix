//! Touch frames over the seat's touch channel, and the pinch they make.
//!
//! A touch device reports what its surface saw as **frames**: every contact
//! on it in one scan — touching, or lifting in this one — with the surface's
//! kind, its physical extent and the physical buttons held. A contact the
//! device stops reporting has lifted. A driver injects each frame whole
//! (`touch_inject`), so two devices' contacts can never interleave, and the
//! seat owner drains them (`touch_read`) into its gesture recogniser, which is
//! where what a contact *means* — a tap, a scroll, a pinch — is decided. The
//! frame carries device facts only: no policy, no screen geometry.
//!
//! A pinch reaches an application as a [`PinchPhase`] and a scale in 16.16
//! fixed point relative to the fingers' spread when it began
//! ([`PINCH_SCALE_ONE`]), so a zoom is the zoom it began at times the scale and
//! accumulates no rounding however long the pinch runs.
//!
//! # Wire layout
//!
//! A record is exactly [`TouchFrame::WIRE_LEN`] bytes, little-endian:
//!
//! | offset | size | field     | meaning                                        |
//! |-------:|-----:|-----------|------------------------------------------------|
//! |      0 |    4 | `magic`   | [`TOUCH_FRAME_MAGIC`]                           |
//! |      4 |    2 | `version` | ABI version ([`crate::ABI_VERSION_CURRENT`])   |
//! |      6 |    2 | `device`  | the injector's own index of the device          |
//! |      8 |    8 | `source`  | the injecting task, stamped by the kernel       |
//! |     16 |    8 | `time`    | monotonic nanoseconds at injection, likewise    |
//! |     24 |    1 | `surface` | a [`TouchSurface`] code                         |
//! |     25 |    1 | `buttons` | [`TouchButtons`] bits                           |
//! |     26 |    1 | `count`   | contacts carried, at most [`TOUCH_CONTACTS_MAX`] |
//! |     27 |    1 | reserved  | must be zero                                    |
//! |     28 |    2 | `width`   | physical width, tenths of a millimetre, 0 unknown |
//! |     30 |    2 | `height`  | physical height, likewise                       |
//! |     32 |   80 | contacts  | [`TOUCH_CONTACTS_MAX`] slots of 8 bytes         |
//!
//! A contact slot is `id` (`u16`), `x` and `y` (`u16`, normalised so 0 and
//! `u16::MAX` are the surface's two edges), a flags byte (bit 0: touching,
//! bit 1: the device judges it a palm, not a finger) and a zero byte. Slots
//! past `count` are zero.

use crate::le::{put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::Errno;

/// Magic number identifying an `abi-v1` touch frame record (`"TCH1"`
/// little-endian).
pub const TOUCH_FRAME_MAGIC: u32 = u32::from_le_bytes(*b"TCH1");

/// The most contacts one frame carries: ten fingers, the most any surface
/// tracks. A validation bound on the record, not a capacity.
pub const TOUCH_CONTACTS_MAX: usize = 10;

/// Byte length of one contact slot.
const CONTACT_LEN: usize = 8;

/// Offset of the first contact slot.
const CONTACTS_OFFSET: usize = 32;

/// Contact flag: the contact is touching the surface.
pub const CONTACT_TOUCHING: u8 = 1 << 0;
/// Contact flag: the device judges the contact a palm, not a finger.
pub const CONTACT_PALM: u8 = 1 << 1;
/// Every contact flag this version defines.
const CONTACT_FLAGS: u8 = CONTACT_TOUCHING | CONTACT_PALM;

/// A pinch's scale while the fingers are as far apart as when it began: one,
/// in 16.16 fixed point.
pub const PINCH_SCALE_ONE: u32 = 1 << 16;

/// Where a pinch is in its life.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PinchPhase {
    /// The fingers began to pinch; the scale is [`PINCH_SCALE_ONE`].
    Begin = 1,
    /// The fingers moved.
    Update = 2,
    /// The fingers lifted, or another joined them: the zoom stands.
    End = 3,
    /// The seat ended the pinch before the fingers did — the surface went
    /// away or the seat changed hands: the zoom returns to where it began.
    Cancel = 4,
}

impl PinchPhase {
    /// The wire code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode a wire code.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for any code but the four defined.
    pub const fn from_code(code: u8) -> Result<Self, Errno> {
        match code {
            1 => Ok(Self::Begin),
            2 => Ok(Self::Update),
            3 => Ok(Self::End),
            4 => Ok(Self::Cancel),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether the pinch is over.
    #[must_use]
    pub const fn ends(self) -> bool {
        matches!(self, Self::End | Self::Cancel)
    }
}

/// What kind of surface a frame comes from, which decides what a contact on
/// it means.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TouchSurface {
    /// A touchpad with separate buttons: a contact moves the pointer from
    /// wherever it is.
    Touchpad = 1,
    /// A touchpad whose whole surface is its button, which it reports as the
    /// primary button whichever finger pressed.
    Clickpad = 2,
    /// A touchscreen: a contact is the pointer at the place touched.
    Screen = 3,
}

impl TouchSurface {
    /// The wire code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode a wire code.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for any code but the three defined.
    pub const fn from_code(code: u8) -> Result<Self, Errno> {
        match code {
            1 => Ok(Self::Touchpad),
            2 => Ok(Self::Clickpad),
            3 => Ok(Self::Screen),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether a contact names a place on the screen rather than moving the
    /// pointer from where it is.
    #[must_use]
    pub const fn is_direct(self) -> bool {
        matches!(self, Self::Screen)
    }
}

/// The physical buttons held during a frame: bit 0 the primary (a clickpad's
/// surface), bit 1 the secondary, bit 2 the middle.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TouchButtons(u8);

impl TouchButtons {
    /// No button held.
    pub const NONE: Self = Self(0);
    /// The primary button.
    pub const PRIMARY: u8 = 1 << 0;
    /// The secondary button.
    pub const SECONDARY: u8 = 1 << 1;
    /// The middle button.
    pub const MIDDLE: u8 = 1 << 2;
    /// Every button bit this version defines.
    const ALL: u8 = Self::PRIMARY | Self::SECONDARY | Self::MIDDLE;

    /// The buttons whose bits `bits` sets.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a bit this version does not define.
    pub const fn from_bits(bits: u8) -> Result<Self, Errno> {
        if bits & !Self::ALL != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// The bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `button` (one of the bit constants) is held.
    #[must_use]
    pub const fn holds(self, button: u8) -> bool {
        self.0 & button != 0
    }
}

/// A surface's physical size, in tenths of a millimetre on each axis; zero on
/// an axis the device does not state.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TouchExtent {
    /// Width, tenths of a millimetre.
    pub width: u16,
    /// Height, tenths of a millimetre.
    pub height: u16,
}

/// Whether a contact is on the surface in its frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContactPhase {
    /// Touching the surface.
    Down,
    /// Lifted in this frame, at its last position.
    Up,
}

/// What the device judges a contact to be.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContactKind {
    /// A finger.
    Finger,
    /// A palm or another contact that is not a deliberate finger.
    Palm,
}

/// One contact of a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Contact {
    /// The device's tracking id, the same in every frame while the contact
    /// stays down.
    pub id: u16,
    /// On the surface, or lifting.
    pub phase: ContactPhase,
    /// Finger or palm.
    pub kind: ContactKind,
    /// Position across the surface: 0 at one edge, `u16::MAX` at the other.
    pub x: u16,
    /// Position down the surface, likewise.
    pub y: u16,
}

impl Contact {
    /// A finger touching at `(x, y)`.
    #[must_use]
    pub const fn finger(id: u16, x: u16, y: u16) -> Self {
        Self {
            id,
            phase: ContactPhase::Down,
            kind: ContactKind::Finger,
            x,
            y,
        }
    }

    /// This contact, lifting where it is.
    #[must_use]
    pub const fn lifted(self) -> Self {
        Self {
            phase: ContactPhase::Up,
            ..self
        }
    }

    fn flags(self) -> u8 {
        let touching = match self.phase {
            ContactPhase::Down => CONTACT_TOUCHING,
            ContactPhase::Up => 0,
        };
        let palm = match self.kind {
            ContactKind::Finger => 0,
            ContactKind::Palm => CONTACT_PALM,
        };
        touching | palm
    }

    fn write_to(self, slot: &mut [u8; CONTACT_LEN]) {
        put_u16(slot, 0, self.id);
        put_u16(slot, 2, self.x);
        put_u16(slot, 4, self.y);
        slot[6] = self.flags();
        slot[7] = 0;
    }

    fn read_from(slot: [u8; CONTACT_LEN]) -> Result<Self, Errno> {
        let flags = slot[6];
        if flags & !CONTACT_FLAGS != 0 {
            return Err(Errno::OutOfRange);
        }
        if slot[7] != 0 {
            return Err(Errno::BadMagic);
        }
        Ok(Self {
            id: read_u16(&slot, 0),
            phase: if flags & CONTACT_TOUCHING != 0 {
                ContactPhase::Down
            } else {
                ContactPhase::Up
            },
            kind: if flags & CONTACT_PALM != 0 {
                ContactKind::Palm
            } else {
                ContactKind::Finger
            },
            x: read_u16(&slot, 2),
            y: read_u16(&slot, 4),
        })
    }
}

/// One frame a touch device reported.
///
/// [`from_bytes`](Self::from_bytes) is the only way to build one from
/// untrusted bytes, and it validates the whole record; [`push`](Self::push)
/// refuses a contact the record could not carry, so a frame in hand is always
/// one the wire can state.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TouchFrame {
    device: u16,
    source: u64,
    time_ns: u64,
    surface: TouchSurface,
    buttons: TouchButtons,
    extent: TouchExtent,
    contacts: [Contact; TOUCH_CONTACTS_MAX],
    count: u8,
}

/// The value an unused contact slot holds in memory; never on the wire.
const NO_CONTACT: Contact = Contact {
    id: 0,
    phase: ContactPhase::Up,
    kind: ContactKind::Finger,
    x: 0,
    y: 0,
};

impl TouchFrame {
    /// Encoded size of a frame on the wire, in bytes.
    pub const WIRE_LEN: usize = CONTACTS_OFFSET + TOUCH_CONTACTS_MAX * CONTACT_LEN;

    /// An empty frame from the injector's device `device`, a `surface` of
    /// `extent` with `buttons` held. The source and time are the kernel's to
    /// stamp.
    #[must_use]
    pub const fn new(
        device: u16,
        surface: TouchSurface,
        buttons: TouchButtons,
        extent: TouchExtent,
    ) -> Self {
        Self {
            device,
            source: 0,
            time_ns: 0,
            surface,
            buttons,
            extent,
            contacts: [NO_CONTACT; TOUCH_CONTACTS_MAX],
            count: 0,
        }
    }

    /// Add `contact` to the frame.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] when the frame already carries
    /// [`TOUCH_CONTACTS_MAX`] contacts, or one with the same id: a frame
    /// names each contact once.
    pub fn push(&mut self, contact: Contact) -> Result<(), Errno> {
        let count = usize::from(self.count);
        if count == TOUCH_CONTACTS_MAX || self.contacts().iter().any(|held| held.id == contact.id) {
            return Err(Errno::OutOfRange);
        }
        self.contacts[count] = contact;
        self.count += 1;
        Ok(())
    }

    /// The same frame attributed to the injecting task `source` at the
    /// monotonic instant `time_ns`: what the kernel writes over whatever the
    /// injector stated, so no driver can speak for another, and a frame's age
    /// is the time it arrived rather than the time the seat got round to it.
    #[must_use]
    pub const fn stamped(self, source: u64, time_ns: u64) -> Self {
        Self {
            source,
            time_ns,
            ..self
        }
    }

    /// The contacts the frame carries.
    #[must_use]
    pub fn contacts(&self) -> &[Contact] {
        &self.contacts[..usize::from(self.count)]
    }

    /// The injector's own index of the device the frame came from.
    #[must_use]
    pub const fn device(&self) -> u16 {
        self.device
    }

    /// The task that injected the frame, as the kernel stamped it.
    #[must_use]
    pub const fn source(&self) -> u64 {
        self.source
    }

    /// When the frame was injected, in monotonic nanoseconds, as the kernel
    /// stamped it.
    #[must_use]
    pub const fn time_ns(&self) -> u64 {
        self.time_ns
    }

    /// What kind of surface the frame came from.
    #[must_use]
    pub const fn surface(&self) -> TouchSurface {
        self.surface
    }

    /// The physical buttons held.
    #[must_use]
    pub const fn buttons(&self) -> TouchButtons {
        self.buttons
    }

    /// The surface's physical size.
    #[must_use]
    pub const fn extent(&self) -> TouchExtent {
        self.extent
    }

    /// Encode the frame little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, TOUCH_FRAME_MAGIC);
        put_u16(&mut out, 4, crate::ABI_VERSION_CURRENT_U16);
        put_u16(&mut out, 6, self.device);
        put_u64(&mut out, 8, self.source);
        put_u64(&mut out, 16, self.time_ns);
        out[24] = self.surface.code();
        out[25] = self.buttons.bits();
        out[26] = self.count;
        put_u16(&mut out, 28, self.extent.width);
        put_u16(&mut out, 30, self.extent.height);
        let (slots, _) = out[CONTACTS_OFFSET..].as_chunks_mut::<CONTACT_LEN>();
        for (slot, contact) in slots.iter_mut().zip(self.contacts()) {
            contact.write_to(slot);
        }
        out
    }

    /// Decode `bytes`, refusing anything the record cannot mean.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole frame.
    /// * [`Errno::BadMagic`] — wrong magic, a non-zero reserved byte, or a
    ///   slot past `count` that is not zero.
    /// * [`Errno::AbiVersionUnsupported`] — not this ABI version.
    /// * [`Errno::OutOfRange`] — an unknown surface, button bit or contact
    ///   flag, more than [`TOUCH_CONTACTS_MAX`] contacts, or two contacts
    ///   with one id.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != TOUCH_FRAME_MAGIC {
            return Err(Errno::BadMagic);
        }
        if u32::from(read_u16(bytes, 4)) != crate::ABI_VERSION_CURRENT {
            return Err(Errno::AbiVersionUnsupported);
        }
        if bytes[27] != 0 {
            return Err(Errno::BadMagic);
        }
        let count = bytes[26];
        if usize::from(count) > TOUCH_CONTACTS_MAX {
            return Err(Errno::OutOfRange);
        }
        let mut frame = Self::new(
            read_u16(bytes, 6),
            TouchSurface::from_code(bytes[24])?,
            TouchButtons::from_bits(bytes[25])?,
            TouchExtent {
                width: read_u16(bytes, 28),
                height: read_u16(bytes, 30),
            },
        )
        .stamped(read_u64(bytes, 8), read_u64(bytes, 16));
        let (slots, _) = bytes[CONTACTS_OFFSET..Self::WIRE_LEN].as_chunks::<CONTACT_LEN>();
        for (index, slot) in slots.iter().enumerate() {
            if index < usize::from(count) {
                frame.push(Contact::read_from(*slot)?)?;
            } else if slot.iter().any(|&byte| byte != 0) {
                return Err(Errno::BadMagic);
            }
        }
        Ok(frame)
    }
}

#[cfg(test)]
#[path = "touch_tests.rs"]
mod tests;
