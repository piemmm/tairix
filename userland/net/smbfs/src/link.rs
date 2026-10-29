//! One connection's plumbing: its worker's session, the orders waiting for
//! room toward the worker, and the bytes waiting for room in the socket.
//!
//! The worker blocks writing when its pipe is full and reads no order while
//! it does, so the front never stops reading it: a front that waited on its
//! socket before reading the worker, while the worker waited on the front and
//! the server on the worker, would wait for ever. The bytes the socket has
//! not taken wait here instead — a worker sends only what the front's orders
//! call for, so they are bounded, and a worker sending more is lying and is
//! contained. The socket is what waits: while an order waits for room toward
//! the worker, the front takes no further socket event, and the stack holds
//! the server back.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::net::SocketId;
use tairix_abi::Errno;
use tairix_log::Sink;
use tairix_sandbox::session::{SandboxSession, SessionBounds, SessionError, SessionTransport};

use crate::host::{Arming, Host};
#[cfg(doc)]
use crate::wire::REPORT_MAX;
use crate::wire::{encode_stage, Order, CHUNK_MAX, ORDER_MAX, ORDER_QUEUE, REPORT_QUEUE};

/// The most bytes the socket may hold back: what every order in flight on a
/// connection can call for.
pub const BACKLOG_MAX: usize = 4 << 20;

/// Why a link can carry no more.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LinkError {
    /// The worker failed, or sent more than an honest one would.
    Worker,
    /// The socket failed.
    Socket(Errno),
    /// No room could be had.
    OutOfMemory,
}

/// One connection's worker and what waits on either side of it.
pub struct Link<T: SessionTransport, S: Sink> {
    session: SandboxSession<T, S>,
    /// Orders that found the worker's queue full, in order; every later one
    /// waits behind them, so none overtakes another.
    outbox: VecDeque<Vec<u8>>,
    /// Bytes the socket has not taken, sent before any more.
    backlog: Vec<u8>,
    order: Vec<u8>,
}

impl<T: SessionTransport, S: Sink> Link<T, S> {
    /// The link over a launched worker, reporting its failure to `sink`.
    ///
    /// # Errors
    ///
    /// [`LinkError::OutOfMemory`] when its queues cannot be committed; the
    /// worker is then reaped.
    pub fn new(worker: T, sink: S) -> Result<Self, LinkError> {
        let mut order = Vec::new();
        let mut backlog = Vec::new();
        let reserved =
            order.try_reserve_exact(ORDER_MAX).is_ok() && backlog.try_reserve(CHUNK_MAX).is_ok();
        let bounds = SessionBounds::new(ORDER_QUEUE, REPORT_QUEUE)
            .ok()
            .filter(|_| reserved);
        let Some(bounds) = bounds else {
            let _ = worker.dispose();
            return Err(LinkError::OutOfMemory);
        };
        let session =
            SandboxSession::new(worker, bounds, sink).map_err(|_| LinkError::OutOfMemory)?;
        Ok(Self {
            session,
            outbox: VecDeque::new(),
            backlog,
            order,
        })
    }

    /// Queue `order` for the worker.
    ///
    /// # Errors
    ///
    /// [`LinkError::Worker`] once it has failed; [`LinkError::OutOfMemory`].
    pub fn order(&mut self, order: &Order<'_>) -> Result<(), LinkError> {
        order
            .encode(&mut self.order)
            .map_err(|_| LinkError::OutOfMemory)?;
        self.send_order()
    }

    /// Queue for the worker the staging of `parts`, joined, for the write
    /// `request`.
    ///
    /// # Errors
    ///
    /// As [`Self::order`].
    pub fn stage(&mut self, request: u32, parts: &[&[u8]]) -> Result<(), LinkError> {
        encode_stage(request, parts, &mut self.order).map_err(|_| LinkError::OutOfMemory)?;
        self.send_order()
    }

    /// Send the order just encoded, or queue it behind those waiting.
    fn send_order(&mut self) -> Result<(), LinkError> {
        if self.outbox.is_empty() {
            match self.session.send(&self.order) {
                Ok(()) => return Ok(()),
                Err(SessionError::OutboundFull) => {}
                Err(_) => return Err(LinkError::Worker),
            }
        }
        let mut held = Vec::new();
        held.try_reserve_exact(self.order.len())
            .map_err(|_| LinkError::OutOfMemory)?;
        held.extend_from_slice(&self.order);
        self.outbox
            .try_reserve(1)
            .map_err(|_| LinkError::OutOfMemory)?;
        self.outbox.push_back(held);
        Ok(())
    }

