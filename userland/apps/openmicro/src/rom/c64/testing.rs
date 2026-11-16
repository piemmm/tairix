//! What the C64 ROMs' tests share: a machine with a BASIC ROM, a KERNAL
//! and the character ROM, time run on, and keys pressed as a typist does.

use alloc::string::String;
use alloc::vec::Vec;

use crate::machine::c64::keyboard::Key;
use crate::machine::c64::{sid, Config, Machine, Roms, Standard};
use crate::rom::c64::charset;

/// A jiffy on a PAL machine, in cycles.
pub const JIFFY: u64 = 16421;

/// A machine with `basic` and `kernal`.
pub fn machine(standard: Standard, basic: &[u8], kernal: &[u8]) -> Machine {
    let character = charset::build().expect("the character ROM builds").bytes;
    Machine::new(
        Config {
            standard,
            sid: sid::Model::Mos6581,
        },
        Roms {
            basic,
            kernal,
            character: &character,
        },
    )
    .expect("the images fit")
}

/// The machine run `cycles` more.
pub fn run_for(machine: &mut Machine, cycles: u64) {
    let until = machine.cycles() + cycles;
    machine.run_until(until);
}

/// `key` pressed with `held`, as a typist does: the shift keys first, the
/// key for three jiffies.
pub fn press(machine: &mut Machine, held: &[Key], key: Key) {
    for &shift in held {
        machine.set_key(shift, true);
    }
    run_for(machine, 2 * JIFFY);
    machine.set_key(key, true);
    run_for(machine, 3 * JIFFY);
    machine.set_key(key, false);
    run_for(machine, 2 * JIFFY);
    for &shift in held {
        machine.set_key(shift, false);
    }
    run_for(machine, 3 * JIFFY);
}

/// The key that types `character` as the upper-case set shows it, and
/// whether SHIFT goes with it.
pub fn key_for(character: char) -> Option<(bool, Key)> {
    Key::typing(character).map(|(key, shifted)| (shifted, key))
}

/// `text` typed fast: each key and its SHIFT down for two jiffies, then up
/// for two, which the scan sees as a fresh press each time.
pub fn type_text(machine: &mut Machine, text: &str) {
    for character in text.chars() {
        let (shifted, key) =
            key_for(character).unwrap_or_else(|| panic!("no key types {character:?}"));
        machine.set_key(Key::LEFT_SHIFT, shifted);
        machine.set_key(key, true);
        run_for(machine, 2 * JIFFY);
        machine.set_key(key, false);
        machine.set_key(Key::LEFT_SHIFT, false);
        run_for(machine, 2 * JIFFY);
    }
}

/// The screen's rows from the top to the last holding text, each as the
/// upper-case set shows it, trailing spaces dropped and the cursor's cell
/// unreversed.
pub fn screen_rows(machine: &Machine) -> Vec<String> {
    let rows: Vec<String> = (0..25u16)
        .map(|row| {
            let text: String = (0..40u16)
                .map(|column| shown(machine.ram(0x0400 + row * 40 + column)))
                .collect();
            String::from(text.trim_end())
        })
        .collect();
    let used = rows
        .iter()
        .rposition(|row| !row.is_empty())
        .map_or(0, |last| last + 1);
    rows.into_iter().take(used).collect()
}

/// A screen code as the character it shows in the upper-case set; the
/// graphics as `~`.
fn shown(code: u8) -> char {
    match code & 0x7F {
        code @ 0x00..=0x1F => char::from(code + 0x40),
        code @ 0x20..=0x3F => char::from(code),
        _ => '~',
    }
}
