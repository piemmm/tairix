//! Scenarios on a Commodore 64 (`plans/C64.md` §6): a PAL machine with a
//! 1541 as device 8, and one as device 9 where a scenario names two disks.
//! A script's steps are those of the Model B's but for the keys: `press:` a
//! key by its label in [`LABELS`], `restore` RESTORE, and `stoprestore` the
//! two together; `swap:` puts a disk into device 8. The only disk a
//! scenario names is `blank`, a 1541's freshly formatted one. The screen is
//! read from `$0400` as the upper-case set shows it, a graphic as `\xC0`
//! on, reversed cells as the rest, as the cursor blinks.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{centiseconds, Observation, Scenario, ScenarioError};
use crate::machine::c64::drive::{Drive, Unit};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::{sid, Config, Machine, Roms as Fitted, Standard};
use crate::media::c1541::{d64, Surface};

/// What the clean-room ROMs show in place of what the originals show, where
/// they show their own name.
pub const OWN_NAMES: [(&str, &str); 2] = [
    ("COMMODORE 64 BASIC V2", "OPENMICRO 64 BASIC V2"),
    ("CBM DOS V2.6 1541", "OPENMICRO DOS 1541"),
];

/// The keys `press:` names, by their labels.
pub const LABELS: [(&str, Key); 14] = [
    ("RETURN", Key::RETURN),
    ("STOP", Key::RUN_STOP),
    ("HOME", Key::HOME),
    ("DEL", Key::DELETE),
    ("SPACE", Key::SPACE),
    ("DOWN", Key::CURSOR_DOWN),
    ("RIGHT", Key::CURSOR_RIGHT),
    ("F1", Key::F1),
    ("F3", Key::F3),
    ("F5", Key::F5),
    ("F7", Key::F7),
    ("SHIFT", Key::LEFT_SHIFT),
    ("CBM", Key::COMMODORE),
    ("CTRL", Key::CONTROL),
];

/// The screen's first character, and its size.
const SCREEN: u16 = 0x0400;
const ROWS: u16 = 25;
const COLUMNS: u16 = 40;
/// The cycles a centisecond lasts on the PAL machine a scenario runs.
const CENTISECOND: u64 = Standard::Pal.clock() / 100;
/// How long a typed key is held, and then left up, in centiseconds: three
/// jiffies' scans each.
const KEY_HELD: u64 = 5;
/// Centiseconds a scenario's machine runs before its first step: the
/// KERNAL's start, the drive's with it.
const STARTED: u64 = 300;
/// Centiseconds RESTORE is held.
const RESTORE_HELD: u64 = 5;

/// The ROM images a scenario's machine runs.
#[derive(Debug, Clone, Copy)]
pub struct Roms<'a> {
    /// The KERNAL.
    pub kernal: &'a [u8],
    /// BASIC.
    pub basic: &'a [u8],
    /// The character generator.
    pub character: &'a [u8],
    /// The 1541's DOS.
    pub dos: &'a [u8],
}

/// Run the machine on `centiseconds`.
fn wait(machine: &mut Machine, centiseconds: u64) {
    machine.run_until(machine.cycles() + centiseconds * CENTISECOND);
}

/// Press `key` with the keys `held` down, letting them all go together.
fn press_with(machine: &mut Machine, key: Key, held: &[Key]) {
    for &modifier in held {
        machine.set_key(modifier, true);
    }
    machine.set_key(key, true);
    wait(machine, KEY_HELD);
    machine.set_key(key, false);
    for &modifier in held {
        machine.set_key(modifier, false);
    }
    wait(machine, KEY_HELD);
}

/// Type `text` a key at a time, a RETURN for each new line.
fn type_text(machine: &mut Machine, text: &str) -> Result<(), ScenarioError> {
    for ch in text.chars() {
        let (key, shifted) =
            Key::typing(ch).ok_or_else(|| ScenarioError::Step(format!("no key types {ch:?}")))?;
        press_with(machine, key, if shifted { &[Key::LEFT_SHIFT] } else { &[] });
    }
    Ok(())
}

/// RESTORE pressed and let go, with RUN STOP held where `stop`.
fn press_restore(machine: &mut Machine, stop: bool) {
    machine.set_key(Key::RUN_STOP, stop);
    machine.set_restore(true);
    wait(machine, RESTORE_HELD);
    machine.set_restore(false);
    machine.set_key(Key::RUN_STOP, false);
    wait(machine, RESTORE_HELD);
}

/// The disk a scenario names.
fn disk(name: &str) -> Result<Surface, ScenarioError> {
    let image = match name {
        "blank" => d64::formatted(b"BLANK", *b"00"),
        _ => return Err(ScenarioError::Disc(String::from(name))),
    };
    d64::decode(&image).map_err(|_| ScenarioError::Disc(String::from(name)))
}

