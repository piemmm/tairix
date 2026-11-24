//! Thickening as RISC OS 5's Draw module does it, recorded: every segment
//! becomes a hexagon of its own, each join and cap a shape beside it, all
//! filled together, rather than one outline.
//!
//! - **Segments.** From `S` to `E` the hexagon is `E, E+n, S+n, S, S−n, E−n`,
//!   `n` the half width across the segment. A segment of no length makes
//!   nothing and joins nothing.
//! - **Joins**, at each vertex between two drawn segments, on the outer
//!   side: a mitre `V, V+n₁, tip, V+n₂`, a bevel where the mitre would pass
//!   the limit; a round join is a circle.
//! - **Caps** at each end of each run of drawn segments. The leading cap is
//!   at the end, the trailing cap at the start. A run's end cap comes as a
//!   gap or the subpath ends it; the subpath's first run's start cap waits
//!   for the subpath's end, where a closing line may join it instead.
//! - **A subpath with no drawn length** and no gap is a dot: a circle if
//!   both caps are round, else nothing.
//!
//! A thickness of nought draws the thinnest line: every subpath's start then
//! adds nothing to the winding number. The cap and join block is read when
//! first needed, and each of its fields checked only as it is used, so a
//! path that needs no join never refuses a bad one.
//!
//! The arithmetic of the half width, a mitre's tip and the width of a
//! dot-sized line differs from RISC OS's in low bits (`docs/src/desktop/
//! tairix-bridge.md`).

use tairix_drawing::riscos::format::{
    CAP_BUTT, CAP_ROUND, CAP_SQUARE, CAP_TRIANGLE, JOIN_BEVEL, JOIN_MITRE, JOIN_ROUND,
};

use crate::geometry::{length_of, Point};
use crate::path::Element;
use crate::stage::{Guest, Refusal, Sink, Stop};

/// The Draw module's cap and join block: its join, each end's cap, its
/// mitre limit and each end's triangle cap, read from its four words and
/// written to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Style {
    pub(crate) join: u32,
    /// The leading cap: at a run's end.
    pub(crate) end_cap: u32,
    /// The trailing cap: at a run's start.
    pub(crate) start_cap: u32,
    /// The longest a mitre may be, in line widths, sixteen bits of fraction.
    pub(crate) mitre: i32,
    pub(crate) end_triangle: Triangle,
    pub(crate) start_triangle: Triangle,
}

impl Style {
    /// The block its words hold.
    fn of([kinds, mitre, end_triangle, start_triangle]: [u32; 4]) -> Self {
        let [join, end_cap, start_cap, _] = kinds.to_le_bytes();
        Self {
            join: u32::from(join),
            end_cap: u32::from(end_cap),
            start_cap: u32::from(start_cap),
            mitre: mitre.cast_signed(),
            end_triangle: Triangle::of(end_triangle),
            start_triangle: Triangle::of(start_triangle),
        }
    }

    /// Its block's words.
    pub(crate) const fn words(self) -> [u32; 4] {
        [
            self.join | (self.end_cap << 8) | (self.start_cap << 16),
            self.mitre.cast_unsigned(),
            self.end_triangle.word(),
            self.start_triangle.word(),
        ]
    }
}

/// A triangular cap's size, across and along its line, each in
/// [`Triangle::UNIT`] parts of the line's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Triangle {
    pub(crate) width: u32,
    pub(crate) length: u32,
}

impl Triangle {
    /// The parts of its line's width the block holds a size in.
    pub(crate) const UNIT: u32 = 256;

    /// The size a block's word holds: its length in the top half, its width
    /// in the bottom.
    const fn of(word: u32) -> Self {
        Self {
            width: word & 0xFFFF,
            length: word >> 16,
        }
    }

    /// Its block's word.
    const fn word(self) -> u32 {
        (self.length << 16) | self.width
    }
}

/// A drawn segment's direction: its end less its start.
type Direction = (i64, i64);

