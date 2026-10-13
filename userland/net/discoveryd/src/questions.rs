//! The questions the front asks through its decoder: each shared by every
//! request asking it, and what each holds on every link.
//!
//! A question's id is its slot, and a slot is reused only once the decoder
//! has said it stopped asking, or has been replaced: frames cross the pipe in
//! order, so no answer to the question the slot held can follow that word,
//! and an id names one question at a time. Ids therefore never run out.
//!
//! What a question holds is kept as keyed fingerprints of its answers, which
//! is all the front needs to hold the decoder to its word: an answer is added
//! once, renewed or retired only while held, and never past the most records
//! an honest engine's cache holds on one link — counted across every question
//! there, since each cached record answers at most one. The answers
//! themselves live in the requests' queues and in the decoder's cache, so a
//! request that joins a question already asked is brought up to date by a
//! replay from the decoder rather than from a second copy here.
//!
//! A question's requests are kept in runs — told every edge, owed the replay
//! in flight, owed the next replay, and muted once they take nothing more —
//! so an edge is offered to the live run alone, a replayed answer to the run
//! owed it, and requests joining while a replay is owed share it. An answer
//! renewed again within a second of its last telling is not told again.
//!
//! An answer finds its question, a request its question by name, and the
//! front the next question to ask, stop or replay, without a scan of the
//! table. Only a link edge and a replaced decoder, both rare, visit every
//! question.

use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::ops::Range;

use tairix_abi::discovery_ipc::{Answer, Change, Entry};
use tairix_abi::net_ipc::{address_parts, IF_NAME_LEN};
use tairix_abi::Errno;
use tairix_collections::HashMap;
use tairix_hash::{BuildFastHash, HashSeed, SipHash13};
use tairix_inline::{ArrayVec, IntrusiveList, Link, LinkStore};
use tairix_net::dns::Name;
use tairix_net::mdns::{MAX_QUESTIONS, MAX_RECORDS, PER_RECORD_INTERVAL_NS};

use crate::wire::Form;

/// The question slots an account is sure of however busy the table: a
/// sixteenth of what an engine asks at once.
pub const ACCOUNT_FLOOR: usize = MAX_QUESTIONS / 16;

/// The slots kept for accounts below their floor. One at or past it raises a
/// fresh question only while more than this many remain free, so accounts
/// asking all they may can together fill no more than three quarters of the
/// table, and four floors' worth is always left for the rest.
pub const FLOOR_RESERVE: usize = 4 * ACCOUNT_FLOOR;

const _: () = assert!(FLOOR_RESERVE < MAX_QUESTIONS);

/// Requests one session may have asking one question: two, so a replacement
/// can start before the request it replaces stops, and no more, since each
/// further copy costs every answer's fan-out and buys nothing.
pub const SESSION_COPIES: usize = 2;

/// What a subscription costs the decoder, for the caller to charge to the
/// account asking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Spend {
    /// A question no request asks yet, queried on every link.
    Ask,
    /// A replay of everything a question already asked holds.
    Replay,
}

/// Where in its question's subscribers a request sits, which says what it is
/// told.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Run {
    /// Told every edge as it arrives.
    Live,
    /// Owed the replay in flight. Until it ends, a live edge is already
    /// reflected in what the replay will tell, so it is told nothing else.
    Sent,
    /// Joined after the decoder was asked, and owed the next replay.
    Wanted,
    /// Its request lost answers and takes nothing more, so nothing is offered.
    Muted,
}

impl Run {
    /// The runs in order; each one's end is the next one's start.
    const ORDER: [Self; 4] = [Self::Live, Self::Sent, Self::Wanted, Self::Muted];

    const fn rank(self) -> usize {
        match self {
            Self::Live => 0,
            Self::Sent => 1,
            Self::Wanted => 2,
            Self::Muted => 3,
        }
    }
}

/// One request asking a question.
#[derive(Copy, Clone, Debug)]
struct Subscriber {
    session: u32,
    request: u32,
    /// The account the request is charged to.
    account: u32,
}

/// Where each subscription sits in its question's subscribers, by
/// `(question, session)`: that session's requests asking it, with each one's
/// index. Bounded by [`SESSION_COPIES`], which it also enforces.
type Places = HashMap<(u32, u32), ArrayVec<(u32, usize), SESSION_COPIES>, BuildFastHash>;

/// Record that `subscriber` of question `question` now sits at `at`.
fn place(places: &mut Places, question: u32, subscriber: &Subscriber, at: usize) {
    if let Some(copy) = places
        .get_mut(&(question, subscriber.session))
        .and_then(|copies| {
            copies
                .iter_mut()
                .find(|(request, _)| *request == subscriber.request)
        })
    {
        copy.1 = at;
    }
}

