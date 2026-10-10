//! The live `audio-v1` transport and a stream over its shared ring: the one
//! syscall-backed piece of this crate, built only for a freestanding program
//! (feature `rt`).
//!
//! A stream's notify mailbox is derived by the service from the caller's
//! attested pid and handed back in the grant, so it is bound here only once a
//! grant names it, and admitted to the audio service alone: nobody else may
//! forge a stream's state into it. The service hands a closed stream's slot to
//! the next stream, so a mailbox stays bound for the life of the process and
//! is bound only the first time a grant names it.

use alloc::vec::Vec;

use tairix_abi::audio::{AudioNotify, OpenParams, StreamGrant, AUDIO_ENDPOINT, AUDIO_NOTIFY_LEN};
use tairix_abi::driver::audio::Frames;
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{Errno, Notice, NoticeTopic, ORIGIN_WIRE_LEN};
use tairix_rt::shm::SharedRegion;

use crate::stream::{AudioTransport, NotifyDrain, OpenFailure, StreamClient, Written};

/// Notifications a stream's mailbox holds before the service's next is
/// dropped: well past the periods a client can fall behind by before its
/// next wake drains them all.
const NOTIFY_CAPACITY: usize = 64;

/// The wait-set token the blocking wait parks on its mailbox under.
const NOTIFY_TOKEN: u64 = 1;

/// The audio service's rendezvous, and the mailbox the current stream's
/// notifications arrive in.
pub struct RtAudio {
    /// The mailboxes this process has bound, each admitted to the service.
    bound: Vec<u64>,
    /// The current stream's mailbox.
    notify: Option<u64>,
    /// The wait-set the blocking wait parks on.
    parked: Option<u64>,
    /// The mailbox that wait-set watches.
    watching: Option<u64>,
}

impl Default for RtAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl RtAudio {
    /// A transport with no stream yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bound: Vec::new(),
            notify: None,
            parked: None,
            watching: None,
        }
    }

    /// Take the mailbox `grant` names for the stream's notifications.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to bind the mailbox or to admit the service to
    /// it, or [`Errno::OutOfMemory`] recording it.
    pub fn adopt(&mut self, grant: &StreamGrant) -> Result<(), Errno> {
        let port = grant.notify_endpoint;
        if !self.bound.contains(&port) {
            self.bound.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            let bound = tairix_rt::port_bind(port, AUDIO_NOTIFY_LEN, NOTIFY_CAPACITY);
            if bound < 0 {
                return Err(Errno::from_syscall(bound));
            }
            tairix_rt::port_admit(port, AUDIO_ENDPOINT)?;
            self.bound.push(port);
        }
        self.notify = Some(port);
        Ok(())
    }

    /// The mailbox the current stream's notifications arrive in, for a caller
    /// that parks on a wait-set of its own.
    #[must_use]
    pub const fn notify_port(&self) -> Option<u64> {
        self.notify
    }

    /// The wait-set the blocking wait parks on, watching `port`.
    fn wait_set(&mut self, port: u64) -> Result<u64, Errno> {
        let set = if let Some(set) = self.parked {
            set
        } else {
            let created = tairix_rt::waitset_create();
            let set = u64::try_from(created).map_err(|_| Errno::from_syscall(created))?;
            self.parked = Some(set);
            set
        };
        if self.watching == Some(port) {
            return Ok(set);
        }
        // A mailbox left watched would wake every wait on what it holds.
        if let Some(old) = self.watching.take() {
            watch(set, WaitSetOp::Del, old)?;
        }
        watch(set, WaitSetOp::Add, port)?;
        self.watching = Some(port);
        Ok(set)
    }
}

/// Add `port` to wait-set `set`, or take it out.
fn watch(set: u64, op: WaitSetOp, port: u64) -> Result<(), Errno> {
    match tairix_rt::waitset_ctl(set, op, WaitSourceKind::Port, port, NOTIFY_TOKEN) {
        0 => Ok(()),
        refused => Err(Errno::from_syscall(refused)),
    }
}

