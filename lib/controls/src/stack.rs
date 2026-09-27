//! Where a column of stacked plates is drawn, and the gap between them.
//!
//! A surface that stacks plates down a column — a settings pane its groups, a
//! storage pane its volume cards, a Properties window its sections — places
//! them through this one definition, read by its measurement, its paint and
//! its hit test alike. The column is laid out whole, each plate at its natural
//! size from the top; an owner whose column outgrows the space it has shows it
//! through a [`ScrollView`](crate::ScrollView), which is what lets the column
//! rest at any pixel with the plates at its edges cut rather than dropped.

use alloc::vec::Vec;

use tairix_geometry::{to_i32, Rect, Scale};
use tairix_theme::Theme;

/// The gap between stacked plates, and between them and the column's own
/// edges: the theme's control gap, so a column breathes at whatever density
/// the desktop is drawn at.
#[must_use]
pub fn gap(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1)
}

/// The width a plate takes in a column `width` pixels wide: the column less
/// the gap either side of it.
///
/// Read by [`place`] and by whatever measures a plate's height — a group's
/// rows wrap into that width, so measuring against a different one would
/// reserve the wrong height.
#[must_use]
pub fn plate_width(width: u32, scale: Scale, theme: &Theme) -> u32 {
    width.saturating_sub(gap(scale, theme).saturating_mul(2))
}

/// The column width a plate `plate` pixels wide needs: the inverse of
/// [`plate_width`], for an owner sizing its surface from what a plate asks
/// for.
#[must_use]
pub fn column_width(plate: u32, scale: Scale, theme: &Theme) -> u32 {
    plate.saturating_add(gap(scale, theme).saturating_mul(2))
}

/// The height a column needs to seat plates of the given `heights` whole:
/// each plate, the gap above each, and one beneath the last — exactly what
/// [`place`] lays them out in.
#[must_use]
pub fn height(heights: impl IntoIterator<Item = u32>, scale: Scale, theme: &Theme) -> u32 {
    let gap = gap(scale, theme);
    heights.into_iter().fold(gap, |total, plate| {
        total.saturating_add(plate).saturating_add(gap)
    })
}

/// Where each of `count` plates, whose heights `height` answers, is drawn
/// down `bounds` — at its natural size, from the top, with the gap between
/// each and around the whole.
#[must_use]
pub fn place(
    bounds: Rect,
    count: usize,
    scale: Scale,
    theme: &Theme,
    height: impl Fn(usize) -> u32,
) -> Vec<(usize, Rect)> {
    let gap = gap(scale, theme);
    let width = plate_width(bounds.width, scale, theme);
    let mut top = bounds.top().saturating_add(to_i32(gap));
    let mut placed = Vec::with_capacity(count);
    for index in 0..count {
        let plate = height(index);
        placed.push((
            index,
            Rect::new(bounds.left().saturating_add(to_i32(gap)), top, width, plate),
        ));
        top = top.saturating_add(to_i32(plate.saturating_add(gap)));
    }
    placed
}
