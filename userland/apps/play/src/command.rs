//! The command line: what `play` was asked to do.
//!
//! GNU-shaped: short options cluster (`-qv`), a value is attached or the next
//! argument (`-g-6`, `-g -6`, `--gain=-6`, `--gain -6`), `--loop`'s count is
//! optional and so only ever attached, and `--` ends the options.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::num::NonZeroU32;

use tairix_abi::audio::AudioGain;
use tairix_abi::driver::audio::StreamDirection;
use tairix_audio::target::AudioTarget;
use tairix_audio::volume::{typed_level, LevelError};
use tairix_player::{Extent, List, Passes, Settings, Span};
use tairix_util::argv::option_value;

/// The synopsis a usage error is answered with.
pub const USAGE: &str = "Usage: play [OPTION]... FILE...";

/// What the command line asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// The bundle's own help document.
    Help,
    /// The version line.
    Version,
    /// The sinks this session may play on.
    ListDevices,
    /// Play the files.
    Play(Options),
}

/// Whether the full-screen interface is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Interface {
    /// Whenever the terminal is this process's to draw on.
    Auto,
    /// Asked for: a session without a terminal is refused.
    Forced,
    /// Never.
    Off,
}

/// What a playback was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// The files, in order.
    pub files: Vec<String>,
    /// Whether the full-screen interface is drawn.
    pub interface: Interface,
    /// No interface and no progress line.
    pub quiet: bool,
    /// Each file's format and timing on standard error.
    pub verbose: bool,
    /// The sink.
    pub target: AudioTarget,
    /// The stream's level.
    pub gain: AudioGain,
    /// Where each file begins.
    pub start: Span,
    /// How much of each file plays, from its start.
    pub duration: Option<Span>,
    /// How many times the list plays.
    pub passes: Passes,
}

impl Options {
    /// The list these options play, and how it is heard on the sink
    /// `device_id` names — what [`target`](Self::target) resolved to.
    #[must_use]
    pub fn playback(&self, device_id: u32) -> (List, Settings) {
        let extent = Extent {
            start: self.start,
            duration: self.duration,
        };
        let settings = Settings {
            gain: self.gain,
            device_id,
            normalise: false,
        };
        (List::new(self.files.clone(), extent, self.passes), settings)
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            interface: Interface::Auto,
            quiet: false,
            verbose: false,
            target: AudioTarget::DEFAULT_SINK,
            gain: AudioGain::UNITY,
            start: Span::ZERO,
            duration: None,
            passes: Passes::ONCE,
        }
    }
}

/// Why a command line was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option this program does not have.
    Unknown(String),
    /// An option that takes a value was given none.
    Missing(&'static str),
    /// An option given a value it takes none of.
    Unwanted(&'static str),
    /// A value that is not one the option takes.
    Invalid {
        /// The option.
        option: &'static str,
        /// What is wrong with it.
        why: &'static str,
    },
    /// No file to play.
    NoFiles,
    /// Standard input, which a sound needing seeks cannot be read from.
    StandardInput,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(option) => write!(f, "unrecognized option '{option}'"),
            Self::Missing(option) => write!(f, "option '{option}' requires an argument"),
            Self::Unwanted(option) => write!(f, "option '{option}' doesn't allow an argument"),
            Self::Invalid { option, why } => write!(f, "{option}: {why}"),
            Self::NoFiles => f.write_str("missing file operand"),
            Self::StandardInput => f.write_str("'-': standard input cannot be played"),
        }
    }
}

/// The options, each with its short spelling where it has one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Flag {
    Quiet,
    Verbose,
    Ui,
    NoUi,
    Device,
    Gain,
    Start,
    Duration,
    Loop,
    ListDevices,
    Help,
    Version,
}

