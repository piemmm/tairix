//! A session whose HostFS discs are served in-process, over the fixture
//! tree, by the server a worker runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_riscos_filing::hostfs::protocol::Request;
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::os::{Host, Kernel, Pulse};

/// The host's real time: 2020, in centiseconds since 1900.
pub const NOW: u64 = 0x58_0000_0000;

/// The side holding the session's files, answering as `answer` does.
pub struct Relay<F> {
    /// How it answers a request.
    pub answer: F,
}

impl<F: FnMut(&[u8], &mut Vec<u8>) -> bool> Host for Relay<F> {
    fn monotonic(&mut self) -> u64 {
        0
    }

    fn real_time(&mut self) -> u64 {
        NOW
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Continue
    }

    fn wait(&mut self, _: Option<u64>) -> Pulse {
        Pulse::Stop
    }

    fn input(&mut self) -> Option<InputEvent> {
        None
    }

    fn relay(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        (self.answer)(request, reply)
    }
}

/// A served session, its server, and how many reads of a file's bytes have
/// crossed the relay.
pub struct Session {
    /// The session.
    pub kernel: Kernel,
    /// Its server.
    pub server: Rc<RefCell<Server<Handle>>>,
    /// The reads of a file's bytes that have crossed the relay.
    pub reads: Rc<Cell<usize>>,
}

/// A session served over `tree`'s `roots`: each a path, its folder's name
/// and whether it may be written.
pub fn session(tree: &Tree, roots: &[(&str, &str, bool)]) -> Session {
    session_with(tree, None, roots)
}

/// As [`session`], `own` — a path and its folder's name — the session's own
/// disc ahead of them.
pub fn session_with(
    tree: &Tree,
    own: Option<(&str, &str)>,
    roots: &[(&str, &str, bool)],
) -> Session {
    let root = |path: &str, name: &str, writable| Given {
        dir: tree.open_dir(path, false).expect("a root"),
        name: name.to_string(),
        writable,
    };
    let own = own.map(|(path, name)| root(path, name, true));
    let given = roots
        .iter()
        .map(|&(path, name, writable)| root(path, name, writable))
        .collect();
    let server = Rc::new(RefCell::new(Server::new(own, given, [9; 16], 1)));
    let reads = Rc::new(Cell::new(0));
    let (serving, counted) = (Rc::clone(&server), Rc::clone(&reads));
    let kernel = super::kernel_on(Box::new(Relay {
        answer: move |request: &[u8], reply: &mut Vec<u8>| {
            if matches!(Request::decode(request), Some(Request::ReadBytes { .. })) {
                counted.set(counted.get() + 1);
            }
            serving.borrow_mut().serve(request, reply);
            true
        },
    }));
    Session {
        kernel,
        server,
        reads,
    }
}
