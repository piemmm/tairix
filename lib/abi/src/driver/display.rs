//! Display driver class (`drivers/display/*`).
//!
//! A display driver presents a single linear pixel surface. The
//! trait is intentionally minimal: querying the active mode and
//! presenting a fully-rendered frame are the only operations the
//! Stage 4 first drivers (`vesa`, `framebuffer`, `gpu_virtio`) need.
//! Compositing, damage tracking, and GPU acceleration live above
//! this trait in `userland/gui/wm`.

use super::DriverError;

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
/// [`Display::present_rects`] in process, and in the fixed-width
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

/// The damage a present names: one to [`MAX_DAMAGE_RECTS`] pairwise-disjoint
/// rectangles, held inline so a request carrying them stays a plain copyable
/// value. Disjoint, so a present never makes its receiver convert a pixel
/// twice.
///
/// Constructing one is the only way to name a present's damage, so the count
/// bound and the "no empty or overlapping rectangle" rules hold before a byte
/// is encoded as well as after one is decoded — a decoded list is exactly as trustworthy as
/// a locally built one. Rectangles beyond the live count are zero, which is
/// what makes a frame's reserved tail checkable.
///
/// Equality compares the rectangles named, not the dead slots behind them.
#[derive(Copy, Clone, Debug)]
pub struct DamageList {
    rects: [DamageRect; MAX_DAMAGE_RECTS],
    count: u8,
}

/// The zero rectangle filling a [`DamageList`]'s unused slots. Not a valid
/// damage rectangle — it is never inside the live prefix.
pub(crate) const NO_RECT: DamageRect = DamageRect {
    x: 0,
    y: 0,
    width_px: 0,
    height_px: 0,
};

