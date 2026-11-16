//! The KERNAL from outside: a BASIC ROM made with the builder drives it
//! through its entry points, keys are pressed on the matrix, and each test
//! reads the machine back.

use alloc::vec::Vec;

use super::entry;
use super::ws::{page, zp};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::memory::{BASIC_BASE, BASIC_BYTES};
use crate::machine::c64::{Machine, Standard};
use crate::rom::asm::{abs, byte, imm, Asm};
use crate::rom::c64::system;
use crate::rom::c64::testing::{key_for, press, run_for, JIFFY};

mod editor;
mod files;
mod keyboard;
mod tape;

/// Where a test program marks that it has run to its end.
const DONE: u16 = 0xC000;
const DONE_MARK: u8 = 0x5A;
/// Where the input loop keeps what CHRIN returned, and its count.
const INPUT: u16 = 0xC100;
const INPUT_COUNT: u16 = 0xC0FF;

/// A BASIC ROM whose cold start runs `body`; the test's tables go where
/// `Asm::later` holds them.
fn basic_rom(body: impl FnOnce(&mut Asm)) -> Vec<u8> {
    let mut a = Asm::new(BASIC_BASE, BASIC_BYTES);
    a.word("cold");
    a.word("warm");
    a.here("cold");
    body(&mut a);
    a.here("warm");
    a.jmp("warm");
    a.place_later();
    a.finish().expect("the test BASIC assembles").bytes
}

/// A BASIC ROM that runs `body`, marks [`DONE`] and halts.
fn program(body: impl FnOnce(&mut Asm)) -> Vec<u8> {
    basic_rom(|a| {
        body(a);
        a.lda(imm(DONE_MARK));
        a.sta(abs(DONE));
        a.here("halt");
        a.jmp("halt");
    })
}

/// A BASIC ROM that prints `text`, then keeps what CHRIN returns at
/// [`INPUT`].
fn print_then_input(text: &'static [u8]) -> Vec<u8> {
    basic_rom(|a| {
        a.ldx(imm(0));
        a.stx(abs(INPUT_COUNT));
        if !text.is_empty() {
            a.here("print");
            a.lda(abs("text").x());
            a.jsr(entry::CHROUT);
            a.inx();
            a.cpx(imm(byte(text.len())));
            a.bne("print");
            a.later(move |a| {
                a.here("text");
                a.bytes(text);
            });
        }
        a.here("input");
        a.jsr(entry::CHRIN);
        a.ldx(abs(INPUT_COUNT));
        a.sta(abs(INPUT).x());
        a.inc(abs(INPUT_COUNT));
        a.jmp("input");
    })
}

fn machine(standard: Standard, basic: &[u8]) -> Machine {
    let system = system().expect("the BASIC and KERNAL ROMs assemble");
    crate::rom::c64::testing::machine(standard, basic, system.kernal())
}

/// A machine run four seconds from power-on, as long as the original's
/// reset takes.
fn settled(standard: Standard, basic: &[u8]) -> Machine {
    let mut machine = machine(standard, basic);
    machine.run_until(4 * standard.clock());
    machine
}

/// Run until the program marks [`DONE`], within `seconds`.
fn run(machine: &mut Machine, seconds: u64) {
    let clock = Standard::Pal.clock();
    let end = machine.cycles() + seconds * clock;
    while machine.ram(DONE) != DONE_MARK {
        assert!(
            machine.cycles() < end,
            "the program finished, at {:04X}",
            machine.cpu().pc()
        );
        run_for(machine, clock / 50);
    }
}

/// `keys` typed, ten jiffies then let pass: letters, digits and the space
/// as themselves, `|` RETURN, `#` SHIFT-RETURN, `"` the quote, `>` and `<`
/// the cursor right and left, `v` and `^` down and up, `~` DEL, `+` INST,
/// `h` HOME, `c` CLR, `r` and `o` RVS ON and OFF, `g` the Commodore key
/// with A and `s` SHIFT with A.
fn type_keys(machine: &mut Machine, keys: &str) {
    for character in keys.chars() {
        let (held, key): (&[Key], Key) = match character {
            'A'..='Z' | '0'..='9' | ' ' => match key_for(character) {
                Some((_, key)) => (&[], key),
                None => panic!("no key for {character}"),
            },
            '|' => (&[], Key::RETURN),
            '#' => (&[Key::LEFT_SHIFT], Key::RETURN),
            '"' => (&[Key::LEFT_SHIFT], Key::DIGIT_2),
            '>' => (&[], Key::CURSOR_RIGHT),
            '<' => (&[Key::LEFT_SHIFT], Key::CURSOR_RIGHT),
            'v' => (&[], Key::CURSOR_DOWN),
            '^' => (&[Key::LEFT_SHIFT], Key::CURSOR_DOWN),
            '~' => (&[], Key::DELETE),
            '+' => (&[Key::LEFT_SHIFT], Key::DELETE),
            'h' => (&[], Key::HOME),
            'c' => (&[Key::LEFT_SHIFT], Key::HOME),
            'r' => (&[Key::CONTROL], Key::DIGIT_9),
            'o' => (&[Key::CONTROL], Key::DIGIT_0),
            'g' => (&[Key::COMMODORE], Key::A),
            's' => (&[Key::LEFT_SHIFT], Key::A),
            _ => panic!("no key for {character}"),
        };
        press(machine, held, key);
    }
    run_for(machine, 10 * JIFFY);
}

