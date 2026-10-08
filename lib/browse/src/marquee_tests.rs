//! Tests for the marquee: what a band selects as it grows, shrinks and
//! scrolls, what Escape takes back, how fast a held band scrolls, and what a
//! moved band repaints.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_theme::Theme;

use super::{autoscroll, band, begin, cancel, report_band, step, sweep, Frame};
use crate::browser::Browser;
use crate::chrome::ToolbarBand;
use crate::entry::Entry;
use crate::layout::ViewMode;
use crate::render::{entry_rect, listing_area};
use crate::source::{DirectorySource, Listing};

/// A root holding `0` files and nothing else.
struct Flat(usize);

impl DirectorySource for Flat {
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
        if !components.is_empty() {
            return Err(Errno::NotFound);
        }
        Ok(Listing::Ready(
            (0..self.0)
                .map(|n| Entry::file(format!("f{n:04}")))
                .collect(),
        ))
    }
}

const WINDOW: Rect = Rect::new(0, 0, 480, 320);

fn browser(count: usize, view: ViewMode) -> Browser<Flat> {
    let mut browser = Browser::open_root(Flat(count)).expect("the root lists");
    browser.set_view_mode(view);
    browser
}

fn frame(theme: &Theme) -> Frame<'_> {
    Frame {
        scale: Scale::ONE,
        theme,
        viewport: WINDOW,
        toolbar: ToolbarBand::Hidden,
    }
}

fn rect_of(browser: &Browser<Flat>, frame: Frame<'_>, index: usize) -> Rect {
    entry_rect(
        browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
        index,
    )
    .expect("the entry shows")
}

fn selected(browser: &Browser<Flat>) -> Vec<usize> {
    browser.selection().iter().collect()
}

fn inside(rect: Rect, dx: i32, dy: i32) -> Point {
    Point::new(rect.left() + dx, rect.top() + dy)
}

#[test]
fn a_band_selects_every_row_it_touches_and_releases_those_it_leaves() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(50, ViewMode::List);
    let first = rect_of(&browser, frame, 0);
    let mut marquee = begin(&browser, frame, inside(first, 4, 2)).expect("in the item area");

    let fourth = rect_of(&browser, frame, 3);
    assert!(sweep(
        &mut browser,
        frame,
        &mut marquee,
        inside(fourth, 60, 1)
    ));
    assert_eq!(selected(&browser), [0, 1, 2, 3]);

    let second = rect_of(&browser, frame, 1);
    assert!(sweep(
        &mut browser,
        frame,
        &mut marquee,
        inside(second, 60, 1)
    ));
    assert_eq!(selected(&browser), [0, 1]);
    // A sweep that moves no edge across an entry changes nothing.
    assert!(!sweep(
        &mut browser,
        frame,
        &mut marquee,
        inside(second, 70, 2)
    ));
}

#[test]
fn a_band_adds_to_what_it_began_with_and_escape_takes_its_part_back() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(50, ViewMode::List);
    browser.select(9).expect("in range");
    browser.toggle_selection(2).expect("in range");
    let first = rect_of(&browser, frame, 0);
    let mut marquee = begin(&browser, frame, inside(first, 4, 2)).expect("in the item area");

    let fourth = rect_of(&browser, frame, 3);
    sweep(&mut browser, frame, &mut marquee, inside(fourth, 60, 1));
    assert_eq!(selected(&browser), [0, 1, 2, 3, 9]);
    // Shrinking past an entry the band began with leaves it selected.
    sweep(&mut browser, frame, &mut marquee, inside(first, 60, 3));
    assert_eq!(selected(&browser), [0, 2, 9]);

    cancel(&mut browser, marquee);
    assert_eq!(selected(&browser), [2, 9]);
}

#[test]
fn a_band_in_a_grid_takes_the_block_of_tiles_it_crosses_and_no_gap() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(40, ViewMode::Grid);
    let a = rect_of(&browser, frame, 0);
    let b = rect_of(&browser, frame, 1);
    let per_line = (0..40)
        .position(|index| rect_of(&browser, frame, index).top() != a.top())
        .expect("the grid wraps");

    // From inside tile 1 to inside the tile below tile 2.
    let below = rect_of(&browser, frame, per_line + 2);
    let mut marquee = begin(&browser, frame, inside(b, 3, 3)).expect("in the item area");
    sweep(&mut browser, frame, &mut marquee, inside(below, 3, 3));
    assert_eq!(selected(&browser), [1, 2, per_line + 1, per_line + 2]);

    // A band wholly within the gap between two tiles touches neither, until
    // it reaches the second.
    let gap = (a.right(), b.left());
    assert!(gap.0 < gap.1, "the tiles stand apart");
    browser.clear_selection();
    let mut marquee = begin(&browser, frame, Point::new(gap.0, a.top() + 2)).expect("in the area");
    sweep(
        &mut browser,
        frame,
        &mut marquee,
        Point::new(gap.1 - 1, a.bottom() - 2),
    );
    assert!(selected(&browser).is_empty());
    sweep(
        &mut browser,
        frame,
        &mut marquee,
        Point::new(gap.1, a.top() + 2),
    );
    assert_eq!(selected(&browser), [1]);
}

