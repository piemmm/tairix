//! The one desktop-session disposable-UI cache policy (`plans/SMARTRAM.md`
//! section 6.4).
//!
//! A scale/theme-rasterised pointer cursor and a rasterised notification
//! glyph are the same kind of memory from the reclaim model's point of
//! view: cheap to lose, expensive to rebuild, owned by the seat that is
//! showing them, and invalidated wholesale the moment the scale, theme, or
//! source set changes. [`disposable_ui_candidate`] is the one declaration
//! of that classification, and every constructor here assembles its
//! [`ReclaimCache`] from it in one place, differing only in its budget, so the
//! window manager's cursor cache and the taskbar's icon cache — which may not
//! depend on each other — never carry two copies of the same six declared
//! dimensions.
//!
//! # Why this lives here and not in the desktop session
//!
//! The natural first guess is that this is desktop-session policy and
//! belongs in `userland/gui/session`. It cannot. The window manager and
//! the taskbar each need to *name* the type of cache they hold and to
//! offer their embedder a way to build one, and neither may depend on the
//! session — the session composes them, and the reverse edge is
//! forbidden. They may not depend on each other either. `lib/reclaim` is
//! the only crate both already depend on and that sits below every
//! `userland/gui/*` crate, so it is the sole layering-legal home for the
//! one shared definition.
//!
//! This is not new policy leaking into the generic model:
//! [`ReclaimClass::DisposableUi`] and [`ReclaimOwner::DesktopSession`]
//! are already part of the closed taxonomy `model` defines, and this
//! module only composes them into the one declaration both consumers
//! would otherwise each spell out.
//!
//! # Every one of them keeps a reserve
//!
//! Every constructor declares
//! [`UI_CACHE_RESERVE_BYTES`] irreducible,
//! clamped by each cache's own derived ceiling. They differ in how much they
//! hold above that — a cursor fraction for pixels a rasterise rebuilds, a
//! screenful for the bulkier kinds bounded by what can be seen, the machine's
//! share for glass stacked deeper than that — and in which bands take it;
//! none of them is ever emptied outright, because
//! a session with no rasterised
//! pixels left redraws the same screen through a filesystem read, a
//! sandbox round trip, or an IPC call per element, every repaint.
//!
//! # One constructor, injected
//!
//! There is exactly one way to build such a cache, and it demands the
//! real backing size, the real gauge, and the real audit sink. There is
//! deliberately no parameterless fallback: a cache built without a live
//! gauge would classify and serve correctly while retaining nothing, and
//! a desktop that silently rasterised every cursor afresh — with the
//! diagnostics that would reveal it discarded — is a defect that looks
//! exactly like working software. A consumer therefore takes its cache as
//! a constructor argument, and the session that knows the display size,
//! the seat, and the process gauge assembles it.

use tairix_log::Sink;

use crate::cache::{CachedBytes, ReclaimCache};
use crate::model::{
    CacheBudget, CacheCandidate, InvalidationSource, RebuildCost, ReclaimClass, ReclaimOwner,
    ReclaimRule, Sensitivity, UI_CACHE_RESERVE_BYTES,
};
use core::hash::{BuildHasher, Hash};

use crate::pressure::PressureGauge;

/// The classification every desktop-session disposable-UI raster cache
/// declares: a rasterised cursor image or notification glyph is expensive
/// to rebuild (a rasterisation pass), holds user-visible rendered data,
/// is invalidated wholesale by a generation token (the active scale paired
/// with a theme or source-set identity), is simply dropped on reclaim (the
/// canonical vector source rebuilds it), and is charged to the seat
/// showing it.
#[must_use]
pub const fn disposable_ui_candidate(seat: u64, entry_metadata_bytes: usize) -> CacheCandidate {
    CacheCandidate {
        class: Some(ReclaimClass::DisposableUi),
        owner: Some(ReclaimOwner::DesktopSession { seat }),
        rebuild_cost: RebuildCost::Expensive,
        sensitivity: Some(Sensitivity::UserData),
        invalidation: Some(InvalidationSource::GenerationToken),
        rule: Some(ReclaimRule::Drop),
        entry_metadata_bytes,
    }
}

