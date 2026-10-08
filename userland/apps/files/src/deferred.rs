//! What the file manager reads off its event loop, and the policy that decides
//! when it has an answer.
//!
//! Every read this app makes is a read of somebody's disk: the directory the
//! user navigated to, the folder cue each visible folder draws, and the program
//! stores the *Open With…* chooser is built from. Run on the loop that owes the
//! window a frame, each one freezes the window for as long as that disk takes —
//! which on a slow or contended device is not a frame but a visible stall.
//!
//! So they run on a worker, and the loop learns an answer landed through the
//! wait-set it already parks in. The listing, probe and scan policies are the
//! shared ones ([`tairix_browse::ListingDesk`], [`tairix_browse::Probes`] and
//! `tairix_util::defer::JobDesk`, the last linked into the target build
//! alone); what is here is the one policy none covers.
//!
//! # The Properties read
//!
//! A Properties window describes one node, which is one `fs_stat` plus one
//! call per extended-attribute key — thirty-odd round trips for a node
//! carrying a full set. [`PropertyReads`] is keyed by *window*, because
//! several Properties windows are open at once and one window's read must not
//! displace another's; a re-read of the same window supersedes its own
//! outstanding one, since only the latest answer describes the node now.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_browse::{EntryKind, Properties};

/// One browser window's directory-listing consumer.
///
/// Each window lists on its own: two windows sharing one consumer each threw
/// the other's answer away as stale and asked again for its own, so neither
/// ever listed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct FilesClient(u64);

/// Where each browser window's [`FilesClient`] comes from: never the same one
/// twice, so a window opened after another closed cannot be answered with the
/// closed one's read.
#[derive(Debug, Default)]
pub struct FilesClients(u64);

impl FilesClients {
    /// The consumer the next browser window lists under.
    pub fn mint(&mut self) -> FilesClient {
        self.0 = self.0.wrapping_add(1);
        FilesClient(self.0)
    }
}

impl FilesClient {
    /// The number this consumer was minted as, which its watch's wait-set
    /// token is derived from.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.0
    }

    /// The consumer minted as `number`, recovered from a wait-set token.
    #[must_use]
    pub const fn from_number(number: u64) -> Self {
        Self(number)
    }
}

/// One Properties window's read: the node to describe and which window asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PropertyJob {
    /// The window the answer belongs to.
    pub window: u64,
    /// The node's absolute path, by the one spelling every read and write of
    /// it uses.
    pub path: String,
    /// What the listing called it, which decides how the node is opened to be
    /// described (a link is described as itself, not as its target).
    pub kind: EntryKind,
}

/// What the Properties windows have asked to be read and what has come back.
///
/// Free of locks, threads, and syscalls: the embedder supplies the exclusion
/// and the blocking, so every rule here is a host test.
#[derive(Debug, Default)]
pub struct PropertyReads {
    /// Reads asked for and not yet started, in the order asked.
    wanted: Vec<PropertyJob>,
    /// Windows whose read is running, so a window closed mid-read is not
    /// answered into a slot nobody owns.
    reading: Vec<u64>,
    /// Answers waiting to be collected, one per window.
    answers: Vec<(u64, Result<Properties, Errno>)>,
    /// Whether an answer has landed since the embedder last asked.
    landed: bool,
    /// Set once the embedder is tearing down.
    stopping: bool,
}

impl PropertyReads {
    /// A desk with nothing asked for and nothing answered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wanted: Vec::new(),
            reading: Vec::new(),
            answers: Vec::new(),
            landed: false,
            stopping: false,
        }
    }

    /// Ask for `job`, answering whether a worker should be woken.
    ///
    /// A window's outstanding request is *replaced*: a re-read follows a write
    /// the window just made, so the older answer describes a node that has
    /// since changed and showing it would undo what the user did. Any answer
    /// the window had not collected goes with it, for the same reason.
    pub fn submit(&mut self, job: PropertyJob) -> bool {
        if self.stopping {
            return false;
        }
        let window = job.window;
        self.wanted.retain(|held| held.window != window);
        self.answers.retain(|(id, _)| *id != window);
        self.wanted.push(job);
        true
    }

    /// Whether any read is waiting for a worker to take it.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping && !self.wanted.is_empty()
    }

    /// Take the next read to run, or `None` when there is nothing to do.
    pub fn next_job(&mut self) -> Option<PropertyJob> {
        if self.stopping || self.wanted.is_empty() {
            return None;
        }
        let job = self.wanted.remove(0);
        self.reading.push(job.window);
        Some(job)
    }

    /// Record what reading `window`'s node produced, answering whether the
    /// embedder's loop is owed a wake.
    ///
    /// An answer for a window that is no longer reading — it was closed, or
    /// asked again — is dropped: nothing owns the slot, and holding it would
    /// leave a stale summary for the next window to be given that id.
    pub fn deliver(&mut self, window: u64, answer: Result<Properties, Errno>) -> bool {
        let Some(index) = self.reading.iter().position(|id| *id == window) else {
            return false;
        };
        self.reading.remove(index);
        if self.stopping {
            return false;
        }
        self.answers.retain(|(id, _)| *id != window);
        self.answers.push((window, answer));
        self.landed = true;
        true
    }

    /// Take `window`'s answer, if one has landed.
    pub fn take(&mut self, window: u64) -> Option<Result<Properties, Errno>> {
        let index = self.answers.iter().position(|(id, _)| *id == window)?;
        Some(self.answers.remove(index).1)
    }

    /// Whether an answer has landed since this was last asked, clearing the
    /// record.
    pub fn take_landed(&mut self) -> bool {
        core::mem::take(&mut self.landed)
    }

    /// Forget everything `window` asked for, because it has closed.
    ///
    /// A read already running is left to finish and its answer dropped on
    /// delivery: a worker mid-`fs_stat` cannot be recalled, and the alternative
    /// is a window id that could be reused while an answer for it is still in
    /// flight.
    pub fn forget(&mut self, window: u64) {
        self.wanted.retain(|job| job.window != window);
        self.answers.retain(|(id, _)| *id != window);
        self.reading.retain(|id| *id != window);
    }

    /// Stop handing out work, so a parked worker leaves.
    pub fn stop(&mut self) {
        self.stopping = true;
        self.wanted.clear();
        self.answers.clear();
        self.landed = false;
    }
}

#[cfg(test)]
#[path = "deferred_tests.rs"]
mod tests;
