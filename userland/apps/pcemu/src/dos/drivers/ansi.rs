//! `ANSI.SYS`: a console driver loaded from the disk whose output obeys
//! the ANSI escape sequences MS-DOS's own takes — cursor movement and
//! position, saving and restoring it, erasing, graphics renditions, screen
//! modes and wrap, the position report — and whose input carries key
//! reassignments. Its file is a stub: the header's strategy and interrupt
//! entries, and the INT 29h handler it installs, far-call the ROM with DS
//! its own segment, where everything it keeps lies.
//!
//! Keys are read through INT 16h function `00h`, so an enhanced keyboard's
//! extra keys arrive as the 83-key ones they duplicate, as `/K` asks; `/K`,
//! `/X` and `/R` are taken and change nothing more.
//!
//! It answers INT 2Fh's installation check, function `1A00h`, and serves
//! the console's display information through generic IOCTL
//! ([`super::ansi_ioctl`]), the display hardware found at INIT.

use super::{local, unknown, Driver};
use crate::asm::{Reg16, Reg8};
use crate::bios::bda;
use crate::cpu::flag;
use crate::dos::device::{self, attribute, command, field};
use crate::dos::engine::Far;
use crate::dos::rom::entry;
use alloc::vec;
use alloc::vec::Vec;

/// Where things lie in the driver's image.
pub(super) mod at {
    /// The strategy entry: a far jump to the ROM's.
    pub const STRATEGY: u16 = 0x12;
    /// The interrupt entry.
    pub const INTERRUPT: u16 = 0x17;
    /// The INT 29h handler.
    pub const FAST: u16 = 0x21;
    /// The INT 2Fh handler: the installation check answered, every other
    /// call chained to the handler kept at [`OLD_MULTIPLEX`].
    pub const MULTIPLEX: u16 = 0x2B;
    pub const OLD_MULTIPLEX: u16 = 0x3C;
    /// The parser's state.
    pub const STATE: u16 = 0x40;
    /// Numeric parameters taken.
    pub const COUNT: u16 = 0x41;
    /// A sequence's private mark, `=` or `?`.
    pub const MARK: u16 = 0x42;
    /// The rendition characters are written with.
    pub const ATTRIBUTE: u16 = 0x43;
    /// Whether a full line wraps.
    pub const WRAP: u16 = 0x44;
    /// The saved row and column.
    pub const SAVED: u16 = 0x45;
    /// Bytes queued for input, and the next.
    pub const QUEUE_LEN: u16 = 0x47;
    /// The next of them.
    pub const QUEUE_AT: u16 = 0x48;
    /// Bytes a reassignment's parameters have taken.
    pub const BYTES_LEN: u16 = 0x49;
    /// The numeric parameters.
    pub const PARAMS: u16 = 0x4A;
    /// Bytes of the reassignment table.
    pub const KEYS_LEN: u16 = 0x5A;
    /// Set while a number is being read.
    pub const NUMBER: u16 = 0x5C;
    /// The input queue.
    pub const QUEUE: u16 = 0x60;
    /// A reassignment's parameters, a byte each, strings as their bytes.
    pub const BYTES: u16 = 0xA0;
    /// The reassignment table: entries of a key's code, the length of what
    /// it types, then that.
    pub const KEYS: u16 = 0x120;
    /// The display hardware INIT found, as
    /// [`super::super::ansi_ioctl::hardware`] keeps it; the VGA's scan-line
    /// settings offered in text modes and the one in force at INIT; and the
    /// EGA's blink last set.
    pub const HARDWARE: u16 = 0x220;
    pub const SCANS: u16 = 0x221;
    pub const MAX_SCANS: u16 = 0x222;
    pub const INTENSITY: u16 = 0x223;
    /// Where the VGA BIOS's state is read into.
    pub const VIDEO_STATE: u16 = 0x224;
    /// The image's end.
    pub const END: u16 = 0x264;

    const _: () =
        assert!(MULTIPLEX as usize + super::MULTIPLEX_STUB.len() <= OLD_MULTIPLEX as usize);
    const _: () = assert!(OLD_MULTIPLEX + 4 <= STATE);
}