/// Build one desktop-session disposable-UI cache, bounded by the real
/// output's backing byte size (`fb_bytes`, run through
/// [`CacheBudget::from_backing`] so a 4K display gets a proportionately
/// larger cache than a 640×480 one) and governed by the process's live
/// `pressure` gauge.
///
/// `seat` is the owning seat (for the classification's
/// [`ReclaimOwner::DesktopSession`]) and `entry_metadata_bytes` is the
/// caller's truthful bound on its own per-entry bookkeeping (its key's
/// size plus this cache's share of the map/index node overhead).
#[must_use]
pub fn disposable_ui_cache<K, V, E, S>(
    label: &'static str,
    seat: u64,
    fb_bytes: usize,
    entry_metadata_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: S,
) -> ReclaimCache<K, V, E, S>
where
    K: Eq + Hash,
    V: CachedBytes,
    E: PartialEq + Clone,
    S: BuildHasher,
{
    ui_cache(
        label,
        seat,
        entry_metadata_bytes,
        disposable_ui_budget(fb_bytes),
        pressure,
        sink,
        hasher,
    )
}

/// The budget [`disposable_ui_cache`] holds its cache to on an output of
/// `fb_bytes`: what one it built is held to again when the output changes
/// size ([`ReclaimCache::rebudget`]).
#[must_use]
pub fn disposable_ui_budget(fb_bytes: usize) -> CacheBudget {
    CacheBudget::from_backing(fb_bytes).with_reserved_floor(UI_CACHE_RESERVE_BYTES)
}

/// Build a desktop-session cache of *screen-sized* rendered pixels,
/// ceilinged at one screenful (`fb_bytes`) instead of the small fraction
/// [`disposable_ui_cache`] allows a cursor or a notification glyph.
///
/// A window's rendered furniture strips and a window's frosted backdrop
/// are the same *kind* of memory as those — both declare the identical
/// [`disposable_ui_candidate`] classification — but they are bulkier, and
/// they are bounded by a different fact about the machine: what can be
/// seen. No more of either than fills the screen can be visible at once,
/// so a screenful is the honest ceiling, and anything above it belongs to
/// a minimised, off-screen, or stacked-under window — exactly the entries
/// the least-recently-composited eviction order should take first. Sizing
/// them by the cursor cache's fraction instead would refuse a single
/// ordinary window and rebuild it every frame.
#[must_use]
pub fn screenful_ui_cache<K, V, E, S>(
    label: &'static str,
    seat: u64,
    fb_bytes: usize,
    entry_metadata_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: S,
) -> ReclaimCache<K, V, E, S>
where
    K: Eq + Hash,
    V: CachedBytes,
    E: PartialEq + Clone,
    S: BuildHasher,
{
    ui_cache(
        label,
        seat,
        entry_metadata_bytes,
        screenful_ui_budget(fb_bytes),
        pressure,
        sink,
        hasher,
    )
}

/// The budget [`screenful_ui_cache`] holds its cache to on an output of
/// `fb_bytes`.
#[must_use]
pub fn screenful_ui_budget(fb_bytes: usize) -> CacheBudget {
    CacheBudget::from_ceiling(fb_bytes).with_reserved_floor(UI_CACHE_RESERVE_BYTES)
}

/// Build a desktop-session cache of pixels the session cannot rebuild
/// without leaving its own address space — a decoded on-disk icon (a
/// capability-gated read plus a parser-sandbox round trip each) — held to
/// [`working_set_ui_budget`].
///
/// Declared exactly like [`disposable_ui_cache`]. What differs is what it
/// keeps, and for how long: rebuilding one of these costs a read and a sandbox
/// round trip, the resources a machine short of memory has least of, so
/// everything decoded stays — a folder browsed and left, the scroll-back of a
/// long one — while memory allows. Below the ceiling only least-recent use
/// takes an entry, mild and moderate pressure take nothing, and severe and
/// critical take it down to the shared irreducible reserve like every other UI
/// cache.
#[must_use]
pub fn working_set_ui_cache<K, V, E, S>(
    label: &'static str,
    seat: u64,
    (fb_bytes, memory_bytes): (usize, usize),
    entry_metadata_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: S,
) -> ReclaimCache<K, V, E, S>
where
    K: Eq + Hash,
    V: CachedBytes,
    E: PartialEq + Clone,
    S: BuildHasher,
{
    ui_cache(
        label,
        seat,
        entry_metadata_bytes,
        working_set_ui_budget(fb_bytes, memory_bytes),
        pressure,
        sink,
        hasher,
    )
}

