//! Fail-closed configuration-descriptor reader: which storage interface,
//! wire transport, and endpoints this class driver drives.
//!
//! The interface node the host-controller driver emits carries the device's
//! `vid:pid:class` identity but not its endpoint numbers, and the URB
//! transport's servers refuse any endpoint that is not one of the
//! interface's *configured* endpoints. The class driver therefore reads the
//! device's own configuration descriptor over control-IN (the standard
//! `GET_DESCRIPTOR` every USB device serves) and derives the same facts the
//! host-controller driver derived at enumeration — the mass-storage
//! interface's number (the `wIndex` of the class requests), its wire
//! transport (the interface protocol byte), its command set (the sub-class
//! byte), and the endpoints that transport uses — from the descriptor
//! stream, never assumed (USB 2.0 §9.6):
//!
//! * **BOT** (protocol `0x50`): the first bulk-IN + bulk-OUT pair.
//! * **CBI** (protocol `0x00`): the bulk pair plus the interrupt-IN
//!   completion endpoint.
//! * **UAS** (protocol `0x62`): four bulk pipes, each named by the Pipe
//!   Usage descriptor (UAS §4.9) that follows its endpoint descriptor.
//!
//! The descriptor bytes come from the device and are hostile input: every
//! length is validated, the walk is bounded by the buffer, and a stream
//! that cannot be parsed refuses the whole device rather than guessing.

use tairix_abi::Errno;
use tairix_usb::descriptor::{
    descriptors, ConfigurationHeader, Malformed, DESC_TYPE_ENDPOINT, DESC_TYPE_INTERFACE,
    ENDPOINT_ADDR_DIR_IN, ENDPOINT_ADDR_NUMBER_MASK, ENDPOINT_ATTR_BULK, ENDPOINT_ATTR_INTERRUPT,
    ENDPOINT_ATTR_TYPE_MASK, ENDPOINT_DESCRIPTOR_LEN, INTERFACE_DESCRIPTOR_LEN,
};

use crate::scsi::CommandSet;

/// `bDescriptorType` of the UAS Pipe Usage descriptor (UAS §4.9, the
/// class-specific `CS_INTERFACE`-shaped value).
const DESC_TYPE_PIPE_USAGE: u8 = 0x24;
/// Length of a Pipe Usage descriptor (UAS §4.9).
const PIPE_USAGE_DESC_LEN: usize = 4;

/// Mass-storage interface class (USB 2.0 §9.6.5 `bInterfaceClass`).
const CLASS_MASS_STORAGE: u8 = 0x08;
/// Sub-class: SCSI transparent command set.
pub const SUBCLASS_SCSI: u8 = 0x06;
/// Sub-class: UFI (the USB floppy command set).
pub const SUBCLASS_UFI: u8 = 0x04;
/// Protocol: Bulk-Only Transport.
pub const PROTOCOL_BOT: u8 = 0x50;
/// Protocol: Control/Bulk/Interrupt with command-completion interrupt.
pub const PROTOCOL_CBI: u8 = 0x00;
/// Protocol: USB Attached SCSI.
pub const PROTOCOL_UAS: u8 = 0x62;

/// UAS Pipe Usage ids (UAS §4.9 table 8).
const PIPE_ID_COMMAND: u8 = 0x01;
const PIPE_ID_STATUS: u8 = 0x02;
const PIPE_ID_DATA_IN: u8 = 0x03;
const PIPE_ID_DATA_OUT: u8 = 0x04;

/// The four UAS pipes' endpoint numbers, by Pipe Usage id.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UasEndpoints {
    /// Command pipe (bulk-OUT).
    pub command: u8,
    /// Status pipe (bulk-IN).
    pub status: u8,
    /// Data-in pipe (bulk-IN).
    pub data_in: u8,
    /// Data-out pipe (bulk-OUT).
    pub data_out: u8,
}

