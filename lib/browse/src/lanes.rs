//! Which volume each piece of a surface's off-loop work reads or writes, so a
//! slow or failing device holds up only the work aimed at it.
//!
//! Every piece of work runs on the [`Lane`] of the volume it touches, routed
//! through the namespace's own bindings ([`Routes`]) without touching the
//! device. One worker serves a lane at a time ([`Serving`]): a volume's reads
//! keep the order the reader takes its work in, and an eject waits for the
//! read under way on its own volume and no other. Work another set of workers
//! does on a volume — the desktop's eject, made by its listing readers while
//! its icon readers read the same volume — holds that volume's lane of these
//! workers off them (`hold`) for as long as it takes. Workers come and go as the
//! lanes need them ([`Crew`]), so their number follows the volumes at work,
//! and the artwork they decode is bounded by what the machine leaves room for
//! ([`Decodes`]), however many volumes are shown.
//!
//! The one definition the file manager's readers and the desktop session's
//! listing and file work both take their volumes from.
//!
//! # No I/O, and no lock
//!
//! The policy alone, so it compiles and is tested on the host. A program holds
//! it under its readers' mutex; `Routes::read`, with the `rt` feature, is the
//! one read of the view list it is routed by.

use alloc::vec::Vec;

use tairix_abi::sysinfo::{ViewBindingRecord, VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED};
use tairix_icon::ArtworkKey;
use tairix_reclaim::PressureBand;

use crate::mount::{MountAct, Projection};

/// The share of the work one worker serves at a time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lane {
    /// Work on no place a listing named: the System Information service, the
    /// discovery service, the program stores, the system's own artwork.
    Services,
    /// A place no published volume answers for: the `/Storage` catalog, a
    /// volume that publishes no identity, or anywhere before the namespace is
    /// first read.
    Unnamed,
    /// A place the volume published under this identity answers for.
    Volume([u8; VOLUME_ID_LEN]),
}

impl Lane {
    /// The lane of the volume published as `volume_id`.
    #[must_use]
    pub fn of_volume(volume_id: [u8; VOLUME_ID_LEN]) -> Self {
        if volume_id == VOLUME_ID_UNPUBLISHED {
            Self::Unnamed
        } else {
            Self::Volume(volume_id)
        }
    }
}

/// The namespace's bindings, by which a place is routed to its lane.
///
/// Routed by the namespace as last read: work taken before a mount change is
/// read again runs on the lane its place then fell in, so a read of a volume
/// just attached can still hold it as an eject of it is carried out, which is
/// then refused as busy, saying so.
#[derive(Debug, Default)]
pub struct Routes {
    views: Vec<ViewBindingRecord>,
}

impl Routes {
    /// Routes through `views`, the namespace's bindings in table order.
    #[must_use]
    pub const fn new(views: Vec<ViewBindingRecord>) -> Self {
        Self { views }
    }

    /// The lane of the place `components` names, root-first: the volume shown
    /// by the binding covering it.
    #[must_use]
    pub fn lane_of<S: AsRef<str>>(&self, components: &[S]) -> Lane {
        self.lane_by(&components.iter().map(|part| part.as_ref().as_bytes()))
    }

    /// The lane of the absolute `path`.
    #[must_use]
    pub fn lane_of_path(&self, path: &str) -> Lane {
        self.lane_by(
            &path
                .split('/')
                .filter(|part| !part.is_empty())
                .map(str::as_bytes),
        )
    }

    /// The lane decoding `key` runs on: that of the file it reads, or the
    /// services' for artwork no listing named.
    #[must_use]
    pub fn lane_of_artwork(&self, key: &ArtworkKey) -> Lane {
        key.read_path()
            .map_or(Lane::Services, |path| self.lane_of_path(path))
    }