/// The budget [`working_set_ui_cache`] holds its cache to on an output of
/// `fb_bytes` in a machine of `memory_bytes`: the machine's standard share of
/// its memory ([`CacheBudget::from_backing`]), never below one screenful —
/// which is what a machine that cannot report its memory, `0`, leaves — and
/// all of it the working set mild and moderate pressure leave alone.
#[must_use]
pub fn working_set_ui_budget(fb_bytes: usize, memory_bytes: usize) -> CacheBudget {
    let ceiling = machine_share(fb_bytes, memory_bytes);
    CacheBudget::from_ceiling(ceiling)
        .with_working_set_floor(ceiling)
        .with_reserved_floor(UI_CACHE_RESERVE_BYTES)
}

/// The ceiling of a cache worth keeping beyond what one output shows: the
/// standard share of a machine of `memory_bytes`, never below one screenful of
/// `fb_bytes`.
const fn machine_share(fb_bytes: usize, memory_bytes: usize) -> usize {
    let share = CacheBudget::from_backing(memory_bytes).hard();
    if share > fb_bytes {
        share
    } else {
        fb_bytes
    }
}

/// Build a desktop-session cache of pixels every surface in a stack of
/// translucent ones needs whole: a frosted window's backdrop.
///
/// [`screenful_ui_cache`]'s bound is what can be seen, and stacked glass breaks
/// it: a frost is its window's whole rectangle, and a window beneath a
/// translucent one still shows through it, so two overlapping windows want two
/// frosts of the same pixels. The ceiling is therefore the larger of one
/// screenful and the standard share of the machine's memory
/// ([`CacheBudget::from_backing`] over `memory_bytes`; `0`, a machine that
/// cannot report it, leaves the screenful).
///
/// Mild and moderate pressure take it back to one screenful — every window's
/// glass in a single layer, giving up only what is stacked beneath — and
/// severe and critical to the shared reserve, like every UI cache.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the screenful constructors' arguments plus the memory figure \
              this ceiling is the machine's share of"
)]
pub fn stacked_ui_cache<K, V, E, S>(
    label: &'static str,
    seat: u64,
    fb_bytes: usize,
    memory_bytes: usize,
    entry_metadata_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: S,
) -> ReclaimCache<K, V, E, S>
where
    K: Eq + Hash,
    V: CachedBytes,
    E: PartialEq + Clone,
    S: BuildHasher,
{
    ui_cache(
        label,
        seat,
        entry_metadata_bytes,
        stacked_ui_budget(fb_bytes, memory_bytes),
        pressure,
        sink,
        hasher,
    )
}

/// The budget [`stacked_ui_cache`] holds its cache to on an output of
/// `fb_bytes` in a machine of `memory_bytes`.
#[must_use]
pub fn stacked_ui_budget(fb_bytes: usize, memory_bytes: usize) -> CacheBudget {
    let ceiling = machine_share(fb_bytes, memory_bytes);
    CacheBudget::from_ceiling(ceiling)
        .with_working_set_floor(fb_bytes)
        .with_reserved_floor(UI_CACHE_RESERVE_BYTES)
}

