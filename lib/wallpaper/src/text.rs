//! The desktop's text as the user chose it — a family and a size in points,
//! each the theme's own until chosen — and the one resolution of that choice
//! against the font store into the text every application draws.

use alloc::string::{String, ToString};
use core::fmt::Write as _;

use tairix_abi::font_ipc::FamilyEntry;
use tairix_theme::{line_box_px, points_of, DesktopText, FamilyKey, Fonts};

use crate::input::parse_decimal;

/// The smallest size in points the `font.size` key holds.
pub const TEXT_POINTS_MIN: u16 = 6;

/// The largest size in points the `font.size` key holds.
pub const TEXT_POINTS_MAX: u16 = 48;

/// The spelling both keys give the theme's own value.
const THEME_VALUE: &str = "theme";

/// The family the desktop's interface text is drawn in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextFamily {
    /// The theme's own family.
    #[default]
    Theme,
    /// An installed family, by its key. Whether the store holds it is asked
    /// where it is resolved, since a choice outlives the image that shipped
    /// the family.
    Named(FamilyKey),
}

impl TextFamily {
    /// Decode the `theme` keyword or a family key; `None` for anything else.
    pub(crate) fn from_value(value: &str) -> Option<Self> {
        if value == THEME_VALUE {
            return Some(Self::Theme);
        }
        FamilyKey::new(value).ok().map(Self::Named)
    }

    pub(crate) fn render_value(self) -> String {
        match self {
            Self::Theme => THEME_VALUE.to_string(),
            Self::Named(key) => key.as_str().to_string(),
        }
    }
}

/// The desktop's body size.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextSize {
    /// The theme's own size.
    #[default]
    Theme,
    /// A size in points of em, within
    /// [`TEXT_POINTS_MIN`]`..=`[`TEXT_POINTS_MAX`].
    Points(u16),
}

impl TextSize {
    /// The size of `points` points, or `None` outside the bounds the key
    /// holds.
    #[must_use]
    pub fn points(points: u16) -> Option<Self> {
        (TEXT_POINTS_MIN..=TEXT_POINTS_MAX)
            .contains(&points)
            .then_some(Self::Points(points))
    }

    /// Decode the `theme` keyword or a bare decimal size in points; `None`
    /// for anything else, including a size outside the bounds — refused, not
    /// clamped, since a clamped value is one nobody chose.
    pub(crate) fn from_value(value: &str) -> Option<Self> {
        if value == THEME_VALUE {
            return Some(Self::Theme);
        }
        Self::points(u16::try_from(parse_decimal(value)?).ok()?)
    }

    pub(crate) fn render_value(self) -> String {
        match self {
            Self::Theme => THEME_VALUE.to_string(),
            Self::Points(points) => {
                let mut out = String::new();
                let _ = write!(out, "{points}");
                out
            }
        }
    }
}

/// What a text choice came to against the store.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedText {
    /// The text to draw, or `None` where each theme draws its own.
    pub text: Option<DesktopText>,
    /// A chosen family the store does not offer, which fell back to the
    /// theme's: said where it is resolved, never drawn as nothing.
    pub unknown: Option<FamilyKey>,
}

/// The text `family` at `size` comes to over `theme`'s own fonts, with
/// `offered` the families the store lists.
///
/// A size in points becomes a line-box height through the drawing family's
/// own line box, so a change of family keeps the glyphs the size they were.
/// Where the store lists neither the drawing family nor the theme's — no font
/// service, which draws nothing anyway — or where the choice comes to the
/// theme's own text, the answer is `None`: there is nothing to lay over.
#[must_use]
pub fn resolve(
    family: TextFamily,
    size: TextSize,
    theme: &Fonts,
    offered: &[FamilyEntry],
) -> ResolvedText {
    let line_box = |key: FamilyKey| {
        offered
            .iter()
            .find(|entry| entry.key == key)
            .map(FamilyEntry::line_box)
    };
    let (drawn, unknown) = match family {
        TextFamily::Theme => (theme.ui_family(), None),
        TextFamily::Named(key) if line_box(key).is_some() => (key, None),
        TextFamily::Named(key) => (theme.ui_family(), Some(key)),
    };
    let own = theme.base_size_px();
    let px = match size {
        TextSize::Points(points) => line_box(drawn).map(|drawn_box| line_box_px(points, drawn_box)),
        TextSize::Theme if drawn == theme.ui_family() => Some(own),
        // The theme's size in points, drawn in another family's line box;
        // where either box is unlisted, the theme's own line box stands.
        TextSize::Theme => Some(
            line_box(theme.ui_family())
                .zip(line_box(drawn))
                .map_or(own, |(theme_box, drawn_box)| {
                    line_box_px(points_of(own, theme_box), drawn_box)
                }),
        ),
    };
    let text = px
        .filter(|&px| drawn != theme.ui_family() || px != own)
        .and_then(|px| DesktopText::new(drawn, px).ok());
    ResolvedText { text, unknown }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
