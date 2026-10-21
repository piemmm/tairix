//! The user's own sound choices (`plans/SOUND.md`): kept in this session's
//! private app-data scope, handed to the cue authority while this session
//! holds the seat, and read and changed by the Settings application's Sound
//! Effects pane through the pinboard rendezvous.
//!
//! [`Effects`] is pure. The store, the files and the cue authority are reached
//! by the session's worker, which carries out what this asks for and hands
//! back what came of it, so the compositing loop never waits on any of them.
//! A chosen file's path never leaves this process: the pane is told its name,
//! and the cue authority is handed the open file.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::origin::ProcId;
use tairix_abi::pinboard_ipc::{SoundFile, SoundReading};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::Errno;
use tairix_appconf::{Refusal, Registry, SettingPath};
use tairix_appdata::{AppDataHost, Settings as SettingsStore};
use tairix_soundtheme::{layered, SoundKey, SoundSettings, UserChoices, PUSHABLE};

const EVENTS: usize = SoundEvent::ALL.len();

/// The first wait before a round trip that came to nothing — a read of the
/// stored choices, or a push the cue authority had no room for or did not
/// answer — is tried again; each further one waits twice as long, up to
/// [`RETRY_MAX_NS`].
///
/// Neither has a wake of its own, so the retry is a one-shot deadline: long
/// enough that a flooded or restarting service recovers, short enough that
/// the user's choices are in force within a moment of it doing so.
pub const RETRY_NS: u64 = 250_000_000;

/// The longest wait between attempts at a round trip that came to nothing.
pub const RETRY_MAX_NS: u64 = 8_000_000_000;

/// What reading the stored choices produced: the choices, or none when the
/// store could not be read, and a ready-to-print line for each reason they
/// are not all the user's.
pub struct LoadedChoices {
    /// The choices to hand over, or `None` for a store that could not be
    /// read: never a stand-in a change could be stored over.
    pub choices: Option<UserChoices>,
    /// One `stderr` line per refused setting or unreadable store, in the
    /// session's `desktop:` diagnosis convention.
    pub warnings: Vec<String>,
}

/// Read the user's sound choices out of this application's private scope.
///
/// A setting the registry refuses is left out, saying so; a store that cannot
/// be read yields no choices, saying so, so the read is tried again.
pub fn load_choices(host: &mut dyn AppDataHost) -> LoadedChoices {
    let store = SettingsStore::open_without_defaults(host);
    if let Some(err) = store.store_refusal() {
        return unread(&alloc::format!("{err:?}"));
    }
    match store.document() {
        Ok(document) => {
            let (choices, refused) = UserChoices::load(&document);
            LoadedChoices {
                choices: Some(choices),
                warnings: refused
                    .into_iter()
                    .map(|key| {
                        alloc::format!(
                            "desktop: the sound setting `{key}` is not one kept; left out\n"
                        )
                    })
                    .collect(),
            }
        }
        Err(err) => unread(&alloc::format!("{err}")),
    }
}

/// A read that came to nothing, for `why`.
fn unread(why: &str) -> LoadedChoices {
    LoadedChoices {
        choices: None,
        warnings: alloc::vec![alloc::format!(
            "desktop: sound choices could not be read ({why}); the machine's are in force until \
             they are\n"
        )],
    }
}

/// Store `choices` in this application's private scope, replacing every sound
/// setting it held and nothing else, and answer with what the store then
/// holds.
///
/// # Errors
///
/// The app-data service's refusal; nothing was stored.
pub fn store_choices(
    host: &mut dyn AppDataHost,
    choices: &UserChoices,
) -> Result<UserChoices, Errno> {
    let mut store = SettingsStore::open_without_defaults(host);
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    let document = choices.document();
    for &key in SoundSettings::KEYS {
        let name = SoundSettings::name(key);
        match document.get(name) {
            Some(value) => store.set(name, value).map_err(|_| Errno::OutOfRange)?,
            None => store.unset(name),
        }
    }
    if let Err(err) = store.commit() {
        // Withdrawn, so the next commit this process makes cannot publish it.
        let _ = store.abandon();
        return Err(err);
    }
    let held = store.document().map_err(|_| Errno::OutOfRange)?;
    Ok(UserChoices::load(&held).0)
}

