//! Display driver class (`drivers/display/*`).
//!
//! A display driver drives one output and allocates the buffers its device
//! reads ([`Display`]); a presenter fills them and has them shown by id, so no
//! frame is copied into memory the device reaches after it is drawn. A device
//! that composites layers or draws a pointer in hardware says so through
//! [`AcceleratedDisplay`] and [`HardwareCursor`]. Compositing policy lives
//! above this trait, in `userland/gui/wm`.

use super::DriverError;
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::seat::ReleaseSurface;
use crate::Errno;

/// The live seat-lease check a display driver applies before scanout.
///
/// The present right is *derived from* the live seat lease, not from the
/// ability to reach the framebuffer: mapping the surface (`CAP_MMIO_MAP`)
/// and owning the seat (`CAP_DISPLAY`) are separate facts, and this seam
/// is what couples them. A [`DriverHost`](crate::DriverHost) presenting
/// on behalf of a client exposes a gate bound to that client's
/// [`SeatLease`](crate::seat::SeatLease); the driver consults it at the
/// top of every [`Display::present`] / [`AcceleratedDisplay::present_layers`]
/// call, so a client whose lease was revoked cannot scan out even though
/// its framebuffer mapping still exists. The gate holds the lease handle
/// itself — the driver never sees or supplies it, so it cannot be forged
/// from the driver side.
pub trait SeatGate {
    /// Check that the presenting client's lease is the seat's live one.
    ///
    /// # Errors
    ///
    /// * [`DriverError::SeatRevoked`] if the client's lease was forcibly
    ///   revoked — the distinct refusal is how a well-behaved compositor
    ///   learns it lost the seat rather than scribbling over the new
    ///   foreground.
    /// * [`DriverError::PermissionDenied`] if the lease is not the live
    ///   grant for any other reason (the seat is unowned, held by another
    ///   task, or the handle is stale).
    fn check_present(&self) -> Result<(), DriverError>;
}

/// Pixel encodings supported by the abi-v1 display trait.
///
/// Names follow the byte order of the first pixel in memory; the
/// first letter is the byte at offset 0. New formats must take the
/// next free integer and are added in `abi-v2`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum DisplayFormat {
    /// 32-bit colour, byte order R, G, B, A.
    Rgba8888 = 1,
    /// 32-bit colour, byte order B, G, R, A.
    Bgra8888 = 2,
}

impl DisplayFormat {
    /// Raw on-wire value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a format from its wire value.
    ///
    /// # Errors
    ///
    /// [`crate::Errno::OutOfRange`] if `value` is not a known format (fail
    /// closed on a malformed argument).
    pub const fn from_u8(value: u8) -> Result<Self, crate::Errno> {
        match value {
            1 => Ok(Self::Rgba8888),
            2 => Ok(Self::Bgra8888),
            _ => Err(crate::Errno::OutOfRange),
        }
    }

    /// The format's short display spelling, for a reader naming a scan-out
    /// mode. One definition, so the command line and the desktop monitor
    /// cannot spell the same mode differently.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rgba8888 => "RGBA8888",
            Self::Bgra8888 => "BGRA8888",
        }
    }

    /// Bytes per pixel in this format.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8888 | Self::Bgra8888 => 4,
        }
    }
}

/// Whether a display is producing a picture.
///
/// Two states rather than VESA DPMS's four: standby and suspend were a CRT's
/// intermediate savings, and every panel and link this ABI drives treats them
/// as off, so a third state would name a distinction nothing below it keeps.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DisplayPower {
    /// Scanning the presented frame out.
    On = 1,
    /// Output switched off to save energy: the panel shows nothing and may
    /// sleep.
    Off = 2,
}

impl DisplayPower {
    /// Raw on-wire value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a power state from its wire value.
    ///
    /// # Errors
    ///
    /// [`crate::Errno::OutOfRange`] if `value` names neither state.
    pub const fn from_u8(value: u8) -> Result<Self, crate::Errno> {
        match value {
            1 => Ok(Self::On),
            2 => Ok(Self::Off),
            _ => Err(crate::Errno::OutOfRange),
        }
    }
}

/// The most rectangles one present may name.
///
/// A present carries its damage inline — as a slice through
/// [`Display::present`] in process, and in the fixed-width
/// [`DisplayRequest::Present`](crate::display_ipc::DisplayRequest) frame on
/// the wire — so this is a format bound the decoder enforces, not a capacity
/// that grows with the machine: a hostile request can never make a service
/// iterate further than this.
///
/// A frame's damage is a handful of disjoint places — a popup and the title
/// band its owner repaints, or the two rectangles a moved cursor leaves —
/// and eight of them keep the request frame at 152 bytes, small enough that
/// the whole request stays one plain copyable value. A producer holding more
/// rectangles than this merges them down to it ([`DamageList::fitted`]):
/// over-covering costs pixels, losing a rectangle would cost correctness.
pub const MAX_DAMAGE_RECTS: usize = 8;

/// One axis-aligned pixel rectangle of a presented frame, in surface
/// coordinates (origin top-left) — the *damage* a present names as changed
/// since the previous frame, so a driver can blit only the touched pixels.
///
/// A rectangle is validated against the active [`DisplayMode`] with
/// [`DamageRect::validate_in`] before any pixel access: a zero-sized or
/// out-of-bounds rectangle is refused, never clamped, so a hostile client
/// cannot steer the blit outside the frame it supplied.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DamageRect {
    /// X of the rectangle's left edge, in pixels from the surface's left.
    pub x: u32,
    /// Y of the rectangle's top edge, in pixels from the surface's top.
    pub y: u32,
    /// Width in pixels; never zero in a valid rectangle.
    pub width_px: u32,
    /// Height in pixels; never zero in a valid rectangle.
    pub height_px: u32,
}

/// The damage a present names: one to `N` pairwise-disjoint rectangles, held
/// inline so a request carrying them stays a plain copyable value. Disjoint,
/// so a present never makes its receiver convert a pixel twice. `N` is at
/// most 255, the most a wire's count byte holds; a wider list does not build:
///
/// ```compile_fail
/// use tairix_abi::driver::display::DamageRects;
/// let _ = DamageRects::<256>::new(&[]);
/// ```
///
/// Constructing one is the only way to name a present's damage, so the count
/// bound and the "no empty or overlapping rectangle" rules hold before a byte
/// is encoded as well as after one is decoded — a decoded list is exactly as trustworthy as
/// a locally built one. Rectangles beyond the live count are zero, which is
/// what makes a frame's reserved tail checkable.
///
/// Equality compares the rectangles named, not the dead slots behind them.
#[derive(Copy, Clone, Debug)]
pub struct DamageRects<const N: usize> {
    rects: [DamageRect; N],
    count: u8,
}

/// The damage a display present names: at most [`MAX_DAMAGE_RECTS`].
pub type DamageList = DamageRects<MAX_DAMAGE_RECTS>;

/// The zero rectangle filling a [`DamageRects`]'s unused slots. Not a valid
/// damage rectangle — it is never inside the live prefix.
pub(crate) const NO_RECT: DamageRect = DamageRect {
    x: 0,
    y: 0,
    width_px: 0,
    height_px: 0,
};

impl<const N: usize> DamageRects<N> {
    const COUNTED_IN_A_BYTE: () =
        assert!(N <= u8::MAX as usize, "a damage list's count is one byte");

