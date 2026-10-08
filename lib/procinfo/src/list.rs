//! The shared paged-list walk for sysinfo queries that return a homogeneous
//! sequence of fixed-size records.
//!
//! Every `sysinfo-v1` list query answers with a run of fixed-[`WIRE_LEN`]
//! records that the client pages through with a [`PageRequest`]. The paging
//! loop is identical across them: request a page, reject a structurally
//! invalid reply, decode each record, and stop on a short page. It lives here
//! once rather than being copied per query.
//!
//! [`WIRE_LEN`]: tairix_abi::sysinfo::ProcessRecord::WIRE_LEN

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use tairix_abi::sysinfo::{PageRequest, SysinfoQueryId};
use tairix_abi::Errno;

use crate::request::{call, CallError};
use crate::transport::Transport;

/// Decode an inline byte field for display, substituting `U+FFFD` for any
/// invalid byte rather than failing.
///
/// Shared by the process and mount row renderers (and consumers such as
/// `top`'s own row layout) so none re-implements lossy decoding; a display
/// routine never panics on hostile bytes.
#[must_use]
pub fn field_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Why a paged sysinfo list walk did not complete.
///
/// [`Call`](ListError::Call) carries a transport/capability failure
/// (including a structurally invalid reply, reported as [`Errno::BadMagic`]);
/// [`Sink`](ListError::Sink) carries the [`Errno`] a caller's per-record sink
/// raised (typically a terminal write). Distinguishing them lets a consuming
/// tool map each onto the right line of its own error enum. The same type serves every paged walk, so the process and mount
/// tools share one error shape rather than each inventing one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListError {
    /// The query failed, was denied, or returned a structurally invalid
    /// reply.
    Call(CallError),
    /// The per-record sink raised an error (e.g. a failed terminal write).
    Sink(Errno),
}

/// Whether a paged walk should carry on past the record just delivered.
///
/// A sink that has taken all the records its caller can hold — a periodic
/// sampler bounding how much of a huge or hostile reply it will
/// accumulate — answers [`Stop`](WalkStep::Stop) and the walk ends
/// successfully, without asking for a further page. Ending early is
/// therefore an ordinary `Ok` outcome the caller chose, while a genuine
/// failure is the only thing that comes back as [`Err`]: a deliberate
/// truncation can never be mistaken for a broken service, and no caller
/// has to smuggle "I have enough" through an error it then has to catch
/// back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalkStep {
    /// Deliver the next record.
    Continue,
    /// End the walk here; no further record is delivered and no further
    /// page is requested.
    Stop,
}

/// A walk id distinct among this process's walks: every page of one walk is
/// answered from one reading of its list.
fn next_walk() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    loop {
        let walk = NEXT.fetch_add(1, Ordering::Relaxed);
        if walk != PageRequest::FRESH {
            return walk;
        }
    }
}

/// Page through `query` and hand each record's raw bytes to `on_record`,
/// until the list is exhausted or `on_record` answers
/// [`WalkStep::Stop`].
///
/// Each page is asked for with a [`PageRequest`] for `page` records of
/// `record_len` bytes. The walk **fails closed**: a reply whose length is not
/// a whole number of records, one that would overflow the page offset, or a
/// `record_len` or `page` of zero, is rejected rather than partially
/// delivered.
///
/// Public so a consumer with its own bounding or cadence policy (for
/// example a periodic sampler that must cap how many records a single walk
/// may accumulate) can drive the same paging loop directly instead of
/// re-implementing it; every query-specific `for_each_*` walk in this crate
/// is built on it. Such a consumer bounds itself by answering
/// [`WalkStep::Stop`], which ends the walk as a success, so its own
/// truncation stays distinguishable from a service that failed.
///
/// # Errors
///
/// * [`ListError::Call`] — the transport failed, the service denied the
///   query, the reply was structurally invalid, or the service let the walk
///   go before it ended ([`Errno::Interrupted`]), which its caller starts
///   again.
/// * [`ListError::Sink`] — `on_record` returned an error; the walk stops at
///   that record.
pub fn walk_pages(
    transport: &dyn Transport,
    query: SysinfoQueryId,
    record_len: usize,
    page: u16,
    on_record: impl FnMut(&[u8]) -> Result<WalkStep, ListError>,
) -> Result<(), ListError> {
    walk_pages_with(
        transport,
        query,
        record_len,
        page,
        PageRequest::to_le_bytes,
        on_record,
    )
}

