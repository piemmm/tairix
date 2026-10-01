//! The ribbon of light behind the login column.
//!
//! It is its own layer beneath the painted surface, repainted only where the
//! ribbon moved, so a frame of the ribbon never paints the column again and a
//! keystroke never paints the ribbon.

use tairix_geometry::{Rect, Region};
use tairix_raster::Surface;
use tairix_ribbon::Light;
use tairix_theme::motion::SceneClock;

/// The ribbon of light the login screen stands over.
pub struct Scene {
    light: Light,
    motion: SceneClock,
    /// The ribbon's pixels, the screen's size.
    layer: Surface,
    /// The rectangle the column stands in, which the ribbon keeps dark.
    clear: Rect,
    /// What the last frame repainted.
    damage: Region,
}

impl Scene {
    /// The ribbon across `screen`, kept clear of `clear`, painted whole as it
    /// stands at `now_ns` and holding still when `still`; `None` when the heap
    /// will not give it.
    #[must_use]
    pub fn new(screen: Rect, clear: Rect, now_ns: u64, still: bool) -> Option<Self> {
        let mut light = Light::new((screen.width, screen.height), clear, 0.0)?;
        let mut layer = Surface::new(screen.width, screen.height)?;
        light.paint(&mut layer, Rect::new(0, 0, screen.width, screen.height));
        Some(Self {
            light,
            motion: SceneClock::new(now_ns, still),
            layer,
            clear,
            damage: Region::new(),
        })
    }

    /// Nanoseconds from `now_ns` until the ribbon's next frame, or `None`
    /// while it holds still.
    #[must_use]
    pub fn due_in(&self, now_ns: u64) -> Option<u64> {
        self.motion.due_ns().map(|due| due.saturating_sub(now_ns))
    }

    /// Move the ribbon on to `now_ns` if a frame is due, repainting what it
    /// moved, and answer whether anything did; [`damage`](Self::damage) then
    /// holds those pixels.
    pub fn advance(&mut self, now_ns: u64) -> bool {
        if !self.motion.frame_due(now_ns) {
            return false;
        }
        let t = self.motion.advance(now_ns);
        self.damage.clear();
        if !self.light.step(t, self.clear, &mut self.damage) {
            return false;
        }
        self.light.paint_moved(&mut self.layer, |_, _| {});
        !self.damage.is_empty()
    }

    /// The pixels the last frame that moved repainted.
    #[must_use]
    pub const fn damage(&self) -> &Region {
        &self.damage
    }

    /// The ribbon's pixels.
    #[must_use]
    pub const fn layer(&self) -> &Surface {
        &self.layer
    }
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
