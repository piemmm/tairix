//! TAIRiX HID: the report-descriptor model and the decoders every HID class
//! driver runs (`plans/HID.md`).
//!
//! A class driver hands the device's report descriptor to
//! [`ReportDescriptor::parse`], builds a [`HidDevice`] from it, configures it
//! through its [`HidTransport`], and feeds it each input report; the device
//! answers key, pointer and touch records on the driver's [`SeatSink`]. The
//! transport — USB, I2C — is the driver's; nothing here knows of one.
//!
//! Everything validates its input whole and fails closed: a malformed
//! descriptor is refused, and a malformed report changes nothing held.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, PointerInput};
use tairix_abi::touch::TouchFrame;
use tairix_abi::{DriverError, Errno};

pub mod boot;
pub mod config;
pub mod console;
pub mod descriptor;
pub mod device;
pub mod keyboard;
pub mod mouse;
pub mod touch;
pub mod usages;

#[cfg(test)]
mod test_support;

pub use config::HidTransport;
pub use console::KeyboardConsole;
pub use descriptor::{DescriptorError, ReportDescriptor, ReportId, MAX_DESCRIPTOR, MAX_REPORT};
pub use device::{Applications, HidDevice};

/// What a decoder made of one input report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decoded {
    /// The report is no application's of this decoder.
    NotMine,
    /// The report was read and its records delivered.
    Applied,
    /// The report is this decoder's but cut short: nothing was applied.
    Malformed,
}

impl Decoded {
    /// Two decoders' outcomes for one report: a malformed reading wins,
    /// then an applied one.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Malformed, _) | (_, Self::Malformed) => Self::Malformed,
            (Self::Applied, _) | (_, Self::Applied) => Self::Applied,
            (Self::NotMine, Self::NotMine) => Self::NotMine,
        }
    }
}

/// Where a decoder delivers what it read: the seat's three channels.
pub trait SeatSink {
    /// A resolved key edge.
    ///
    /// # Errors
    ///
    /// The delivery's refusal.
    fn key(&mut self, record: &KeyInput) -> Result<(), DriverError>;

    /// A pointer motion, button or scroll.
    ///
    /// # Errors
    ///
    /// The delivery's refusal.
    fn pointer(&mut self, record: &PointerInput) -> Result<(), DriverError>;

    /// A touch surface's frame.
    ///
    /// # Errors
    ///
    /// The delivery's refusal.
    fn touch(&mut self, frame: &TouchFrame) -> Result<(), DriverError>;
}

/// Whether `field` sits under the top-level `application`.
pub(crate) fn in_application(
    model: &ReportDescriptor,
    field: &descriptor::Field,
    application: descriptor::CollectionIndex,
) -> bool {
    field
        .collection
        .is_some_and(|collection| model.top_level(collection) == application)
}

/// Push `value`, or `None` when memory for it runs out.
pub(crate) fn try_push<T>(list: &mut Vec<T>, value: T) -> Option<()> {
    list.try_reserve(1).ok()?;
    list.push(value);
    Some(())
}

/// Collect `items`, or `None` when memory for them runs out.
pub(crate) fn try_collect<T>(items: impl Iterator<Item = T>) -> Option<Vec<T>> {
    let mut list = Vec::new();
    for item in items {
        try_push(&mut list, item)?;
    }
    Some(list)
}

/// Classify a transport refusal for a driver's pump loop: only the transport
/// endpoint itself having gone ([`Errno::NotFound`]) is the device leaving;
/// every other refusal, including one this build cannot read, is a fault the
/// loop rides out under [`pump_error_limit_reached`].
#[must_use]
pub const fn transport_error(err: Errno) -> DriverError {
    match err {
        Errno::NotFound => DriverError::NotFound,
        _ => DriverError::DeviceFault,
    }
}

/// Count one consecutive pump failure and say whether `limit` is reached;
/// saturating, so a long-running driver cannot wrap back under it.
#[must_use]
pub fn pump_error_limit_reached(consecutive_errors: &mut u8, limit: u8) -> bool {
    *consecutive_errors = consecutive_errors.saturating_add(1);
    *consecutive_errors >= limit
}
