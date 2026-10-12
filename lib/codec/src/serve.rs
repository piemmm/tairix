//! The freestanding loop a codec driver serves its endpoint from.

use tairix_abi::driver::codec::{Codec, CODEC_MAX_REPLY, CODEC_MAX_REQUEST};
use tairix_abi::hwlink::LinkDuty;
use tairix_abi::PROC_ID_HEX_LEN;
use tairix_audiochan::{exit, fail};
use tairix_drvrt::serve::{instance_field, log_event, Endpoint, Service};
use tairix_drvrt::{Recorder, RtSupplier};
use tairix_log::{EventId, Field, FieldValue, Level};

use crate::{CodecServer, Record};

const CODEC_HELD: EventId = EventId(24_400);
const CODEC_ABANDONED: EventId = EventId(24_401);
const CODEC_REFUSED: EventId = EventId(24_402);

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
    let mut server = CodecServer::new(codec, RtSupplier::new(endpoint), Log, endpoint);
    let mut request = [0u8; CODEC_MAX_REQUEST];
    let service = Service::bind(
        Endpoint {
            id: endpoint,
            request: &mut request,
            max_reply: CODEC_MAX_REPLY,
            capacity: ENDPOINT_CAPACITY,
        },
        &[],
    );
    let ended = match service {
        Ok(service) => service.run(&mut server),
        Err(ended) => ended,
    };
    fail(exit::NO_SERVICE, ended.reason(), None)
}

/// The system log, as the server records to it.
struct Log;

impl Recorder<Record> for Log {
    fn record(&mut self, record: Record) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        match record {
            Record::Held { holder } => log_event(
                Level::Info,
                CODEC_HELD,
                "codec: held by its interface's driver",
                &[instance_field("holder", holder, &mut hex)],
            ),
            Record::Abandoned { holder } => log_event(
                Level::Info,
                CODEC_ABANDONED,
                "codec: stopped with its ended holder",
                &[instance_field("holder", holder, &mut hex)],
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
                        CODEC_REFUSED,
                        "codec: request refused",
                        &[op, reason, instance_field("caller", caller, &mut hex)],
                    ),
                    None => log_event(
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