const QUEUE_ROOM: u8 = 64;
const BYTES_ROOM: u8 = 128;
const KEYS_ROOM: u16 = at::END - at::KEYS;
const PARAMS: u8 = 8;

/// The parser's states.
mod state {
    pub const PLAIN: u8 = 0;
    pub const ESCAPE: u8 = 1;
    pub const SEQUENCE: u8 = 2;
    pub const QUOTED: u8 = 3;
}

const ESC: u8 = 0x1B;

/// The driver's file: its header, named `CON` so it becomes the console,
/// its stubs, and room for what it keeps.
#[must_use]
pub fn image() -> Vec<u8> {
    let mut image = vec![0u8; usize::from(at::END)];
    let attributes = attribute::CHARACTER
        | attribute::GENERIC
        | attribute::FAST
        | attribute::STDOUT
        | attribute::STDIN;
    image[..4].copy_from_slice(&[0xFF; 4]);
    image[4..6].copy_from_slice(&attributes.to_le_bytes());
    image[6..8].copy_from_slice(&at::STRATEGY.to_le_bytes());
    image[8..10].copy_from_slice(&at::INTERRUPT.to_le_bytes());
    image[10..18].copy_from_slice(device::CON);
    let rom = crate::bios::layout::SEGMENT.to_le_bytes();
    let far = |offset: u16| {
        let [low, high] = offset.to_le_bytes();
        [low, high, rom[0], rom[1]]
    };
    let strategy = usize::from(at::STRATEGY);
    image[strategy] = 0xEA;
    image[strategy + 1..strategy + 5].copy_from_slice(&far(entry::STRATEGY));
    for (stub, target, back) in [
        (at::INTERRUPT, entry::ANSI, 0xCB),
        (at::FAST, entry::ANSI_FAST, 0xCF),
    ] {
        let at = usize::from(stub);
        image[at..at + 4].copy_from_slice(&[0x1E, 0x0E, 0x1F, 0x9A]);
        image[at + 4..at + 8].copy_from_slice(&far(target));
        image[at + 8..at + 10].copy_from_slice(&[0x1F, back]);
    }
    let multiplex = usize::from(at::MULTIPLEX);
    image[multiplex..multiplex + MULTIPLEX_STUB.len()].copy_from_slice(&MULTIPLEX_STUB);
    image
}

/// INT 2Fh: `CMP AX,1A00h`, `JNE` past `MOV AL,0FFh` and `IRET`, to `JMP FAR
/// [CS:OLD_MULTIPLEX]`.
const MULTIPLEX_STUB: [u8; 13] = {
    let [low, high] = at::OLD_MULTIPLEX.to_le_bytes();
    [
        0x3D, 0x00, 0x1A, 0x75, 0x03, 0xB0, 0xFF, 0xCF, 0x2E, 0xFF, 0x2E, low, high,
    ]
};

/// The byte INT 29h hands it.
const FAST_BYTE: u16 = local::SCRATCH + 2;
/// The row and column a step's move goes to.
const TARGET: u16 = local::SCRATCH + 3;
/// Spaces a tab still owes.
const SPACES: u16 = local::SCRATCH + 5;

/// The BIOS calls an act is made of, as the phase that follows each.
mod step {
    pub const WROTE: u8 = 1;
    pub const SCROLLED: u8 = 2;
    pub const MOVED: u8 = 3;
    pub const DONE: u8 = 4;
    pub const KEY: u8 = 5;
    pub const LOOKED: u8 = 6;
    pub const FLUSHING: u8 = 7;
    /// INIT's questions of the VGA BIOS and the EGA's.
    pub const VGA_ASKED: u8 = 8;
    pub const EGA_ASKED: u8 = 9;
}

/// A request to `ANSI.SYS`.
pub fn serve(d: &mut Driver<'_, '_>) {
    match d.get8(field::COMMAND) {
        command::INIT => init(d),
        command::OPEN | command::CLOSE | command::OUTPUT_STATUS | command::OUTPUT_FLUSH => {
            d.finish(None);
        }
        command::INPUT => input(d),
        command::PEEK => look(d, true),
        command::INPUT_STATUS => look(d, false),
        command::INPUT_FLUSH => flush(d),
        command::OUTPUT | command::OUTPUT_VERIFY => output(d, false),
        command::GENERIC_IOCTL => super::ansi_ioctl::serve(d),
        _ => unknown(d),
    }
}

