//! Reading a document in a step at a time, so a long read is put down between
//! steps once its window closes, and never holds another window's save up
//! behind it.

use alloc::vec::Vec;

/// A document partway read in: the chunks read so far, and where the next
/// read starts.
///
/// The length the file measured when it was opened sizes the chunks, so a
/// file that has not changed is read straight into chunks that fit it. A file
/// that has grown since is read on to where it now ends, rather than cut at
/// the length it had, so a save cannot quietly drop what was added.
#[derive(Debug, Default)]
pub struct Reading {
    chunks: Vec<Vec<u8>>,
    offset: u64,
    measured: u64,
    /// Where a read past the measured length lands before what came is kept
    /// exactly.
    scratch: Vec<u8>,
}

/// What one step of a [`Reading`] came to.
#[derive(Debug)]
pub enum ReadStep<E> {
    /// There is more to read.
    Partial,
    /// The whole document, read to its end.
    Done(Vec<Vec<u8>>),
    /// The read was refused, carrying why.
    Refused(E),
    /// The room for what was read could not be held.
    NoMemory,
}

impl Reading {
    /// A read of a file that measured `measured` bytes when opened.
    #[must_use]
    pub const fn new(measured: u64) -> Self {
        Self {
            chunks: Vec::new(),
            offset: 0,
            measured,
            scratch: Vec::new(),
        }
    }

    /// Read on through at most `budget` more bytes, at most `chunk` a call to
    /// `read` — which fills what it is handed from the offset it is given,
    /// short only where the file ends.
    pub fn step<E>(
        &mut self,
        budget: usize,
        chunk: usize,
        mut read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> ReadStep<E> {
        let chunk = chunk.max(1);
        let mut left = budget.max(1);
        while left > 0 {
            let unread = self.measured.saturating_sub(self.offset);
            let want = usize::try_from(unread)
                .unwrap_or(usize::MAX)
                .min(chunk)
                .min(left);
            let (got, wanted) = if want > 0 {
                let Some(mut buf) = tairix_util::fallible::filled(want, 0u8) else {
                    return ReadStep::NoMemory;
                };
                let got = match read(self.offset, &mut buf) {
                    Ok(got) => got.min(want),
                    Err(err) => return ReadStep::Refused(err),
                };
                buf.truncate(got);
                if !self.keep(buf) {
                    return ReadStep::NoMemory;
                }
                (got, want)
            } else {
                // A byte first, doubling as the file proves to have grown:
                // one that has not costs a one-byte read, not a chunk's room.
                let grown = self.offset.saturating_sub(self.measured);
                let wanted = usize::try_from(grown)
                    .unwrap_or(usize::MAX)
                    .clamp(1, chunk)
                    .min(left);
                if self.scratch.len() < wanted {
                    let Some(scratch) = tairix_util::fallible::filled(wanted, 0u8) else {
                        return ReadStep::NoMemory;
                    };
                    self.scratch = scratch;
                }
                let got = match read(self.offset, &mut self.scratch[..wanted]) {
                    Ok(got) => got.min(wanted),
                    Err(err) => return ReadStep::Refused(err),
                };
                let mut buf = Vec::new();
                if buf.try_reserve_exact(got).is_err() {
                    return ReadStep::NoMemory;
                }
                buf.extend_from_slice(&self.scratch[..got]);
                if !self.keep(buf) {
                    return ReadStep::NoMemory;
                }
                (got, wanted)
            };
            self.offset = self.offset.saturating_add(got as u64);
            left = left.saturating_sub(got);
            if got < wanted {
                return ReadStep::Done(core::mem::take(&mut self.chunks));
            }
        }
        ReadStep::Partial
    }

    /// Keep `buf`, unless it is empty; whether the room for it was held.
    fn keep(&mut self, buf: Vec<u8>) -> bool {
        if buf.is_empty() {
            return true;
        }
        if self.chunks.try_reserve(1).is_err() {
            return false;
        }
        self.chunks.push(buf);
        true
    }
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;
