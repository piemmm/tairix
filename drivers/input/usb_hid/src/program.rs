//! The freestanding `Run` program: map the interface's transport, bring the
//! interface up, and serve its reports for the life of the device.
//!
//! Each report read is a blocking URB call the host-controller driver answers
//! when the device reports, so the driver parks between reports. Every record
//! goes to the boot seat; a device that goes, or keeps faulting, is let go of
//! first, so nothing it held stays pressed.

use tairix_abi::input::{KeyInput, PointerInput};
use tairix_abi::seat::SEAT_PRIMARY;
use tairix_abi::touch::TouchFrame;
use tairix_abi::{DriverError, Errno, HwProperty, LOG_FIELD_VALUE_MAX};
use tairix_caps::CapabilitySet;
use tairix_drv_input_usb_hid::bringup::{bring_up, Bound, BringupError, HidLink};
use tairix_drv_input_usb_hid::REQUIRED_CAPS;
use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
use tairix_hid::{pump_error_limit_reached, transport_error, Decoded, SeatSink};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::LogSink;
use tairix_usb::device::INT_TRANSFER_MAX;
use tairix_usb::transport::{UrbCall, UrbClient, UrbLink};
use tairix_util::fmt::{format_hex_bytes, format_hex_u64};

/// The driver host could not be built from the kernel-delivered grants.
const EXIT_NO_HOST: i32 = 80;
/// The node carried no URB endpoint, shared buffer or interface number.
const EXIT_NO_TRANSPORT: i32 = 81;
/// The interface could not be brought up.
const EXIT_BRINGUP_FAILED: i32 = 82;
/// The interface kept faulting and was given up.
const EXIT_DEVICE_FAULT: i32 = 83;

/// The interface is up and its reports are being served.
const HID_READY: EventId = EventId(4244);
/// One slice of the interface's report descriptor, in hex.
const HID_DESCRIPTOR: EventId = EventId(4245);
/// The interface could not be brought up.
const HID_REFUSED: EventId = EventId(4246);
/// A report read faulted.
const HID_FAULT: EventId = EventId(4247);
/// The interface went; everything it held was released.
const HID_DETACHED: EventId = EventId(4248);

/// Consecutive report faults ridden out before the device is given up.
const MAX_CONSECUTIVE_FAULTS: u8 = 4;

/// Descriptor bytes per log record, two hex characters each.
const DESCRIPTOR_SLICE: usize = 64;

const _: () = assert!(DESCRIPTOR_SLICE * 2 <= LOG_FIELD_VALUE_MAX);

/// The class driver's call to its interface's URB endpoint.
struct IpcUrbCall {
    endpoint: u64,
}

impl UrbCall for IpcUrbCall {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(self.endpoint, request, reply).map_err(Errno::from_syscall)
    }
}

/// The interface's control transfers, over its URB link.
struct Link(UrbLink<'static, IpcUrbCall>);

impl HidLink for Link {
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        self.0.control_in(setup, data)
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        self.0.control_out(setup, data)
    }

    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        self.0.control_no_data(setup)
    }
}

/// The boot seat. A record the seat refuses is its decision, and the device
/// carries on with the next.
struct Seat;

impl SeatSink for Seat {
    fn key(&mut self, record: &KeyInput) -> Result<(), DriverError> {
        let _ = tairix_rt::key_inject(SEAT_PRIMARY, record);
        Ok(())
    }

    fn pointer(&mut self, record: &PointerInput) -> Result<(), DriverError> {
        let _ = tairix_rt::pointer_inject(SEAT_PRIMARY, record);
        Ok(())
    }

    fn touch(&mut self, frame: &TouchFrame) -> Result<(), DriverError> {
        let _ = tairix_rt::touch_inject(SEAT_PRIMARY, frame);
        Ok(())
    }
}

fn driver_caps() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    for &cap in REQUIRED_CAPS {
        caps.insert(cap);
    }
    caps
}

