//! Card artwork: faces, backs and shadows, drawn as vector geometry at the
//! card's own size and kept as tiles the painter lays down.
//!
//! Nothing is a bitmap scaled from elsewhere. Each suit is a parametric
//! outline, each index is set in the desktop's serif face, and each court
//! card's crown is its own shape, so a card is as sharp on a dense display
//! as at the reference density and re-colours with the table and the theme.
//!
//! A tile is drawn the first time it is needed and kept until the card's
//! size or colours change, or until the machine asks for memory back
//! ([`CardArt::trim`]): a game shows most of the deck soon after the deal,
//! and drawing a face is far dearer than laying one down.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::font_ipc::{FamilyKey, FontWeight};
use tairix_font::BitmapFont;
use tairix_raster::{
    cast_shadow, Affine, Color, FillRule, Gradient, GradientKind, GradientStop, Paint, Reorient,
    Ring, RingInk, ShadowCast, SpreadMethod, Surface, SUBPIXEL,
};
use tairix_theme::{Appearance, Theme};
use tairix_util::mathf;

use crate::card::{Face, Rank, Suit};
use crate::prefs::{Back, Baize, Preferences};

/// The square grid every outline below is authored on.
const DESIGN: u32 = 1000;

/// The face the indices are set in: a serif, as printed cards' are. A
/// spelling the key grammar refused would leave the indices no face at all,
/// so the fallback is the fixed-pitch one every image ships; a test asserts
/// the serif's spelling resolves.
const INDEX_FACE: FamilyKey = match FamilyKey::new("noto-serif") {
    Ok(key) => key,
    Err(_) => FamilyKey::MONO,
};

/// Every colour a table and its cards are drawn with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Palette {
    /// The table at its centre.
    pub felt: Color,
    /// The table at its edges.
    pub felt_edge: Color,
    /// An empty place's plate.
    pub slot: Color,
    /// An empty place's rim, and its faint mark.
    pub slot_rim: Color,
    /// Text on the table.
    pub text: Color,
    /// Quieter text on the table.
    pub muted: Color,
    /// What a hint and a drop's landing place glow with.
    pub accent: Color,
    /// The status line's band: the baize's edge, deepened, and opaque, as
    /// it is the window's own ground.
    pub band: Color,
    /// The top of a card's face.
    pub face_top: Color,
    /// The bottom of a card's face.
    pub face_bottom: Color,
    /// A card's rim.
    pub edge: Color,
    /// Each suit's ink, in [`Suit::ALL`] order.
    pub suits: [Color; 4],
    /// A court card's panel, by suit.
    pub court: [Color; 4],
    /// The crown's light, middle and shade, and its outline.
    pub gold: [Color; 4],
    /// The back's light, middle and dark.
    pub back: [Color; 3],
    /// A resting card's shadow.
    pub shadow: Color,
    /// A lifted card's shadow.
    pub lifted: Color,
}

