//! Dashing as RISC OS 5's Draw module does it, recorded: each subpath
//! starts the pattern afresh at its start offset, the pattern carries on
//! across corners and through gaps, and the closing line's last part
//! becomes the close, a gap's or a line's as the pattern is off or on there.
//!
//! The pattern block is a start offset, a count and that many lengths, all
//! words. Lengths are unsigned, so a negative one is a dash too long to end;
//! the elements are taken in turn, on and off alternately, so an odd count
//! swaps on and off each time round; a count of nought behaves as one, and
//! the elements are read only as dashing reaches them. A run ending exactly
//! where a segment does hands the segment's end to the next run too, as a
//! piece of no length.
//!
//! A pattern whose lengths sum to nought never moves on: RISC OS 5.30 never
//! returns from it, and here it leaves the path undashed.

use tairix_drawing::riscos::format::{DASH_COUNT, DASH_LENGTHS};

use crate::geometry::{length_of, Point};
use crate::path::Element;
use crate::stage::{Guest, Pen, Refusal, Sink, Stop};

/// The dash pattern a block in guest memory holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    block: u32,
}

/// Where in the pattern dashing is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Phase {
    index: u32,
    on: bool,
    /// What is left of the current element.
    remaining: u64,
}

/// The pattern's header, read when dashing first needs it.
#[derive(Clone, Copy, Debug)]
struct Header {
    start: u32,
    count: u32,
}

/// Where the block holds its count and its lengths, as a guest address is
/// offset.
#[expect(clippy::cast_possible_truncation, reason = "a word")]
const COUNT: u32 = DASH_COUNT as u32;
#[expect(clippy::cast_possible_truncation, reason = "two words")]
const LENGTHS: u32 = DASH_LENGTHS as u32;

impl Pattern {
    /// The pattern in the block at `block`.
    pub(crate) const fn at(block: u32) -> Self {
        Self { block }
    }

    fn header(self, guest: &mut dyn Guest) -> Result<Header, Stop> {
        Ok(Header {
            start: guest.read_word(self.block)?,
            count: guest.read_word(self.block.wrapping_add(COUNT))?,
        })
    }

    fn length(self, guest: &mut dyn Guest, index: u32) -> Result<u64, Stop> {
        guest.yield_point()?;
        let at = self
            .block
            .wrapping_add(LENGTHS)
            .wrapping_add(index.wrapping_mul(4));
        Ok(u64::from(guest.read_word(at)?))
    }

    fn after(self, guest: &mut dyn Guest, header: Header, phase: Phase) -> Result<Phase, Stop> {
        let index = match phase.index.wrapping_add(1) {
            next if next >= header.count => 0,
            next => next,
        };
        Ok(Phase {
            index,
            on: !phase.on,
            remaining: self.length(guest, index)?,
        })
    }

    /// Where each subpath starts in the pattern: the start offset walked
    /// through the elements, a whole cycle's length taken off at once once
    /// one is known. `None` where a cycle has no length.
    fn first(self, guest: &mut dyn Guest, header: Header) -> Result<Option<Phase>, Stop> {
        let mut offset = u64::from(header.start);
        let mut phase = Phase {
            index: 0,
            on: true,
            remaining: self.length(guest, 0)?,
        };
        let mut cycle: Option<u64> = Some(0);
        loop {
            if offset < phase.remaining {
                phase.remaining -= offset;
                return Ok(Some(phase));
            }
            offset -= phase.remaining;
            cycle = cycle.map(|sum| sum + phase.remaining);
            phase = self.after(guest, header, phase)?;
            if phase.index == 0 && phase.on {
                match cycle.take() {
                    Some(0) => return Ok(None),
                    Some(sum) => offset %= sum,
                    None => {}
                }
            }
        }
    }
}

/// Splits each subpath's segments into the pattern's dashes and gaps.
pub(crate) struct Dasher<'a> {
    next: &'a mut dyn Sink,
    pattern: Pattern,
    /// The header and each subpath's first phase, once read; `None` inside
    /// for a pattern that never moves on.
    start: Option<(Header, Option<Phase>)>,
    phase: Option<Phase>,
    pen: Pen,
}

