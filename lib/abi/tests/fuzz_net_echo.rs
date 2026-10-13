//! Deterministic fuzz harness for the `SocketEcho` delivery decoder.
//!
//! An echo socket's delivery port is an inbox any process may post to, so the
//! client decodes every frame it finds there from a less trusted sender. The
//! invariants driven here:
//!
//! * decoding any byte image as a [`SocketEcho`] never panics and never reads
//!   out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so the
//!   reply hop limit in the frame's spare byte, the outcome and the payload
//!   are carried without normalisation;
//! * an error outcome never carries a payload or a reply hop limit.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use core::num::NonZeroU8;

use tairix_abi::net::{EchoOutcome, SocketAddr, SocketEcho};
use tairix_abi::net_ipc::NetAddrFamily;
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

fn v4(a: u8, b: u8, c: u8, d: u8) -> SocketAddr {
    let mut addr = [0u8; 16];
    addr[..4].copy_from_slice(&[a, b, c, d]);
    SocketAddr {
        family: NetAddrFamily::V4,
        addr,
        port: 0,
    }
}

fn seeds() -> Vec<Vec<u8>> {
    let v6 = SocketAddr {
        family: NetAddrFamily::V6,
        addr: [0x20; 16],
        port: 0,
    };
    [
        SocketEcho {
            socket: 7,
            outcome: EchoOutcome::Reply,
            source: v6,
            sequence: 4321,
            reply_hop_limit: NonZeroU8::new(55),
            payload: b"echoed payload",
        },
        SocketEcho {
            socket: 3,
            outcome: EchoOutcome::Reply,
            source: v4(198, 51, 100, 1),
            sequence: 1,
            reply_hop_limit: None,
            payload: &[],
        },
        SocketEcho {
            socket: 9,
            outcome: EchoOutcome::HopLimitExceeded,
            source: v4(203, 0, 113, 9),
            sequence: 17,
            reply_hop_limit: None,
            payload: &[],
        },
        SocketEcho {
            socket: 1,
            outcome: EchoOutcome::PacketTooBig { mtu: 1280 },
            source: v4(192, 0, 2, 1),
            sequence: 5,
            reply_hop_limit: None,
            payload: &[],
        },
    ]
    .iter()
    .map(|echo| {
        let mut out = vec![0u8; SocketEcho::MAX_WIRE_LEN];
        let len = echo.encode(&mut out).expect("the seed fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn exercise(bytes: &[u8]) {
    let Ok(echo) = SocketEcho::parse(bytes) else {
        return;
    };
    if echo.outcome != EchoOutcome::Reply {
        assert!(echo.payload.is_empty(), "an error carries no payload");
        assert!(
            echo.reply_hop_limit.is_none(),
            "an error carries no reply hop limit"
        );
    }
    // The decoder consumes exactly the header and declared payload and
    // tolerates trailing bytes, so the canonical re-encoding equals the
    // prefix it read — nothing within the frame is normalised away.
    let mut out = vec![0u8; SocketEcho::MAX_WIRE_LEN];
    let len = echo.encode(&mut out).expect("an accepted frame re-encodes");
    assert_eq!(
        &out[..len],
        &bytes[..len],
        "a frame was normalised on decode"
    );
}

#[test]
fn decoding_any_socket_echo_frame_never_panics() {
    let seeds = seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_socket_echo_frame_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = rng.pick(&seeds);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise(&mutated);
        exercise(&seed[..rng.at_most(seed.len())]);
        let mut longer = seed.clone();
        longer.push(rng.next_u8());
        exercise(&longer);
        let mut noise = vec![0u8; rng.at_most(SocketEcho::MAX_WIRE_LEN + 8)];
        rng.fill(&mut noise);
        exercise(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
