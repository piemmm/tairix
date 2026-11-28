//! A context's screen as its window shows it (`plans/RISCOS-EMULATOR.md`
//! §15.3): the guest's lines as the worker last sent them, the text cursor
//! and the pointer drawn over them and never into them, and what has
//! changed since the picture was last painted.

use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageList, DamageRect, MAX_DAMAGE_RECTS};
use tairix_geometry::{Rect, Region};
use tairix_raster::resample::Region as Placement;
use tairix_raster::{Pixel, Surface};
use tairix_riscos_session::protocol::{Cursor, Flash, Screen, Shape};

/// Opaque black: a blanked screen.
const BLACK: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: u8::MAX,
};

/// The pointer as last told.
struct Pointer {
    width: u32,
    height: u32,
    active: (u32, u32),
    colours: [Pixel; 3],
    /// Each pixel a colour 0 to 3, nought clear.
    pixels: Vec<u8>,
}

/// What a paint draws over the guest's lines: the cursor's rectangle, and
/// the pointer with the rectangle it covers and its top left.
struct Over<'a> {
    cursor: Option<Rect>,
    pointer: Option<(&'a Pointer, Rect, (i64, i64))>,
}

/// One context's screen.
pub struct Picture {
    screen: Screen,
    /// The guest's pixels, row by row.
    lines: Vec<Pixel>,
    cursor: Option<Cursor>,
    /// A flashing cursor's phase.
    lit: bool,
    pointer: Option<Pointer>,
    /// Where the pointer's active point is, in screen pixels.
    at: Option<(i32, i32)>,
    /// What changed since the picture was last painted.
    changed: Region,
}

/// `&RRGGBB` as an opaque pixel.
const fn opaque(colour: u32) -> Pixel {
    let [_, r, g, b] = colour.to_be_bytes();
    Pixel {
        r,
        g,
        b,
        a: u8::MAX,
    }
}

/// Every pixel of an `width` by `height` screen, black: none where there is
/// no memory for them.
fn black(width: u32, height: u32) -> Option<Vec<Pixel>> {
    let count = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    let mut lines = Vec::new();
    lines.try_reserve_exact(count).ok()?;
    lines.resize(count, BLACK);
    Some(lines)
}

impl Picture {
    /// A black screen of `screen`'s geometry: none where there is no memory
    /// for it.
    #[must_use]
    pub fn new(screen: Screen) -> Option<Self> {
        let mut changed = Region::with_budget(MAX_DAMAGE_RECTS);
        changed.add(Rect::new(0, 0, screen.width, screen.height));
        Some(Self {
            screen,
            lines: black(screen.width, screen.height)?,
            cursor: None,
            lit: true,
            pointer: None,
            at: None,
            changed,
        })
    }

    /// The screen's geometry.
    #[must_use]
    pub const fn screen(&self) -> Screen {
        self.screen
    }

    /// The screen's geometry is now `screen`: its lines black again where
    /// its size changed, as the worker sends them all again. False where
    /// there was no memory for the new size, the old kept.
    pub fn reshape(&mut self, screen: Screen) -> bool {
        if (screen.width, screen.height) != (self.screen.width, self.screen.height) {
            let Some(lines) = black(screen.width, screen.height) else {
                return false;
            };
            self.lines = lines;
        }
        self.screen = screen;
        self.owe_whole();
        true
    }

    /// Everything to be painted again, as into a fresh picture.
    pub fn owe_whole(&mut self) {
        self.changed.clear();
        self.changed.add(self.bounds());
    }

    /// The lines from `first`, `pixels` holding each `&RRGGBB` as four
    /// little-endian bytes, as a damage record does.
    pub fn damage(&mut self, first: u32, pixels: &[u8]) {
        let width = self.screen.width as usize;
        let Some(start) = (first as usize).checked_mul(width) else {
            return;
        };
        let Some(target) = self.lines.get_mut(start..) else {
            return;
        };
        let words = pixels.as_chunks::<4>().0;
        for (pixel, word) in target.iter_mut().zip(words) {
            *pixel = opaque(u32::from_le_bytes(*word));
        }
        let lines = u32::try_from(words.len() / width.max(1)).unwrap_or(u32::MAX);
        self.change(Rect::new(0, to_i32(first), self.screen.width, lines));
    }

