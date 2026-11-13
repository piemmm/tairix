//! Scenarios: steps typed and keys pressed on a machine, and what its screen
//! and discs then show. The ROMs' tests drive the machine through these
//! steps, and the recorded corpus (`plans/BEEB.md` §7) is a set of scenarios
//! with the observations DFS 1.20, BASIC II and MOS 1.20 gave for them,
//! replayed against the clean-room ROMs.
//!
//! A script is steps separated by `;`: `type:` text, `\n` a RETURN; `press:`
//! a key by its label; `wait:` seconds, to the centisecond; `swap:` a disc
//! [`discs::named`] into drive 0; `break` and `shiftbreak`.
//!
//! A corpus file holds scenarios a block each, separated by blank lines:
//! `scenario <name>`, then `discs <name>...` for drives 0 and 1, `protected`
//! for drive 0 write-protected, `check <drive>` for each drive whose image
//! is observed, `ours` for an observation this machine's ROMs define rather
//! than the originals, and `script <steps>`. A fixtures file holds each
//! scenario's observation: `scenario <name>`, a `row <text>` for each of the
//! screen's rows that is not blank, `\\` and `\xNN` escaping a backslash and
//! a character outside printable ASCII, and `disc <drive> <sha-256>` for each
//! drive checked. `#` starts a comment line in either.

pub mod discs;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::machine::fdc::PhysicalDrive;
use crate::machine::keyboard::Key;
use crate::machine::{Machine, RomError};
use crate::media::disc::{Disc, ImageFormat};

/// 2 MHz cycles in a centisecond.
const CENTISECOND: u64 = 20_000;
/// The teletext screen, and where the VDU drivers keep its first
/// character's address.
const TELETEXT: u16 = 0x7C00;
const TELETEXT_SIZE: u16 = 0x0400;
const SCREEN_START: u16 = 0x0350;
/// How long a typed key is held, and then left up, for the keyboard scan
/// to take it, in centiseconds.
const KEY_HELD: u64 = 3;
/// Cycles BREAK is held, then centiseconds the machine runs on with any
/// key held with it still down, then without.
const BREAK_HELD: u64 = 2_000;
const BREAK_KEYS_HELD: u64 = 50;
const BREAK_SETTLED: u64 = 100;
/// Centiseconds a scenario's machine runs before its first step.
const STARTED: u64 = 100;
/// The sideways slots the corpus machine's BASIC and DFS sit in.
const BASIC_SLOT: usize = 15;
const DFS_SLOT: usize = 14;
/// The longest wait a step may ask for, in centiseconds: ten minutes.
const LONGEST_WAIT: u64 = 60_000;

/// What the clean-room ROMs show in place of what the originals show, where
/// they show their own name: applied to an observation of the originals
/// before it is recorded.
pub const OWN_NAMES: [(&str, &str); 1] = [("Acorn DFS", "TAIRiX DFS")];

/// Why a scenario could not be read or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    /// A corpus or fixtures line, by its number from 1, that is not one.
    Line(usize),
    /// A step no script may take.
    Step(String),
    /// A disc no scenario may name.
    Disc(String),
    /// A scenario's fixture is missing, or a fixture has no scenario.
    Unmatched(String),
    /// The machine refused a ROM image.
    Rom(RomError),
}

/// A scenario from a corpus file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    /// Its name, unique in its file.
    pub name: String,
    /// The discs in drives 0 and 1, as many as given.
    pub discs: Vec<String>,
    /// Whether drive 0's disc is write-protected.
    pub protected: bool,
    /// The drives whose images are observed.
    pub checked: Vec<PhysicalDrive>,
    /// Whether the observation is this machine's own, not the originals'.
    pub ours: bool,
    /// Its steps.
    pub script: String,
}

/// What a scenario left: the screen's rows that are not blank, and the
/// SHA-256 of each checked drive's image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The rows, top first.
    pub rows: Vec<String>,
    /// Each checked drive and its image's digest.
    pub discs: Vec<(PhysicalDrive, [u8; 32])>,
}