/// INT 29h: one byte, AL, written through the escape sequences.
pub fn fast(d: &mut Driver<'_, '_>) {
    if d.phase() == 0 && d.local16(local::DONE) == 0 {
        let byte = d.g.r8(Reg8::Al);
        d.set_local8(FAST_BYTE, byte);
    }
    output(d, true);
}

/// `INIT`: the rendition plain, wrap on, nothing queued or reassigned, INT
/// 29h taken over and INT 2Fh hooked; then the display hardware found as
/// DOS 4's `ANSI.SYS` finds it — which adapters' memory answers, then the
/// VGA BIOS's state, else the EGA BIOS's answer.
fn init(d: &mut Driver<'_, '_>) {
    use super::ansi_ioctl::{answering, ega_found, vga_found};
    let segment = d.instance();
    match d.phase() {
        step::VGA_ASKED => {
            if let Some(kind) = vga_found(d) {
                let found = d.own8(at::HARDWARE);
                d.set_own8(at::HARDWARE, found | kind);
                return d.finish(None);
            }
            d.g.set_r16(Reg16::Ax, 0x1200);
            d.g.set_r16(Reg16::Bx, 0x0010);
            d.set_phase(step::EGA_ASKED);
            d.bios(0x10);
        }
        step::EGA_ASKED => {
            let found = d.own8(at::HARDWARE) | ega_found(d);
            d.set_own8(at::HARDWARE, found);
            d.finish(None);
        }
        _ => {
            d.g.write_bytes(segment, at::STATE, &[0; (at::END - at::STATE) as usize]);
            d.set_own8(at::ATTRIBUTE, 0x07);
            d.set_own8(at::WRAP, 1);
            d.g.write16(0, 0x29 * 4, at::FAST);
            d.g.write16(0, 0x29 * 4 + 2, segment);
            let (offset, chained) = (d.g.read16(0, 0x2F * 4), d.g.read16(0, 0x2F * 4 + 2));
            d.g.write16(segment, at::OLD_MULTIPLEX, offset);
            d.g.write16(segment, at::OLD_MULTIPLEX + 2, chained);
            d.g.write16(0, 0x2F * 4, at::MULTIPLEX);
            d.g.write16(0, 0x2F * 4 + 2, segment);
            d.set_far(field::ADDRESS, Far::new(segment, at::END));
            let found = answering(d);
            d.set_own8(at::HARDWARE, found);
            d.g.regs_mut().set_sreg(crate::cpu::Seg::Es, segment);
            d.g.set_r16(Reg16::Di, at::VIDEO_STATE);
            d.g.set_r16(Reg16::Ax, 0x1B00);
            d.g.set_r16(Reg16::Bx, 0);
            d.set_phase(step::VGA_ASKED);
            d.bios(0x10);
        }
    }
}

/// The screen's geometry and the cursor, as the BIOS keeps them.
#[derive(Clone, Copy, Debug)]
struct Cursor {
    page: u8,
    row: u8,
    column: u8,
    rows: u8,
    columns: u8,
}

fn cursor(d: &mut Driver<'_, '_>) -> Cursor {
    let screen = crate::bios::video::Screen::of(d.g);
    let page = d.g.bda8(bda::ACTIVE_PAGE) & 7;
    let [column, row] = d.g.bda16(bda::CURSOR + u16::from(page) * 2).to_le_bytes();
    let rows = u8::try_from(screen.text_rows()).unwrap_or(25).max(1);
    let columns = u8::try_from(screen.text_columns()).unwrap_or(80).max(1);
    Cursor {
        page,
        row: row.min(rows - 1),
        column: column.min(columns - 1),
        rows,
        columns,
    }
}

