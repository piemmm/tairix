//! The marquee: a band dragged across the listing's ground that selects every
//! entry it touches as it grows.
//!
//! The band's anchor is held in layout coordinates, so scrolling while it is
//! held grows it; its head is the pointer, in window coordinates, clamped to
//! the item area. It selects an entry whose body it touches — a grid tile's
//! picture and name, not the ground around them. Which tiles it surely touches
//! is arithmetic over the view's lines and slots, only the tiles along its
//! edges are tested one by one, and a sweep changes only the entries entering
//! or leaving the band, so a band costs what changes rather than what it
//! covers.

use tairix_controls::scroll::{ScrollOrientation, ScrollView};
use tairix_controls::{blend_area, fill_area};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::browser::Browser;
use crate::layout::BandMarks;
use crate::render::{entry_body, tile_core, view_layout_for, Frame};
use crate::select::Selection;
use crate::source::DirectorySource;

/// How deep, in logical pixels, the strip at each end of the item area is in
/// which a held band scrolls the listing.
const SCROLL_ZONE: u32 = 24;

/// How fast a band held at the very end of the item area scrolls the listing,
/// in logical pixels a second.
const SCROLL_SPEED: u32 = 1200;

/// A band being dragged across the listing.
#[derive(Clone, Debug)]
pub struct Marquee {
    anchor: Point,
    head: Point,
    base: Selection,
    touched: BandMarks,
    /// What a sweep builds the next marks in, so a step allocates nothing.
    next: BandMarks,
}

impl Marquee {
    /// The pointer end of the band, in window coordinates.
    #[must_use]
    pub const fn head(&self) -> Point {
        self.head
    }
}

/// Begin a band at window `point` on the listing's ground, adding to the
/// selection as it stands. `None` when `point` is outside the item area.
#[must_use]
pub fn begin<S: DirectorySource>(
    browser: &Browser<S>,
    frame: Frame<'_>,
    point: Point,
) -> Option<Marquee> {
    let anchor = scrolled(browser, frame).to_content(point)?;
    Some(Marquee {
        anchor,
        head: point,
        base: browser.selection().clone(),
        touched: BandMarks::default(),
        next: BandMarks::default(),
    })
}

