//! Ribbon, a TAIRiX screensaver: the minimal clock, its time and date held
//! still while a ribbon of orange light travels around them.
//!
//! The time is the host's reading, spelled as the icon bar spells it, set in a
//! hairline weight of the desktop's own face with the date spelled out beneath
//! it. The text never moves; the ribbon is what changes. Under reduced motion
//! the ribbon holds still too, and only the reading changing redraws anything.

#![no_std]
#![deny(missing_docs)]

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::font_ipc::FontWeight;
use tairix_abi::Errno;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_parallel::Pool;
use tairix_raster::{Color, Surface};
use tairix_ribbon::Light;
use tairix_saver::{footprint, DateShown, Frame, Host, NextFrame, Scene, Setup, Told};
use tairix_theme::motion::SceneClock;
use tairix_theme::{TextRole, Theme};

/// The time's line box, and where its baseline sits, in thousandths of the
/// screen's height.
const TIME_BOX: u32 = 290;
const TIME_BASELINE: u32 = 380;

/// The date's line box, and how far its baseline sits beneath the time's, in
/// thousandths of the time's line box.
const DATE_BOX: u32 = 259;
const DATE_DROP: u32 = 431;

/// The widest the time is drawn, in thousandths of the screen's width.
const TIME_WIDEST: u32 = 800;

/// The time's hairline weight, and the date's.
const TIME_WEIGHT: FontWeight = match FontWeight::new(150) {
    Ok(weight) => weight,
    Err(_) => panic!("the hairline weight is on the axis"),
};
const DATE_WEIGHT: FontWeight = FontWeight::REGULAR;

/// The time's ink, and the date's quieter one.
const TIME_INK: Color = Color::rgb(0xF8, 0xF8, 0xF9);
const DATE_INK: Color = Color::rgb(0xD6, 0xD6, 0xD9);

/// A time as long as any the face tells, which its type is fitted to.
const LONGEST_TIME: &str = "00:00";

/// The minimal clock screensaver.
pub struct Ribbon {
    /// The reading the block was last composed from; `None` before the first
    /// frame.
    shown: Option<Told>,
    /// The date told beneath the time, if one is.
    date: DateShown,
    /// The time's type and the date's.
    fonts: [BitmapFont; 2],
    /// The cell every figure of the time is set in: its widest figure.
    figure: u32,
    /// Where each line's baseline sits, in rows.
    baselines: [i32; 2],
    /// The lines composed together, or `None` when there is nothing to show.
    block: Option<Surface>,
    at: Point,
    light: Light,
    /// The threads the ribbon is painted across.
    runner: Pool,
    motion: SceneClock,
    refresh: RefreshRate,
    screen: (u32, u32),
}

impl Ribbon {
    /// The face `setup` describes.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for an option the host did not send as the
    /// manifest declares it, [`Errno::OutOfMemory`] when the heap will not
    /// give the ribbon.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let start = setup.start;
        let screen = (start.region.width(), start.region.height());
        let (fonts, baselines) = typeset(&setup.theme(), setup.scale, screen);
        // Arranged around the text once the first reading is composed.
        let mut light = Light::new(screen, Rect::EMPTY, 0.0).ok_or(Errno::OutOfMemory)?;
        let runner = setup.pool();
        light.spread_over(&runner);
        Ok(Self {
            shown: None,
            date: if setup.switch("date")? {
                DateShown::Long
            } else {
                DateShown::Hidden
            },
            fonts,
            figure: fonts[0].cell_width(),
            baselines,
            block: None,
            at: Point::ORIGIN,
            light,
            runner,
            motion: SceneClock::new(start.now_ns, start.reduced_motion, start.refresh),
            refresh: start.refresh,
            screen,
        })
    }
}

