//! Parse an `mdadm` command line into a [`Command`].
//!
//! `mdadm` is a mode tool: exactly one mode flag (`--create`, `--detail`,
//! `--examine`, `--add`, `--remove`, `--stop`) selects what to do, and the
//! operands and value options are interpreted in that mode's grammar. The
//! spelling tracks Linux `mdadm` — the same short and long flags, the same
//! `--long=value` / `-x value` grammar, and `--` for end-of-options — so a
//! user who knows `mdadm` finds this familiar. Where TAIRiX genuinely differs
//! (no `/dev`: a device is `node:<id>` and an array is a hexadecimal identity),
//! the difference is in the *operands*, not the flags.
//!
//! Parsing is pure and fails closed: an unknown flag, a missing value, a level
//! the composer does not offer, a value option in a mode that does not use it,
//! or the wrong operand count is a typed [`ParseError`], never a guess.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::raid::RaidLevel;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// A fully parsed `mdadm` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Show the command's own help and exit (`-h`, `-?`, `--help`).
    Help,
    /// Print the tool version and exit (`-V`, `--version`).
    Version,
    /// Report per-array detail. `array` is the identity operand, or [`None`]
    /// for every array (`-D`, `--detail [<array>]`).
    Detail {
        /// The array identity operand, or [`None`] for all arrays.
        array: Option<String>,
    },
    /// List every device the composer holds (`-E`, `--examine`).
    Examine,
    /// Create an array over the named devices (`-C`, `--create`).
    Create(CreateArgs),
    /// Admit a candidate device into an array's absent slot (`-a`, `--add`).
    Add {
        /// The array to admit the device into.
        array: String,
        /// The device to admit (`node:<id>`).
        device: String,
    },
    /// Retire a member device from an array (`-r`, `--remove`).
    Remove {
        /// The array to retire the member from.
        array: String,
        /// The device to retire (`node:<id>`).
        device: String,
    },
    /// Stop a live array (`-S`, `--stop`).
    Stop {
        /// The array to stop.
        array: String,
    },
}

/// The parts of a `--create` request that survive parsing: the level, the
/// requested slot count, the optional stripe unit, and the device operands
/// (each `node:<id>`, resolved later).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateArgs {
    /// The level to compose.
    pub level: RaidLevel,
    /// The number of member slots requested (`--raid-devices`).
    pub raid_devices: u16,
    /// The stripe unit in logical blocks, or [`None`] to let the composer
    /// choose; only meaningful for a striped level.
    pub chunk_blocks: Option<u32>,
    /// The device operands, in slot order (each `node:<id>`).
    pub devices: Vec<String>,
}

/// The mode a command line selected before its operands are validated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Mode {
    Create,
    Detail,
    Examine,
    Add,
    Remove,
    Stop,
}

impl Mode {
    /// The user-facing flag naming this mode, for a diagnostic.
    const fn flag(self) -> &'static str {
        match self {
            Self::Create => "--create",
            Self::Detail => "--detail",
            Self::Examine => "--examine",
            Self::Add => "--add",
            Self::Remove => "--remove",
            Self::Stop => "--stop",
        }
    }
}

/// Why a command line did not parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// No mode flag was given.
    NoMode,
    /// Two different mode flags were given.
    ConflictingModes,
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `--level` value that names nothing.
    BadLevel(String),
    /// A level TAIRiX does not compose (e.g. RAID4).
    LevelNotSupported(String),
    /// A `--raid-devices` value that is not a positive count.
    BadRaidDevices(String),
    /// A `--chunk` value that is not a block count.
    BadChunk(String),
    /// `--create` without `--level`.
    MissingLevel,
    /// `--create` without `--raid-devices`.
    MissingRaidDevices,
    /// `--chunk` given for a level that stores no stripe.
    ChunkNotAllowed,
    /// A value option (`--level`, `--raid-devices`, `--chunk`) in a mode that
    /// does not use it.
    OptionNotAllowed {
        /// The offending option.
        option: &'static str,
        /// The mode it was given in.
        mode: &'static str,
    },
    /// `--create` was given `n` devices but `--raid-devices=m`.
    DeviceCountMismatch {
        /// The count `--raid-devices` requested.
        expected: u16,
        /// The number of device operands given.
        got: usize,
    },
    /// A required operand was absent.
    MissingOperand(&'static str),
    /// An operand was given where none (or no more) was expected.
    UnexpectedOperand(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMode => f.write_str(
                "no mode given: use one of --create, --detail, --examine, --add, --remove, --stop",
            ),
            Self::ConflictingModes => f.write_str("only one mode may be given"),
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadLevel(value) => write!(f, "{value} is not a RAID level", value = quote(value)),
            Self::LevelNotSupported(value) => {
                write!(
                    f,
                    "RAID level {value} is not supported",
                    value = quote(value)
                )
            }
            Self::BadRaidDevices(value) => {
                write!(
                    f,
                    "{value} is not a positive device count",
                    value = quote(value)
                )
            }
            Self::BadChunk(value) => {
                write!(f, "{value} is not a block count", value = quote(value))
            }
            Self::MissingLevel => f.write_str("--create requires --level"),
            Self::MissingRaidDevices => f.write_str("--create requires --raid-devices"),
            Self::ChunkNotAllowed => {
                f.write_str("--chunk applies only to a striped level (raid0/5/6/10/tp)")
            }
            Self::OptionNotAllowed { option, mode } => {
                write!(
                    f,
                    "option {option} is not valid with {mode}",
                    option = quote(option)
                )
            }
            Self::DeviceCountMismatch { expected, got } => write!(
                f,
                "--raid-devices={expected} but {got} device(s) were given"
            ),
            Self::MissingOperand(what) => write!(f, "missing {what}"),
            Self::UnexpectedOperand(value) => {
                write!(f, "unexpected operand {value}", value = quote(value))
            }
        }
    }
}

