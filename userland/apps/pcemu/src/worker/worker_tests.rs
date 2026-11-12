//! The worker run against a stand-in for its front-end: a link that answers
//! every fetch and store from images it holds, hands each frame slot straight
//! back, and keeps a clock that moves only by the waits the worker asks for.

use super::port::{End, Link};
use super::*;
use crate::chips::keyboard::Scan;
use crate::cpu::jit::cache::Request;
use crate::cpu::jit::interpreted::{Compiler, Interpreted};
use crate::cpu::CpuModel;
use crate::dos::harness::screen;
use crate::dos::setup::default_disk;
use crate::link::debug::Said;
use crate::link::layout::Layout;
use crate::link::{
    Command, Event, FixedDisk, Form, InsertRefusal, Refusal, Resume, Setup, Unkept, INPUT_WINDOW,
    TYPE_WINDOW,
};
use crate::machine::Template;
use crate::storage::hdc::{LowLevel, Recording};
use crate::storage::{BlockDevice, MediaError, MemoryDisk, SECTOR};
use crate::time::NANOS_PER_SECOND as SECOND;
use crate::video::{ShownText, Unread};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_abi::time::CivilTime;
use tairix_reclaim::PressureBand;
use tairix_sandbox::link::Waited;
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::session::Share;
use tairix_sandbox::wire::Writer;
use tairix_sandbox::ProtoError;

use super::testing::{memory, TestMemory, DISCARD};
use crate::folder::memhost::{MemDir, MemHost};
use crate::folder::server::Server;
use crate::folder::{DosName, Folder};

/// What the stand-in front-end saw and holds.
#[derive(Default)]
struct Front {
    queued: VecDeque<Vec<u8>>,
    clock: Nanos,
    media: Vec<(u16, Box<dyn BlockDevice>)>,
    fetches: usize,
    stores: usize,
    frames: Vec<(u8, u16, u16)>,
    audio_frames: usize,
    credit: u32,
    ready: bool,
    refused: Option<Refusal>,
    inserted: Vec<(u8, u16, Result<(), InsertRefusal>)>,
    ejected: Vec<(u8, u16)>,
    out_of_range: Vec<u8>,
    formatted: Vec<(u8, LowLevel)>,
    /// Each change to whether the mouse's driver follows the host's pointer.
    pointing: Vec<bool>,
    /// Each time the guest set its clock card to.
    clock_sets: Vec<CivilTime>,
    /// Each change to what the drives' lamps show.
    lights: Vec<crate::board::Lights>,
    /// Commands sent ahead of the next answer, as a front-end's input that
    /// was already in the pipe.
    ahead: Vec<Vec<u8>>,
    /// Every store is answered with this failure.
    fail_stores: Option<MediaError>,
    /// Slots are handed straight back.
    release: bool,
    /// The snapshot each recall is answered from.
    recall: Vec<u8>,
    /// Whether the machine said it resumed.
    resumed: Option<bool>,
    /// The snapshot pieces the worker sent, and how its suspend ended.
    kept: Vec<u8>,
    suspended: Option<Result<u32, Unkept>>,
    /// Each text the worker read back: its columns, rows and codes.
    texts: Vec<Result<ShownText, Unread>>,
    /// The characters given to type the worker has accounted for.
    typed: u32,
    /// Each change to whether adapter 0 shows text.
    text_shown: Vec<bool>,
    /// The folders it shares, by slot, and each drive it was told one is.
    folders: Vec<(u8, Server<MemDir>)>,
    shared: Vec<(u8, Option<u8>)>,
    /// What compiles the blocks the worker asks for, and the processor they
    /// are of, and whether it does: otherwise every request is left
    /// unanswered.
    compiler: Option<(Compiler, CpuModel)>,
    compiling: bool,
    /// Every block request the worker made, and how many were answered with
    /// code.
    asked: Vec<u32>,
    compiled: usize,
    /// Each stop the debugger reported, and each look it answered: its
    /// number, its lines and what memory and the stack held; the resets
    /// and detaches it said it took.
    stops: Vec<(crate::debug::probe::Reason, crate::debug::Registers)>,
    seen: Vec<Seen>,
    resets: usize,
    detached: usize,
    /// The words that a translated block misbehaved.
    interpreting: usize,
}

/// A look's answer as the stand-in front-end keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    number: u32,
    before: u8,
    lines: Vec<(u16, String)>,
    memory: Vec<u8>,
    stack: Vec<u8>,
}

impl Seen {
    fn of(seen: &crate::link::debug::Seen<'_>) -> Self {
        Self {
            number: seen.number,
            before: seen.listing.before,
            lines: seen
                .listing
                .lines()
                .map(|line| (line.ip, String::from(line.text)))
                .collect(),
            memory: seen.memory.to_vec(),
            stack: seen.stack.to_vec(),
        }
    }
}

impl Front {
    fn queue(&mut self, command: &Command<'_>) {
        self.queued.push_back(encoded(command));
    }

    fn image(&mut self, id: u16) -> &mut Box<dyn BlockDevice> {
        &mut self
            .media
            .iter_mut()
            .find(|(medium, _)| *medium == id)
            .expect("an image the worker named")
            .1
    }

    /// Answer fetch `request` of `count` sectors of `medium` from `first`.
    fn fetch(&mut self, request: u32, medium: u16, (first, count): (u64, u16)) {
        self.fetches += 1;
        let mut data = vec![0u8; usize::from(count) * SECTOR];
        let image = self.image(medium);
        let answer = data
            .chunks_mut(SECTOR)
            .zip(first..)
            .try_for_each(|(sector, n)| image.read(n, sector.try_into().unwrap()));
        let command = Command::Fetched {
            request,
            answer: answer.map(|()| &data[..]),
        };
        self.answer(encoded(&command));
    }

    fn answer(&mut self, command: Vec<u8>) {
        let ahead = core::mem::take(&mut self.ahead);
        self.queued.extend(ahead);
        self.queued.push_back(command);
    }

