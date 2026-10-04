//! Rasterising a [`VectorCursor`] onto a `lib/raster` [`Surface`] at any
//! pixel side.
//!
//! The side is asked for in **pixels**, not as a factor of the asset's own
//! design grid, because the grid is an authoring detail that differs
//! between a built-in cursor and a decoded SVG one: a caller naming a
//! factor would get a different pointer size from each, so swapping cursor
//! sets would resize the pointer. The caller names the size it wants and
//! every set honours it.
//!
//! The artwork is first fitted to that side's pixel grid (`crate::fit`), so
//! its upright and level edges land on pixel boundaries at every size rather
//! than only at the size the grid was authored for. A declared [`Outline`] is
//! then stroked around the fitted silhouette a whole number of pixels wide,
//! and the stack is drawn through `lib/raster`'s single artwork path, so the
//! cursor library owns no scan converter or colour arithmetic of its own. A
//! degenerate side or cursor fails closed with `None` rather than panicking.

use alloc::vec::Vec;

use tairix_geometry::saturate_i32;
use tairix_raster::{layer_count, FillRule, Layer, Node, Paint, Surface, MAX_GROUP_DEPTH};
use tairix_svg::geom::{LineCap, LineJoin, StrokeStyle, SubPath};
use tairix_svg::stroke::stroke_outline;
use tairix_util::mathf::round_i32;

use crate::fit::{Fit, FIT_UNITS};
use crate::image::CursorImage;
use crate::vector::{Outline, VectorCursor};

/// How far a rounded corner of the outline may depart from a true arc, in
/// pixels.
const OUTLINE_TOLERANCE_PX: f64 = 0.125;

/// The miter limit, in half-widths as the stroker states it, that mitres
/// every corner of a right angle or wider and leaves any sharper one round.
///
/// A right angle's miter reaches `sqrt 2` half-widths from its corner.
const RIGHT_ANGLE_MITER: f64 = 1.5;

/// The most points an outline may be built from.
///
/// A fixed containment bound, not a capacity: it caps the work and memory a
/// decoded asset's silhouette can demand of its band. A pointer's outline
/// needs a few thousand points; artwork past this draws no cursor at all
/// rather than one missing its rim.
const MAX_OUTLINE_POINTS: usize = 1 << 16;

impl VectorCursor {
    /// Rasterise this cursor into a `side`x`side` pixel image.
    ///
    /// Returns `None` for a zero `side`, a cursor whose design grid is
    /// degenerate, a pixel buffer that cannot be allocated, a group whose
    /// isolation buffer cannot be, or fitted artwork or an outline past its
    /// containment bound — the caller falls back to a smaller side or a
    /// different cursor rather than showing a half-composited one.
    ///
    /// The hotspot is always a pixel corner and the artwork is laid out
    /// from it, so artwork symmetric about its hotspot rasterises
    /// symmetrically at every side, and the stack goes through
    /// [`Surface::layered`] so the body meets its outline without the pale
    /// seam that compositing already-anti-aliased shapes leaves.
    #[must_use]
    pub fn rasterise(&self, side: u32) -> Option<CursorImage> {
        let fit = Fit::new(self, side)?;
        let body = fit.nodes(self.nodes())?;
        let mut nodes = Vec::with_capacity(body.len() + 1);
        if let Some(outline) = self.outline() {
            nodes.push(Node::Fill(band(
                &fit,
                &body,
                outline,
                self.design_size(),
                side,
            )?));
        }
        nodes.extend(body);

        let mut drawn = false;
        let surface = Surface::layered(side, side, layer_count(&nodes), |surface| {
            drawn = surface.draw_artwork(&nodes, fit.design());
        })?;
        drawn.then(|| CursorImage::new(surface, fit.hotspot()))
    }
}