impl Palette {
    /// The colours `theme` and `prefs` call for.
    #[must_use]
    pub fn resolve(theme: &Theme, prefs: &Preferences) -> Self {
        let palette = theme.palette();
        let dark = theme.appearance() == Appearance::Dark;
        let (felt, felt_edge) = match prefs.baize {
            Baize::Green => (Color::rgb(0x1E, 0x6E, 0x40), Color::rgb(0x0E, 0x40, 0x23)),
            Baize::Slate => (Color::rgb(0x2F, 0x4E, 0x63), Color::rgb(0x16, 0x29, 0x36)),
            Baize::Claret => (Color::rgb(0x63, 0x22, 0x31), Color::rgb(0x33, 0x0F, 0x18)),
            Baize::Desktop => {
                let surface = Color::from(palette.surface);
                (surface, shade(surface, if dark { -14 } else { -22 }))
            }
        };
        let on_light = prefs.baize == Baize::Desktop && !dark;
        let (text, muted) = if on_light {
            (
                Color::from(palette.on_surface),
                Color::from(palette.on_surface_muted),
            )
        } else {
            (
                Color::rgba(255, 255, 255, 230),
                Color::rgba(255, 255, 255, 150),
            )
        };
        let red = Color::rgb(0xC0, 0x16, 0x2C);
        let ink = Color::rgb(0x18, 0x1A, 0x1E);
        let suits = if prefs.four_colours {
            [
                Color::rgb(0x12, 0x7A, 0x3A),
                Color::rgb(0x17, 0x5C, 0xC8),
                red,
                ink,
            ]
        } else {
            [ink, red, red, ink]
        };
        let court = if prefs.four_colours {
            [
                Color::rgb(0xEA, 0xF5, 0xEE),
                Color::rgb(0xEA, 0xF1, 0xFB),
                Color::rgb(0xFB, 0xEE, 0xF0),
                Color::rgb(0xEE, 0xF1, 0xF5),
            ]
        } else {
            let pale_red = Color::rgb(0xFB, 0xEE, 0xF0);
            let pale_ink = Color::rgb(0xEE, 0xF1, 0xF5);
            [pale_ink, pale_red, pale_red, pale_ink]
        };
        let back = match prefs.back {
            Back::Sapphire => [
                Color::rgb(0x45, 0x7C, 0xD8),
                Color::rgb(0x1F, 0x4F, 0xA3),
                Color::rgb(0x11, 0x2F, 0x69),
            ],
            Back::Crimson => [
                Color::rgb(0xCC, 0x3A, 0x50),
                Color::rgb(0x9C, 0x1C, 0x2E),
                Color::rgb(0x5E, 0x10, 0x1D),
            ],
            Back::Emerald => [
                Color::rgb(0x2B, 0xA7, 0x70),
                Color::rgb(0x12, 0x71, 0x4A),
                Color::rgb(0x0A, 0x45, 0x2D),
            ],
            Back::Graphite => [
                Color::rgb(0x66, 0x6F, 0x7C),
                Color::rgb(0x3A, 0x40, 0x49),
                Color::rgb(0x20, 0x24, 0x2A),
            ],
        };
        Self {
            felt,
            felt_edge,
            slot: Color::rgba(0, 0, 0, 40),
            slot_rim: if on_light {
                Color::rgba(0, 0, 0, 60)
            } else {
                Color::rgba(255, 255, 255, 56)
            },
            text,
            muted,
            accent: Color::from(palette.accent),
            band: deepen(felt_edge, if on_light { 18 } else { 56 }),
            face_top: Color::rgb(0xFF, 0xFF, 0xFF),
            face_bottom: Color::rgb(0xF2, 0xEF, 0xE7),
            edge: Color::rgba(0x20, 0x20, 0x24, 110),
            suits,
            court,
            gold: [
                Color::rgb(0xFA, 0xE1, 0x93),
                Color::rgb(0xE0, 0xB0, 0x48),
                Color::rgb(0xA6, 0x78, 0x1E),
                Color::rgb(0x6B, 0x4A, 0x10),
            ],
            back,
            shadow: Color::rgba(0, 0, 0, 120),
            lifted: Color::rgba(0, 0, 0, 96),
        }
    }

    /// `suit`'s ink.
    #[must_use]
    pub fn ink(&self, suit: Suit) -> Color {
        self.suits
            .get(suit.index())
            .copied()
            .unwrap_or(Color::rgb(0, 0, 0))
    }
}

/// `colour` darkened as `share` of 255 of black laid over it would, opaque.
fn deepen(colour: Color, share: u8) -> Color {
    let keep = |channel: u8| {
        u8::try_from(u16::from(channel) * u16::from(255 - share) / 255).unwrap_or(channel)
    };
    Color::rgb(keep(colour.r), keep(colour.g), keep(colour.b))
}

/// `colour` lightened (positive) or darkened (negative) by `by` levels.
#[must_use]
pub fn shade(colour: Color, by: i16) -> Color {
    let adjust = |channel: u8| {
        let value = i16::from(channel).saturating_add(by);
        u8::try_from(value.clamp(0, 255)).unwrap_or(channel)
    };
    Color::rgba(
        adjust(colour.r),
        adjust(colour.g),
        adjust(colour.b),
        colour.a,
    )
}

/// A shadow mask and where it lies from the card that casts it.
#[derive(Debug)]
pub struct Shadow {
    /// The mask, tinted when laid down.
    pub mask: Surface,
    /// Its origin from the card's.
    pub offset: (i32, i32),
}