    /// The worker's pipe has room: write to it, then queue what waited.
    ///
    /// # Errors
    ///
    /// [`LinkError::Worker`] when the worker has failed.
    pub fn writable(&mut self) -> Result<(), LinkError> {
        self.session.on_writable().map_err(|_| LinkError::Worker)?;
        while let Some(held) = self.outbox.front() {
            match self.session.send(held) {
                Ok(()) => {
                    self.outbox.pop_front();
                }
                Err(SessionError::OutboundFull) => break,
                Err(_) => return Err(LinkError::Worker),
            }
        }
        Ok(())
    }

    /// The worker has written: read it.
    ///
    /// # Errors
    ///
    /// [`LinkError::Worker`] when the worker has failed or broke the framing.
    pub fn readable(&mut self) -> Result<(), LinkError> {
        self.session.on_readable().map_err(|_| LinkError::Worker)
    }

    /// Copy the next report the worker sent into `into`, which holds
    /// [`REPORT_MAX`] bytes, so the front can order more while acting on it;
    /// whether there was one.
    ///
    /// # Errors
    ///
    /// [`LinkError::Worker`] when the worker has failed or broke the framing,
    /// and once it has ended, which a connection's worker never does of its
    /// own accord: what it reported before is taken first.
    pub fn next_report(&mut self, into: &mut Vec<u8>) -> Result<bool, LinkError> {
        let taken = self.session.recv(|payload| {
            // The session admits no frame past the largest report, which
            // `into` holds.
            into.clear();
            into.extend_from_slice(payload);
        });
        match taken {
            Ok(Some(())) => Ok(true),
            Ok(None) if self.session.peer_finished() => Err(LinkError::Worker),
            Ok(None) => Ok(false),
            Err(_) => Err(LinkError::Worker),
        }
    }

    /// Send `bytes` to `socket`, keeping what it does not take.
    ///
    /// # Errors
    ///
    /// [`LinkError::Socket`] for the stack's refusal;
    /// [`LinkError::Worker`] past [`BACKLOG_MAX`];
    /// [`LinkError::OutOfMemory`].
    pub fn transmit<H: Host>(
        &mut self,
        host: &mut H,
        socket: SocketId,
        bytes: &[u8],
    ) -> Result<(), LinkError> {
        let taken = if self.backlog.is_empty() {
            host.send(socket, bytes).map_err(LinkError::Socket)?
        } else {
            0
        };
        let rest = bytes.get(taken..).unwrap_or_default();
        if rest.is_empty() {
            return Ok(());
        }
        if self.backlog.len() + rest.len() > BACKLOG_MAX {
            return Err(LinkError::Worker);
        }
        self.backlog
            .try_reserve(rest.len())
            .map_err(|_| LinkError::OutOfMemory)?;
        self.backlog.extend_from_slice(rest);
        Ok(())
    }

    /// The socket has room: send what it held back, one chunk at a time
    /// until it takes no more.
    ///
    /// # Errors
    ///
    /// [`LinkError::Socket`] for the stack's refusal.
    pub fn room<H: Host>(&mut self, host: &mut H, socket: SocketId) -> Result<(), LinkError> {
        let mut sent = 0;
        while sent < self.backlog.len() {
            let chunk = &self.backlog[sent..(sent + CHUNK_MAX).min(self.backlog.len())];
            let taken = host
                .send(socket, chunk)
                .map_err(LinkError::Socket)?
                .min(chunk.len());
            sent += taken;
            if taken < chunk.len() {
                break;
            }
        }
        self.backlog.drain(..sent);
        Ok(())
    }

    /// What the connection waits on, its socket open or not.
    #[must_use]
    pub fn arming(&self, socket: bool) -> Arming {
        Arming {
            socket: socket && self.outbox.is_empty(),
            worker: self.session.descriptors(),
            read: self.session.wants_read(),
            write: self.session.wants_write(),
        }
    }
}
