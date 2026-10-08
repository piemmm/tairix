//! A complete, replaceable set of pointer cursors — one [`VectorCursor`] per
//! [`CursorKind`] — and the built-in default set.
//!
//! A [`CursorTheme`] is the cursor analogue of `lib/theme`'s palette: a fixed
//! record with one cursor per kind, so a lookup can never miss. Because it is
//! plain data built from [`VectorCursor`]s, an entirely different cursor set
//! is just a different `CursorTheme`, with no change to the window manager.
//!
//! The built-in set ([`CursorTheme::builtin`]) draws each cursor as a light
//! body inside a dark [`Outline`], so it stays legible on any background, and
//! the busy cursor's ring carries a coloured arc.

use alloc::vec::Vec;

use tairix_raster::{Color, FillRule, Layer, Paint};
use tairix_svg::pathdata::parse_path_data;
use tairix_theme::CursorKind;
use tairix_util::mathf::round_i32;

use crate::store::CURSOR_BASE_SIDE_PX;
use crate::vector::{Outline, Shape, VectorCursor};

/// Design units per logical pixel of the reference side.
///
/// The built-in art is authored in those pixels. A pixel is split this finely
/// so a curve or a diagonal lands where the drawing puts it rather than on
/// the nearest whole pixel.
const UNITS_PER_PX: u32 = 64;

/// The design grid every built-in cursor is authored on: the reference side.
const DESIGN: u32 = CURSOR_BASE_SIDE_PX * UNITS_PER_PX;

/// The dark rim each built-in cursor is drawn inside, one pixel at the
/// reference side.
const OUTLINE: Outline = Outline {
    color: Color::rgb(24, 24, 32),
    width: UNITS_PER_PX,
};

/// The light body, legible against the rim and against a dark background.
const BODY: Color = Color::rgb(250, 250, 252);

/// The busy ring's moving arc.
const BUSY_ARC: Color = Color::rgb(36, 120, 232);

/// The furthest a flattened curve of the built-in art departs from the true
/// one, in logical pixels: a tenth of a pixel on a pointer drawn at ten times
/// its reference size.
const FLATNESS_PX: f64 = 0.01;

/// The most points one built-in part may flatten to.
///
/// A containment bound, not a capacity: the parts flatten to a few hundred
/// points, so this only stops a defect in the art from allocating without
/// end.
const MAX_PART_POINTS: usize = 4096;

/// One [`VectorCursor`] per [`CursorKind`].
///
/// Stored as fixed fields rather than a map so every kind is always defined
/// and [`cursor`](Self::cursor) is total.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorTheme {
    arrow: VectorCursor,
    text: VectorCursor,
    pointer: VectorCursor,
    move_: VectorCursor,
    busy: VectorCursor,
    resize_horizontal: VectorCursor,
    resize_vertical: VectorCursor,
    resize_diagonal_rising: VectorCursor,
    resize_diagonal_falling: VectorCursor,
    crosshair: VectorCursor,
    drag_copy: VectorCursor,
    drag_move: VectorCursor,
}

impl CursorTheme {
    /// Construct a cursor theme by asking `cursor` for the artwork of every
    /// [`CursorKind`] in turn.
    ///
    /// The set is built from the kind rather than from an argument list so a
    /// caller cannot silently mis-order two cursors, and so adding a kind is a
    /// compile error here rather than a shape shown for the wrong pointer.
    #[must_use]
    pub fn from_cursors<F>(mut cursor: F) -> Self
    where
        F: FnMut(CursorKind) -> VectorCursor,
    {
        Self {
            arrow: cursor(CursorKind::Arrow),
            text: cursor(CursorKind::Text),
            pointer: cursor(CursorKind::Pointer),
            move_: cursor(CursorKind::Move),
            busy: cursor(CursorKind::Busy),
            resize_horizontal: cursor(CursorKind::ResizeHorizontal),
            resize_vertical: cursor(CursorKind::ResizeVertical),
            resize_diagonal_rising: cursor(CursorKind::ResizeDiagonalRising),
            resize_diagonal_falling: cursor(CursorKind::ResizeDiagonalFalling),
            crosshair: cursor(CursorKind::Crosshair),
            drag_copy: cursor(CursorKind::DragCopy),
            drag_move: cursor(CursorKind::DragMove),
        }
    }