/// The tiles one card size is drawn from.
#[derive(Debug)]
pub struct CardArt {
    width: u32,
    height: u32,
    radius: u32,
    hairline: u32,
    palette: Palette,
    faces: Vec<Option<Surface>>,
    back: Option<Surface>,
    rest: Option<Shadow>,
    lift: Option<Shadow>,
    baize: Option<Surface>,
}

impl CardArt {
    /// No tiles yet, for cards `width`×`height` with corners of `radius`.
    #[must_use]
    pub fn new(width: u32, height: u32, radius: u32, hairline: u32, palette: Palette) -> Self {
        Self {
            width,
            height,
            radius,
            hairline,
            palette,
            faces: (0..52).map(|_| None).collect(),
            back: None,
            rest: None,
            lift: None,
            baize: None,
        }
    }

    /// Adopt a card size or palette, forgetting every tile and the baize drawn
    /// for another.
    pub fn retune(
        &mut self,
        width: u32,
        height: u32,
        radius: u32,
        hairline: u32,
        palette: Palette,
    ) {
        let same = (
            self.width,
            self.height,
            self.radius,
            self.hairline,
            self.palette,
        ) == (width, height, radius, hairline, palette);
        if !same {
            *self = Self::new(width, height, radius, hairline, palette);
        }
    }

    /// Give every tile and the baize back, as memory pressure asks.
    pub fn trim(&mut self) {
        let (width, height, radius, hairline, palette) = (
            self.width,
            self.height,
            self.radius,
            self.hairline,
            self.palette,
        );
        *self = Self::new(width, height, radius, hairline, palette);
    }

    /// The colours the tiles are drawn in.
    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    /// `face`'s tile, drawn now if it is not yet.
    pub fn face(&mut self, face: Face) -> Option<&Surface> {
        let index = face.suit.index() * 13 + usize::from(face.rank.get() - 1);
        let missing = self.faces.get(index).is_some_and(Option::is_none);
        if missing {
            let tile = self.draw_face(face);
            if let Some(slot) = self.faces.get_mut(index) {
                *slot = tile;
            }
        }
        self.faces.get(index).and_then(Option::as_ref)
    }

    /// The back's tile.
    pub fn back(&mut self) -> Option<&Surface> {
        if self.back.is_none() {
            self.back = self.draw_back();
        }
        self.back.as_ref()
    }

    /// The baize `width`×`height`, shaded by `shade` when it is not yet: a
    /// repaint copies it rather than shading every pixel again.
    pub fn baize(
        &mut self,
        width: u32,
        height: u32,
        shade: impl FnOnce(&mut Surface),
    ) -> Option<&Surface> {
        let fits = self
            .baize
            .as_ref()
            .is_some_and(|baize| (baize.width(), baize.height()) == (width, height));
        if !fits {
            self.baize = Surface::new(width, height).map(|mut baize| {
                shade(&mut baize);
                baize
            });
        }
        self.baize.as_ref()
    }

    /// The shadow a resting card casts, or a lifted one.
    pub fn shadow(&mut self, lifted: bool) -> Option<&Shadow> {
        let slot = if lifted {
            &mut self.lift
        } else {
            &mut self.rest
        };
        if slot.is_none() {
            let (drop, radius) = if lifted {
                (self.height * 4 / 100, (self.height * 3 / 100).max(1))
            } else {
                (self.hairline, self.hairline.max(1))
            };
            *slot = card_shadow(self.width, self.height, self.radius, drop, radius);
        }
        slot.as_ref()
    }

    fn blank(&self) -> Option<Surface> {
        let mut tile = Surface::new(self.width, self.height)?;
        tile.fill_vertical_gradient(
            0,
            0,
            self.width,
            self.height,
            self.palette.face_top,
            self.palette.face_bottom,
        );
        Some(tile)
    }

    fn finish(&self, tile: &mut Surface) {
        tile.frame_ring(
            0,
            0,
            self.width,
            self.height,
            Ring::uniform(self.radius, self.hairline),
            self.palette.edge,
        );
    }

    fn draw_face(&self, face: Face) -> Option<Surface> {
        let mut tile = self.blank()?;
        let ink = self.palette.ink(face.suit);
        match face.rank.get() {
            1 => self.ace(&mut tile, face.suit, ink),
            11..=13 => self.court(&mut tile, face, ink),
            n => self.pips(&mut tile, face.suit, n, ink),
        }
        self.index(&mut tile, face, ink);
        self.finish(&mut tile);
        Some(tile)
    }

