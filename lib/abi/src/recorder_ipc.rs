//! The screen-recording protocol: what the desktop session and the recorder it
//! starts say to each other over the recorder's two pipes (`plans/CAPTURE.md`).
//!
//! Every message is one `lib/sandbox` frame whose first byte names it, and
//! anything that is not exactly one well-formed message is refused whole. Only
//! the session starts a recording: the recorder serves the session over the
//! pipes it was started with — the session's own child's, or, for a recording
//! whose sound an administrator allowed, those the login broker wired it with
//! and handed the session — and binds no endpoint, so nothing else can reach
//! it. The authority it records under — the capture
//! capability, and the seat's room for the desktop's own sound — is checked by
//! the kernel and the audio service, never claimed here.
//!
//! The pixels never cross a pipe. The session grants the recorder
//! [`RECORDER_SLOTS`] slots of its own memory, each laid out as
//! [`RecorderSlot`] says; it composes a frame into a free one and names it,
//! and the recorder hands the slot back once it has encoded it.
//!
//! The conversation runs in one order. [`RecorderMessage::Hello`] says the
//! recorder is up; [`SessionMessage::Start`] delegates it the output file and
//! grants it the slots; [`RecorderMessage::Ready`] says it has mapped them;
//! each [`SessionMessage::Frame`] is answered by [`RecorderMessage::Released`];
//! and [`SessionMessage::Stop`] by [`RecorderMessage::Finished`]. No message
//! names a process: the session has its recorder's from the kernel — the child
//! it spawned, or the broker's child as the broker read it — so the file and
//! the slots go to that process whatever a recorder might claim. The recorder
//! maps each slot only as a grant of the session its spawner named as its one
//! operand: the session itself, or the broker's attested requester.

use crate::driver::display::{DamageRect, DamageRects, DisplayFormat, DisplayMode, NO_RECT};
use crate::le::{exact_len, read_i32, read_u16, read_u32, read_u64};
use crate::Errno;

/// The entry point of the recorder component, which only the desktop session
/// starts.
pub const RECORDER_RUN_PATH: &str = "/System/Components/recorder.app/Run";

/// The protocol both sides speak; a `Start` naming another is refused.
pub const RECORDER_PROTOCOL_VERSION: u16 = 1;

/// The slots a recording runs on: one being encoded while the session
/// composes into the other, so neither waits on the other.
pub const RECORDER_SLOTS: usize = 2;

/// The most rectangles one frame names. The session fits what changed into
/// them, over-covering rather than losing a change.
pub const RECORDER_DAMAGE_RECTS: usize = 64;

/// One frame's damage.
pub type RecorderDamage = DamageRects<RECORDER_DAMAGE_RECTS>;

/// The soonest one frame follows another: thirty a second, rounded up so no
/// second holds a thirty-first.
pub const RECORDER_FRAME_SPACING_NS: u64 = 1_000_000_000 / 30 + 1;

/// How long a recorder has from its start to its `Ready` before the session
/// ends it.
pub const RECORDER_START_DEADLINE_NS: u64 = 10_000_000_000;

/// How long a recorder has from `Stop` to `Finished` — writing out what it
/// holds and the seek index — before the session ends it, keeping the file as
/// far as it got.
pub const RECORDER_FINISH_DEADLINE_NS: u64 = 30_000_000_000;

/// How a slot's pixels are laid out: premultiplied RGBA, as a window frame
/// is. A capture is opaque, so they are its straight colours too.
const FORMAT: DisplayFormat = DisplayFormat::Rgba8888;

const OP_START: u8 = 1;
const OP_FRAME: u8 = 2;
const OP_STOP: u8 = 3;

const OP_HELLO: u8 = 0x81;
const OP_READY: u8 = 0x82;
const OP_RELEASED: u8 = 0x83;
const OP_FINISHED: u8 = 0x84;
const OP_UNSTARTED: u8 = 0x85;

const START_SLOTS_AT: usize = 32;
const START_WIRE_LEN: usize = START_SLOTS_AT + RECORDER_SLOTS * 8;
const FRAME_RECTS_AT: usize = 16;
const FRAME_WIRE_LEN: usize = FRAME_RECTS_AT + RECORDER_DAMAGE_RECTS * DamageRect::WIRE_LEN;
const STOP_WIRE_LEN: usize = 1;
const HELLO_WIRE_LEN: usize = 1;
const READY_WIRE_LEN: usize = 1;
const RELEASED_WIRE_LEN: usize = 2;
const FINISHED_WIRE_LEN: usize = 16;
const UNSTARTED_WIRE_LEN: usize = 8;