/// Writes the request's bytes — or INT 29h's — through the parser, each
/// act its BIOS calls.
fn output(d: &mut Driver<'_, '_>, fast: bool) {
    loop {
        match d.phase() {
            step::WROTE => {
                d.set_phase(0);
                let here = cursor(d);
                let wrap = d.own8(at::WRAP) != 0;
                if here.column + 1 < here.columns {
                    return move_to(d, here, here.row, here.column + 1);
                }
                if !wrap {
                    continue;
                }
                return line_feed(d, here, 0);
            }
            step::SCROLLED => {
                let [row, column] = d.local16(TARGET).to_le_bytes();
                let here = cursor(d);
                return move_to(d, here, row, column);
            }
            step::MOVED | step::DONE => {
                d.set_phase(0);
                if d.local8(SPACES) > 0 {
                    let left = d.local8(SPACES) - 1;
                    d.set_local8(SPACES, left);
                    return print(d, b' ');
                }
            }
            _ => {}
        }
        let done = d.local16(local::DONE);
        let byte = if fast {
            if done > 0 {
                return;
            }
            d.local8(FAST_BYTE)
        } else {
            if done >= d.get16(field::COUNT) {
                return d.finish(None);
            }
            let buffer = d.get_far(field::ADDRESS).add(done);
            d.g.read8(buffer.segment, buffer.offset)
        };
        let act = feed(d, byte);
        if !matches!(act, Act::Again) {
            d.set_local16(local::DONE, done + 1);
        }
        if perform(d, act) {
            return;
        }
    }
}

/// What a byte asks the screen for, once the parser has taken it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Nothing yet.
    None,
    /// The escape is shown and the byte after it taken again.
    Again,
    /// A byte to show.
    Show(u8),
    /// A sequence ended with `final`.
    Sequence(u8),
}

/// Feeds `byte` to the parser.
fn feed(d: &mut Driver<'_, '_>, byte: u8) -> Act {
    match d.own8(at::STATE) {
        state::ESCAPE if byte == b'[' => {
            d.set_own8(at::STATE, state::SEQUENCE);
            for field in [at::COUNT, at::MARK, at::BYTES_LEN, at::NUMBER] {
                d.set_own8(field, 0);
            }
            let segment = d.instance();
            d.g.write_bytes(segment, at::PARAMS, &[0; 2 * PARAMS as usize]);
            Act::None
        }
        state::ESCAPE => {
            d.set_own8(at::STATE, state::PLAIN);
            Act::Again
        }
        state::SEQUENCE => sequence(d, byte),
        state::QUOTED => {
            if byte == b'"' {
                d.set_own8(at::STATE, state::SEQUENCE);
            } else {
                push_byte(d, byte);
            }
            Act::None
        }
        _ if byte == ESC => {
            d.set_own8(at::STATE, state::ESCAPE);
            Act::None
        }
        _ => Act::Show(byte),
    }
}

/// A byte inside a sequence: a digit, a separator, a string, a mark, or the
/// final byte that ends it.
fn sequence(d: &mut Driver<'_, '_>, byte: u8) -> Act {
    let count = d.own8(at::COUNT);
    match byte {
        b'0'..=b'9' => {
            if count < PARAMS {
                let at = at::PARAMS + u16::from(count) * 2;
                let value = d
                    .own16(at)
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
                d.set_own16(at, value);
            }
            d.set_own8(at::NUMBER, 1);
            Act::None
        }
        b';' => {
            end_number(d, true);
            Act::None
        }
        b'"' => {
            end_number(d, false);
            d.set_own8(at::STATE, state::QUOTED);
            Act::None
        }
        b'=' | b'?' if count == 0 && d.own8(at::NUMBER) == 0 => {
            d.set_own8(at::MARK, byte);
            Act::None
        }
        b'@'..=b'~' => {
            end_number(d, false);
            d.set_own8(at::STATE, state::PLAIN);
            Act::Sequence(byte)
        }
        _ => {
            d.set_own8(at::STATE, state::PLAIN);
            Act::Show(byte)
        }
    }
}

