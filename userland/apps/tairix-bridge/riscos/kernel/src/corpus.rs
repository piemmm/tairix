//! The recorded observation corpus (`plans/RISCOS-EMULATOR.md` §17): scenarios
//! of SWI calls, and what genuine RISC OS left after each.
//!
//! A scenario file is lines of steps, `#` starting a comment:
//!
//! - `scenario NAME [ours|reference] [lasting]` begins one, its [`Kind`]:
//!   `ours` marks one whose fixture states this emulator's own behaviour,
//!   which a recording leaves alone, and `reference` one recorded but never
//!   replayed, since it reads the machine itself; `lasting` marks one that
//!   changes the machine in a way RISC OS gives no means to undo, which the
//!   reference harness, running every scenario on one machine, runs after
//!   all the others;
//! - `call SWI rN=VALUE …` calls a SWI, the registers not named zero, a value
//!   being a number (`&` for hex), a quoted string, or `$NAME[+N]` for one
//!   kept;
//! - `keep rN NAME` keeps a register the last call answered;
//! - `block NAME SIZE` makes a block of memory, every byte `&EE`, and keeps
//!   its address;
//! - `write $NAME[+N] ITEM …` writes into a block from there on: `w=WORD`
//!   four bytes low first, `b=BYTE` one, `s="TEXT"` the text's bytes;
//! - `show ITEM …` records what the last call left, each in its order:
//!   registers (`rN`), a register less a kept address (`rN-NAME`), flags
//!   (`n`, `z`, `c`, `v`), bytes of memory at a kept address or a
//!   register's (`$NAME[+N]:LENGTH`, `rN:LENGTH`), and a word there less a
//!   kept address, a pointer into a block (`$NAME[+N]@FROM`, `rN@FROM`) —
//!   only what the SWI defines,
//!   since the rest is its caller's; memory at a register is shown only
//!   where the call succeeded, a failed call defining no more than R0.
//!
//! An observation is a scenario's recorded lines, in the one format the
//! reference harness writes and the replay compares: per `show`, the call's
//! ordinal, then each item as `KEY=VALUE` — a register, a difference or a
//! pointer as `&XXXXXXXX`, a flag as `0` or `1`, bytes as two upper-case hex
//! digits each
//! — and `error=&N` whenever V is set, the error block's number in place of
//! every item an error leaves undefined.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod replay;

pub use replay::{check_group, differences, replay, StillHost, SPACE};

/// The registers a call takes and answers.
pub const REGISTERS: u8 = 10;

pub use tairix_obey::UNWRITTEN;

/// The largest block a scenario makes: room for the largest table a call
/// writes, `OS_Memory` 7's physical memory arrangement table.
pub const LARGEST_BLOCK: u32 = 64 * 1024;

/// The most bytes one item shows.
pub const LONGEST_SHOWN: u32 = 64;

/// A value a call is given.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    /// A number.
    Number(u32),
    /// A string, passed as a pointer to it.
    Text(String),
    /// A value kept from an earlier call, plus an offset.
    Kept {
        /// Its name.
        name: String,
        /// The offset added.
        offset: i32,
    },
}

/// A processor flag a call leaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flag {
    /// Negative.
    N,
    /// Zero.
    Z,
    /// Carry.
    C,
    /// Overflow: an error, with R0 pointing at it.
    V,
}

impl Flag {
    /// Its name in a scenario and an observation.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::N => "n",
            Self::Z => "z",
            Self::C => "c",
            Self::V => "v",
        }
    }

    /// Its bit in the flags BASIC's `SYS` returns, and that bit's place.
    #[must_use]
    pub const fn bit(self) -> u32 {
        match self {
            Self::V => 1,
            Self::C => 2,
            Self::Z => 4,
            Self::N => 8,
        }
    }

    fn of(name: &str) -> Option<Self> {
        [Self::N, Self::Z, Self::C, Self::V]
            .into_iter()
            .find(|flag| flag.name() == name)
    }
}

/// Where shown bytes are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum At {
    /// A kept address, plus an offset.
    Kept {
        /// Its name.
        name: String,
        /// The offset added.
        offset: i32,
    },
    /// The address a register holds.
    Register(u8),
}

