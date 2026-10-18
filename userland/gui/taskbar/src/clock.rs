//! The taskbar clock: the time and date it shows, and the room it keeps.
//!
//! [`Clock`] holds its lines already written: reading an instant in a zone and
//! writing it in a region's conventions is the caller's concern, so the bar
//! carries no time ABI of its own. An empty time draws nothing.
//!
//! The clock's own menu is how a time gets *set*, so a machine with no wall
//! time must still show a clock to reach it by: [`UNSET_LABEL`] is what a
//! caller shows then, and the menu reads the same constant to say the time is
//! unset.
//!
//! The room the clock keeps is measured from the widest each line can be in
//! its conventions, stated by the caller when they change
//! ([`Clock::set_widest`]), with every digit as wide as the widest: the clock
//! holds still as the minutes and days turn.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_theme::{TextRole, Theme};

use crate::edge::Orientation;

/// The role the time is set in: the body face rather than a caption's,
/// because the time is read at a glance from across the bar.
pub(crate) const CLOCK_ROLE: TextRole = TextRole::Body;

/// The role the date is set in, beneath or beside the time.
pub(crate) const DATE_ROLE: TextRole = TextRole::Caption;

/// The label a caller sets when no wall-clock time has been established.
///
/// `HH:mm`-shaped, so the clock keeps its place and its width, and
/// unmistakably not a reading. The one definition of that spelling: the
/// session spells an unset reading as this, and
/// [`clock_menu`](crate::clock_menu) reads it back to state that the time is
/// unset.
pub const UNSET_LABEL: &str = "--:--";

/// The two lines a clock shows, each already written in its reader's
/// conventions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClockLines {
    /// The time of day.
    pub time: String,
    /// The date; empty for none.
    pub date: String,
}

/// The clock's display.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Clock {
    lines: ClockLines,
    heading: String,
    widest: Vec<ClockLines>,
    /// Changed with `widest`, so a room measured from it is known stale.
    shape: u64,
}

impl Clock {
    /// A clock with nothing to show yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: ClockLines {
                time: String::new(),
                date: String::new(),
            },
            heading: String::new(),
            widest: Vec::new(),
            shape: 0,
        }
    }

    /// The lines it shows.
    #[must_use]
    pub const fn lines(&self) -> &ClockLines {
        &self.lines
    }

    /// The reading as the clock's menu states it.
    #[must_use]
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// Show `lines`, stated in the clock's menu as `heading`.
    pub fn set_reading(&mut self, lines: ClockLines, heading: impl Into<String>) {
        self.lines = lines;
        self.heading = heading.into();
    }

    /// The lines it keeps room for.
    #[must_use]
    pub fn widest(&self) -> &[ClockLines] {
        &self.widest
    }

    /// Keep room for each of `samples`: between them, every line at its
    /// widest in the conventions it is written in.
    pub fn set_widest(&mut self, samples: Vec<ClockLines>) {
        if samples != self.widest {
            self.widest = samples;
            self.shape = self.shape.wrapping_add(1);
        }
    }

    /// What the room is measured from, and which measurement it is.
    pub(crate) fn shape(&self) -> (&[ClockLines], u64) {
        (&self.widest, self.shape)
    }
}

/// The faces and spacing a clock is set in at one scale.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClockFaces {
    /// The time's face.
    pub time: BitmapFont,
    /// The date's face.
    pub date: BitmapFont,
    /// The room kept either side of the lines.
    pub pad: u32,
    /// The room between the lines when they stand side by side.
    pub gap: u32,
}

impl ClockFaces {
    /// The clock's faces in `theme` at `scale`.
    #[must_use]
    pub(crate) fn of(theme: &Theme, scale: Scale) -> Self {
        let metrics = theme.metrics();
        Self {
            time: BitmapFont::for_role(theme.fonts(), CLOCK_ROLE, scale),
            date: BitmapFont::for_role(theme.fonts(), DATE_ROLE, scale),
            pad: scale.scale_length(metrics.control_inset),
            gap: scale.scale_length(metrics.control_gap),
        }
    }

    /// Whether the lines stand one beneath the other in a region `height`
    /// tall, rather than side by side: the one rule the room and the drawing
    /// both follow.
    #[must_use]
    pub(crate) fn stacks(self, height: u32) -> bool {
        height >= self.stacked_height()
    }

    /// The height the two lines take one beneath the other.
    fn stacked_height(self) -> u32 {
        self.time
            .line_height()
            .saturating_add(self.date.line_height())
    }
}

/// The room along a bar of `orientation` its clock needs, in a region `cross`
/// deep across it, to show any of `samples` whole: on a horizontal bar the
/// width the lines take stacked or side by side, on a vertical one the height
/// they take stacked.
#[must_use]
pub(crate) fn clock_room(
    faces: ClockFaces,
    samples: &[ClockLines],
    orientation: Orientation,
    cross: u32,
) -> u32 {
    let padded = |content: u32| content.saturating_add(faces.pad.saturating_mul(2));
    if orientation == Orientation::Vertical {
        return padded(faces.stacked_height());
    }
    // Any time holds an `HH:mm` reading and the unset placeholder too.
    let time = samples
        .iter()
        .map(|sample| widest_width(faces.time, &sample.time))
        .chain([
            widest_width(faces.time, "00:00"),
            faces.time.text_width(UNSET_LABEL),
        ])
        .max()
        .unwrap_or(0);
    let date = samples
        .iter()
        .map(|sample| widest_width(faces.date, &sample.date))
        .max()
        .unwrap_or(0);
    let content = if date == 0 {
        time
    } else if faces.stacks(cross) {
        time.max(date)
    } else {
        time.saturating_add(faces.gap).saturating_add(date)
    };
    padded(content)
}

/// `text`'s width in `font` with every digit as wide as the widest.
fn widest_width(font: BitmapFont, text: &str) -> u32 {
    let widest_digit = ('0'..='9')
        .max_by_key(|digit| font.advance(*digit))
        .unwrap_or('0');
    let shaped: String = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_digit() {
                widest_digit
            } else {
                ch
            }
        })
        .collect();
    font.text_width(&shaped)
}
