//! Which attached volume a command line's name for one means: the one
//! resolution the `unmount` and `label` tools share.
//!
//! A volume's durable root (`id::<volume-id>`) names the volume published
//! under that key, whatever it is mounted as. A path — a name holding a slash,
//! or `.` or `..` — names the binding at the place it spells, matched as
//! spelled (`.` dropped, a relative one joined to the working directory, as
//! util-linux's `umount` reads a mount point) and canonicalised only when that
//! names none, so a volume that no longer answers is still named by where it
//! is mounted. A bare word names the catalog entry it spells or, failing
//! that, the file a volume was attached from. A binding is unique but a file
//! name is not: two users' images may share one, so a volume the caller
//! attached is taken over another's, and a name two of either could mean, or
//! one longer than the listing keeps, names nothing. Another user's private
//! volume shows neither its source nor its bindings but its catalog entry, so
//! a miss with one listed may be that volume.

use alloc::format;
use alloc::string::String;
use core::fmt;

use tairix_abi::sysinfo::{MountRecord, ViewBindingRecord, VOLUME_ID_LEN};
use tairix_abi::{Errno, STORAGE_CATALOG};
use tairix_path::{Root, Step};

use crate::mount::Namespace;

/// What a name is read with beside the namespace: who reads it, and how a
/// path it spells resolves.
pub trait Caller {
    /// The user the command runs as, as the kernel attests them, or `None`
    /// when it will not say.
    fn user(&self) -> Option<u32>;

    /// The directory `path` names, absolute and canonical, as util-linux
    /// canonicalises a mount point.
    ///
    /// # Errors
    ///
    /// The refusal to resolve it, [`Errno::NotFound`] for a directory that
    /// does not exist.
    fn canonical(&self, path: &str) -> Result<String, Errno>;
}

/// Why a name names no volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unnamed {
    /// No mounted filesystem matches it.
    NotFound,
    /// None shown to the caller matches it, though another user's private
    /// volume, whose source and bindings are withheld, might.
    NotShown,
    /// The path it spells could not be resolved, for this reason.
    Path(Errno),
    /// More than one volume was attached from a file of that name.
    Ambiguous,
    /// The listing keeps only the start of a file name that long, so no
    /// volume is known to be the one it names.
    NameCut,
    /// It names a binding that shows no volume: the catalog itself.
    NoVolume,
}

/// Why a name names no volume, as a diagnostic says it after the name.
impl fmt::Display for Unnamed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not mounted"),
            Self::NotShown => f.write_str(
                "no volume shown to you; another user's private volume is named by where it is \
                 mounted in /Storage",
            ),
            Self::Path(errno) => write!(f, "{errno}"),
            Self::Ambiguous => f.write_str(
                "more than one volume was mounted from a file of that name; name it by where it \
                 is mounted",
            ),
            Self::NameCut => f.write_str(
                "the mount listing keeps only the start of that file name; name the volume by \
                 where it is mounted",
            ),
            Self::NoVolume => f.write_str("no volume is mounted there"),
        }
    }
}

/// The volume `name` names in `namespace`, read as `caller` reads it.
///
/// # Errors
///
/// [`Unnamed`]: why the name names no volume.
pub fn named_volume<'n, C: Caller + ?Sized>(
    namespace: &'n Namespace,
    name: &str,
    caller: &C,
) -> Result<&'n MountRecord, Unnamed> {
    let unseen = || {
        if namespace.volumes().iter().any(MountRecord::is_withheld) {
            Unnamed::NotShown
        } else {
            Unnamed::NotFound
        }
    };
    if let Some(id) = volume_root(name) {
        return namespace
            .volumes()
            .iter()
            .find(|volume| volume.volume_id() == id)
            .ok_or_else(unseen);
    }
    let bound_at = |at: &str| -> Option<&ViewBindingRecord> {
        namespace
            .views()
            .iter()
            .find(|view| view.target_bytes() == at.as_bytes())
    };
    let path = name.contains('/') || matches!(name, "." | "..");
    let view = if path {
        match spelled(name, caller).as_deref().and_then(bound_at) {
            Some(view) => Some(view),
            None => bound_at(&caller.canonical(name).map_err(|errno| match errno {
                Errno::NotFound => Unnamed::NotFound,
                errno => Unnamed::Path(errno),
            })?),
        }
    } else {
        bound_at(&format!("/{STORAGE_CATALOG}/{name}"))
    };
    if let Some(view) = view {
        if !view.shows_volume() {
            return Err(Unnamed::NoVolume);
        }
        return namespace.volume_of(view).ok_or(Unnamed::NotFound);
    }
    if path {
        return Err(unseen());
    }
    by_file_name(name, caller.user(), namespace.volumes()).map_err(|why| match why {
        Unnamed::NotFound => unseen(),
        why => why,
    })
}

