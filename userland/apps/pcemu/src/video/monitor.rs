//! The monitors a card can drive, and how each turns the card's codes into
//! colour. The monitor is the user's choice, not the card's.

use super::raster::{Conversion, Palette};

/// A monochrome phosphor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phosphor {
    /// Green, as the IBM 5151.
    Green,
    /// Amber.
    Amber,
    /// White.
    White,
}

impl Phosphor {
    /// The phosphor's colour at full drive.
    #[must_use]
    pub const fn bright(self) -> [u8; 3] {
        match self {
            Self::Green => [0x7A, 0xFF, 0x8C],
            Self::Amber => [0xFF, 0xC2, 0x40],
            Self::White => [0xFF, 0xFF, 0xFF],
        }
    }

    /// The phosphor's colour at normal drive.
    #[must_use]
    pub const fn normal(self) -> [u8; 3] {
        match self {
            Self::Green => [0x1A, 0xC8, 0x3C],
            Self::Amber => [0xD0, 0x8A, 0x00],
            Self::White => [0xB8, 0xB8, 0xB8],
        }
    }

    /// The colour at `level` of 255 drive, scaled along the phosphor's own
    /// hue.
    #[must_use]
    pub fn at(self, level: u8) -> [u8; 3] {
        let full = self.bright();
        full.map(|c| u8::try_from(u16::from(c) * u16::from(level) / 255).unwrap_or(u8::MAX))
    }
}

/// The monitor attached to a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Monitor {
    /// A digital or analog colour monitor matching the card: the IBM 5153
    /// for a CGA, the 5154 for an EGA, an analog display for a VGA.
    Colour,
    /// A composite colour monitor or television, on a CGA's composite
    /// output.
    Composite,
    /// A monochrome monitor.
    Mono(Phosphor),
}

/// The IBM 5153's sixteen colours: RGBI at two thirds and one third, and
/// the monitor's own circuit turning dark yellow brown.
pub const CGA_COLOURS: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0xAA],
    [0x00, 0xAA, 0x00],
    [0x00, 0xAA, 0xAA],
    [0xAA, 0x00, 0x00],
    [0xAA, 0x00, 0xAA],
    [0xAA, 0x55, 0x00],
    [0xAA, 0xAA, 0xAA],
    [0x55, 0x55, 0x55],
    [0x55, 0x55, 0xFF],
    [0x55, 0xFF, 0x55],
    [0x55, 0xFF, 0xFF],
    [0xFF, 0x55, 0x55],
    [0xFF, 0x55, 0xFF],
    [0xFF, 0xFF, 0x55],
    [0xFF, 0xFF, 0xFF],
];

/// The monochrome cards' codes: dark, normal and bright.
pub mod mono {
    /// No beam.
    pub const DARK: u32 = 0;
    /// Normal intensity.
    pub const NORMAL: u32 = 1;
    /// High intensity.
    pub const BRIGHT: u32 = 2;
}

/// The conversion for a monochrome card's codes on `phosphor`.
#[must_use]
pub fn mono_conversion(phosphor: Phosphor) -> Conversion {
    Conversion::Palette(Palette::new(&[
        [0, 0, 0],
        phosphor.normal(),
        phosphor.bright(),
    ]))
}

/// The conversion for RGBI codes on `monitor`; a monochrome monitor shows
/// their luminance.
#[must_use]
pub fn rgbi_conversion(monitor: Monitor) -> Conversion {
    match monitor {
        Monitor::Mono(phosphor) => {
            let mut shades = [[0u8; 3]; 16];
            for (shade, &[r, g, b]) in shades.iter_mut().zip(&CGA_COLOURS) {
                let luma = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
                *shade = phosphor.at(u8::try_from(luma).unwrap_or(u8::MAX));
            }
            Conversion::Palette(Palette::new(&shades))
        }
        Monitor::Colour | Monitor::Composite => Conversion::Palette(Palette::new(&CGA_COLOURS)),
    }
}