    fn take(&mut self, payload: &[u8]) {
        match Event::decode(payload).expect("the worker sends only events") {
            Event::Ready => self.ready = true,
            Event::Refused(refusal) => self.refused = Some(refusal),
            Event::Inserted {
                drive,
                medium,
                outcome,
            } => self.inserted.push((drive, medium, outcome)),
            Event::Ejected { drive, medium } => self.ejected.push((drive, medium)),
            Event::Frame {
                slot,
                width,
                height,
            } => {
                self.frames.push((slot, width, height));
                if self.release {
                    self.queue(&Command::Released { slot });
                }
            }
            Event::OutOfRange { adapter } => self.out_of_range.push(adapter),
            Event::Audio(pcm) => self.audio_frames += pcm.len() / 4,
            Event::Fetch {
                request,
                medium,
                first,
                count,
            } => self.fetch(request, medium, (first, count)),
            Event::Store {
                request,
                medium,
                sector,
                data,
            } => {
                self.stores += 1;
                let answer = match self.fail_stores {
                    Some(error) => Err(error),
                    None => self.image(medium).write(sector, data),
                };
                self.answer(encoded(&Command::Stored { request, answer }));
            }
            Event::Formatted { drive, format } => self.formatted.push((drive, format)),
            Event::Credit { applied } => self.credit = applied,
            Event::Pointing { follows } => self.pointing.push(follows),
            Event::ClockSet(time) => self.clock_sets.push(time),
            Event::Lights(lights) => self.lights.push(lights),
            Event::Resumed(resumed) => self.resumed = Some(resumed),
            Event::Recall {
                request,
                offset,
                len,
            } => {
                let (offset, len) = (offset as usize, len as usize);
                let bytes = self.recall[offset..offset + len].to_vec();
                self.answer(encoded(&Command::Recalled {
                    request,
                    bytes: &bytes,
                }));
            }
            Event::Kept(piece) => self.kept.extend_from_slice(piece),
            Event::Suspended(outcome) => self.suspended = Some(outcome),
            Event::Text(outcome) => self.texts.push(outcome.map(|text| ShownText {
                columns: text.columns,
                rows: text.rows,
                codes: text.codes.to_vec(),
            })),
            Event::Typed(count) => self.typed += count,
            Event::TextShown(text) => self.text_shown.push(text),
            Event::Folder { request, slot, op } => {
                let server = self
                    .folders
                    .iter_mut()
                    .find(|(held, _)| *held == slot)
                    .map(|(_, server)| server)
                    .expect("a folder the front-end shares");
                let answer = encoded(&Command::Foldered {
                    request,
                    answer: server.call(&op),
                });
                self.answer(answer);
            }
            Event::Shared { slot, drive } => self.shared.push((slot, drive)),
            Event::Compile {
                request,
                cs,
                ip,
                bytes,
            } => self.compile(request, Key { cs, ip }, bytes),
            Event::Debug(Said::Stopped { reason, registers }) => {
                self.stops.push((reason, registers));
            }
            Event::Debug(Said::Seen(seen)) => self.seen.push(Seen::of(&seen)),
            Event::Debug(Said::Reset) => self.resets += 1,
            Event::Debug(Said::Detached) => self.detached += 1,
            Event::Interpreting => self.interpreting += 1,
        }
    }

    /// Block request `request`, for the block at `key`: compiled, its handle
    /// the address it is installed at, where there is a compiler.
    fn compile(&mut self, request: u32, key: Key, bytes: &[u8]) {
        self.asked.push(request);
        let Some((compiler, model)) = self.compiler.as_ref().filter(|_| self.compiling) else {
            return;
        };
        let block = Request {
            number: request,
            key,
            model: *model,
            bytes: bytes.to_vec(),
        };
        let code = compiler.compile(&block).map(|address| Installed {
            address,
            len: CODE_LEN,
        });
        self.compiled += usize::from(code.is_some());
        self.answer(encoded(&Command::Compiled { request, code }));
    }
}

/// Bytes the stand-in front-end says each block's code takes.
const CODE_LEN: u64 = 4096;