/// A question's subscribers, kept in the order of [`Run::ORDER`].
struct Subscribers {
    list: Vec<Subscriber>,
    /// Where the live, sent and wanted runs end; the muted run fills the rest.
    ends: [usize; 3],
}

impl Subscribers {
    const fn new() -> Self {
        Self {
            list: Vec::new(),
            ends: [0; 3],
        }
    }

    fn range(&self, run: Run) -> Range<usize> {
        let rank = run.rank();
        let start = rank.checked_sub(1).map_or(0, |before| self.ends[before]);
        let end = self.ends.get(rank).copied().unwrap_or(self.list.len());
        start..end
    }

    fn run_of(&self, at: usize) -> Run {
        Run::ORDER
            .into_iter()
            .find(|&run| self.range(run).contains(&at))
            .unwrap_or(Run::Muted)
    }

    fn swap(&mut self, places: &mut Places, question: u32, a: usize, b: usize) {
        if a == b {
            return;
        }
        self.list.swap(a, b);
        place(places, question, &self.list[a], a);
        place(places, question, &self.list[b], b);
    }

    /// Add `subscriber` at the end of `run`, returning where it sits. Room
    /// for it was reserved.
    fn insert(
        &mut self,
        places: &mut Places,
        question: u32,
        subscriber: Subscriber,
        run: Run,
    ) -> usize {
        self.list.push(subscriber);
        let mut at = self.list.len() - 1;
        for boundary in (run.rank()..3).rev() {
            let first_after = self.ends[boundary];
            self.swap(places, question, at, first_after);
            at = first_after;
            self.ends[boundary] += 1;
        }
        at
    }

    /// Move the subscriber at `at` to the start of the later run `to`,
    /// returning where it now sits. Only places at or after `at` change.
    fn demote(&mut self, places: &mut Places, question: u32, mut at: usize, to: Run) -> usize {
        for boundary in self.run_of(at).rank()..to.rank() {
            let last = self.ends[boundary] - 1;
            self.swap(places, question, at, last);
            at = last;
            self.ends[boundary] -= 1;
        }
        at
    }

    /// Take out the subscriber at `at`.
    fn remove(&mut self, places: &mut Places, question: u32, at: usize) -> Option<Subscriber> {
        let at = self.demote(places, question, at, Run::Muted);
        let last = self.list.len().checked_sub(1)?;
        self.swap(places, question, at, last);
        self.list.pop()
    }

    /// Offer every subscriber of `run` to `each`, muting the ones it says
    /// take nothing more.
    fn offer(
        &mut self,
        places: &mut Places,
        question: u32,
        run: Run,
        each: &mut dyn FnMut(u32, u32) -> bool,
    ) {
        // Backwards, so muting one moves only places already offered.
        for at in self.range(run).rev() {
            let subscriber = self.list[at];
            if !each(subscriber.session, subscriber.request) {
                self.demote(places, question, at, Run::Muted);
            }
        }
    }
}

/// What a question holds on one link: each answer's fingerprint, with when it
/// was last told.
struct Held {
    interface: [u8; IF_NAME_LEN],
    answers: HashMap<u64, u64, BuildFastHash>,
}

/// One question a request asks.
struct Question {
    form: Form,
    name: Name,
    subscribers: Subscribers,
    /// The token of the replay in flight, while one is.
    sent: Option<u32>,
    held: Vec<Held>,
    /// Whether the current decoder has been asked it.
    asked: bool,
}

/// Where one slot stands.
#[allow(
    clippy::large_enum_variant,
    reason = "a slot is live nearly all its life; boxing the question would cost an \
              allocation per question asked and an indirection per answer judged"
)]
enum State {
    /// Holds nothing; its id is free.
    Free,
    /// A question a request asks.
    Live(Question),
    /// Every request stopped asking. Its id stays taken, and charged to the
    /// account of the request that left it last, until the decoder says it
    /// stopped too; `sent` once the decoder has been told.
    Stopping { sent: bool, left_by: u32 },
}

/// One id's slot and the lists it is threaded on.
struct Slot {
    state: State,
    /// Its chain under the hash of its form and name, while live.
    name_link: Link,
    /// On the list of questions to ask, while live and not yet asked.
    ask_link: Link,
    /// On the list of stops to send, while stopping and not yet sent.
    stop_link: Link,
    /// On the list of replays to send, while requests want one and none is
    /// in flight.
    replay_link: Link,
}

impl Slot {
    const fn free() -> Self {
        Self {
            state: State::Free,
            name_link: Link::UNLINKED,
            ask_link: Link::UNLINKED,
            stop_link: Link::UNLINKED,
            replay_link: Link::UNLINKED,
        }
    }

    fn live(&self) -> Option<&Question> {
        match &self.state {
            State::Live(question) => Some(question),
            _ => None,
        }
    }
}