/// Grow `marquee` to window `head`: every entry the band touches is selected,
/// and one it no longer touches is released unless the band began with it.
/// Answers whether any entry's mark changed.
pub fn sweep<S: DirectorySource>(
    browser: &mut Browser<S>,
    frame: Frame<'_>,
    marquee: &mut Marquee,
    head: Point,
) -> bool {
    marquee.head = head;
    let layout = view_layout_for(
        browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let view = layout.view(browser.scroll_offset());
    let band = band_in_layout(&view, marquee);
    let core = layout.band_cells_within(band, tile_core(frame.scale, frame.theme));
    let cells = layout.band_cells(band);
    let entries = browser.entries();
    marquee.next.rebuild(core, &cells, |index| {
        let body = layout
            .layout_rect(index)
            .zip(entries.get(index))
            .and_then(|(cell, entry)| entry_body(entry, cell, frame.scale, frame.theme));
        body.is_some_and(|body| !body.intersection(&band).is_empty())
    });
    let changed = browser.move_band(&marquee.base, &marquee.touched, &marquee.next);
    core::mem::swap(&mut marquee.touched, &mut marquee.next);
    changed
}

/// Scroll the listing `delta` pixels and grow the band to where its held head
/// now lies, answering whether the listing moved.
pub fn step<S: DirectorySource>(
    browser: &mut Browser<S>,
    frame: Frame<'_>,
    marquee: &mut Marquee,
    delta: i64,
) -> bool {
    let layout = view_layout_for(
        browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let model = layout.scroll_model(browser.scroll_offset());
    let moved = model.scroll_by(delta).offset();
    if moved == model.offset() {
        return false;
    }
    browser.set_scroll_offset(moved);
    let head = marquee.head;
    sweep(browser, frame, marquee, head);
    true
}

/// Take back everything the band selected.
pub fn cancel<S: DirectorySource>(browser: &mut Browser<S>, marquee: Marquee) {
    browser.restore_selection(marquee.base);
}

/// How fast the listing scrolls under a held band, in pixels of offset a
/// second: non-zero while the head is in the strip at either end of the item
/// area and the listing can still move that way, faster the deeper it is.
#[must_use]
pub fn autoscroll<S: DirectorySource>(
    browser: &Browser<S>,
    frame: Frame<'_>,
    marquee: &Marquee,
) -> i64 {
    let layout = view_layout_for(
        browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let model = layout.scroll_model(browser.scroll_offset());
    // The head is a window point, so the strips are measured on the window
    // rectangle the items show through, not on the scrolled content.
    let shown = layout.view(model.offset()).viewport();
    let (start, end, at) = match layout.axis() {
        ScrollOrientation::Vertical => (shown.top(), shown.bottom(), marquee.head.y),
        ScrollOrientation::Horizontal => (shown.left(), shown.right(), marquee.head.x),
    };
    let (start, end, at) = (i64::from(start), i64::from(end), i64::from(at));
    // The strips may not meet, so a short item area keeps a middle where the
    // band rests.
    let zone = i64::from(frame.scale.scale_length(SCROLL_ZONE)).min((end - start) / 3);
    if zone <= 0 {
        return 0;
    }
    let speed = i64::from(frame.scale.scale_length(SCROLL_SPEED));
    let toward_start = start + zone - at;
    let toward_end = at - (end - zone) + 1;
    let screen = if toward_start > 0 {
        -(speed * toward_start.min(zone) / zone)
    } else if toward_end > 0 {
        speed * toward_end.min(zone) / zone
    } else {
        0
    };
    let offset_speed = if layout.scrolls_toward_the_leading_edge() {
        -screen
    } else {
        screen
    };
    let can_move = match offset_speed.signum() {
        -1 => model.offset() > 0,
        1 => model.offset() < model.range().max_offset(),
        _ => false,
    };
    if can_move {
        offset_speed
    } else {
        0
    }
}

/// The band's window rectangle, cut to the item area, or `None` when none of
/// it shows.
#[must_use]
pub fn band<S: DirectorySource>(
    browser: &Browser<S>,
    frame: Frame<'_>,
    marquee: &Marquee,
) -> Option<Rect> {
    let view = scrolled(browser, frame);
    view.to_window(band_in_layout(&view, marquee))
}

/// Report the band's own pixels that moving it from `before` to `after`
/// repainted: what only one of them covers, and both outlines. The entries
/// whose marks changed are the listing's to report.
pub fn report_band(before: Option<Rect>, after: Option<Rect>, scale: Scale, damage: &mut Region) {
    if before == after {
        return;
    }
    let mut moved = Region::new();
    for band in [before, after].into_iter().flatten() {
        moved.add(band);
    }
    // Inside both outlines the tint lies over the same entries as before.
    if let (Some(before), Some(after)) = (before, after) {
        let edge = outline(scale);
        moved.subtract(before.inset(edge).intersection(&after.inset(edge)));
    }
    for rect in moved.rects() {
        damage.add(*rect);
    }
}

/// Paint the band: the theme's selection tint across it, outlined in the
/// accent.
pub fn draw(surface: &mut Surface, band: Rect, scale: Scale, theme: &Theme) {
    let palette = theme.palette();
    blend_area(surface, band, palette.selection_fill);
    let edge = outline(scale);
    let mut inner = band;
    for side in [
        inner.take_top(edge),
        inner.take_bottom(edge),
        inner.take_left(edge),
        inner.take_right(edge),
    ] {
        fill_area(surface, side, palette.accent.into());
    }
}

/// The outline's thickness.
fn outline(scale: Scale) -> u32 {
    scale.scale_length(1).max(1)
}

/// The item area, scrolled to the browser's offset.
fn scrolled<S: DirectorySource>(browser: &Browser<S>, frame: Frame<'_>) -> ScrollView {
    view_layout_for(
        browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    )
    .view(browser.scroll_offset())
}

/// The band in layout coordinates: from its anchor to its head, the head held
/// inside the item area.
fn band_in_layout(view: &ScrollView, marquee: &Marquee) -> Rect {
    let area = view.viewport();
    if area.is_empty() {
        return Rect::EMPTY;
    }
    let held = Point::new(
        marquee.head.x.clamp(area.left(), area.right() - 1),
        marquee.head.y.clamp(area.top(), area.bottom() - 1),
    );
    view.to_content(held)
        .map_or(Rect::EMPTY, |head| spanning(marquee.anchor, head))
}

/// The smallest rectangle holding both `a` and `b`.
fn spanning(a: Point, b: Point) -> Rect {
    let span = |p: i32, q: i32| {
        let (low, high) = (p.min(q), p.max(q));
        let len = u32::try_from(i64::from(high) - i64::from(low) + 1).unwrap_or(u32::MAX);
        (low, len)
    };
    let (x, width) = span(a.x, b.x);
    let (y, height) = span(a.y, b.y);
    Rect::new(x, y, width, height)
}

#[cfg(test)]
#[path = "marquee_tests.rs"]
mod tests;