impl DamageList {
    /// The list naming `rects`.
    ///
    /// # Errors
    ///
    /// [`crate::Errno::LengthOutOfRange`] if `rects` is empty, holds more than
    /// [`MAX_DAMAGE_RECTS`] entries, holds an empty rectangle, or holds two
    /// that overlap.
    pub fn new(rects: &[DamageRect]) -> Result<Self, crate::Errno> {
        let count = u8::try_from(rects.len()).map_err(|_| crate::Errno::LengthOutOfRange)?;
        if rects.is_empty() || rects.len() > MAX_DAMAGE_RECTS {
            return Err(crate::Errno::LengthOutOfRange);
        }
        let mut slots = [NO_RECT; MAX_DAMAGE_RECTS];
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
    /// dropped, while more than [`MAX_DAMAGE_RECTS`] remain the pair whose
    /// union adds the fewest pixels is merged into that union, and then any two
    /// that overlap are merged until none do. `None` when nothing is left to
    /// name.
    ///
    /// Merging over-covers by the least it can, where presenting the bounding
    /// box would turn two far-apart edits — a band's moving edges — into the
    /// whole area between them.
    #[must_use]
    pub fn fitted(rects: impl IntoIterator<Item = DamageRect>) -> Option<Self> {
        let mut slots = [NO_RECT; MAX_DAMAGE_RECTS + 1];
        let mut count = 0;
        for rect in rects {
            if rect.width_px == 0 || rect.height_px == 0 {
                continue;
            }
            if count == slots.len() {
                merge_cheapest(&mut slots, &mut count);
            }
            slots[count] = rect;
            count += 1;
        }
        if count > MAX_DAMAGE_RECTS {
            merge_cheapest(&mut slots, &mut count);
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

impl PartialEq for DamageList {
    fn eq(&self, other: &Self) -> bool {
        self.rects() == other.rects()
    }
}

impl Eq for DamageList {}

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

/// Merge the two rectangles of `slots[..count]` whose union adds the fewest
/// pixels, leaving one fewer.
fn merge_cheapest(slots: &mut [DamageRect], count: &mut usize) {
    let area = |rect: DamageRect| u64::from(rect.width_px) * u64::from(rect.height_px);
    let mut cheapest = None;
    for a in 0..*count {
        for b in a + 1..*count {
            let union = slots[a].union(slots[b]);
            let growth = area(union).saturating_sub(area(slots[a]).saturating_add(area(slots[b])));
            if cheapest.is_none_or(|(_, _, least)| growth < least) {
                cheapest = Some((a, b, growth));
            }
        }
    }
    if let Some((a, b, _)) = cheapest {
        slots[a] = slots[a].union(slots[b]);
        *count -= 1;
        slots[b] = slots[*count];
    }
}

impl DamageRect {
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

/// Mode-information record returned by [`Display::mode_info`].
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

/// What a display device reports about *itself*, beyond the mode it is
/// scanning out: the memory it owns and what its hardware compositor can do.
///
/// The device half of the graphics reading a monitor draws. It is deliberately
/// separate from [`AccelCaps`]: a device with no hardware compositor still has
/// (or has not) memory of its own, and both facts are the driver's to state —
/// nothing above it can measure them.
///
/// An in-process driver-trait value, not a wire record: the monitor's
/// on-the-wire form is [`DisplayStats`](crate::display_ipc::DisplayStats).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DisplayDeviceReport {
    /// Bytes of the device's own memory currently holding scan-out or layer
    /// sources. Necessarily `0` when [`Self::mem_total_bytes`] is.
    pub mem_resident_bytes: u64,
    /// Bytes of memory the device owns. `0` means the device has **no memory
    /// of its own** — a firmware framebuffer scanning out of system RAM — not
    /// that none is free.
    pub mem_total_bytes: u64,
    /// What the hardware compositor can do, or [`None`] where the device has
    /// none and every frame is composited in software.
    pub accel: Option<AccelCaps>,
}

impl DisplayDeviceReport {
    /// A device with no memory of its own and no hardware compositor: what a
    /// firmware framebuffer honestly is.
    pub const SOFTWARE: Self = Self {
        mem_resident_bytes: 0,
        mem_total_bytes: 0,
        accel: None,
    };
}

/// Trait every display driver implements.
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
    /// Report the active mode.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if hardware enumeration fails.
    /// * [`DriverError::Unsupported`] if the driver was loaded into a
    ///   headless host that cannot expose a surface.
    ///
    /// # Capabilities
    ///
    /// Caller must present the driver's [`DriverHandle`].
    ///
    /// [`DriverHandle`]: crate::driver::DriverHandle
    fn mode_info(&self) -> Result<DisplayMode, DriverError>;

    /// Report what this device owns and what its compositor can do.
    ///
    /// The default is [`DisplayDeviceReport::SOFTWARE`] — no memory of its
    /// own, no hardware compositor — which is the truth for a firmware
    /// framebuffer and for any driver that scans out of system RAM. A device
    /// with dedicated memory or an [`AcceleratedDisplay`] engine overrides it;
    /// the default never overstates what the silicon can do.
    fn device_report(&self) -> DisplayDeviceReport {
        DisplayDeviceReport::SOFTWARE
    }

    /// Present a fully-rendered frame.
    ///
    /// `frame` must contain at least
    /// `mode_info()?.stride_bytes * mode_info()?.height_px` bytes laid
    /// out in the format reported by [`Self::mode_info`].
    ///
    /// # Errors
    ///
    /// * [`DriverError::BufferTooSmall`] if `frame` is shorter than
    ///   the active mode requires.
    /// * [`DriverError::DeviceFault`] if the underlying hardware
    ///   rejected the present.
    /// * [`DriverError::Busy`] if a previous present is still in
    ///   flight.
    ///
    /// # Capabilities
    ///
    /// Caller must present the driver's [`DriverHandle`].
    ///
    /// [`DriverHandle`]: crate::driver::DriverHandle
    fn present(&mut self, frame: &[u8]) -> Result<(), DriverError>;

    /// Present a fully-rendered frame of which only `damage` changed.
    ///
    /// `frame` carries the **whole** surface exactly as for
    /// [`Self::present`]; `damage` names the rectangles that differ from
    /// the previously presented frame, so a driver whose scan-out path is
    /// a copy can blit only the touched pixels. It holds one to
    /// [`MAX_DAMAGE_RECTS`] rectangles and the caller keeps them disjoint,
    /// so no pixel is blitted twice.
    ///
    /// **One call carries a whole frame's damage.** A frame that changed two
    /// far-apart places — a menu and the title band its owner repainted —
    /// costs one dispatch and two rectangle-sized blits, never a blit of the
    /// box spanning them, and never one dispatch per rectangle.
    ///
    /// The default forwards to the full-frame [`Self::present`] — a correct
    /// (if unoptimised) implementation for every existing driver — after
    /// validating the whole list against the active mode, so a malformed
    /// rectangle is refused identically on both paths and nothing is
    /// presented partially.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if the list is empty, longer than
    ///   [`MAX_DAMAGE_RECTS`], or holds a rectangle that is empty or falls
    ///   outside the active mode's surface.
    /// * Everything [`Self::present`] can return.
    ///
    /// # Capabilities
    ///
    /// Caller must present the driver's
    /// [`DriverHandle`](crate::driver::DriverHandle), exactly as for
    /// [`Self::present`].
    fn present_rects(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        let mode = self.mode_info()?;
        DamageRect::validate_list(damage, &mode)?;
        self.present(frame)
    }

    /// Switch the display's output on or off.
    ///
    /// The surface keeps its pixels while off, and a present is still
    /// accepted, so switching back on shows the frame last presented.
    /// Switching to the state already in force succeeds and changes nothing.
    ///
    /// The default is [`DriverError::Unsupported`]: a scan-out surface with
    /// no power control of its own cannot switch anything off, and the
    /// default never claims a control the device lacks.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the device has no power control.
    /// * [`DriverError::DeviceFault`] if the control refused the change.
    ///
    /// # Capabilities
    ///
    /// Caller must present the driver's
    /// [`DriverHandle`](crate::driver::DriverHandle), exactly as for
    /// [`Self::present`].
    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        let _ = power;
        Err(DriverError::Unsupported)
    }
}

/// What an [`AcceleratedDisplay`] back-end can composite in hardware.
///
/// Reported by [`AcceleratedDisplay::accel_caps`] so the compositor can
/// decide, per frame, whether the hardware path can serve the current
/// window stack or whether it must fall back to the software
/// [`Display::present`] path (the software path is
/// always the fallback).
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AccelCaps {
    /// Maximum number of layers the hardware can composite in a single
    /// [`AcceleratedDisplay::present_layers`] call. A back-end always
    /// supports at least one layer.
    pub max_layers: u32,
    /// Largest layer width, in pixels, the hardware can source.
    pub max_width_px: u32,
    /// Largest layer height, in pixels, the hardware can source.
    pub max_height_px: u32,
    /// Whether the hardware can scale a whole layer's contribution by a
    /// constant [`AccelLayer::opacity`] in addition to its per-pixel
    /// alpha. A back-end that reports `false` requires every layer to
    /// carry `opacity == 255`.
    pub per_layer_opacity: bool,
}

/// One source layer in an [`AcceleratedDisplay::present_layers`] call.
///
/// Layers are composited back-to-front: index `0` is the bottom-most
/// layer and the last layer is the top-most, each placed at its
/// destination origin and blended *over* the layers beneath it.
///
/// `pixels` is **premultiplied-alpha** pixel data in the display's
/// active [`DisplayMode::format`], stored row-major with `stride_bytes`
/// between consecutive scanlines. Premultiplied alpha matches the
/// desktop compositor's pixel model, so a layer can be
/// handed to the hardware without an extra straight-alpha conversion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AccelLayer<'a> {
    /// Premultiplied-alpha source pixels in the display's active format.
    pub pixels: &'a [u8],
    /// Layer width in pixels.
    pub width_px: u32,
    /// Layer height in pixels.
    pub height_px: u32,
    /// Bytes between consecutive scanlines of `pixels`. Must hold at
    /// least `width_px * format.bytes_per_pixel()` bytes.
    pub stride_bytes: u32,
    /// X position of the layer's top-left pixel in the scan-out surface.
    /// May be negative; the off-screen portion is clipped.
    pub dst_x: i32,
    /// Y position of the layer's top-left pixel in the scan-out surface.
    /// May be negative; the off-screen portion is clipped.
    pub dst_y: i32,
    /// Constant opacity applied to the whole layer, `0` fully
    /// transparent, `255` fully opaque (per-pixel alpha only). Requires
    /// [`AccelCaps::per_layer_opacity`] unless it is `255`.
    pub opacity: u8,
}

impl AccelLayer<'_> {
    /// Number of source bytes the layer's geometry requires
    /// (`stride_bytes * height_px`), or [`DriverError::LengthOutOfRange`]
    /// if the product overflows the host's address width.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] if the byte count overflows.
    pub fn required_len(&self, format: DisplayFormat) -> Result<usize, DriverError> {
        let min_stride = self
            .width_px
            .checked_mul(format.bytes_per_pixel())
            .ok_or(DriverError::LengthOutOfRange)?;
        if self.width_px == 0 || self.height_px == 0 || self.stride_bytes < min_stride {
            return Err(DriverError::LengthOutOfRange);
        }
        let bytes = u64::from(self.stride_bytes)
            .checked_mul(u64::from(self.height_px))
            .ok_or(DriverError::LengthOutOfRange)?;
        usize::try_from(bytes).map_err(|_| DriverError::LengthOutOfRange)
    }
}

