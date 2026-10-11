//! The network report: the interfaces a Switchboard instance read, published
//! over [`SWITCHBOARD_ENDPOINT`](super::SWITCHBOARD_ENDPOINT) each time the
//! [`Network`](crate::notice::NoticeTopic::Network) notice moves, so the
//! session's notification area can state them without reading gated state
//! itself.
//!
//! It is a frame of its own, told apart from a
//! [`SwitchboardRequest`](super::SwitchboardRequest) by its magic
//! ([`is_network_report`]). A publisher whose authority does not reach the
//! interface-state query says so ([`NetworkReport::UNMEASURED`]), and the
//! session then shows no network status at all rather than a fabricated
//! "offline". It carries only what that query answers, so stating the network
//! needs no authority over the interfaces' hardware facts. A decode fails
//! closed on the first fault: an unknown magic or version, a set reserved bit
//! or byte, more interfaces than the frame holds, interfaces omitted while it
//! names fewer than it holds, an unmeasured report naming any, or an interface
//! record its own decode refuses.

use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::net_ipc::NetInterfaceStateRecord;
use crate::Errno;

use super::SWITCHBOARD_VERSION_V1;

/// Magic number identifying a network-report frame (`"SWN1"` little-endian).
pub const NETWORK_REPORT_MAGIC: u32 = u32::from_le_bytes(*b"SWN1");

/// Most interfaces one report names: a validation bound on the frame, not a
/// capacity. A report counts the interfaces beyond it as omitted.
pub const NETWORK_REPORT_INTERFACES_MAX: usize = 8;

/// Whether `bytes` opens a network-report frame rather than a request.
#[must_use]
pub fn is_network_report(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && read_u32(bytes, 0) == NETWORK_REPORT_MAGIC
}

/// The interfaces one Switchboard instance could read, or that it could not.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NetworkReport {
    measured: bool,
    count: u8,
    omitted: u16,
    interfaces: [Option<NetInterfaceStateRecord>; NETWORK_REPORT_INTERFACES_MAX],
}

/// Header: magic (4), version (2), flags (1), count (1), omitted (2),
/// reserved (2).
const HEADER_LEN: usize = 12;

/// The flag set on a report whose interfaces were read.
const FLAG_MEASURED: u8 = 1;

impl NetworkReport {
    /// A publisher whose authority does not reach the interface queries.
    pub const UNMEASURED: Self = Self {
        measured: false,
        count: 0,
        omitted: 0,
        interfaces: [None; NETWORK_REPORT_INTERFACES_MAX],
    };

    /// Encoded width of every report, measured or not.
    pub const WIRE_LEN: usize =
        HEADER_LEN + NETWORK_REPORT_INTERFACES_MAX * NetInterfaceStateRecord::WIRE_LEN;

    /// A report naming `interfaces`, in the order the stack lists them. Those
    /// beyond [`NETWORK_REPORT_INTERFACES_MAX`] are counted as omitted.
    #[must_use]
    pub fn measured(interfaces: &[NetInterfaceStateRecord]) -> Self {
        let named = interfaces.len().min(NETWORK_REPORT_INTERFACES_MAX);
        let mut slots = [None; NETWORK_REPORT_INTERFACES_MAX];
        for (slot, interface) in slots.iter_mut().zip(interfaces) {
            *slot = Some(*interface);
        }
        Self {
            measured: true,
            // Bounded by NETWORK_REPORT_INTERFACES_MAX (8).
            count: u8::try_from(named).unwrap_or(u8::MAX),
            omitted: u16::try_from(interfaces.len() - named).unwrap_or(u16::MAX),
            interfaces: slots,
        }
    }

    /// Whether the publisher could read the interfaces at all.
    #[must_use]
    pub const fn is_measured(&self) -> bool {
        self.measured
    }

    /// The interfaces the report names, each with its link and addresses.
    pub fn interfaces(&self) -> impl Iterator<Item = &NetInterfaceStateRecord> {
        self.interfaces.iter().flatten()
    }

    /// How many interfaces the stack listed beyond those named.
    #[must_use]
    pub const fn omitted(&self) -> u16 {
        self.omitted
    }

    /// Encode `self` little-endian; unnamed slots are zero.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, NETWORK_REPORT_MAGIC);
        put_u16(&mut out, 4, SWITCHBOARD_VERSION_V1);
        out[6] = if self.measured { FLAG_MEASURED } else { 0 };
        out[7] = self.count;
        put_u16(&mut out, 8, self.omitted);
        for (index, interface) in self.interfaces().enumerate() {
            let at = HEADER_LEN + index * NetInterfaceStateRecord::WIRE_LEN;
            out[at..at + NetInterfaceStateRecord::WIRE_LEN]
                .copy_from_slice(&interface.to_le_bytes());
        }
        out
    }

    /// Decode a report, failing closed on the first fault.
    ///
    /// # Errors
    ///
    /// - [`Errno::LengthOutOfRange`] for a frame that is not exactly
    ///   [`Self::WIRE_LEN`] bytes, or names more interfaces than it holds.
    /// - [`Errno::BadMagic`] for another magic, and
    ///   [`Errno::AbiVersionUnsupported`] for another version.
    /// - [`Errno::OutOfRange`] for a set reserved bit or byte, an unmeasured
    ///   report naming anything, interfaces omitted while fewer than
    ///   [`NETWORK_REPORT_INTERFACES_MAX`] are named, or a slot past the count
    ///   that is not zero.
    /// - Whatever an interface record's own decode refuses.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != Self::WIRE_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        if read_u32(bytes, 0) != NETWORK_REPORT_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != SWITCHBOARD_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let flags = bytes[6];
        if flags & !FLAG_MEASURED != 0 || bytes[10..HEADER_LEN] != [0; 2] {
            return Err(Errno::OutOfRange);
        }
        let measured = flags == FLAG_MEASURED;
        let count = bytes[7];
        let omitted = read_u16(bytes, 8);
        if usize::from(count) > NETWORK_REPORT_INTERFACES_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !measured && (count != 0 || omitted != 0) {
            return Err(Errno::OutOfRange);
        }
        if omitted != 0 && usize::from(count) < NETWORK_REPORT_INTERFACES_MAX {
            return Err(Errno::OutOfRange);
        }
        let mut interfaces = [None; NETWORK_REPORT_INTERFACES_MAX];
        for (index, slot) in interfaces.iter_mut().enumerate() {
            let at = HEADER_LEN + index * NetInterfaceStateRecord::WIRE_LEN;
            let entry = &bytes[at..at + NetInterfaceStateRecord::WIRE_LEN];
            if index < usize::from(count) {
                *slot = Some(NetInterfaceStateRecord::from_bytes(entry)?);
            } else if entry.iter().any(|&byte| byte != 0) {
                return Err(Errno::OutOfRange);
            }
        }
        Ok(Self {
            measured,
            count,
            omitted,
            interfaces,
        })
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
