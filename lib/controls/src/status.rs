//! The status band along a window's foot.
//!
//! A band carries readings — where the pointer is, what is selected, the
//! zoom — in slots each as wide as the most it ever reads, so the band never
//! shifts as they change, and a message in the room between them. Every
//! application's band is this one: its height, how it is cut into slots, and
//! how its text is set and toned, so the bands of two windows side by side
//! read alike.

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::paint::{fill_area, paint_run, role_font};

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
