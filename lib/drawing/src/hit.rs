//! Which objects a point or a rectangle meets, as an editor picks them.
//!
//! A point falls on an object where its fill covers it, or within reach of
//! its outline: half its stroke's width and the tolerance, or the tolerance
//! alone where nothing strokes it, so an outline with no paint can still be
//! picked. Hidden objects are passed over; a clipped group is met only
//! within its clip. Every walk skips the chunks of children whose bounds
//! cannot meet what is asked.

use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Path, Point};
use tairix_util::fallible;
use tairix_util::mathf::fmax;

use crate::drawing::{Drawing, NodePath};
use crate::node::{Kind, Node};
use crate::text::Glyphs;
use crate::DrawingError;

/// The topmost visible object `point`, in the drawing's space, falls on,
/// within `tolerance` of its outline: the path to it from its layer.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the path or an outline cannot be held.
pub fn hit(
    drawing: &Drawing,
    point: Point,
    tolerance: f64,
    glyphs: &dyn Glyphs,
) -> Result<Option<NodePath>, DrawingError> {
    let layers = drawing.layers();
    for index in (0..layers.len()).rev() {
        let Some(layer) = layers.get(index) else {
            continue;
        };
        let mut path = Vec::new();
        if !fallible::push(&mut path, index) {
            return Err(DrawingError::OutOfMemory);
        }
        if hits(layer, point, tolerance, glyphs, &mut path)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Whether `point`, in `node`'s parent's space, falls on `node`; the path
/// below it is pushed onto `path` for a group.
fn hits(
    node: &Node,
    point: Point,
    tolerance: f64,
    glyphs: &dyn Glyphs,
    path: &mut NodePath,
) -> Result<bool, DrawingError> {
    if !node.visible() || !node.bounds().inflated(tolerance).contains(point) {
        return Ok(false);
    }
    let Some(back) = node.transform().invert() else {
        return Ok(false);
    };
    let local = back.map(point);
    // The tolerance is a distance in the parent's space.
    let reach = tolerance / fmax(node.transform().max_scale(), f64::MIN_POSITIVE);
    match node.kind() {
        Kind::Group { children, clip } => {
            if let Some(clip) = clip {
                if !covers(clip, local, glyphs)? {
                    return Ok(false);
                }
            }
            let near = Bounds::of_point(local).inflated(reach);
            let mut candidates = Vec::new();
            for (index, _) in children.meeting(near) {
                if !fallible::push(&mut candidates, index) {
                    return Err(DrawingError::OutOfMemory);
                }
            }
            for index in candidates.into_iter().rev() {
                let Some(child) = children.get(index) else {
                    continue;
                };
                if !fallible::push(path, index) {
                    return Err(DrawingError::OutOfMemory);
                }
                if hits(child, local, reach, glyphs, path)? {
                    return Ok(true);
                }
                path.pop();
            }
            Ok(false)
        }
        Kind::Shape { shape, style } => {
            let outline = shape.to_path()?;
            Ok(falls_on(&outline, style, local, reach))
        }
        Kind::Text { text, .. } => {
            // Text is met anywhere on the line it sets, not only on its ink.
            let setting = text.set(glyphs)?;
            if setting.glyphs.is_empty() {
                return Ok(text.estimated_bounds().inflated(reach).contains(local));
            }
            Ok(setting.glyphs.iter().any(|placed| {
                let cell = Bounds::new(
                    Point::new(0.0, -1.0),
                    Point::new(placed.glyph.advance(), 0.25),
                );
                cell.transformed(placed.at).inflated(reach).contains(local)
            }))
        }
        // A picture is met anywhere on its rectangle, a transparent pixel
        // included: it is one object, and its edge is what a reader aims at.
        Kind::Image { image } => Ok(image.place().inflated(reach).contains(local)),
    }
}

/// Whether `point` falls on `outline` painted as `style`.
fn falls_on(outline: &Path, style: &crate::style::Style, point: Point, reach: f64) -> bool {
    if style.fill.paints() && outline.contains(point, style.fill_rule) {
        return true;
    }
    let pen = if style.stroke.paints() {
        style.stroke.width / 2.0
    } else {
        0.0
    };
    within(outline, point, pen + reach)
}

/// Whether `point` lies within `distance` of `outline`.
fn within(outline: &Path, point: Point, distance: f64) -> bool {
    let near = Bounds::of_point(point).inflated(distance);
    let squared = distance * distance;
    outline
        .subpaths
        .iter()
        .flat_map(tairix_bezier::Subpath::filled_segments)
        .filter(|segment| segment.bounds().intersects(near))
        .any(|segment| segment.nearest(point).1 <= squared)
}

/// Whether a clip covers `point`, in the clipped group's space.
fn covers(clip: &Node, point: Point, glyphs: &dyn Glyphs) -> Result<bool, DrawingError> {
    if !clip.visible() || !clip.bounds().contains(point) {
        return Ok(false);
    }
    let Some(back) = clip.transform().invert() else {
        return Ok(false);
    };
    let local = back.map(point);
    match clip.kind() {
        Kind::Group {
            children,
            clip: inner,
        } => {
            if let Some(inner) = inner {
                if !covers(inner, local, glyphs)? {
                    return Ok(false);
                }
            }
            for (_, child) in children.meeting(Bounds::of_point(local)) {
                if covers(child, local, glyphs)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Kind::Shape { shape, style } => Ok(shape.to_path()?.contains(local, style.fill_rule)),
        Kind::Text { text, .. } => Ok(text.set(glyphs)?.covers(local)),
        Kind::Image { image } => Ok(image.covers(local)),
    }
}

/// Every visible object among `children`, those within `area` in their
/// parent's space — wholly, or `touching` it at all — as their index.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the list cannot be held.
pub fn within_area(
    children: &crate::children::Children,
    to_parent: Affine,
    area: Bounds,
    touching: bool,
) -> Result<Vec<usize>, DrawingError> {
    let Some(back) = to_parent.invert() else {
        return Ok(Vec::new());
    };
    let local = area.transformed(back);
    let mut found = Vec::new();
    for (index, child) in children.meeting(local) {
        if !child.visible() {
            continue;
        }
        let bounds = child.bounds().transformed(to_parent);
        let taken = if touching {
            bounds.intersects(area)
        } else {
            area.contains_bounds(bounds)
        };
        if taken && !fallible::push(&mut found, index) {
            return Err(DrawingError::OutOfMemory);
        }
    }
    Ok(found)
}

#[cfg(test)]
#[path = "hit_tests.rs"]
mod tests;
