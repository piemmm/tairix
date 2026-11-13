//! A machine's screen in a window: its frame kept as straight RGBA8, the
//! rows changed since it was last drawn, the largest 4:3 picture the window
//! holds, and the tube drawn over it.
//!
//! A frame arrives whole, and only the rows that differ are counted changed,
//! or row by row from a producer that already knows which rows it wrote. A
//! draw enlarges only what a change reaches in a target it has drawn before.

use alloc::vec::Vec;
use core::mem;
use core::ops::Range;

use tairix_abi::driver::display::RefreshRate;
use tairix_geometry::{Rect, Region};
use tairix_parallel::JobRunner;
use tairix_raster::resample::{resample_window, Region as Target, ResampleError};
use tairix_raster::{Pixel, Rgba8Image, SharpError, SharpScaler, Surface};

use crate::{frame_due, Phase, Pipeline, Tube};

/// A monitor's shape: four across for every three down.
pub const ASPECT: (u32, u32) = (4, 3);

/// The bytes of one straight RGBA8 dot.
pub const DOT: usize = 4;

/// The largest [`ASPECT`] rectangle a `width`×`height` area holds, centred on
/// whole pixels; `None` for one too small to hold any.
#[must_use]
pub fn fit(width: u32, height: u32) -> Option<Target> {
    let (across, down) = ASPECT;
    let (w, h) = if u64::from(width) * u64::from(down) <= u64::from(height) * u64::from(across) {
        (width, width * down / across)
    } else {
        (height * across / down, height)
    };
    (w > 0 && h > 0).then(|| Target {
        x: (width - w) / 2,
        y: (height - h) / 2,
        width: w,
        height: h,
    })
}

/// How a frame is enlarged to its picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scaling {
    /// Each dot a block of whole pixels, blended only where blocks meet.
    #[default]
    Sharp,
    /// Bilinear throughout.
    Smooth,
}

/// Why a screen could not be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawError {
    /// The sharp enlargement refused.
    Sharp(SharpError),
    /// The smooth one did.
    Smooth(ResampleError),
}

/// The frame a machine last showed and what of it is still to be drawn.
#[derive(Debug, Default)]
pub struct Screen {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    scaling: Scaling,
    scaler: SharpScaler,
    /// The smooth enlargement's rows on their way to the surface.
    smoothed: Vec<u8>,
    /// The source rows changed since the last draw.
    changed: Option<Range<u32>>,
    /// The target last drawn whole, while it still holds this frame.
    drawn: Option<Target>,
}

