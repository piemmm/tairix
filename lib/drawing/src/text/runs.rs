//! Runs: spans of a text's content set otherwise than its own style, and the
//! edits that keep them following the content.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::{try_arc, SharedSlice, SharedStr};
use tairix_colour::Rgba;
use tairix_util::fallible;

use super::Text;
use crate::limits::{self, MAX_BASELINE_SHIFT, MAX_FAMILY_BYTES, MAX_TEXT_FAMILIES, MAX_TEXT_RUNS};
use crate::style::Paint;
use crate::DrawingError;

/// What a run paints its glyphs' fill or stroke with in place of the
/// object's. A gradient paints a whole object, never a run.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RunPaint {
    /// Nothing.
    None,
    /// One colour, its alpha the paint's opacity.
    Colour(Rgba),
}

impl RunPaint {
    /// The paint it is.
    #[must_use]
    pub const fn paint(self) -> Paint {
        match self {
            Self::None => Paint::None,
            Self::Colour(colour) => Paint::Colour(colour),
        }
    }

    /// `paint` as a run may hold it: `None` for a gradient.
    #[must_use]
    pub const fn of(paint: &Paint) -> Option<Self> {
        match paint {
            Paint::None => Some(Self::None),
            Paint::Colour(colour) => Some(Self::Colour(*colour)),
            Paint::Gradient(_) => None,
        }
    }
}

/// What a run overrides of its text's own style; `None` is the text's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunStyle {
    /// The font families asked for, in order.
    pub family: Option<SharedStr>,
    /// The font size, in the object's units.
    pub size: Option<f64>,
    /// The weight, 1–1000.
    pub weight: Option<u16>,
    /// Whether it is set italic.
    pub italic: Option<bool>,
    /// Extra space after each character, in the object's units.
    pub letter_spacing: Option<f64>,
    /// How far its glyphs are raised above the baseline, in ems of its own
    /// size; negative lowers them.
    pub baseline_shift: Option<f64>,
    /// What fills its glyphs.
    pub fill: Option<RunPaint>,
    /// What strokes its glyphs, at the object's stroke width.
    pub stroke: Option<RunPaint>,
}

impl RunStyle {
    /// Whether it overrides nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether every value it overrides is one a drawing may hold.
    fn holds(&self) -> bool {
        let family = self.family.as_ref().is_none_or(|family| {
            family.len() <= MAX_FAMILY_BYTES && !family.chars().any(char::is_control)
        });
        family
            && self
                .size
                .is_none_or(|size| limits::length(size) && size > 0.0)
            && self
                .weight
                .is_none_or(|weight| (1..=1000).contains(&weight))
            && self.letter_spacing.is_none_or(limits::coordinate)
            && self
                .baseline_shift
                .is_none_or(|shift| shift.is_finite() && shift.abs() <= MAX_BASELINE_SHIFT)
    }
}

/// A span of a text's content and how it is set.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Where it starts in the content, in bytes, on a character boundary.
    pub start: usize,
    /// Where it ends, past its last byte, on a character boundary.
    pub end: usize,
    /// What it overrides.
    pub style: Arc<RunStyle>,
}

impl Run {
    /// Whether it holds the character starting at byte `at`.
    #[must_use]
    pub const fn holds(&self, at: usize) -> bool {
        self.start <= at && at < self.end
    }
}

impl Text {
    /// Whether its runs are ones a drawing may hold: in order, disjoint and
    /// non-empty on character boundaries of the content, each overriding
    /// something a drawing may hold and differing from a neighbour it
    /// touches, no more than [`MAX_TEXT_RUNS`] naming no more than
    /// [`MAX_TEXT_FAMILIES`] families.
    pub(super) fn check_runs(&self) -> Result<(), DrawingError> {
        if self.runs.len() > MAX_TEXT_RUNS {
            return Err(DrawingError::TooComplex);
        }
        let mut families: Vec<&str> = Vec::new();
        let mut reached = 0;
        let mut previous: Option<&Run> = None;
        for run in self.runs.iter() {
            let placed = run.start >= reached
                && run.start < run.end
                && run.end <= self.content.len()
                && self.content.is_char_boundary(run.start)
                && self.content.is_char_boundary(run.end);
            let canonical = !run.style.is_empty()
                && previous
                    .is_none_or(|before| before.end < run.start || before.style != run.style);
            if !placed || !canonical || !run.style.holds() {
                return Err(DrawingError::Invalid);
            }
            if let Some(family) = &run.style.family {
                if !families.contains(&family.as_str()) {
                    if families.len() == MAX_TEXT_FAMILIES {
                        return Err(DrawingError::TooComplex);
                    }
                    if !fallible::push(&mut families, family.as_str()) {
                        return Err(DrawingError::OutOfMemory);
                    }
                }
            }
            reached = run.end;
            previous = Some(run);
        }
        Ok(())
    }

    /// The run holding the character that starts at byte `at`, if any.
    #[must_use]
    pub fn run_at(&self, at: usize) -> Option<&Run> {
        let index = self.runs.partition_point(|run| run.end <= at);
        self.runs.get(index).filter(|run| run.holds(at))
    }