    /// The list naming `rects`.
    ///
    /// # Errors
    ///
    /// [`crate::Errno::LengthOutOfRange`] if `rects` is empty, holds more than
    /// `N` entries, holds an empty rectangle, or holds two that overlap.
    pub fn new(rects: &[DamageRect]) -> Result<Self, crate::Errno> {
        let () = Self::COUNTED_IN_A_BYTE;
        let count = u8::try_from(rects.len()).map_err(|_| crate::Errno::LengthOutOfRange)?;
        if rects.is_empty() || rects.len() > N {
            return Err(crate::Errno::LengthOutOfRange);
        }
        let mut slots = [NO_RECT; N];
        for (index, (slot, rect)) in slots.iter_mut().zip(rects).enumerate() {
            let overlapped = rects[..index].iter().any(|earlier| earlier.overlaps(*rect));
            if rect.width_px == 0 || rect.height_px == 0 || overlapped {
                return Err(crate::Errno::LengthOutOfRange);
            }
            *slot = *rect;
        }
        Ok(Self {
            rects: slots,
            count,
        })
    }

    /// The list covering `rects`, however many there are: empty rectangles are
    /// dropped, while more than `N` remain the pair whose union adds the
    /// fewest pixels is merged into that union, and then any two that overlap
    /// are merged until none do. `None` when nothing is left to name.
    ///
    /// Merging over-covers by the least it can, where presenting the bounding
    /// box would turn two far-apart edits — a band's moving edges — into the
    /// whole area between them.
    #[must_use]
    pub fn fitted(rects: impl IntoIterator<Item = DamageRect>) -> Option<Self> {
        let mut slots = [NO_RECT; N];
        let mut count = 0;
        for rect in rects {
            if rect.width_px == 0 || rect.height_px == 0 {
                continue;
            }
            if count == N {
                absorb_cheapest(&mut slots, rect);
            } else {
                slots[count] = rect;
                count += 1;
            }
        }
        merge_overlapping(&mut slots, &mut count);
        Self::new(&slots[..count]).ok()
    }

    /// The rectangles the present names.
    #[must_use]
    pub fn rects(&self) -> &[DamageRect] {
        &self.rects[..usize::from(self.count)]
    }

    /// How many rectangles the present names.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the list names no rectangle — never, for a list built by
    /// [`new`](Self::new) or [`fitted`](Self::fitted).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The smallest rectangle covering every one this list names.
    #[must_use]
    pub fn bounds(&self) -> DamageRect {
        self.rects()
            .iter()
            .fold(NO_RECT, |covered, rect| covered.union(*rect))
    }
}

impl<const N: usize> PartialEq for DamageRects<N> {
    fn eq(&self, other: &Self) -> bool {
        self.rects() == other.rects()
    }
}

impl<const N: usize> Eq for DamageRects<N> {}

/// Merge overlapping rectangles of `slots[..count]` into their unions until no
/// two overlap; each merge leaves one fewer, so it ends.
fn merge_overlapping(slots: &mut [DamageRect], count: &mut usize) {
    'merge: loop {
        for a in 0..*count {
            for b in a + 1..*count {
                if slots[a].overlaps(slots[b]) {
                    slots[a] = slots[a].union(slots[b]);
                    *count -= 1;
                    slots[b] = slots[*count];
                    continue 'merge;
                }
            }
        }
        return;
    }
}

/// Take `incoming` into the full `slots` by merging the two rectangles of
/// them all whose union adds the fewest pixels.
fn absorb_cheapest(slots: &mut [DamageRect], incoming: DamageRect) {
    let area = |rect: DamageRect| u64::from(rect.width_px) * u64::from(rect.height_px);
    let growth = |a: DamageRect, b: DamageRect| {
        area(a.union(b)).saturating_sub(area(a).saturating_add(area(b)))
    };
    // `None` as the second of a pair names `incoming`.
    let mut cheapest: Option<(usize, Option<usize>, u64)> = None;
    for a in 0..slots.len() {
        let pairs = (a + 1..slots.len()).map(|b| (Some(b), growth(slots[a], slots[b])));
        for (b, cost) in pairs.chain(core::iter::once((None, growth(slots[a], incoming)))) {
            if cheapest.is_none_or(|(_, _, least)| cost < least) {
                cheapest = Some((a, b, cost));
            }
        }
    }
    match cheapest {
        Some((a, Some(b), _)) => {
            slots[a] = slots[a].union(slots[b]);
            slots[b] = incoming;
        }
        Some((a, None, _)) => slots[a] = slots[a].union(incoming),
        None => {}
    }
}

impl DamageRect {
    /// Encoded size on the wire: x, y, width and height, each a
    /// little-endian `u32`.
    pub const WIRE_LEN: usize = 16;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, self.x);
        put_u32(&mut out, 4, self.y);
        put_u32(&mut out, 8, self.width_px);
        put_u32(&mut out, 12, self.height_px);
        out
    }

    /// Decode from the first [`Self::WIRE_LEN`] bytes of `bytes`. Any four
    /// values decode: whether the rectangle fits a surface is the reader's
    /// to check against the surface ([`Self::validate_in`]).
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when `bytes` is shorter than a rectangle.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        let bytes = bytes.get(..Self::WIRE_LEN).ok_or(Errno::BufferTooSmall)?;
        Ok(Self {
            x: read_u32(bytes, 0),
            y: read_u32(bytes, 4),
            width_px: read_u32(bytes, 8),
            height_px: read_u32(bytes, 12),
        })
    }

    /// The rectangle covering the whole of `mode`'s surface.
    #[must_use]
    pub const fn full(mode: &DisplayMode) -> Self {
        Self {
            x: 0,
            y: 0,
            width_px: mode.width_px,
            height_px: mode.height_px,
        }
    }

    /// The smallest rectangle covering both `self` and `other`; an empty one
    /// covers nothing and leaves the other as it is.
    ///
    /// The edges are summed in `u64` and narrowed back, so an edge past
    /// `u32::MAX` saturates rather than wrapping the union small.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let empty = |rect: &Self| rect.width_px == 0 || rect.height_px == 0;
        if empty(&self) {
            return other;
        }
        if empty(&other) {
            return self;
        }
        let far = |start: u32, extent: u32| {
            u32::try_from(u64::from(start) + u64::from(extent)).unwrap_or(u32::MAX)
        };
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = far(self.x, self.width_px).max(far(other.x, other.width_px));
        let bottom = far(self.y, self.height_px).max(far(other.y, other.height_px));
        Self {
            x,
            y,
            width_px: right - x,
            height_px: bottom - y,
        }
    }

    /// Whether this rectangle and `other` share a pixel; an empty one shares
    /// none.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        let far = |start: u32, extent: u32| u64::from(start) + u64::from(extent);
        u64::from(self.x) < far(other.x, other.width_px)
            && u64::from(other.x) < far(self.x, self.width_px)
            && u64::from(self.y) < far(other.y, other.height_px)
            && u64::from(other.y) < far(self.y, self.height_px)
    }

    /// Whether this rectangle covers the whole of `mode`'s surface.
    #[must_use]
    pub const fn covers(&self, mode: &DisplayMode) -> bool {
        self.x == 0
            && self.y == 0
            && self.width_px == mode.width_px
            && self.height_px == mode.height_px
    }

    /// Check the rectangle is non-empty and lies wholly inside `mode`'s
    /// surface.
    ///
    /// The extents are summed in `u64`, so a hostile `x + width_px` cannot
    /// wrap back into range.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] if the rectangle is empty or any
    /// edge falls outside the surface.
    pub const fn validate_in(&self, mode: &DisplayMode) -> Result<(), DriverError> {
        if self.width_px == 0 || self.height_px == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        let right = self.x as u64 + self.width_px as u64;
        let bottom = self.y as u64 + self.height_px as u64;
        if right > mode.width_px as u64 || bottom > mode.height_px as u64 {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(())
    }

    /// Check a whole present's damage list against `mode`.
    ///
    /// The list is checked before its first pixel is read, so a bad
    /// rectangle refuses the present rather than half-applying it.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] if the list is empty, holds more
    /// than [`MAX_DAMAGE_RECTS`] rectangles, or any rectangle is empty or
    /// falls outside the surface.
    pub fn validate_list(damage: &[Self], mode: &DisplayMode) -> Result<(), DriverError> {
        if damage.is_empty() || damage.len() > MAX_DAMAGE_RECTS {
            return Err(DriverError::LengthOutOfRange);
        }
        for rect in damage {
            rect.validate_in(mode)?;
        }
        Ok(())
    }
}