/// The wire transport a matched interface speaks, with the endpoints that
/// transport uses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StorageProtocol {
    /// Bulk-Only Transport over the bulk endpoint pair.
    Bot {
        /// Endpoint number of the first bulk-IN endpoint.
        bulk_in: u8,
        /// Endpoint number of the first bulk-OUT endpoint.
        bulk_out: u8,
    },
    /// Control/Bulk/Interrupt: the bulk pair plus the completion
    /// interrupt.
    Cbi {
        /// Endpoint number of the first bulk-IN endpoint.
        bulk_in: u8,
        /// Endpoint number of the first bulk-OUT endpoint.
        bulk_out: u8,
        /// Endpoint number of the command-completion interrupt-IN
        /// endpoint.
        interrupt_in: u8,
    },
    /// USB Attached SCSI over the four named pipes.
    Uas(UasEndpoints),
}

/// The facts the class driver derives from the configuration descriptor:
/// the storage interface, its command set, and its transport endpoints.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StorageInterface {
    /// `bInterfaceNumber` — the `wIndex` of the class requests.
    pub interface_number: u8,
    /// The command set the sub-class byte declares.
    pub command_set: CommandSet,
    /// The wire transport and its endpoints.
    pub protocol: StorageProtocol,
}

/// The `(sub-class, protocol)` pairs this driver serves, mapped to the
/// command set the sub-class declares. Anything else — vendor-specific
/// sets, the interrupt-less CB variant (protocol `0x01`), ATAPI sub-classes
/// — is left for a driver that actually implements it (fail closed, never
/// half-served).
fn accepted_command_set(sub_class: u8, protocol: u8) -> Option<CommandSet> {
    match (sub_class, protocol) {
        (SUBCLASS_SCSI, PROTOCOL_BOT | PROTOCOL_UAS) => Some(CommandSet::Transparent),
        (SUBCLASS_UFI, PROTOCOL_BOT | PROTOCOL_CBI) => Some(CommandSet::Ufi),
        _ => None,
    }
}

/// One matched interface's endpoint collection state.
#[derive(Copy, Clone, Debug, Default)]
struct Collect {
    bulk_in: Option<u8>,
    bulk_out: Option<u8>,
    interrupt_in: Option<u8>,
    /// The most recent bulk endpoint (number, is-IN), awaiting the Pipe
    /// Usage descriptor that names its UAS pipe.
    pending_pipe: Option<(u8, bool)>,
    command: Option<u8>,
    status: Option<u8>,
    data_in: Option<u8>,
    data_out: Option<u8>,
}

impl Collect {
    /// The completed transport for `protocol`, if every endpoint it needs
    /// has been seen.
    fn complete(&self, protocol: u8) -> Option<StorageProtocol> {
        match protocol {
            PROTOCOL_BOT => Some(StorageProtocol::Bot {
                bulk_in: self.bulk_in?,
                bulk_out: self.bulk_out?,
            }),
            PROTOCOL_CBI => Some(StorageProtocol::Cbi {
                bulk_in: self.bulk_in?,
                bulk_out: self.bulk_out?,
                interrupt_in: self.interrupt_in?,
            }),
            PROTOCOL_UAS => Some(StorageProtocol::Uas(UasEndpoints {
                command: self.command?,
                status: self.status?,
                data_in: self.data_in?,
                data_out: self.data_out?,
            })),
            _ => None,
        }
    }

