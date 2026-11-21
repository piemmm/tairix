//! Graphics (`plans/RISCOS-EMULATOR.md` §8): the graphics window, origin
//! and cursors, `OS_Plot`'s every code drawn through the colour blocks
//! zero page holds, and VDU 5's text at the graphics cursor.
//!
//! Coordinates arrive in OS units from the origin and become pixels from
//! the screen's bottom left by the mode's eigen factors; every shape is
//! drawn in pixels, clipped to the window a row at a time.

use alloc::vec::Vec;

use tairix_collections::BitVec;
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::output::workspace;
use tairix_util::fallible::try_push;
use tairix_vdu::plot::{
    self as codes, ARC, BLOCK, CIRCLE, CIRCLE_FILL, DOTTED_WITHOUT_ENDS, ELLIPSE_FILL,
    FILL_RIGHT_TO_BACKGROUND, FILL_RIGHT_TO_NON_FOREGROUND, FILL_TO_FOREGROUND,
    FILL_TO_NON_BACKGROUND, FLOOD, FLOOD_TO_FOREGROUND, LINE, PARALLELOGRAM, POINT, RECTANGLE,
    SECTOR, SEGMENT, TRIANGLE,
};

use crate::arc::{self, Circle};
use crate::canvas::{Block, Canvas};
use crate::changed::{cover, Rect};
use crate::colour::Colours;
use crate::raster::{self, Full, Point, Round, Span, View, Walk};
use crate::state::Vdu;
use crate::stream::Effect;
use crate::text::flags;

/// The graphics the drivers keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Graphics {
    /// The window, in pixels.
    pub(crate) window: Rect,
    /// The origin, in OS units from the screen's bottom left.
    pub(crate) origin: (i32, i32),
    /// The cursor in OS units from the origin, and in pixels.
    pub(crate) cursor: (i32, i32),
    pub(crate) cursor_pixels: Point,
    /// The two points visited before it, in pixels.
    pub(crate) old: Point,
    pub(crate) older: Point,
    /// The last point a code named, in pixels.
    pub(crate) new_point: Point,
    /// VDU 5's characters: their size and spacing in pixels.
    pub(crate) char_size: (u32, u32),
    pub(crate) char_space: (u32, u32),
}

impl Graphics {
    /// A mode's: the whole screen, the origin and cursors at its corner.
    pub(crate) const fn for_screen(width: u32, height: u32) -> Self {
        Self {
            window: Rect {
                left: 0,
                bottom: 0,
                right: width.cast_signed() - 1,
                top: height.cast_signed() - 1,
            },
            origin: (0, 0),
            cursor: (0, 0),
            cursor_pixels: (0, 0),
            old: (0, 0),
            older: (0, 0),
            new_point: (0, 0),
            char_size: (8, 8),
            char_space: (8, 8),
        }
    }
}

/// Which colour a code plots in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ink {
    Foreground,
    Inverse,
    Background,
}

impl Ink {
    /// The ink a plot code's low two bits name, as `HLineAddr`'s R3 names
    /// it: `None` for nothing plotted.
    pub(crate) const fn of(bits: u32) -> Option<Self> {
        match bits.to_le_bytes()[0] & codes::INK {
            codes::FOREGROUND => Some(Self::Foreground),
            codes::INVERSE => Some(Self::Inverse),
            codes::BACKGROUND => Some(Self::Background),
            _ => None,
        }
    }
}

/// The furthest a point's pixels may lie from the screen's corner: far past
/// any screen, near enough that sums and differences of a shape's points stay
/// within a word.
const REACH: i32 = 1 << 28;

impl Vdu {
    /// A point in OS units from the origin, in pixels, held within
    /// [`REACH`]. The sums wrap as the ARM's do.
    pub(crate) fn pixels_of(&self, x: i32, y: i32) -> Point {
        let format = &self.screen.format;
        (
            (x.wrapping_add(self.graphics.origin.0) >> format.x_eig()).clamp(-REACH, REACH),
            (y.wrapping_add(self.graphics.origin.1) >> format.y_eig()).clamp(-REACH, REACH),
        )
    }