    /// The rank and suit in the top-left corner, and the same turned a half
    /// turn in the bottom-right, as a printed card carries them.
    fn index(&self, tile: &mut Surface, face: Face, ink: Color) {
        let (w, h) = (self.width, self.height);
        let column = (w * 20 / 100).max(4);
        let Some(mut corner) = Surface::new(column, (h * 28 / 100).max(4)) else {
            return;
        };
        let px = (h * 19 / 100).max(8);
        let font = BitmapFont::new(INDEX_FACE, px).with_weight(FontWeight::BOLD);
        let text = rank_text(face.rank);
        // A ten is two figures in a column built for one: it is set a little
        // smaller rather than run into the pips.
        let font = if text.len() > 1 {
            BitmapFont::new(INDEX_FACE, (px * 76 / 100).max(8)).with_weight(FontWeight::BOLD)
        } else {
            font
        };
        let centre = i32::try_from(column / 2).unwrap_or(0);
        let width = i32::try_from(font.text_width(text)).unwrap_or(0);
        let top = i32::try_from(h / 60).unwrap_or(0);
        font.draw_text(&mut corner, centre - width / 2, top, text, ink);
        let pip = (w * 12 / 100).max(3);
        let below = top + i32::try_from(font.line_height() * 84 / 100).unwrap_or(0);
        if let Some(mask) = suit_mask(face.suit, pip) {
            corner.blit_tinted(
                centre - i32::try_from(pip / 2).unwrap_or(0),
                below,
                &mask,
                ink,
            );
        }
        let inset = i32::try_from(w * 2 / 100).unwrap_or(0);
        tile.blit(inset, i32::try_from(h * 3 / 100).unwrap_or(0), &corner);
        if let Some(turned) = corner.reoriented(Reorient::HalfTurn) {
            let x = i32::try_from(w.saturating_sub(column)).unwrap_or(0) - inset;
            let y = i32::try_from(h.saturating_sub(turned.height())).unwrap_or(0)
                - i32::try_from(h * 3 / 100).unwrap_or(0);
            tile.blit(x, y, &turned);
        }
    }

    fn ace(&self, tile: &mut Surface, suit: Suit, ink: Color) {
        let side = if suit == Suit::Spades {
            self.width * 56 / 100
        } else {
            self.width * 46 / 100
        };
        let Some(mask) = suit_mask(suit, side) else {
            return;
        };
        let x = i32::try_from(self.width.saturating_sub(side) / 2).unwrap_or(0);
        let y = i32::try_from(self.height.saturating_sub(side) / 2).unwrap_or(0);
        if suit == Suit::Spades {
            // The ace of spades is the deck's signature card, so it carries a
            // ring the others do not.
            let ring = (side * 128 / 100).min(self.width);
            tile.wash_ring(
                self.width.saturating_sub(ring) / 2,
                self.height.saturating_sub(ring) / 2,
                ring,
                ring,
                Ring::uniform(ring / 2, self.hairline.max(1)),
                RingInk::Solid(Color::rgba(ink.r, ink.g, ink.b, 90)),
            );
        }
        tile.blit_tinted(x, y, &mask, ink);
    }

    /// The classic arrangements of two to ten, the lower half turned.
    fn pips(&self, tile: &mut Surface, suit: Suit, rank: u8, ink: Color) {
        let (w, h) = (self.width, self.height);
        let side = (w * 18 / 100).max(3);
        let Some(upright) = suit_mask(suit, side) else {
            return;
        };
        let turned = upright.reoriented(Reorient::HalfTurn);
        let columns = [w * 32 / 100, w / 2, w * 68 / 100];
        let (top, bottom) = (h * 21 / 100, h * 79 / 100);
        for &(column, row) in arrangement(rank) {
            let cx = columns.get(usize::from(column)).copied().unwrap_or(w / 2);
            let cy = top + (bottom - top) * u32::from(row) / 12;
            let mask = if row > 6 {
                turned.as_ref().unwrap_or(&upright)
            } else {
                &upright
            };
            let x = i32::try_from(cx).unwrap_or(0) - i32::try_from(side / 2).unwrap_or(0);
            let y = i32::try_from(cy).unwrap_or(0) - i32::try_from(side / 2).unwrap_or(0);
            tile.blit_tinted(x, y, mask, ink);
        }
    }

