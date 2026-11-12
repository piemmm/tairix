//! Deterministic fuzz harness for the machine session, both ways. For any
//! bytes and any order of them:
//!
//! 1. A command or event payload either decodes to what encodes back to the
//!    same bytes, or is refused; decoding never panics.
//! 2. A configuration the decoder accepts builds a machine or is refused by
//!    the board, never a panic.
//! 3. The front-end's host, fed a hostile worker's events, never panics:
//!    whatever it hands its owner it has checked, and the first event it
//!    cannot believe ends the worker.
//! 4. The worker, fed a hostile front-end's commands and answers after a
//!    start, never panics and never runs away: it ends, or serves until the
//!    session closes.
//! 5. A shared folder's server, answering whatever a hostile worker asks of
//!    it through the host, never panics.
//! 6. The debugger's requests and reports are among them: a hostile
//!    front-end attaches, sets breakpoints, stops and sends the machine on
//!    in and out of turn; a hostile worker reports stops and answers looks
//!    that may not be true.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tairix_abi::Errno;
use tairix_fuzzseed::Prng;
use tairix_log::DiscardSink;
use tairix_pcemu::cpu::jit::interpreted::Interpreted;
use tairix_pcemu::cpu::jit::MOST_CODE_BYTES;
use tairix_pcemu::cpu::Regs;
use tairix_pcemu::debug::probe::{
    Access as Watching, Breakpoint, Cycle, Direction, Go, Hit, Master, Reason,
};
use tairix_pcemu::debug::{Activity, Registers};
use tairix_pcemu::folder::memhost::{MemDir, MemHost};
use tairix_pcemu::folder::server::Server;
use tairix_pcemu::folder::{
    Access, Creation, DosName, Folder, From, Op, Refusal, Reply, SharePath, IO_MOST, SHARES,
};
use tairix_pcemu::front::{Host, HostEvent};
use tairix_pcemu::link::debug::{Debug, Listing, Look, Said, Seen};
use tairix_pcemu::link::layout::Layout;
use tairix_pcemu::link::{config, Command, Event, Form, Installed, Resume, Setup, Text, Unkept};
use tairix_pcemu::machine::Template;
use tairix_pcemu::storage::SECTOR;
use tairix_pcemu::time::Nanos;
use tairix_pcemu::video::Unread;
use tairix_pcemu::worker::port::{End, Link};
use tairix_pcemu::worker::{serve, Memory};
use tairix_reclaim::{PressureBand, PressureGauge, ReportedPressure};
use tairix_sandbox::link::Waited;
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::session::{SessionDescriptors, SessionTransport, Share};
use tairix_sandbox::wire::{Reader, Writer};
use tairix_sandbox::ProtoError;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 200;

/// Machines a smoke sweep starts: each boots its firmware and a diskette.
const SMOKE_MACHINES: u64 = 4;

/// Turns a hostile front-end gets before it closes the session: long enough
/// for the machine it starts to boot from the diskette it inserts.
const TURNS: usize = 2500;

static DISCARD: DiscardSink = DiscardSink;

