//! Mounting a disk image for the user who opens it, and ejecting what they
//! mounted.
//!
//! One rule for every launcher — the file manager, the desktop — as for a
//! document: the image is opened read-write where the user may write it,
//! unless they asked for it read-only, and handed to the kernel, which
//! recognises what it holds and attaches it at `/Storage/<name>`, private to
//! them (`plans/DISK-IMAGES.md`). Each launcher carries a mount out on a
//! `MountWorker`, and an eject wherever its own holds on the volume have let
//! go — `carry_out` is the one body of both — and tells the user what one
//! came to through [`MountDone::shown`], so both say it in the same words. A
//! volume that went away before what was written to it was saved is ejected
//! only once the user agrees to let that go ([`lost_eject_question`]).

use alloc::collections::BTreeSet;
#[cfg(feature = "rt")]
use alloc::collections::TryReserveError;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::{format, vec};
use core::ops::Bound;

#[cfg(any(feature = "rt", test))]
use tairix_abi::imageattach::ImageAttach;
use tairix_abi::imageattach::{ImageAccess, ImageAttachReply};
#[cfg(any(feature = "rt", test))]
use tairix_abi::netattach::Presentation;
use tairix_abi::sysinfo::{
    MountAvailability, ViewBindingRecord, VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED,
};
use tairix_abi::volume::DetachMode;
#[cfg(any(feature = "rt", test))]
use tairix_abi::volume::VolumeDetachRequest;
#[cfg(feature = "rt")]
use tairix_abi::volume::VOLUME_DETACH_LEN;
use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertSeverity, AlertSpec};
use tairix_abi::{Errno, STORAGE_CATALOG};

use crate::activate::{Activation, BundleIntent};
use crate::browser::{Browser, Location};
use crate::chrome::{ContextCommand, ContextMenuModel};
use crate::error::BrowseError;
use crate::places::{Place, Places, VolumeFacts};
use crate::source::DirectorySource;
use crate::vfs::components_from_absolute_path;

/// Why an act was refused before it began: as many wait as may.
pub const CROWDED: &str = "too many disks are being mounted or ejected";

/// Why a mount was refused: there was no memory for the worker it runs on.
pub const UNSERVED: &str = "there was not enough memory to mount disks";

/// Why a read-only mount was refused: the user has the image mounted
/// writable already, and the one volume a file holds cannot be both.
pub const MOUNTED_WRITABLE: &str =
    "it is mounted read-write already, so eject it to mount it read-only";

/// How writable a mount asks its volume to be: read-only when `read_only`,
/// otherwise writable where it can be.
const fn access(read_only: bool) -> ImageAccess {
    if read_only {
        ImageAccess::ReadOnly
    } else {
        ImageAccess::WritableWherePossible
    }
}

/// The attach a double-click asks for: private to the user, named from the
/// volume, as writable as [`access`] asks.
#[cfg(any(feature = "rt", test))]
const fn private(read_only: bool) -> ImageAttach<'static> {
    ImageAttach {
        name: None,
        presentation: Presentation::Private,
        access: access(read_only),
        target: None,
        expects: None,
        adopt: true,
        file_name: None,
    }
}

/// The switch that makes the file manager's operand a disk image or archive
/// to attach and show, rather than a place to open at: how the desktop starts
/// one to open an image when none is running.
///
/// Spelled once because the desktop session names it and the file manager
/// parses it, and neither may depend on the other.
pub const MOUNT_SWITCH: &str = "--mount";

/// Why an eject is asked for, which decides how the kernel takes the volume
/// down and what the user is told of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EjectAsked {
    /// The user asked to eject it.
    ByUser,
    /// The user asked to eject a volume that went away with what was written
    /// to it unsaved, agreeing to let that go.
    LettingGo,
    /// The last window showing a disk image or archive the user opened closed:
    /// it is taken down only if nothing is using it, and is otherwise left as
    /// it is without a word.
    Finished,
}

impl EjectAsked {
    /// The detach mode an eject asked so carries out: one letting go forces a
    /// volume only if it is lost still.
    #[must_use]
    pub const fn mode(self) -> DetachMode {
        match self {
            Self::ByUser => DetachMode::Plain,
            Self::LettingGo => DetachMode::Lost,
            Self::Finished => DetachMode::Idle,
        }
    }
}

/// The detach of the volume published as `volume_id`, asked as `asked`.
#[cfg(any(feature = "rt", test))]
const fn detach_request(volume_id: [u8; VOLUME_ID_LEN], asked: EjectAsked) -> VolumeDetachRequest {
    VolumeDetachRequest {
        volume_id,
        mode: asked.mode(),
    }
}