/// One thing a `show` records.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Item {
    /// A register.
    Register(u8),
    /// A register less a kept address.
    Relative {
        /// The register.
        register: u8,
        /// The kept address's name.
        from: String,
    },
    /// A flag.
    Flag(Flag),
    /// Bytes of memory.
    Bytes {
        /// Where.
        at: At,
        /// How many.
        length: u32,
    },
    /// The word in memory at `at`, less a kept address: a pointer into a
    /// block.
    Pointer {
        /// Where the word is.
        at: At,
        /// The kept address's name.
        from: String,
    },
}

impl At {
    /// Its key, as a scenario names it and an observation records it.
    fn key(&self) -> String {
        match self {
            Self::Register(register) => format!("r{register}"),
            Self::Kept { name, offset: 0 } => format!("${name}"),
            Self::Kept { name, offset } if *offset > 0 => format!("${name}+&{offset:X}"),
            Self::Kept { name, offset } => format!("${name}-&{:X}", offset.unsigned_abs()),
        }
    }
}

impl Item {
    /// Whether a failed call leaves it undefined: R0, which the error
    /// replaces, and memory at any register.
    #[must_use]
    pub const fn undefined_after_error(&self) -> bool {
        matches!(
            self,
            Self::Register(0)
                | Self::Relative { register: 0, .. }
                | Self::Bytes {
                    at: At::Register(_),
                    ..
                }
                | Self::Pointer {
                    at: At::Register(_),
                    ..
                }
        )
    }

    /// Its key, as a scenario names it and an observation records it.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Register(register) => format!("r{register}"),
            Self::Relative { register, from } => format!("r{register}-{from}"),
            Self::Flag(flag) => flag.name().to_string(),
            Self::Bytes { at, length } => format!("{}:{length}", at.key()),
            Self::Pointer { at, from } => format!("{}@{from}", at.key()),
        }
    }
}

/// One step of a scenario.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// A SWI called, by name, with the registers named.
    Call {
        /// The SWI's name.
        swi: String,
        /// Each register given, by number.
        registers: Vec<(u8, Value)>,
    },
    /// A register the last call answered, kept by name.
    Keep {
        /// The register.
        register: u8,
        /// Its name.
        name: String,
    },
    /// A block made, every byte [`UNWRITTEN`], its address kept by name.
    Block {
        /// Its name.
        name: String,
        /// Its size, at most [`LARGEST_BLOCK`].
        size: u32,
    },
    /// Bytes written into a block.
    Write {
        /// Where: a kept address, plus an offset.
        at: Value,
        /// What.
        bytes: Vec<u8>,
    },
    /// What the last call left, recorded.
    Show {
        /// What is shown, in its order.
        items: Vec<Item>,
    },
}

/// Where a scenario's fixture comes from, and what it is held against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Recorded from genuine RISC OS, and replayed on the kernel.
    Replayed,
    /// The emulator's own observation, which a recording leaves alone.
    Ours,
    /// Recorded from genuine RISC OS and never replayed: what it reads is the
    /// machine itself, held against what the emulator records of it.
    Reference,
}

/// A scenario.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scenario {
    /// Its name.
    pub name: String,
    /// Where its fixture comes from.
    pub kind: Kind,
    /// Whether it changes the machine in a way RISC OS cannot undo.
    pub lasting: bool,
    /// Its steps.
    pub steps: Vec<Step>,
}

/// What a scenario left: its recorded lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Observation {
    /// The scenario's name.
    pub name: String,
    /// Its lines, as the harness writes them.
    pub lines: Vec<String>,
}

/// Why a file was refused, and the line that refused it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    /// The line, counting from one.
    pub line: usize,
    /// Why.
    pub reason: &'static str,
}

fn refused(line: usize, reason: &'static str) -> ParseError {
    ParseError { line, reason }
}

/// A name scenarios and kept values may have: what the harness can carry
/// through a BASIC string and a variable alike.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A name a kept value may have: as a scenario's, but for `-`, which would
/// read as an offset's sign after it.
fn valid_kept_name(name: &str) -> bool {
    valid_name(name) && !name.contains('-')
}

fn register(word: &str) -> Option<u8> {
    let number = word.strip_prefix('r')?.parse::<u8>().ok()?;
    (number < REGISTERS).then_some(number)
}