/// The scenario's machine: `roms` in place, a drive for each disk it names
/// with the disk in, run on to its first step.
///
/// # Errors
///
/// [`ScenarioError::Rom`] for a ROM the machine refuses, and
/// [`ScenarioError::Disc`] for a disk no scenario may name.
pub fn machine(roms: Roms<'_>, scenario: &Scenario) -> Result<Machine, ScenarioError> {
    let config = Config {
        standard: Standard::Pal,
        sid: sid::Model::Mos6581,
    };
    let fitted = Fitted {
        basic: roms.basic,
        kernal: roms.kernal,
        character: roms.character,
    };
    let mut machine = Machine::new(config, fitted).map_err(ScenarioError::Rom)?;
    let fitted = scenario.discs.len().max(1);
    for (index, unit) in Unit::ALL.into_iter().enumerate().take(fitted) {
        machine
            .fit_drive(unit, roms.dos)
            .map_err(ScenarioError::Rom)?;
        if let Some(name) = scenario.discs.get(index) {
            let mut surface = disk(name)?;
            surface.write_protected = scenario.protected && unit == Unit::Eight;
            drive(&mut machine, unit)?.insert(surface);
        }
    }
    wait(&mut machine, STARTED);
    Ok(machine)
}

fn drive(machine: &mut Machine, unit: Unit) -> Result<&mut Drive, ScenarioError> {
    machine
        .drive_mut(unit)
        .ok_or_else(|| ScenarioError::Disc(format!("no drive {unit:?}")))
}

/// `scenario` run on `roms`, and what it left.
///
/// # Errors
///
/// As [`machine`], and [`ScenarioError::Step`] for a step the script may not
/// take.
pub fn run(roms: Roms<'_>, scenario: &Scenario) -> Result<Observation, ScenarioError> {
    let mut machine = machine(roms, scenario)?;
    for step in scenario.script.split(';') {
        step_on(&mut machine, step)?;
    }
    let screen = screen_text(&machine)
        .into_iter()
        .filter(|row| !row.is_empty())
        .collect();
    let mut discs = Vec::new();
    for &checked in &scenario.checked {
        let unit = Unit::of(checked);
        let surface = drive(&mut machine, unit)?
            .disk()
            .ok_or_else(|| ScenarioError::Disc(format!("no disk in drive {unit:?}")))?;
        discs.push((checked, tairix_crypto::sha256(&surface.encode().1)));
    }
    Ok(Observation {
        rows: screen,
        discs,
    })
}

fn step_on(machine: &mut Machine, step: &str) -> Result<(), ScenarioError> {
    let (verb, argument) = step.split_once(':').unwrap_or((step, ""));
    match verb {
        "type" => type_text(machine, &argument.replace("\\n", "\n"))?,
        "press" => press_with(machine, key(argument)?, &[]),
        "down" => machine.set_key(key(argument)?, true),
        "up" => machine.set_key(key(argument)?, false),
        "wait" => wait(machine, centiseconds(argument)?),
        "swap" => {
            let surface = disk(argument)?;
            drive(machine, Unit::Eight)?.insert(surface);
        }
        "restore" => press_restore(machine, false),
        "stoprestore" => press_restore(machine, true),
        "" => {}
        _ => return Err(ScenarioError::Step(String::from(step))),
    }
    Ok(())
}

fn key(label: &str) -> Result<Key, ScenarioError> {
    LABELS
        .iter()
        .find(|(named, _)| *named == label)
        .map(|&(_, key)| key)
        .ok_or_else(|| ScenarioError::Step(format!("no key {label}")))
}

/// The screen's rows as the upper-case set shows them, without trailing
/// spaces.
#[must_use]
pub fn screen_text(machine: &Machine) -> Vec<String> {
    (0..ROWS)
        .map(|row| {
            let line: String = (0..COLUMNS)
                .map(|column| shown(machine.ram(SCREEN + row * COLUMNS + column)))
                .collect();
            String::from(line.trim_end_matches(' '))
        })
        .collect()
}

/// A screen code as the upper-case set shows it: letters and `@[£]↑←` as
/// `@` to `_`, the rest of the first half as itself, a graphic from `\xC0`;
/// reversed or not alike, as the cursor blinks.
fn shown(code: u8) -> char {
    match code & 0x7F {
        code @ 0x00..=0x1F => char::from(code + 0x40),
        code @ 0x20..=0x3F => char::from(code),
        code => char::from(code + 0x80),
    }
}

/// `text` with [`OWN_NAMES`] put in place of the originals' names.
#[must_use]
pub fn own_names(text: &str) -> String {
    OWN_NAMES
        .iter()
        .fold(String::from(text), |text, (theirs, ours)| {
            text.replace(theirs, ours)
        })
}