/// What each of `mdadm`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Mode(Mode),
    Level,
    RaidDevices,
    Chunk,
    Help,
    Version,
}

/// `mdadm`'s options, as Linux `mdadm` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('C', &["create"], Flag::Mode(Mode::Create)),
    Opt::flag('D', &["detail"], Flag::Mode(Mode::Detail)),
    Opt::flag('E', &["examine"], Flag::Mode(Mode::Examine)),
    Opt::flag('a', &["add"], Flag::Mode(Mode::Add)),
    Opt::flag('r', &["remove"], Flag::Mode(Mode::Remove)),
    Opt::flag('S', &["stop"], Flag::Mode(Mode::Stop)),
    Opt::valued('l', &["level"], Flag::Level),
    Opt::valued('n', &["raid-devices"], Flag::RaidDevices),
    Opt::valued('c', &["chunk"], Flag::Chunk),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
    Opt::flag('V', &["version"], Flag::Version),
];

/// Parse the argument vector (excluding `argv[0]`).
///
/// It is read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere
/// among the operands, clustered (`-Cl0`), a value attached or following, a
/// long option by any unambiguous prefix, `--` ending them. A second,
/// different mode is refused as it is read; help and version wait for the
/// whole line, and help wins over version and every mode.
///
/// # Errors
///
/// A [`ParseError`] describing the first thing that did not parse; an option
/// `getopt` refuses is a [`ParseError::Option`].
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut state = State::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(operand) => state.operands.push(String::from(operand)),
            Arg::Opt(Flag::Mode(mode), _) => state.set_mode(mode)?,
            Arg::Opt(Flag::Level, value) => state.set_level(value.unwrap_or_default())?,
            Arg::Opt(Flag::RaidDevices, value) => {
                state.set_raid_devices(value.unwrap_or_default())?;
            }
            Arg::Opt(Flag::Chunk, value) => state.set_chunk(value.unwrap_or_default())?,
            Arg::Opt(Flag::Help, _) => state.saw_help = true,
            Arg::Opt(Flag::Version, _) => state.saw_version = true,
        }
    }
    state.finish()
}

/// The accumulator a single parse pass fills.
#[derive(Default)]
struct State {
    mode: Option<Mode>,
    level: Option<RaidLevel>,
    raid_devices: Option<u16>,
    chunk_blocks: Option<u32>,
    operands: Vec<String>,
    saw_help: bool,
    saw_version: bool,
}

impl State {
    /// Record a mode, rejecting a second, different one.
    fn set_mode(&mut self, mode: Mode) -> Result<(), ParseError> {
        match self.mode {
            Some(existing) if existing != mode => Err(ParseError::ConflictingModes),
            _ => {
                self.mode = Some(mode);
                Ok(())
            }
        }
    }

    /// Parse and record a `--level` value.
    fn set_level(&mut self, value: &str) -> Result<(), ParseError> {
        self.level = Some(parse_level(value)?);
        Ok(())
    }

    /// Parse and record a `--raid-devices` value.
    fn set_raid_devices(&mut self, value: &str) -> Result<(), ParseError> {
        let count = value
            .parse::<u16>()
            .map_err(|_| ParseError::BadRaidDevices(value.to_string()))?;
        if count == 0 {
            return Err(ParseError::BadRaidDevices(value.to_string()));
        }
        self.raid_devices = Some(count);
        Ok(())
    }

    /// Parse and record a `--chunk` value.
    fn set_chunk(&mut self, value: &str) -> Result<(), ParseError> {
        let blocks = value
            .parse::<u32>()
            .map_err(|_| ParseError::BadChunk(value.to_string()))?;
        self.chunk_blocks = Some(blocks);
        Ok(())
    }

