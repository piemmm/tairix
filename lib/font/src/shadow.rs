//! The soft shadow behind a run of text drawn over ground its drawer does not
//! control — a label over a wallpaper, a caption over a photograph.
//!
//! The shadow is the run's own coverage, blurred and dropped just below the
//! ink, so the ground darkens around each stroke and nowhere else: the
//! picture stays whole, and the text keeps its separation from it.

use core::ops::Range;

use alloc::vec::Vec;

use tairix_geometry::Scale;
use tairix_raster::{div255, soften_coverage, Color, Pixel, Surface, SOFTEN_PASSES};

use crate::client::FontClient;
use crate::font::{draw_coverage, BitmapFont, Coverage};
use crate::glyph_cache::CachedGlyph;

/// How far below the ink the shadow sits, in logical pixels.
const DROP: u32 = 1;

/// The radius of each box pass the shadow is blurred by, in logical pixels.
const SOFTNESS: u32 = 1;

/// How much the blurred coverage is amplified before it is drawn.
///
/// Blurring a thin stroke spreads its coverage and lowers its peak to about a
/// third; amplifying restores a dense core against the stroke and keeps the
/// soft falloff beyond it.
const SPREAD: u32 = 2;

/// A soft shadow drawn behind a text run.
///
/// One definition of the shadow's drop, softness, and order — every shadow,
/// then the ink — so every surface that needs legible text over a picture
/// draws the same one rather than deriving its own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TextShadow {
    color: Color,
    drop: u32,
    radius: u32,
}

impl TextShadow {
    /// A shadow in `color` at `scale`: dropped one logical pixel below the ink
    /// and softened across about three more.
    ///
    /// Neither length is ever less than one physical pixel: a fraction of a
    /// pixel rounds to nothing, and a shadow that vanishes at some densities
    /// is worse than none at all.
    #[must_use]
    pub fn new(color: Color, scale: Scale) -> Self {
        Self {
            color,
            drop: scale.scale_length(DROP).max(1),
            radius: scale.scale_length(SOFTNESS).max(1),
        }
    }

    /// The colour the shadow is drawn in where it is densest.
    #[must_use]
    pub const fn color(self) -> Color {
        self.color
    }

    /// How far below the ink the shadow sits, in physical pixels.
    #[must_use]
    pub const fn drop(self) -> u32 {
        self.drop
    }

    /// How far past the ink the shadow can reach in every direction before it
    /// is dropped, in physical pixels.
    #[must_use]
    pub const fn reach(self) -> u32 {
        self.radius.saturating_mul(SOFTEN_PASSES)
    }

    /// This shadow at `strength` of its own opacity, for a run whose ink is
    /// fading by the same amount: a line on its way out takes its shadow with
    /// it.
    #[must_use]
    pub fn faded(self, strength: u8) -> Self {
        let Color { r, g, b, a } = self.color;
        Self {
            color: Color::rgba(r, g, b, div255(u32::from(a) * u32::from(strength))),
            ..self
        }
    }
}

/// Composite `shadow` for `text` drawn by `font` from the pen at `at`, and
/// answer the pen its ink ends at. The run is already warmed.
///
/// A shadow whose memory cannot be had is not drawn: the ink still is, so the
/// text stays readable wherever its ground allows.
pub(crate) fn draw_run_shadow(
    font: BitmapFont,
    client: &mut impl FontClient,
    surface: &mut Surface,
    at: (i32, i32),
    text: &str,
    shadow: TextShadow,
) -> i32 {
    let (x, y) = at;
    let mut ink = Ink::default();
    let pen = font.walk_on(client, x, text, |left, glyph| ink.include(left, glyph));
    if shadow.color.a == 0 {
        return pen;
    }
    let Some(mut mask) = ShadowMask::for_run(&ink, y, shadow, surface) else {
        return pen;
    };
    font.walk_on(client, x, text, |left, glyph| mask.add(left, glyph));
    mask.soften(shadow);
    mask.draw(surface, shadow);
    pen
}

/// The columns a run's glyph bitmaps span, and the tallest of them.
#[derive(Default)]
struct Ink {
    columns: Option<Range<i64>>,
    height: u32,
}

impl Ink {
    /// Take in one glyph whose bitmap starts at column `left`.
    fn include(&mut self, left: i32, glyph: &CachedGlyph) {
        if glyph.width == 0 || glyph.height == 0 {
            return;
        }
        let (start, end) = (i64::from(left), i64::from(left) + i64::from(glyph.width));
        self.columns = Some(match self.columns.take() {
            Some(held) => held.start.min(start)..held.end.max(end),
            None => start..end,
        });
        self.height = self.height.max(glyph.height);
    }
}

/// A run's coverage laid into one block, which is blurred into its shadow.
struct ShadowMask {
    /// The block's top-left in the paint's coordinates, the drop applied.
    x: i64,
    y: i64,
    width: u32,
    height: u32,
    /// Where the run's glyph rows start once dropped.
    line_top: i64,
    levels: Vec<u8>,
}