/// Which way a sync pulse goes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Polarity {
    /// High during the pulse.
    Positive,
    /// Low during the pulse.
    Negative,
}

/// Whether a frame is sent whole or as two fields.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Scan {
    /// Every line of each frame.
    Progressive,
    /// Alternate lines in each of two fields.
    Interlaced,
}

/// One axis of a timing: what is shown, then the front porch, the sync pulse
/// and the back porch.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Axis {
    /// Pixels or lines shown.
    pub active: u16,
    /// Blanking before the sync pulse.
    pub front: u16,
    /// The sync pulse.
    pub sync: u16,
    /// Blanking after it.
    pub back: u16,
}

impl Axis {
    /// The whole axis, shown and blanked.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.active as u32 + self.front as u32 + self.sync as u32 + self.back as u32
    }
}

/// How a frame is clocked out.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Timing {
    /// The pixel clock, in kHz.
    pub clock_khz: u32,
    /// Pixels per line.
    pub horizontal: Axis,
    /// Lines per frame, an interlaced frame's two fields together.
    pub vertical: Axis,
    /// The horizontal sync pulse's polarity.
    pub hsync: Polarity,
    /// The vertical sync pulse's polarity.
    pub vsync: Polarity,
    /// Whether the frame is interlaced.
    pub scan: Scan,
    /// How many times each pixel is sent: 2 where a code doubles a narrow
    /// timing's clock.
    pub repetition: u8,
}

impl Timing {
    /// The active pixels across and lines down: the size the timing shows.
    #[must_use]
    pub fn active_size(&self) -> (u32, u32) {
        (
            u32::from(self.horizontal.active),
            u32::from(self.vertical.active),
        )
    }

    /// The frame rate, in millihertz; `0` for a timing with no blanking or
    /// clock to divide.
    #[must_use]
    pub fn refresh_mhz(&self) -> u32 {
        let pixels = u64::from(self.horizontal.total()) * u64::from(self.vertical.total());
        let clock_hz = u64::from(self.clock_khz) * 1_000;
        clock_hz
            .saturating_mul(1_000)
            .checked_div(pixels)
            .and_then(|refresh| u32::try_from(refresh).ok())
            .unwrap_or(0)
    }

    /// The rate a line's active pixels pass at, in kHz: the pixel clock
    /// scaled by the active share of a line, which is what a compositor that
    /// fills each line during its active part must keep up with; `0` for a
    /// line with no blanking or clock.
    #[must_use]
    pub fn active_rate_khz(&self) -> u32 {
        (u64::from(self.clock_khz) * u64::from(self.horizontal.active))
            .checked_div(u64::from(self.horizontal.total()))
            .and_then(|rate| u32::try_from(rate).ok())
            .unwrap_or(0)
    }
}

impl Timing {
    /// Encoded size on the wire: the clock (4), each axis's active, front,
    /// sync and back (8 each), the flags (1) — the horizontal and vertical
    /// syncs' polarities and the scan — the repetition (1), and a reserved
    /// pair (2) that must be zero.
    pub const WIRE_LEN: usize = 24;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, self.clock_khz);
        for (at, axis) in [(4, &self.horizontal), (12, &self.vertical)] {
            put_u16(&mut out, at, axis.active);
            put_u16(&mut out, at + 2, axis.front);
            put_u16(&mut out, at + 4, axis.sync);
            put_u16(&mut out, at + 6, axis.back);
        }
        out[20] = (u8::from(self.hsync == Polarity::Positive) * TIMING_HSYNC_POSITIVE)
            | (u8::from(self.vsync == Polarity::Positive) * TIMING_VSYNC_POSITIVE)
            | (u8::from(self.scan == Scan::Interlaced) * TIMING_INTERLACED);
        out[21] = self.repetition;
        out
    }

    /// Decode from `bytes`, refusing a timing nothing could clock out.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — `bytes` is shorter than
    ///   [`Self::WIRE_LEN`].
    /// * [`Errno::BadMagic`] — an unknown flag or a dirty reserved pair.
    /// * [`Errno::LengthOutOfRange`] — no clock, an axis showing nothing or
    ///   pulsing no sync, or a repetition outside `1..=MAX_REPETITION`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        let bytes = bytes.get(..Self::WIRE_LEN).ok_or(Errno::BufferTooSmall)?;
        let flags = bytes[20];
        if flags & !TIMING_FLAGS_KNOWN != 0 || bytes[22] != 0 || bytes[23] != 0 {
            return Err(Errno::BadMagic);
        }
        let axis = |at: usize| Axis {
            active: read_u16(bytes, at),
            front: read_u16(bytes, at + 2),
            sync: read_u16(bytes, at + 4),
            back: read_u16(bytes, at + 6),
        };
        let polarity = |bit: u8| {
            if flags & bit == 0 {
                Polarity::Negative
            } else {
                Polarity::Positive
            }
        };
        let timing = Self {
            clock_khz: read_u32(bytes, 0),
            horizontal: axis(4),
            vertical: axis(12),
            hsync: polarity(TIMING_HSYNC_POSITIVE),
            vsync: polarity(TIMING_VSYNC_POSITIVE),
            scan: if flags & TIMING_INTERLACED == 0 {
                Scan::Progressive
            } else {
                Scan::Interlaced
            },
            repetition: bytes[21],
        };
        let shown = |axis: &Axis| axis.active != 0 && axis.sync != 0;
        if timing.clock_khz == 0
            || !shown(&timing.horizontal)
            || !shown(&timing.vertical)
            || !(1..=MAX_REPETITION).contains(&timing.repetition)
        {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(timing)
    }
}

/// The most times a timing may send each pixel: CTA-861's ten.
pub const MAX_REPETITION: u8 = 10;

/// A timing's flag: the horizontal sync pulses high.
const TIMING_HSYNC_POSITIVE: u8 = 1 << 0;
/// A timing's flag: the vertical sync pulses high.
const TIMING_VSYNC_POSITIVE: u8 = 1 << 1;
/// A timing's flag: the frame is interlaced.
const TIMING_INTERLACED: u8 = 1 << 2;
/// Every flag a timing's wire form defines.
const TIMING_FLAGS_KNOWN: u8 = TIMING_HSYNC_POSITIVE | TIMING_VSYNC_POSITIVE | TIMING_INTERLACED;

/// The mode a display's output runs, as [`Display::output`] answers it.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DisplayMode {
    /// Surface width in pixels.
    pub width_px: u32,
    /// Surface height in pixels.
    pub height_px: u32,
    /// Distance in bytes between the start of consecutive scanlines.
    pub stride_bytes: u32,
    /// Pixel encoding.
    pub format: DisplayFormat,
}

impl DisplayMode {
    /// The bytes a surface of this mode spans: every row a stride long.
    #[must_use]
    pub const fn len_bytes(&self) -> u64 {
        self.stride_bytes as u64 * self.height_px as u64
    }
}

/// The rate an output scans out at, in millihertz.
///
/// Not part of [`DisplayMode`]: a mode is a frame's layout and is compared
/// whole to decide whether every buffer must be rebuilt, and a rate that
/// changes alone rebuilds nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct RefreshRate(u32);