fn encoded(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

struct TestLink(Rc<RefCell<Front>>);

impl Link for TestLink {
    fn recv<R>(
        &mut self,
        wait: Option<Nanos>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        let mut front = self.0.borrow_mut();
        if let Some(command) = front.queued.pop_front() {
            return Ok(Waited::Frame(take(&command)));
        }
        match wait {
            // Nothing will ever come: the test is over.
            None => Ok(Waited::Closed),
            Some(wait) => {
                front.clock += wait;
                Ok(Waited::Nothing)
            }
        }
    }

    fn flush(&mut self) -> Result<(), ProtoError> {
        Ok(())
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.borrow_mut().take(payload);
        Ok(())
    }

    fn now(&self) -> Nanos {
        self.0.borrow().clock
    }
}

type TestWorker = Worker<TestLink, HeapShare, TestMemory>;

fn setup(config: crate::board::BoardConfig, fixed: Option<FixedDisk>) -> Setup {
    Setup {
        config,
        fixed: [fixed, None],
        audio_target: 2048,
        room: 96 << 20,
        seed: [7; 16],
        band: PressureBand::Normal,
        clock: None,
        resume: None,
    }
}

/// A worker for `setup`, its front-end holding `media`, and the front-end.
fn open(
    setup: &Setup,
    media: Vec<(u16, Box<dyn BlockDevice>)>,
) -> (Result<TestWorker, End>, Rc<RefCell<Front>>) {
    open_running(setup, media, |executor| Box::new(executor))
}

/// As [`open`], the worker's blocks run on what `run` makes of the
/// front-end's compiler's executor.
fn open_running(
    setup: &Setup,
    media: Vec<(u16, Box<dyn BlockDevice>)>,
    run: impl FnOnce(Interpreted) -> Box<dyn crate::cpu::jit::dispatch::Execute>,
) -> (Result<TestWorker, End>, Rc<RefCell<Front>>) {
    let front = Rc::new(RefCell::new(Front {
        media,
        release: true,
        ..Front::default()
    }));
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let (executor, compiler) = Interpreted::new();
    let worker = TestWorker::open(
        TestLink(Rc::clone(&front)),
        share,
        (memory(), run(executor)),
        &DISCARD,
        setup,
    );
    front.borrow_mut().compiler = Some((compiler, setup.config.cpu));
    (worker, front)
}

/// A worker resuming `kept` for `setup`, its front-end holding `media`.
fn resumed(
    mut setup: Setup,
    media: Vec<(u16, Box<dyn BlockDevice>)>,
    kept: Vec<u8>,
) -> (TestWorker, Rc<RefCell<Front>>) {
    setup.resume = Some(Resume {
        len: u32::try_from(kept.len()).unwrap(),
        away: 0,
    });
    let front = Rc::new(RefCell::new(Front {
        media,
        release: true,
        recall: kept,
        ..Front::default()
    }));
    let share = HeapShare::new(Layout::of(&setup.config.video).len()).unwrap();
    let worker = TestWorker::open(
        TestLink(Rc::clone(&front)),
        share,
        (memory(), Box::new(Interpreted::new().0)),
        &DISCARD,
        &setup,
    );
    (worker.unwrap(), front)
}

/// A turbo XT with its default disk, run.
fn default_machine() -> (TestWorker, Rc<RefCell<Front>>) {
    let disk = default_disk(Recording::Rll, 0x2026_1111).unwrap();
    let fixed = FixedDisk {
        medium: 1,
        sectors: disk.device.sectors(),
        read_only: false,
        kind: disk.kind,
        format: disk.low_level,
    };
    let (worker, front) = open(
        &setup(Template::TurboXt.config(), Some(fixed)),
        vec![(1, Box::new(disk.device))],
    );
    let mut worker = worker.unwrap();
    front.borrow_mut().queue(&Command::Run);
    assert_eq!(worker.turn(), None);
    (worker, front)
}

/// Turns until `seen` holds of the machine, for at most `within` of the
/// front-end's clock.
fn turn_until(
    worker: &mut TestWorker,
    front: &Rc<RefCell<Front>>,
    within: Nanos,
    seen: impl Fn(&TestWorker) -> bool,
) {
    let deadline = front.borrow().clock + within;
    while !seen(worker) {
        assert!(
            front.borrow().clock < deadline,
            "never got there:\n{}",
            screen(&worker.run.machine)
        );
        assert_eq!(worker.turn(), None);
    }
}

fn at_prompt(worker: &TestWorker) -> bool {
    crate::dos::waits_for_key(&worker.run.machine)
        && screen(&worker.run.machine).trim_end().ends_with("C:\\>")
}

fn typed(front: &Rc<RefCell<Front>>, text: &str) {
    for byte in text.bytes().chain(*b"\r") {
        for &(scan, down) in crate::bios::keystrokes(byte, false).as_slice() {
            front.borrow_mut().queue(&Command::Key { scan, down });
        }
    }
}

#[test]
fn the_default_disk_boots_to_the_prompt_through_the_front_ends_media() {
    let (mut worker, front) = default_machine();
    assert!(front.borrow().ready);
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    let front = front.borrow();
    assert!(front.fetches > 0, "the disk was read through the front-end");
    assert!(
        !front.frames.is_empty() && front.audio_frames > 0,
        "frames and sound came"
    );
    let emulated = worker.run.machine.now();
    assert!(
        emulated <= front.clock && front.clock - emulated <= TICK + SLICE,
        "paced to the host's clock"
    );
    assert_eq!(
        front.credit, worker.applied,
        "every command applied was reported"
    );
}

#[test]
fn a_typed_command_writes_through_the_front_end_and_the_cache_spares_fetches() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    let fetched = front.borrow().fetches;
    typed(&front, "md c:\\made");
    turn_for(&mut worker, &front, SECOND);
    turn_until(&mut worker, &front, 30 * SECOND, at_prompt);
    assert!(
        front.borrow().stores > 0,
        "the directory was written to the image:\n{}",
        screen(&worker.run.machine)
    );
    typed(&front, "dir c:\\ /a");
    turn_until(&mut worker, &front, 30 * SECOND, |w| {
        at_prompt(w) && screen(&w.run.machine).contains("MADE")
    });
    assert!(
        front.borrow().fetches - fetched < 20,
        "a re-read root and FAT came from the cache"
    );
}

#[test]
fn a_failed_store_is_the_guests_disk_error() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    front.borrow_mut().fail_stores = Some(MediaError::Failed);
    typed(&front, "md c:\\never");
    turn_until(&mut worker, &front, 30 * SECOND, |w| {
        screen(&w.run.machine).contains("Write fault error writing drive C")
    });
}

#[test]
fn commands_that_arrive_while_a_disk_waits_are_kept_and_applied_after_in_order() {
    let (mut worker, front) = default_machine();
    let key = Command::Key {
        scan: Scan::plain(0x1E),
        down: true,
    };
    let ahead = vec![
        encoded(&key),
        encoded(&Command::Fill { frames: 1 }),
        encoded(&Command::Hold),
    ];
    front.borrow_mut().ahead = ahead;
    let before = worker.applied;
    while front.borrow().fetches == 0 {
        assert_eq!(worker.turn(), None);
    }
    assert!(worker.run.running, "kept while the disk waited");
    assert_eq!(worker.turn(), None);
    assert!(
        !worker.run.running,
        "the hold was applied once the machine was back in the loop"
    );
    assert_eq!(
        worker.applied,
        before + 2,
        "the key and the hold were counted, the fill was not"
    );
    assert_eq!(worker.wait(), None, "a held machine waits for a command");
}

#[test]
fn more_commands_than_the_window_while_a_disk_waits_end_the_worker() {
    let (mut worker, front) = default_machine();
    let key = encoded(&Command::Key {
        scan: Scan::plain(0x1E),
        down: false,
    });
    front.borrow_mut().ahead = vec![key; INPUT_WINDOW as usize + 1];
    let end = loop {
        if let Some(end) = worker.turn() {
            break end;
        }
    };
    assert_eq!(end, End::Violation("more commands than the window allows"));
}

#[test]
fn an_answer_to_nothing_asked_ends_the_worker() {
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&Command::Stored {
        request: 99,
        answer: Ok(()),
    });
    assert_eq!(
        worker.turn(),
        Some(End::Violation("an answer to nothing asked"))
    );
}

/// Turns for `span` of the front-end's clock.
fn turn_for(worker: &mut TestWorker, front: &Rc<RefCell<Front>>, span: Nanos) {
    let until = front.borrow().clock + span;
    while front.borrow().clock < until {
        assert_eq!(worker.turn(), None);
    }
}

