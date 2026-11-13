//! Deterministic fuzz harness for the machine worker's protocol.
//!
//! The owner and the sandboxed worker each read what the other sends. The
//! invariants:
//!
//! * neither side's decoder panics, for arbitrary bytes or a mutated real
//!   message, and what either accepts is the one encoding of what it decodes
//!   to;
//! * every frame reaching the worker, well formed or not, is answered with
//!   exactly one reply that decodes, and the session goes on;
//! * the owner's client believes the worker's answer to whatever it asked;
//!   and
//! * whatever frame reaches the client, it never panics, and believes a
//!   slice only of the length asked, carrying sound only when asked.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_beeb::machine::fdc::PhysicalDrive;
use tairix_beeb::machine::keyboard::Key;
use tairix_beeb::media::disc::ImageFormat;
use tairix_beeb::worker::protocol::{
    Change, Configuration, Input, Reply, Request, Slot, ANALOGUE_CHANNELS, FIRE_BUTTONS,
    MAX_INPUTS, SHARE_BYTES, SLOTS,
};
use tairix_beeb::worker::{MachineClient, MachineService};
use tairix_fuzzseed::Prng;
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::supervise::Requests;
use tairix_sandbox::{FrameOut, ProtoError, SessionService, SessionStep};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 200;
/// Requests a drawn session makes.
const SESSION: usize = 8;
/// The longest slice a drawn request runs: long enough to cross a frame and
/// its interrupts, short enough to keep a sweep quick.
const MOST_CYCLES: usize = 40_000;
/// A tape of one data block.
const TAPE: &[u8] = b"UEF File!\0\x0A\x00\x00\x01\x03\x00\x00\x00***";

struct Collected(Vec<Vec<u8>>);

impl FrameOut for Collected {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

/// The real images most drawn requests carry.
struct Media {
    os: Vec<u8>,
    basic: Vec<u8>,
    dfs: Vec<u8>,
    disc: Vec<u8>,
}

impl Media {
    fn build() -> Self {
        Self {
            os: tairix_beeb::rom::mos::build().expect("the MOS").bytes,
            basic: tairix_beeb::rom::basic::build().expect("BASIC").bytes,
            dfs: tairix_beeb::rom::dfs::build().expect("the DFS").bytes,
            disc: tairix_beeb::scenario::discs::named("testdisc").expect("a disc"),
        }
    }
}

/// `bytes` as a request, if they decode as one, checked to be its one
/// encoding.
fn canonical_request(bytes: &[u8]) -> Option<Request<'_>> {
    let request = Request::decode(bytes).ok()?;
    assert_eq!(request.encode(), bytes, "a request has one encoding");
    Some(request)
}

/// `bytes` as a reply, if they decode as one, checked to be its one
/// encoding.
fn canonical_reply(bytes: &[u8]) -> Option<Reply<'_>> {
    let reply = Reply::decode(bytes).ok()?;
    assert_eq!(reply.encode(), bytes, "a reply has one encoding");
    Some(reply)
}

fn coin(rng: &mut Prng) -> bool {
    rng.below(2) == 0
}

fn configuration<'a>(rng: &mut Prng, media: &'a Media, noise: &'a [u8]) -> Configuration<'a> {
    let mut slots = [Slot::Empty; SLOTS];
    slots[15] = Slot::Rom(&media.basic);
    if coin(rng) {
        slots[14] = Slot::Rom(&media.dfs);
    }
    for _ in 0..rng.below(3) {
        slots[rng.below(SLOTS)] = if rng.below(4) == 0 {
            Slot::Rom(noise)
        } else {
            Slot::Ram
        };
    }
    Configuration {
        os: if rng.below(8) == 0 { noise } else { &media.os },
        slots,
        links: rng.next_u8(),
    }
}

fn change(rng: &mut Prng) -> Change {
    match rng.below(4) {
        0 => Change::Break(coin(rng)),
        1 => Change::Analogue(rng.next_u8() % ANALOGUE_CHANNELS, rng.next_u16()),
        2 => Change::Fire(rng.next_u8() & FIRE_BUTTONS),
        _ => Change::Key(*rng.pick(Key::ALL), coin(rng)),
    }
}

/// A slice's changes, each at a cycle within it, in order.
fn inputs(rng: &mut Prng, cycles: u32) -> Vec<Input> {
    let count = if rng.below(16) == 0 {
        rng.at_most(MAX_INPUTS)
    } else {
        rng.at_most(8)
    };
    let mut at: Vec<u32> = (0..count).map(|_| rng.next_u32() % cycles).collect();
    at.sort_unstable();
    at.into_iter()
        .map(|at| Input {
            at,
            change: change(rng),
        })
        .collect()
}

fn either<'a>(rng: &mut Prng, real: &'a [u8], noise: &'a [u8]) -> &'a [u8] {
    if rng.below(4) == 0 {
        noise
    } else {
        real
    }
}