fn command_bytes(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

fn event_bytes(event: &Event<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    event.encode(&mut w);
    w.finish()
}

fn setup(rng: &mut Prng) -> Setup {
    let template = *rng.pick(&[
        Template::IbmPc,
        Template::IbmXt,
        Template::TurboXt,
        Template::Workstation186,
    ]);
    Setup {
        config: template.config(),
        fixed: [None, None],
        audio_target: 1 + rng.next_u32() % 8192,
        room: 64 << 20,
        seed: [rng.next_u8(); 16],
        band: PressureBand::from_depth(rng.next_u8() % 5),
        clock: None,
        resume: (rng.below(8) == 0).then(|| Resume {
            len: 1 + rng.next_u32() % 4096,
            away: rng.next_u64(),
        }),
    }
}

/// Sectors of each standard diskette, and some that are none.
const COUNTS: [u64; 6] = [720, 1440, 2400, 2880, 1000, 0];

/// Text a front-end types: commands, Tab, Enter and the upper half.
const TYPED: &[u8] = b"dir /w\r\techo \x82\xDB\xFF ~";

/// The epoch the hostile worker's share's server carries in its handles.
const EPOCH: u32 = 0x00_1234;

/// Names a folder request names: the served folder's, and one it lacks.
const NAMES: [&[u8; 11]; 5] = [
    b"FILE    TXT",
    b"DIR        ",
    b"NEW     DAT",
    b"LONGNA~1TEX",
    b"NOWHERE    ",
];

/// A slot a share may take.
fn share_slot(rng: &mut Prng) -> u8 {
    u8::try_from(rng.below(SHARES)).unwrap()
}

fn dos_name(rng: &mut Prng) -> DosName {
    DosName::new(**rng.pick(&NAMES), false).unwrap()
}

fn share_path(rng: &mut Prng) -> SharePath {
    let names: Vec<DosName> = (0..rng.below(3)).map(|_| dos_name(rng)).collect();
    SharePath::of(&names).unwrap()
}

/// A handle a request names: one the server may have given, or any.
fn handle(rng: &mut Prng) -> u32 {
    if rng.below(2) == 0 {
        EPOCH << 8 | (1 + rng.next_u32() % 4)
    } else {
        rng.next_u32()
    }
}

/// A well-formed request of every kind, with places, sizes and handles
/// right and wrong.
fn op<'a>(rng: &mut Prng, data: &'a [u8]) -> Op<'a> {
    let any = rng.next_u32();
    let offset = *rng.pick(&[0, 100, 4000, u32::MAX - 1, any]);
    match rng.below(13) {
        0 => Op::Look {
            path: share_path(rng),
        },
        1 => Op::Find {
            pattern: if rng.below(2) == 0 {
                DosName::new(*b"???????????", true).unwrap()
            } else {
                dos_name(rng)
            },
            attributes: rng.next_u8(),
            from: if rng.below(2) == 0 {
                From::Start {
                    dir: share_path(rng),
                }
            } else {
                From::Next {
                    table: rng.next_u32() % 4,
                    index: *rng.pick(&[0, 1, 2, 3, u32::MAX]),
                }
            },
        },
        2 => Op::Open {
            path: share_path(rng),
            access: *rng.pick(&[Access::Read, Access::Write, Access::Both]),
            creation: *rng.pick(&[Creation::Existing, Creation::Truncate, Creation::New]),
        },
        3 => Op::Read {
            handle: handle(rng),
            offset,
            len: 1 + rng.next_u32() % u32::try_from(IO_MOST).unwrap(),
        },
        4 => Op::Write {
            handle: handle(rng),
            offset,
            bytes: &data[..=rng.below(data.len() - 1)],
        },
        5 => Op::Resize {
            handle: handle(rng),
            size: offset,
        },
        6 => Op::Close {
            handle: handle(rng),
            stamp: (rng.below(2) == 0)
                .then(|| tairix_abi::time::Time64::from_secs(i64::from(rng.next_u32()))),
        },
        7 => Op::Commit {
            handle: handle(rng),
        },
        8 => Op::Size {
            handle: handle(rng),
        },
        9 => Op::Remove {
            path: share_path(rng),
            directory: rng.below(2) == 0,
        },
        10 => Op::Rename {
            from: share_path(rng),
            to: share_path(rng),
        },
        11 => Op::MakeDir {
            path: share_path(rng),
        },
        _ => Op::Space,
    }
}

/// A share command: one the worker takes where the slot agrees, and ends
/// over where it does not.
fn sharing(rng: &mut Prng) -> Command<'static> {
    let slot = share_slot(rng);
    if rng.below(2) == 0 {
        Command::Share {
            slot,
            label: DosName::new(*b"HOST       ", false).unwrap(),
            writable: rng.below(2) == 0,
        }
    } else {
        Command::Unshare { slot }
    }
}

/// The folder the hostile worker's requests are answered from.
fn served_folder() -> Server<MemDir> {
    let host = MemHost::new()
        .file("file.txt", &[0x41; 1500])
        .file("DIR/inner.txt", b"inner")
        .file("Long Name.text", b"long")
        .locked("file.txt");
    Server::new(
        host.root(),
        DosName::new(*b"HOST       ", false).unwrap(),
        true,
        EPOCH,
    )
}