    /// The lane the eject desk carries `act` out on: an eject's volume's, so
    /// it waits for the read under way there. Only ejects wait there — a
    /// mount or a retitle goes to the mount worker — so any other act is
    /// placed where its own reads would run.
    #[must_use]
    pub fn lane_of_act(&self, act: &MountAct) -> Lane {
        match act {
            MountAct::Eject { volume_id, .. } | MountAct::Retitle { volume_id, .. } => {
                Lane::of_volume(*volume_id)
            }
            MountAct::Attach { path, .. } => self.lane_of_path(path),
        }
    }

    /// The places the volume published as `volume_id` is shown at, by the
    /// bindings last read: where a surface's own holds must leave before an
    /// eject of it.
    pub fn shown_at(
        &self,
        volume_id: [u8; VOLUME_ID_LEN],
    ) -> impl Iterator<Item = Projection> + '_ {
        self.views
            .iter()
            .filter(move |view| view.volume_id() == volume_id)
            .filter_map(Projection::of)
    }

    fn lane_by<'p>(&self, place: &(impl Iterator<Item = &'p [u8]> + Clone)) -> Lane {
        tairix_procinfo::covering(&self.views, place)
            .map_or(Lane::Unnamed, |view| Lane::of_volume(view.volume_id()))
    }
}

#[cfg(feature = "rt")]
impl Routes {
    /// The namespace's bindings as they stand, read from the live view list:
    /// a call to the System Information service, so it is made off any
    /// interactive loop. A refused or failed query, or one the heap could not
    /// hold, yields none, so every place shares one lane rather than one
    /// guessed from part of the list.
    #[must_use]
    pub fn read() -> Self {
        let mut views = Vec::new();
        let read = tairix_procinfo::for_each_view(&tairix_procinfo::IpcTransport, |view| {
            views
                .try_reserve(1)
                .map_err(|_| tairix_abi::Errno::OutOfMemory)?;
            views.push(*view);
            Ok(tairix_procinfo::WalkStep::Continue)
        });
        if read.is_err() {
            views.clear();
        }
        Self { views }
    }
}

/// The lanes the workers are serving now, each by one worker at a time, and
/// those held off them by work done elsewhere, or waiting to be.
#[derive(Debug, Default)]
pub struct Serving(Vec<(Lane, Turn)>);

/// Whose a lane is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Turn {
    /// A worker serves it.
    Worker,
    /// A hold waits for the work under way there to end, and no worker takes
    /// more there meanwhile, so that work is the last before the hold.
    Awaited,
    /// A hold has it.
    Held,
}

impl Serving {
    /// No lane served.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Whether no worker serves `lane` and nothing holds it or waits to.
    #[must_use]
    pub fn is_free(&self, lane: Lane) -> bool {
        !self.0.iter().any(|(at, _)| *at == lane)
    }

    /// Whether the heap has room for one more worker to begin, so the
    /// [`begin`](Self::begin) after it allocates nothing.
    pub fn has_room(&mut self) -> bool {
        self.0.try_reserve(1).is_ok()
    }

    /// A worker began work on `lane`.
    pub fn begin(&mut self, lane: Lane) {
        self.0.push((lane, Turn::Worker));
    }

    /// The worker on `lane`, or the hold of it, finished there, answering
    /// whether a hold waits for the lane, to be woken.
    pub fn end(&mut self, lane: Lane) -> bool {
        if let Some(at) = self
            .0
            .iter()
            .position(|&(at, turn)| at == lane && turn != Turn::Awaited)
        {
            self.0.swap_remove(at);
        }
        self.0.contains(&(lane, Turn::Awaited))
    }

    /// Wait to hold `lane`: no worker takes work there from now on.
    /// `false` where the heap has no room to note the wait.
    #[must_use]
    pub fn await_lane(&mut self, lane: Lane) -> bool {
        if self.0.try_reserve(1).is_err() {
            return false;
        }
        self.0.push((lane, Turn::Awaited));
        true
    }

