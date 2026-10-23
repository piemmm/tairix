//! Turning composited pixels into the bytes a scan-out frame holds.
//!
//! Every program that presents through [`crate::RemoteDisplay`] faces the
//! same three questions: how big is the frame for this mode, which byte
//! order does this format want, and is this damage rectangle a sub-region
//! or the whole screen. The answers are pixel-exact — a wrong channel order
//! renders the display in false colour, and a damage rectangle that lies
//! shows stale pixels — so there is one definition of each here, shared by
//! the compositing window manager and the graphical login screen rather
//! than copied into both.
//!
//! What is *not* here is the composition itself. A compositor blending many
//! windows through a back buffer and a program blitting one surface are
//! genuinely different loops; both encode their result through
//! [`ChannelOrder::encode`] — a whole run of it through
//! [`ChannelOrder::encode_run`], which is that same encoder in bulk — and
//! neither pretends to be the other.
//!
//! The two per-pixel codecs are `#[inline]`: each is reached once per pixel
//! from another module or crate — the window-frame codec's rows, the
//! compositor's reveal encode — and the desktop's crates are built with many
//! codegen units and no link-time optimisation in the profile the debug image
//! uses, so without it a four-byte shuffle costs an out-of-line call.

use tairix_abi::driver::display::{DamageRect, DisplayFormat, DisplayMode, MAX_DAMAGE_RECTS};
use tairix_geometry::{Rect, Region};
use tairix_raster::Pixel;

/// The byte order a display format wants each pixel written in.
///
/// A format whose byte order this crate does not know is refused at
/// configure time rather than guessed, because guessing renders the whole
/// screen in false colour.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChannelOrder {
    /// Red, green, blue, alpha.
    Rgba,
    /// Blue, green, red, alpha.
    Bgra,
}

impl ChannelOrder {
    /// The order `format` wants, or `None` for a format with no software
    /// encoding here.
    ///
    /// The format set is open, so a caller resolves this once at configure
    /// time and refuses the mode on `None` rather than reaching a pixel it
    /// does not know how to write.
    #[must_use]
    pub const fn for_format(format: DisplayFormat) -> Option<Self> {
        match format {
            DisplayFormat::Rgba8888 => Some(Self::Rgba),
            DisplayFormat::Bgra8888 => Some(Self::Bgra),
            _ => None,
        }
    }

    /// Encode one premultiplied pixel into its four bytes: the form a
    /// scan-out frame and a window frame both hold.
    #[must_use]
    #[inline]
    pub const fn encode(self, pixel: Pixel) -> [u8; 4] {
        match self {
            Self::Rgba => [pixel.r, pixel.g, pixel.b, pixel.a],
            Self::Bgra => [pixel.b, pixel.g, pixel.r, pixel.a],
        }
    }

    /// Decode four premultiplied bytes a client wrote, the inverse of
    /// [`encode`](Self::encode), with every colour channel clamped to the
    /// pixel's alpha.
    ///
    /// The bytes are untrusted, and a channel above its alpha breaks the
    /// invariant every blend relies on, so the decoded pixel is always
    /// premultiplied whatever the client wrote. An encoded pixel decodes to
    /// itself.
    #[must_use]
    #[inline]
    pub fn decode(self, bytes: [u8; 4]) -> Pixel {
        let [b0, b1, b2, a] = bytes;
        let (r, g, b) = match self {
            Self::Rgba => (b0, b1, b2),
            Self::Bgra => (b2, b1, b0),
        };
        Pixel {
            r: r.min(a),
            g: g.min(a),
            b: b.min(a),
            a,
        }
    }

