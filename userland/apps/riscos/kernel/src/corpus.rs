//! The recorded observation corpus (`plans/RISCOS-EMULATOR.md` §17): scenarios
//! of SWI calls, and what genuine RISC OS left after each.
//!
//! A scenario file is lines of steps, `#` starting a comment:
//!
//! - `scenario NAME [ours]` begins one; `ours` marks one whose fixture states
//!   this emulator's own behaviour, which a recording leaves alone;
//! - `call SWI rN=VALUE …` calls a SWI, the registers not named zero, a value
//!   being a number (`&` for hex), a quoted string, or `$NAME[+N]` for one
//!   kept;
//! - `keep rN NAME` keeps a register the last call answered;
//! - `show ITEM …` records registers (`rN`) and flags (`n`, `z`, `c`, `v`)
//!   the last call left: only those the SWI defines, since the rest are its
//!   caller's.
//!
//! An observation is a scenario's recorded lines, in the one format the
//! reference harness writes and the replay compares: per `show`, the call's
//! ordinal, then `rN=&XXXXXXXX` for each register shown, `F=0` or `F=1` for
//! each flag, and `error=&N` whenever V is set, the error block's number in
//! place of an R0 that points at it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The registers a call takes and answers.
pub const REGISTERS: u8 = 10;

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
    /// What the last call left, recorded.
    Show {
        /// The registers shown.
        registers: Vec<u8>,
        /// The flags shown.
        flags: Vec<Flag>,
    },
}

/// A scenario.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scenario {
    /// Its name.
    pub name: String,
    /// Whether its fixture is this emulator's own.
    pub ours: bool,
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

fn value(text: &str, kept: &[String]) -> Option<Value> {
    if let Some(quoted) = text.strip_prefix('"') {
        let inner = quoted.strip_suffix('"')?;
        return (!inner.contains('"')).then(|| Value::Text(inner.to_string()));
    }
    if let Some(reference) = text.strip_prefix('$') {
        let (name, offset) = match reference.find(['+', '-']) {
            Some(at) => (
                &reference[..at],
                number(&reference[at..].replace('+', ""))?.cast_signed(),
            ),
            None => (reference, 0),
        };
        return kept.iter().any(|known| known == name).then(|| Value::Kept {
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
            if !valid_name(name) {
                return Err(refused(line, "a kept value's name"));
            }
            kept.push((*name).to_string());
            Step::Keep {
                register: target,
                name: (*name).to_string(),
            }
        }
        "show" => {
            let mut registers = Vec::new();
            let mut flags = Vec::new();
            for item in rest {
                match (register(item), Flag::of(item)) {
                    (Some(register), _) => registers.push(register),
                    (None, Some(flag)) => flags.push(flag),
                    (None, None) => {
                        return Err(refused(line, "registers and n, z, c and v are shown"))
                    }
                }
            }
            Step::Show { registers, flags }
        }
        _ => return Err(refused(line, "a step is call, keep or show")),
    })
}

/// The scenarios a scenario file holds.
pub fn parse_scenarios(text: &str) -> Result<Vec<Scenario>, ParseError> {
    let mut scenarios: Vec<Scenario> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let code = raw.split_once('#').map_or(raw, |(code, _)| code).trim();
        if code.is_empty() {
            continue;
        }
        let words = words(code).ok_or(refused(line, "an unclosed string"))?;
        let [keyword, rest @ ..] = words.as_slice() else {
            continue;
        };
        if *keyword == "scenario" {
            let (name, ours) = match rest {
                [name] => (*name, false),
                [name, "ours"] => (*name, true),
                _ => return Err(refused(line, "a scenario is named, and may be marked ours")),
            };
            if !valid_name(name) || scenarios.iter().any(|s| s.name == name) {
                return Err(refused(
                    line,
                    "a scenario's name is unique, letters, digits, - and _",
                ));
            }
            scenarios.push(Scenario {
                name: name.to_string(),
                ours,
                steps: Vec::new(),
            });
            kept.clear();
            continue;
        }
        let scenario = scenarios
            .last_mut()
            .ok_or(refused(line, "a step outside a scenario"))?;
        let step = step(keyword, rest, &mut kept, line)?;
        if !matches!(step, Step::Call { .. })
            && !scenario
                .steps
                .iter()
                .any(|step| matches!(step, Step::Call { .. }))
        {
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
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn scenarios_parse_their_calls_kept_values_and_shows() {
        let text = "# areas\nscenario make-one\ncall XOS_DynamicArea r0=0 r1=-1 r2=&2000 r8=\"A title\" # made\nkeep r1 area\ncall XOS_DynamicArea r0=2 r1=$area r2=$area+&10\nshow r2 r5 v c\n\nscenario ours-one ours\ncall OS_ReadMonotonicTime\n";
        let scenarios = parse_scenarios(text).unwrap();
        assert_eq!(scenarios.len(), 2);
        assert_eq!(
            scenarios[0].steps,
            vec![
                Step::Call {
                    swi: "XOS_DynamicArea".into(),
                    registers: vec![
                        (0, Value::Number(0)),
                        (1, Value::Number(u32::MAX)),
                        (2, Value::Number(0x2000)),
                        (8, Value::Text("A title".into())),
                    ],
                },
                Step::Keep {
                    register: 1,
                    name: "area".into()
                },
                Step::Call {
                    swi: "XOS_DynamicArea".into(),
                    registers: vec![
                        (0, Value::Number(2)),
                        (
                            1,
                            Value::Kept {
                                name: "area".into(),
                                offset: 0
                            }
                        ),
                        (
                            2,
                            Value::Kept {
                                name: "area".into(),
                                offset: 0x10
                            }
                        ),
                    ],
                },
                Step::Show {
                    registers: vec![2, 5],
                    flags: vec![Flag::V, Flag::C]
                },
            ]
        );
        assert!(scenarios[1].ours);
    }

    #[test]
    fn a_malformed_scenario_is_refused_with_its_line() {
        for (text, line) in [
            ("call X", 1),
            ("scenario a\nshow r1", 2),
            ("scenario a\ncall X r10=1", 2),
            ("scenario a\ncall X r1=$nothing", 2),
            ("scenario a\ncall X r1=1 r1=2", 2),
            ("scenario a\nscenario a", 2),
            ("scenario a b c", 1),
            ("scenario a\ncall X r8=\"open", 2),
            ("scenario a\nmangle", 2),
            ("scenario a\ncall X\nshow flags", 3),
        ] {
            assert_eq!(
                parse_scenarios(text).map_err(|e| e.line),
                Err(line),
                "{text}"
            );
        }
    }

    #[test]
    fn observations_round_trip_through_their_format() {
        let observations = vec![
            Observation {
                name: "one".into(),
                lines: vec!["1 r1=&00000100 v=0 c=0 z=0 n=0".into()],
            },
            Observation {
                name: "two".into(),
                lines: vec![
                    "1 error=&000001C1 r1=&00000000".into(),
                    "2 r2=&00002000".into(),
                ],
            },
        ];
        let text = format_observations(&["Recorded from RISC OS 5.30."], &observations);
        assert!(text.starts_with("# Recorded from RISC OS 5.30.\n"));
        assert_eq!(parse_observations(&text).unwrap(), observations);
        assert!(
            parse_observations("1 r1=&0").is_err(),
            "a line outside a scenario"
        );
    }
}