    /// The cursor for `kind`. Total — every kind always resolves.
    #[must_use]
    pub fn cursor(&self, kind: CursorKind) -> &VectorCursor {
        match kind {
            CursorKind::Arrow => &self.arrow,
            CursorKind::Text => &self.text,
            CursorKind::Pointer => &self.pointer,
            CursorKind::Move => &self.move_,
            CursorKind::Busy => &self.busy,
            CursorKind::ResizeHorizontal => &self.resize_horizontal,
            CursorKind::ResizeVertical => &self.resize_vertical,
            CursorKind::ResizeDiagonalRising => &self.resize_diagonal_rising,
            CursorKind::ResizeDiagonalFalling => &self.resize_diagonal_falling,
            CursorKind::Crosshair => &self.crosshair,
            CursorKind::DragCopy => &self.drag_copy,
            CursorKind::DragMove => &self.drag_move,
        }
    }

    /// The built-in default cursor set: a light body inside a dark outline
    /// for every kind, with a coloured arc on the busy ring.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_cursors(builtin_cursor)
    }
}

/// The built-in artwork for `kind`.
fn builtin_cursor(kind: CursorKind) -> VectorCursor {
    match kind {
        CursorKind::Arrow => outlined(ARROW_TIP, ARROW),
        CursorKind::Text => outlined(CENTRE, I_BEAM),
        CursorKind::Pointer => builtin_pointer(),
        CursorKind::Move => outlined(CENTRE, MOVE),
        CursorKind::Busy => builtin_busy(),
        CursorKind::ResizeHorizontal => outlined(CENTRE, RESIZE_HORIZONTAL),
        CursorKind::ResizeVertical => outlined(CENTRE, RESIZE_VERTICAL),
        CursorKind::ResizeDiagonalRising => outlined(CENTRE, RESIZE_DIAGONAL_RISING),
        CursorKind::ResizeDiagonalFalling => outlined(CENTRE, RESIZE_DIAGONAL_FALLING),
        CursorKind::Crosshair => builtin_crosshair(),
        CursorKind::DragCopy => badged(COPY_MARK),
        CursorKind::DragMove => badged(MOVE_MARK),
    }
}

/// The centre of the reference box, where every symmetric cursor pivots.
const CENTRE: (f64, f64) = (16.0, 16.0);

/// The arrow's hotspot: the pixel its rim rounds over at the tip, which sits
/// one outline width above and left of the body's own point.
const ARROW_TIP: (f64, f64) = (1.0, 1.0);

/// The arrow: a vertical left edge, a 45-degree leading edge, and a tail
/// kinked out of the notch between them.
const ARROW: &[(f64, f64)] = &[
    (2.0, 2.0),
    (2.0, 17.0),
    (5.6, 13.4),
    (8.0, 19.1),
    (10.3, 18.1),
    (8.0, 12.6),
    (12.6, 12.6),
];

/// The I-beam shown over editable text: a stem and two serifs.
const I_BEAM: &[(f64, f64)] = &[
    (12.0, 7.0),
    (20.0, 7.0),
    (20.0, 9.0),
    (17.0, 9.0),
    (17.0, 23.0),
    (20.0, 23.0),
    (20.0, 25.0),
    (12.0, 25.0),
    (12.0, 23.0),
    (15.0, 23.0),
    (15.0, 9.0),
    (12.0, 9.0),
];

/// The four-way move cursor: a cross whose arms end in right-angled heads.
///
/// The heads are kept short beside the arms, so the gaps between them stay
/// open: heads reaching along the whole diagonal between two tips would close
/// the cross into a diamond.
const MOVE: &[(f64, f64)] = &[
    (16.0, 5.0),
    (19.0, 8.0),
    (17.0, 8.0),
    (17.0, 15.0),
    (24.0, 15.0),
    (24.0, 13.0),
    (27.0, 16.0),
    (24.0, 19.0),
    (24.0, 17.0),
    (17.0, 17.0),
    (17.0, 24.0),
    (19.0, 24.0),
    (16.0, 27.0),
    (13.0, 24.0),
    (15.0, 24.0),
    (15.0, 17.0),
    (8.0, 17.0),
    (8.0, 19.0),
    (5.0, 16.0),
    (8.0, 13.0),
    (8.0, 15.0),
    (15.0, 15.0),
    (15.0, 8.0),
    (13.0, 8.0),
];

