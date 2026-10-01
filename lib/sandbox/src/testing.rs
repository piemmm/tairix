//! What the protocol tests drive their workers through: a sink that keeps
//! nothing, a sandbox over a worker in this process, and workers that lie.

use alloc::vec::Vec;

use tairix_log::{Event, Sink};

use crate::host::ParserSandbox;
use crate::loopback::LoopbackLauncher;
use crate::worker::Service;

/// Keeps no event: a test that asserts on what is logged brings its own.
pub(crate) struct NullSink;

impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

/// A sandbox starting a fresh `S` in this process for every worker.
pub(crate) fn loopback<S: Service + Default>(
) -> ParserSandbox<LoopbackLauncher<fn() -> S>, NullSink> {
    ParserSandbox::new(LoopbackLauncher::new(S::default as fn() -> S), NullSink)
}

/// Answers every request with the one reply it holds.
pub(crate) struct Scripted(pub(crate) Vec<u8>);

impl Service for Scripted {
    fn handle(&mut self, _request: &[u8]) -> Vec<u8> {
        self.0.clone()
    }
}

/// A sandbox whose every worker answers `reply`, whatever it is asked.
pub(crate) fn scripted(
    reply: Vec<u8>,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Scripted>, NullSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(move || Scripted(reply.clone())),
        NullSink,
    )
}

/// Serves as `S` does until the request naming `op`, whose reply it
/// corrupts: a worker that turns hostile part-way through a session.
pub(crate) struct Tampering<S> {
    inner: S,
    op: u8,
    tamper: fn(Vec<u8>) -> Vec<u8>,
}

impl<S: Service> Service for Tampering<S> {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let reply = self.inner.handle(request);
        if request.first().copied() == Some(self.op) {
            (self.tamper)(reply)
        } else {
            reply
        }
    }
}

/// A sandbox over `S` whose workers corrupt `op`'s reply with `tamper`.
pub(crate) fn tampering<S: Service + Default>(
    op: u8,
    tamper: fn(Vec<u8>) -> Vec<u8>,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Tampering<S>>, NullSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(move || Tampering {
            inner: S::default(),
            op,
            tamper,
        }),
        NullSink,
    )
}