impl RefreshRate {
    /// The slowest rate an output may state: one frame a second.
    ///
    /// A format bound, not a capacity: a rate outside
    /// `MIN_MHZ..=MAX_MHZ` is refused, so a broken or hostile driver cannot
    /// set a desktop waking at a nanosecond period or stalling for a minute.
    pub const MIN_MHZ: u32 = 1_000;

    /// The fastest rate an output may state: a thousand frames a second.
    pub const MAX_MHZ: u32 = 1_000_000;

    /// The rate the desktop paces an output at that states none: the
    /// firmware framebuffers, which are handed no timing.
    pub const ASSUMED: Self = Self(60_000);

    /// The rate an output `stated`, or [`Self::ASSUMED`] for one that states
    /// none.
    #[must_use]
    pub const fn or_assumed(stated: Option<Self>) -> Self {
        match stated {
            Some(rate) => rate,
            None => Self::ASSUMED,
        }
    }

    /// The rate of `millihertz`, or `None` outside the bound.
    #[must_use]
    pub const fn from_mhz(millihertz: u32) -> Option<Self> {
        if millihertz >= Self::MIN_MHZ && millihertz <= Self::MAX_MHZ {
            Some(Self(millihertz))
        } else {
            None
        }
    }

    /// The rate a `timing` scans at, or `None` where it states none inside
    /// the bound.
    #[must_use]
    pub fn of(timing: &Timing) -> Option<Self> {
        Self::from_mhz(timing.refresh_mhz())
    }

    /// The rate, in millihertz.
    #[must_use]
    pub const fn mhz(self) -> u32 {
        self.0
    }

    /// One frame's period at this rate, in nanoseconds.
    #[must_use]
    pub const fn frame_ns(self) -> u64 {
        1_000_000_000_000 / self.0 as u64
    }
}

/// What an output runs: its mode, and the rate it scans at where its device
/// states one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutputMode {
    /// The frame layout.
    pub mode: DisplayMode,
    /// The scan-out rate, or `None` where the device states none.
    pub refresh: Option<RefreshRate>,
}

/// What a display device is, beyond the mode it scans out: the memory it
/// owns, its layer compositor and its cursor plane — the device half of the
/// graphics reading a monitor draws, gathered from [`Display::memory`],
/// [`Display::as_accelerated`] and [`Display::hardware_cursor`].
///
/// An in-process value, not a wire record: the monitor's on-the-wire form is
/// [`DisplayStats`](crate::display_ipc::DisplayStats).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DisplayDeviceReport {
    /// Bytes of the device's own memory holding buffers. Necessarily `0`
    /// when [`Self::mem_total_bytes`] is.
    pub mem_resident_bytes: u64,
    /// Bytes of memory the device owns. `0` means the device has **no memory
    /// of its own** — a firmware framebuffer scanning out of system RAM — not
    /// that none is free.
    pub mem_total_bytes: u64,
    /// What the hardware compositor can do, or [`None`] where the device has
    /// none and every frame is composited in software.
    pub accel: Option<AccelCaps>,
    /// The cursor plane, or [`None`] where the pointer is composited in
    /// software.
    pub cursor: Option<CursorCaps>,
}

impl DisplayDeviceReport {
    /// A device with no memory of its own, no hardware compositor and no
    /// cursor plane: what a firmware framebuffer honestly is.
    pub const SOFTWARE: Self = Self {
        mem_resident_bytes: 0,
        mem_total_bytes: 0,
        accel: None,
        cursor: None,
    };

    /// What `display` is.
    pub fn of(display: &mut dyn Display) -> Self {
        let memory = display.memory();
        Self {
            mem_resident_bytes: memory.resident_bytes,
            mem_total_bytes: memory.total_bytes,
            accel: display.as_accelerated().map(|accel| accel.accel_caps()),
            cursor: display.hardware_cursor().map(|cursor| cursor.cursor_caps()),
        }
    }
}

/// Where a display driver's device reaches the kernel console it hosts, as
/// `console_adopt` answers it.
///
/// The console is laid out as [`DisplayFormat::Bgra8888`] at the mode the
/// driver asked for, `stride_bytes` from one row to the next.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HostedConsole {
    /// Where the device reads the first pixel: its I/O virtual address behind
    /// a translation unit, else its bus address.
    pub device_addr: u64,
    /// Bytes from one row to the next.
    pub stride_bytes: u32,
}

impl HostedConsole {
    /// Encoded size on the wire: the device address, the stride, and a
    /// reserved word that must be zero.
    pub const WIRE_LEN: usize = 16;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        out[..8].copy_from_slice(&self.device_addr.to_le_bytes());
        put_u32(&mut out, 8, self.stride_bytes);
        out
    }

    /// Decode from `bytes`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a short buffer, [`Errno::BadMagic`] for
    /// a dirty reserved word, and [`Errno::LengthOutOfRange`] for a stride of
    /// zero.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        let bytes = bytes.get(..Self::WIRE_LEN).ok_or(Errno::BufferTooSmall)?;
        if read_u32(bytes, 12) != 0 {
            return Err(Errno::BadMagic);
        }
        let stride_bytes = read_u32(bytes, 8);
        if stride_bytes == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        let mut address = [0u8; 8];
        address.copy_from_slice(&bytes[..8]);
        Ok(Self {
            device_addr: u64::from_le_bytes(address),
            stride_bytes,
        })
    }
}

/// Most layers one [`AcceleratedDisplay::present_layers`] may carry: a wire
/// bound, as [`MAX_DAMAGE_RECTS`] is, not what any engine can composite —
/// that is [`AccelCaps::max_layers`], never above it.
pub const MAX_ACCEL_LAYERS: usize = 8;

/// A buffer a display driver allocated in memory its device reads, by the id
/// the driver gave it. Never zero.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BufferId(core::num::NonZeroU32);

impl BufferId {
    /// The id `raw`, or `None` for `0`, which names no buffer.
    #[must_use]
    pub const fn new(raw: u32) -> Option<Self> {
        match core::num::NonZeroU32::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A buffer [`Display::alloc_buffer`] made: its id, the shared region it is,
/// which the driver grants the presenter that asked, and its row stride.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocatedBuffer {
    /// The driver's name for the buffer.
    pub id: BufferId,
    /// The shared region holding it.
    pub region: u64,
    /// Bytes from one row to the next.
    pub stride_bytes: u32,
}

/// How a present ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Presented {
    /// The buffer is on screen, and every buffer it replaced is free.
    Shown,
    /// The device takes the buffer at its next frame start; the buffer it
    /// replaces is read until [`Display::service_events`] reports the latch.
    Queued,
}

/// What [`Display::service_events`] found.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Serviced {
    /// A queued present latched, so the buffer it replaced is free.
    pub latched: bool,
}

/// The memory a display device owns, as [`Display::memory`] reports it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DisplayMemory {
    /// Bytes of it holding buffers. Necessarily `0` when
    /// [`Self::total_bytes`] is.
    pub resident_bytes: u64,
    /// Bytes the device owns. `0` means it has **no memory of its own** — it
    /// scans out of system RAM — not that none is free.
    pub total_bytes: u64,
}

impl DisplayMemory {
    /// A device scanning out of system RAM.
    pub const NONE: Self = Self {
        resident_bytes: 0,
        total_bytes: 0,
    };
}

/// Trait every display driver implements: an output, and the buffers its
/// device reads, which the driver allocates and a presenter fills.
///
/// # Capabilities
///
/// Every method is gated by ownership of the [`DriverHandle`] returned
/// from the driver's `register` entry point. The load-time grant of
/// [`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD) is what
/// permits the host to issue that handle; the per-method dispatcher
/// re-verifies the handle on every call.
///
/// [`DriverHandle`]: crate::driver::DriverHandle
pub trait Display {
    /// The mode the display's output runs, or [`None`] while there is no
    /// output: no sink connected, or none the device can drive.
    ///
    /// The mode is the driver's to choose for the sink it has. It moves only
    /// across [`Self::service_events`], so a caller that read it before a
    /// call it then makes on the display has read the state that call meets.
    fn output(&self) -> Option<DisplayMode>;

