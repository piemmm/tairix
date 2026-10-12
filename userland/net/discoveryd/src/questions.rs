//! The questions the front asks through its decoder: each shared by every
//! request asking it, and what each holds on every link.
//!
//! A question's id is its slot, and a slot is reused only once the decoder
//! has said it stopped asking: frames cross the pipe in order, so no answer
//! to the question the slot held can follow that word, and an id names one
//! question at a time. Ids therefore never run out.
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
//! An answer finds its question, a request its question by name, and the
//! front the next question to ask or stop, without a scan of the table.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};

use tairix_abi::discovery_ipc::{Answer, Change, Entry};
use tairix_abi::net_ipc::{address_parts, IF_NAME_LEN};
use tairix_abi::Errno;
use tairix_collections::{HashMap, HashSet};
use tairix_hash::{BuildFastHash, HashSeed, SipHash13};
use tairix_inline::{IntrusiveList, Link, LinkStore};
use tairix_net::dns::Name;
use tairix_net::mdns::{MAX_QUESTIONS, MAX_RECORDS};

use crate::wire::Form;

/// Where one request's view of a question stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Replay {
    /// It is told every edge as it arrives.
    Live,
    /// It joined after the decoder was asked, and is owed what the question
    /// already holds; the replay has not been sent.
    Wanted(u32),
    /// The replay was sent. Until it ends, a live edge is already reflected
    /// in what it will be told, so it is told nothing else.
    Sent(u32),
}

impl Replay {
    const fn token(self) -> Option<u32> {
        match self {
            Self::Live => None,
            Self::Wanted(token) | Self::Sent(token) => Some(token),
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
    replay: Replay,
}

/// Whose replay a token is: the question it replays and the request owed it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Owed {
    question: u32,
    session: u32,
    request: u32,
}

/// What a question holds on one link.
struct Held {
    interface: [u8; IF_NAME_LEN],
    answers: HashSet<u64, BuildFastHash>,
}

/// One question a request asks.
struct Question {
    form: Form,
    name: Name,
    subscribers: Vec<Subscriber>,
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
}

impl Slot {
    const fn free() -> Self {
        Self {
            state: State::Free,
            name_link: Link::UNLINKED,
            ask_link: Link::UNLINKED,
            stop_link: Link::UNLINKED,
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

/// What the front does with one answer from its decoder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Tell every request asking this question that is not owed a replay
    /// ([`Questions::told`]).
    Tell(u32),
    /// Tell this one request: `(session, request)`.
    TellOne(u32, u32),
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
    /// Replays owed, oldest first, as `(question, token)`; an entry whose
    /// token has gone is skipped.
    replays: VecDeque<(u32, u32)>,
    /// Whose replay each live token is.
    tokens: HashMap<u32, Owed, BuildFastHash>,
    /// Each subscription's place in its question's subscribers, by
    /// `(question, session, request)`, so neither leaving nor a replayed
    /// answer walks a question every request may share.
    positions: HashMap<(u32, u32, u32), u32, BuildFastHash>,
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
            replays: VecDeque::new(),
            tokens: HashMap::with_hasher(BuildFastHash::new()),
            positions: HashMap::with_hasher(BuildFastHash::new()),
            next_token: 0,
            held_on: Vec::new(),
            charges: HashMap::with_hasher(BuildFastHash::new()),
            key,
        }
    }

    /// The slots `account` keeps: its subscriptions and the questions it left
    /// stopping.
    #[must_use]
    pub fn charged(&self, account: u32) -> usize {
        self.charges.get(&account).copied().unwrap_or(0)
    }

