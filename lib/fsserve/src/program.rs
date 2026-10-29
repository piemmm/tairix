//! The program every served format's driver runs (`plans/FSSERVE.md` §2):
//! read the grant, open the volume, offer it to the kernel, then answer the
//! kernel's calls and publish on the commit timer until the volume leaves.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::FilesystemWrite;
use tairix_abi::driver::{DriverExit, DriverHandle};
use tairix_abi::fsserve::{ServeFrame, Span, FSSERVE_REPLY_MAX, FSSERVE_REQUEST_MAX};
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::Time64;
use tairix_abi::volume::{VolumeCtl, VolumeInfo, VOLUME_INFO_LEN};
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::Errno;
use tairix_drvrt::exit::{end, error, step};
use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

use tairix_rt::shm::ServeWindow;

use crate::{
    Clock, GrantedVolume, ServedFilesystem, ServedFormat, ServedHost, Server, VolumeAccess, Window,
    WritebackBudget, REQUIRED_CAPABILITIES,
};

/// The kernel's calls on the volume a grant names.
struct RtVolume(u64);

impl VolumeAccess for RtVolume {
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), Errno> {
        tairix_rt::volume_read(self.0, lba, buf)
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), Errno> {
        tairix_rt::volume_write(self.0, lba, buf)
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        let mut reply = [0u8; VOLUME_INFO_LEN];
        let len = tairix_rt::volume_ctl(self.0, ctl, &mut reply)?;
        if len == 0 {
            return Ok(None);
        }
        VolumeInfo::decode(reply.get(..len).ok_or(Errno::OutOfRange)?).map(Some)
    }
}

impl Window for ServeWindow {
    fn span(&mut self, span: Span) -> Option<&mut [u8]> {
        self.span_mut(span.range())
    }
}

/// The kernel's clocks.
struct RtClock;

impl Clock for RtClock {
    fn now_ns(&self) -> Option<u64> {
        Some(tairix_rt::clock_get())
    }

    fn wall_time(&self) -> Option<Time64> {
        tairix_rt::wall_time().ok()?.stamp()
    }
}

const ENDPOINT_TOKEN: u64 = 0;
const PRESSURE_TOKEN: u64 = 1;

/// Serve the volume this driver's grant names in format `F`, answering the
/// status the driver ends with.
#[must_use]
pub fn run<F: ServedFormat>() -> i32 {
    let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
    let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
        return end(DriverExit::NoHost, &[]);
    };
    let Some(volume) = host.resources().find_map(HwResource::volume_id) else {
        return end(
            DriverExit::NoResources,
            &[step("the node grants no volume")],
        );
    };
    let Ok(handle) = DriverHandle::from_raw(volume) else {
        return end(
            DriverExit::NoResources,
            &[step("the volume grant names nothing")],
        );
    };
    let granted = match GrantedVolume::open(RtVolume(volume)) {
        Ok(granted) => granted,
        Err(err) => {
            return end(
                DriverExit::BringupFailed,
                &[step("the volume could not be described"), error(err)],
            )
        }
    };
    // The wake goes in first, so the bound below reads the band in force
    // rather than its fail-closed unknown.
    let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
        return end(
            DriverExit::NoService,
            &[step("the serve wait set could not be made")],
        );
    };
    if !tairix_procinfo::pressure::watch(set, PRESSURE_TOKEN) {
        return end(
            DriverExit::NoService,
            &[step("the memory-pressure wake was refused")],
        );
    }
    let budget = WritebackBudget {
        backing: tairix_rt::boot_facts()
            .ok()
            .and_then(|facts| usize::try_from(facts.memory_bytes).ok())
            .unwrap_or(0),
        gauge: tairix_rt::pressure::gauge(),
    };
    let read_only = granted.info().read_only;
    let mut opened = match F::open(granted, read_only, budget) {
        Ok(opened) => opened,
        Err(why) => {
            // The kernel retracts the volume with this reason on record.
            let _ = tairix_rt::volume_serve(volume, &ServeFrame::Refuse(why));
            return end(
                DriverExit::BringupFailed,
                &[step("the format would not open the volume"), error(why)],
            );
        }
    };
    // Kept for the life of the process, which reports its commit deadline here.
    let Some(host) =
        tairix_util::fallible::collected(1, core::iter::once(ServedHost::new(RtClock)))
    else {
        return end(
            DriverExit::NoService,
            &[step("the volume host could not be allocated")],
        );
    };
    let reports: &'static [ServedHost<RtClock>] = Vec::leak(host);
    let Some(reports) = reports.first() else {
        return end(
            DriverExit::NoService,
            &[step("the volume host could not be allocated")],
        );
    };
    opened.fs.set_volume_host(handle, reports);
    let offer = opened.offer();
    let Ok(Some((endpoint, mut window))) =
        tairix_rt::volume_serve(volume, &ServeFrame::Serve(offer))
    else {
        return end(
            DriverExit::NoService,
            &[step("the kernel did not take the volume")],
        );
    };
    if tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Endpoint,
        endpoint,
        ENDPOINT_TOKEN,
    ) != 0
    {
        return end(
            DriverExit::NoService,
            &[step("the serving endpoint could not be watched")],
        );
    }
    let Ok(server) = Server::new(opened.fs, reports) else {
        return end(
            DriverExit::NoService,
            &[step("the server could not be built")],
        );
    };
    serve(server, reports, set, endpoint, &mut window)
}

