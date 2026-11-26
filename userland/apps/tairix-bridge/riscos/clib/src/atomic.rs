//! The atomic operations, chunk 6, as RISC OS 5.30's library gives them for
//! objects of one, two, four and eight bytes: store, load, exchange,
//! compare-and-exchange, and fetch-and-add, -xor, -or and -and, then a
//! fence and the atomic flag's test-and-set and clear.
//!
//! Each takes the object's address in R0 and a memory order in R1, which
//! changes nothing; a value follows in R2, or R2 and R3 for eight bytes.
//! Compare-and-exchange takes the expected value's address in R2 and the
//! desired value in R3, its high word on the stack for eight bytes. An
//! object's bytes are little-endian, as the processor's. A routine runs
//! whole before the client goes on, so each is atomic as RISC OS 5.30's is
//! with interrupts off.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::chunk::{own, Entry};
use crate::client::stacked;

/// Chunk 6's entries, in RISC OS 5.30's order; a weak compare-and-exchange
/// is the strong one's routine.
pub(crate) const ENTRIES: [Entry; 40] = {
    let mut entries = own([
        lock_free,
        store::<1>,
        store::<2>,
        store::<4>,
        store::<8>,
        load::<1>,
        load::<2>,
        load::<4>,
        load::<8>,
        exchange::<1>,
        exchange::<2>,
        exchange::<4>,
        exchange::<8>,
        compare_exchange::<1>,
        compare_exchange::<2>,
        compare_exchange::<4>,
        compare_exchange::<8>,
        compare_exchange::<1>,
        compare_exchange::<2>,
        compare_exchange::<4>,
        compare_exchange::<8>,
        fetch::<1, ADD>,
        fetch::<2, ADD>,
        fetch::<4, ADD>,
        fetch::<8, ADD>,
        fetch::<1, XOR>,
        fetch::<2, XOR>,
        fetch::<4, XOR>,
        fetch::<8, XOR>,
        fetch::<1, OR>,
        fetch::<2, OR>,
        fetch::<4, OR>,
        fetch::<8, OR>,
        fetch::<1, AND>,
        fetch::<2, AND>,
        fetch::<4, AND>,
        fetch::<8, AND>,
        fence,
        test_and_set,
        clear,
    ]);
    let mut n = 0;
    while n < 4 {
        entries[WEAK + n] = Entry::Shared {
            chunk: 6,
            entry: STRONG + n,
        };
        n += 1;
    }
    entries
};

/// Where the strong and the weak compare-and-exchange start.
const STRONG: usize = 13;
const WEAK: usize = 17;

/// The object of `SIZE` bytes at `at`.
fn read<const SIZE: usize>(os: &mut Os<'_>, at: u32) -> Result<u64, Fail> {
    let mut bytes = [0; 8];
    os.as_caller().read_bytes(at, &mut bytes[..SIZE])?;
    Ok(u64::from_le_bytes(bytes))
}

/// The object of `SIZE` bytes at `at` made `value`'s low bytes.
fn write<const SIZE: usize>(os: &mut Os<'_>, at: u32, value: u64) -> Result<(), Fail> {
    os.as_caller().write_bytes(at, &value.to_le_bytes()[..SIZE])
}

/// The value R`n` holds, with R`n + 1` above it for eight bytes.
fn value<const SIZE: usize>(regs: &Regs, n: usize) -> u64 {
    let low = u64::from(regs.r[n]);
    if SIZE == 8 {
        u64::from(regs.r[n + 1]) << 32 | low
    } else {
        low
    }
}

/// `value` answered in R0, and R1 for eight bytes.
fn answer<const SIZE: usize>(regs: &mut Regs, value: u64) {
    let bytes = value.to_le_bytes();
    regs.r[0] = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if SIZE == 8 {
        regs.r[1] = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    }
}

/// `atomic_is_lock_free`: nought, whatever it is asked.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn lock_free(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = 0;
    Ok(())
}

/// Store: the object made the value.
fn store<const SIZE: usize>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    write::<SIZE>(os, regs.r[0], value::<SIZE>(regs, 2))
}

/// Load: the object's value.
fn load<const SIZE: usize>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let value = read::<SIZE>(os, regs.r[0])?;
    answer::<SIZE>(regs, value);
    Ok(())
}

/// Exchange: the object made the value, its old one answered.
fn exchange<const SIZE: usize>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0];
    let old = read::<SIZE>(os, at)?;
    write::<SIZE>(os, at, value::<SIZE>(regs, 2))?;
    answer::<SIZE>(regs, old);
    Ok(())
}

/// Compare-and-exchange: where the object holds the value expected it is
/// made the one desired, answering one; else the value expected is made the
/// object's, answering nought.
fn compare_exchange<const SIZE: usize>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (at, expected_at) = (regs.r[0], regs.r[2]);
    let desired = if SIZE == 8 {
        let [high] = stacked(os)?;
        u64::from(high) << 32 | u64::from(regs.r[3])
    } else {
        u64::from(regs.r[3])
    };
    let current = read::<SIZE>(os, at)?;
    let matched = current == read::<SIZE>(os, expected_at)?;
    if matched {
        write::<SIZE>(os, at, desired)?;
    } else {
        write::<SIZE>(os, expected_at, current)?;
    }
    regs.r[0] = u32::from(matched);
    Ok(())
}

/// What a fetch does to the object with the value.
const ADD: u8 = 0;
const XOR: u8 = 1;
const OR: u8 = 2;
const AND: u8 = 3;

/// Fetch-and-`OP`: the object made its value `OP` the value given, the old
/// one answered.
fn fetch<const SIZE: usize, const OP: u8>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0];
    let (old, operand) = (read::<SIZE>(os, at)?, value::<SIZE>(regs, 2));
    let new = match OP {
        ADD => old.wrapping_add(operand),
        XOR => old ^ operand,
        OR => old | operand,
        _ => old & operand,
    };
    write::<SIZE>(os, at, new)?;
    answer::<SIZE>(regs, old);
    Ok(())
}

/// A fence: nothing to wait for where one routine runs at a time.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn fence(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// `atomic_flag_test_and_set`: the flag's word made one, its old word
/// answered.
fn test_and_set(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0];
    let old = read::<4>(os, at)?;
    write::<4>(os, at, 1)?;
    answer::<4>(regs, old);
    Ok(())
}

/// `atomic_flag_clear`: the flag's word made nought.
fn clear(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    write::<4>(os, regs.r[0], 0)
}

#[cfg(test)]
mod tests;
