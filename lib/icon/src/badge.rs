//! A colour badge: a symbol in white on a plate of one hue — a settings
//! category's or pane's rounded square, or the disc a network map stands a
//! node on.
//!
//! A reader finds a category by colour before they read its name, so the hue
//! is part of the icon's identity rather than a tint a theme applies — the
//! same badge reads on the light and dark desktops alike. It is vector art
//! drawn at the exact side a slot asks for: the plate's flat edges fall on the
//! slot's own pixel boundaries, and nothing is ever resampled, so no scale
//! leaves one edge of a stroke solid and its opposite edge grey.

use alloc::vec::Vec;

use tairix_raster::{
    Affine, Color, FillRule, Gradient, GradientKind, GradientStop, Layer, Node, Paint,
    SpreadMethod, Surface,
};
use tairix_svg::geom::place;

use crate::glyph::IconKind;
use crate::symbol::{self, Outline, Placement, DESIGN, SYMBOL_GRID, UNIT};
use crate::vector::VectorIcon;

/// The square plate: its corners ease into its sides rather than meeting
/// them at a circle's tangent, on the symbol grid.
const SQUARE: Outline = Outline::Path(
    "M6 0H18C22.3 0 24 1.7 24 6V18C24 22.3 22.3 24 18 24H6C1.7 24 0 22.3 0 18V6\
     C0 1.7 1.7 0 6 0Z",
);

/// The round plate, filling the symbol grid.
const DISC: Outline = Outline::Circle {
    centre: (SYMBOL_GRID / 2.0, SYMBOL_GRID / 2.0),
    radius: SYMBOL_GRID / 2.0,
};

/// The warning plate: a triangle standing on its base, its corners eased.
const TRIANGLE: Outline = Outline::Path(
    "M13.168 1.997L23.632 21.203Q24.8 23.3 22.4 23.3H1.6Q-0.8 23.3 0.368 21.203\
     L10.832 1.997Q12 -0.1 13.168 1.997Z",
);

/// The outline a badge's plate is cut to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum PlateShape {
    /// The rounded square a settings category stands on.
    Square,
    /// A disc.
    Round,
    /// A triangle standing on its base: a warning.
    Triangle,
}

impl PlateShape {
    fn outline(self) -> Outline {
        match self {
            Self::Square => SQUARE,
            Self::Round => DISC,
            Self::Triangle => TRIANGLE,
        }
    }

    /// The fraction of the plate's side its symbol is drawn at: smaller on a
    /// disc, whose sides curve in toward the symbol's corners, and smaller
    /// again in a triangle's narrowing top.
    fn symbol_fraction(self) -> f64 {
        match self {
            Self::Square => 0.7,
            Self::Round => 0.62,
            Self::Triangle => 0.54,
        }
    }

    /// Where the symbol stands on the plate: centred, but lowered in a
    /// triangle toward the broad part of it.
    fn symbol_placement(self) -> Placement {
        let centred = Placement::centred(self.symbol_fraction());
        match self {
            Self::Square | Self::Round => centred,
            Self::Triangle => Placement {
                origin: (centred.origin.0, centred.origin.1 + TRIANGLE_DROP * UNIT),
                ..centred
            },
        }
    }
}

/// How far below the grid's centre a triangle's symbol stands, in symbol
/// units: its visual centre sits low, where the plate is widest.
const TRIANGLE_DROP: f64 = 2.8;

/// How far the light catching the plate's upper face reaches down it.
const SHEEN_REACH: f64 = 0.6;

/// The light on the plate's upper face at its brightest.
const SHEEN: Color = Color::rgba(255, 255, 255, 40);

/// The colour a symbol is drawn in on its plate.
const INK: Color = Color::rgba(255, 255, 255, 255);

/// The hue of a badge's plate.
///
/// A closed set, so a category is given one of a few hues that read apart from
/// each other rather than a colour of its own: kin categories share one, the
/// way the input devices all stand on grey.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum BadgeHue {
    /// Neutral steel: the machine's own parts.
    Grey,
    /// Near-black: how the screen looks and when it locks.
    Graphite,
    /// The signal blue of connections and people.
    Blue,
    /// A clear sky, and what travels through the air.
    Sky,
    /// Night, and the hours.
    Indigo,
    /// Violet.
    Purple,
    /// Blue-green.
    Teal,
    /// Energy, and the machine coming up.
    Green,
    /// Sound.
    Pink,
    /// Something wants attention.
    Red,
    /// Warmth: colour chosen for its own sake.
    Orange,
}

