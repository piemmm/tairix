//! The freestanding `Run` program: map the interface's transport, open the
//! function over it, bind a port for every stream it can start, and serve the
//! device channel for the life of the device.
//!
//! Every request is a blocking URB call the host controller answers, and the
//! serve loop parks on the stream ports between periods, so nothing spins.

use tairix_abi::usb_urb::{
    iso_notify_endpoint_for, IsoGrant, IsoStartParams, UsbSpeed, ISO_MAX_SLOTS, ISO_NOTIFY_LEN,
};
use tairix_abi::{DriverError, Errno, HwProperty, Origin, ProcId};
use tairix_audiochan::{exit, fail, Wake};
use tairix_caps::CapabilitySet;
use tairix_drv_audio_usb_uac::engine::{UacTransport, UsbAudio};
use tairix_drv_audio_usb_uac::REQUIRED_CAPS;
use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
use tairix_usb::device::CTRL_DATA_LEN;
use tairix_usb::transport::{read_configuration, UrbCall, UrbClient, UrbLink};

/// Stream endpoints one function may name: every endpoint number, each way.
const STREAM_ENDPOINTS: usize = 32;

/// The slot an endpoint address takes in the per-endpoint tables.
fn slot_of(endpoint: u8) -> usize {
    usize::from(endpoint & 0x0F) * 2 + usize::from(endpoint >> 7)
}

/// The class driver's call to its interface's URB endpoint.
struct IpcUrbCall {
    endpoint: u64,
}

impl UrbCall for IpcUrbCall {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(self.endpoint, request, reply).map_err(Errno::from_syscall)
    }
}

/// This process's mapping of one stream's region.
struct Region {
    base: u64,
    len: usize,
    bytes: &'static mut [u8],
}

impl Drop for Region {
    fn drop(&mut self) {
        let _ = tairix_rt::shm_unmap(self.base, self.len);
    }
}

/// The live transport: the URB link, the regions of the streams running, and
/// the ports their notices arrive on.
struct Live {
    link: UrbLink<'static, IpcUrbCall>,
    regions: [Option<Region>; STREAM_ENDPOINTS],
    ports: [Option<u64>; STREAM_ENDPOINTS],
}

impl Live {
    /// Map the region `grant` delegated for a stream of `len` bytes.
    fn map(grant: &IsoGrant, len: usize) -> Result<Region, Errno> {
        let mut mapped_len = 0u64;
        let mapped = tairix_rt::shm_map_from(grant.region_grant, grant.grantor, &mut mapped_len);
        let base = u64::try_from(mapped).map_err(|_| Errno::from_syscall(mapped))?;
        let (Ok(address), Ok(mapped_len)) = (usize::try_from(base), usize::try_from(mapped_len))
        else {
            return Err(Errno::DeviceFault);
        };
        if address == 0 || mapped_len < len || address.checked_add(mapped_len).is_none() {
            let _ = tairix_rt::shm_unmap(base, mapped_len);
            return Err(Errno::BufferTooSmall);
        }
        // SAFETY: the kernel mapped `mapped_len` bytes read-write at `address`
        // into this process, and the slice covers the first `len` of them.
        // The mapping lives until this `Region` drops and unmaps it, and the
        // per-endpoint table holds one region an endpoint, dropping the old
        // before the new is stored, so no other slice in this process covers
        // these bytes. The host controller maps the same frames: a slot is
        // written only while it is free and read only once it is reported
        // finished, which is what orders the two sides. The address arrives
        // as an integer, so its provenance is the mapping's exposed one.
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::with_exposed_provenance_mut::<u8>(address),
                len,
            )
        };
        Ok(Region {
            base,
            len: mapped_len,
            bytes,
        })
    }
}

impl UacTransport for Live {
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        self.link.control_in(setup, data)
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        self.link.control_out(setup, data)
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), Errno> {
        self.link.client_mut().claim_interface(interface)
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<(), Errno> {
        self.link.client_mut().set_interface(interface, alternate)
    }

    fn iso_start(&mut self, params: IsoStartParams) -> Result<IsoGrant, Errno> {
        let slot = slot_of(params.endpoint);
        if self.ports.get(slot).copied().flatten().is_none() {
            // A stream whose notices nothing would read must not start.
            return Err(Errno::NotFound);
        }
        self.regions[slot] = None;
        let grant = self.link.client_mut().iso_start(params)?;
        match Self::map(&grant, params.layout.region_len()) {
            Ok(region) => {
                self.regions[slot] = Some(region);
                Ok(grant)
            }
            Err(errno) => {
                let _ = self.link.client_mut().iso_stop(params.endpoint);
                Err(errno)
            }
        }
    }

    fn iso_queue(&mut self, endpoint: u8, slot: u16) -> Result<(), Errno> {
        self.link.client_mut().iso_queue(endpoint, slot)
    }

    fn iso_stop(&mut self, endpoint: u8) -> Result<(), Errno> {
        let stopped = self.link.client_mut().iso_stop(endpoint);
        if let Some(region) = self.regions.get_mut(slot_of(endpoint)) {
            *region = None;
        }
        stopped
    }

    fn region(&mut self, endpoint: u8) -> Option<&mut [u8]> {
        self.regions
            .get_mut(slot_of(endpoint))?
            .as_mut()
            .map(|region| &mut *region.bytes)
    }

    fn next_notice(&mut self) -> Result<Option<(ProcId, [u8; ISO_NOTIFY_LEN])>, Errno> {
        for port in self.ports.iter().flatten() {
            loop {
                let mut frame = [0u8; ISO_NOTIFY_LEN];
                let mut origin = [0u8; tairix_abi::ORIGIN_WIRE_LEN];
                match tairix_rt::ipc_recv(*port, &mut frame, &mut origin) {
                    Ok(ISO_NOTIFY_LEN) => {
                        // A sender the kernel attests but this side cannot
                        // read is nobody to believe.
                        let Ok(sender) = Origin::from_bytes(&origin) else {
                            continue;
                        };
                        return Ok(Some((sender.proc_id(), frame)));
                    }
                    // A message of any other length is no notice; it is gone
                    // from the port either way.
                    Ok(_) => {}
                    Err(code) if Errno::from_syscall(code) == Errno::WouldBlock => break,
                    Err(code) => return Err(Errno::from_syscall(code)),
                }
            }
        }
        Ok(None)
    }

    fn now_ns(&self) -> u64 {
        tairix_rt::clock_get()
    }
}