/// The ROM images a scenario's machine runs: the OS, BASIC in slot 15 and
/// the DFS in slot 14.
#[derive(Debug, Clone, Copy)]
pub struct Roms<'a> {
    /// The OS.
    pub os: &'a [u8],
    /// BASIC.
    pub basic: &'a [u8],
    /// The DFS.
    pub dfs: &'a [u8],
}

/// Run the machine on `centiseconds`.
pub fn wait(machine: &mut Machine, centiseconds: u64) {
    machine.run_until(machine.cycles() + centiseconds * CENTISECOND);
}

/// Mode 7's text, a line a row from the screen's top left however it has
/// scrolled, without trailing spaces.
#[must_use]
pub fn mode7_text(machine: &Machine) -> Vec<String> {
    let start = u16::from_le_bytes([machine.peek(SCREEN_START), machine.peek(SCREEN_START + 1)]);
    (0..25u16)
        .map(|row| {
            (0..40u16)
                .map(|column| {
                    let offset = (start.wrapping_sub(TELETEXT) + row * 40 + column) % TELETEXT_SIZE;
                    char::from(machine.peek(TELETEXT + offset) & 0x7F)
                })
                .collect::<String>()
        })
        .map(|line| String::from(line.trim_end_matches(' ')))
        .collect()
}

/// Type `text` at the keyboard a key at a time, a carriage return for each
/// new line.
///
/// # Errors
///
/// [`ScenarioError::Step`] for a character no key types.
pub fn type_text(machine: &mut Machine, text: &str) -> Result<(), ScenarioError> {
    for ch in text.chars() {
        let ch = if ch == '\n' { '\r' } else { ch };
        let (key, shifted) =
            Key::typing(ch).ok_or_else(|| ScenarioError::Step(format!("no key types {ch:?}")))?;
        press_with(machine, key, if shifted { &[Key::SHIFT] } else { &[] });
    }
    Ok(())
}

/// Press `key` and let it go, as a typed key is.
pub fn press(machine: &mut Machine, key: Key) {
    press_with(machine, key, &[]);
}

