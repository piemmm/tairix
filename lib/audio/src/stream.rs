//! The client half of `audio-v1`: the code a program links to play or record
//! sound.
//!
//! There is one transport and no bypass, so this is the whole of a program's
//! audio surface. It holds no capability, opens no endpoint and issues no
//! syscall: the IPC round trip and the park on the notify mailbox are the
//! caller's, supplied through [`AudioTransport`], which keeps the client
//! host-testable against a mock service and keeps this crate free of I/O.
//!
//! # Writing at a position, not into a buffer
//!
//! A client names the **frame** its samples belong at. Where that is ahead of
//! what the ring already carries, the distance is closed with the format's own
//! silence, so the position the service reads never lies about where the
//! samples that follow it belong — which is what makes gapless playback and
//! synchronisation exact arithmetic. Where it is behind, the write is refused:
//! those frames are published and may already have been played, and quietly
//! dropping the request would leave the caller believing they were not.
//!
//! # Parking, never polling
//!
//! A client with a full ring parks on the stream's notify mailbox and is woken
//! when the service has taken frames. There is no retry loop and no sleep.

use alloc::vec::Vec;

use tairix_abi::audio::{
    decode_clock_reply, decode_enumerate_reply, decode_open_reply, decode_state_reply,
    decode_streams_reply, AudioDeviceDescriptor, AudioGain, AudioNotify, AudioRequest, ClockReport,
    OpenParams, StreamDescriptor, StreamGrant, StreamReport, StreamState, AUDIO_MAX_REPLY,
    AUDIO_MAX_REQUEST, AUDIO_NOTIFY_LEN,
};
use tairix_abi::driver::audio::{Frames, StreamDirection};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::reply::decode_status_reply;
use tairix_abi::Errno;

/// The caller's IPC, injected so this crate performs none of its own.
pub trait AudioTransport {
    /// Send `request` to the audio service and receive its reply, returning
    /// the reply's length.
    ///
    /// # Errors
    ///
    /// The transport's own [`Errno`] — a vanished endpoint, a refused send.
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;

    /// Park on the stream's notify mailbox until the service wakes it, then
    /// take the frame.
    ///
    /// Blocking by contract: a caller that returns immediately with nothing
    /// turns the client's wait into the busy loop the charter forbids.
    ///
    /// # Errors
    ///
    /// The transport's own [`Errno`].
    fn wait_notify(&mut self, out: &mut [u8]) -> Result<usize, Errno>;

    /// Take the frame waiting on the stream's notify mailbox, or `None` when
    /// none is; never parks.
    ///
    /// # Errors
    ///
    /// The transport's own [`Errno`].
    fn try_notify(&mut self, out: &mut [u8]) -> Result<Option<usize>, Errno>;
}

/// Every device of `direction` the caller may see, by ascending id.
///
/// # Errors
///
/// The service's or the transport's refusal, [`Errno::BadMagic`] for a
/// service whose ids do not ascend, or [`Errno::OutOfMemory`].
pub fn devices<T: AudioTransport>(
    transport: &mut T,
    direction: StreamDirection,
) -> Result<Vec<AudioDeviceDescriptor>, Errno> {
    let mut reply = [0u8; AUDIO_MAX_REPLY];
    walk(
        |after| {
            decode_enumerate_reply(StreamClient::send(
                transport,
                &AudioRequest::Enumerate { direction, after },
                &mut reply,
            )?)
        },
        |device| device.device_id,
    )
}

/// Every stream the service holds, by ascending id. The service answers this
/// only to a caller holding `CAP_SYSINFO_INTROSPECT`.
///
/// # Errors
///
/// As [`devices`].
pub fn streams<T: AudioTransport>(transport: &mut T) -> Result<Vec<StreamDescriptor>, Errno> {
    let mut reply = [0u8; AUDIO_MAX_REPLY];
    walk(
        |after| {
            decode_streams_reply(StreamClient::send(
                transport,
                &AudioRequest::ListStreams { after },
                &mut reply,
            )?)
        },
        |stream| stream.stream_id,
    )
}

/// A change to one device's controls.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeviceControl {
    /// Make it its direction's default.
    Default,
    /// Set its own level.
    Level(AudioGain),
    /// Mute or unmute it.
    Mute(bool),
}

