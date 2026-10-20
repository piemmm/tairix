//! Display lists as the fifth-generation HVS reads them: planes of linear RGB
//! surfaces, each placed and scaled onto a channel's output and blended over
//! those before it, after Linux's `vc4_plane_mode_set`.

use tairix_abi::driver::display::DisplayFormat;
use tairix_abi::DriverError;
use tairix_inline::ArrayVec;

use crate::regs::field;

/// The word that ends a display list.
pub(crate) const END: u32 = 1 << 31;

/// The most words one plane takes: eight control, position, size, context,
/// pointer and pitch words, the scaled size, the line-buffer base, the
/// trapezoidal filter across and the polyphase one down, and the four filter
/// kernel offsets.
pub(crate) const PLANE_WORDS_MAX: usize = 18;

/// One plane's words.
pub(crate) type PlaneWords = ArrayVec<u32, PLANE_WORDS_MAX>;

/// Words of the polyphase filter kernel in display-list memory.
pub(crate) const KERNEL_WORDS: usize = 11;

/// Line-buffer words the HVS has for vertically scaled planes.
pub(crate) const LBM_WORDS: u32 = 60 * 1024;

const CTL0_VALID: u32 = 1 << 30;
const CTL0_SIZE_SHIFT: u32 = 24;
const CTL0_UNITY: u32 = 1 << 15;
const CTL0_ORDER_SHIFT: u32 = 13;
const CTL0_ALPHA_EXPAND: u32 = 1 << 12;
const CTL0_RGB_EXPAND: u32 = 1 << 11;
const CTL0_SCL1_SHIFT: u32 = 8;
const CTL0_SCL0_SHIFT: u32 = 5;
const FORMAT_RGBA8888: u32 = 7;
/// The byte orders of a 32-bit pixel: B, G, R, A in memory reads as ARGB.
const ORDER_ARGB: u32 = 2;
const ORDER_ABGR: u32 = 3;
const CTL2_ALPHA_FIXED: u32 = 1 << 30;
const CTL2_ALPHA_PREMULT: u32 = 1 << 29;
const CTL2_ALPHA_MIX: u32 = 1 << 28;
const OPAQUE: u32 = 0xFFF;

/// Where an unscaled plane holds its position, its size and its pointer: the
/// words a cursor moved in a live list rewrites.
pub(crate) const UNITY_POSITION: usize = 1;
pub(crate) const UNITY_SIZE: usize = 3;
pub(crate) const UNITY_POINTER: usize = 5;
/// A word the HVS writes as it walks the list.
const CONTEXT: u32 = 0xC0C0_C0C0;
const PPF_AGC: u32 = 1 << 30;
/// Bits of a polyphase filter's phase.
const PHASE_BITS: u32 = 6;

/// How one axis of a plane is resized.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Scaling {
    None,
    /// The polyphase filter: enlarging, or shrinking by no more than a third.
    Ppf,
    /// The trapezoidal filter: shrinking further.
    Tpz,
}

impl Scaling {
    /// How a `src`-pixel axis is drawn `dst` pixels long.
    pub const fn of(src: u32, dst: u32) -> Self {
        if src == dst {
            Self::None
        } else if src > 1 && 3 * dst as u64 >= 2 * src as u64 {
            Self::Ppf
        } else {
            Self::Tpz
        }
    }

    /// The control word's scaler mode for a plane resized `x` across and
    /// `y` down; the unity plane says so in its own bit.
    const fn mode(x: Self, y: Self) -> u32 {
        match (x, y) {
            (Self::Ppf, Self::Ppf) | (Self::None, Self::None) => 0,
            (Self::Tpz, Self::Ppf) => 1,
            (Self::Ppf, Self::Tpz) => 2,
            (Self::Tpz, Self::Tpz) => 3,
            (Self::Ppf, Self::None) => 4,
            (Self::None, Self::Ppf) => 5,
            (Self::None, Self::Tpz) => 6,
            (Self::Tpz, Self::None) => 7,
        }
    }
}

/// How a plane's pixels blend over the planes before it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Alpha {
    /// Opaque, whatever its alpha bytes hold.
    Opaque,
    /// Its own premultiplied alpha, scaled by a constant opacity.
    Premultiplied(u8),
}

