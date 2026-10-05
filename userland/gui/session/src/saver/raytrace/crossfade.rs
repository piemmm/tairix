//! A paint's change faded in rather than laid straight on: the tiles it
//! touches go from what the screen showed to the painter's new picture, frame
//! by frame, over the wait until the next paint.
//!
//! The painter paints into a picture of its own here, never the screen. What
//! the screen showed is kept only where the change lies, and a frame writes
//! only there, so a fade costs what the change touches however much of the
//! screen stays as it was.

use tairix_parallel::JobRunner;
use tairix_raster::{mix_span, DitherRow, Pixel};
use tairix_theme::Timeline;
use tairix_wm::{Color, Compositor, Region, Surface, WindowId};

use super::tiles::{Tiles, TILE};

/// The fewest pixels worth handing another core to blend or copy: a pixel
/// costs a few multiplies, so a hand-off must carry many.
const GRAIN: usize = 16_384;

/// The pixels of a whole tile.
const TILE_PIXELS: usize = (TILE * TILE) as usize;

/// One screen's changes faded in, one at a time.
pub(super) struct Crossfade {
    /// The picture the painter paints: what the screen shows once a fade is
    /// over.
    picture: Surface,
    /// What the screen showed over the changing tiles as their fade began.
    before: Surface,
    tiles: Tiles,
    /// The cover of `tiles` each frame of the fade marks.
    cover: Region,
    timeline: Timeline,
    /// How far in the change on screen is, of 255.
    shown: u8,
}

impl Crossfade {
    /// The bytes a crossfade over a `size` screen holds.
    pub(super) fn bytes((width, height): (u32, u32)) -> u64 {
        let pixel = u64::try_from(size_of::<Pixel>()).unwrap_or(u64::MAX);
        2u64.saturating_mul(u64::from(width))
            .saturating_mul(u64::from(height))
            .saturating_mul(pixel)
    }

    /// A crossfade over a `size` screen showing black; `None` when the heap
    /// will not hold it.
    pub(super) fn new((width, height): (u32, u32)) -> Option<Self> {
        Some(Self {
            picture: Surface::filled(width, height, Color::rgb(0, 0, 0).premultiply())?,
            before: Surface::new(width, height)?,
            tiles: Tiles::new((width, height))?,
            cover: Region::new(),
            timeline: Timeline::SETTLED,
            shown: u8::MAX,
        })
    }

    /// Whether the screen shows all of the last change.
    pub(super) const fn settled(&self) -> bool {
        self.shown == u8::MAX
    }

    /// Begin fading in, over `timeline`, the change touching `tiles` whose
    /// cover is `cover`, the last fade having settled: keep what the screen
    /// shows there, and answer the picture to paint the change into. `cover`
    /// is taken, and left holding nothing of use.
    pub(super) fn begin(
        &mut self,
        (tiles, cover): (&Tiles, &mut Region),
        timeline: Timeline,
        runner: &dyn JobRunner,
    ) -> &mut Surface {
        self.tiles.copy_from(tiles);
        core::mem::swap(&mut self.cover, cover);
        let picture = &self.picture;
        each_span(&self.tiles, &mut self.before, runner, &|y, x, kept| {
            if let Some((_, shown)) = picture.row_span(y, x, span_width(kept)) {
                for (kept, shown) in kept.iter_mut().zip(shown) {
                    *kept = *shown;
                }
            }
        });
        self.timeline = timeline;
        self.shown = 0;
        &mut self.picture
    }

    /// How far in the fade wants the change at `now_ns`, of 255, where the
    /// screen shows otherwise.
    pub(super) fn due(&self, now_ns: u64) -> Option<u8> {
        let weight = self.timeline.progress(now_ns);
        (weight != self.shown).then_some(weight)
    }

    /// Show the change `weight` of the way in on window `wm`, `size`,
    /// marking its cover; `false` when the window has gone.
    pub(super) fn show(
        &mut self,
        weight: u8,
        (wm, size): (WindowId, (u32, u32)),
        compositor: &mut Compositor,
    ) -> bool {
        let runner = compositor.job_runner();
        let drawn = compositor.repaint_window(wm, size, &self.cover, |screen, _| {
            self.draw(screen, weight, runner);
        });
        if drawn {
            self.shown(weight);
        }
        drawn
    }

    /// Blend the change `weight` of the way in onto `screen`, across
    /// `runner`, writing its tiles and nothing else.
    fn draw(&self, screen: &mut Surface, weight: u8, runner: &dyn JobRunner) {
        let (before, after) = (&self.before, &self.picture);
        each_span(&self.tiles, screen, runner, &|y, x, span| {
            let width = span_width(span);
            if let (Some((_, from)), Some((_, to))) =
                (before.row_span(y, x, width), after.row_span(y, x, width))
            {
                mix_span(span, from, to, weight, DitherRow::at(y), x);
            }
        });
    }

    /// Note that the screen shows the change `weight` of the way in.
    fn shown(&mut self, weight: u8) {
        self.shown = weight;
        if weight == u8::MAX {
            self.timeline.settle();
        }
    }
}

/// Hand `visit` each row of each run of `tiles` in `surface`, with the column
/// it starts at, whole tile rows a band across `runner`.
fn each_span(
    tiles: &Tiles,
    surface: &mut Surface,
    runner: &dyn JobRunner,
    visit: &(dyn Fn(u32, u32, &mut [Pixel]) + Sync),
) {
    let pieces = tairix_parallel::bands(runner, tiles.count().saturating_mul(TILE_PIXELS), GRAIN);
    if pieces == 0 {
        return;
    }
    let per_band = tairix_raster::band_rows(tiles.rows(), pieces).saturating_mul(TILE);
    let height = surface.height();
    tairix_parallel::for_each_drawn(
        runner,
        surface.row_bands_mut(0..height, per_band),
        &|mut band| {
            let lines = band.rows();
            let rows = (lines.start / TILE) as usize..lines.end.div_ceil(TILE) as usize;
            for (columns, run) in tiles.spans(rows) {
                let width = columns.end - columns.start;
                for y in run.start.max(lines.start)..run.end.min(lines.end) {
                    if let Some((first, span)) = band.row_span_mut(y, columns.start, width) {
                        visit(y, first, span);
                    }
                }
            }
        },
    );
}

/// The columns `span` holds.
fn span_width(span: &[Pixel]) -> u32 {
    u32::try_from(span.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "crossfade_tests.rs"]
mod tests;