/// What the session is to carry out for the user's sound choices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectsWork {
    /// Store `choices`, the ask numbered `number`, and answer the pinboard call
    /// `ticket` once the store has spoken.
    Store {
        /// Which ask it is, in the order asked.
        number: u64,
        /// The choices to store.
        choices: UserChoices,
        /// The call to answer, for an ask a caller made.
        ticket: Option<u64>,
    },
    /// Hand `choices` to the cue authority's instance `authority`.
    Push {
        /// Where the files are delegated to.
        authority: ProcId,
        /// What is handed over.
        choices: UserChoices,
    },
}

/// What plays for an event, as the pane is told: owned, so it can cross to the
/// worker that answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Shown {
    /// The theme's sound.
    Theme,
    /// The chosen file, by its name.
    Chosen(String),
    /// The chosen file, by its name, which could not be handed over.
    Unavailable(String),
}

/// What the pane is told of the user's choices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reading {
    /// The settings in effect, as a document of every setting a user decides.
    pub document: String,
    /// What plays for each event.
    pub files: [Shown; EVENTS],
}

impl Reading {
    /// The reading as the pinboard protocol carries it.
    #[must_use]
    pub fn as_wire(&self) -> SoundReading<'_> {
        SoundReading {
            document: &self.document,
            files: core::array::from_fn(|at| match &self.files[at] {
                Shown::Theme => SoundFile::Theme,
                Shown::Chosen(name) => SoundFile::Chosen(name),
                Shown::Unavailable(name) => SoundFile::Unavailable(name),
            }),
        }
    }
}

/// What a pane's read of the choices is answered from: the choices stored and
/// what each file came to at the last push, with the machine's document read
/// on the worker that answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadAsk {
    /// The stored choices.
    pub choices: UserChoices,
    /// Which events' files could not be handed over.
    pub unavailable: [bool; EVENTS],
}

impl ReadAsk {
    /// The reading over `machine`, the machine's document.
    #[must_use]
    pub fn reading(&self, machine: &SoundSettings) -> Reading {
        let document = self.choices.over(machine).document_of(&PUSHABLE).render();
        let files = core::array::from_fn(|at| {
            let event = SoundEvent::ALL[at];
            match self.choices.file(event).map(leaf_of) {
                None => Shown::Theme,
                Some(name) if self.unavailable[at] => Shown::Unavailable(String::from(name)),
                Some(name) => Shown::Chosen(String::from(name)),
            }
        });
        Reading { document, files }
    }
}

/// A path's last named component: what a file is called.
fn leaf_of(path: &str) -> &str {
    path.rsplit('/')
        .find(|component| !component.is_empty())
        .unwrap_or(path)
}

/// One attempt at a push, to tell a repeat from a change.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Attempt {
    authority: ProcId,
    choices: UserChoices,
}

/// When a round trip that came to nothing is tried again, and the wait it
/// took to get there.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Backoff {
    at: u64,
    wait: u64,
}

impl Backoff {
    /// The attempt after `earlier`, from `now`.
    fn after(earlier: Option<Self>, now: u64) -> Self {
        let wait = earlier.map_or(RETRY_NS, |earlier| {
            earlier.wait.saturating_mul(2).min(RETRY_MAX_NS)
        });
        Self {
            at: now.saturating_add(wait),
            wait,
        }
    }
}

/// A push on its way: its files delegated on the worker, then its frame
/// posted from the loop, until its answer lands.
#[derive(Clone, Debug)]
struct Flight {
    /// What it hands over, and to whom.
    attempt: Attempt,
    /// The epoch it was made in.
    epoch: u64,
    /// Which events' files could not be handed over.
    unavailable: [bool; EVENTS],
    /// The frame to post, once its files are delegated and until it is.
    frame: Option<(String, [u64; EVENTS])>,
}