/// Detach the volume published as `volume_id`, which the caller attached,
/// once what it holds is written back, as `asked` asks.
///
/// # Errors
///
/// The kernel's refusal: [`Errno::Busy`] while a file on it is open or an
/// image volume lies on it, [`Errno::PermissionDenied`] for a volume the
/// caller may not detach.
#[cfg(feature = "rt")]
fn eject(volume_id: [u8; VOLUME_ID_LEN], asked: EjectAsked) -> Result<(), Errno> {
    let mut frame = [0u8; VOLUME_DETACH_LEN];
    detach_request(volume_id, asked).encode(&mut frame)?;
    match tairix_rt::volume_detach(&frame) {
        0 => Ok(()),
        err => Err(Errno::from_syscall(err)),
    }
}

/// The button index of the eject itself in the question
/// [`lost_eject_question`] builds.
pub const LOST_EJECT_INDEX: usize = 1;

/// The question put before ejecting `name`, a volume that went away before
/// what was written to it was saved: ejecting it lets that go and closes
/// what is open on it, so it destroys, and is never the answer `Enter` gives.
/// `None` only if the compiled-in button labels were not ones an alert can
/// carry.
#[must_use]
pub fn lost_eject_question(name: &str) -> Option<AlertSpec> {
    let cancel = AlertButton::new("Cancel", AlertButtonRole::Cancel).ok()?;
    let eject = AlertButton::new("Eject", AlertButtonRole::Destructive).ok()?;
    AlertSpec::fitted(
        AlertSeverity::Warning,
        &format!("Eject \u{201c}{}\u{201d}?", tairix_util::utf8::shown(name)),
        "It went away before everything written to it was saved, and what was \
         not saved is lost. Ejecting it lets that go and closes any file still \
         open on it.",
        &[cancel, eject],
    )
    .ok()
}

/// A disk image asked to be mounted, or a volume asked to be ejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountAct {
    /// Mount the image at `path`, read-only when `read_only`.
    Attach {
        /// The image's absolute path.
        path: String,
        /// Whether to mount it read-only however its file may be written.
        read_only: bool,
    },
    /// Eject the volume published as `volume_id`, shown as `name`.
    Eject {
        /// The identity a detach names it by.
        volume_id: [u8; VOLUME_ID_LEN],
        /// What the user calls it.
        name: String,
        /// Why it is ejected: a volume lost with writes unsaved is let go only
        /// once the user is asked.
        asked: EjectAsked,
    },
}

impl MountAct {
    /// The eject a choice on the rail's `row` asks for: of the volume the row
    /// shows, which `viewer` must have attached.
    #[must_use]
    pub fn eject_row(row: &Place, viewer: Option<u32>) -> Option<Self> {
        let facts = row
            .facts()
            .filter(|facts| viewer.is_some_and(|uid| facts.ejectable_by(uid)))?;
        Some(Self::eject(facts, row.label()))
    }

    /// The eject the entry `browser` has chosen asks for: of the volume that
    /// catalog entry shows, where its menu offers `viewer` an eject.
    #[must_use]
    pub fn eject_chosen<S: DirectorySource>(
        browser: &Browser<S>,
        viewer: Option<u32>,
    ) -> Option<Self> {
        let uid = viewer?;
        let offered = ContextMenuModel::for_browser(browser, false).viewed_by(uid);
        if !offered.is_enabled(ContextCommand::Eject) {
            return None;
        }
        let entry = browser.chosen_entry()?;
        Some(Self::eject(entry.volume()?, entry.name()))
    }

    /// The read-only mount the entry `browser` has chosen asks for, where its
    /// menu offers one: `None` where it offers none, or nothing is chosen.
    ///
    /// # Errors
    ///
    /// Why the image cannot be named, as an open of it is refused: a name no
    /// path may carry, say.
    pub fn read_only_of<S: DirectorySource>(
        browser: &mut Browser<S>,
    ) -> Result<Option<Self>, BrowseError> {
        let offered = ContextMenuModel::for_browser(browser, false);
        if !offered.is_enabled(ContextCommand::MountReadOnly) {
            return Ok(None);
        }
        match browser.activate_selected(BundleIntent::Launch) {
            Ok(Activation::MountImage { path }) => Ok(Some(Self::Attach {
                path,
                read_only: true,
            })),
            Ok(_) | Err(BrowseError::NoSuchEntry) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The eject the user asked for of the volume `facts` describe, shown as
    /// `name`.
    #[must_use]
    pub fn eject(facts: &VolumeFacts, name: &str) -> Self {
        Self::Eject {
            volume_id: facts.volume_id,
            name: String::from(name),
            asked: if facts.availability == MountAvailability::UnavailableLost {
                EjectAsked::LettingGo
            } else {
                EjectAsked::ByUser
            },
        }
    }

    /// The eject of the disk image or archive published as `volume_id`, shown
    /// as `name`, that the user opened and has finished with.
    #[must_use]
    pub fn finished(volume_id: [u8; VOLUME_ID_LEN], name: &str) -> Self {
        Self::Eject {
            volume_id,
            name: String::from(name),
            asked: EjectAsked::Finished,
        }
    }

    /// What the user calls what the act is about: the image's file name, or
    /// the volume's.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Attach { path, .. } => path.rsplit('/').next().unwrap_or(path),
            Self::Eject { name, .. } => name,
        }
    }