/// A request the owner can make, carrying the real images, the session's
/// last snapshot or noise.
fn request<'a>(
    rng: &mut Prng,
    media: &'a Media,
    snapshot: &'a [u8],
    noise: &'a [u8],
) -> Request<'a> {
    let drive = *rng.pick(&PhysicalDrive::ALL);
    match rng.below(14) {
        0 => Request::Configure(Box::new(configuration(rng, media, noise))),
        1 => Request::InsertDisc {
            drive,
            format: *rng.pick(&[ImageFormat::Ssd, ImageFormat::Dsd]),
            write_protected: coin(rng),
            image: either(rng, &media.disc, noise),
        },
        2 => Request::EjectDisc(drive),
        3 => Request::InsertTape(either(rng, TAPE, noise)),
        4 => Request::EjectTape,
        5 => Request::RewindTape,
        6 => Request::RecordTape(coin(rng)),
        7 => Request::PowerOn,
        8 => Request::TakeSnapshot,
        9 => Request::RestoreSnapshot(either(rng, snapshot, noise)),
        10 => Request::CollectDiscs([coin(rng), coin(rng)]),
        _ => {
            let cycles = 1 + u32::try_from(rng.at_most(MOST_CYCLES)).expect("small");
            Request::Run {
                cycles,
                sound: coin(rng),
                inputs: inputs(rng, cycles),
            }
        }
    }
}

/// The one reply the worker makes to `frame`, which decodes.
fn answered(service: &mut MachineService<HeapShare>, frame: &[u8]) -> Vec<u8> {
    let mut out = Collected(Vec::new());
    assert_eq!(service.handle(frame, &mut out), SessionStep::Continue);
    let [reply] = <[Vec<u8>; 1]>::try_from(out.0).expect("one reply a frame");
    assert!(
        canonical_reply(&reply).is_some(),
        "the worker's reply decodes"
    );
    reply
}

/// `request` made as the owner makes it, its frame and the reply the
/// client believes.
fn exchange(
    service: &mut MachineService<HeapShare>,
    client: &mut MachineClient,
    request: &Request<'_>,
) -> (Vec<u8>, Vec<u8>) {
    let frame = request.encode();
    assert_eq!(
        Request::decode(&frame),
        Ok(request.clone()),
        "a request arrives as it was made"
    );
    client.ask(request).expect("idle between exchanges");
    assert_eq!(client.outgoing(), Some(frame.as_slice()));
    client.sent();
    let reply = answered(service, &frame);
    assert!(
        client.answer(&reply).is_ok(),
        "the worker's answer is believed"
    );
    (frame, reply)
}

/// The exchange's frames mutated: the request's handed to the worker, the
/// reply's to a client that asked the same.
fn mutations(
    rng: &mut Prng,
    service: &mut MachineService<HeapShare>,
    request: &Request<'_>,
    frame: &[u8],
    reply: &[u8],
) {
    let sent = rng.mutated(frame);
    let long = matches!(
        canonical_request(&sent),
        Some(Request::Run { cycles, .. }) if usize::try_from(cycles).map_or(true, |cycles| cycles > MOST_CYCLES)
    );
    if !long {
        answered(service, &sent);
    }

    let forged = rng.mutated(reply);
    canonical_reply(&forged);
    let mut client = MachineClient::new();
    client.ask(request).expect("a new client is idle");
    client.sent();
    match (client.answer(&forged), request) {
        (Ok(Reply::Ran(ran)), Request::Run { cycles, sound, .. }) => {
            assert_eq!(
                ran.cycles, *cycles,
                "a slice believed only of the length asked"
            );
            assert!(
                *sound || ran.sound.is_empty(),
                "sound believed only when asked"
            );
        }
        (Ok(Reply::Media(media)), Request::CollectDiscs(drives)) => {
            assert!(
                media.tape.is_none()
                    && media
                        .discs
                        .iter()
                        .all(|written| drives[written.drive.index()]),
                "a collect believed only of the drives it marks"
            );
        }
        _ => {}
    }
}

#[test]
fn the_worker_answers_every_frame_and_the_owner_believes_only_answers() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_worker_answers_every_frame_and_the_owner_believes_only_answers",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let media = Media::build();
    let mut iteration: u64 = 0;
    loop {
        let share = HeapShare::new(SHARE_BYTES).expect("a share");
        let mut service = MachineService::new(share).expect("a share large enough");
        let mut client = MachineClient::new();
        let mut snapshot = Vec::new();
        for step in 0..SESSION {
            let len = rng.at_most(512);
            let noise = rng.noise(len);
            let request = if step == 0 && rng.below(8) != 0 {
                Request::Configure(Box::new(configuration(&mut rng, &media, &noise)))
            } else {
                request(&mut rng, &media, &snapshot, &noise)
            };
            let (frame, reply) = exchange(&mut service, &mut client, &request);
            mutations(&mut rng, &mut service, &request, &frame, &reply);
            if let Some(Reply::Snapshot(taken)) = canonical_reply(&reply) {
                snapshot = taken.to_vec();
            }
        }

        let len = rng.at_most(64);
        let noise = rng.noise(len);
        canonical_request(&noise);
        canonical_reply(&noise);
        assert!(
            MachineClient::new().answer(&noise).is_err(),
            "a reply nothing asked for"
        );

        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