/// Ends the number being read, if one is, as a parameter; `empty` makes a
/// zero of a separator with none before it.
fn end_number(d: &mut Driver<'_, '_>, empty: bool) {
    let count = d.own8(at::COUNT);
    if d.own8(at::NUMBER) == 0 && !empty {
        return;
    }
    if count < PARAMS {
        let value = d.own16(at::PARAMS + u16::from(count) * 2);
        push_byte(d, value.to_le_bytes()[0]);
        d.set_own8(at::COUNT, count + 1);
    }
    d.set_own8(at::NUMBER, 0);
}

fn push_byte(d: &mut Driver<'_, '_>, byte: u8) {
    let len = d.own8(at::BYTES_LEN);
    if len < BYTES_ROOM {
        d.set_own8(at::BYTES + u16::from(len), byte);
        d.set_own8(at::BYTES_LEN, len + 1);
    }
}

/// Parameter `n`, or `default` where it is missing or zero.
fn param(d: &mut Driver<'_, '_>, n: u8, default: u16) -> u16 {
    if n >= d.own8(at::COUNT) {
        return default;
    }
    match d.own16(at::PARAMS + u16::from(n) * 2) {
        0 => default,
        value => value,
    }
}

/// Carries `act` out: `true` when a BIOS call is under way.
fn perform(d: &mut Driver<'_, '_>, act: Act) -> bool {
    let here = cursor(d);
    match act {
        Act::None => false,
        Act::Again => {
            print(d, ESC);
            true
        }
        Act::Show(byte) => show(d, here, byte),
        Act::Sequence(last) => command_of(d, here, last),
    }
}

/// Shows `byte`: the controls a teletype obeys, a tab as spaces to the
/// next eighth column, every other byte in the rendition.
fn show(d: &mut Driver<'_, '_>, here: Cursor, byte: u8) -> bool {
    match byte {
        b'\r' => move_to(d, here, here.row, 0),
        b'\n' => line_feed(d, here, here.column),
        0x08 => move_to(d, here, here.row, here.column.saturating_sub(1)),
        0x07 => {
            d.g.set_r16(Reg16::Ax, 0x0E07);
            d.g.set_r8(Reg8::Bh, here.page);
            d.set_phase(step::DONE);
            d.bios(0x10);
        }
        b'\t' => {
            let spaces = 7 - here.column % 8;
            d.set_local8(SPACES, spaces);
            print(d, b' ');
        }
        _ => print(d, byte),
    }
    true
}

/// Writes `byte` at the cursor in the rendition; the cursor moves on once
/// it is written.
fn print(d: &mut Driver<'_, '_>, byte: u8) {
    let here = cursor(d);
    let attribute = d.own8(at::ATTRIBUTE);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([byte, 0x09]));
    d.g.set_r16(Reg16::Bx, u16::from_le_bytes([attribute, here.page]));
    d.g.set_r16(Reg16::Cx, 1);
    d.set_phase(step::WROTE);
    d.bios(0x10);
}

/// Moves the cursor to `row`, `column`.
fn move_to(d: &mut Driver<'_, '_>, here: Cursor, row: u8, column: u8) {
    d.g.set_r16(Reg16::Ax, 0x0200);
    d.g.set_r8(Reg8::Bh, here.page);
    d.g.set_r16(
        Reg16::Dx,
        u16::from_le_bytes([column.min(here.columns - 1), row.min(here.rows - 1)]),
    );
    d.set_phase(step::MOVED);
    d.bios(0x10);
}

/// Moves down a line to `column`, the screen scrolled up a line in the
/// rendition from its last.
fn line_feed(d: &mut Driver<'_, '_>, here: Cursor, column: u8) {
    if here.row + 1 < here.rows {
        return move_to(d, here, here.row + 1, column);
    }
    d.set_local16(TARGET, u16::from_le_bytes([here.row, column]));
    scroll(d, here, 1, step::SCROLLED);
}

/// Scrolls the whole screen up `lines`, `0` clearing it.
fn scroll(d: &mut Driver<'_, '_>, here: Cursor, lines: u8, next: u8) {
    let attribute = d.own8(at::ATTRIBUTE);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([lines, 0x06]));
    d.g.set_r8(Reg8::Bh, attribute);
    d.g.set_r16(Reg16::Cx, 0);
    d.g.set_r16(
        Reg16::Dx,
        u16::from_le_bytes([here.columns - 1, here.rows - 1]),
    );
    d.set_phase(next);
    d.bios(0x10);
}

