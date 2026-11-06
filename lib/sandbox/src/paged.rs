//! The page cache a streaming worker reads its owner's file through.
//!
//! The worker is handed a file's length, never the file. It holds the pages its
//! decoder has read in a bounded least-recently-used cache keyed by a seed its
//! owner draws, so a file cannot choose which of its pages collide. A read that
//! meets a page the cache does not hold answers "not yet" and records the pages
//! it lacked, which `Pages::need` turns into one bounded ask; a page the
//! request being served has read is not given up while that request lasts, so
//! each retry gets further than the last.
//!
//! Every streaming decode — a sound ([`crate::audiodecode`]) or a movie
//! ([`crate::moviedecode`]) — reads through this one cache.

use alloc::vec::Vec;

use tairix_collections::LruMap;
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_sound::{InputError, SoundInput};

/// Bytes one page of a worker's cache holds; every need starts on a page.
pub const PAGE_BYTES: usize = 4096;

/// Most bytes one need asks for, so one supply is bounded however a file is
/// laid out.
pub const MAX_NEED_BYTES: usize = MAX_NEED_PAGES * PAGE_BYTES;

/// [`PAGE_BYTES`] as a file offset counts it.
pub(crate) const PAGE: u64 = PAGE_BYTES as u64;

/// Most pages one need asks for.
pub(crate) const MAX_NEED_PAGES: usize = 128;

/// Pages a need asks for past those the read that missed wants: the bytes a
/// sequential decode reads next, so a stream costs one exchange a window.
pub(crate) const READ_AHEAD_PAGES: u64 = 32;

// One read, wherever it falls, misses at most the pages it straddles, and
// every need of them fits one supply.
const _: () = assert!(tairix_sound::MAX_READ / PAGE_BYTES < MAX_NEED_PAGES);

/// The pages a need asks for: those the read that missed wants, then the
/// read-ahead behind them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Need {
    first: u64,
    needed_end: u64,
    end: u64,
}

/// Why supplied pages could not be held.
pub(crate) enum Shortfall {
    /// Every page held is one the request has read.
    WorkingSet,
    /// A page's buffer could not be had.
    Memory,
}

/// Why no need answers an attempt's miss.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Unasked {
    /// The attempt missed no page: its input failed some other way.
    NoMiss,
    /// It lacked more pages at once than one need carries.
    TooWide,
}

/// One held page of the file: a whole page, or the file's tail.
struct Page {
    bytes: Vec<u8>,
    /// The request that last read it. A page the current request has read is
    /// never given up before that request is answered.
    read_in: u64,
}

/// The pages of one file a worker holds, read as its decoder's input.
pub(crate) struct Pages {
    held: LruMap<u64, Page, BuildSipHash13>,
    len: u64,
    /// The most pages held at once.
    capacity: usize,
    /// The request being served, counted from one; zero is no request.
    request: u64,
    /// The first and last page the failing read of this attempt lacked.
    miss: Option<(u64, u64)>,
}

impl Pages {
    /// The cache of a `len`-byte file holding at most `capacity` pages, keyed
    /// by `seed`; `None` when its table cannot be had.
    pub(crate) fn new(len: u64, seed: HashSeed, capacity: usize) -> Option<Self> {
        let held =
            LruMap::try_with_capacity_and_hasher(capacity, BuildSipHash13::with_seed(seed)).ok()?;
        Some(Self {
            held,
            len,
            capacity,
            request: 0,
            miss: None,
        })
    }

    /// Serve a new request: every page the last one read may now be given up.
    pub(crate) fn next_request(&mut self) {
        self.request += 1;
    }

    /// Begin an attempt at the request being served, nothing yet missed.
    pub(crate) fn next_attempt(&mut self) {
        self.miss = None;
    }

    fn page_count(&self) -> u64 {
        self.len.div_ceil(PAGE)
    }

    /// The need that answers this attempt's miss, or why no need could.
    pub(crate) fn need(&self) -> Result<Need, Unasked> {
        let (first, last) = self.miss.ok_or(Unasked::NoMiss)?;
        let needed_end = last + 1;
        if needed_end - first > MAX_NEED_PAGES as u64 {
            return Err(Unasked::TooWide);
        }
        let limit = (first + MAX_NEED_PAGES as u64)
            .min(needed_end + READ_AHEAD_PAGES)
            .min(self.page_count());
        let mut end = needed_end;
        while end < limit && !self.held.contains_key(&end) {
            end += 1;
        }
        Ok(Need {
            first,
            needed_end,
            end,
        })
    }

