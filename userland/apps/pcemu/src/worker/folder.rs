//! A shared folder as the machine's DOS reaches it: each call a request to
//! the front-end through the port, the machine stopped until it is answered.
//! A file read in small pieces is fetched a whole piece at a time; what was
//! fetched ahead is forgotten as soon as its file is written or opened again,
//! and a read reaching past it always asks, so a file another program grows
//! is read to its new end.

use super::port::{Link, Port};
use crate::folder::{Folder, Op, Refusal, Reply, IO_MOST};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

/// What one fetch brings ahead of a small read: a whole piece.
const AHEAD: usize = IO_MOST;

/// Bytes of one open file fetched ahead of where it was read.
struct Ahead {
    handle: u32,
    node: u32,
    offset: u32,
    bytes: Vec<u8>,
}

impl Ahead {
    /// Where in what was fetched the read of `len` bytes from `offset`
    /// through `handle` lies, if it lies there whole. A read reaching past
    /// it asks again, so one at the file's end sees what was added since.
    fn covers(&self, handle: u32, offset: u32, len: usize) -> Option<core::ops::Range<usize>> {
        if self.handle != handle {
            return None;
        }
        let start = usize::try_from(offset.checked_sub(self.offset)?).ok()?;
        let end = start.checked_add(len)?;
        (end <= self.bytes.len()).then_some(start..end)
    }
}

/// A share the front-end serves, reached through the port.
pub struct RemoteFolder<L> {
    port: Rc<RefCell<Port<L>>>,
    slot: u8,
    /// The node each handle open is, so a write through any of them
    /// forgets what was fetched ahead of a read through another.
    nodes: Vec<(u32, u32)>,
    ahead: Ahead,
    /// A reply's bytes, lent to the caller until the next call.
    out: Vec<u8>,
}

impl<L: Link> RemoteFolder<L> {
    /// Share `slot`, reached through `port`.
    pub fn new(port: Rc<RefCell<Port<L>>>, slot: u8) -> Self {
        Self {
            port,
            slot,
            nodes: Vec::new(),
            ahead: Ahead {
                handle: 0,
                node: 0,
                offset: 0,
                bytes: Vec::new(),
            },
            out: Vec::new(),
        }
    }

    /// Ask the front-end, a read's bytes kept in `into`.
    fn ask(
        port: &RefCell<Port<L>>,
        slot: u8,
        op: &Op<'_>,
        into: &mut Vec<u8>,
    ) -> Result<Option<Reply<'static>>, Refusal> {
        let mut port = port.borrow_mut();
        match port.folder(slot, op)?.detach() {
            Ok(reply) => Ok(Some(reply)),
            Err(bytes) => {
                into.clear();
                into.try_reserve(bytes.len()).map_err(|_| Refusal::Fault)?;
                into.extend_from_slice(bytes);
                Ok(None)
            }
        }
    }

    /// Forget what was fetched ahead of a read of the node `handle` is.
    fn forget(&mut self, handle: u32) {
        let node = self
            .nodes
            .iter()
            .find(|&&(held, _)| held == handle)
            .map(|&(_, node)| node);
        if node.is_none_or(|node| node == self.ahead.node) {
            self.ahead.bytes.clear();
        }
    }

    fn read(&mut self, handle: u32, offset: u32, len: u32) -> Result<Reply<'_>, Refusal> {
        let want = usize::try_from(len).map_err(|_| Refusal::Fault)?;
        if let Some(range) = self.ahead.covers(handle, offset, want) {
            return Ok(Reply::Read(&self.ahead.bytes[range]));
        }
        if want >= AHEAD {
            let op = Op::Read {
                handle,
                offset,
                len,
            };
            return match Self::ask(&self.port, self.slot, &op, &mut self.out)? {
                Some(reply) => Ok(reply),
                None => Ok(Reply::Read(&self.out)),
            };
        }
        let fetch = Op::Read {
            handle,
            offset,
            len: u32::try_from(AHEAD).map_err(|_| Refusal::Fault)?,
        };
        self.ahead.bytes.clear();
        if Self::ask(&self.port, self.slot, &fetch, &mut self.ahead.bytes)?.is_some() {
            return Err(Refusal::Fault);
        }
        self.ahead.handle = handle;
        self.ahead.offset = offset;
        self.ahead.node = self
            .nodes
            .iter()
            .find(|&&(held, _)| held == handle)
            .map_or(0, |&(_, node)| node);
        let end = want.min(self.ahead.bytes.len());
        Ok(Reply::Read(&self.ahead.bytes[..end]))
    }
}

impl<L: Link> Folder for RemoteFolder<L> {
    fn call(&mut self, op: &Op<'_>) -> Result<Reply<'_>, Refusal> {
        match *op {
            Op::Read {
                handle,
                offset,
                len,
            } => return self.read(handle, offset, len),
            Op::Write { handle, .. } | Op::Resize { handle, .. } => self.forget(handle),
            Op::Close { handle, .. } => {
                self.forget(handle);
                self.nodes.retain(|&(held, _)| held != handle);
            }
            _ => {}
        }
        let answered = Self::ask(&self.port, self.slot, op, &mut self.out)?;
        if let Some(Reply::Opened { handle, file, .. }) = answered {
            self.nodes.retain(|&(held, _)| held != handle);
            self.nodes.push((handle, file));
            // A file opened again is read afresh, as it now is.
            if file == self.ahead.node {
                self.ahead.bytes.clear();
            }
        }
        Ok(answered.unwrap_or(Reply::Read(&self.out)))
    }
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;