#[test]
fn a_frame_unchanged_is_not_sent_again_and_slots_wait_for_their_release() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    turn_for(&mut worker, &front, SECOND);
    front.borrow_mut().frames.clear();
    turn_for(&mut worker, &front, SECOND);
    let sent = front.borrow().frames.len();
    // The cursor turns on or off every eighth refresh of 70 a second, and
    // nothing else moves.
    assert!(
        (1..=9).contains(&sent),
        "{sent} frames sent for a second of a blinking cursor"
    );
    front.borrow_mut().release = false;
    front.borrow_mut().frames.clear();
    turn_for(&mut worker, &front, SECOND);
    let held: Vec<u8> = front.borrow().frames.iter().map(|frame| frame.0).collect();
    assert_eq!(
        held.len(),
        2,
        "both slots lent, then none more until one comes back: {held:?}"
    );
    assert_ne!(held[0], held[1]);
    front
        .borrow_mut()
        .queue(&Command::Released { slot: held[0] });
    front.borrow_mut().release = true;
    turn_for(&mut worker, &front, SECOND);
    assert!(
        front.borrow().frames.len() > 2,
        "the released slot was used again"
    );
    front
        .borrow_mut()
        .queue(&Command::Released { slot: held[0] });
    let end = loop {
        if let Some(end) = worker.turn() {
            break end;
        }
    };
    assert_eq!(end, End::Violation("a slot released that was not lent"));
}

/// A boot sector that unlocks the VGA's timing registers and sets 256
/// characters to a line: a frame wider than any monitor of the period shows.
fn too_wide(asm: &mut crate::asm::Asm) {
    use crate::asm::{Reg16, Reg8};
    for (index, value) in [(0x11, 0x00), (0x00, 0xFF), (0x01, 0xFF)] {
        asm.mov_imm16(Reg16::Dx, 0x3D4);
        asm.mov_imm8(Reg8::Al, index);
        asm.out_dx_al();
        asm.mov_imm16(Reg16::Dx, 0x3D5);
        asm.mov_imm8(Reg8::Al, value);
        asm.out_dx_al();
    }
    crate::dos::harness::stop(asm);
}

/// The front-end's machine started from the diskette `image` in drive A:.
fn booted_from(worker: &mut TestWorker, front: &Rc<RefCell<Front>>, image: Vec<u8>) {
    let sectors = u64::try_from(image.len() / SECTOR).unwrap();
    front
        .borrow_mut()
        .media
        .push((5, Box::new(MemoryDisk::new(image, true).unwrap())));
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 5,
        form: Form::Sectors {
            count: sectors,
            read_only: true,
        },
    });
    front.borrow_mut().queue(&Command::Reset);
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().inserted.last(), Some(&(0, 5, Ok(()))));
}

#[test]
fn a_frame_larger_than_the_monitor_shows_is_out_of_range_once() {
    let (mut worker, front) = default_machine();
    booted_from(
        &mut worker,
        &front,
        crate::dos::harness::boot_image(too_wide),
    );
    turn_for(&mut worker, &front, 5 * SECOND);
    assert_eq!(
        front.borrow().out_of_range,
        [0],
        "said once, while it lasts"
    );
}

#[test]
fn a_raw_diskette_and_an_archive_go_in_and_come_out() {
    let (mut worker, front) = default_machine();
    let floppy = MemoryDisk::new(vec![0xF6; 1440 * 1024], false).unwrap();
    front.borrow_mut().media.push((5, Box::new(floppy)));
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 5,
        form: Form::Sectors {
            count: 2880,
            read_only: false,
        },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().inserted.last(), Some(&(0, 5, Ok(()))));
    let odd = MemoryDisk::new(vec![0; 1000 * SECTOR], false).unwrap();
    front.borrow_mut().media.push((6, Box::new(odd)));
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 6,
        form: Form::Sectors {
            count: 1000,
            read_only: false,
        },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(
        front.borrow().inserted.last(),
        Some(&(0, 6, Err(InsertRefusal::Format)))
    );
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 6,
        form: Form::Sectors {
            count: 2880,
            read_only: false,
        },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(
        front.borrow().inserted.last(),
        Some(&(0, 6, Err(InsertRefusal::Medium))),
        "a name already used"
    );
    front.borrow_mut().queue(&Command::Insert {
        drive: 3,
        medium: 7,
        form: Form::Sectors {
            count: 2880,
            read_only: false,
        },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(
        front.borrow().inserted.last(),
        Some(&(3, 7, Err(InsertRefusal::NoDrive)))
    );
    let sector = [0xE5u8; 512];
    let mut archive =
        crate::storage::imd::tests::archive(&[(5, 0, 0, 2, &[(1, 1, &sector), (2, 2, &[0xF6])])]);
    let bytes = u32::try_from(archive.len()).unwrap();
    archive.resize(archive.len().next_multiple_of(SECTOR), 0);
    let archive = MemoryDisk::new(archive, true).unwrap();
    front.borrow_mut().media.push((8, Box::new(archive)));
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 8,
        form: Form::Archive { bytes },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().inserted.last(), Some(&(0, 8, Ok(()))));
    assert_eq!(
        front.borrow().ejected.last(),
        Some(&(0, 5)),
        "the raw diskette came out for it"
    );
    front.borrow_mut().queue(&Command::Eject { drive: 0 });
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().ejected.last(), Some(&(0, 8)));
    let junk = MemoryDisk::new(vec![0x55; SECTOR], true).unwrap();
    front.borrow_mut().media.push((9, Box::new(junk)));
    front.borrow_mut().queue(&Command::Insert {
        drive: 0,
        medium: 9,
        form: Form::Archive { bytes: 300 },
    });
    assert_eq!(worker.turn(), None);
    assert_eq!(
        front.borrow().inserted.last(),
        Some(&(0, 9, Err(InsertRefusal::Archive)))
    );
}

#[test]
fn a_held_machine_stands_still_and_a_long_wait_on_the_disk_is_not_caught_up() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    front.borrow_mut().queue(&Command::Hold);
    assert_eq!(worker.turn(), None);
    let held = worker.run.machine.now();
    front.borrow_mut().clock += 10 * SECOND;
    assert_eq!(
        worker.turn(),
        Some(End::Closed),
        "a held machine waits for ever, and the test sends nothing"
    );
    assert_eq!(worker.run.machine.now(), held);
}

#[test]
fn a_reset_keeps_the_media_and_boots_again() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    front.borrow_mut().queue(&Command::Reset);
    assert_eq!(worker.turn(), None);
    assert!(!at_prompt(&worker));
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
}

#[test]
fn a_low_level_format_is_reported_for_the_front_end_to_keep() {
    let (mut worker, front) = default_machine();
    let image = crate::dos::harness::boot_image(|asm| {
        use crate::asm::Reg8;
        for (reg, value) in [
            (Reg8::Ah, 0x05),
            (Reg8::Al, 2),
            (Reg8::Ch, 0),
            (Reg8::Cl, 1),
            (Reg8::Dh, 0),
            (Reg8::Dl, 0x80),
        ] {
            asm.mov_imm8(reg, value);
        }
        asm.int(0x13);
        crate::dos::harness::stop(asm);
    });
    booted_from(&mut worker, &front, image);
    turn_until(&mut worker, &front, 30 * SECOND, |_| {
        told_of_a_format(&front)
    });
    let formatted = front.borrow().formatted.clone();
    assert_eq!(
        formatted,
        [(
            0,
            LowLevel {
                interleave: 2,
                bad_tracks: Vec::new()
            }
        )]
    );
}

