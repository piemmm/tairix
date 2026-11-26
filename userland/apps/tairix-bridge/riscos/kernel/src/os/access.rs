//! Guest memory as the kernel reaches it: with a privileged mode's rights,
//! or, for a routine acting for its caller, with the caller's; a page at a
//! time, an access a page refuses taken as the abort the SWI making it would
//! have taken.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Frame, FRAME_WORDS, PAGE_SIZE};

use super::{Fail, Os};

/// A page's frame and the offset of `address` in it, if the rights reached
/// with may read it, or write it when `write`.
fn page_of(os: &Os<'_>, address: u32, write: bool, privileged: bool) -> Option<(Frame, usize)> {
    os.memory.page_with(address, write, privileged)
}

/// The words of the page holding `address`, if the rights reached with may
/// read it, and the offset of `address` in them.
fn readable<'a>(
    os: &'a Os<'_>,
    address: u32,
    privileged: bool,
) -> Option<(&'a [u32; FRAME_WORDS], usize)> {
    let (frame, offset) = page_of(os, address, false, privileged)?;
    Some((os.memory.frame_words(frame)?, offset))
}

/// As [`readable`], for a write: whatever watches the page is told it is
/// written.
fn writable<'a>(
    os: &'a mut Os<'_>,
    address: u32,
    privileged: bool,
) -> Option<(&'a mut [u32; FRAME_WORDS], usize)> {
    let (frame, offset) = page_of(os, address, true, privileged)?;
    Some((os.memory.frame_words_mut(frame)?, offset))
}

/// The byte at `offset` in a frame's words.
const fn byte(words: &[u32; FRAME_WORDS], offset: usize) -> u8 {
    words[offset / 4].to_le_bytes()[offset % 4]
}

/// Sets the byte at `offset` in a frame's words to `value`.
fn set_byte(words: &mut [u32; FRAME_WORDS], offset: usize, value: u8) {
    let shift = 8 * (offset % 4);
    let word = &mut words[offset / 4];
    *word = (*word & !(0xFF << shift)) | (u32::from(value) << shift);
}

/// How many bytes from `offset` lie before the next word boundary, at most
/// `length`.
fn to_boundary(offset: usize, length: usize) -> usize {
    ((4 - offset % 4) % 4).min(length)
}

/// Fills `out` from the bytes at `offset` in a frame's words, a whole word
/// at a time between the boundaries.
fn copy_out(words: &[u32; FRAME_WORDS], offset: usize, out: &mut [u8]) {
    let (head, rest) = out.split_at_mut(to_boundary(offset, out.len()));
    for (n, slot) in head.iter_mut().enumerate() {
        *slot = byte(words, offset + n);
    }
    let aligned = offset + head.len();
    let (body, tail) = rest.as_chunks_mut::<4>();
    for (bytes, word) in body.iter_mut().zip(&words[aligned / 4..]) {
        *bytes = word.to_le_bytes();
    }
    let after = aligned + 4 * body.len();
    for (n, slot) in tail.iter_mut().enumerate() {
        *slot = byte(words, after + n);
    }
}

/// Writes `bytes` into a frame's words at `offset`, a whole word at a time
/// between the boundaries.
fn copy_in(words: &mut [u32; FRAME_WORDS], offset: usize, bytes: &[u8]) {
    let (head, rest) = bytes.split_at(to_boundary(offset, bytes.len()));
    for (n, &value) in head.iter().enumerate() {
        set_byte(words, offset + n, value);
    }
    let aligned = offset + head.len();
    let (body, tail) = rest.as_chunks::<4>();
    for (word, chunk) in words[aligned / 4..].iter_mut().zip(body) {
        *word = u32::from_le_bytes(*chunk);
    }
    let after = aligned + 4 * body.len();
    for (n, &value) in tail.iter().enumerate() {
        set_byte(words, after + n, value);
    }
}

/// The bytes `count` words from `address` take where a page walk moves them
/// whole: `address` aligned, and the block short of the top of memory, past
/// which a word-by-word access wraps.
fn aligned_block(address: u32, count: usize) -> Option<usize> {
    let length = count.checked_mul(4)?;
    let last = u32::try_from(length.checked_sub(1)?).ok()?;
    (address.is_multiple_of(4) && address.checked_add(last).is_some()).then_some(length)
}

/// The most bytes [`Reach::copy_bytes`] holds at once.
const COPY_CHUNK: u32 = 4096;