/// The outline band around the fitted `body`: every contour its drawn fills
/// enclose, stroked twice the outline's pixel width, so the part outside the
/// silhouette is exactly that far from it, filled as one layer to lie beneath
/// it.
///
/// The joins are mitred up to a right angle and round past it: a corner of a
/// right angle or wider is square, as the body's own corner is, and a
/// sharper one, whose miter would reach out in a spike several rims long, is
/// round.
///
/// A group drawn through a mask is left out, since its visible edge is the
/// mask's rather than its own contours'. The stroke is taken about the
/// hotspot's corner, so a band around symmetric artwork is symmetric too.
/// `None` when the band would pass [`MAX_OUTLINE_POINTS`].
fn band(fit: &Fit, body: &[Node], outline: Outline, design: u32, side: u32) -> Option<Layer> {
    let (ax, ay) = fit.anchor();
    let about = |value: i32, anchor: i64| f64::from(saturate_i32(i64::from(value) - anchor));
    let mut silhouette = Vec::new();
    drawn_contours(body, 0, &mut |contour| {
        silhouette.push(SubPath::closed(
            contour
                .iter()
                .map(|&(x, y)| (about(x, ax), about(y, ay)))
                .collect(),
        ));
    });
    let style = StrokeStyle {
        width: 2.0 * f64::from(outline_pixels(outline, design, side)) * f64::from(FIT_UNITS),
        cap: LineCap::Butt,
        join: LineJoin::MiterOrRound,
        miter_limit: RIGHT_ANGLE_MITER,
        dashes: Vec::new(),
        dash_offset: 0.0,
    };
    let tolerance = OUTLINE_TOLERANCE_PX * f64::from(FIT_UNITS);
    let pieces = stroke_outline(&silhouette, &style, tolerance, MAX_OUTLINE_POINTS).ok()?;
    // Rounded about the anchor, half away from it, so a band around mirrored
    // artwork is mirrored to the last sub-unit.
    let placed = |offset: f64, anchor: i64| {
        saturate_i32(i64::from(round_i32(offset)).saturating_add(anchor))
    };
    let contours = pieces
        .iter()
        .map(|piece| {
            piece
                .points
                .iter()
                .map(|&(x, y)| (placed(x, ax), placed(y, ay)))
                .collect()
        })
        .collect();
    Some(Layer::filled(
        Paint::Solid(outline.color),
        FillRule::NonZero,
        contours,
    ))
}

/// How many pixels wide `outline` is at `side`: its width stretched as the
/// artwork is and rounded, never under one pixel so a rim is never lost.
fn outline_pixels(outline: Outline, design: u32, side: u32) -> u32 {
    let stretched = u64::from(outline.width) * u64::from(side);
    let design = u64::from(design.max(1));
    let pixels = (2 * stretched + design) / (2 * design);
    u32::try_from(pixels).unwrap_or(u32::MAX).max(1)
}

/// Hand every contour of `nodes`, `depth` groups deep, that encloses any
/// area to `visit`, looking into a group only when no mask shapes it and the
/// renderer would draw it.
///
/// A contour enclosing nothing draws nothing, so it has no edge to outline.
fn drawn_contours(nodes: &[Node], depth: usize, visit: &mut impl FnMut(&[(i32, i32)])) {
    for node in nodes {
        match node {
            Node::Fill(layer) => {
                for contour in layer.contours.iter().filter(|contour| encloses(contour)) {
                    visit(contour);
                }
            }
            Node::Group(group) if group.mask.is_none() && depth < MAX_GROUP_DEPTH => {
                drawn_contours(&group.children, depth + 1, visit);
            }
            Node::Group(_) => {}
        }
    }
}

/// Whether `contour` encloses any area: it has a non-zero shoelace sum.
fn encloses(contour: &[(i32, i32)]) -> bool {
    let next = contour.iter().cycle().skip(1);
    contour
        .iter()
        .zip(next)
        .map(|(&(x0, y0), &(x1, y1))| {
            i128::from(x0) * i128::from(y1) - i128::from(x1) * i128::from(y0)
        })
        .sum::<i128>()
        != 0
}

#[cfg(test)]
#[path = "raster_tests.rs"]
mod tests;