/// What a test compares: the screen with the cursor's cell unreversed, the
/// rows continuing a line, the cursor, the line's last column, what CHRIN
/// returned, and QTSW, RVS, INSRT, COLOR and NDX.
struct Seen {
    screen: Vec<u8>,
    continuations: Vec<usize>,
    cursor: (u8, u8),
    line_end: u8,
    input: Vec<u8>,
    modes: [u8; 5],
}

fn state(machine: &Machine) -> Seen {
    let mut screen: Vec<u8> = (0x0400..0x07E8).map(|at| machine.ram(at)).collect();
    let line = u16::from_le_bytes([machine.ram(zp::PNT), machine.ram(zp::PNT + 1)]);
    let cursor = line.wrapping_add(u16::from(machine.ram(zp::PNTR)));
    if let Some(cell) = cursor
        .checked_sub(0x0400)
        .map(usize::from)
        .filter(|&cell| cell < screen.len())
    {
        screen[cell] &= 0x7F;
    }
    Seen {
        screen,
        continuations: (0..25)
            .filter(|&row| machine.ram(zp::LDTB1 + row) & 0x80 == 0)
            .map(usize::from)
            .collect(),
        cursor: (machine.ram(zp::TBLX), machine.ram(zp::PNTR)),
        line_end: machine.ram(zp::LNMX),
        input: (0..machine.ram(INPUT_COUNT))
            .map(|i| machine.ram(INPUT + u16::from(i)))
            .collect(),
        modes: [
            machine.ram(zp::QTSW),
            machine.ram(zp::RVS),
            machine.ram(zp::INSRT),
            machine.ram(page::COLOR),
            machine.ram(zp::NDX),
        ],
    }
}

#[test]
fn the_reset_sets_memory_up_and_starts_basic() {
    let rom = program(|a| {
        for (at, from) in [
            (0xC001, 0xD020u16),
            (0xC002, 0xD021),
            (0xC003, page::TVSFLG),
        ] {
            a.lda(abs(from));
            a.sta(abs(at));
        }
    });
    let mut machine = machine(Standard::Pal, &rom);
    run(&mut machine, 3);
    assert_eq!(
        machine.ram(page::MEMSIZ + 1),
        0xA0,
        "the top of memory is BASIC's ROM"
    );
    assert_eq!(machine.ram(page::MEMSTR + 1), 0x08, "the bottom of memory");
    assert_eq!(machine.ram(page::HIBASE), 0x04, "the screen's page");
    assert_eq!(machine.ram(0xC001) & 0x0F, 14, "a light blue border");
    assert_eq!(machine.ram(0xC002) & 0x0F, 6, "on blue");
    assert_eq!(machine.ram(0xC003), 1, "a PAL machine");
    assert_eq!(machine.ram(0x0400), 0x20, "the screen cleared");
    assert_eq!(
        (0..16)
            .map(|v| u16::from_le_bytes([machine.ram(0x0314 + 2 * v), machine.ram(0x0315 + 2 * v)]))
            .collect::<Vec<_>>(),
        [
            0xEA31, 0xFE66, 0xFE47, 0xF34A, 0xF291, 0xF20E, 0xF250, 0xF333, 0xF157, 0xF1CA, 0xF6ED,
            0xF13E, 0xF32F, 0xFE66, 0xF4A5, 0xF5ED
        ],
        "the vectors at the original's addresses"
    );
}

#[test]
fn an_ntsc_machine_is_told_apart() {
    let rom = program(|a| {
        a.lda(abs(page::TVSFLG));
        a.sta(abs(0xC001));
    });
    let mut machine = machine(Standard::Ntsc, &rom);
    run(&mut machine, 3);
    assert_eq!(machine.ram(0xC001), 0, "an NTSC machine");
}

#[test]
fn the_jiffy_clock_counts_sixtieths() {
    let mut machine = settled(Standard::Pal, &print_then_input(b""));
    let clock = |machine: &Machine| {
        u32::from_be_bytes([
            0,
            machine.ram(zp::TIME),
            machine.ram(zp::TIME + 1),
            machine.ram(zp::TIME + 2),
        ])
    };
    let before = clock(&machine);
    run_for(&mut machine, 10 * Standard::Pal.clock());
    assert!(
        (599..=600).contains(&(clock(&machine) - before)),
        "ten seconds"
    );
}