/// Press `key` with the keys `held` down, letting them all go together.
pub fn press_with(machine: &mut Machine, key: Key, held: &[Key]) {
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

/// Press BREAK and let it go with the keys `held` down, as SHIFT is for a
/// boot, the keys let go once the machine has started again.
pub fn press_break(machine: &mut Machine, held: &[Key]) {
    for &key in held {
        machine.set_key(key, true);
    }
    machine.set_break(true);
    machine.run_until(machine.cycles() + BREAK_HELD);
    machine.set_break(false);
    wait(machine, BREAK_KEYS_HELD);
    for &key in held {
        machine.set_key(key, false);
    }
    wait(machine, BREAK_SETTLED);
}

/// The corpus machine: `roms` in place, the scenario's discs in its drives,
/// run on to its first step.
///
/// # Errors
///
/// [`ScenarioError::Rom`] for a ROM the machine refuses, and
/// [`ScenarioError::Disc`] for a disc no scenario may name.
pub fn machine(roms: Roms<'_>, scenario: &Scenario) -> Result<Machine, ScenarioError> {
    let mut machine = Machine::new(roms.os).map_err(ScenarioError::Rom)?;
    for (slot, image) in [(BASIC_SLOT, roms.basic), (DFS_SLOT, roms.dfs)] {
        machine
            .insert_rom(slot, image)
            .map_err(ScenarioError::Rom)?;
    }
    for (drive, name) in PhysicalDrive::ALL.into_iter().zip(&scenario.discs) {
        let protect = scenario.protected && drive == PhysicalDrive::Zero;
        machine.insert_disc(drive, disc(name, protect)?);
    }
    wait(&mut machine, STARTED);
    Ok(machine)
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
    let screen = mode7_text(&machine)
        .into_iter()
        .filter(|row| !row.is_empty())
        .collect();
    let mut discs = Vec::new();
    for &drive in &scenario.checked {
        let disc = machine
            .eject_disc(drive)
            .ok_or_else(|| ScenarioError::Disc(format!("no disc in drive {drive:?}")))?;
        discs.push((drive, tairix_crypto::sha256(&disc.encode().image)));
        machine.insert_disc(drive, disc);
    }
    Ok(Observation {
        rows: screen,
        discs,
    })
}

fn disc(name: &str, protected: bool) -> Result<Disc, ScenarioError> {
    let image = discs::named(name).ok_or_else(|| ScenarioError::Disc(String::from(name)))?;
    Disc::decode(&image, ImageFormat::Ssd, protected)
        .map_err(|_| ScenarioError::Disc(String::from(name)))
}

fn step_on(machine: &mut Machine, step: &str) -> Result<(), ScenarioError> {
    let (verb, argument) = step.split_once(':').unwrap_or((step, ""));
    match verb {
        "type" => type_text(machine, &argument.replace("\\n", "\n"))?,
        "press" => press(machine, key(argument)?),
        "wait" => wait(machine, centiseconds(argument)?),
        "swap" => machine.insert_disc(PhysicalDrive::Zero, disc(argument, false)?),
        "break" => press_break(machine, &[]),
        "shiftbreak" => press_break(machine, &[Key::SHIFT]),
        "" => {}
        _ => return Err(ScenarioError::Step(String::from(step))),
    }
    Ok(())
}

fn key(label: &str) -> Result<Key, ScenarioError> {
    Key::ALL
        .iter()
        .copied()
        .find(|key| key.label() == label)
        .ok_or_else(|| ScenarioError::Step(format!("no key {label}")))
}

/// Seconds written as `S` or `S.F`, to the centisecond.
fn centiseconds(seconds: &str) -> Result<u64, ScenarioError> {
    let refused = || ScenarioError::Step(format!("wait:{seconds}"));
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let whole: u64 = whole.parse().map_err(|_| refused())?;
    let hundredths: u64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().map_err(|_| refused())? * 10,
        2 => fraction.parse().map_err(|_| refused())?,
        _ => return Err(refused()),
    };
    let total = whole
        .checked_mul(100)
        .and_then(|centiseconds| centiseconds.checked_add(hundredths))
        .filter(|&total| total <= LONGEST_WAIT)
        .ok_or_else(refused)?;
    Ok(total)
}

/// The scenarios of a corpus file.
///
/// # Errors
///
/// [`ScenarioError::Line`] for the first line that is not one a corpus
/// holds, or a scenario without a script or with a name already used.
pub fn parse_corpus(text: &str) -> Result<Vec<Scenario>, ScenarioError> {
    let mut scenarios: Vec<Scenario> = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        let bad = ScenarioError::Line(number);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        if field == "scenario" {
            if value.is_empty() || scenarios.iter().any(|scenario| scenario.name == value) {
                return Err(bad);
            }
            scenarios.push(Scenario {
                name: String::from(value),
                discs: Vec::new(),
                protected: false,
                checked: Vec::new(),
                ours: false,
                script: String::new(),
            });
            continue;
        }
        let scenario = scenarios.last_mut().ok_or(bad.clone())?;
        match (field, value) {
            ("discs", names) if !names.is_empty() && scenario.discs.is_empty() => {
                scenario.discs = names.split(' ').map(String::from).collect();
                if scenario.discs.len() > 2 || scenario.discs.iter().any(String::is_empty) {
                    return Err(bad);
                }
            }
            ("protected", "") if !scenario.protected => scenario.protected = true,
            ("ours", "") if !scenario.ours => scenario.ours = true,
            ("check", drive) => {
                let drive = drive_named(drive).ok_or(bad.clone())?;
                if scenario.checked.contains(&drive) {
                    return Err(bad);
                }
                scenario.checked.push(drive);
            }
            ("script", steps) if !steps.is_empty() && scenario.script.is_empty() => {
                scenario.script = String::from(steps);
            }
            _ => return Err(bad),
        }
    }
    if let Some(scenario) = scenarios.iter().find(|scenario| scenario.script.is_empty()) {
        return Err(ScenarioError::Unmatched(scenario.name.clone()));
    }
    Ok(scenarios)
}

