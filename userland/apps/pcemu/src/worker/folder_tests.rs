//! A share reached through the port, against a stand-in front-end that
//! answers each request from the server over an in-memory folder and counts
//! the reads it was asked.

use super::super::cache::SectorCache;
use super::super::testing::{memory, DISCARD};
use super::super::Memory;
use super::*;
use crate::folder::memhost::{MemDir, MemHost};
use crate::folder::server::Server;
use crate::folder::{Access, Creation, DosName, SharePath};
use crate::link::{Command, Event};
use crate::time::Nanos;
use alloc::collections::VecDeque;
use tairix_hash::HashSeed;
use tairix_reclaim::CacheBudget;
use tairix_sandbox::link::Waited;
use tairix_sandbox::wire::Writer;
use tairix_sandbox::ProtoError;

/// The stand-in front-end: its folder, the answers waiting to be read, and
/// each read it was asked, as its offset and length.
struct Front {
    server: Server<MemDir>,
    answers: VecDeque<Vec<u8>>,
    reads: Vec<(u32, u32)>,
    /// Asked nothing more is answered: the session ends.
    silent: bool,
}

struct ServeLink(Rc<RefCell<Front>>);

impl Link for ServeLink {
    fn recv<R>(
        &mut self,
        _wait: Option<Nanos>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        let answer = self.0.borrow_mut().answers.pop_front();
        Ok(answer.map_or(Waited::Closed, |answer| Waited::Frame(take(&answer))))
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        let mut front = self.0.borrow_mut();
        let Ok(Event::Folder { request, op, .. }) = Event::decode(payload) else {
            return Ok(());
        };
        if front.silent {
            return Ok(());
        }
        if let Op::Read { offset, len, .. } = op {
            front.reads.push((offset, len));
        }
        let answer = {
            let mut w = Writer::new();
            Command::Foldered {
                request,
                answer: front.server.call(&op),
            }
            .encode(&mut w);
            w.finish()
        };
        front.answers.push_back(answer);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ProtoError> {
        Ok(())
    }

    fn now(&self) -> Nanos {
        0
    }
}

fn served(host: &MemHost) -> (RemoteFolder<ServeLink>, Rc<RefCell<Front>>) {
    let label = DosName::new(*b"HOST       ", false).unwrap();
    let front = Rc::new(RefCell::new(Front {
        server: Server::new(host.root(), label, true, 0x51),
        answers: VecDeque::new(),
        reads: Vec::new(),
        silent: false,
    }));
    let cache = SectorCache::new(
        CacheBudget::from_backing(1 << 20),
        HashSeed::from_bytes([3; 16]),
        memory().gauge(),
        &DISCARD,
    );
    let port = Port::new(ServeLink(Rc::clone(&front)), cache).unwrap();
    (RemoteFolder::new(Rc::new(RefCell::new(port)), 0), front)
}

fn path(field: &[u8; 11]) -> SharePath {
    SharePath::of(&[DosName::new(*field, false).unwrap()]).unwrap()
}

fn open(folder: &mut RemoteFolder<ServeLink>, field: &[u8; 11]) -> u32 {
    let op = Op::Open {
        path: path(field),
        access: Access::Both,
        creation: Creation::Existing,
    };
    match folder.call(&op) {
        Ok(Reply::Opened { handle, .. }) => handle,
        other => panic!("{other:?}"),
    }
}

fn read(folder: &mut RemoteFolder<ServeLink>, handle: u32, offset: u32, len: u32) -> Vec<u8> {
    match folder.call(&Op::Read {
        handle,
        offset,
        len,
    }) {
        Ok(Reply::Read(bytes)) => bytes.to_vec(),
        other => panic!("{other:?}"),
    }
}

fn write(folder: &mut RemoteFolder<ServeLink>, handle: u32, offset: u32, bytes: &[u8]) {
    let wrote = folder.call(&Op::Write {
        handle,
        offset,
        bytes,
    });
    assert!(matches!(wrote, Ok(Reply::Wrote(n)) if n as usize == bytes.len()));
}

fn piece() -> u32 {
    u32::try_from(AHEAD).unwrap()
}

/// A file of `len` bytes, each its offset's low byte.
fn counting(len: usize) -> Vec<u8> {
    (0..len).map(|at| at.to_le_bytes()[0]).collect()
}

#[test]
fn small_reads_are_answered_from_one_fetch_of_a_whole_piece() {
    let host = MemHost::new().file("DATA.BIN", &counting(4000));
    let (mut folder, front) = served(&host);
    let handle = open(&mut folder, b"DATA    BIN");
    for record in 0..20u32 {
        let bytes = read(&mut folder, handle, record * 128, 128);
        assert_eq!(bytes, counting(4000)[record as usize * 128..][..128]);
    }
    assert_eq!(front.borrow().reads, [(0, piece())]);
}

#[test]
fn a_read_reaching_past_what_was_fetched_asks_again_and_sees_the_file_grown() {
    let host = MemHost::new().file("LOG.TXT", b"first ");
    let (mut folder, front) = served(&host);
    let handle = open(&mut folder, b"LOG     TXT");
    assert_eq!(read(&mut folder, handle, 0, 100), b"first ");
    host.replace("LOG.TXT", b"first second");
    assert_eq!(read(&mut folder, handle, 6, 100), b"second");
    assert_eq!(front.borrow().reads, [(0, piece()), (6, piece())]);
}

#[test]
fn a_write_through_any_handle_on_the_file_forgets_what_was_fetched_of_it() {
    let host = MemHost::new()
        .file("DATA.BIN", &counting(1000))
        .file("OTHER.BIN", &counting(1000));
    let (mut folder, front) = served(&host);
    let reading = open(&mut folder, b"DATA    BIN");
    let writing = open(&mut folder, b"DATA    BIN");
    let other = open(&mut folder, b"OTHER   BIN");
    assert_eq!(read(&mut folder, reading, 0, 4), [0, 1, 2, 3]);
    write(&mut folder, other, 0, b"elsewhere");
    assert_eq!(read(&mut folder, reading, 4, 4), [4, 5, 6, 7]);
    assert_eq!(
        front.borrow().reads.len(),
        1,
        "another file's write kept it"
    );
    write(&mut folder, writing, 0, b"new!");
    assert_eq!(read(&mut folder, reading, 0, 4), b"new!");
    assert_eq!(
        front.borrow().reads.len(),
        2,
        "its own file's write forgot it"
    );
}

#[test]
fn a_file_opened_again_is_read_afresh() {
    let host = MemHost::new().file("DATA.BIN", b"before");
    let (mut folder, front) = served(&host);
    let handle = open(&mut folder, b"DATA    BIN");
    assert_eq!(read(&mut folder, handle, 0, 3), b"bef");
    host.replace("DATA.BIN", b"after!");
    let again = open(&mut folder, b"DATA    BIN");
    assert_eq!(read(&mut folder, handle, 0, 3), b"aft");
    assert_eq!(read(&mut folder, again, 3, 3), b"er!");
    assert_eq!(front.borrow().reads.len(), 3);
}

#[test]
fn a_read_of_a_whole_piece_goes_straight_to_the_front_end() {
    let host = MemHost::new().file("BIG.BIN", &counting(3 * AHEAD));
    let (mut folder, front) = served(&host);
    let handle = open(&mut folder, b"BIG     BIN");
    let bytes = read(&mut folder, handle, piece(), piece());
    assert_eq!(bytes, counting(3 * AHEAD)[AHEAD..][..AHEAD]);
    assert_eq!(read(&mut folder, handle, 0, 1), [0]);
    assert_eq!(
        front.borrow().reads,
        [(piece(), piece()), (0, piece())],
        "the large read fetched nothing ahead"
    );
}

#[test]
fn a_refusal_is_the_folders_answer_and_an_ended_session_the_share_gone() {
    let host = MemHost::new();
    let (mut folder, front) = served(&host);
    let missing = folder.call(&Op::Look {
        path: path(b"NOWHERE    "),
    });
    assert_eq!(missing.err(), Some(Refusal::NotFound));
    front.borrow_mut().silent = true;
    let unanswered = folder.call(&Op::Space);
    assert_eq!(unanswered.err(), Some(Refusal::Gone));
}