impl Alpha {
    /// The control word that says so.
    const fn control(self) -> u32 {
        match self {
            Self::Opaque => CTL2_ALPHA_FIXED | field(OPAQUE, 4, 12),
            Self::Premultiplied(u8::MAX) => CTL2_ALPHA_PREMULT | field(OPAQUE, 4, 12),
            // Twelve bits of constant alpha, scaling the plane's own.
            Self::Premultiplied(opacity) => {
                let wide = (opacity as u32 * OPAQUE + 127) / 255;
                CTL2_ALPHA_PREMULT | CTL2_ALPHA_MIX | field(wide, 4, 12)
            }
        }
    }
}

/// A linear surface the HVS reads, at its bus address.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Surface {
    /// The address the HVS reads its first byte at.
    pub bus: u32,
    /// Bytes from one line's start to the next's.
    pub stride: u32,
    /// Pixels across.
    pub width: u32,
    /// Lines down.
    pub height: u32,
    /// How a pixel's bytes lie.
    pub format: DisplayFormat,
}

/// Where a plane lands on its output.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Place {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Place {
    /// A `width` by `height` surface drawn as large as an output of
    /// `out_width` by `out_height` holds it with its aspect kept, centred.
    pub fn fit(width: u32, height: u32, out_width: u32, out_height: u32) -> Self {
        let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let (ow, oh) = (u64::from(out_width), u64::from(out_height));
        // Narrower than the output: full height, bars at the sides.
        let (pw, ph) = if w * oh > ow * h {
            (ow, (h * ow + w / 2) / w)
        } else {
            ((w * oh + h / 2) / h, oh)
        };
        let (pw, ph) = (pw.clamp(1, ow), ph.clamp(1, oh));
        let narrow = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        Self {
            x: narrow((ow - pw) / 2),
            y: narrow((oh - ph) / 2),
            width: narrow(pw),
            height: narrow(ph),
        }
    }
}

/// Line-buffer words a plane of `source_width` pixels resized to `place`
/// takes: none unless it is scaled down the screen.
pub(crate) const fn lbm_words(source_width: u32, x: Scaling, y: Scaling, place_width: u32) -> u32 {
    let per_line = if matches!(x, Scaling::Tpz) {
        place_width
    } else {
        source_width
    };
    let lines = match y {
        Scaling::None => return 0,
        Scaling::Tpz => 8,
        Scaling::Ppf => 16,
    };
    (per_line * lines).next_multiple_of(128) / 4
}

/// The polyphase filter word resizing a `src`-pixel axis to `dst` pixels, its
/// phase starting half a pixel in, as Linux's `vc4_write_ppf` sets it.
fn ppf(src: u32, dst: u32) -> u32 {
    let src = src << 16;
    let scale = (src / dst) & !1;
    // Half the rounding error of the scale, less half a pixel, held below a
    // whole pixel; a negative phase is the field's two's complement.
    let half_error = ((src - dst * scale) >> (16 - PHASE_BITS)) >> 1;
    let half_pixel = 1 << (PHASE_BITS - 1);
    let phase = if half_error >= half_pixel + (1 << PHASE_BITS) {
        (1 << PHASE_BITS) - 1
    } else {
        half_error.wrapping_sub(half_pixel)
    };
    PPF_AGC | field(scale, 8, 17) | field(phase, 0, 7)
}

/// The two trapezoidal filter words shrinking a `src`-pixel axis to `dst`.
fn tpz(src: u32, dst: u32) -> [u32; 2] {
    let src = src << 16;
    let (scale, recip) = if dst << 16 < src {
        let scale = src / dst;
        (scale, u32::MAX / scale)
    } else {
        ((1 << 16) + 1, (1 << 16) - 1)
    };
    [field(scale, 8, 21), field(recip, 0, 16)]
}