impl Flag {
    fn long(name: &str) -> Option<Self> {
        Some(match name {
            "quiet" => Self::Quiet,
            "verbose" => Self::Verbose,
            "ui" => Self::Ui,
            "no-ui" => Self::NoUi,
            "device" => Self::Device,
            "gain" => Self::Gain,
            "start" => Self::Start,
            "duration" => Self::Duration,
            "loop" => Self::Loop,
            "list-devices" => Self::ListDevices,
            "help" => Self::Help,
            "version" => Self::Version,
            _ => return None,
        })
    }

    const fn short(letter: char) -> Option<Self> {
        Some(match letter {
            'q' => Self::Quiet,
            'v' => Self::Verbose,
            'd' => Self::Device,
            'g' => Self::Gain,
            's' => Self::Start,
            't' => Self::Duration,
            'l' => Self::Loop,
            'h' | '?' => Self::Help,
            _ => return None,
        })
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Quiet => "--quiet",
            Self::Verbose => "--verbose",
            Self::Ui => "--ui",
            Self::NoUi => "--no-ui",
            Self::Device => "--device",
            Self::Gain => "--gain",
            Self::Start => "--start",
            Self::Duration => "--duration",
            Self::Loop => "--loop",
            Self::ListDevices => "--list-devices",
            Self::Help => "--help",
            Self::Version => "--version",
        }
    }

    /// Whether a value always follows, attached or as the next argument.
    const fn takes_value(self) -> bool {
        matches!(
            self,
            Self::Device | Self::Gain | Self::Start | Self::Duration
        )
    }
}

/// What the parse has gathered.
struct Parse {
    options: Options,
    list: bool,
}

impl Parse {
    /// Take `flag`, whose value — if it has one — is `value`. A help or
    /// version request answers at once, whatever follows it.
    fn take(&mut self, flag: Flag, value: Option<&str>) -> Result<Option<Command>, UsageError> {
        let options = &mut self.options;
        match flag {
            Flag::Help => return Ok(Some(Command::Help)),
            Flag::Version => return Ok(Some(Command::Version)),
            Flag::ListDevices => self.list = true,
            Flag::Quiet => {
                options.quiet = true;
                options.interface = Interface::Off;
            }
            Flag::Verbose => options.verbose = true,
            Flag::Ui => options.interface = Interface::Forced,
            Flag::NoUi => options.interface = Interface::Off,
            Flag::Loop => options.passes = loop_passes(value)?,
            Flag::Device => options.target = sink(value.unwrap_or_default())?,
            Flag::Gain => options.gain = gain(value.unwrap_or_default())?,
            Flag::Start => options.start = span(value.unwrap_or_default(), "--start")?,
            Flag::Duration => {
                options.duration = Some(span(value.unwrap_or_default(), "--duration")?);
            }
        }
        Ok(None)
    }
}

/// Read the command line, `args` without the program's own name.
///
/// # Errors
///
/// [`UsageError`] for anything this program does not take.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut parse = Parse {
        options: Options::default(),
        list: false,
    };
    let mut index = 0;
    let mut operands_only = false;
    while let Some(&arg) = args.get(index) {
        index += 1;
        if operands_only || arg == "-" || !arg.starts_with('-') {
            if arg == "-" {
                return Err(UsageError::StandardInput);
            }
            parse.options.files.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            operands_only = true;
            continue;
        }
        let answered = if let Some(long) = arg.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let flag = Flag::long(name).ok_or_else(|| UsageError::Unknown(arg.to_string()))?;
            let value = if flag.takes_value() {
                Some(
                    option_value(attached, args, &mut index)
                        .ok_or(UsageError::Missing(flag.name()))?,
                )
            } else if flag == Flag::Loop {
                attached
            } else if attached.is_some() {
                return Err(UsageError::Unwanted(flag.name()));
            } else {
                None
            };
            parse.take(flag, value)?
        } else {
            short_cluster(&mut parse, &arg[1..], args, &mut index)?
        };
        if let Some(command) = answered {
            return Ok(command);
        }
    }
    if parse.list {
        return Ok(Command::ListDevices);
    }
    if parse.options.files.is_empty() {
        return Err(UsageError::NoFiles);
    }
    Ok(Command::Play(parse.options))
}