    /// The text cursor is now `cursor`, lit afresh.
    pub fn set_cursor(&mut self, cursor: Option<Cursor>) {
        if let Some(old) = self.cursor_rect() {
            self.change(old);
        }
        self.cursor = cursor;
        self.lit = true;
        if let Some(new) = self.cursor_rect() {
            self.change(new);
        }
    }

    /// How the cursor flashes, where one is shown.
    #[must_use]
    pub fn cursor_flash(&self) -> Option<Flash> {
        self.cursor.map(|cursor| cursor.flash)
    }

    /// A flashing cursor's phase turned over.
    pub fn flash(&mut self) {
        if self
            .cursor_flash()
            .is_some_and(|flash| flash != Flash::Steady)
        {
            self.lit = !self.lit;
            if let Some(rect) = self.cursor_cell() {
                self.change(rect);
            }
        }
    }

    /// The pointer's shape is now `shape`: false where there was no memory
    /// for it, none then shown.
    pub fn set_pointer(&mut self, shape: Option<&Shape<'_>>) -> bool {
        if let Some(old) = self.pointer_rect() {
            self.change(old);
        }
        self.pointer = None;
        let Some(shape) = shape else {
            return true;
        };
        let mut pixels = Vec::new();
        if pixels.try_reserve_exact(shape.pixels.len()).is_err() {
            return false;
        }
        pixels.extend_from_slice(shape.pixels);
        self.pointer = Some(Pointer {
            width: shape.width,
            height: shape.height,
            active: (u32::from(shape.active.0), u32::from(shape.active.1)),
            colours: shape.colours.map(opaque),
            pixels,
        });
        if let Some(new) = self.pointer_rect() {
            self.change(new);
        }
        true
    }

    /// Whether the guest shows a pointer.
    #[must_use]
    pub const fn shows_pointer(&self) -> bool {
        self.pointer.is_some()
    }

    /// The pointer's active point is now at screen pixel `(x, y)`.
    pub fn set_pointer_at(&mut self, x: i32, y: i32) {
        if let Some(old) = self.pointer_rect() {
            self.change(old);
        }
        self.at = Some((x, y));
        if let Some(new) = self.pointer_rect() {
            self.change(new);
        }
    }

    /// The rectangle of a `width` by `height` window the picture is scaled
    /// into: the largest of the mode's pixel aspect, centred.
    #[must_use]
    pub fn placement(&self, (width, height): (u32, u32)) -> Option<Placement> {
        let s = &self.screen;
        tairix_crt::screen::fit_aspect(width, height, (s.width << s.x_eig, s.height << s.y_eig))
    }

    /// What changed since last painted, drawn into `picture`, which is the
    /// screen's size: the guest's lines, the cursor and the pointer over
    /// them, and the rectangles painted. None where nothing changed, or
    /// `picture` is not the screen's size.
    pub fn paint(&mut self, picture: &mut Surface) -> Option<DamageList> {
        if self.changed.is_empty()
            || (picture.width(), picture.height()) != (self.screen.width, self.screen.height)
        {
            return None;
        }
        let mut rects = [DamageRect {
            x: 0,
            y: 0,
            width_px: 0,
            height_px: 0,
        }; MAX_DAMAGE_RECTS];
        let mut count = 0;
        let over = Over {
            cursor: self.cursor_rect(),
            pointer: self.pointer_drawn(),
        };
        for rect in self.changed.rects() {
            let (x, y) = (on_screen(rect.origin.x), on_screen(rect.origin.y));
            for row in y..y + rect.height {
                self.paint_row(picture, row, x..x + rect.width, &over);
            }
            if let Some(slot) = rects.get_mut(count) {
                *slot = DamageRect {
                    x,
                    y,
                    width_px: rect.width,
                    height_px: rect.height,
                };
                count += 1;
            }
        }
        self.changed.clear();
        DamageList::new(&rects[..count]).ok()
    }

