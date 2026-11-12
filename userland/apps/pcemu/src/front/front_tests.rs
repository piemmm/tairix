//! The host and the worker joined in one process through a model of the
//! pipe pair, the host's owner standing in for the application: it holds the
//! images, answers every request at once, and keeps what it was shown. The
//! host's checks are then driven by a worker that breaks the protocol on
//! purpose.

use super::*;
use crate::board::Lights;
use crate::cpu::jit::cache::{Key, Request};
use crate::cpu::jit::interpreted::{Compiler, Interpreted};
use crate::dos::harness::screen;
use crate::dos::setup::default_disk;
use crate::folder::memhost::{MemDir, MemHost};
use crate::folder::server::Server;
use crate::folder::{DosName, Folder};
use crate::link::{FixedDisk, Installed, Setup, QUEUE_MOST, RELEASES_KEPT, WORKER_INBOUND};
use crate::machine::Template;
use crate::storage::hdc::Recording;
use crate::storage::BlockDevice;
use crate::time::{Nanos, NANOS_PER_SECOND as SECOND};
use crate::video::ShownText;
use crate::worker::port::End;
use crate::worker::testing::{memory, TestMemory, DISCARD};
use crate::worker::{SandboxLink, Stepped};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_abi::Errno;
use tairix_log::DiscardSink;
use tairix_sandbox::link::TimedChannel;
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::proto::Channel;
use tairix_sandbox::session::{SessionDescriptors, Share};
use tairix_sandbox::WorkerLink;

/// The two pipes, and the clock the worker's waits move.
#[derive(Default)]
struct Pipes {
    to_worker: Vec<u8>,
    to_front: Vec<u8>,
    clock: Nanos,
}

/// The host's end: one read or write a call, never waiting.
struct FrontEnd {
    pipes: Rc<RefCell<Pipes>>,
    share: Arc<dyn Share>,
}

impl SessionTransport for FrontEnd {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut pipes = self.pipes.borrow_mut();
        if pipes.to_front.is_empty() {
            return Err(Errno::WouldBlock);
        }
        let n = buf.len().min(pipes.to_front.len());
        buf[..n].copy_from_slice(&pipes.to_front[..n]);
        pipes.to_front.drain(..n);
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.pipes.borrow_mut().to_worker.extend_from_slice(buf);
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

/// The application standing in front of the host.
struct App {
    host: Host<FrontEnd, DiscardSink>,
    media: Vec<(u16, Box<dyn BlockDevice>)>,
    frames: usize,
    last_frame: Vec<u8>,
    audio_frames: usize,
    seen: Vec<HostEvent<'static>>,
    ended: Option<Ending>,
    /// The snapshot a suspend handed over.
    kept: Option<Vec<u8>>,
    /// Each text read back: its columns, rows and codes.
    texts: Vec<Result<ShownText, Unread>>,
    /// The folders it shares, by slot.
    folders: Vec<(u8, Server<MemDir>)>,
    /// What compiles the blocks asked for, where blocks are compiled, and
    /// how many were.
    compiler: Option<Compiler>,
    compiled: usize,
    /// Why each stop the debugger reported was made, and the number of
    /// each look answered.
    stops: Vec<crate::debug::probe::Reason>,
    looks: Vec<u32>,
    /// The words that a translated block misbehaved.
    interpreting: usize,
}

impl App {
    /// Answer the fetch of `count` sectors of `medium` from `first`.
    fn fetch(&mut self, medium: u16, first: u64, count: u16) {
        let image = &mut self
            .media
            .iter_mut()
            .find(|(id, _)| *id == medium)
            .unwrap()
            .1;
        let mut data = vec![0u8; usize::from(count) * SECTOR];
        let answer = data
            .chunks_mut(SECTOR)
            .zip(first..)
            .try_for_each(|(sector, n)| image.read(n, sector.try_into().unwrap()));
        self.host.answer_fetch(answer.map(|()| &data[..])).unwrap();
    }

    /// Answer `block`, compiled where there is a compiler, its handle the
    /// address its code is installed at.
    fn compile(&mut self, block: &Request) {
        let code = self
            .compiler
            .as_ref()
            .and_then(|compiler| compiler.compile(block));
        self.compiled += usize::from(code.is_some());
        let code = code.map(|address| Installed { address, len: 4096 });
        self.host.answer_block(block.number, code).unwrap();
    }

    /// Write `data` to sector `sector` of `medium`, and answer.
    fn store(&mut self, medium: u16, sector: u64, data: &[u8; SECTOR]) {
        let image = &mut self
            .media
            .iter_mut()
            .find(|(id, _)| *id == medium)
            .unwrap()
            .1;
        let answer = image.write(sector, data);
        self.host.answer_store(answer).unwrap();
    }

