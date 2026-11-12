//! A CGA's composite output, as a colour television decodes it.
//!
//! The dot clock is four times the NTSC colour carrier, so every dot is a
//! quarter of a carrier cycle. The card makes each colour's chroma a square
//! wave at one of eight phases of the carrier, the phase-shifted clocks its
//! colour multiplexer selects between; intensity adds a level of its own.
//! Its colour burst is colour 6's chroma, peaking on the first dot of every
//! four. The television takes luma as the mean over one carrier cycle and
//! the U and V components by demodulating the same four dots against the
//! burst, so a pattern of dots finer than a cycle becomes colour: the
//! artifact colours of 640-pixel graphics, a lone first dot of four showing
//! the burst's own yellow-brown, the second red, the third blue and the
//! fourth green. The card's logic delays and the set's chroma filter, which
//! turn a real CGA's hues a little further, are not modelled.

use super::raster::Conversion;

/// The signal's scale: white at full intensity.
const FULL: i32 = 256;
/// What the chroma square wave contributes at its high level.
const CHROMA: i32 = 170;
/// What the intensity bit contributes.
const INTENSITY: i32 = FULL - CHROMA;
/// Chroma's gain into colour, in 256ths.
const SATURATION: i32 = 150;

/// Each colour's chroma phase, in eighths of a carrier cycle, the one its
/// multiplexer selects to decode nearest its own hue; `None` for black and
/// white, whose chroma is held low and high.
const PHASE: [Option<u8>; 8] = [
    None,
    Some(3),
    Some(6),
    Some(5),
    Some(1),
    Some(2),
    Some(7),
    None,
];

/// The signal level of dot `colour` a quarter `quarter` of a carrier cycle
/// from the burst: the square wave averaged over the dot.
const fn level(colour: u8, quarter: u8) -> i32 {
    let chroma = match PHASE[(colour & 7) as usize] {
        None => {
            if colour & 7 == 7 {
                2
            } else {
                0
            }
        }
        Some(phase) => {
            let mut high = 0;
            let mut half = 0;
            while half < 2 {
                let eighth = (quarter * 2 + half + 8 - phase) % 8;
                if eighth < 4 {
                    high += 1;
                }
                half += 1;
            }
            high
        }
    };
    chroma * CHROMA / 2 + if colour & 8 != 0 { INTENSITY } else { 0 }
}

const fn levels() -> [[i32; 4]; 16] {
    let mut table = [[0; 4]; 16];
    let mut colour = 0;
    while colour < 16 {
        let mut quarter = 0;
        while quarter < 4 {
            table[colour as usize][quarter as usize] = level(colour, quarter);
            quarter += 1;
        }
        colour += 1;
    }
    table
}

const LEVELS: [[i32; 4]; 16] = levels();

/// A line of composite decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Composite {
    /// Whether the colour burst is sent; without it a set shows greyscale.
    pub burst: bool,
    /// The carrier phase of the line's first dot.
    pub phase: u8,
}

/// The conversion for a CGA on a composite colour monitor whose visible
/// line starts `left` dots after sync.
#[must_use]
pub const fn conversion(burst: bool, left: u32) -> Conversion {
    Conversion::Composite(Composite {
        burst,
        phase: (left % 4) as u8,
    })
}

fn clamp(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

impl Composite {
    /// Decodes a line of RGBI codes into RGBA.
    #[allow(
        clippy::many_single_char_names,
        reason = "Y, U, V, R, G and B are the colour components' own names"
    )]
    pub fn line(&self, codes: &[u32], out: &mut [u8]) {
        let phase = usize::from(self.phase);
        for (x, pixel) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
            .take(codes.len())
        {
            let (mut y, mut u, mut v) = (0, 0, 0);
            // One carrier cycle of dots, x - 2 to x + 1; off the line is black.
            for k in 0..4 {
                let Some(at) = (x + k).checked_sub(2) else {
                    continue;
                };
                let Some(&code) = codes.get(at) else { continue };
                let quarter = (at + phase) % 4;
                let s = LEVELS[usize::try_from(code & 15).unwrap_or(0)][quarter];
                y += s;
                // The burst peaks on the first dot of the cycle, the -U axis.
                match quarter {
                    0 => u -= s,
                    1 => v += s,
                    2 => u += s,
                    _ => v -= s,
                }
            }
            let y = y / 4;
            let (u, v) = if self.burst {
                (u * SATURATION / 256, v * SATURATION / 256)
            } else {
                (0, 0)
            };
            // YUV to RGB, in 256ths.
            let r = y + (292 * v) / 256;
            let g = y - (101 * u + 149 * v) / 256;
            let b = y + (520 * u) / 256;
            pixel.copy_from_slice(&[clamp(r), clamp(g), clamp(b), 0xFF]);
        }
    }
}

crate::snapshot::state!(Composite { burst, phase });

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(codes: &[u32], burst: bool) -> alloc::vec::Vec<[u8; 3]> {
        let mut out = alloc::vec![0u8; codes.len() * 4];
        Composite { burst, phase: 0 }.line(codes, &mut out);
        out.as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    #[test]
    fn white_and_black_carry_no_colour() {
        let white = decode(&[15; 16], true);
        assert_eq!(white[8], [255, 255, 255]);
        let black = decode(&[0; 16], true);
        assert_eq!(black[8], [0, 0, 0]);
        let grey = decode(&[7; 16], true)[8];
        assert!(
            grey[0] == grey[1] && grey[1] == grey[2],
            "light grey stays grey: {grey:?}"
        );
    }

    #[test]
    fn the_primaries_decode_to_their_hues() {
        let hue = |code: u32| decode(&[code; 32], true)[16];
        let [r, g, b] = hue(4);
        assert!(r > g && r > b, "red: {r} {g} {b}");
        let [r, g, b] = hue(2);
        assert!(g > r && g > b, "green: {r} {g} {b}");
        let [r, g, b] = hue(1);
        assert!(b > r && b > g, "blue: {r} {g} {b}");
    }

    #[test]
    fn a_lone_dot_in_each_place_of_the_cycle_gives_the_cga_artifact_hues() {
        let hue = |place: usize| {
            let pattern: alloc::vec::Vec<u32> = (0..32)
                .map(|i| if i % 4 == place { 15 } else { 0 })
                .collect();
            decode(&pattern, true)[16]
        };
        let [r, g, b] = hue(0);
        assert!(r > b && g > b, "yellow-brown: {r} {g} {b}");
        let [r, g, b] = hue(1);
        assert!(r > g && r > b, "red: {r} {g} {b}");
        let [r, g, b] = hue(2);
        assert!(b > r && b > g, "blue: {r} {g} {b}");
        let [r, g, b] = hue(3);
        assert!(g > r && g > b, "green: {r} {g} {b}");
    }

    #[test]
    fn fine_patterns_become_artifact_colour_and_mono_drops_it() {
        let pattern: alloc::vec::Vec<u32> =
            (0..32).map(|i| if i % 4 < 2 { 15 } else { 0 }).collect();
        let [r, g, b] = decode(&pattern, true)[16];
        assert!(
            r.abs_diff(g).max(g.abs_diff(b)) > 40,
            "alternate pixel pairs show colour: {r} {g} {b}"
        );
        let [r, g, b] = decode(&pattern, false)[16];
        assert!(r == g && g == b, "no burst, no colour");
    }
}
