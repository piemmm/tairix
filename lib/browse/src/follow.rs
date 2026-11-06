//! Following the files a listed bundle is pictured from (`plans/ICONS.md`
//! I22).
//!
//! A listed bundle's picture is read from its manifest and the icon the
//! manifest names, which a copy may still be writing when the bundle is first
//! shown. So a reader arms a change wait on the manifest, `Resources/` and the
//! icon before it reads them, a thread of the readers' own parks on those
//! waits, and a change has the bundle read again ([`ArtworkDesk::moved`]),
//! decoded only where its files differ
//! ([`ArtworkJob::produce`](tairix_icon::ArtworkJob::produce)). Which bundles
//! are followed is the desk's, as far as its bound allows; what holds their
//! waits is [`BundleFollows`], which keeps a follow only while its desk still
//! follows the bundle through the wait it was armed in, and lets go of every
//! follow on a volume about to be ejected ([`BundleFollows::release_within`]).
//! The desktop session's icon readers and the file manager's follow bundles
//! through this one definition.
//!
//! # No I/O, and no lock
//!
//! [`BundleFollows`] is bookkeeping over any wait handle, so it is tested on
//! the host; the `rt` feature adds the waits, the read they guard, and the
//! follower's life.

use alloc::string::String;
use alloc::vec::Vec;
#[cfg(feature = "rt")]
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::fs::DocumentStamp;
#[cfg(feature = "rt")]
use tairix_abi::fs::OpenFlags;
#[cfg(feature = "rt")]
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
#[cfg(feature = "rt")]
use tairix_abi::BundleEntry;
use tairix_abi::Errno;
#[cfg(feature = "rt")]
use tairix_icon::artwork::{ArtworkRasteriser, ArtworkReader, BundleFiles};
#[cfg(feature = "rt")]
use tairix_icon::desk::ArtworkJob;
use tairix_icon::{ArtworkDesk, BundleVersion, ListedBundle};
#[cfg(feature = "rt")]
use tairix_rt::sync::{Condvar, Mutex};

#[cfg(feature = "rt")]
use crate::lanes::Decodes;
use crate::lanes::{Lane, Routes};

/// What a bundle's picture waits on while its files are not followed.
const BUNDLES_UNFOLLOWED: &str =
    "a bundle's own icon is read again only when its folder's listing moves";

/// The token the follower's own wake is reported under; every followed file's
/// is past it.
#[cfg(feature = "rt")]
const WAKE_TOKEN: u64 = 0;

/// Why a surface's listed bundles go unfollowed, which its follower states
/// once.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unfollowed {
    /// The follower's thread was refused.
    NoThread(Errno),
    /// The follower has no wake: the kernel refused its pipe.
    NoWake,
    /// The follower's wait could not be made.
    NoWait(Errno),
    /// The follower's wake could not join its wait.
    WakeRefused(Errno),
    /// The follower's wait failed.
    WaitFailed(Errno),
}

impl core::fmt::Display for Unfollowed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoThread(err) => write!(f, "no icon follow thread ({err:?})"),
            Self::NoWake => f.write_str("no icon follow wake"),
            Self::NoWait(err) => write!(f, "no icon follow wait ({err:?})"),
            Self::WakeRefused(err) => write!(f, "no icon follow wake ({err:?})"),
            Self::WaitFailed(err) => write!(f, "icon follow wait failed ({err:?})"),
        }?;
        write!(f, "; {BUNDLES_UNFOLLOWED}")
    }
}

/// One followed bundle: the handle, `H`, each of its files' change waits is
/// armed on, with the token its change is reported under.
#[derive(Debug)]
pub struct FollowedBundle<H> {
    bundle: ListedBundle,
    waits: Vec<(u64, H)>,
}

impl<H> FollowedBundle<H> {
    /// `bundle`, followed through `waits`.
    #[must_use]
    pub fn new(bundle: ListedBundle, waits: Vec<(u64, H)>) -> Self {
        Self { bundle, waits }
    }

