//! The slideshow's running order: which of the shipped pictures it shows, in
//! what order, and when the next is due.
//!
//! The pictures are the catalog's own, narrowed to the one category the
//! options name — every picture when they name none, or name one the store no
//! longer holds, so a stale choice still shows pictures rather than a black
//! screen. A shuffled slideshow shows every picture once before any repeats,
//! and never the same one twice running from one pass into the next.

use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::fallible;
use tairix_wallpaper::{SlideOrder, SlideshowOptions, WallpaperCategory};
use tairix_window::WallpaperName;

use super::seed_from;

/// A slideshow's pictures and its place among them.
pub(super) struct Slides {
    /// The catalog positions shown, in this pass's order.
    order: Vec<usize>,
    /// Where in `order` the next picture is.
    next: usize,
    shuffled: bool,
    rng: NonCryptoRng,
    interval_ns: u64,
    /// When the next picture is due, or `None` with no picture to show.
    due_ns: Option<u64>,
}

impl Slides {
    /// The slideshow `options` describe over `catalog`, its first picture due
    /// at `now_ns`, or `None` when the heap will not give its running order.
    pub(super) fn new(
        catalog: &[WallpaperName],
        options: &SlideshowOptions,
        now_ns: u64,
    ) -> Option<Self> {
        let named = options.source.category().map(WallpaperCategory::as_str);
        let chosen = |name: &WallpaperName| named.is_none_or(|category| name.category == category);
        let matched = catalog.iter().filter(|name| chosen(name)).count();
        let every = matched == 0;
        let mut order = Vec::new();
        if !fallible::reserve(&mut order, if every { catalog.len() } else { matched }) {
            return None;
        }
        order.extend(
            catalog
                .iter()
                .enumerate()
                .filter(|(_, name)| every || chosen(name))
                .map(|(at, _)| at),
        );
        let mut slides = Self {
            due_ns: (!order.is_empty()).then_some(now_ns),
            order,
            next: 0,
            shuffled: options.order == SlideOrder::Shuffled,
            rng: NonCryptoRng::seed_from_u64(seed_from(now_ns)),
            interval_ns: options.interval.saturating_total_nanos(),
        };
        if slides.shuffled {
            slides.shuffle(None);
        }
        Some(slides)
    }

    /// When the next picture is due, or `None` with none to show.
    pub(super) const fn due_ns(&self) -> Option<u64> {
        self.due_ns
    }

    /// The catalog position to show at `now_ns`, if one is due; the one after
    /// it falls due an interval later.
    pub(super) fn take_due(&mut self, now_ns: u64) -> Option<usize> {
        if now_ns < self.due_ns? {
            return None;
        }
        let shown = *self.order.get(self.next)?;
        self.next += 1;
        if self.next == self.order.len() {
            self.next = 0;
            if self.shuffled {
                self.shuffle(Some(shown));
            }
        }
        self.due_ns = Some(now_ns.saturating_add(self.interval_ns));
        Some(shown)
    }

    /// Put the pass in a fresh random order that does not open on `after`, the
    /// picture the last pass ended on.
    fn shuffle(&mut self, after: Option<usize>) {
        let len = self.order.len();
        for at in (1..len).rev() {
            let with = self.below(at + 1);
            self.order.swap(at, with);
        }
        if len > 1 && after.is_some() && self.order.first().copied() == after {
            let with = 1 + self.below(len - 1);
            self.order.swap(0, with);
        }
    }

    /// A uniform draw in `0..bound`, which is not zero.
    fn below(&mut self, bound: usize) -> usize {
        let drawn = self
            .rng
            .next_below(u64::try_from(bound).unwrap_or(u64::MAX));
        usize::try_from(drawn).unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "slides_tests.rs"]
mod tests;