    /// Add request `(session, request)` of `account` to the question of `form`
    /// about `name`, asking it if no request yet does, and return its id.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] when a new question would pass the most an
    /// engine asks at once, and [`Errno::OutOfMemory`] when a table cannot
    /// grow.
    pub fn subscribe(
        &mut self,
        form: Form,
        name: Name,
        session: u32,
        request: u32,
        account: u32,
    ) -> Result<u32, Errno> {
        if !self.charges.contains_key(&account) && self.charges.try_reserve(1).is_err() {
            return Err(Errno::OutOfMemory);
        }
        let hash = self.name_hash(form, &name);
        if let Some(id) = self.find(hash, form, &name) {
            self.join(id, session, request, account)?;
            self.charge(account);
            return Ok(id);
        }
        if self.in_use >= MAX_QUESTIONS {
            return Err(Errno::LimitExceeded);
        }
        let mut subscribers = Vec::new();
        let reused = self.free.last().copied();
        // A new slot reserves its own place on the free stack, so releasing
        // it later cannot fail and strand its id.
        let room = subscribers.try_reserve(1).is_ok()
            && self.positions.try_reserve(1).is_ok()
            && self.by_name.try_reserve(1).is_ok()
            && (reused.is_some()
                || (self.slots.try_reserve(1).is_ok()
                    && self.free.try_reserve(self.slots.len() + 1).is_ok()));
        if !room {
            return Err(Errno::OutOfMemory);
        }
        let id = if let Some(id) = reused {
            self.free.pop();
            id
        } else {
            self.slots.push(Slot::free());
            u32::try_from(self.slots.len() - 1).map_err(|_| Errno::LimitExceeded)?
        };
        let index = id as usize;
        subscribers.push(Subscriber {
            session,
            request,
            account,
            replay: Replay::Live,
        });
        self.slots[index].state = State::Live(Question {
            form,
            name,
            subscribers,
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
        let _ = self.positions.try_insert((id, session, request), 0);
        self.charge(account);
        Ok(id)
    }

    /// Take request `(session, request)` off question `id`, retiring the
    /// question when no request is left asking it.
    pub fn unsubscribe(&mut self, id: u32, session: u32, request: u32) {
        let index = id as usize;
        let Some(State::Live(question)) = self.slots.get_mut(index).map(|slot| &mut slot.state)
        else {
            return;
        };
        let Some(place) = self.positions.remove(&(id, session, request)) else {
            return;
        };
        let at = place as usize;
        if !question
            .subscribers
            .get(at)
            .is_some_and(|held| held.session == session && held.request == request)
        {
            return;
        }
        let gone = question.subscribers.swap_remove(at);
        if let Some(moved) = question.subscribers.get(at) {
            if let Some(moved_to) = self.positions.get_mut(&(id, moved.session, moved.request)) {
                *moved_to = place;
            }
        }
        if let Some(token) = gone.replay.token() {
            self.tokens.remove(&token);
        }
        if !question.subscribers.is_empty() {
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
    pub fn next_replay(&mut self) -> Option<(u32, u32)> {
        while let Some(&(id, token)) = self.replays.front() {
            if self.tokens.get(&token).map(|owed| owed.question) == Some(id) {
                return Some((id, token));
            }
            self.replays.pop_front();
        }
        None
    }

    /// The replay `token` was sent.
    pub fn replay_sent(&mut self, token: u32) {
        if self
            .replays
            .front()
            .is_some_and(|&(_, front)| front == token)
        {
            self.replays.pop_front();
        }
        if let Some(subscriber) = self.subscriber_mut(token) {
            subscriber.replay = Replay::Sent(token);
        }
    }

    /// The replay `token` has ended: from here its request is told every edge.
    pub fn replayed(&mut self, token: u32) {
        if let Some(subscriber) = self.subscriber_mut(token) {
            subscriber.replay = Replay::Live;
        }
        self.tokens.remove(&token);
    }

    /// The requests [`Verdict::Tell`] names for `id`: every one asking it
    /// that is not owed a replay.
    pub fn told(&self, id: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.slots
            .get(id as usize)
            .and_then(Slot::live)
            .into_iter()
            .flat_map(|question| question.subscribers.iter())
            .filter(|subscriber| subscriber.replay == Replay::Live)
            .map(|subscriber| (subscriber.session, subscriber.request))
    }

    /// Judge one answer the decoder sent. `linked` says whether the decoder
    /// has carried out the edge that brought `interface` up and not been told
    /// it went down since.
    pub fn on_answer(&mut self, entry: &Entry<'_>, linked: bool) -> Verdict {
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
                answers: HashSet::with_hasher(BuildFastHash::new()),
            });
            question.held.len() - 1
        };
        let held = &mut question.held[at];
        match change {
            Change::Added => {
                if held.answers.contains(&print) {
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
                if !held.answers.try_insert(print).is_ok_and(|added| added) {
                    return Verdict::Stale;
                }
                *total += 1;
            }
            Change::Refreshed => {
                if !held.answers.contains(&print) {
                    return Verdict::Stale;
                }
            }
            Change::Retired => {
                if !held.answers.remove(&print) {
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

    /// Judge one replayed answer for the replay `token`: only a request still
    /// owed it is told, and only what the question holds.
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
            .any(|held| held.interface == *interface && held.answers.contains(&print));
        let owed = self
            .tokens
            .get(&token)
            .filter(|owed| owed.question == *request)
            .and_then(|owed| {
                let at = *self
                    .positions
                    .get(&(owed.question, owed.session, owed.request))?;
                question.subscribers.get(at as usize)
            })
            .filter(|subscriber| subscriber.replay == Replay::Sent(token));
        match (holds, owed) {
            (true, Some(subscriber)) => Verdict::TellOne(subscriber.session, subscriber.request),
            _ => Verdict::Stale,
        }
    }

    /// Forget everything held on `interface`, returning each request that
    /// held something there, once.
    pub fn flush_link(&mut self, interface: [u8; IF_NAME_LEN]) -> Vec<(u32, u32)> {
        let mut flushed = Vec::new();
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
            for subscriber in &question.subscribers {
                note(&mut flushed, (subscriber.session, subscriber.request));
            }
        }
        flushed.sort_unstable();
        flushed.dedup();
        flushed
    }

    /// A new decoder knows nothing: every question is to be asked again,
    /// every replay is void — the new decoder tells every request everything
    /// afresh — every stop is moot, and everything held is forgotten. Returns
    /// each request and link that held something, once.
    pub fn forget_decoder(&mut self) -> Vec<(u32, u32, [u8; IF_NAME_LEN])> {
        self.tokens.clear();
        self.replays.clear();
        self.held_on.clear();
        let mut flushed = Vec::new();
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
                    for subscriber in &mut question.subscribers {
                        subscriber.replay = Replay::Live;
                    }
                    for held in question.held.drain(..) {
                        for subscriber in &question.subscribers {
                            note(
                                &mut flushed,
                                (subscriber.session, subscriber.request, held.interface),
                            );
                        }
                    }
                    if question.asked {
                        question.asked = false;
                        let _ = self.unasked.push_back(&mut AskView(&mut self.slots), index);
                    }
                }
            }
        }
        flushed.sort_unstable();
        flushed.dedup();
        flushed
    }

    /// Whether no question is live or still being stopped.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.in_use == 0
    }