    /// Let the host send, take every event, and answer what asks.
    fn pump(&mut self) {
        self.host.on_writable();
        self.host.on_readable();
        while let Some(event) = self.host.event() {
            match event {
                HostEvent::Frame { pixels, .. } => {
                    self.frames += 1;
                    self.last_frame.clear();
                    self.last_frame.extend_from_slice(pixels);
                }
                HostEvent::Audio(pcm) => self.audio_frames += pcm.len() / 4,
                HostEvent::Fetch {
                    medium,
                    first,
                    count,
                } => self.fetch(medium, first, count),
                HostEvent::Store {
                    medium,
                    sector,
                    data,
                } => {
                    let data = *data;
                    self.store(medium, sector, &data);
                }
                HostEvent::Ended(ending) => self.ended = Some(ending),
                HostEvent::Ready => self.seen.push(HostEvent::Ready),
                HostEvent::Refused(refusal) => self.seen.push(HostEvent::Refused(refusal)),
                HostEvent::OutOfRange { adapter } => {
                    self.seen.push(HostEvent::OutOfRange { adapter });
                }
                HostEvent::Inserted {
                    drive,
                    medium,
                    outcome,
                } => self.seen.push(HostEvent::Inserted {
                    drive,
                    medium,
                    outcome,
                }),
                HostEvent::Ejected { drive, medium } => {
                    self.seen.push(HostEvent::Ejected { drive, medium });
                }
                HostEvent::Formatted { drive, format } => {
                    self.seen.push(HostEvent::Formatted { drive, format });
                }
                HostEvent::Pointing { follows } => {
                    self.seen.push(HostEvent::Pointing { follows });
                }
                HostEvent::ClockSet(time) => self.seen.push(HostEvent::ClockSet(time)),
                HostEvent::Lights(lights) => self.seen.push(HostEvent::Lights(lights)),
                HostEvent::Resumed(resumed) => self.seen.push(HostEvent::Resumed(resumed)),
                HostEvent::Suspended(outcome) => {
                    self.seen.push(HostEvent::Suspended(outcome));
                    self.kept = self.host.take_kept();
                }
                HostEvent::TextShown(text) => self.seen.push(HostEvent::TextShown(text)),
                HostEvent::Shared { slot, drive } => {
                    self.seen.push(HostEvent::Shared { slot, drive });
                }
                HostEvent::Folder { slot, op } => {
                    let server = self
                        .folders
                        .iter_mut()
                        .find(|(held, _)| *held == slot)
                        .map(|(_, server)| server)
                        .unwrap();
                    let answer = server.call(&op);
                    self.host.answer_folder(answer).unwrap();
                }
                HostEvent::Text(outcome) => self.texts.push(outcome.map(|text| ShownText {
                    columns: text.columns,
                    rows: text.rows,
                    codes: text.codes.to_vec(),
                })),
                HostEvent::Compile {
                    request,
                    model,
                    cs,
                    ip,
                    bytes,
                } => {
                    let block = Request {
                        number: request,
                        key: Key { cs, ip },
                        model,
                        bytes: bytes.to_vec(),
                    };
                    self.compile(&block);
                }
                HostEvent::Stopped { reason, .. } => self.stops.push(reason),
                HostEvent::Seen(seen) => self.looks.push(seen.number),
                HostEvent::Interpreting => self.interpreting += 1,
            }
        }
        self.host.on_writable();
    }
}

/// The worker's end: it runs the application whenever it has nothing to
/// read, and its waits move the clock.
struct WorkerEnd {
    pipes: Rc<RefCell<Pipes>>,
    app: Rc<RefCell<App>>,
}

impl Channel for WorkerEnd {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.read_within(buf, None)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.pipes.borrow_mut().to_front.extend_from_slice(buf);
        Ok(buf.len())
    }
}

impl TimedChannel for WorkerEnd {
    fn read_within(&mut self, buf: &mut [u8], wait: Option<u64>) -> Result<usize, Errno> {
        if self.pipes.borrow().to_worker.is_empty() {
            self.app.borrow_mut().pump();
        }
        let mut pipes = self.pipes.borrow_mut();
        if pipes.to_worker.is_empty() {
            return match wait {
                // The application has nothing more to say: the test is over.
                None => Ok(0),
                Some(wait) => {
                    pipes.clock += wait;
                    Err(Errno::TimedOut)
                }
            };
        }
        let n = buf.len().min(pipes.to_worker.len());
        buf[..n].copy_from_slice(&pipes.to_worker[..n]);
        pipes.to_worker.drain(..n);
        Ok(n)
    }
}

std::thread_local! {
    static CLOCK: RefCell<Option<Rc<RefCell<Pipes>>>> = const { RefCell::new(None) };
}

extern crate std;

fn clock() -> Nanos {
    CLOCK.with(|pipes| {
        pipes
            .borrow()
            .as_ref()
            .map_or(0, |pipes| pipes.borrow().clock)
    })
}

type Worker = Stepped<SandboxLink<WorkerEnd>, HeapShare, TestMemory>;

/// The turbo XT the joined tests run, and its default disk.
fn turbo_xt() -> (Setup, Box<dyn BlockDevice>) {
    let disk = default_disk(Recording::Rll, 0x2026_1111).unwrap();
    let fixed = FixedDisk {
        medium: 1,
        sectors: disk.device.sectors(),
        read_only: false,
        kind: disk.kind,
        format: disk.low_level,
    };
    let setup = Setup {
        config: Template::TurboXt.config(),
        fixed: [Some(fixed), None],
        audio_target: 2048,
        room: 96 << 20,
        seed: [3; 16],
        band: PressureBand::Normal,
        clock: None,
        resume: None,
    };
    (setup, Box::new(disk.device))
}

/// A turbo XT with its default disk, the application holding the disk, the
/// worker begun and the machine run.
fn joined() -> (Worker, Rc<RefCell<App>>) {
    let (setup, disk) = turbo_xt();
    let (worker, app) = join(setup, disk, (None, false));
    app.borrow_mut().host.run().unwrap();
    (worker, app)
}