/// [`walk_pages`] over the records `decode` reads from each chunk, handing
/// each to `sink`: a record that does not decode fails the walk as the
/// service's error, and a refusal from `sink` as the sink's.
///
/// # Errors
///
/// As [`walk_pages`].
pub fn walk_records<R>(
    transport: &dyn Transport,
    query: SysinfoQueryId,
    record_len: usize,
    page: u16,
    decode: impl Fn(&[u8]) -> Result<R, Errno>,
    mut sink: impl FnMut(&R) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_pages(transport, query, record_len, page, |chunk| {
        let record = decode(chunk).map_err(|errno| ListError::Call(CallError::Service(errno)))?;
        sink(&record).map_err(ListError::Sink)
    })
}

/// [`walk_pages`] for a query whose payload carries more than its page:
/// `make_request` encodes each page's payload from the [`PageRequest`] the
/// walk has reached.
///
/// # Errors
///
/// As [`walk_pages`].
pub fn walk_pages_with<const N: usize>(
    transport: &dyn Transport,
    query: SysinfoQueryId,
    record_len: usize,
    page: u16,
    make_request: impl Fn(&PageRequest) -> [u8; N],
    mut on_record: impl FnMut(&[u8]) -> Result<WalkStep, ListError>,
) -> Result<(), ListError> {
    // An empty page is answered empty for ever, so the walk would never end.
    if record_len == 0 || page == 0 {
        return Err(ListError::Call(CallError::Service(Errno::LengthOutOfRange)));
    }
    let walk = next_walk();
    let mut offset: u32 = 0;
    loop {
        let request = make_request(&PageRequest {
            offset,
            limit: page,
            flags: 0,
            walk,
        });
        let reply = call(transport, query, &request).map_err(ListError::Call)?;
        let count = reply.len() / record_len;
        // More than was asked for breaks the protocol as surely as a partial
        // record does: nothing of such a page is believed.
        if reply.len() % record_len != 0 || count > usize::from(page) {
            return Err(ListError::Call(CallError::Service(Errno::BadMagic)));
        }
        for chunk in reply.chunks_exact(record_len) {
            if on_record(chunk)? == WalkStep::Stop {
                return Ok(());
            }
        }
        if count < usize::from(page) {
            return Ok(());
        }
        let advanced = u32::try_from(count)
            .map_err(|_| ListError::Call(CallError::Service(Errno::LengthOutOfRange)))?;
        offset = offset
            .checked_add(advanced)
            .ok_or(ListError::Call(CallError::Service(Errno::LengthOutOfRange)))?;
    }
}

#[cfg(test)]
mod tests {
    use super::{walk_pages, walk_pages_with, ListError, WalkStep};
    use crate::request::CallError;
    use crate::transport::Transport;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::sysinfo::{PageRequest, SysinfoQueryId, SysinfoRequestHeader};
    use tairix_abi::Errno;

    /// One record per byte value, so a page's worth of records is trivially
    /// countable and the paging arithmetic is what is under test.
    const RECORD_LEN: usize = 1;
    const PAGE: u16 = 4;

    /// A stand-in that always answers a full page, so the walk only ever
    /// ends because the sink says so — never because the list ran out.
    struct Endless {
        pages: RefCell<usize>,
    }