    /// A court card: a tinted panel holding the crown of the rank's own
    /// shape, the rank set large beneath it, and the suit. Set one way up,
    /// because a card on a screen is never turned round.
    fn court(&self, tile: &mut Surface, face: Face, ink: Color) {
        let (w, h) = (self.width, self.height);
        // Clear of the corner indices on either side.
        let (left, top) = (w * 235 / 1000, h * 80 / 1000);
        let (panel_w, panel_h) = (w.saturating_sub(left * 2), h.saturating_sub(top * 2));
        let tint = self
            .palette
            .court
            .get(face.suit.index())
            .copied()
            .unwrap_or(self.palette.face_top);
        let radius = (self.radius * 6 / 10).max(1);
        tile.fill_round_rect(left, top, panel_w, panel_h, radius, tint);
        tile.wash_ring(
            left,
            top,
            panel_w,
            panel_h,
            Ring::uniform(radius, self.hairline.max(1)),
            RingInk::Solid(Color::rgba(ink.r, ink.g, ink.b, 150)),
        );
        let at = |share: u32| i32::try_from(top + panel_h * share / 100).unwrap_or(0);
        let centre = i32::try_from(left + panel_w / 2).unwrap_or(0);
        let half = |extent: u32| i32::try_from(extent / 2).unwrap_or(0);

        let crown_w = panel_w * 76 / 100;
        let crown_h = crown_w * 62 / 100;
        self.crown(
            tile,
            face.rank,
            (centre - half(crown_w), at(10)),
            (crown_w, crown_h),
        );

        let letter =
            BitmapFont::new(INDEX_FACE, (panel_h * 38 / 100).max(8)).with_weight(FontWeight::BOLD);
        let text = rank_text(face.rank);
        let width = letter.text_width(text);
        letter.draw_text(tile, centre - half(width), at(31), text, ink);

        let pip = panel_w * 36 / 100;
        if let Some(mask) = suit_mask(face.suit, pip) {
            tile.blit_tinted(centre - half(pip), at(72), &mask, ink);
        }
    }

    fn crown(&self, onto: &mut Surface, rank: Rank, (x, y): (i32, i32), (w, h): (u32, u32)) {
        let contours = crown_contours(rank);
        let gold = self.palette.gold;
        // The outline first, the crown a little inside it, lit from above.
        if let Some(mut outline) = Surface::new(w, h) {
            if outline.fill_contours(&contours, DESIGN, FillRule::NonZero, &Paint::Solid(gold[3])) {
                onto.blit(x, y, &outline);
            }
        }
        let inset = (w / 28).max(1);
        let (iw, ih) = (w.saturating_sub(inset * 2), h.saturating_sub(inset * 2));
        let Some(mut body) = Surface::new(iw, ih) else {
            return;
        };
        let paint = Paint::Gradient(Gradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: gold[0],
                },
                GradientStop {
                    offset: 0.55,
                    color: gold[1],
                },
                GradientStop {
                    offset: 1.0,
                    color: gold[2],
                },
            ],
            spread: SpreadMethod::Pad,
            to_gradient: vertical(100.0, 900.0),
        });
        if body.fill_contours(&contours, DESIGN, FillRule::NonZero, &paint) {
            let offset = i32::try_from(inset).unwrap_or(0);
            onto.blit(x + offset, y + offset, &body);
        }
    }

    fn draw_back(&self) -> Option<Surface> {
        let (w, h) = (self.width, self.height);
        let mut tile = Surface::new(w, h)?;
        tile.fill(self.palette.face_top);
        let border = (w * 5 / 100).max(2);
        let (pw, ph) = (w.saturating_sub(border * 2), h.saturating_sub(border * 2));
        let [light, middle, dark] = self.palette.back;
        let mut panel = Surface::new(pw, ph)?;
        let ramp = Paint::Gradient(Gradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: shade(middle, 18),
                },
                GradientStop {
                    offset: 1.0,
                    color: dark,
                },
            ],
            spread: SpreadMethod::Pad,
            to_gradient: diagonal(),
        });
        let whole = vec![vec![(0, 0), (1000, 0), (1000, 1000), (0, 1000)]];
        let _ = panel.fill_contours(&whole, DESIGN, FillRule::NonZero, &ramp);
        lattice(&mut panel, (w * 9 / 100).max(4), self.hairline, light);
        let inset = (w * 4 / 100).max(2);
        panel.wash_ring(
            inset,
            inset,
            pw.saturating_sub(inset * 2),
            ph.saturating_sub(inset * 2),
            Ring::uniform((self.radius / 2).max(1), self.hairline.max(1)),
            RingInk::Solid(Color::rgba(255, 255, 255, 110)),
        );
        rosette(&mut panel, light);
        panel.mask_to_round_rect(0, 0, pw, ph, (self.radius * 6 / 10).max(1));
        let at = i32::try_from(border).unwrap_or(0);
        tile.blit(at, at, &panel);
        self.finish(&mut tile);
        Some(tile)
    }
}

