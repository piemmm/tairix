//! The serve loop's work without its syscalls: answering one call, or
//! servicing one device event, over an injected [`ChannelIo`], so the whole
//! control plane is host-tested and the freestanding `serve` is only the
//! wait and the I/O.
//!
//! The device's causes are read after every call as well as on every event
//! wake. A call that waited on the device — a codec verb, a control request —
//! may have consumed the very wake an elapsed period raised, and a period left
//! unserviced stalls its stream for good once nothing else is queued behind
//! it.

use tairix_abi::driver::audio::{Audio, AudioInterrupt, MAX_DEVICE_ENDPOINTS};
use tairix_abi::driver::audio_channel::{
    AttachParams, AudioChannelNotify, AudioChannelRequest, AUDIO_CHANNEL_MAX_REQUEST,
};
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::{Errno, ProcId};

use crate::AudioChannelServer;

/// Endpoint slots the dispatcher holds a mapping for: the ABI's own endpoint
/// ceiling, so a device cannot present one it could not serve.
const ENDPOINT_SLOTS: usize = MAX_DEVICE_ENDPOINTS as usize;

/// A shared PCM region the mixer granted, as this process mapped it.
pub trait MappedRegion {
    /// The mapping's bytes, which may run past the ring the endpoint agreed.
    fn bytes(&mut self) -> &mut [u8];
}

/// The I/O around the pure server: the call endpoint, the mixer's regions,
/// and its notify ports.
pub trait ChannelIo {
    /// A mapped region.
    type Region: MappedRegion;

    /// Take the next call into `request`, answering its ticket and length, or
    /// `None` when none is waiting.
    fn recv(&mut self, request: &mut [u8]) -> Option<(u64, usize)>;

    /// Answer the call `ticket` with `reply`.
    fn reply(&mut self, ticket: u64, reply: &[u8]);

    /// The attested caller of the call `ticket`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to name it.
    fn caller(&mut self, ticket: u64) -> Result<ProcId, Errno>;

    /// Map the region `grant` names, which `owner` granted.
    ///
    /// # Errors
    ///
    /// The kernel's refusal of the mapping.
    fn map(&mut self, grant: u64, owner: ProcId) -> Result<Self::Region, Errno>;

    /// Give a mapping back.
    fn unmap(&mut self, region: Self::Region);

    /// Send `frame` to the notify port `port`. A send that fails costs a late
    /// mixer wake, which the next period recovers.
    fn notify(&mut self, port: u64, frame: &[u8]);

    /// Record that a granted region of `mapped` bytes is short of the
    /// `expected` bytes the agreed ring of `ring_frames` frames needs.
    fn short_region(&mut self, mapped: usize, expected: usize, ring_frames: u32);
}

/// A mapping and the length of the ring an endpoint agreed within it.
struct Attached<R> {
    region: R,
    ring_len: usize,
}

impl<R: MappedRegion> Attached<R> {
    fn ring(&mut self) -> &mut [u8] {
        let ring_len = self.ring_len;
        let bytes = self.region.bytes();
        let len = ring_len.min(bytes.len());
        &mut bytes[..len]
    }
}

/// The pure server, the regions it services, and the I/O around them.
pub struct Dispatcher<A: Audio, I: ChannelIo> {
    server: AudioChannelServer<A>,
    io: I,
    regions: [Option<Attached<I::Region>>; ENDPOINT_SLOTS],
    request: [u8; AUDIO_CHANNEL_MAX_REQUEST],
}

impl<A: Audio, I: ChannelIo> Dispatcher<A, I> {
    /// Serve `audio` over `io`. Its event sources stay masked until a region
    /// is attached: a device left clocking by its bring-up has nowhere to put
    /// frames.
    pub fn new(audio: A, io: I) -> Self {
        let mut server = AudioChannelServer::new(audio);
        let _ = server.audio_mut().set_event_interrupts(false);
        Self {
            server,
            io,
            regions: [const { None }; ENDPOINT_SLOTS],
            request: [0; AUDIO_CHANNEL_MAX_REQUEST],
        }
    }

    /// The call endpoint woke: answer the call waiting, then read the
    /// device's causes.
    pub fn on_call(&mut self) {
        self.serve_call();
        self.on_event();
    }