    /// A point in pixels, in OS units from the origin, wrapping as the ARM's
    /// sums do.
    pub(crate) fn units_of(&self, p: Point) -> (i32, i32) {
        let format = &self.screen.format;
        (
            (p.0 << format.x_eig()).wrapping_sub(self.graphics.origin.0),
            (p.1 << format.y_eig()).wrapping_sub(self.graphics.origin.1),
        )
    }

    /// `VDU 24`: the graphics window, from the origin, inclusive; a window
    /// any part of which is off the screen, or inside out, is ignored.
    pub(crate) fn set_graphics_window(&mut self, left: i32, bottom: i32, right: i32, top: i32) {
        let (left, bottom) = self.pixels_of(left, bottom);
        let (right, top) = self.pixels_of(right, top);
        let format = &self.screen.format;
        let on_screen = left >= 0
            && bottom >= 0
            && right < format.width().cast_signed()
            && top < format.height().cast_signed();
        if on_screen && left <= right && bottom <= top {
            self.graphics.window = Rect {
                left,
                bottom,
                right,
                top,
            };
        }
    }

    /// The point a code at `(x, y)` names, absolute or from the cursor.
    fn point_named(&self, plot: u8, x: i16, y: i16) -> (i32, i32) {
        if plot & codes::ABSOLUTE == 0 {
            (
                self.graphics.cursor.0.wrapping_add(i32::from(x)),
                self.graphics.cursor.1.wrapping_add(i32::from(y)),
            )
        } else {
            (i32::from(x), i32::from(y))
        }
    }

    /// Moves the cursor to `point`, in OS units, the two before it kept.
    fn visit(&mut self, point: (i32, i32)) {
        let pixels = self.pixels_of(point.0, point.1);
        self.graphics.older = self.graphics.old;
        self.graphics.old = self.graphics.cursor_pixels;
        self.graphics.cursor = point;
        self.graphics.cursor_pixels = pixels;
    }

    /// `VDU 25`: the code at `(x, y)`; a code the drivers do not draw goes to
    /// `UKPlotV`.
    pub(crate) fn plot(
        &mut self,
        os: &mut Os<'_>,
        code: u8,
        x: i16,
        y: i16,
    ) -> Result<Option<Effect>, Fail> {
        let point = self.point_named(code, x, y);
        self.graphics.new_point = self.pixels_of(point.0, point.1);
        let shape = code & codes::SHAPE;
        if shape > ELLIPSE_FILL {
            self.graphics.cursor = point;
            return Ok(Some(Effect::UnknownPlot {
                code,
                x: point.0,
                y: point.1,
            }));
        }
        self.visit(point);
        let ink = Ink::of(u32::from(code));
        if shape == BLOCK {
            return self.block_copy(os, code & !codes::SHAPE).map(|()| None);
        }
        let Some(ink) = ink else {
            return Ok(None);
        };
        if !self.screen.format.graphic() {
            return Ok(None);
        }
        let block = ink_block(os, ink)?;
        let (older, old, new) = (
            self.graphics.older,
            self.graphics.old,
            self.graphics.cursor_pixels,
        );
        let window = self.graphics.window;
        let rows = self.view((0, 0));
        match shape {
            LINE..=DOTTED_WITHOUT_ENDS => self.plot_line(os, shape >> 3, old, new, &block)?,
            POINT => self.draw_spans(os, [(new.1, new.0, new.0)], &block)?,
            FILL_TO_NON_BACKGROUND
            | FILL_RIGHT_TO_BACKGROUND
            | FILL_TO_FOREGROUND
            | FILL_RIGHT_TO_NON_FOREGROUND => self.line_fill(os, shape, &block)?,
            TRIANGLE => {
                let filled =
                    raster::polygon(&[older, old, new], rows).map_err(|Full| os.no_memory())?;
                self.draw_spans(os, filled.spans(), &block)?;
            }
            RECTANGLE => self.draw_spans(os, rectangle(old, new, window), &block)?,
            PARALLELOGRAM => {
                let fourth = (older.0 + new.0 - old.0, older.1 + new.1 - old.1);
                let filled = raster::polygon(&[older, old, new, fourth], rows)
                    .map_err(|Full| os.no_memory())?;
                self.draw_spans(os, filled.spans(), &block)?;
            }
            FLOOD | FLOOD_TO_FOREGROUND => self.flood(os, shape == FLOOD_TO_FOREGROUND, &block)?,
            CIRCLE | CIRCLE_FILL => {
                let circle = self.circle(old, new);
                self.draw_round(os, &circle.round, old, shape == CIRCLE_FILL, &block)?;
            }
            ARC..=SECTOR => self.plot_arc(os, shape, older, old, new, &block)?,
            _ => {
                let round = Round::ellipse(old.0 - older.0, (new.0 - older.0, new.1 - older.1));
                self.draw_round(os, &round, older, shape == ELLIPSE_FILL, &block)?;
            }
        }
        Ok(None)
    }