impl BadgeHue {
    /// The plate's colour at its top edge and at its bottom edge: lit from
    /// above, like the rest of the desktop's artwork.
    ///
    /// Every ramp is dark enough at its midpoint that the white symbol stands
    /// at least 3:1 clear of it, the contrast non-text marks need to be read.
    #[must_use]
    pub const fn ramp(self) -> (Color, Color) {
        match self {
            Self::Grey => (Color::rgb(0xa9, 0xa9, 0xb0), Color::rgb(0x74, 0x74, 0x7c)),
            Self::Graphite => (Color::rgb(0x5f, 0x5f, 0x66), Color::rgb(0x2a, 0x2a, 0x30)),
            Self::Blue => (Color::rgb(0x4d, 0xa3, 0xff), Color::rgb(0x0a, 0x62, 0xe6)),
            Self::Sky => (Color::rgb(0x4c, 0xbc, 0xf5), Color::rgb(0x08, 0x7c, 0xc6)),
            Self::Indigo => (Color::rgb(0x86, 0x82, 0xff), Color::rgb(0x46, 0x40, 0xd4)),
            Self::Purple => (Color::rgb(0xc9, 0x7b, 0xff), Color::rgb(0x8a, 0x3a, 0xd6)),
            Self::Teal => (Color::rgb(0x2f, 0xc2, 0xb1), Color::rgb(0x06, 0x86, 0x79)),
            Self::Green => (Color::rgb(0x45, 0xc7, 0x62), Color::rgb(0x16, 0x8b, 0x38)),
            Self::Pink => (Color::rgb(0xff, 0x6b, 0x8c), Color::rgb(0xdb, 0x22, 0x4c)),
            Self::Red => (Color::rgb(0xff, 0x6e, 0x61), Color::rgb(0xdb, 0x30, 0x27)),
            Self::Orange => (Color::rgb(0xff, 0x8a, 0x3d), Color::rgb(0xd2, 0x52, 0x0a)),
        }
    }

    /// The plate's middle shade: the colour it is seen as, and the one a line
    /// drawn in its hue takes.
    #[must_use]
    pub const fn middle(self) -> Color {
        let (top, bottom) = self.ramp();
        Color::rgb(
            top.r.midpoint(bottom.r),
            top.g.midpoint(bottom.g),
            top.b.midpoint(bottom.b),
        )
    }
}

impl IconKind {
    /// The hue of the badge this kind is drawn as, or `None` for a kind whose
    /// built-in picture is a tintable glyph.
    ///
    /// Only the settings categories and panes are badges: their colour is how
    /// a reader finds them, where every other built-in icon takes the colour
    /// of the control drawing it.
    #[must_use]
    pub const fn badge(self) -> Option<BadgeHue> {
        Some(match self {
            Self::Settings
            | Self::Keyboard
            | Self::Mouse
            | Self::Trackpad
            | Self::Touchscreen
            | Self::Printer
            | Self::Storage
            | Self::About => BadgeHue::Grey,
            Self::Appearance | Self::LockScreen => BadgeHue::Graphite,
            Self::Display
            | Self::Networking
            | Self::Bluetooth
            | Self::Accessibility
            | Self::Users
            | Self::Ethernet
            | Self::TcpIp
            | Self::AlertInformation
            | Self::AlertQuestion => BadgeHue::Blue,
            Self::Wallpaper | Self::WiFi => BadgeHue::Sky,
            Self::Screensaver | Self::DateTime => BadgeHue::Indigo,
            Self::Sharing | Self::Caching => BadgeHue::Purple,
            Self::Language | Self::Dns => BadgeHue::Teal,
            Self::Power | Self::Startup => BadgeHue::Green,
            Self::Sound | Self::SoundDevices | Self::SoundEffects => BadgeHue::Pink,
            Self::Notifications | Self::BoardFan | Self::AlertError => BadgeHue::Red,
            Self::Theme | Self::AlertWarning => BadgeHue::Orange,
            _ => return None,
        })
    }