/// The most pinboard calls whose answers may wait on the private-store worker
/// at once.
///
/// A containment bound below the worker's room: any process of the account
/// may call, and each call it keeps unanswered holds a place, so without it a
/// caller could crowd out the session's own round trips.
pub const MAX_HELD_CALLS: usize = 4;

/// The session's side of the user's sound choices.
#[derive(Clone, Debug, Default)]
pub struct Effects {
    /// Pinboard calls handed to the worker and not yet answered.
    held_calls: usize,
    /// What the store holds, once read.
    stored: Option<UserChoices>,
    /// Whether a read of the stored choices is under way.
    loading: bool,
    /// When a read that came to nothing is tried again.
    load_retry: Option<Backoff>,
    /// The newest choices asked to be stored, until that ask lands: what the
    /// next change is made over, so one never undoes another on its way.
    asked: Option<(u64, UserChoices)>,
    numbered: u64,
    /// The cue authority's instance, as its notice last said.
    authority: Option<ProcId>,
    /// Whether this session holds the seat.
    holding: bool,
    /// Advanced whenever the authority forgets what it held of this session
    /// — a new instance, the seat lost or taken again — so an answer to a
    /// push made before is never taken for one made since.
    epoch: u64,
    /// What the authority holds of this session's, as last handed over.
    handed: Option<Attempt>,
    /// A push the authority refused, owed again only for a change.
    refused: Option<Attempt>,
    /// The push on its way, if one is.
    flight: Option<Flight>,
    /// When a push that came to nothing is tried again.
    retry: Option<Backoff>,
    /// Which events' files could not be handed over at the last push.
    unavailable: [bool; EVENTS],
    /// The pick showing: its event, and the call to answer once it ends.
    picking: Option<(SoundEvent, u64)>,
}

impl Effects {
    /// Nothing read, nothing handed over.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a place for one more pinboard call's work on the worker,
    /// answering whether there was one.
    pub fn admit_call(&mut self) -> bool {
        if self.held_calls >= MAX_HELD_CALLS {
            return false;
        }
        self.held_calls += 1;
        true
    }

    /// Give back an admitted call's place: it was answered, or its work never
    /// reached the worker.
    pub fn release_call(&mut self) {
        self.held_calls = self.held_calls.saturating_sub(1);
    }

    /// Whether a read of the stored choices is owed at `now`, which this then
    /// counts as under way: until one has been read, and none is, once any
    /// retry is due.
    pub fn next_load(&mut self, now: u64) -> bool {
        let owed = self.stored.is_none()
            && !self.loading
            && self.load_retry.is_none_or(|retry| now >= retry.at);
        self.loading |= owed;
        owed
    }

    /// The read of the stored choices came to `choices` at `now`, or to
    /// nothing, which is tried again: a change is never made over choices the
    /// store was not read for.
    pub fn loaded(&mut self, now: u64, choices: Option<UserChoices>) {
        self.loading = false;
        match choices {
            Some(choices) => {
                self.stored = Some(choices);
                self.load_retry = None;
            }
            None => self.load_retry = Some(Backoff::after(self.load_retry, now)),
        }
    }

    /// The cue authority's instance is `authority`, as its notice says: a new
    /// one holds none of this session's choices.
    pub fn authority_is(&mut self, authority: Option<ProcId>) {
        if self.authority != authority {
            self.authority = authority;
            self.forget();
        }
    }

    /// Whether this session holds the seat: the authority drops a session's
    /// choices with its hold, so taking the seat again owes them anew. Only a
    /// change does anything.
    pub fn seat(&mut self, holding: bool) {
        if self.holding != holding {
            self.holding = holding;
            self.forget();
        }
    }

    /// The authority holds nothing of this session's any more.
    fn forget(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.handed = None;
        self.refused = None;
        self.retry = None;
    }

