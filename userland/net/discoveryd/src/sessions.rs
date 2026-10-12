//! Client sessions: who opened each, the requests in it, and the answers
//! queued for it until it collects them.
//!
//! A session is rung once when answers wait and not again until it has
//! collected all of them, so a slow reader costs one doorbell however many
//! answers queue. What clients hold is bounded: sessions and requests per
//! account and sessions in all, the answers queued per session, per account
//! and in all. A request whose updates find no room is told so once, after
//! everything queued before the loss. An answer reaches its request, a call
//! its session, and an exit its owner's sessions, by id: nothing here is a
//! scan of the table.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{
    CollectWriter, Entry, COLLECT_MIN_CAPACITY, ENTRY_MAX, REQUESTS_PER_ACCOUNT,
    SESSIONS_PER_ACCOUNT, SESSION_QUEUE_BYTES,
};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::{Errno, ProcId};
use tairix_collections::{HashMap, HashSet};
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;

use crate::query::QUESTIONS_PER_REQUEST;

/// The questions one request is subscribed to.
pub type Subscribed = ArrayVec<u32, QUESTIONS_PER_REQUEST>;

/// Sessions the service holds at once, for every account together.
///
/// A fixed containment bound on the table, room for a hundred accounts at
/// their own bound; what the sessions can queue is bounded apart from it, by
/// [`QUEUED_BYTES_MAX`].
pub const MAX_SESSIONS: usize = 4096;

/// Bytes of answers every session's queue holds together.
///
/// A fixed containment bound: readers that never collect cost the service
/// this much and no more, however many sessions they hold. Past it a
/// request's update is dropped and the request told so, as past its own
/// session's [`SESSION_QUEUE_BYTES`].
pub const QUEUED_BYTES_MAX: usize = 16 * 1024 * 1024;

/// How many accounts are guaranteed room to queue at once.
///
/// The fairness divisor: one account's sessions together hold at most this
/// fraction of [`QUEUED_BYTES_MAX`], so readers that never collect take the
/// service's room from everyone else only once this many accounts hold them.
pub const ACCOUNT_SHARES: usize = 16;

/// Bytes of answers one account's sessions hold together.
pub const ACCOUNT_QUEUE_BYTES: usize = QUEUED_BYTES_MAX / ACCOUNT_SHARES;

const _: () = assert!(SESSION_QUEUE_BYTES <= ACCOUNT_QUEUE_BYTES);

/// Storage a drained queue keeps: a reader that collects as it is rung
/// reuses it, and one that took a burst once does not hold the burst's room.
const RETAINED_QUEUE: usize = 1024;

/// Whether a request's updates have been dropped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Loss {
    /// None has.
    None,
    /// Some have, and it has not been told.
    Owed,
    /// It has been told; nothing more is queued for it.
    Told,
}

/// One request in a session: the questions it asks.
#[derive(Debug)]
pub struct Request {
    /// Its id within the session.
    pub id: u32,
    /// The questions it is subscribed to.
    pub questions: Subscribed,
    loss: Loss,
}

/// One client session.
#[derive(Debug)]
pub struct Session {
    id: u32,
    owner: ProcId,
    uid: u32,
    port: u64,
    requests: HashMap<u32, Request, BuildFastHash>,
    next_request: u32,
    queue: VecDeque<u8>,
    /// Requests owed a loss notice.
    losses_owed: usize,
    /// A doorbell is outstanding: the client has not yet collected
    /// everything it was rung for.
    rung: bool,
    /// Answers wait and the doorbell could not be posted for want of room in
    /// the client's port.
    ring_owed: bool,
}

/// What one account holds.
#[derive(Clone, Copy, Debug, Default)]
struct Account {
    sessions: usize,
    requests: usize,
    /// Bytes its sessions' queues hold together.
    queued: usize,
}

/// Every open session.
pub struct Sessions {
    table: HashMap<u32, Session, BuildFastHash>,
    accounts: HashMap<u32, Account, BuildFastHash>,
    /// The sessions each owning process holds.
    owners: HashMap<ProcId, Vec<u32>, BuildFastHash>,
    /// Sessions whose doorbell is owed. It has room for every open session,
    /// so owing one can never fail.
    owed: HashSet<u32, BuildFastHash>,
    next_id: u32,
    /// Bytes every queue holds together.
    queued: usize,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            table: HashMap::with_hasher(BuildFastHash::new()),
            accounts: HashMap::with_hasher(BuildFastHash::new()),
            owners: HashMap::with_hasher(BuildFastHash::new()),
            owed: HashSet::with_hasher(BuildFastHash::new()),
            next_id: 0,
            queued: 0,
        }
    }
}