    /// This act refused for `reason`, as the user is told it.
    #[must_use]
    pub fn refused(&self, reason: &'static str) -> MountRefusal {
        let undone = match self {
            Self::Attach { .. } => "mounted",
            Self::Eject { .. } => "ejected",
        };
        MountRefusal {
            headline: tairix_util::utf8::refusal_headline(self.name(), undone),
            reason,
        }
    }

    /// This act, taken out of `self` without copying what it names: the
    /// worker hands the act it was lent back in its answer.
    #[cfg(feature = "rt")]
    fn take(&mut self) -> Self {
        match self {
            Self::Attach { path, read_only } => Self::Attach {
                path: core::mem::take(path),
                read_only: *read_only,
            },
            Self::Eject {
                volume_id,
                name,
                asked,
            } => Self::Eject {
                volume_id: *volume_id,
                name: core::mem::take(name),
                asked: *asked,
            },
        }
    }
}

/// What a [`MountAct`] came to.
#[derive(Debug)]
pub struct MountDone {
    /// The act.
    pub act: MountAct,
    /// What carrying it out answered.
    pub answer: Result<Mounted, Errno>,
}

impl MountDone {
    /// The volume this attach made — its identity and its name in the storage
    /// catalog — or `None` where it made none: one already attached is
    /// someone's own already, so only what this attach made is the
    /// attaching surface's to let go.
    #[must_use]
    pub fn attached(&self) -> Option<([u8; VOLUME_ID_LEN], &str)> {
        match &self.answer {
            Ok(Mounted::Image(ImageAttachReply::Attached(volume))) => {
                Some((volume.volume_id, volume.name()))
            }
            _ => None,
        }
    }

    /// Where the volume to show the user is, root-first — the one the image
    /// holds, whether it was attached now or already — or `None` for a volume
    /// ejected, which the mount table reports.
    ///
    /// # Errors
    ///
    /// The act's refusal, saying why: a read-only mount of an image the user
    /// has mounted writable already is refused rather than shown, so a
    /// window onto it is never taken for the read-only one asked for.
    pub fn shown(self) -> Result<Option<Vec<String>>, MountRefusal> {
        // One the user finished with that something is still using stays, and
        // one gone already is gone as asked: the user asked for neither, so is
        // told of neither.
        if let (MountAct::Eject { asked, .. }, Err(err)) = (&self.act, &self.answer) {
            let gone = *asked == EjectAsked::Finished && *err == Errno::NotFound;
            if gone || asked.mode().left_in_use(*err) {
                return Ok(None);
            }
        }
        let asked = match self.act {
            MountAct::Attach { read_only, .. } => access(read_only),
            MountAct::Eject { .. } => ImageAccess::WritableWherePossible,
        };
        let reason = match self.answer {
            // Never taken up for this session, so never shown as what was
            // asked for.
            Ok(Mounted::Image(ImageAttachReply::AlreadyAttached(volume)))
                if !asked.accepts(volume.read_only()) =>
            {
                MOUNTED_WRITABLE
            }
            Ok(Mounted::Image(
                ImageAttachReply::Attached(volume) | ImageAttachReply::AlreadyAttached(volume),
            )) => {
                return Ok(Some(vec![
                    String::from(STORAGE_CATALOG),
                    String::from(volume.name()),
                ]))
            }
            Ok(Mounted::Ejected) => return Ok(None),
            Ok(Mounted::Image(ImageAttachReply::Refused(why))) => why.reason(),
            // No launcher here asks for a directory or a name of its own.
            Ok(Mounted::Image(
                ImageAttachReply::TargetRefused(errno) | ImageAttachReply::NameRefused(errno),
            )) => attach_refusal(errno),
            Err(err) => match self.act {
                MountAct::Attach { .. } => attach_refusal(err),
                MountAct::Eject { .. } => eject_refusal(err),
            },
        };
        Err(self.act.refused(reason))
    }
}

