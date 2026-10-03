//! The 8-bit sRGB colour, opaque or with straight alpha.
//!
//! These are colour *values*: what a theme authors, a settings document
//! stores and a picker edits. Compositing belongs to the rasteriser's
//! premultiplied pixel (`lib/raster`), so none happens here beyond resolving
//! one authored colour against one ground.

/// An opaque colour with 8 bits per channel.
///
/// A colour that can never be translucent — a desktop backdrop, a terminal
/// scheme's ink — holds this rather than an [`Rgba`], so a round trip never
/// has to invent an alpha.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Black.
    pub const BLACK: Self = Self::new(0, 0, 0);

    /// White.
    pub const WHITE: Self = Self::new(255, 255, 255);

    /// The colour with these channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// This colour, fully opaque.
    #[must_use]
    pub const fn opaque(self) -> Rgba {
        self.with_alpha(u8::MAX)
    }

    /// This colour at opacity `a`.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Rgba {
        Rgba::new(self.r, self.g, self.b, a)
    }

    /// The channels as `[r, g, b]`.
    #[must_use]
    pub const fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl From<Rgb> for Rgba {
    fn from(rgb: Rgb) -> Self {
        rgb.opaque()
    }
}

/// A straight-alpha colour with 8 bits per channel.
///
/// Channels are **not** premultiplied: `a` is an independent opacity, `0`
/// fully transparent and `255` fully opaque.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Rgba {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha: `0` fully transparent, `255` fully opaque.
    pub a: u8,
}

impl Rgba {
    /// Fully transparent (all channels zero).
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// An opaque colour from its red, green, and blue channels.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, u8::MAX)
    }

    /// A colour from all four channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// A colour from `[r, g, b, a]`.
    #[must_use]
    pub const fn from_array([r, g, b, a]: [u8; 4]) -> Self {
        Self::new(r, g, b, a)
    }

    /// The same colour with its alpha replaced by `a`.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Its colour channels, the alpha set aside.
    #[must_use]
    pub const fn without_alpha(self) -> Rgb {
        Rgb::new(self.r, self.g, self.b)
    }

    /// The channels as `[r, g, b, a]`.
    #[must_use]
    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// True when the colour is fully opaque (`a == 255`).
    #[must_use]
    pub const fn is_opaque(self) -> bool {
        self.a == u8::MAX
    }

    /// This colour mixed toward `other` by `permille` (`0` keeps `self`,
    /// `1000` returns `other`), each channel interpolated independently and
    /// rounded to nearest.
    ///
    /// A theme derives a role's near neighbours this way — the darker plate of
    /// a pressed control, the brighter one of a hovered one — rather than
    /// authoring a token per interaction state.
    #[must_use]
    pub fn mix(self, other: Self, permille: u16) -> Self {
        Self {
            r: mix_channel(self.r, other.r, permille),
            g: mix_channel(self.g, other.g, permille),
            b: mix_channel(self.b, other.b, permille),
            a: mix_channel(self.a, other.a, permille),
        }
    }

    /// This colour as it reads over the opaque `ground` it is drawn on,
    /// keeping `ground`'s own opacity.
    ///
    /// A surface that lays a translucent role down, rather than compositing
    /// it, needs the one colour it resolves to, or its alpha would punch a
    /// hole through the surface.
    #[must_use]
    pub fn over(self, ground: Self) -> Self {
        let permille = u16::try_from(u32::from(self.a) * 1000 / 255).unwrap_or(1000);
        ground.mix(self, permille).with_alpha(ground.a)
    }
}

/// One channel interpolated toward `to` by `permille`, an out-of-range weight
/// saturating at the endpoint rather than wrapping.
fn mix_channel(from: u8, to: u8, permille: u16) -> u8 {
    let weight = u32::from(permille.min(1000));
    let blended = (u32::from(from) * (1000 - weight) + u32::from(to) * weight + 500) / 1000;
    u8::try_from(blended).unwrap_or(u8::MAX)
}
