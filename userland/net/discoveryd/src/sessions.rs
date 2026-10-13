//! Client sessions: who opened each, the requests in it, and the answers
//! queued for it until it collects them.
//!
//! A session is rung once when answers wait and not again until it has
//! collected all of them, so a slow reader costs one doorbell however many
//! answers queue. What clients hold is bounded: sessions and requests per
//! account and sessions in all, the answers queued per session, per account
//! and in all. A request whose updates find no room is told so once, after
//! everything queued before the loss. A doorbell a full port refuses is
//! owed, and the port watched for room only while one is owed there. An
//! answer reaches its request, a call its session, an exit its owner's
//! sessions, a retry the sessions owed a doorbell, and a collect the losses
//! its session owes, by id or by list: nothing here is a scan of the table.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{
    CollectWriter, Entry, COLLECT_MIN_CAPACITY, ENTRY_MAX, REQUESTS_PER_ACCOUNT,
    SESSIONS_PER_ACCOUNT, SESSION_QUEUE_BYTES,
};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::{Errno, ProcId};
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;

use crate::query::QUESTIONS_PER_REQUEST;

/// The questions one request is subscribed to.
pub type Subscribed = ArrayVec<u32, QUESTIONS_PER_REQUEST>;

/// What queueing an entry for one request did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Queued {
    /// The port to ring, when the session was quiet.
    pub ring: Option<u64>,
    /// Whether the request still takes entries: one that has lost some is
    /// told so and takes nothing more.
    pub taking: bool,
}

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
    /// The last flush pass it was told in, so a request asking two questions
    /// is told once per pass.
    flushed_in: u64,
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
    /// The requests that came to be owed a loss, newest last, so a collect
    /// visits only them. A request is owed at most once, and room for every
    /// request was made as it was added, so listing one never allocates; a
    /// request stopped meanwhile is skipped where it was listed.
    lost: Vec<u32>,
    /// A doorbell is outstanding: the client has not yet collected
    /// everything it was rung for.
    rung: bool,
    /// Answers wait and the doorbell could not be posted for want of room in
    /// the client's port.
    ring_owed: bool,
    /// Where the session sits in [`Sessions::owed`], while it is listed.
    owed_at: Option<usize>,
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
    /// Sessions whose doorbell is owed, each recording where it sits. It has
    /// room for every open session, so owing one never allocates.
    owed: Vec<u32>,
    /// The client ports owed doorbells ring, for the owner to watch for room.
    rooms: Rooms,
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
            owed: Vec::new(),
            rooms: Rooms::default(),
            next_id: 0,
            queued: 0,
        }
    }
}

