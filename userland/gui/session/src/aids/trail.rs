//! Pointer trails: fading copies of the pointer along the path it has just
//! taken, so a moving pointer can be followed by eye.
//!
//! Each copy is where the pointer was a fixed interval ago, read from the
//! path rather than from the samples the device happened to report, so the
//! copies sit evenly along it however unevenly the samples arrived, and draw
//! back into the pointer as soon as it stops.

use tairix_geometry::Point;
use tairix_inline::{ArrayVec, RingBuf};
use tairix_theme::Timeline;
use tairix_wallpaper::PointerTrail;
use tairix_wm::{Ghost, MAX_GHOSTS};

/// How strongly the copy nearest the pointer is drawn; each older one is
/// weaker by the same step, down toward nothing.
const NEAREST_OPACITY: u32 = 150;

/// The closest two kept samples of the path may be: finer than a frame, so
/// the path keeps its shape, and coarse enough that [`PATH_SAMPLES`] spans
/// the longest trail however fast the device reports.
const SAMPLE_NS: u64 = 4_000_000;

/// Samples of the path kept: at [`SAMPLE_NS`] apart, a quarter of a second of
/// it, which reaches past the longest trail.
const PATH_SAMPLES: usize = 64;

/// How many copies a trail of `length` draws, and how far apart in time.
const fn shape(length: PointerTrail) -> Option<(usize, u64)> {
    match length {
        PointerTrail::Off => None,
        PointerTrail::Short => Some((3, 16_000_000)),
        PointerTrail::Medium => Some((5, 20_000_000)),
        PointerTrail::Long => Some((MAX_GHOSTS, 24_000_000)),
    }
}

/// The path the pointer has just taken, and the trail drawn along it.
#[derive(Clone, Debug)]
pub struct Trail {
    length: PointerTrail,
    /// Where the pointer was and when, oldest first.
    path: RingBuf<(u64, Point), PATH_SAMPLES>,
}

impl Default for Trail {
    fn default() -> Self {
        Self::new()
    }
}

