//! The screen `EDIT` draws on: the one it found kept, an eighty-column
//! text mode set where that is not it, `/H`'s font of eight-line
//! characters loaded where the display has one, and all put back as it
//! was. The editor's cells are written to the page in one move, and the
//! cursor placed and shaped as the editor stands.

use super::ui::{Screen, Shade};
use super::view::Cells;
use super::{at, keep, load, Asked, Video};
use crate::bios::bda;
use crate::dos::program::{Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// The text modes, and the colour and monochrome ones of eighty columns.
const MONO_TEXT: u8 = 7;
const COLOUR_TEXT: u8 = 3;
const GREY_TEXT: u8 = 2;
/// The equipment word's bits that say the display is monochrome.
const MONO_DISPLAY: u16 = 0x30;
/// INT 10h `1A00h`'s answer from a VGA, and `12h`'s `BL` from a BIOS
/// that is no EGA's.
const VGA: u8 = 0x1A;
const NO_EGA: u8 = 0x10;
/// The cursor's shape hidden, and the underlines DOS starts with.
const HIDDEN: u16 = 0x2000;
const MONO_LINE: u16 = 0x0B0C;
const COLOUR_LINE: u16 = 0x0607;

const fn is_text(mode: u8) -> bool {
    mode <= COLOUR_TEXT || mode == MONO_TEXT
}

/// The segment of the video memory mode `mode` shows.
const fn video_segment(mode: u8) -> u16 {
    if mode == MONO_TEXT {
        0xB000
    } else {
        0xB800
    }
}

/// The rows the BIOS says the screen has: an EGA's or VGA's count, or the
/// twenty-five of the adapters that keep none.
fn rows_now(p: &mut Program<'_, '_>) -> u8 {
    match p.g.bda8(bda::ROWS) {
        0 => 25,
        last => last.saturating_add(1),
    }
}

/// The bytes of a page of `rows` and `columns`, as many as the kept
/// screen holds.
fn page_bytes(rows: u8, columns: u16) -> usize {
    (usize::from(rows) * usize::from(columns) * 2).min(usize::from(at::SCREEN_LEN))
}

locals! {
    /// The screen found kept and set for the editor: its rows, columns and
    /// shade packed as [`Screen::packed`] gives them.
    pub struct EditScreen {}
}

mod phase {
    pub const MODE_SET: u8 = 1;
    pub const VGA_ASKED: u8 = 2;
    pub const EGA_ASKED: u8 = 3;
    pub const FONT_SET: u8 = 4;
}

impl EditScreen {
    /// The screen as it is found: kept, and a mode of the editor's own set
    /// where it is not eighty columns of text.
    fn found(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mode = p.g.bda8(bda::VIDEO_MODE) & 0x7F;
        let columns = p.g.bda16(bda::COLUMNS);
        let page = p.g.bda8(bda::ACTIVE_PAGE);
        let rows = rows_now(p);
        let cursor = p.g.bda16(bda::CURSOR + u16::from(page.min(7)) * 2);
        let shape = p.g.bda16(bda::CURSOR_SHAPE);
        let mut video = Video {
            mode,
            page,
            rows,
            cursor,
            shape,
            ..Video::default()
        };
        if is_text(mode) {
            let mut cells = alloc::vec![0u8; page_bytes(rows, columns)];
            let offset = p.g.bda16(bda::PAGE_START);
            p.g.read_bytes(video_segment(mode), offset, &mut cells);
            p.g.write_bytes(p.own, at::SCREEN, &cells);
            video.saved = true;
        }
        let wide = matches!(mode, GREY_TEXT | COLOUR_TEXT | MONO_TEXT) && columns == 80;
        if !wide {
            video.set = true;
            keep(p, at::VIDEO, video);
            let mono = p.g.bda16(bda::EQUIPMENT) & MONO_DISPLAY == MONO_DISPLAY;
            *phase = phase::MODE_SET;
            return Flow::Call(Frame::of(&Int::bios(
                p.own,
                0x10,
                u16::from(if mono { MONO_TEXT } else { COLOUR_TEXT }),
            )));
        }
        keep(p, at::VIDEO, video);
        Self::most(p, phase)
    }

    /// `/H`: the display asked whether it is a VGA or an EGA, to show more
    /// rows.
    fn most(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let asked: Asked = load(p, at::ASKED);
        if !asked.most {
            return Self::ready(p);
        }
        *phase = phase::VGA_ASKED;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x1A00)))
    }

    /// The font of eight-line characters loaded.
    fn font(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::FONT_SET;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x1112)))
    }

    /// The screen set: where the editor draws, and what it draws on.
    fn ready(p: &mut Program<'_, '_>) -> Flow {
        let mut video: Video = load(p, at::VIDEO);
        let mode = p.g.bda8(bda::VIDEO_MODE) & 0x7F;
        let asked: Asked = load(p, at::ASKED);
        video.segment = video_segment(mode);
        video.offset = p.g.bda16(bda::PAGE_START);
        video.shown = u16::MAX;
        keep(p, at::VIDEO, video);
        let shade = if asked.mono || matches!(mode, MONO_TEXT | GREY_TEXT) {
            Shade::Mono
        } else if asked.plain {
            Shade::Plain
        } else {
            Shade::Colour
        };
        let columns = u8::try_from(p.g.bda16(bda::COLUMNS)).unwrap_or(80);
        let screen = Screen {
            rows: rows_now(p),
            columns,
            shade,
        };
        Flow::Return(Ok(screen.packed()))
    }
}

