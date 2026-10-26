//! When the file manager asks the reader for the network catalogue, and what
//! each answer does (`plans/ZEROCONF.md` Z9).
//!
//! A browse asks the whole segment what it offers, so it runs only while a
//! window shows the network or is on its way there, and the session closes
//! as the last one leaves. The service rings the session's doorbell whenever
//! what it holds moves; nothing here polls.

use alloc::vec::Vec;

use tairix_browse::NetworkItem;
use tairix_discovery::DiscoveryError;

/// How long a pass that left work waits before it is asked again, and the
/// least a lost request waits before it is replaced: long enough that an
/// overloaded service is not pressed, short enough for a user waiting on it.
pub const PACE_NS: u64 = 2_000_000_000;

/// What the loop asks the reader to do with the catalogue.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NetworkAsk {
    /// Open the session if none is open, take what the service holds, and
    /// bring the catalogue's questions in line with it.
    Collect,
    /// Close the session.
    Close,
}

/// What a collect found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkReading {
    /// The catalogue's items now, in its order.
    pub items: Vec<NetworkItem>,
    /// The pass left work for the next: a question stalled, or a lost one
    /// was held back.
    pub unsettled: bool,
}

/// What the reader answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkAnswer {
    /// What a collect found, or why none could be made.
    Read(Result<NetworkReading, DiscoveryError>),
    /// The session is closed.
    Closed,
}

/// What the windows need of the browse this turn.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Need {
    /// A window shows the network or is on its way there.
    pub wanted: bool,
    /// A window waits on an answer to show the network.
    pub waiting: bool,
    /// The service rang the session's doorbell.
    pub rung: bool,
}

/// What an answer means for the windows that want the network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkLanded {
    /// The catalogue's items now, held as [`Browse::latest`].
    Items,
    /// The catalogue can no longer be read, for this reason.
    Failed(DiscoveryError),
    /// Nothing a window shows.
    Nothing,
}

/// The loop's side of the browse: what it has asked the reader, and the last
/// answer while the session holds.
#[derive(Debug, Default)]
pub struct Browse {
    asked: Option<NetworkAsk>,
    /// Something moved while a collect was out, so another is owed.
    again: bool,
    /// The reader holds a session, or a collect will open one.
    open: bool,
    /// The last items while the session holds: what a window navigating to
    /// the network meanwhile is shown at once.
    latest: Option<Vec<NetworkItem>>,
    /// Counts the answers taken, so a window tells one it has shown from one
    /// it has not.
    generation: u64,
    retry_at: Option<u64>,
}

impl Browse {
    /// What to ask the reader at `now` for what the windows `need`, or
    /// `None`. At most one collect is out at a time: a ring meanwhile owes
    /// one more once it answers.
    pub fn next(&mut self, need: Need, now: u64) -> Option<NetworkAsk> {
        if !need.wanted {
            self.again = false;
            self.retry_at = None;
            self.latest = None;
            if !self.open {
                return None;
            }
            self.open = false;
            self.asked = Some(NetworkAsk::Close);
            return Some(NetworkAsk::Close);
        }
        let moved = need.rung || self.retry_at.is_some_and(|at| now >= at);
        if self.asked == Some(NetworkAsk::Collect) {
            self.again |= moved;
            return None;
        }
        let starved = !self.open || (need.waiting && self.latest.is_none());
        if !(moved || starved || self.again) {
            return None;
        }
        self.open = true;
        self.again = false;
        self.retry_at = None;
        self.asked = Some(NetworkAsk::Collect);
        Some(NetworkAsk::Collect)
    }

    /// Take the reader's `answer`, landed at `now`.
    pub fn landed(&mut self, answer: NetworkAnswer, now: u64) -> NetworkLanded {
        let read = match answer {
            NetworkAnswer::Closed => {
                if self.asked == Some(NetworkAsk::Close) {
                    self.asked = None;
                }
                return NetworkLanded::Nothing;
            }
            NetworkAnswer::Read(read) => read,
        };
        if self.asked == Some(NetworkAsk::Collect) {
            self.asked = None;
        }
        if !self.open {
            return NetworkLanded::Nothing;
        }
        match read {
            Ok(reading) => {
                self.retry_at = reading.unsettled.then(|| now.saturating_add(PACE_NS));
                self.generation = self.generation.wrapping_add(1);
                self.latest = Some(reading.items);
                NetworkLanded::Items
            }
            Err(err) => {
                // Asked again only when a window asks again: a service that
                // is absent or refuses would otherwise be pressed in a loop.
                self.latest = None;
                self.retry_at = None;
                self.again = false;
                NetworkLanded::Failed(err)
            }
        }
    }

    /// The answer the session holds and which it is: what every window that
    /// wants the network shows once, and a window navigating there meanwhile
    /// is shown at once.
    #[must_use]
    pub fn latest(&self) -> Option<(u64, &[NetworkItem])> {
        self.latest.as_deref().map(|items| (self.generation, items))
    }

    /// When the loop must wake to ask again with nothing else to bring it.
    #[must_use]
    pub const fn retry_at(&self) -> Option<u64> {
        self.retry_at
    }
}

/// Why opening an endpoint mounts nothing: this system has no client for the
/// shares the catalogue lists (`plans/SMB.md` SMB5, SMB9).
pub const MOUNT_UNAVAILABLE: &str = "connecting to network shares is not available on this system";

/// The headline a refusal to show the network is stated under.
pub const NOT_SHOWN: &str = "The network could not be shown";

/// The headline a catalogue that stopped answering is stated under.
pub const STOPPED: &str = "Network browsing stopped";

/// Why the network could not be browsed, as a notice states it.
#[must_use]
pub const fn refusal(err: DiscoveryError) -> &'static str {
    match err {
        DiscoveryError::Unavailable => "link-local discovery is not running",
        DiscoveryError::Refused(tairix_abi::Errno::PermissionDenied) => {
            "this program may not browse the network"
        }
        DiscoveryError::Refused(tairix_abi::Errno::LimitExceeded) => {
            "the discovery service is at its limit"
        }
        DiscoveryError::Refused(_) => "the discovery service refused",
        DiscoveryError::Transport(_) => "the discovery service could not be reached",
        DiscoveryError::Malformed => "the discovery service answered in a way it never does",
    }
}

#[cfg(test)]
#[path = "netbrowse_tests.rs"]
mod tests;
