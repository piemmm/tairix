//! A processed path plotted: its segments scan converted on the screen the
//! VDU drivers plot on, all at once or a subpath at a time, in the graphics
//! foreground, or kept as the clip a later fill is held within.
//!
//! RISC OS 5's `Draw_FillClipped` and `Draw_StrokeClipped` work through two
//! fill style bits of `Draw_ProcessPath`'s own: bit 6 makes the clip from the
//! pixels a fill would plot, and bit 7 plots only those of a fill's pixels
//! the clip holds. The clip is the module's, kept until the next is made;
//! before any is made it holds nothing.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::path::Element;
use crate::raster::{self, class, Rule, Segment, Window};
use crate::stage::{Guest, Refusal, Sink, Span, Stop};

/// What a fill is plotted on: the screen's or sprite's pixels as the VDU
/// drivers hold them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Surface {
    /// Each axis's eigen factor: an OS unit's pixels as a shift.
    pub(crate) eig: (u32, u32),
    /// The graphics origin, in OS units from the bottom left.
    pub(crate) origin: (i32, i32),
    /// The graphics window, in pixels.
    pub(crate) window: Window,
}

impl Surface {
    /// A point in Draw's internal units, in 256ths of a pixel from the
    /// bottom left.
    fn place(self, x: i32, y: i32) -> (i64, i64) {
        let along =
            |v: i32, origin: i32, eig: u32| (i64::from(v) + (i64::from(origin) << 8)) >> eig;
        (
            along(x, self.origin.0, self.eig.0),
            along(y, self.origin.1, self.eig.1),
        )
    }
}

/// The most segments a fill may hold within `bytes` of memory.
pub(crate) fn most_segments(bytes: u64) -> usize {
    usize::try_from(bytes / core::mem::size_of::<Segment>() as u64).unwrap_or(usize::MAX)
}

/// What a fill does with its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Use {
    /// Plotted a subpath at a time, rather than all at once.
    pub(crate) each: bool,
    /// Kept as the clip rather than plotted.
    pub(crate) make_clip: bool,
    /// Held within the clip.
    pub(crate) within_clip: bool,
}

/// Plots a path's segments.
pub(crate) struct Fill {
    surface: Surface,
    rule: Rule,
    classes: u32,
    using: Use,
    /// The clip a fill is held within.
    clip: Vec<Span>,
    /// The clip being made.
    pub(crate) made: Vec<Span>,
    segments: Vec<Segment>,
    /// The most segments it holds: RISC OS keeps a fill's edges in guest
    /// memory, which the session's ceiling bounds, and so does this.
    most: usize,
    /// What each subpath's scan works in and gives, kept for the next.
    scratch: raster::Scratch,
    spans: Vec<Span>,
    clipped: Vec<Span>,
    start: (i64, i64),
    at: (i64, i64),
    winds: bool,
}

impl Fill {
    /// A fill of style `style`'s rule and classes, held within `clip` where
    /// asked.
    pub(crate) fn new(
        surface: Surface,
        style: u32,
        using: Use,
        clip: Vec<Span>,
        most: usize,
    ) -> Self {
        Self {
            surface,
            rule: Rule::of(style),
            classes: style & class::ALL,
            using,
            clip,
            made: Vec::new(),
            segments: Vec::new(),
            most,
            scratch: raster::Scratch::default(),
            spans: Vec::new(),
            clipped: Vec::new(),
            start: (0, 0),
            at: (0, 0),
            winds: true,
        }
    }

    /// The clip it was held within, given back.
    pub(crate) fn into_clip(self) -> Vec<Span> {
        self.clip
    }

    fn segment(&mut self, to: (i64, i64), boundary: bool) -> Result<(), Stop> {
        let from = core::mem::replace(&mut self.at, to);
        // A dot where a segment already ends, or a gap going nowhere, adds
        // no pixel.
        let adds_nothing = from == to
            && (!boundary
                || self
                    .segments
                    .last()
                    .is_some_and(|last| last.boundary && last.to == to));
        if adds_nothing {
            return Ok(());
        }
        if self.segments.len() >= self.most {
            return Err(Stop::NoRoom);
        }
        let segment = Segment {
            from,
            to,
            boundary,
            winds: self.winds,
        };
        fallible::try_push(&mut self.segments, segment).map_err(|_| Stop::NoRoom)
    }

    /// The segments so far scan converted, and plotted, kept or held within
    /// the clip.
    fn flush(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        let Self {
            surface,
            rule,
            classes,
            using,
            clip,
            made,
            segments,
            scratch,
            spans,
            clipped,
            ..
        } = self;
        if segments.is_empty() {
            return Ok(());
        }
        spans.clear();
        let mut short = false;
        raster::scan(
            segments,
            *rule,
            *classes,
            surface.window,
            scratch,
            || guest.yield_point().map_err(Stop::Fail),
            |row, left, right| short |= fallible::try_push(spans, (row, left, right)).is_err(),
        )?;
        segments.clear();
        if short {
            return Err(Stop::NoRoom);
        }
        let plotted: &[Span] = if using.within_clip {
            within(spans, clip, clipped)?;
            clipped
        } else {
            spans
        };
        if using.make_clip {
            if !fallible::reserve(made, plotted.len()) {
                return Err(Stop::NoRoom);
            }
            made.extend_from_slice(plotted);
            return Ok(());
        }
        guest.plot(plotted).map_err(Stop::Fail)
    }
}

/// The parts of `spans` that `clip` holds, into `kept`; both in row order
/// and each row's runs in column order.
fn within(spans: &[Span], clip: &[Span], kept: &mut Vec<Span>) -> Result<(), Stop> {
    kept.clear();
    let mut from = 0;
    for &(row, left, right) in spans {
        while from < clip.len() && (clip[from].0, clip[from].2) < (row, left) {
            from += 1;
        }
        for &(clip_row, clip_left, clip_right) in &clip[from..] {
            if clip_row != row || clip_left > right {
                break;
            }
            let (l, r) = (left.max(clip_left), right.min(clip_right));
            if l <= r {
                fallible::try_push(kept, (row, l, r)).map_err(|_| Stop::NoRoom)?;
            }
        }
    }
    Ok(())
}

/// `spans` sorted by row and column, those that touch or overlap merged:
/// the clip made a subpath at a time, as one set.
pub(crate) fn normalised(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_unstable();
    let mut kept: usize = 0;
    for i in 0..spans.len() {
        let span = spans[i];
        match kept.checked_sub(1).map(|last| spans[last]) {
            Some((row, _, right)) if row == span.0 && span.1 <= right.saturating_add(1) => {
                spans[kept - 1].2 = right.max(span.2);
            }
            _ => {
                spans[kept] = span;
                kept += 1;
            }
        }
    }
    spans.truncate(kept);
    spans
}

impl Sink for Fill {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        match element {
            Element::Move { to, winds } => {
                if self.using.each {
                    self.flush(guest)?;
                }
                self.start = self.surface.place(to.x, to.y);
                self.at = self.start;
                self.winds = winds;
                Ok(())
            }
            Element::Line { to } => self.segment(self.surface.place(to.x, to.y), true),
            Element::Gap { to } => self.segment(self.surface.place(to.x, to.y), false),
            Element::Close { gap } => self.segment(self.start, !gap),
            Element::Curve { .. } => Err(Refusal::NeedsFlattening.into()),
        }
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        self.flush(guest)
    }
}

#[cfg(test)]
mod tests;