    /// Bytes `need` covers: its pages, the last cut at the file's end.
    pub(crate) fn need_bytes(&self, need: Need) -> (u64, usize) {
        let offset = need.first * PAGE;
        let end = (need.end * PAGE).min(self.len);
        (offset, usize::try_from(end - offset).unwrap_or(usize::MAX))
    }

    /// Hold the pages `bytes` holds, which are `need`'s.
    ///
    /// A page the request needs displaces any page the request has not read,
    /// and is refused only when every page held is one it has; read-ahead
    /// displaces nothing the request has read, so it stops there instead.
    pub(crate) fn take(&mut self, need: Need, bytes: &[u8]) -> Result<(), Shortfall> {
        for (page, chunk) in (need.first..need.end).zip(bytes.chunks(PAGE_BYTES)) {
            if self.held.contains_key(&page) {
                continue;
            }
            let needed = page < need.needed_end;
            let mut buffer = if self.held.len() < self.capacity {
                let mut buffer = Vec::new();
                buffer
                    .try_reserve_exact(PAGE_BYTES)
                    .map_err(|_| Shortfall::Memory)?;
                buffer
            } else {
                // Pages are refreshed as they are read, and a retry reads
                // again every page the request read before, so the least
                // recent page is one the request has read only when all are.
                let spare = self
                    .held
                    .peek_lru()
                    .is_some_and(|(_, lru)| lru.read_in != self.request);
                let recycled = if spare { self.held.pop_lru() } else { None };
                match recycled {
                    Some((_, recycled)) => recycled.bytes,
                    None if needed => return Err(Shortfall::WorkingSet),
                    None => return Ok(()),
                }
            };
            buffer.clear();
            buffer.extend_from_slice(chunk);
            let read_in = if needed { self.request } else { 0 };
            self.held
                .try_insert(
                    page,
                    Page {
                        bytes: buffer,
                        read_in,
                    },
                )
                .map_err(|_| Shortfall::Memory)?;
        }
        Ok(())
    }
}

impl SoundInput for Pages {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let wanted = u64::try_from(buf.len()).map_err(|_| InputError::Failed)?;
        let end = offset.saturating_add(wanted).min(self.len);
        let mut missing: Option<(u64, u64)> = None;
        for page in offset / PAGE..=(end - 1) / PAGE {
            let Some(held) = self.held.get_mut(&page) else {
                missing = Some((missing.map_or(page, |(first, _)| first), page));
                continue;
            };
            held.read_in = self.request;
            if missing.is_some() {
                continue;
            }
            let start = page * PAGE;
            let (from, to) = (offset.max(start), end.min(start + PAGE));
            let (Some(source), Some(target)) = (
                span(from - start, to - start).and_then(|range| held.bytes.get(range)),
                span(from - offset, to - offset).and_then(|range| buf.get_mut(range)),
            ) else {
                return Err(InputError::Failed);
            };
            target.copy_from_slice(source);
        }
        if let Some(span) = missing {
            self.miss.get_or_insert(span);
            return Err(InputError::Unavailable);
        }
        usize::try_from(end - offset).map_err(|_| InputError::Failed)
    }
}

/// `from..to` as indices, both within one read and so within `usize`.
fn span(from: u64, to: u64) -> Option<core::ops::Range<usize>> {
    Some(usize::try_from(from).ok()?..usize::try_from(to).ok()?)
}

/// Whether a need of `len` bytes at `offset` in a file of `file_len` bytes is
/// shaped as an honest worker asks one: on a page, whole pages or to the
/// file's end, inside the file, and no more than one supply carries.
pub(crate) fn shaped_need(offset: u64, len: usize, file_len: u64) -> bool {
    let end = offset.checked_add(len as u64);
    let whole = len.is_multiple_of(PAGE_BYTES) || end == Some(file_len);
    offset.is_multiple_of(PAGE)
        && len > 0
        && len <= MAX_NEED_BYTES
        && whole
        && end.is_some_and(|end| end <= file_len)
}
