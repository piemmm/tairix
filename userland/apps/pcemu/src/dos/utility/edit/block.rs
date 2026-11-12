//! The document's store: a DOS memory block, reached through the guest's
//! memory a segment's run at a time.

use super::text::Store;
use crate::bios::guest::Guest;
use crate::dos::utility::place;

/// `len` bytes of the guest's memory from paragraph `segment`.
pub struct Block<'a, 'm> {
    pub g: &'a mut Guest<'m>,
    pub segment: u16,
    pub len: u32,
}

/// The bytes moved at a time, an offset a segment can hold from any start.
const RUN: usize = 0x8000;

impl Block<'_, '_> {
    /// Where the run `done` bytes past `at` starts.
    fn start(&self, at: u32, done: usize) -> (u16, u16) {
        place(
            self.segment,
            at.saturating_add(u32::try_from(done).unwrap_or(u32::MAX)),
        )
    }
}

impl Store for Block<'_, '_> {
    fn capacity(&self) -> u32 {
        self.len
    }

    fn read(&mut self, at: u32, out: &mut [u8]) {
        let mut done = 0;
        while done < out.len() {
            let end = (done + RUN).min(out.len());
            let (segment, offset) = self.start(at, done);
            self.g.read_bytes(segment, offset, &mut out[done..end]);
            done = end;
        }
    }

    fn write(&mut self, at: u32, data: &[u8]) {
        let mut done = 0;
        while done < data.len() {
            let end = (done + RUN).min(data.len());
            let (segment, offset) = self.start(at, done);
            self.g.write_bytes(segment, offset, &data[done..end]);
            done = end;
        }
    }
}
