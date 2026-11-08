//! A path measured along its length: where a distance along it falls, and
//! which way the path runs there — what dashes are cut at and text is set
//! along.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

use tairix_util::fallible::try_push;

use crate::cubic::ArcTable;
use crate::path::Path;
use crate::point::Point;
use crate::segment::Segment;

/// One segment as measured.
#[derive(Copy, Clone, Debug)]
struct Measured {
    segment: Segment,
    /// The length from the path's start to the segment's start.
    from: f64,
    length: f64,
    /// A cubic's index into the tables.
    table: Option<usize>,
}

/// A path measured along its length: every segment's length, in the order
/// the path runs, and each cubic's arc table beside them, so a line costs a
/// length and no table. A move between sub-paths adds no length, so a
/// distance runs on from one sub-path into the next.
#[derive(Clone, Debug, Default)]
pub struct PathMeasure {
    segments: Vec<Measured>,
    tables: Vec<ArcTable>,
    length: f64,
}

impl PathMeasure {
    /// `path`, measured.
    ///
    /// # Errors
    ///
    /// When the measurements cannot be held.
    pub fn new(path: &Path) -> Result<Self, TryReserveError> {
        let mut measure = Self::default();
        for segment in path.segments() {
            let (length, table) = match segment {
                Segment::Line(line) => (line.length(), None),
                Segment::Cubic(cubic) => {
                    let table = cubic.arc_table();
                    try_push(&mut measure.tables, table)?;
                    (table.length(), Some(measure.tables.len() - 1))
                }
            };
            try_push(
                &mut measure.segments,
                Measured {
                    segment,
                    from: measure.length,
                    length,
                    table,
                },
            )?;
            measure.length += length;
        }
        Ok(measure)
    }

    /// The whole length.
    #[must_use]
    pub const fn length(&self) -> f64 {
        self.length
    }

    /// The point `distance` along the path from its start and the unit
    /// direction the path runs in there; `None` before the start, past the
    /// end, or on a path of no length.
    #[must_use]
    pub fn at(&self, distance: f64) -> Option<(Point, Point)> {
        if !(0.0..=self.length).contains(&distance) || self.length <= 0.0 {
            return None;
        }
        let reaching = self
            .segments
            .partition_point(|measured| measured.from + measured.length < distance);
        let index = (reaching..self.segments.len()).find(|&i| self.segments[i].length > 0.0)?;
        let measured = self.segments[index];
        let t = self.param_at(index, distance - measured.from);
        let direction = direction_at(measured.segment, t)?;
        Some((measured.segment.eval(t), direction))
    }

    /// Segment `index`'s length.
    pub(crate) fn segment_length(&self, index: usize) -> f64 {
        self.segments[index].length
    }

    /// The parameter `along` into segment `index`.
    pub(crate) fn param_at(&self, index: usize, along: f64) -> f64 {
        let measured = &self.segments[index];
        match measured.table {
            Some(table) => self.tables[table].param_at(along),
            None if measured.length > 0.0 => (along / measured.length).clamp(0.0, 1.0),
            None => 0.0,
        }
    }
}

/// The unit direction `segment` runs in at `t`, from the end tangent its
/// derivative gives no direction for at a cusp of coincident controls.
fn direction_at(segment: Segment, t: f64) -> Option<Point> {
    let derivative = segment.deriv(t);
    let direction = if derivative.length() > 0.0 {
        derivative
    } else {
        let (leaving, arriving) = segment.tangents();
        if t < 0.5 {
            leaving
        } else {
            arriving
        }
    };
    let length = direction.length();
    (length > 0.0).then(|| direction * (1.0 / length))
}

#[cfg(test)]
#[path = "measure_tests.rs"]
mod tests;