/// A well-formed command the worker takes and goes on serving after: input,
/// control and readings, a reset or a hold only now and then.
fn benign(rng: &mut Prng) -> Command<'static> {
    use tairix_pcemu::chips::keyboard::Scan;
    match rng.below(40) {
        0 => Command::Insert {
            drive: rng.next_u8() % 5,
            medium: rng.next_u16(),
            form: Form::Sectors {
                count: *rng.pick(&COUNTS),
                read_only: rng.next_u8() & 1 == 0,
            },
        },
        1 => Command::Insert {
            drive: rng.next_u8() % 5,
            medium: rng.next_u16(),
            form: Form::Archive {
                bytes: 1 + rng.next_u32() % 4096,
            },
        },
        2 => Command::Eject {
            drive: rng.next_u8() % 5,
        },
        3 => Command::Reset,
        4 => Command::Hold,
        5..=8 => Command::Run,
        9 => Command::ReleaseKeys,
        10..=13 => Command::Mouse {
            dx: rng.next_u32().cast_signed() % 300,
            dy: rng.next_u32().cast_signed() % 300,
            buttons: rng.next_u8() & 3,
        },
        14..=16 => Command::Fill {
            frames: rng.next_u32() % 8192,
        },
        17 => Command::Pressure(PressureBand::from_depth(rng.next_u8() % 5)),
        19 => Command::Suspend,
        20 => Command::ReadText,
        21 => Command::Type(&TYPED[..=rng.below(8)]),
        22 => Command::StopTyping,
        23 => Command::Interpret,
        18 => {
            let den = 1 + rng.next_u64() % 7;
            let hz = 1_000_000 + rng.next_u64() % 24_000_001;
            let rate = tairix_pcemu::time::Hertz::ratio(hz * den, den).unwrap();
            Command::Speed(tairix_pcemu::time::CpuClock::new(rate).unwrap())
        }
        _ => Command::Key {
            scan: Scan::plain(rng.next_u8()),
            down: rng.next_u8() & 1 == 0,
        },
    }
}

/// A command the worker ends over when it comes unasked: an answer to no
/// request, a slot never lent.
fn fatal<'a>(rng: &mut Prng, data: &'a [u8]) -> Command<'a> {
    match rng.below(6) {
        5 => Command::Compiled {
            request: rng.next_u32() % 4,
            code: installed(rng),
        },
        4 => Command::Foldered {
            request: rng.next_u32() % 4,
            answer: if rng.below(2) == 0 {
                Ok(Reply::Done)
            } else {
                Err(Refusal::Gone)
            },
        },
        3 => Command::Recalled {
            request: rng.next_u32() % 4,
            bytes: &data[..=rng.below(SECTOR)],
        },
        0 => Command::Fetched {
            request: rng.next_u32() % 4,
            answer: Ok(&data[..SECTOR * (1 + rng.below(4))]),
        },
        1 => Command::Stored {
            request: rng.next_u32() % 4,
            answer: Ok(()),
        },
        _ => Command::Released {
            slot: rng.next_u8() % 6,
        },
    }
}

/// A breakpoint of any kind, valid or not.
fn breakpoint(rng: &mut Prng) -> Breakpoint {
    match rng.below(4) {
        0 => Breakpoint::Code(rng.next_u32() % 0x10_8000),
        1 => Breakpoint::Memory {
            first: rng.next_u32() % 0x10_0000,
            len: rng.next_u32() % 0x2_0000,
            access: if rng.below(2) == 0 {
                Watching::Write
            } else {
                Watching::Any
            },
        },
        2 => Breakpoint::Port {
            first: rng.next_u16(),
            last: rng.next_u16(),
            direction: *rng.pick(&[Direction::In, Direction::Out, Direction::Both]),
        },
        _ => Breakpoint::Interrupt {
            vector: rng.next_u8(),
            ah: (rng.below(2) == 0).then(|| rng.next_u8()),
        },
    }
}

/// A look within the bounds an answer holds.
fn look(rng: &mut Prng) -> Look {
    Look {
        number: rng.next_u32() % 4,
        code: (rng.next_u16(), rng.next_u16()),
        lines: (rng.next_u8() % 17, rng.next_u8() % 49),
        memory: (rng.next_u16(), rng.next_u16()),
        bytes: rng.next_u16() % 2049,
        stack: rng.next_u8() % 65,
    }
}

/// A debugger's request, in turn or not.
fn debugging(rng: &mut Prng) -> Command<'static> {
    let request = match rng.below(8) {
        0 => Debug::Attach,
        1 => Debug::Detach,
        2 | 3 => Debug::Add(breakpoint(rng)),
        4 => Debug::Remove(breakpoint(rng)),
        5 => Debug::Break,
        6 => Debug::Go(*rng.pick(&[Go::Continue, Go::Step, Go::StepOver, Go::StepOut])),
        _ => Debug::Look(look(rng)),
    };
    Command::Debug(request)
}