/// A view of the slots through one of their link fields.
macro_rules! link_view {
    ($name:ident, $field:ident) => {
        struct $name<'a>(&'a mut [Slot]);

        impl LinkStore for $name<'_> {
            fn link(&self, index: usize) -> Option<&Link> {
                self.0.get(index).map(|slot| &slot.$field)
            }

            fn link_mut(&mut self, index: usize) -> Option<&mut Link> {
                self.0.get_mut(index).map(|slot| &mut slot.$field)
            }
        }
    };
}

link_view!(NameView, name_link);
link_view!(AskView, ask_link);
link_view!(StopView, stop_link);
link_view!(ReplayView, replay_link);

/// What the front does with one answer from its decoder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Tell the requests asking this question that are live
    /// ([`Questions::tell`]).
    Tell(u32),
    /// Tell the requests owed this question's replay in flight.
    TellReplayed(u32),
    /// Nothing to tell: a renewal told less than a second ago.
    Absorbed,
    /// Nothing to tell: it crossed a stop, a flush, or a link edge in flight.
    Stale,
    /// No honest decoder sends it.
    Lie,
}

/// Every question the front asks.
pub struct Questions {
    slots: Vec<Slot>,
    /// Free ids, as a stack.
    free: Vec<u32>,
    /// Live and stopping slots: what the decoder may still be asking.
    in_use: usize,
    /// Chain head per keyed hash of a form and name.
    by_name: HashMap<u64, IntrusiveList, BuildFastHash>,
    /// Live questions the current decoder has not been asked.
    unasked: IntrusiveList,
    /// Stopping questions the current decoder has not been told of.
    stops: IntrusiveList,
    /// Questions owed a replay and with none in flight, oldest first.
    replays: IntrusiveList,
    places: Places,
    /// The token the next replay is sent under.
    next_token: u32,
    /// Answers held on each link, across every question.
    held_on: Vec<([u8; IF_NAME_LEN], usize)>,
    /// Per account, the subscriptions its requests hold and the questions it
    /// left stopping: the slots it keeps from everyone else.
    charges: HashMap<u32, usize, BuildFastHash>,
    key: HashSeed,
}

