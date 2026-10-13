//! The freestanding loop an encoder driver serves its endpoint from.

use tairix_abi::driver::audio::Audio;
use tairix_abi::driver::encoder::{Encoder, ENCODER_MAX_REPLY, ENCODER_MAX_REQUEST};
use tairix_abi::driver::DeviceWake;
use tairix_abi::hwlink::LinkDuty;
use tairix_abi::{DriverError, ProcId, PROC_ID_HEX_LEN};
use tairix_audiochan::Channel;
use tairix_drvrt::exit::{end, step, DriverExit};
use tairix_drvrt::serve::{instance_field, log_event, Endpoint, Service};
use tairix_drvrt::{Recorder, RtSupplier, Supplier};
use tairix_inline::ArrayVec;
use tairix_log::{EventId, Field, FieldValue, Level};

use crate::{EncoderServer, Record};

const ENCODER_HELD: EventId = EventId(24_800);
const ENCODER_ABANDONED: EventId = EventId(24_801);
const ENCODER_MOVED: EventId = EventId(24_802);
const ENCODER_REFUSED: EventId = EventId(24_803);

/// The one engine an encoder serves makes one call at a time beside its
/// posted wait; the queue's bound is containment, not capacity.
const ENDPOINT_CAPACITY: usize = 4;

/// Event sources one encoder's driver waits on: its own and its audio's,
/// with the audio channel's endpoint between them.
const WAKES_MAX: usize = 8;

type Server<E> = EncoderServer<E, RtSupplier, Log>;

/// Bind the encoder node's endpoint under its `duty` and serve `encoder` from
/// it, parked beside the encoder's event sources, `wakes`, for the life of the
/// driver process.
///
/// Never returns on the success path; a set-up refusal, a lost endpoint or an
/// event source that faulted ends the driver, its reason recorded.
pub fn serve<E: Encoder>(encoder: E, duty: &LinkDuty, wakes: &[DeviceWake]) -> i32 {
    let endpoint = duty.endpoint();
    let mut server = Server::new(encoder, RtSupplier::new(endpoint), Log, endpoint);
    run(&mut server, endpoint, wakes)
}

/// Serve `encoder` as [`serve`] does with the audio its connector carries
/// beside it, `channel`, in the one wait set: `wakes` are the encoder's event
/// sources and `audio_wakes` the audio's. The channel reads its device's
/// causes after everything the encoder does, since each may have moved the
/// connector the audio follows.
///
/// Never returns on the success path; ends as [`serve`] does.
pub fn serve_with_audio<E: Encoder, A: Audio>(
    encoder: E,
    duty: &LinkDuty,
    wakes: &[DeviceWake],
    channel: Channel<A>,
    audio_wakes: &[DeviceWake],
) -> i32 {
    let endpoint = duty.endpoint();
    let mut all = ArrayVec::<DeviceWake, WAKES_MAX>::new();
    let joined = wakes
        .iter()
        .copied()
        .chain([DeviceWake::Calls(channel.endpoint())])
        .chain(audio_wakes.iter().copied())
        .all(|wake| all.try_push(wake).is_ok());
    if !joined {
        return end(
            DriverExit::NoService,
            &[step("more event sources than one wait set is given")],
        );
    }
    let mut with_audio = WithAudio {
        server: Server::new(encoder, RtSupplier::new(endpoint), Log, endpoint),
        channel,
        encoder_wakes: wakes.len(),
    };
    run(&mut with_audio, endpoint, &all)
}

fn run<S: Supplier>(supplier: &mut S, endpoint: u64, wakes: &[DeviceWake]) -> i32 {
    let mut request = [0u8; ENCODER_MAX_REQUEST];
    let service = Service::bind(
        Endpoint {
            id: endpoint,
            request: &mut request,
            max_reply: ENCODER_MAX_REPLY,
            capacity: ENDPOINT_CAPACITY,
        },
        wakes,
    );
    let ended = match service {
        Ok(service) => service.run(supplier),
        Err(ended) => ended,
    };
    ended.end()
}

/// An encoder and the audio its connector carries, served together.
struct WithAudio<E: Encoder, A: Audio> {
    server: Server<E>,
    channel: Channel<A>,
    /// The encoder's own wakes come first; the channel's endpoint, then its
    /// device's sources, follow.
    encoder_wakes: usize,
}

impl<E: Encoder, A: Audio> Supplier for WithAudio<E, A> {
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        self.server.serve(ticket, frame);
        self.channel.on_event();
    }

    fn peer_exited(&mut self, peer: ProcId) {
        self.server.peer_exited(peer);
    }

    fn woke(&mut self, index: usize) -> Result<(), DriverError> {
        match index.cmp(&self.encoder_wakes) {
            core::cmp::Ordering::Less => {
                self.server.woke(index)?;
                self.channel.on_event();
            }
            core::cmp::Ordering::Equal => self.channel.on_call(),
            core::cmp::Ordering::Greater => self.channel.on_event(),
        }
        Ok(())
    }

    fn deadline_us(&self) -> Option<u64> {
        self.server.deadline_us()
    }

    fn expired(&mut self) -> Result<(), DriverError> {
        self.server.expired()?;
        self.channel.on_event();
        Ok(())
    }
}

/// The system log, as the server records to it.
struct Log;

impl Recorder<Record> for Log {
    fn record(&mut self, record: Record) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        match record {
            Record::Held { holder } => log_event(
                Level::Info,
                ENCODER_HELD,
                "encoder: held by its display engine",
                &[instance_field("holder", holder, &mut hex)],
            ),
            Record::Abandoned { holder } => log_event(
                Level::Info,
                ENCODER_ABANDONED,
                "encoder: its holder ended; its phase stands",
                &[instance_field("holder", holder, &mut hex)],
            ),
            Record::Moved { moves, connected } => log_event(
                Level::Info,
                ENCODER_MOVED,
                "encoder: the connector moved",
                &[
                    Field {
                        key: "moves",
                        value: FieldValue::UnsignedInt(moves),
                    },
                    Field {
                        key: "connected",
                        value: FieldValue::UnsignedInt(u64::from(connected)),
                    },
                ],
            ),
            Record::Refused { op, caller, reason } => {
                let reason = Field {
                    key: "reason",
                    value: FieldValue::Error(reason),
                };
                let op = Field {
                    key: "op",
                    value: FieldValue::UnsignedInt(op.map_or(0, |op| op as u64)),
                };
                match caller {
                    Some(caller) => log_event(
                        Level::Warn,
                        ENCODER_REFUSED,
                        "encoder: request refused",
                        &[op, reason, instance_field("caller", caller, &mut hex)],
                    ),
                    None => log_event(
                        Level::Warn,
                        ENCODER_REFUSED,
                        "encoder: request refused",
                        &[op, reason],
                    ),
                }
            }
        }
    }
}