impl Routine for EditScreen {
    const ID: Id = Id::EditScreen;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::MODE_SET => {
                take!(child);
                Self::most(p, phase)
            }
            phase::VGA_ASKED => {
                if take!(child).to_le_bytes()[0] == VGA {
                    return Self::font(p, phase);
                }
                *phase = phase::EGA_ASKED;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(NO_EGA),
                    ..Int::bios(p.own, 0x10, 0x1200)
                }))
            }
            phase::EGA_ASKED => {
                take!(child);
                if p.result(Returned::Bx).to_le_bytes()[0] != NO_EGA {
                    return Self::font(p, phase);
                }
                Self::ready(p)
            }
            phase::FONT_SET => {
                take!(child);
                let mut video: Video = load(p, at::VIDEO);
                video.font = true;
                keep(p, at::VIDEO, video);
                Self::ready(p)
            }
            _ => Self::found(p, phase),
        }
    }
}

/// The editor's cells written to the page it draws, in one move.
pub fn blit(p: &mut Program<'_, '_>, cells: &Cells) {
    let video: Video = load(p, at::VIDEO);
    let bytes: Vec<u8> = cells
        .grid
        .iter()
        .flat_map(|cell| cell.to_le_bytes())
        .collect();
    p.g.write_bytes(video.segment, video.offset, &bytes);
}

/// The cursor's shape: the line DOS shows while the editor puts text in,
/// a block while it types over, none where the editor shows none.
#[must_use]
pub fn shape(video: &Video, shown: bool, overtype: bool) -> u16 {
    let line = match video.shape {
        shape if shape & HIDDEN == 0 => shape,
        _ if video.mode == MONO_TEXT => MONO_LINE,
        _ => COLOUR_LINE,
    };
    match (shown, overtype) {
        (false, _) => HIDDEN,
        (true, false) => line,
        (true, true) => line & 0x00FF,
    }
}

/// The call placing the cursor at `row`, `column` of the page.
pub fn place(p: &Program<'_, '_>, video: &Video, row: u8, column: u8) -> Frame {
    Frame::of(&Int {
        bx: u16::from(video.page) << 8,
        dx: u16::from_le_bytes([column, row]),
        ..Int::bios(p.own, 0x10, 0x0200)
    })
}

/// The call giving the cursor `shape`.
pub fn reshape(p: &Program<'_, '_>, shape: u16) -> Frame {
    Frame::of(&Int {
        cx: shape,
        ..Int::bios(p.own, 0x10, 0x0100)
    })
}

/// The INT 10h call loading the font that shows `rows` rows on an EGA or
/// VGA, none for the twenty-five a mode starts with: fourteen-line
/// characters give a VGA twenty-eight, eight-line ones forty-three or fifty.
const fn font_for(rows: u8) -> Option<u16> {
    match rows {
        0..=25 => None,
        26..=28 => Some(0x1111),
        _ => Some(0x1112),
    }
}

locals! {
    /// The screen put back as the editor found it: its mode and font where
    /// the editor set its own, its cells, and its cursor.
    pub struct EditRestore {}
}

mod back {
    pub const MODE: u8 = 1;
    pub const FONT: u8 = 2;
    pub const PAGE: u8 = 3;
    pub const PLACED: u8 = 4;
    pub const SHAPED: u8 = 5;
}

impl EditRestore {
    /// The cells written back and the cursor put where it was.
    fn cells(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let video: Video = load(p, at::VIDEO);
        if video.saved {
            let columns = p.g.bda16(bda::COLUMNS);
            let mut cells = alloc::vec![0u8; page_bytes(video.rows, columns)];
            p.g.read_bytes(p.own, at::SCREEN, &mut cells);
            let offset = p.g.bda16(bda::PAGE_START);
            p.g.write_bytes(video_segment(video.mode), offset, &cells);
        }
        let [column, row] = video.cursor.to_le_bytes();
        *phase = back::PLACED;
        Flow::Call(place(p, &video, row, column))
    }
}

impl Routine for EditRestore {
    const ID: Id = Id::EditRestore;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let video: Video = load(p, at::VIDEO);
        match *phase {
            back::MODE => {
                take!(child);
                if let Some(font) = font_for(video.rows) {
                    *phase = back::FONT;
                    return Flow::Call(Frame::of(&Int::bios(p.own, 0x10, font)));
                }
                *phase = back::PAGE;
                Flow::Call(Frame::of(&Int::bios(
                    p.own,
                    0x10,
                    0x0500 | u16::from(video.page),
                )))
            }
            back::FONT => {
                take!(child);
                *phase = back::PAGE;
                Flow::Call(Frame::of(&Int::bios(
                    p.own,
                    0x10,
                    0x0500 | u16::from(video.page),
                )))
            }
            back::PAGE => {
                take!(child);
                Self::cells(p, phase)
            }
            back::PLACED => {
                take!(child);
                *phase = back::SHAPED;
                Flow::Call(reshape(p, video.shape))
            }
            back::SHAPED => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ if video.set || video.font => {
                *phase = back::MODE;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, u16::from(video.mode))))
            }
            _ => Self::cells(p, phase),
        }
    }
}