    /// The rate the output scans at, or `None` where the device states none.
    ///
    /// It moves across [`Self::service_events`] exactly as [`Self::output`]
    /// does. The default states none, the truth for a device handed no timing.
    fn refresh(&self) -> Option<RefreshRate> {
        None
    }

    /// Collect what woke the display's event sources — a sink arriving or
    /// leaving, a mode set completing, a queued present latching — and move
    /// [`Self::output`] to match.
    ///
    /// The hosting loop calls it when one of the sources the driver handed it
    /// fired, and after every request it serves where
    /// [`Self::requests_take_wakes`], since a request that waited on the
    /// device may have taken the wake a source raised; a call that finds
    /// nothing to do changes nothing. Every source that fired is quiet
    /// again on return, whatever the answer: a source that cannot be quieted
    /// is the driver's to stop raising, so a failing device cannot spin the
    /// loop. A queued present the output stops for latches here at once. The
    /// default reports nothing, for a display that names no source.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the device's state could not be
    ///   read. [`Self::output`] then says what the display can still drive.
    fn service_events(&mut self) -> Result<Serviced, DriverError> {
        Ok(Serviced::default())
    }

    /// Whether serving a request can wait on the device's interrupt and so
    /// take a wake one of its event sources raised, which owes
    /// [`Self::service_events`] a call after every request. The default is
    /// `false`: a display whose requests never wait is serviced on its
    /// sources' wakes alone.
    fn requests_take_wakes(&self) -> bool {
        false
    }

    /// Stop waiting for the present it queued to latch: the hosting loop gave
    /// up on it at its deadline and released its presenter as though it had,
    /// so the next present must not be refused for it. What the output shows
    /// meanwhile is the driver's to settle. The default does nothing, for a
    /// display that queues none.
    fn abandon_queued(&mut self) {}

    /// The memory the device owns. The default is [`DisplayMemory::NONE`],
    /// the truth for a firmware framebuffer and any device scanning out of
    /// system RAM.
    fn memory(&self) -> DisplayMemory {
        DisplayMemory::NONE
    }

    /// How many output-sized buffers a presenter rotates through: one where
    /// a present has finished with its buffer when it returns, more where the
    /// device reads a buffer while it is shown.
    fn frame_count(&self) -> u32;

    /// Allocate a buffer of `width_px` × `height_px` in the output's format,
    /// zeroed, in memory the device reads.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Stale`] if there is no output.
    /// * [`DriverError::LengthOutOfRange`] for a zero or oversized extent.
    /// * [`DriverError::NoSpace`] past what the driver lets one presenter
    ///   hold, a budget derived from the memory its device reaches.
    /// * [`DriverError::OutOfMemory`] when no memory can be had.
    fn alloc_buffer(
        &mut self,
        width_px: u32,
        height_px: u32,
    ) -> Result<AllocatedBuffer, DriverError>;

    /// Release buffer `id`, once the device no longer reads it: a buffer on
    /// screen or queued is released when it is replaced, and one a device has
    /// not yet let go of is held until it has. The id is the presenter's no
    /// more either way.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for an id this driver did not allocate.
    fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError>;

    /// Show the output-sized buffer `id`, of which only `damage` changed since
    /// the previous present.
    ///
    /// `damage` holds one to [`MAX_DAMAGE_RECTS`] disjoint rectangles inside
    /// the output, validated before any pixel is touched; a driver whose
    /// device reads the buffer in place may ignore it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Stale`] if there is no output, or the buffer is not
    ///   its size.
    /// * [`DriverError::NotFound`] for an id this driver did not allocate.
    /// * [`DriverError::LengthOutOfRange`] for a malformed damage list.
    /// * [`DriverError::Busy`] while a queued present has not latched.
    /// * [`DriverError::OutOfMemory`] when memory the present needs cannot be
    ///   had.
    /// * [`DriverError::DeviceFault`] if the device refused it.
    fn present(&mut self, id: BufferId, damage: &[DamageRect]) -> Result<Presented, DriverError>;

    /// The lease the display's buffers were presented under has ended, and
    /// `surface` says what the seat shows until a presenter's next frame:
    /// its text console, or nothing while another presenter takes over.
    ///
    /// A display shows the console's surface, or nothing, never a buffer the
    /// outgoing presenter left: one principal's screen is never shown to the
    /// next. The default does nothing, for a display whose surface the
    /// console draws into itself, which repaints or clears it.
    fn released(&mut self, _surface: ReleaseSurface) {}

    /// Switch the display's output on or off.
    ///
    /// The surface keeps its pixels while off, and a present is still
    /// accepted, so switching back on shows the frame last presented. A
    /// queued present an output switched off was to take latches at the
    /// [`Self::service_events`] the hosting loop calls after it. Switching to
    /// the state already in force succeeds and changes nothing.
    ///
    /// The default is [`DriverError::Unsupported`]: a scan-out surface with
    /// no power control of its own cannot switch anything off, and the
    /// default never claims a control the device lacks.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the device has no power control.
    /// * [`DriverError::DeviceFault`] if the control refused the change.
    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        let _ = power;
        Err(DriverError::Unsupported)
    }

    /// The device's layer compositor, or [`None`] where it has none and every
    /// frame is composited in software. The default has none.
    fn as_accelerated(&mut self) -> Option<&mut dyn AcceleratedDisplay> {
        None
    }

    /// The device's cursor plane, or [`None`] where it has none and the
    /// pointer is composited in software. The default has none.
    fn hardware_cursor(&mut self) -> Option<&mut dyn HardwareCursor> {
        None
    }
}

/// What an [`AcceleratedDisplay`] can composite in hardware.
///
/// These bound each stack; a stack within them can still outrun what the
/// hardware composites at once. The software [`Display::present`] path is
/// always the fallback for a stack the device does not take.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AccelCaps {
    /// Most layers one [`AcceleratedDisplay::present_layers`] composites, at
    /// least one and at most [`MAX_ACCEL_LAYERS`].
    pub max_layers: u32,
    /// Largest layer width, in pixels, the hardware can source.
    pub max_width_px: u32,
    /// Largest layer height, in pixels, the hardware can source.
    pub max_height_px: u32,
    /// Whether the hardware can scale a whole layer's contribution by a
    /// constant [`AccelLayer::opacity`] as well as its per-pixel alpha.
    /// Without it every layer carries `opacity == 255`.
    pub per_layer_opacity: bool,
    /// Whether a layer's destination may differ in size from its source:
    /// the hardware scales it.
    pub hw_scaling: bool,
    /// Whether a translucent layer the hardware blends reaches the panel
    /// without banding: blended deeper than 8 bits a channel, then dithered
    /// or carried that deep to the panel. Without it a smooth gradient
    /// beneath translucency steps into plateaus. A statement of fact, shown
    /// to the user: a compositor hands the hardware translucent layers
    /// either way.
    pub band_free_blend: bool,
}

