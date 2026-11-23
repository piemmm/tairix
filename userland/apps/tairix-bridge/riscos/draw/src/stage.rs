//! The stages a path passes through, in RISC OS's order: subpaths closed,
//! curves flattened, the path dashed, thickened, flattened again and
//! transformed, then handed to what the call asked for. Each stage passes on
//! what it makes as it makes it, so an error leaves what went before it
//! written, as RISC OS's does.

use tairix_riscos_kernel::os::Fail;

use crate::flatten::{self, Threshold, TooDeep};
use crate::geometry::{Matrix, Point};
use crate::path::Element;
use crate::raster::NoRoom;

/// A run of pixels on a row: the row, then its first and last column.
pub(crate) type Span = (i32, i32, i32);

/// What processing reaches beyond itself: guest memory, and the screen.
pub(crate) trait Guest {
    fn read_word(&mut self, at: u32) -> Result<u32, Fail>;
    fn write_word(&mut self, at: u32, value: u32) -> Result<(), Fail>;
    /// Plots `spans` in the graphics foreground, within the graphics window.
    fn plot(&mut self, spans: &[Span]) -> Result<(), Fail>;
    /// A step of work that runs as long as the path asks, where the host
    /// may stop a call that would never end.
    fn yield_point(&mut self) -> Result<(), Fail>;
}

/// A refusal of the Draw module's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Flags RISC OS reserves.
    Reserved,
    /// Floating point, which RISC OS has never had.
    Unsupported,
    BadElement,
    /// The path's continuations lead back on themselves, which RISC OS
    /// follows for ever.
    Looped,
    Order,
    MayChangeLength,
    OutputFull,
    NeedsFlattening,
    BadCapJoin,
    Overflow,
}

/// Why processing stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Refused(Refusal),
    /// A read or write of guest memory failed.
    Fail(Fail),
    /// The host refused memory.
    NoRoom,
    /// Flattening that would not end: RISC OS's runs out of stack.
    TooDeep,
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<Fail> for Stop {
    fn from(fail: Fail) -> Self {
        Self::Fail(fail)
    }
}

impl From<NoRoom> for Stop {
    fn from(_: NoRoom) -> Self {
        Self::NoRoom
    }
}

impl From<TooDeep> for Stop {
    fn from(_: TooDeep) -> Self {
        Self::TooDeep
    }
}

/// What takes a path's elements, one at a time.
pub(crate) trait Sink {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop>;
    /// The path is over.
    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop>;
}

/// The current point and the current subpath's start, as elements move
/// them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pen {
    pub(crate) at: Point,
    pub(crate) start: Point,
}

impl Pen {
    pub(crate) fn follow(&mut self, element: Element) {
        match element {
            Element::Move { to, .. } => {
                self.at = to;
                self.start = to;
            }
            Element::Close { .. } => self.at = self.start,
            Element::Curve { to, .. } | Element::Line { to } | Element::Gap { to } => self.at = to,
        }
    }
}

/// Closes each open subpath with a line back to its start.
pub(crate) struct Closer<'a> {
    next: &'a mut dyn Sink,
    open: bool,
    /// The last subpath's start, as read.
    pub(crate) last_start: Option<Point>,
}

impl<'a> Closer<'a> {
    pub(crate) fn new(next: &'a mut dyn Sink) -> Self {
        Self {
            next,
            open: false,
            last_start: None,
        }
    }
}

const CLOSE: Element = Element::Close { gap: false };

impl Sink for Closer<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        match element {
            Element::Move { to, .. } => {
                if self.open {
                    self.next.element(guest, CLOSE)?;
                }
                self.open = true;
                self.last_start = Some(to);
            }
            Element::Close { .. } => self.open = false,
            Element::Curve { .. } | Element::Line { .. } | Element::Gap { .. } => {}
        }
        self.next.element(guest, element)
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        if self.open {
            self.open = false;
            self.next.element(guest, CLOSE)?;
        }
        self.next.finish(guest)
    }
}

/// Flattens each curve into lines.
pub(crate) struct Flattener<'a> {
    next: &'a mut dyn Sink,
    threshold: Threshold,
    pen: Pen,
}

impl<'a> Flattener<'a> {
    pub(crate) fn new(next: &'a mut dyn Sink, threshold: Threshold) -> Self {
        Self {
            next,
            threshold,
            pen: Pen::default(),
        }
    }
}

impl Sink for Flattener<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        let from = self.pen.at;
        self.pen.follow(element);
        let Element::Curve { c1, c2, to } = element else {
            return self.next.element(guest, element);
        };
        let next = &mut *self.next;
        flatten::flatten(from, c1, c2, to, self.threshold, |p| {
            next.element(guest, Element::Line { to: p })
        })
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        self.next.finish(guest)
    }
}

/// Hands each element on as a step of work: a small path can ask for work
/// without end, a line the coordinate range long dashed a unit at a time.
pub(crate) struct Steps<'a> {
    next: &'a mut dyn Sink,
}

impl<'a> Steps<'a> {
    pub(crate) fn new(next: &'a mut dyn Sink) -> Self {
        Self { next }
    }
}

impl Sink for Steps<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        guest.yield_point()?;
        self.next.element(guest, element)
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        self.next.finish(guest)
    }
}

/// Maps each point through the matrix.
pub(crate) struct Transformer<'a> {
    next: &'a mut dyn Sink,
    matrix: Matrix,
}

impl<'a> Transformer<'a> {
    pub(crate) fn new(next: &'a mut dyn Sink, matrix: Matrix) -> Self {
        Self { next, matrix }
    }
}

impl Sink for Transformer<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        let matrix = self.matrix;
        let mapped =
            element.try_map(|p| matrix.apply(p).ok_or(Stop::Refused(Refusal::Overflow)))?;
        self.next.element(guest, mapped)
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        self.next.finish(guest)
    }
}

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tests;