impl AudioTransport for RtAudio {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(AUDIO_ENDPOINT, request, reply).map_err(Errno::from_syscall)
    }

    fn wait_notify(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
        let port = self.notify.ok_or(Errno::NotConnected)?;
        loop {
            if let Some(len) = self.try_notify(out)? {
                return Ok(len);
            }
            let set = self.wait_set(port)?;
            let mut token = 0;
            let parked = tairix_rt::waitset_wait(set, u64::MAX, &mut token);
            if parked != 0 {
                return Err(Errno::from_syscall(parked));
            }
        }
    }

    fn try_notify(&mut self, out: &mut [u8]) -> Result<Option<usize>, Errno> {
        let port = self.notify.ok_or(Errno::NotConnected)?;
        let mut from = [0u8; ORIGIN_WIRE_LEN];
        match tairix_rt::ipc_recv(port, out, &mut from).map_err(Errno::from_syscall) {
            Ok(len) => Ok(Some(len)),
            Err(Errno::WouldBlock) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// An open stream and the shared ring its frames move through.
pub struct LiveStream {
    client: StreamClient,
    region: SharedRegion,
    geometry: PcmGeometry,
    drain: NotifyDrain,
}

impl LiveStream {
    /// Open a stream on `params`, take its notify mailbox, and attach a ring
    /// of the shape it was granted.
    ///
    /// # Errors
    ///
    /// [`OpenFailure`]; a stream opened before the step that failed is closed
    /// again first.
    pub fn open(transport: &mut RtAudio, params: &OpenParams) -> Result<Self, OpenFailure> {
        let client = StreamClient::open(transport, params).map_err(OpenFailure::Refused)?;
        match Self::equip(transport, &client) {
            Ok((region, geometry)) => Ok(Self {
                client,
                region,
                geometry,
                drain: NotifyDrain::new(NOTIFY_CAPACITY),
            }),
            Err(failure) => {
                let _ = client.close(transport);
                Err(failure)
            }
        }
    }

    /// Bind `client`'s mailbox and hand the service its ring.
    fn equip(
        transport: &mut RtAudio,
        client: &StreamClient,
    ) -> Result<(SharedRegion, PcmGeometry), OpenFailure> {
        let grant = client.grant();
        transport.adopt(&grant).map_err(OpenFailure::Notify)?;
        let geometry = PcmGeometry::new(
            grant.ring_frames,
            grant.format,
            grant.channel_map.channels(),
        )
        .map_err(OpenFailure::Ring)?;
        let region = SharedRegion::create(geometry.region_len())
            .ok_or(OpenFailure::Ring(Errno::OutOfMemory))?;
        let granted = tairix_rt::shm_grant(region.id(), AUDIO_ENDPOINT);
        let handle =
            u64::try_from(granted).map_err(|_| OpenFailure::Ring(Errno::from_syscall(granted)))?;
        client
            .attach(transport, handle)
            .map_err(OpenFailure::Ring)?;
        Ok((region, geometry))
    }

    /// The stream.
    #[must_use]
    pub const fn client(&self) -> &StreamClient {
        &self.client
    }

    /// The stream, to start, stop or report on.
    pub fn client_mut(&mut self) -> &mut StreamClient {
        &mut self.client
    }

    /// The ring's shape.
    #[must_use]
    pub const fn geometry(&self) -> PcmGeometry {
        self.geometry
    }

    /// The shared ring, bound for one transfer.
    ///
    /// # Errors
    ///
    /// What the ring refuses when the service has corrupted its header.
    pub fn ring(&mut self) -> Result<PcmRing<'_>, Errno> {
        PcmRing::bind(self.region.bytes_mut(), self.geometry)
    }

    /// Publish `samples` so their first frame lands at `at`
    /// ([`StreamClient::write_at`]).
    ///
    /// # Errors
    ///
    /// As [`StreamClient::write_at`].
    pub fn write_at(&mut self, at: Frames, samples: &[u8]) -> Result<Written, Errno> {
        let mut ring = PcmRing::bind(self.region.bytes_mut(), self.geometry)?;
        self.client.write_at(&mut ring, at, samples)
    }

    /// The next notification for this stream, never parking
    /// ([`StreamClient::take_notify`]).
    ///
    /// # Errors
    ///
    /// As [`StreamClient::take_notify`].
    pub fn take_notify(&mut self, transport: &mut RtAudio) -> Result<Option<AudioNotify>, Errno> {
        self.client.take_notify(transport, &mut self.drain)
    }

    /// Close the stream; its ring is unmapped as this is dropped.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn close(self, transport: &mut RtAudio) -> Result<(), Errno> {
        self.client.close(transport)
    }
}

/// How many capture streams the `AudioCapture` notice says are moving frames
/// on the machine: none before the audio service has published one.
#[must_use]
pub fn live_captures() -> u32 {
    let mut payload = [0u8; tairix_abi::notice::NOTICE_PAYLOAD_MAX];
    let read = tairix_rt::notice_read(NoticeTopic::AudioCapture, &mut payload);
    let Some(published) = usize::try_from(read)
        .ok()
        .and_then(|len| payload.get(..len))
    else {
        return 0;
    };
    match Notice::decode(NoticeTopic::AudioCapture, published) {
        Ok(Notice::AudioCapture { live }) => live,
        _ => 0,
    }
}