impl Screen {
    /// A screen with no frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            scaling: Scaling::Sharp,
            scaler: SharpScaler::new(),
            smoothed: Vec::new(),
            changed: None,
            drawn: None,
        }
    }

    /// The frame's dots and lines; zero before the first.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// How the frame is enlarged.
    #[must_use]
    pub const fn scaling(&self) -> Scaling {
        self.scaling
    }

    /// Enlarge by `scaling` from now on, the next draw whole.
    pub fn set_scaling(&mut self, scaling: Scaling) {
        if scaling != self.scaling {
            self.scaling = scaling;
            self.drawn = None;
        }
    }

    /// Take a whole `width`×`height` frame, answering whether it changed what
    /// is shown: a frame not of that size is refused.
    pub fn take(&mut self, width: u32, height: u32, pixels: &[u8]) -> bool {
        let Some(stride) = stride(width) else {
            return false;
        };
        let whole = usize::try_from(height)
            .ok()
            .and_then(|h| h.checked_mul(stride));
        if stride == 0 || whole != Some(pixels.len()) {
            return false;
        }
        if (width, height) != (self.width, self.height) {
            let mut fresh = Vec::new();
            if fresh.try_reserve_exact(pixels.len()).is_err() {
                return false;
            }
            fresh.extend_from_slice(pixels);
            self.pixels = fresh;
            (self.width, self.height) = (width, height);
            self.drawn = None;
            return true;
        }
        let rows = self
            .pixels
            .chunks_exact(stride)
            .zip(pixels.chunks_exact(stride));
        let mut changed = rows.enumerate().filter(|(_, (old, new))| old != new);
        let Some((first, _)) = changed.next() else {
            return false;
        };
        let last = changed.next_back().map_or(first, |(row, _)| row);
        let band = first * stride..(last + 1) * stride;
        self.pixels[band.clone()].copy_from_slice(&pixels[band]);
        self.mark(as_row(first)..as_row(last + 1));
        true
    }

    /// Hold a `width`×`height` frame written row by row through
    /// [`row_mut`](Self::row_mut): a new size starts black and draws whole.
    /// `false`, the screen as it was, for a size it cannot hold.
    pub fn reshape(&mut self, width: u32, height: u32) -> bool {
        if (width, height) == (self.width, self.height) {
            return true;
        }
        let Some(len) = stride(width).and_then(|stride| {
            usize::try_from(height)
                .ok()
                .and_then(|h| h.checked_mul(stride))
        }) else {
            return false;
        };
        let mut fresh = Vec::new();
        if len == 0 || fresh.try_reserve_exact(len).is_err() {
            return false;
        }
        fresh.resize(len, 0);
        self.pixels = fresh;
        (self.width, self.height) = (width, height);
        self.drawn = None;
        true
    }

    /// Row `row`'s dots to write, counted changed; `None` past the frame.
    pub fn row_mut(&mut self, row: u32) -> Option<&mut [u8]> {
        let stride = stride(self.width)?;
        let start = usize::try_from(row).ok()?.checked_mul(stride)?;
        let end = start.checked_add(stride)?;
        if end > self.pixels.len() {
            return None;
        }
        self.mark(row..row.checked_add(1)?);
        self.pixels.get_mut(start..end)
    }

    /// Forget what has been drawn: the next draw is whole.
    pub fn invalidate(&mut self) {
        self.drawn = None;
    }

    /// The frame's straight RGBA8 dots, row after row.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Draw into `target` of `surface` what it does not yet show, answering
    /// the target rows drawn: all of them for a target not drawn before, else
    /// those the changed rows reach.
    ///
    /// # Errors
    ///
    /// [`DrawError`]: the target is empty or too large, or the heap would
    /// not hold the enlargement.
    pub fn draw(&mut self, surface: &mut Surface, target: Target) -> Result<Range<u32>, DrawError> {
        let Ok(frame) = Rgba8Image::new(self.width, self.height, &self.pixels) else {
            return Ok(0..0);
        };
        let whole = self.drawn != Some(target);
        let rows = match (whole, mem::take(&mut self.changed), self.scaling) {
            (true, _, _) => 0..target.height,
            (false, Some(changed), Scaling::Sharp) => self.scaler.rows_reached(changed),
            (false, Some(changed), Scaling::Smooth) => {
                smoothed_rows(changed, self.height, target.height)
            }
            (false, None, _) => return Ok(0..0),
        };
        let drawn = match self.scaling {
            Scaling::Sharp => self
                .scaler
                .draw(&frame, surface, target, rows.clone())
                .map_err(DrawError::Sharp),
            Scaling::Smooth => smooth(&frame, surface, target, rows.clone(), &mut self.smoothed),
        };
        // What a refused draw left in the surface is unknown, so it is drawn
        // whole next.
        self.drawn = drawn.is_ok().then_some(target);
        drawn.map(|()| rows)
    }

    fn mark(&mut self, rows: Range<u32>) {
        self.changed = Some(match self.changed.take() {
            Some(was) => was.start.min(rows.start)..was.end.max(rows.end),
            None => rows,
        });
    }
}

fn stride(width: u32) -> Option<usize> {
    usize::try_from(width).ok()?.checked_mul(DOT)
}

