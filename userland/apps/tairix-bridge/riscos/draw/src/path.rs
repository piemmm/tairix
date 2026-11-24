//! Path elements as the Draw module reads and writes them: a word whose low
//! byte is the element's code, then its points.

use tairix_drawing::riscos::format::{
    element_bytes, CLOSE, CLOSE_WITH_GAP, CURVE, ELEMENT_POINTS, GAP, LINE, MOVE, MOVE_UNWOUND,
    POINT,
};

use crate::geometry::Point;

/// Where an element's points begin after its code's word, and where a
/// continuation's address is, as a guest address is offset.
#[expect(clippy::cast_possible_truncation, reason = "a word")]
pub(crate) const POINTS_AT: u32 = ELEMENT_POINTS as u32;

/// A point's bytes, as a guest address is offset.
#[expect(clippy::cast_possible_truncation, reason = "two words")]
pub(crate) const POINT_BYTES: u32 = POINT as u32;

/// The bytes the element of code `code` takes in a path, as a guest address
/// is offset; nought for a code that names none.
#[expect(clippy::cast_possible_truncation, reason = "seven words at most")]
pub(crate) const fn bytes_of(code: u32) -> u32 {
    match element_bytes(code) {
        Some(bytes) => bytes as u32,
        None => 0,
    }
}

/// An element of a path, but its end and continuations, which a reader
/// follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Element {
    /// A new subpath from `to`, adding to the winding number or not.
    Move {
        /// Where the subpath starts.
        to: Point,
        /// Whether its segments count toward the winding number.
        winds: bool,
    },
    /// The subpath closed back to its start, by a line or by a gap.
    Close {
        /// Whether by a gap, never stroked.
        gap: bool,
    },
    /// A cubic Bézier curve.
    Curve {
        /// Its first control point.
        c1: Point,
        /// Its second.
        c2: Point,
        /// Where it ends.
        to: Point,
    },
    /// An invisible segment: it winds but is never stroked.
    Gap {
        /// Where it ends.
        to: Point,
    },
    /// A straight line.
    Line {
        /// Where it ends.
        to: Point,
    },
}

impl Element {
    pub(crate) const fn code(self) -> u32 {
        match self {
            Self::Move { winds: true, .. } => MOVE,
            Self::Move { winds: false, .. } => MOVE_UNWOUND,
            Self::Close { gap: true } => CLOSE_WITH_GAP,
            Self::Close { gap: false } => CLOSE,
            Self::Curve { .. } => CURVE,
            Self::Gap { .. } => GAP,
            Self::Line { .. } => LINE,
        }
    }

    /// The bytes it takes in a path.
    pub(crate) const fn bytes(self) -> u32 {
        bytes_of(self.code())
    }

    /// Its points, in the order a path holds them.
    pub(crate) const fn points(self) -> [Option<Point>; 3] {
        match self {
            Self::Close { .. } => [None; 3],
            Self::Curve { c1, c2, to } => [Some(c1), Some(c2), Some(to)],
            Self::Move { to, .. } | Self::Gap { to } | Self::Line { to } => [Some(to), None, None],
        }
    }

    /// Its points' words, each x then y, in the order a path holds them.
    pub(crate) fn point_words(self) -> impl Iterator<Item = u32> {
        self.points()
            .into_iter()
            .flatten()
            .flat_map(|p| [p.x.cast_unsigned(), p.y.cast_unsigned()])
    }

    /// The element with each point mapped by `map`.
    pub(crate) fn try_map<E>(
        self,
        mut map: impl FnMut(Point) -> Result<Point, E>,
    ) -> Result<Self, E> {
        Ok(match self {
            Self::Close { gap } => Self::Close { gap },
            Self::Curve { c1, c2, to } => Self::Curve {
                c1: map(c1)?,
                c2: map(c2)?,
                to: map(to)?,
            },
            Self::Move { to, winds } => Self::Move {
                to: map(to)?,
                winds,
            },
            Self::Gap { to } => Self::Gap { to: map(to)? },
            Self::Line { to } => Self::Line { to: map(to)? },
        })
    }

    /// The element `code` names, its points `read` in turn; `None` for a
    /// code that is no drawing element.
    pub(crate) fn decode<E>(
        code: u32,
        mut read: impl FnMut() -> Result<Point, E>,
    ) -> Result<Option<Self>, E> {
        Ok(Some(match code {
            MOVE | MOVE_UNWOUND => Self::Move {
                to: read()?,
                winds: code == MOVE,
            },
            CLOSE_WITH_GAP | CLOSE => Self::Close {
                gap: code == CLOSE_WITH_GAP,
            },
            CURVE => Self::Curve {
                c1: read()?,
                c2: read()?,
                to: read()?,
            },
            GAP => Self::Gap { to: read()? },
            LINE => Self::Line { to: read()? },
            _ => return Ok(None),
        }))
    }
}

/// Which elements may follow which: a subpath begins at a move and ends at
/// a close or the next move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Order {
    in_subpath: bool,
}

impl Order {
    /// Whether `element` may come next, the order moved on past it if so.
    pub(crate) fn admit(&mut self, element: Element) -> bool {
        match element {
            Element::Move { .. } => {
                self.in_subpath = true;
                true
            }
            Element::Close { .. } => core::mem::replace(&mut self.in_subpath, false),
            Element::Curve { .. } | Element::Gap { .. } | Element::Line { .. } => self.in_subpath,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_element_takes_its_codes_and_its_words() {
        let p = Point::new(1, 2);
        let cases = [
            (Element::Move { to: p, winds: true }, 2, 12),
            (
                Element::Move {
                    to: p,
                    winds: false,
                },
                3,
                12,
            ),
            (Element::Close { gap: true }, 4, 4),
            (Element::Close { gap: false }, 5, 4),
            (
                Element::Curve {
                    c1: p,
                    c2: p,
                    to: p,
                },
                6,
                28,
            ),
            (Element::Gap { to: p }, 7, 12),
            (Element::Line { to: p }, 8, 12),
        ];
        for (element, code, bytes) in cases {
            assert_eq!(element.code(), code);
            assert_eq!(element.bytes(), bytes);
            let mut points = element.points().into_iter().flatten();
            let decoded = Element::decode::<()>(code, || Ok(points.next().unwrap_or_default()));
            assert_eq!(decoded, Ok(Some(element)));
        }
        assert_eq!(Element::decode::<()>(9, || Ok(p)), Ok(None));
        assert_eq!(Element::decode::<()>(0, || Ok(p)), Ok(None));
    }

    #[test]
    fn segments_and_closes_need_a_subpath_and_a_close_ends_one() {
        let p = Point::default();
        let line = Element::Line { to: p };
        let close = Element::Close { gap: false };
        let mut order = Order::default();
        assert!(!order.admit(line), "a line before any move");
        assert!(!order.admit(close));
        assert!(order.admit(Element::Move {
            to: p,
            winds: false
        }));
        assert!(order.admit(line));
        assert!(order.admit(Element::Gap { to: p }));
        assert!(order.admit(close));
        assert!(!order.admit(close), "a second close");
        assert!(!order.admit(line), "a line after a close");
        assert!(order.admit(Element::Move { to: p, winds: true }));
        assert!(
            order.admit(Element::Move { to: p, winds: true }),
            "a move after a move"
        );
    }
}