    /// Row `row`'s pixels `columns` painted: the guest's, the cursor
    /// inverting them, the pointer over both.
    fn paint_row(
        &self,
        picture: &mut Surface,
        row: u32,
        columns: core::ops::Range<u32>,
        over: &Over<'_>,
    ) {
        let width = columns.end - columns.start;
        let Some((first, span)) = picture.row_span_mut(row, columns.start, width) else {
            return;
        };
        let from = row as usize * self.screen.width as usize + first as usize;
        let Some(guest) = self.lines.get(from..from + span.len()) else {
            return;
        };
        if self.screen.blanked {
            span.fill(BLACK);
            return;
        }
        span.copy_from_slice(guest);
        let shown = first..first + len32(span.len());
        if let Some(cursor) = over.cursor.filter(|rect| covers_row(rect, row)) {
            for column in overlap(&cursor, &shown) {
                let pixel = &mut span[(column - first) as usize];
                *pixel = Pixel {
                    r: !pixel.r,
                    g: !pixel.g,
                    b: !pixel.b,
                    a: u8::MAX,
                };
            }
        }
        let Some((pointer, rect, (left, top))) = over.pointer else {
            return;
        };
        if !covers_row(&rect, row) {
            return;
        }
        let shape_row = usize::try_from(i64::from(row) - top).unwrap_or(0);
        for column in overlap(&rect, &shown) {
            let shape_column = usize::try_from(i64::from(column) - left).unwrap_or(0);
            let value = pointer
                .pixels
                .get(shape_row * pointer.width as usize + shape_column)
                .copied()
                .unwrap_or(0);
            let colour = value
                .checked_sub(1)
                .and_then(|index| pointer.colours.get(usize::from(index)));
            if let Some(&colour) = colour {
                span[(column - first) as usize] = colour;
            }
        }
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.screen.width, self.screen.height)
    }

    fn change(&mut self, rect: Rect) {
        let clipped = rect.intersection(&self.bounds());
        if clipped.width > 0 && clipped.height > 0 {
            self.changed.add(clipped);
        }
    }

    /// The rectangle a shown cursor covers.
    fn cursor_rect(&self) -> Option<Rect> {
        let shown = self.cursor?.flash == Flash::Steady || self.lit;
        self.cursor_cell().filter(|_| shown)
    }

    /// The rectangle the cursor covers when lit.
    fn cursor_cell(&self) -> Option<Rect> {
        let cursor = self.cursor?;
        let cell = Rect::new(
            to_i32(cursor.x),
            to_i32(cursor.y + cursor.top),
            cursor.width,
            cursor.bottom - cursor.top + 1,
        );
        Some(cell.intersection(&self.bounds()))
    }

    /// The rectangle of the screen a shown pointer covers.
    fn pointer_rect(&self) -> Option<Rect> {
        self.pointer_drawn().map(|(_, rect, _)| rect)
    }

    /// A shown pointer: its shape, the rectangle of the screen it covers, and
    /// where its top left is, which may lie off the screen.
    fn pointer_drawn(&self) -> Option<(&Pointer, Rect, (i64, i64))> {
        let pointer = self.pointer.as_ref()?;
        let (x, y) = self.at?;
        let left = i64::from(x) - i64::from(pointer.active.0);
        let top = i64::from(y) - i64::from(pointer.active.1);
        let whole = Rect::new(
            clamp_i32(left),
            clamp_i32(top),
            pointer.width,
            pointer.height,
        );
        let clipped = whole.intersection(&self.bounds());
        (clipped.width > 0 && clipped.height > 0).then_some((pointer, clipped, (left, top)))
    }
}

fn len32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn clamp_i32(value: i64) -> i32 {
    i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
}

/// A coordinate of a rectangle held within the screen.
fn on_screen(at: i32) -> u32 {
    u32::try_from(at).unwrap_or(0)
}

fn covers_row(rect: &Rect, row: u32) -> bool {
    let top = on_screen(rect.origin.y);
    row >= top && row < top + rect.height
}

/// The columns of `shown` that `rect` covers.
fn overlap(rect: &Rect, shown: &core::ops::Range<u32>) -> core::ops::Range<u32> {
    let left = on_screen(rect.origin.x);
    left.max(shown.start)..(left + rect.width).min(shown.end)
}

#[cfg(test)]
mod tests;
