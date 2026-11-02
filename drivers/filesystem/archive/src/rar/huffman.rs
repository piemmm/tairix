//! Canonical prefix codes, as every RAR version builds them from code
//! lengths: the shortest code all zeros, codes of a length assigned in
//! symbol order.

use tairix_abi::DriverError;

use super::bits::Reader;

/// The longest code any table holds.
const MAX_LEN: usize = 15;
/// Bits the lookup table resolves at once.
const QUICK: usize = 10;
/// The most symbols a table holds: RAR 5's main table.
pub(super) const MAX_SYMBOLS: usize = 306;

/// A prefix code, decoded through a table for codes of up to [`QUICK`] bits
/// and by length for the rest.
pub(super) struct Code {
    /// Each [`QUICK`]-bit prefix's symbol and length, where a code that
    /// short is the prefix; zero where none is.
    quick: [u16; 1 << QUICK],
    /// Each length's codes' limit, left-justified to [`MAX_LEN`] bits.
    limit: [u32; MAX_LEN + 1],
    /// Each length's first code, and that code's index in `symbols`.
    first: [u32; MAX_LEN + 1],
    offset: [u32; MAX_LEN + 1],
    /// The coded symbols, by length then symbol.
    symbols: [u16; MAX_SYMBOLS],
}

impl Code {
    /// A code no symbol has: every decode through it is refused.
    pub(super) const fn empty() -> Self {
        Self {
            quick: [0; 1 << QUICK],
            limit: [0; MAX_LEN + 1],
            first: [0; MAX_LEN + 1],
            offset: [0; MAX_LEN + 1],
            symbols: [0; MAX_SYMBOLS],
        }
    }

    /// Rebuild the code from `lengths`, one a symbol, zero for a symbol
    /// unused. A code that leaves some bit strings unassigned is accepted,
    /// and refuses only a string it is actually given.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a length past fifteen, more symbols
    /// than a table holds, or lengths no prefix code has.
    pub(super) fn build(&mut self, lengths: &[u8]) -> Result<(), DriverError> {
        if lengths.len() > MAX_SYMBOLS {
            return Err(DriverError::MediumError);
        }
        let mut count = [0u32; MAX_LEN + 1];
        for &len in lengths {
            *count
                .get_mut(usize::from(len))
                .ok_or(DriverError::MediumError)? += 1;
        }
        count[0] = 0;
        let kraft: u32 = (1..=MAX_LEN).map(|len| count[len] << (MAX_LEN - len)).sum();
        if kraft > 1 << MAX_LEN {
            return Err(DriverError::MediumError);
        }
        let mut code = 0u32;
        let mut index = 0u32;
        for (len, &n) in count.iter().enumerate().skip(1) {
            self.first[len] = code;
            self.offset[len] = index;
            code += n;
            index += n;
            self.limit[len] = code << (MAX_LEN - len);
            code <<= 1;
        }
        let mut next = self.offset;
        for (symbol, &len) in (0u16..).zip(lengths) {
            if len != 0 {
                let at = &mut next[usize::from(len)];
                *self
                    .symbols
                    .get_mut(slot(*at))
                    .ok_or(DriverError::MediumError)? = symbol;
                *at += 1;
            }
        }
        self.quick.fill(0);
        for (len, &n) in count.iter().enumerate().take(QUICK + 1).skip(1) {
            let span = 1usize << (QUICK - len);
            for k in 0..n {
                let entry = self.symbols[slot(self.offset[len] + k)] << 4 | narrow(len);
                let start = slot(self.first[len] + k) << (QUICK - len);
                self.quick
                    .get_mut(start..start + span)
                    .ok_or(DriverError::MediumError)?
                    .fill(entry);
            }
        }
        Ok(())
    }

    /// The next symbol `br` holds.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a bit string no code is, or one the
    /// data ends inside.
    pub(super) fn decode(&self, br: &mut Reader<'_>) -> Result<u16, DriverError> {
        let (symbol, len) = self
            .resolve(br.peek(narrow32(MAX_LEN)))
            .ok_or(DriverError::MediumError)?;
        br.skip(len)?;
        Ok(symbol)
    }

    /// The symbol whose code the fifteen bits `bits` begin with, and the
    /// code's length; `None` where no code is.
    pub(super) fn resolve(&self, bits: u32) -> Option<(u16, u32)> {
        let entry = self.quick[slot(bits >> (MAX_LEN - QUICK))];
        if entry != 0 {
            return Some((entry >> 4, u32::from(entry & 15)));
        }
        for len in QUICK + 1..=MAX_LEN {
            if bits < self.limit[len] {
                let index =
                    (self.offset[len] + (bits >> (MAX_LEN - len))).wrapping_sub(self.first[len]);
                return self
                    .symbols
                    .get(slot(index))
                    .map(|&symbol| (symbol, narrow32(len)));
            }
        }
        None
    }
}

/// `value` as an index.
fn slot(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A code length, at most fifteen, as a table entry's low bits.
fn narrow(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(0)
}

/// A code length, at most fifteen, as a bit count.
fn narrow32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(0)
}