impl Scene for Ribbon {
    /// The ribbon's next frame, the reading changing, or both; the first
    /// frame paints the whole screen.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        let now_ns = frame.now_ns;
        let first = self.shown.is_none();
        let told = Told::of(frame.wall, self.date);
        let mut retold = None;
        if self.shown != Some(told) {
            let was = footprint(self.block.as_ref(), self.at);
            (self.block, self.at) = compose(
                (told.time(), told.date()),
                self.fonts,
                self.figure,
                self.baselines,
                self.screen,
            );
            self.shown = Some(told);
            let text = was.union(&footprint(self.block.as_ref(), self.at));
            if !text.is_empty() {
                damage.add(text);
                retold = Some(text);
            }
        }
        let mut moved = false;
        if retold.is_some() || self.motion.frame_due(now_ns) {
            let t = self.motion.advance(now_ns, self.refresh);
            moved = self
                .light
                .step(t, footprint(self.block.as_ref(), self.at), damage);
        }
        let (block, at) = (self.block.as_ref(), self.at);
        if first {
            let whole = Rect::new(0, 0, self.screen.0, self.screen.1);
            damage.add(whole);
            self.light.paint(surface, whole, &self.runner);
            letter(surface, block, at, whole);
        } else {
            if moved {
                self.light.paint_moved(surface, &self.runner);
                for strip in self.light.strips() {
                    letter(surface, block, at, *strip);
                }
            }
            if let Some(text) = retold {
                self.light.paint(surface, text, &self.runner);
                letter(surface, block, at, text);
            }
        }
        let reading = frame.reading_changes_ns(SceneClock::frame_on(self.refresh));
        NextFrame::At(self.motion.due_ns().map_or(reading, |due| due.min(reading)))
    }
}

/// The `time` and `date` lines in `fonts`, every figure `figure` wide, composed into
/// one transparent block on its `baselines`, and where the block sits: each
/// line centred on a `screen` on its own, so the time stands in the same
/// place whatever the date beneath it. No block, at the origin, when there is
/// nothing to show or no surface to hold it.
fn compose(
    (time, date): (&str, &str),
    [time_font, date_font]: [BitmapFont; 2],
    figure: u32,
    baselines: [i32; 2],
    screen: (u32, u32),
) -> (Option<Surface>, Point) {
    let lines = [
        (!time.is_empty()).then(|| {
            let width = tabular_width(time_font, figure, time);
            line_box(screen.0, width, baselines[0], time_font)
        }),
        (!date.is_empty()).then(|| {
            line_box(
                screen.0,
                date_font.text_width(date),
                baselines[1],
                date_font,
            )
        }),
    ];
    let bounds = lines
        .iter()
        .flatten()
        .fold(Rect::EMPTY, |bounds, line| bounds.union(line));
    if bounds.is_empty() {
        return (None, Point::ORIGIN);
    }
    let Some(mut block) = Surface::new(bounds.width, bounds.height) else {
        return (None, Point::ORIGIN);
    };
    let within = |line: &Rect| (line.left() - bounds.left(), line.top() - bounds.top());
    if let Some(line) = &lines[0] {
        set_tabular(&mut block, (time_font, figure), within(line), time);
    }
    if let Some(line) = &lines[1] {
        let (x, y) = within(line);
        let _ = date_font.draw_text(&mut block, x, y, date, DATE_INK);
    }
    (Some(block), Point::new(bounds.left(), bounds.top()))
}