    impl Transport for Endless {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            SysinfoRequestHeader::from_bytes(request)?;
            *self.pages.borrow_mut() += 1;
            Ok(alloc::vec![7u8; usize::from(PAGE) * RECORD_LEN])
        }
    }

    fn walk(
        transport: &dyn Transport,
        record_len: usize,
        on_record: impl FnMut(&[u8]) -> Result<WalkStep, ListError>,
    ) -> Result<(), ListError> {
        walk_pages(
            transport,
            SysinfoQueryId::MOUNT_LIST,
            record_len,
            PAGE,
            on_record,
        )
    }

    /// Answers `total` one-byte records a page at a time, keeping every
    /// payload it was asked with.
    struct Recording {
        total: usize,
        payloads: RefCell<Vec<Vec<u8>>>,
    }

    impl Transport for Recording {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            SysinfoRequestHeader::from_bytes(request)?;
            let payload = &request[SysinfoRequestHeader::WIRE_LEN..];
            self.payloads.borrow_mut().push(payload.to_vec());
            let page = PageRequest::from_bytes(payload)?;
            let offset = usize::try_from(page.offset).map_err(|_| Errno::LengthOutOfRange)?;
            let take = self
                .total
                .saturating_sub(offset)
                .min(usize::from(page.limit));
            Ok(alloc::vec![7u8; take * RECORD_LEN])
        }
    }

    /// Answers its first page with one record more than was asked for, and
    /// every later one empty.
    struct Overlong {
        answered: RefCell<bool>,
    }

    impl Transport for Overlong {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            let payload = &request[SysinfoRequestHeader::WIRE_LEN..];
            let page = PageRequest::from_bytes(payload)?;
            if self.answered.replace(true) {
                return Ok(Vec::new());
            }
            Ok(alloc::vec![7u8; (usize::from(page.limit) + 1) * RECORD_LEN])
        }
    }

    /// A page longer than was asked for breaks the protocol, so none of it
    /// reaches the sink.
    #[test]
    fn a_page_longer_than_was_asked_for_is_refused_whole() {
        let transport = Overlong {
            answered: RefCell::new(false),
        };
        let mut seen = 0usize;
        let result = walk(&transport, RECORD_LEN, |_| {
            seen += 1;
            Ok(WalkStep::Continue)
        });
        assert_eq!(
            result,
            Err(ListError::Call(CallError::Service(Errno::BadMagic)))
        );
        assert_eq!(seen, 0);
    }

    /// A whole record that does not decode ends the walk as the service's
    /// fault, and nothing after it reaches the sink.
    #[test]
    fn a_record_that_does_not_decode_ends_the_walk_as_the_service_s_fault() {
        let transport = Recording {
            total: 3,
            payloads: RefCell::new(Vec::new()),
        };
        let decoded = core::cell::Cell::new(0usize);
        let mut seen = 0usize;
        let result = super::walk_records(
            &transport,
            SysinfoQueryId::MOUNT_LIST,
            RECORD_LEN,
            PAGE,
            |_| {
                decoded.set(decoded.get() + 1);
                if decoded.get() == 2 {
                    return Err(Errno::BadMagic);
                }
                Ok(())
            },
            |()| {
                seen += 1;
                Ok(WalkStep::Continue)
            },
        );
        assert_eq!(
            result,
            Err(ListError::Call(CallError::Service(Errno::BadMagic)))
        );
        assert_eq!((decoded.get(), seen), (2, 1));
    }

    /// Each page asks for the window after the last, every page naming the
    /// walk's one id, and the short page ends the walk; the next walk names
    /// another.
    #[test]
    fn each_page_asks_for_the_window_after_the_last() {
        let pages_of = || {
            let transport = Recording {
                total: usize::from(PAGE) * 2 + 1,
                payloads: RefCell::new(Vec::new()),
            };
            let mut seen = 0usize;
            let result = walk(&transport, RECORD_LEN, |_| {
                seen += 1;
                Ok(WalkStep::Continue)
            });
            assert_eq!(result, Ok(()));
            assert_eq!(seen, transport.total);
            let asked: Vec<PageRequest> = transport
                .payloads
                .borrow()
                .iter()
                .map(|payload| PageRequest::from_bytes(payload).unwrap())
                .collect();
            asked
        };
        let asked = pages_of();
        let walk = asked[0].walk;
        assert_ne!(walk, PageRequest::FRESH, "a walk names itself");
        let at = |offset: u16| PageRequest {
            offset: u32::from(offset),
            limit: PAGE,
            flags: 0,
            walk,
        };
        assert_eq!(asked, [at(0), at(PAGE), at(PAGE * 2)]);
        assert_ne!(pages_of()[0].walk, walk, "another walk, another id");
    }

    /// A payload carrying more than its page is sent whole, on every page.
    #[test]
    fn an_extended_payload_rides_every_page() {
        let transport = Recording {
            total: usize::from(PAGE) + 1,
            payloads: RefCell::new(Vec::new()),
        };
        let result = walk_pages_with(
            &transport,
            SysinfoQueryId::NET_INTERFACE_RATES,
            RECORD_LEN,
            PAGE,
            |page| {
                let mut payload = [0xA5u8; PageRequest::WIRE_LEN + 2];
                payload[..PageRequest::WIRE_LEN].copy_from_slice(&page.to_le_bytes());
                payload
            },
            |_| Ok(WalkStep::Continue),
        );
        assert_eq!(result, Ok(()));
        let payloads = transport.payloads.borrow();
        assert_eq!(payloads.len(), 2);
        for payload in payloads.iter() {
            assert_eq!(payload.len(), PageRequest::WIRE_LEN + 2);
            assert_eq!(payload[PageRequest::WIRE_LEN..], [0xA5, 0xA5]);
        }
    }

    /// A page of no records is refused before anything is asked, rather than
    /// answered empty and asked again at the same offset for ever.
    #[test]
    fn an_empty_page_is_refused_rather_than_walked_for_ever() {
        let transport = Endless {
            pages: RefCell::new(0),
        };
        let result = walk_pages(
            &transport,
            SysinfoQueryId::MOUNT_LIST,
            RECORD_LEN,
            0,
            |_| Ok(WalkStep::Continue),
        );
        assert_eq!(
            result,
            Err(ListError::Call(CallError::Service(Errno::LengthOutOfRange)))
        );
        assert_eq!(*transport.pages.borrow(), 0);
    }

    #[test]
    fn a_stopping_sink_ends_the_walk_successfully_mid_page() {
        let transport = Endless {
            pages: RefCell::new(0),
        };
        let seen = RefCell::new(0usize);
        let result = walk(&transport, RECORD_LEN, |_| {
            *seen.borrow_mut() += 1;
            if *seen.borrow() == 2 {
                Ok(WalkStep::Stop)
            } else {
                Ok(WalkStep::Continue)
            }
        });
        // Ending early is the caller's own decision, so it is `Ok`: only a
        // real failure is an `Err`.
        assert_eq!(result, Ok(()));
        assert_eq!(*seen.borrow(), 2);
        // The rest of the page is not delivered and no further page is
        // requested.
        assert_eq!(*transport.pages.borrow(), 1);
    }

    #[test]
    fn a_stopping_sink_and_a_failing_sink_are_different_outcomes() {
        let transport = Endless {
            pages: RefCell::new(0),
        };
        assert_eq!(walk(&transport, RECORD_LEN, |_| Ok(WalkStep::Stop)), Ok(()));
        assert_eq!(
            walk(&transport, RECORD_LEN, |_| Err(ListError::Sink(
                Errno::NotFound
            ))),
            Err(ListError::Sink(Errno::NotFound))
        );
    }

    #[test]
    fn a_continuing_sink_keeps_paging_a_full_page() {
        let transport = Endless {
            pages: RefCell::new(0),
        };
        let seen = RefCell::new(0usize);
        let result = walk(&transport, RECORD_LEN, |_| {
            *seen.borrow_mut() += 1;
            if *seen.borrow() > usize::from(PAGE) {
                Ok(WalkStep::Stop)
            } else {
                Ok(WalkStep::Continue)
            }
        });
        assert_eq!(result, Ok(()));
        // The first record of the second page is what stopped it, so the
        // walk did request that second page.
        assert_eq!(*transport.pages.borrow(), 2);
    }

    #[test]
    fn a_zero_record_length_is_refused_rather_than_dividing_by_zero() {
        let transport = Endless {
            pages: RefCell::new(0),
        };
        assert_eq!(
            walk(&transport, 0, |_| Ok(WalkStep::Continue)),
            Err(ListError::Call(CallError::Service(Errno::LengthOutOfRange)))
        );
        // Refused before any query was issued.
        assert_eq!(*transport.pages.borrow(), 0);
    }
}