/// The resize double arrows, one per axis a window edge can be dragged along.
///
/// Each is one closed ring — a head at either end joined by a thin shaft —
/// centred on the box so the hotspot sits at its middle. The vertical arrow
/// is the horizontal one transposed, and the falling diagonal is the rising
/// one mirrored about the centre column. A diagonal's heads are right-angled
/// corners whose two sides are level and upright, so they land on whole
/// pixels as crisply as a straight arrow's do. The unit tests hold the
/// rasterised coverage to those relations, and to a half turn about the
/// hotspot leaving each unchanged: a resize edge drags either way.
const RESIZE_HORIZONTAL: &[(f64, f64)] = &[
    (6.0, 16.0),
    (10.0, 12.0),
    (10.0, 15.0),
    (22.0, 15.0),
    (22.0, 12.0),
    (26.0, 16.0),
    (22.0, 20.0),
    (22.0, 17.0),
    (10.0, 17.0),
    (10.0, 20.0),
];

/// The up-down double arrow: [`RESIZE_HORIZONTAL`] transposed.
const RESIZE_VERTICAL: &[(f64, f64)] = &[
    (16.0, 6.0),
    (12.0, 10.0),
    (15.0, 10.0),
    (15.0, 22.0),
    (12.0, 22.0),
    (16.0, 26.0),
    (20.0, 22.0),
    (17.0, 22.0),
    (17.0, 10.0),
    (20.0, 10.0),
];

/// The bottom-left/top-right double arrow. Its shaft is two pixels across
/// measured square to it, like the straight arrows' shafts.
const RESIZE_DIAGONAL_RISING: &[(f64, f64)] = &[
    (23.0, 9.0),
    (23.0, 15.0),
    (20.707, 12.707),
    (12.707, 20.707),
    (15.0, 23.0),
    (9.0, 23.0),
    (9.0, 17.0),
    (11.293, 19.293),
    (19.293, 11.293),
    (17.0, 9.0),
];

/// The top-left/bottom-right double arrow: [`RESIZE_DIAGONAL_RISING`]
/// mirrored.
const RESIZE_DIAGONAL_FALLING: &[(f64, f64)] = &[
    (9.0, 9.0),
    (9.0, 15.0),
    (11.293, 12.707),
    (19.293, 20.707),
    (17.0, 23.0),
    (23.0, 23.0),
    (23.0, 17.0),
    (20.707, 19.293),
    (12.707, 11.293),
    (15.0, 9.0),
];

/// A pointing hand for clickable controls: an upright index finger, three
/// folded fingers and a thumb, one layer each, in SVG path data. The folded
/// fingers stand apart by less than the rim is wide, so the rim fills the
/// gaps and draws the lines between them.
const HAND: &[&str] = &[
    "M9 5 A2 2 0 0 1 13 5 V19 H9 Z",
    "M13.9 11 A1.4 1.4 0 0 1 16.7 11 V19 H13.9 Z",
    "M17.6 12 A1.4 1.4 0 0 1 20.4 12 V19 H17.6 Z",
    "M21.3 13.3 A1.2 1.2 0 0 1 23.7 13.3 V19 H21.3 Z",
    "M9 15 H23.7 V22 A3 3 0 0 1 20.7 25 H12 A3 3 0 0 1 9 22 Z",
    "M4.505 16.567 L9.405 21.167 A1.6 1.6 0 0 0 11.595 18.833 \
     L6.695 14.233 A1.6 1.6 0 0 0 4.505 16.567 Z",
];

/// The pointing hand's hotspot: the top of the index finger's rim.
const FINGERTIP: (f64, f64) = (11.0, 2.0);

/// The busy cursor's light ring: two circles, the inner one the hole.
const BUSY_RING: &str = "M25 16 A9 9 0 1 1 7 16 A9 9 0 1 1 25 16 Z \
                         M21 16 A5 5 0 1 1 11 16 A5 5 0 1 1 21 16 Z";

/// The coloured arc on the busy ring, from twelve o'clock round a third of
/// the ring and a little more.
const BUSY_SWEEP: &str = "M16 7 A9 9 0 0 1 23.281 21.290 L20.045 18.939 A5 5 0 0 0 16 11 Z";

/// The crosshair's four arms, two pixels across, stopping three pixels short
/// of the centre so the pixel being picked out stays in view.
const CROSSHAIR: &str = "M15 5 H17 V13 H15 Z M15 19 H17 V27 H15 Z \
                         M5 15 H13 V17 H5 Z M19 15 H27 V17 H19 Z";

/// The badge a drag cursor wears below and right of its arrow, clear of it.
const BADGE: &str = "M28 22 A6 6 0 1 1 16 22 A6 6 0 1 1 28 22 Z";

/// The badge's ground: the busy arc's blue, so the two drag cursors read as
/// the system's own marks.
const BADGE_GROUND: Color = BUSY_ARC;