/// A number as scenarios write it: decimal, negative, or `&` hex.
fn number(text: &str) -> Option<u32> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let magnitude = match digits.strip_prefix('&') {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    Some(if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    })
}

/// A kept reference, `NAME[+N]` or `NAME-N` after its `$`: the name and
/// offset, the name one kept.
fn reference<'a>(text: &'a str, kept: &[String]) -> Option<(&'a str, i32)> {
    let (name, offset) = match text.find(['+', '-']) {
        Some(at) => (
            &text[..at],
            number(&text[at..].replace('+', ""))?.cast_signed(),
        ),
        None => (text, 0),
    };
    kept.iter()
        .any(|known| known == name)
        .then_some((name, offset))
}

fn value(text: &str, kept: &[String]) -> Option<Value> {
    if let Some(quoted) = text.strip_prefix('"') {
        let inner = quoted.strip_suffix('"')?;
        return (!inner.contains('"')).then(|| Value::Text(inner.to_string()));
    }
    if let Some(text) = text.strip_prefix('$') {
        let (name, offset) = reference(text, kept)?;
        return Some(Value::Kept {
            name: name.to_string(),
            offset,
        });
    }
    number(text).map(Value::Number)
}

/// The words of a line, a quoted string being one word.
fn words(line: &str) -> Option<Vec<&str>> {
    let mut words = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let end = if let Some(after) = rest.strip_prefix('"') {
            1 + after.find('"')? + 1
        } else if let Some(at) = rest.find("=\"") {
            let close = rest[at + 2..].find('"')?;
            match rest.find(char::is_whitespace) {
                Some(space) if space < at => space,
                _ => at + 2 + close + 1,
            }
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        words.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    Some(words)
}

/// What a `write` puts down for one of its items.
fn written(item: &str, out: &mut Vec<u8>) -> Option<()> {
    let (kind, given) = item.split_once('=')?;
    match kind {
        "w" => out.extend(number(given)?.to_le_bytes()),
        "b" => out.push(u8::try_from(number(given)?).ok()?),
        "s" => {
            let text = given.strip_prefix('"')?.strip_suffix('"')?;
            if text.contains('"') {
                return None;
            }
            out.extend(text.bytes());
        }
        _ => return None,
    }
    Some(())
}

/// Where a `show`'s memory item is: a kept address or a register.
fn shown_at(text: &str, kept: &[String]) -> Option<At> {
    Some(match text.strip_prefix('$') {
        Some(text) => {
            let (name, offset) = reference(text, kept)?;
            At::Kept {
                name: name.to_string(),
                offset,
            }
        }
        None => At::Register(register(text)?),
    })
}

/// One of a `show`'s items.
fn shown_item(word: &str, kept: &[String]) -> Option<Item> {
    if let Some(register) = register(word) {
        return Some(Item::Register(register));
    }
    if let Some(flag) = Flag::of(word) {
        return Some(Item::Flag(flag));
    }
    if let Some((at, length)) = word.split_once(':') {
        let length = length.parse::<u32>().ok()?;
        if length == 0 || length > LONGEST_SHOWN {
            return None;
        }
        let at = shown_at(at, kept)?;
        return Some(Item::Bytes { at, length });
    }
    if let Some((at, from)) = word.split_once('@') {
        let at = shown_at(at, kept)?;
        return kept
            .iter()
            .any(|known| known == from)
            .then(|| Item::Pointer {
                at,
                from: from.to_string(),
            });
    }
    let (register_word, from) = word.split_once('-')?;
    let register = register(register_word)?;
    kept.iter()
        .any(|known| known == from)
        .then(|| Item::Relative {
            register,
            from: from.to_string(),
        })
}

/// The step a line of `keyword` and its `rest` gives, the values kept so
/// far being `kept`.
fn step(
    keyword: &str,
    rest: &[&str],
    kept: &mut Vec<String>,
    line: usize,
) -> Result<Step, ParseError> {
    Ok(match keyword {
        "call" => {
            let [swi, assignments @ ..] = rest else {
                return Err(refused(line, "a call names its SWI"));
            };
            if !valid_name(swi) {
                return Err(refused(line, "a SWI's name"));
            }
            let mut registers: Vec<(u8, Value)> = Vec::new();
            for assignment in assignments {
                let (target, given) = assignment
                    .split_once('=')
                    .ok_or(refused(line, "a register given as rN=VALUE"))?;
                let target = register(target).ok_or(refused(line, "a register from r0 to r9"))?;
                if registers.iter().any(|(r, _)| *r == target) {
                    return Err(refused(line, "a register given twice"));
                }
                let given = value(given, kept)
                    .ok_or(refused(line, "a value: a number, a string, or one kept"))?;
                registers.push((target, given));
            }
            Step::Call {
                swi: (*swi).to_string(),
                registers,
            }
        }
        "keep" => {
            let [target, name] = rest else {
                return Err(refused(line, "keep rN NAME"));
            };
            let target = register(target).ok_or(refused(line, "a register from r0 to r9"))?;
            if !valid_kept_name(name) {
                return Err(refused(line, "a kept value's name, letters, digits and _"));
            }
            kept.push((*name).to_string());
            Step::Keep {
                register: target,
                name: (*name).to_string(),
            }
        }
        "block" => {
            let [name, size] = rest else {
                return Err(refused(line, "block NAME SIZE"));
            };
            if !valid_kept_name(name) {
                return Err(refused(line, "a kept value's name, letters, digits and _"));
            }
            let size = number(size)
                .filter(|size| (1..=LARGEST_BLOCK).contains(size))
                .ok_or(refused(
                    line,
                    "a block of a byte up to the largest a scenario makes",
                ))?;
            kept.push((*name).to_string());
            Step::Block {
                name: (*name).to_string(),
                size,
            }
        }
        "write" => {
            let [at, items @ ..] = rest else {
                return Err(refused(line, "write $NAME ITEM…"));
            };
            let at = at
                .strip_prefix('$')
                .and_then(|_| value(at, kept))
                .ok_or(refused(line, "a write is to a kept address"))?;
            let mut bytes = Vec::new();
            for item in items {
                written(item, &mut bytes)
                    .ok_or(refused(line, "w=WORD, b=BYTE or s=\"TEXT\" is written"))?;
            }
            if bytes.is_empty() {
                return Err(refused(line, "a write puts something down"));
            }
            Step::Write { at, bytes }
        }
        "show" => {
            let mut items = Vec::new();
            for word in rest {
                items.push(shown_item(word, kept).ok_or(refused(
                    line,
                    "registers, differences, flags and bytes of memory are shown",
                ))?);
            }
            Step::Show { items }
        }
        _ => return Err(refused(line, "a step is call, keep, block, write or show")),
    })
}

/// `line` up to its comment: the first `#` outside a quoted string.
fn uncommented(line: &str) -> &str {
    let mut quoted = false;
    for (at, b) in line.bytes().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return &line[..at],
            _ => {}
        }
    }
    line
}