fn as_row(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The target rows a bilinear enlargement or reduction of `height` source
/// rows to `target` reads `changed` from: each target row's footprint, a
/// source row either side.
fn smoothed_rows(changed: Range<u32>, height: u32, target: u32) -> Range<u32> {
    if changed.is_empty() || height == 0 {
        return 0..0;
    }
    let reach = 1 + u64::from(height.div_ceil(target.max(1)));
    let (height, target) = (u64::from(height), u64::from(target));
    let start = u64::from(changed.start).saturating_sub(reach) * target / height;
    let end = (u64::from(changed.end) + reach) * target;
    let end = end.div_ceil(height).min(target);
    let as_target = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    as_target(start)..as_target(end)
}

/// `rows` of `target` drawn from `frame` bilinear, through `scratch`.
fn smooth(
    frame: &Rgba8Image<'_>,
    surface: &mut Surface,
    target: Target,
    rows: Range<u32>,
    scratch: &mut Vec<u8>,
) -> Result<(), DrawError> {
    if target.width == 0 || target.height == 0 {
        return Err(DrawError::Smooth(ResampleError::EmptyDestination));
    }
    let rows = rows.start..rows.end.min(target.height);
    if rows.is_empty() {
        return Ok(());
    }
    let window = Target {
        x: 0,
        y: rows.start,
        width: target.width,
        height: rows.end - rows.start,
    };
    let row_bytes =
        stride(window.width).ok_or(DrawError::Smooth(ResampleError::DestinationTooLarge))?;
    let len = usize::try_from(window.height)
        .ok()
        .and_then(|height| row_bytes.checked_mul(height))
        .ok_or(DrawError::Smooth(ResampleError::DestinationTooLarge))?;
    scratch.clear();
    if scratch.try_reserve_exact(len).is_err() {
        return Err(DrawError::Smooth(ResampleError::OutOfMemory));
    }
    scratch.resize(len, 0);
    let whole = Target {
        x: 0,
        y: 0,
        width: frame.width(),
        height: frame.height(),
    };
    resample_window(frame, whole, target.width, target.height, window, scratch)
        .map_err(DrawError::Smooth)?;
    for (line, y) in scratch.chunks_exact(row_bytes).zip(rows) {
        let Some(at) = target.y.checked_add(y) else {
            break;
        };
        let Some((first, span)) = surface.row_span_mut(at, target.x, target.width) else {
            continue;
        };
        let skip = usize::try_from(first - target.x).unwrap_or(usize::MAX);
        let dots = line.as_chunks::<DOT>().0.iter().skip(skip);
        for (pixel, dot) in span.iter_mut().zip(dots) {
            *pixel = Pixel {
                r: dot[0],
                g: dot[1],
                b: dot[2],
                a: 0xFF,
            };
        }
    }
    Ok(())
}

/// How the window a picture is drawn into is shown.
#[derive(Clone, Copy)]
pub struct Drawing<'a> {
    /// The desktop's scale, in percent.
    pub percent: u32,
    /// The runner the tube's stages spread across.
    pub runner: &'a dyn JobRunner,
    /// The display's refresh rate a moving tube is paced to.
    pub refresh: RefreshRate,
    /// Now, on the monotonic clock.
    pub now_ns: u64,
}

/// A [`Screen`] seen through a [`Tube`]: drawn plain into a surface of its
/// own, the tube over that, and what changed copied into the window.
#[derive(Debug, Default)]
pub struct Picture {
    screen: Screen,
    tube: Tube,
    crt: Pipeline,
    /// The screen drawn plain, the tube's input, at the target's size.
    plain: Option<Surface>,
    /// Whether the screen was last drawn into `plain` rather than the window.
    screen_in_plain: bool,
    phase: Phase,
    due: Option<u64>,
}

impl Picture {
    /// A picture with no frame yet, seen through `tube`.
    #[must_use]
    pub fn new(tube: Tube) -> Self {
        Self {
            tube,
            ..Self::default()
        }
    }

    /// The screen.
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The screen, to take frames or rows.
    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    /// Take a whole frame ([`Screen::take`]).
    pub fn take(&mut self, width: u32, height: u32, pixels: &[u8]) -> bool {
        self.screen.take(width, height, pixels)
    }

    /// The tube.
    #[must_use]
    pub const fn tube(&self) -> Tube {
        self.tube
    }

    /// When the moving tube next steps, if it moves.
    #[must_use]
    pub const fn due(&self) -> Option<u64> {
        self.due
    }

    /// Look through `tube` from now on, redrawn whole.
    pub fn set_tube(&mut self, tube: Tube) {
        self.tube = tube;
        self.crt.clear();
    }

    /// Forget what has been drawn: the next draw is whole.
    pub fn invalidate(&mut self) {
        self.screen.invalidate();
        self.crt.clear();
    }

    /// Step a moving tube that is due at `now_ns`, answering whether it moved.
    pub fn step(&mut self, now_ns: u64) -> bool {
        if self.due.is_none_or(|due| due > now_ns) {
            return false;
        }
        self.due = None;
        self.phase = self.phase.advance();
        true
    }

    /// Draw into `target` of `window` what it does not yet show, answering
    /// the window's region drawn. A tube the heap cannot hold is shown
    /// plain, whole, until it can be.
    ///
    /// # Errors
    ///
    /// [`DrawError`], from the screen's enlargement.
    pub fn draw(
        &mut self,
        window: &mut Surface,
        target: Target,
        drawing: Drawing<'_>,
    ) -> Result<Region, DrawError> {
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

fn placed(target: Target, rows: Range<u32>) -> Option<Rect> {
    Some(Rect::new(
        i32::try_from(target.x).ok()?,
        i32::try_from(target.y.checked_add(rows.start)?).ok()?,
        target.width,
        rows.end.checked_sub(rows.start)?,
    ))
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