/// Guest memory reached with one mode's rights: a privileged mode's, as the
/// kernel reaches it, or, from [`Os::as_caller`], those of the code a
/// routine acts for. An access the rights refuse aborts, as the code making
/// it would.
pub struct Reach<'a, 'o> {
    os: &'a mut Os<'o>,
    privileged: bool,
}

impl<'o> Os<'o> {
    /// Guest memory reached as the code the routine running acts for would
    /// reach it: with user mode's rights where user-mode code called the
    /// routine, a privileged mode's where privileged code did, and for a
    /// SWI's work.
    pub fn as_caller(&mut self) -> Reach<'_, 'o> {
        Reach {
            privileged: !self.world.user_caller,
            os: self,
        }
    }

    /// Guest memory reached with a privileged mode's rights.
    const fn privileged(&mut self) -> Reach<'_, 'o> {
        Reach {
            os: self,
            privileged: true,
        }
    }
}

impl<'a> Reach<'a, '_> {
    /// Walks `length` bytes from `address` a page at a time, handing `each`
    /// a page's frame, the offset in it, how far into the range the page
    /// starts, and how many of its bytes the range covers; `each` answers
    /// whether memory backs the frame. Stops at the first page the rights
    /// refuse, that no memory backs, or that the range would wrap past the
    /// top of memory to reach, and gives where that page is.
    fn walk(
        &mut self,
        address: u32,
        length: usize,
        write: bool,
        mut each: impl FnMut(&mut Os<'_>, Frame, usize, usize, usize) -> bool,
    ) -> Result<(), u32> {
        let mut at = address;
        let mut done = 0;
        while done < length {
            let (frame, offset) = page_of(self.os, at, write, self.privileged).ok_or(at)?;
            let n = (PAGE_SIZE as usize - offset).min(length - done);
            if !each(self.os, frame, offset, done, n) {
                return Err(at);
            }
            done += n;
            if done < length {
                at = u32::try_from(n)
                    .ok()
                    .and_then(|n| at.checked_add(n))
                    .ok_or(at)?;
            }
        }
        Ok(())
    }

    /// Fills `out` from guest memory at `address`.
    ///
    /// # Errors
    ///
    /// The abort a read in the range takes.
    pub fn read_bytes(&mut self, address: u32, out: &mut [u8]) -> Result<(), Fail> {
        self.walk(address, out.len(), false, |os, frame, offset, done, n| {
            os.memory
                .frame_words(frame)
                .map(|words| copy_out(words, offset, &mut out[done..done + n]))
                .is_some()
        })
        .map_err(|at| self.os.aborted(at))
    }

    /// Appends `length` bytes of guest memory from `address` to `out`. The
    /// range is proven readable before the host grants room for it, so a
    /// length no guest memory backs costs the host nothing.
    ///
    /// # Errors
    ///
    /// The abort a read in the range takes; "System heap full" where the
    /// host refuses the room.
    pub fn read_onto(
        &mut self,
        out: &mut Vec<u8>,
        address: u32,
        length: usize,
    ) -> Result<(), Fail> {
        self.walk(address, length, false, |os, frame, _, _, _| {
            os.memory.frame_words(frame).is_some()
        })
        .map_err(|at| self.os.aborted(at))?;
        out.try_reserve(length).map_err(|_| self.os.no_memory())?;
        let start = out.len();
        out.resize(start + length, 0);
        let read = self.read_bytes(address, &mut out[start..]);
        if read.is_err() {
            out.truncate(start);
        }
        read
    }

    /// `length` bytes of guest memory from `address`, read as
    /// [`Reach::read_onto`] reads them.
    ///
    /// # Errors
    ///
    /// As [`Reach::read_onto`].
    pub fn read_vec(&mut self, address: u32, length: usize) -> Result<Vec<u8>, Fail> {
        let mut bytes = Vec::new();
        self.read_onto(&mut bytes, address, length)?;
        Ok(bytes)
    }

    /// Writes `bytes` to guest memory at `address`.
    ///
    /// # Errors
    ///
    /// The abort a write in the range takes, the pages before it written.
    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), Fail> {
        self.walk(address, bytes.len(), true, |os, frame, offset, done, n| {
            os.memory
                .frame_words_mut(frame)
                .map(|words| copy_in(words, offset, &bytes[done..done + n]))
                .is_some()
        })
        .map_err(|at| self.os.aborted(at))
    }

    /// The words of the page holding `address`, which the rights may write,
    /// and the index of the word holding `address` among them: a writer of
    /// many words pays one lookup a page. Whatever watches the page is told
    /// it is written.
    ///
    /// # Errors
    ///
    /// The abort an access there takes.
    pub fn page_words_mut(self, address: u32) -> Result<(&'a mut [u32; FRAME_WORDS], usize), Fail> {
        let Some((frame, offset)) = page_of(self.os, address, true, self.privileged)
            .filter(|&(frame, _)| self.os.memory.frame_words(frame).is_some())
        else {
            return Err(self.os.aborted(address));
        };
        self.os
            .memory
            .frame_words_mut(frame)
            .map(|words| (words, offset / 4))
            .ok_or(Fail::Unwind(super::Unwind::Ended(super::Ended::Fault)))
    }

    /// The words of the page holding `address`, which the rights may read,
    /// and the index of the word holding `address` among them.
    ///
    /// # Errors
    ///
    /// The abort an access there takes.
    pub fn page_words(self, address: u32) -> Result<(&'a [u32; FRAME_WORDS], usize), Fail> {
        let Some((frame, offset)) = page_of(self.os, address, false, self.privileged)
            .filter(|&(frame, _)| self.os.memory.frame_words(frame).is_some())
        else {
            return Err(self.os.aborted(address));
        };
        self.os
            .memory
            .frame_words(frame)
            .map(|words| (words, offset / 4))
            .ok_or(Fail::Unwind(super::Unwind::Ended(super::Ended::Fault)))
    }

    /// The byte at `address`.
    ///
    /// # Errors
    ///
    /// The abort the read takes.
    pub fn read_byte(&mut self, address: u32) -> Result<u8, Fail> {
        let value =
            readable(self.os, address, self.privileged).map(|(words, offset)| byte(words, offset));
        value.ok_or_else(|| self.os.aborted(address))
    }

    /// The first byte at or after `address` that is not a space, and where
    /// it is; a long run of spaces yields to the host as it is read.
    ///
    /// # Errors
    ///
    /// The abort a read takes, or the unwind a yield meets.
    pub fn skip_spaces(&mut self, address: u32) -> Result<(u8, u32), Fail> {
        let mut at = address;
        loop {
            let byte = self.read_byte(at)?;
            if byte != b' ' {
                return Ok((byte, at));
            }
            at = at.wrapping_add(1);
            self.os.yield_point()?;
        }
    }

    /// Writes the byte `value` at `address`.
    ///
    /// # Errors
    ///
    /// The abort the write takes.
    pub fn write_byte(&mut self, address: u32, value: u8) -> Result<(), Fail> {
        self.write_bytes(address, &[value])
    }

    /// The pair of signed 16-bit coordinates at `address`, as an `OS_Word`
    /// block holds a point.
    ///
    /// # Errors
    ///
    /// The abort the read takes.
    pub fn read_coordinates(&mut self, address: u32) -> Result<(i32, i32), Fail> {
        let mut bytes = [0; 4];
        self.read_bytes(address, &mut bytes)?;
        let [x0, x1, y0, y1] = bytes;
        Ok((
            i32::from(i16::from_le_bytes([x0, x1])),
            i32::from(i16::from_le_bytes([y0, y1])),
        ))
    }

    /// `(x, y)` written at `address` as a pair of 16-bit coordinates, each
    /// cut to its low 16 bits.
    ///
    /// # Errors
    ///
    /// The abort the write takes.
    pub fn write_coordinates(&mut self, address: u32, (x, y): (i32, i32)) -> Result<(), Fail> {
        let [x0, x1, ..] = x.to_le_bytes();
        let [y0, y1, ..] = y.to_le_bytes();
        self.write_bytes(address, &[x0, x1, y0, y1])
    }

    /// The word at `address`, which need not be aligned: its four bytes in
    /// order, as a SWI reading a block takes them.
    ///
    /// # Errors
    ///
    /// The abort the read takes.
    pub fn read_word(&mut self, address: u32) -> Result<u32, Fail> {
        if address.is_multiple_of(4) {
            let value = readable(self.os, address, self.privileged)
                .map(|(words, offset)| words[offset / 4]);
            return value.ok_or_else(|| self.os.aborted(address));
        }
        let mut bytes = [0; 4];
        self.read_bytes(address, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Writes the word `value` at `address`, which need not be aligned.
    ///
    /// # Errors
    ///
    /// The abort the write takes.
    pub fn write_word(&mut self, address: u32, value: u32) -> Result<(), Fail> {
        self.write_bytes(address, &value.to_le_bytes())
    }

    /// Fills `out` with the words from `address` on, each read as
    /// [`Reach::read_word`] reads it, the address wrapping past the top of
    /// memory.
    ///
    /// # Errors
    ///
    /// The abort a read takes.
    pub fn read_words(&mut self, address: u32, out: &mut [u32]) -> Result<(), Fail> {
        let Some(length) = aligned_block(address, out.len()) else {
            for (i, word) in (0u32..).zip(out) {
                *word = self.read_word(address.wrapping_add(4 * i))?;
            }
            return Ok(());
        };
        self.walk(address, length, false, |os, frame, offset, done, n| {
            os.memory
                .frame_words(frame)
                .map(|words| {
                    out[done / 4..(done + n) / 4]
                        .copy_from_slice(&words[offset / 4..(offset + n) / 4]);
                })
                .is_some()
        })
        .map_err(|at| self.os.aborted(at))
    }

    /// Writes `words` from `address` on, each as [`Reach::write_word`]
    /// writes it, the address wrapping past the top of memory.
    ///
    /// # Errors
    ///
    /// The abort a write takes, the words before it written.
    pub fn write_words(&mut self, address: u32, words: &[u32]) -> Result<(), Fail> {
        let Some(length) = aligned_block(address, words.len()) else {
            for (i, &word) in (0u32..).zip(words) {
                self.write_word(address.wrapping_add(4 * i), word)?;
            }
            return Ok(());
        };
        self.walk(address, length, true, |os, frame, offset, done, n| {
            os.memory
                .frame_words_mut(frame)
                .map(|page| {
                    page[offset / 4..(offset + n) / 4]
                        .copy_from_slice(&words[done / 4..(done + n) / 4]);
                })
                .is_some()
        })
        .map_err(|at| self.os.aborted(at))
    }

    /// The byte at `address` if the rights may read it, without aborting.
    pub(crate) fn peek_byte(&mut self, address: u32) -> Option<u8> {
        readable(self.os, address, self.privileged).map(|(words, offset)| byte(words, offset))
    }

    /// The word at `address`, a multiple of four, if the rights may read it,
    /// without aborting: for walking what a caller hands over as the caller
    /// itself would.
    pub fn peek_word(&mut self, address: u32) -> Option<u32> {
        if !address.is_multiple_of(4) {
            return None;
        }
        readable(self.os, address, self.privileged).map(|(words, offset)| words[offset / 4])
    }

    /// Writes the word `value` at `address`, a multiple of four, if the
    /// rights may write it, without aborting.
    pub(crate) fn poke_word(&mut self, address: u32, value: u32) -> Option<()> {
        if !address.is_multiple_of(4) {
            return None;
        }
        let (words, offset) = writable(self.os, address, self.privileged)?;
        words[offset / 4] = value;
        Some(())
    }

    /// Writes the byte `value` at `address` if the rights may write it,
    /// without aborting.
    pub(crate) fn poke_byte(&mut self, address: u32, value: u8) -> Option<()> {
        let (words, offset) = writable(self.os, address, self.privileged)?;
        set_byte(words, offset, value);
        Some(())
    }

    /// The bytes at `address` up to, not including, the first that `ends`,
    /// which is asked of each in turn, at most `most` of them; and whether
    /// one did end them within that, the byte after the most asked too.
    /// Each page is looked up once.
    ///
    /// # Errors
    ///
    /// The abort a read takes; "System heap full" where the host refuses the
    /// room.
    pub fn read_until(
        &mut self,
        address: u32,
        most: usize,
        mut ends: impl FnMut(u8) -> bool,
    ) -> Result<(Vec<u8>, bool), Fail> {
        let mut text = Vec::new();
        let mut at = address;
        loop {
            let room = most - text.len();
            let Some((words, offset)) = readable(self.os, at, self.privileged) else {
                return Err(self.os.aborted(at));
            };
            let span = (PAGE_SIZE as usize - offset).min(room.saturating_add(1));
            if text.try_reserve(span.min(room)).is_err() {
                return Err(self.os.no_memory());
            }
            for n in 0..span {
                let value = byte(words, offset + n);
                if ends(value) {
                    return Ok((text, true));
                }
                if text.len() == most {
                    return Ok((text, false));
                }
                text.push(value);
            }
            at = at.wrapping_add(super::len32(span));
        }
    }

    /// How far from `address` the first byte that `ends` lies, within `most`
    /// bytes; none where no byte that near does. Read a page at a time, as a
    /// SWI scanning the guest's memory reads it, nothing copied.
    ///
    /// # Errors
    ///
    /// The abort an access gives, or the unwind a long scan's yield meets.
    pub fn position(
        &mut self,
        address: u32,
        most: u32,
        mut ends: impl FnMut(u8) -> bool,
    ) -> Result<Option<u32>, Fail> {
        let mut done = 0;
        while done < most {
            let at = address.wrapping_add(done);
            let Some((words, offset)) = readable(self.os, at, self.privileged) else {
                return Err(self.os.aborted(at));
            };
            let span = (PAGE_SIZE - super::len32(offset)).min(most - done);
            for n in 0..span {
                if ends(byte(words, offset + n as usize)) {
                    return Ok(Some(done + n));
                }
            }
            done += span;
            self.os.yield_point()?;
        }
        Ok(None)
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
            (_, false) => Err(self.os.buffer_overflow()),
        }
    }

    /// A control-terminated string at `address`, as most SWIs take their
    /// text: up to its first byte below 32, at most `most` bytes.
    ///
    /// # Errors
    ///
    /// The abort a read takes, or "Buffer overflow" past `most`.
    pub fn read_text(&mut self, address: u32, most: usize) -> Result<Vec<u8>, Fail> {
        self.read_ended(address, most, |b| b < b' ')
    }

    /// A zero-terminated string at `address`, at most `most` bytes.
    ///
    /// # Errors
    ///
    /// The abort a read takes, or "Buffer overflow" past `most`.
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
            self.os.yield_point()?;
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
            self.os.yield_point()?;
        }
        Ok(())
    }

    /// Writes `text` and a terminating `end` at `address`, refusing with
    /// "Buffer overflow" where `room` bytes do not hold both.
    ///
    /// # Errors
    ///
    /// "Buffer overflow", or the abort the write takes.
    pub fn write_terminated(
        &mut self,
        address: u32,
        text: &[u8],
        end: u8,
        room: usize,
    ) -> Result<(), Fail> {
        if text.len() >= room {
            return Err(self.os.buffer_overflow());
        }
        self.write_bytes(address, text)?;
        self.write_byte(address.wrapping_add(super::len32(text.len())), end)
    }
}