/// A sequence ended with `last`: `true` when a BIOS call is under way.
fn command_of(d: &mut Driver<'_, '_>, here: Cursor, last: u8) -> bool {
    let first = param(d, 0, 1);
    let step = |n: u16| u8::try_from(n).unwrap_or(u8::MAX);
    match last {
        b'A' => {
            move_to(d, here, here.row.saturating_sub(step(first)), here.column);
            true
        }
        b'B' => {
            move_to(d, here, here.row.saturating_add(step(first)), here.column);
            true
        }
        b'C' => {
            move_to(d, here, here.row, here.column.saturating_add(step(first)));
            true
        }
        b'D' => {
            move_to(d, here, here.row, here.column.saturating_sub(step(first)));
            true
        }
        b'H' | b'f' => {
            let column = param(d, 1, 1);
            move_to(
                d,
                here,
                step(first).saturating_sub(1),
                step(column).saturating_sub(1),
            );
            true
        }
        b's' => {
            d.set_own8(at::SAVED, here.row);
            d.set_own8(at::SAVED + 1, here.column);
            false
        }
        b'u' => {
            let (row, column) = (d.own8(at::SAVED), d.own8(at::SAVED + 1));
            move_to(d, here, row, column);
            true
        }
        b'J' if param(d, 0, 0) == 2 => {
            d.set_local16(TARGET, 0);
            scroll(d, here, 0, step::SCROLLED);
            true
        }
        b'K' => {
            let attribute = d.own8(at::ATTRIBUTE);
            d.g.set_r16(Reg16::Ax, 0x0920);
            d.g.set_r16(Reg16::Bx, u16::from_le_bytes([attribute, here.page]));
            d.g.set_r16(Reg16::Cx, u16::from(here.columns - here.column));
            d.set_phase(step::DONE);
            d.bios(0x10);
            true
        }
        b'm' => {
            rendition(d);
            false
        }
        b'h' | b'l' => mode(d, last == b'h'),
        b'n' if param(d, 0, 0) == 6 => {
            report(d, here);
            false
        }
        b'p' => {
            reassign(d);
            false
        }
        _ => false,
    }
}

/// `m`: each parameter changes the rendition, as a colour display shows it.
fn rendition(d: &mut Driver<'_, '_>) {
    // ANSI's colour order against the PC's: red is the PC's 4, blue its 1.
    const COLOURS: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
    let mut attribute = d.own8(at::ATTRIBUTE);
    let count = d.own8(at::COUNT).max(1);
    for n in 0..count {
        let value = if n < d.own8(at::COUNT) {
            d.own16(at::PARAMS + u16::from(n) * 2)
        } else {
            0
        };
        attribute = match value {
            0 => 0x07,
            1 => attribute | 0x08,
            4 => (attribute & 0x88) | 0x01,
            5 => attribute | 0x80,
            7 => (attribute & 0x88) | 0x70,
            8 => (attribute & 0xF0) | (attribute >> 4 & 0x07),
            30..=37 => (attribute & 0xF8) | COLOURS[usize::from(value - 30)],
            40..=47 => (attribute & 0x8F) | COLOURS[usize::from(value - 40)] << 4,
            _ => attribute,
        };
    }
    d.set_own8(at::ATTRIBUTE, attribute);
}

/// `h` and `l`: `7` turns wrap on or off; set, a number a screen mode the
/// BIOS takes changes to it.
fn mode(d: &mut Driver<'_, '_>, set: bool) -> bool {
    let value = param(d, 0, 0);
    if value == 7 {
        d.set_own8(at::WRAP, u8::from(set));
        return false;
    }
    if !set || !matches!(value, 0..=6 | 13..=19) {
        return false;
    }
    d.g.set_r16(Reg16::Ax, u16::from(u8::try_from(value).unwrap_or(0)));
    d.set_phase(step::DONE);
    d.bios(0x10);
    true
}

