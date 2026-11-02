//! The bit reader a member's packed data is read through, most significant
//! bit first in every RAR version.

use tairix_abi::DriverError;
use tairix_fsserve::VolumeAccess;

use crate::region::ByteInput;

/// Where a member's packed bytes come from, a buffer at a time.
pub(super) trait Source {
    /// The bytes available, reading more when none are: empty at the end.
    fn chunk(&mut self) -> &[u8];
    /// Consume `n` of the bytes [`Self::chunk`] answered.
    fn consume(&mut self, n: usize);
}

impl<A: VolumeAccess> Source for ByteInput<'_, '_, A> {
    fn chunk(&mut self) -> &[u8] {
        ByteInput::chunk(self)
    }

    fn consume(&mut self, n: usize) {
        ByteInput::consume(self, n);
    }
}

/// The most zero bytes PPMd's range coder may take past the data's end: its
/// coder looks a few bytes ahead of the symbols it has decoded.
const PAST_END_MAX: u32 = 8;

/// Where a member's packed bits stand between reads.
#[derive(Default)]
pub(super) struct Bits {
    /// The next bits, most significant first.
    hold: u64,
    /// How many of `hold`'s bits are the data's.
    count: u32,
    /// Bits taken from the data so far.
    taken: u64,
    /// Zero bytes the range coder has taken past the data's end.
    past_end: u32,
}

/// A member's bits and the input they are pulled from, for one call.
pub(super) struct Reader<'a> {
    bits: &'a mut Bits,
    input: &'a mut dyn Source,
}

impl<'a> Reader<'a> {
    pub(super) fn new(bits: &'a mut Bits, input: &'a mut dyn Source) -> Self {
        Self { bits, input }
    }

    fn refill(&mut self) {
        let count = self.bits.count;
        let room = (64 - count) / 8;
        let chunk = self.input.chunk();
        if let Some(word) = chunk.first_chunk::<8>() {
            // As many whole bytes as the held bits have room for, at once.
            let bits = room * 8;
            let word = u64::from_be_bytes(*word);
            self.bits.hold |= word.checked_shr(64 - bits).unwrap_or(0) << (64 - bits - count);
            self.bits.count += bits;
            self.input.consume(usize::try_from(room).unwrap_or(0));
            return;
        }
        let take = chunk.len().min(usize::try_from(room).unwrap_or(0));
        for &byte in &chunk[..take] {
            self.bits.hold |= u64::from(byte) << (56 - self.bits.count);
            self.bits.count += 8;
        }
        self.input.consume(take);
        if take > 0 && self.bits.count <= 56 {
            self.refill();
        }
    }

    /// The next `n` bits, 1 to 32, without taking them: zeros past the end.
    pub(super) fn peek(&mut self, n: u32) -> u32 {
        if self.bits.count < n {
            self.refill();
        }
        low32(self.bits.hold >> (64 - n))
    }

    /// Take `n` bits, at most 32, of those [`Self::peek`] showed.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] past the data's end.
    pub(super) fn skip(&mut self, n: u32) -> Result<(), DriverError> {
        if n > self.bits.count {
            self.refill();
            if n > self.bits.count {
                return Err(DriverError::MediumError);
            }
        }
        self.bits.hold = self.bits.hold.checked_shl(n).unwrap_or(0);
        self.bits.count -= n;
        self.bits.taken += u64::from(n);
        Ok(())
    }

    /// The next `n` bits, 0 to 32.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] past the data's end.
    pub(super) fn bits(&mut self, n: u32) -> Result<u32, DriverError> {
        if n == 0 {
            return Ok(0);
        }
        let value = self.peek(n);
        self.skip(n)?;
        Ok(value)
    }

    /// The next bit.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] past the data's end.
    pub(super) fn bit(&mut self) -> Result<bool, DriverError> {
        Ok(self.bits(1)? == 1)
    }

    /// The next eight bits.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] past the data's end.
    pub(super) fn byte(&mut self) -> Result<u8, DriverError> {
        Ok(self.bits(8)?.to_le_bytes()[0])
    }

    /// Step to the next whole byte of the data.
    pub(super) fn align(&mut self) {
        // The held bits always end on a byte of the data, so they hold the
        // rest of the current one.
        let partial = u32::from(self.bits.taken.to_le_bytes()[0] & 7);
        if partial != 0 {
            let drop = 8 - partial;
            self.bits.hold <<= drop;
            self.bits.count -= drop;
            self.bits.taken += u64::from(drop);
        }
    }

    /// Bits taken from the data so far.
    pub(super) const fn taken(&self) -> u64 {
        self.bits.taken
    }

    /// The next byte for PPMd's range coder: zero for a few past the end.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a coder reading on past those.
    pub(super) fn coder_byte(&mut self) -> Result<u8, DriverError> {
        if self.bits.count < 8 {
            self.refill();
        }
        if self.bits.count >= 8 {
            return self.byte();
        }
        self.bits.past_end += 1;
        if self.bits.past_end > PAST_END_MAX {
            return Err(DriverError::MediumError);
        }
        Ok(0)
    }
}

/// The low 32 bits of `value`.
pub(super) const fn low32(value: u64) -> u32 {
    let [b0, b1, b2, b3, ..] = value.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}