/// Take one cluster of short options, the first of which taking a value ends
/// it with the rest of the cluster or the next argument.
fn short_cluster(
    parse: &mut Parse,
    cluster: &str,
    args: &[&str],
    index: &mut usize,
) -> Result<Option<Command>, UsageError> {
    for (at, letter) in cluster.char_indices() {
        let flag = Flag::short(letter).ok_or_else(|| {
            let mut unknown = String::from("-");
            unknown.push(letter);
            UsageError::Unknown(unknown)
        })?;
        let rest = &cluster[at + letter.len_utf8()..];
        let attached = (!rest.is_empty()).then_some(rest);
        if flag.takes_value() {
            let value =
                option_value(attached, args, index).ok_or(UsageError::Missing(flag.name()))?;
            return parse.take(flag, Some(value));
        }
        if flag == Flag::Loop {
            return parse.take(flag, attached);
        }
        if let Some(command) = parse.take(flag, None)? {
            return Ok(Some(command));
        }
    }
    Ok(None)
}

fn loop_passes(value: Option<&str>) -> Result<Passes, UsageError> {
    let Some(value) = value else {
        return Ok(Passes::Forever);
    };
    let invalid = UsageError::Invalid {
        option: "--loop",
        why: "the count must be a whole number of passes, one or more",
    };
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid);
    }
    value
        .parse::<NonZeroU32>()
        .map(Passes::Times)
        .map_err(|_| invalid)
}

fn sink(value: &str) -> Result<AudioTarget, UsageError> {
    let target = AudioTarget::parse(value).map_err(|_| UsageError::Invalid {
        option: "--device",
        why: "not an audio: sink reference, such as audio:sink/default",
    })?;
    if target.direction != StreamDirection::Playback {
        return Err(UsageError::Invalid {
            option: "--device",
            why: "names a source; sound is played on a sink",
        });
    }
    Ok(target)
}

/// A level in decibels, to the hundredth: `-6`, `-3.5`, `0`.
fn gain(value: &str) -> Result<AudioGain, UsageError> {
    typed_level(value).map_err(|err| UsageError::Invalid {
        option: "--gain",
        why: match err {
            LevelError::Malformed => "a level in decibels, to the hundredth, such as -6 or -3.5",
            LevelError::Boost => "a stream can only be attenuated: 0 dB or below",
        },
    })
}

/// A span spelled `[[HH:]MM:]SS[.fraction]`.
fn span(value: &str, option: &'static str) -> Result<Span, UsageError> {
    let invalid = UsageError::Invalid {
        option,
        why: "a time such as 90, 1:30, 1:02:03 or 12.5",
    };
    let fields: Vec<&str> = value.split(':').collect();
    if fields.len() > 3 {
        return Err(invalid);
    }
    let Some((last, higher)) = fields.split_last() else {
        return Err(invalid);
    };
    let (seconds, fraction) = last.split_once('.').unwrap_or((last, ""));
    if fraction.len() > 9 || (last.contains('.') && fraction.is_empty()) {
        return Err(invalid);
    }
    let mut total: u64 = 0;
    for (position, field) in higher.iter().copied().chain([seconds]).enumerate() {
        let number = whole(field).ok_or_else(|| invalid.clone())?;
        if position > 0 && number >= 60 {
            return Err(invalid);
        }
        total = total
            .checked_mul(60)
            .and_then(|total| total.checked_add(number))
            .ok_or_else(|| invalid.clone())?;
    }
    let mut nanos = 0u64;
    let mut scale = 100_000_000u64;
    for digit in fraction.bytes() {
        if !digit.is_ascii_digit() {
            return Err(invalid);
        }
        nanos += u64::from(digit - b'0') * scale;
        scale /= 10;
    }
    total
        .checked_mul(1_000_000_000)
        .and_then(|whole| whole.checked_add(nanos))
        .map(Span::from_nanos)
        .ok_or(invalid)
}

/// A field of digits alone.
fn whole(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