impl ShadowMask {
    /// The block `ink`'s shadow needs on `surface`, or `None` when none of it
    /// lands there or its memory cannot be had.
    ///
    /// It is the run's footprint — the ink grown by the blur's reach, dropped
    /// — cut to what the surface admits and grown back by the reach where it
    /// was cut. Every pixel that is drawn therefore averages exactly the
    /// coverage it would with nothing cut, and the block is never larger than
    /// the surface by more than the reach on each side, however long the run.
    fn for_run(ink: &Ink, top: i32, shadow: TextShadow, surface: &Surface) -> Option<Self> {
        let columns = ink.columns.clone()?;
        let reach = i64::from(shadow.reach());
        let top = i64::from(top) + i64::from(shadow.drop);
        let across = columns.start - reach..columns.end + reach;
        let down = top - reach..top + i64::from(ink.height) + reach;
        let (x, w) = placed(&across)?;
        let (y, h) = placed(&down)?;
        let (seen_across, seen_down) = surface.admitted(x, y, w, h)?;
        let across = grown(&seen_across, reach, &across);
        let down = grown(&seen_down, reach, &down);
        let width = u32::try_from(across.end - across.start).ok()?;
        let height = u32::try_from(down.end - down.start).ok()?;
        let count = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
        let mut levels = Vec::new();
        levels.try_reserve_exact(count).ok()?;
        levels.resize(count, 0);
        Some(Self {
            x: across.start,
            y: down.start,
            width,
            height,
            line_top: top,
            levels,
        })
    }

    /// Lay the coverage of one glyph whose bitmap starts at column `left` into
    /// the block, over what earlier glyphs laid there.
    fn add(&mut self, left: i32, glyph: &CachedGlyph) {
        let coverage = Coverage::of(glyph);
        let dx = i64::from(left) - self.x;
        let dy = self.line_top - self.y;
        let (Some(columns), Some(rows)) = (
            clipped(dx, coverage.width, self.width),
            clipped(dy, coverage.height, self.height),
        ) else {
            return;
        };
        let (Ok(width), Ok(glyph_width)) =
            (usize::try_from(self.width), usize::try_from(coverage.width))
        else {
            return;
        };
        for (source_row, row) in rows.source.zip(rows.target) {
            let (Some(from), Some(into)) =
                (source_row.checked_mul(glyph_width), row.checked_mul(width))
            else {
                continue;
            };
            let (Some(line), Some(held)) = (
                coverage
                    .levels
                    .get(from + columns.source.start..from + columns.source.end),
                self.levels
                    .get_mut(into + columns.target.start..into + columns.target.end),
            ) else {
                continue;
            };
            for (&level, slot) in line.iter().zip(held.iter_mut()) {
                *slot = slot.saturating_add(div255(u32::from(level) * u32::from(u8::MAX - *slot)));
            }
        }
    }

    /// Blur the laid coverage into the shadow's softness.
    fn soften(&mut self, shadow: TextShadow) {
        let (Ok(width), Ok(height), Ok(radius)) = (
            usize::try_from(self.width),
            usize::try_from(self.height),
            usize::try_from(shadow.radius),
        ) else {
            return;
        };
        let mut aux = Vec::new();
        if aux.try_reserve_exact(self.levels.len()).is_err() {
            return;
        }
        aux.resize(self.levels.len(), 0);
        soften_coverage(&mut self.levels, width, height, radius, &mut aux);
    }

    /// Composite the block onto `surface` in the shadow's colour.
    fn draw(&self, surface: &mut Surface, shadow: TextShadow) {
        let (Ok(x), Ok(y)) = (i32::try_from(self.x), i32::try_from(self.y)) else {
            return;
        };
        let coverage = Coverage {
            width: self.width,
            height: self.height,
            levels: &self.levels,
        };
        draw_coverage(
            surface,
            x,
            y,
            coverage,
            self.width,
            &spread_sources(shadow.color),
        );
    }
}

/// `span` as a paint start and length, clamped to the non-negative
/// coordinates a paint can name, or `None` when nothing of it is left.
fn placed(span: &Range<i64>) -> Option<(u32, u32)> {
    let start = span.start.max(0);
    let end = span.end.min(i64::from(u32::MAX));
    let len = u32::try_from(end.checked_sub(start)?)
        .ok()
        .filter(|len| *len > 0)?;
    Some((u32::try_from(start).ok()?, len))
}

/// `seen` grown by `reach` on both sides, kept within `whole`.
fn grown(seen: &Range<u32>, reach: i64, whole: &Range<i64>) -> Range<i64> {
    let start = (i64::from(seen.start) - reach).max(whole.start);
    let end = (i64::from(seen.end) + reach).min(whole.end);
    start..end.max(start)
}

/// Which of a glyph's `count` rows (or columns) placed at `offset` into a
/// block `limit` long land inside it, and where.
struct Clipped {
    source: Range<usize>,
    target: Range<usize>,
}

fn clipped(offset: i64, count: u32, limit: u32) -> Option<Clipped> {
    let first = (-offset).max(0);
    let last = (i64::from(limit) - offset).min(i64::from(count));
    if first >= last {
        return None;
    }
    let source = usize::try_from(first).ok()?..usize::try_from(last).ok()?;
    let target = usize::try_from(offset + first).ok()?..usize::try_from(offset + last).ok()?;
    Some(Clipped { source, target })
}

/// The premultiplied source pixel for each blurred coverage level, amplified
/// by [`SPREAD`] and saturating at the colour's own alpha.
fn spread_sources(color: Color) -> [Pixel; 256] {
    let source = color.premultiply();
    let mut sources = [source; 256];
    for (level, slot) in (0u32..).zip(sources.iter_mut()) {
        let spread = u8::try_from(level.saturating_mul(SPREAD).min(255)).unwrap_or(u8::MAX);
        *slot = source.scale_alpha(spread);
    }
    sources
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod tests;
