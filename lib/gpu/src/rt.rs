//! The served device over the kernel's IPC: the serve loop a GPU's driver
//! runs on its channel, and [`Device::connect`], a client's way to it.

use alloc::boxed::Box;
use alloc::sync::Arc;

use tairix_abi::gpu::{GPU_MAX_REQUEST, GPU_REGION_MAX, GPU_REGION_MIN};
use tairix_abi::{Errno, ProcId, WaitSetOp, WaitSourceKind};
use tairix_rt::shm::{MappedGrant, SharedRegion};
use tairix_rt::sync::{Mutex, WorkerWake};

use crate::device::Device;
use crate::remote::{Link, Remote};
use crate::serve::{Channel, Region, Server};
use crate::software::Spawn;
use crate::wire::ops::ExportToken;

/// The most workers a serve loop runs requests on, whatever the processors:
/// a containment bound on the threads one driver keeps.
pub const MAX_SERVE_WORKERS: usize = 8;

// The largest region must carry the largest module, which crosses whole.
const _: () = assert!(GPU_REGION_MAX >= tairix_spirv::MAX_MODULE_BYTES);

const ENDPOINT_TOKEN: u64 = 1;
const PEER_EXIT_TOKEN: u64 = 2;
const WAKE_TOKEN: u64 = 3;

/// The kernel's channel, as the runtime serves it.
struct RtChannel {
    endpoint: u64,
    wake: WorkerWake,
}

/// A client's region, mapped from its grant.
struct RtRegion(Mutex<MappedGrant>);

impl Region for RtRegion {
    fn len(&self) -> usize {
        self.0.lock().bytes().len()
    }

    fn read(&self, at: usize, out: &mut [u8]) {
        let grant = self.0.lock();
        if let Some(bytes) = grant.bytes().get(at..at.saturating_add(out.len())) {
            out.copy_from_slice(bytes);
        }
    }

    fn write(&self, at: usize, data: &[u8]) {
        let mut grant = self.0.lock();
        if let Some(bytes) = grant.bytes_mut().get_mut(at..at.saturating_add(data.len())) {
            bytes.copy_from_slice(data);
        }
    }
}

impl Channel for RtChannel {
    fn reply(&self, ticket: u64, frame: &[u8]) {
        // A caller gone meanwhile had its call withdrawn: no one to answer.
        let _ = tairix_rt::call_reply(self.endpoint, ticket, frame);
    }

    fn map(&self, client: ProcId, grant: u64) -> Result<Arc<dyn Region>, Errno> {
        let mapped = MappedGrant::map(client, grant, GPU_REGION_MIN)?;
        crate::shared(RtRegion(Mutex::new(mapped)))
            .map(|region| region as Arc<dyn Region>)
            .map_err(|_| Errno::OutOfMemory)
    }

    fn watch(&self, client: ProcId) -> Result<(), Errno> {
        tairix_rt::peer_watch(client)
    }

    fn unwatch(&self, client: ProcId) {
        let _ = tairix_rt::peer_unwatch(client);
    }

    fn token(&self) -> Result<ExportToken, Errno> {
        let mut token = ExportToken::default();
        tairix_rt::random_fill(&mut token)?;
        Ok(token)
    }

    fn wake(&self) {
        self.wake.nudge();
    }
}