/// The subpath being thickened.
#[derive(Clone, Copy, Debug)]
struct Subpath {
    start: Point,
    at: Point,
    /// The first run's first drawn segment, whose start cap waits.
    first: Option<Direction>,
    /// The current run's last drawn segment.
    last: Option<Direction>,
    /// A gap has ended a run.
    after_gap: bool,
}

/// Turns each subpath into the shapes that fill as its thick line.
pub(crate) struct Thickener<'a> {
    next: &'a mut dyn Sink,
    thickness: i32,
    width: u32,
    block: u32,
    style: Option<Style>,
    subpath: Option<Subpath>,
}

/// `value` shifted down `bits`, kept to its low word as the processor's
/// registers keep it.
fn low_word(value: i64, bits: u32) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "RISC OS's registers keep a product's low word"
    )]
    let word = (value >> bits) as i32;
    word
}

impl<'a> Thickener<'a> {
    /// A thickener drawing lines `thickness` thick, its caps and joins from
    /// the block at `block`.
    pub(crate) fn new(next: &'a mut dyn Sink, thickness: i32, block: u32) -> Self {
        Self {
            next,
            thickness,
            width: thickness.unsigned_abs(),
            block,
            style: None,
            subpath: None,
        }
    }

    fn style(&mut self, guest: &mut dyn Guest) -> Result<Style, Stop> {
        if let Some(style) = self.style {
            return Ok(style);
        }
        let mut words = [0; 4];
        guest.read_words(self.block, &mut words)?;
        let style = Style::of(words);
        self.style = Some(style);
        Ok(style)
    }