/// The longest message the session sends: a frame.
pub const RECORDER_SESSION_MESSAGE_MAX: usize = FRAME_WIRE_LEN;

/// The longest message a recorder sends: its `Finished`.
pub const RECORDER_MESSAGE_MAX: usize = FINISHED_WIRE_LEN;

const _: () = assert!(
    RECORDER_MESSAGE_MAX >= HELLO_WIRE_LEN
        && RECORDER_MESSAGE_MAX >= READY_WIRE_LEN
        && RECORDER_MESSAGE_MAX >= RELEASED_WIRE_LEN
        && RECORDER_MESSAGE_MAX >= UNSTARTED_WIRE_LEN
);
const _: () = assert!(RECORDER_SESSION_MESSAGE_MAX >= START_WIRE_LEN);

/// One slot's layout: a picture of the recorded area, row by row.
///
/// Both sides derive it from the size a `Start` names, so the session never
/// describes it and a recorder cannot claim another.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecorderSlot {
    width: u32,
    height: u32,
}

impl RecorderSlot {
    /// The slot for a `width` × `height` area.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an empty area, [`Errno::LengthOutOfRange`]
    /// for one whose slot a `usize` cannot measure.
    pub fn new(width: u32, height: u32) -> Result<Self, Errno> {
        if width == 0 || height == 0 {
            return Err(Errno::OutOfRange);
        }
        if width.checked_mul(FORMAT.bytes_per_pixel()).is_none() {
            return Err(Errno::LengthOutOfRange);
        }
        let slot = Self { width, height };
        slot.checked_len().ok_or(Errno::LengthOutOfRange)?;
        Ok(slot)
    }

    fn checked_len(self) -> Option<usize> {
        usize::try_from(self.width)
            .ok()?
            .checked_mul(FORMAT.bytes_per_pixel() as usize)?
            .checked_mul(usize::try_from(self.height).ok()?)
    }

    /// The area's width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The area's height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The bytes one row takes.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.width as usize * FORMAT.bytes_per_pixel() as usize
    }

    /// The slot's whole length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checked_len().unwrap_or(0)
    }

    /// Whether the slot holds no bytes, which [`Self::new`] never builds.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How a slot is laid out, as a window frame of the area's size: what the
    /// session composes it by and the recorder reads it by.
    #[must_use]
    pub const fn display_mode(&self) -> DisplayMode {
        DisplayMode {
            width_px: self.width,
            height_px: self.height,
            stride_bytes: self.width * FORMAT.bytes_per_pixel(),
            format: FORMAT,
        }
    }
}

/// What a recording hears besides the picture.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum RecordSound {
    /// Nothing: the recording is silent.
    #[default]
    None,
    /// The default microphone.
    Microphone,
    /// The desktop's own sound: the mix its output plays.
    Desktop,
    /// The microphone and the desktop's own sound, mixed.
    Both,
}

impl RecordSound {
    /// Every choice, in the order a menu lists them.
    pub const ALL: [Self; 4] = [Self::None, Self::Microphone, Self::Desktop, Self::Both];

    /// Whether the recording hears the microphone.
    #[must_use]
    pub const fn microphone(self) -> bool {
        matches!(self, Self::Microphone | Self::Both)
    }

    /// Whether the recording hears the desktop's own sound.
    #[must_use]
    pub const fn desktop(self) -> bool {
        matches!(self, Self::Desktop | Self::Both)
    }

    const fn wire(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Microphone => 1,
            Self::Desktop => 2,
            Self::Both => 3,
        }
    }

    const fn from_wire(byte: u8) -> Result<Self, Errno> {
        match byte {
            0 => Ok(Self::None),
            1 => Ok(Self::Microphone),
            2 => Ok(Self::Desktop),
            3 => Ok(Self::Both),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// How a recording is begun.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordStart {
    /// The recorded area's size, which every slot is laid out for.
    pub slot: RecorderSlot,
    /// What the recording hears.
    pub sound: RecordSound,
    /// The handle the session delegated the recorder its output under: a
    /// hidden file the session opened, written with positional writes.
    pub output: u64,
    /// Each slot's grant handle, in slot order: the session's memory, granted
    /// to the recorder's process alone and mapped as the session's grant.
    pub slots: [u64; RECORDER_SLOTS],
    /// When the recording begins on the monotonic clock, which every frame's
    /// and every sound's time is measured from.
    pub started_ns: u64,
}

/// What the session says to its recorder.
// Decoded and handled at once; boxing a frame's damage would cost an
// allocation a frame.
#[allow(clippy::large_enum_variant)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SessionMessage {
    /// Begin: sent once, answering the recorder's `Hello`.
    Start(RecordStart),
    /// A frame is in a slot.
    Frame {
        /// The slot holding it.
        slot: u8,
        /// The monotonic instant it shows.
        at_ns: u64,
        /// What changed since the frame before it, in the slot's pixels; the
        /// first frame names the whole area.
        damage: RecorderDamage,
    },
    /// Finish the recording and say so.
    Stop,
}

