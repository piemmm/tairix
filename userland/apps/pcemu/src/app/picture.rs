//! A machine window's picture: the screen drawn into the window straight or,
//! while one of the tube's effects is on, through the tube.

use core::mem;
use core::ops::Range;

use tairix_abi::driver::display::RefreshRate;
use tairix_crt::{frame_due, Phase, Pipeline, Tube};
use tairix_geometry::{Rect, Region};
use tairix_parallel::JobRunner;
use tairix_raster::resample::Region as Target;
use tairix_raster::{SharpError, Surface};

use super::screen::Screen;

/// What a picture is drawn with.
#[derive(Clone, Copy)]
pub struct Drawing<'a> {
    /// The desktop's density, in percent of the reference.
    pub percent: u32,
    /// The workers the tube's stages spread across.
    pub runner: &'a dyn JobRunner,
    /// The display's refresh rate, which the tube's motion keeps to.
    pub refresh: RefreshRate,
    /// The monotonic time of the draw, in nanoseconds.
    pub now_ns: u64,
}

/// A machine's screen and the tube it is drawn through.
#[derive(Debug, Default)]
pub struct Picture {
    screen: Screen,
    tube: Tube,
    crt: Pipeline,
    /// The plain picture the tube draws over, while one of its effects is on.
    plain: Option<Surface>,
    /// The screen last drew into `plain`, so what it remembers drawing is not
    /// what the window shows.
    screen_in_plain: bool,
    phase: Phase,
    due: Option<u64>,
}

impl Picture {
    /// A picture with no frame yet, drawn through `tube`.
    #[must_use]
    pub fn new(tube: Tube) -> Self {
        Self {
            tube,
            ..Self::default()
        }
    }

    /// The screen as the machine last showed it.
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Take a frame, as [`Screen::take`]: whether it changed anything.
    pub fn take(&mut self, width: u32, height: u32, pixels: &[u8]) -> bool {
        self.screen.take(width, height, pixels)
    }

    /// The tube the picture is drawn through.
    #[must_use]
    pub const fn tube(&self) -> Tube {
        self.tube
    }

    /// When the tube's next animation step is due; none while nothing moves.
    #[must_use]
    pub const fn due(&self) -> Option<u64> {
        self.due
    }

    /// Draw through `tube` from the next draw on, its stages begun afresh.
    pub fn set_tube(&mut self, tube: Tube) {
        self.tube = tube;
        self.crt.clear();
    }

    /// Draw the whole picture at the next draw: the window under it was
    /// painted over.
    pub fn invalidate(&mut self) {
        self.screen.invalidate();
        self.crt.clear();
    }

    /// Move to the tube's next animation step if it is due by `now_ns`:
    /// whether it was, and the picture wants drawing.
    pub fn step(&mut self, now_ns: u64) -> bool {
        if self.due.is_none_or(|due| due > now_ns) {
            return false;
        }
        self.due = None;
        self.phase = self.phase.advance();
        true
    }

    /// Draw into `target` of `window` what changed since the last draw: the
    /// part of the window drawn.
    ///
    /// The tube draws while one of its effects is on and the process can
    /// have its buffers, and the plain picture otherwise, replacing the
    /// tube's whole: a refused buffer costs the effect, never the picture.
    ///
    /// # Errors
    ///
    /// As [`Screen::draw`], for the plain picture.
    pub fn draw(
        &mut self,
        window: &mut Surface,
        target: Target,
        drawing: Drawing<'_>,
    ) -> Result<Region, SharpError> {
        let (_, passes) = self.tube.passes(drawing.percent);
        if passes > 0 {
            if let Some(drawn) = self.draw_tubed(window, target, drawing) {
                return Ok(drawn);
            }
        }
        self.plain = None;
        self.crt.clear();
        self.due = None;
        if mem::take(&mut self.screen_in_plain) {
            self.screen.invalidate();
        }
        let rows = self.screen.draw(window, target)?;
        let mut drawn = Region::new();
        if let Some(rect) = placed(target, rows) {
            drawn.add(rect);
        }
        Ok(drawn)
    }

    /// Draw the picture into the plain surface, the tube over it, and what
    /// the tube changed into `target` of `window`: none where the process
    /// cannot have the buffers.
    fn draw_tubed(
        &mut self,
        window: &mut Surface,
        target: Target,
        drawing: Drawing<'_>,
    ) -> Option<Region> {
        let fits = self
            .plain
            .as_ref()
            .is_some_and(|plain| (plain.width(), plain.height()) == (target.width, target.height));
        if !fits {
            // The old buffer goes before the new one is asked for.
            self.plain = None;
            self.crt.clear();
            self.screen_in_plain = false;
            self.plain = Some(Surface::new(target.width, target.height)?);
        }
        if !mem::replace(&mut self.screen_in_plain, true) {
            self.screen.invalidate();
        }
        let (left, top) = (i32::try_from(target.x).ok()?, i32::try_from(target.y).ok()?);
        let plain = self.plain.as_mut()?;
        let whole = Target {
            x: 0,
            y: 0,
            ..target
        };
        let rows = self.screen.draw(plain, whole).ok()?;
        let damage = placed(whole, rows)?;
        let frame = self.crt.render(
            plain,
            damage,
            self.tube,
            drawing.percent,
            self.phase,
            drawing.runner,
        )?;
        let mut drawn = frame.changed.clone();
        drawn.translate(left, top);
        for at in drawn.rects() {
            if let (Ok(x), Ok(y)) = (u32::try_from(at.left()), u32::try_from(at.top())) {
                window.with_clip(x, y, at.width, at.height, |window| {
                    window.overwrite(left, top, frame.picture);
                });
            }
        }
        let animated = self.crt.animated(self.tube, drawing.percent);
        self.due = frame_due(animated, self.due, drawing.now_ns, drawing.refresh);
        Some(drawn)
    }
}

/// The window's rectangle of `rows` of a picture drawn into `target`.
fn placed(target: Target, rows: Range<u32>) -> Option<Rect> {
    Some(Rect::new(
        i32::try_from(target.x).ok()?,
        i32::try_from(target.y.checked_add(rows.start)?).ok()?,
        target.width,
        rows.end.checked_sub(rows.start)?,
    ))
}

#[cfg(test)]
#[path = "picture_tests.rs"]
mod tests;
