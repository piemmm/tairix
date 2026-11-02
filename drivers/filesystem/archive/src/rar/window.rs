//! The decoded history a match copies from: a power-of-two ring holding at
//! least as far back as a match may reach, and the bytes decoded but not
//! yet given out.
//!
//! A ring grows with what is decoded into it, doubling up to the size its
//! stream needs, because a match never reaches further back than the bytes
//! produced so far: memory follows the output a stream really has, never
//! the dictionary or length its header claims.

use alloc::vec::Vec;

use tairix_abi::DriverError;

/// Room every window keeps past the bytes owed out: more than any one
/// symbol of any version decodes, so decoding one never overwrites them.
pub(super) const MATCH_ROOM: u64 = 0x2000;

/// The least window: decoding a few bytes needs no less.
const MIN_SIZE: u64 = 0x4000;

/// The most a ring starts at: a member decoding more grows it, its copying
/// paid off against what it decodes.
const FIRST_MAX: u64 = 1 << 20;

/// A stream's decoded history.
pub(super) struct Window {
    buf: Vec<u8>,
    mask: u64,
    /// The size the ring grows to; until it is reached, nothing wraps.
    cap: u64,
    /// Bytes decoded since the stream began.
    pos: u64,
    /// The furthest back a match may reach.
    reach: u64,
}

impl Window {
    /// The window for a stream whose matches reach at most `reach` back and
    /// which decodes `total` bytes in all: growing to no larger than
    /// whichever of the two binds.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] where its first ring cannot be had.
    pub(super) fn new(reach: u64, total: u64) -> Result<Self, DriverError> {
        let need = reach.min(total.saturating_add(MATCH_ROOM)).max(MIN_SIZE);
        let cap = need
            .checked_next_power_of_two()
            .ok_or(DriverError::OutOfMemory)?;
        let first = cap.min(FIRST_MAX);
        let buf = ring(&[], first)?;
        Ok(Self {
            buf,
            mask: first - 1,
            cap,
            pos: 0,
            reach,
        })
    }

    /// Bytes decoded since the stream began.
    pub(super) const fn pos(&self) -> u64 {
        self.pos
    }

    /// How far a decoder runs ahead of the bytes owed out before giving
    /// them: a quarter of the window, so what it decodes always fits.
    pub(super) const fn stretch(&self) -> u64 {
        self.cap / 4
    }

    /// How far a decoder runs ahead of the bytes owed out to give a reader
    /// with room for `room` more: that far, and never past a stretch.
    pub(super) const fn ahead(&self, room: u64) -> u64 {
        if room < self.stretch() {
            room
        } else {
            self.stretch()
        }
    }

    const fn size(&self) -> u64 {
        self.mask + 1
    }

    fn at(&self, pos: u64) -> usize {
        // Masked below the buffer's length, which is a `usize`.
        usize::try_from(pos & self.mask).unwrap_or(0)
    }

    /// Decode `byte`.
    pub(super) fn literal(&mut self, byte: u8) {
        let at = self.at(self.pos);
        self.buf[at] = byte;
        self.pos += 1;
    }

    /// Decode `len` bytes copied from `dist` back.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a distance of nothing, or one
    /// reaching before the stream or further than its dictionary.
    pub(super) fn copy(&mut self, dist: u64, len: u32) -> Result<(), DriverError> {
        if dist == 0 || dist > self.pos || dist > self.reach || dist > self.size() {
            return Err(DriverError::MediumError);
        }
        let len = u64::from(len);
        let (from, to) = (self.at(self.pos - dist), self.at(self.pos));
        let run = usize::try_from(len).unwrap_or(usize::MAX);
        if dist >= len && from.max(to).saturating_add(run) <= self.buf.len() {
            self.buf.copy_within(from..from + run, to);
        } else {
            for i in 0..len {
                let byte = self.buf[self.at(self.pos + i - dist)];
                let at = self.at(self.pos + i);
                self.buf[at] = byte;
            }
        }
        self.pos += len;
        Ok(())
    }

    /// Make room for the next symbol, growing the ring where it may still
    /// grow; refuse where decoding it would overwrite bytes from `owed` on,
    /// which are decoded but not yet given out.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] where it would: a stream whose pending
    /// filters or output outgrow its window is out of shape;
    /// [`DriverError::OutOfMemory`] where a larger ring cannot be had.
    #[inline]
    pub(super) fn has_room(&mut self, owed: u64) -> Result<(), DriverError> {
        // With a symbol's room left before the ring's end, nothing owed can
        // be overwritten.
        if self.pos + MATCH_ROOM <= self.size() {
            return Ok(());
        }
        self.make_room(owed)
    }

    #[cold]
    fn make_room(&mut self, owed: u64) -> Result<(), DriverError> {
        let wanted = self.pos + MATCH_ROOM;
        if self.size() < self.cap {
            let size = wanted
                .checked_next_power_of_two()
                .ok_or(DriverError::OutOfMemory)?
                .min(self.cap);
            // Nothing has wrapped below the cap, so the bytes keep their places.
            let held = usize::try_from(self.pos)
                .ok()
                .filter(|&held| held <= self.buf.len())
                .ok_or(DriverError::MediumError)?;
            self.buf = ring(&self.buf[..held], size)?;
            self.mask = size - 1;
        }
        if self.pos.saturating_sub(owed) + MATCH_ROOM > self.size() {
            return Err(DriverError::MediumError);
        }
        Ok(())
    }

    /// Copy out the decoded bytes from `from`, as many as `out` holds.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for bytes not yet decoded, or decoded so
    /// long ago the window no longer holds them.
    pub(super) fn read(&self, from: u64, out: &mut [u8]) -> Result<(), DriverError> {
        let len = out.len() as u64;
        let end = from.checked_add(len).ok_or(DriverError::MediumError)?;
        if end > self.pos || self.pos - from > self.size() {
            return Err(DriverError::MediumError);
        }
        let start = self.at(from);
        let first = (self.buf.len() - start).min(out.len());
        out[..first].copy_from_slice(&self.buf[start..start + first]);
        let rest = out.len() - first;
        out[first..].copy_from_slice(&self.buf[..rest]);
        Ok(())
    }
}

/// A ring of `size` bytes holding `held` from its start, the rest zeros.
fn ring(held: &[u8], size: u64) -> Result<Vec<u8>, DriverError> {
    let len = usize::try_from(size).map_err(|_| DriverError::OutOfMemory)?;
    let mut ring = Vec::new();
    ring.try_reserve_exact(len)
        .map_err(|_| DriverError::OutOfMemory)?;
    ring.extend_from_slice(held);
    ring.resize(len, 0);
    Ok(ring)
}