    /// The bundle followed.
    #[must_use]
    pub const fn bundle(&self) -> &ListedBundle {
        &self.bundle
    }

    /// Whether the change reported under `token` is one of its files'.
    #[must_use]
    pub fn holds(&self, token: u64) -> bool {
        self.waits.iter().any(|(held, _)| *held == token)
    }

    /// Its waits, each with the token it is reported under, to end.
    #[must_use]
    pub fn into_waits(self) -> Vec<(u64, H)> {
        self.waits
    }
}

/// The bundles a surface's artwork readers follow, through the handles `H`
/// their waits are armed on, and the follows let go whose waits are still to
/// end.
///
/// A follow let go is ended by the reader of its own volume's lane, never
/// where it is let go — a paint or a sweep on the loop that owes a frame —
/// since closing a file may wait on its volume.
#[derive(Debug)]
pub struct BundleFollows<H> {
    /// The wait the followed files' changes are members of, once the follower
    /// has made it.
    set: Option<u64>,
    followed: Vec<FollowedBundle<H>>,
    ending: Vec<FollowedBundle<H>>,
}

impl<H> BundleFollows<H> {
    /// Nothing followed, and no wait to follow through yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            set: None,
            followed: Vec::new(),
            ending: Vec::new(),
        }
    }

    /// The wait the followed files' changes are members of: `None` until the
    /// follower makes it, and again once its wait has failed.
    #[must_use]
    pub const fn set(&self) -> Option<u64> {
        self.set
    }

    /// The follower made `set`: files are followed through it from now on.
    pub fn joined(&mut self, set: u64) {
        self.set = Some(set);
    }

    /// Hold `followed`, the waits a read of its bundle armed through the
    /// follower's wait `set`, in place of what that bundle held, which is let
    /// go. It is let go at once instead where any wait was `refused` — a
    /// bundle whose files are not all followed is not followed, and `desk` is
    /// told — where `desk` stopped following the bundle while the read went
    /// on, and where `set` is the follower's no longer: nothing would let it go
    /// later.
    pub fn keep<S: Clone + Eq>(
        &mut self,
        desk: &mut ArtworkDesk<S>,
        (followed, set): (FollowedBundle<H>, u64),
        refused: bool,
    ) {
        self.let_go([&followed.bundle]);
        if refused {
            desk.unfollowable(&followed.bundle);
        }
        if !refused && self.set == Some(set) && desk.follows(&followed.bundle) {
            self.followed.push(followed);
        } else {
            self.ending.push(followed);
        }
    }

    /// The bundle the change reported under `token` is to.
    #[must_use]
    pub fn holder(&self, token: u64) -> Option<&ListedBundle> {
        self.followed
            .iter()
            .find(|followed| followed.holds(token))
            .map(FollowedBundle::bundle)
    }

    /// The follower's wait failed: nothing is followed from now on, and every
    /// follow is let go.
    pub fn lost(&mut self) {
        self.set = None;
        self.ending.append(&mut self.followed);
    }

    /// Take a follow let go whose volume's lane `open` admits, routed by
    /// `routes`, with that lane, for its reader to end.
    pub fn next_ending(
        &mut self,
        routes: &Routes,
        open: impl Fn(Lane) -> bool,
    ) -> Option<(Lane, FollowedBundle<H>)> {
        let at = self
            .ending
            .iter()
            .position(|followed| open(routes.lane_of_path(&followed.bundle.dir)))?;
        let followed = self.ending.swap_remove(at);
        Some((routes.lane_of_path(&followed.bundle.dir), followed))
    }

    /// The version the bundle a listing names at `dir`, as `listed`, is
    /// pictured at when asked for at `side` ([`ArtworkDesk::version_of`]),
    /// letting go of the follows of the bundles `desk` forgot to remember it.
    /// Answers too whether a reader is owed a wake: to end a follow let go, or
    /// to read anew the files of a bundle followed again.
    pub fn version_of<S: Clone + Eq>(
        &mut self,
        desk: &mut ArtworkDesk<S>,
        dir: &str,
        listed: DocumentStamp,
        side: u32,
    ) -> (BundleVersion, bool) {
        let recorded = desk.recorded();
        let version = desk.version_of(dir, listed, side);
        let read = desk.recorded() != recorded;
        let let_go = self.let_go(desk.take_unfollowed().as_slice());
        (version, let_go || read)
    }

    /// Withdraw the thumbnails and let go of the follows nothing asked about
    /// since the last sweep ([`ArtworkDesk::sweep_thumbnails`],
    /// [`ArtworkDesk::sweep_follows`]), answering whether a follow was let
    /// go. Run, as they are, only after a pass over every surface that shows
    /// either.
    pub fn sweep<S: Clone + Eq>(&mut self, desk: &mut ArtworkDesk<S>) -> bool {
        desk.sweep_thumbnails();
        let unasked = desk.sweep_follows();
        self.let_go(&unasked)
    }

    /// Let go of the follow of every bundle within `roots` — the places a
    /// volume about to be ejected is shown at, by whole components — and have
    /// `desk` follow none of them until each is asked about anew, so one being
    /// armed meanwhile is let go as it is kept. Answers whether one was held,
    /// so a reader is owed a wake to end it on the volume's lane ahead of the
    /// eject.
    pub fn release_within<S: Clone + Eq>(
        &mut self,
        desk: &mut ArtworkDesk<S>,
        roots: &[&[String]],
    ) -> bool {
        let within = |dir: &str| {
            crate::vfs::components_from_absolute_path(dir)
                .is_ok_and(|at| crate::mount::within(roots, &at))
        };
        desk.stop_following_where(&within);
        let mut any = false;
        let mut at = 0;
        while let Some(followed) = self.followed.get(at) {
            if within(&followed.bundle.dir) {
                let followed = self.followed.swap_remove(at);
                self.ending.push(followed);
                any = true;
            } else {
                at += 1;
            }
        }
        any
    }

    /// Let go of the follows of `bundles`, answering whether one was held.
    fn let_go<'b>(&mut self, bundles: impl IntoIterator<Item = &'b ListedBundle>) -> bool {
        let mut any = false;
        for bundle in bundles {
            let at = self
                .followed
                .iter()
                .position(|followed| followed.bundle == *bundle);
            if let Some(at) = at {
                let followed = self.followed.swap_remove(at);
                self.ending.push(followed);
                any = true;
            }
        }
        any
    }
}