/// A shadow cast by a card `width`×`height` with corners of `radius`.
fn card_shadow(width: u32, height: u32, radius: u32, drop: u32, blur: u32) -> Option<Shadow> {
    let mut shape = Surface::new(width, height)?;
    shape.fill_round_rect(0, 0, width, height, radius, Color::rgb(0, 0, 0));
    let (mask, offset) = cast_shadow(
        &shape,
        ShadowCast {
            drop_x: 0,
            drop_y: drop,
            radius: blur,
        },
    )?;
    Some(Shadow { mask, offset })
}

/// Diagonal lines both ways across `panel`, `step` apart.
fn lattice(panel: &mut Surface, step: u32, hairline: u32, light: Color) {
    let (w, h) = (
        i32::try_from(panel.width()).unwrap_or(0),
        i32::try_from(panel.height()).unwrap_or(0),
    );
    let step = i32::try_from(step.max(2)).unwrap_or(2);
    let ink = Color::rgba(light.r, light.g, light.b, 70);
    let weight = i32::try_from(hairline.max(1)).unwrap_or(1) * SUBPIXEL;
    let sub = |x: i32, y: i32| (x * SUBPIXEL, y * SUBPIXEL);
    let mut offset = -h;
    while offset < w {
        panel.stroke_polyline(&[sub(offset, 0), sub(offset + h, h)], weight, ink);
        panel.stroke_polyline(&[sub(offset + h, 0), sub(offset, h)], weight, ink);
        offset += step;
    }
}

/// The transform taking the design grid's `top`..`bottom` to a ramp's
/// `0..1`, top to bottom.
fn vertical(top: f64, bottom: f64) -> Affine {
    let span = bottom - top;
    Affine {
        a: 0.0,
        b: 0.0,
        c: 1.0 / span,
        d: 1.0,
        e: -top / span,
        f: 0.0,
    }
}

/// The transform taking the design grid's top-left corner to a ramp's `0`
/// and its bottom-right to `1`.
fn diagonal() -> Affine {
    Affine {
        a: 1.0 / 2000.0,
        b: 0.0,
        c: 1.0 / 2000.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    }
}

/// How a rank is printed.
#[must_use]
pub const fn rank_text(rank: Rank) -> &'static str {
    match rank.get() {
        1 => "A",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        9 => "9",
        10 => "10",
        11 => "J",
        12 => "Q",
        _ => "K",
    }
}

/// Where a rank's pips lie: a column of three (left, centre, right) and a row
/// in twelfths of the pip field, top to bottom.
fn arrangement(rank: u8) -> &'static [(u8, u8)] {
    match rank {
        2 => &[(1, 0), (1, 12)],
        3 => &[(1, 0), (1, 6), (1, 12)],
        4 => &[(0, 0), (2, 0), (0, 12), (2, 12)],
        5 => &[(0, 0), (2, 0), (1, 6), (0, 12), (2, 12)],
        6 => &[(0, 0), (2, 0), (0, 6), (2, 6), (0, 12), (2, 12)],
        7 => &[(0, 0), (2, 0), (1, 3), (0, 6), (2, 6), (0, 12), (2, 12)],
        8 => &[
            (0, 0),
            (2, 0),
            (1, 3),
            (0, 6),
            (2, 6),
            (1, 9),
            (0, 12),
            (2, 12),
        ],
        9 => &[
            (0, 0),
            (2, 0),
            (0, 4),
            (2, 4),
            (1, 6),
            (0, 8),
            (2, 8),
            (0, 12),
            (2, 12),
        ],
        _ => &[
            (0, 0),
            (2, 0),
            (1, 2),
            (0, 4),
            (2, 4),
            (0, 8),
            (2, 8),
            (1, 10),
            (0, 12),
            (2, 12),
        ],
    }
}

