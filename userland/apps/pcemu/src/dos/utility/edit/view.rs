//! The screen `EDIT` draws, composed as cells — a character and its
//! attribute each — for the program to write to the video memory at once:
//! the menu bar, the text window in its frame with its title, the text with
//! its selection, the scroll bars, and the status line with the cursor's
//! line and column. Menus and dialogs draw over it with the same helpers.

use super::editor::{advance, Editor, Spot};
use super::text::{Store, Text};
use alloc::vec::Vec;

/// The characters the frames and bars are drawn with, from code page 437.
pub mod glyph {
    pub const TOP_LEFT: u8 = 0xDA;
    pub const TOP_RIGHT: u8 = 0xBF;
    pub const BOTTOM_LEFT: u8 = 0xC0;
    pub const BOTTOM_RIGHT: u8 = 0xD9;
    pub const ACROSS: u8 = 0xC4;
    pub const DOWN: u8 = 0xB3;
    pub const TEE_LEFT: u8 = 0xC3;
    pub const TEE_RIGHT: u8 = 0xB4;
    pub const TRACK: u8 = 0xB0;
    pub const THUMB: u8 = 0xDB;
    pub const UP_ARROW: u8 = 0x18;
    pub const DOWN_ARROW: u8 = 0x19;
    pub const RIGHT_ARROW: u8 = 0x1A;
    pub const LEFT_ARROW: u8 = 0x1B;
}

/// The attributes a screen is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub menu: u8,
    pub hot: u8,
    pub chosen: u8,
    pub chosen_hot: u8,
    pub disabled: u8,
    pub text: u8,
    pub selected: u8,
    pub bar: u8,
    pub status: u8,
    pub dialog: u8,
    pub field: u8,
    pub shadow: u8,
}

impl Palette {
    /// White on blue, a grey menu bar and a cyan status line.
    pub const COLOUR: Self = Self {
        menu: 0x70,
        hot: 0x7F,
        chosen: 0x07,
        chosen_hot: 0x0F,
        disabled: 0x78,
        text: 0x17,
        selected: 0x71,
        bar: 0x70,
        status: 0x30,
        dialog: 0x70,
        field: 0x07,
        shadow: 0x08,
    };

    /// For a monochrome display, or `/B`'s.
    pub const MONO: Self = Self {
        menu: 0x70,
        hot: 0x7F,
        chosen: 0x07,
        chosen_hot: 0x0F,
        disabled: 0x70,
        text: 0x07,
        selected: 0x70,
        bar: 0x70,
        status: 0x70,
        dialog: 0x70,
        field: 0x07,
        shadow: 0x07,
    };

    /// For a display without bright colours, `/NOHI`'s: the key letters
    /// and the chosen entry told apart by colour alone.
    pub const PLAIN: Self = Self {
        menu: 0x70,
        hot: 0x74,
        chosen: 0x07,
        chosen_hot: 0x04,
        disabled: 0x78,
        text: 0x17,
        selected: 0x71,
        bar: 0x70,
        status: 0x30,
        dialog: 0x70,
        field: 0x07,
        shadow: 0x08,
    };

    /// The palette with its text in `attribute`, the selection the same
    /// colours reversed.
    #[must_use]
    pub const fn with_text(self, attribute: u8) -> Self {
        Self {
            text: attribute,
            selected: attribute.rotate_left(4) & 0x7F,
            ..self
        }
    }
}

/// A screen being drawn: its rows of cells, each a character in the low
/// byte and its attribute in the high.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cells {
    pub rows: u8,
    pub columns: u8,
    pub grid: Vec<u16>,
}

impl Cells {
    #[must_use]
    pub fn new(rows: u8, columns: u8) -> Self {
        Self {
            rows,
            columns,
            grid: alloc::vec![0x0720; usize::from(rows) * usize::from(columns)],
        }
    }

    /// `byte` put at `row`, `column` in `attribute`, nothing off the screen.
    pub fn put(&mut self, row: u8, column: u8, byte: u8, attribute: u8) {
        if row < self.rows && column < self.columns {
            self.grid[usize::from(row) * usize::from(self.columns) + usize::from(column)] =
                u16::from_le_bytes([byte, attribute]);
        }
    }

    /// `text` written from `row`, `column`.
    pub fn write(&mut self, row: u8, column: u8, text: &[u8], attribute: u8) {
        for (at, &byte) in (column..self.columns).zip(text) {
            self.put(row, at, byte, attribute);
        }
    }

    /// `text` written with its letter after `&` in `hot`, the `&` not shown:
    /// the columns it took.
    pub fn label(&mut self, row: u8, column: u8, text: &[u8], attribute: u8, hot: u8) -> u8 {
        let mut at = column;
        let mut marked = false;
        for &byte in text {
            if byte == b'&' && !marked {
                marked = true;
                continue;
            }
            let hot_letter = marked;
            marked = false;
            self.put(row, at, byte, if hot_letter { hot } else { attribute });
            at = at.saturating_add(1);
        }
        at - column
    }

