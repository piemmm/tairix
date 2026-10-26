//! The directory the unit tests drive a walk against: fresh ids, what each
//! open request asks in the words the tests match on, and the refusals and
//! bounds a test sets.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{Answer, Change, Entry, Query};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::Errno;
use tairix_net::dns::WireName;

use crate::walk::Directory;
use crate::DiscoveryError;

/// A directory that hands out fresh ids and records what each open request
/// asks, in the words the tests match on.
#[derive(Default)]
pub(crate) struct Book {
    pub(crate) next: u32,
    pub(crate) open: BTreeMap<u32, String>,
    /// Asks refused, by the start of their description.
    pub(crate) refusals: Vec<(&'static str, Errno)>,
    /// The most requests it holds; a start past them is refused.
    pub(crate) limit: Option<usize>,
    /// Starts asked of it, refused or not.
    pub(crate) attempts: usize,
    pub(crate) starts: usize,
    /// Every call made of it, in order.
    pub(crate) calls: Vec<String>,
}

impl Directory for Book {
    fn start(&mut self, query: &Query<'_>) -> Result<u32, DiscoveryError> {
        self.attempts += 1;
        let what = describe(query);
        if let Some(&(_, errno)) = self
            .refusals
            .iter()
            .find(|(refused, _)| what.starts_with(refused))
        {
            return Err(DiscoveryError::Refused(errno));
        }
        if self.limit.is_some_and(|limit| self.open.len() >= limit) {
            return Err(DiscoveryError::Refused(Errno::LimitExceeded));
        }
        self.next += 1;
        self.starts += 1;
        self.calls.push(format!("start {what}"));
        self.open.insert(self.next, what);
        Ok(self.next)
    }

    fn stop(&mut self, request: u32) -> Result<(), DiscoveryError> {
        if let Some(what) = self.open.remove(&request) {
            self.calls.push(format!("stop {what}"));
        }
        Ok(())
    }
}

impl Book {
    pub(crate) fn id(&self, what: &str) -> u32 {
        self.open
            .iter()
            .find(|(_, asked)| asked.as_str() == what)
            .map_or_else(
                || panic!("{what} is not open: {:?}", self.open),
                |(&id, _)| id,
            )
    }

    pub(crate) fn asks(&self) -> Vec<&str> {
        self.open.values().map(String::as_str).collect()
    }
}

fn describe(query: &Query<'_>) -> String {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).to_string();
    match query {
        Query::Types => String::from("types"),
        Query::Browse { service } => format!("browse {}", text(service.name)),
        Query::Resolve { instance, .. } => format!("resolve {}", text(instance)),
        Query::Host { name, .. } => format!("host {}", WireName(name)),
        Query::Reverse { address } => format!("reverse {address}"),
        Query::Computers => String::from("computers"),
    }
}

pub(crate) fn eth0() -> [u8; IF_NAME_LEN] {
    let mut name = [0u8; tairix_abi::net_ipc::IF_NAME_LEN];
    name[..4].copy_from_slice(b"eth0");
    name
}

pub(crate) fn entry(request: u32, change: Change, answer: Answer<'_>) -> Entry<'_> {
    Entry::Answer {
        request,
        interface: eth0(),
        change,
        ttl: 120,
        answer,
    }
}

pub(crate) fn wire(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.') {
        out.push(u8::try_from(label.len()).expect("short"));
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}