impl<'a> Dasher<'a> {
    pub(crate) fn new(next: &'a mut dyn Sink, pattern: Pattern) -> Self {
        Self {
            next,
            pattern,
            start: None,
            phase: None,
            pen: Pen::default(),
        }
    }

    fn begin(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        let (_, first) = if let Some(start) = self.start {
            start
        } else {
            let header = self.pattern.header(guest)?;
            let start = (header, self.pattern.first(guest, header)?);
            self.start = Some(start);
            start
        };
        self.phase = first;
        Ok(())
    }

    fn header(&self) -> Option<Header> {
        self.start.map(|(header, _)| header)
    }

    fn piece(on: bool, to: Point) -> Element {
        if on {
            Element::Line { to }
        } else {
            Element::Gap { to }
        }
    }

    /// The drawn segment from the pen to `to` dashed, its last part the
    /// close if `close`.
    fn dash(&mut self, guest: &mut dyn Guest, to: Point, close: bool) -> Result<(), Stop> {
        let (Some(mut phase), Some(header)) = (self.phase, self.header()) else {
            return Err(Refusal::Order.into());
        };
        let from = self.pen.at;
        let (dx, dy) = (
            i64::from(to.x) - i64::from(from.x),
            i64::from(to.y) - i64::from(from.y),
        );
        let length = length_of(dx, dy);
        let mut left = length;
        let mut at = from;
        while phase.remaining <= left {
            let end = if phase.remaining == left {
                to
            } else {
                step(at, dx, dy, phase.remaining, length)
            };
            self.next.element(guest, Self::piece(phase.on, end))?;
            left -= phase.remaining;
            at = end;
            phase = self.pattern.after(guest, header, phase)?;
        }
        phase.remaining -= left;
        self.phase = Some(phase);
        let last = if close {
            Element::Close { gap: !phase.on }
        } else {
            Self::piece(phase.on, to)
        };
        self.next.element(guest, last)
    }

    /// The pattern moved on along a gap, which is passed on whole.
    fn gap(&mut self, guest: &mut dyn Guest, to: Point) -> Result<(), Stop> {
        let (Some(mut phase), Some(header)) = (self.phase, self.header()) else {
            return Err(Refusal::Order.into());
        };
        let from = self.pen.at;
        let mut left = length_of(
            i64::from(to.x) - i64::from(from.x),
            i64::from(to.y) - i64::from(from.y),
        );
        while phase.remaining <= left {
            left -= phase.remaining;
            phase = self.pattern.after(guest, header, phase)?;
        }
        phase.remaining -= left;
        self.phase = Some(phase);
        self.next.element(guest, Element::Gap { to })
    }
}

/// `run` further along a segment of `(dx, dy)` and `length` from `at`: each
/// axis's share cut toward nought on its own, as RISC OS steps a dash.
fn step(at: Point, dx: i64, dy: i64, run: u64, length: u64) -> Point {
    let share = |d: i64| -> i32 {
        let moved = i128::from(d) * i128::from(run) / i128::from(length);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the point lies between the segment's ends, so the move's low word, \
                      added wrapping, lands on it"
        )]
        let moved = moved as i32;
        moved
    };
    Point::new(at.x.wrapping_add(share(dx)), at.y.wrapping_add(share(dy)))
}

impl Sink for Dasher<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        if let Element::Move { .. } = element {
            self.begin(guest)?;
        }
        let dashing = self.phase.is_some();
        let done = match element {
            _ if !dashing => self.next.element(guest, element),
            Element::Move { .. } | Element::Close { gap: true } => {
                self.next.element(guest, element)
            }
            Element::Line { to } => self.dash(guest, to, false),
            Element::Close { gap: false } => self.dash(guest, self.pen.start, true),
            Element::Gap { to } => self.gap(guest, to),
            Element::Curve { .. } => Err(Refusal::NeedsFlattening.into()),
        };
        self.pen.follow(element);
        done
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        self.next.finish(guest)
    }
}

#[cfg(test)]
mod tests;