fn told_of_a_format(front: &Rc<RefCell<Front>>) -> bool {
    !front.borrow().formatted.is_empty()
}

#[test]
fn a_first_command_other_than_a_start_or_a_share_too_small_is_refused() {
    let front = Rc::new(RefCell::new(Front::default()));
    front.borrow_mut().queue(&Command::Run);
    let end = serve(
        TestLink(Rc::clone(&front)),
        HeapShare::new(1).unwrap(),
        (memory(), Box::new(Interpreted::new().0)),
        &DISCARD,
    );
    assert_eq!(end, End::Violation("a first command other than a start"));
    let front = Rc::new(RefCell::new(Front::default()));
    front
        .borrow_mut()
        .queue(&Command::Start(setup(Template::IbmPc.config(), None)));
    let end = serve(
        TestLink(Rc::clone(&front)),
        HeapShare::new(1).unwrap(),
        (memory(), Box::new(Interpreted::new().0)),
        &DISCARD,
    );
    assert_eq!(end, End::Refused(Refusal::Share));
    assert_eq!(front.borrow().refused, Some(Refusal::Share));
}

#[test]
fn a_share_of_the_layouts_size_is_written_with_the_frames_named() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    let (slot, width, height) = *front.borrow().frames.last().unwrap();
    let place = worker.run.layout.slot(slot).unwrap();
    let mut pixels = vec![0u8; usize::from(width) * usize::from(height) * 4];
    assert!(worker.share.copy_out(place.offset, &mut pixels));
    assert!(
        pixels.chunks(4).any(|p| p[..3] != [0, 0, 0]),
        "the prompt's text is lit"
    );
    assert!(pixels.chunks(4).all(|p| p[3] == 0xFF), "opaque");
}

#[test]
fn keys_a_guest_never_reads_are_credited_on_arrival_so_a_reset_still_gets_through() {
    // Keys were credited only once the keyboard took them, so a guest that
    // read none held the window shut and nothing more, a reset included,
    // could reach the worker.
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&Command::Hold);
    assert_eq!(worker.turn(), None);
    let before = worker.applied;
    for n in 0..100u8 {
        front.borrow_mut().queue(&Command::Key {
            scan: Scan::plain(0x10 + n % 20),
            down: n % 2 == 0,
        });
    }
    assert_eq!(worker.turn(), None);
    assert_eq!(
        front.borrow().credit,
        before + 100,
        "every key credited as it came"
    );
    assert!(
        !worker.run.keys.is_empty(),
        "most still wait for the keyboard"
    );
    front.borrow_mut().queue(&Command::Reset);
    assert_eq!(worker.turn(), None);
    assert!(
        worker.run.keys.is_empty(),
        "a power cycle leaves no key waiting"
    );
    assert_eq!(front.borrow().credit, before + 101);
}

#[test]
fn a_press_beyond_the_key_queue_is_dropped_with_its_release_and_a_release_of_all_never_is() {
    use crate::link::{QUEUE_MOST, RELEASES_KEPT};
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&Command::Hold);
    assert_eq!(worker.turn(), None);
    for _ in 0..QUEUE_MOST {
        worker.queue_key(KeyEvent::Key(Scan::plain(0x1E), true));
    }
    assert_eq!(worker.run.keys.len(), QUEUE_MOST - RELEASES_KEPT);
    worker.queue_key(KeyEvent::Key(Scan::plain(0x1E), false));
    assert_eq!(
        worker.run.keys.len(),
        QUEUE_MOST - RELEASES_KEPT,
        "the dropped press's release dropped too"
    );
    worker.queue_key(KeyEvent::Key(Scan::plain(0x1F), false));
    assert_eq!(
        worker.run.keys.len(),
        QUEUE_MOST - RELEASES_KEPT + 1,
        "a release of a press that was kept"
    );
    while worker.run.keys.len() < QUEUE_MOST {
        worker.queue_key(KeyEvent::Key(Scan::plain(0x20), false));
    }
    worker.queue_key(KeyEvent::ReleaseAll);
    assert_eq!(
        worker.run.keys.iter().copied().collect::<Vec<_>>(),
        [KeyEvent::ReleaseAll],
        "it gives way to nothing"
    );
}

#[test]
fn the_default_disks_mouse_driver_follows_the_host_pointer_over_the_picture() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    assert_eq!(
        front.borrow().pointing,
        [true],
        "AUTOEXEC.BAT loads MOUSE.COM, which then holds the mouse"
    );
    let picture = worker.run.pictures[0].expect("the colour card drew");
    let area = picture.displayed.expect("a picture inside the border");
    let middle = |from: u32, to: u32| u16::try_from(u32::midpoint(from, to)).unwrap();
    front.borrow_mut().queue(&Command::Pointer {
        adapter: 0,
        x: middle(area.left, area.right),
        y: u16::try_from(area.bottom + 5).unwrap(),
        buttons: 0,
    });
    assert_eq!(worker.turn(), None);
    let at = worker.run.machine.board().host_pointer().unwrap();
    assert!(
        at.x.abs_diff(0x8000) < 0x100 && at.y == u16::MAX,
        "the middle across, and the border below the picture its last line: {at:?}"
    );
    crate::cpu::Bus::write(worker.run.machine.board_mut(), 0xCC, 0, 0);
    turn_for(&mut worker, &front, TICK);
    assert_eq!(front.borrow().pointing, [true, false], "INT 33h taken");
    assert_eq!(worker.run.machine.board().host_pointer(), None);
}

#[test]
fn a_time_the_guest_sets_is_reported_once_the_card_runs_with_it() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    assert!(
        front.borrow().clock_sets.is_empty(),
        "the front-end's own setting"
    );
    typed(&front, "time 12:34:56");
    turn_until(&mut worker, &front, 30 * SECOND, |_| {
        !front.borrow().clock_sets.is_empty()
    });
    let set = front.borrow().clock_sets[0];
    assert_eq!((set.hour, set.minute), (12, 34));
    assert!(set.second >= 56, "read as the card ran on: {set:?}");
    turn_for(&mut worker, &front, SECOND);
    assert_eq!(
        front.borrow().clock_sets.len(),
        1,
        "reported once a setting"
    );
}