/// Serve `device` on `endpoint`, a GPU's device channel its driver bound, for
/// the life of the process: its requests run on as many as `workers` threads
/// `threads` starts, at most [`MAX_SERVE_WORKERS`].
///
/// # Errors
///
/// Why the loop could not go on: a device not in this process, a wake or
/// wait set the kernel refused, or a channel lost.
pub fn serve(device: &Device, endpoint: u64, threads: &dyn Spawn, workers: usize) -> Errno {
    let Some(local) = device.served.clone() else {
        return Errno::NotSupported;
    };
    let wake = WorkerWake::create();
    let Some(read) = wake.read_end() else {
        return Errno::OutOfMemory;
    };
    let set = match Errno::syscall_result(tairix_rt::waitset_create()) {
        Ok(set) => set,
        Err(errno) => return errno,
    };
    for (kind, id, token) in [
        (WaitSourceKind::Endpoint, endpoint, ENDPOINT_TOKEN),
        (WaitSourceKind::PeerExit, 0, PEER_EXIT_TOKEN),
        (WaitSourceKind::Stream, u64::from(read), WAKE_TOKEN),
    ] {
        let joined = tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token);
        if joined != 0 {
            return Errno::from_syscall(joined);
        }
    }
    let channel = Arc::new(RtChannel { endpoint, wake });
    let server = Server::new(Arc::clone(&local), Arc::clone(&channel) as Arc<dyn Channel>);
    let workers = server.start_workers(threads, workers.min(MAX_SERVE_WORKERS));
    let mut request = [0u8; GPU_MAX_REQUEST];
    let mut token = 0u64;
    loop {
        let timeout = server
            .deadline()
            .map_or(u64::MAX, |deadline| deadline.saturating_sub(local.now()));
        match tairix_rt::waitset_wait(set, timeout, &mut token) {
            0 => {}
            ret if Errno::from_syscall(ret) == Errno::TimedOut => {
                server.expire(local.now());
                continue;
            }
            ret => return Errno::from_syscall(ret),
        }
        match token {
            ENDPOINT_TOKEN => loop {
                let call = match tairix_rt::call_recv_ready(endpoint, &mut request) {
                    Ok(Some(call)) => call,
                    Ok(None) => break,
                    Err(errno) => return errno,
                };
                // Read while the call is in service, before anything else.
                let Ok(origin) = tairix_rt::peer_origin(endpoint, call.ticket) else {
                    continue;
                };
                let frame = request.get(..call.len).unwrap_or(&[]);
                server.serve(call.ticket, origin.proc_id(), frame, workers);
            },
            PEER_EXIT_TOKEN => {
                while let Ok(peer) = tairix_rt::peer_exit_take() {
                    server.client_exited(peer);
                }
            }
            _ => {
                channel.wake.drain();
                server.moved();
            }
        }
    }
}

/// A client's channel to a served device, and its region.
struct RtLink {
    endpoint: u64,
    region: Mutex<SharedRegion>,
    len: usize,
}

impl Link for RtLink {
    fn call(&self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(self.endpoint, request, reply).map_err(Errno::from_syscall)
    }

    fn region_len(&self) -> usize {
        self.len
    }

    fn write_region(&self, at: usize, data: &[u8]) {
        let mut region = self.region.lock();
        if let Some(bytes) = region
            .bytes_mut()
            .get_mut(at..at.saturating_add(data.len()))
        {
            bytes.copy_from_slice(data);
        }
    }

    fn read_region(&self, at: usize, out: &mut [u8]) {
        let region = self.region.lock();
        if let Some(bytes) = region.bytes().get(at..at.saturating_add(out.len())) {
            out.copy_from_slice(bytes);
        }
    }
}

impl Device {
    /// The device a driver serves on `endpoint`, a channel
    /// [`GPU_DEVICES`](tairix_abi::sysinfo::SysinfoQueryId::GPU_DEVICES)
    /// lists, reached through a region of `region_bytes` the device copies
    /// its bytes through: between [`GPU_REGION_MIN`] and [`GPU_REGION_MAX`],
    /// and as large as the largest module this client makes a pipeline of,
    /// which crosses whole.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a region outside those bounds,
    /// [`Errno::PermissionDenied`] for a caller without the capability the
    /// channel demands, and the channel's or the region's refusal otherwise.
    pub fn connect(endpoint: u64, region_bytes: usize) -> Result<Self, Errno> {
        if !(GPU_REGION_MIN..=GPU_REGION_MAX).contains(&region_bytes) {
            return Err(Errno::LengthOutOfRange);
        }
        let region = SharedRegion::create(region_bytes).ok_or(Errno::OutOfMemory)?;
        let grant = tairix_rt::shm_grant(region.id(), endpoint);
        let grant = u64::try_from(grant).map_err(|_| Errno::from_syscall(grant))?;
        let link = RtLink {
            endpoint,
            len: region.bytes().len(),
            region: Mutex::new(region),
        };
        let remote = Remote::open(Box::new(link), grant)?;
        Ok(Self {
            host: crate::shared(remote).map_err(|_| Errno::OutOfMemory)?,
            served: None,
        })
    }
}