/// What a [`MountAct`] carried out.
#[derive(Debug)]
pub enum Mounted {
    /// The image's answer: the volume it holds, or why it holds none.
    Image(ImageAttachReply),
    /// The volume was ejected.
    Ejected,
}

/// A mount or eject that was refused, as the user is told it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountRefusal {
    /// What could not be done, naming the image or the volume.
    pub headline: String,
    /// Why, in the clause a notice states it in.
    pub reason: &'static str,
}

/// The worker an interactive surface carries its mounts out on, each
/// answered in turn: an attach waits for its volume's driver, which on the
/// surface's loop, or behind its other reads, would hold everything else up.
#[cfg(feature = "rt")]
pub type MountWorker = tairix_rt::work::Worker<
    (),
    MountAct,
    MountDone,
    tairix_util::defer::JobQueue<MountAct, MountDone>,
>;

/// How many acts may wait, answered or not, for a `MountWorker` or for what
/// must happen ahead of one: each is a click of the user's, so a burst past
/// this is refused, saying why.
pub const ACTS_WAITING: usize = 8;

/// A place a volume is shown at besides its catalog entry: a view binding of
/// it, such as the directory `mount IMAGE DIR` bound it at too.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Projection {
    /// The identity of the volume shown.
    pub volume_id: [u8; VOLUME_ID_LEN],
    /// Where, root-first.
    pub components: Vec<String>,
}

impl Projection {
    /// The place `view` shows its volume at, or `None` for a binding of no
    /// published volume, or one whose path does not parse.
    #[must_use]
    pub fn of(view: &ViewBindingRecord) -> Option<Self> {
        if view.volume_id() == VOLUME_ID_UNPUBLISHED {
            return None;
        }
        let target = core::str::from_utf8(view.target_bytes()).ok()?;
        Some(Self {
            volume_id: view.volume_id(),
            components: components_from_absolute_path(target).ok()?,
        })
    }
}

/// The places the volume published as `volume_id` is shown at — its rail
/// row or, for one the rail does not offer, its catalog entry in `places`,
/// and each of `projections` of it: where a surface's own windows must leave
/// before an eject of it, or their holds refuse it.
pub fn shown_at<'a>(
    places: &'a Places,
    projections: &'a [Projection],
    volume_id: [u8; VOLUME_ID_LEN],
) -> impl Iterator<Item = &'a [String]> {
    places
        .rows()
        .iter()
        .chain(places.unoffered())
        .filter(move |row| {
            row.facts()
                .is_some_and(|facts| facts.volume_id == volume_id)
        })
        .map(Place::components)
        .chain(
            projections
                .iter()
                .filter(move |shown| shown.volume_id == volume_id)
                .map(|shown| shown.components.as_slice()),
        )
}

/// Whether the place `at` lies within one of `roots`, the places a volume
/// being ejected is shown at: by whole components, so a sibling whose name
/// merely begins with a root's is not.
#[must_use]
pub fn within(roots: &[&[String]], at: &[String]) -> bool {
    roots.iter().any(|root| at.starts_with(root))
}

/// Whether any of the places `reached` lies within one of `roots`, as
/// [`within`] answers for one, at one range query a root however many places
/// there are: those beginning with a root sort together from the root itself.
#[must_use]
pub fn any_within(reached: &BTreeSet<Vec<String>>, roots: &[&[String]]) -> bool {
    roots.iter().any(|root| {
        reached
            .range::<[String], _>((Bound::Included(*root), Bound::Unbounded))
            .next()
            .is_some_and(|at| at.starts_with(root))
    })
}

/// Whether `browser` shows, or is on its way to, a place within one of
/// `roots`.
pub fn holds_within<S: DirectorySource>(browser: &Browser<S>, roots: &[&[String]]) -> bool {
    browser.folder().is_some_and(|at| within(roots, at))
        || matches!(browser.listing_target(), Some(Location::Folder(at)) if within(roots, at))
}

/// A [`MountWorker`], not yet started.
///
/// # Errors
///
/// The refusal of the memory for the acts it holds.
#[cfg(feature = "rt")]
pub fn mount_worker() -> Result<MountWorker, TryReserveError> {
    tairix_rt::work::Worker::queued(
        |(): &mut (), act: &mut MountAct| carry_out(act.take()),
        (),
        tairix_rt::sync::WorkerWake::create(),
        ACTS_WAITING,
    )
}