/// `suit`'s shape as a coverage mask `side` pixels square.
#[must_use]
pub fn suit_mask(suit: Suit, side: u32) -> Option<Surface> {
    let mut mask = Surface::new(side.max(1), side.max(1))?;
    let painted = mask.fill_contours(
        &suit_contours(suit),
        DESIGN,
        FillRule::NonZero,
        &Paint::Solid(Color::rgb(255, 255, 255)),
    );
    painted.then_some(mask)
}

/// `suit`'s outline on the design grid.
#[must_use]
pub fn suit_contours(suit: Suit) -> Vec<Vec<(i32, i32)>> {
    match suit {
        Suit::Hearts => vec![heart(false)],
        Suit::Spades => {
            // Turned point up the curve runs the other way round, and every
            // outline must wind alike for the non-zero rule to unite them.
            let mut body = heart(true);
            body.reverse();
            vec![body, stem()]
        }
        Suit::Diamonds => vec![diamond()],
        Suit::Clubs => vec![
            circle(500.0, 300.0, 215.0, 40),
            circle(285.0, 575.0, 215.0, 40),
            circle(715.0, 575.0, 215.0, 40),
            vec![(500, 320), (670, 600), (330, 600)],
            stem(),
        ],
    }
}

/// The classic heart curve, filling the grid, point down; turned point up
/// it is the body of a spade.
fn heart(point_up: bool) -> Vec<(i32, i32)> {
    const STEPS: u32 = 72;
    (0..STEPS)
        .map(|step| {
            let t = 2.0 * core::f64::consts::PI * f64::from(step) / f64::from(STEPS);
            let s = mathf::sin(t);
            let x = 16.0 * s * s * s;
            let y = 13.0 * mathf::cos(t)
                - 5.0 * mathf::cos(2.0 * t)
                - 2.0 * mathf::cos(3.0 * t)
                - mathf::cos(4.0 * t);
            // The curve spans x ±16 and y −17..12; map it into the grid with
            // a little room, y downwards.
            let gx = 500.0 + x * 29.5;
            let gy = if point_up {
                // A spade's body sits high, leaving room for its stem.
                80.0 + (y + 17.0) * 23.5
            } else {
                40.0 + (12.0 - y) * 31.5
            };
            (mathf::round_i32(gx), mathf::round_i32(gy))
        })
        .collect()
}

/// The four suits about the centre of the back.
fn rosette(panel: &mut Surface, light: Color) {
    let (pw, ph) = (panel.width(), panel.height());
    let side = (pw * 17 / 100).max(3);
    let reach = i32::try_from(side * 95 / 100).unwrap_or(0);
    let (cx, cy) = (
        i32::try_from(pw / 2).unwrap_or(0),
        i32::try_from(ph / 2).unwrap_or(0),
    );
    let disc = side * 33 / 10;
    if let Some(mut plate) = Surface::new(disc, disc) {
        let round = vec![circle(500.0, 500.0, 480.0, 48)];
        let shadow = Paint::Solid(Color::rgba(0, 0, 0, 60));
        if plate.fill_contours(&round, DESIGN, FillRule::NonZero, &shadow) {
            let half = i32::try_from(disc / 2).unwrap_or(0);
            panel.blit(cx - half, cy - half, &plate);
        }
    }
    let tint = Color::rgba(light.r, light.g, light.b, 235);
    for (suit, (dx, dy)) in Suit::ALL
        .into_iter()
        .zip([(0, -1), (1, 0), (0, 1), (-1, 0)])
    {
        if let Some(mask) = suit_mask(suit, side) {
            let half = i32::try_from(side / 2).unwrap_or(0);
            panel.blit_tinted(cx + dx * reach - half, cy + dy * reach - half, &mask, tint);
        }
    }
}

