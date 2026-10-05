//! Showing where the pointer is: Ctrl pressed and released on its own sends
//! two rings closing in on the pointer, drawing the eye to wherever it is.
//!
//! Each ring eases in from wide, slowing as it arrives, and fades as it
//! closes, the second a beat behind the first. Each is edged in a thin dark
//! rim, so it reads over a light picture as well as a dark one. With motion
//! reduced, one ring stands around the pointer for the same time instead.

use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_geometry::Scale;
use tairix_theme::motion::{ease_out, smoothstep};
use tairix_theme::{Theme, Timeline};
use tairix_wm::{Color, Halo, HaloRing};

/// How long each ring takes to close in.
const RING_NS: u64 = 570 * MS;

/// How far the second ring follows the first.
const LAG_NS: u64 = 150 * MS;

/// How long the pointer is shown for, rings and all.
const SHOWN_NS: u64 = RING_NS + LAG_NS;

/// How long a ring takes to fade in, and how far into its life it begins to
/// fade out: it is gone as it arrives.
const FADE_IN_NS: u64 = 90 * MS;
const FADE_OUT_AT_NS: u64 = 350 * MS;

/// How wide the rings start and how close to the pointer they end, in
/// logical pixels.
const FROM_RADIUS_PX: u32 = 80;
const TO_RADIUS_PX: u32 = 14;

/// The one ring that stands around the pointer when motion is reduced.
const STILL_RADIUS_PX: u32 = 32;

/// How wide a ring's band is, and the rim either side of it, in logical
/// pixels.
const BAND_PX: u32 = 3;
const RIM_PX: u32 = 1;

/// How dark the rim is where its ring is strongest.
const RIM_ALPHA: u8 = 120;

/// The rings sent to the pointer, while they are.
#[derive(Clone, Debug, Default)]
pub struct Beacon {
    started_ns: Option<u64>,
    /// Whether the rings were last drawn standing still, as reduced motion
    /// draws them, so they owe no frame until they go.
    still: bool,
}

impl Beacon {
    /// No rings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started_ns: None,
            still: false,
        }
    }

    /// Send the rings, starting at `now_ns` — again from the start if they
    /// were already on their way.
    pub fn start(&mut self, now_ns: u64) {
        self.started_ns = Some(now_ns);
    }

    /// Take the rings away.
    pub fn stop(&mut self) {
        self.started_ns = None;
    }

    /// The halo to draw at `now_ns` in `theme` on an output at `scale`, empty
    /// once the pointer has been shown for long enough.
    pub fn halo(&mut self, now_ns: u64, theme: &Theme, scale: Scale) -> Halo {
        let mut halo = Halo::new();
        let Some(started) = self.started_ns else {
            return halo;
        };
        if now_ns.saturating_sub(started) >= SHOWN_NS {
            self.started_ns = None;
            return halo;
        }
        let ink = Color::from(theme.palette().accent);
        self.still = theme.motion().reduced_motion();
        if self.still {
            ring(
                &mut halo,
                scale.scale_length(STILL_RADIUS_PX),
                u8::MAX,
                ink,
                scale,
            );
            return halo;
        }
        // The follower first, beneath the ring ahead of it.
        for delay in [LAG_NS, 0] {
            let Some(into) = now_ns
                .saturating_sub(started)
                .checked_sub(delay)
                .filter(|into| *into < RING_NS)
            else {
                continue;
            };
            let closed = u32::from(ease_out(progress(into, RING_NS)));
            let radius = FROM_RADIUS_PX - (FROM_RADIUS_PX - TO_RADIUS_PX) * closed / 255;
            ring(
                &mut halo,
                scale.scale_length(radius),
                strength(into),
                ink,
                scale,
            );
        }
        halo
    }

    /// Nanoseconds until the rings next change, or `None` while there are
    /// none. A still ring changes only when it goes.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64) -> Option<u64> {
        let started = self.started_ns?;
        let left = started.saturating_add(SHOWN_NS).saturating_sub(now_ns);
        Some(if self.still {
            left
        } else {
            left.min(Timeline::FRAME_NS)
        })
    }
}

/// How far `into` is through `span`, over the byte range the shared curves
/// take.
fn progress(into: u64, span: u64) -> u8 {
    u8::try_from(into.min(span) * u64::from(u8::MAX) / span.max(1)).unwrap_or(u8::MAX)
}

/// How strongly a ring `into` its life is drawn: rising to full as it
/// appears, falling to nothing as it arrives.
fn strength(into: u64) -> u8 {
    let arriving = smoothstep(progress(into, FADE_IN_NS));
    let leaving = into
        .checked_sub(FADE_OUT_AT_NS)
        .map_or(0, |out| smoothstep(progress(out, RING_NS - FADE_OUT_AT_NS)));
    arriving.min(u8::MAX - leaving)
}

/// Lay one ring `radius` pixels out at `strength`: its dark rim, then its
/// band in `ink`.
fn ring(halo: &mut Halo, radius: u32, strength: u8, ink: Color, scale: Scale) {
    let band = scale.scale_length(BAND_PX).max(1);
    let rim = scale.scale_length(RIM_PX).max(1);
    let weaken = |alpha: u8| {
        u8::try_from(u32::from(alpha) * u32::from(strength) / u32::from(u8::MAX)).unwrap_or(u8::MAX)
    };
    let _ = halo.push(HaloRing {
        radius: radius.saturating_add(rim),
        width: band.saturating_add(2 * rim),
        color: Color::rgba(0, 0, 0, weaken(RIM_ALPHA)),
    });
    let _ = halo.push(HaloRing {
        radius,
        width: band,
        color: Color::rgba(ink.r, ink.g, ink.b, weaken(ink.a)),
    });
}

#[cfg(test)]
#[path = "beacon_tests.rs"]
mod tests;