/// `6n`: the cursor's place queued for input, as `ESC[row;columnR`.
fn report(d: &mut Driver<'_, '_>, here: Cursor) {
    let digits = |n: u8| [b'0' + n / 10 % 10, b'0' + n % 10];
    let [row_tens, row_units] = digits(here.row + 1);
    let [column_tens, column_units] = digits(here.column + 1);
    queue(
        d,
        &[
            ESC,
            b'[',
            row_tens,
            row_units,
            b';',
            column_tens,
            column_units,
            b'R',
            b'\r',
        ],
    );
}

/// Queues `bytes` for the next reads, after what is queued already; what
/// passes the queue's room is dropped.
fn queue(d: &mut Driver<'_, '_>, bytes: &[u8]) {
    let segment = d.instance();
    let len = d.own8(at::QUEUE_LEN).min(QUEUE_ROOM);
    let next = d.own8(at::QUEUE_AT).min(len);
    let kept = usize::from(len - next);
    let mut held = [0u8; QUEUE_ROOM as usize];
    d.g.read_bytes(segment, at::QUEUE + u16::from(next), &mut held[..kept]);
    let added = bytes.len().min(held.len() - kept);
    held[kept..kept + added].copy_from_slice(&bytes[..added]);
    d.g.write_bytes(segment, at::QUEUE, &held[..kept + added]);
    d.set_own8(
        at::QUEUE_LEN,
        u8::try_from(kept + added).unwrap_or(QUEUE_ROOM),
    );
    d.set_own8(at::QUEUE_AT, 0);
}

/// The next queued byte, taken when `take`.
fn queued(d: &mut Driver<'_, '_>, take: bool) -> Option<u8> {
    let len = d.own8(at::QUEUE_LEN).min(QUEUE_ROOM);
    let next = d.own8(at::QUEUE_AT);
    if next >= len {
        return None;
    }
    let byte = d.own8(at::QUEUE + u16::from(next));
    if take {
        d.set_own8(at::QUEUE_AT, next + 1);
    }
    Some(byte)
}

/// `p`: the key the first parameter — `0;n` an extended key's — names now
/// types the rest; given no rest, or itself, it types itself again.
fn reassign(d: &mut Driver<'_, '_>) {
    let segment = d.instance();
    let len = usize::from(d.own8(at::BYTES_LEN).min(BYTES_ROOM));
    let mut taken = [0u8; BYTES_ROOM as usize];
    let taken = &mut taken[..len];
    d.g.read_bytes(segment, at::BYTES, taken);
    let (code, typed) = match &*taken {
        [0, scan, rest @ ..] => ([0, *scan], rest),
        [key, rest @ ..] => ([*key, 0], rest),
        [] => return,
    };
    let mut keys = Keys::read(d);
    keys.remove(code);
    if !(typed.is_empty() || (code[1] == 0 && typed == [code[0]])) {
        keys.add(code, typed);
    }
    keys.write(d);
}

/// The reassignment table: entries of a key's code, the length of what it
/// types, then that.
struct Keys {
    bytes: [u8; KEYS_ROOM as usize],
    len: usize,
}

impl Keys {
    fn read(d: &mut Driver<'_, '_>) -> Self {
        let segment = d.instance();
        let len = usize::from(d.own16(at::KEYS_LEN).min(KEYS_ROOM));
        let mut bytes = [0u8; KEYS_ROOM as usize];
        d.g.read_bytes(segment, at::KEYS, &mut bytes[..len]);
        Self { bytes, len }
    }

    fn write(&self, d: &mut Driver<'_, '_>) {
        let segment = d.instance();
        d.g.write_bytes(segment, at::KEYS, &self.bytes[..self.len]);
        d.set_own16(at::KEYS_LEN, u16::try_from(self.len).unwrap_or(0));
    }

    /// Where `code`'s entry starts and ends.
    fn find(&self, code: [u8; 2]) -> Option<(usize, usize)> {
        let table = &self.bytes[..self.len];
        let mut at = 0;
        while at + 3 <= table.len() {
            let end = (at + 3 + usize::from(table[at + 2])).min(table.len());
            if table[at..at + 2] == code {
                return Some((at, end));
            }
            at = end;
        }
        None
    }

