//! The Raspberry Pi category: the board's CPU fan on its 40-pin header, and
//! what the cooling service reads of it (`plans/COOLING.md` CL7).
//!
//! At the user's direction the one board-named surface in userland. It is
//! shown only where the board's identity names a Raspberry Pi, and reaches
//! the hardware only through the cooling service and the settings writer.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::PlatformCompatible;
use tairix_abi::thermal_ipc::{FanFault, FanMode, ThermalStatus};
use tairix_controls::{FieldControl, FieldGroup, FieldRow};

/// The `compatible` vendor prefix of every Raspberry Pi board.
const RASPBERRY_PI_VENDOR: &[u8] = b"raspberrypi,";

/// Whether `board` names a Raspberry Pi.
#[must_use]
pub fn names_raspberry_pi(board: &PlatformCompatible) -> bool {
    board
        .strings()
        .any(|name| name.starts_with(RASPBERRY_PI_VENDOR))
}

/// The 40-pin header's physical pin for each of GPIO 0–27, the one layout
/// every Raspberry Pi with the header shares.
const HEADER_PINS: [u8; 28] = [
    27, 28, 3, 5, 7, 29, 31, 26, 24, 21, 19, 23, 32, 33, 8, 10, 36, 11, 12, 35, 38, 40, 15, 16, 18,
    22, 37, 13,
];

/// How a line is offered: its GPIO number, and its header pin where it has
/// one.
#[must_use]
pub fn line_label(line: u8) -> String {
    match HEADER_PINS.get(usize::from(line)) {
        Some(pin) => format!("GPIO {line} (pin {pin})"),
        None => format!("GPIO {line}"),
    }
}

/// The temperature as a reading row shows it, to a tenth of a degree.
#[must_use]
pub fn temperature_text(tenths: i32) -> String {
    let sign = if tenths < 0 { "-" } else { "" };
    let magnitude = tenths.unsigned_abs();
    format!("{sign}{}.{} °C", magnitude / 10, magnitude % 10)
}

const STATUS_CAPTION: &str = "STATUS";
const TEMPERATURE_LABEL: &str = "Processor temperature";
const FAN_LABEL: &str = "Fan";
const UNREAD: &str = "not read yet";
const NO_SERVICE: &str = "the cooling service did not answer";

/// What the fan is doing, in a word.
fn fan_state(reading: &ThermalStatus) -> &'static str {
    match (reading.policy.mode(), reading.running) {
        (FanMode::Off, _) => "Off",
        (_, true) => "Running",
        (_, false) => "Stopped",
    }
}

/// The sentence a fault owes the reader.
#[must_use]
pub const fn fault_sentence(fault: FanFault) -> &'static str {
    match fault {
        FanFault::NoLine => "No control line is chosen, so the fan cannot be switched.",
        FanFault::NoHeader => {
            "The header's lines are not being served, so the fan cannot be switched."
        }
        FanFault::LineUnavailable => {
            "The header would not lend the control line, so the fan cannot be switched."
        }
        FanFault::NoSensor => {
            "This board reports no processor temperature, so the fan runs all the time."
        }
        FanFault::SensorFailed => {
            "The processor's temperature cannot be read, so the fan runs until it can."
        }
    }
}

/// What the cooling service has answered this window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Cooling {
    /// Nothing yet.
    #[default]
    Unread,
    /// The service could not be asked, or its answer did not decode.
    Unanswered,
    /// Its status.
    Read(ThermalStatus),
}

impl Cooling {
    /// The status, once one has been read.
    #[must_use]
    pub const fn status(&self) -> Option<&ThermalStatus> {
        match self {
            Self::Read(status) => Some(status),
            Self::Unread | Self::Unanswered => None,
        }
    }

    /// Whether `next` differs from this only in its temperature, so the rows
    /// it shapes stand and only the temperature's reading moves.
    #[must_use]
    pub fn only_temperature_moves(&self, next: &Self) -> bool {
        match (self, next) {
            (Self::Read(held), Self::Read(next)) => {
                ThermalStatus {
                    temperature: next.temperature,
                    ..*held
                } == *next
            }
            _ => false,
        }
    }
}

/// The STATUS group: the processor's temperature and the fan, from the
/// cooling service's `reading`, or a statement of why there is none.
#[must_use]
pub fn status_group(reading: &Cooling) -> FieldGroup {
    FieldGroup::new(STATUS_CAPTION, status_rows(reading))
}

/// The STATUS group's rows, in order: the temperature, then the fan.
#[must_use]
pub fn status_rows(reading: &Cooling) -> Vec<FieldRow> {
    let unmeasured = |why: &str| FieldControl::Unmeasured(String::from(why));
    let (temperature, fan, fault) = match reading {
        Cooling::Unread => (unmeasured(UNREAD), unmeasured(UNREAD), None),
        Cooling::Unanswered => (unmeasured(NO_SERVICE), unmeasured(NO_SERVICE), None),
        Cooling::Read(reading) => (
            reading.temperature.map_or_else(
                || unmeasured("no processor sensor answered"),
                |temperature| FieldControl::Reading(temperature_text(temperature.tenths())),
            ),
            FieldControl::Reading(String::from(fan_state(reading))),
            reading.fault,
        ),
    };
    let mut fan = FieldRow::new(FAN_LABEL, fan);
    if let Some(fault) = fault {
        fan = fan.with_description(fault_sentence(fault));
    }
    alloc::vec![FieldRow::new(TEMPERATURE_LABEL, temperature), fan]
}

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;
