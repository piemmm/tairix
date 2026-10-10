//! The freestanding loop a codec driver serves its endpoint from.

use tairix_abi::driver::codec::{Codec, CODEC_MAX_REPLY, CODEC_MAX_REQUEST};
use tairix_abi::hwlink::LinkDuty;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
use tairix_abi::{CapabilityId, ProcId, PROC_ID_HEX_LEN};
use tairix_audiochan::{exit, fail};
use tairix_caps::CapabilitySet;
use tairix_drvrt::RtSupplier;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::LogSink;

use crate::{CodecServer, Record, Recorder};

const CODEC_HELD: EventId = EventId(24_400);
const CODEC_ABANDONED: EventId = EventId(24_401);
const CODEC_REFUSED: EventId = EventId(24_402);

const ENDPOINT_TOKEN: u64 = 0;
const PEER_EXIT_TOKEN: u64 = 1;

/// The interface drivers a codec's links name call one at a time and are
/// answered at once; the queue's bound is containment, not capacity.
const ENDPOINT_CAPACITY: usize = 4;

/// Bind the codec node's endpoint under its `duty` and serve `codec` from it
/// for the life of the driver process.
///
/// Never returns on the success path; a set-up refusal or a lost endpoint
/// ends the driver with [`exit::NO_SERVICE`] and its reason logged.
pub fn serve<C: Codec>(codec: C, duty: &LinkDuty) -> i32 {
    let endpoint = duty.endpoint();
    // Only a caller whose grant covers this endpoint gets in: a codec link
    // on this node.
    let mut send_caps = CapabilitySet::empty();
    send_caps.insert(CapabilityId::IPC_ENDPOINT);
    if tairix_rt::call_create(
        endpoint,
        &send_caps,
        &CapabilitySet::empty(),
        CODEC_MAX_REQUEST,
        CODEC_MAX_REPLY,
        ENDPOINT_CAPACITY,
    ) != 0
    {
        return fail(
            exit::NO_SERVICE,
            "codec: the endpoint could not be bound",
            None,
        );
    }
    let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
        return fail(
            exit::NO_SERVICE,
            "codec: the wait set could not be made",
            None,
        );
    };
    let add = |kind, id, token| tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token) == 0;
    if !(add(WaitSourceKind::Endpoint, endpoint, ENDPOINT_TOKEN)
        && add(WaitSourceKind::PeerExit, 0, PEER_EXIT_TOKEN))
    {
        return fail(
            exit::NO_SERVICE,
            "codec: the wait set could not be filled",
            None,
        );
    }
    let mut server = CodecServer::new(codec, RtSupplier::new(endpoint), Log, endpoint);
    let mut request = [0u8; CODEC_MAX_REQUEST];
    loop {
        let mut token = 0u64;
        let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
        if woke < 0 {
            return fail(exit::NO_SERVICE, "codec: the serve wait set faulted", None);
        }
        if woke != 0 {
            continue;
        }
        match token {
            ENDPOINT_TOKEN => match tairix_rt::call_recv_ready(endpoint, &mut request) {
                Ok(Some(call)) => server.serve(call.ticket, &request[..call.len]),
                Ok(None) => {}
                Err(_) => return fail(exit::NO_SERVICE, "codec: the endpoint was lost", None),
            },
            PEER_EXIT_TOKEN => {
                while let Ok(peer) = tairix_rt::peer_exit_take() {
                    server.peer_exited(peer);
                }
            }
            _ => {}
        }
    }
}

/// The system log, as the server records to it.
struct Log;

impl Recorder for Log {
    fn record(&mut self, record: Record) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        match record {
            Record::Held { holder } => emit(
                Level::Info,
                CODEC_HELD,
                "codec: held by its interface's driver",
                &[instance("holder", holder, &mut hex)],
            ),
            Record::Abandoned { holder } => emit(
                Level::Info,
                CODEC_ABANDONED,
                "codec: stopped with its ended holder",
                &[instance("holder", holder, &mut hex)],
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
                    Some(caller) => emit(
                        Level::Warn,
                        CODEC_REFUSED,
                        "codec: request refused",
                        &[op, reason, instance("caller", caller, &mut hex)],
                    ),
                    None => emit(
                        Level::Warn,
                        CODEC_REFUSED,
                        "codec: request refused",
                        &[op, reason],
                    ),
                }
            }
        }
    }
}

fn instance<'h>(
    key: &'static str,
    instance: ProcId,
    hex: &'h mut [u8; PROC_ID_HEX_LEN],
) -> Field<'h> {
    Field {
        key,
        value: FieldValue::Str(instance.write_hex(hex)),
    }
}

fn emit(level: Level, id: EventId, message: &'static str, fields: &[Field<'_>]) {
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