    /// The circle about `centre` through `edge`, round in OS units whatever
    /// the pixels' shape.
    fn circle(&self, centre: Point, edge: Point) -> Circle {
        let format = &self.screen.format;
        let (xe, ye) = (format.x_eig(), format.y_eig());
        let (tall, wide) = if ye >= xe {
            (1 << (ye - xe), 1)
        } else {
            (1, 1 << (xe - ye))
        };
        Circle::through((edge.0 - centre.0, edge.1 - centre.1), tall, wide)
    }

    /// The window about `centre`.
    fn view(&self, centre: Point) -> View {
        let window = self.graphics.window;
        View {
            left: window.left.saturating_sub(centre.0),
            bottom: window.bottom.saturating_sub(centre.1),
            right: window.right.saturating_sub(centre.0),
            top: window.top.saturating_sub(centre.1),
        }
    }

    /// Draws `spans`, clipped to the window, with `block`.
    pub(crate) fn draw_spans(
        &mut self,
        os: &mut Os<'_>,
        spans: impl IntoIterator<Item = Span>,
        block: &Block,
    ) -> Result<(), Fail> {
        let window = self.graphics.window;
        let canvas = self.screen.canvas();
        let mut drawn: Option<Rect> = None;
        for (y, left, right) in spans {
            let (left, right) = (left.max(window.left), right.min(window.right));
            if y < window.bottom || y > window.top || left > right {
                continue;
            }
            span(os, &canvas, y, left, right, block)?;
            drawn = Some(widen(
                drawn,
                Rect {
                    left,
                    bottom: y,
                    right,
                    top: y,
                },
            ));
        }
        if let Some(rect) = drawn {
            cover(os, rect)?;
        }
        Ok(())
    }

    /// A line of form `form`, 0 to 7: its ends drawn or left, dotted or
    /// solid, the dots started afresh or carried on. Only the part within
    /// the window is walked; the dots go on across the rest.
    fn plot_line(
        &mut self,
        os: &mut Os<'_>,
        form: u8,
        from: Point,
        to: Point,
        block: &Block,
    ) -> Result<(), Fail> {
        let walk = Walk::new(from, to);
        let skip_first = matches!(form, 4..=7);
        let skip_last = matches!(form, 1 | 3 | 5 | 7);
        let dotted = matches!(form, 2 | 3 | 6 | 7);
        if dotted && matches!(form, 2 | 3) {
            self.colours.dot_at = 0;
        }
        let last = walk.len() - 1;
        let skipped = |place: u64| (place == 0 && skip_first) || (place == last && skip_last);
        let (dot, dot_at, length) = (
            self.colours.dot,
            u64::from(self.colours.dot_at),
            u64::from(self.colours.dot_length.max(1)),
        );
        let shown = |place: u64| {
            if skipped(place) {
                return false;
            }
            let before = place - u64::from(skip_first && place > 0);
            !dotted || dot >> (63 - (dot_at + before) % length) & 1 != 0
        };
        let pixels = walk
            .within(self.graphics.window)
            .filter(|&(place, _)| shown(place))
            .map(|(_, (x, y))| (y, x, x));
        self.draw_spans(os, pixels, block)?;
        if dotted {
            let dots = walk.len() - u64::from(skipped(0)) - u64::from(last > 0 && skipped(last));
            self.colours.dot_at = u32::try_from((dot_at + dots) % length).unwrap_or(0);
        }
        Ok(())
    }

