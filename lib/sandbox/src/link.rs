//! The worker's end of a session for a worker with work of its own.
//!
//! [`crate::session::serve_session`] is a pure reactor: it does nothing until a
//! frame arrives, so it reads its pipe blocking and whole frames at a time. A
//! worker that runs between its parent's frames — an emulated machine, which
//! owes its parent a picture every refresh and sound every few milliseconds —
//! cannot wait on its pipe longer than its next piece of work allows, and a
//! read that gives up mid-frame must not lose the bytes it took. A
//! [`WorkerLink`] reads into the same committed accumulator the owner's seam
//! does, so a frame arrives whole across any number of bounded waits.
//!
//! The deadlock argument is the session's: the owner never blocks, so a worker
//! blocked writing is woken when the owner drains, and one waiting to read is
//! woken by the owner's write or its own wait ending.

use tairix_abi::Errno;

use alloc::vec::Vec;

use crate::proto::{write_all, Channel, Inbound, ProtoError, FRAME_HEADER_LEN, MAX_FRAME};
use crate::session::{SessionError, MIN_QUEUE_BYTES};

/// A blocking [`Channel`] whose reads can also give up after a wait.
pub trait TimedChannel: Channel {
    /// Read up to `buf.len()` bytes, waiting at most `wait_ns` for the first
    /// of them — for ever where it is `None`, not at all where it is zero.
    /// `Ok(0)` is end-of-stream.
    ///
    /// # Errors
    ///
    /// [`Errno::TimedOut`] when the wait ended with nothing read, else the
    /// transport's typed failure.
    fn read_within(&mut self, buf: &mut [u8], wait_ns: Option<u64>) -> Result<usize, Errno>;
}

/// What one [`WorkerLink::recv`] found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Waited<R> {
    /// A whole frame, as the taker answered it.
    Frame(R),
    /// No whole frame yet: the wait ended, or only part of one arrived.
    Nothing,
    /// The parent closed the stream on a frame boundary.
    Closed,
}

/// The worker's end of a session, read with a wait.
///
/// What it sends is queued and written in one transport write by
/// [`WorkerLink::flush`], so a worker that ends each piece of work with a
/// handful of frames pays one write for them.
pub struct WorkerLink<C> {
    chan: C,
    inbound: Inbound,
    outbound: Vec<u8>,
}

impl<C: TimedChannel> WorkerLink<C> {
    /// The link over `chan`, taking in frames of up to `inbound_bytes` of
    /// framed traffic, committed now.
    ///
    /// # Errors
    ///
    /// [`SessionError::BoundTooSmall`] below [`MIN_QUEUE_BYTES`];
    /// [`SessionError::OutOfMemory`] when the accumulator cannot be had.
    pub fn new(chan: C, inbound_bytes: usize) -> Result<Self, SessionError> {
        if inbound_bytes < MIN_QUEUE_BYTES {
            return Err(SessionError::BoundTooSmall);
        }
        let inbound = Inbound::committed(inbound_bytes).map_err(|_| SessionError::OutOfMemory)?;
        Ok(Self {
            chan,
            inbound,
            outbound: Vec::new(),
        })
    }

    /// Largest payload a parent's frame may carry.
    #[must_use]
    pub fn max_recv_payload(&self) -> usize {
        self.inbound.max_payload()
    }

    /// Lend the next whole frame from the parent to `take`, making at most one
    /// transport read, which waits at most `wait_ns` for its first byte (for
    /// ever where it is `None`).
    ///
    /// A frame already held is lent without reading at all, so a worker
    /// drains a burst before it waits again.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Oversize`] for a frame declaring more than
    /// [`Self::max_recv_payload`], refused before its payload is read;
    /// [`ProtoError::PeerClosed`] for a stream ending mid-frame; the
    /// transport's failure otherwise.
    pub fn recv<R>(
        &mut self,
        wait_ns: Option<u64>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        if self.inbound.head_oversize() {
            return Err(ProtoError::Oversize);
        }
        if !self.inbound.head_whole() {
            let Self { chan, inbound, .. } = self;
            match inbound.fill(|buf| chan.read_within(buf, wait_ns)) {
                Ok(0) if inbound.whole() => return Ok(Waited::Closed),
                Ok(0) | Err(Errno::BrokenPipe) => return Err(ProtoError::PeerClosed),
                Ok(_) => {}
                Err(Errno::TimedOut) => return Ok(Waited::Nothing),
                Err(errno) => return Err(ProtoError::Channel(errno)),
            }
            if self.inbound.head_oversize() {
                return Err(ProtoError::Oversize);
            }
        }
        Ok(self
            .inbound
            .take(take)
            .map_or(Waited::Nothing, Waited::Frame))
    }

    /// Frame `payload` to go with the next [`Self::flush`].
    ///
    /// # Errors
    ///
    /// [`ProtoError::Oversize`] for a payload above [`MAX_FRAME`], nothing
    /// queued; [`ProtoError::OutOfMemory`] when the room cannot be had.
    pub fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        let declared = u32::try_from(payload.len())
            .ok()
            .filter(|_| payload.len() <= MAX_FRAME)
            .ok_or(ProtoError::Oversize)?;
        self.outbound
            .try_reserve(FRAME_HEADER_LEN + payload.len())
            .map_err(|_| ProtoError::OutOfMemory)?;
        self.outbound.extend_from_slice(&declared.to_le_bytes());
        self.outbound.extend_from_slice(payload);
        Ok(())
    }

    /// Write everything queued, blocking until the parent has room for all
    /// of it.
    ///
    /// # Errors
    ///
    /// [`ProtoError::PeerClosed`] or [`ProtoError::Channel`] on transport
    /// failure; what was queued is dropped, since the session is over.
    pub fn flush(&mut self) -> Result<(), ProtoError> {
        let written = write_all(&mut self.chan, &self.outbound);
        self.outbound.clear();
        written
    }

    /// Queue `payload` and flush.
    ///
    /// # Errors
    ///
    /// As [`Self::queue`] and [`Self::flush`].
    pub fn send(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.queue(payload)?;
        self.flush()
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
