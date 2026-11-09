//! The grid's icon-artwork paint side: the decode cache a tile resolves
//! through, and the deferring lookup a miss is recorded on.
//!
//! One tile's icon costs a bounded VFS read plus a round trip to the parser
//! sandbox. Inside a paint that is one round trip per visible tile on a
//! folder's first frame and another per row revealed by a scroll — seconds of
//! frozen window on a store like `/System/Commands`, a 256-square master per
//! bundle. So the paint neither reads nor decodes: it resolves through this
//! cache, whose misses go to an injected [`ArtworkResolver`] that records them
//! for the reader thread, and a tile with nothing yet draws its built-in
//! glyph.
//!
//! The cache stays here, on the paint side, because a picture is handed out as
//! a borrow into it: a cache behind the reader's lock could not lend one for
//! longer than the guard. Only the desk crosses that lock, so the read and the
//! sandbox round trip happen with nothing held.
//!
//! The cache and the resolver are injected — only the running program knows the
//! screen's frame size, the live pressure gauge, the audit sink, the capability
//! the read runs under, and whether the kernel granted a thread to defer to —
//! so every rule below is a host test. A listed bundle's version is asked of
//! the resolver once a turn ([`IconPipeline::begin_turn`]), however often the
//! turn walks its tile.

use alloc::boxed::Box;

use tairix_browse::render::ShownArt;
use tairix_icon::{ArtworkCache, ArtworkResolver, ByTurn, IconArtworkSource, Landed};

/// The decode cache and the resolver its misses are produced through.
pub struct IconPipeline {
    /// The retained decode outcomes, keyed by what was resolved and the pixel
    /// side.
    cache: ArtworkCache,
    /// Where a miss goes: the reader thread's desk, or — with no thread to
    /// defer to — a resolver that reads and decodes in the paint, as this app
    /// did before it had one.
    resolver: ByTurn,
}

impl IconPipeline {
    /// A pipeline over a ready-built `cache` and the `resolver` its misses are
    /// produced through.
    #[must_use]
    pub fn new(cache: ArtworkCache, resolver: Box<dyn ArtworkResolver>) -> Self {
        Self {
            cache,
            resolver: ByTurn::new(resolver),
        }
    }

    /// A turn of the event loop begins: each listed bundle's version is asked
    /// of the resolver once more, the first time the turn needs it.
    pub fn begin_turn(&mut self) {
        self.resolver.begin_turn();
    }

    /// The lookup a paint is handed: the cache bound to its resolver, so a
    /// miss is recorded rather than read.
    pub fn source(&mut self) -> IconArtworkSource<'_> {
        IconArtworkSource::new(&mut self.cache, &mut self.resolver)
    }

    /// Give back what a changed memory-pressure band no longer allows,
    /// answering the bytes released.
    pub fn trim(&mut self) -> usize {
        self.cache.trim()
    }

    /// Whether the cache would keep one picture of `bytes` at the band in
    /// force now.
    #[must_use]
    pub fn holds(&self, bytes: usize) -> bool {
        self.cache.holds(bytes)
    }

    /// Hold the cache to what an output of `fb_bytes` allows, the output
    /// having changed size.
    pub fn rescale(&mut self, fb_bytes: usize) {
        self.cache.rescale(fb_bytes);
    }

    /// Whether `art` draws through a decode `landed` answered, so its slot
    /// repaints; one that does not is still shown though the pass will not
    /// repaint it, and the resolver hears so, so a sweep keeps what it waits
    /// on.
    pub fn moved_by(&mut self, landed: &Landed, art: ShownArt<'_>) -> bool {
        let moved = landed.resolves(art.request, art.side);
        if !moved {
            self.cache
                .still_shown(&mut self.resolver, art.request, art.side);
        }
        moved
    }
}

impl Drop for IconPipeline {
    /// Release every retained decode, overwriting the pixels first, so one
    /// user's decoded artwork never outlives their window in reusable heap —
    /// on every way out of the app, not the ones a future edit remembers to
    /// spell.
    fn drop(&mut self) {
        self.cache.teardown();
    }
}

#[cfg(test)]
#[path = "icons_tests.rs"]
mod tests;