    /// Record a Pipe Usage id for the pending bulk endpoint, enforcing the
    /// pipe's direction (UAS §4.9: command/data-out ride OUT endpoints,
    /// status/data-in ride IN endpoints). A duplicate pipe id or a pipe on
    /// an endpoint of the wrong direction poisons the interface (`false`),
    /// so a malformed UAS interface is skipped, never half-assembled.
    fn assign_pipe(&mut self, pipe_id: u8) -> bool {
        let Some((number, is_in)) = self.pending_pipe.take() else {
            // A Pipe Usage descriptor with no preceding bulk endpoint;
            // tolerated (the spec also allows the interface-level form
            // that precedes the endpoints).
            return true;
        };
        let slot = match (pipe_id, is_in) {
            (PIPE_ID_COMMAND, false) => &mut self.command,
            (PIPE_ID_STATUS, true) => &mut self.status,
            (PIPE_ID_DATA_IN, true) => &mut self.data_in,
            (PIPE_ID_DATA_OUT, false) => &mut self.data_out,
            _ => return false,
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(number);
        true
    }
}

/// Find mass-storage interface `interface` (alternate setting 0) in the full
/// configuration descriptor stream and derive its transport endpoints —
/// exactly the endpoints the host-controller driver configured at
/// enumeration. `interface` is the one the driver's node names: its
/// interface requests may address no other.
///
/// # Errors
///
/// * [`Errno::BadMagic`] for a malformed configuration header
///   ([`ConfigurationHeader::decode`]).
/// * [`Errno::LengthOutOfRange`] for a stream shorter than its header
///   states, or a descriptor whose announced length is shorter than its type
///   requires or runs past the stream (a zero `bLength` cannot advance the
///   walk and is refused, never looped on).
/// * [`Errno::NotFound`] if `interface` does not complete: it is absent,
///   outside the accepted class/sub-class/protocol set, lacks the endpoints
///   its transport needs, or its UAS pipes are malformed.
pub fn find_storage_interface(stream: &[u8], interface: u8) -> Result<StorageInterface, Errno> {
    let header = ConfigurationHeader::decode(stream).map_err(|Malformed| Errno::BadMagic)?;
    let stream = stream.get(..header.total).ok_or(Errno::LengthOutOfRange)?;

    // The matched interface, once seen: (number, protocol, command set).
    // Descriptors that follow it (until the next interface descriptor)
    // belong to it. `poisoned` marks a matched interface whose UAS pipes
    // turned out malformed; its remaining descriptors are skipped.
    let mut matched: Option<(u8, u8, CommandSet)> = None;
    let mut poisoned = false;
    let mut collect = Collect::default();
    for descriptor in descriptors(&stream[header.length..]) {
        let descriptor = descriptor.map_err(|Malformed| Errno::LengthOutOfRange)?;
        let length = descriptor.len();
        match descriptor[1] {
            DESC_TYPE_INTERFACE => {
                if length < INTERFACE_DESCRIPTOR_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                // The previous matched interface's endpoint list has ended
                // without completing; keep looking for a later matching
                // interface.
                matched = None;
                poisoned = false;
                collect = Collect::default();
                let alternate_setting = descriptor[3];
                let class = descriptor[5];
                let sub_class = descriptor[6];
                let protocol = descriptor[7];
                if alternate_setting == 0
                    && descriptor[2] == interface
                    && class == CLASS_MASS_STORAGE
                {
                    if let Some(command_set) = accepted_command_set(sub_class, protocol) {
                        matched = Some((descriptor[2], protocol, command_set));
                    }
                }
            }
            DESC_TYPE_ENDPOINT => {
                if length < ENDPOINT_DESCRIPTOR_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                if matched.is_some() && !poisoned {
                    let address = descriptor[2];
                    let attributes = descriptor[3];
                    let number = address & ENDPOINT_ADDR_NUMBER_MASK;
                    let is_in = address & ENDPOINT_ADDR_DIR_IN != 0;
                    if number != 0 {
                        match attributes & ENDPOINT_ATTR_TYPE_MASK {
                            ENDPOINT_ATTR_BULK => {
                                collect.pending_pipe = Some((number, is_in));
                                if is_in {
                                    if collect.bulk_in.is_none() {
                                        collect.bulk_in = Some(number);
                                    }
                                } else if collect.bulk_out.is_none() {
                                    collect.bulk_out = Some(number);
                                }
                            }
                            ENDPOINT_ATTR_INTERRUPT if is_in && collect.interrupt_in.is_none() => {
                                collect.interrupt_in = Some(number);
                            }
                            _ => {}
                        }
                    }
                }
            }
            DESC_TYPE_PIPE_USAGE if matched.is_some() && !poisoned => {
                if length < PIPE_USAGE_DESC_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                if !collect.assign_pipe(descriptor[2]) {
                    // Malformed UAS pipes: skip this interface and keep
                    // scanning for a later servable one.
                    poisoned = true;
                }
            }
            _ => {}
        }
        if let Some((interface_number, protocol, command_set)) = matched {
            if !poisoned {
                if let Some(protocol) = collect.complete(protocol) {
                    return Ok(StorageInterface {
                        interface_number,
                        command_set,
                        protocol,
                    });
                }
            }
        }
    }
    Err(Errno::NotFound)
}

#[cfg(test)]
#[path = "desc_tests.rs"]
mod tests;