/// The kernel's own accessors: each [`Reach`]'s, with a privileged mode's
/// rights.
impl Os<'_> {
    /// Where the application space the session's layout gives ends: the most
    /// a slot could ever reach.
    #[must_use]
    pub fn application_ceiling(&self) -> u32 {
        let layout = self.memory.layout();
        layout.top_32bit.max(layout.top_26bit)
    }

    /// [`Reach::read_bytes`], privileged.
    pub fn read_bytes(&mut self, address: u32, out: &mut [u8]) -> Result<(), Fail> {
        self.privileged().read_bytes(address, out)
    }

    /// [`Reach::read_onto`], privileged.
    pub fn read_onto(
        &mut self,
        out: &mut Vec<u8>,
        address: u32,
        length: usize,
    ) -> Result<(), Fail> {
        self.privileged().read_onto(out, address, length)
    }

    /// [`Reach::read_vec`], privileged.
    pub fn read_vec(&mut self, address: u32, length: usize) -> Result<Vec<u8>, Fail> {
        self.privileged().read_vec(address, length)
    }

    /// [`Reach::write_bytes`], privileged.
    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), Fail> {
        self.privileged().write_bytes(address, bytes)
    }

    /// [`Reach::page_words_mut`], privileged.
    pub fn page_words_mut(
        &mut self,
        address: u32,
    ) -> Result<(&mut [u32; FRAME_WORDS], usize), Fail> {
        self.privileged().page_words_mut(address)
    }

    /// [`Reach::page_words`], privileged.
    pub fn page_words(&mut self, address: u32) -> Result<(&[u32; FRAME_WORDS], usize), Fail> {
        self.privileged().page_words(address)
    }

    /// [`Reach::read_byte`], privileged.
    pub fn read_byte(&mut self, address: u32) -> Result<u8, Fail> {
        self.privileged().read_byte(address)
    }

    /// [`Reach::skip_spaces`], privileged.
    pub fn skip_spaces(&mut self, address: u32) -> Result<(u8, u32), Fail> {
        self.privileged().skip_spaces(address)
    }

    /// [`Reach::write_byte`], privileged.
    pub fn write_byte(&mut self, address: u32, value: u8) -> Result<(), Fail> {
        self.privileged().write_byte(address, value)
    }

    /// [`Reach::read_coordinates`], privileged.
    pub fn read_coordinates(&mut self, address: u32) -> Result<(i32, i32), Fail> {
        self.privileged().read_coordinates(address)
    }

    /// [`Reach::write_coordinates`], privileged.
    pub fn write_coordinates(&mut self, address: u32, point: (i32, i32)) -> Result<(), Fail> {
        self.privileged().write_coordinates(address, point)
    }

    /// [`Reach::read_word`], privileged.
    pub fn read_word(&mut self, address: u32) -> Result<u32, Fail> {
        self.privileged().read_word(address)
    }

    /// [`Reach::write_word`], privileged.
    pub fn write_word(&mut self, address: u32, value: u32) -> Result<(), Fail> {
        self.privileged().write_word(address, value)
    }

    /// [`Reach::read_words`], privileged.
    pub fn read_words(&mut self, address: u32, out: &mut [u32]) -> Result<(), Fail> {
        self.privileged().read_words(address, out)
    }

    /// [`Reach::write_words`], privileged.
    pub fn write_words(&mut self, address: u32, words: &[u32]) -> Result<(), Fail> {
        self.privileged().write_words(address, words)
    }

    /// [`Reach::peek_byte`], privileged: for the kernel's reading of what the
    /// guest may have overwritten.
    pub(crate) fn peek_byte(&mut self, address: u32) -> Option<u8> {
        self.privileged().peek_byte(address)
    }

    /// [`Reach::peek_word`], privileged.
    pub(crate) fn peek_word(&mut self, address: u32) -> Option<u32> {
        self.privileged().peek_word(address)
    }

    /// [`Reach::poke_word`], privileged.
    pub(crate) fn poke_word(&mut self, address: u32, value: u32) -> Option<()> {
        self.privileged().poke_word(address, value)
    }

    /// [`Reach::poke_byte`], privileged: for the kernel's own memory.
    pub(crate) fn poke_byte(&mut self, address: u32, value: u8) -> Option<()> {
        self.privileged().poke_byte(address, value)
    }

    /// [`Reach::read_until`], privileged.
    pub fn read_until(
        &mut self,
        address: u32,
        most: usize,
        ends: impl FnMut(u8) -> bool,
    ) -> Result<(Vec<u8>, bool), Fail> {
        self.privileged().read_until(address, most, ends)
    }

    /// [`Reach::position`], privileged.
    pub fn position(
        &mut self,
        address: u32,
        most: u32,
        ends: impl FnMut(u8) -> bool,
    ) -> Result<Option<u32>, Fail> {
        self.privileged().position(address, most, ends)
    }

    /// [`Reach::read_ended`], privileged.
    pub(crate) fn read_ended(
        &mut self,
        address: u32,
        most: usize,
        ends: impl FnMut(u8) -> bool,
    ) -> Result<Vec<u8>, Fail> {
        self.privileged().read_ended(address, most, ends)
    }

    /// [`Reach::read_text`], privileged.
    pub fn read_text(&mut self, address: u32, most: usize) -> Result<Vec<u8>, Fail> {
        self.privileged().read_text(address, most)
    }

    /// [`Reach::read_zstring`], privileged.
    pub fn read_zstring(&mut self, address: u32, most: usize) -> Result<Vec<u8>, Fail> {
        self.privileged().read_zstring(address, most)
    }

    /// [`Reach::copy_bytes`], privileged.
    pub fn copy_bytes(&mut self, from: u32, to: u32, bytes: u32) -> Result<(), Fail> {
        self.privileged().copy_bytes(from, to, bytes)
    }

    /// [`Reach::fill`], privileged.
    pub fn fill(&mut self, at: u32, bytes: u32, value: u8) -> Result<(), Fail> {
        self.privileged().fill(at, bytes, value)
    }

    /// [`Reach::write_terminated`], privileged.
    pub fn write_terminated(
        &mut self,
        address: u32,
        text: &[u8],
        end: u8,
        room: usize,
    ) -> Result<(), Fail> {
        self.privileged().write_terminated(address, text, end, room)
    }
}