impl<H> Default for BundleFollows<H> {
    fn default() -> Self {
        Self::new()
    }
}

/// What a surface's artwork readers and its follower share under one lock.
#[cfg(feature = "rt")]
pub trait FollowWork {
    /// The parts of it they reach, borrowed together.
    fn follow_parts(&mut self) -> FollowParts<'_>;
}

/// The parts of a surface's work its artwork readers and follower reach.
#[cfg(feature = "rt")]
pub struct FollowParts<'a> {
    /// The desk the decodes are recorded on.
    pub desk: &'a mut ArtworkDesk<Lane>,
    /// The follows of the listed bundles it shows.
    pub follows: &'a mut BundleFollows<tairix_rt::File>,
    /// How many decodes may run at once.
    pub decodes: &'a mut Decodes,
    /// The routes a read is put on its volume's lane by.
    pub routes: &'a Routes,
}

/// What a surface's artwork readers share with the thread their followed
/// files wake: its wake, and the tokens the files' changes are reported
/// under.
#[cfg(feature = "rt")]
pub struct Follower {
    /// Wakes the follower to wait again over the waits armed since, and to
    /// leave once its desk stops.
    wake: tairix_rt::sync::WorkerWake,
    /// The token the next followed file's change is reported under.
    next_token: AtomicU64,
}

