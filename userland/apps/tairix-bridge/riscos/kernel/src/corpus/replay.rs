//! A scenario replayed on a session's SWIs, as the reference harness ran it
//! on genuine RISC OS, and a group's fixtures held to its scenarios. Each
//! crate's corpus test replays its own groups on a kernel holding the
//! built-in modules they need.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

use super::{
    check_observation, format_observations, parse_observations, parse_scenarios, At, Flag, Item,
    Kind, Scenario, Step, Value, UNWRITTEN,
};
use crate::memory::layout::APPLICATION;
use crate::memory::Bits;
use crate::os::{len32, Ending, Fail, Host, Kernel, Os, Pulse, Regs};

/// The application space a replay works in: room for every string and block
/// a scenario passes.
pub const SPACE: u32 = 0x10_0000;

/// A host still at the session's start, with no input: what a replay runs
/// on.
pub struct StillHost;

impl Host for StillHost {
    fn monotonic(&mut self) -> u64 {
        0
    }

    fn real_time(&mut self) -> u64 {
        0
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Continue
    }

    fn input(&mut self) -> Option<crate::input::InputEvent> {
        None
    }

    fn relay(&mut self, _request: &[u8], _reply: &mut alloc::vec::Vec<u8>) -> bool {
        false
    }

    fn wait(&mut self, _: Option<u64>, _: Ending) -> Pulse {
        Pulse::Stop
    }
}

/// A group's scenarios and fixtures held to each other: each scenario has
/// exactly one fixture, in order, recording just what its shows ask, and the
/// fixtures are in the one format the reference harness writes.
///
/// # Errors
///
/// What is wrong, naming the scenario or line.
pub fn check_group(scenarios: &str, fixtures: &str) -> Result<(), String> {
    let scenarios =
        parse_scenarios(scenarios).map_err(|e| format!("scenarios:{}: {}", e.line, e.reason))?;
    let observations =
        parse_observations(fixtures).map_err(|e| format!("fixtures:{}: {}", e.line, e.reason))?;
    let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
    let fixed: Vec<&str> = observations.iter().map(|o| o.name.as_str()).collect();
    if names != fixed {
        return Err("one fixture per scenario, in order".to_string());
    }
    for (scenario, observation) in scenarios.iter().zip(&observations) {
        check_observation(scenario, observation)
            .map_err(|e| format!("{}: its line {}: {}", scenario.name, e.line, e.reason))?;
    }
    let header: Vec<&str> = fixtures
        .lines()
        .take_while(|line| line.starts_with("# "))
        .map(|line| &line[2..])
        .collect();
    if format_observations(&header, &observations) != fixtures {
        return Err("fixtures not as the harness writes them".to_string());
    }
    Ok(())
}

/// Every line of `scenarios` that its replay through `replay` leaves other
/// than `fixtures` records, each saying where; a scenario marked `reference`
/// is not replayed.
///
/// # Errors
///
/// A scenarios or fixtures file that does not parse.
pub fn differences(
    group: &str,
    scenarios: &str,
    fixtures: &str,
    mut replay: impl FnMut(&Scenario) -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let scenarios =
        parse_scenarios(scenarios).map_err(|e| format!("{group}:{}: {}", e.line, e.reason))?;
    let observations =
        parse_observations(fixtures).map_err(|e| format!("{group}:{}: {}", e.line, e.reason))?;
    let mut differ = Vec::new();
    for (scenario, fixture) in scenarios.iter().zip(&observations) {
        if scenario.kind == Kind::Reference {
            continue;
        }
        let ours = match replay(scenario) {
            Ok(ours) => ours,
            Err(reason) => {
                differ.push(format!("{group} {}: {reason}", scenario.name));
                continue;
            }
        };
        for (index, (mine, theirs)) in ours.iter().zip(&fixture.lines).enumerate() {
            if mine != theirs {
                differ.push(format!(
                    "{group} {} line {}\n  RISC OS: {theirs}\n  ours:    {mine}",
                    scenario.name,
                    index + 1
                ));
            }
        }
        if ours.len() != fixture.lines.len() {
            differ.push(format!("{group} {}: a line per show", scenario.name));
        }
    }
    Ok(differ)
}

/// A replay's place in application space: the SWI names it looks up at the
/// start, and the strings and blocks it passes after them.
struct Space {
    next: u32,
}

impl Space {
    const NAMES: u32 = APPLICATION;

    const fn new() -> Self {
        Self {
            next: APPLICATION + 0x100,
        }
    }

    /// `bytes` of room, word-aligned as BASIC's are.
    fn take(&mut self, bytes: u32) -> Result<u32, String> {
        let at = self.next;
        self.next = bytes
            .checked_next_multiple_of(4)
            .and_then(|room| self.next.checked_add(room))
            .filter(|&next| next <= APPLICATION + SPACE)
            .ok_or("past the replay's room")?;
        Ok(at)
    }
}

fn kept_value(kept: &[(String, u32)], name: &str, offset: i32) -> Result<u32, String> {
    kept.iter()
        .find(|(k, _)| k == name)
        .map(|&(_, v)| v.wrapping_add_signed(offset))
        .ok_or_else(|| format!("{name} used before it is kept"))
}