impl Sessions {
    /// Panic unless the table's counts, lists and indexes agree with its
    /// sessions: the oracle the fuzz harness holds the table to.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_consistent(&self) {
        use alloc::collections::BTreeMap;
        let mut queued = 0;
        let mut accounts: BTreeMap<u32, (usize, usize, usize)> = BTreeMap::new();
        let mut owed_ports: BTreeMap<u64, u32> = BTreeMap::new();
        for (&id, session) in &self.table {
            assert_eq!(session.id, id);
            queued += session.queue.len();
            let account = accounts.entry(session.uid).or_default();
            account.0 += 1;
            account.1 += session.requests.len();
            account.2 += session.queue.len();
            let owed = session
                .requests
                .values()
                .filter(|request| request.loss == Loss::Owed)
                .count();
            assert_eq!(session.losses_owed, owed, "losses owed miscounted");
            for request in session
                .requests
                .values()
                .filter(|request| request.loss == Loss::Owed)
            {
                assert!(session.lost.contains(&request.id), "a loss owed unlisted");
            }
            if let Some(at) = session.owed_at {
                assert_eq!(self.owed.get(at), Some(&id), "an owed session out of place");
                *owed_ports.entry(session.port).or_default() += 1;
            }
            assert!(
                self.owners
                    .get(&session.owner)
                    .is_some_and(|sessions| sessions.contains(&id)),
                "a session its owner does not list",
            );
        }
        assert_eq!(self.queued, queued);
        assert!(self.queued <= QUEUED_BYTES_MAX);
        let recorded: BTreeMap<u32, (usize, usize, usize)> = self
            .accounts
            .iter()
            .map(|(&uid, account)| (uid, (account.sessions, account.requests, account.queued)))
            .collect();
        assert_eq!(recorded, accounts, "accounts disagree with their sessions");
        let listed = self
            .table
            .values()
            .filter(|session| session.owed_at.is_some())
            .count();
        assert_eq!(listed, self.owed.len());
        assert!(self.owed.capacity() >= self.table.len());
        assert!(self
            .rooms
            .ports
            .windows(2)
            .all(|pair| pair[0].port < pair[1].port));
        for room in &self.rooms.ports {
            assert_eq!(
                owed_ports.get(&room.port).copied().unwrap_or(0),
                room.owed,
                "a room counts doorbells not owed"
            );
        }
        for port in owed_ports.keys() {
            assert!(
                self.rooms.ports.iter().any(|room| room.port == *port),
                "a port owed a doorbell has no room"
            );
        }
        let marked = self.rooms.ports.iter().filter(|room| room.dirty).count();
        assert_eq!(marked, self.rooms.dirty.len(), "a dirty port listed twice");
        for port in &self.rooms.dirty {
            assert!(
                self.rooms
                    .ports
                    .iter()
                    .any(|room| room.port == *port && room.dirty),
                "a dirty port with no room"
            );
        }
    }

    /// Panic unless no session is open and no doorbell is owed or watched
    /// for: the leak oracle once every client has gone and the owner synced.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_idle(&self) {
        self.assert_consistent();
        assert!(self.table.is_empty() && self.accounts.is_empty() && self.owners.is_empty());
        assert!(self.owed.is_empty() && self.queued == 0);
        assert!(self.rooms.ports.is_empty() && self.rooms.dirty.is_empty());
    }

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
                .is_ok()
            && self.rooms.reserve(self.table.len() + 1);
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
            lost: Vec::new(),
            rung: false,
            ring_owed: false,
            owed_at: None,
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
        let live = session.requests.len();
        if session.requests.try_reserve(1).is_err() || session.lost.try_reserve(live + 1).is_err() {
            return Err(Errno::OutOfMemory);
        }
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
                flushed_in: 0,
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
    /// to name that request.
    pub fn queue(&mut self, session: u32, request: u32, entry: &Entry<'_>) -> Queued {
        const TAKES_NOTHING: Queued = Queued {
            ring: None,
            taking: false,
        };
        let Some(held) = self.table.get_mut(&session) else {
            return TAKES_NOTHING;
        };
        let (Some(account), Some(slot)) = (
            self.accounts.get_mut(&held.uid),
            held.requests.get_mut(&request),
        ) else {
            return TAKES_NOTHING;
        };
        if slot.loss != Loss::None {
            return TAKES_NOTHING;
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
            held.lost.push(request);
        }
        Queued {
            ring: held.ring(),
            taking: fits,
        }
    }

    /// Queue a flush of `interface` for request `request` of session
    /// `session` in flush pass `pass` (counted from 1), unless the request was
    /// told in that pass already; as [`Self::queue`].
    pub fn flush(
        &mut self,
        session: u32,
        request: u32,
        interface: [u8; IF_NAME_LEN],
        pass: u64,
    ) -> Option<u64> {
        let told = self
            .table
            .get_mut(&session)
            .and_then(|held| held.requests.get_mut(&request))
            .map(|held| core::mem::replace(&mut held.flushed_in, pass) == pass)?;
        if told {
            return None;
        }
        self.queue(session, request, &Entry::Flush { request, interface })
            .ring
    }

    /// Record how posting a doorbell to session `id` went.
    pub fn rang(&mut self, id: u32, result: Result<(), Errno>) {
        let Some(session) = self.table.get_mut(&id) else {
            return;
        };
        if result == Err(Errno::WouldBlock) {
            session.rung = false;
            session.ring_owed = true;
            if session.owed_at.is_none() {
                // Room for every open session was reserved at its open, so
                // neither of these allocates.
                session.owed_at = Some(self.owed.len());
                self.owed.push(id);
                self.rooms.owe(session.port);
            }
        } else {
            // Posted, or to a port that is gone and takes nothing — its
            // owner's exit closes the session.
            session.ring_owed = false;
            unlist(&mut self.table, &mut self.owed, &mut self.rooms, id);
        }
    }

    /// Bring the owner's watches on client ports in line with the doorbells
    /// owed: `watch` a port once a doorbell is owed there, reporting whether
    /// the watch took, and `unwatch` it once none is. Only ports whose count
    /// crossed zero since the last call are visited; a refused watch is tried
    /// again at that port's next owed doorbell.
    pub fn sync_rooms(&mut self, watch: &mut dyn FnMut(u64) -> bool, unwatch: &mut dyn FnMut(u64)) {
        self.rooms.sync(watch, unwatch);
    }

    /// Post every doorbell owed through `ring`, which is given the port and
    /// the session; one refused again for want of room stays owed.
    pub fn retry_owed(&mut self, ring: &mut dyn FnMut(u64, u32) -> Result<(), Errno>) {
        // Backwards, so the entry an unlisting moves into a visited place has
        // been visited too.
        for at in (0..self.owed.len()).rev() {
            let id = self.owed[at];
            let still_owed = self.table.get_mut(&id).is_some_and(|session| {
                if session.rung || !session.ring_owed {
                    return session.ring_owed;
                }
                let blocked = ring(session.port, session.id) == Err(Errno::WouldBlock);
                session.ring_owed = blocked;
                session.rung = !blocked;
                blocked
            });
            if !still_owed {
                unlist(&mut self.table, &mut self.owed, &mut self.rooms, id);
            }
        }
    }

    /// The requests the account `uid` holds.
    fn requests_of(&self, uid: u32) -> usize {
        self.accounts
            .get(&uid)
            .map_or(0, |account| account.requests)
    }

    /// Drop session `id` and everything counted for it.
    fn remove(&mut self, id: u32) -> Option<Session> {
        unlist(&mut self.table, &mut self.owed, &mut self.rooms, id);
        let session = self.table.remove(&id)?;
        self.queued = self.queued.saturating_sub(session.queue.len());
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
            while let Some(&id) = self.lost.last() {
                if let Some(request) = self
                    .requests
                    .get_mut(&id)
                    .filter(|request| request.loss == Loss::Owed)
                {
                    if writer.push(&Entry::Lost { request: id }).is_err() {
                        break;
                    }
                    request.loss = Loss::Told;
                    self.losses_owed -= 1;
                }
                self.lost.pop();
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

/// How many owed doorbells ring each client port, and whether the owner
/// watches the port for room.
#[derive(Debug, Default)]
struct Rooms {
    /// Sorted by port.
    ports: Vec<Room>,
    /// Ports changed since the owner last synced, each listed once.
    dirty: Vec<u64>,
}

#[derive(Debug)]
struct Room {
    port: u64,
    owed: u32,
    watched: bool,
    dirty: bool,
}

impl Rooms {
    /// Room for `sessions` more ports, so neither list allocates while that
    /// many sessions are owed.
    fn reserve(&mut self, sessions: usize) -> bool {
        self.ports.try_reserve(sessions).is_ok() && self.dirty.try_reserve(sessions).is_ok()
    }

    /// A doorbell came to be owed on `port`.
    fn owe(&mut self, port: u64) {
        let at = match self.ports.binary_search_by_key(&port, |room| room.port) {
            Ok(at) => at,
            Err(at) => {
                self.ports.insert(
                    at,
                    Room {
                        port,
                        owed: 0,
                        watched: false,
                        dirty: false,
                    },
                );
                at
            }
        };
        let room = &mut self.ports[at];
        room.owed += 1;
        if !room.watched {
            mark(&mut self.dirty, room);
        }
    }

    /// A doorbell owed on `port` is owed no more.
    fn unowe(&mut self, port: u64) {
        let Ok(at) = self.ports.binary_search_by_key(&port, |room| room.port) else {
            return;
        };
        let room = &mut self.ports[at];
        room.owed = room.owed.saturating_sub(1);
        if room.owed > 0 {
            return;
        }
        // A room still listed dirty is left for the sync to drop, so a port
        // owed again before then is never listed twice.
        if room.watched {
            mark(&mut self.dirty, room);
        } else if !room.dirty {
            self.ports.remove(at);
        }
    }

    fn sync(&mut self, watch: &mut dyn FnMut(u64) -> bool, unwatch: &mut dyn FnMut(u64)) {
        let Self { ports, dirty } = self;
        for port in dirty.drain(..) {
            let Ok(at) = ports.binary_search_by_key(&port, |room| room.port) else {
                continue;
            };
            let room = &mut ports[at];
            room.dirty = false;
            if room.owed > 0 {
                if !room.watched {
                    room.watched = watch(port);
                }
            } else {
                if room.watched {
                    unwatch(port);
                }
                ports.remove(at);
            }
        }
    }
}

/// List `room` as changed, once.
fn mark(dirty: &mut Vec<u64>, room: &mut Room) {
    if !room.dirty {
        room.dirty = true;
        dirty.push(room.port);
    }
}

/// Take session `id` off the owed list in O(1), moving the last entry into
/// its place and telling that entry's session where it went.
fn unlist(
    table: &mut HashMap<u32, Session, BuildFastHash>,
    owed: &mut Vec<u32>,
    rooms: &mut Rooms,
    id: u32,
) {
    let Some((at, port)) = table
        .get_mut(&id)
        .and_then(|session| Some((session.owed_at.take()?, session.port)))
    else {
        return;
    };
    rooms.unowe(port);
    owed.swap_remove(at);
    if let Some(&moved) = owed.get(at) {
        if let Some(session) = table.get_mut(&moved) {
            session.owed_at = Some(at);
        }
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