/// Where a hostile front-end says a block's code was installed: anywhere, or
/// nowhere.
fn installed(rng: &mut Prng) -> Option<Installed> {
    (rng.below(2) == 0).then(|| Installed {
        address: rng.next_u64(),
        len: 1 + rng.next_u64() % MOST_CODE_BYTES as u64,
    })
}

/// A well-formed command of any kind but the start.
fn command(rng: &mut Prng, data: &[u8]) -> Vec<u8> {
    match rng.below(9) {
        0 | 1 => command_bytes(&fatal(rng, data)),
        2 => command_bytes(&sharing(rng)),
        3 => command_bytes(&debugging(rng)),
        _ => command_bytes(&benign(rng)),
    }
}

/// A well-formed event of every kind.
fn event(rng: &mut Prng, data: &[u8; SECTOR]) -> Vec<u8> {
    let pcm = [0u8; 64];
    let unread = [Unread::NoText, Unread::TooLarge, Unread::Memory];
    let (columns, rows) = (1 + rng.below(4), 1 + rng.below(3));
    let listing_bytes;
    let event = match rng.below(25) {
        21..=24 => {
            listing_bytes = listing(rng);
            Event::Debug(said(rng, data, &listing_bytes))
        }
        20 => Event::Compile {
            request: rng.next_u32() % 4,
            cs: rng.next_u16(),
            ip: rng.next_u16(),
            bytes: &data[..=rng.below(31)],
        },
        18 => Event::Folder {
            request: rng.next_u32() % 4,
            slot: share_slot(rng),
            op: op(rng, data),
        },
        19 => Event::Shared {
            slot: share_slot(rng),
            drive: (rng.below(2) == 0).then(|| rng.next_u8() % 26),
        },
        15 => Event::Text(if rng.below(2) == 0 {
            Err(*rng.pick(&unread))
        } else {
            Ok(Text {
                columns: u16::try_from(columns).unwrap(),
                rows: u16::try_from(rows).unwrap(),
                codes: &data[..columns * rows],
            })
        }),
        16 if rng.below(8) == 0 => Event::Interpreting,
        16 => Event::Typed(rng.next_u32() % 2000),
        17 => Event::TextShown(rng.next_u8() & 1 == 0),
        11 => Event::Resumed(rng.next_u8() & 1 == 0),
        12 => Event::Recall {
            request: rng.next_u32() % 4,
            offset: rng.next_u32() % 8192,
            len: rng.next_u32() % 40_000,
        },
        13 => Event::Kept(&data[..=rng.below(SECTOR - 1)]),
        14 => Event::Suspended(match rng.below(3) {
            0 => Err(Unkept::Memory),
            1 => Err(Unkept::Size),
            _ => Ok(rng.next_u32() % 4096),
        }),
        0 => Event::Ready,
        1 => Event::Frame {
            slot: rng.next_u8() % 6,
            width: rng.next_u16() % 900,
            height: rng.next_u16() % 700,
        },
        2 => Event::OutOfRange {
            adapter: rng.next_u8() % 3,
        },
        3 => Event::Audio(&pcm[..4 * (1 + rng.below(15))]),
        4 => Event::Fetch {
            request: rng.next_u32() % 4,
            medium: rng.next_u16() % 4,
            first: rng.next_u64() % 3000,
            count: rng.next_u16() % 70,
        },
        5 => Event::Store {
            request: rng.next_u32() % 4,
            medium: rng.next_u16() % 4,
            sector: rng.next_u64() % 3000,
            data,
        },
        6 => Event::Inserted {
            drive: rng.next_u8() % 5,
            medium: rng.next_u16() % 4,
            outcome: Ok(()),
        },
        7 => Event::Ejected {
            drive: rng.next_u8() % 5,
            medium: rng.next_u16() % 4,
        },
        8 => Event::Credit {
            applied: rng.next_u32() % 70,
        },
        9 => Event::Formatted {
            drive: rng.next_u8() % 3,
            format: tairix_pcemu::storage::hdc::LowLevel {
                interleave: rng.next_u8(),
                bad_tracks: vec![rng.next_u32() % 4000],
            },
        },
        _ => Event::Refused(tairix_pcemu::link::Refusal::Memory),
    };
    event_bytes(&event)
}

