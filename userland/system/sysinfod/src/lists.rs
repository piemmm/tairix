//! Reading a list whole, from the kernel or from a peer service, refusing any
//! answer that is not a whole number of whole records.

use alloc::vec::Vec;

use tairix_abi::reply::decode_page_reply;
use tairix_abi::Errno;

/// Records the first read of a kernel list has room for: most lists fit, and
/// a longer one doubles from here.
const FIRST_READ_RECORDS: usize = 64;

/// Read a kernel list whole, its `record_len`-byte records back to back.
///
/// `read` fills a buffer with the list's records from its start and answers
/// how many bytes it wrote. A full buffer may have more behind it, so the
/// list is read again into one twice the size, never joined from two
/// readings a change could fall between.
///
/// # Errors
///
/// What `read` answers; [`Errno::BadMagic`] for an answer longer than its
/// buffer or ending part way through a record; [`Errno::OutOfMemory`] when
/// the buffer cannot grow; [`Errno::LengthOutOfRange`] for a `record_len`
/// of zero.
pub fn read_whole(
    record_len: usize,
    mut read: impl FnMut(&mut [u8]) -> Result<usize, Errno>,
) -> Result<Vec<u8>, Errno> {
    if record_len == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    let mut capacity = record_len
        .checked_mul(FIRST_READ_RECORDS)
        .ok_or(Errno::OutOfMemory)?;
    let mut list = Vec::new();
    loop {
        list.try_reserve_exact(capacity - list.len())
            .map_err(|_| Errno::OutOfMemory)?;
        list.resize(capacity, 0);
        let written = read(&mut list)?;
        if written > capacity || written % record_len != 0 {
            return Err(Errno::BadMagic);
        }
        if written < capacity {
            list.truncate(written);
            return Ok(list);
        }
        capacity = capacity.checked_mul(2).ok_or(Errno::OutOfMemory)?;
    }
}

/// Page a peer service's list to its short page, decoding each
/// `record_len`-byte record with `decode`.
///
/// `call` asks for the page of at most `limit` records starting at an
/// offset, writing the peer's paged reply into the buffer it is given, and
/// answers the reply's length. A peer is not trusted to end its list: one
/// longer than `most_bytes` is refused rather than accumulated.
///
/// # Errors
///
/// What `call` or `decode` answers; what [`decode_page_reply`] refuses,
/// including a page of more than `limit` records; [`Errno::BadMagic`] for a
/// reply longer than `reply`; [`Errno::LimitExceeded`] past `most_bytes`;
/// [`Errno::LengthOutOfRange`] for a zero `record_len` or `limit`, or an
/// offset past the protocol's; [`Errno::OutOfMemory`] when the records cannot
/// be held.
pub fn page_peer<R>(
    record_len: usize,
    limit: u16,
    most_bytes: usize,
    reply: &mut [u8],
    mut call: impl FnMut(u32, &mut [u8]) -> Result<usize, Errno>,
    decode: impl Fn(&[u8]) -> Result<R, Errno>,
) -> Result<Vec<R>, Errno> {
    if record_len == 0 || limit == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    let mut records = Vec::new();
    let mut bytes = 0usize;
    loop {
        let offset = u32::try_from(records.len()).map_err(|_| Errno::LengthOutOfRange)?;
        let answered = call(offset, reply)?;
        let page = reply.get(..answered).ok_or(Errno::BadMagic)?;
        let (count, body) = decode_page_reply(page, record_len, limit)?;
        bytes = bytes
            .checked_add(body.len())
            .filter(|&bytes| bytes <= most_bytes)
            .ok_or(Errno::LimitExceeded)?;
        records
            .try_reserve(usize::from(count))
            .map_err(|_| Errno::OutOfMemory)?;
        for chunk in body.chunks_exact(record_len) {
            records.push(decode(chunk)?);
        }
        if count < limit {
            return Ok(records);
        }
    }
}

#[cfg(test)]
#[path = "lists_tests.rs"]
mod tests;