/// Apply `control` to the device `device_id`. The service admits it for the
/// login session holding the room the device serves, for anybody while the
/// room is unclaimed, and for nobody while it is withheld.
///
/// # Errors
///
/// The service's refusal — [`Errno::SeatNotOwner`] outside the room's
/// tenancy, [`Errno::NotFound`] for no such device — or the transport's.
pub fn set_control<T: AudioTransport>(
    transport: &mut T,
    device_id: u32,
    control: DeviceControl,
) -> Result<(), Errno> {
    let request = match control {
        DeviceControl::Default => AudioRequest::SetDefault { device_id },
        DeviceControl::Level(level) => AudioRequest::SetLevel { device_id, level },
        DeviceControl::Mute(muted) => AudioRequest::SetMute { device_id, muted },
    };
    let mut reply = [0u8; AUDIO_MAX_REPLY];
    decode_status_reply(StreamClient::send(transport, &request, &mut reply)?)
}

/// Device controls waiting to be applied, with at most one round trip to the
/// service in flight: a control asked for meanwhile replaces one of its kind
/// still waiting for its device, so a drag across a volume slider costs one
/// request at a time however fast the pointer moves. Every round trip lists
/// the devices after applying what it carries.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlQueue {
    queued: Vec<(u32, DeviceControl)>,
    trip: Trip,
}

/// Where a [`ControlQueue`]'s round trips stand.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Trip {
    /// None is in flight and no listing is owed.
    #[default]
    Idle,
    /// None is in flight and a listing is owed.
    Owed,
    /// One is in flight; `again` when another listing is owed after it,
    /// because what it lists may predate the change that asked.
    InFlight {
        /// Whether another is owed.
        again: bool,
    },
}

impl ControlQueue {
    /// A queue that owes nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queued: Vec::new(),
            trip: Trip::Idle,
        }
    }

    /// Ask for `control` on `device_id`, replacing a waiting one of its kind.
    ///
    /// It goes to the back however long the one it replaces has waited:
    /// making two devices default is ordered, so the one chosen last must be
    /// the one applied last.
    pub fn ask(&mut self, device_id: u32, control: DeviceControl) {
        self.queued
            .retain(|&(device, held)| device != device_id || !same_kind(held, control));
        self.queued.push((device_id, control));
    }

    /// Put `controls` ahead of whatever waits, as they were asked earlier: one
    /// of a kind already waiting for its device is dropped, the waiting one
    /// being newer.
    pub fn ask_first(&mut self, mut controls: Vec<(u32, DeviceControl)>) {
        controls.retain(|&(device_id, control)| !self.waits(device_id, control));
        controls.append(&mut self.queued);
        self.queued = controls;
    }

    fn waits(&self, device_id: u32, control: DeviceControl) -> bool {
        self.queued
            .iter()
            .any(|&(device, held)| device == device_id && same_kind(held, control))
    }

    /// Ask for the devices to be listed again.
    pub fn refresh(&mut self) {
        self.trip = match self.trip {
            Trip::Idle | Trip::Owed => Trip::Owed,
            Trip::InFlight { .. } => Trip::InFlight { again: true },
        };
    }

    /// The controls of the round trip to start, if one is owed and none is
    /// in flight.
    pub fn next_trip(&mut self) -> Option<Vec<(u32, DeviceControl)>> {
        match self.trip {
            Trip::InFlight { .. } => return None,
            Trip::Idle if self.queued.is_empty() => return None,
            Trip::Idle | Trip::Owed => {}
        }
        self.trip = Trip::InFlight { again: false };
        Some(core::mem::take(&mut self.queued))
    }

    /// The round trip in flight has come back.
    pub fn landed(&mut self) {
        self.trip = match self.trip {
            Trip::InFlight { again: true } | Trip::Owed => Trip::Owed,
            Trip::InFlight { again: false } | Trip::Idle => Trip::Idle,
        };
    }
}

const fn same_kind(a: DeviceControl, b: DeviceControl) -> bool {
    matches!(
        (a, b),
        (DeviceControl::Default, DeviceControl::Default)
            | (DeviceControl::Level(_), DeviceControl::Level(_))
            | (DeviceControl::Mute(_), DeviceControl::Mute(_))
    )
}

