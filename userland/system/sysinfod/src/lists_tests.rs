extern crate std;

use super::*;
use alloc::vec;
use core::cell::{Cell, RefCell};
use tairix_abi::reply::{encode_page_reply, PAGE_HEADER_LEN, STATUS_REPLY_LEN};

const RECORD: usize = 4;

/// A kernel list as `read_whole` sees it: each read writes as many whole
/// records as fit, from the list as it stands at that read.
fn kernel_read(list: &[u8], buffer: &mut [u8]) -> usize {
    let fits = buffer.len() / RECORD * RECORD;
    let written = list.len().min(fits);
    buffer[..written].copy_from_slice(&list[..written]);
    written
}

fn records(count: u32) -> std::vec::Vec<u8> {
    (0..count).flat_map(u32::to_le_bytes).collect()
}

/// A list longer than the first read is read again into a buffer twice the
/// size until one holds it, and comes back whole.
#[test]
fn a_long_kernel_list_is_read_whole() {
    let list = records(200);
    let reads = Cell::new(0usize);
    let read = read_whole(RECORD, |buffer| {
        reads.set(reads.get() + 1);
        Ok(kernel_read(&list, buffer))
    });
    assert_eq!(read, Ok(list));
    assert_eq!(reads.get(), 3, "64, then 128, then 256 records");
}

/// A list that changes between reads comes back as one reading of it, never
/// the start of one joined to the rest of another.
#[test]
fn a_kernel_list_changing_between_reads_is_one_reading() {
    let readings = RefCell::new(vec![records(100), records(70).into_iter().rev().collect()]);
    let read = read_whole(RECORD, |buffer| {
        let list = readings.borrow_mut().remove(0);
        Ok(kernel_read(&list, buffer))
    });
    let second: std::vec::Vec<u8> = records(70).into_iter().rev().collect();
    assert_eq!(read, Ok(second));
}

/// A read ending part way through a record is refused, not cut back to the
/// records before it.
#[test]
fn a_kernel_read_ending_part_way_through_a_record_is_refused() {
    assert_eq!(
        read_whole(RECORD, |_| Ok(RECORD * 3 + 1)),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        read_whole(RECORD, |buffer| Ok(buffer.len() + RECORD)),
        Err(Errno::BadMagic),
        "longer than its buffer"
    );
    assert_eq!(read_whole(0, |_| Ok(0)), Err(Errno::LengthOutOfRange));
}

/// The reply a peer sends for `offset` of a `total`-record list.
fn peer_page(total: u32, offset: u32, limit: u16, reply: &mut [u8]) -> Result<usize, Errno> {
    let end = total.min(offset.saturating_add(u32::from(limit)));
    let page: std::vec::Vec<[u8; RECORD]> = (offset..end).map(u32::to_le_bytes).collect();
    encode_page_reply(&page, limit, reply)
}

const LIMIT: u16 = 8;
const REPLY: usize = STATUS_REPLY_LEN + PAGE_HEADER_LEN + LIMIT as usize * RECORD;

fn decode(chunk: &[u8]) -> Result<u32, Errno> {
    chunk
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| Errno::BadMagic)
}

/// A peer's list is paged to its short page, each page asked for at the
/// records already read.
#[test]
fn a_peer_list_is_paged_to_its_short_page() {
    let asked = RefCell::new(std::vec::Vec::new());
    let mut reply = [0u8; REPLY];
    let read = page_peer(
        RECORD,
        LIMIT,
        usize::MAX,
        &mut reply,
        |offset, reply| {
            asked.borrow_mut().push(offset);
            peer_page(20, offset, LIMIT, reply)
        },
        decode,
    );
    assert_eq!(read, Ok((0..20).collect()));
    assert_eq!(*asked.borrow(), [0, 8, 16]);
}

/// A peer answering full pages for ever is refused once its list passes the
/// most this service reads, rather than read until memory runs out.
#[test]
fn a_peer_answering_full_pages_for_ever_is_refused() {
    let pages = Cell::new(0usize);
    let most = 10 * usize::from(LIMIT) * RECORD;
    let mut reply = [0u8; REPLY];
    let read = page_peer(
        RECORD,
        LIMIT,
        most,
        &mut reply,
        |_, reply| {
            pages.set(pages.get() + 1);
            peer_page(u32::MAX, 0, LIMIT, reply)
        },
        decode,
    );
    assert_eq!(read, Err(Errno::LimitExceeded));
    assert_eq!(pages.get(), 11);
}

/// A page claiming more records than were asked for, or a reply longer than
/// its buffer, is refused whole.
#[test]
fn a_peer_page_past_its_bounds_is_refused() {
    let mut reply = [0u8; REPLY];
    let overlong = page_peer(
        RECORD,
        LIMIT - 1,
        usize::MAX,
        &mut reply,
        |offset, reply| peer_page(20, offset, LIMIT, reply),
        decode,
    );
    assert_eq!(overlong, Err(Errno::LengthOutOfRange));
    let mut reply = [0u8; REPLY];
    let past = page_peer(
        RECORD,
        LIMIT,
        usize::MAX,
        &mut reply,
        |_, reply| Ok(reply.len() + 1),
        decode,
    );
    assert_eq!(past, Err(Errno::BadMagic));
    let mut reply = [0u8; REPLY];
    let empty = page_peer(RECORD, 0, usize::MAX, &mut reply, |_, _| Ok(0), decode);
    assert_eq!(empty, Err(Errno::LengthOutOfRange));
}