/// The scenarios a scenario file holds.
pub fn parse_scenarios(text: &str) -> Result<Vec<Scenario>, ParseError> {
    let mut scenarios: Vec<Scenario> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let code = uncommented(raw).trim();
        if code.is_empty() {
            continue;
        }
        let words = words(code).ok_or(refused(line, "an unclosed string"))?;
        let [keyword, rest @ ..] = words.as_slice() else {
            continue;
        };
        if *keyword == "scenario" {
            // An `ours` scenario never reaches the machine, so it cannot last.
            let (name, kind, lasting) =
                match rest {
                    [name] => (*name, Kind::Replayed, false),
                    [name, "lasting"] => (*name, Kind::Replayed, true),
                    [name, "ours"] => (*name, Kind::Ours, false),
                    [name, "reference"] => (*name, Kind::Reference, false),
                    [name, "reference", "lasting"] => (*name, Kind::Reference, true),
                    _ => return Err(refused(
                        line,
                        "a scenario is named, and may be marked ours or reference, then lasting",
                    )),
                };
            if !valid_name(name) || scenarios.iter().any(|s| s.name == name) {
                return Err(refused(
                    line,
                    "a scenario's name is unique, letters, digits, - and _",
                ));
            }
            scenarios.push(Scenario {
                name: name.to_string(),
                kind,
                lasting,
                steps: Vec::new(),
            });
            kept.clear();
            continue;
        }
        let scenario = scenarios
            .last_mut()
            .ok_or(refused(line, "a step outside a scenario"))?;
        let step = step(keyword, rest, &mut kept, line)?;
        let called = scenario
            .steps
            .iter()
            .any(|step| matches!(step, Step::Call { .. }));
        if matches!(step, Step::Keep { .. } | Step::Show { .. }) && !called {
            return Err(refused(line, "keep and show follow a call"));
        }
        scenario.steps.push(step);
    }
    Ok(scenarios)
}