/// The machine `setup` describes, the application holding `disk`, the
/// worker begun on `kept` where it is a snapshot to resume, and the blocks
/// it asks for compiled where `compiling`.
fn join(
    setup: Setup,
    disk: Box<dyn BlockDevice>,
    (kept, compiling): (Option<Vec<u8>>, bool),
) -> (Worker, Rc<RefCell<App>>) {
    let pipes = Rc::new(RefCell::new(Pipes::default()));
    CLOCK.with(|clock| *clock.borrow_mut() = Some(Rc::clone(&pipes)));
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let front = FrontEnd {
        pipes: Rc::clone(&pipes),
        share: Arc::new(share.clone()),
    };
    let host = match kept {
        Some(kept) => Host::resume(front, DiscardSink, setup, kept, 0),
        None => Host::start(front, DiscardSink, setup),
    }
    .unwrap();
    let app = Rc::new(RefCell::new(App {
        host,
        media: vec![(1, disk)],
        frames: 0,
        last_frame: Vec::new(),
        audio_frames: 0,
        seen: Vec::new(),
        ended: None,
        kept: None,
        texts: Vec::new(),
        folders: Vec::new(),
        compiler: None,
        compiled: 0,
        stops: Vec::new(),
        looks: Vec::new(),
        interpreting: 0,
    }));
    let channel = WorkerEnd {
        pipes,
        app: Rc::clone(&app),
    };
    let link = SandboxLink::new(WorkerLink::new(channel, WORKER_INBOUND).unwrap(), clock);
    let (executor, compiler) = Interpreted::new();
    app.borrow_mut().compiler = compiling.then_some(compiler);
    let worker = Worker::begin(link, share, (memory(), Box::new(executor)), &DISCARD).unwrap();
    (worker, app)
}

fn turn_until(worker: &mut Worker, within: Nanos, seen: impl Fn(&Worker) -> bool) {
    let deadline = clock() + within;
    while !seen(worker) {
        assert!(
            clock() < deadline,
            "never got there:\n{}",
            screen(worker.machine())
        );
        assert_eq!(worker.turn(), None);
    }
}

fn at_prompt(worker: &Worker) -> bool {
    crate::dos::waits_for_key(worker.machine())
        && screen(worker.machine()).trim_end().ends_with("C:\\>")
}

#[test]
fn the_default_disk_boots_to_the_prompt_through_the_host_and_its_application() {
    let (mut worker, app) = joined();
    turn_until(&mut worker, 60 * SECOND, at_prompt);
    let app = app.borrow();
    assert_eq!(app.seen.first(), Some(&HostEvent::Ready));
    assert!(app.frames > 0 && app.audio_frames > 0);
    assert!(
        app.last_frame.chunks(4).any(|p| p[..3] != [0, 0, 0]),
        "the frame copied out shows the prompt"
    );
    assert_eq!(app.ended, None);
}

#[test]
fn a_folder_shared_through_the_host_is_a_drive_until_it_is_unshared() {
    let folder = MemHost::new().file("readme.txt", b"Through the host.\r\n");
    let (mut worker, app) = joined();
    let label = DosName::new(*b"HOST       ", false).unwrap();
    app.borrow_mut()
        .folders
        .push((0, Server::new(folder.root(), label, true, 0x99)));
    app.borrow_mut().host.share(0, label, true).unwrap();
    assert_eq!(
        app.borrow_mut().host.share(0, label, true),
        Err(Refused::Busy),
        "the slot shares a folder already"
    );
    turn_until(&mut worker, 60 * SECOND, at_prompt);
    assert!(app.borrow().seen.contains(&HostEvent::Shared {
        slot: 0,
        drive: Some(3)
    }));
    for &byte in b"type d:readme.txt\r" {
        for &(scan, down) in crate::bios::keystrokes(byte, false).as_slice() {
            app.borrow_mut().host.key(scan, down);
        }
    }
    let typed = clock();
    turn_until(&mut worker, 30 * SECOND, |w| {
        clock() > typed + SECOND
            && at_prompt(w)
            && screen(w.machine()).contains("Through the host.")
    });
    app.borrow_mut().host.unshare(0).unwrap();
    assert_eq!(
        app.borrow_mut().host.unshare(0),
        Err(Refused::NoRequest),
        "nothing is shared there now"
    );
    assert_eq!(worker.turn(), None);
    assert_eq!(worker.machine().board().shared_drive(3), None);
}

#[test]
fn typing_through_the_host_reaches_the_guest_and_its_writes_the_image() {
    let (mut worker, app) = joined();
    turn_until(&mut worker, 60 * SECOND, at_prompt);
    for &byte in b"md c:\\joined\r" {
        for &(scan, down) in crate::bios::keystrokes(byte, false).as_slice() {
            app.borrow_mut().host.key(scan, down);
        }
    }
    let typed = clock();
    turn_until(&mut worker, 30 * SECOND, |w| {
        clock() > typed + SECOND && at_prompt(w)
    });
    assert!(
        screen(worker.machine()).contains("C:\\>md c:\\joined"),
        "{}",
        screen(worker.machine())
    );
    let image = &mut app.borrow_mut().media[0].1;
    let mut found = false;
    let mut sector = [0u8; SECTOR];
    for n in 0..image.sectors() {
        image.read(n, &mut sector).unwrap();
        found |= sector.windows(11).any(|w| w == b"JOINED     ");
    }
    assert!(found, "the directory is on the application's image");
}

