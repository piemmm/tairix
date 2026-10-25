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
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

/// The synopsis the short-help switches print when `play`'s own Help tree
/// is unavailable.
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
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
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
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Invalid { option, why } => write!(f, "{option}: {why}"),
            Self::NoFiles => f.write_str("missing file operand"),
            Self::StandardInput => f.write_str("'-': standard input cannot be played"),
        }
    }
}

/// What each of `play`'s options means.
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

/// `play`'s options and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('q', &["quiet"], Flag::Quiet),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::long(&["ui"], Takes::Nothing, Flag::Ui),
    Opt::long(&["no-ui"], Takes::Nothing, Flag::NoUi),
    Opt::valued('d', &["device"], Flag::Device),
    Opt::valued('g', &["gain"], Flag::Gain),
    Opt::valued('s', &["start"], Flag::Start),
    Opt::valued('t', &["duration"], Flag::Duration),
    Opt {
        short: Some('l'),
        long: &["loop"],
        takes: Takes::Attached,
        means: Flag::Loop,
    },
    Opt::long(&["list-devices"], Takes::Nothing, Flag::ListDevices),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
    Opt::long(&["version"], Takes::Nothing, Flag::Version),
];

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

/// Read the command line, `args` without the program's own name, as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the files,
/// clustered, a value attached or the next argument but `--loop`'s count only
/// attached, a long option by any unambiguous prefix, `--` ending them. A help
/// or version request answers at once, `--list-devices` once the whole line is
/// read.
///
/// # Errors
///
/// * [`UsageError::Option`] — an option `getopt` refuses, in its words.
/// * [`UsageError::Invalid`] — a value the option does not take.
/// * [`UsageError::StandardInput`] — `-` among the files.
/// * [`UsageError::NoFiles`] — no file to play.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut parse = Parse {
        options: Options::default(),
        list: false,
    };
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Operand("-") => return Err(UsageError::StandardInput),
            Arg::Operand(file) => parse.options.files.push(file.to_string()),
            Arg::Opt(flag, value) => {
                if let Some(command) = parse.take(flag, value)? {
                    return Ok(command);
                }
            }
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