    /// Turn the accumulated state into a [`Command`], validating operands.
    fn finish(self) -> Result<Command, ParseError> {
        // Help and version win over any mode, and over each other help wins.
        if self.saw_help {
            return Ok(Command::Help);
        }
        if self.saw_version {
            return Ok(Command::Version);
        }
        let mode = self.mode.ok_or(ParseError::NoMode)?;
        match mode {
            Mode::Create => self.finish_create(),
            Mode::Detail => {
                self.reject_value_options(Mode::Detail)?;
                let mut operands = self.operands.into_iter();
                let array = operands.next();
                if let Some(extra) = operands.next() {
                    return Err(ParseError::UnexpectedOperand(extra));
                }
                Ok(Command::Detail { array })
            }
            Mode::Examine => {
                self.reject_value_options(Mode::Examine)?;
                if let Some(extra) = self.operands.into_iter().next() {
                    return Err(ParseError::UnexpectedOperand(extra));
                }
                Ok(Command::Examine)
            }
            Mode::Add | Mode::Remove => {
                self.reject_value_options(mode)?;
                let mut operands = self.operands.iter();
                let array = operands
                    .next()
                    .ok_or(ParseError::MissingOperand("array"))?
                    .clone();
                let device = operands
                    .next()
                    .ok_or(ParseError::MissingOperand("device"))?
                    .clone();
                if let Some(extra) = operands.next() {
                    return Err(ParseError::UnexpectedOperand(extra.clone()));
                }
                if mode == Mode::Add {
                    Ok(Command::Add { array, device })
                } else {
                    Ok(Command::Remove { array, device })
                }
            }
            Mode::Stop => {
                self.reject_value_options(Mode::Stop)?;
                let mut operands = self.operands.into_iter();
                let array = operands.next().ok_or(ParseError::MissingOperand("array"))?;
                if let Some(extra) = operands.next() {
                    return Err(ParseError::UnexpectedOperand(extra));
                }
                Ok(Command::Stop { array })
            }
        }
    }

    /// Build a `--create` command, validating the level/count/chunk/devices.
    fn finish_create(self) -> Result<Command, ParseError> {
        let level = self.level.ok_or(ParseError::MissingLevel)?;
        let raid_devices = self.raid_devices.ok_or(ParseError::MissingRaidDevices)?;
        if self.chunk_blocks.is_some() && !level.is_striped() {
            return Err(ParseError::ChunkNotAllowed);
        }
        if self.operands.is_empty() {
            return Err(ParseError::MissingOperand("device"));
        }
        if self.operands.len() != usize::from(raid_devices) {
            return Err(ParseError::DeviceCountMismatch {
                expected: raid_devices,
                got: self.operands.len(),
            });
        }
        Ok(Command::Create(CreateArgs {
            level,
            raid_devices,
            chunk_blocks: self.chunk_blocks,
            devices: self.operands,
        }))
    }

    /// Reject a value option in a mode that does not consume it.
    fn reject_value_options(&self, mode: Mode) -> Result<(), ParseError> {
        let flag = mode.flag();
        if self.level.is_some() {
            return Err(ParseError::OptionNotAllowed {
                option: "--level",
                mode: flag,
            });
        }
        if self.raid_devices.is_some() {
            return Err(ParseError::OptionNotAllowed {
                option: "--raid-devices",
                mode: flag,
            });
        }
        if self.chunk_blocks.is_some() {
            return Err(ParseError::OptionNotAllowed {
                option: "--chunk",
                mode: flag,
            });
        }
        Ok(())
    }
}

/// Map a `--level` value to a [`RaidLevel`].
///
/// Accepts the numeric forms, the `raidN` forms, the descriptive words
/// `mirror`/`stripe`, and `tp` for triple parity — the spellings Linux
/// `mdadm` accepts, plus `tp`. RAID4 is named explicitly as unsupported
/// (TAIRiX has no dedicated-parity level), separately from an unknown level.
fn parse_level(value: &str) -> Result<RaidLevel, ParseError> {
    let lowered = value.to_ascii_lowercase();
    let level = match lowered.as_str() {
        "0" | "raid0" | "stripe" => RaidLevel::Stripe,
        "1" | "raid1" | "mirror" => RaidLevel::Mirror,
        "5" | "raid5" => RaidLevel::Parity,
        "6" | "raid6" => RaidLevel::DualParity,
        "10" | "raid10" => RaidLevel::Raid10,
        "tp" | "raid-tp" | "triple" | "tripleparity" => RaidLevel::TripleParity,
        "4" | "raid4" => return Err(ParseError::LevelNotSupported(value.to_string())),
        _ => return Err(ParseError::BadLevel(value.to_string())),
    };
    Ok(level)
}

#[cfg(test)]
mod tests;