/// The observations a fixtures file, or a recording, holds.
pub fn parse_observations(text: &str) -> Result<Vec<Observation>, ParseError> {
    let mut observations: Vec<Observation> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("scenario ") {
            if !valid_name(name) || observations.iter().any(|o| o.name == name) {
                return Err(refused(index + 1, "a scenario's name is unique"));
            }
            observations.push(Observation {
                name: name.to_string(),
                lines: Vec::new(),
            });
        } else {
            let observation = observations
                .last_mut()
                .ok_or(refused(index + 1, "a line outside a scenario"))?;
            observation.lines.push(line.to_string());
        }
    }
    Ok(observations)
}

/// Refuses an observation that is not what its scenario's shows record: a
/// line per `show`, each the ordinal of the call before it, then the show's
/// items in its order, and the error number exactly when V was set, in place
/// of every item an error leaves undefined.
pub fn check_observation(scenario: &Scenario, observation: &Observation) -> Result<(), ParseError> {
    let mut lines = observation.lines.iter().enumerate();
    let mut call = 0;
    for step in &scenario.steps {
        match step {
            Step::Call { .. } => call += 1,
            Step::Keep { .. } | Step::Block { .. } | Step::Write { .. } => {}
            Step::Show { items } => {
                let (index, line) = lines
                    .next()
                    .ok_or(refused(observation.lines.len() + 1, "a line per show"))?;
                check_shown(line, call, items).map_err(|reason| refused(index + 1, reason))?;
            }
        }
    }
    match lines.next() {
        Some((index, _)) => Err(refused(index + 1, "a line per show")),
        None => Ok(()),
    }
}

fn check_shown(line: &str, call: usize, items: &[Item]) -> Result<(), &'static str> {
    let mut words: Vec<&str> = line.split(' ').collect();
    if words.first().and_then(|word| word.parse::<usize>().ok()) != Some(call) {
        return Err("a line starts with the ordinal of the call it shows");
    }
    let error = words.last().and_then(|word| word.strip_prefix("error="));
    if let Some(number) = error {
        if !is_word(number) {
            return Err("an error number is a full word");
        }
        words.pop();
    }
    let failed = error.is_some();
    let mut shown = words.iter().skip(1);
    for item in items {
        if failed && item.undefined_after_error() {
            continue;
        }
        let key = item.key();
        let value = shown
            .next()
            .and_then(|word| word.strip_prefix(key.as_str()))
            .and_then(|word| word.strip_prefix('='))
            .ok_or("each item shown, in order, by its key")?;
        let good = match item {
            Item::Register(_) | Item::Relative { .. } | Item::Pointer { .. } => is_word(value),
            Item::Flag(flag) => {
                let set = match value {
                    "0" => false,
                    "1" => true,
                    _ => return Err("each flag shown, 0 or 1"),
                };
                if *flag == Flag::V && set != failed {
                    return Err("an error number exactly when V is set");
                }
                true
            }
            Item::Bytes { length, .. } => {
                value.len() == 2 * *length as usize && value.bytes().all(is_upper_hex)
            }
        };
        if !good {
            return Err("a word is &XXXXXXXX, bytes two hex digits each");
        }
    }
    if shown.next().is_some() {
        return Err("nothing but what the show asks");
    }
    Ok(())
}

const fn is_upper_hex(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b'A'..=b'F')
}

/// A full word as the harness writes it: `&` and eight upper-case hex digits.
fn is_word(text: &str) -> bool {
    text.strip_prefix('&')
        .is_some_and(|hex| hex.len() == 8 && hex.bytes().all(is_upper_hex))
}

/// A fixtures file: `header` as comments, then each observation.
#[must_use]
pub fn format_observations(header: &[&str], observations: &[Observation]) -> String {
    let mut out = String::new();
    for line in header {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    for observation in observations {
        out.push_str("\nscenario ");
        out.push_str(&observation.name);
        out.push('\n');
        for line in &observation.lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
