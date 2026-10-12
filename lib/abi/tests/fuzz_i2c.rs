//! Deterministic fuzz harness for the `i2c-v1` wire surface.
//!
//! An I²C controller's driver drives every target on its bus, so every frame
//! it decodes arrives from a less trusted consumer, and every reply a consumer
//! decodes from a process it did not write. The invariants driven here:
//!
//! * decoding any byte image as a request or a reply never panics and never
//!   reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so a
//!   controller asking the kernel whether its caller holds a quoted link asks
//!   about precisely the record the caller sent;
//! * an accepted request names an I²C link, and every message it carries lies
//!   within the channel's buffer and its own bound.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::driver::i2c::{
    decode_close_reply, decode_open_reply, decode_transfer_reply, encode_close_reply,
    encode_error_reply, encode_open_reply, encode_transfer_reply, BusFacts, Direction, I2cAddress,
    I2cChannel, I2cFault, I2cMessage, I2cRequest, I2cTransaction, RepeatedStart, I2C_BUFFER_LEN,
    I2C_CONTROLLER_ENDPOINTS, I2C_MAX_MESSAGE_LEN, I2C_MAX_REPLY, I2C_MAX_REQUEST,
};
use tairix_abi::hwlink::{LinkRequest, LinkRole, RegionGrant};
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 8_000;

fn scramble(frame: &mut [u8], most: usize, rng: &mut Prng) {
    if frame.is_empty() {
        return;
    }
    for _ in 0..rng.at_most(most) {
        let pos = rng.below(frame.len());
        frame[pos] ^= rng.next_u8();
    }
}

fn link(selector: &[u32]) -> LinkRequest {
    LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(5), 0, selector, b"")
        .expect("a valid seed link")
}

fn message(address: u8, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: I2cAddress::new(address).expect("a device address"),
        direction,
        offset,
        len,
    }
}

fn request_seeds() -> Vec<Vec<u8>> {
    let read_register = I2cTransaction::new(&[
        message(0x68, Direction::Write, 0, 1),
        message(0x68, Direction::Read, 1, 7),
    ])
    .expect("valid");
    let edid = I2cTransaction::new(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 1),
        message(0x50, Direction::Read, 2, 128),
    ])
    .expect("valid");
    [
        I2cRequest::Open(link(&[0x68])),
        I2cRequest::Transfer {
            link: link(&[0x68]),
            transaction: read_register,
        },
        I2cRequest::Transfer {
            link: link(&[]),
            transaction: edid,
        },
        I2cRequest::Close(link(&[])),
    ]
    .iter()
    .map(|request| {
        let mut out = vec![0u8; I2C_MAX_REQUEST];
        let len = request.encode(&mut out).expect("the seed fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn reply_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let mut push = |encode: &dyn Fn(&mut [u8]) -> Result<usize, Errno>| {
        let mut out = vec![0u8; I2C_MAX_REPLY];
        let len = encode(&mut out).expect("the seed fits");
        out.truncate(len);
        seeds.push(out);
    };
    let channel = I2cChannel {
        facts: BusFacts {
            speed_hz: 100_000,
            repeated_start: RepeatedStart::BeforeFinalRead,
        },
        buffer: RegionGrant {
            grant: 0x6001,
            grantor: ProcId::from_raw([0x5A; PROC_ID_LEN]),
        },
    };
    push(&|out| encode_open_reply(out, &channel));
    push(&|out| encode_transfer_reply(out, Ok(())));
    push(&|out| encode_transfer_reply(out, Err(I2cFault::NoAck { message: 1 })));
    push(&|out| encode_close_reply(out));
    push(&|out| encode_error_reply(out, Errno::PermissionDenied));
    seeds
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = I2cRequest::decode(bytes) else {
        return;
    };
    assert_eq!(
        request.link().role(),
        LinkRole::I2c,
        "a link of another role"
    );
    if let I2cRequest::Transfer { transaction, .. } = &request {
        for message in transaction.messages() {
            assert!(usize::from(message.len) <= I2C_MAX_MESSAGE_LEN);
            assert!(message.span().end <= I2C_BUFFER_LEN, "past the buffer");
        }
    }
    let mut out = vec![0u8; I2C_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_replies(bytes: &[u8]) {
    let mut out = vec![0u8; I2C_MAX_REPLY];
    if let Ok(channel) = decode_open_reply(bytes) {
        let len = encode_open_reply(&mut out, &channel).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if let Ok(outcome) = decode_transfer_reply(bytes) {
        let len = encode_transfer_reply(&mut out, outcome).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if decode_close_reply(bytes).is_ok() {
        let len = encode_close_reply(&mut out).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
}

#[test]
fn decoding_any_i2c_frame_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_i2c_frame_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = rng.pick(&requests);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_request(&mutated);
        exercise_request(&seed[..rng.at_most(seed.len())]);
        let mut longer = seed.clone();
        longer.push(rng.next_u8());
        exercise_request(&longer);
        let mut noise = vec![0u8; rng.at_most(I2C_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_replies(&mutated);
        exercise_replies(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(I2C_MAX_REPLY + 8)];
        rng.fill(&mut noise);
        exercise_replies(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