/// Why a recording finished.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordEnding {
    /// The session asked it to stop.
    Stopped,
    /// Its volume filled, and the file was cut at the last whole cluster.
    DiskFull,
}

impl RecordEnding {
    const fn wire(self) -> u8 {
        match self {
            Self::Stopped => 1,
            Self::DiskFull => 2,
        }
    }

    const fn from_wire(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Stopped),
            2 => Ok(Self::DiskFull),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// What a recorder says to the session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecorderMessage {
    /// The recorder is up and speaks this protocol, sent first and once.
    Hello,
    /// The slots are mapped and the first frame may follow.
    Ready,
    /// A slot's frame is encoded, and the slot is the session's again.
    Released {
        /// The slot.
        slot: u8,
    },
    /// The recording is finished and its file whole; no frame follows.
    Finished {
        /// The bytes the file holds.
        bytes: u64,
        /// Why it finished.
        ending: RecordEnding,
    },
    /// The recorder cannot record, for this reason, and ends; only before
    /// `Ready`.
    Unstarted(Errno),
}

impl SessionMessage {
    /// Encode into the front of `out`, answering the bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it, and
    /// [`Errno::OutOfRange`] for a frame naming a slot past the ring.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Start(_) => START_WIRE_LEN,
            Self::Frame { slot, .. } if usize::from(*slot) >= RECORDER_SLOTS => {
                return Err(Errno::OutOfRange);
            }
            Self::Frame { .. } => FRAME_WIRE_LEN,
            Self::Stop => STOP_WIRE_LEN,
        };
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        match self {
            Self::Start(start) => {
                out[0] = OP_START;
                out[1] = start.sound.wire();
                out[2..4].copy_from_slice(&RECORDER_PROTOCOL_VERSION.to_le_bytes());
                out[4..8].copy_from_slice(&start.slot.width().to_le_bytes());
                out[8..12].copy_from_slice(&start.slot.height().to_le_bytes());
                out[16..24].copy_from_slice(&start.output.to_le_bytes());
                out[24..32].copy_from_slice(&start.started_ns.to_le_bytes());
                for (index, grant) in start.slots.iter().enumerate() {
                    let at = START_SLOTS_AT + index * 8;
                    out[at..at + 8].copy_from_slice(&grant.to_le_bytes());
                }
            }
            Self::Frame {
                slot,
                at_ns,
                damage,
            } => {
                out[0] = OP_FRAME;
                out[1] = *slot;
                out[2] = u8::try_from(damage.len()).unwrap_or(0);
                out[8..16].copy_from_slice(&at_ns.to_le_bytes());
                for (index, rect) in damage.rects().iter().enumerate() {
                    let at = FRAME_RECTS_AT + index * DamageRect::WIRE_LEN;
                    out[at..at + DamageRect::WIRE_LEN].copy_from_slice(&rect.to_le_bytes());
                }
            }
            Self::Stop => out[0] = OP_STOP,
        }
        Ok(len)
    }

    /// Decode one frame's payload.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for an unknown message or a stray byte,
    /// [`Errno::BufferTooSmall`] or [`Errno::LengthOutOfRange`] for a payload
    /// shorter or longer than its message, [`Errno::AbiVersionUnsupported`]
    /// for a `Start` of another protocol, [`Errno::OutOfRange`] for a `Start`
    /// whose output or a slot's grant is zero or whose slots share a grant,
    /// and the refusals of its fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let (&op, _) = bytes.split_first().ok_or(Errno::LengthOutOfRange)?;
        match op {
            OP_START => {
                exact_len(bytes, START_WIRE_LEN)?;
                if bytes[12..16] != [0; 4] {
                    return Err(Errno::BadMagic);
                }
                if read_u16(bytes, 2) != RECORDER_PROTOCOL_VERSION {
                    return Err(Errno::AbiVersionUnsupported);
                }
                let output = read_u64(bytes, 16);
                let mut slots = [0u64; RECORDER_SLOTS];
                for (index, grant) in slots.iter_mut().enumerate() {
                    *grant = read_u64(bytes, START_SLOTS_AT + index * 8);
                }
                let repeated = (1..RECORDER_SLOTS).any(|at| slots[..at].contains(&slots[at]));
                if output == 0 || slots.contains(&0) || repeated {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::Start(RecordStart {
                    slot: RecorderSlot::new(read_u32(bytes, 4), read_u32(bytes, 8))?,
                    sound: RecordSound::from_wire(bytes[1])?,
                    output,
                    slots,
                    started_ns: read_u64(bytes, 24),
                }))
            }
            OP_FRAME => decode_frame(bytes),
            OP_STOP => {
                exact_len(bytes, STOP_WIRE_LEN)?;
                Ok(Self::Stop)
            }
            _ => Err(Errno::BadMagic),
        }
    }
}

