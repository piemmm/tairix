//! The KERNAL from outside: a BASIC ROM made with the builder drives it
//! through its entry points, keys are pressed on the matrix, and each test
//! reads the machine back.

use alloc::vec::Vec;

use super::ws::{page, vector, zp};
use super::{entry, fixed};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::memory::{BASIC_BASE, BASIC_BYTES};
use crate::machine::c64::{Machine, Standard};
use crate::rom::asm::{abs, byte, imm, zp as z, Asm};
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
    const TABLE_COPY: u16 = 0xC100;
    let rom = program(|a| {
        for (at, from) in [
            (0xC001, 0xD020u16),
            (0xC002, 0xD021),
            (0xC003, page::TVSFLG),
        ] {
            a.lda(abs(from));
            a.sta(abs(at));
        }
        // Software restores the vectors from RESTOR's own table.
        a.ldx(imm(vector::BYTES - 1));
        a.here("copy_table");
        a.lda(abs(fixed::VECTOR_TABLE).x());
        a.sta(abs(TABLE_COPY).x());
        a.dex();
        a.bpl("copy_table");
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
    let vectors = |from: u16| {
        (0..16)
            .map(|v| u16::from_le_bytes([machine.ram(from + 2 * v), machine.ram(from + 1 + 2 * v)]))
            .collect::<Vec<_>>()
    };
    let defaults = [
        0xEA31, 0xFE66, 0xFE47, 0xF34A, 0xF291, 0xF20E, 0xF250, 0xF333, 0xF157, 0xF1CA, 0xF6ED,
        0xF13E, 0xF32F, 0xFE66, 0xF4A5, 0xF5ED,
    ];
    assert_eq!(
        vectors(vector::CINV),
        defaults,
        "the vectors at the original's addresses"
    );
    assert_eq!(
        vectors(TABLE_COPY),
        defaults,
        "RESTOR's table where the original's lies"
    );
}

#[test]
fn the_screen_set_up_s_three_entries_do_what_the_original_s_do() {
    const JUNK: u8 = 0x55;
    const SEEN: u16 = 0xC010;
    let settings = [
        page::XMAX,
        page::KOUNT,
        page::DELAY,
        page::KEYLOG,
        page::KEYLOG + 1,
        page::MODE,
    ];
    for entry in [fixed::SCREEN_INIT, fixed::EDITOR_INIT, fixed::CURSOR_INIT] {
        let rom = program(move |a| {
            a.sei();
            a.lda(imm(JUNK));
            for at in settings.into_iter().chain([page::COLOR, 0x0400, 0xD020]) {
                a.sta(abs(at));
            }
            a.sta(abs(zp::BLNON));
            a.sta(abs(zp::BLNCT));
            a.lda(imm(0x80));
            a.jsr(entry);
            a.lda(abs(zp::BLNCT));
            a.sta(abs(SEEN));
            a.lda(abs(zp::BLNSW));
            a.sta(abs(SEEN + 1));
            a.lda(abs(0xD020));
            a.sta(abs(SEEN + 2));
            a.cli();
        });
        let mut machine = machine(Standard::Pal, &rom);
        run(&mut machine, 3);
        let whole = entry == fixed::SCREEN_INIT;
        let from_settings = entry != fixed::CURSOR_INIT;
        assert_eq!(
            machine.ram(SEEN + 2) & 0x0F,
            if whole { 14 } else { JUNK & 0x0F },
            "{entry:04X}: the VIC-II's registers"
        );
        assert_eq!(
            settings.map(|at| machine.ram(at)),
            if from_settings {
                [10, 4, 10, 0x48, 0xEB, 0]
            } else {
                [JUNK; 6]
            },
            "{entry:04X}: the editor's and the keyboard's settings"
        );
        assert_eq!(
            machine.ram(zp::BLNON),
            if from_settings { 0 } else { JUNK },
            "{entry:04X}: the cursor's flash"
        );
        assert_eq!(
            machine.ram(page::COLOR),
            if from_settings { 14 } else { 0x80 },
            "{entry:04X}: the text colour, from A at the third"
        );
        assert_eq!(
            [machine.ram(SEEN), machine.ram(SEEN + 1)],
            [12, 12],
            "{entry:04X}: the cursor's phase"
        );
        assert_eq!(machine.ram(0x0400), 0x20, "{entry:04X}: the screen cleared");
    }
}

#[test]
fn cint_s_standard_part_and_ioinit_keep_x_and_y_and_the_irq_enters_where_the_original_s_does() {
    const SEEN: u16 = 0xC010;
    let kernal = system().expect("the BASIC and KERNAL ROMs assemble");
    let vector = usize::from(crate::cpu::IRQ_VECTOR - crate::machine::c64::memory::KERNAL_BASE);
    assert_eq!(
        kernal.kernal()[vector..vector + 2],
        fixed::IRQ_ENTRY.to_le_bytes(),
        "the IRQ vector"
    );
    let rom = program(|a| {
        a.sei();
        a.lda(imm(0x55));
        a.sta(abs(page::TVSFLG));
        a.ldx(imm(0x11));
        a.ldy(imm(0x22));
        a.jsr(fixed::CINT_STANDARD);
        a.stx(abs(SEEN));
        a.sty(abs(SEEN + 1));
        a.ldx(imm(0x33));
        a.ldy(imm(0x44));
        a.jsr(fixed::IOINIT);
        a.sty(abs(SEEN + 2));
        a.cli();
    });
    let mut machine = machine(Standard::Pal, &rom);
    run(&mut machine, 3);
    assert_eq!(
        [SEEN, SEEN + 1, SEEN + 2].map(|at| machine.ram(at)),
        [0x11, 0x22, 0x44],
        "X and Y kept"
    );
    assert_eq!(machine.ram(page::TVSFLG), 1, "a PAL machine found");
    let ticks = machine.ram(zp::TIME + 2);
    run_for(&mut machine, 10 * JIFFY);
    assert_ne!(
        machine.ram(zp::TIME + 2),
        ticks,
        "the jiffy interrupt started"
    );
}

#[test]
fn the_bodies_software_calls_directly_do_what_their_entries_do() {
    const SEEN: u16 = 0xC010;
    let entry = |name: &str| {
        let index = super::ENTRIES
            .iter()
            .position(|&(named, _)| named == name)
            .expect("an entry of the jump table");
        super::JUMP_TABLE + 3 * u16::try_from(index).expect("a small index")
    };
    let listening = [fixed::LISTEN, fixed::SECOND, fixed::CIOUT, fixed::UNLSN];
    let listening_by_entry = ["LISTEN", "SECOND", "CIOUT", "UNLSN"].map(entry);
    let talking = [fixed::TALK, fixed::TKSA, fixed::UNTLK];
    let talking_by_entry = ["TALK", "TKSA", "UNTLK"].map(entry);
    let rom = program(move |a| {
        a.sei();
        // With no device on the bus, each way to it ends as the other does.
        for (seen, calls) in [
            (SEEN, &listening[..]),
            (SEEN + 1, &listening_by_entry[..]),
            (SEEN + 2, &talking[..]),
            (SEEN + 3, &talking_by_entry[..]),
        ] {
            a.lda(imm(0));
            a.sta(z(zp::STATUS));
            for (&call, value) in calls.iter().zip([8, 0x6F, b'X', 0]) {
                a.lda(imm(value));
                a.jsr(call);
            }
            a.lda(z(zp::STATUS));
            a.sta(abs(seen));
        }
        a.lda(z(zp::TIME + 2));
        a.sta(abs(SEEN + 4));
        a.jsr(fixed::UDTIM);
        a.lda(z(zp::TIME + 2));
        a.sta(abs(SEEN + 5));
        a.lda(imm(super::keyboard::NO_KEY));
        a.sta(z(zp::LSTX));
        a.lda(imm(0));
        a.sta(z(zp::NDX));
        a.jsr(fixed::SCNKEY);
        a.lda(z(zp::NDX));
        a.sta(abs(SEEN + 6));
        a.lda(abs(page::KEYD));
        a.sta(abs(SEEN + 7));
        a.sei();
        a.lda(imm(b'Q'));
        a.jsr(fixed::SCREEN_OUT);
        a.php();
        a.pla();
        a.sta(abs(SEEN + 8));
    });
    let mut machine = machine(Standard::Pal, &rom);
    machine.set_key(Key::A, true);
    run(&mut machine, 3);
    let seen = |at: u16| machine.ram(SEEN + at);
    assert_eq!(seen(0) & 0x80, 0x80, "LISTEN finds no device");
    assert_eq!(seen(0), seen(1), "the bodies listen as the entries do");
    assert_eq!(seen(2) & 0x80, 0x80, "TALK finds no device");
    assert_eq!(seen(2), seen(3), "the bodies talk as the entries do");
    assert_eq!(seen(5), seen(4).wrapping_add(1), "UDTIM counts a jiffy");
    assert_eq!((seen(6), seen(7)), (1, b'A'), "SCNKEY takes the key held");
    assert_eq!(machine.ram(0x0400), 0x11, "Q at the cursor");
    assert_eq!(seen(8) & 0x04, 0, "interrupts on after the screen's output");
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
