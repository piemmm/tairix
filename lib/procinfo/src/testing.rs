//! A test-only `sysinfod` stand-in for the paged record walks.

use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::sysinfo::{PageRequest, SysinfoQueryId, SysinfoRequestHeader};
use tairix_abi::Errno;

use crate::transport::Transport;

/// Answers paged list queries from a fixed set of encoded records, decoding
/// each request exactly as the real service does, and records which queries
/// it was asked.
pub(crate) struct PagedFixture {
    records: Vec<Vec<u8>>,
    pub(crate) deny: bool,
    /// When set, every query is answered with exactly these bytes, for a
    /// test of how a walk meets a malformed reply.
    pub(crate) reply: Option<Vec<u8>>,
    pub(crate) seen: RefCell<Vec<SysinfoQueryId>>,
}

impl PagedFixture {
    /// A fixture serving `records`, each already in its wire form.
    pub(crate) fn new(records: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            records: records.into_iter().collect(),
            deny: false,
            reply: None,
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl Transport for PagedFixture {
    fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
        let header = SysinfoRequestHeader::from_bytes(request)?;
        self.seen.borrow_mut().push(header.query);
        if self.deny {
            return Err(Errno::PermissionDenied);
        }
        if let Some(reply) = &self.reply {
            return Ok(reply.clone());
        }
        let payload = &request[SysinfoRequestHeader::WIRE_LEN
            ..SysinfoRequestHeader::WIRE_LEN + header.payload_len as usize];
        let req = PageRequest::from_bytes(payload)?;
        let offset = req.offset as usize;
        Ok(self
            .records
            .iter()
            .skip(offset)
            .take(usize::from(req.limit))
            .flatten()
            .copied()
            .collect())
    }
}