#[test]
fn the_fixed_disks_lamp_lights_as_dos_boots_and_goes_out_after() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    turn_for(&mut worker, &front, SECOND);
    let lights = front.borrow().lights.clone();
    assert!(
        lights.iter().any(|shown| shown.fixed == 0b01),
        "the boot read drive C: {lights:?}"
    );
    assert_eq!(
        lights.last().map(|shown| shown.fixed),
        Some(0),
        "and the lamp is out at the prompt"
    );
    assert!(
        lights.windows(2).all(|pair| pair[0] != pair[1]),
        "told only of changes"
    );
}

#[test]
fn a_machine_suspended_is_resumed_exactly_where_it_stopped() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    typed(&front, "DIR");
    for _ in 0..20 {
        assert_eq!(worker.turn(), None);
    }
    front.borrow_mut().queue(&Command::Suspend);
    assert_eq!(worker.turn(), None);
    let kept = core::mem::take(&mut front.borrow_mut().kept);
    assert_eq!(
        front.borrow().suspended,
        Some(Ok(u32::try_from(kept.len()).unwrap()))
    );
    let print = fingerprint(&worker.run.config, &worker.run.fixed).unwrap();
    let stopped = worker.run.machine.snapshot(&print).unwrap();
    let media = core::mem::take(&mut front.borrow_mut().media);
    let config = worker.run.config;
    let fixed = setup(config, None).fixed;
    let mut start = setup(config, None);
    start.fixed = [
        Some(FixedDisk {
            medium: 1,
            sectors: worker.run.fixed[0].unwrap().sectors,
            read_only: false,
            kind: worker.run.fixed[0].unwrap().kind,
            format: worker.run.machine.board().fixed_format(0).unwrap().clone(),
        }),
        fixed[1].clone(),
    ];
    drop(worker);
    let (mut again, front) = resumed(start, media, kept);
    assert_eq!(front.borrow().resumed, Some(true));
    assert!(front.borrow().ready);
    assert!(
        again.run.machine.snapshot(&print).unwrap() == stopped,
        "the machine is as it stopped"
    );
    front.borrow_mut().queue(&Command::Run);
    turn_until(&mut again, &front, 60 * SECOND, |worker| {
        at_prompt(worker) && screen(&worker.run.machine).contains("bytes free")
    });
}

#[test]
fn a_snapshot_that_does_not_take_starts_the_machine_afresh() {
    let disk = default_disk(Recording::Rll, 0x2026_1111).unwrap();
    let fixed = FixedDisk {
        medium: 1,
        sectors: disk.device.sectors(),
        read_only: false,
        kind: disk.kind,
        format: disk.low_level,
    };
    let start = setup(Template::TurboXt.config(), Some(fixed));
    let mut garbage = vec![0x5Au8; 1000];
    garbage[..4].copy_from_slice(b"RLZ1");
    let (mut worker, front) = resumed(start, vec![(1, Box::new(disk.device))], garbage);
    assert_eq!(front.borrow().resumed, Some(false));
    assert!(front.borrow().ready);
    front.borrow_mut().queue(&Command::Run);
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
}

#[test]
fn the_screens_text_is_read_back_and_text_given_to_type_reaches_the_shell() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    front.borrow_mut().queue(&Command::ReadText);
    assert_eq!(worker.turn(), None);
    let shown = front.borrow_mut().texts.pop().unwrap().unwrap();
    assert_eq!((shown.columns, shown.rows), (80, 25));
    let prompt = shown
        .codes
        .chunks(80)
        .rev()
        .find(|row| row.iter().any(|&code| code != b' '))
        .unwrap();
    assert!(prompt.starts_with(b"C:\\>"), "{prompt:02X?}");
    front
        .borrow_mut()
        .queue(&Command::Type(b"echo pasted \x82\r"));
    turn_until(&mut worker, &front, 30 * SECOND, |w| {
        at_prompt(w) && screen(&w.run.machine).contains("pasted")
    });
    turn_for(&mut worker, &front, SECOND / 10);
    assert_eq!(front.borrow().typed, 14);
    front.borrow_mut().queue(&Command::ReadText);
    assert_eq!(worker.turn(), None);
    let shown = front.borrow_mut().texts.pop().unwrap().unwrap();
    assert!(
        shown
            .codes
            .chunks(80)
            .any(|row| row.starts_with(b"pasted \x82 ")),
        "the code typed through Alt and the keypad"
    );
}

#[test]
fn what_is_stopped_or_reset_is_accounted_for_untyped() {
    let (mut worker, front) = default_machine();
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    let lots = vec![b'z'; TYPE_WINDOW];
    front.borrow_mut().queue(&Command::Type(&lots));
    front.borrow_mut().queue(&Command::StopTyping);
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().typed, 1024, "none begun");
    front.borrow_mut().queue(&Command::Type(b"zzz"));
    front.borrow_mut().queue(&Command::Reset);
    assert_eq!(worker.turn(), None);
    assert_eq!(front.borrow().typed, 1027);
    assert!(!screen(&worker.run.machine).contains('z'));
}

#[test]
fn more_to_type_than_the_window_ends_the_worker() {
    let (mut worker, front) = default_machine();
    let lots = vec![b'z'; TYPE_WINDOW];
    front.borrow_mut().queue(&Command::Hold);
    front.borrow_mut().queue(&Command::Type(&lots));
    front.borrow_mut().queue(&Command::Type(b"z"));
    let end = loop {
        if let Some(end) = worker.turn() {
            break end;
        }
    };
    assert_eq!(end, End::Violation("more to type than the window holds"));
}

/// Turns until the commands sent ahead of the next answer have gone with it.
fn turn_past_ahead(worker: &mut TestWorker, front: &Rc<RefCell<Front>>) -> Option<End> {
    let deadline = front.borrow().clock + 30 * SECOND;
    while !front.borrow().ahead.is_empty() {
        assert!(front.borrow().clock < deadline, "no disk was asked for");
        if let Some(end) = worker.turn() {
            return Some(end);
        }
    }
    None
}