#[test]
fn input_beyond_the_window_waits_in_the_host_and_motion_runs_together() {
    let (_worker, app) = joined();
    let mut app = app.borrow_mut();
    for _ in 0..INPUT_WINDOW * 2 {
        app.host.key(Scan::plain(0x1E), true);
    }
    // The start and the run took two of the window.
    let held = INPUT_WINDOW as usize + 2;
    assert_eq!(
        app.host.held(),
        held,
        "the window's worth sent, the rest held"
    );
    app.host.mouse(3, 4, 1);
    app.host.mouse(-1, 2, 1);
    assert_eq!(
        app.host.held(),
        held + 1,
        "two motions with the buttons unchanged are one"
    );
    app.host.mouse(0, 0, 0);
    assert_eq!(app.host.held(), held + 2);
    app.host.point(0, 10, 10, 0);
    app.host.point(0, 300, 20, 0);
    assert_eq!(app.host.held(), held + 3, "a run of places is the last");
    app.host.point(1, 300, 20, 0);
    app.host.point(1, 300, 20, 1);
    assert_eq!(
        app.host.held(),
        held + 5,
        "another adapter's, and a button's change, each kept"
    );
    while app.host.held() < QUEUE_MOST - RELEASES_KEPT {
        app.host.key(Scan::plain(0x1F), true);
    }
    app.host.key(Scan::plain(0x20), true);
    app.host.key(Scan::plain(0x20), false);
    assert_eq!(
        app.host.held(),
        QUEUE_MOST - RELEASES_KEPT,
        "a press with no room is dropped, its release with it"
    );
    app.host.key(Scan::plain(0x1F), false);
    assert_eq!(
        app.host.held(),
        QUEUE_MOST - RELEASES_KEPT + 1,
        "a release always has room"
    );
}

/// A host whose worker is the test: its events are written straight into
/// the host's pipe.
fn host_alone(setup: Setup) -> (Host<FrontEnd, DiscardSink>, Rc<RefCell<Pipes>>) {
    let pipes = Rc::new(RefCell::new(Pipes::default()));
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let front = FrontEnd {
        pipes: Rc::clone(&pipes),
        share: Arc::new(share),
    };
    (Host::start(front, DiscardSink, setup).unwrap(), pipes)
}

fn plain_setup() -> Setup {
    Setup {
        config: Template::IbmXt.config(),
        fixed: [None, None],
        audio_target: 2048,
        room: 96 << 20,
        seed: [0; 16],
        band: PressureBand::Normal,
        clock: None,
        resume: None,
    }
}

/// Write `event` into the host's pipe as the worker would.
fn from_worker(pipes: &Rc<RefCell<Pipes>>, event: &Event<'_>) {
    let mut w = Writer::new();
    event.encode(&mut w);
    let payload = w.finish();
    let mut pipes = pipes.borrow_mut();
    pipes
        .to_front
        .extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    pipes.to_front.extend_from_slice(&payload);
}