    /// Hold `lane`, waited for by [`await_lane`](Self::await_lane), once no
    /// worker serves it and no other hold has it, answering whether it is
    /// held. The hold ends as a worker's turn does, by [`end`](Self::end).
    pub fn take_awaited(&mut self, lane: Lane) -> bool {
        if self
            .0
            .iter()
            .any(|&(at, turn)| at == lane && turn != Turn::Awaited)
        {
            return false;
        }
        let awaited = self
            .0
            .iter_mut()
            .find(|entry| **entry == (lane, Turn::Awaited));
        awaited.is_some_and(|entry| {
            entry.1 = Turn::Held;
            true
        })
    }
}

/// The workers one kind of work is served by: how many live, how many wait,
/// and how many are on their way.
///
/// Whenever every worker is busy a spare is started, so work arriving for a
/// free lane is taken at once however long the others are held up; a worker
/// finding two already waiting leaves, so the crew shrinks back to the lanes
/// at work and two spare, and a lone piece of work arriving now and then is
/// taken by a waiting worker rather than costing a thread made and ended.
#[derive(Debug, Default)]
pub struct Crew {
    alive: usize,
    idle: usize,
    starting: usize,
    refusals: Refused,
    stopping: bool,
    /// Memory is short: a worker with nothing to do lets go of what it holds
    /// for its work before it rests.
    lean: bool,
}

/// The spares refused since a worker last arrived. A run of refusals is
/// stated once, and no spare is tried again on every take: once per burst of
/// work, not never, since a crew of one or two rests but cannot leave.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Refused {
    #[default]
    None,
    /// Stated, and no spare is tried until a worker rests or arrives.
    Held,
    /// Stated, and a worker has rested since, so a spare may be tried again.
    Stated,
}

/// What a worker that found no work does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Rest {
    /// It waits to be woken.
    Wait,
    /// It leaves: another already waits, or the crew is stopping.
    Leave,
}

impl Crew {
    /// A crew of no workers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            alive: 0,
            idle: 0,
            starting: 0,
            refusals: Refused::None,
            stopping: false,
            lean: false,
        }
    }

    /// Adopt the machine's memory-pressure `band`, answering whether memory
    /// has just become short: the waiting workers are then owed a wake, to
    /// let go of what they hold. What an idle worker holds is speculative, so
    /// it is kept only while the band is normal.
    pub fn adopt_band(&mut self, band: PressureBand) -> bool {
        let lean = band != PressureBand::Normal;
        !core::mem::replace(&mut self.lean, lean) && lean
    }

    /// Whether memory is short.
    #[must_use]
    pub const fn lean(&self) -> bool {
        self.lean
    }

    /// Count the worker the crew's owner is about to start.
    pub fn start(&mut self) {
        self.starting += 1;
        self.alive += 1;
    }

    /// A worker took work, answering whether it must start a spare — counted
    /// from now — because no other waits or is on its way, and no start has
    /// been refused since a worker last rested or arrived.
    pub fn took(&mut self) -> bool {
        let spare = !self.stopping
            && self.refusals != Refused::Held
            && self.idle == 0
            && self.starting == 0;
        if spare {
            self.starting += 1;
            self.alive += 1;
        }
        spare
    }

    /// A worker counted on its way is serving.
    pub fn arrived(&mut self) {
        self.starting = self.starting.saturating_sub(1);
        self.refusals = Refused::None;
    }

    /// A worker counted on its way could not be started, answering whether
    /// that is worth stating: the first refusal since a worker last arrived.
    pub fn refused(&mut self) -> bool {
        self.starting = self.starting.saturating_sub(1);
        self.alive = self.alive.saturating_sub(1);
        core::mem::replace(&mut self.refusals, Refused::Held) == Refused::None
    }

    /// A worker found no work: it waits, or leaves where two already wait or
    /// the crew is stopping. Either way a refused spare may be tried again.
    pub fn rest(&mut self) -> Rest {
        if self.refusals == Refused::Held {
            self.refusals = Refused::Stated;
        }
        if self.stopping || self.idle >= 2 {
            self.alive = self.alive.saturating_sub(1);
            return Rest::Leave;
        }
        self.idle += 1;
        Rest::Wait
    }

    /// A waiting worker woke to look again.
    pub fn woke(&mut self) {
        self.idle = self.idle.saturating_sub(1);
    }

    /// Send every worker away: no spare is started, and each leaves when it
    /// next rests.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Whether the workers have been sent away.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }

    /// How many workers live, those on their way among them.
    #[must_use]
    pub const fn alive(&self) -> usize {
        self.alive
    }
}