#[test]
fn scrolling_under_a_held_band_grows_it_from_its_anchor() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(200, ViewMode::List);
    let first = rect_of(&browser, frame, 0);
    let area = listing_area(
        &browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let mut marquee = begin(&browser, frame, inside(first, 4, 1)).expect("in the item area");
    let bottom = Point::new(area.left() + 40, area.bottom() - 1);
    sweep(&mut browser, frame, &mut marquee, bottom);
    let before = selected(&browser);
    let last = *before.last().expect("the band selected something");
    assert_eq!(before, (0..=last).collect::<Vec<_>>());

    let row = i64::from(first.height);
    assert!(step(&mut browser, frame, &mut marquee, row * 5));
    assert_eq!(selected(&browser), (0..=last + 5).collect::<Vec<_>>());
    // Scrolling back shrinks it again; the anchor stayed on the first row.
    assert!(step(&mut browser, frame, &mut marquee, -row * 5));
    assert_eq!(selected(&browser), before);
    // A listing that cannot move further takes no step.
    assert!(!step(&mut browser, frame, &mut marquee, -row));
}

#[test]
fn a_band_held_at_an_end_scrolls_faster_the_deeper_it_is() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(200, ViewMode::List);
    let area = listing_area(
        &browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let first = rect_of(&browser, frame, 0);
    let mut marquee = begin(&browser, frame, inside(first, 4, 1)).expect("in the item area");
    let x = area.left() + 40;

    let mut speed_at = |browser: &mut Browser<Flat>, y: i32| {
        sweep(browser, frame, &mut marquee, Point::new(x, y));
        autoscroll(browser, frame, &marquee)
    };
    assert_eq!(
        speed_at(&mut browser, area.top() + 100),
        0,
        "the middle rests"
    );
    let deepest = speed_at(&mut browser, area.bottom() - 1);
    let shallow = speed_at(&mut browser, area.bottom() - 20);
    assert!(
        deepest > shallow && shallow > 0,
        "{deepest} > {shallow} > 0"
    );
    assert_eq!(deepest, 1200, "the full speed at the very edge");
    // Pressed past the area's end is as deep as the edge.
    assert_eq!(speed_at(&mut browser, area.bottom() + 50), deepest);
    // The top strip cannot scroll a listing already at its start.
    assert_eq!(speed_at(&mut browser, area.top()), 0);
    // Scrolled, the strips stay where the window shows them.
    browser.set_scroll_offset(400);
    assert_eq!(speed_at(&mut browser, area.top()), -1200);
    assert_eq!(
        speed_at(&mut browser, area.top() + 100),
        0,
        "the middle rests"
    );
    assert_eq!(speed_at(&mut browser, area.bottom() - 1), 1200);
}

/// A band held at the bottom keeps the listing moving at the speed it asks,
/// well past the depth of the strip, not creeping to a stop once scrolled.
#[test]
fn a_band_held_at_the_bottom_keeps_scrolling() {
    let theme = Theme::dark();
    let frame = frame(&theme);
    let mut browser = browser(200, ViewMode::List);
    let area = listing_area(
        &browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    let first = rect_of(&browser, frame, 0);
    let mut marquee = begin(&browser, frame, inside(first, 4, 1)).expect("in the item area");
    sweep(
        &mut browser,
        frame,
        &mut marquee,
        Point::new(area.left() + 40, area.bottom() - 1),
    );
    for _ in 0..30 {
        let speed = autoscroll(&browser, frame, &marquee);
        assert_eq!(speed, 1200, "at offset {}", browser.scroll_offset());
        assert!(step(&mut browser, frame, &mut marquee, speed / 60));
    }
    assert_eq!(browser.scroll_offset(), 30 * 20);
}

#[test]
fn a_band_outside_the_item_area_does_not_begin() {
    let theme = Theme::dark();
    let mut frame = frame(&theme);
    frame.toolbar = ToolbarBand::Shown;
    let browser = browser(5, ViewMode::List);
    let area = listing_area(
        &browser,
        frame.scale,
        frame.theme,
        frame.viewport,
        frame.toolbar,
    );
    assert!(begin(&browser, frame, Point::new(area.left() + 2, area.top() - 1)).is_none());
}

#[test]
fn a_moved_band_repaints_its_new_ground_and_both_outlines_only() {
    let mut damage = Region::new();
    let before = Rect::new(10, 10, 100, 100);
    let after = Rect::new(10, 10, 140, 100);
    report_band(Some(before), Some(after), Scale::ONE, &mut damage);
    // The new strip and both right-hand outlines changed...
    assert!(damage.contains(Point::new(130, 50)));
    assert!(damage.contains(Point::new(109, 50)));
    assert!(damage.contains(Point::new(149, 50)));
    // ...and the shared top outline, but not the shared interior.
    assert!(damage.contains(Point::new(50, 10)));
    assert!(!damage.contains(Point::new(50, 50)));

    let mut unchanged = Region::new();
    report_band(Some(before), Some(before), Scale::ONE, &mut unchanged);
    assert!(unchanged.is_empty());

    let mut gone = Region::new();
    report_band(Some(before), None, Scale::ONE, &mut gone);
    assert!(gone.contains(Point::new(50, 50)));

    // The band drawn and the band reported agree on where it is.
    let theme = Theme::dark();
    let frame = frame(&theme);
    let browser = browser(50, ViewMode::List);
    let first = rect_of(&browser, frame, 0);
    let marquee = begin(&browser, frame, inside(first, 4, 2)).expect("in the item area");
    let shown = band(&browser, frame, &marquee).expect("a one-pixel band shows");
    assert_eq!(shown, Rect::new(first.left() + 4, first.top() + 2, 1, 1));
}