#[cfg(feature = "rt")]
impl Follower {
    /// A follower whose thread is yet to start, its wake unarmed where the
    /// kernel refuses the pipe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            wake: tairix_rt::sync::WorkerWake::create(),
            next_token: AtomicU64::new(WAKE_TOKEN + 1),
        }
    }

    /// Wake the follower: to wait again over the waits armed since, or to
    /// leave once its desk stops.
    pub fn nudge(&self) {
        self.wake.nudge();
    }

    /// The follower thread's whole life over `work`: make its wait, have the
    /// desk follow the bundles it remembers from then on, and wait over the
    /// followed files' changes and its own wake, each change having its
    /// bundle read again — a reader woken on `signal` for it — until the desk
    /// stops. A wait it cannot make, or one that fails, is stated through
    /// `report`, and the bundles are then read again only as their listings
    /// move.
    pub fn follow<W: FollowWork>(
        &self,
        (work, signal): (&Mutex<W>, &Condvar),
        report: impl Fn(Unfollowed),
    ) {
        let Some(wake) = self.wake.read_end() else {
            report(Unfollowed::NoWake);
            return;
        };
        let set = match tairix_rt::WaitSet::create() {
            Ok(set) => set,
            Err(err) => {
                report(Unfollowed::NoWait(err));
                return;
            }
        };
        if let Err(err) = set.add(WaitSourceKind::Stream, u64::from(wake), WAKE_TOKEN) {
            report(Unfollowed::WakeRefused(err));
            return;
        }
        {
            let mut held = work.lock();
            let FollowParts { desk, follows, .. } = held.follow_parts();
            if desk.stopping() {
                return;
            }
            follows.joined(set.handle());
            desk.follow_bundles();
        }
        loop {
            match set.wait_until(u64::MAX) {
                Ok(Some(WAKE_TOKEN)) => {
                    self.wake.drain();
                    if work.lock().follow_parts().desk.stopping() {
                        return;
                    }
                }
                Ok(Some(token)) => {
                    let moved = {
                        let mut held = work.lock();
                        let FollowParts { desk, follows, .. } = held.follow_parts();
                        let followed = follows.holder(token);
                        followed.is_some_and(|bundle| desk.moved(bundle))
                    };
                    if moved {
                        signal.notify_one();
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    work.lock().follow_parts().follows.lost();
                    signal.notify_one();
                    report(Unfollowed::WaitFailed(err));
                    return;
                }
            }
        }
    }

    /// Read and decode `job`, taken on `lane`, with no lock held — a listed
    /// bundle's files read first, their follow armed where its desk follows
    /// it, and not decoded again where they are as its picture was last read
    /// from — and deliver it, answering whether the embedder's loop is owed a
    /// wake.
    pub fn decode<W, R, D>(
        &self,
        (work, signal): (&Mutex<W>, &Condvar),
        (lane, job): (Lane, &ArtworkJob),
        seams: (&mut R, &mut D),
    ) -> bool
    where
        W: FollowWork,
        R: ArtworkReader + ?Sized,
        D: ArtworkRasteriser + ?Sized,
    {
        let produced = job.produce(
            seams,
            |bundle, reader| self.read_bundle((work, signal), bundle, reader),
            |bundle| work.lock().follow_parts().desk.files_of(bundle),
        );
        let mut held = work.lock();
        let FollowParts {
            desk,
            decodes,
            routes,
            ..
        } = held.follow_parts();
        // A decode recorded while every slot was taken woke no one.
        if decodes.ended() && desk.has_work() {
            signal.notify_one();
        }
        let route = |queued: &ArtworkJob| routes.lane_of_artwork(&queued.key);
        desk.deliver_produced(job, produced, route, &lane).wake()
    }

    /// What `bundle`'s own picture is read from now: its manifest and the icon
    /// it names, each at the version it reports. Where its desk follows it,
    /// the files' change waits are armed first, so no change after this read
    /// goes unreported.
    fn read_bundle<W, R>(
        &self,
        (work, signal): (&Mutex<W>, &Condvar),
        bundle: &ListedBundle,
        reader: &mut R,
    ) -> BundleFiles
    where
        W: FollowWork,
        R: ArtworkReader + ?Sized,
    {
        let set = {
            let mut held = work.lock();
            let FollowParts { desk, follows, .. } = held.follow_parts();
            follows.set().filter(|_| desk.follows(bundle))
        };
        let manifest = tairix_appstore::manifest_path(&bundle.dir);
        let mut waits = Vec::new();
        let mut refused = false;
        if let Some(set) = set {
            let resources = tairix_path::join(&bundle.dir, BundleEntry::Resources.as_str());
            refused |= !self.arm(set, &manifest, OpenFlags::READ, &mut waits);
            refused |= !self.arm(set, &resources, OpenFlags::DIRECTORY, &mut waits);
        }
        let icon = tairix_icon::bundle_icon_path(reader, &bundle.dir);
        if let (Some(set), Some(icon)) = (set, icon.as_deref()) {
            refused |= !self.arm(set, icon, OpenFlags::READ, &mut waits);
        }
        if let Some(set) = set {
            self.keep_waits((work, signal), bundle, (waits, set), refused);
        }
        BundleFiles {
            manifest: stamp_of(&manifest),
            icon: icon.as_deref().and_then(stamp_of),
        }
    }

    /// Open `path` as `flags` say and wait on its changes in the follower's
    /// `set` under a token of its own, keeping both in `waits`. Answers false
    /// only for a wait refused — its volume reports no change, or a limit was
    /// reached: a node not there yet has nothing to follow, and its making is
    /// a change to the folder above it.
    fn arm(
        &self,
        set: u64,
        path: &str,
        flags: OpenFlags,
        waits: &mut Vec<(u64, tairix_rt::File)>,
    ) -> bool {
        if waits.try_reserve(1).is_err() {
            return false;
        }
        let Ok(file) = tairix_rt::File::open(path.as_bytes(), flags) else {
            return true;
        };
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let fd = u64::from(file.fd());
        if tairix_rt::waitset_ctl(set, WaitSetOp::Add, WaitSourceKind::File, fd, token) != 0 {
            return false;
        }
        waits.push((token, file));
        true
    }

    /// Hold `waits`, `bundle`'s files' change waits just armed through the
    /// follower's wait `set`, in place of those it held, which a reader woken
    /// on `signal` ends on its lane, and wake the follower to wait over them.
    /// A refused wait gives the follow up, as does a bundle its desk stopped
    /// following while they were armed.
    fn keep_waits<W: FollowWork>(
        &self,
        (work, signal): (&Mutex<W>, &Condvar),
        bundle: &ListedBundle,
        (waits, set): (Vec<(u64, tairix_rt::File)>, u64),
        refused: bool,
    ) {
        let followed = FollowedBundle::new(bundle.clone(), waits);
        {
            let mut held = work.lock();
            let FollowParts { desk, follows, .. } = held.follow_parts();
            follows.keep(desk, (followed, set), refused);
        }
        signal.notify_one();
        self.wake.nudge();
    }
}

#[cfg(feature = "rt")]
impl Default for Follower {
    fn default() -> Self {
        Self::new()
    }
}

/// End `followed`, a follow let go, as the reader of its volume's lane does:
/// each of its waits taken out of the follower's before its file closes, so
/// no file opened later under the same number is refused a wait of its own.
#[cfg(feature = "rt")]
pub fn end<W: FollowWork>(work: &Mutex<W>, followed: FollowedBundle<tairix_rt::File>) {
    let set = work.lock().follow_parts().follows.set();
    for (token, file) in followed.into_waits() {
        if let Some(set) = set {
            let fd = u64::from(file.fd());
            let _ = tairix_rt::waitset_ctl(set, WaitSetOp::Del, WaitSourceKind::File, fd, token);
        }
        drop(file);
    }
}

/// The version of the node at `path`, read through a resolve-only open, or
/// `None` where nothing is there.
#[cfg(feature = "rt")]
fn stamp_of(path: &str) -> Option<DocumentStamp> {
    let file = tairix_rt::File::open(path.as_bytes(), OpenFlags::empty()).ok()?;
    file.stat().ok().map(|stat| DocumentStamp::of(&stat))
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;