/// How many artwork decodes run at once: as many as fit in what one document
/// decoder may map, and one where none fits, so a pool of more than one never
/// maps more together than one document decoder may. A decode finding none
/// free stays queued, which delays decoration and never a listing.
#[derive(Debug)]
pub struct Decodes {
    running: usize,
    most: usize,
}

impl Decodes {
    /// One decode at a time, until the machine's size is known.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            running: 0,
            most: 1,
        }
    }

    /// Let `most` run at once, never fewer than one.
    pub fn admit(&mut self, most: usize) {
        self.most = most.max(1);
    }

    /// The most that run at once: the decoders worth keeping.
    #[must_use]
    pub const fn most(&self) -> usize {
        self.most
    }

    /// Whether a decode may start now.
    #[must_use]
    pub const fn may_start(&self) -> bool {
        self.running < self.most
    }

    /// A decode started.
    pub fn started(&mut self) {
        self.running += 1;
    }

    /// A decode ended, answering whether that freed a slot none was free in:
    /// a decode recorded meanwhile woke no worker.
    #[must_use]
    pub fn ended(&mut self) -> bool {
        let was_full = !self.may_start();
        self.running = self.running.saturating_sub(1);
        was_full
    }
}

impl Default for Decodes {
    fn default() -> Self {
        Self::new()
    }
}

/// What a lane's workers park on for work, and apart from it what a hold of a
/// lane parks on for the work under way there to end, so a wake meant for a
/// worker is never taken by a holder.
#[cfg(feature = "rt")]
pub struct LaneSignal {
    work: tairix_rt::sync::Condvar,
    freed: tairix_rt::sync::Condvar,
}

#[cfg(feature = "rt")]
impl LaneSignal {
    /// Nothing signalled yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            work: tairix_rt::sync::Condvar::new(),
            freed: tairix_rt::sync::Condvar::new(),
        }
    }

    /// Wake a waiting worker: work was recorded.
    pub fn notify_one(&self) {
        self.work.notify_one();
    }

    /// Wake every waiting worker.
    pub fn notify_all(&self) {
        self.work.notify_all();
    }

    /// What the workers park on for work, for a seam that wakes them itself.
    #[must_use]
    pub const fn work(&self) -> &tairix_rt::sync::Condvar {
        &self.work
    }
}

#[cfg(feature = "rt")]
impl Default for LaneSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Work its readers take a lane at a time, under one lock.
#[cfg(feature = "rt")]
pub trait LaneWork {
    /// One unit of the work.
    type Read;

    /// The lanes being read, and the readers reading them.
    fn lanes(&mut self) -> (&mut Serving, &mut Crew);

    /// The next unit of work on a lane no reader serves, and its lane, `last`
    /// — the lane the asking reader has just served — first: `None` with
    /// nothing to do, and once the work is stopping.
    fn next_read(&mut self, last: Option<Lane>) -> Option<(Lane, Self::Read)>;
}

