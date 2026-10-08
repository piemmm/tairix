//! What a round moved in the listing view, as the rectangles it repainted.
//!
//! # Why a mark rather than a control report
//!
//! The listing has no retained control tree: every row and tile is built
//! afresh from the browser's own state each frame ([`tairix_browse::render`]),
//! so there is no control to report its own bounds when a highlight moves.
//! What *did* move is therefore the difference between two readings of that
//! state — which shown entries are selected, and the scroll offset — resolved
//! back to rectangles through the renderer's own geometry, so the reported
//! rectangle and the painted one are the same fact.
//!
//! Everything else the view draws (the entries themselves, the toolbar's
//! enable states, the view mode) changes only when the listing is replaced,
//! which is a whole-window repaint the caller concludes for itself.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_browse::render::{entry_rect, item_area, scrollbar_bounds, visible_range, Frame};
use tairix_browse::{Browser, DirectorySource};
use tairix_geometry::Region;

/// The listing's drawn interaction state: the marks a round can move while
/// the entries beneath them stay the same.
///
/// Only the shown entries' marks are held, so taking one costs what the window
/// shows, however large the listing or its selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMark {
    offset: u64,
    shown: Range<usize>,
    selected: Vec<bool>,
}

impl ViewMark {
    /// The listing's drawn state right now.
    #[must_use]
    pub fn of<S: DirectorySource>(browser: &Browser<S>, frame: Frame<'_>) -> Self {
        let shown = visible_range(
            browser,
            frame.scale,
            frame.theme,
            frame.viewport,
            frame.toolbar,
        );
        Self {
            offset: browser.scroll_offset(),
            selected: shown
                .clone()
                .map(|index| browser.is_selected(index))
                .collect(),
            shown,
        }
    }

    /// Report what moving from `self` to the browser's current state
    /// repainted, answering whether anything did.
    ///
    /// A scroll draws *every* entry somewhere new and moves the bar's thumb
    /// with them, so it marks the whole item area and the gutter; a selection
    /// that changed within one offset marks only the shown entries it changed.
    pub fn report<S: DirectorySource>(
        self,
        browser: &Browser<S>,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> bool {
        let now = Self::of(browser, frame);
        if now == self {
            return false;
        }
        if now.offset != self.offset || now.shown != self.shown {
            scrolled(frame, damage);
            return true;
        }
        let changed = self
            .selected
            .iter()
            .zip(&now.selected)
            .zip(self.shown)
            .filter(|((was, is), _)| was != is);
        for (_, index) in changed {
            if let Some(rect) = entry_rect(
                browser,
                frame.scale,
                frame.theme,
                frame.viewport,
                frame.toolbar,
                index,
            ) {
                damage.add(rect);
            }
        }
        true
    }
}

/// Report what a scroll of the listing repainted: every entry the view draws,
/// and the bar whose thumb moved with them.
fn scrolled(frame: Frame<'_>, damage: &mut Region) {
    damage.add(item_area(frame.scale, frame.theme, frame.viewport));
    if let Some(bar) = scrollbar_bounds(frame.scale, frame.theme, frame.viewport, frame.toolbar) {
        damage.add(bar);
    }
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
