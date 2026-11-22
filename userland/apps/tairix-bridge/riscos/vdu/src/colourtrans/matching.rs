//! ColourTrans's match: which of a palette's colours, or which pixel of a
//! direct-colour mode, lies nearest a wanted colour or furthest from it, by
//! the error the loadings weigh, as RISC OS 5 picks it.

use tairix_colour::Rgb;
use tairix_vdu::palette::default_256;

use crate::colour::Direct;
use crate::palette::EIGHT_BIT_COLOURS;

/// The weights of the squared error ColourTrans minimises, held as the
/// word `*ColourTransLoadings` and `MiscOp` 0 set: `&BBGGRRxx`, a byte a
/// channel, its low byte kept but unused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Loadings(pub(crate) u32);

impl Loadings {
    /// RISC OS's own: red two, green four, blue one.
    pub(crate) const DEFAULT: Self = Self(0x0104_0200);

    /// Red's, green's and blue's loadings, the low byte nought.
    pub(crate) const fn of_channels([red, green, blue]: [u8; 3]) -> Self {
        Self(u32::from_le_bytes([0, red, green, blue]))
    }

    /// Red's, green's and blue's loadings.
    pub(crate) const fn channels(self) -> [u8; 3] {
        let [_, red, green, blue] = self.0.to_le_bytes();
        [red, green, blue]
    }

    /// The error between colours `a` and `b`.
    pub(crate) const fn error(self, a: Rgb, b: Rgb) -> u32 {
        let [red, green, blue] = self.channels();
        term(red, a.r, b.r) + term(green, a.g, b.g) + term(blue, a.b, b.b)
    }
}

/// One channel's weighted squared difference.
const fn term(weight: u8, x: u8, y: u8) -> u32 {
    let difference = x.abs_diff(y) as u32;
    weight as u32 * difference * difference
}

/// Which way a match looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    Nearest,
    Furthest,
}

impl Look {
    /// Whether error `error` beats `best`: strictly nearer, or at least as
    /// far, so the first of equally near and the last of equally far win.
    const fn beats(self, error: u32, best: u32) -> bool {
        match self {
            Self::Nearest => error < best,
            Self::Furthest => error >= best,
        }
    }
}

/// The index among `colours` of the match for `wanted`; nought for none.
pub(crate) fn search(colours: &[Rgb], wanted: Rgb, loadings: Loadings, look: Look) -> usize {
    let mut best: Option<(u32, usize)> = None;
    for (i, &colour) in colours.iter().enumerate() {
        let error = loadings.error(colour, wanted);
        if best.is_none_or(|(b, _)| look.beats(error, b)) {
            best = Some((error, i));
        }
    }
    best.map_or(0, |(_, i)| i)
}

/// Whether 256 colours are the default palette's as ColourTrans tells it:
/// each channel's top four bits alike, as VIDC1 held a colour.
pub(crate) fn is_default_256(colours: &[Rgb]) -> bool {
    colours.len() == EIGHT_BIT_COLOURS
        && colours.iter().zip(0..=u8::MAX).all(|(colour, pixel)| {
            let [r, g, b] = default_256(pixel);
            [colour.r >> 4, colour.g >> 4, colour.b >> 4] == [r >> 4, g >> 4, b >> 4]
        })
}

/// The default 256-colour palette's match, made from the palette's own form
/// as RISC OS 5 makes it: for each tint the nearest colour of that tint, the
/// last of equally near, then of those four the nearest, or looking furthest
/// the furthest, the first of equals. Looking furthest thus finds the tint
/// whose nearest colour lies furthest, not the furthest colour.
pub(crate) fn default_256_match(wanted: Rgb, loadings: Loadings, look: Look) -> u8 {
    let mut chosen: Option<(u32, u8)> = None;
    for tint in 0..4u8 {
        let mut nearest: Option<(u32, u8)> = None;
        for pixel in (tint..=u8::MAX).step_by(4) {
            let [r, g, b] = default_256(pixel);
            let error = loadings.error(Rgb::new(r, g, b), wanted);
            if nearest.is_none_or(|(n, _)| error <= n) {
                nearest = Some((error, pixel));
            }
        }
        if let Some((error, pixel)) = nearest {
            let wins = chosen.is_none_or(|(c, _)| match look {
                Look::Nearest => error < c,
                Look::Furthest => error > c,
            });
            if wins {
                chosen = Some((error, pixel));
            }
        }
    }
    chosen.map_or(0, |(_, pixel)| pixel)
}

/// A direct-colour mode's match: each channel cut to its bits, or looking
/// furthest, every one of those bits inverted.
pub(crate) fn direct_match(direct: Direct, wanted: Rgb, look: Look) -> u32 {
    let pixel = direct.pack(wanted.to_array());
    match look {
        Look::Nearest => pixel,
        Look::Furthest => pixel ^ direct.colour_bits(),
    }
}

#[cfg(test)]
mod tests;