/// The cache every constructor above builds: the one classification under
/// `budget`, which carries the shared reserve no band may take.
fn ui_cache<K, V, E, S>(
    label: &'static str,
    seat: u64,
    entry_metadata_bytes: usize,
    budget: CacheBudget,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: S,
) -> ReclaimCache<K, V, E, S>
where
    K: Eq + Hash,
    V: CachedBytes,
    E: PartialEq + Clone,
    S: BuildHasher,
{
    ReclaimCache::new(
        label,
        disposable_ui_candidate(seat, entry_metadata_bytes),
        budget,
        pressure,
        sink,
        hasher,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::AdmissionRefusal;
    use crate::pressure::{PressureBand, ReportedPressure};
    use tairix_hash::BuildFastHash;
    use tairix_log::DiscardSink;

    /// The cache shape every builder under test produces.
    type GlyphCache = ReclaimCache<u32, Glyph, u64, BuildFastHash>;

    extern crate std;
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    #[derive(Debug, Eq, PartialEq)]
    struct Glyph {
        bytes: Vec<u8>,
    }

    impl CachedBytes for Glyph {
        fn payload_bytes(&self) -> usize {
            self.bytes.len()
        }

        fn wipe(&mut self) {
            self.bytes.fill(0);
        }
    }

    #[test]
    fn the_candidate_classifies_without_refusal() {
        let candidate = disposable_ui_candidate(3, 64);
        let policy = candidate.classify().expect("admissible");
        assert_eq!(policy.class(), ReclaimClass::DisposableUi);
        assert_eq!(policy.owner(), ReclaimOwner::DesktopSession { seat: 3 });
        assert_eq!(policy.rebuild_cost(), RebuildCost::Expensive);
        assert_eq!(policy.sensitivity(), Sensitivity::UserData);
        assert_eq!(policy.invalidation(), InvalidationSource::GenerationToken);
        assert_eq!(policy.rule(), ReclaimRule::Drop);
    }

    #[test]
    fn oversized_entry_metadata_is_refused_like_any_other_candidate() {
        let candidate = disposable_ui_candidate(0, usize::MAX);
        assert_eq!(
            candidate.classify(),
            Err(AdmissionRefusal::UnboundedMetadata)
        );
    }

    #[test]
    fn the_chrome_ceiling_retains_a_desktop_of_furniture_the_glyph_fraction_cannot() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        // One 1080p output, and one full-width window's title/border
        // strips at that width — the figures a real desktop retains.
        let fb_bytes = 1920 * 1080 * 4;
        let strips = 1920 * 32 * 4;
        let furniture = |key: u32, cache: &mut ReclaimCache<u32, Glyph, u64, BuildFastHash>| {
            let _ = cache.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0x11; strips],
                })
            });
        };

        let mut chrome: ReclaimCache<u32, Glyph, u64, BuildFastHash> = screenful_ui_cache(
            "test.chrome",
            1,
            fb_bytes,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        let mut glyphs: ReclaimCache<u32, Glyph, u64, BuildFastHash> = disposable_ui_cache(
            "test.glyphs",
            1,
            fb_bytes,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        for key in 0..8u32 {
            furniture(key, &mut chrome);
            furniture(key, &mut glyphs);
        }

        assert_eq!(
            chrome.len(),
            8,
            "a screenful holds a whole desktop of furniture"
        );
        assert!(chrome.charged_bytes() <= CacheBudget::from_ceiling(fb_bytes).hard());
        assert!(
            glyphs.len() < chrome.len(),
            "the cursor/icon fraction is far too small for window furniture"
        );
        assert!(glyphs.charged_bytes() <= CacheBudget::from_backing(fb_bytes).hard());
    }

    /// The working-set ceiling never falls below what one frame draws, even
    /// on a machine that cannot report its memory: a sixteenth of the output
    /// cannot hold them, and a cache that evicts an icon the next paint asks
    /// for again either draws the wrong picture or decodes it afresh every
    /// frame, at a capability-gated read and a sandbox round trip each.
    #[test]
    fn the_working_set_ceiling_holds_a_frame_of_icons_the_glyph_fraction_cannot() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        // A file manager's default 480x480 window, and the sixteen 42-pixel
        // icons its grid draws in one frame — the measured figures, not a
        // round number.
        let fb_bytes = 480 * 4 * 480;
        let icon = 42 * 42 * 4;
        let tiles = 16u32;

        let mut icons: ReclaimCache<u32, Glyph, u64, BuildFastHash> = working_set_ui_cache(
            "test.icons",
            1,
            (fb_bytes, 0),
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        let mut fraction: ReclaimCache<u32, Glyph, u64, BuildFastHash> = disposable_ui_cache(
            "test.fraction",
            1,
            fb_bytes,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        for key in 0..tiles {
            for cache in [&mut icons, &mut fraction] {
                let _ = cache.get_or_build(&1, key, || {
                    Some(Glyph {
                        bytes: vec![0x33; icon],
                    })
                });
            }
        }

        assert_eq!(
            icons.len(),
            tiles as usize,
            "a frame's icons did not all fit the working-set ceiling"
        );
        assert!(
            fraction.len() < tiles as usize,
            "the cursor fraction is what could not hold one frame of them"
        );
    }

    /// What was decoded is kept beyond what one screen shows, up to the
    /// machine's share of its memory: browsing back to a folder left a while
    /// ago draws its icons without a read or a round trip.
    #[test]
    fn the_working_set_ceiling_is_the_machine_s_share_not_one_screen() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        let share = CacheBudget::from_backing(MACHINE).hard();
        assert!(
            share > 4 * SCREEN,
            "the fixture's share is not past a screenful"
        );
        assert_eq!(working_set_ui_budget(SCREEN, MACHINE).hard(), share);
        assert_eq!(working_set_ui_budget(SCREEN, 0).hard(), SCREEN);

        let icon = 64 * 64 * 4;
        let held = u32::try_from(4 * SCREEN / icon).expect("four screenfuls of icons");
        let mut icons: ReclaimCache<u32, Glyph, u64, BuildFastHash> = working_set_ui_cache(
            "test.icons.share",
            1,
            (SCREEN, MACHINE),
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        for key in 0..held {
            let _ = icons.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0x44; icon],
                })
            });
        }
        assert_eq!(
            icons.len(),
            held as usize,
            "an icon was evicted below the share"
        );
    }

    /// Mild and moderate pressure take no decoded icon at all; only severe and
    /// critical do, and then down to the shared reserve. Re-deriving one costs
    /// a read and a sandbox round trip, the resources a tightening machine has
    /// least of, so giving icons back early costs more than it frees.
    #[test]
    fn only_severe_pressure_takes_a_decoded_icon() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        let icon = 64 * 64 * 4;
        let mut icons: ReclaimCache<u32, Glyph, u64, BuildFastHash> = working_set_ui_cache(
            "test.icons.bands",
            1,
            (SCREEN, MACHINE),
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        let held = u32::try_from(2 * SCREEN / icon).expect("two screenfuls of icons");
        for key in 0..held {
            let _ = icons.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0x44; icon],
                })
            });
        }
        assert!(icons.charged_bytes() > UI_CACHE_RESERVE_BYTES);

        for band in [PressureBand::Mild, PressureBand::Moderate] {
            gauge.report(band);
            assert_eq!(icons.enforce_pressure(), 0, "{band:?} took a decoded icon");
            assert_eq!(icons.len(), held as usize, "{band:?}");
        }
        gauge.report(PressureBand::Severe);
        assert!(icons.enforce_pressure() > 0, "severe pressure took nothing");
        assert!(icons.charged_bytes() <= UI_CACHE_RESERVE_BYTES);
        assert!(!icons.is_empty(), "severe pressure took the reserve");
    }

    #[test]
    fn every_desktop_cache_keeps_its_reserve_at_the_deepest_band() {
        use crate::model::UI_CACHE_RESERVE_BYTES;
        use crate::pressure::shrink_target;

        // One 1080p output, and a glyph small enough that a screenful of them
        // fits inside the smallest of the three budgets.
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        let fb_bytes = 1920 * 1080 * 4;

        let mut caches: Vec<(&str, GlyphCache)> = vec![
            (
                "cursor",
                disposable_ui_cache(
                    "test.cursor",
                    1,
                    fb_bytes,
                    32,
                    gauge,
                    sink,
                    BuildFastHash::new(),
                ),
            ),
            (
                "chrome",
                screenful_ui_cache(
                    "test.chrome",
                    1,
                    fb_bytes,
                    32,
                    gauge,
                    sink,
                    BuildFastHash::new(),
                ),
            ),
            (
                "icons",
                working_set_ui_cache(
                    "test.icons",
                    1,
                    (fb_bytes, 0),
                    32,
                    gauge,
                    sink,
                    BuildFastHash::new(),
                ),
            ),
        ];
        for (what, cache) in &mut caches {
            let _ = cache.get_or_build(&1, 0, || {
                Some(Glyph {
                    bytes: vec![0x55; 4096],
                })
            });
            assert!(cache.charged_bytes() > 0, "{what}");
        }

        for band in [PressureBand::Severe, PressureBand::Critical] {
            gauge.report(band);
            for (what, cache) in &mut caches {
                assert_eq!(
                    cache.enforce_pressure(),
                    0,
                    "{band:?} took {what}'s reserve"
                );
                assert_eq!(cache.len(), 1, "{band:?} {what}");
            }
        }

        // Above the reserve the ordinary order still applies: a screenful
        // cache holding more than the reserve gives the excess back at mild
        // pressure and keeps the reserve. Furniture strips at this width, so
        // the figures are a real desktop's.
        gauge.report(PressureBand::Normal);
        let strips = 1920 * 32 * 4;
        let mut chrome: ReclaimCache<u32, Glyph, u64, BuildFastHash> = screenful_ui_cache(
            "test.chrome.deep",
            1,
            fb_bytes,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        for key in 0..8u32 {
            let _ = chrome.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0x22; strips],
                })
            });
        }
        assert!(chrome.charged_bytes() > UI_CACHE_RESERVE_BYTES);
        gauge.report(PressureBand::Mild);
        assert!(chrome.enforce_pressure() > 0, "the speculation must go");
        assert!(chrome.charged_bytes() <= UI_CACHE_RESERVE_BYTES);
        assert!(!chrome.is_empty(), "but not the reserve");

        // The reserve is the smaller of the shared figure and the cache's own
        // display-derived ceiling, so a small budget reserves all of itself
        // and a screen-sized one reserves the shared figure.
        let small = CacheBudget::from_backing(fb_bytes).with_reserved_floor(UI_CACHE_RESERVE_BYTES);
        let large = CacheBudget::from_ceiling(fb_bytes).with_reserved_floor(UI_CACHE_RESERVE_BYTES);
        assert_eq!(small.reserved(), small.hard());
        assert_eq!(large.reserved(), UI_CACHE_RESERVE_BYTES);
        assert_eq!(
            shrink_target(PressureBand::Critical, ReclaimClass::DisposableUi, large),
            UI_CACHE_RESERVE_BYTES
        );
    }

    /// A 1024×768 output and a 256 MiB machine, the QEMU desktop's figures.
    const SCREEN: usize = 1024 * 768 * 4;
    const MACHINE: usize = 256 << 20;

    fn stacked(memory: usize, gauge: &'static ReportedPressure) -> GlyphCache {
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        stacked_ui_cache(
            "test.frost",
            1,
            SCREEN,
            memory,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        )
    }

    /// A desktop cache constructor, at an output's size.
    type Build = fn(usize, &'static ReportedPressure, &'static DiscardSink) -> GlyphCache;

    /// One constructor, and the budget it names for an output.
    type Kind = (Build, fn(usize) -> CacheBudget);

    /// A cache held again to the budget its constructor names for another
    /// output admits exactly what one built for that output does, at every
    /// band: the budget each constructor holds its cache to is that one
    /// function, whichever path reached it.
    #[test]
    fn a_rebudgeted_cache_admits_what_one_built_for_its_output_does() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        let (small, large) = (640 * 480 * 4, 3840 * 2160 * 4);
        let kinds: [Kind; 4] = [
            (
                |fb, gauge, sink| {
                    disposable_ui_cache("t", 1, fb, 32, gauge, sink, BuildFastHash::new())
                },
                disposable_ui_budget,
            ),
            (
                |fb, gauge, sink| {
                    screenful_ui_cache("t", 1, fb, 32, gauge, sink, BuildFastHash::new())
                },
                screenful_ui_budget,
            ),
            (
                |fb, gauge, sink| {
                    working_set_ui_cache(
                        "t",
                        1,
                        (fb, MACHINE),
                        32,
                        gauge,
                        sink,
                        BuildFastHash::new(),
                    )
                },
                |fb| working_set_ui_budget(fb, MACHINE),
            ),
            (
                |fb, gauge, sink| {
                    stacked_ui_cache("t", 1, fb, MACHINE, 32, gauge, sink, BuildFastHash::new())
                },
                |fb| stacked_ui_budget(fb, MACHINE),
            ),
        ];
        for (build, budget) in kinds {
            let mut moved = build(small, gauge, sink);
            let _ = moved.rebudget(budget(large));
            let built = build(large, gauge, sink);
            for band in [
                PressureBand::Normal,
                PressureBand::Moderate,
                PressureBand::Critical,
            ] {
                gauge.report(band);
                for bytes in [1 << 12, 1 << 16, 1 << 20, 1 << 24, 1 << 27] {
                    assert_eq!(
                        moved.holds(1, bytes),
                        built.holds(1, bytes),
                        "{band:?} {bytes}"
                    );
                }
            }
            gauge.report(PressureBand::Normal);
        }
    }

    /// Two overlapping windows each wider than half the screen want two frosts
    /// of the same pixels, which one screenful cannot hold and the machine's
    /// share can.
    #[test]
    fn the_stacked_ceiling_is_the_machines_share_and_never_below_a_screenful() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let frost = 930 * 590 * 4;
        assert!(
            2 * frost > SCREEN,
            "the premise: the pair outgrows a screenful"
        );

        assert!(stacked(MACHINE, gauge).holds(2, 2 * frost));
        assert!(
            !stacked(MACHINE, gauge).holds(1, CacheBudget::from_backing(MACHINE).hard() + 1),
            "and the share is a ceiling"
        );
        for memory in [0, SCREEN] {
            let cache = stacked(memory, gauge);
            assert!(
                cache.holds(1, frost),
                "{memory}: a screenful still holds one"
            );
            assert!(
                !cache.holds(2, 2 * frost),
                "{memory}: a machine whose share is under a screenful keeps the screenful"
            );
        }
    }

    /// Pressure gives the stacked glass back first: mild and moderate keep one
    /// screenful of it, every window's glass in a single layer, and severe the
    /// shared reserve.
    #[test]
    fn pressure_takes_the_stacked_glass_and_keeps_one_layer_until_severe() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let mut cache = stacked(MACHINE, gauge);
        let frost = SCREEN / 2;
        for key in 0..3u32 {
            let _ = cache.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0x66; frost],
                })
            });
        }
        assert_eq!(
            cache.len(),
            3,
            "three half-screen frosts are one and a half screenfuls"
        );

        for band in [PressureBand::Mild, PressureBand::Moderate] {
            gauge.report(band);
            cache.enforce_pressure();
            assert!(cache.charged_bytes() <= SCREEN, "{band:?}");
            assert!(!cache.is_empty(), "{band:?} kept a layer of glass");
            assert!(cache.holds(1, frost), "{band:?} still frosts a window");
        }
        gauge.report(PressureBand::Severe);
        cache.enforce_pressure();
        assert!(cache.charged_bytes() <= UI_CACHE_RESERVE_BYTES);
    }

    #[test]
    fn a_wired_cache_is_bounded_by_the_real_backing_size() {
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let sink: &'static DiscardSink = Box::leak(Box::new(DiscardSink));
        let mut cache: ReclaimCache<u32, Glyph, u64, BuildFastHash> = disposable_ui_cache(
            "test.wired",
            1,
            64 * 1024,
            32,
            gauge,
            sink,
            BuildFastHash::new(),
        );
        for key in 0..8u32 {
            let _ = cache.get_or_build(&1, key, || {
                Some(Glyph {
                    bytes: vec![0xAA; 1024],
                })
            });
        }
        let hard = CacheBudget::from_backing(64 * 1024).hard();
        assert!(cache.charged_bytes() <= hard);
        assert!(!cache.is_empty(), "normal pressure still admits entries");
    }
}
