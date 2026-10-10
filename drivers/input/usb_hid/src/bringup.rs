//! Bringing one HID interface up (`plans/HID.md`): its report descriptor
//! read and parsed, its protocol chosen, its idle rate set to report on
//! change, and its features configured.
//!
//! An interface whose descriptor does not parse is read in boot protocol when
//! it is a boot keyboard or boot mouse, and refused otherwise. A request the
//! device does not implement is answered with a STALL, which leaves it as it
//! is; any other fault fails the bring-up, and the interface vanishing is
//! [`BringupError::Gone`].

use alloc::vec::Vec;

use tairix_abi::{DriverError, Errno};
use tairix_hid::{
    boot, DescriptorError, HidDevice, HidTransport, ReportDescriptor, ReportId, MAX_DESCRIPTOR,
};
use tairix_usb::transport::{read_configuration, ConfigurationError};

use crate::interface::{BootLayout, HidInterface, InterfaceError};
use crate::requests::{self, Protocol};

/// The control transfers bring-up makes on the driver's interface.
pub trait HidLink {
    /// A control-IN transfer of `data.len()` bytes, answering the bytes the
    /// device delivered.
    ///
    /// # Errors
    ///
    /// The transfer's [`Errno`]: [`Errno::NotFound`] once the interface has
    /// gone, [`Errno::EndpointStalled`] for a request the device refused.
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno>;

    /// A control-OUT transfer whose data stage carries `data`.
    ///
    /// # Errors
    ///
    /// As [`Self::control_in`].
    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno>;

    /// A control transfer with no data stage.
    ///
    /// # Errors
    ///
    /// As [`Self::control_in`].
    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno>;
}

/// A brought-up interface.
#[derive(Debug)]
pub struct Bound {
    /// The engine serving its applications.
    pub device: HidDevice,
    /// How its configuration describes it.
    pub interface: HidInterface,
    /// The protocol it reports in.
    pub protocol: Protocol,
    /// The longest report it sends: what each interrupt-IN request names.
    pub longest_input: usize,
    /// Its report descriptor as delivered, for the driver's log; empty when it
    /// delivered none.
    pub descriptor: Vec<u8>,
}

/// Why an interface could not be brought up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BringupError {
    /// The interface went while being brought up.
    Gone,
    /// A control transfer failed.
    Transfer(Errno),
    /// The configuration descriptor does not describe the interface.
    Interface(InterfaceError),
    /// Its report descriptor does not parse, and it has no boot layout.
    Descriptor(DescriptorError),
    /// It carries no application the seat serves.
    NothingServed,
    /// Memory for its descriptors or model ran out.
    OutOfMemory,
}

/// A transfer's failure: the interface gone, or a fault.
fn failed(errno: Errno) -> BringupError {
    if errno == Errno::NotFound {
        BringupError::Gone
    } else {
        BringupError::Transfer(errno)
    }
}

/// An optional request's outcome: a STALL is the device declining it.
fn optional(result: Result<(), Errno>) -> Result<(), BringupError> {
    match result {
        Ok(()) | Err(Errno::EndpointStalled) => Ok(()),
        Err(errno) => Err(failed(errno)),
    }
}

/// A model's failure: memory, or the descriptor itself.
fn unmodelled(error: DescriptorError) -> BringupError {
    if error == DescriptorError::OutOfMemory {
        BringupError::OutOfMemory
    } else {
        BringupError::Descriptor(error)
    }
}

/// `len` zeroed bytes, or [`BringupError::OutOfMemory`].
fn zeroed(len: usize) -> Result<Vec<u8>, BringupError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| BringupError::OutOfMemory)?;
    bytes.resize(len, 0);
    Ok(bytes)
}