    /// The choices a change is made over, once the store has been read.
    fn base(&self) -> Result<UserChoices, Errno> {
        self.asked
            .as_ref()
            .map(|(_, choices)| choices)
            .or(self.stored.as_ref())
            .cloned()
            .ok_or(Errno::Busy)
    }

    /// Ask for `choices` to be stored, answering `ticket` once it has been.
    fn ask(&mut self, choices: UserChoices, ticket: Option<u64>) -> EffectsWork {
        self.numbered = self.numbered.wrapping_add(1);
        self.asked = Some((self.numbered, choices.clone()));
        EffectsWork::Store {
            number: self.numbered,
            choices,
            ticket,
        }
    }

    /// Lay a pane's `text` over the choices, answering the store to ask for.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] before the stored choices are read, and
    /// [`Errno::OutOfRange`] for a document [`UserChoices::apply`] refuses,
    /// with the refusal to state.
    pub fn apply(
        &mut self,
        text: &str,
        ticket: u64,
    ) -> Result<EffectsWork, (Errno, Option<Refusal<SoundKey>>)> {
        let mut choices = self.base().map_err(|err| (err, None))?;
        choices
            .apply(text)
            .map_err(|refusal| (Errno::OutOfRange, Some(refusal)))?;
        Ok(self.ask(choices, Some(ticket)))
    }

    /// Have `event` play its theme's sound again, turning it on.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a sound no user's choices reach, and
    /// [`Errno::Busy`] before the stored choices are read.
    pub fn use_theme(&mut self, event: SoundEvent, ticket: u64) -> Result<EffectsWork, Errno> {
        if !layered(event) {
            return Err(Errno::OutOfRange);
        }
        let mut choices = self.base()?;
        choices.use_theme(event);
        Ok(self.ask(choices, Some(ticket)))
    }

    /// The user is to choose the file `event` plays, the call `ticket`
    /// answered once the picker closes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a sound no user's choices reach,
    /// [`Errno::Busy`] while another pick of a sound is showing or before the
    /// stored choices are read.
    pub fn begin_pick(&mut self, event: SoundEvent, ticket: u64) -> Result<(), Errno> {
        if !layered(event) {
            return Err(Errno::OutOfRange);
        }
        if self.picking.is_some() || self.stored.is_none() {
            return Err(Errno::Busy);
        }
        self.picking = Some((event, ticket));
        Ok(())
    }

    /// The pick of a sound could not be shown after all: its call is answered
    /// with the refusal, and nothing is picking.
    pub fn abandon_pick(&mut self) -> Option<u64> {
        self.picking.take().map(|(_, ticket)| ticket)
    }

    /// The user chose the file at `path`: the store to ask for, which answers
    /// the pick's call once it has spoken. `None` with nothing picking.
    pub fn chosen(&mut self, path: &SettingPath) -> Option<EffectsWork> {
        let (event, ticket) = self.picking.take()?;
        let Ok(mut choices) = self.base() else {
            self.picking = Some((event, ticket));
            return None;
        };
        // The event was checked as the pick began.
        choices.choose(event, path);
        Some(self.ask(choices, Some(ticket)))
    }

    /// The user closed the picker without choosing: the call to answer.
    pub fn cancelled(&mut self) -> Option<u64> {
        self.abandon_pick()
    }

    /// Whether a pick of a sound is showing.
    #[must_use]
    pub const fn is_picking(&self) -> bool {
        self.picking.is_some()
    }

    /// The store asked as `number` came to `result`: what it holds now, or its
    /// refusal, after which a change is made over what it held before.
    pub fn stored(&mut self, number: u64, result: Result<UserChoices, Errno>) {
        if self
            .asked
            .as_ref()
            .is_some_and(|(asked, _)| *asked == number)
        {
            self.asked = None;
        }
        if let Ok(choices) = result {
            self.stored = Some(choices);
        }
    }