/// What the host made of `events`, after a ready.
fn verdict(events: &[Event<'_>]) -> Option<Ending> {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Ready);
    for event in events {
        from_worker(&pipes, event);
    }
    host.on_readable();
    let mut ending = None;
    while let Some(event) = host.event() {
        if let HostEvent::Ended(end) = event {
            ending = Some(end);
        }
        host.on_readable();
    }
    ending
}

#[test]
fn a_worker_that_breaks_the_protocol_is_ended_for_it() {
    let sector = [0u8; SECTOR];
    let cases: [(&[Event<'_>], &str); 11] = [
        (
            &[Event::Frame {
                slot: 9,
                width: 1,
                height: 1,
            }],
            "a slot past the layout",
        ),
        (
            &[Event::Frame {
                slot: 0,
                width: 4000,
                height: 1,
            }],
            "a frame larger than its slot",
        ),
        (
            &[Event::Fetch {
                request: 0,
                medium: 7,
                first: 0,
                count: 1,
            }],
            "a medium never named",
        ),
        (
            &[Event::Fetch {
                request: 5,
                medium: 7,
                first: 0,
                count: 1,
            }],
            "a request out of turn",
        ),
        (
            &[Event::Store {
                request: 0,
                medium: 1,
                sector: 0,
                data: &sector,
            }],
            "a medium never named",
        ),
        (
            &[Event::Credit { applied: 2 }],
            "credit for commands never sent",
        ),
        (&[Event::Ready], "ready twice"),
        (
            &[Event::Ejected {
                drive: 0,
                medium: 3,
            }],
            "an image ejected that was not in its drive",
        ),
        (
            &[Event::Inserted {
                drive: 1,
                medium: 3,
                outcome: Ok(()),
            }],
            "an answer to an insert never asked",
        ),
        (
            &[Event::Lights(Lights {
                floppy: 0b10,
                fixed: 0,
            })],
            "a lamp of a drive the machine does not have",
        ),
        (
            &[Event::Lights(Lights {
                floppy: 0,
                fixed: 0b01,
            })],
            "a lamp of a drive the machine does not have",
        ),
    ];
    for (events, why) in cases {
        assert_eq!(verdict(events), Some(Ending::Violation(why)), "{events:?}");
    }
    assert_eq!(
        verdict(&[
            Event::OutOfRange { adapter: 0 },
            Event::Credit { applied: 1 },
            Event::Lights(Lights {
                floppy: 0b01,
                fixed: 0
            }),
        ]),
        None,
        "those are honest: the XT has drive A and no fixed disk attached"
    );
}

#[test]
fn a_machine_suspended_through_the_host_resumes_through_it() {
    let (mut worker, app) = joined();
    turn_until(&mut worker, 60 * SECOND, at_prompt);
    let shown = screen(worker.machine());
    app.borrow_mut().host.suspend().unwrap();
    assert_eq!(
        app.borrow_mut().host.suspend(),
        Err(Refused::Suspending),
        "asked once"
    );
    while app.borrow().kept.is_none() {
        assert_eq!(worker.turn(), None);
        app.borrow_mut().pump();
    }
    assert!(app.borrow().seen.contains(&HostEvent::Suspended(Ok(()))));
    let kept = app.borrow_mut().kept.take().unwrap();
    let disk = app.borrow_mut().media.pop().unwrap().1;
    drop(worker);
    let (setup, _) = turbo_xt();
    let (mut again, app) = join(setup, disk, (Some(kept), false));
    assert_eq!(
        again.turn(),
        Some(End::Closed),
        "held, it waits for a command"
    );
    assert_eq!(
        app.borrow().seen[..2],
        [HostEvent::Resumed(true), HostEvent::Ready],
        "it says it resumed before it is ready"
    );
    assert_eq!(
        screen(again.machine()),
        shown,
        "and stands where it stopped"
    );
}

#[test]
fn a_machine_says_it_is_ready_before_it_is_told_to_run() {
    let (setup, disk) = turbo_xt();
    let (mut worker, app) = join(setup, disk, (None, false));
    assert_eq!(
        worker.turn(),
        Some(End::Closed),
        "held, it waits for a command"
    );
    assert_eq!(app.borrow().seen, [HostEvent::Ready]);
}

/// What the host made of `events` from a worker resuming a snapshot of
/// `kept` bytes, and what it sent the worker.
fn resume_verdict(
    kept: usize,
    events: &[Event<'_>],
) -> (Option<Ending>, Vec<HostEvent<'static>>, Vec<u8>) {
    let pipes = Rc::new(RefCell::new(Pipes::default()));
    let setup = plain_setup();
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let front = FrontEnd {
        pipes: Rc::clone(&pipes),
        share: Arc::new(share),
    };
    let snapshot: Vec<u8> = (0..kept).map(|n| u8::try_from(n % 251).unwrap()).collect();
    let mut host = Host::resume(front, DiscardSink, setup, snapshot, 0).unwrap();
    for event in events {
        from_worker(&pipes, event);
    }
    host.on_readable();
    let (mut ending, mut seen) = (None, Vec::new());
    while let Some(event) = host.event() {
        match event {
            HostEvent::Ended(end) => ending = Some(end),
            HostEvent::Resumed(resumed) => seen.push(HostEvent::Resumed(resumed)),
            HostEvent::Ready => seen.push(HostEvent::Ready),
            _ => {}
        }
        host.on_readable();
    }
    host.on_writable();
    let sent = core::mem::take(&mut pipes.borrow_mut().to_worker);
    (ending, seen, sent)
}

#[test]
fn a_resuming_worker_is_answered_its_snapshot_and_nothing_past_it() {
    let (ending, seen, sent) = resume_verdict(
        1000,
        &[
            Event::Recall {
                request: 0,
                offset: 990,
                len: 10,
            },
            Event::Resumed(true),
            Event::Ready,
        ],
    );
    assert_eq!(ending, None);
    assert_eq!(seen, [HostEvent::Resumed(true), HostEvent::Ready]);
    let answer: Vec<u8> = (990..1000)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    let mut w = Writer::new();
    Command::Recalled {
        request: 0,
        bytes: &answer,
    }
    .encode(&mut w);
    let expected = w.finish();
    assert!(
        sent.windows(expected.len()).any(|bytes| bytes == expected),
        "the bytes recalled were sent"
    );
    let cases: [(&[Event<'_>], &str); 4] = [
        (
            &[Event::Recall {
                request: 0,
                offset: 995,
                len: 10,
            }],
            "a recall past the snapshot",
        ),
        (
            &[Event::Recall {
                request: 1,
                offset: 0,
                len: 10,
            }],
            "a request out of turn",
        ),
        (&[Event::Ready], "ready before saying whether it resumed"),
        (
            &[Event::Resumed(false), Event::Resumed(false)],
            "a resume never asked",
        ),
    ];
    for (events, why) in cases {
        assert_eq!(
            resume_verdict(1000, events).0,
            Some(Ending::Violation(why)),
            "{events:?}"
        );
    }
}

#[test]
fn a_snapshot_nobody_asked_for_or_cut_short_ends_the_worker() {
    assert_eq!(
        verdict(&[Event::Resumed(true)]),
        Some(Ending::Violation("a resume never asked"))
    );
    assert_eq!(
        verdict(&[Event::Kept(&[1, 2, 3])]),
        Some(Ending::Violation("a snapshot never asked for"))
    );
    assert_eq!(
        verdict(&[Event::Suspended(Ok(0))]),
        Some(Ending::Violation("a suspend never asked"))
    );
    let (mut host, pipes) = host_alone(plain_setup());
    host.suspend().unwrap();
    for event in [
        Event::Ready,
        Event::Kept(&[1, 2, 3]),
        Event::Suspended(Ok(4)),
    ] {
        from_worker(&pipes, &event);
    }
    host.on_readable();
    let mut ending = None;
    while let Some(event) = host.event() {
        if let HostEvent::Ended(end) = event {
            ending = Some(end);
        }
        host.on_readable();
    }
    assert_eq!(
        ending,
        Some(Ending::Violation(
            "a snapshot not the length it was said to be"
        ))
    );
}

#[test]
fn a_worker_naming_its_events_before_it_is_ready_is_ended() {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Credit { applied: 1 });
    host.on_readable();
    assert_eq!(
        host.event(),
        Some(HostEvent::Ended(Ending::Violation(
            "an event before the machine was ready"
        )))
    );
    assert_eq!(host.event(), None, "said once");
    assert_eq!(host.run(), Err(Refused::Ended));
}

/// How a host whose first fixed disk is a read-only 100-sector image ends
/// once its ready worker says `events`, if it does.
fn ending_after(events: &[Event<'_>]) -> Option<Ending> {
    let mut setup = plain_setup();
    setup.fixed[0] = Some(FixedDisk {
        medium: 1,
        sectors: 100,
        read_only: true,
        kind: crate::dos::setup::ST_4038,
        format: LowLevel {
            interleave: 3,
            bad_tracks: Vec::new(),
        },
    });
    let (mut host, pipes) = host_alone(setup);
    from_worker(&pipes, &Event::Ready);
    for event in events {
        from_worker(&pipes, event);
    }
    host.on_readable();
    let mut ending = None;
    while let Some(event) = host.event() {
        if let HostEvent::Ended(end) = event {
            ending = Some(end);
        }
    }
    ending
}

#[test]
fn a_fetch_past_its_medium_and_a_second_request_are_refused() {
    assert_eq!(
        ending_after(&[Event::Fetch {
            request: 0,
            medium: 1,
            first: 99,
            count: 1
        }]),
        None,
        "the last sector"
    );
    assert_eq!(
        ending_after(&[Event::Fetch {
            request: 0,
            medium: 1,
            first: 99,
            count: 2
        }]),
        Some(Ending::Violation("a run past its medium"))
    );
    assert_eq!(
        ending_after(&[Event::Fetch {
            request: 0,
            medium: 1,
            first: 0,
            count: 0
        }]),
        Some(Ending::Violation("a run past its medium"))
    );
    let twice = [
        Event::Fetch {
            request: 0,
            medium: 1,
            first: 0,
            count: 1,
        },
        Event::Fetch {
            request: 1,
            medium: 1,
            first: 0,
            count: 1,
        },
    ];
    assert_eq!(
        ending_after(&twice),
        Some(Ending::Violation(
            "a second request before the first was answered"
        ))
    );
    let sector = [0u8; SECTOR];
    assert_eq!(
        ending_after(&[Event::Store {
            request: 0,
            medium: 1,
            sector: 0,
            data: &sector
        }]),
        Some(Ending::Violation("a store the medium cannot take")),
        "read-only"
    );
}

#[test]
fn a_format_naming_tracks_its_drive_lacks_is_refused() {
    let bad = LowLevel {
        interleave: 3,
        bad_tracks: vec![733 * 5],
    };
    assert_eq!(
        ending_after(&[Event::Formatted {
            drive: 0,
            format: bad
        }]),
        Some(Ending::Violation("a format the drive cannot have"))
    );
    let unsorted = LowLevel {
        interleave: 3,
        bad_tracks: vec![9, 4],
    };
    assert_eq!(
        ending_after(&[Event::Formatted {
            drive: 0,
            format: unsorted
        }]),
        Some(Ending::Violation("a format the drive cannot have"))
    );
    assert_eq!(
        ending_after(&[Event::Formatted {
            drive: 1,
            format: LowLevel {
                interleave: 1,
                bad_tracks: Vec::new()
            }
        }]),
        Some(Ending::Violation(
            "a format of a drive the machine does not have"
        ))
    );
}

#[test]
fn an_answer_with_no_request_or_of_the_wrong_size_is_refused() {
    let (mut host, _pipes) = host_alone(plain_setup());
    assert_eq!(host.answer_fetch(Ok(&[0; SECTOR])), Err(Refused::NoRequest));
    assert_eq!(host.answer_store(Ok(())), Err(Refused::NoRequest));
    assert_eq!(
        host.insert(
            4,
            Form::Sectors {
                count: 720,
                read_only: false
            }
        ),
        Err(Refused::NoDrive)
    );
    let first = host
        .insert(
            0,
            Form::Sectors {
                count: 720,
                read_only: false,
            },
        )
        .unwrap();
    assert_eq!(
        host.insert(
            0,
            Form::Sectors {
                count: 720,
                read_only: false
            }
        ),
        Err(Refused::Busy),
        "the first not yet answered"
    );
    assert_eq!(
        first, 1,
        "named after the fixed disks, of which there are none"
    );
}

#[test]
fn a_share_smaller_than_the_layout_refuses_the_start() {
    let setup = plain_setup();
    let pipes = Rc::new(RefCell::new(Pipes::default()));
    let share = HeapShare::new(Layout::of(&setup.config.video).len() - 1).unwrap();
    let front = FrontEnd {
        pipes,
        share: Arc::new(share),
    };
    assert!(matches!(
        Host::start(front, DiscardSink, setup),
        Err(StartError::Share)
    ));
}

#[test]
fn a_command_the_host_cannot_frame_ends_the_worker_rather_than_leaving_it_out_of_reach() {
    let (mut host, _pipes) = host_alone(plain_setup());
    host.out = Vec::new();
    let held = tairix_fuzzseed::meter::refusing_above(0, || host.run());
    assert_eq!(held, Ok(()), "the command was taken");
    assert!(
        !host.wants_read() && !host.wants_write(),
        "the session was contained"
    );
    assert_eq!(host.event(), Some(HostEvent::Ended(Ending::Failed)));
    assert_eq!(host.run(), Err(Refused::Ended));
}

/// The commands the host has written for its worker since last asked.
fn sent(host: &mut Host<FrontEnd, DiscardSink>, pipes: &Rc<RefCell<Pipes>>) -> Vec<Vec<u8>> {
    host.on_writable();
    let bytes = core::mem::take(&mut pipes.borrow_mut().to_worker);
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        frames.push(bytes[at + 4..at + 4 + len].to_vec());
        at += 4 + len;
    }
    frames
}

fn decoded(frames: &[Vec<u8>]) -> Vec<Command<'_>> {
    frames
        .iter()
        .map(|frame| Command::decode(frame).unwrap())
        .collect()
}

/// The characters each command sent typed.
fn pieces(frames: &[Vec<u8>]) -> Vec<usize> {
    decoded(frames)
        .iter()
        .filter_map(|command| match command {
            Command::Type(codes) => Some(codes.len()),
            _ => None,
        })
        .collect()
}

/// A host whose worker is the test and has said it is ready, its start
/// already taken.
fn ready_host() -> (Host<FrontEnd, DiscardSink>, Rc<RefCell<Pipes>>) {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Ready);
    take_events(&mut host);
    sent(&mut host, &pipes);
    (host, pipes)
}

fn take_events(host: &mut Host<FrontEnd, DiscardSink>) {
    host.on_readable();
    while host.event().is_some() {}
}

#[test]
fn a_read_of_the_text_is_answered_once_and_text_unasked_ends_the_worker() {
    assert_eq!(
        verdict(&[Event::Text(Err(Unread::NoText))]),
        Some(Ending::Violation("text never asked for"))
    );
    let (mut host, pipes) = ready_host();
    host.read_text().unwrap();
    assert_eq!(host.read_text(), Err(Refused::Busy));
    assert_eq!(decoded(&sent(&mut host, &pipes)), [Command::ReadText]);
    let text = Text {
        columns: 2,
        rows: 1,
        codes: b"\x01b",
    };
    from_worker(&pipes, &Event::Text(Ok(text)));
    host.on_readable();
    assert_eq!(host.event(), Some(HostEvent::Text(Ok(text))));
    host.read_text().unwrap();
    from_worker(&pipes, &Event::Text(Err(Unread::TooLarge)));
    host.on_readable();
    assert_eq!(host.event(), Some(HostEvent::Text(Err(Unread::TooLarge))));
}

#[test]
fn text_to_type_crosses_within_the_window_and_a_key_pressed_stops_it() {
    let (mut host, pipes) = ready_host();
    host.type_text(vec![b'a'; 3000]).unwrap();
    assert!(host.typing());
    assert_eq!(host.type_text(vec![b'b']), Err(Refused::Busy));
    assert_eq!(pieces(&sent(&mut host, &pipes)), [TYPE_WINDOW]);
    from_worker(&pipes, &Event::Typed(100));
    take_events(&mut host);
    assert!(
        pieces(&sent(&mut host, &pipes)).is_empty(),
        "less than half the window waits"
    );
    from_worker(&pipes, &Event::Typed(500));
    take_events(&mut host);
    assert_eq!(pieces(&sent(&mut host, &pipes)), [600]);
    let a = Scan::plain(0x1E);
    host.key(a, true);
    assert_eq!(
        decoded(&sent(&mut host, &pipes)),
        [
            Command::StopTyping,
            Command::Key {
                scan: a,
                down: true
            }
        ]
    );
    assert!(host.typing(), "until the worker accounts for what it holds");
    from_worker(&pipes, &Event::Typed(1024));
    take_events(&mut host);
    assert!(!host.typing());
    assert!(
        sent(&mut host, &pipes).is_empty(),
        "what waited here was dropped"
    );
}

#[test]
fn a_reset_drops_the_text_waiting_to_be_typed() {
    let (mut host, pipes) = ready_host();
    host.type_text(vec![b'a'; 3000]).unwrap();
    sent(&mut host, &pipes);
    host.reset().unwrap();
    from_worker(&pipes, &Event::Typed(1024));
    take_events(&mut host);
    assert_eq!(decoded(&sent(&mut host, &pipes)), [Command::Reset]);
    assert!(!host.typing());
}

#[test]
fn text_to_type_waits_behind_every_command_held_before_it() {
    let (mut host, pipes) = ready_host();
    let a = Scan::plain(0x1E);
    for _ in 0..INPUT_WINDOW + 3 {
        host.key(a, false);
    }
    host.type_text(b"dir".to_vec()).unwrap();
    let before = sent(&mut host, &pipes);
    assert_eq!(
        before.len(),
        INPUT_WINDOW as usize - 1,
        "the start took one"
    );
    assert!(
        pieces(&before).is_empty(),
        "keys are held, so the text waits"
    );
    from_worker(
        &pipes,
        &Event::Credit {
            applied: INPUT_WINDOW,
        },
    );
    take_events(&mut host);
    let after = sent(&mut host, &pipes);
    assert_eq!(
        decoded(&after),
        [
            Command::Key {
                scan: a,
                down: false
            },
            Command::Key {
                scan: a,
                down: false
            },
            Command::Key {
                scan: a,
                down: false
            },
            Command::Key {
                scan: a,
                down: false
            },
            Command::Type(b"dir"),
        ]
    );
}

#[test]
fn text_no_key_types_is_refused_and_typing_more_than_given_ends_the_worker() {
    let (mut host, _pipes) = ready_host();
    assert_eq!(host.type_text(Vec::new()), Err(Refused::Untypeable));
    assert_eq!(host.type_text(vec![b'a', 0x07]), Err(Refused::Untypeable));
    assert_eq!(
        host.type_text(vec![b'a'; MAX_TYPED + 1]),
        Err(Refused::Untypeable)
    );
    assert!(!host.typing());
    assert_eq!(
        verdict(&[Event::Typed(1)]),
        Some(Ending::Violation("typed more than it was given"))
    );
}

/// The block events `host` lends, as `(request, model, cs, ip, bytes)`.
fn blocks_lent(host: &mut Host<FrontEnd, DiscardSink>) -> Vec<(u32, CpuModel, u16, u16, Vec<u8>)> {
    host.on_readable();
    let mut lent = Vec::new();
    while let Some(event) = host.event() {
        if let HostEvent::Compile {
            request,
            model,
            cs,
            ip,
            bytes,
        } = event
        {
            lent.push((request, model, cs, ip, bytes.to_vec()));
        }
        host.on_readable();
    }
    lent
}

/// The commands the host has sent the worker.
fn sent_commands(pipes: &Rc<RefCell<Pipes>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = &pipes.borrow().to_worker[..];
    while at.len() >= 4 {
        let len = u32::from_le_bytes(at[..4].try_into().unwrap()) as usize;
        out.push(at[4..4 + len].to_vec());
        at = &at[4 + len..];
    }
    out
}

fn compiled(request: u32, code: Option<Installed>) -> Vec<u8> {
    let mut w = Writer::new();
    Command::Compiled { request, code }.encode(&mut w);
    w.finish()
}

fn block(request: u32) -> Event<'static> {
    Event::Compile {
        request,
        cs: 0x1000,
        ip: 0x0100,
        bytes: &[0x40, 0xEB, 0xFE],
    }
}

#[test]
fn blocks_are_lent_with_the_machine_s_processor_and_answered_each_once() {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Ready);
    for request in 0..4 {
        from_worker(&pipes, &block(request));
    }
    let lent = blocks_lent(&mut host);
    let model = plain_setup().config.cpu;
    assert_eq!(
        lent,
        (0..4)
            .map(|request| (request, model, 0x1000, 0x0100, vec![0x40, 0xEB, 0xFE]))
            .collect::<Vec<_>>()
    );
    let code = Installed {
        address: 0x4_0000_0000,
        len: 200,
    };
    host.answer_block(2, Some(code)).unwrap();
    assert_eq!(
        host.answer_block(2, None),
        Err(Refused::NoRequest),
        "answered"
    );
    assert_eq!(
        host.answer_block(9, None),
        Err(Refused::NoRequest),
        "never asked"
    );
    host.on_writable();
    assert!(sent_commands(&pipes).contains(&compiled(2, Some(code))));
    // The window has room again for one more.
    from_worker(&pipes, &block(4));
    assert_eq!(blocks_lent(&mut host).len(), 1);
    assert_eq!(host.ended, None);
}

#[test]
fn a_block_asked_for_out_of_turn_or_past_the_window_ends_the_worker() {
    assert_eq!(
        verdict(&[block(1)]),
        Some(Ending::Violation("a block asked for out of turn"))
    );
    let over: Vec<Event<'_>> = (0..5).map(block).collect();
    assert_eq!(
        verdict(&over),
        Some(Ending::Violation(
            "more blocks asked for than the window holds"
        ))
    );
    assert_eq!(
        verdict(&[block(0), block(0)]),
        Some(Ending::Violation("a block asked for out of turn"))
    );
}

#[test]
fn a_worker_whose_blocks_misbehaved_says_so_once_and_its_blocks_are_answered_here() {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Ready);
    from_worker(&pipes, &Event::Interpreting);
    from_worker(&pipes, &block(0));
    host.on_readable();
    let (mut said, mut lent) = (0, 0);
    while let Some(event) = host.event() {
        match event {
            HostEvent::Interpreting => said += 1,
            HostEvent::Compile { .. } => lent += 1,
            _ => {}
        }
        host.on_readable();
    }
    assert_eq!((said, lent), (1, 0), "said, and no block lent after");
    host.on_writable();
    let sent = sent_commands(&pipes);
    assert!(sent.contains(&compiled(0, None)), "answered with no code");
    let mut w = Writer::new();
    Command::Interpret.encode(&mut w);
    assert!(!sent.contains(&w.finish()), "it asks for no more already");
    assert_eq!(
        verdict(&[Event::Interpreting, Event::Interpreting]),
        Some(Ending::Violation(
            "a second word that its blocks misbehaved"
        ))
    );
}