    /// What `code` types, when it has been reassigned.
    fn typed(&self, code: [u8; 2]) -> Option<&[u8]> {
        self.find(code)
            .map(|(start, end)| &self.bytes[start + 3..end])
    }

    fn remove(&mut self, code: [u8; 2]) {
        if let Some((start, end)) = self.find(code) {
            self.bytes.copy_within(end..self.len, start);
            self.len -= end - start;
        }
    }

    /// Adds `code` typing `typed`, when the table has room for it.
    fn add(&mut self, code: [u8; 2], typed: &[u8]) {
        let typed = &typed[..typed.len().min(usize::from(u8::MAX))];
        let end = self.len + 3 + typed.len();
        if end > self.bytes.len() {
            return;
        }
        self.bytes[self.len..self.len + 2].copy_from_slice(&code);
        self.bytes[self.len + 2] = u8::try_from(typed.len()).unwrap_or(u8::MAX);
        self.bytes[self.len + 3..end].copy_from_slice(typed);
        self.len = end;
    }
}

/// `INPUT`: queued bytes first; then keys from INT 16h, an extended one as
/// zero and its scan code, a reassigned one as what it types, Ctrl+Break —
/// the BIOS's zero word — as Ctrl+C.
fn input(d: &mut Driver<'_, '_>) {
    let count = d.get16(field::COUNT);
    let buffer = d.get_far(field::ADDRESS);
    if d.phase() == step::KEY {
        let pressed = d.g.r16(Reg16::Ax);
        let [code, scan] = pressed.to_le_bytes();
        let key = if code == 0 || code == 0xE0 {
            [0, scan]
        } else {
            [code, 0]
        };
        let keys = Keys::read(d);
        match keys.typed(key) {
            _ if pressed == 0 => queue(d, &[crate::dos::char::CTRL_C]),
            Some(typed) => queue(d, typed),
            None if key[0] == 0 => queue(d, &[0, scan]),
            None => queue(d, &[code]),
        }
        d.set_phase(0);
    }
    loop {
        let done = d.local16(local::DONE);
        if done >= count {
            return d.finish(None);
        }
        let Some(byte) = queued(d, true) else {
            d.g.set_r8(Reg8::Ah, 0x00);
            d.set_phase(step::KEY);
            d.bios(0x16);
            return;
        };
        let at = buffer.add(done);
        d.g.write8(at.segment, at.offset, byte);
        d.set_local16(local::DONE, done + 1);
    }
}

/// `PEEK` when `peek`, else `INPUT_STATUS`: busy while nothing is queued
/// and no key waits.
fn look(d: &mut Driver<'_, '_>, peek: bool) {
    if d.phase() == step::LOOKED {
        if d.g.regs().flag(flag::ZF) {
            return d.finish_busy(true);
        }
        if peek {
            let [code, _] = d.g.r16(Reg16::Ax).to_le_bytes();
            d.set8(field::MEDIA, code);
        }
        return d.finish_busy(false);
    }
    if let Some(byte) = queued(d, false) {
        if peek {
            d.set8(field::MEDIA, byte);
        }
        return d.finish_busy(false);
    }
    d.g.set_r8(Reg8::Ah, 0x01);
    d.set_phase(step::LOOKED);
    d.bios(0x16);
}

/// `INPUT_FLUSH`: what is queued dropped, and every key waiting.
fn flush(d: &mut Driver<'_, '_>) {
    match d.phase() {
        step::LOOKED if !d.g.regs().flag(flag::ZF) => {
            d.g.set_r8(Reg8::Ah, 0x00);
            d.set_phase(step::FLUSHING);
            d.bios(0x16);
        }
        step::LOOKED => d.finish(None),
        _ => {
            d.set_own8(at::QUEUE_LEN, 0);
            d.set_own8(at::QUEUE_AT, 0);
            d.g.set_r8(Reg8::Ah, 0x01);
            d.set_phase(step::LOOKED);
            d.bios(0x16);
        }
    }
}
