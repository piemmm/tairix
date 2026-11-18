//! The status band along a window's foot.
//!
//! A band carries readings — where the pointer is, what is selected, the
//! zoom, a lamp lit while a drive works — in slots each as wide as the most it
//! ever reads, so the band never shifts as they change, and a message in the
//! room between them. Every application's band is this one: its height, how
//! it is cut into slots, how its text is set and toned, and how a lamp and a
//! button sit in it, so the bands of two windows side by side read alike.

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::paint::{fill_area, paint_run, role_font};
use crate::Button;

/// The room a band keeps above and below a button it holds, in logical
/// pixels.
const BUTTON_INSET: u32 = 2;

/// How a run of a status band's text reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StatusTone {
    /// What the band reports: a reading, or a message.
    Ink,
    /// What stands in where there is nothing to report.
    Muted,
    /// A check that found nothing wrong.
    Success,
    /// A condition worth knowing of.
    Warning,
    /// A failure.
    Danger,
}

impl StatusTone {
    fn color(self, theme: &Theme) -> Color {
        let palette = theme.palette();
        Color::from(match self {
            Self::Ink => palette.on_surface,
            Self::Muted => palette.on_surface_muted,
            Self::Success => palette.success,
            Self::Warning => palette.warning,
            Self::Danger => palette.danger,
        })
    }
}

/// A status band's measures in a theme at a scale: the face its text is set
/// in and the gap above and below its line and beside each slot.
#[derive(Copy, Clone, Debug)]
pub struct StatusBand {
    face: BitmapFont,
    gap: u32,
}

/// A lamp at the leading edge of a band's slot, lit or not, and half its
/// side before the slot's text: from [`StatusBand::lamp`].
#[derive(Copy, Clone, Debug)]
pub struct StatusLamp {
    band: StatusBand,
    /// The first row of the line box it fills.
    first: u32,
    side: u32,
}

impl StatusLamp {
    /// What it takes at its slot's leading edge.
    const fn width(&self) -> u32 {
        self.side + self.side / 2
    }

    /// The width of a slot holding the lamp, then at most `widest`.
    #[must_use]
    pub fn slot_width(&self, widest: &str) -> u32 {
        self.width() + self.band.slot_width(widest)
    }

    /// Where the text of the lamp's slot `slot` lies: past the lamp.
    #[must_use]
    pub fn text(&self, slot: Rect) -> Rect {
        let lamp = self.width().min(slot.width);
        Rect::new(
            slot.left().saturating_add_unsigned(lamp),
            slot.top(),
            slot.width - lamp,
            slot.height,
        )
    }

    /// Paint the lamp toned `tone` at `slot`'s leading edge, beside the
    /// capitals of the slot's text on the band's line.
    pub fn paint(&self, surface: &mut Surface, slot: Rect, theme: &Theme, tone: StatusTone) {
        let side = self.side.min(slot.width).min(slot.height);
        let top = self
            .band
            .face
            .centred_top(slot.top(), slot.height)
            .saturating_add_unsigned(self.first);
        if let (true, Ok(x), Ok(y)) = (side > 0, u32::try_from(slot.left()), u32::try_from(top)) {
            surface.fill_round_rect(x, y, side, side, side / 2, tone.color(theme));
        }
    }
}

/// A band cut into its slots: `L` taken from its leading edge and `T` from
/// its trailing edge, the first of each outermost, and the message in the
/// room left between.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StatusSlots<const L: usize, const T: usize> {
    /// The leading slots, from the band's leading edge inward.
    pub leading: [Rect; L],
    /// The message.
    pub message: Rect,
    /// The trailing slots, from the band's trailing edge inward.
    pub trailing: [Rect; T],
}

impl StatusBand {
    /// The band in `theme` at `scale`.
    #[must_use]
    pub fn new(theme: &Theme, scale: Scale) -> Self {
        Self {
            face: role_font(theme, scale, TextRole::Caption),
            gap: scale.scale_length(theme.metrics().control_gap).max(1),
        }
    }

    /// The face its text is set in.
    #[must_use]
    pub const fn face(&self) -> BitmapFont {
        self.face
    }

    /// Its height: a line of its face and a gap above and below.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.face.line_height().max(1) + self.gap * 2
    }

    /// The width of a slot holding at most `widest`.
    #[must_use]
    pub fn slot_width(&self, widest: &str) -> u32 {
        self.face.text_width(widest) + self.gap * 2
    }

    /// `band` cut into slots `leading` and `trailing` wide, inside a gap at
    /// either end; slots the band has no room for are empty.
    #[must_use]
    pub fn slots<const L: usize, const T: usize>(
        &self,
        band: Rect,
        leading: [u32; L],
        trailing: [u32; T],
    ) -> StatusSlots<L, T> {
        let mut rest = Rect::new(
            band.left().saturating_add_unsigned(self.gap),
            band.top(),
            band.width.saturating_sub(self.gap * 2),
            band.height,
        );
        let trailing = trailing.map(|width| rest.take_right(width));
        let leading = leading.map(|width| rest.take_left(width));
        StatusSlots {
            leading,
            message: rest,
            trailing,
        }
    }

    /// The height of a band holding a button: its own, or a button's with
    /// room above and below it where that is more.
    #[must_use]
    pub fn height_holding_button(&self, scale: Scale, theme: &Theme) -> u32 {
        let button = Button::height(scale, theme)
            .saturating_add(scale.scale_length(BUTTON_INSET).saturating_mul(2));
        self.height().max(button)
    }

    /// Where a button lies in `slot`: as tall as a button, centred on the
    /// band's line.
    #[must_use]
    pub fn button_in(slot: Rect, scale: Scale, theme: &Theme) -> Rect {
        let height = Button::height(scale, theme).min(slot.height);
        Rect::new(
            slot.left(),
            slot.top()
                .saturating_add_unsigned((slot.height - height) / 2),
            slot.width,
            height,
        )
    }

    /// The lamp its slots hold, measured once for the slots and the paint
    /// that use it: a capital's height, standing where the capitals stand, so
    /// it reads as part of the word beside it. A face reporting no capital
    /// has it two thirds of the line, centred.
    #[must_use]
    pub fn lamp(&self) -> StatusLamp {
        let (first, side) = if let Some((first, last)) = self.face.capital_rows() {
            (first, (last - first + 1).max(2))
        } else {
            let side = (self.face.line_height() * 2 / 3).max(2);
            (self.face.glyph_height().saturating_sub(side) / 2, side)
        };
        StatusLamp {
            band: *self,
            first,
            side,
        }
    }

    /// Paint `band`: its plate, then each part's text, toned, at its slot's
    /// leading edge on the band's one line, elided where its slot is too
    /// narrow for it.
    pub fn paint(
        &self,
        surface: &mut Surface,
        band: Rect,
        theme: &Theme,
        parts: &[(Rect, &str, StatusTone)],
    ) {
        fill_area(surface, band, Color::from(theme.palette().surface_raised));
        let top = self.face.centred_top(band.top(), band.height);
        for &(slot, text, tone) in parts {
            if text.is_empty() || slot.is_empty() {
                continue;
            }
            let run = self.face.elide_to_width(text, slot.width);
            paint_run(
                surface,
                self.face,
                run,
                (slot.left(), top),
                tone.color(theme),
                None,
            );
        }
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