impl Trail {
    /// No trail, and no path.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            length: PointerTrail::Off,
            path: RingBuf::new(),
        }
    }

    /// Draw a trail of `length` from now on; a trail turned off forgets its
    /// path.
    pub fn set_length(&mut self, length: PointerTrail) {
        self.length = length;
        if shape(length).is_none() {
            self.path.clear();
        }
    }

    /// Take in that the pointer is at `at` at `now_ns`.
    pub fn observe(&mut self, now_ns: u64, at: Point) {
        let Some((count, spacing)) = shape(self.length) else {
            return;
        };
        match self.path.back().copied() {
            Some((_, last)) if last == at => return,
            // Input arrives within a frame of the hand moving, so a pointer
            // found somewhere new was where it rested until a frame ago.
            Some((then, last)) => {
                let rested_until = now_ns.saturating_sub(Timeline::FRAME_NS).max(then);
                if rested_until > then {
                    self.record(rested_until, last);
                }
            }
            None => {}
        }
        self.record(now_ns, at);
        self.forget_before(now_ns.saturating_sub(span(count, spacing)));
    }

    /// Add `at` at `ns` to the path. The newest sample stays live until it
    /// is [`SAMPLE_NS`] past the one before it — a later sample replaces it —
    /// so the kept samples are at least that far apart.
    fn record(&mut self, ns: u64, at: Point) {
        let len = self.path.len();
        let settled = len
            .checked_sub(2)
            .and_then(|before| self.path.get(before))
            .map(|(before_ns, _)| *before_ns);
        let newest = len.checked_sub(1);
        if let Some(back) = newest.and_then(|index| self.path.get_mut(index)) {
            let live = back.0 == ns
                || settled.is_some_and(|before_ns| back.0.saturating_sub(before_ns) < SAMPLE_NS);
            if live {
                *back = (ns, at);
                return;
            }
        }
        let _ = self.path.push_back_overwrite((ns, at));
    }

    /// Drop what the trail can no longer reach, keeping the last sample
    /// before `ns` so the path there can still be interpolated.
    fn forget_before(&mut self, ns: u64) {
        while self.path.get(1).is_some_and(|(next, _)| *next <= ns) {
            let _ = self.path.pop_front();
        }
    }

    /// The copies to draw at `now_ns` behind a pointer now at `current`,
    /// oldest first, into `out`. A copy that has caught up with the pointer
    /// is not drawn, since the pointer covers it.
    ///
    /// Each copy is further back in time than the one before it, so one walk
    /// back along the path places them all.
    pub fn ghosts(&self, now_ns: u64, current: Point, out: &mut ArrayVec<Ghost, MAX_GHOSTS>) {
        out.clear();
        let Some((count, spacing)) = shape(self.length) else {
            return;
        };
        let steps = u32::try_from(count).unwrap_or(u32::MAX).saturating_add(1);
        let mut samples = self.path.iter().rev().copied().peekable();
        let mut newer: Option<(u64, Point)> = None;
        let mut last: Option<Point> = None;
        for behind in 1..=count {
            let back = u64::try_from(behind)
                .unwrap_or(u64::MAX)
                .saturating_mul(spacing);
            let at_ns = now_ns.saturating_sub(back);
            while let Some(sample) = samples.next_if(|(ns, _)| *ns > at_ns) {
                newer = Some(sample);
            }
            let at = match (samples.peek().copied(), newer) {
                (Some(older), Some(newer)) => between(older, newer, at_ns),
                (Some((_, older)), None) => older,
                (None, Some((_, oldest))) => oldest,
                (None, None) => return,
            };
            // A pointer that rested leaves several copies on one spot; the
            // newest, strongest one stands for them all.
            if at == current || last == Some(at) {
                continue;
            }
            last = Some(at);
            let fresh = steps.saturating_sub(u32::try_from(behind).unwrap_or(u32::MAX));
            let opacity = u8::try_from(NEAREST_OPACITY * fresh / steps.saturating_sub(1).max(1))
                .unwrap_or(u8::MAX);
            let _ = out.try_push(Ghost { at, opacity });
        }
        out.reverse();
    }

    /// Nanoseconds until the trail next changes, or `None` once every copy
    /// has caught up with the pointer.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64) -> Option<u64> {
        let (count, spacing) = shape(self.length)?;
        // One sample is where the pointer is, not a path it took.
        if self.path.len() < 2 {
            return None;
        }
        let (moved_ns, _) = self.path.back()?;
        let caught_up = moved_ns.saturating_add(span(count, spacing));
        (now_ns < caught_up).then(|| (caught_up - now_ns).min(Timeline::FRAME_NS))
    }

    /// Forget the path, so nothing trails the pointer until it moves again.
    pub fn clear(&mut self) {
        self.path.clear();
    }
}

/// Where the pointer was at `ns`, between the sample `older` before it and
/// `newer` after it.
fn between((older_ns, older): (u64, Point), (newer_ns, newer): (u64, Point), ns: u64) -> Point {
    let span = i64::try_from(newer_ns.saturating_sub(older_ns))
        .unwrap_or(i64::MAX)
        .max(1);
    let into = i64::try_from(ns.saturating_sub(older_ns))
        .unwrap_or(i64::MAX)
        .min(span);
    let lerp = |from: i32, to: i32| {
        let moved = i64::from(from) + (i64::from(to) - i64::from(from)) * into / span;
        i32::try_from(moved).unwrap_or(from)
    };
    Point::new(lerp(older.x, newer.x), lerp(older.y, newer.y))
}

/// How far back in time a trail of `count` copies `spacing` apart reaches.
fn span(count: usize, spacing: u64) -> u64 {
    u64::try_from(count)
        .unwrap_or(u64::MAX)
        .saturating_mul(spacing)
}

#[cfg(test)]
#[path = "trail_tests.rs"]
mod tests;