fn log_event(id: EventId, level: Level, message: &'static str, fields: &[Field<'_>]) {
    log(
        &LogSink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

fn log_hex(id: EventId, level: Level, message: &'static str, key: &'static str, value: u64) {
    let mut hex = [0u8; 16];
    log_event(
        id,
        level,
        message,
        &[Field {
            key,
            value: FieldValue::Str(format_hex_u64(value, &mut hex)),
        }],
    );
}

/// The interface's transport: its URB link and interface number.
fn map_transport() -> Result<(Link, u8), i32> {
    let Ok(host) = RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None) else {
        return Err(EXIT_NO_HOST);
    };
    let (Some(endpoint), Some(interface), Ok(shm)) = (
        host.endpoint_grant(),
        host.property(HwProperty::UsbInterface)
            .and_then(|number| u8::try_from(number).ok()),
        host.shared_buffer(INT_TRANSFER_MAX),
    ) else {
        return Err(EXIT_NO_TRANSPORT);
    };
    let client = UrbClient::new(IpcUrbCall { endpoint });
    Ok((Link(UrbLink::new(client, shm)), interface))
}

/// Why bring-up failed, for the log.
const fn refusal(error: BringupError) -> (&'static str, u64) {
    match error {
        BringupError::Gone => ("usb-hid: the interface went while being brought up", 0),
        BringupError::Transfer(errno) => ("usb-hid: a control transfer failed", errno as u64),
        BringupError::Interface(_) => (
            "usb-hid: the configuration does not describe the interface",
            0,
        ),
        BringupError::Descriptor(error) => (
            "usb-hid: the report descriptor does not parse and the interface has no boot layout",
            error as u64,
        ),
        BringupError::NothingServed => {
            ("usb-hid: the interface carries nothing the seat serves", 0)
        }
        BringupError::OutOfMemory => ("usb-hid: memory for the interface ran out", 0),
    }
}

/// Log the report descriptor, the evidence a capture of an unfamiliar device
/// needs, a slice per record.
fn log_descriptor(descriptor: &[u8]) {
    for (index, slice) in descriptor.chunks(DESCRIPTOR_SLICE).enumerate() {
        let mut hex = [0u8; DESCRIPTOR_SLICE * 2];
        log_event(
            HID_DESCRIPTOR,
            Level::Info,
            "usb-hid: report descriptor",
            &[
                Field {
                    key: "offset",
                    value: FieldValue::UnsignedInt((index * DESCRIPTOR_SLICE) as u64),
                },
                Field {
                    key: "bytes",
                    value: FieldValue::Str(format_hex_bytes(slice, &mut hex)),
                },
            ],
        );
    }
}

fn log_ready(bound: &Bound) {
    let applications = bound.device.applications();
    let u = |value: usize| FieldValue::UnsignedInt(value as u64);
    log_event(
        HID_READY,
        Level::Info,
        "usb-hid: interface up, serving reports",
        &[
            Field {
                key: "report_protocol",
                value: FieldValue::Bool(
                    bound.protocol == tairix_drv_input_usb_hid::requests::Protocol::Report,
                ),
            },
            Field {
                key: "keyboards",
                value: u(applications.keyboards),
            },
            Field {
                key: "mice",
                value: u(applications.mice),
            },
            Field {
                key: "touchpads",
                value: u(applications.touchpads),
            },
            Field {
                key: "touchscreens",
                value: u(applications.touchscreens),
            },
            Field {
                key: "longest_report",
                value: u(bound.longest_input),
            },
        ],
    );
}

/// Serve `bound`'s reports until the interface goes or keeps faulting.
fn serve(link: &mut Link, bound: &mut Bound) -> i32 {
    let mut report = [0u8; INT_TRANSFER_MAX];
    let mut faults = 0u8;
    loop {
        let read = link.0.interrupt_in(
            bound.interface.interrupt_endpoint,
            bound.longest_input,
            &mut report,
        );
        match read {
            Ok(len) => {
                faults = 0;
                // A report an application finds cut short changes nothing it
                // holds; the device's next report carries its state again.
                let _: Result<Decoded, DriverError> = bound.device.input(&report[..len], &mut Seat);
            }
            Err(errno) if transport_error(errno) == DriverError::NotFound => {
                let _ = bound.device.release(&mut Seat);
                log_hex(
                    HID_DETACHED,
                    Level::Info,
                    "usb-hid: interface gone, released",
                    "errno_hex",
                    errno as u64,
                );
                return 0;
            }
            Err(errno) => {
                log_hex(
                    HID_FAULT,
                    Level::Warn,
                    "usb-hid: a report read faulted",
                    "errno_hex",
                    errno as u64,
                );
                if pump_error_limit_reached(&mut faults, MAX_CONSECUTIVE_FAULTS) {
                    let _ = bound.device.release(&mut Seat);
                    return EXIT_DEVICE_FAULT;
                }
            }
        }
    }
}

fn main() -> i32 {
    let (mut link, interface) = match map_transport() {
        Ok(transport) => transport,
        Err(code) => return code,
    };
    let mut bound = match bring_up(&mut link, interface) {
        Ok(bound) => bound,
        Err(error) => {
            let (message, detail) = refusal(error);
            log_hex(HID_REFUSED, Level::Error, message, "detail_hex", detail);
            return if error == BringupError::Gone {
                0
            } else {
                EXIT_BRINGUP_FAILED
            };
        }
    };
    log_descriptor(&bound.descriptor);
    log_ready(&bound);
    serve(&mut link, &mut bound)
}

tairix_rt::entry!(main);