    /// The rectangle `top`..=`bottom`, `left`..=`right` filled with `byte`.
    pub fn fill(&mut self, top: u8, left: u8, bottom: u8, right: u8, byte: u8, attribute: u8) {
        for row in top..=bottom {
            for column in left..=right {
                self.put(row, column, byte, attribute);
            }
        }
    }

    /// A box with a single-line frame, cleared inside, `title` centred in
    /// its top edge.
    pub fn frame(&mut self, top: u8, left: u8, bottom: u8, right: u8, attribute: u8, title: &[u8]) {
        self.fill(top, left, bottom, right, b' ', attribute);
        for column in left + 1..right {
            self.put(top, column, glyph::ACROSS, attribute);
            self.put(bottom, column, glyph::ACROSS, attribute);
        }
        for row in top + 1..bottom {
            self.put(row, left, glyph::DOWN, attribute);
            self.put(row, right, glyph::DOWN, attribute);
        }
        self.put(top, left, glyph::TOP_LEFT, attribute);
        self.put(top, right, glyph::TOP_RIGHT, attribute);
        self.put(bottom, left, glyph::BOTTOM_LEFT, attribute);
        self.put(bottom, right, glyph::BOTTOM_RIGHT, attribute);
        if !title.is_empty() {
            let width = u8::try_from(title.len() + 2)
                .unwrap_or(u8::MAX)
                .min(right - left - 1);
            let at = left + (right - left + 1 - width) / 2;
            self.put(top, at, b' ', attribute);
            self.write(top, at + 1, &title[..usize::from(width - 2)], attribute);
            self.put(top, at + width - 1, b' ', attribute);
        }
    }

    /// The shadow a dialog or menu at that rectangle casts: the attribute
    /// of the cells to its right and below darkened.
    pub fn shadow(&mut self, top: u8, left: u8, bottom: u8, right: u8, attribute: u8) {
        let mut darken = |row: u8, column: u8| {
            if row < self.rows && column < self.columns {
                let cell = &mut self.grid
                    [usize::from(row) * usize::from(self.columns) + usize::from(column)];
                *cell = (*cell & 0x00FF) | (u16::from(attribute) << 8);
            }
        };
        for row in top + 1..=bottom + 1 {
            darken(row, right + 1);
            darken(row, right + 2);
        }
        for column in left + 2..=right + 2 {
            darken(bottom + 1, column);
        }
    }
}

/// The menu bar's entries: their labels, `&` before the key letter, and
/// whether each sits at the bar's right.
pub const BAR: [(&[u8], bool); 5] = [
    (b"&File", false),
    (b"&Edit", false),
    (b"&Search", false),
    (b"&Options", false),
    (b"&Help", true),
];

/// The columns each menu bar entry starts at on a screen `columns` wide.
#[must_use]
pub fn menu_columns(columns: u8) -> [u8; 5] {
    let mut out = [0u8; 5];
    let mut at = 2u8;
    for (slot, &(label, right)) in out.iter_mut().zip(&BAR) {
        let width = u8::try_from(label.len() - 1).unwrap_or(0);
        *slot = if right {
            columns.saturating_sub(width + 2)
        } else {
            at
        };
        at = at.saturating_add(width + 2);
    }
    out
}

/// How the menu bar is shown: plain, with its key letters while Alt is
/// down, or with entry `n` chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bar {
    #[default]
    Plain,
    Keys,
    Chosen(u8),
}

/// The menu bar drawn in row 0.
pub fn menu_bar(cells: &mut Cells, palette: &Palette, bar: Bar) {
    let columns = cells.columns;
    cells.fill(0, 0, 0, columns - 1, b' ', palette.menu);
    for (n, (&(label, _), at)) in BAR.iter().zip(menu_columns(columns)).enumerate() {
        let chosen = bar == Bar::Chosen(u8::try_from(n).unwrap_or(0));
        let (attribute, hot) = match bar {
            _ if chosen => (palette.chosen, palette.chosen_hot),
            Bar::Plain => (palette.menu, palette.menu),
            _ => (palette.menu, palette.hot),
        };
        cells.put(0, at.saturating_sub(1), b' ', attribute);
        let width = cells.label(0, at, label, attribute, hot);
        cells.put(0, at + width, b' ', attribute);
    }
}

/// Where the text window lies: rows 2 to the third from the foot, and
/// columns 1 to the second from the right.
#[must_use]
pub const fn window(rows: u8, columns: u8) -> (u8, u8) {
    (rows.saturating_sub(4), columns.saturating_sub(2))
}

/// The thumb's place on a bar of `length` cells, at `at` of `extent`.
pub fn thumb(at: u32, extent: u32, length: u8) -> u8 {
    let track = u32::from(length.saturating_sub(2).max(1));
    let place = if extent <= 1 {
        0
    } else {
        u64::from(at.min(extent - 1)) * u64::from(track - 1) / u64::from(extent - 1)
    };
    1 + u8::try_from(place).unwrap_or(0)
}

