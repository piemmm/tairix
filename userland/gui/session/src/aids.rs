//! Pointer aids: what the desktop does to help a user find the pointer and
//! follow it — growing it when it is shaken, leaving a trail behind it, and
//! sending rings to it when Ctrl is pressed on its own.
//!
//! Each is stepped once a frame against the one clock reading the frame is
//! shown at, and each asks for a frame only while it is changing, so an idle
//! desktop with every aid on still parks indefinitely. Nothing is drawn while
//! the pointer itself is withheld from the screen — under the screensaver —
//! and whatever was in flight is dropped rather than resumed later.
//!
//! The shadow a pointer may cast is not an aid in this sense: it is part of
//! the pointer's artwork
//! ([`CursorController::set_shadow`](tairix_wm::CursorController::set_shadow)).

mod beacon;
mod shake;
mod trail;

use tairix_geometry::Point;
use tairix_inline::ArrayVec;
use tairix_wallpaper::{DesktopSettings, PointerTrail};
use tairix_wm::{Compositor, CursorController, Ghost, Halo, MAX_GHOSTS};

use crate::switchuser::park_within;

use beacon::Beacon;
use shake::Shake;
use trail::Trail;

/// Which aids the user has asked for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AidPolicy {
    /// Whether shaking the pointer grows it.
    pub shake: bool,
    /// How long a trail it leaves.
    pub trail: PointerTrail,
    /// Whether a lone press of Ctrl shows where it is.
    pub locate: bool,
}

impl AidPolicy {
    /// The aids `settings` ask for.
    #[must_use]
    pub const fn of(settings: &DesktopSettings) -> Self {
        Self {
            shake: settings.cursor_shake,
            trail: settings.cursor_trail,
            locate: settings.cursor_locate,
        }
    }

    /// No aid at all.
    pub const NONE: Self = Self {
        shake: false,
        trail: PointerTrail::Off,
        locate: false,
    };
}

/// The pointer aids and what each has in flight.
#[derive(Clone, Debug)]
pub struct PointerAids {
    policy: AidPolicy,
    shake: Shake,
    trail: Trail,
    beacon: Beacon,
    /// The trail as last drawn, kept to be drawn into again.
    ghosts: ArrayVec<Ghost, MAX_GHOSTS>,
}

impl Default for PointerAids {
    fn default() -> Self {
        Self::new()
    }
}

impl PointerAids {
    /// Aids with none asked for.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            policy: AidPolicy::NONE,
            shake: Shake::new(),
            trail: Trail::new(),
            beacon: Beacon::new(),
            ghosts: ArrayVec::new(),
        }
    }

    /// Offer the aids `policy` asks for from now on. One turned off stops at
    /// once, and what it had in flight is gone at the next frame.
    pub fn set_policy(&mut self, policy: AidPolicy) {
        if !policy.shake {
            self.shake.reset();
        }
        if !policy.locate {
            self.beacon.stop();
        }
        self.trail.set_length(policy.trail);
        self.policy = policy;
    }

    /// The aids offered.
    #[must_use]
    pub const fn policy(&self) -> AidPolicy {
        self.policy
    }

    /// Show where the pointer is, starting at `now_ns`: the answer to a lone
    /// press of Ctrl, if the user asked for one.
    pub fn locate(&mut self, now_ns: u64) {
        if self.policy.locate {
            self.beacon.start(now_ns);
        }
    }

    /// Step every aid to `now_ns` with the pointer at `at`, and put what each
    /// now draws on screen.
    pub fn advance(
        &mut self,
        now_ns: u64,
        at: Point,
        cursor: &mut CursorController,
        compositor: &mut Compositor,
    ) {
        if compositor.cursor_hidden() {
            self.rest(at, cursor, compositor);
            return;
        }
        let scale = compositor.scale();
        let enlargement = if self.policy.shake {
            self.shake.observe(now_ns, at, scale);
            self.shake.level(now_ns, compositor.theme().motion())
        } else {
            0
        };
        cursor.set_enlargement(enlargement, at, compositor);
        self.trail.observe(now_ns, at);
        self.trail.ghosts(now_ns, at, &mut self.ghosts);
        compositor.set_pointer_trail(&self.ghosts);
        let halo = self.beacon.halo(now_ns, compositor.theme(), scale);
        compositor.set_pointer_halo(&halo);
    }

    /// Drop everything in flight and draw nothing, with the pointer back at
    /// its own size for when it is shown again.
    fn rest(&mut self, at: Point, cursor: &mut CursorController, compositor: &mut Compositor) {
        self.shake.reset();
        self.trail.clear();
        self.beacon.stop();
        self.ghosts.clear();
        cursor.set_enlargement(0, at, compositor);
        compositor.set_pointer_trail(&[]);
        compositor.set_pointer_halo(&Halo::new());
    }

    /// Fold what the aids next owe a frame into `park_ns`, relative to
    /// `now_ns`; nothing while every one of them is at rest.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        let park_ns = park_within(park_ns, self.shake.next_frame_in(now_ns));
        let park_ns = park_within(park_ns, self.trail.next_frame_in(now_ns));
        park_within(park_ns, self.beacon.next_frame_in(now_ns))
    }
}

#[cfg(test)]
#[path = "aids_tests.rs"]
mod tests;
