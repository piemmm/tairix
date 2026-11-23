//! Deterministic fuzz harness for the `gpu-v1` frame.
//!
//! A GPU's driver decodes every request from a client process it does not
//! trust, and a client decodes every reply from a driver it did not write.
//! The invariants driven here:
//!
//! * decoding any byte image as a request, a reply to any op, a region span
//!   or a served-GPU record never panics and never reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so no
//!   two frames mean one request and no reply reads as another op's;
//! * an accepted span lies within the region it was checked against.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::gpu::{
    decode_reply, encode_reply, GpuDeviceDescriptor, GpuError, GpuOp, GpuRequest, GpuStatus,
    RegionSpan, GPU_CHANNEL_ENDPOINTS, GPU_MAX_BODY, GPU_MAX_REPLY, GPU_MAX_REQUEST,
    GPU_REGION_MAX, GPU_REGION_MIN,
};
use tairix_abi::Errno;
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
    let bodies: [(GpuOp, &[u8]); 5] = [
        (GpuOp::Open, &7u64.to_le_bytes()),
        (GpuOp::Close, &[]),
        (GpuOp::Wait, &[1; 36]),
        (GpuOp::Upload, &[2; 17]),
        (GpuOp::CreateRenderPipeline, &[3; GPU_MAX_BODY]),
    ];
    bodies
        .iter()
        .map(|&(op, body)| {
            let mut out = vec![0u8; GPU_MAX_REQUEST];
            let len = GpuRequest { op, body }
                .encode(&mut out)
                .expect("the seed fits");
            out.truncate(len);
            out
        })
        .collect()
}

fn reply_seeds() -> Vec<Vec<u8>> {
    let replies: [(GpuOp, GpuStatus, &[u8]); 4] = [
        (GpuOp::Info, GpuStatus::Ok, &[0x40, 0, 0, 0]),
        (GpuOp::Open, GpuStatus::Refused(Errno::Busy), &[]),
        (GpuOp::Submit, GpuStatus::Device(GpuError::Lost), &[2; 5]),
        (GpuOp::Wait, GpuStatus::Device(GpuError::TimedOut), &[]),
    ];
    replies
        .iter()
        .map(|&(op, status, body)| {
            let mut out = vec![0u8; GPU_MAX_REPLY];
            let len = encode_reply(op, status, body, &mut out).expect("the seed fits");
            out.truncate(len);
            out
        })
        .collect()
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = GpuRequest::decode(bytes) else {
        return;
    };
    assert!(request.body.len() <= GPU_MAX_BODY);
    assert_eq!(GpuOp::from_raw(request.op.raw()), Some(request.op));
    let mut out = vec![0u8; GPU_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_reply(bytes: &[u8], op: GpuOp) {
    let Ok((status, body)) = decode_reply(bytes, op) else {
        return;
    };
    if let GpuStatus::Refused(errno) = status {
        assert_ne!(errno.as_i32(), 0, "a refusal names its reason");
    }
    let mut out = vec![0u8; GPU_MAX_REPLY];
    let len = encode_reply(op, status, body, &mut out).expect("an accepted reply re-encodes");
    assert_eq!(&out[..len], bytes, "a reply was normalised on decode");
    let other = GpuOp::ALL[usize::from(op.raw()) % GpuOp::ALL.len()];
    if other != op {
        assert!(
            decode_reply(bytes, other).is_err(),
            "a reply read as another op's"
        );
    }
}

fn exercise_span(bytes: &[u8], region: usize) {
    let Ok(span) = RegionSpan::decode(bytes) else {
        return;
    };
    let mut out = [0u8; RegionSpan::WIRE_LEN];
    span.encode(&mut out).expect("a span re-encodes");
    assert_eq!(&out[..], &bytes[..RegionSpan::WIRE_LEN]);
    if let Ok(range) = span.within(region) {
        assert!(
            range.start <= range.end && range.end <= region,
            "a span past its region"
        );
        assert_eq!(Ok(range.len()), usize::try_from(span.len));
    }
}

fn exercise_descriptor(bytes: &[u8]) {
    let Ok(descriptor) = GpuDeviceDescriptor::from_le_bytes(bytes) else {
        return;
    };
    assert_eq!(
        &descriptor.to_le_bytes()[..],
        &bytes[..GpuDeviceDescriptor::WIRE_LEN],
        "a record was normalised on decode"
    );
}

#[test]
fn decoding_any_gpu_frame_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_gpu_frame_never_panics",
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
        let mut noise = vec![0u8; rng.at_most(GPU_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let op = *rng.pick(&GpuOp::ALL);
        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_reply(&mutated, op);
        exercise_reply(&seed[..rng.at_most(seed.len())], op);
        let mut noise = vec![0u8; rng.at_most(GPU_MAX_REPLY + 8)];
        rng.fill(&mut noise);
        exercise_reply(&noise, op);

        let mut span = [0u8; RegionSpan::WIRE_LEN + 2];
        rng.fill(&mut span);
        let region = GPU_REGION_MIN + rng.below(GPU_REGION_MAX - GPU_REGION_MIN + 1);
        exercise_span(&span[..rng.at_most(span.len())], region);

        let mut record = GpuDeviceDescriptor {
            endpoint: GPU_CHANNEL_ENDPOINTS.endpoint(rng.next_u32()),
            node: rng.next_u32(),
        }
        .to_le_bytes()
        .to_vec();
        scramble(&mut record, 2, &mut rng);
        exercise_descriptor(&record[..rng.at_most(record.len())]);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