    /// Draws a circle or ellipse about `centre`, filled or its outline,
    /// worked out only across the window.
    fn draw_round(
        &mut self,
        os: &mut Os<'_>,
        round: &Round,
        centre: Point,
        filled: bool,
        block: &Block,
    ) -> Result<(), Fail> {
        let view = self.view(centre);
        if filled {
            self.draw_spans(os, round.fill(view).map(placed(view, centre)), block)
        } else {
            self.draw_spans(os, round.outline(view).map(placed(view, centre)), block)
        }
    }

    /// An arc, segment or sector about `centre` from `start` round to the
    /// direction of `end`, worked out only across the window.
    fn plot_arc(
        &mut self,
        os: &mut Os<'_>,
        shape: u8,
        centre: Point,
        start: Point,
        end: Point,
        block: &Block,
    ) -> Result<(), Fail> {
        let circle = self.circle(centre, start);
        let (from, toward) = (
            (start.0 - centre.0, start.1 - centre.1),
            (end.0 - centre.0, end.1 - centre.1),
        );
        let view = self.view(centre);
        let spans = match shape {
            ARC => {
                let mut runs = Vec::new();
                arc::arc(&circle, from, toward, view, &mut runs).map(|()| runs)
            }
            SEGMENT => arc::segment(&circle, from, toward, view),
            _ => arc::sector(&circle, from, toward, view),
        }
        .map_err(|Full| os.no_memory())?;
        self.draw_spans(os, spans.into_iter().map(placed(view, centre)), block)
    }

    /// The horizontal line fills: from the point, across pixels of the
    /// background or not the foreground, both ways or rightward alone, as
    /// the code names; the cursors left at the span's ends.
    fn line_fill(&mut self, os: &mut Os<'_>, shape: u8, block: &Block) -> Result<(), Fail> {
        let (x, y) = self.graphics.cursor_pixels;
        let window = self.graphics.window;
        if !window.holds(x, y) {
            return Ok(());
        }
        let format = self.screen.format;
        let mask = format.pixel_mask();
        let back = Colours::gcol_pixel(&format, &self.colours.back) & mask;
        let fore = Colours::gcol_pixel(&format, &self.colours.fore) & mask;
        let fills = |p: u32| match shape {
            FILL_TO_NON_BACKGROUND => p & mask == back,
            FILL_RIGHT_TO_BACKGROUND => p & mask != back,
            FILL_TO_FOREGROUND => p & mask != fore,
            _ => p & mask == fore,
        };
        let canvas = self.screen.canvas();
        let mut line = Line::new(canvas, window);
        line.start(y);
        if !fills(line.at(os, x)?) {
            return Ok(());
        }
        let both_ways = matches!(shape, FILL_TO_NON_BACKGROUND | FILL_TO_FOREGROUND);
        let (left, right) = line.run(os, x, both_ways, |_, p| fills(p))?;
        span(os, &canvas, y, left, right, block)?;
        cover(
            os,
            Rect {
                left,
                bottom: y,
                right,
                top: y,
            },
        )?;
        self.graphics.old = (left, y);
        self.graphics.cursor_pixels = (right, y);
        self.graphics.cursor = self.units_of((right, y));
        Ok(())
    }