    /// It with `change` given what the runs override over `range` of the
    /// content — each run's style there, and an empty one where none lies —
    /// and taking its answer: the span parted from its neighbours, styles that
    /// come to override nothing dropped, and neighbours that come to set the
    /// same joined.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the runs cannot be held, and the
    /// text's own refusals of what the change makes ([`Text::check`]).
    pub fn restyled(
        &self,
        range: Range<usize>,
        change: impl Fn(&RunStyle) -> RunStyle,
    ) -> Result<Self, DrawingError> {
        let range = self.boundaries(range);
        let mut runs: Vec<Run> = Vec::new();
        if !fallible::reserve(&mut runs, self.runs.len() + 2) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut reached = range.start;
        for run in self.runs.iter() {
            if run.end <= range.start || run.start >= range.end {
                push_run(&mut runs, run.start..run.end, &run.style)?;
                continue;
            }
            if run.start < range.start {
                push_run(&mut runs, run.start..range.start, &run.style)?;
            }
            let inside = run.start.max(range.start)..run.end.min(range.end);
            if reached < inside.start {
                push_run(
                    &mut runs,
                    reached..inside.start,
                    &try_arc(change(&RunStyle::default()))?,
                )?;
            }
            push_run(&mut runs, inside.clone(), &try_arc(change(&run.style))?)?;
            reached = inside.end;
            if run.end > range.end {
                push_run(&mut runs, range.end..run.end, &run.style)?;
            }
        }
        if reached < range.end {
            push_run(
                &mut runs,
                reached..range.end,
                &try_arc(change(&RunStyle::default()))?,
            )?;
        }
        runs.sort_by_key(|run| run.start);
        let restyled = Self {
            runs: canonical(runs)?,
            ..self.clone()
        };
        restyled.check()?;
        Ok(restyled)
    }

    /// It with `range` of its content replaced by `with`, the runs following
    /// the content: a run wholly within the range goes, one across its ends
    /// is cut there, and those after it move with the text. `with` takes the
    /// style of the range's first character, or where the range is empty of
    /// the character before it — at the very start, of the one after it.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the content or the runs cannot be
    /// held, and the text's own refusals of what the edit makes
    /// ([`Text::check`]).
    pub fn replaced(&self, range: Range<usize>, with: &str) -> Result<Self, DrawingError> {
        let range = self.boundaries(range);
        let content = fallible::concatenated(&[
            &self.content[..range.start],
            with,
            &self.content[range.end..],
        ])
        .ok_or(DrawingError::OutOfMemory)?;
        let inserted = range.start..range.start + with.len();
        let moved = |at: usize| at - range.end + inserted.end;
        let inherited = self
            .run_for(range.clone())
            .and_then(|index| self.runs.get(index as usize));
        let mut runs: Vec<Run> = Vec::new();
        if !fallible::reserve(&mut runs, self.runs.len() + 2) {
            return Err(DrawingError::OutOfMemory);
        }
        for run in self.runs.iter() {
            if run.start < range.start {
                push_run(&mut runs, run.start..run.end.min(range.start), &run.style)?;
            }
            if run.end > range.end {
                push_run(
                    &mut runs,
                    moved(run.start.max(range.end))..moved(run.end),
                    &run.style,
                )?;
            }
        }
        if let Some(run) = inherited {
            push_run(&mut runs, inserted, &run.style)?;
        }
        runs.sort_by_key(|run| run.start);
        let replaced = Self {
            content: SharedStr::try_from_string(content)?,
            runs: canonical(runs)?,
            ..self.clone()
        };
        replaced.check()?;
        Ok(replaced)
    }

    /// The index of the run what replaces `range` is set in: the run of the
    /// range's first character, or where the range is empty of the character
    /// before it — at the very start, of the one after it; `None` for the
    /// text's own style.
    #[must_use]
    pub fn run_for(&self, range: Range<usize>) -> Option<u32> {
        let range = self.boundaries(range);
        let at = if range.is_empty() && range.start > 0 {
            self.content[..range.start]
                .char_indices()
                .next_back()
                .map_or(0, |(at, _)| at)
        } else {
            range.start
        };
        let index = self.runs.partition_point(|run| run.end <= at);
        self.runs
            .get(index)
            .filter(|run| run.holds(at))
            .and_then(|_| u32::try_from(index).ok())
    }

    /// `range` held within the content, each end moved back to a character
    /// boundary.
    fn boundaries(&self, range: Range<usize>) -> Range<usize> {
        let floor = |at: usize| {
            (0..=at.min(self.content.len()))
                .rev()
                .find(|&at| self.content.is_char_boundary(at))
                .unwrap_or(0)
        };
        let end = floor(range.end);
        floor(range.start).min(end)..end
    }
}

/// Push a run over `span` set as `style`, unless the span is empty.
fn push_run(
    runs: &mut Vec<Run>,
    span: Range<usize>,
    style: &Arc<RunStyle>,
) -> Result<(), DrawingError> {
    if span.is_empty() {
        return Ok(());
    }
    fallible::try_push(
        runs,
        Run {
            start: span.start,
            end: span.end,
            style: Arc::clone(style),
        },
    )
    .map_err(|_| DrawingError::OutOfMemory)
}

/// Runs in order, those overriding nothing dropped and neighbours that touch
/// and set the same joined: the one form a text holds them in.
pub(crate) fn canonical(runs: Vec<Run>) -> Result<SharedSlice<Run>, DrawingError> {
    let mut joined: Vec<Run> = Vec::new();
    if !fallible::reserve(&mut joined, runs.len()) {
        return Err(DrawingError::OutOfMemory);
    }
    for run in runs.into_iter().filter(|run| !run.style.is_empty()) {
        match joined.last_mut() {
            Some(last) if last.end == run.start && last.style == run.style => last.end = run.end,
            _ => joined.push(run),
        }
    }
    Ok(SharedSlice::try_from_vec(joined)?)
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
