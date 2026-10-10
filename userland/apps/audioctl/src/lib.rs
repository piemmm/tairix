//! `audioctl`: the sound devices' controls from a terminal.
//!
//! It lists the sinks and sources the audio service shows the caller and the
//! streams on them, and makes a device its direction's default, sets its
//! level, or mutes it. A device is named by an `audio:` reference and
//! resolved against the devices listed now, so `audio:sink/default` changes
//! whichever sink is the default at that moment.
//!
//! The audio service decides who may change a control: the login session
//! holding the room the device serves, anybody while the room is unclaimed,
//! nobody while it is withheld. The tool holds no authority of its own over
//! them and reports a refusal with its reason.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::audio::AudioDeviceDescriptor;
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::Errno;
use tairix_audio::stream::DeviceControl;
use tairix_audio::target::{AudioTarget, TargetError};
use tairix_audio::volume::{typed_level, LevelError};
use tairix_help::{own_short_help, HelpSource};
use tairix_procinfo::{
    emit_self_scope_omission, for_each_audio_stream, render_audio_device, render_audio_stream,
    CallError, ListError, Output, StreamScope, Transport, WalkStep, AUDIO_DEVICE_HEADER,
    AUDIO_STREAM_HEADER,
};

/// The command word the tool's help and `stdinfo` records go by.
pub const OWN_WORD: &str = "audioctl";

/// What the short-help switches print when the bundle's own Help tree cannot
/// be read.
pub const USAGE: &str = "\
Usage: audioctl [devices]
       audioctl streams [-a|--all]
       audioctl default DEVICE
       audioctl level DEVICE LEVEL
       audioctl mute DEVICE
       audioctl unmute DEVICE
List the sound devices and the streams on them, or change a device's
controls. DEVICE is an audio: reference such as audio:sink/default; LEVEL is
in decibels, 0 or below.
";

/// What `audioctl` was asked to do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// List the sinks and sources.
    Devices,
    /// List the streams: the caller's own, or every principal's.
    Streams {
        /// Every principal's, which needs `CAP_SYSINFO_GLOBAL`.
        all: bool,
    },
    /// Change one device's controls.
    Set(AudioTarget, DeviceControl),
    /// Show the tool's own short help.
    Help,
    /// Show the version.
    Version,
}

/// Why an argument vector is not a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// A command word the tool does not have.
    UnknownCommand(String),
    /// An option the command does not take.
    UnknownOption(String),
    /// The command needs this operand.
    Missing(&'static str),
    /// An operand past those the command takes.
    Extra(String),
    /// Not a sink or source reference.
    Device(String, TargetError),
    /// Not a level.
    Level(String, LevelError),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(word) => write!(f, "unknown command '{word}'"),
            Self::UnknownOption(option) => write!(f, "unrecognized option '{option}'"),
            Self::Missing(what) => write!(f, "missing {what}"),
            Self::Extra(operand) => write!(f, "extra operand '{operand}'"),
            Self::Device(text, err) => write!(f, "'{text}': {err}"),
            Self::Level(text, LevelError::Malformed) => write!(
                f,
                "'{text}': a level is decibels to the hundredth, such as -6 or -3.5"
            ),
            Self::Level(text, LevelError::Boost) => {
                write!(f, "'{text}': a level only attenuates: 0 dB or below")
            }
        }
    }
}

/// Read the argument vector, the program's own name excluded.
///
/// A level is often negative, so an operand of a digit after its `-` is a
/// number rather than an option: no option of this tool is spelled so.
///
/// # Errors
///
/// The [`UsageError`] the first malformed argument makes.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut words: Vec<&str> = Vec::new();
    let mut all: Option<&str> = None;
    let mut options_ended = false;
    for &arg in args {
        let numeric = arg
            .strip_prefix('-')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
        if options_ended || numeric || !arg.starts_with('-') || arg == "-" {
            words.push(arg);
            continue;
        }
        match arg {
            "--" => options_ended = true,
            "-h" | "-?" | "--help" => return Ok(Command::Help),
            "--version" => return Ok(Command::Version),
            "-a" | "--all" => all = Some(arg),
            _ => return Err(UsageError::UnknownOption(String::from(arg))),
        }
    }
    let (word, operands) = match words.split_first() {
        Some((word, operands)) => (*word, operands),
        None => ("devices", &[][..]),
    };
    if let Some(option) = all.filter(|_| word != "streams") {
        return Err(UsageError::UnknownOption(String::from(option)));
    }
    let command = match word {
        "devices" => Command::Devices,
        "streams" => Command::Streams { all: all.is_some() },
        "default" => Command::Set(device(operands)?, DeviceControl::Default),
        "level" => {
            let target = device(operands)?;
            let text = operands.get(1).ok_or(UsageError::Missing("a level"))?;
            let level =
                typed_level(text).map_err(|err| UsageError::Level(String::from(*text), err))?;
            Command::Set(target, DeviceControl::Level(level))
        }
        "mute" => Command::Set(device(operands)?, DeviceControl::Mute(true)),
        "unmute" => Command::Set(device(operands)?, DeviceControl::Mute(false)),
        other => return Err(UsageError::UnknownCommand(String::from(other))),
    };
    let taken = match command {
        Command::Set(_, DeviceControl::Level(_)) => 2,
        Command::Set(..) => 1,
        _ => 0,
    };
    match operands.get(taken) {
        Some(extra) => Err(UsageError::Extra(String::from(*extra))),
        None => Ok(command),
    }
}