/// One layer of an [`AcceleratedDisplay::present_layers`]: the part `src` of
/// buffer `buffer`, shown at `dst_*` on the output.
///
/// Layers are composited back-to-front: index `0` is the bottom-most, each
/// blended *over* those beneath it. A buffer holds **premultiplied-alpha**
/// pixels in the output's format, the desktop compositor's pixel model.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AccelLayer {
    /// The buffer the layer shows.
    pub buffer: BufferId,
    /// The part of the buffer shown.
    pub src: DamageRect,
    /// X of the layer's top-left pixel on the output; may be negative, the
    /// off-screen part clipped.
    pub dst_x: i32,
    /// Y of the layer's top-left pixel on the output; may be negative.
    pub dst_y: i32,
    /// Width the layer is shown at, `src.width_px` unless
    /// [`AccelCaps::hw_scaling`].
    pub dst_width_px: u32,
    /// Height the layer is shown at, `src.height_px` unless
    /// [`AccelCaps::hw_scaling`].
    pub dst_height_px: u32,
    /// Constant opacity over the whole layer, `255` opaque; anything else
    /// needs [`AccelCaps::per_layer_opacity`].
    pub opacity: u8,
}

impl AccelLayer {
    /// Check the layer against what `caps` admits, its source inside a buffer
    /// of `buffer_width` × `buffer_height`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] for an empty or out-of-bounds
    ///   source, a destination larger than `caps` sources, or a destination
    ///   whose size differs from its source without hardware scaling.
    /// * [`DriverError::Unsupported`] for an opacity the hardware cannot
    ///   apply.
    pub fn validate(
        &self,
        caps: &AccelCaps,
        buffer_width: u32,
        buffer_height: u32,
    ) -> Result<(), DriverError> {
        let inside = |start: u32, extent: u32, bound: u32| {
            extent != 0 && start.checked_add(extent).is_some_and(|end| end <= bound)
        };
        let scaled =
            (self.dst_width_px, self.dst_height_px) != (self.src.width_px, self.src.height_px);
        if !inside(self.src.x, self.src.width_px, buffer_width)
            || !inside(self.src.y, self.src.height_px, buffer_height)
            || self.dst_width_px == 0
            || self.dst_height_px == 0
            || self.dst_width_px > caps.max_width_px
            || self.dst_height_px > caps.max_height_px
            || (scaled && !caps.hw_scaling)
        {
            return Err(DriverError::LengthOutOfRange);
        }
        if self.opacity != u8::MAX && !caps.per_layer_opacity {
            return Err(DriverError::Unsupported);
        }
        Ok(())
    }
}

/// A display device that composites a stack of layers in hardware, reached
/// through [`Display::as_accelerated`].
///
/// The driver composites the layers with its own fixed-function or
/// programmable compositor and scans the result out, so the presenter never
/// composites the whole screen in software. Every layer names a buffer the
/// driver allocated ([`Display::alloc_buffer`]); none is copied.
pub trait AcceleratedDisplay {
    /// What the hardware compositor can do.
    fn accel_caps(&self) -> AccelCaps;

    /// Composite `layers` back-to-front over opaque black and scan the result
    /// out.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] for more layers than
    ///   [`AccelCaps::max_layers`], or a layer [`AccelLayer::validate`]
    ///   refuses.
    /// * [`DriverError::Unsupported`] for an opacity the hardware cannot
    ///   apply.
    /// * [`DriverError::OutOfRange`] for a stack within the caps that one of
    ///   the outputs cannot composite at once, such as scaled layers
    ///   outrunning its line buffer. No output is moved off what it showed.
    /// * [`DriverError::NotFound`] for a buffer this driver did not allocate.
    /// * [`DriverError::Stale`] if there is no output.
    /// * [`DriverError::Busy`] while a queued present has not latched.
    /// * [`DriverError::OutOfMemory`] when memory the present needs cannot be
    ///   had.
    /// * [`DriverError::DeviceFault`] if the device refused it.
    fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<Presented, DriverError>;
}

/// The size of pointer image a [`HardwareCursor`] shows.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CursorCaps {
    /// Widest image, in pixels.
    pub max_width_px: u32,
    /// Tallest image, in pixels.
    pub max_height_px: u32,
}

/// A display device's cursor plane, reached through
/// [`Display::hardware_cursor`]: a small image the device draws over the
/// output itself, so moving it composites nothing.
pub trait HardwareCursor {
    /// The image size the plane shows.
    fn cursor_caps(&self) -> CursorCaps;

    /// Show buffer `id` as the pointer, its hot spot `(hot_x, hot_y)` pixels
    /// in from its top-left.
    ///
    /// The device has taken the image when this returns, so the presenter
    /// may rewrite the buffer for the next one at once.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] for a buffer larger than
    ///   [`Self::cursor_caps`] or a hot spot outside it.
    /// * [`DriverError::NotFound`] for a buffer this driver did not allocate.
    /// * [`DriverError::DeviceFault`] if the device refused it.
    fn set_cursor(&mut self, id: BufferId, hot_x: u32, hot_y: u32) -> Result<(), DriverError>;

    /// Put the pointer's hot spot at `(x, y)` on the output.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if the device refused it.
    fn move_cursor(&mut self, x: i32, y: i32) -> Result<(), DriverError>;

    /// Take the pointer off the screen.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if the device refused it.
    fn hide_cursor(&mut self) -> Result<(), DriverError>;
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    #[test]
    fn format_byte_width() {
        assert_eq!(DisplayFormat::Rgba8888.bytes_per_pixel(), 4);
        assert_eq!(DisplayFormat::Bgra8888.bytes_per_pixel(), 4);
    }

    #[test]
    fn format_discriminants_are_frozen() {
        assert_eq!(DisplayFormat::Rgba8888.as_u8(), 1);
        assert_eq!(DisplayFormat::Bgra8888.as_u8(), 2);
    }

    #[test]
    fn a_mode_spans_every_row_a_stride_long_at_any_extent() {
        let mode = DisplayMode {
            width_px: 640,
            height_px: 480,
            stride_bytes: 2560 + 64,
            format: DisplayFormat::Bgra8888,
        };
        assert_eq!(mode.len_bytes(), (2560 + 64) * 480);
        let widest = DisplayMode {
            width_px: u32::MAX / 4,
            height_px: u32::MAX,
            stride_bytes: u32::MAX,
            format: DisplayFormat::Rgba8888,
        };
        assert_eq!(
            widest.len_bytes(),
            u64::from(u32::MAX) * u64::from(u32::MAX)
        );
    }

    #[test]
    fn a_power_state_round_trips_and_nothing_else_decodes() {
        for power in [DisplayPower::On, DisplayPower::Off] {
            assert_eq!(DisplayPower::from_u8(power.as_u8()), Ok(power));
        }
        for bad in [0, 3, u8::MAX] {
            assert_eq!(DisplayPower::from_u8(bad), Err(crate::Errno::OutOfRange));
        }
    }

    #[test]
    fn a_display_with_no_power_control_says_so() {
        let mut d = MockDisplay::new();
        for power in [DisplayPower::Off, DisplayPower::On] {
            assert_eq!(d.set_power(power), Err(DriverError::Unsupported));
        }
    }

