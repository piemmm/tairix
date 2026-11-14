use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_abi::{Errno, WaitSourceKind};
use tairix_log::DiscardSink;
use tairix_sandbox::loopback::{HeapShare, LoopbackSession, MemberLedger};
use tairix_sandbox::members::{MemberSet, Members};
use tairix_sandbox::session::{SessionDescriptors, SessionTransport, Share};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_sandbox::{FrameOut, SessionService, SessionStep};

use super::{Link, FAILED, UNREACHED};
use crate::worker::protocol::{
    session_bounds, Configuration, Reply, Request, Slot, SHARE_BYTES, SLOTS,
};
use crate::worker::MachineService;

const READ: u64 = 5;
const WRITE: u64 = 6;
/// Every worker's pipes, as the kernel gives a replacement the numbers its
/// predecessor's freed.
const PIPES: SessionDescriptors = SessionDescriptors {
    read_fd: 7,
    write_fd: 8,
};

/// The machine worker, ended after `answers` requests as a worker that exits
/// ends its stream.
struct Ending {
    service: MachineService<HeapShare>,
    answers: usize,
}

impl SessionService for Ending {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let step = self.service.handle(request, out);
        self.answers = self.answers.saturating_sub(1);
        if self.answers == 0 {
            SessionStep::Finished
        } else {
            step
        }
    }
}

/// A loopback worker on [`PIPES`], noting whether its descriptors were
/// still watched as it closed them.
struct Numbered {
    worker: LoopbackSession<Ending>,
    ledger: MemberLedger,
    watched_at_close: Rc<Cell<bool>>,
}

impl SessionTransport for Numbered {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.worker.read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.worker.write(buf)
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        Some(PIPES)
    }

    fn share(&self) -> Option<&Arc<dyn Share>> {
        self.worker.share()
    }

    fn dispose(self) -> Option<i32> {
        if self.ledger.names(PIPES.read_fd) || self.ledger.names(PIPES.write_fd) {
            self.watched_at_close.set(true);
        }
        self.worker.dispose()
    }
}

struct Launcher {
    ledger: MemberLedger,
    watched_at_close: Rc<Cell<bool>>,
    answers: usize,
}

impl SessionLauncher for Launcher {
    type Transport = Numbered;

    fn launch(&mut self) -> Result<Numbered, Errno> {
        let share = HeapShare::new(SHARE_BYTES).ok_or(Errno::OutOfMemory)?;
        let service = MachineService::new(share.clone()).ok_or(Errno::OutOfMemory)?;
        let worker = LoopbackSession::new(Ending {
            service,
            answers: self.answers,
        })
        .sharing(Arc::new(share));
        Ok(Numbered {
            worker,
            ledger: self.ledger.clone(),
            watched_at_close: Rc::clone(&self.watched_at_close),
        })
    }
}

type TestLink = Link<Launcher, DiscardSink, MemberLedger>;

/// A link whose workers each answer `answers` requests, started; its
/// wait-set, and whether a worker's pipes were watched as they closed.
fn started(answers: usize) -> (TestLink, MemberLedger, Rc<Cell<bool>>) {
    let ledger = MemberLedger::default();
    let watched_at_close = Rc::new(Cell::new(false));
    let launcher = Launcher {
        ledger: ledger.clone(),
        watched_at_close: Rc::clone(&watched_at_close),
        answers,
    };
    let session = SupervisedSession::new(launcher, session_bounds().expect("bounds"), DiscardSink);
    let mut link = Link::new(session, Members::over(ledger.clone(), READ, WRITE));
    assert!(link.start(0).is_some(), "a worker starts");
    (link, ledger, watched_at_close)
}

/// The machine built: asked, sent and answered.
fn configure(link: &mut TestLink, now: u64) {
    let os = crate::rom::mos::model_b().expect("the MOS").bytes;
    let mut slots = [Slot::Empty; SLOTS];
    slots[0] = Slot::Ram;
    let request = Request::Configure(Box::new(Configuration::ModelB {
        os: &os,
        slots: Box::new(slots),
        links: 0,
    }));
    link.ask(&request).expect("nothing is asked");
    assert_eq!(link.sync(now), Ok(None));
    let mut frames = Vec::new();
    assert_eq!(link.serve(now, (true, false), &mut frames), None);
    assert_eq!(frames.len(), 1, "one answer");
    assert_eq!(Reply::decode(&frames[0]), Ok(Reply::Done));
}

const RUN: Request<'static> = Request::Run {
    cycles: 1000,
    sound: false,
    inputs: Vec::new(),
};

/// The regression: a worker found gone as a request was sent left the
/// request asked, so its replacement was never asked anything, and the
/// machine stood still until closing it timed out.
#[test]
fn a_worker_lost_as_a_request_is_sent_leaves_its_replacement_to_be_asked() {
    let (mut link, ledger, _) = started(1);
    configure(&mut link, 0);
    link.ask(&RUN).expect("nothing is asked");
    assert_eq!(link.sync(1), Ok(Some(UNREACHED)));
    assert!(link.is_idle(), "nothing is left asked");
    assert!(ledger.members().is_empty(), "nothing is left watched");
    let due = link.restart_deadline().expect("a replacement is due");
    assert!(link.start(due).is_some(), "the replacement starts");
    configure(&mut link, due);
}

#[test]
fn a_worker_that_ends_leaves_nothing_asked_or_watched() {
    let (mut link, ledger, _) = started(1);
    configure(&mut link, 0);
    assert_eq!(
        ledger.members(),
        [(WaitSourceKind::Stream, PIPES.read_fd, READ)]
    );
    let mut frames = Vec::new();
    assert_eq!(link.serve(1, (true, false), &mut frames), Some(FAILED));
    assert!(frames.is_empty());
    assert!(link.is_idle() && !link.is_live());
    assert!(ledger.members().is_empty());
}

/// The regression's other half: a window's pipes stayed watched after it let
/// its worker go, and the next session given their numbers could not be.
#[test]
fn a_link_let_go_stops_watching_its_pipes_before_it_closes_them() {
    let (mut link, ledger, watched_at_close) = started(usize::MAX);
    configure(&mut link, 0);
    assert!(ledger.names(PIPES.read_fd));
    drop(link);
    assert!(!watched_at_close.get(), "closed while watched");
    assert!(ledger.members().is_empty());
}

#[test]
fn pipes_that_cannot_be_watched_are_an_error_for_the_owner() {
    let (mut link, ledger, _) = started(usize::MAX);
    let mut elsewhere = ledger.clone();
    assert_eq!(
        elsewhere.add(WaitSourceKind::Stream, PIPES.read_fd, 99),
        Ok(())
    );
    link.ask(&RUN).expect("nothing is asked");
    assert_eq!(link.sync(0), Err(Errno::AlreadyExists));
}