/// Carry `act` out on the calling thread, as a [`MountWorker`] does a mount:
/// an attach waits for the volume's driver, an eject for its flush. An app
/// whose own reads could hold the volume an eject takes out carries the eject
/// out where those reads run, so none overlaps it.
#[cfg(feature = "rt")]
#[must_use]
pub fn carry_out(act: MountAct) -> MountDone {
    let answer = match &act {
        MountAct::Attach { path, read_only } => {
            tairix_rt::image_attach_path(path.as_bytes(), &private(*read_only)).map(Mounted::Image)
        }
        MountAct::Eject {
            volume_id, asked, ..
        } => eject(*volume_id, *asked).map(|()| Mounted::Ejected),
    };
    MountDone { act, answer }
}

/// Why mounting an image was refused, given the kernel's `err`.
const fn attach_refusal(err: Errno) -> &'static str {
    match err {
        Errno::PermissionDenied => "you may not read it",
        Errno::NotFound => "it does not exist",
        Errno::Busy => {
            "another program is writing it, it is being mounted or ejected already, or \
             the volume mounted from it was lost and must be ejected first"
        }
        Errno::LimitExceeded => "too many disk images are mounted, or nested too deeply",
        Errno::TimedOut => "its disk's driver did not answer",
        Errno::Interrupted => "mounting it was interrupted",
        Errno::OutOfMemory => "there was not enough memory to mount it",
        Errno::NotSupported => "the volume it is on cannot name it",
        Errno::NotImplemented => "this system does not mount disk images",
        Errno::DeviceOffline => "its device was removed",
        Errno::DeviceFault | Errno::MediumError => "its device failed",
        _ => "it could not be mounted",
    }
}

