//! The taskbar clock label.
//!
//! [`Clock`] holds the *rendered* clock text shown at the trailing end of the
//! bar. Formatting a `Time64` value into a label (and the locale and
//! 12/24-hour policy that drives it) is a caller concern: the taskbar stores
//! only the string to draw, so the bar carries no time ABI of its own
//! (keeps absolute time in `lib/abi`). An empty label draws
//! nothing.
//!
//! The clock's own menu is how a time gets *set*, so a machine with no
//! wall time must still show a clock to reach it by: [`UNSET_LABEL`] is what
//! a caller draws then, and the menu's heading reads the same constant to say
//! the time is unset.

use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_theme::{TextRole, Theme};

/// The role the clock's label is set in: the body face rather than a
/// caption's, because the time is read at a glance from across the bar.
pub(crate) const CLOCK_ROLE: TextRole = TextRole::Body;

/// The label a caller sets when no wall-clock time has been established.
///
/// `HH:mm`-shaped, so the clock keeps its place and its width, and
/// unmistakably not a reading. The one definition of that spelling: the
/// session spells an unset reading as this, and
/// [`clock_menu`](crate::clock_menu) reads it back to state that the time is
/// unset.
pub const UNSET_LABEL: &str = "--:--";

/// The clock's display label.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Clock {
    label: String,
}

/// What the clock's room at `scale` is measured from: the face its label is
/// set in and the padding either side of it.
#[must_use]
pub(crate) fn clock_room_key(theme: &Theme, scale: Scale) -> (BitmapFont, u32) {
    (
        BitmapFont::for_role(theme.fonts(), CLOCK_ROLE, scale),
        scale.scale_length(theme.metrics().control_inset),
    )
}

/// The room along a horizontal bar its clock needs: the widest label it can
/// be given in `font` — an `HH:mm` reading with every digit as wide as the
/// widest, or [`UNSET_LABEL`] — with `pad` either side, so a region this long
/// shows whatever minute it is whole.
#[must_use]
pub(crate) fn clock_room((font, pad): (BitmapFont, u32)) -> u32 {
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let digit = DIGITS
        .iter()
        .map(|digit| font.text_width(digit))
        .max()
        .unwrap_or(0);
    let reading = digit.saturating_mul(4).saturating_add(font.text_width(":"));
    reading
        .max(font.text_width(UNSET_LABEL))
        .saturating_add(pad.saturating_mul(2))
}

impl Clock {
    /// A clock with no label yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            label: String::new(),
        }
    }

    /// The current display label (empty until set).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Replace the display label with the caller's already-formatted text.
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }
}