/// Where a shown item is.
fn address(at: &At, regs: &Regs, kept: &[(String, u32)]) -> Result<u32, String> {
    match at {
        At::Register(register) => Ok(regs.r[usize::from(*register)]),
        At::Kept { name, offset } => kept_value(kept, name, *offset),
    }
}

/// `fail` read for a replay: a fault is the replay's own failure.
fn faulted(fail: Fail) -> String {
    format!("guest memory refused: {fail:?}")
}

/// What `item` of a show records, its key first; nothing for an item an
/// error leaves undefined.
fn shown(
    os: &mut Os<'_>,
    item: &Item,
    regs: &Regs,
    failed: bool,
    kept: &[(String, u32)],
) -> Result<String, String> {
    if failed && item.undefined_after_error() {
        return Ok(String::new());
    }
    let key = item.key();
    Ok(match item {
        Item::Register(register) => format!(" {key}=&{:08X}", regs.r[usize::from(*register)]),
        Item::Relative { register, from } => {
            let difference =
                regs.r[usize::from(*register)].wrapping_sub(kept_value(kept, from, 0)?);
            format!(" {key}=&{difference:08X}")
        }
        Item::Flag(flag) => {
            let set = match flag {
                Flag::V => failed,
                Flag::N => regs.flag(1 << 31),
                Flag::Z => regs.flag(1 << 30),
                Flag::C => regs.flag(1 << 29),
            };
            format!(" {key}={}", u8::from(set))
        }
        Item::Pointer { at, from } => {
            let word = os.read_word(address(at, regs, kept)?).map_err(faulted)?;
            format!(
                " {key}=&{:08X}",
                word.wrapping_sub(kept_value(kept, from, 0)?)
            )
        }
        Item::Bytes { at, length } => {
            let address = address(at, regs, kept)?;
            let bytes = os.read_vec(address, *length as usize).map_err(faulted)?;
            let mut text = format!(" {key}=");
            for byte in bytes {
                let _ = write!(text, "{byte:02X}");
            }
            text
        }
    })
}

/// What `scenario` leaves on `kernel`, as the harness records it: each call
/// made from a slot of application space the replay makes current.
///
/// # Errors
///
/// What stops the replay: a SWI not known, guest memory refused, an unwind.
pub fn replay(kernel: &mut Kernel, scenario: &Scenario) -> Result<Vec<String>, String> {
    let memory = kernel.memory();
    let slot = memory
        .create_slot(Bits::Bit32)
        .map_err(|e| format!("a slot: {e:?}"))?;
    memory
        .set_slot_size(slot, SPACE)
        .map_err(|e| format!("its memory: {e:?}"))?;
    memory
        .switch_slot(slot)
        .map_err(|e| format!("made current: {e:?}"))?;
    let mut kept: Vec<(String, u32)> = Vec::new();
    let mut space = Space::new();
    let mut last = (Regs::default(), false);
    let mut calls = 0;
    let mut lines = Vec::new();
    for step in &scenario.steps {
        let mut os = kernel.os();
        match step {
            Step::Call { swi, registers } => {
                calls += 1;
                let mut name = swi.clone().into_bytes();
                name.push(0);
                os.write_bytes(Space::NAMES, &name).map_err(faulted)?;
                let mut lookup = Regs::new([0, Space::NAMES, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                os.perform(0x2_0039, &mut lookup)
                    .map_err(|_| format!("{swi} is not a SWI"))?;
                let mut regs = Regs::default();
                for (register, value) in registers {
                    regs.r[usize::from(*register)] = match value {
                        Value::Number(number) => *number,
                        Value::Text(text) => {
                            let mut bytes = text.clone().into_bytes();
                            bytes.push(0);
                            let at = space.take(len32(bytes.len()))?;
                            os.write_bytes(at, &bytes).map_err(faulted)?;
                            at
                        }
                        Value::Kept { name, offset } => kept_value(&kept, name, *offset)?,
                    };
                }
                let failed = match os.perform(lookup.r[0], &mut regs) {
                    Ok(()) => false,
                    Err(Fail::Error(block)) => {
                        regs.r[0] = block;
                        true
                    }
                    Err(Fail::Unwind(unwind)) => return Err(format!("{swi} unwound: {unwind:?}")),
                };
                last = (regs, failed);
            }
            Step::Keep { register, name } => {
                let value = last.0.r[usize::from(*register)];
                kept.retain(|(k, _)| k != name);
                kept.push((name.clone(), value));
            }
            Step::Block { name, size } => {
                let at = space.take(*size)?;
                os.write_bytes(at, &alloc::vec![UNWRITTEN; *size as usize])
                    .map_err(faulted)?;
                kept.retain(|(k, _)| k != name);
                kept.push((name.clone(), at));
            }
            Step::Write { at, bytes } => {
                let Value::Kept { name, offset } = at else {
                    return Err("a write is to a kept address".to_string());
                };
                os.write_bytes(kept_value(&kept, name, *offset)?, bytes)
                    .map_err(faulted)?;
            }
            Step::Show { items } => {
                let (regs, failed) = last;
                let mut line = calls.to_string();
                for item in items {
                    line.push_str(&shown(&mut os, item, &regs, failed, &kept)?);
                }
                if failed {
                    let number = os.read_word(regs.r[0]).map_err(faulted)?;
                    let _ = write!(line, " error=&{number:08X}");
                }
                lines.push(line);
            }
        }
    }
    Ok(lines)
}