/// One reader's whole life over `work`: park on `signal` until work is wanted
/// on a lane no reader serves, `carry` it out with no lock held through what
/// the reader holds, `own` — `spare` starting first the spare the crew
/// counted — and leave once two other readers already wait or the crew stops.
/// `carry` answers whether its embedder is owed a wake, which `wake` gives.
/// With no room to note the lane it would serve, a reader parks rather than
/// taking work it could not account for. While the crew is lean a reader with
/// nothing to do first lets `shed` give up what `own` holds, off the lock,
/// once per piece of work it carried, and looks again before it rests.
#[cfg(feature = "rt")]
pub fn serve<W: LaneWork, S>(
    work: &tairix_rt::sync::Mutex<W>,
    signal: &LaneSignal,
    spare: impl Fn(),
    own: &mut S,
    mut carry: impl FnMut(&mut S, Lane, W::Read) -> bool,
    mut shed: impl FnMut(&mut S),
    wake: impl Fn(),
) {
    let mut last = None;
    loop {
        let (lane, read, start) = {
            let mut held = work.lock();
            // Only carrying work takes anything up, so once is enough.
            let mut shed_since_carry = false;
            loop {
                let next = if held.lanes().0.has_room() {
                    held.next_read(last)
                } else {
                    None
                };
                if let Some((lane, read)) = next {
                    let (serving, crew) = held.lanes();
                    serving.begin(lane);
                    let start = crew.took();
                    // What its last lane still holds is another reader's to
                    // take now.
                    if last.is_some_and(|last| last != lane) {
                        signal.notify_one();
                    }
                    break (lane, read, start);
                }
                if !shed_since_carry && held.lanes().1.lean() {
                    drop(held);
                    shed(own);
                    shed_since_carry = true;
                    // Work recorded while the lock was down woke nobody.
                    held = work.lock();
                    continue;
                }
                match held.lanes().1.rest() {
                    Rest::Leave => return,
                    Rest::Wait => {
                        held = signal.work.wait(held);
                        held.lanes().1.woke();
                        // A reader that waited served nothing since.
                        last = None;
                    }
                }
            }
        };
        if start {
            spare();
        }
        let owed = carry(own, lane, read);
        if work.lock().lanes().0.end(lane) {
            signal.freed.notify_all();
        }
        last = Some(lane);
        if owed {
            wake();
        }
    }
}

/// Adopt the band this process's pressure gauge holds for `work`'s workers
/// ([`Crew::adopt_band`]), waking those that wait once memory becomes short.
#[cfg(feature = "rt")]
pub fn adopt_band<W: LaneWork>(work: &tairix_rt::sync::Mutex<W>, signal: &LaneSignal) {
    let band = tairix_rt::pressure::gauge().band();
    if work.lock().lanes().1.adopt_band(band) {
        signal.notify_all();
    }
}

/// A lane of a set of workers held off them by work done elsewhere: none takes
/// work there until it drops.
#[cfg(feature = "rt")]
pub struct Held<'a, W: LaneWork> {
    work: &'a tairix_rt::sync::Mutex<W>,
    signal: &'a LaneSignal,
    lane: Lane,
}

/// Hold `lane` of `work`'s workers off them, parking until the work under way
/// there has ended: from the call no worker takes work there until the hold
/// drops, so what the holder does follows every read of the lane begun before
/// it and overlaps none begun after. `None` where the heap has no room to note
/// the hold.
#[cfg(feature = "rt")]
pub fn hold<'a, W: LaneWork>(
    work: &'a tairix_rt::sync::Mutex<W>,
    signal: &'a LaneSignal,
    lane: Lane,
) -> Option<Held<'a, W>> {
    let mut held = work.lock();
    if !held.lanes().0.await_lane(lane) {
        return None;
    }
    while !held.lanes().0.take_awaited(lane) {
        held = signal.freed.wait(held);
    }
    drop(held);
    Some(Held { work, signal, lane })
}

#[cfg(feature = "rt")]
impl<W: LaneWork> Drop for Held<'_, W> {
    fn drop(&mut self) {
        if self.work.lock().lanes().0.end(self.lane) {
            self.signal.freed.notify_all();
        } else {
            // Work recorded on the lane while it was held woke no worker.
            self.signal.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