/// The time's and the date's type for a `screen` at `scale`, and their
/// baselines: the time a share of the screen's height, narrowed to fit its
/// width, and the date in proportion beneath it.
fn typeset(
    theme: &Theme,
    scale: Scale,
    (width, height): (u32, u32),
) -> ([BitmapFont; 2], [i32; 2]) {
    let family = BitmapFont::for_role(theme.fonts(), TextRole::Display, scale).family();
    let set = |px: u32, weight| BitmapFont::new(family, px.max(1)).with_weight(weight);
    let room = share(width, TIME_WIDEST);
    let widest = |font: BitmapFont| tabular_width(font, font.cell_width(), LONGEST_TIME);
    let mut time = set(share(height, TIME_BOX), TIME_WEIGHT);
    let mut px = time.pixel_height();
    let mut wide = widest(time);
    // Hinted advances do not scale exactly with the size, so the size is
    // narrowed in proportion until the time fits.
    while wide > room && px > 1 {
        px = scaled(px, room, wide).min(px - 1);
        time = set(px, TIME_WEIGHT);
        wide = widest(time);
    }
    let date = set(share(time.pixel_height(), DATE_BOX), DATE_WEIGHT);
    let baseline = i32::try_from(share(height, TIME_BASELINE)).unwrap_or(0);
    let drop = i32::try_from(share(time.pixel_height(), DATE_DROP)).unwrap_or(0);
    ([time, date], [baseline, baseline.saturating_add(drop)])
}

/// The box a line `width` pixels wide and set in `font` occupies, centred
/// across a screen `screen` pixels wide on its `baseline`.
fn line_box(screen: u32, width: u32, baseline: i32, font: BitmapFont) -> Rect {
    let left = i32::try_from(screen.saturating_sub(width) / 2).unwrap_or(0);
    Rect::new(
        left,
        baseline - line_top(font),
        width.max(1),
        font.line_height(),
    )
}

/// Whether `ch` is set in a figure's cell: a figure, or the dash an unset
/// clock shows in one's place.
fn tabular(ch: char) -> bool {
    ch.is_ascii_digit() || ch == '-'
}

/// How wide `time` is set in `font` with every figure in a cell `figure`
/// wide.
fn tabular_width(font: BitmapFont, figure: u32, time: &str) -> u32 {
    time.chars().fold(0, |width, ch| {
        width.saturating_add(if tabular(ch) {
            figure
        } else {
            font.advance(ch)
        })
    })
}

/// Set `time` in `font` on `block` from `at`, every figure centred in a cell
/// `figure` wide, so neither the time's width nor any figure's place changes
/// from one minute to the next.
fn set_tabular(block: &mut Surface, (font, figure): (BitmapFont, u32), at: (i32, i32), time: &str) {
    let mut pen = at.0;
    for ch in time.chars() {
        let advance = font.advance(ch);
        let cell = if tabular(ch) { figure } else { advance };
        let inset = i32::try_from(cell.saturating_sub(advance) / 2).unwrap_or(0);
        let mut glyph = [0; 4];
        let _ = font.draw_text(
            block,
            pen + inset,
            at.1,
            ch.encode_utf8(&mut glyph),
            TIME_INK,
        );
        pen = pen.saturating_add(i32::try_from(cell).unwrap_or(0));
    }
}

/// Blit the lines `block` at `at` onto `surface`, within `area` alone.
fn letter(surface: &mut Surface, block: Option<&Surface>, at: Point, area: Rect) {
    let Some(block) = block else {
        return;
    };
    let reach = area.intersection(&Rect::new(at.x, at.y, block.width(), block.height()));
    let Some((x, y)) = reach.surface_origin() else {
        return;
    };
    surface.with_clip(x, y, reach.width, reach.height, |surface| {
        surface.blit(at.x, at.y, block);
    });
}

/// How far a font's line box reaches above its baseline.
fn line_top(font: BitmapFont) -> i32 {
    i32::try_from(font.baseline()).unwrap_or(0)
}

/// `extent` scaled by `thousandths / 1000`, rounded down.
fn share(extent: u32, thousandths: u32) -> u32 {
    u32::try_from(u64::from(extent) * u64::from(thousandths) / 1000).unwrap_or(u32::MAX)
}

/// `value` scaled by `numerator / denominator`, which is below one.
fn scaled(value: u32, numerator: u32, denominator: u32) -> u32 {
    u32::try_from(u64::from(value) * u64::from(numerator) / u64::from(denominator.max(1)))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests;