    /// The flood fills: every pixel joined to the point across pixels of
    /// the background, or across pixels not the foreground. A pixel once
    /// filled is never read again, so every decision reads the screen as it
    /// was before the fill.
    fn flood(&mut self, os: &mut Os<'_>, to_foreground: bool, block: &Block) -> Result<(), Fail> {
        let (x, y) = self.graphics.cursor_pixels;
        let window = self.graphics.window;
        if !window.holds(x, y) {
            return Ok(());
        }
        let format = self.screen.format;
        let mask = format.pixel_mask();
        let colour = if to_foreground {
            &self.colours.fore
        } else {
            &self.colours.back
        };
        let target = Colours::gcol_pixel(&format, colour) & mask;
        let fills = |p: u32| (p & mask == target) != to_foreground;
        let width = (window.right - window.left + 1).cast_unsigned() as usize;
        let height = (window.top - window.bottom + 1).cast_unsigned() as usize;
        let index = |(px, py): Point| {
            (py - window.bottom).cast_unsigned() as usize * width
                + (px - window.left).cast_unsigned() as usize
        };
        let mut filled = BitVec::try_with_len(width * height, false).map_err(|_| os.no_memory())?;
        let open = |filled: &BitVec, point: Point, p: u32| {
            fills(p) && filled.get(index(point)) == Some(false)
        };
        let canvas = self.screen.canvas();
        let mut seeds: Vec<Point> = Vec::new();
        try_push(&mut seeds, (x, y)).map_err(|_| os.no_memory())?;
        let mut line = Line::new(canvas, window);
        let mut beside = Vec::new();
        let mut drawn: Option<Rect> = None;
        let mut spans = 0u32;
        while let Some((sx, sy)) = seeds.pop() {
            if filled.get(index((sx, sy))) != Some(false) {
                continue;
            }
            line.start(sy);
            if !fills(line.at(os, sx)?) {
                continue;
            }
            let (left, right) = line.run(os, sx, true, |px, p| open(&filled, (px, sy), p))?;
            filled.set_range(index((left, sy))..index((right, sy)) + 1, true);
            span(os, &canvas, sy, left, right, block)?;
            drawn = Some(widen(
                drawn,
                Rect {
                    left,
                    bottom: sy,
                    right,
                    top: sy,
                },
            ));
            for ny in [sy + 1, sy - 1] {
                if ny < window.bottom || ny > window.top {
                    continue;
                }
                beside.clear();
                canvas.pixels(
                    os,
                    canvas.row_of(ny),
                    left.cast_unsigned(),
                    right.cast_unsigned(),
                    &mut beside,
                )?;
                let mut inside = false;
                for (px, &p) in (left..=right).zip(&beside) {
                    let here = open(&filled, (px, ny), p);
                    if here && !inside {
                        try_push(&mut seeds, (px, ny)).map_err(|_| os.no_memory())?;
                    }
                    inside = here;
                }
            }
            spans += 1;
            if spans.is_multiple_of(256) {
                os.yield_point()?;
            }
        }
        if let Some(rect) = drawn {
            cover(os, rect)?;
        }
        Ok(())
    }

    /// The block codes: `form` 0 or 4 moves the cursor alone; 1 and 5 move
    /// the rectangle the last two points bound so its bottom left is at the
    /// cursor, the background left behind; 2, 3, 6 and 7 copy it there. The
    /// source is what lies on the screen, the destination what lies in the
    /// window.
    fn block_copy(&mut self, os: &mut Os<'_>, form: u8) -> Result<(), Fail> {
        if form & codes::INK == 0 || !self.screen.format.graphic() {
            return Ok(());
        }
        let (a, b, to) = (
            self.graphics.older,
            self.graphics.old,
            self.graphics.cursor_pixels,
        );
        let corner = (a.0.min(b.0), a.1.min(b.1));
        let format = self.screen.format;
        let source = Rect {
            left: corner.0.max(0),
            bottom: corner.1.max(0),
            right: a.0.max(b.0).min(format.width().cast_signed() - 1),
            top: a.1.max(b.1).min(format.height().cast_signed() - 1),
        };
        if source.left > source.right || source.bottom > source.top {
            return Ok(());
        }
        let (dx, dy) = (to.0 - corner.0, to.1 - corner.1);
        let canvas = self.screen.canvas();
        let across = (source.right - source.left + 1).cast_unsigned() as usize;
        let lines = (source.top - source.bottom + 1).cast_unsigned() as usize;
        let mut pixels = Vec::new();
        if pixels.try_reserve_exact(across * lines).is_err() {
            return Err(os.no_memory());
        }
        for y in source.bottom..=source.top {
            canvas.pixels(
                os,
                canvas.row_of(y),
                source.left.cast_unsigned(),
                source.right.cast_unsigned(),
                &mut pixels,
            )?;
        }
        let window = self.graphics.window;
        if form & codes::INK == codes::BLOCK_MOVE {
            let clear = Block::read(os, workspace::BACKGROUND_STORE)?;
            self.draw_spans(
                os,
                rectangle(
                    (source.left, source.bottom),
                    (source.right, source.top),
                    window,
                ),
                &clear,
            )?;
            cover(os, source)?;
        }
        let first = source.left + dx;
        let (left, right) = (
            first.max(window.left),
            (source.right + dx).min(window.right),
        );
        let (bottom, top) = (
            (source.bottom + dy).max(window.bottom),
            (source.top + dy).min(window.top),
        );
        if left > right || bottom > top {
            return Ok(());
        }
        let skip = (left - first).cast_unsigned() as usize;
        let take = (right - left + 1).cast_unsigned() as usize;
        for y in bottom..=top {
            let from = (y - dy - source.bottom).cast_unsigned() as usize * across + skip;
            canvas.put_pixels(
                os,
                canvas.row_of(y),
                left.cast_unsigned(),
                &pixels[from..from + take],
            )?;
        }
        cover(
            os,
            Rect {
                left,
                bottom,
                right,
                top,
            },
        )
    }