impl Sessions {
    /// Open a session for `owner`, running as `uid`, ringing on `port`.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] past [`SESSIONS_PER_ACCOUNT`] for the account
    /// or [`MAX_SESSIONS`] in all, [`Errno::OutOfMemory`] when a table cannot
    /// grow.
    pub fn open(&mut self, owner: ProcId, uid: u32, port: u64) -> Result<u32, Errno> {
        let held = self
            .accounts
            .get(&uid)
            .map_or(0, |account| account.sessions);
        if held >= SESSIONS_PER_ACCOUNT || self.table.len() >= MAX_SESSIONS {
            return Err(Errno::LimitExceeded);
        }
        let room = self.table.try_reserve(1).is_ok()
            && self.accounts.try_reserve(1).is_ok()
            && self.owners.try_reserve(1).is_ok()
            && self
                .owed
                .try_reserve((self.table.len() + 1).saturating_sub(self.owed.len()))
                .is_ok();
        let listed = room
            && if let Some(sessions) = self.owners.get_mut(&owner) {
                sessions.try_reserve(1).is_ok()
            } else {
                let mut sessions = Vec::new();
                sessions.try_reserve(1).is_ok() && self.owners.try_insert(owner, sessions).is_ok()
            };
        if !listed {
            return Err(Errno::OutOfMemory);
        }
        let id = fresh_id(&mut self.next_id, |id| self.table.contains_key(&id));
        let session = Session {
            id,
            owner,
            uid,
            port,
            requests: HashMap::with_hasher(BuildFastHash::new()),
            next_request: 0,
            queue: VecDeque::new(),
            losses_owed: 0,
            rung: false,
            ring_owed: false,
        };
        // Room for each was reserved above, so none of these can fail.
        let _ = self.table.try_insert(id, session);
        let account = Account {
            sessions: held + 1,
            ..self.accounts.get(&uid).copied().unwrap_or_default()
        };
        let _ = self.accounts.try_insert(uid, account);
        if let Some(sessions) = self.owners.get_mut(&owner) {
            sessions.push(id);
        }
        Ok(id)
    }

    /// Whether `owner` holds any session.
    #[must_use]
    pub fn holds_any(&self, owner: ProcId) -> bool {
        self.owners.contains_key(&owner)
    }

    /// The session `id`, if `owner` opened it. A session id is not a
    /// capability: another principal naming it is refused as if it did not
    /// exist.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] otherwise.
    pub fn owned(&mut self, id: u32, owner: ProcId) -> Result<&mut Session, Errno> {
        self.table
            .get_mut(&id)
            .filter(|session| session.owner == owner)
            .ok_or(Errno::NotFound)
    }

    /// Close session `id` of `owner`, returning it with its requests.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] as [`Self::owned`].
    pub fn close(&mut self, id: u32, owner: ProcId) -> Result<Session, Errno> {
        self.owned(id, owner)?;
        self.remove(id).ok_or(Errno::NotFound)
    }

    /// Close one session `owner` holds, returning it, or `None` once it holds
    /// none: an exited owner's sessions are closed by calling this until then.
    pub fn close_any(&mut self, owner: ProcId) -> Option<Session> {
        let id = *self.owners.get(&owner)?.last()?;
        self.remove(id)
    }

    /// Whether the account holding session `id` of `owner` may start another
    /// request.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] as [`Self::owned`].
    pub fn has_room(&mut self, id: u32, owner: ProcId) -> Result<bool, Errno> {
        let uid = self.owned(id, owner)?.uid;
        Ok(self.requests_of(uid) < REQUESTS_PER_ACCOUNT)
    }

    /// Add a request asking `questions` to session `id` of `owner`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] as [`Self::owned`], [`Errno::LimitExceeded`] past
    /// [`REQUESTS_PER_ACCOUNT`] for the account, and [`Errno::OutOfMemory`]
    /// when a table cannot grow.
    pub fn add_request(
        &mut self,
        id: u32,
        owner: ProcId,
        questions: Subscribed,
    ) -> Result<u32, Errno> {
        let uid = self.owned(id, owner)?.uid;
        let requests = self.requests_of(uid);
        if requests >= REQUESTS_PER_ACCOUNT {
            return Err(Errno::LimitExceeded);
        }
        let session = self.owned(id, owner)?;
        session
            .requests
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let Session {
            requests: held,
            next_request,
            ..
        } = session;
        let request = fresh_id(next_request, |request| held.contains_key(&request));
        let _ = held.try_insert(
            request,
            Request {
                id: request,
                questions,
                loss: Loss::None,
            },
        );
        if let Some(account) = self.accounts.get_mut(&uid) {
            account.requests = requests + 1;
        }
        Ok(request)
    }