    /// Decode a run of four-byte pixels a client wrote into `out`, every one
    /// opaque whatever alpha it carries: the bulk form of
    /// [`decode`](Self::decode) for a picture that may not show what it
    /// covers. With alpha full the clamp leaves every channel as it is, so
    /// this is a byte-order copy.
    ///
    /// Writes `min(bytes.len() / 4, out.len())` pixels and reports that
    /// count; a trailing group of fewer than four bytes is left alone.
    #[must_use]
    pub fn decode_opaque_run(self, bytes: &[u8], out: &mut [Pixel]) -> usize {
        let (groups, _trailing) = bytes.as_chunks::<4>();
        let count = groups.len().min(out.len());
        let (groups, out) = (&groups[..count], &mut out[..count]);
        // Whole lanes of eight, so the optimiser sees a fixed-width body.
        let (lanes, tail) = groups.as_chunks::<8>();
        let (lanes_out, tail_out) = out.as_chunks_mut::<8>();
        for (lane, slots) in lanes.iter().zip(lanes_out) {
            for (bytes, slot) in lane.iter().zip(slots) {
                *slot = self.decode_opaque(*bytes);
            }
        }
        for (bytes, slot) in tail.iter().zip(tail_out) {
            *slot = self.decode_opaque(*bytes);
        }
        count
    }

    /// Lay a run of four-byte pixels written in `from`'s order into `out` in
    /// this order, every one opaque whatever alpha it carries: what
    /// [`decode_opaque_run`](Self::decode_opaque_run) then
    /// [`encode_run`](Self::encode_run) make of it, with no pixel between.
    ///
    /// Writes and reports `min(bytes.len(), out.len()) / 4` pixels.
    #[must_use]
    pub fn transcode_opaque_run(self, from: Self, bytes: &[u8], out: &mut [u8]) -> usize {
        let (groups, _trailing) = bytes.as_chunks::<4>();
        let (slots, _) = out.as_chunks_mut::<4>();
        let count = groups.len().min(slots.len());
        let (groups, slots) = (&groups[..count], &mut slots[..count]);
        // Each pixel a word, in whole lanes of eight, so the optimiser sees a
        // fixed-width body of shifts and masks.
        let swap = from != self;
        let relay = |&bytes: &[u8; 4]| {
            let word = u32::from_le_bytes(bytes);
            let word = if swap {
                (word & 0xFF00) | (word << 16 & 0xFF_0000) | (word >> 16 & 0xFF)
            } else {
                word
            };
            (word | 0xFF00_0000).to_le_bytes()
        };
        let (lanes, tail) = groups.as_chunks::<8>();
        let (lanes_out, tail_out) = slots.as_chunks_mut::<8>();
        for (lane, out) in lanes.iter().zip(lanes_out) {
            for (bytes, slot) in lane.iter().zip(out) {
                *slot = relay(bytes);
            }
        }
        for (bytes, slot) in tail.iter().zip(tail_out) {
            *slot = relay(bytes);
        }
        count
    }

    /// [`decode`](Self::decode) with alpha taken as full.
    #[inline]
    fn decode_opaque(self, [b0, b1, b2, _]: [u8; 4]) -> Pixel {
        self.decode([b0, b1, b2, u8::MAX])
    }

    /// Encode a run of premultiplied pixels into `out`, returning the number
    /// of whole pixels written.
    ///
    /// The bulk form of [`encode`](Self::encode) — `out` is walked in whole
    /// four-byte groups and each is handed to that one encoder, so a run and
    /// a single pixel can never disagree about a channel. There is no
    /// bulk-copy special case for the order that already matches the pixel in
    /// memory: a matching order is already a four-byte move per pixel, with no
    /// shuffle left for the optimiser to remove.
    ///
    /// Writes `min(pixels.len(), out.len() / 4)` pixels and reports that
    /// count. A short `out` truncates rather than refusing or overrunning, a
    /// trailing group of fewer than four bytes is left alone rather than
    /// half-written, and a longer `out` keeps its tail untouched. The caller
    /// — the window manager, passing one row-span slice of its back buffer
    /// and the frame bytes for that same span — compares the count against
    /// `pixels.len()` to see whether the span it asked for is the span that
    /// reached the frame.
    #[must_use]
    pub fn encode_run(self, pixels: &[Pixel], out: &mut [u8]) -> usize {
        let (groups, _trailing) = out.as_chunks_mut::<4>();
        let mut written = 0;
        for (slot, pixel) in groups.iter_mut().zip(pixels) {
            *slot = self.encode(*pixel);
            written += 1;
        }
        written
    }
}