    /// `VDU 16`: the graphics window cleared in the background colour and
    /// action.
    pub(crate) fn clear_graphics(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if !self.screen.format.graphic() {
            return Ok(());
        }
        let block = Block::read(os, workspace::BACKGROUND)?;
        let w = self.graphics.window;
        self.draw_spans(
            os,
            rectangle((w.left, w.bottom), (w.right, w.top), w),
            &block,
        )
    }

    /// VDU 5: `c` at the graphics cursor, its top left there, in the
    /// foreground colour and action, its background left as it was; then
    /// the cursor on a character.
    pub(crate) fn vdu5_print(&mut self, os: &mut Os<'_>, c: u8) -> Result<(), Fail> {
        if !self.screen.format.graphic() {
            return Ok(());
        }
        let glyph = self.font.glyph(c).copied().unwrap_or([0; 8]);
        let (size_x, size_y) = self.graphics.char_size;
        let (left, top) = self.graphics.cursor_pixels;
        let window = self.graphics.window;
        let (lowest, highest) = ((top - window.top).max(0), top - window.bottom);
        let spans = (0u32..).zip(glyph).flat_map(move |(row, bits)| {
            let runs = lit_runs(bits, size_x);
            (eighth(row, size_y).max(lowest)..eighth(row + 1, size_y).min(highest + 1)).flat_map(
                move |down| {
                    runs.into_iter()
                        .map(move |(from, to)| (top - down, left + from, left + to))
                },
            )
        });
        let block = Block::read(os, workspace::FOREGROUND)?;
        self.draw_spans(os, spans, &block)?;
        if self.text.flags & flags::NO_ADVANCE == 0 {
            self.vdu5_step(1);
        }
        Ok(())
    }

    /// VDU 127 in VDU 5: the character cell at the cursor blanked in the
    /// background colour.
    pub(crate) fn vdu5_blank(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let (size_x, size_y) = self.graphics.char_size;
        let (left, top) = self.graphics.cursor_pixels;
        let block = Block::read(os, workspace::BACKGROUND_STORE)?;
        let rect = rectangle(
            (left, top - size_y.cast_signed() + 1),
            (left + size_x.cast_signed() - 1, top),
            self.graphics.window,
        );
        self.draw_spans(os, rect, &block)
    }

    /// The graphics cursor moved `n` characters along, wrapping at the
    /// window's edges unless told not to.
    pub(crate) fn vdu5_step(&mut self, n: i32) {
        let (dx, dy) = self.vdu5_char_step();
        let space = self.graphics.char_space;
        let (x, y) = self.graphics.cursor_pixels;
        let moved = (
            x + n * dx * space.0.cast_signed(),
            y + n * dy * space.1.cast_signed(),
        );
        self.vdu5_place(moved, n > 0);
    }

    /// The graphics cursor moved `n` lines along.
    pub(crate) fn vdu5_line(&mut self, n: i32) {
        let (dx, dy) = self.vdu5_line_step();
        let space = self.graphics.char_space;
        let (x, y) = self.graphics.cursor_pixels;
        let moved = (
            x + n * dx * space.0.cast_signed(),
            y + n * dy * space.1.cast_signed(),
        );
        self.vdu5_place_line(moved);
    }

