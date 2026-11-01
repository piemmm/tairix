//! Which volume each piece of the file manager's work reads or writes, so a
//! slow or failing device holds up only the work aimed at it.
//!
//! Every piece of work runs on the [`Lane`] of the volume it touches, routed
//! through the namespace's own bindings ([`Routes`]) without touching the
//! device. One worker serves a lane at a time ([`Serving`]): a volume's reads
//! keep the order the reader takes its work in, and an eject waits for the
//! read under way on its own volume and no other. Workers come and go as the
//! lanes need them ([`Crew`]), so their number follows the volumes the
//! windows show, and the artwork they decode is bounded by what the machine
//! leaves room for ([`Decodes`]), however many volumes are shown.
//!
//! # No I/O, and no lock
//!
//! The policy alone, so it compiles and is tested on the host. The program
//! holds it under the reader's mutex.

use alloc::vec::Vec;

use tairix_abi::sysinfo::{ViewBindingRecord, VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED};
use tairix_browse::mount::MountAct;
use tairix_icon::ArtworkKey;

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

    /// The lane `act` is carried out on: an eject on its volume's, so it waits
    /// for the read under way there; a mount on the lane of the image file.
    #[must_use]
    pub fn lane_of_act(&self, act: &MountAct) -> Lane {
        match act {
            MountAct::Eject { volume_id, .. } => Lane::of_volume(*volume_id),
            MountAct::Attach { path, .. } => self.lane_of_path(path),
        }
    }

    fn lane_by<'p>(&self, place: &(impl Iterator<Item = &'p [u8]> + Clone)) -> Lane {
        tairix_procinfo::covering(&self.views, place)
            .map_or(Lane::Unnamed, |view| Lane::of_volume(view.volume_id()))
    }
}

/// The lanes the workers are serving now, each by one worker at a time.
#[derive(Debug, Default)]
pub struct Serving(Vec<Lane>);

impl Serving {
    /// No lane served.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Whether no worker serves `lane`.
    #[must_use]
    pub fn is_free(&self, lane: Lane) -> bool {
        !self.0.contains(&lane)
    }

    /// A worker began work on `lane`.
    pub fn begin(&mut self, lane: Lane) {
        self.0.push(lane);
    }

    /// The worker on `lane` finished its work there.
    pub fn end(&mut self, lane: Lane) {
        if let Some(at) = self.0.iter().position(|served| *served == lane) {
            self.0.swap_remove(at);
        }
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
    /// A start was refused since a worker last rested or arrived, so no spare
    /// is tried again on every take: once per burst of work, not never, since
    /// a crew of one or two rests but cannot leave.
    retry_held: bool,
    /// A refusal was stated since a worker last arrived, so a run of them is
    /// stated once.
    stated: bool,
    stopping: bool,
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
            retry_held: false,
            stated: false,
            stopping: false,
        }
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
        let spare = !self.stopping && !self.retry_held && self.idle == 0 && self.starting == 0;
        if spare {
            self.starting += 1;
            self.alive += 1;
        }
        spare
    }

    /// A worker counted on its way is serving.
    pub fn arrived(&mut self) {
        self.starting = self.starting.saturating_sub(1);
        self.retry_held = false;
        self.stated = false;
    }

    /// A worker counted on its way could not be started, answering whether
    /// that is worth stating: the first refusal since a worker last arrived.
    pub fn refused(&mut self) -> bool {
        self.starting = self.starting.saturating_sub(1);
        self.alive = self.alive.saturating_sub(1);
        self.retry_held = true;
        !core::mem::replace(&mut self.stated, true)
    }

    /// A worker found no work: it waits, or leaves where two already wait or
    /// the crew is stopping. Either way a refused spare may be tried again.
    pub fn rest(&mut self) -> Rest {
        self.retry_held = false;
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

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