#[test]
fn text_to_type_while_a_disk_waits_is_kept_beside_a_full_window_of_commands() {
    let (mut worker, front) = default_machine();
    let key = encoded(&Command::Key {
        scan: Scan::plain(0x1E),
        down: false,
    });
    let mut ahead = vec![key; INPUT_WINDOW as usize];
    ahead.push(encoded(&Command::Type(&[b'a'; TYPE_WINDOW])));
    front.borrow_mut().ahead = ahead;
    assert_eq!(turn_past_ahead(&mut worker, &front), None);
    turn_for(&mut worker, &front, SECOND / 10);
}

#[test]
fn more_to_type_than_the_window_while_a_disk_waits_ends_the_worker() {
    let (mut worker, front) = default_machine();
    front.borrow_mut().ahead = vec![
        encoded(&Command::Type(&[b'a'; TYPE_WINDOW])),
        encoded(&Command::Type(b"a")),
    ];
    assert_eq!(
        turn_past_ahead(&mut worker, &front),
        Some(End::Violation("more to type than the window holds"))
    );
}

#[test]
fn a_shared_folder_is_a_drive_the_shell_reads_and_copies_to() {
    let host = MemHost::new().file("readme.txt", b"Hello from the host.\r\n");
    let (mut worker, front) = default_machine();
    let label = DosName::new(*b"HOST       ", false).unwrap();
    front
        .borrow_mut()
        .folders
        .push((0, Server::new(host.root(), label, true, 0x77)));
    front.borrow_mut().queue(&Command::Share {
        slot: 0,
        label,
        writable: true,
    });
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    assert_eq!(
        front.borrow().shared,
        [(0, None), (0, Some(3))],
        "no drive until the boot gives the shares theirs, then D:, the first after the fixed disk's"
    );
    typed(&front, "type d:readme.txt");
    turn_for(&mut worker, &front, SECOND / 10);
    turn_until(&mut worker, &front, 30 * SECOND, |w| {
        at_prompt(w) && screen(&w.run.machine).contains("Hello from the host.")
    });
    typed(&front, "copy d:readme.txt d:copy.txt");
    turn_for(&mut worker, &front, SECOND / 10);
    turn_until(&mut worker, &front, 30 * SECOND, |w| {
        at_prompt(w) && screen(&w.run.machine).contains("1 file(s) copied")
    });
    assert_eq!(
        host.bytes("COPY.TXT").as_deref(),
        Some(&b"Hello from the host.\r\n"[..])
    );
    front.borrow_mut().queue(&Command::Unshare { slot: 0 });
    assert_eq!(worker.turn(), None);
    assert!(worker.run.machine.board_mut().share_mut(0).is_none());
}

#[test]
fn a_share_of_a_slot_already_sharing_ends_the_worker_as_does_an_unshare_of_none() {
    let label = DosName::new(*b"HOST       ", false).unwrap();
    let share = Command::Share {
        slot: 0,
        label,
        writable: false,
    };
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&share);
    front.borrow_mut().queue(&share);
    assert_eq!(
        worker.turn(),
        Some(End::Violation(
            "a folder shared in a slot already sharing one"
        ))
    );
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&Command::Unshare { slot: 1 });
    assert_eq!(
        worker.turn(),
        Some(End::Violation("a share stopped that was not shared"))
    );
}

/// Bytes of code the worker's machine holds.
fn held(worker: &mut TestWorker) -> u64 {
    worker.run.machine.blocks().map_or(0, |(jit, _)| jit.held())
}

/// Blocks the front-end's compiler has run.
fn runs(front: &Rc<RefCell<Front>>) -> u64 {
    front
        .borrow()
        .compiler
        .as_ref()
        .map_or(0, |(compiler, _)| compiler.runs())
}

/// A program that runs a tight loop eighty thousand times over a word, then
/// prints the word in hex and exits.
const LOOPING: [u8; 57] = [
    0xBB, 0x34, 0x12, // MOV BX, 1234h
    0xBA, 0x28, 0x00, // MOV DX, 40
    0xB9, 0xD0, 0x07, // outer: MOV CX, 2000
    0x89, 0xD8, // inner: MOV AX, BX
    0xD1, 0xE0, // SHL AX, 1
    0x01, 0xC3, // ADD BX, AX
    0x31, 0xCB, // XOR BX, CX
    0x43, // INC BX
    0xE2, 0xF5, // LOOP inner
    0x4A, // DEC DX
    0x75, 0xEF, // JNZ outer
    0xBE, 0x04, 0x00, // MOV SI, 4
    0xB1, 0x04, // digit: MOV CL, 4
    0xD3, 0xC3, // ROL BX, CL
    0x89, 0xDA, // MOV DX, BX
    0x80, 0xE2, 0x0F, // AND DL, 0Fh
    0x80, 0xC2, 0x30, // ADD DL, '0'
    0x80, 0xFA, 0x39, // CMP DL, '9'
    0x76, 0x03, // JBE +3
    0x80, 0xC2, 0x07, // ADD DL, 7
    0xB4, 0x02, // MOV AH, 2
    0xCD, 0x21, // INT 21h
    0x4E, // DEC SI
    0x75, 0xE5, // JNZ digit
    0xB4, 0x4C, // MOV AH, 4Ch
    0xCD, 0x21, // INT 21h
];

/// A turbo XT at its prompt with [`LOOPING`] shared as `D:WORK.COM`, its
/// blocks compiled where `compiling`, and `room` its worker's.
fn looping_machine(compiling: bool, room: u64) -> (TestWorker, Rc<RefCell<Front>>) {
    looping_running(compiling, room, |executor| Box::new(executor))
}

/// As [`looping_machine`], its blocks run on what `run` makes of the
/// compiler's executor.
fn looping_running(
    compiling: bool,
    room: u64,
    run: impl FnOnce(Interpreted) -> Box<dyn crate::cpu::jit::dispatch::Execute>,
) -> (TestWorker, Rc<RefCell<Front>>) {
    let disk = default_disk(Recording::Rll, 0x2026_1111).unwrap();
    let fixed = FixedDisk {
        medium: 1,
        sectors: disk.device.sectors(),
        read_only: false,
        kind: disk.kind,
        format: disk.low_level,
    };
    let mut setup = setup(Template::TurboXt.config(), Some(fixed));
    setup.room = room;
    let (worker, front) = open_running(&setup, vec![(1, Box::new(disk.device))], run);
    let mut worker = worker.unwrap();
    let host = MemHost::new().file("work.com", &LOOPING);
    let label = DosName::new(*b"HOST       ", false).unwrap();
    {
        let mut front = front.borrow_mut();
        front.compiling = compiling;
        front
            .folders
            .push((0, Server::new(host.root(), label, false, 0x77)));
        front.queue(&Command::Share {
            slot: 0,
            label,
            writable: false,
        });
        front.queue(&Command::Run);
    }
    turn_until(&mut worker, &front, 60 * SECOND, at_prompt);
    (worker, front)
}