    /// The device's event source woke: read its causes, move a period for
    /// every attached endpoint whose boundary passed, and tell the mixer.
    pub fn on_event(&mut self) {
        let causes = match self.server.audio_mut().take_interrupt() {
            Ok(causes) => causes,
            Err(err) => {
                // A cause register that cannot be read would raise the same
                // line for ever, so the sources are held down and every
                // endpoint is told none will raise a period again.
                let _ = self.server.audio_mut().set_event_interrupts(false);
                for endpoint in 0..MAX_DEVICE_ENDPOINTS {
                    self.notify_fault(endpoint, err.as_errno());
                }
                return;
            }
        };
        if causes.is_empty() {
            return;
        }
        if !self.server.any_attached() {
            let _ = self.server.audio_mut().set_event_interrupts(false);
            return;
        }
        for endpoint in 0..MAX_DEVICE_ENDPOINTS {
            self.report_endpoint(causes, endpoint);
        }
    }

    /// Move a period for `endpoint` if its boundary passed, and send what its
    /// causes name.
    fn report_endpoint(&mut self, causes: AudioInterrupt, endpoint: u16) {
        if AudioInterrupt::names(causes.period_elapsed, endpoint) {
            self.service_period(endpoint);
        } else if AudioInterrupt::names(causes.xrun, endpoint) {
            self.service_loss(endpoint);
        }
        if AudioInterrupt::names(causes.jack_changed, endpoint) {
            if let Ok(facts) = self.server.audio().endpoint_facts(endpoint) {
                self.notify(
                    endpoint,
                    AudioChannelNotify::JackChanged {
                        endpoint,
                        jack: facts.jack,
                    },
                );
            }
        }
    }

    /// A boundary passed: refill from the ring, and report the clock pair,
    /// any loss, and a drain played out.
    fn service_period(&mut self, endpoint: u16) {
        let Some(attached) = self.regions[usize::from(endpoint)].as_mut() else {
            return;
        };
        match self.server.service(endpoint, attached.ring()) {
            Ok(serviced) => {
                let position = serviced.report.position;
                self.notify(
                    endpoint,
                    AudioChannelNotify::PeriodElapsed {
                        endpoint,
                        position,
                        sampled_at: serviced.report.sampled_at,
                    },
                );
                if serviced.lost_frames != 0 {
                    self.notify(
                        endpoint,
                        AudioChannelNotify::Xrun {
                            endpoint,
                            position,
                            lost_frames: serviced.lost_frames,
                        },
                    );
                }
                // The mixer handed these frames over long before they were
                // heard, so only this side can say when the last one was.
                if !serviced.report.running {
                    self.notify(endpoint, AudioChannelNotify::Drained { endpoint, position });
                }
            }
            Err(err) => self.fault(endpoint, err),
        }
    }

    /// A loss with no boundary: nothing moved, so the position comes from a
    /// service rather than being invented.
    fn service_loss(&mut self, endpoint: u16) {
        let Some(attached) = self.regions[usize::from(endpoint)].as_mut() else {
            return;
        };
        match self.server.service(endpoint, attached.ring()) {
            Ok(serviced) if serviced.lost_frames != 0 => self.notify(
                endpoint,
                AudioChannelNotify::Xrun {
                    endpoint,
                    position: serviced.report.position,
                    lost_frames: serviced.lost_frames,
                },
            ),
            Ok(_) => {}
            Err(err) => self.fault(endpoint, err),
        }
    }

    /// Servicing `endpoint` failed: re-arming into a device that just faulted
    /// would storm, and nothing else will ever wake its mixer.
    fn fault(&mut self, endpoint: u16, err: Errno) {
        let _ = self.server.audio_mut().set_event_interrupts(false);
        self.notify_fault(endpoint, err);
    }

    fn notify(&mut self, endpoint: u16, what: AudioChannelNotify) {
        if let Some(port) = self.server.notify_endpoint(endpoint) {
            self.io.notify(port, &what.encode());
        }
    }

    fn notify_fault(&mut self, endpoint: u16, reason: Errno) {
        self.notify(endpoint, AudioChannelNotify::Faulted { endpoint, reason });
    }