    /// The push owed, retries aside: this session holds the seat, and the
    /// authority holds neither its stored choices nor a refusal of them.
    fn owed(&self) -> Option<(ProcId, &UserChoices)> {
        let (authority, choices) = (self.authority?, self.stored.as_ref()?);
        let made = |attempt: &Option<Attempt>| {
            attempt
                .as_ref()
                .is_some_and(|made| made.authority == authority && made.choices == *choices)
        };
        (self.holding && !made(&self.handed) && !made(&self.refused))
            .then_some((authority, choices))
    }

    /// The push owed at `now`, if one is and none is on its way: its files are
    /// to be delegated, after which [`Self::take_post`] hands over its frame.
    ///
    /// Asked on every turn of the loop, so it copies the choices only once a
    /// push is actually owed.
    pub fn next_push(&mut self, now: u64) -> Option<EffectsWork> {
        if self.flight.is_some() || self.retry.is_some_and(|retry| now < retry.at) {
            return None;
        }
        let (authority, choices) = self.owed()?;
        let choices = choices.clone();
        self.flight = Some(Flight {
            attempt: Attempt {
                authority,
                choices: choices.clone(),
            },
            epoch: self.epoch,
            unavailable: [false; EVENTS],
            frame: None,
        });
        Some(EffectsWork::Push { authority, choices })
    }

    /// The files of the push on its way are delegated: `files`, with
    /// `unavailable` those that could not be, beside `document`, the choices
    /// it carries.
    pub fn handed_over(
        &mut self,
        document: String,
        files: [u64; EVENTS],
        unavailable: [bool; EVENTS],
    ) {
        if let Some(flight) = self.flight.as_mut() {
            flight.unavailable = unavailable;
            flight.frame = Some((document, files));
        }
    }

    /// The frame of the push on its way, once its files are delegated: taken
    /// once, to be posted to the authority.
    pub fn take_post(&mut self) -> Option<(String, [u64; EVENTS])> {
        self.flight.as_mut()?.frame.take()
    }

    /// The push on its way came to `result` at `now` — the authority's answer,
    /// or why it could not be asked — answering the refusal to state. An
    /// answer to a push made before the authority last forgot this session's
    /// choices says nothing of what it holds now, so only the push owed next
    /// follows from it.
    pub fn pushed(&mut self, now: u64, result: Result<(), Errno>) -> Option<Errno> {
        let flight = self.flight.take()?;
        if flight.epoch != self.epoch {
            return None;
        }
        match result {
            Ok(()) => {
                self.unavailable = flight.unavailable;
                self.handed = Some(flight.attempt);
                self.refused = None;
                self.retry = None;
                None
            }
            Err(err @ (Errno::WouldBlock | Errno::TimedOut | Errno::Busy | Errno::OutOfMemory)) => {
                self.retry = Some(Backoff::after(self.retry, now));
                Some(err)
            }
            Err(err) => {
                self.refused = Some(flight.attempt);
                self.retry = None;
                Some(err)
            }
        }
    }

    /// When this next needs a turn with nothing else to wake it: a push or a
    /// read that came to nothing is tried again. Nothing while there is
    /// nothing to try, so a deadline never outlives the work it is for.
    #[must_use]
    pub fn due_at(&self) -> Option<u64> {
        let push = self
            .retry
            .filter(|_| self.flight.is_none() && self.owed().is_some());
        let load = self
            .load_retry
            .filter(|_| self.stored.is_none() && !self.loading);
        match (push, load) {
            (Some(push), Some(load)) => Some(push.at.min(load.at)),
            (push, load) => push.or(load).map(|retry| retry.at),
        }
    }

    /// `park_ns` shortened to the retry due, or left as it is when none is.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        crate::switchuser::park_within(park_ns, self.due_at().map(|at| at.saturating_sub(now_ns)))
    }

    /// What a pane's read is answered from.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] before the stored choices are read.
    pub fn read_ask(&self) -> Result<ReadAsk, Errno> {
        Ok(ReadAsk {
            choices: self.stored.clone().ok_or(Errno::Busy)?,
            unavailable: self.unavailable,
        })
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