    /// The half width across a segment of direction `(dx, dy)`, to its left.
    fn normal(&self, (dx, dy): Direction) -> Point {
        let length = i128::from(length_of(dx, dy)).max(1);
        let width = i128::from(self.width);
        let half = |v: i128| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "half a width that fits a word"
            )]
            let half = ((v / length) >> 1) as i32;
            half
        };
        Point::new(half(-i128::from(dy) * width), half(i128::from(dx) * width))
    }

    fn polygon(&mut self, guest: &mut dyn Guest, points: &[Point]) -> Result<(), Stop> {
        let Some((&first, rest)) = points.split_first() else {
            return Ok(());
        };
        self.next.element(
            guest,
            Element::Move {
                to: first,
                winds: true,
            },
        )?;
        for &to in rest {
            self.next.element(guest, Element::Line { to })?;
        }
        self.next.element(guest, Element::Close { gap: false })
    }

    /// A circle about `centre` as wide as the line: four quarters from its
    /// right, anticlockwise, their handles the radius's 36194/65536th. The
    /// radius keeps the thickness's sign, so a negative one starts the
    /// circle at its left.
    fn circle(&mut self, guest: &mut dyn Guest, centre: Point) -> Result<(), Stop> {
        let r = self.thickness >> 1;
        let k = low_word(i64::from(r) * 36194, 16);
        let at = |x: i32, y: i32| centre + Point::new(x, y);
        self.next.element(
            guest,
            Element::Move {
                to: at(r, 0),
                winds: true,
            },
        )?;
        for (c1, c2, to) in [
            (at(r, k), at(k, r), at(0, r)),
            (at(-k, r), at(-r, k), at(-r, 0)),
            (at(-r, -k), at(-k, -r), at(0, -r)),
            (at(k, -r), at(r, -k), at(r, 0)),
        ] {
            self.next.element(guest, Element::Curve { c1, c2, to })?;
        }
        self.next.element(guest, Element::Close { gap: false })
    }

    fn join(
        &mut self,
        guest: &mut dyn Guest,
        vertex: Point,
        d1: Direction,
        d2: Direction,
    ) -> Result<(), Stop> {
        let style = self.style(guest)?;
        match style.join {
            JOIN_ROUND => return self.circle(guest, vertex),
            JOIN_MITRE if style.mitre <= 0 => return Err(Refusal::BadCapJoin.into()),
            JOIN_MITRE | JOIN_BEVEL => {}
            _ => return Err(Refusal::BadCapJoin.into()),
        }
        let (n1, n2) = (self.normal(d1), self.normal(d2));
        let cross = i128::from(d1.0) * i128::from(d2.1) - i128::from(d1.1) * i128::from(d2.0);
        let dot = i128::from(d1.0) * i128::from(d2.0) + i128::from(d1.1) * i128::from(d2.1);
        // A left turn, or a reversal, has its outer side to the right; either
        // way the join winds anticlockwise, as every shape here does.
        let left = cross > 0 || (cross == 0 && dot < 0);
        let (o1, o2) = if left { (-n1, -n2) } else { (n1, n2) };
        let (p, q) = (vertex + o1, vertex + o2);
        let (first, second) = if left { (p, q) } else { (q, p) };
        if style.join == JOIN_MITRE && within_mitre(o1, o2, style.mitre) {
            let tip = vertex + mitre_tip(o1, o2);
            self.polygon(guest, &[vertex, first, tip, second])
        } else {
            self.polygon(guest, &[vertex, first, second])
        }
    }

    /// The cap at `at`, a run's end if `end`, its segment's direction `d`.
    fn cap(
        &mut self,
        guest: &mut dyn Guest,
        at: Point,
        d: Direction,
        end: bool,
    ) -> Result<(), Stop> {
        let style = self.style(guest)?;
        let (kind, triangle) = if end {
            (style.end_cap, style.end_triangle)
        } else {
            (style.start_cap, style.start_triangle)
        };
        // An end cap is a start cap facing back along its segment.
        let n = if end { -self.normal(d) } else { self.normal(d) };
        let back = n.turned();
        match kind {
            CAP_BUTT => Ok(()),
            CAP_ROUND => self.circle(guest, at),
            CAP_SQUARE => self.polygon(guest, &[at, at + n, at + n - back, at - n - back, at - n]),
            CAP_TRIANGLE => {
                let [width, length] = [triangle.width, triangle.length].map(u32::cast_signed);
                // The sizes are in parts of the whole width, of which `n` is half.
                let shift = Triangle::UNIT.trailing_zeros() - 1;
                let scaled = |p: Point, by: i32| {
                    Point::new(p.x.wrapping_mul(by) >> shift, p.y.wrapping_mul(by) >> shift)
                };
                let v = scaled(n, width);
                let tip = at - scaled(back, length);
                self.polygon(guest, &[at + n, at + v, tip, at - v, at - n, at])
            }
            _ => Err(Refusal::BadCapJoin.into()),
        }
    }

    fn hexagon(
        &mut self,
        guest: &mut dyn Guest,
        from: Point,
        to: Point,
        d: Direction,
    ) -> Result<(), Stop> {
        let n = self.normal(d);
        self.polygon(guest, &[to, to + n, from + n, from, from - n, to - n])
    }

    /// A drawn segment from the pen to `to`.
    fn segment(
        &mut self,
        guest: &mut dyn Guest,
        mut subpath: Subpath,
        to: Point,
    ) -> Result<Subpath, Stop> {
        let from = subpath.at;
        subpath.at = to;
        let d = (
            i64::from(to.x) - i64::from(from.x),
            i64::from(to.y) - i64::from(from.y),
        );
        if d == (0, 0) {
            return Ok(subpath);
        }
        match subpath.last {
            Some(previous) => self.join(guest, from, previous, d)?,
            None if subpath.after_gap => self.cap(guest, from, d, false)?,
            None => subpath.first = Some(d),
        }
        self.hexagon(guest, from, to, d)?;
        subpath.last = Some(d);
        Ok(subpath)
    }

    /// The subpath ended without closing, or closed by a gap: its caps.
    fn open_end(&mut self, guest: &mut dyn Guest, subpath: Subpath) -> Result<(), Stop> {
        if let Some(first) = subpath.first {
            self.cap(guest, subpath.start, first, false)?;
        }
        if let Some(last) = subpath.last {
            self.cap(guest, subpath.at, last, true)?;
        }
        if subpath.first.is_none() && subpath.last.is_none() && !subpath.after_gap {
            let style = self.style(guest)?;
            if style.start_cap == CAP_ROUND && style.end_cap == CAP_ROUND {
                self.circle(guest, subpath.start)?;
            }
        }
        Ok(())
    }

    /// The subpath closed by a line: the line drawn, and joined to the first
    /// segment where the first run reaches the start, else capped there.
    fn closed(&mut self, guest: &mut dyn Guest, subpath: Subpath) -> Result<(), Stop> {
        let subpath = self.segment(guest, subpath, subpath.start)?;
        match (subpath.last, subpath.first) {
            (Some(last), Some(first)) => self.join(guest, subpath.start, last, first),
            (Some(last), None) => self.cap(guest, subpath.start, last, true),
            (None, Some(first)) => self.cap(guest, subpath.start, first, false),
            (None, None) => Ok(()),
        }
    }
}