/// The copy badge's plus.
const COPY_MARK: &str = "M21 18 H23 V21 H26 V23 H23 V26 H21 V23 H18 V21 H21 Z";

/// The move badge's arrow, pointing on.
const MOVE_MARK: &str = "M18 21 H21.5 V18.5 L26 22 L21.5 25.5 V23 H18 Z";

/// The arrow with a badge bearing `mark`, pivoting on the arrow's tip.
fn badged(mark: &str) -> VectorCursor {
    let arrow = Layer::filled(
        Paint::Solid(BODY),
        FillRule::NonZero,
        alloc::vec![grid(ARROW)],
    );
    let badge = Layer::filled(
        Paint::Solid(BADGE_GROUND),
        FillRule::NonZero,
        contours(BADGE),
    );
    let mark = Layer::filled(Paint::Solid(BODY), FillRule::NonZero, contours(mark));
    VectorCursor::new(
        DESIGN,
        units(ARROW_TIP.0),
        units(ARROW_TIP.1),
        alloc::vec![arrow, badge, mark],
    )
    .with_outline(OUTLINE)
}

/// The crosshair, pivoting on the open centre between its arms.
fn builtin_crosshair() -> VectorCursor {
    let arms = Layer::filled(Paint::Solid(BODY), FillRule::NonZero, contours(CROSSHAIR));
    VectorCursor::new(DESIGN, units(CENTRE.0), units(CENTRE.1), alloc::vec![arms])
        .with_outline(OUTLINE)
}

/// The pointing hand, one layer per part of [`HAND`].
fn builtin_pointer() -> VectorCursor {
    let parts = HAND
        .iter()
        .map(|part| Layer::filled(Paint::Solid(BODY), FillRule::NonZero, contours(part)))
        .collect();
    VectorCursor::new(DESIGN, units(FINGERTIP.0), units(FINGERTIP.1), parts).with_outline(OUTLINE)
}

/// The busy cursor: a light ring with a coloured arc on it, pivoting on the
/// centre.
fn builtin_busy() -> VectorCursor {
    let ring = Layer::filled(Paint::Solid(BODY), FillRule::EvenOdd, contours(BUSY_RING));
    let sweep = Layer::filled(
        Paint::Solid(BUSY_ARC),
        FillRule::NonZero,
        contours(BUSY_SWEEP),
    );
    VectorCursor::new(
        DESIGN,
        units(CENTRE.0),
        units(CENTRE.1),
        alloc::vec![ring, sweep],
    )
    .with_outline(OUTLINE)
}

/// A light body of `silhouette` inside the built-in rim, with its hotspot at
/// `hotspot`.
fn outlined(hotspot: (f64, f64), silhouette: &[(f64, f64)]) -> VectorCursor {
    let body = Shape::from_points(BODY, &grid(silhouette));
    VectorCursor::new(
        DESIGN,
        units(hotspot.0),
        units(hotspot.1),
        alloc::vec![body],
    )
    .with_outline(OUTLINE)
}

/// `points`, in logical pixels, on the design grid.
fn grid(points: &[(f64, f64)]) -> Vec<(i32, i32)> {
    points.iter().map(|&(x, y)| (units(x), units(y))).collect()
}

/// A logical-pixel coordinate on the design grid.
fn units(px: f64) -> i32 {
    round_i32(px * f64::from(UNITS_PER_PX))
}

/// SVG path data in logical pixels, flattened through `lib/svg`'s one
/// flattener and put on the design grid.
///
/// The paths are this crate's own and a unit test parses each, so the empty
/// drawing a malformed one would leave is never reached.
fn contours(path: &str) -> Vec<Vec<(i32, i32)>> {
    parse_path_data(path, FLATNESS_PX, MAX_PART_POINTS, None)
        .unwrap_or_default()
        .iter()
        .map(|subpath| grid(&subpath.points))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_path_data, BADGE, BUSY_RING, BUSY_SWEEP, COPY_MARK, FLATNESS_PX, HAND,
        MAX_PART_POINTS, MOVE_MARK,
    };

    #[test]
    fn every_built_in_path_is_well_formed() {
        for path in HAND
            .iter()
            .chain([&BUSY_RING, &BUSY_SWEEP, &BADGE, &COPY_MARK, &MOVE_MARK])
        {
            let parts = parse_path_data(path, FLATNESS_PX, MAX_PART_POINTS, None)
                .unwrap_or_else(|err| panic!("{path}: {err:?}"));
            assert!(parts.iter().all(|part| part.points.len() >= 3), "{path}");
        }
    }
}