/// The identity `name` spells as a volume's durable root, `id::<volume-id>`
/// with nothing below it, or `None` for any other name.
fn volume_root(name: &str) -> Option<[u8; VOLUME_ID_LEN]> {
    let path = tairix_path::parse(name).ok()?;
    match path.root() {
        Root::VolumeId(id) if path.components().is_empty() => Some(*id.as_bytes()),
        _ => None,
    }
}

/// The view path `name` spells, nothing resolved: a `.` dropped, a relative
/// one joined to the working directory, as util-linux's `umount` reads a
/// mount point. `None` where only resolving can say: a `..`, or a spelling
/// the grammar refuses. A bind pins every directory down to its target, so a
/// target is always a link-free spelling of what it binds.
fn spelled<C: Caller + ?Sized>(name: &str, caller: &C) -> Option<String> {
    let (root, steps) = tairix_path::steps(name).ok()?;
    let mut at = match root {
        Root::View => String::from("/"),
        Root::Relative => caller.canonical(".").ok()?,
        Root::Alias(_) | Root::VolumeId(_) => return None,
    };
    for step in steps {
        match step.ok()? {
            Step::Name(part) => tairix_path::push_name(&mut at, part),
            Step::Up => return None,
        }
    }
    Some(at)
}

/// The volume attached from the file `name`, in one pass over `volumes`.
fn by_file_name<'v>(
    name: &str,
    user: Option<u32>,
    volumes: &'v [MountRecord],
) -> Result<&'v MountRecord, Unnamed> {
    let (mut taken, mut takers) = (None, 0usize);
    let (mut anothers, mut others) = (None, 0usize);
    let mut cut = false;
    for volume in volumes {
        let source = volume.source_bytes();
        if volume.source_cut() {
            cut |= name.as_bytes().starts_with(source);
            continue;
        }
        if source != name.as_bytes() {
            continue;
        }
        if volume.owner().is_some_and(|owner| Some(owner) != user) {
            anothers.get_or_insert(volume);
            others += 1;
        } else {
            taken.get_or_insert(volume);
            takers += 1;
        }
    }
    match (taken, takers, anothers, others) {
        (Some(volume), 1, ..) | (None, 0, Some(volume), 1) => Ok(volume),
        (Some(_), ..) | (None, 0, Some(_), _) => Err(Unnamed::Ambiguous),
        _ if cut => Err(Unnamed::NameCut),
        _ => Err(Unnamed::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec::Vec;

    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::sysinfo::{
        MountAvailability, MountOwner, MountRecord, MountVolumeState, ViewBindingRecord,
        MOUNT_SOURCE_MAX,
    };
    use tairix_abi::Errno;

    use super::{named_volume, Caller, Unnamed};
    use crate::mount::Namespace;

    /// The user these tests resolve names as.
    const VIEWER: u32 = 1000;

    /// The viewer, working in `/Users/ada`, where `missing` does not exist and
    /// `forbidden` may not be entered.
    struct Ada;

    impl Caller for Ada {
        fn user(&self) -> Option<u32> {
            Some(VIEWER)
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            let at = if path == "." {
                String::from("/Users/ada")
            } else if path.starts_with('/') {
                String::from(path)
            } else {
                alloc::format!("/Users/ada/{}", path.trim_start_matches("./"))
            };
            match at.rsplit('/').next() {
                Some("missing") => Err(Errno::NotFound),
                Some("forbidden") => Err(Errno::PermissionDenied),
                _ => Ok(at),
            }
        }
    }

    /// A volume `source` names, shown at `target`, published as `id`.
    fn volume(source: &[u8], target: &[u8], id: u8) -> MountRecord {
        MountRecord::new(
            source,
            target,
            b"fat32",
            MountFlags::NOEXEC,
            MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::Available,
                medium: None,
            },
            [id; 16],
        )
        .expect("a record")
    }

    /// A disk image `owner` attached from `file`, shown at `target`.
    fn image(file: &[u8], target: &[u8], owner: u32, id: u8) -> MountRecord {
        volume(file, target, id).with_owner(MountOwner {
            uid: owner,
            private: false,
        })
    }

    /// The namespace of `volumes`, each shown from its root at its target,
    /// beside the catalog, which shows none.
    fn namespace(volumes: Vec<MountRecord>) -> Namespace {
        let mut views: Vec<ViewBindingRecord> = volumes
            .iter()
            .map(|volume| {
                ViewBindingRecord::new(
                    volume.target_bytes(),
                    b"/",
                    MountFlags::NOEXEC,
                    volume.volume_id(),
                )
                .expect("a binding")
            })
            .collect();
        views.push(
            ViewBindingRecord::new(b"/Storage", b"", MountFlags::NOEXEC, [0; 16])
                .expect("the catalog"),
        );
        Namespace::new(volumes, views)
    }

    /// The key of the volume `name` names in `namespace`, or why none.
    fn named(namespace: &Namespace, name: &str) -> Result<[u8; 16], Unnamed> {
        named_volume(namespace, name, &Ada).map(MountRecord::volume_id)
    }

    /// A catalog word, a path spelled or resolved, and a durable root each
    /// name the volume shown there or published so; the catalog itself shows
    /// none, and a path that will not resolve says why.
    #[test]
    fn a_volume_is_named_by_its_entry_its_path_or_its_key() {
        let ns = namespace(alloc::vec![
            volume(b"usb1", b"/Storage/usb1", 1),
            image(b"disk.adf", b"/Users/ada/disk", VIEWER, 2),
        ]);
        assert_eq!(named(&ns, "usb1"), Ok([1; 16]));
        assert_eq!(named(&ns, "/Storage/usb1"), Ok([1; 16]));
        assert_eq!(
            named(&ns, "./disk"),
            Ok([2; 16]),
            "a relative path from the working directory"
        );
        let root = alloc::format!("id::{}", tairix_path::VolumeId::from_bytes([2u8; 16]));
        assert_eq!(named(&ns, &root), Ok([2; 16]));
        assert_eq!(
            named(&ns, "/Storage"),
            Err(Unnamed::NoVolume),
            "the catalog shows none"
        );
        assert_eq!(named(&ns, "/Storage/missing"), Err(Unnamed::NotFound));
        assert_eq!(
            named(&ns, "/Storage/forbidden"),
            Err(Unnamed::Path(Errno::PermissionDenied))
        );
    }

    /// A file name names the volume attached from it, the caller's own over
    /// another's; one more than one could mean, or one the listing keeps only
    /// the start of, names none, and a miss beside a withheld volume may be
    /// that volume.
    #[test]
    fn a_file_name_names_the_callers_own_volume_first() {
        let long = "x".repeat(MOUNT_SOURCE_MAX + 4);
        let ns = namespace(alloc::vec![
            image(b"wb.adf", b"/Storage/wb", VIEWER + 1, 3),
            image(b"wb.adf", b"/Users/ada/wb", VIEWER, 4),
            image(b"twin.adf", b"/Storage/twin", VIEWER + 1, 5),
            image(b"twin.adf", b"/Storage/twin-2", VIEWER + 2, 6),
            image(long.as_bytes(), b"/Storage/long", VIEWER, 7),
        ]);
        assert_eq!(named(&ns, "wb.adf"), Ok([4; 16]), "the caller's own");
        assert_eq!(named(&ns, "twin.adf"), Err(Unnamed::Ambiguous));
        assert_eq!(named(&ns, &long), Err(Unnamed::NameCut));
        assert_eq!(named(&ns, "gone.adf"), Err(Unnamed::NotFound));
        let other = image(b"x.adf", b"/Storage/x", VIEWER + 1, 8).withheld();
        let withheld = namespace(alloc::vec![other]);
        assert_eq!(named(&withheld, "gone.adf"), Err(Unnamed::NotShown));
    }

    /// Every reason reads as the clause a diagnostic puts after the name.
    #[test]
    fn a_reason_reads_as_a_clause_after_the_name() {
        assert_eq!(alloc::format!("{}", Unnamed::NotFound), "not mounted");
        assert_eq!(
            alloc::format!("{}", Unnamed::Path(Errno::PermissionDenied)),
            "permission denied"
        );
        assert!(alloc::format!("{}", Unnamed::NotShown).ends_with("mounted in /Storage"));
    }
}