/// How the text window is drawn: its title, its colours, and whether it
/// has scroll bars.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    pub palette: &'a Palette,
    pub title: &'a [u8],
    pub scroll_bars: bool,
}

/// The editor drawn: its window, text and bars, and the status line with
/// `hint`, the help it gives.
pub fn draw<S: Store>(
    cells: &mut Cells,
    look: Look<'_>,
    editor: &Editor,
    text: &mut Text<'_, S>,
    hint: &[u8],
) {
    let palette = look.palette;
    let (rows, columns) = (cells.rows, cells.columns);
    cells.frame(1, 0, rows - 2, columns - 1, palette.text, look.title);
    let (shown, width) = window(rows, columns);
    let selection = selected(editor, text);
    let mut start = Some(editor.top.start);
    for row in 0..shown {
        let Some(at) = start else {
            break;
        };
        let end = text.line_end(at);
        let bytes = text.read(at..end);
        let mut column = 0u16;
        let right = editor.left.saturating_add(u16::from(width));
        for (n, &byte) in bytes.iter().enumerate() {
            if column >= right {
                break;
            }
            let next = advance(column, byte, editor.tab);
            let offset = at + u32::try_from(n).unwrap_or(0);
            let chosen = selection
                .as_ref()
                .is_some_and(|range| range.contains(&offset));
            for cell in column..next {
                if let Some(shown_at) = cell
                    .checked_sub(editor.left)
                    .filter(|&shown_at| shown_at < u16::from(width))
                {
                    let glyph = if byte == b'\t' { b' ' } else { byte };
                    cells.put(
                        2 + row,
                        1 + u8::try_from(shown_at).unwrap_or(0),
                        glyph,
                        if chosen {
                            palette.selected
                        } else {
                            palette.text
                        },
                    );
                }
            }
            column = next;
        }
        let line_break = selection
            .as_ref()
            .is_some_and(|range| range.contains(&end) && text.byte(end).is_some());
        if let Some(shown_at) = column
            .checked_sub(editor.left)
            .filter(|&shown_at| line_break && shown_at < u16::from(width))
        {
            cells.put(
                2 + row,
                1 + u8::try_from(shown_at).unwrap_or(0),
                b' ',
                palette.selected,
            );
        }
        start = text.after_end(end);
    }
    if look.scroll_bars {
        scroll_bars(cells, palette, editor);
    }
    status(cells, palette, hint, editor.cursor);
}

/// The scroll bars along the window's right and foot, each thumb where
/// the cursor stands.
fn scroll_bars(cells: &mut Cells, palette: &Palette, editor: &Editor) {
    let (rows, columns) = (cells.rows, cells.columns);
    let (bar_length, _) = window(rows, columns);
    cells.put(2, columns - 1, glyph::UP_ARROW, palette.bar);
    for row in 3..=bar_length {
        cells.put(row, columns - 1, glyph::TRACK, palette.bar);
    }
    cells.put(1 + bar_length, columns - 1, glyph::DOWN_ARROW, palette.bar);
    cells.put(
        1 + thumb(editor.cursor.line, editor.lines, bar_length) + 1,
        columns - 1,
        glyph::THUMB,
        palette.bar,
    );
    let foot = rows - 2;
    cells.put(foot, 1, glyph::LEFT_ARROW, palette.bar);
    for column in 2..columns - 2 {
        cells.put(foot, column, glyph::TRACK, palette.bar);
    }
    cells.put(foot, columns - 2, glyph::RIGHT_ARROW, palette.bar);
    let across = thumb(
        u32::from(editor.cursor.column),
        u32::from(super::editor::MOST_COLUMN) + 1,
        columns - 2,
    );
    cells.put(foot, 1 + across, glyph::THUMB, palette.bar);
}

/// The status line: `hint` at its left, and the cursor's line and column
/// at its right.
pub fn status(cells: &mut Cells, palette: &Palette, hint: &[u8], cursor: Spot) {
    let (row, columns) = (cells.rows - 1, cells.columns);
    cells.fill(row, 0, row, columns - 1, b' ', palette.status);
    cells.write(row, 1, hint, palette.status);
    let place = alloc::format!(
        "{:05}:{:03}",
        u64::from(cursor.line) + 1,
        u32::from(cursor.column) + 1
    );
    let at = columns.saturating_sub(u8::try_from(place.len() + 1).unwrap_or(0));
    cells.put(row, at - 2, glyph::DOWN, palette.status);
    cells.write(row, at, place.as_bytes(), palette.status);
}

/// The selection's bytes, as the editor reckons them.
fn selected<S: Store>(editor: &Editor, text: &mut Text<'_, S>) -> Option<core::ops::Range<u32>> {
    let mut copy = *editor;
    let mut edit = super::editor::Edit {
        editor: &mut copy,
        text,
    };
    edit.selection()
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
