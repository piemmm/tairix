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
//! came to through [`MountDone::shown`], so both say it in the same words.

#[cfg(feature = "rt")]
use alloc::collections::TryReserveError;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::{format, vec};

use tairix_abi::imageattach::ImageAttachReply;
#[cfg(any(feature = "rt", test))]
use tairix_abi::imageattach::{ImageAccess, ImageAttach};
#[cfg(any(feature = "rt", test))]
use tairix_abi::netattach::Presentation;
use tairix_abi::sysinfo::{ViewBindingRecord, VOLUME_ID_UNPUBLISHED};
#[cfg(feature = "rt")]
use tairix_abi::volume::{VolumeDetachRequest, VOLUME_DETACH_LEN};
use tairix_abi::{Errno, STORAGE_CATALOG};

use crate::browser::{Browser, Location};
use crate::places::{Place, Places};
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

/// The attach a double-click asks for: private to the user, named from the
/// volume, read-only when `read_only` and otherwise writable where it can be.
#[cfg(any(feature = "rt", test))]
const fn private(read_only: bool) -> ImageAttach<'static> {
    ImageAttach {
        name: None,
        presentation: Presentation::Private,
        access: if read_only {
            ImageAccess::ReadOnly
        } else {
            ImageAccess::WritableWherePossible
        },
        target: None,
        expects: None,
        adopt: true,
    }
}

/// Detach the volume published as `volume_id`, which the caller attached,
/// once what it holds is written back.
///
/// # Errors
///
/// The kernel's refusal: [`Errno::Busy`] while a file on it is open or an
/// image volume lies on it, [`Errno::PermissionDenied`] for a volume the
/// caller may not detach.
#[cfg(feature = "rt")]
fn eject(volume_id: [u8; 16]) -> Result<(), Errno> {
    let mut frame = [0u8; VOLUME_DETACH_LEN];
    VolumeDetachRequest {
        volume_id,
        force: false,
    }
    .encode(&mut frame)?;
    match tairix_rt::volume_detach(&frame) {
        0 => Ok(()),
        err => Err(Errno::from_syscall(err)),
    }
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
        volume_id: [u8; 16],
        /// What the user calls it.
        name: String,
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
        Some(Self::Eject {
            volume_id: facts.volume_id,
            name: String::from(row.label()),
        })
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
            headline: format!(
                "\u{201c}{}\u{201d} could not be {undone}",
                tairix_util::utf8::shown(self.name())
            ),
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
            Self::Eject { volume_id, name } => Self::Eject {
                volume_id: *volume_id,
                name: core::mem::take(name),
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
        let asked_read_only = matches!(
            self.act,
            MountAct::Attach {
                read_only: true,
                ..
            }
        );
        let reason = match self.answer {
            Ok(Mounted::Image(ImageAttachReply::AlreadyAttached(volume)))
                if asked_read_only && !volume.read_only() =>
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
    pub volume_id: [u8; 16],
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
/// row in `places` and each of `projections` of it: where a surface's own
/// windows must leave before an eject of it, or their holds refuse it.
pub fn shown_at<'a>(
    places: &'a Places,
    projections: &'a [Projection],
    volume_id: [u8; 16],
) -> impl Iterator<Item = &'a [String]> {
    places
        .rows()
        .iter()
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

/// Whether `browser` shows, or is on its way to, a place within one of
/// `roots`.
pub fn holds_within<S: DirectorySource>(browser: &Browser<S>, roots: &[&[String]]) -> bool {
    let within = |at: &[String]| roots.iter().any(|root| at.starts_with(root));
    browser.folder().is_some_and(within)
        || matches!(browser.listing_target(), Some(Location::Folder(at)) if within(at))
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
        MountAct::Eject { volume_id, .. } => eject(*volume_id).map(|()| Mounted::Ejected),
    };
    MountDone { act, answer }
}

/// Why mounting an image was refused, given the kernel's `err`.
const fn attach_refusal(err: Errno) -> &'static str {
    match err {
        Errno::PermissionDenied => "you may not read it",
        Errno::NotFound => "it does not exist",
        Errno::Busy => "another program is writing it, or it is being mounted already",
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
        attach_refusal, eject_refusal, holds_within, private, shown_at, MountAct, MountDone,
        MountRefusal, Mounted, Projection, CROWDED, MOUNTED_WRITABLE, UNSERVED,
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
    use tairix_abi::sysinfo::ViewBindingRecord;
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
                reason: "another program is writing it, or it is being mounted already",
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

    /// A rail holding one image volume `uid` attached, at `/Storage/disk`.
    fn rail(uid: u32) -> Places {
        Places::new(
            &components(&["Users", "ada"]),
            &[Volume {
                label: String::from("disk"),
                target: String::from("/Storage/disk"),
                medium: None,
                facts: VolumeFacts {
                    filesystem: String::from("affs"),
                    free: None,
                    availability: tairix_abi::sysinfo::MountAvailability::Available,
                    owner: Some(uid),
                    volume_id: [5; 16],
                },
                offered: true,
            }],
        )
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
