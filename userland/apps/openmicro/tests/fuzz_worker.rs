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
//!   slice only of the length asked, carrying sound only when asked and the
//!   lamps and media only of the machine configured: its model's lights, and
//!   discs only of the drives it has.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_openmicro::firmware::{
    C1541_DOS_IMAGE, C64_BASIC_IMAGE, C64_CHARACTER_IMAGE, C64_KERNAL_IMAGE,
};
use tairix_openmicro::machine::bbc::fdc::PhysicalDrive;
use tairix_openmicro::machine::c64::keyboard::{Key as C64Key, Keycap};
use tairix_openmicro::machine::c64::{joystick, sid, Standard};
use tairix_openmicro::machine::{bbc, electron, Model, ANALOGUE_CHANNELS, FIRE_BUTTONS};
use tairix_openmicro::media::c1541::d64;
use tairix_openmicro::media::disc::ImageFormat;
use tairix_openmicro::worker::protocol::{
    Change, Configuration, DeckKey, DiscFormat, Input, ModelKey, Plus1, Reply, Request, Slot,
    MAX_INPUTS, SHARE_BYTES, SLOTS,
};
use tairix_openmicro::worker::{MachineClient, MachineService};
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
/// A C64's tape of three pulses.
const TAP: &[u8] = b"C64-TAPE-RAW\x01\x00\x00\x00\x03\x00\x00\x00\x30\x30\x42";
/// A program of two bytes for the screen.
const PRG: &[u8] = &[0x00, 0x04, 0x01, 0x02];
/// Every disc format a request may claim.
const DISC_FORMATS: [DiscFormat; 4] = [
    DiscFormat::Acorn(ImageFormat::Ssd),
    DiscFormat::Acorn(ImageFormat::Dsd),
    DiscFormat::D64,
    DiscFormat::G64,
];

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
    electron_os: Vec<u8>,
    basic: Vec<u8>,
    dfs: Vec<u8>,
    plus1: Vec<u8>,
    disc: Vec<u8>,
    disk: Vec<u8>,
    cartridge: Vec<u8>,
}

impl Media {
    fn build() -> Self {
        Self {
            os: tairix_openmicro::rom::mos::model_b()
                .expect("the MOS")
                .bytes,
            electron_os: tairix_openmicro::rom::mos::electron()
                .expect("the OS")
                .bytes,
            basic: tairix_openmicro::rom::basic::build().expect("BASIC").bytes,
            dfs: tairix_openmicro::rom::dfs::build().expect("the DFS").bytes,
            plus1: tairix_openmicro::rom::plus1::build()
                .expect("the Plus 1 ROM")
                .bytes,
            disc: tairix_openmicro::scenario::discs::named("testdisc").expect("a disc"),
            disk: d64::formatted(b"FUZZ", *b"FZ"),
            cartridge: generic_cartridge(),
        }
    }
}