/// Answer the kernel's calls on `endpoint` and publish on the commit timer
/// until the volume leaves, answering the status the driver ends with.
fn serve<F: ServedFilesystem>(
    mut server: Server<'_, F, RtClock>,
    reports: &ServedHost<RtClock>,
    set: u64,
    endpoint: u64,
    window: &mut ServeWindow,
) -> i32 {
    let (Some(mut request), Some(mut reply)) = (
        tairix_util::fallible::filled(FSSERVE_REQUEST_MAX, 0u8),
        tairix_util::fallible::filled(FSSERVE_REPLY_MAX, 0u8),
    ) else {
        return end(
            DriverExit::NoService,
            &[step("the call buffers could not be allocated")],
        );
    };
    let timed_out = -i64::from(Errno::TimedOut.as_i32());
    loop {
        let now = tairix_rt::clock_get();
        if reports.take_due(now) {
            // A publish that fails leaves its transaction for the next
            // operation, exactly as an in-kernel volume's does.
            let _ = server.publish();
            continue;
        }
        let timeout_ns = reports.due().map_or(u64::MAX, |at| at.saturating_sub(now));
        let mut token = 0u64;
        let status = tairix_rt::waitset_wait(set, timeout_ns, &mut token);
        if status == timed_out {
            continue;
        }
        if status < 0 {
            return end(
                DriverExit::ServeFailed,
                &[step("the serve wait set faulted")],
            );
        }
        match token {
            ENDPOINT_TOKEN => loop {
                match tairix_rt::call_recv_ready(endpoint, &mut request) {
                    Ok(Some(call)) => {
                        let len = server.answer(&request[..call.len], window, &mut reply);
                        // A caller the kernel withdrew is answered by nobody.
                        let _ = tairix_rt::call_reply(endpoint, call.ticket, &reply[..len]);
                    }
                    Ok(None) => break,
                    // The kernel withdrew the volume, and its endpoint with it.
                    Err(Errno::NotFound) => return end(DriverExit::Removed, &[]),
                    Err(_) => {
                        return end(
                            DriverExit::ServeFailed,
                            &[step("a call could not be received")],
                        )
                    }
                }
            },
            PRESSURE_TOKEN => {
                let _ = tairix_procinfo::pressure::refresh();
            }
            _ => {}
        }
    }
}