impl Questions {
    /// An empty table fingerprinting under `key`.
    #[must_use]
    pub fn new(key: HashSeed) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            in_use: 0,
            by_name: HashMap::with_hasher(BuildFastHash::new()),
            unasked: IntrusiveList::new(),
            stops: IntrusiveList::new(),
            replays: IntrusiveList::new(),
            places: HashMap::with_hasher(BuildFastHash::new()),
            next_token: 0,
            held_on: Vec::new(),
            charges: HashMap::with_hasher(BuildFastHash::new()),
            key,
        }
    }

    /// Panic unless the table's counts, runs and indexes agree with its
    /// slots: the oracle the fuzz harness holds the table to after every
    /// step.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_consistent(&self) {
        use alloc::collections::{BTreeMap, BTreeSet};
        let mut in_use = 0;
        let mut subscriptions = 0;
        let mut charges: BTreeMap<u32, usize> = BTreeMap::new();
        let free: BTreeSet<u32> = self.free.iter().copied().collect();
        assert_eq!(free.len(), self.free.len(), "a free id listed twice");
        for (index, slot) in self.slots.iter().enumerate() {
            let id = u32::try_from(index).expect("ids fit");
            match &slot.state {
                State::Free => assert!(free.contains(&id), "free slot {id} unlisted"),
                State::Stopping { left_by, .. } => {
                    in_use += 1;
                    *charges.entry(*left_by).or_default() += 1;
                }
                State::Live(question) => {
                    in_use += 1;
                    let runs = &question.subscribers;
                    assert!(
                        runs.ends[0] <= runs.ends[1]
                            && runs.ends[1] <= runs.ends[2]
                            && runs.ends[2] <= runs.list.len(),
                        "runs out of order"
                    );
                    assert!(
                        question.sent.is_some() || runs.range(Run::Sent).is_empty(),
                        "requests owed a replay never sent"
                    );
                    assert!(
                        question.asked || runs.range(Run::Live).len() == runs.ends[2],
                        "a replay owed on a question never asked"
                    );
                    let wants = question.asked
                        && question.sent.is_none()
                        && !runs.range(Run::Wanted).is_empty();
                    assert_eq!(
                        slot.replay_link.is_linked(),
                        wants,
                        "replay list out of step"
                    );
                    for (at, subscriber) in runs.list.iter().enumerate() {
                        subscriptions += 1;
                        *charges.entry(subscriber.account).or_default() += 1;
                        assert_eq!(
                            self.places
                                .get(&(id, subscriber.session))
                                .and_then(|copies| {
                                    copies
                                        .iter()
                                        .find(|(request, _)| *request == subscriber.request)
                                })
                                .map(|&(_, place)| place),
                            Some(at),
                            "a subscription out of place",
                        );
                    }
                }
            }
            if !matches!(slot.state, State::Live(_)) {
                assert!(
                    !slot.replay_link.is_linked(),
                    "a replay owed on no question"
                );
            }
            if !matches!(slot.state, State::Free) {
                assert!(!free.contains(&id), "live slot {id} listed free");
            }
        }
        assert_eq!(self.in_use, in_use);
        assert!(in_use <= MAX_QUESTIONS);
        assert_eq!(free.len() + in_use, self.slots.len());
        assert_eq!(
            self.places.values().map(ArrayVec::len).sum::<usize>(),
            subscriptions
        );
        assert!(self.places.values().all(|copies| !copies.is_empty()));
        let recorded: BTreeMap<u32, usize> = self.charges.iter().map(|(&a, &n)| (a, n)).collect();
        assert_eq!(
            recorded, charges,
            "charges disagree with what accounts hold"
        );
    }

    /// Panic unless the table holds nothing: no question live or stopping,
    /// and no subscription, charge, replay or held answer left behind.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_idle(&self) {
        self.assert_consistent();
        assert_eq!(self.in_use, 0, "a question outlived every request");
        assert!(self.by_name.is_empty() && self.unasked.is_empty() && self.stops.is_empty());
        assert!(self.charges.is_empty() && self.places.is_empty() && self.replays.is_empty());
        assert!(
            self.held_on.iter().all(|&(_, total)| total == 0),
            "answers held for no question"
        );
    }

    /// The slots `account` keeps: its subscriptions and the questions it left
    /// stopping.
    #[must_use]
    pub fn charged(&self, account: u32) -> usize {
        self.charges.get(&account).copied().unwrap_or(0)
    }

    /// Add request `(session, request)` of `account` to the question of `form`
    /// about `name`, asking it if no request yet does, and return its id. A
    /// request joining a question already asked is owed a replay, sharing one
    /// owed already. What it costs the decoder — a fresh question, or a fresh
    /// replay — is had only when `may` allows it, asked last so a charge is
    /// only ever made for a subscription that is.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] when a new question would pass the most an
    /// engine asks at once, or take one of the [`FLOOR_RESERVE`] for an
    /// account at its [`ACCOUNT_FLOOR`] already, or the session already has
    /// [`SESSION_COPIES`] requests asking it; [`Errno::AlreadyExists`] when this request asks it
    /// already; [`Errno::Busy`] when `may` refuses what it costs; and
    /// [`Errno::OutOfMemory`] when a table cannot grow.
    pub fn subscribe(
        &mut self,
        form: Form,
        name: Name,
        session: u32,
        request: u32,
        account: u32,
        may: &mut dyn FnMut(Spend) -> bool,
    ) -> Result<u32, Errno> {
        if !self.charges.contains_key(&account) && self.charges.try_reserve(1).is_err() {
            return Err(Errno::OutOfMemory);
        }
        let hash = self.name_hash(form, &name);
        if let Some(id) = self.find(hash, form, &name) {
            self.join(id, session, request, account, may)?;
            self.charge(account);
            return Ok(id);
        }
        let reserved =
            self.charged(account) >= ACCOUNT_FLOOR && self.in_use + FLOOR_RESERVE >= MAX_QUESTIONS;
        if self.in_use >= MAX_QUESTIONS || reserved {
            return Err(Errno::LimitExceeded);
        }
        let mut subscribers = Subscribers::new();
        let reused = self.free.last().copied();
        // A new slot reserves its own place on the free stack, so releasing
        // it later cannot fail and strand its id.
        let room = subscribers.list.try_reserve(1).is_ok()
            && self.places.try_reserve(1).is_ok()
            && self.by_name.try_reserve(1).is_ok()
            && (reused.is_some()
                || (self.slots.try_reserve(1).is_ok()
                    && self.free.try_reserve(self.slots.len() + 1).is_ok()));
        if !room {
            return Err(Errno::OutOfMemory);
        }
        let id = match reused {
            Some(id) => id,
            None => u32::try_from(self.slots.len()).map_err(|_| Errno::LimitExceeded)?,
        };
        if !may(Spend::Ask) {
            return Err(Errno::Busy);
        }
        if reused.is_some() {
            self.free.pop();
        } else {
            self.slots.push(Slot::free());
        }
        let index = id as usize;
        let subscriber = Subscriber {
            session,
            request,
            account,
        };
        let at = subscribers.insert(&mut self.places, id, subscriber, Run::Live);
        self.slots[index].state = State::Live(Question {
            form,
            name,
            subscribers,
            sent: None,
            held: Vec::new(),
            asked: false,
        });
        self.in_use += 1;
        // Room for the head was reserved above.
        if !self.by_name.contains_key(&hash) {
            let _ = self.by_name.try_insert(hash, IntrusiveList::new());
        }
        if let Some(chain) = self.by_name.get_mut(&hash) {
            let _ = chain.push_back(&mut NameView(&mut self.slots), index);
        }
        let _ = self.unasked.push_back(&mut AskView(&mut self.slots), index);
        let mut copies = ArrayVec::new();
        let _ = copies.try_push((request, at));
        let _ = self.places.try_insert((id, session), copies);
        self.charge(account);
        Ok(id)
    }

    /// Take request `(session, request)` off question `id`, retiring the
    /// question when no request is left asking it.
    pub fn unsubscribe(&mut self, id: u32, session: u32, request: u32) {
        let index = id as usize;
        let Self {
            slots,
            places,
            replays,
            ..
        } = self;
        let Some(State::Live(question)) = slots.get_mut(index).map(|slot| &mut slot.state) else {
            return;
        };
        let Some((copy, at)) = places.get(&(id, session)).and_then(|copies| {
            copies
                .iter()
                .enumerate()
                .find(|(_, held)| held.0 == request)
                .map(|(copy, held)| (copy, held.1))
        }) else {
            return;
        };
        if !question
            .subscribers
            .list
            .get(at)
            .is_some_and(|held| held.session == session && held.request == request)
        {
            return;
        }
        let Some(gone) = question.subscribers.remove(places, id, at) else {
            return;
        };
        if let Some(copies) = places.get_mut(&(id, session)) {
            copies.swap_remove(copy);
            if copies.is_empty() {
                places.remove(&(id, session));
            }
        }
        let still_wanted = !question.subscribers.range(Run::Wanted).is_empty();
        if !still_wanted && slots[index].replay_link.is_linked() {
            let _ = replays.unlink(&mut ReplayView(slots), index);
        }
        let Some(State::Live(question)) = self.slots.get(index).map(|slot| &slot.state) else {
            return;
        };
        if !question.subscribers.list.is_empty() {
            self.uncharge(gone.account);
            return;
        }
        let (asked, form, name) = (question.asked, question.form, question.name);
        let hash = self.name_hash(form, &name);
        self.release_held(index);
        if let Some(chain) = self.by_name.get_mut(&hash) {
            let _ = chain.unlink(&mut NameView(&mut self.slots), index);
            if chain.is_empty() {
                self.by_name.remove(&hash);
            }
        }
        if asked {
            // Its id, and its charge, are held until the decoder says it
            // stopped too.
            self.slots[index].state = State::Stopping {
                sent: false,
                left_by: gone.account,
            };
            let _ = self.stops.push_back(&mut StopView(&mut self.slots), index);
        } else {
            let _ = self.unasked.unlink(&mut AskView(&mut self.slots), index);
            self.release(index);
            self.uncharge(gone.account);
        }
    }

    /// The next question the decoder should stop asking, if any.
    #[must_use]
    pub fn next_stop(&self) -> Option<u32> {
        self.stops
            .front()
            .and_then(|index| u32::try_from(index).ok())
    }

    /// The decoder was told to stop `id`.
    pub fn stopped(&mut self, id: u32) {
        let index = id as usize;
        if let Some(State::Stopping { sent, .. }) =
            self.slots.get_mut(index).map(|slot| &mut slot.state)
        {
            if !*sent {
                *sent = true;
                let _ = self.stops.unlink(&mut StopView(&mut self.slots), index);
            }
        }
    }

    /// The decoder says it stopped asking `id`, freeing the id; `false` when
    /// it was never told to.
    pub fn on_stopped(&mut self, id: u32) -> bool {
        let index = id as usize;
        match self.slots.get(index).map(|slot| &slot.state) {
            Some(&State::Stopping {
                sent: true,
                left_by,
            }) => {
                self.release(index);
                self.uncharge(left_by);
                true
            }
            _ => false,
        }
    }

    /// The next question the decoder should be asked, if any.
    #[must_use]
    pub fn next_ask(&self) -> Option<(u32, Form, Name)> {
        let index = self.unasked.front()?;
        let question = self.slots.get(index)?.live()?;
        Some((u32::try_from(index).ok()?, question.form, question.name))
    }

    /// The decoder was asked `id`.
    pub fn asked(&mut self, id: u32) {
        let index = id as usize;
        if let Some(State::Live(question)) = self.slots.get_mut(index).map(|slot| &mut slot.state) {
            if !question.asked {
                question.asked = true;
                let _ = self.unasked.unlink(&mut AskView(&mut self.slots), index);
            }
        }
    }

    /// The next replay the decoder should be asked for, as
    /// `(question, token)`, if any.
    #[must_use]
    pub fn next_replay(&self) -> Option<(u32, u32)> {
        let index = self.replays.front()?;
        Some((u32::try_from(index).ok()?, self.next_token))
    }

    /// The replay of `id` was sent under `token`: the requests that wanted it
    /// are owed it now, and later joiners want the next.
    pub fn replay_sent(&mut self, id: u32, token: u32) {
        let index = id as usize;
        if self
            .slots
            .get(index)
            .is_some_and(|slot| slot.replay_link.is_linked())
        {
            let _ = self.replays.unlink(&mut ReplayView(&mut self.slots), index);
        }
        let Some(State::Live(question)) = self.slots.get_mut(index).map(|slot| &mut slot.state)
        else {
            return;
        };
        let runs = &mut question.subscribers;
        runs.ends[1] = runs.ends[2];
        question.sent = Some(token);
        self.next_token = token.wrapping_add(1);
    }

    /// The replay `token` of `id` has ended: from here the requests it was
    /// owed to are told every edge, and the next replay, if wanted, is due.
    pub fn replayed(&mut self, id: u32, token: u32) {
        let index = id as usize;
        let Some(State::Live(question)) = self.slots.get_mut(index).map(|slot| &mut slot.state)
        else {
            return;
        };
        if question.sent != Some(token) {
            return;
        }
        question.sent = None;
        let runs = &mut question.subscribers;
        runs.ends[0] = runs.ends[1];
        if !runs.range(Run::Wanted).is_empty() {
            let _ = self
                .replays
                .push_back(&mut ReplayView(&mut self.slots), index);
        }
    }

    /// Tell `each` request `verdict` names, as `(session, request)`. One `each`
    /// says takes nothing more is muted, and offered nothing after.
    pub fn tell(&mut self, verdict: Verdict, each: &mut dyn FnMut(u32, u32) -> bool) {
        let (id, run) = match verdict {
            Verdict::Tell(id) => (id, Run::Live),
            Verdict::TellReplayed(id) => (id, Run::Sent),
            Verdict::Absorbed | Verdict::Stale | Verdict::Lie => return,
        };
        let Self { slots, places, .. } = self;
        if let Some(State::Live(question)) = slots.get_mut(id as usize).map(|slot| &mut slot.state)
        {
            question.subscribers.offer(places, id, run, each);
        }
    }

    /// Judge one answer the decoder sent at `now`. `linked` says whether the
    /// decoder has carried out the edge that brought `interface` up and not
    /// been told it went down since.
    pub fn on_answer(&mut self, entry: &Entry<'_>, linked: bool, now: u64) -> Verdict {
        let Entry::Answer {
            request,
            interface,
            change,
            answer,
            ..
        } = entry
        else {
            return Verdict::Lie;
        };
        let id = *request;
        let print = fingerprint(self.key, answer);
        let question = match self.slots.get_mut(id as usize).map(|slot| &mut slot.state) {
            None | Some(State::Free) => return Verdict::Lie,
            Some(State::Stopping { .. }) => return Verdict::Stale,
            Some(State::Live(question)) => question,
        };
        if !linked || !question.asked {
            return Verdict::Stale;
        }
        let total = held_total(&mut self.held_on, *interface);
        let at = if let Some(at) = question
            .held
            .iter()
            .position(|held| held.interface == *interface)
        {
            at
        } else {
            if *change != Change::Added || question.held.try_reserve(1).is_err() {
                return Verdict::Stale;
            }
            question.held.push(Held {
                interface: *interface,
                answers: HashMap::with_hasher(BuildFastHash::new()),
            });
            question.held.len() - 1
        };
        let held = &mut question.held[at];
        match change {
            Change::Added => {
                if held.answers.contains_key(&print) {
                    // A duplicate addition: an edge that crossed a flush the
                    // front made.
                    return Verdict::Stale;
                }
                // An honest engine's cache holds no more on one link, and
                // each record there answers one question at most.
                let total = match total {
                    None => return Verdict::Stale,
                    Some(total) if *total >= MAX_RECORDS => return Verdict::Lie,
                    Some(total) => total,
                };
                if !held
                    .answers
                    .try_insert(print, now)
                    .is_ok_and(|was| was.is_none())
                {
                    return Verdict::Stale;
                }
                *total += 1;
            }
            Change::Refreshed => {
                let Some(told) = held.answers.get_mut(&print) else {
                    return Verdict::Stale;
                };
                // An honest responder repeats a record at most this often,
                // so a renewal sooner is a copy, and saying it again tells a
                // request nothing.
                if now < told.saturating_add(PER_RECORD_INTERVAL_NS) {
                    return Verdict::Absorbed;
                }
                *told = now;
            }
            Change::Retired => {
                if held.answers.remove(&print).is_none() {
                    return Verdict::Stale;
                }
                if let Some(total) = total {
                    *total = total.saturating_sub(1);
                }
                if held.answers.is_empty() {
                    question.held.swap_remove(at);
                }
            }
        }
        Verdict::Tell(id)
    }

    /// Judge one answer the replay `token` of the question it names replayed:
    /// only what the question holds is told, to the requests owed the replay.
    #[must_use]
    pub fn on_held(&self, token: u32, entry: &Entry<'_>) -> Verdict {
        let Entry::Answer {
            request,
            interface,
            answer,
            ..
        } = entry
        else {
            return Verdict::Lie;
        };
        let question = match self.slots.get(*request as usize).map(|slot| &slot.state) {
            None | Some(State::Free) => return Verdict::Lie,
            Some(State::Stopping { .. }) => return Verdict::Stale,
            Some(State::Live(question)) => question,
        };
        let print = fingerprint(self.key, answer);
        let holds = question
            .held
            .iter()
            .any(|held| held.interface == *interface && held.answers.contains_key(&print));
        if holds && question.sent == Some(token) {
            Verdict::TellReplayed(*request)
        } else {
            Verdict::Stale
        }
    }

    /// The link `interface` went down: every question forgets what it held
    /// there, naming through `each` every request that held something and
    /// still takes answers — once per question it asks, so a request asking
    /// two may be named twice.
    pub fn flush_link(&mut self, interface: [u8; IF_NAME_LEN], each: &mut dyn FnMut(u32, u32)) {
        if let Some(at) = self.held_on.iter().position(|(name, _)| *name == interface) {
            self.held_on.swap_remove(at);
        }
        for slot in &mut self.slots {
            let State::Live(question) = &mut slot.state else {
                continue;
            };
            let Some(at) = question
                .held
                .iter()
                .position(|held| held.interface == interface)
            else {
                continue;
            };
            question.held.swap_remove(at);
            let runs = &question.subscribers;
            for subscriber in &runs.list[..runs.ends[2]] {
                each(subscriber.session, subscriber.request);
            }
        }
    }

    /// A new decoder knows nothing: every question is to be asked again,
    /// every replay is void — the new decoder tells every request everything
    /// afresh — every stop is moot, and everything held is forgotten, link by
    /// link, each request that held something named through `each` as
    /// [`Self::flush_link`] names it.
    pub fn forget_decoder(&mut self, each: &mut dyn FnMut(u32, u32, [u8; IF_NAME_LEN])) {
        self.replays.clear(&mut ReplayView(&mut self.slots));
        for index in 0..self.slots.len() {
            match &mut self.slots[index].state {
                State::Free => {}
                &mut State::Stopping { sent, left_by } => {
                    if !sent {
                        let _ = self.stops.unlink(&mut StopView(&mut self.slots), index);
                    }
                    // The new decoder never asked it, so no word will come.
                    self.release(index);
                    self.uncharge(left_by);
                }
                State::Live(question) => {
                    let runs = &mut question.subscribers;
                    runs.ends = [runs.ends[2]; 3];
                    question.sent = None;
                    if question.asked {
                        question.asked = false;
                        let _ = self.unasked.push_back(&mut AskView(&mut self.slots), index);
                    }
                }
            }
        }
        // Every link anything is held on is counted there, so this forgets it
        // all.
        while let Some(&(interface, _)) = self.held_on.last() {
            self.flush_link(interface, &mut |session, request| {
                each(session, request, interface);
            });
        }
    }

    /// Whether no question is live or still being stopped.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.in_use == 0
    }

    /// Add a request to the live question `id`: told every edge when the
    /// decoder has not been asked it yet, and otherwise owed a replay.
    fn join(
        &mut self,
        id: u32,
        session: u32,
        request: u32,
        account: u32,
        may: &mut dyn FnMut(Spend) -> bool,
    ) -> Result<(), Errno> {
        let index = id as usize;
        let Self {
            slots,
            places,
            replays,
            ..
        } = self;
        let Some(State::Live(question)) = slots.get_mut(index).map(|slot| &mut slot.state) else {
            return Err(Errno::NotFound);
        };
        let held = places.get(&(id, session));
        if held.is_some_and(|copies| copies.iter().any(|&(asking, _)| asking == request)) {
            return Err(Errno::AlreadyExists);
        }
        let copies = held.map_or(0, ArrayVec::len);
        if copies >= SESSION_COPIES {
            return Err(Errno::LimitExceeded);
        }
        question
            .subscribers
            .list
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        if copies == 0 {
            places.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        }
        let run = if question.asked {
            Run::Wanted
        } else {
            Run::Live
        };
        let fresh_replay = run == Run::Wanted && question.subscribers.range(Run::Wanted).is_empty();
        // Last, so a replay is only ever charged for a join that happens.
        if fresh_replay && !may(Spend::Replay) {
            return Err(Errno::Busy);
        }
        let subscriber = Subscriber {
            session,
            request,
            account,
        };
        let at = question.subscribers.insert(places, id, subscriber, run);
        let due = fresh_replay && question.sent.is_none();
        if let Some(copies) = places.get_mut(&(id, session)) {
            let _ = copies.try_push((request, at));
        } else {
            let mut copies = ArrayVec::new();
            let _ = copies.try_push((request, at));
            let _ = places.try_insert((id, session), copies);
        }
        if due {
            let _ = replays.push_back(&mut ReplayView(slots), index);
        }
        Ok(())
    }

    /// The live question of `form` about `name`, hashed as `hash`.
    fn find(&self, hash: u64, form: Form, name: &Name) -> Option<u32> {
        let mut next = self.by_name.get(&hash).and_then(IntrusiveList::front);
        while let Some(index) = next {
            let slot = self.slots.get(index)?;
            if slot
                .live()
                .is_some_and(|question| question.form == form && question.name == *name)
            {
                return u32::try_from(index).ok();
            }
            next = slot.name_link.next();
        }
        None
    }

    /// The keyed hash a question of `form` about `name` is chained by.
    fn name_hash(&self, form: Form, name: &Name) -> u64 {
        let mut hash = SipHash13::new(self.key);
        hash.write_u8(form as u8);
        name.hash(&mut hash);
        hash.finish()
    }

    /// Take what the question in slot `index` holds off each link's count.
    fn release_held(&mut self, index: usize) {
        let Some(State::Live(question)) = self.slots.get_mut(index).map(|slot| &mut slot.state)
        else {
            return;
        };
        for held in question.held.drain(..) {
            if let Some(total) = held_total(&mut self.held_on, held.interface) {
                *total = total.saturating_sub(held.answers.len());
            }
        }
    }

    /// One more slot kept by `account`; room for its entry was made before
    /// anything changed.
    fn charge(&mut self, account: u32) {
        match self.charges.get_mut(&account) {
            Some(held) => *held += 1,
            None => {
                let _ = self.charges.try_insert(account, 1);
            }
        }
    }

    /// One slot fewer kept by `account`.
    fn uncharge(&mut self, account: u32) {
        if let Some(held) = self.charges.get_mut(&account) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                self.charges.remove(&account);
            }
        }
    }

    /// Free slot `index` for another question.
    fn release(&mut self, index: usize) {
        self.slots[index].state = State::Free;
        self.in_use = self.in_use.saturating_sub(1);
        if let (Ok(id), Ok(())) = (u32::try_from(index), self.free.try_reserve(1)) {
            self.free.push(id);
        }
    }
}