#[test]
fn a_host_interpreting_answers_every_block_itself_with_no_code() {
    let (mut host, pipes) = host_alone(plain_setup());
    from_worker(&pipes, &Event::Ready);
    from_worker(&pipes, &block(0));
    assert_eq!(blocks_lent(&mut host).len(), 1, "asked before it was told");
    host.interpret();
    host.interpret();
    for request in 1..10 {
        from_worker(&pipes, &block(request));
        assert!(
            blocks_lent(&mut host).is_empty(),
            "none lent once interpreting"
        );
    }
    host.answer_block(0, None).unwrap();
    host.on_writable();
    let mut w = Writer::new();
    Command::Interpret.encode(&mut w);
    let interpret = w.finish();
    let sent = sent_commands(&pipes);
    assert_eq!(
        sent.iter().filter(|command| **command == interpret).count(),
        1,
        "told once"
    );
    for request in 0..10 {
        assert!(sent.contains(&compiled(request, None)), "block {request}");
    }
    assert_eq!(host.ended, None);
}

#[test]
fn a_machine_whose_host_compiles_its_blocks_runs_them_to_the_prompt() {
    let (setup, disk) = turbo_xt();
    let (mut worker, app) = join(setup, disk, (None, true));
    app.borrow_mut().host.run().unwrap();
    turn_until(&mut worker, 60 * SECOND, at_prompt);
    let app = app.borrow();
    assert!(app.compiled >= 1, "{} blocks compiled", app.compiled);
    assert!(app.compiler.as_ref().unwrap().runs() > 0, "and run");
    assert_eq!(app.ended, None);
}

#[cfg(test)]
#[path = "debugger_tests.rs"]
mod debugger;
