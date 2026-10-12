//! Deterministic fuzz harness for the `irqline-v1` wire surface and the
//! driver's own node record.
//!
//! A second-level interrupt controller's driver decodes every frame from a
//! less trusted consumer, and a consumer decodes every reply from a process it
//! did not write; a driver decodes its own `HwSelfNode` to pick the variant it
//! drives. The invariants driven here:
//!
//! * decoding any byte image as a request, a reply or a node record never
//!   panics and never reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so a
//!   controller asking the kernel whether its caller holds a quoted line asks
//!   about precisely the record the caller sent;
//! * an accepted request names an interrupt line and no other role's.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::blkio::BlkStatus;
use tairix_abi::driver::irqline::{
    decode_reply, encode_reply, IrqLineRequest, INTERRUPT_LINE_ENDPOINTS, IRQLINE_MAX_REPLY,
    IRQLINE_MAX_REQUEST,
};
use tairix_abi::hwlink::{LinkRequest, LinkRole};
use tairix_abi::{Errno, HwMatchKey, HwSelfNode};
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

fn request_seeds() -> Vec<Vec<u8>> {
    [
        LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(21), 4, &[4], b""),
        LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(9), 0, &[17, 3], b"cec-tx"),
    ]
    .into_iter()
    .map(|line| {
        let mut out = vec![0u8; IRQLINE_MAX_REQUEST];
        let line = line.expect("a valid seed line");
        let len = IrqLineRequest::Wait(line).encode(&mut out).expect("fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn reply_seeds() -> Vec<Vec<u8>> {
    [Ok(()), Err(Errno::Busy), Err(Errno::NotFound)]
        .into_iter()
        .map(|outcome| {
            let mut out = vec![0u8; IRQLINE_MAX_REPLY];
            let len = encode_reply(&mut out, outcome).expect("fits");
            out.truncate(len);
            out
        })
        .collect()
}

fn node_seeds() -> Vec<Vec<u8>> {
    let key = |text: &[u8]| HwMatchKey::compatible(text).expect("fits");
    [
        HwSelfNode::new(42, &[key(b"brcm,bcm2711-hdmi0")], BlkStatus::Ok),
        HwSelfNode::new(
            7,
            &[key(b"brcm,bcm2711-l2-intc"), key(b"brcm,l2-intc")],
            BlkStatus::Reset,
        ),
        HwSelfNode::new(1, &[], BlkStatus::Offline),
    ]
    .into_iter()
    .map(|record| record.expect("fits").to_le_bytes().to_vec())
    .collect()
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = IrqLineRequest::decode(bytes) else {
        return;
    };
    assert_eq!(request.link().role(), LinkRole::InterruptLine);
    let mut out = vec![0u8; IRQLINE_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_reply(bytes: &[u8]) {
    let outcome = decode_reply(bytes);
    if bytes.len() != IRQLINE_MAX_REPLY {
        assert!(outcome.is_err());
    }
}

fn exercise_node(bytes: &[u8]) {
    let Ok(record) = HwSelfNode::from_bytes(bytes) else {
        return;
    };
    let encoded = record.to_le_bytes();
    assert_eq!(
        &encoded[..],
        &bytes[..HwSelfNode::WIRE_LEN],
        "a record was normalised on decode"
    );
}

#[test]
fn decoding_any_line_frame_or_node_record_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let nodes = node_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_line_frame_or_node_record_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = rng.pick(&requests);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_request(&mutated);
        exercise_request(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(IRQLINE_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 4, &mut rng);
        exercise_reply(&mutated);
        exercise_reply(&seed[..rng.at_most(seed.len())]);

        let seed = rng.pick(&nodes);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 16, &mut rng);
        exercise_node(&mutated);
        exercise_node(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(HwSelfNode::WIRE_LEN + 8)];
        rng.fill(&mut noise);
        exercise_node(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