/// A report of a worker's debugger, its look's answer listing `listing`.
fn said<'a>(rng: &mut Prng, data: &'a [u8], listing: &'a (u8, u8, Vec<u8>)) -> Said<'a> {
    match rng.below(4) {
        0 => Said::Reset,
        1 => Said::Detached,
        2 => Said::Stopped {
            reason: reason(rng),
            registers: registers(rng),
        },
        _ => {
            let (before, after, encoded) = listing;
            Said::Seen(Seen {
                number: rng.next_u32() % 4,
                listing: Listing::new(*before, *after, encoded),
                memory: &data[..rng.below(64)],
                stack: &data[..2 * rng.below(8)],
            })
        }
    }
}

/// Why a hostile worker says the processor stopped.
fn reason(rng: &mut Prng) -> Reason {
    let hit = Hit {
        master: *rng.pick(&[Master::Cpu, Master::Coprocessor, Master::Dma]),
        cycle: *rng.pick(&[Cycle::Read, Cycle::Write, Cycle::In, Cycle::Out]),
        at: rng.next_u32() % 0x10_0000,
        value: rng.next_u8(),
    };
    match rng.below(4) {
        0 => Reason::Break,
        1 => Reason::Stepped,
        2 => Reason::Breakpoint(breakpoint(rng)),
        _ => Reason::Watched {
            watch: breakpoint(rng),
            hit,
        },
    }
}

/// The registers a hostile worker says the processor stopped with.
fn registers(rng: &mut Prng) -> Registers {
    let mut regs = Regs::default();
    for value in regs.gpr.iter_mut().chain(&mut regs.seg) {
        *value = rng.next_u16();
    }
    Registers {
        regs,
        cycles: rng.next_u64(),
        activity: *rng.pick(&[Activity::Executing, Activity::Halted, Activity::Waiting]),
        emulating: rng.below(8) == 0,
        coprocessor: None,
    }
}

/// A listing of up to a few lines, some of which follow one another.
fn listing(rng: &mut Prng) -> (u8, u8, Vec<u8>) {
    let mut ip = rng.next_u16();
    let lines: Vec<_> = (0..rng.below(5))
        .map(|_| {
            let len = 1 + rng.below(4);
            let line = tairix_pcemu::debug::disasm::Line {
                ip,
                bytes: vec![0x90; len],
                text: String::from("NOP"),
            };
            if rng.below(4) != 0 {
                ip = ip.wrapping_add(u16::try_from(len).unwrap());
            }
            line
        })
        .collect();
    Listing::encode(&lines, rng.below(3))
}

fn round_trips(rng: &mut Prng) {
    let data = rng.noise(SECTOR * 4);
    let sector: [u8; SECTOR] = data[..SECTOR].try_into().unwrap();
    let mut payloads = vec![
        command(rng, &data),
        command_bytes(&Command::Start(setup(rng))),
        event(rng, &sector),
    ];
    let mutated: Vec<Vec<u8>> = payloads.iter().map(|p| rng.mutated(p)).collect();
    payloads.extend(mutated);
    let len = rng.at_most(64);
    payloads.push(rng.noise(len));
    for payload in &payloads {
        if let Ok(command) = Command::decode(payload) {
            assert_eq!(
                &command_bytes(&command),
                payload,
                "a command reads back as written"
            );
        }
        if let Ok(event) = Event::decode(payload) {
            assert_eq!(
                &event_bytes(&event),
                payload,
                "an event reads back as written"
            );
        }
    }
}

fn configs_build_or_are_refused(rng: &mut Prng) {
    let mut w = Writer::new();
    config::put_config(&mut w, &setup(rng).config);
    let bytes = rng.mutated(&w.finish());
    let mut r = Reader::new(&bytes);
    if let Ok(config) = r.whole(config::config) {
        let _ = tairix_pcemu::bios::machine(&config, [None, None]);
    }
}

/// The host's end of a pipe a hostile worker writes into.
struct Pipe {
    from_worker: Rc<RefCell<Vec<u8>>>,
    share: Arc<dyn Share>,
}