/// Why ejecting a volume was refused, given the kernel's `err`.
const fn eject_refusal(err: Errno) -> &'static str {
    match err {
        Errno::Busy => "a file on it is open, or a disk image on it is mounted",
        Errno::PermissionDenied => "only whoever mounted it ejects it",
        Errno::NotFound => "it is not mounted",
        Errno::DeviceFault => "what it holds could not be written back",
        Errno::OutOfMemory => "there was not enough memory to eject it",
        _ => "it could not be ejected",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        attach_refusal, detach_request, eject_refusal, holds_within, lost_eject_question, private,
        shown_at, within, EjectAsked, MountAct, MountDone, MountRefusal, Mounted, Projection,
        CROWDED, LOST_EJECT_INDEX, MOUNTED_WRITABLE, UNSERVED,
    };
    use crate::browser::Browser;
    use crate::entry::Entry;
    use crate::places::{Places, Volume, VolumeFacts};
    use crate::source::{DirectorySource, Listing};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use tairix_abi::driver::filesystem::MountFlags;
    use tairix_abi::imageattach::{ImageAccess, ImageAttachReply, ImageRefusal};
    use tairix_abi::netattach::{AttachReply, Presentation};
    use tairix_abi::sysinfo::{MountAvailability, ViewBindingRecord};
    use tairix_abi::volume::DetachMode;
    use tairix_abi::window_ipc::{AlertButtonRole, AlertSeverity};
    use tairix_abi::Errno;

    fn attach(path: &str) -> MountAct {
        MountAct::Attach {
            path: String::from(path),
            read_only: false,
        }
    }

    fn eject(name: &str) -> MountAct {
        MountAct::Eject {
            volume_id: [7; 16],
            name: String::from(name),
            asked: EjectAsked::ByUser,
        }
    }

    fn done(act: MountAct, answer: Result<Mounted, Errno>) -> MountDone {
        MountDone { act, answer }
    }

    fn volume(name: &str) -> AttachReply {
        AttachReply::new([9; 16], name, false).expect("a valid name")
    }

    #[test]
    fn a_double_click_mounts_privately_under_the_volumes_own_name() {
        for (read_only, access) in [
            (false, ImageAccess::WritableWherePossible),
            (true, ImageAccess::ReadOnly),
        ] {
            let attach = private(read_only);
            assert_eq!(attach.presentation, Presentation::Private);
            assert_eq!(attach.access, access);
            assert_eq!(
                (attach.name, attach.target, attach.expects),
                (None, None, None)
            );
        }
    }

    #[test]
    fn a_mounted_image_shows_its_volume_whether_new_or_already_there() {
        for reply in [
            ImageAttachReply::Attached(volume("workbench")),
            ImageAttachReply::AlreadyAttached(volume("workbench")),
        ] {
            let shown = done(attach("/Users/ada/Home/wb.adf"), Ok(Mounted::Image(reply))).shown();
            assert_eq!(
                shown,
                Ok(Some(vec![
                    String::from("Storage"),
                    String::from("workbench")
                ]))
            );
        }
        assert_eq!(
            done(eject("workbench"), Ok(Mounted::Ejected)).shown(),
            Ok(None)
        );
    }

    #[test]
    fn a_read_only_mount_of_an_image_mounted_writable_says_so_rather_than_showing_it() {
        let read_only = MountAct::Attach {
            path: String::from("/Users/ada/Home/Lemmings.adf"),
            read_only: true,
        };
        let writable = ImageAttachReply::AlreadyAttached(volume("lemmings"));
        assert_eq!(
            done(read_only.clone(), Ok(Mounted::Image(writable))).shown(),
            Err(MountRefusal {
                headline: String::from("\u{201c}Lemmings.adf\u{201d} could not be mounted"),
                reason: MOUNTED_WRITABLE,
            })
        );
        let already_read_only = ImageAttachReply::AlreadyAttached(
            AttachReply::new([9; 16], "lemmings", true).expect("a valid name"),
        );
        assert!(
            done(read_only, Ok(Mounted::Image(already_read_only)))
                .shown()
                .is_ok(),
            "mounted read-only already: shown"
        );
    }

    #[test]
    fn a_refused_act_names_what_was_not_done_and_says_why() {
        assert_eq!(
            done(
                attach("/Users/ada/Home/Lemmings.adf"),
                Ok(Mounted::Image(ImageAttachReply::Refused(
                    ImageRefusal::Partitioned
                )))
            )
            .shown(),
            Err(MountRefusal {
                headline: String::from("\u{201c}Lemmings.adf\u{201d} could not be mounted"),
                reason: "it holds a partitioned disk, whose partitions are not mounted",
            })
        );
        assert_eq!(
            done(attach("/Users/ada/Home/disc.iso"), Err(Errno::Busy)).shown(),
            Err(MountRefusal {
                headline: String::from("\u{201c}disc.iso\u{201d} could not be mounted"),
                reason: "another program is writing it, it is being mounted or ejected \
                         already, or the volume mounted from it was lost and must be ejected \
                         first",
            })
        );
        assert_eq!(
            done(eject("floppy\u{202e}"), Err(Errno::Busy)).shown(),
            Err(MountRefusal {
                headline: String::from("\u{201c}floppy\u{fffd}\u{201d} could not be ejected"),
                reason: "a file on it is open, or a disk image on it is mounted",
            })
        );
        for reason in [CROWDED, UNSERVED] {
            assert_eq!(
                attach("/Users/ada/Home/a.hdf").refused(reason).reason,
                reason
            );
        }
    }

    #[test]
    fn an_unexplained_refusal_is_still_stated() {
        assert_eq!(attach_refusal(Errno::Stale), "it could not be mounted");
        assert_eq!(eject_refusal(Errno::Stale), "it could not be ejected");
        assert_eq!(
            eject_refusal(Errno::OutOfMemory),
            "there was not enough memory to eject it"
        );
    }

    /// Every folder lists one folder, `sub`, so a browser may stand anywhere,
    /// and a listing of `pending` waits.
    struct Anywhere;

    impl DirectorySource for Anywhere {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            if components.last().is_some_and(|last| last == "pending") {
                return Ok(Listing::Pending);
            }
            Ok(Listing::Ready(vec![Entry::directory("sub")]))
        }
    }

    fn components(path: &[&str]) -> Vec<String> {
        path.iter().copied().map(String::from).collect()
    }

    /// The facts of an image volume `uid` attached, `availability` as it is.
    fn image_facts(uid: u32, availability: MountAvailability) -> VolumeFacts {
        VolumeFacts {
            filesystem: String::from("affs"),
            free: None,
            availability,
            owner: Some(uid),
            volume_id: [5; 16],
            read_only: false,
        }
    }

    /// A rail holding one image volume `uid` attached, at `/Storage/disk`.
    fn rail(uid: u32) -> Places {
        Places::new(
            &components(&["Users", "ada"]),
            &[Volume {
                label: String::from("disk"),
                target: String::from("/Storage/disk"),
                medium: None,
                facts: image_facts(uid, MountAvailability::Available),
                offered: true,
            }],
        )
    }

    #[test]
    fn a_place_is_within_a_root_by_whole_components() {
        let disk = components(&["Storage", "disk"]);
        let roots = [disk.as_slice()];
        assert!(within(&roots, &disk), "the root itself");
        assert!(within(&roots, &components(&["Storage", "disk", "Games"])));
        assert!(
            !within(&roots, &components(&["Storage", "disk2"])),
            "a sibling whose name begins with the root's"
        );
        assert!(
            !within(&roots, &components(&["Storage"])),
            "the catalog above"
        );
        assert!(!within(&[], &disk), "nothing is within no roots");
    }

    #[test]
    fn an_eject_reaches_a_volume_no_rail_row_offers() {
        let places = Places::new(
            &components(&["Users", "ada"]),
            &[Volume {
                label: String::from("game"),
                target: String::from("/Storage/game"),
                medium: None,
                facts: image_facts(1000, MountAvailability::UnavailableLost),
                offered: false,
            }],
        );
        assert!(places.rows().iter().all(|row| row.facts().is_none()));
        let roots: Vec<&[String]> = shown_at(&places, &[], [5; 16]).collect();
        assert_eq!(roots, [components(&["Storage", "game"]).as_slice()]);
    }

    #[test]
    fn a_lost_volume_is_ejected_by_a_detach_that_takes_only_the_lost() {
        let lost = MountAct::eject(
            &image_facts(1000, MountAvailability::UnavailableLost),
            "game",
        );
        assert_eq!(
            lost,
            MountAct::Eject {
                volume_id: [5; 16],
                name: String::from("game"),
                asked: EjectAsked::LettingGo,
            }
        );
        assert_eq!(
            detach_request([5; 16], EjectAsked::LettingGo).mode,
            DetachMode::Lost
        );
        for availability in [
            MountAvailability::Available,
            MountAvailability::UnavailableDirty,
            MountAvailability::Recovering,
        ] {
            assert!(matches!(
                MountAct::eject(&image_facts(1000, availability), "game"),
                MountAct::Eject {
                    asked: EjectAsked::ByUser,
                    ..
                }
            ));
        }
        let plain = detach_request([5; 16], EjectAsked::ByUser);
        assert_eq!((plain.volume_id, plain.mode), ([5; 16], DetachMode::Plain));
    }

    #[test]
    fn only_a_volume_the_attach_made_is_the_attachers_to_let_go() {
        let made = done(
            attach("/Users/ada/Home/wb.adf"),
            Ok(Mounted::Image(ImageAttachReply::Attached(volume(
                "workbench",
            )))),
        );
        assert_eq!(made.attached(), Some(([9; 16], "workbench")));
        let there = done(
            attach("/Users/ada/Home/wb.adf"),
            Ok(Mounted::Image(ImageAttachReply::AlreadyAttached(volume(
                "workbench",
            )))),
        );
        assert_eq!(there.attached(), None, "someone's own already");
        assert_eq!(done(attach("/x.adf"), Err(Errno::Busy)).attached(), None);
        assert_eq!(done(eject("disk"), Ok(Mounted::Ejected)).attached(), None);
    }

    /// Places within a root are found among any number of others, by whole
    /// components: a sibling whose name begins with the root's is not one, and
    /// a place sorting just before the root is no place within it.
    #[test]
    fn any_within_finds_a_place_within_a_root_among_many() {
        let path = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| String::from(*part))
                .collect::<Vec<_>>()
        };
        let mut reached = alloc::collections::BTreeSet::new();
        for at in 0..1000 {
            reached.insert(path(&["Users", "ann", "Home", &alloc::format!("f{at}")]));
        }
        reached.insert(path(&["Storage", "game"]));
        reached.insert(path(&["Storage", "gamez", "x"]));
        let roots = |parts: &[&str]| path(parts);
        let game = roots(&["Storage", "game"]);
        let disc = roots(&["Storage", "disc"]);
        let gam = roots(&["Storage", "gam"]);
        let home = roots(&["Users", "ann", "Home"]);
        assert!(
            super::any_within(&reached, &[game.as_slice()]),
            "the root itself"
        );
        assert!(super::any_within(&reached, &[home.as_slice()]));
        assert!(!super::any_within(&reached, &[disc.as_slice()]));
        assert!(
            !super::any_within(&reached, &[gam.as_slice()]),
            "by whole components, not by spelling"
        );
        assert!(super::any_within(
            &reached,
            &[disc.as_slice(), game.as_slice()]
        ));
        assert!(!super::any_within(
            &alloc::collections::BTreeSet::new(),
            &[game.as_slice()]
        ));
    }

    #[test]
    fn a_finished_image_is_ejected_only_if_idle_and_silently_kept_if_in_use() {
        let finished = MountAct::finished([5; 16], "game");
        assert_eq!(
            detach_request([5; 16], EjectAsked::Finished).mode,
            DetachMode::Idle
        );
        for in_use in [Errno::Busy, Errno::NotEmpty] {
            assert_eq!(
                done(finished.clone(), Err(in_use)).shown(),
                Ok(None),
                "an image still in use stays, and nothing is said of it"
            );
        }
        assert!(
            done(finished.clone(), Err(Errno::DeviceFault))
                .shown()
                .is_err(),
            "what it held could not be written back: that is said"
        );
        assert_eq!(
            done(finished.clone(), Err(Errno::NotFound)).shown(),
            Ok(None),
            "an image unmounted some other way meanwhile is gone as asked"
        );
        assert_eq!(done(finished, Ok(Mounted::Ejected)).shown(), Ok(None));
        assert!(
            done(eject("disk"), Err(Errno::Busy)).shown().is_err(),
            "the user's own eject refused is said"
        );
        assert!(
            done(eject("disk"), Err(Errno::NotFound)).shown().is_err(),
            "the user's own eject of what is not mounted is said"
        );
    }

    #[test]
    fn ejecting_a_lost_volume_asks_first() {
        let question = lost_eject_question("game\u{202e}").expect("a question");
        assert_eq!(question.severity(), AlertSeverity::Warning);
        assert!(
            question.headline().contains("\u{201c}game\u{fffd}\u{201d}"),
            "{}",
            question.headline()
        );
        let buttons: Vec<_> = question.buttons().collect();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].role(), AlertButtonRole::Cancel);
        assert_eq!(
            buttons[LOST_EJECT_INDEX].role(),
            AlertButtonRole::Destructive
        );
        assert_eq!(buttons[LOST_EJECT_INDEX].label(), "Eject");
    }

    #[test]
    fn a_rail_row_ejects_only_a_volume_its_viewer_attached() {
        let places = rail(1000);
        let row = places
            .rows()
            .iter()
            .find(|row| row.facts().is_some())
            .expect("the volume's row");
        assert_eq!(
            MountAct::eject_row(row, Some(1000)),
            Some(MountAct::Eject {
                volume_id: [5; 16],
                name: String::from("disk"),
                asked: EjectAsked::ByUser,
            })
        );
        assert_eq!(MountAct::eject_row(row, Some(1001)), None, "another's");
        assert_eq!(MountAct::eject_row(row, None), None, "an unknown viewer");
        assert!(
            places.rows()[0].facts().is_none()
                && MountAct::eject_row(&places.rows()[0], Some(1000)).is_none(),
            "a folder of the user's ejects nothing"
        );
    }

    #[test]
    fn an_eject_takes_every_window_on_its_volume_and_none_elsewhere() {
        let places = rail(1000);
        let bound = |target: &[u8], id: u8| {
            Projection::of(
                &ViewBindingRecord::new(target, b"/", MountFlags::NOEXEC, [id; 16])
                    .expect("a binding"),
            )
        };
        let projections: Vec<Projection> = [
            bound(b"/Users/ada/Home/wb", 5),
            bound(b"/Users/ada/Home/other", 6),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(
            bound(b"/Storage/data", 0),
            None,
            "an unpublished volume is no one's to eject"
        );
        let roots: Vec<&[String]> = shown_at(&places, &projections, [5; 16]).collect();
        assert_eq!(
            roots,
            [
                components(&["Storage", "disk"]).as_slice(),
                components(&["Users", "ada", "Home", "wb"]).as_slice()
            ],
            "its catalog entry and every other place it is bound at"
        );
        assert_eq!(shown_at(&places, &[], [6; 16]).count(), 0, "another volume");
        let at = |path: &[&str]| Browser::open_at(Anywhere, components(path)).expect("lists");
        assert!(holds_within(&at(&["Storage", "disk"]), &roots), "its root");
        assert!(
            holds_within(&at(&["Storage", "disk", "sub"]), &roots),
            "within it"
        );
        assert!(!holds_within(&at(&["Storage"]), &roots), "the catalog");
        assert!(
            !holds_within(&at(&["Storage", "disk2"]), &roots),
            "a sibling"
        );
        assert!(!holds_within(&at(&["Users", "ada"]), &roots));
        let mut on_its_way = at(&["Users", "ada"]);
        on_its_way
            .navigate_to(components(&["Storage", "disk", "pending"]))
            .expect("asked for");
        assert!(on_its_way.listing_target().is_some(), "the listing waits");
        assert!(
            holds_within(&on_its_way, &roots),
            "a navigation there holds it too"
        );
        let mut from_the_catalog = at(&["Storage"]);
        from_the_catalog
            .navigate_to(components(&["Storage", "disk", "pending"]))
            .expect("asked for");
        assert!(holds_within(&from_the_catalog, &roots));
        assert_eq!(
            from_the_catalog.navigate_to(components(&["Storage"])),
            Ok(true),
            "already there, its navigation onto the volume given up for a fresh read"
        );
        assert!(
            !holds_within(&from_the_catalog, &roots),
            "sent to the catalog it shows, its navigation onto the volume is given up"
        );
    }
}