    /// The way a character moves the cursor, across and up, as text's axes
    /// run.
    fn vdu5_char_step(&self) -> (i32, i32) {
        let f = self.text.flags;
        let reversed = f & flags::X_REVERSED != 0;
        match (f & flags::SWAPPED != 0, reversed) {
            (false, false) => (1, 0),
            (false, true) => (-1, 0),
            (true, false) => (0, -1),
            (true, true) => (0, 1),
        }
    }

    /// The way a line moves it: down the screen, or across it where
    /// characters run down.
    fn vdu5_line_step(&self) -> (i32, i32) {
        let f = self.text.flags;
        let reversed = f & flags::Y_REVERSED != 0;
        match (f & flags::SWAPPED != 0, reversed) {
            (false, false) => (0, -1),
            (false, true) => (0, 1),
            (true, false) => (1, 0),
            (true, true) => (-1, 0),
        }
    }

    /// The cursor at `moved`, back to the window's start of the next line
    /// where it has left the window going forward.
    fn vdu5_place(&mut self, moved: Point, forward: bool) {
        let w = self.graphics.window;
        let outside = moved.0 > w.right || moved.0 < w.left;
        if forward && outside && self.text.flags & flags::NO_EDGES == 0 {
            self.set_vdu5_cursor((w.left, moved.1));
            self.vdu5_line(1);
        } else {
            self.set_vdu5_cursor(moved);
        }
    }

    /// The cursor at `moved` a line on, to the window's top left where that
    /// leaves the window.
    fn vdu5_place_line(&mut self, moved: Point) {
        let w = self.graphics.window;
        let outside = moved.1 < w.bottom || moved.1 > w.top;
        if outside && self.text.flags & flags::NO_EDGES == 0 {
            self.vdu5_home();
        } else {
            self.set_vdu5_cursor(moved);
        }
    }

    /// VDU 13 in VDU 5: to the window's left edge.
    pub(crate) fn vdu5_return(&mut self) {
        let left = self.graphics.window.left;
        let y = self.graphics.cursor_pixels.1;
        self.set_vdu5_cursor((left, y));
    }

    /// VDU 30 in VDU 5: to the window's top left.
    pub(crate) fn vdu5_home(&mut self) {
        let w = self.graphics.window;
        self.set_vdu5_cursor((w.left, w.top));
    }

    /// VDU 31 in VDU 5: `x` characters and `y` lines from the top left.
    pub(crate) fn vdu5_tab(&mut self, x: u32, y: u32) {
        let w = self.graphics.window;
        let space = self.graphics.char_space;
        let at = (
            w.left + (x * space.0).cast_signed(),
            w.top - (y * space.1).cast_signed(),
        );
        self.set_vdu5_cursor(at);
    }

    /// The graphics cursor to `p`, in pixels, held within [`REACH`].
    fn set_vdu5_cursor(&mut self, p: Point) {
        let p = (p.0.clamp(-REACH, REACH), p.1.clamp(-REACH, REACH));
        self.graphics.cursor_pixels = p;
        self.graphics.cursor = self.units_of(p);
    }
}

/// The first of `size` pixels in the `n`th eighth of them.
const fn eighth(n: u32, size: u32) -> i32 {
    (n * size).div_ceil(8).cast_signed()
}

/// The columns, from a character's left, of a glyph row's runs of lit bits,
/// `size` pixels across, bit 7 leftmost: each bit is an eighth of the
/// character, and a run with no column empty.
fn lit_runs(bits: u8, size: u32) -> [(i32, i32); 4] {
    let mut runs = [(1, 0); 4];
    let mut found = 0;
    let mut bit = 0;
    while bit < 8 {
        if bits & (0x80 >> bit) == 0 {
            bit += 1;
            continue;
        }
        let first = bit;
        while bit < 8 && bits & (0x80 >> bit) != 0 {
            bit += 1;
        }
        if let Some(run) = runs.get_mut(found) {
            *run = (eighth(first, size), eighth(bit, size) - 1);
            found += 1;
        }
    }
    runs
}

/// The pixels a fill reads of a line at a time.
const STRETCH: i32 = 64;