/// A generic 8K cartridge.
fn generic_cartridge() -> Vec<u8> {
    let mut image = b"C64 CARTRIDGE   ".to_vec();
    image.extend_from_slice(&0x40u32.to_be_bytes());
    image.extend_from_slice(&[1, 0, 0, 0, 0, 1]);
    image.resize(0x40, 0);
    image.extend_from_slice(b"CHIP");
    image.extend_from_slice(&0x2010u32.to_be_bytes());
    image.extend_from_slice(&[0, 0, 0, 0, 0x80, 0x00, 0x20, 0x00]);
    image.resize(image.len() + 0x2000, 0xEA);
    image
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

/// A configure for any model, its images mostly real.
fn configuration<'a>(rng: &mut Prng, media: &'a Media, noise: &'a [u8]) -> Configuration<'a> {
    if rng.below(3) == 0 {
        return Configuration::C64 {
            standard: *rng.pick(&Standard::ALL),
            sid: *rng.pick(&sid::Model::ALL),
            kernal: either(rng, C64_KERNAL_IMAGE, noise, 8),
            basic: either(rng, C64_BASIC_IMAGE, noise, 8),
            character: either(rng, C64_CHARACTER_IMAGE, noise, 8),
            drives: [
                coin(rng).then(|| either(rng, C1541_DOS_IMAGE, noise, 8)),
                (rng.below(4) == 0).then_some(C1541_DOS_IMAGE),
            ],
        };
    }
    if coin(rng) {
        return Configuration::Electron {
            os: either(rng, &media.electron_os, noise, 8),
            basic: either(rng, &media.basic, noise, 8),
            plus1: coin(rng).then(|| Plus1 {
                rom: either(rng, &media.plus1, noise, 8),
                cartridges: [[None; 2], [(rng.below(4) == 0).then_some(noise), None]],
            }),
        };
    }
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
    Configuration::ModelB {
        os: either(rng, &media.os, noise, 8),
        slots: Box::new(slots),
        links: rng.next_u8(),
    }
}

/// A change, its key mostly the session's model's.
fn change(rng: &mut Prng, model: Option<Model>) -> Change {
    let model = match model {
        Some(model) if rng.below(16) != 0 => model,
        _ => *rng.pick(&Model::ALL),
    };
    let channel = u8::try_from(rng.below(ANALOGUE_CHANNELS)).expect("four channels");
    match rng.below(5) {
        0 => Change::Break(coin(rng)),
        1 => Change::Analogue(channel, rng.next_u16()),
        2 => Change::Fire(rng.next_u8() & FIRE_BUTTONS),
        3 => Change::Joystick(u8::from(coin(rng)), rng.next_u8() & joystick::ALL),
        _ => Change::Key(
            match model {
                Model::ModelB => ModelKey::ModelB(*rng.pick(bbc::keyboard::Key::ALL)),
                Model::Electron => ModelKey::Electron(*rng.pick(electron::keyboard::Key::ALL)),
                Model::C64 if rng.below(8) == 0 => ModelKey::C64(Keycap::Restore),
                Model::C64 => ModelKey::C64(Keycap::Matrix(*rng.pick(&C64Key::ALL))),
            },
            coin(rng),
        ),
    }
}

/// A slice's changes, each at a cycle within it, in order.
fn inputs(rng: &mut Prng, cycles: u32, model: Option<Model>) -> Vec<Input> {
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
            change: change(rng, model),
        })
        .collect()
}

