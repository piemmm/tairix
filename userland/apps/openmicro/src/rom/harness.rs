//! An Electron for the ROMs' tests, its OS and BASIC fitted and, when asked
//! for, the Plus 1 with its ROM: typed at as a user types, and read back off
//! its screen.

use alloc::string::String;
use alloc::vec::Vec;

use crate::machine::electron::keyboard::{Key, Modifier};
use crate::machine::electron::{Machine, PLUS1_ROM};
use crate::machine::CLOCK;
use crate::rom::mos::ws::vdu;

/// Cycles in a centisecond.
pub(crate) const CS: u64 = CLOCK / 100;

/// An Electron at BASIC's prompt, and its OS's image.
pub(crate) fn electron() -> (Machine, Vec<u8>) {
    boot(false)
}

/// An Electron with the Plus 1 and its ROM at BASIC's prompt, and its OS's
/// image.
pub(crate) fn electron_with_plus1() -> (Machine, Vec<u8>) {
    boot(true)
}

fn boot(plus1: bool) -> (Machine, Vec<u8>) {
    let os = crate::rom::mos::electron()
        .expect("the Electron's OS assembles")
        .bytes;
    let basic = crate::rom::basic::build().expect("BASIC assembles").bytes;
    let mut machine = Machine::new(&os).expect("a 16 KiB OS");
    machine.fit_basic(&basic).expect("BASIC fits");
    if plus1 {
        let rom = crate::rom::plus1::build()
            .expect("the Plus 1 ROM assembles")
            .bytes;
        machine.fit_plus1();
        machine
            .insert_rom(PLUS1_ROM, &rom)
            .expect("slot 12 takes the Plus 1's ROM");
    }
    wait(&mut machine, 300);
    (machine, os)
}

/// Run for `centiseconds`.
pub(crate) fn wait(machine: &mut Machine, centiseconds: u64) {
    let until = machine.cycles() + centiseconds * CS;
    machine.run_until(until);
}

/// Hold `keys` together for long enough to be read, then let them go.
pub(crate) fn press(machine: &mut Machine, keys: &[Key]) {
    for &key in keys {
        machine.set_key(key, true);
    }
    wait(machine, 6);
    for &key in keys {
        machine.set_key(key, false);
    }
    wait(machine, 4);
}

/// Type `text` as the keyboard types it with CAPS LOCK on.
pub(crate) fn type_text(machine: &mut Machine, text: &str) {
    for ch in text.chars() {
        let (key, modifier) = Key::typing(ch).expect("a typable character");
        match modifier {
            Modifier::None => press(machine, &[key]),
            Modifier::Shift => press(machine, &[Key::SHIFT, key]),
            Modifier::Ctrl => press(machine, &[Key::CTRL, key]),
        }
    }
}

/// The two-colour modes' text, row by row, each cell matched against the
/// OS's font; a cell matching none reads `?`.
pub(crate) fn screen(machine: &Machine, os: &[u8]) -> Vec<String> {
    let mode = machine.peek(vdu::MODE);
    let start = u16::from_le_bytes([
        machine.peek(vdu::SCREEN_START),
        machine.peek(vdu::SCREEN_START + 1),
    ]);
    let (columns, rows, base) = match mode {
        0 => (80, 32, 0x3000),
        3 => (80, 25, 0x4000),
        4 => (40, 32, 0x5800),
        6 => (40, 25, 0x6000),
        _ => panic!("mode {mode} is not a two-colour mode"),
    };
    let font = &os[..0x300];
    (0..rows)
        .map(|row| {
            let line: String = (0..columns)
                .map(|column| {
                    let cell: Vec<u8> = (0..8)
                        .map(|line| {
                            let mut at = u32::from(start) + (row * columns + column) * 8 + line;
                            if at >= 0x8000 {
                                at = at - 0x8000 + base;
                            }
                            machine.peek(u16::try_from(at).unwrap())
                        })
                        .collect();
                    match font.chunks(8).position(|glyph| glyph == cell) {
                        Some(n) => char::from(b' ' + u8::try_from(n).unwrap()),
                        None => '?',
                    }
                })
                .collect();
            String::from(line.trim_end())
        })
        .collect()
}

/// Whether the screen holds the line `wanted`, the cursor's underline aside.
pub(crate) fn shows(machine: &Machine, os: &[u8], wanted: &str) -> bool {
    screen(machine, os)
        .iter()
        .any(|line| line.trim_end_matches('_') == wanted)
}

/// The last number printed on the screen.
pub(crate) fn last_number(machine: &Machine, os: &[u8]) -> Option<i64> {
    screen(machine, os)
        .iter()
        .rev()
        .find_map(|line| line.trim().trim_end_matches('_').parse().ok())
}