/// The outer offsets' mean square, standing for the half width's square,
/// and their dot product: the turn's cosine, in those units.
fn spread(o1: Point, o2: Point) -> (i128, i128) {
    let square = |p: Point| i128::from(p.x).pow(2) + i128::from(p.y).pow(2);
    let dot = i128::from(o1.x) * i128::from(o2.x) + i128::from(o1.y) * i128::from(o2.y);
    ((square(o1) + square(o2)) >> 1, dot)
}

/// Whether a mitre between the outer offsets `o1` and `o2` stays within
/// `limit` line widths: one over the cosine of half the turn no more than
/// the limit.
fn within_mitre(o1: Point, o2: Point, limit: i32) -> bool {
    let (square, dot) = spread(o1, o2);
    i128::from(limit).pow(2) * (square + dot) >= (square << 33)
}

/// The mitre's tip from its vertex, along the offsets' bisector as RISC OS
/// finds it, each axis rounded down.
fn mitre_tip(o1: Point, o2: Point) -> Point {
    let (square, dot) = spread(o1, o2);
    let along = square + dot;
    if along <= 0 {
        return o1;
    }
    let reach = |a: i32, b: i32| {
        let tip = ((i128::from(a) + i128::from(b)) * square).div_euclid(along);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a tip within the limit lies within a few widths of its vertex, kept to a \
                      word as the processor keeps it"
        )]
        let tip = tip as i32;
        tip
    };
    Point::new(reach(o1.x, o2.x), reach(o1.y, o2.y))
}

impl Sink for Thickener<'_> {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        if self.width == 0 {
            let thin = match element {
                Element::Move { to, .. } => Element::Move { to, winds: false },
                other => other,
            };
            return self.next.element(guest, thin);
        }
        match (element, self.subpath.take()) {
            (Element::Move { to, .. }, previous) => {
                if let Some(previous) = previous {
                    self.open_end(guest, previous)?;
                }
                self.subpath = Some(Subpath {
                    start: to,
                    at: to,
                    first: None,
                    last: None,
                    after_gap: false,
                });
                Ok(())
            }
            (Element::Curve { .. }, _) => Err(Refusal::NeedsFlattening.into()),
            (_, None) => Err(Refusal::Order.into()),
            (Element::Line { to }, Some(subpath)) => {
                self.subpath = Some(self.segment(guest, subpath, to)?);
                Ok(())
            }
            (Element::Gap { to }, Some(mut subpath)) => {
                if let Some(last) = subpath.last.take() {
                    self.cap(guest, subpath.at, last, true)?;
                }
                subpath.at = to;
                subpath.after_gap = true;
                self.subpath = Some(subpath);
                Ok(())
            }
            (Element::Close { gap: true }, Some(subpath)) => self.open_end(guest, subpath),
            (Element::Close { gap: false }, Some(subpath)) => self.closed(guest, subpath),
        }
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        if let Some(subpath) = self.subpath.take() {
            self.open_end(guest, subpath)?;
        }
        self.next.finish(guest)
    }
}

#[cfg(test)]
mod tests;