/// What [`LOOPING`] printed once run at the prompt, `each` told of every
/// turn.
fn looped(
    worker: &mut TestWorker,
    front: &Rc<RefCell<Front>>,
    mut each: impl FnMut(&mut TestWorker, &Rc<RefCell<Front>>),
) -> String {
    typed(front, "d:work");
    let deadline = front.borrow().clock + 60 * SECOND;
    turn_for(worker, front, SECOND / 10);
    while !at_prompt(worker) {
        assert!(
            front.borrow().clock < deadline,
            "{}",
            screen(&worker.run.machine)
        );
        assert_eq!(worker.turn(), None);
        each(worker, front);
    }
    let shown = screen(&worker.run.machine);
    let after = shown.split("d:work").nth(1).unwrap_or_default();
    after.split_whitespace().next().unwrap_or_default().into()
}

#[test]
fn hot_blocks_are_asked_for_in_turn_and_no_more_than_the_window_holds() {
    let (mut worker, front) = looping_machine(false, 96 << 20);
    let printed = looped(&mut worker, &front, |_, _| {});
    assert_eq!(printed.len(), 4, "{printed}");
    let asked = front.borrow().asked.clone();
    assert_eq!(
        asked,
        (0..u32::try_from(COMPILE_WINDOW).unwrap()).collect::<Vec<_>>(),
        "none answered, so the window's worth"
    );
}

#[test]
fn a_program_run_through_its_compiled_blocks_computes_what_the_interpreter_does() {
    let (mut plain, front) = looping_machine(false, 96 << 20);
    let interpreted = looped(&mut plain, &front, |_, _| {});
    let (mut worker, front) = looping_machine(true, 96 << 20);
    let before = runs(&front);
    let compiled = looped(&mut worker, &front, |_, _| {});
    assert_eq!(compiled, interpreted);
    assert!(
        front.borrow().compiled >= 2,
        "{} compiled",
        front.borrow().compiled
    );
    assert!(
        runs(&front) - before > 50_000,
        "{} blocks run",
        runs(&front) - before
    );
}

#[test]
fn an_answer_to_a_block_never_asked_for_ends_the_worker() {
    let (mut worker, front) = default_machine();
    front.borrow_mut().queue(&Command::Compiled {
        request: 77,
        code: None,
    });
    assert_eq!(
        worker.turn(),
        Some(End::Violation("an answer to a block never asked for"))
    );
}

#[test]
fn told_to_interpret_it_asks_for_no_more_blocks_and_runs_those_it_has() {
    let (mut worker, front) = looping_machine(true, 96 << 20);
    let mut told = None;
    let printed = looped(&mut worker, &front, |_, front| {
        if told.is_none() && front.borrow().compiled >= 1 {
            front.borrow_mut().queue(&Command::Interpret);
            told = Some((front.borrow().asked.len(), runs(front)));
        }
    });
    let (asked, ran) = told.expect("a block was compiled");
    assert_eq!(printed.len(), 4, "{printed}");
    assert_eq!(
        front.borrow().asked.len(),
        asked,
        "asked for no more once told"
    );
    assert!(runs(&front) > ran + 1_000, "the block installed ran on");
}

/// Translated code that answers no exit: a code generator's defect.
struct Misbehaving(Interpreted);

impl crate::cpu::jit::dispatch::Execute for Misbehaving {
    fn run(
        &mut self,
        _: u64,
        _: &mut crate::cpu::jit::frame::Frame,
        _: &crate::cpu::jit::page::PageMap,
        _: &mut crate::cpu::jit::dispatch::Env<'_>,
    ) -> Option<crate::cpu::jit::Exit> {
        None
    }

    fn adopt(&mut self, address: u64, len: u64) -> Option<u64> {
        self.0.adopt(address, len)
    }

    fn release(&mut self, code: u64) {
        self.0.release(code);
    }

    fn held(&self) -> u64 {
        self.0.held()
    }
}

#[test]
fn a_block_that_misbehaves_is_said_once_and_the_machine_computes_on_interpreted() {
    let (mut plain, front) = looping_machine(false, 96 << 20);
    let interpreted = looped(&mut plain, &front, |_, _| {});
    let (mut worker, front) =
        looping_running(true, 96 << 20, |executor| Box::new(Misbehaving(executor)));
    let mut said = None;
    let printed = looped(&mut worker, &front, |_, front| {
        if said.is_none() && front.borrow().interpreting > 0 {
            said = Some(front.borrow().asked.len());
        }
    });
    assert_eq!(printed, interpreted);
    let asked = said.expect("the misbehaviour was said");
    assert_eq!(front.borrow().interpreting, 1, "once");
    assert_eq!(
        front.borrow().asked.len(),
        asked,
        "no block asked for after"
    );
}

#[test]
fn code_past_its_room_is_given_up_whole_and_asked_for_again() {
    // Room for the code of two blocks, as the stand-in says each takes.
    let room = 16 * 2 * CODE_LEN;
    let (mut worker, front) = looping_machine(true, room);
    let mut most = 0;
    let printed = looped(&mut worker, &front, |worker, _| {
        most = most.max(held(worker));
    });
    assert_eq!(printed.len(), 4, "{printed}");
    assert!(most <= 2 * CODE_LEN, "held {most}");
    assert!(
        front.borrow().compiled > 2,
        "compiled again once given up: {}",
        front.borrow().compiled
    );
}

#[test]
fn under_moderate_pressure_no_code_is_held_or_asked_for() {
    let (mut worker, front) = looping_machine(true, 96 << 20);
    let mut pressed = None;
    let printed = looped(&mut worker, &front, |worker, front| {
        if pressed.is_none() && held(worker) > 0 {
            front
                .borrow_mut()
                .queue(&Command::Pressure(PressureBand::Moderate));
            pressed = Some(front.borrow().asked.len());
        } else if pressed.is_some() {
            assert_eq!(held(worker), 0, "nothing held under pressure");
        }
    });
    let asked = pressed.expect("code was held");
    assert_eq!(printed.len(), 4, "{printed}");
    assert_eq!(front.borrow().asked.len(), asked, "asked for no more");
}

#[cfg(test)]
#[path = "debugger_tests.rs"]
mod debugger;