/// A spade's or club's stem: a flare from the body to a broad foot.
fn stem() -> Vec<(i32, i32)> {
    const STEPS: u32 = 8;
    let mut points = Vec::with_capacity(20);
    for step in 0..=STEPS {
        let t = f64::from(step) / f64::from(STEPS);
        // Concave sides: the stem narrows quickly under the body and flares
        // out to its foot.
        let half = 34.0 + 200.0 * t * t;
        points.push((
            mathf::round_i32(500.0 + half),
            mathf::round_i32(620.0 + 340.0 * t),
        ));
    }
    for step in (0..=STEPS).rev() {
        let t = f64::from(step) / f64::from(STEPS);
        let half = 34.0 + 200.0 * t * t;
        points.push((
            mathf::round_i32(500.0 - half),
            mathf::round_i32(620.0 + 340.0 * t),
        ));
    }
    points
}

/// A diamond with gently concave sides, as cards print it.
fn diamond() -> Vec<(i32, i32)> {
    let corners = [
        (500.0, 20.0),
        (860.0, 500.0),
        (500.0, 980.0),
        (140.0, 500.0),
    ];
    let mut points = Vec::with_capacity(48);
    for (index, &(x0, y0)) in corners.iter().enumerate() {
        let (x1, y1) = corners[(index + 1) % corners.len()];
        // Each side bows towards the centre through a quadratic's control
        // point pulled in from the side's middle.
        let (mx, my) = (f64::midpoint(x0, x1), f64::midpoint(y0, y1));
        let (cx, cy) = (mx + (500.0 - mx) * 0.16, my + (500.0 - my) * 0.16);
        for step in 0..12 {
            let t = f64::from(step) / 12.0;
            let u = 1.0 - t;
            let x = u * u * x0 + 2.0 * u * t * cx + t * t * x1;
            let y = u * u * y0 + 2.0 * u * t * cy + t * t * y1;
            points.push((mathf::round_i32(x), mathf::round_i32(y)));
        }
    }
    points
}

/// A circle on the grid, clockwise as every other outline here, so the
/// non-zero rule unites overlapping parts.
fn circle(cx: f64, cy: f64, r: f64, steps: u32) -> Vec<(i32, i32)> {
    (0..steps)
        .map(|step| {
            let t = 2.0 * core::f64::consts::PI * f64::from(step) / f64::from(steps);
            (
                mathf::round_i32(cx + r * mathf::cos(t)),
                mathf::round_i32(cy + r * mathf::sin(t)),
            )
        })
        .collect()
}

/// The crown each court rank wears: the king's three tall points, the
/// queen's four slender ones, the jack's low coronet.
fn crown_contours(rank: Rank) -> Vec<Vec<(i32, i32)>> {
    match rank.get() {
        13 => vec![
            vec![
                (120, 700),
                (120, 380),
                (310, 560),
                (500, 160),
                (690, 560),
                (880, 380),
                (880, 700),
            ],
            vec![(110, 700), (890, 700), (890, 900), (110, 900)],
            circle(120.0, 330.0, 70.0, 24),
            circle(500.0, 110.0, 75.0, 24),
            circle(880.0, 330.0, 70.0, 24),
        ],
        12 => vec![
            vec![
                (140, 720),
                (140, 430),
                (270, 590),
                (380, 330),
                (500, 560),
                (620, 330),
                (730, 590),
                (860, 430),
                (860, 720),
            ],
            vec![(130, 720), (870, 720), (870, 880), (130, 880)],
            circle(140.0, 395.0, 55.0, 20),
            circle(380.0, 290.0, 55.0, 20),
            circle(620.0, 290.0, 55.0, 20),
            circle(860.0, 395.0, 55.0, 20),
        ],
        _ => vec![
            vec![
                (170, 660),
                (170, 480),
                (335, 610),
                (500, 400),
                (665, 610),
                (830, 480),
                (830, 660),
            ],
            vec![(160, 660), (840, 660), (840, 860), (160, 860)],
            vec![(500, 260), (580, 350), (500, 440), (420, 350)],
        ],
    }
}

#[cfg(test)]
#[path = "art_tests.rs"]
mod tests;