/// Collect what `next` answers after each id in turn until it answers
/// [`Errno::NotFound`]. An id that does not ascend would walk for ever, so it
/// is refused.
fn walk<R, K: Copy + Default + Ord>(
    mut next: impl FnMut(K) -> Result<R, Errno>,
    id: impl Fn(&R) -> K,
) -> Result<Vec<R>, Errno> {
    let mut found = Vec::new();
    let mut after = K::default();
    loop {
        match next(after) {
            Ok(record) if id(&record) > after => {
                after = id(&record);
                found.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
                found.push(record);
            }
            Ok(_) => return Err(Errno::BadMagic),
            Err(Errno::NotFound) => return Ok(found),
            Err(err) => return Err(err),
        }
    }
}

/// Why a stream could not be opened with its ring attached; nothing of it is
/// left open.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OpenFailure {
    /// The service refused the stream, or was not there.
    Refused(Errno),
    /// Its notify mailbox could not be bound to this process and the service.
    Notify(Errno),
    /// Its shared ring could not be made, granted, or attached.
    Ring(Errno),
}

impl OpenFailure {
    /// The refusal underneath.
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Refused(errno) | Self::Notify(errno) | Self::Ring(errno) => errno,
        }
    }
}

impl core::fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (what, errno) = match self {
            Self::Refused(errno) => ("the audio service refused the stream", errno),
            Self::Notify(errno) => ("the stream's notify mailbox could not be bound", errno),
            Self::Ring(errno) => ("the stream's ring could not be attached", errno),
        };
        write!(f, "{what}: {errno}")
    }
}

/// When a client draining its notify mailbox must read its stream's state
/// back.
///
/// The service's notifications are best effort, and it drops one only when the
/// mailbox is full. A dropped notification therefore leaves a full mailbox
/// behind it, which wakes the client, and the drain that empties it takes at
/// least a mailbox's worth — so that drain, and only that one, may have missed
/// something, a stream's `Idle` or `DeviceLost` included.
#[derive(Copy, Clone, Debug)]
pub struct NotifyDrain {
    capacity: usize,
    taken: usize,
}

impl NotifyDrain {
    /// A drain of a mailbox holding `capacity` notifications.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self { capacity, taken: 0 }
    }

    /// One notification was taken.
    pub fn took(&mut self) {
        self.taken = self.taken.saturating_add(1);
    }

    /// The mailbox is empty: whether the stream's state must be read back.
    pub fn emptied(&mut self) -> bool {
        core::mem::take(&mut self.taken) >= self.capacity
    }
}

/// What one [`StreamClient::write_at`] moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct Written {
    /// Frames of gap-filling silence published before the samples.
    pub silence_frames: u32,
    /// Frames of the caller's own samples published.
    pub sample_frames: u32,
}

impl Written {
    /// Whether the ring took nothing at all, so the caller should park.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.silence_frames == 0 && self.sample_frames == 0
    }
}

/// One open stream.
///
/// Holds the grant and the last state the service reported, and nothing else:
/// the ring's positions live in the ring, so there is no second copy of them
/// here to drift from the truth.
///
/// Deliberately neither [`Copy`] nor [`Clone`]: it is a handle on a service
/// resource, and [`Self::close`] consumes it so a closed stream cannot be
/// operated on. A duplicable handle would make that consumption decorative
/// and let two holders each believe they owned the stream.
#[derive(Debug)]
pub struct StreamClient {
    grant: StreamGrant,
    direction: StreamDirection,
    state: StreamState,
    /// The position the last state change happened at, so a resume after a
    /// seat switch starts on the frame the pause stopped on.
    changed_at: Frames,
}