    /// Remove request `request` of session `id` of `owner`, returning it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the caller holds no such session or the
    /// session no such request.
    pub fn remove_request(
        &mut self,
        id: u32,
        owner: ProcId,
        request: u32,
    ) -> Result<Request, Errno> {
        let session = self.owned(id, owner)?;
        let removed = session.requests.remove(&request).ok_or(Errno::NotFound)?;
        if removed.loss == Loss::Owed {
            session.losses_owed = session.losses_owed.saturating_sub(1);
        }
        let uid = session.uid;
        if let Some(account) = self.accounts.get_mut(&uid) {
            account.requests = account.requests.saturating_sub(1);
        }
        Ok(removed)
    }

    /// Take what session `id` of `owner` has queued, as many entries as fit
    /// `capacity`, into `out`, returning the reply's length. Never waits.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] as [`Self::owned`], [`Errno::OutOfRange`] for a
    /// capacity under [`COLLECT_MIN_CAPACITY`], and [`Errno::BufferTooSmall`]
    /// when `out` is shorter than it.
    pub fn collect(
        &mut self,
        id: u32,
        owner: ProcId,
        capacity: usize,
        out: &mut [u8],
    ) -> Result<usize, Errno> {
        let session = self.owned(id, owner)?;
        let before = session.queue.len();
        let len = session.collect(capacity, out)?;
        let drained = before - session.queue.len();
        let uid = session.uid;
        self.queued = self.queued.saturating_sub(drained);
        if let Some(account) = self.accounts.get_mut(&uid) {
            account.queued = account.queued.saturating_sub(drained);
        }
        Ok(len)
    }

    /// Queue `entry` for request `request` of session `session`, rewritten
    /// to name that request. Returns the port to ring, when the session was
    /// quiet.
    pub fn queue(&mut self, session: u32, request: u32, entry: &Entry<'_>) -> Option<u64> {
        let held = self.table.get_mut(&session)?;
        let account = self.accounts.get_mut(&held.uid)?;
        let slot = held.requests.get_mut(&request)?;
        if slot.loss != Loss::None {
            return None;
        }
        let entry = entry.for_request(request);
        let len = entry.wire_len();
        let fits = held.queue.len() + len <= SESSION_QUEUE_BYTES
            && account.queued + len <= ACCOUNT_QUEUE_BYTES
            && self.queued + len <= QUEUED_BYTES_MAX
            && push_entry(&mut held.queue, &entry);
        if fits {
            self.queued += len;
            account.queued += len;
        } else {
            slot.loss = Loss::Owed;
            held.losses_owed += 1;
        }
        held.ring()
    }

    /// Queue a flush of `interface` for request `request` of session
    /// `session`; as [`Self::queue`].
    pub fn flush(
        &mut self,
        session: u32,
        request: u32,
        interface: [u8; IF_NAME_LEN],
    ) -> Option<u64> {
        self.queue(session, request, &Entry::Flush { request, interface })
    }

    /// The sessions whose doorbell is owed, by port.
    pub fn owed_rings(&self) -> impl Iterator<Item = u64> + '_ {
        self.owed
            .iter()
            .filter_map(|id| self.table.get(id))
            .map(Session::port)
    }

    /// Record how posting a doorbell to session `id` went.
    pub fn rang(&mut self, id: u32, result: Result<(), Errno>) {
        let Some(session) = self.table.get_mut(&id) else {
            return;
        };
        if result == Err(Errno::WouldBlock) {
            session.rung = false;
            session.ring_owed = true;
            let _ = self.owed.try_insert(id);
        } else {
            // Posted, or to a port that is gone and takes nothing — its
            // owner's exit closes the session.
            session.ring_owed = false;
            self.owed.remove(&id);
        }
    }

    /// Post every doorbell owed through `ring`, which is given the port and
    /// the session; one refused again for want of room stays owed.
    pub fn retry_owed(&mut self, ring: &mut dyn FnMut(u64, u32) -> Result<(), Errno>) {
        let Self { table, owed, .. } = self;
        owed.retain(|id| {
            let Some(session) = table.get_mut(id) else {
                return false;
            };
            if session.rung || !session.ring_owed {
                return session.ring_owed;
            }
            let blocked = ring(session.port, session.id) == Err(Errno::WouldBlock);
            session.ring_owed = blocked;
            session.rung = !blocked;
            blocked
        });
    }

    /// The requests the account `uid` holds.
    fn requests_of(&self, uid: u32) -> usize {
        self.accounts
            .get(&uid)
            .map_or(0, |account| account.requests)
    }

    /// Drop session `id` and everything counted for it.
    fn remove(&mut self, id: u32) -> Option<Session> {
        let session = self.table.remove(&id)?;
        self.queued = self.queued.saturating_sub(session.queue.len());
        self.owed.remove(&id);
        if let Some(account) = self.accounts.get_mut(&session.uid) {
            account.sessions = account.sessions.saturating_sub(1);
            account.requests = account.requests.saturating_sub(session.requests.len());
            account.queued = account.queued.saturating_sub(session.queue.len());
            if account.sessions == 0 {
                self.accounts.remove(&session.uid);
            }
        }
        if let Some(owned) = self.owners.get_mut(&session.owner) {
            if let Some(at) = owned.iter().position(|&held| held == id) {
                owned.swap_remove(at);
            }
            if owned.is_empty() {
                self.owners.remove(&session.owner);
            }
        }
        Some(session)
    }
}

