//! Guest memory as the kernel reaches it: with a privileged mode's rights,
//! a page at a time, an access a page refuses taken as the abort the SWI
//! making it would have taken.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Frame, FRAME_WORDS, PAGE_SIZE};

use super::{Fail, Os};

/// A page's frame and the offset of `address` in it, if a privileged mode
/// may read it, or write it when `write`.
fn page_of(os: &Os<'_>, address: u32, write: bool) -> Option<(Frame, usize)> {
    os.memory.privileged_page(address, write)
}

/// The byte at `offset` in a frame's words.
const fn byte(words: &[u32; FRAME_WORDS], offset: usize) -> u8 {
    words[offset / 4].to_le_bytes()[offset % 4]
}

/// The most bytes [`Os::copy_bytes`] holds at once.
const COPY_CHUNK: u32 = 4096;

impl Os<'_> {
    /// Fills `out` from guest memory at `address`.
    pub fn read_bytes(&mut self, address: u32, out: &mut [u8]) -> Result<(), Fail> {
        let mut at = address;
        let mut done = 0;
        while done < out.len() {
            let (frame, offset) = page_of(self, at, false).ok_or_else(|| self.aborted(at))?;
            let n = (PAGE_SIZE as usize - offset).min(out.len() - done);
            if let Some(words) = self.memory.frame_words(frame) {
                for (i, slot) in out[done..done + n].iter_mut().enumerate() {
                    *slot = byte(words, offset + i);
                }
            }
            done += n;
            if done < out.len() {
                at = self.advance(at, n)?;
            }
        }
        Ok(())
    }

    /// Writes `bytes` to guest memory at `address`.
    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), Fail> {
        let mut at = address;
        let mut done = 0;
        while done < bytes.len() {
            let (frame, offset) = page_of(self, at, true).ok_or_else(|| self.aborted(at))?;
            let n = (PAGE_SIZE as usize - offset).min(bytes.len() - done);
            if let Some(words) = self.memory.frame_words_mut(frame) {
                for (i, &value) in bytes[done..done + n].iter().enumerate() {
                    let place = offset + i;
                    let shift = 8 * (place % 4);
                    let word = &mut words[place / 4];
                    *word = (*word & !(0xFF << shift)) | (u32::from(value) << shift);
                }
            }
            done += n;
            if done < bytes.len() {
                at = self.advance(at, n)?;
            }
        }
        Ok(())
    }

    /// The words of the page holding `address`, which a privileged mode may
    /// write, and the index of the word holding `address` among them: a
    /// writer of many words pays one lookup a page. Whatever watches the
    /// page is told it is written.
    ///
    /// # Errors
    ///
    /// The abort an access there takes.
    pub fn page_words_mut(
        &mut self,
        address: u32,
    ) -> Result<(&mut [u32; FRAME_WORDS], usize), Fail> {
        let Some((frame, offset)) = page_of(self, address, true)
            .filter(|&(frame, _)| self.memory.frame_words(frame).is_some())
        else {
            return Err(self.aborted(address));
        };
        self.memory
            .frame_words_mut(frame)
            .map(|words| (words, offset / 4))
            .ok_or(Fail::Unwind(super::Unwind::Ended(super::Ended::Fault)))
    }

    /// The words of the page holding `address`, which a privileged mode may
    /// read, and the index of the word holding `address` among them.
    ///
    /// # Errors
    ///
    /// The abort an access there takes.
    pub fn page_words(&mut self, address: u32) -> Result<(&[u32; FRAME_WORDS], usize), Fail> {
        let Some((frame, offset)) = page_of(self, address, false)
            .filter(|&(frame, _)| self.memory.frame_words(frame).is_some())
        else {
            return Err(self.aborted(address));
        };
        self.memory
            .frame_words(frame)
            .map(|words| (words, offset / 4))
            .ok_or(Fail::Unwind(super::Unwind::Ended(super::Ended::Fault)))
    }

    /// `at` moved on by `n` bytes of a page; an access that would wrap past
    /// the top of memory aborts there.
    fn advance(&mut self, at: u32, n: usize) -> Result<u32, Fail> {
        u32::try_from(n)
            .ok()
            .and_then(|n| at.checked_add(n))
            .ok_or_else(|| self.aborted(at))
    }

    /// The byte at `address`.
    pub fn read_byte(&mut self, address: u32) -> Result<u8, Fail> {
        let (frame, offset) = page_of(self, address, false).ok_or_else(|| self.aborted(address))?;
        Ok(self
            .memory
            .frame_words(frame)
            .map_or(0, |words| byte(words, offset)))
    }

    /// The first byte at or after `address` that is not a space, and where
    /// it is; a long run of spaces yields to the host as it is read.
    pub fn skip_spaces(&mut self, address: u32) -> Result<(u8, u32), Fail> {
        let mut at = address;
        loop {
            let byte = self.read_byte(at)?;
            if byte != b' ' {
                return Ok((byte, at));
            }
            at = at.wrapping_add(1);
            self.yield_point()?;
        }
    }

    /// Writes the byte `value` at `address`.
    pub fn write_byte(&mut self, address: u32, value: u8) -> Result<(), Fail> {
        self.write_bytes(address, &[value])
    }

    /// The word at `address`, which need not be aligned: its four bytes in
    /// order, as a SWI reading a block takes them.
    pub fn read_word(&mut self, address: u32) -> Result<u32, Fail> {
        if address.is_multiple_of(4) {
            let (frame, offset) =
                page_of(self, address, false).ok_or_else(|| self.aborted(address))?;
            return Ok(self
                .memory
                .frame_words(frame)
                .map_or(0, |words| words[offset / 4]));
        }
        let mut bytes = [0; 4];
        self.read_bytes(address, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Writes the word `value` at `address`, which need not be aligned.
    pub fn write_word(&mut self, address: u32, value: u32) -> Result<(), Fail> {
        self.write_bytes(address, &value.to_le_bytes())
    }

    /// The byte at `address` if a privileged mode may read it, without
    /// aborting: for the kernel's reading of what the guest may have
    /// overwritten.
    pub(crate) fn peek_byte(&mut self, address: u32) -> Option<u8> {
        let (frame, offset) = page_of(self, address, false)?;
        self.memory
            .frame_words(frame)
            .map(|words| byte(words, offset))
    }

    /// The word at `address`, a multiple of four, if a privileged mode may
    /// read it, without aborting.
    pub(crate) fn peek_word(&mut self, address: u32) -> Option<u32> {
        if !address.is_multiple_of(4) {
            return None;
        }
        let (frame, offset) = page_of(self, address, false)?;
        self.memory
            .frame_words(frame)
            .map(|words| words[offset / 4])
    }

    /// Writes the word `value` at `address`, a multiple of four, if a
    /// privileged mode may write it, without aborting.
    pub(crate) fn poke_word(&mut self, address: u32, value: u32) -> Option<()> {
        if !address.is_multiple_of(4) {
            return None;
        }
        let (frame, offset) = page_of(self, address, true)?;
        let words = self.memory.frame_words_mut(frame)?;
        words[offset / 4] = value;
        Some(())
    }

    /// Writes the byte `value` at `address` if a privileged mode may write
    /// it, without aborting: for the kernel's own memory.
    pub(crate) fn poke_byte(&mut self, address: u32, value: u8) -> Option<()> {
        let (frame, offset) = page_of(self, address, true)?;
        let words = self.memory.frame_words_mut(frame)?;
        let shift = 8 * (offset % 4);
        let word = &mut words[offset / 4];
        *word = (*word & !(0xFF << shift)) | (u32::from(value) << shift);
        Some(())
    }

    /// The bytes at `address` up to, not including, the first that `ends`,
    /// which is asked of each in turn, at most `most` of them; and whether
    /// one did end them within that.
    pub fn read_until(
        &mut self,
        address: u32,
        most: usize,
        mut ends: impl FnMut(u8) -> bool,
    ) -> Result<(Vec<u8>, bool), Fail> {
        let mut text = Vec::new();
        let mut at = address;
        while text.len() < most {
            let value = self.read_byte(at)?;
            if ends(value) {
                return Ok((text, true));
            }
            if text.try_reserve(1).is_err() {
                return Err(self.no_memory());
            }
            text.push(value);
            at = at.wrapping_add(1);
        }
        let ended = ends(self.read_byte(at)?);
        Ok((text, ended))
    }

    /// A string at `address` ending at the first byte that `ends`, at most
    /// `most` bytes before it; "Buffer overflow" past that.
    pub(crate) fn read_ended(
        &mut self,
        address: u32,
        most: usize,
        ends: impl FnMut(u8) -> bool,
    ) -> Result<Vec<u8>, Fail> {
        match self.read_until(address, most, ends)? {
            (text, true) => Ok(text),
            (_, false) => Err(self.buffer_overflow()),
        }
    }

    /// A control-terminated string at `address`, as most SWIs take their
    /// text: up to its first byte below 32, at most `most` bytes.
    pub fn read_text(&mut self, address: u32, most: usize) -> Result<Vec<u8>, Fail> {
        self.read_ended(address, most, |b| b < b' ')
    }

    /// A zero-terminated string at `address`, at most `most` bytes.
    pub fn read_zstring(&mut self, address: u32, most: usize) -> Result<Vec<u8>, Fail> {
        self.read_ended(address, most, |b| b == 0)
    }

    /// Copies `bytes` from `from` to `to` a chunk at a time, as if through a
    /// buffer the whole length: ranges that overlap are copied from the end
    /// where `to` lies above `from`, so neither chunk overwrites what is yet
    /// to be read.
    ///
    /// # Errors
    ///
    /// The abort an access gives, or the unwind a long copy's yield meets.
    pub fn copy_bytes(&mut self, from: u32, to: u32, bytes: u32) -> Result<(), Fail> {
        let mut chunk = [0; COPY_CHUNK as usize];
        let backward = to > from && to - from < bytes;
        let mut done = 0;
        while done < bytes {
            let length = (bytes - done).min(COPY_CHUNK);
            let offset = if backward {
                bytes - done - length
            } else {
                done
            };
            let part = &mut chunk[..length as usize];
            self.read_bytes(from.wrapping_add(offset), part)?;
            self.write_bytes(to.wrapping_add(offset), part)?;
            done += length;
            self.yield_point()?;
        }
        Ok(())
    }

    /// Writes `bytes` bytes of `value` at `at`, a chunk at a time.
    ///
    /// # Errors
    ///
    /// The abort an access gives, or the unwind a long fill's yield meets.
    pub fn fill(&mut self, at: u32, bytes: u32, value: u8) -> Result<(), Fail> {
        let chunk = [value; COPY_CHUNK as usize];
        let mut done = 0;
        while done < bytes {
            let length = (bytes - done).min(COPY_CHUNK);
            self.write_bytes(at.wrapping_add(done), &chunk[..length as usize])?;
            done += length;
            self.yield_point()?;
        }
        Ok(())
    }

    /// Writes `text` and a terminating `end` at `address`, refusing with
    /// "Buffer overflow" where `room` bytes do not hold both.
    pub fn write_terminated(
        &mut self,
        address: u32,
        text: &[u8],
        end: u8,
        room: usize,
    ) -> Result<(), Fail> {
        if text.len() >= room {
            return Err(self.buffer_overflow());
        }
        self.write_bytes(address, text)?;
        self.write_byte(address.wrapping_add(super::len32(text.len())), end)
    }
}