/// A line of the window as a fill walks it: read a stretch at a time in the
/// way the walk goes, so a walk costs what it covers, not the window's
/// width.
struct Line {
    canvas: Canvas,
    window: Rect,
    row: u32,
    /// The pixels held, from `first` to `last`; none where `last` is short
    /// of `first`.
    first: i32,
    last: i32,
    pixels: Vec<u32>,
}

impl Line {
    const fn new(canvas: Canvas, window: Rect) -> Self {
        Self {
            canvas,
            window,
            row: 0,
            first: 1,
            last: 0,
            pixels: Vec::new(),
        }
    }

    /// Moves to the line `y` pixels up, holding none of it.
    fn start(&mut self, y: i32) {
        self.row = self.canvas.row_of(y);
        (self.first, self.last) = (1, 0);
    }

    /// Pixel `x`, which is within the window.
    fn at(&mut self, os: &mut Os<'_>, x: i32) -> Result<u32, Fail> {
        if x < self.first || x > self.last {
            let (from, to) = if self.last < self.first {
                (x - STRETCH / 2, x + STRETCH / 2)
            } else if x < self.first {
                (x - STRETCH + 1, x)
            } else {
                (x, x + STRETCH - 1)
            };
            let (from, to) = (from.max(self.window.left), to.min(self.window.right));
            self.pixels.clear();
            self.canvas.pixels(
                os,
                self.row,
                from.cast_unsigned(),
                to.cast_unsigned(),
                &mut self.pixels,
            )?;
            (self.first, self.last) = (from, to);
        }
        Ok(self.pixels[(x - self.first).cast_unsigned() as usize])
    }

    /// The run through `x` of the pixels `open` admits, to the window's
    /// edges: rightward alone unless `both` ways.
    fn run(
        &mut self,
        os: &mut Os<'_>,
        x: i32,
        both: bool,
        mut open: impl FnMut(i32, u32) -> bool,
    ) -> Result<(i32, i32), Fail> {
        let mut left = x;
        while both && left > self.window.left && open(left - 1, self.at(os, left - 1)?) {
            left -= 1;
        }
        let mut right = x;
        while right < self.window.right && open(right + 1, self.at(os, right + 1)?) {
            right += 1;
        }
        Ok((left, right))
    }
}

/// A span about `centre`, cut to `view`, as one on the screen.
fn placed(view: View, centre: Point) -> impl Fn(Span) -> Span {
    move |(k, left, right)| {
        let (left, right) = view.cut((left, right));
        (centre.1 + k, centre.0 + left, centre.0 + right)
    }
}

/// The rows of the rectangle with corners `a` and `b` within `window`.
fn rectangle(a: Point, b: Point, window: Rect) -> impl Iterator<Item = Span> {
    let (left, right) = (
        a.0.min(b.0).max(window.left),
        a.0.max(b.0).min(window.right),
    );
    (a.1.min(b.1).max(window.bottom)..=a.1.max(b.1).min(window.top)).map(move |y| (y, left, right))
}

/// A box widened over another.
fn widen(drawn: Option<Rect>, r: Rect) -> Rect {
    match drawn {
        None => r,
        Some(d) => Rect {
            left: d.left.min(r.left),
            bottom: d.bottom.min(r.bottom),
            right: d.right.max(r.right),
            top: d.top.max(r.top),
        },
    }
}

/// The colour block an ink plots with.
pub(crate) fn ink_block(os: &mut Os<'_>, ink: Ink) -> Result<Block, Fail> {
    match ink {
        Ink::Foreground => Block::read(os, workspace::FOREGROUND),
        Ink::Background => Block::read(os, workspace::BACKGROUND),
        Ink::Inverse => Ok(Block::solid(0, u32::MAX)),
    }
}

/// Writes pixels `left` to `right` of the row `y` up with `block`.
pub(crate) fn span(
    os: &mut Os<'_>,
    canvas: &Canvas,
    y: i32,
    left: i32,
    right: i32,
    block: &Block,
) -> Result<(), Fail> {
    let row = canvas.row_of(y);
    let (ora, eor) = block.for_line(row);
    canvas.span(
        os,
        row,
        left.cast_unsigned(),
        right.cast_unsigned(),
        ora,
        eor,
    )
}

#[cfg(test)]
mod tests;
