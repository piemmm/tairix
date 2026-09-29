//! 32-bit ADMA2 descriptor tables.
//!
//! The SDHCI ADMA2 engine walks a table of 64-bit descriptors in host
//! memory (SD Host Controller Simplified Specification v3.00 §1.13). Each
//! 32-bit-address descriptor is laid out little-endian as:
//!
//! ```text
//! bits [63:32]  Address  — 32-bit data-buffer base
//! bits [31:16]  Length   — byte length (the value 0 means 65536)
//! bits  [5:4]   Act      — 0b10 = Tran (move Length bytes to/from Address)
//! bit    2      Int      — raise the DMA interrupt at this descriptor
//! bit    1      End      — last descriptor of the table
//! bit    0      Valid    — the descriptor is valid
//! ```
//!
//! A transfer moves through one physically contiguous staging area, so its
//! table is a run of `Tran` descriptors over consecutive 64 KiB spans, the
//! last marked `End`.

/// Serialised size of one 32-bit ADMA2 descriptor, in bytes.
pub const DESC_BYTES: usize = 8;

/// Largest byte length one 32-bit ADMA2 descriptor can carry: the 16-bit
/// `Length` field, where the encoded value `0` denotes the maximum. A
/// format-fixed bound (the descriptor layout), not a scalable capacity.
pub const MAX_DESC_BYTES: usize = 1 << 16;

/// `Valid`: the descriptor is valid (attribute bit 0).
const ATTR_VALID: u16 = 1 << 0;
/// `End`: the last descriptor of the table (attribute bit 1).
const ATTR_END: u16 = 1 << 1;
/// `Act = Tran` (attribute bits `[5:4]` = `0b10`): move `Length` bytes
/// between the card and the descriptor's `Address`.
const ATTR_ACT_TRAN: u16 = 0b10 << 4;

/// How many descriptors a transfer of `len` bytes takes.
#[must_use]
pub const fn descriptors_for(len: usize) -> usize {
    len.div_ceil(MAX_DESC_BYTES)
}

/// Encode the table moving `len` bytes at device address `addr` into
/// `table`, returning the bytes of it the descriptors fill.
///
/// `None` — nothing encoded that the controller could reach — when `len` is
/// zero, `table` is too short for [`descriptors_for`]`(len)` descriptors, or
/// the span runs past the 32-bit address field.
#[must_use]
pub fn encode_table(addr: u32, len: usize, table: &mut [u8]) -> Option<usize> {
    let count = descriptors_for(len);
    let used = count.checked_mul(DESC_BYTES)?;
    if len == 0 || used > table.len() {
        return None;
    }
    let last = u64::from(addr).checked_add(u64::try_from(len).ok()?)?;
    if last > 1 << 32 {
        return None;
    }
    let mut next = addr;
    for (index, slot) in table[..used]
        .as_chunks_mut::<DESC_BYTES>()
        .0
        .iter_mut()
        .enumerate()
    {
        let span = (len - index * MAX_DESC_BYTES).min(MAX_DESC_BYTES);
        let end = if index + 1 == count { ATTR_END } else { 0 };
        encode(slot, next, span, end);
        // The final span may end exactly at the top of the address space,
        // where no further descriptor is taken.
        next = next.wrapping_add(u32::try_from(span).ok()?);
    }
    Some(used)
}

/// Write one `Valid | Tran` descriptor (plus `end`) for `len` bytes at `addr`.
///
/// `len` is `1..=`[`MAX_DESC_BYTES`], and 65536 is encoded as the field's
/// `0`, exactly as the controller decodes it.
fn encode(slot: &mut [u8; DESC_BYTES], addr: u32, len: usize, end: u16) {
    // The mask leaves `0..=0xFFFF`, so the conversion never truncates; the
    // `0` fallback is unreachable and only keeps the path panic-free.
    let length_field = u16::try_from(len & (MAX_DESC_BYTES - 1)).unwrap_or(0);
    let attr = ATTR_VALID | ATTR_ACT_TRAN | end;
    slot[0..2].copy_from_slice(&attr.to_le_bytes());
    slot[2..4].copy_from_slice(&length_field.to_le_bytes());
    slot[4..8].copy_from_slice(&addr.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(desc: &[u8]) -> (u16, u16, u32) {
        (
            u16::from_le_bytes([desc[0], desc[1]]),
            u16::from_le_bytes([desc[2], desc[3]]),
            u32::from_le_bytes([desc[4], desc[5], desc[6], desc[7]]),
        )
    }

    #[test]
    fn one_block_is_one_terminating_tran_descriptor() {
        let mut table = [0u8; 4 * DESC_BYTES];
        assert_eq!(encode_table(0x1234_0000, 512, &mut table), Some(DESC_BYTES));
        let (attr, len, addr) = fields(&table);
        assert_eq!(attr, ATTR_VALID | ATTR_END | ATTR_ACT_TRAN);
        assert_eq!((len, addr), (512, 0x1234_0000));
    }

    #[test]
    fn a_long_transfer_spans_consecutive_64k_descriptors_ending_once() {
        let mut table = [0xAAu8; 8 * DESC_BYTES];
        let len = 3 * MAX_DESC_BYTES + 4096;
        assert_eq!(
            encode_table(0xC000_0000, len, &mut table),
            Some(4 * DESC_BYTES)
        );
        let descs: [(u16, u16, u32); 4] =
            core::array::from_fn(|i| fields(&table[i * DESC_BYTES..(i + 1) * DESC_BYTES]));
        let body = ATTR_VALID | ATTR_ACT_TRAN;
        assert_eq!(
            descs,
            [
                (body, 0, 0xC000_0000),
                (body, 0, 0xC001_0000),
                (body, 0, 0xC002_0000),
                (body | ATTR_END, 4096, 0xC003_0000),
            ],
            "65536 is encoded as a zero length; only the last ends the table"
        );
        assert!(
            table[4 * DESC_BYTES..].iter().all(|&b| b == 0xAA),
            "nothing past the table"
        );
    }

    #[test]
    fn an_exact_multiple_of_64k_ends_on_its_last_full_descriptor() {
        let mut table = [0u8; 2 * DESC_BYTES];
        assert_eq!(
            encode_table(0, 2 * MAX_DESC_BYTES, &mut table),
            Some(2 * DESC_BYTES)
        );
        assert_eq!(fields(&table[DESC_BYTES..]).0 & ATTR_END, ATTR_END);
        assert_eq!(fields(&table).0 & ATTR_END, 0);
    }

    #[test]
    fn a_table_or_address_space_too_small_encodes_nothing() {
        let mut table = [0u8; DESC_BYTES];
        assert_eq!(encode_table(0, MAX_DESC_BYTES + 1, &mut table), None);
        assert_eq!(encode_table(0, 0, &mut table), None);
        assert_eq!(encode_table(0xFFFF_F000, 0x2000, &mut table), None);
        assert_eq!(table, [0; DESC_BYTES], "a refusal writes nothing");
        assert_eq!(
            encode_table(0xFFFF_F000, 0x1000, &mut table),
            Some(DESC_BYTES)
        );
    }
}
