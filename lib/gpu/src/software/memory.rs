//! The software device's memory: an allocation's little-endian words.

use alloc::vec::Vec;
use core::fmt;

use crate::fair::{Fair, FairGuard};
use crate::memory::Charge;
use crate::Error;

/// One allocation's memory on the software device: little-endian words,
/// zeroed when made, and the charge they hold until the last holder lets
/// them go.
///
/// The words have their own lock, so the CPU's reads and writes and the work
/// running over them never touch them at once, and neither takes the
/// device's lock to do it. Lockers have the words in turn, in the order they
/// asked, so neither a CPU thread locking again at once nor the work can keep
/// the other out. Work holds the lock for a whole command, and shares the
/// words between the cores it runs on as atomic views.
pub(crate) struct Memory {
    words: Fair<Vec<u32>>,
    charge: Charge,
    _overhead: Charge,
}

/// A memory's words, held in its locker's turn.
pub(crate) type Words<'m> = FairGuard<'m, Vec<u32>>;

/// Freeing the words takes as long as they are large, so no fair lock is held
/// as they go.
#[cfg(test)]
impl Drop for Memory {
    fn drop(&mut self) {
        crate::fair::held::note_drop();
    }
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("bytes", &self.charge.bytes())
            .finish_non_exhaustive()
    }
}

impl Memory {
    /// The host bytes an allocation of `len` keeps besides those it was
    /// asked for: its words rounded up whole, and the memory itself as its
    /// holders share it.
    pub(crate) const fn overhead(len: u64) -> u64 {
        let rounding = (4 - len % 4) % 4;
        rounding + crate::model::shared_bytes::<Self>()
    }

    /// `len` zeroed bytes under `charge`, held as whole words, and what they
    /// keep besides under `overhead`.
    pub(crate) fn zeroed(len: u64, charge: Charge, overhead: Charge) -> Result<Self, Error> {
        let words = usize::try_from(len.div_ceil(4)).map_err(|_| Error::OutOfMemory)?;
        let words = tairix_util::fallible::filled(words, 0u32).ok_or(Error::OutOfMemory)?;
        Ok(Self {
            words: Fair::new(words),
            charge,
            _overhead: overhead,
        })
    }

    /// The words, once every locker that asked before has had them.
    pub(crate) fn lock(&self) -> Words<'_> {
        self.words.lock()
    }

    /// Lock `a` and `b`, which are different memories, in one order every
    /// caller shares, so two lockers of the same pair never wait on each
    /// other.
    pub(crate) fn lock_pair<'m>(a: &'m Self, b: &'m Self) -> (Words<'m>, Words<'m>) {
        if core::ptr::from_ref(a) < core::ptr::from_ref(b) {
            let first = a.lock();
            (first, b.lock())
        } else {
            let second = b.lock();
            (a.lock(), second)
        }
    }
}

/// Copy `out.len()` bytes from byte `offset` of `words` into `out`; `None`
/// past the words.
pub(crate) fn read_bytes(words: &[u32], offset: usize, out: &mut [u8]) -> Option<()> {
    let end = offset.checked_add(out.len())?;
    if end > words.len().checked_mul(4)? {
        return None;
    }
    let lead = (4 - offset % 4) % 4;
    let (head, rest) = out.split_at_mut(lead.min(out.len()));
    if let Some(word) = words.get(offset / 4) {
        let within = offset % 4;
        head.copy_from_slice(word.to_le_bytes().get(within..within + head.len())?);
    }
    let first = (offset + head.len()) / 4;
    let (middle, tail) = rest.as_chunks_mut::<4>();
    let whole = words.get(first..first + middle.len())?;
    for (bytes, word) in middle.iter_mut().zip(whole) {
        *bytes = word.to_le_bytes();
    }
    if let Some(word) = words.get(first + middle.len()) {
        tail.copy_from_slice(word.to_le_bytes().get(..tail.len())?);
    }
    Some(())
}

/// Copy `data` into `words` from byte `offset`; `None`, changing nothing,
/// past the words.
pub(crate) fn write_bytes(words: &mut [u32], offset: usize, data: &[u8]) -> Option<()> {
    let end = offset.checked_add(data.len())?;
    if end > words.len().checked_mul(4)? {
        return None;
    }
    let lead = (4 - offset % 4) % 4;
    let (head, rest) = data.split_at(lead.min(data.len()));
    patch(words, offset, head)?;
    let first = (offset + head.len()) / 4;
    let (middle, tail) = rest.as_chunks::<4>();
    let whole = words.get_mut(first..first + middle.len())?;
    for (word, bytes) in whole.iter_mut().zip(middle) {
        *word = u32::from_le_bytes(*bytes);
    }
    patch(words, (first + middle.len()) * 4, tail)
}

/// Write `bytes`, which lie within one word, at byte `offset` of `words`.
fn patch(words: &mut [u32], offset: usize, bytes: &[u8]) -> Option<()> {
    if bytes.is_empty() {
        return Some(());
    }
    let word = words.get_mut(offset / 4)?;
    let within = offset % 4;
    let mut current = word.to_le_bytes();
    current
        .get_mut(within..within + bytes.len())?
        .copy_from_slice(bytes);
    *word = u32::from_le_bytes(current);
    Some(())
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