/// The words placing `surface` at `place`, blended by `alpha`, `kernel` the
/// word offset of the filter kernel and `lbm` the line-buffer base it scales
/// through.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a surface or placement no field can hold.
pub(crate) fn plane(
    surface: &Surface,
    place: &Place,
    (kernel, lbm): (u32, u32),
    alpha: Alpha,
) -> Result<PlaneWords, DriverError> {
    let fits = surface.width.max(surface.height) < 1 << 13
        && place.width.max(place.height) < 1 << 13
        && place.x < 1 << 14
        && place.y < 1 << 12
        && surface.stride < 1 << 16
        && kernel < 1 << 14
        && surface.width != 0
        && surface.height != 0
        && place.width != 0
        && place.height != 0;
    if !fits {
        return Err(DriverError::OutOfRange);
    }
    let x = Scaling::of(surface.width, place.width);
    let y = Scaling::of(surface.height, place.height);
    let unity = matches!((x, y), (Scaling::None, Scaling::None));
    let order = match surface.format {
        DisplayFormat::Bgra8888 => ORDER_ARGB,
        _ => ORDER_ABGR,
    };
    let scl = Scaling::mode(x, y);
    let mut words = PlaneWords::new();
    let mut push = |word| words.try_push(word).map_err(|_| DriverError::OutOfRange);
    push(
        CTL0_VALID
            | field(order, CTL0_ORDER_SHIFT, 2)
            | FORMAT_RGBA8888
            | if unity { CTL0_UNITY } else { 0 }
            | field(scl, CTL0_SCL0_SHIFT, 3)
            | field(scl, CTL0_SCL1_SHIFT, 3)
            | CTL0_ALPHA_EXPAND
            | CTL0_RGB_EXPAND,
    )?;
    push(position(place))?;
    push(alpha.control())?;
    if !unity {
        push(field(place.width, 0, 13) | field(place.height, 16, 13))?;
    }
    push(size(surface))?;
    push(CONTEXT)?;
    push(surface.bus)?;
    push(CONTEXT)?;
    push(field(surface.stride, 0, 16))?;
    if y != Scaling::None {
        push(lbm)?;
    }
    if x == Scaling::Ppf {
        push(ppf(surface.width, place.width))?;
    }
    if y == Scaling::Ppf {
        push(ppf(surface.height, place.height))?;
        push(CONTEXT)?;
    }
    if x == Scaling::Tpz {
        let [scale, recip] = tpz(surface.width, place.width);
        push(scale)?;
        push(recip)?;
    }
    if y == Scaling::Tpz {
        let [scale, recip] = tpz(surface.height, place.height);
        push(scale)?;
        push(recip)?;
        push(CONTEXT)?;
    }
    if x == Scaling::Ppf || y == Scaling::Ppf {
        for _ in 0..4 {
            push(field(kernel, 0, 14))?;
        }
    }
    let size = u32::try_from(words.len()).map_err(|_| DriverError::OutOfRange)?;
    words[0] |= field(size, CTL0_SIZE_SHIFT, 6);
    Ok(words)
}

/// A plane's position word.
pub(crate) const fn position(place: &Place) -> u32 {
    field(place.x, 0, 14) | field(place.y, 16, 12)
}

/// Bits of each side in a plane's source size word.
const SIZE_BITS: u32 = 13;

/// A plane's source size word.
pub(crate) const fn size(surface: &Surface) -> u32 {
    field(surface.width, 0, SIZE_BITS) | field(surface.height, 16, SIZE_BITS)
}

/// The size word no larger, on either axis, than both size words `a` and
/// `b`.
pub(crate) const fn narrower(a: u32, b: u32) -> u32 {
    const fn least(a: u32, b: u32) -> u32 {
        if a < b {
            a
        } else {
            b
        }
    }
    let side = (1 << SIZE_BITS) - 1;
    field(least(a & side, b & side), 0, SIZE_BITS)
        | field(least((a >> 16) & side, (b >> 16) & side), 16, SIZE_BITS)
}

/// Three nine-bit filter coefficients packed into one word.
const fn coefficients(c0: i32, c1: i32, c2: i32) -> u32 {
    field(c0.cast_unsigned(), 0, 9)
        | field(c1.cast_unsigned(), 9, 9)
        | field(c2.cast_unsigned(), 18, 9)
}

/// The Mitchell-Netravali (B = C = 1/3) kernel the polyphase filter resizes
/// through, in display-list memory's mirrored form.
///
/// Reference: Mitchell and Netravali, "Reconstruction Filters in Computer
/// Graphics" (SIGGRAPH 1988), as Linux's `vc4_hvs` uploads it.
pub(crate) const fn kernel() -> [u32; KERNEL_WORDS] {
    let half = [
        coefficients(0, -2, -6),
        coefficients(-8, -10, -8),
        coefficients(-3, 2, 18),
        coefficients(50, 82, 119),
        coefficients(155, 187, 213),
        coefficients(227, 227, 0),
    ];
    let mut words = [0; KERNEL_WORDS];
    let mut at = 0;
    while at < KERNEL_WORDS {
        words[at] = if at < half.len() {
            half[at]
        } else {
            half[KERNEL_WORDS - at - 1]
        };
        at += 1;
    }
    words
}

#[cfg(test)]
#[path = "dlist_tests.rs"]
mod tests;