/// A display back-end that can composite a stack of layers in hardware.
///
/// This is the optional GPU-accelerated path. A driver
/// that exposes it composites the supplied [`AccelLayer`] stack with its
/// own fixed-function or programmable compositor (e.g. the Raspberry Pi
/// `VideoCore` HVS plane engine) and scans the result out itself, so the
/// host never composites the whole screen in software. Every
/// `AcceleratedDisplay` is also a [`Display`]: the software
/// full-frame [`Display::present`] path remains available as the
/// mandatory fallback when the layer stack exceeds [`AccelCaps`].
///
/// # Capabilities
///
/// Every method is gated by ownership of the driver's
/// [`DriverHandle`](crate::driver::DriverHandle), exactly as for
/// [`Display`].
pub trait AcceleratedDisplay: Display {
    /// Report what the hardware compositor can do this frame.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if querying the hardware fails.
    /// * [`DriverError::Unsupported`] if acceleration is unavailable on
    ///   this device even though the trait is implemented.
    fn accel_caps(&self) -> Result<AccelCaps, DriverError>;

    /// Composite `layers` back-to-front and scan the result out.
    ///
    /// Index `0` is the bottom layer; the last entry is on top. The
    /// scan-out surface is cleared to opaque black before the first
    /// layer is blended, so callers need not supply a full-screen base
    /// layer.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if more layers are supplied
    ///   than [`AccelCaps::max_layers`], or a layer's geometry exceeds
    ///   [`AccelCaps::max_width_px`] / [`AccelCaps::max_height_px`].
    /// * [`DriverError::BufferTooSmall`] if a layer's `pixels` is
    ///   shorter than its geometry requires.
    /// * [`DriverError::Unsupported`] if a layer requests a per-layer
    ///   [`AccelLayer::opacity`] the hardware cannot apply.
    /// * [`DriverError::DeviceFault`] if the hardware rejected the
    ///   present.
    /// * [`DriverError::Busy`] if a previous present is still in flight.
    fn present_layers(&mut self, layers: &[AccelLayer<'_>]) -> Result<(), DriverError>;
}

#[cfg(test)]
mod tests {
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
        let mut d = MockDisplay {
            mode: DisplayMode {
                width_px: 4,
                height_px: 2,
                stride_bytes: 16,
                format: DisplayFormat::Rgba8888,
            },
            frame_len_seen: core::cell::Cell::new(0),
        };
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

    struct MockDisplay {
        mode: DisplayMode,
        frame_len_seen: core::cell::Cell<usize>,
    }

    impl Display for MockDisplay {
        fn mode_info(&self) -> Result<DisplayMode, DriverError> {
            Ok(self.mode)
        }

        fn present(&mut self, frame: &[u8]) -> Result<(), DriverError> {
            let required = (self.mode.stride_bytes as usize) * (self.mode.height_px as usize);
            if frame.len() < required {
                return Err(DriverError::BufferTooSmall);
            }
            self.frame_len_seen.set(frame.len());
            Ok(())
        }
    }

    #[test]
    fn trait_is_object_safe_and_callable() {
        let mut d = MockDisplay {
            mode: DisplayMode {
                width_px: 4,
                height_px: 2,
                stride_bytes: 16,
                format: DisplayFormat::Rgba8888,
            },
            frame_len_seen: core::cell::Cell::new(0),
        };
        let dyn_ref: &mut dyn Display = &mut d;
        let Ok(info) = dyn_ref.mode_info() else {
            unreachable!("mock always succeeds")
        };
        assert_eq!(info.width_px, 4);
        assert_eq!(dyn_ref.present(&[0u8; 8]), Err(DriverError::BufferTooSmall));
        assert!(dyn_ref.present(&[0u8; 32]).is_ok());
        assert_eq!(d.frame_len_seen.get(), 32);
    }

    fn layer(width: u32, height: u32, stride: u32) -> AccelLayer<'static> {
        AccelLayer {
            pixels: &[],
            width_px: width,
            height_px: height,
            stride_bytes: stride,
            dst_x: 0,
            dst_y: 0,
            opacity: 255,
        }
    }

    #[test]
    fn layer_required_len_uses_stride_times_height() {
        // 4-px BGRA scanline needs 16 bytes; stride 20 × 3 rows = 60.
        let l = layer(4, 3, 20);
        assert_eq!(l.required_len(DisplayFormat::Bgra8888), Ok(60));
    }

    #[test]
    fn layer_rejects_degenerate_or_narrow_geometry() {
        assert_eq!(
            layer(0, 3, 16).required_len(DisplayFormat::Rgba8888),
            Err(DriverError::LengthOutOfRange)
        );
        assert_eq!(
            layer(4, 0, 16).required_len(DisplayFormat::Rgba8888),
            Err(DriverError::LengthOutOfRange)
        );
        // Stride too small to hold one 4-px scanline (needs 16).
        assert_eq!(
            layer(4, 3, 12).required_len(DisplayFormat::Rgba8888),
            Err(DriverError::LengthOutOfRange)
        );
    }

    /// A back-end that records the layer stack it was asked to present
    /// and validates each layer against its reported caps.
    struct MockAccel {
        mode: DisplayMode,
        caps: AccelCaps,
        layers_seen: core::cell::Cell<usize>,
    }

    impl Display for MockAccel {
        fn mode_info(&self) -> Result<DisplayMode, DriverError> {
            Ok(self.mode)
        }
        fn present(&mut self, _frame: &[u8]) -> Result<(), DriverError> {
            Ok(())
        }
    }

    impl AcceleratedDisplay for MockAccel {
        fn accel_caps(&self) -> Result<AccelCaps, DriverError> {
            Ok(self.caps)
        }
        fn present_layers(&mut self, layers: &[AccelLayer<'_>]) -> Result<(), DriverError> {
            if layers.len() > self.caps.max_layers as usize {
                return Err(DriverError::LengthOutOfRange);
            }
            for l in layers {
                let need = l.required_len(self.mode.format)?;
                if l.pixels.len() < need {
                    return Err(DriverError::BufferTooSmall);
                }
                if l.opacity != 255 && !self.caps.per_layer_opacity {
                    return Err(DriverError::Unsupported);
                }
            }
            self.layers_seen.set(layers.len());
            Ok(())
        }
    }

    fn mock_accel() -> MockAccel {
        MockAccel {
            mode: DisplayMode {
                width_px: 4,
                height_px: 2,
                stride_bytes: 16,
                format: DisplayFormat::Bgra8888,
            },
            caps: AccelCaps {
                max_layers: 2,
                max_width_px: 64,
                max_height_px: 64,
                per_layer_opacity: false,
            },
            layers_seen: core::cell::Cell::new(0),
        }
    }

    #[test]
    fn accelerated_present_accepts_well_formed_layers() {
        let mut d = mock_accel();
        let pixels = [0u8; 16]; // 4×1 BGRA, stride 16.
        let dyn_ref: &mut dyn AcceleratedDisplay = &mut d;
        assert_eq!(
            dyn_ref.accel_caps().map(|c| c.max_layers),
            Ok(2),
            "caps round-trip"
        );
        let layers = [AccelLayer {
            pixels: &pixels,
            width_px: 4,
            height_px: 1,
            stride_bytes: 16,
            dst_x: 0,
            dst_y: 0,
            opacity: 255,
        }];
        assert!(dyn_ref.present_layers(&layers).is_ok());
        assert_eq!(d.layers_seen.get(), 1);
    }

    #[test]
    fn accelerated_present_fails_closed_on_over_budget_and_short_and_opacity() {
        let mut d = mock_accel();
        let pixels = [0u8; 16];
        let one = AccelLayer {
            pixels: &pixels,
            width_px: 4,
            height_px: 1,
            stride_bytes: 16,
            dst_x: 0,
            dst_y: 0,
            opacity: 255,
        };
        // Three layers exceeds max_layers == 2.
        assert_eq!(
            d.present_layers(&[one, one, one]),
            Err(DriverError::LengthOutOfRange)
        );
        // A layer whose pixels are shorter than its geometry.
        let short = AccelLayer {
            pixels: &pixels[..8],
            ..one
        };
        assert_eq!(d.present_layers(&[short]), Err(DriverError::BufferTooSmall));
        // Per-layer opacity the hardware does not support.
        let faded = AccelLayer {
            opacity: 128,
            ..one
        };
        assert_eq!(d.present_layers(&[faded]), Err(DriverError::Unsupported));
    }
}

#[cfg(test)]
mod damage_list_tests {
    use super::{DamageList, DamageRect, MAX_DAMAGE_RECTS};

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
}