    const fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
        DamageRect {
            x,
            y,
            width_px,
            height_px,
        }
    }

    #[test]
    fn a_union_is_the_box_around_both_and_ignores_an_empty_one() {
        assert_eq!(rect(2, 3, 4, 5).union(rect(10, 1, 2, 2)), rect(2, 1, 10, 7));
        assert_eq!(rect(2, 3, 4, 5).union(rect(3, 4, 1, 1)), rect(2, 3, 4, 5));
        assert_eq!(rect(2, 3, 4, 5).union(rect(9, 9, 0, 4)), rect(2, 3, 4, 5));
        assert_eq!(rect(9, 9, 4, 0).union(rect(2, 3, 4, 5)), rect(2, 3, 4, 5));
    }

    /// An edge past the coordinate space saturates the union rather than
    /// wrapping it into a small box that would miss the pixels it names.
    #[test]
    fn a_union_past_the_coordinate_space_saturates() {
        let far = rect(u32::MAX - 1, 0, 4, 1);
        assert_eq!(
            rect(0, 0, 1, 1).union(far),
            rect(0, 0, u32::MAX, 1),
            "the far edge is clamped, not wrapped"
        );
    }

    const MODE: DisplayMode = DisplayMode {
        width_px: 4,
        height_px: 2,
        stride_bytes: 16,
        format: DisplayFormat::Rgba8888,
    };

    /// A display that allocates ids and remembers what it showed.
    struct MockDisplay {
        next: u32,
        live: alloc::vec::Vec<BufferId>,
        shown: Option<BufferId>,
    }

    impl MockDisplay {
        fn new() -> Self {
            Self {
                next: 1,
                live: alloc::vec::Vec::new(),
                shown: None,
            }
        }
    }

    impl Display for MockDisplay {
        fn output(&self) -> Option<DisplayMode> {
            Some(MODE)
        }

        fn frame_count(&self) -> u32 {
            1
        }

        fn alloc_buffer(
            &mut self,
            width_px: u32,
            _height_px: u32,
        ) -> Result<AllocatedBuffer, DriverError> {
            let id = BufferId::new(self.next).ok_or(DriverError::NoSpace)?;
            self.next += 1;
            self.live.push(id);
            Ok(AllocatedBuffer {
                id,
                region: u64::from(id.get()) << 8,
                stride_bytes: width_px * 4,
            })
        }

        fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
            let at = self
                .live
                .iter()
                .position(|live| *live == id)
                .ok_or(DriverError::NotFound)?;
            self.live.remove(at);
            Ok(())
        }

        fn present(
            &mut self,
            id: BufferId,
            damage: &[DamageRect],
        ) -> Result<Presented, DriverError> {
            DamageRect::validate_list(damage, &MODE)?;
            if !self.live.contains(&id) {
                return Err(DriverError::NotFound);
            }
            self.shown = Some(id);
            Ok(Presented::Shown)
        }
    }

    #[test]
    fn trait_is_object_safe_and_callable() {
        let mut d = MockDisplay::new();
        let dyn_ref: &mut dyn Display = &mut d;
        assert_eq!(dyn_ref.output(), Some(MODE));
        assert_eq!(dyn_ref.service_events(), Ok(Serviced::default()));
        assert_eq!(dyn_ref.memory(), DisplayMemory::NONE);
        assert!(dyn_ref.as_accelerated().is_none());
        assert!(dyn_ref.hardware_cursor().is_none());
        let buffer = dyn_ref.alloc_buffer(4, 2).expect("allocated");
        assert_eq!(buffer.stride_bytes, 16);
        let whole = [DamageRect::full(&MODE)];
        assert_eq!(dyn_ref.present(buffer.id, &whole), Ok(Presented::Shown));
        assert_eq!(
            dyn_ref.present(buffer.id, &[]),
            Err(DriverError::LengthOutOfRange),
            "a present names its damage"
        );
        assert_eq!(dyn_ref.free_buffer(buffer.id), Ok(()));
        assert_eq!(
            dyn_ref.present(buffer.id, &whole),
            Err(DriverError::NotFound)
        );
        assert_eq!(d.shown, Some(buffer.id));
    }

    #[test]
    fn zero_names_no_buffer() {
        assert_eq!(BufferId::new(0), None);
        assert_eq!(BufferId::new(7).map(BufferId::get), Some(7));
    }

    /// 1080i60: interlaced, both syncs high — every flag set at once.
    const INTERLACED_1080: Timing = Timing {
        clock_khz: 74_250,
        horizontal: Axis {
            active: 1920,
            front: 88,
            sync: 44,
            back: 148,
        },
        vertical: Axis {
            active: 1080,
            front: 4,
            sync: 10,
            back: 31,
        },
        hsync: Polarity::Positive,
        vsync: Polarity::Positive,
        scan: Scan::Interlaced,
        repetition: 1,
    };

    #[test]
    fn a_timing_shows_its_active_pixels_and_lines() {
        assert_eq!(INTERLACED_1080.active_size(), (1920, 1080));
    }

    #[test]
    fn a_timing_round_trips_through_its_wire_form() {
        let doubled = Timing {
            hsync: Polarity::Negative,
            vsync: Polarity::Negative,
            scan: Scan::Progressive,
            repetition: 2,
            ..INTERLACED_1080
        };
        for timing in [INTERLACED_1080, doubled] {
            assert_eq!(Timing::from_bytes(&timing.to_le_bytes()), Ok(timing));
        }
    }

    #[test]
    fn a_timing_nothing_could_clock_out_is_refused() {
        let good = INTERLACED_1080.to_le_bytes();
        assert_eq!(
            Timing::from_bytes(&good[..Timing::WIRE_LEN - 1]),
            Err(Errno::BufferTooSmall)
        );
        let mut flag = good;
        flag[20] |= 1 << 3;
        assert_eq!(Timing::from_bytes(&flag), Err(Errno::BadMagic));
        let mut reserved = good;
        reserved[23] = 1;
        assert_eq!(Timing::from_bytes(&reserved), Err(Errno::BadMagic));
        for (at, value) in [(0, 0u8), (4, 0), (8, 0), (12, 0), (16, 0)] {
            let mut zeroed = good;
            let width = if at == 0 { 4 } else { 2 };
            zeroed[at..at + width].fill(value);
            assert_eq!(
                Timing::from_bytes(&zeroed),
                Err(Errno::LengthOutOfRange),
                "{at}"
            );
        }
        for repetition in [0, MAX_REPETITION + 1] {
            let mut repeated = good;
            repeated[21] = repetition;
            assert_eq!(Timing::from_bytes(&repeated), Err(Errno::LengthOutOfRange));
        }
    }

    /// A display with nothing connected, counting what reaches its present.
    const CAPS: AccelCaps = AccelCaps {
        max_layers: 2,
        max_width_px: 64,
        max_height_px: 64,
        per_layer_opacity: false,
        hw_scaling: false,
        band_free_blend: false,
    };

    fn layer(src: DamageRect) -> AccelLayer {
        AccelLayer {
            buffer: BufferId::new(1).expect("an id"),
            src,
            dst_x: -3,
            dst_y: 2,
            dst_width_px: src.width_px,
            dst_height_px: src.height_px,
            opacity: 255,
        }
    }

    #[test]
    fn a_layer_shows_only_a_part_of_its_buffer_the_hardware_can_take() {
        assert_eq!(layer(rect(0, 0, 16, 8)).validate(&CAPS, 16, 8), Ok(()));
        assert_eq!(layer(rect(4, 2, 12, 6)).validate(&CAPS, 16, 8), Ok(()));
        for src in [
            rect(0, 0, 0, 8),
            rect(0, 0, 17, 8),
            rect(8, 0, 9, 8),
            rect(0, 7, 16, 2),
            rect(u32::MAX, 0, 2, 1),
        ] {
            assert_eq!(
                layer(src).validate(&CAPS, 16, 8),
                Err(DriverError::LengthOutOfRange),
                "{src:?}"
            );
        }
        let wide = layer(rect(0, 0, 65, 1));
        assert_eq!(
            wide.validate(&CAPS, 128, 1),
            Err(DriverError::LengthOutOfRange),
            "wider than the hardware sources"
        );
    }

    #[test]
    fn a_layer_is_scaled_or_faded_only_where_the_hardware_can() {
        let scaled = AccelLayer {
            dst_width_px: 32,
            ..layer(rect(0, 0, 16, 8))
        };
        assert_eq!(
            scaled.validate(&CAPS, 16, 8),
            Err(DriverError::LengthOutOfRange)
        );
        let scaling = AccelCaps {
            hw_scaling: true,
            ..CAPS
        };
        assert_eq!(scaled.validate(&scaling, 16, 8), Ok(()));
        let faded = AccelLayer {
            opacity: 128,
            ..layer(rect(0, 0, 16, 8))
        };
        assert_eq!(faded.validate(&CAPS, 16, 8), Err(DriverError::Unsupported));
        let fading = AccelCaps {
            per_layer_opacity: true,
            ..CAPS
        };
        assert_eq!(faded.validate(&fading, 16, 8), Ok(()));
    }

    /// A display whose compositor shows every layer it admits.
    struct MockAccel {
        display: MockDisplay,
        layers_seen: usize,
    }

    impl Display for MockAccel {
        fn output(&self) -> Option<DisplayMode> {
            self.display.output()
        }

        fn frame_count(&self) -> u32 {
            self.display.frame_count()
        }

        fn alloc_buffer(
            &mut self,
            width_px: u32,
            height_px: u32,
        ) -> Result<AllocatedBuffer, DriverError> {
            self.display.alloc_buffer(width_px, height_px)
        }

        fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
            self.display.free_buffer(id)
        }

        fn present(
            &mut self,
            id: BufferId,
            damage: &[DamageRect],
        ) -> Result<Presented, DriverError> {
            self.display.present(id, damage)
        }

        fn as_accelerated(&mut self) -> Option<&mut dyn AcceleratedDisplay> {
            Some(self)
        }
    }

    impl AcceleratedDisplay for MockAccel {
        fn accel_caps(&self) -> AccelCaps {
            CAPS
        }

        fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<Presented, DriverError> {
            if layers.len() > CAPS.max_layers as usize {
                return Err(DriverError::LengthOutOfRange);
            }
            for layer in layers {
                layer.validate(&CAPS, MODE.width_px, MODE.height_px)?;
            }
            self.layers_seen = layers.len();
            Ok(Presented::Shown)
        }
    }

    #[test]
    fn the_layer_compositor_is_reached_through_the_display() {
        let mut d = MockAccel {
            display: MockDisplay::new(),
            layers_seen: 0,
        };
        let display: &mut dyn Display = &mut d;
        let accel = display.as_accelerated().expect("a compositor");
        assert_eq!(accel.accel_caps(), CAPS);
        let one = layer(rect(0, 0, 4, 2));
        assert_eq!(accel.present_layers(&[one]), Ok(Presented::Shown));
        assert_eq!(
            accel.present_layers(&[one, one, one]),
            Err(DriverError::LengthOutOfRange),
            "past what the hardware composites"
        );
        assert_eq!(d.layers_seen, 1);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::{DamageRect, HostedConsole};
    use crate::Errno;

    #[test]
    fn a_rectangle_round_trips_and_a_short_buffer_does_not_decode() {
        let rect = DamageRect {
            x: 1,
            y: 2,
            width_px: 3,
            height_px: u32::MAX,
        };
        assert_eq!(DamageRect::from_bytes(&rect.to_le_bytes()), Ok(rect));
        assert_eq!(
            DamageRect::from_bytes(&rect.to_le_bytes()[..15]),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn a_hosted_console_round_trips_and_fails_closed() {
        let hosted = HostedConsole {
            device_addr: 0x1_2345_6000,
            stride_bytes: 5120,
        };
        let bytes = hosted.to_le_bytes();
        assert_eq!(HostedConsole::from_bytes(&bytes), Ok(hosted));
        assert_eq!(
            HostedConsole::from_bytes(&bytes[..15]),
            Err(Errno::BufferTooSmall)
        );
        let mut dirty = bytes;
        dirty[15] = 1;
        assert_eq!(HostedConsole::from_bytes(&dirty), Err(Errno::BadMagic));
        let mut flat = bytes;
        flat[8..12].fill(0);
        assert_eq!(
            HostedConsole::from_bytes(&flat),
            Err(Errno::LengthOutOfRange)
        );
    }
}

#[cfg(test)]
mod damage_list_tests {
    use super::{DamageList, DamageRect, DamageRects, MAX_DAMAGE_RECTS};

    fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
        DamageRect {
            x,
            y,
            width_px,
            height_px,
        }
    }

    /// What fits is kept as it is, and empty rectangles name nothing.
    #[test]
    fn a_fitting_list_keeps_its_rectangles_and_drops_empty_ones() {
        let list = DamageList::fitted([rect(0, 0, 4, 4), rect(0, 0, 0, 9), rect(10, 0, 2, 2)])
            .expect("a list");
        assert_eq!(list.rects(), [rect(0, 0, 4, 4), rect(10, 0, 2, 2)]);
        assert!(DamageList::fitted([rect(1, 1, 0, 0)]).is_none());
        assert!(DamageList::fitted([]).is_none());
    }

    /// A list names no pixel twice: one built from overlapping rectangles is
    /// refused, and a fitted one merges each overlapping pair into its union —
    /// so a receiver converts every pixel a present names exactly once.
    #[test]
    fn a_list_names_no_pixel_twice() {
        assert_eq!(
            DamageList::new(&[rect(0, 0, 10, 10), rect(9, 9, 2, 2)]).err(),
            Some(crate::Errno::LengthOutOfRange)
        );
        assert_eq!(
            DamageList::new(&[rect(0, 0, 10, 10), rect(0, 0, 10, 10)]).err(),
            Some(crate::Errno::LengthOutOfRange),
            "the same rectangle twice"
        );
        assert!(DamageList::new(&[rect(0, 0, 10, 10), rect(10, 0, 5, 10)]).is_ok());
        let list = DamageList::fitted([
            rect(0, 0, 10, 10),
            rect(5, 5, 10, 10),
            rect(14, 14, 4, 4),
            rect(40, 0, 2, 2),
        ])
        .expect("a list");
        assert_eq!(list.rects(), [rect(0, 0, 18, 18), rect(40, 0, 2, 2)]);
    }

    /// Past the bound the closest pair merges first, so two far-apart edits
    /// stay two rectangles rather than becoming the box between them.
    #[test]
    fn an_overlong_list_merges_the_pair_whose_union_grows_least() {
        let rects: [DamageRect; MAX_DAMAGE_RECTS + 1] = core::array::from_fn(|i| {
            if i == MAX_DAMAGE_RECTS {
                rect(11, 0, 10, 10)
            } else {
                rect(u32::try_from(i).expect("small") * 100, 0, 10, 10)
            }
        });
        let list = DamageList::fitted(rects).expect("a list");
        assert_eq!(list.len(), MAX_DAMAGE_RECTS);
        assert!(list.rects().contains(&rect(0, 0, 21, 10)), "{list:?}");
        assert!(list.rects().contains(&rect(700, 0, 10, 10)));
        let covered = |x: u32| {
            list.rects()
                .iter()
                .any(|r| r.x <= x && x < r.x + r.width_px)
        };
        for original in &rects {
            assert!(covered(original.x), "{original:?} is still covered");
        }
        assert_eq!(list.bounds(), rect(0, 0, 710, 10));
    }

    /// A list of a larger capacity holds as many as it says, and merges only
    /// past them.
    #[test]
    fn a_larger_list_holds_its_capacity_and_merges_past_it() {
        const WIDE: usize = 128;
        let apart = |i: usize| rect(u32::try_from(i).expect("small") * 20, 0, 10, 10);
        let exact = DamageRects::<WIDE>::fitted((0..WIDE).map(apart)).expect("a list");
        assert_eq!(exact.len(), WIDE);
        assert!(
            (0..WIDE).all(|i| exact.rects()[i] == apart(i)),
            "kept as given"
        );
        let over = DamageRects::<WIDE>::fitted((0..=WIDE).map(apart)).expect("a list");
        assert_eq!(over.len(), WIDE, "one pair merged");
        assert_eq!(over.bounds(), rect(0, 0, 20 * 128 + 10, 10));
    }
}