/// `noise` one time in `odds`, otherwise `real`.
fn either<'a>(rng: &mut Prng, real: &'a [u8], noise: &'a [u8], odds: usize) -> &'a [u8] {
    if rng.below(odds) == 0 {
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
    model: Option<Model>,
) -> Request<'a> {
    let drive = *rng.pick(&PhysicalDrive::ALL);
    match rng.below(18) {
        0 => Request::Configure(Box::new(configuration(rng, media, noise))),
        1 => {
            let format = *rng.pick(&DISC_FORMATS);
            let real = match format {
                DiscFormat::Acorn(_) => &media.disc,
                DiscFormat::D64 | DiscFormat::G64 => &media.disk,
            };
            Request::InsertDisc {
                drive,
                format,
                write_protected: coin(rng),
                image: either(rng, real, noise, 4),
            }
        }
        2 => Request::EjectDisc(drive),
        3 => {
            let tape = *rng.pick(&[TAPE, TAP]);
            Request::InsertTape(either(rng, tape, noise, 4))
        }
        4 => Request::EjectTape,
        5 => Request::RewindTape,
        6 => Request::RecordTape(coin(rng)),
        7 => Request::PowerOn,
        8 => Request::TakeSnapshot,
        9 => Request::RestoreSnapshot(either(rng, snapshot, noise, 4)),
        10 => Request::CollectDiscs([coin(rng), coin(rng)]),
        11 => Request::Deck(*rng.pick(&[DeckKey::Play, DeckKey::Record, DeckKey::Stop])),
        12 => Request::InsertCartridge(either(rng, &media.cartridge, noise, 4)),
        13 => Request::RemoveCartridge,
        14 => Request::LoadProgram(either(rng, PRG, noise, 4)),
        _ => {
            let cycles = 1 + u32::try_from(rng.at_most(MOST_CYCLES)).expect("small");
            Request::Run {
                cycles,
                sound: coin(rng),
                inputs: inputs(rng, cycles, model),
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

/// The machine a session's configure built: its model and its drives.
type Session = (Model, [bool; 2]);

/// A client that has seen the worker configured as `session`, if any.
fn client_of(session: Option<Session>) -> MachineClient {
    let mut client = MachineClient::new();
    if let Some((model, drives)) = session {
        let configure = Request::Configure(Box::new(match model {
            Model::ModelB => Configuration::ModelB {
                os: &[],
                slots: Box::new([Slot::Empty; SLOTS]),
                links: 0,
            },
            Model::Electron => Configuration::Electron {
                os: &[],
                basic: &[],
                plus1: None,
            },
            Model::C64 => Configuration::C64 {
                standard: Standard::Pal,
                sid: sid::Model::Mos6581,
                kernal: &[],
                basic: &[],
                character: &[],
                drives: drives.map(|fitted| fitted.then_some(&[][..])),
            },
        }));
        client.ask(&configure).expect("a new client is idle");
        client.sent();
        client.answer(&Reply::Done.encode()).expect("configured");
    }
    client
}

/// The exchange's frames mutated: the request's handed to the worker, the
/// reply's to a client that asked the same of a worker configured as
/// `session`.
fn mutations(
    rng: &mut Prng,
    service: &mut MachineService<HeapShare>,
    request: &Request<'_>,
    exchanged: (&[u8], &[u8]),
    session: Option<Session>,
) {
    let (frame, reply) = exchanged;
    let sent = rng.mutated(frame);
    let decoded = canonical_request(&sent);
    let long = matches!(
        decoded,
        Some(Request::Run { cycles, .. }) if usize::try_from(cycles).map_or(true, |cycles| cycles > MOST_CYCLES)
    );
    // A first configure the owner did not make would set the session's model
    // behind its client's back; once there is one, another model is refused.
    if session.is_none() && matches!(decoded, Some(Request::Configure(_))) {
        let share = HeapShare::new(SHARE_BYTES).expect("a share");
        answered(
            &mut MachineService::new(share).expect("a share large enough"),
            &sent,
        );
    } else if !long {
        answered(service, &sent);
    }

    let forged = rng.mutated(reply);
    canonical_reply(&forged);
    let mut client = client_of(session);
    client.ask(request).expect("an idle client");
    client.sent();
    let drives = session.map_or([false; 2], |(_, drives)| drives);
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
            let (model, _) = session.expect("a slice believed only once configured");
            assert!(
                (model.has_caps_lock() || !ran.lights.caps_lock)
                    && (model.has_shift_lock() || !ran.lights.shift_lock)
                    && (model.has_play_key() || !ran.loaded.playing)
                    && (model == Model::C64 || !ran.loaded.cartridge)
                    && (0..2)
                        .all(|drive| drives[drive]
                            || !(ran.drives[drive] || ran.loaded.discs[drive])),
                "lamps believed only of the machine's"
            );
        }
        (Ok(Reply::Media(media)), Request::CollectDiscs(marked)) => {
            let (model, _) = session.expect("media believed only once configured");
            assert!(
                media.tape.is_none()
                    && media.discs.iter().all(|written| {
                        let drive = written.drive.index();
                        marked[drive] && drives[drive] && written.format.model() == model
                    }),
                "a collect believed only of the drives it marks and the machine has"
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
        let mut session = None;
        for step in 0..SESSION {
            let len = rng.at_most(512);
            let noise = rng.noise(len);
            let request = if step == 0 && rng.below(8) != 0 {
                Request::Configure(Box::new(configuration(&mut rng, &media, &noise)))
            } else {
                request(
                    &mut rng,
                    &media,
                    &snapshot,
                    &noise,
                    session.map(|(model, _)| model),
                )
            };
            let (frame, reply) = exchange(&mut service, &mut client, &request);
            mutations(&mut rng, &mut service, &request, (&frame, &reply), session);
            match (canonical_reply(&reply), &request) {
                (Some(Reply::Snapshot(taken)), _) => snapshot = taken.to_vec(),
                (Some(Reply::Done), Request::Configure(configuration)) => {
                    session = Some((configuration.model(), configuration.drives()));
                }
                _ => {}
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