impl Session {
    /// The session's id.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// The port its doorbell rings on.
    #[must_use]
    pub const fn port(&self) -> u64 {
        self.port
    }

    /// The account it was opened by.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// The requests it holds.
    pub fn requests(&self) -> impl Iterator<Item = &Request> {
        self.requests.values()
    }

    /// Record the questions request `id` asks.
    pub fn set_questions(&mut self, id: u32, questions: Subscribed) {
        if let Some(request) = self.requests.get_mut(&id) {
            request.questions = questions;
        }
    }

    /// Mark the session rung, returning its port when it was not already.
    fn ring(&mut self) -> Option<u64> {
        if self.rung || self.ring_owed {
            return None;
        }
        self.rung = true;
        Some(self.port)
    }

    /// Take as many queued entries as fit `capacity` into `out`, returning
    /// the reply's length. Never waits: an empty queue is an empty reply.
    fn collect(&mut self, capacity: usize, out: &mut [u8]) -> Result<usize, Errno> {
        if capacity < COLLECT_MIN_CAPACITY {
            return Err(Errno::OutOfRange);
        }
        let out = out.get_mut(..capacity).ok_or(Errno::BufferTooSmall)?;
        let mut writer = CollectWriter::new(out)?;
        let mut scratch = [0u8; ENTRY_MAX];
        while let Some(len) = peek_len(&self.queue) {
            // Only this service wrote the queue, so every entry is whole and
            // decodes; one that is not is dropped with everything after it.
            let Some(slot) = scratch.get_mut(..len).filter(|_| len <= self.queue.len()) else {
                self.queue.clear();
                break;
            };
            for (byte, queued) in slot.iter_mut().zip(&self.queue) {
                *byte = *queued;
            }
            let Ok(entry) = Entry::decode(slot) else {
                self.queue.clear();
                break;
            };
            if writer.push(&entry).is_err() {
                break;
            }
            self.queue.drain(..len);
        }
        // A loss is told after everything queued before it.
        if self.queue.is_empty() {
            if self.queue.capacity() > RETAINED_QUEUE {
                self.queue = VecDeque::new();
            }
            if self.losses_owed > 0 {
                for request in self.requests.values_mut() {
                    if request.loss == Loss::Owed
                        && writer
                            .push(&Entry::Lost {
                                request: request.id,
                            })
                            .is_ok()
                    {
                        request.loss = Loss::Told;
                        self.losses_owed -= 1;
                    }
                }
            }
        }
        let more = !self.queue.is_empty() || self.losses_owed > 0;
        // Rung again only once it has taken everything it was rung for.
        if !more {
            self.rung = false;
        }
        Ok(writer.finish(more))
    }
}

/// The first id from `next` on that `taken` does not hold, advancing `next`
/// past it. There is always one: no table here holds `u32::MAX` entries.
fn fresh_id(next: &mut u32, taken: impl Fn(u32) -> bool) -> u32 {
    loop {
        let id = *next;
        *next = next.wrapping_add(1);
        if !taken(id) {
            return id;
        }
    }
}

/// Append `entry`'s encoding, reporting whether it fitted.
fn push_entry(queue: &mut VecDeque<u8>, entry: &Entry<'_>) -> bool {
    let mut bytes = [0u8; ENTRY_MAX];
    let Ok(len) = entry.encode(&mut bytes) else {
        return false;
    };
    if queue.try_reserve(len).is_err() {
        return false;
    }
    queue.extend(&bytes[..len]);
    true
}

/// The length of the entry at the front of `queue`.
fn peek_len(queue: &VecDeque<u8>) -> Option<usize> {
    let low = *queue.front()?;
    let high = *queue.get(1)?;
    Some(usize::from(u16::from_le_bytes([low, high])))
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