/// The observations of a fixtures file, each with its scenario's name.
///
/// # Errors
///
/// [`ScenarioError::Line`] for the first line that is not one a fixtures
/// file holds.
pub fn parse_fixtures(text: &str) -> Result<Vec<(String, Observation)>, ScenarioError> {
    let mut fixtures: Vec<(String, Observation)> = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        let bad = ScenarioError::Line(number);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        if field == "scenario" {
            if value.is_empty() || fixtures.iter().any(|(name, _)| name == value) {
                return Err(bad);
            }
            fixtures.push((
                String::from(value),
                Observation {
                    rows: Vec::new(),
                    discs: Vec::new(),
                },
            ));
            continue;
        }
        let (_, observation) = fixtures.last_mut().ok_or(bad.clone())?;
        match field {
            "row" => observation.rows.push(unescape(value).ok_or(bad)?),
            "disc" => {
                let (drive, digest) = value.split_once(' ').ok_or(bad.clone())?;
                let drive = drive_named(drive).ok_or(bad.clone())?;
                observation
                    .discs
                    .push((drive, digest_from_hex(digest).ok_or(bad)?));
            }
            _ => return Err(bad),
        }
    }
    Ok(fixtures)
}

/// `fixtures` as a fixtures file holds them, after `header`'s comment lines.
#[must_use]
pub fn format_fixtures(header: &[&str], fixtures: &[(String, Observation)]) -> String {
    let mut text = String::new();
    for line in header {
        text.push_str("# ");
        text.push_str(line);
        text.push('\n');
    }
    for (name, observation) in fixtures {
        text.push('\n');
        text.push_str("scenario ");
        text.push_str(name);
        text.push('\n');
        for row in &observation.rows {
            text.push_str("row ");
            text.push_str(&escape(row));
            text.push('\n');
        }
        for &(drive, digest) in &observation.discs {
            text.push_str("disc ");
            text.push(if drive == PhysicalDrive::Zero {
                '0'
            } else {
                '1'
            });
            text.push(' ');
            text.push_str(&hex(&digest));
            text.push('\n');
        }
    }
    text
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

/// `bytes` as lower-case hexadecimal.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        // Writing to a String does not fail.
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn drive_named(drive: &str) -> Option<PhysicalDrive> {
    match drive {
        "0" => Some(PhysicalDrive::Zero),
        "1" => Some(PhysicalDrive::One),
        _ => None,
    }
}

fn digest_from_hex(text: &str) -> Option<[u8; 32]> {
    let mut digest = [0; 32];
    if text.len() != 64 {
        return None;
    }
    for (byte, pair) in digest.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(digest)
}

/// A row as a fixtures file holds it.
fn escape(row: &str) -> String {
    let mut text = String::new();
    for ch in row.chars() {
        match ch {
            '\\' => text.push_str("\\\\"),
            ' '..='~' => text.push(ch),
            _ => {
                let _ = write!(text, "\\x{:02x}", u32::from(ch));
            }
        }
    }
    text
}

fn unescape(text: &str) -> Option<String> {
    let mut row = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            row.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => row.push('\\'),
            'x' => {
                let pair: String = chars.by_ref().take(2).collect();
                let code = u8::from_str_radix(&pair, 16)
                    .ok()
                    .filter(|_| pair.len() == 2)?;
                row.push(char::from(code));
            }
            _ => return None,
        }
    }
    Some(row)
}

#[cfg(test)]
#[path = "scenario_tests.rs"]
mod tests;