    /// The plate this kind's badge is cut to: the alert marks stand on the
    /// shapes a reader knows them by, every other badge on the square.
    #[must_use]
    pub const fn plate_shape(self) -> PlateShape {
        match self {
            Self::AlertWarning => PlateShape::Triangle,
            Self::AlertError | Self::AlertInformation | Self::AlertQuestion => PlateShape::Round,
            _ => PlateShape::Square,
        }
    }
}

/// `kind`'s own badge at `side` pixels, or `None` for a kind that is not one.
pub(crate) fn badge_picture(kind: IconKind, side: u32) -> Option<Surface> {
    badge(kind, kind.badge()?, kind.plate_shape(), side)
}

/// `kind`'s symbol in white on a plate of `hue` cut to `shape`, at `side`
/// pixels: the badge a settings category wears, for a caller that chooses the
/// hue — a network map colours a host by what it is. `None` for a zero side.
///
/// The plate is filled once at the exact side: nothing abuts it, so it has no
/// seam to resolve, and its flat edges fall on the slot's pixel boundaries.
/// Only the symbol, whose parts meet one another, goes through the
/// seam-resolving supersample of [`Surface::layered`]. A kind with no symbol,
/// or one that cannot be built — a defect in the compiled-in table, which the
/// tests build whole — leaves the plate standing alone, so the colour still
/// shows.
#[must_use]
pub fn badge(kind: IconKind, hue: BadgeHue, shape: PlateShape, side: u32) -> Option<Surface> {
    let (top, bottom) = hue.ramp();
    if side == 0 {
        return None;
    }
    let px_per_unit = f64::from(side) / SYMBOL_GRID;
    let outline = symbol::flatten(&[shape.outline()], symbol::flatness_at(px_per_unit))?;
    let plain = downward(&[(0.0, top), (1.0, bottom)]);
    // The light on the upper face folds into the plate's own ramp: its top
    // edge lifted toward white, and the plain ramp again where the light ends.
    let lit = SHEEN.premultiply().over(top.premultiply()).unpremultiply();
    let reach = plain.sample((0.0, SHEEN_REACH * f64::from(DESIGN)));
    let plate = Layer::filled(
        Paint::Gradient(downward(&[(0.0, lit), (SHEEN_REACH, reach), (1.0, bottom)])),
        FillRule::NonZero,
        place(&outline, Affine::scale(UNIT, UNIT)),
    );
    let mut picture = Surface::new(side, side)?;
    if !picture.draw_artwork(&[Node::Fill(plate)], DESIGN) {
        return None;
    }
    let fraction = shape.symbol_fraction();
    if let Some(ink) = ink(kind, shape.symbol_placement(), px_per_unit * fraction)
        .and_then(|symbol| symbol.rasterise(side))
    {
        picture.blit(0, 0, &ink);
    }
    Some(picture)
}

/// `kind`'s symbol in white, placed by `placement`, flattened for a picture
/// `px_per_unit` pixels to the symbol unit.
///
/// Not fitted to the pixel grid as a glyph is: a symmetric symbol's middle
/// stroke could only be made crisp at half the sides by drawing it half a
/// pixel off its plate's centre.
fn ink(kind: IconKind, placement: Placement, px_per_unit: f64) -> Option<VectorIcon> {
    let layers = symbol::layers(
        symbol::marks(kind)?,
        placement,
        &Paint::Solid(INK),
        symbol::flatness_at(px_per_unit),
    )?;
    Some(VectorIcon::new(DESIGN, layers))
}

/// A ramp through `stops` running from the top of the design grid to its
/// bottom.
fn downward(stops: &[(f64, Color)]) -> Gradient {
    let per_unit = 1.0 / f64::from(DESIGN);
    Gradient {
        kind: GradientKind::Linear,
        stops: stops
            .iter()
            .map(|&(offset, color)| GradientStop { offset, color })
            .collect::<Vec<_>>(),
        spread: SpreadMethod::Pad,
        // The ramp's parameter is the point's height; the other axis is only
        // there to keep the map invertible.
        to_gradient: Affine {
            a: 0.0,
            b: per_unit,
            c: per_unit,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        },
    }
}

#[cfg(test)]
#[path = "badge_tests.rs"]
mod tests;