    /// Add a request to the live question `id`, owing it a replay when the
    /// decoder has been asked already.
    fn join(&mut self, id: u32, session: u32, request: u32, account: u32) -> Result<(), Errno> {
        let Some(State::Live(question)) =
            self.slots.get_mut(id as usize).map(|slot| &mut slot.state)
        else {
            return Err(Errno::NotFound);
        };
        question
            .subscribers
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        self.positions
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let replay = if question.asked {
            if self.replays.len() >= 2 * self.tokens.len() + 64 {
                let tokens = &self.tokens;
                self.replays.retain(|(owner, token)| {
                    tokens.get(token).map(|owed| owed.question) == Some(*owner)
                });
            }
            if self.replays.try_reserve(1).is_err() || self.tokens.try_reserve(1).is_err() {
                return Err(Errno::OutOfMemory);
            }
            let tokens = &self.tokens;
            let token = fresh_token(&mut self.next_token, |token| tokens.contains_key(&token));
            let owed = Owed {
                question: id,
                session,
                request,
            };
            let _ = self.tokens.try_insert(token, owed);
            self.replays.push_back((id, token));
            Replay::Wanted(token)
        } else {
            Replay::Live
        };
        let at = u32::try_from(question.subscribers.len()).map_err(|_| Errno::LimitExceeded)?;
        question.subscribers.push(Subscriber {
            session,
            request,
            account,
            replay,
        });
        let _ = self.positions.try_insert((id, session, request), at);
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

    /// The subscriber owed replay `token`.
    fn subscriber_mut(&mut self, token: u32) -> Option<&mut Subscriber> {
        let owed = *self.tokens.get(&token)?;
        let at = *self
            .positions
            .get(&(owed.question, owed.session, owed.request))?;
        let State::Live(question) = &mut self.slots.get_mut(owed.question as usize)?.state else {
            return None;
        };
        question
            .subscribers
            .get_mut(at as usize)
            .filter(|subscriber| subscriber.replay.token() == Some(token))
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

    /// Free slot `index` for another question.
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

/// The first token from `next` on that `in_use` does not hold, advancing
/// `next` past it.
fn fresh_token(next: &mut u32, in_use: impl Fn(u32) -> bool) -> u32 {
    loop {
        let token = *next;
        *next = next.wrapping_add(1);
        if !in_use(token) {
            return token;
        }
    }
}

fn note<T>(list: &mut Vec<T>, item: T) {
    if list.try_reserve(1).is_ok() {
        list.push(item);
    }
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