/// The device the first operand names.
fn device(operands: &[&str]) -> Result<AudioTarget, UsageError> {
    let text = operands.first().ok_or(UsageError::Missing("a device"))?;
    AudioTarget::parse(text).map_err(|err| UsageError::Device(String::from(*text), err))
}

/// The audio service, as this tool reaches it.
pub trait Sound {
    /// Every device of `direction` the caller is shown, by ascending id.
    ///
    /// # Errors
    ///
    /// The service's or the transport's refusal.
    fn devices(&mut self, direction: StreamDirection) -> Result<Vec<AudioDeviceDescriptor>, Errno>;

    /// Apply `control` to the device `device_id`.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    fn set(&mut self, device_id: u32, control: DeviceControl) -> Result<(), Errno>;
}

/// Why a command did not complete.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The audio service could not list its devices.
    Devices(Errno),
    /// The streams could not be listed.
    Streams(ListError),
    /// No device answers to the target now.
    NoDevice(AudioTarget),
    /// The service refused the change.
    Refused(AudioTarget, Errno),
    /// Standard output failed.
    Output(Errno),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Devices(err) => write!(f, "the audio service could not list its devices: {err}"),
            Self::Streams(ListError::Call(CallError::PermissionDenied)) => f.write_str(
                "every principal's streams are listed under CAP_SYSINFO_GLOBAL, which this session does not hold",
            ),
            Self::Streams(ListError::Call(CallError::Service(err)) | ListError::Sink(err)) => {
                write!(f, "the streams could not be listed: {err}")
            }
            Self::NoDevice(target) => write!(f, "{target}: no such device is connected"),
            Self::Refused(target, Errno::SeatNotOwner) => write!(
                f,
                "{target}: its controls belong to the session holding the room it serves"
            ),
            Self::Refused(target, err) => write!(f, "{target}: refused: {err}"),
            Self::Output(err) => write!(f, "standard output failed: {err}"),
        }
    }
}

/// Carry out `command`. `help` is the tool's own `Help/` tree, read for the
/// short-help switches in `locale`; `info` reaches the System Information API
/// the streams are listed through.
///
/// # Errors
///
/// The [`Failure`] that stopped the command.
pub fn run(
    command: Command,
    locale: Option<&str>,
    sound: &mut dyn Sound,
    info: &dyn Transport,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), Failure> {
    match command {
        Command::Help => {
            let text = own_short_help(help, locale, OWN_WORD)
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| String::from(USAGE));
            emit(out, text.trim_end_matches('\n'))
        }
        Command::Version => emit(
            out,
            &format!("{OWN_WORD} (TAIRiX) {}", env!("CARGO_PKG_VERSION")),
        ),
        Command::Devices => {
            let sinks = sound
                .devices(StreamDirection::Playback)
                .map_err(Failure::Devices)?;
            let sources = sound
                .devices(StreamDirection::Capture)
                .map_err(Failure::Devices)?;
            emit(out, AUDIO_DEVICE_HEADER)?;
            for device in sinks.iter().chain(&sources) {
                emit(out, &render_audio_device(device))?;
            }
            Ok(())
        }
        Command::Streams { all } => {
            let scope = if all {
                StreamScope::Every
            } else {
                StreamScope::Own
            };
            // The header waits for the first answer, so a refused listing
            // prints nothing that reads as an empty one.
            let mut headed = false;
            for_each_audio_stream(info, scope, |stream| {
                if !headed {
                    out.write_line(AUDIO_STREAM_HEADER)?;
                    headed = true;
                }
                out.write_line(&render_audio_stream(stream))
                    .map(|()| WalkStep::Continue)
            })
            .map_err(Failure::Streams)?;
            if !headed {
                emit(out, AUDIO_STREAM_HEADER)?;
            }
            if !all {
                emit_self_scope_omission(out, OWN_WORD, &[OWN_WORD, "streams", "--all"]);
            }
            Ok(())
        }
        Command::Set(target, control) => {
            let devices = sound.devices(target.direction).map_err(Failure::Devices)?;
            let device = target.resolve(&devices).ok_or(Failure::NoDevice(target))?;
            sound
                .set(device.device_id, control)
                .map_err(|err| Failure::Refused(target, err))
        }
    }
}

fn emit(out: &dyn Output, line: &str) -> Result<(), Failure> {
    out.write_line(line).map_err(Failure::Output)
}

#[cfg(test)]
mod tests;