fn driver_caps() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    for &cap in REQUIRED_CAPS {
        caps.insert(cap);
    }
    caps
}

/// The transport, its interface and speed, and the URB endpoint its host
/// controller serves.
type Transport = (UrbLink<'static, IpcUrbCall>, u8, UsbSpeed, u64);

/// Map the interface's transport from the grants its node carries.
fn map_transport() -> Result<Transport, i32> {
    let Ok(host) = RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None) else {
        return Err(fail(
            exit::NO_HOST,
            "usb-audio: the driver host could not be built",
            None,
        ));
    };
    let speed = host
        .property(HwProperty::UsbSpeed)
        .and_then(|byte| u8::try_from(byte).ok())
        .and_then(|byte| UsbSpeed::from_u8(byte).ok());
    let (Some(endpoint), Some(interface), Some(speed), Ok(shm)) = (
        host.endpoint_grant(),
        host.property(HwProperty::UsbInterface)
            .and_then(|number| u8::try_from(number).ok()),
        speed,
        host.shared_buffer(CTRL_DATA_LEN),
    ) else {
        return Err(fail(
            exit::NO_RESOURCES,
            "usb-audio: the node carries no URB endpoint, shared buffer, interface or speed",
            None,
        ));
    };
    let client = UrbClient::new(IpcUrbCall { endpoint });
    Ok((UrbLink::new(client, shm), interface, speed, endpoint))
}

fn main() -> i32 {
    let (mut link, interface, speed, urb_endpoint) = match map_transport() {
        Ok(transport) => transport,
        Err(code) => return code,
    };
    let Ok(config) = read_configuration(&mut |setup, data| link.control_in(setup, data)) else {
        return fail(
            exit::BRINGUP_FAILED,
            "usb-audio: the configuration descriptor could not be read whole",
            None,
        );
    };
    let live = Live {
        link,
        regions: [const { None }; STREAM_ENDPOINTS],
        ports: [None; STREAM_ENDPOINTS],
    };
    let mut audio = match UsbAudio::open(&config, interface, speed, live) {
        Ok(audio) => audio,
        Err(err) => {
            return fail(
                exit::BRINGUP_FAILED,
                "usb-audio: the audio function could not be opened",
                Some(err),
            )
        }
    };
    let Ok(origin) = tairix_rt::self_origin() else {
        return fail(
            exit::NO_HOST,
            "usb-audio: this process's identity could not be read",
            None,
        );
    };
    let mut wanted = [false; STREAM_ENDPOINTS];
    for endpoint in audio.stream_endpoints() {
        wanted[slot_of(endpoint)] = true;
    }
    let mut wakes = [Wake::Port(0); STREAM_ENDPOINTS];
    let mut count = 0usize;
    let capacity = usize::from(ISO_MAX_SLOTS) + 1;
    for (slot, _) in wanted.iter().enumerate().filter(|(_, wanted)| **wanted) {
        // The table's slot is the address it came from: number, then the
        // direction bit.
        let Ok(number) = u8::try_from(slot / 2) else {
            continue;
        };
        let endpoint = number | if slot % 2 == 1 { 0x80 } else { 0 };
        let port = iso_notify_endpoint_for(origin.pid(), endpoint);
        if tairix_rt::port_bind(port, ISO_NOTIFY_LEN, capacity) != 0 {
            return fail(
                exit::BRINGUP_FAILED,
                "usb-audio: a stream's notify port could not be bound",
                Some(DriverError::Busy),
            );
        }
        // The port's id is derived from this process's pid, so only the host
        // controller serving the URB endpoint is admitted to it: no one else
        // can fill it and starve a stream of its notices.
        if let Err(errno) = tairix_rt::port_admit(port, urb_endpoint) {
            return fail(
                exit::BRINGUP_FAILED,
                "usb-audio: a stream's notify port could not be restricted to the host controller",
                Some(DriverError::from_errno(errno)),
            );
        }
        audio.transport_mut().ports[slot] = Some(port);
        wakes[count] = Wake::Port(port);
        count += 1;
    }
    tairix_audiochan::serve(audio, &wakes[..count])
}

tairix_rt::entry!(main);