/// The length in bytes of one scan-out frame for `mode`.
///
/// `None` when the mode cannot describe a frame at all: a zero extent, a
/// stride too small for one scanline, or a size that overflows `usize` —
/// each a mode to refuse rather than a buffer to guess the size of. The
/// caller allocates, so a program reusing a frame across mode changes is
/// not forced through a fresh allocation.
#[must_use]
pub fn scanout_len(mode: &DisplayMode) -> Option<usize> {
    let min_stride = mode.width_px.checked_mul(mode.format.bytes_per_pixel())?;
    if mode.stride_bytes < min_stride || mode.width_px == 0 || mode.height_px == 0 {
        return None;
    }
    let len = u64::from(mode.stride_bytes).checked_mul(u64::from(mode.height_px))?;
    usize::try_from(len).ok()
}

/// Convert already-screen-clipped damage `bounds` into the [`DamageRect`] a
/// region present carries.
///
/// `None` means "present the whole frame instead": the bounds cover the
/// screen, are empty, or — defensively — do not fit the wire fields. A
/// rectangle that cannot be expressed exactly falls back to the full
/// present rather than presenting a wrong region.
#[must_use]
pub fn sub_screen_damage(bounds: &Rect, mode: &DisplayMode) -> Option<DamageRect> {
    let damage = as_damage(bounds)?;
    if damage.covers(mode) {
        return None;
    }
    Some(damage)
}

/// One screen rectangle as the wire's damage rectangle, or `None` when it is
/// empty or has an edge the wire's unsigned fields cannot express.
pub(crate) fn as_damage(rect: &Rect) -> Option<DamageRect> {
    let (x, y) = rect.surface_origin()?;
    Some(DamageRect {
        x,
        y,
        width_px: rect.width,
        height_px: rect.height,
    })
}

/// Fill `out` with the damage list a present carries for `region`, and
/// return the filled prefix — or `None` to present the whole frame instead.
///
/// `region` is the frame's *disjoint* damage, so each rectangle in the list
/// is blitted once and no pixel twice. There are three answers, and this is
/// the one place that chooses between them:
///
/// * the region's own rectangles, whenever they fit the list — the point of
///   the exercise, so two far-apart updates stay two small blits;
/// * its bounding rectangle, when there are more rectangles than the list
///   holds (or, defensively, one cannot be expressed) — over-covering costs
///   pixels, dropping a rectangle would leave stale ones on screen;
/// * `None`, when the damage genuinely covers the surface.
///
/// **Covering and spanning are not the same thing.** Two opposite corners of
/// the screen have a bounding box the size of the screen while changing
/// eight pixels; naming them is exactly what the list is for, so the
/// whole-frame answer is reserved for damage that really is the surface.
///
/// Call it with a non-empty `region`; an empty one names nothing to present.
#[must_use]
pub fn damage_list<'a>(
    region: &Region,
    mode: &DisplayMode,
    out: &'a mut [DamageRect; MAX_DAMAGE_RECTS],
) -> Option<&'a [DamageRect]> {
    let mut count = 0;
    if region.rects().len() <= out.len() {
        for (slot, rect) in out.iter_mut().zip(region.rects()) {
            let Some(damage) = as_damage(rect) else {
                count = 0;
                break;
            };
            *slot = damage;
            count += 1;
        }
    }
    if count == 1 && out[0].covers(mode) {
        return None;
    }
    if count == 0 {
        out[0] = sub_screen_damage(&region.bounds(), mode)?;
        count = 1;
    }
    Some(&out[..count])
}

#[cfg(test)]
#[path = "scanout_tests.rs"]
mod tests;
