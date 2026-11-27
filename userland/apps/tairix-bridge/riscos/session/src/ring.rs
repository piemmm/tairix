//! The screens' damage ring: the spawn share both sides map. The worker
//! writes each damage record's pixels into it and names the record in an
//! [`Event::Damage`](crate::protocol::Event::Damage); the front end copies
//! them out and gives their space back, in the order they were written.
//!
//! A record is contiguous. One that would run past the ring's end starts
//! again at its start, and the bytes it passed over are given back with it.
//! Each side keeps a [`Ring`] and both move it alike, so the front end
//! believes a record only where the worker could have put it.

/// The ring's bytes.
pub const RING_BYTES: u32 = 8 << 20;
/// The most bytes a record holds: a run of damaged lines longer is sent as
/// several.
pub const RECORD_MOST: u32 = RING_BYTES / 4;

/// Where a record went: its offset, and the bytes before the ring's end it
/// passed over to start again at its start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placed {
    /// Its offset.
    pub offset: u32,
    /// The bytes passed over.
    pub skipped: u32,
    /// Its bytes.
    pub bytes: u32,
}

impl Placed {
    /// What its release gives back.
    #[must_use]
    pub const fn released(&self) -> u32 {
        self.skipped + self.bytes
    }
}

/// Why the ring would not move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RingError {
    /// A record not where the next one goes, empty, or larger than a
    /// record may be.
    Misplaced,
    /// More given back than is held.
    Overreleased,
}

/// One side's view of the ring: where the next record goes, and the bytes
/// written and not yet given back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ring {
    head: u32,
    held: u32,
}

impl Ring {
    /// An empty ring.
    #[must_use]
    pub const fn new() -> Self {
        Self { head: 0, held: 0 }
    }

    /// The bytes held.
    #[must_use]
    pub const fn held(&self) -> u32 {
        self.held
    }

    /// Where a record of `bytes` goes, from one byte to [`RECORD_MOST`]: none
    /// while the room it needs is held.
    #[must_use]
    pub const fn place(&self, bytes: u32) -> Option<Placed> {
        if bytes == 0 || bytes > RECORD_MOST {
            return None;
        }
        let (offset, skipped) = if self.head + bytes <= RING_BYTES {
            (self.head, 0)
        } else {
            (0, RING_BYTES - self.head)
        };
        if self.held + skipped + bytes > RING_BYTES {
            return None;
        }
        Some(Placed {
            offset,
            skipped,
            bytes,
        })
    }

    /// A record of `bytes` the other side says it put at `offset`: where it
    /// went, held.
    ///
    /// # Errors
    ///
    /// [`RingError::Misplaced`] where that is not where the next record of
    /// that size goes, nothing moved.
    pub fn accept(&mut self, offset: u32, bytes: u32) -> Result<Placed, RingError> {
        match self.place(bytes) {
            Some(placed) if placed.offset == offset => {
                self.take(placed);
                Ok(placed)
            }
            _ => Err(RingError::Misplaced),
        }
    }

    /// `placed`, which [`Self::place`] gave, held.
    pub const fn take(&mut self, placed: Placed) {
        self.head = (placed.offset + placed.bytes) % RING_BYTES;
        self.held += placed.released();
    }

    /// `bytes` of the oldest records given back.
    ///
    /// # Errors
    ///
    /// [`RingError::Overreleased`] for more than is held, nothing moved.
    pub const fn release(&mut self, bytes: u32) -> Result<(), RingError> {
        if bytes > self.held {
            return Err(RingError::Overreleased);
        }
        self.held -= bytes;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