/// The count of answers held on `interface`, made when a link holds its
/// first; `None` when the table cannot grow.
fn held_total(
    held_on: &mut Vec<([u8; IF_NAME_LEN], usize)>,
    interface: [u8; IF_NAME_LEN],
) -> Option<&mut usize> {
    let at = if let Some(at) = held_on.iter().position(|(name, _)| *name == interface) {
        at
    } else {
        held_on.try_reserve(1).ok()?;
        held_on.push((interface, 0));
        held_on.len() - 1
    };
    held_on.get_mut(at).map(|(_, total)| total)
}

/// The identity of one answer, keyed so a peer cannot choose two answers that
/// collide. Names compare without ASCII case, as the cache holds them.
fn fingerprint(key: HashSeed, answer: &Answer<'_>) -> u64 {
    let mut hash = SipHash13::new(key);
    let folded = |hash: &mut SipHash13, bytes: &[u8]| {
        for &byte in bytes {
            hash.write_u8(byte.to_ascii_lowercase());
        }
    };
    match *answer {
        Answer::Instance { label } => {
            hash.write_u8(1);
            folded(&mut hash, label);
        }
        Answer::Service {
            priority,
            weight,
            port,
            target,
        } => {
            hash.write_u8(2);
            hash.write_u16(priority);
            hash.write_u16(weight);
            hash.write_u16(port);
            folded(&mut hash, target);
        }
        Answer::Text { octets } => {
            hash.write_u8(3);
            hash.write(octets);
        }
        Answer::Address { address } => {
            let (family, octets) = address_parts(address);
            hash.write_u8(4);
            hash.write_u8(family.as_u8());
            hash.write(&octets);
        }
        Answer::Pointer { target } => {
            hash.write_u8(5);
            folded(&mut hash, target);
        }
        Answer::Type { service } => {
            hash.write_u8(6);
            hash.write_u8(service.transport.as_u8());
            folded(&mut hash, service.name);
        }
    }
    hash.finish()
}

#[cfg(test)]
#[path = "questions_tests.rs"]
mod tests;