    /// Receive one call, drive the pure server, and answer it. A decode
    /// failure is answered with the typed error, so the mixer sees the exact
    /// refusal.
    fn serve_call(&mut self) {
        let Some((ticket, len)) = self.io.recv(&mut self.request) else {
            return;
        };
        let request = self
            .request
            .get(..len)
            .ok_or(Errno::LengthOutOfRange)
            .and_then(AudioChannelRequest::decode);
        match request {
            Ok(AudioChannelRequest::Facts) => {
                let reply = self.server.facts_reply();
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::EndpointFacts { endpoint }) => {
                let reply = self.server.endpoint_facts_reply(endpoint);
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Configure(params)) => {
                let reply = self.server.configure_reply(&params);
                // A re-grant leaves an existing mapping the wrong shape, and
                // the server has dropped its attach state with it.
                if !self.server.is_attached(params.endpoint) {
                    self.release_region(params.endpoint);
                }
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Attach(params)) => {
                let status = match self.io.caller(ticket) {
                    Ok(mixer) => self.attach(&params, mixer),
                    Err(err) => encode_status_reply(Err(err)),
                };
                self.io.reply(ticket, &status);
            }
            Ok(AudioChannelRequest::Start { endpoint, at }) => {
                let reply = self.server.start(endpoint, at);
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Stop { endpoint, at }) => {
                let reply = self.server.stop(endpoint, at);
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Drain { endpoint }) => {
                let reply = self.server.drain(endpoint);
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Service { endpoint }) => {
                let reply = match self
                    .regions
                    .get_mut(usize::from(endpoint))
                    .and_then(Option::as_mut)
                {
                    Some(attached) => self.server.service_reply(endpoint, attached.ring()),
                    // Detached, or an index the device does not present: the
                    // server refuses before it touches a slice.
                    None => self.server.service_reply(endpoint, &mut []),
                };
                // The mixer has just made room or taken frames, so sources
                // masked for back-pressure come back up.
                if self.server.any_attached() {
                    let _ = self.server.audio_mut().set_event_interrupts(true);
                }
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Gain {
                endpoint,
                millibel,
                mute,
            }) => {
                let reply = self.server.set_gain(endpoint, millibel, mute);
                self.io.reply(ticket, &reply);
            }
            Ok(AudioChannelRequest::Detach { endpoint }) => {
                let reply = self.server.detach(endpoint);
                self.release_region(endpoint);
                if !self.server.any_attached() {
                    let _ = self.server.audio_mut().set_event_interrupts(false);
                }
                self.io.reply(ticket, &reply);
            }
            Err(err) => self.io.reply(ticket, &encode_status_reply(Err(err))),
        }
    }

    /// Drop `endpoint`'s mapping, if it has one.
    fn release_region(&mut self, endpoint: u16) {
        if let Some(attached) = self
            .regions
            .get_mut(usize::from(endpoint))
            .and_then(Option::take)
        {
            self.io.unmap(attached.region);
        }
    }

    /// Map the region `mixer` granted, check it holds the agreed ring, and
    /// attach the server; on any refusal the mapping goes and no attach state
    /// is kept.
    fn attach(&mut self, params: &AttachParams, mixer: ProcId) -> [u8; STATUS_REPLY_LEN] {
        if usize::from(params.endpoint) >= ENDPOINT_SLOTS {
            return encode_status_reply(Err(Errno::NotFound));
        }
        // A re-attach without a detach replaces the old mapping.
        self.release_region(params.endpoint);
        let mut region = match self.io.map(params.region_grant, mixer) {
            Ok(region) => region,
            Err(err) => return encode_status_reply(Err(err)),
        };
        let status = self.server.attach(params);
        let Some(geometry) = self.server.geometry(params.endpoint) else {
            self.io.unmap(region);
            return status;
        };
        let expected = geometry.region_len();
        let mapped = region.bytes().len();
        if mapped < expected {
            let _ = self.server.detach(params.endpoint);
            self.io.unmap(region);
            self.io.short_region(mapped, expected, params.ring_frames);
            return encode_status_reply(Err(Errno::BufferTooSmall));
        }
        self.regions[usize::from(params.endpoint)] = Some(Attached {
            region,
            ring_len: expected,
        });
        let _ = self.server.audio_mut().set_event_interrupts(true);
        status
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