/// The body of a `Frame`.
fn decode_frame(bytes: &[u8]) -> Result<SessionMessage, Errno> {
    exact_len(bytes, FRAME_WIRE_LEN)?;
    let count = usize::from(bytes[2]);
    if count > RECORDER_DAMAGE_RECTS {
        return Err(Errno::LengthOutOfRange);
    }
    // Slots past the live count are reserved: a rectangle smuggled behind the
    // count is refused, never ignored.
    let tail = FRAME_RECTS_AT + count * DamageRect::WIRE_LEN;
    if bytes[3..8] != [0; 5] || bytes[tail..].iter().any(|&byte| byte != 0) {
        return Err(Errno::BadMagic);
    }
    let slot = bytes[1];
    if usize::from(slot) >= RECORDER_SLOTS {
        return Err(Errno::OutOfRange);
    }
    let mut rects = [NO_RECT; RECORDER_DAMAGE_RECTS];
    for (index, rect) in rects.iter_mut().enumerate().take(count) {
        *rect = DamageRect::from_bytes(&bytes[FRAME_RECTS_AT + index * DamageRect::WIRE_LEN..])?;
    }
    Ok(SessionMessage::Frame {
        slot,
        at_ns: read_u64(bytes, 8),
        damage: RecorderDamage::new(&rects[..count])?,
    })
}

impl RecorderMessage {
    /// Encode into the front of `out`, answering the bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Hello => HELLO_WIRE_LEN,
            Self::Ready => READY_WIRE_LEN,
            Self::Released { .. } => RELEASED_WIRE_LEN,
            Self::Finished { .. } => FINISHED_WIRE_LEN,
            Self::Unstarted(_) => UNSTARTED_WIRE_LEN,
        };
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        match self {
            Self::Hello => out[0] = OP_HELLO,
            Self::Ready => out[0] = OP_READY,
            Self::Released { slot } => {
                out[0] = OP_RELEASED;
                out[1] = *slot;
            }
            Self::Finished { bytes, ending } => {
                out[0] = OP_FINISHED;
                out[1] = ending.wire();
                out[8..16].copy_from_slice(&bytes.to_le_bytes());
            }
            Self::Unstarted(errno) => {
                out[0] = OP_UNSTARTED;
                out[4..8].copy_from_slice(&errno.as_i32().to_le_bytes());
            }
        }
        Ok(len)
    }

    /// Decode one frame's payload.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for an unknown message or a stray byte,
    /// [`Errno::BufferTooSmall`] or [`Errno::LengthOutOfRange`] for a payload
    /// shorter or longer than its message, and [`Errno::OutOfRange`] for a
    /// slot past the ring, or an unknown ending or error.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let (&op, _) = bytes.split_first().ok_or(Errno::LengthOutOfRange)?;
        match op {
            OP_HELLO => {
                exact_len(bytes, HELLO_WIRE_LEN)?;
                Ok(Self::Hello)
            }
            OP_READY => {
                exact_len(bytes, READY_WIRE_LEN)?;
                Ok(Self::Ready)
            }
            OP_RELEASED => {
                exact_len(bytes, RELEASED_WIRE_LEN)?;
                let slot = bytes[1];
                if usize::from(slot) >= RECORDER_SLOTS {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::Released { slot })
            }
            OP_FINISHED => {
                exact_len(bytes, FINISHED_WIRE_LEN)?;
                if bytes[2..8] != [0; 6] {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::Finished {
                    bytes: read_u64(bytes, 8),
                    ending: RecordEnding::from_wire(bytes[1])?,
                })
            }
            OP_UNSTARTED => {
                exact_len(bytes, UNSTARTED_WIRE_LEN)?;
                if bytes[1..4] != [0; 3] {
                    return Err(Errno::BadMagic);
                }
                let errno = Errno::from_i32(read_i32(bytes, 4)).ok_or(Errno::OutOfRange)?;
                Ok(Self::Unstarted(errno))
            }
            _ => Err(Errno::BadMagic),
        }
    }
}

#[cfg(test)]
#[path = "recorder_ipc_tests.rs"]
mod tests;