impl SessionTransport for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut bytes = self.from_worker.borrow_mut();
        if bytes.is_empty() {
            return Err(Errno::WouldBlock);
        }
        let n = buf.len().min(bytes.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        bytes.drain(..n);
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn share(&self) -> Option<&Arc<dyn Share>> {
        Some(&self.share)
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

/// Requests of the shared folder a hostile worker begins with, numbered in
/// turn so the host serves each.
const PROLOGUE: usize = 12;

/// A hostile worker's events into the host; answers how many requests of
/// its share the host handed its owner to serve.
fn hostile_worker(rng: &mut Prng) -> usize {
    let setup = setup(rng);
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let from_worker = Rc::new(RefCell::new(Vec::new()));
    let pipe = Pipe {
        from_worker: Rc::clone(&from_worker),
        share: Arc::new(share),
    };
    let mut host = Host::start(pipe, DiscardSink, setup).unwrap();
    let mut folder = served_folder();
    let label = DosName::new(*b"HOST       ", false).unwrap();
    let sector = [0x5Au8; SECTOR];
    let mut served = 0;
    let mut next_request = 0;
    for step in 0..40 {
        let payload = match (step, rng.below(4)) {
            (0, _) => event_bytes(&Event::Ready),
            (1..=PROLOGUE, _) => event_bytes(&Event::Folder {
                request: next_request,
                slot: 0,
                op: op(rng, &sector),
            }),
            (_, 0) => {
                let base = event(rng, &sector);
                rng.mutated(&base)
            }
            _ => event(rng, &sector),
        };
        let mut pipe = from_worker.borrow_mut();
        pipe.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        pipe.extend_from_slice(&payload);
        drop(pipe);
        host.on_readable();
        while let Some(event) = host.event() {
            let fetch = match event {
                HostEvent::Fetch { count, .. } => Some(usize::from(count)),
                HostEvent::Store { .. } => None,
                HostEvent::Folder { op, .. } => {
                    let answer = folder.call(&op);
                    let _ = host.answer_folder(answer);
                    served += 1;
                    next_request += 1;
                    continue;
                }
                HostEvent::Ended(_) => return served,
                _ => continue,
            };
            match fetch {
                Some(count) => {
                    let data = vec![0u8; count * SECTOR];
                    let _ = host.answer_fetch(Ok(&data));
                }
                None => {
                    let _ = host.answer_store(Ok(()));
                }
            }
        }
        if rng.below(4) == 0 {
            let _ = match rng.below(6) {
                0 => host.attach_debugger(),
                1 => host.add_breakpoint(breakpoint(rng)),
                2 => host.look(look(rng)).map(|_| ()),
                3 => host.debug_go(Go::Step),
                4 => host.debug_break(),
                _ => host.detach_debugger(),
            };
        }
        let slot = share_slot(rng);
        match (step, rng.below(6)) {
            (0, _) => host.share(0, label, true).unwrap(),
            (1..=PROLOGUE, _) => {}
            (_, 0) => {
                let _ = host.share(slot, label, rng.below(2) == 0);
            }
            (_, 1) => {
                let _ = host.unshare(slot);
            }
            _ => {}
        }
        if rng.below(3) == 0 {
            host.key(
                tairix_pcemu::chips::keyboard::Scan::plain(rng.next_u8()),
                rng.next_u8() & 1 == 0,
            );
            host.mouse(1, 1, rng.next_u8() & 3);
            let _ = host.insert(
                rng.next_u8() % 5,
                Form::Sectors {
                    count: 720,
                    read_only: false,
                },
            );
            host.on_writable();
        }
    }
    served
}

struct Gauge(&'static ReportedPressure);

impl Memory for Gauge {
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static) {
        self.0
    }

    fn report(&self, band: PressureBand) {
        self.0.report(band);
    }
}

/// A front-end that starts a machine from a diskette of noise, then says
/// what it likes.
struct Hostile {
    rng: Prng,
    queued: Vec<Vec<u8>>,
    turns: usize,
    clock: Nanos,
    data: Vec<u8>,
    fetches: usize,
}

struct HostileLink(Rc<RefCell<Hostile>>);

impl Link for HostileLink {
    fn recv<R>(
        &mut self,
        wait: Option<Nanos>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        let mut front = self.0.borrow_mut();
        if let Some(payload) = front.queued.pop() {
            return Ok(Waited::Frame(take(&payload)));
        }
        front.turns += 1;
        if front.turns > TURNS {
            return Ok(Waited::Closed);
        }
        if let Some(wait) = wait {
            front.clock += wait;
        }
        // Until the machine has booted far enough to read its diskette, the
        // front-end only answers: every machine reaches a fetch.
        if front.fetches == 0 {
            return Ok(Waited::Nothing);
        }
        let Hostile { rng, data, .. } = &mut *front;
        let payload = match rng.below(4096) {
            0 => {
                let base = command(rng, data);
                rng.mutated(&base)
            }
            1 => command_bytes(&fatal(rng, data)),
            2..=4 => command_bytes(&sharing(rng)),
            5 => command_bytes(&debugging(rng)),
            6..=80 => command_bytes(&benign(rng)),
            _ => return Ok(Waited::Nothing),
        };
        Ok(Waited::Frame(take(&payload)))
    }

    fn flush(&mut self) -> Result<(), ProtoError> {
        Ok(())
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        let mut front = self.0.borrow_mut();
        let answer = match Event::decode(payload) {
            Ok(Event::Fetch { request, count, .. })
                if front.fetches == 0 || front.rng.below(16) != 0 =>
            {
                front.fetches += 1;
                let len = usize::from(count) * SECTOR;
                let data = front.data.get(..len).map(<[u8]>::to_vec);
                Some(command_bytes(&Command::Fetched {
                    request,
                    answer: data
                        .as_deref()
                        .ok_or(tairix_pcemu::storage::MediaError::Failed),
                }))
            }
            Ok(Event::Store { request, .. }) if front.rng.below(16) != 0 => {
                Some(command_bytes(&Command::Stored {
                    request,
                    answer: Ok(()),
                }))
            }
            Ok(Event::Recall { request, len, .. }) if front.rng.below(16) != 0 => {
                let len = usize::try_from(len).unwrap_or(0);
                let bytes = front.rng.noise(len);
                Some(command_bytes(&Command::Recalled {
                    request,
                    bytes: &bytes,
                }))
            }
            Ok(Event::Folder { request, .. }) if front.rng.below(16) != 0 => {
                let len = 1 + front.rng.below(IO_MOST);
                let bytes = front.rng.noise(len);
                let answer = match front.rng.below(5) {
                    0 => Ok(Reply::Done),
                    1 => Ok(Reply::Read(&bytes)),
                    2 => Ok(Reply::Wrote(front.rng.next_u32())),
                    3 => Ok(Reply::Size(front.rng.next_u32())),
                    _ => Err(Refusal::Denied),
                };
                Some(command_bytes(&Command::Foldered { request, answer }))
            }
            Ok(Event::Compile { request, .. }) if front.rng.below(16) != 0 => {
                let code = installed(&mut front.rng);
                Some(command_bytes(&Command::Compiled { request, code }))
            }
            _ => None,
        };
        front.queued.extend(answer);
        Ok(())
    }

    fn now(&self) -> Nanos {
        self.0.borrow().clock
    }
}

/// Run a machine against a hostile front-end; answers how many fetches it
/// answered.
fn hostile_front_end(rng: &mut Prng) -> usize {
    let setup = setup(rng);
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let start = command_bytes(&Command::Start(setup));
    let mut data = rng.noise(64 * SECTOR);
    data[510] = 0x55;
    data[511] = 0xAA;
    let diskette = command_bytes(&Command::Insert {
        drive: 0,
        medium: 1,
        form: Form::Sectors {
            count: 720,
            read_only: false,
        },
    });
    let front = Rc::new(RefCell::new(Hostile {
        rng: Prng::new(rng.next_u64()),
        // Taken from the end: the start, a diskette, then the run.
        queued: vec![command_bytes(&Command::Run), diskette, start],
        turns: 0,
        clock: 0,
        data,
        fetches: 0,
    }));
    let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
    let end = serve(
        HostileLink(Rc::clone(&front)),
        share,
        (Gauge(gauge), Box::new(Interpreted::new().0)),
        &DISCARD,
    );
    assert!(
        matches!(end, End::Closed | End::Violation(_)),
        "a hostile front-end closes the session or is refused: {end:?}"
    );
    let fetches = front.borrow().fetches;
    fetches
}

#[test]
fn hostile_sessions_never_panic_either_way() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_sessions_never_panic_either_way",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        let mut served = 0;
        for _ in 0..SMOKE_ITERATIONS {
            round_trips(&mut rng);
            configs_build_or_are_refused(&mut rng);
            served += hostile_worker(&mut rng);
        }
        assert!(
            served >= PROLOGUE,
            "the share's server answered the worker's requests"
        );
        let fetched: usize = (0..SMOKE_MACHINES)
            .map(|_| hostile_front_end(&mut rng))
            .sum();
        assert!(
            fetched > 0,
            "the machines booted from their diskettes, through fetches"
        );
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