/// Bring interface `number` up over `link`.
///
/// # Errors
///
/// [`BringupError`].
pub fn bring_up(link: &mut dyn HidLink, number: u8) -> Result<Bound, BringupError> {
    let config =
        read_configuration(&mut |setup, data| link.control_in(setup, data)).map_err(|error| {
            match error {
                ConfigurationError::Transfer(errno) => failed(errno),
                ConfigurationError::Malformed => BringupError::Interface(InterfaceError::Malformed),
                ConfigurationError::OutOfMemory => BringupError::OutOfMemory,
            }
        })?;
    let interface = HidInterface::find(&config, number).map_err(BringupError::Interface)?;
    let descriptor = read_report_descriptor(link, number, interface.report_descriptor_len)?;
    let (mut protocol, mut model) = match ReportDescriptor::parse(&descriptor) {
        Ok(model) => (Protocol::Report, model),
        Err(error) => {
            let layout = interface.boot_layout().ok_or_else(|| unmodelled(error))?;
            (Protocol::Boot, boot_model(layout)?)
        }
    };
    optional(link.control_no_data(requests::set_protocol(number, protocol)))?;
    if let (Protocol::Report, Some(layout)) = (protocol, interface.boot_layout()) {
        if answers_boot(link, number)? {
            protocol = Protocol::Boot;
            model = boot_model(layout)?;
        }
    }
    optional(link.control_no_data(requests::set_idle(number)))?;
    let mut device = HidDevice::new(model).ok_or(BringupError::NothingServed)?;
    device
        .configure(&mut Features {
            link,
            interface: number,
        })
        .map_err(|error| failed(error.as_errno()))?;
    let longest_input = device.longest_input();
    Ok(Bound {
        device,
        interface,
        protocol,
        longest_input,
        descriptor,
    })
}

fn boot_model(layout: BootLayout) -> Result<ReportDescriptor, BringupError> {
    match layout {
        BootLayout::Keyboard => boot::keyboard(),
        BootLayout::Mouse => boot::mouse(),
    }
    .map_err(unmodelled)
}

/// The report descriptor the interface states, as delivered: empty when it
/// states none, one past the parser's bound, or refuses the read.
fn read_report_descriptor(
    link: &mut dyn HidLink,
    number: u8,
    stated: Option<u16>,
) -> Result<Vec<u8>, BringupError> {
    let Some(len) = stated.filter(|&len| len != 0 && usize::from(len) <= MAX_DESCRIPTOR) else {
        return Ok(Vec::new());
    };
    let mut descriptor = zeroed(usize::from(len))?;
    match link.control_in(
        requests::get_report_descriptor(number, len),
        &mut descriptor,
    ) {
        Ok(read) => {
            descriptor.truncate(read);
            Ok(descriptor)
        }
        Err(Errno::EndpointStalled) => Ok(Vec::new()),
        Err(errno) => Err(failed(errno)),
    }
}

/// Whether a boot-subclass device says it stayed in boot protocol. The
/// request is optional, so one it declines leaves the protocol it was asked
/// for.
fn answers_boot(link: &mut dyn HidLink, number: u8) -> Result<bool, BringupError> {
    let mut answer = [0u8; 1];
    match link.control_in(requests::get_protocol(number), &mut answer) {
        Ok(1) => Ok(answer[0] == Protocol::Boot as u8),
        Ok(_) | Err(Errno::EndpointStalled) => Ok(false),
        Err(errno) => Err(failed(errno)),
    }
}

/// The interface's feature reports, as the device engine reads and writes
/// them.
struct Features<'a> {
    link: &'a mut dyn HidLink,
    interface: u8,
}

/// A feature request's failure as the engine reads it: the device gone, or
/// declining.
fn refusal(errno: Errno) -> DriverError {
    if errno == Errno::NotFound {
        DriverError::NotFound
    } else {
        DriverError::Unsupported
    }
}

impl HidTransport for Features<'_> {
    fn get_feature(&mut self, id: ReportId, report: &mut [u8]) -> Result<usize, DriverError> {
        let len = u16::try_from(report.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        let setup = requests::get_feature(self.interface, id.id().unwrap_or(0), len);
        self.link.control_in(setup, report).map_err(refusal)
    }

    fn set_feature(&mut self, id: ReportId, report: &[u8]) -> Result<(), DriverError> {
        let len = u16::try_from(report.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        let setup = requests::set_feature(self.interface, id.id().unwrap_or(0), len);
        self.link.control_out(setup, report).map_err(refusal)
    }
}

#[cfg(test)]
#[path = "bringup_tests.rs"]
mod tests;