impl StreamClient {
    /// Open a stream and adopt what the service granted: the requested rate,
    /// format and layout, which the mixer converts to the device's, and a
    /// ring and latency the device's period allows, which need not be the
    /// ones asked for.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn open<T: AudioTransport>(transport: &mut T, params: &OpenParams) -> Result<Self, Errno> {
        let mut reply = [0u8; AUDIO_MAX_REPLY];
        let grant = decode_open_reply(Self::send(
            transport,
            &AudioRequest::Open(*params),
            &mut reply,
        )?)?;
        Ok(Self {
            grant,
            direction: params.direction,
            state: StreamState::Idle,
            changed_at: Frames::ZERO,
        })
    }

    /// What the service granted.
    #[must_use]
    pub const fn grant(&self) -> StreamGrant {
        self.grant
    }

    /// Which way frames flow.
    #[must_use]
    pub const fn direction(&self) -> StreamDirection {
        self.direction
    }

    /// The last state the service reported.
    #[must_use]
    pub const fn state(&self) -> StreamState {
        self.state
    }

    /// The position the stream last changed state at — the frame a paused
    /// stream resumes from.
    #[must_use]
    pub const fn changed_at(&self) -> Frames {
        self.changed_at
    }

    /// Hand the service the shared region the grant's geometry describes.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn attach<T: AudioTransport>(
        &self,
        transport: &mut T,
        region_grant: u64,
    ) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Attach {
                stream_id: self.grant.stream_id,
                region_grant,
            },
        )
    }

    /// Begin moving frames, the first of them at `at`.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn start<T: AudioTransport>(&mut self, transport: &mut T, at: Frames) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Start {
                stream_id: self.grant.stream_id,
                at,
            },
        )?;
        self.state = StreamState::Running;
        self.changed_at = at;
        Ok(())
    }

    /// Stop at `at`, holding the position so a resume is exact.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn stop<T: AudioTransport>(&mut self, transport: &mut T, at: Frames) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Stop {
                stream_id: self.grant.stream_id,
                at,
            },
        )?;
        self.state = StreamState::Paused;
        self.changed_at = at;
        Ok(())
    }

    /// Play out everything queued, then stop.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn drain<T: AudioTransport>(&mut self, transport: &mut T) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Drain {
                stream_id: self.grant.stream_id,
            },
        )?;
        self.state = StreamState::Draining;
        Ok(())
    }

    /// Discard everything queued. The position advances over the discarded
    /// frames, so what follows still belongs where the arithmetic says.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn flush<T: AudioTransport>(&self, transport: &mut T) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Flush {
                stream_id: self.grant.stream_id,
            },
        )
    }

    /// Read the device clock this stream is in the domain of.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn clock<T: AudioTransport>(&self, transport: &mut T) -> Result<ClockReport, Errno> {
        let mut reply = [0u8; AUDIO_MAX_REPLY];
        decode_clock_reply(Self::send(
            transport,
            &AudioRequest::Clock {
                stream_id: self.grant.stream_id,
            },
            &mut reply,
        )?)
    }

    /// Set this stream's own level.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn set_gain<T: AudioTransport>(
        &self,
        transport: &mut T,
        gain: AudioGain,
    ) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Gain {
                stream_id: self.grant.stream_id,
                gain,
            },
        )
    }

    /// Mute or unmute, independently of the gain.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn set_mute<T: AudioTransport>(&self, transport: &mut T, muted: bool) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Mute {
                stream_id: self.grant.stream_id,
                muted,
            },
        )
    }

    /// Read the stream's state, the position it changed at, and its glitch
    /// tallies, adopting the state locally.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn report<T: AudioTransport>(&mut self, transport: &mut T) -> Result<StreamReport, Errno> {
        let mut reply = [0u8; AUDIO_MAX_REPLY];
        let report = decode_state_reply(Self::send(
            transport,
            &AudioRequest::State {
                stream_id: self.grant.stream_id,
            },
            &mut reply,
        )?)?;
        self.state = report.state;
        self.changed_at = report.changed_at;
        Ok(report)
    }

    /// Close the stream and release its region.
    ///
    /// # Errors
    ///
    /// The service's refusal, or the transport's.
    pub fn close<T: AudioTransport>(self, transport: &mut T) -> Result<(), Errno> {
        Self::status(
            transport,
            &AudioRequest::Close {
                stream_id: self.grant.stream_id,
            },
        )
    }

    /// Park until the service reports something about this stream, adopting
    /// any state change it carries.
    ///
    /// A notification for another stream is returned as it arrived rather
    /// than swallowed: a client with several streams demultiplexes on the
    /// stream id it names.
    ///
    /// # Errors
    ///
    /// * [`Errno::BadMagic`] / [`Errno::OutOfRange`] — a malformed frame.
    /// * The transport's own refusal.
    pub fn await_notify<T: AudioTransport>(
        &mut self,
        transport: &mut T,
    ) -> Result<AudioNotify, Errno> {
        let mut frame = [0u8; AUDIO_NOTIFY_LEN];
        let length = transport.wait_notify(&mut frame)?;
        let notify = decode_notify(&frame, length)?;
        self.adopt(notify);
        Ok(notify)
    }

    /// Adopt the next notification waiting, never parking; `None` once the
    /// mailbox is empty. A drain that may have lost one to a full mailbox
    /// ([`NotifyDrain`]) ends instead with the stream's state read back from
    /// the service, handed over as a `StateChanged` at the frame it changed
    /// at.
    ///
    /// # Errors
    ///
    /// As [`Self::await_notify`], or the service's refusal to report.
    pub fn take_notify<T: AudioTransport>(
        &mut self,
        transport: &mut T,
        drain: &mut NotifyDrain,
    ) -> Result<Option<AudioNotify>, Errno> {
        let mut frame = [0u8; AUDIO_NOTIFY_LEN];
        if let Some(length) = transport.try_notify(&mut frame)? {
            drain.took();
            let notify = decode_notify(&frame, length)?;
            self.adopt(notify);
            return Ok(Some(notify));
        }
        if !drain.emptied() {
            return Ok(None);
        }
        let report = self.report(transport)?;
        Ok(Some(AudioNotify::StateChanged {
            stream_id: self.grant.stream_id,
            state: report.state,
            at: report.changed_at,
        }))
    }

    /// Adopt a notification's state change, if it is this stream's.
    pub fn adopt(&mut self, notify: AudioNotify) {
        if let AudioNotify::StateChanged {
            stream_id,
            state,
            at,
        } = notify
        {
            if stream_id == self.grant.stream_id {
                self.state = state;
                self.changed_at = at;
            }
        }
    }

    /// Publish `samples` so their first frame lands at `at`.
    ///
    /// A gap between the ring's producer position and `at` is closed with the
    /// format's own silence first, so a client that skipped material says so
    /// rather than sliding everything after it earlier. A short result means
    /// the ring filled: park on the notify mailbox and offer the remainder,
    /// never retry in a loop.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] — `at` is behind the ring's producer position.
    ///   Those frames are published and may already be audible.
    /// * [`Errno::LengthOutOfRange`] — `samples` is not a whole number of
    ///   frames.
    /// * Whatever the ring refuses when its peer's positions are corrupt.
    pub fn write_at(
        &self,
        ring: &mut PcmRing<'_>,
        at: Frames,
        samples: &[u8],
    ) -> Result<Written, Errno> {
        if self.direction != StreamDirection::Playback {
            return Err(Errno::NotSupported);
        }
        let producer = ring.producer_position()?;
        let gap = at.since(producer).ok_or(Errno::OutOfRange)?;
        let mut written = Written::default();
        if gap > 0 {
            let wanted = u32::try_from(gap).unwrap_or(u32::MAX);
            written.silence_frames = ring.write_silence(wanted)?;
            if written.silence_frames < wanted {
                // The ring filled inside the gap. The caller's samples still
                // belong at `at`, so they are offered again once there is
                // room rather than published at the wrong position.
                return Ok(written);
            }
        }
        written.sample_frames = ring.write(samples)?;
        Ok(written)
    }

    /// Take up to `out`'s capacity of captured frames, returning how many
    /// arrived.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotSupported`] — the stream is a sink, so nothing arrives
    ///   on it.
    /// * Whatever the ring refuses when its peer's positions are corrupt.
    pub fn read_into(&self, ring: &mut PcmRing<'_>, out: &mut [u8]) -> Result<u32, Errno> {
        if self.direction != StreamDirection::Capture {
            return Err(Errno::NotSupported);
        }
        ring.read(out)
    }

    /// Issue `request` and expect the shared status-only reply.
    fn status<T: AudioTransport>(transport: &mut T, request: &AudioRequest) -> Result<(), Errno> {
        let mut reply = [0u8; AUDIO_MAX_REPLY];
        decode_status_reply(Self::send(transport, request, &mut reply)?)
    }

    /// Encode `request`, hand it to the transport, and return the reply.
    fn send<'r, T: AudioTransport>(
        transport: &mut T,
        request: &AudioRequest,
        reply: &'r mut [u8],
    ) -> Result<&'r [u8], Errno> {
        let mut frame = [0u8; AUDIO_MAX_REQUEST];
        let length = request.encode(&mut frame)?;
        let request = frame.get(..length).ok_or(Errno::LengthOutOfRange)?;
        let length = transport.call(request, reply)?;
        reply.get(..length).ok_or(Errno::LengthOutOfRange)
    }
}

/// The notification in the first `length` bytes of `frame`.
fn decode_notify(frame: &[u8], length: usize) -> Result<AudioNotify, Errno> {
    AudioNotify::decode(frame.get(..length).ok_or(Errno::LengthOutOfRange)?)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
