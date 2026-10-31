//! The `unmount` engine: resolve the named volume through the System
//! Information API's mount and view listings, then hand its identity to the
//! kernel's detach path.

use alloc::format;
use alloc::string::String;

use tairix_abi::stdinfo::{Human, Severity, StdInfoKind, StdInfoRecord, Suggestion};
use tairix_abi::sysinfo::{MountAvailability, MountRecord, VOLUME_ID_LEN};
use tairix_abi::{Errno, STORAGE_CATALOG};
use tairix_help::{own_short_help, HelpSource};
use tairix_procinfo::{Namespace, Transport};

use crate::command::Command;
use crate::error::UnmountError;
use crate::io::{Detacher, Output};

/// The one-line usage banner, the short help's fallback when the bundled
/// help document is unavailable.
pub const USAGE: &str = "usage: unmount [-f | --force] [--] NAME";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "unmount";

/// The facts the resolver needs about the matched mount: the volume's
/// stable identity and whether the listing already marks it unavailable.
struct Resolved {
    volume_id: [u8; VOLUME_ID_LEN],
    unavailable: bool,
}

impl Resolved {
    fn of(volume: &MountRecord) -> Self {
        Self {
            volume_id: volume.volume_id(),
            unavailable: volume.availability() != MountAvailability::Available,
        }
    }
}

/// Run a parsed `unmount` command against the injected seams.
///
/// A successful detach writes nothing, matching the established `umount`
/// behaviour; the kernel logs the audited decision.
///
/// # Errors
///
/// * [`UnmountError::NotFound`] — no mounted filesystem matches `NAME`.
/// * [`UnmountError::NotShown`] — none shown to the caller matches `NAME`,
///   though another user's withheld private volume might.
/// * [`UnmountError::Path`] — the path `NAME` spells could not be resolved.
/// * [`UnmountError::Ambiguous`] — `NAME` is a file name more than one
///   volume was attached from.
/// * [`UnmountError::NameCut`] — `NAME` is a file name longer than the
///   listing keeps, so it identifies no volume.
/// * [`UnmountError::NotDetachable`] — the matched mount carries no
///   detachable volume identity (a permanent boot volume or view
///   binding).
/// * [`UnmountError::Detach`] — the kernel refused or failed the detach;
///   `unavailable` distinguishes the surprise-removed case whose plain
///   detach is refused by design (the `--force` suggestion is also
///   emitted on fd 3, additive and ignorable).
/// * [`UnmountError::Service`] — the mount or view listing failed.
/// * [`UnmountError::Output`] — the short help could not be written.
pub fn run(
    command: Command,
    locale: Option<&str>,
    transport: &dyn Transport,
    detacher: &dyn Detacher,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<(), UnmountError> {
    let (name, force) = match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            out.write_all(&bytes).map_err(UnmountError::Output)?;
            return Ok(());
        }
        Command::Unmount { name, force } => (name, force),
    };

    let resolved = resolve(&name, detacher, transport)?;
    if resolved.volume_id == [0u8; VOLUME_ID_LEN] {
        return Err(UnmountError::NotDetachable(name));
    }
    match detacher.detach(resolved.volume_id, force) {
        Ok(()) => Ok(()),
        Err(errno) => {
            if resolved.unavailable && !force {
                emit_force_suggestion(err, &name);
            }
            Err(UnmountError::Detach {
                name,
                errno,
                unavailable: resolved.unavailable,
            })
        }
    }
}

/// Resolve `name` against the namespace: a path — any name holding a slash,
/// canonicalised first — names the binding at it, a catalog entry or a
/// directory a volume was bound at, and a bare word the catalog entry it
/// spells or, failing that, the file a volume was attached from. A binding is
/// unique, but a file name is not: two users' images may share one, so a
/// volume the caller attached is taken over another's, and a name two of
/// either could mean, or one longer than the listing keeps, names nothing.
/// Another user's private volume shows neither its source nor its bindings
/// but its catalog entry, so a miss with one listed may be that volume.
fn resolve(
    name: &str,
    detacher: &dyn Detacher,
    transport: &dyn Transport,
) -> Result<Resolved, UnmountError> {
    let namespace = Namespace::read(transport)?;
    let path = name.contains('/');
    let at = if path {
        detacher.canonical(name).map_err(|errno| match errno {
            Errno::NotFound => UnmountError::NotFound(String::from(name)),
            errno => UnmountError::Path {
                name: String::from(name),
                errno,
            },
        })?
    } else {
        format!("/{STORAGE_CATALOG}/{name}")
    };
    if let Some(view) = namespace
        .views()
        .iter()
        .find(|view| view.target_bytes() == at.as_bytes())
    {
        if !view.shows_volume() {
            return Err(UnmountError::NotDetachable(String::from(name)));
        }
        return namespace
            .volume_of(view)
            .map(Resolved::of)
            .ok_or_else(|| UnmountError::NotFound(String::from(name)));
    }
    let unseen = || {
        if namespace.volumes().iter().any(MountRecord::is_withheld) {
            UnmountError::NotShown(String::from(name))
        } else {
            UnmountError::NotFound(String::from(name))
        }
    };
    if path {
        return Err(unseen());
    }
    by_file_name(name, detacher.user(), namespace.volumes()).map_err(|err| match err {
        UnmountError::NotFound(_) => unseen(),
        err => err,
    })
}

/// The volume attached from the file `name`, in one pass over `volumes`.
fn by_file_name(
    name: &str,
    user: Option<u32>,
    volumes: &[MountRecord],
) -> Result<Resolved, UnmountError> {
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
    let owned = || String::from(name);
    match (taken, takers, anothers, others) {
        (Some(volume), 1, ..) | (None, 0, Some(volume), 1) => Ok(Resolved::of(volume)),
        (Some(_), ..) | (None, 0, Some(_), _) => Err(UnmountError::Ambiguous(owned())),
        _ if cut => Err(UnmountError::NameCut(owned())),
        _ => Err(UnmountError::NotFound(owned())),
    }
}

/// Emit the fd-3 `suggestion` record for a refused plain detach of an
/// unavailable volume: the retained data is held deliberately, and the
/// audited force-unmount is the explicit way to give it up. Best-effort
/// and additive — it never changes the diagnostic, exit status, or
/// pipeline behaviour.
fn emit_force_suggestion(err: &dyn Output, name: &str) {
    let message = String::from("The volume holds retained uncommitted data.");
    let suggestion = format!("Use `unmount --force {name}` to discard it.");
    let ai = format!(
        "{{\"subject\":\"volume_detach\",\
         \"refusal\":{{\"reason\":\"volume_unavailable\",\
         \"retained_data_would_be_discarded\":true}},\
         \"suggestion\":{}}}",
        Suggestion::new(&["unmount", "--force", name])
    );
    let record = StdInfoRecord::new(
        OWN_WORD,
        StdInfoKind::Suggestion,
        "fs.volume_unavailable_force_required",
        Severity::Info,
        Human::with_suggestion(&message, &suggestion),
    )
    .with_ai(&ai);
    let mut buf = [0u8; 512];
    if let Ok(len) = record.write_jsonl(&mut buf) {
        err.info(&buf[..len]);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{run, USAGE};
    use crate::command::parse;
    use crate::error::UnmountError;
    use crate::io::{Detacher, Output};
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::sysinfo::{
        MountAvailability, MountOwner, MountRecord, MountVolumeState, PageRequest, SysinfoQueryId,
        SysinfoRequestHeader, ViewBindingRecord, VolumeClass, MOUNT_SOURCE_MAX,
    };
    use tairix_abi::Errno;
    use tairix_help::{HelpSource, SourceError};
    use tairix_procinfo::Transport;

    /// An in-memory `sysinfod` stand-in answering the mount and view lists
    /// from a fixture, decoding the request the same way the real service
    /// does.
    struct Fixture {
        records: Vec<MountRecord>,
        views: Vec<ViewBindingRecord>,
        fail: Option<Errno>,
    }

    impl Fixture {
        /// `records`, each shown from its root at its own target, as the
        /// kernel lists them.
        fn of(records: Vec<MountRecord>) -> Self {
            let views = records
                .iter()
                .filter(|record| !record.target_bytes().is_empty())
                .map(|record| {
                    ViewBindingRecord::new(
                        record.target_bytes(),
                        b"/",
                        MountFlags::NOEXEC,
                        record.volume_id(),
                    )
                    .expect("view")
                })
                .collect();
            Self {
                records,
                views,
                fail: None,
            }
        }

        /// This fixture with the volume `volume_id` bound at `target` too.
        fn bound_at(mut self, target: &[u8], volume_id: [u8; 16]) -> Self {
            self.views.push(
                ViewBindingRecord::new(target, b"/", MountFlags::NOEXEC, volume_id).expect("view"),
            );
            self
        }
    }

    impl Transport for Fixture {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            let header = SysinfoRequestHeader::from_bytes(request)?;
            if let Some(errno) = self.fail {
                return Err(errno);
            }
            let payload = &request[SysinfoRequestHeader::WIRE_LEN
                ..SysinfoRequestHeader::WIRE_LEN + header.payload_len as usize];
            let req = PageRequest::from_bytes(payload)?;
            let encoded: Vec<Vec<u8>> = match header.query {
                SysinfoQueryId::MOUNT_LIST => self
                    .records
                    .iter()
                    .map(|record| record.to_le_bytes().to_vec())
                    .collect(),
                SysinfoQueryId::VIEW_LIST => self
                    .views
                    .iter()
                    .map(|view| view.to_le_bytes().to_vec())
                    .collect(),
                _ => return Err(Errno::NotImplemented),
            };
            Ok(encoded
                .iter()
                .skip(req.offset as usize)
                .take(req.limit as usize)
                .flatten()
                .copied()
                .collect())
        }
    }

    /// One recorded detach request.
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Detached {
        volume_id: [u8; 16],
        force: bool,
    }

    /// Records every detach request, optionally failing it.
    struct MemDetacher {
        fail: Option<Errno>,
        detached: RefCell<Vec<Detached>>,
    }

    impl MemDetacher {
        fn new() -> Self {
            Self {
                fail: None,
                detached: RefCell::new(Vec::new()),
            }
        }
        fn failing(errno: Errno) -> Self {
            Self {
                fail: Some(errno),
                detached: RefCell::new(Vec::new()),
            }
        }
    }

    impl Detacher for MemDetacher {
        fn user(&self) -> Option<u32> {
            Some(VIEWER)
        }

        fn detach(&self, volume_id: [u8; 16], force: bool) -> Result<(), Errno> {
            self.detached
                .borrow_mut()
                .push(Detached { volume_id, force });
            match self.fail {
                Some(errno) => Err(errno),
                None => Ok(()),
            }
        }

        /// As `realpath` resolves in `/Users/ada`, where nothing named
        /// `missing` exists and `forbidden` may not be entered.
        fn canonical(&self, path: &str) -> Result<String, Errno> {
            let joined = if path.starts_with('/') {
                String::from(path)
            } else {
                alloc::format!("/Users/ada/{path}")
            };
            let parts: Vec<&str> = joined
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .collect();
            match parts.last() {
                Some(&"missing") => Err(Errno::NotFound),
                Some(&"forbidden") => Err(Errno::PermissionDenied),
                _ => Ok(alloc::format!("/{}", parts.join("/"))),
            }
        }
    }

    /// Captures written bytes and fd-3 records.
    struct Recorder {
        written: RefCell<Vec<u8>>,
        infos: RefCell<Vec<String>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                written: RefCell::new(Vec::new()),
                infos: RefCell::new(Vec::new()),
            }
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.written.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
        fn info(&self, record: &[u8]) {
            self.infos
                .borrow_mut()
                .push(String::from_utf8_lossy(record).into_owned());
        }
    }

    /// A help tree with no documents, so the usage banner is the fallback.
    struct NoHelp;

    impl HelpSource for NoHelp {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }

        fn read(
            &self,
            _locale_dir: &str,
            _file_name: &str,
        ) -> Result<Option<Vec<u8>>, SourceError> {
            Ok(None)
        }
    }

    fn record(
        source: &[u8],
        target: &[u8],
        availability: MountAvailability,
        volume_id: [u8; 16],
    ) -> MountRecord {
        MountRecord::new(
            source,
            target,
            b"fat32",
            MountFlags::NOEXEC,
            MountVolumeState {
                usage: VolumeStats::default(),
                availability,
                medium: None,
            },
            volume_id,
        )
        .expect("record")
    }

    /// The user running `unmount` in these tests.
    const VIEWER: u32 = 1000;

    fn table() -> Fixture {
        Fixture::of(alloc::vec![
            record(b"ARXFSRoot", b"/", MountAvailability::Available, [0u8; 16]),
            record(
                b"usb1",
                b"/Storage/usb1",
                MountAvailability::Available,
                [1u8; 16],
            ),
            record(
                b"usb2",
                b"/Storage/usb2",
                MountAvailability::UnavailableDirty,
                [2u8; 16],
            ),
        ])
    }

    /// A disk image `owner` attached from `file`, at `target`.
    /// An image `owner` mounted from `file`, as `sysinfod` lists it to the
    /// viewer: the viewer's own private, another's shared with everyone.
    fn image(file: &[u8], target: &[u8], owner: u32, id: u8) -> MountRecord {
        record(file, target, MountAvailability::Available, [id; 16])
            .with_owner(MountOwner {
                uid: owner,
                private: owner == VIEWER,
            })
            .with_class(VolumeClass::Image)
    }

    /// Another user's private image, as `sysinfod` lists it to the viewer.
    fn withheld_image(file: &[u8], target: &[u8], id: u8) -> MountRecord {
        record(file, target, MountAvailability::Available, [id; 16])
            .with_owner(MountOwner {
                uid: VIEWER + 1,
                private: true,
            })
            .with_class(VolumeClass::Image)
            .withheld()
    }

    fn run_args(
        args: &[&str],
        fixture: &Fixture,
        detacher: &MemDetacher,
        err: &Recorder,
    ) -> Result<(), UnmountError> {
        let out = Recorder::new();
        run(
            parse(args).expect("valid command"),
            None,
            fixture,
            detacher,
            &NoHelp,
            &out,
            err,
        )
    }

    #[test]
    fn help_prints_the_usage_fallback_and_detaches_nothing() {
        let detacher = MemDetacher::new();
        let out = Recorder::new();
        let err = Recorder::new();
        run(
            parse(&["--help"]).expect("valid"),
            None,
            &table(),
            &detacher,
            &NoHelp,
            &out,
            &err,
        )
        .expect("help succeeds");
        assert!(String::from_utf8_lossy(&out.written.borrow()).contains(USAGE));
        assert!(detacher.detached.borrow().is_empty());
    }

    #[test]
    fn a_catalog_name_resolves_to_its_volume_identity() {
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        run_args(&["usb1"], &table(), &detacher, &err).expect("detaches");
        assert_eq!(
            detacher.detached.borrow().as_slice(),
            &[Detached {
                volume_id: [1u8; 16],
                force: false,
            }]
        );
    }

    /// Two users' disk images may share a file name, and the listing shows
    /// both: `unmount` by that name detaches the caller's own, never the one
    /// listed first, and a name only another user's image bears is still
    /// tried, for the kernel to refuse.
    #[test]
    fn a_shared_source_name_resolves_to_the_caller_s_own_volume() {
        let fixture = Fixture::of(alloc::vec![
            image(b"disk.adf", b"/Storage/disk", VIEWER + 1, 3),
            image(b"disk.adf", b"/Storage/disk-2", VIEWER, 4),
        ]);
        let detacher = MemDetacher::new();
        run_args(&["disk.adf"], &fixture, &detacher, &Recorder::new()).expect("detaches");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [4u8; 16]);
        let theirs = Fixture::of(alloc::vec![image(
            b"disk.adf",
            b"/Storage/disk",
            VIEWER + 1,
            3
        )]);
        let detacher = MemDetacher::new();
        run_args(&["disk.adf"], &theirs, &detacher, &Recorder::new()).expect("asked");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [3u8; 16]);
    }

    /// `mount IMAGE DIR` binds the volume at `DIR` too, and `DIR` names it as
    /// the catalog entry does.
    #[test]
    fn the_directory_an_image_was_mounted_on_names_it() {
        let fixture = Fixture::of(alloc::vec![image(
            b"wb.adf",
            b"/Storage/workbench",
            VIEWER,
            5
        )])
        .bound_at(b"/Users/ada/Home/wb", [5; 16]);
        let detacher = MemDetacher::new();
        run_args(
            &["/Users/ada/Home/wb"],
            &fixture,
            &detacher,
            &Recorder::new(),
        )
        .expect("detaches");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [5u8; 16]);
        let detacher = MemDetacher::new();
        assert_eq!(
            run_args(&["/Users/ada/Home"], &fixture, &detacher, &Recorder::new()),
            Err(UnmountError::NotFound(String::from("/Users/ada/Home"))),
            "only the directory it is bound at"
        );
        assert!(detacher.detached.borrow().is_empty());
    }

    /// A path is canonicalised as `mount` resolved it: relative, with a `.`
    /// or a trailing slash, it names the directory it spells.
    #[test]
    fn a_directory_is_named_however_it_is_spelled() {
        let fixture = Fixture::of(alloc::vec![image(
            b"wb.adf",
            b"/Storage/workbench",
            VIEWER,
            5
        )])
        .bound_at(b"/Users/ada/Home/wb", [5; 16]);
        for spelled in ["Home/wb", "./Home/wb", "/Users/ada/Home/wb/"] {
            let detacher = MemDetacher::new();
            run_args(&[spelled], &fixture, &detacher, &Recorder::new()).expect(spelled);
            assert_eq!(
                detacher.detached.borrow()[0].volume_id,
                [5u8; 16],
                "{spelled}"
            );
        }
        let detacher = MemDetacher::new();
        assert_eq!(
            run_args(&["Home/missing"], &fixture, &detacher, &Recorder::new()),
            Err(UnmountError::NotFound(String::from("Home/missing")))
        );
        let refused = run_args(&["Home/forbidden"], &fixture, &detacher, &Recorder::new());
        assert_eq!(
            refused,
            Err(UnmountError::Path {
                name: String::from("Home/forbidden"),
                errno: Errno::PermissionDenied,
            })
        );
        assert_eq!(refused.expect_err("refused").status(), 32);
        assert!(detacher.detached.borrow().is_empty());
    }

    /// Another user's private volume lists neither its source nor its
    /// bindings, so a miss beside one says it may be that volume, and its
    /// catalog entry still names it.
    #[test]
    fn a_miss_beside_another_users_private_volume_says_what_names_it() {
        let fixture = Fixture::of(alloc::vec![withheld_image(b"", b"/Storage/letters", 6)]);
        for name in ["letters.adf", "/Users/them/Home/letters"] {
            let detacher = MemDetacher::new();
            let missed = run_args(&[name], &fixture, &detacher, &Recorder::new());
            assert_eq!(missed, Err(UnmountError::NotShown(String::from(name))));
            let said = alloc::format!("{}", missed.expect_err("missed"));
            assert!(
                said.contains("by where it is mounted in /Storage"),
                "{said}"
            );
        }
        let detacher = MemDetacher::new();
        run_args(&["letters"], &fixture, &detacher, &Recorder::new()).expect("asked");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [6u8; 16]);
    }

    /// A file name two volumes were mounted from names neither, whoever's
    /// they are: the caller is told to name the volume by where it is.
    #[test]
    fn a_file_name_two_volumes_share_names_neither() {
        for owners in [[VIEWER, VIEWER], [VIEWER + 1, VIEWER + 2]] {
            let fixture = Fixture::of(alloc::vec![
                image(b"disk.adf", b"/Storage/disk", owners[0], 3),
                image(b"disk.adf", b"/Storage/disk-2", owners[1], 4),
            ]);
            let detacher = MemDetacher::new();
            assert_eq!(
                run_args(&["disk.adf"], &fixture, &detacher, &Recorder::new()),
                Err(UnmountError::Ambiguous(String::from("disk.adf"))),
                "{owners:?}"
            );
            assert!(detacher.detached.borrow().is_empty());
        }
    }

    /// A file name the listing keeps only the start of identifies no volume:
    /// another image's name may start the same.
    #[test]
    fn a_file_name_the_listing_cut_names_no_volume() {
        let long = "Amiga Workbench 1.3 Install and Extras Collection (Commodore 1988) Disk 1.adf";
        let listed = image(long.as_bytes(), b"/Storage/wb13", VIEWER, 6);
        assert!(listed.source_cut());
        let fixture = Fixture::of(alloc::vec![listed]);
        let other_disk = long.replace("Disk 1", "Disk 2");
        for name in [long, &other_disk, &long[..MOUNT_SOURCE_MAX]] {
            let detacher = MemDetacher::new();
            assert_eq!(
                run_args(&[name], &fixture, &detacher, &Recorder::new()),
                Err(UnmountError::NameCut(String::from(name))),
                "{name}"
            );
            assert!(detacher.detached.borrow().is_empty());
        }
        let detacher = MemDetacher::new();
        run_args(&["wb13"], &fixture, &detacher, &Recorder::new()).expect("by its catalog name");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [6u8; 16]);
    }

    #[test]
    fn a_mount_point_path_resolves_too() {
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        run_args(&["/Storage/usb1"], &table(), &detacher, &err).expect("detaches");
        assert_eq!(detacher.detached.borrow()[0].volume_id, [1u8; 16]);
    }

    #[test]
    fn force_reaches_the_kernel() {
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        run_args(&["-f", "usb2"], &table(), &detacher, &err).expect("force detaches");
        assert_eq!(
            detacher.detached.borrow().as_slice(),
            &[Detached {
                volume_id: [2u8; 16],
                force: true,
            }]
        );
        // A force request carries no refusal, so no advisory is emitted.
        assert!(err.infos.borrow().is_empty());
    }

    #[test]
    fn an_unknown_name_is_not_found_and_detaches_nothing() {
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        assert_eq!(
            run_args(&["usb9"], &table(), &detacher, &err),
            Err(UnmountError::NotFound(String::from("usb9")))
        );
        assert!(detacher.detached.borrow().is_empty());
    }

    #[test]
    fn a_volume_without_identity_is_not_detachable() {
        // The boot root has no published detachable identity: refusing
        // locally beats sending the kernel a nil identity it must refuse.
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        assert_eq!(
            run_args(&["/"], &table(), &detacher, &err),
            Err(UnmountError::NotDetachable(String::from("/")))
        );
        assert!(detacher.detached.borrow().is_empty());
    }

    #[test]
    fn a_refused_plain_detach_of_an_unavailable_volume_suggests_force() {
        let detacher = MemDetacher::failing(Errno::DeviceFault);
        let err = Recorder::new();
        assert_eq!(
            run_args(&["usb2"], &table(), &detacher, &err),
            Err(UnmountError::Detach {
                name: String::from("usb2"),
                errno: Errno::DeviceFault,
                unavailable: true,
            })
        );
        let infos = err.infos.borrow();
        assert_eq!(infos.len(), 1);
        assert!(infos[0].contains("\"suggestion\""));
        assert!(infos[0].contains("--force"));
        assert!(infos[0].contains("usb2"));
    }

    /// A volume's name is the user's, so it reaches the suggested command line
    /// escaped: a quote in it must not end the JSON string it is written in.
    #[test]
    fn a_volume_name_json_would_misread_is_escaped_in_the_suggestion() {
        let fixture = Fixture::of(alloc::vec![record(
            b"odd\"name",
            b"/Storage/odd\"name",
            MountAvailability::UnavailableDirty,
            [3u8; 16],
        )]);
        let detacher = MemDetacher::failing(Errno::DeviceFault);
        let err = Recorder::new();
        assert!(run_args(&["odd\"name"], &fixture, &detacher, &err).is_err());
        let infos = err.infos.borrow();
        assert_eq!(infos.len(), 1);
        assert!(
            infos[0].contains("\"argv\":[\"unmount\",\"--force\",\"odd\\\"name\"]"),
            "{}",
            infos[0]
        );
    }

    #[test]
    fn a_refused_detach_of_a_healthy_volume_emits_no_advisory() {
        let detacher = MemDetacher::failing(Errno::PermissionDenied);
        let err = Recorder::new();
        assert_eq!(
            run_args(&["usb1"], &table(), &detacher, &err),
            Err(UnmountError::Detach {
                name: String::from("usb1"),
                errno: Errno::PermissionDenied,
                unavailable: false,
            })
        );
        assert!(err.infos.borrow().is_empty());
    }

    #[test]
    fn a_failed_unmount_ends_as_util_linux_ends_one() {
        let detach = |errno| UnmountError::Detach {
            name: String::from("workbench"),
            errno,
            unavailable: false,
        };
        assert_eq!(detach(Errno::Busy).status(), 32, "a failed unmount");
        assert_eq!(UnmountError::NotFound(String::from("usb9")).status(), 32);
        assert_eq!(UnmountError::NotDetachable(String::from("/")).status(), 32);
        assert_eq!(UnmountError::Ambiguous(String::from("d.adf")).status(), 32);
        assert_eq!(UnmountError::NameCut(String::from("d.adf")).status(), 32);
        assert_eq!(
            detach(Errno::PermissionDenied).status(),
            1,
            "a detach the caller may not make"
        );
        assert_eq!(UnmountError::Usage.status(), 1);
        assert_eq!(UnmountError::Service(Errno::TimedOut).status(), 2);
        assert_eq!(UnmountError::Output(Errno::BrokenPipe).status(), 2);
    }

    #[test]
    fn a_failure_names_the_volume_and_ends_its_sentence() {
        use alloc::string::ToString;
        let refused = |name: &str, errno, unavailable| {
            UnmountError::Detach {
                name: String::from(name),
                errno,
                unavailable,
            }
            .to_string()
        };
        assert_eq!(
            refused("workbench", Errno::Busy, false),
            alloc::format!("workbench: {}.", Errno::Busy)
        );
        assert!(refused("usb2", Errno::DeviceFault, true).starts_with("usb2: volume holds"));
        assert!(refused("usb2", Errno::DeviceFault, true).ends_with('.'));
        assert_eq!(
            UnmountError::NotFound(String::from("usb9")).to_string(),
            "usb9: not mounted."
        );
        assert_eq!(
            UnmountError::NotDetachable(String::from("/")).to_string(),
            "/: not a detachable volume."
        );
        assert_eq!(
            UnmountError::Ambiguous(String::from("disk.adf")).to_string(),
            "disk.adf: more than one volume was mounted from a file of that name; \
             name it by where it is mounted."
        );
        assert!(UnmountError::NameCut(String::from("disk.adf"))
            .to_string()
            .ends_with("name the volume by where it is mounted."));
        assert_eq!(
            UnmountError::Usage.to_string(),
            "invalid usage",
            "no sentence"
        );
    }

    /// Every locale's `OPTIONS` section documents exactly the switches
    /// this parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the
    /// bundle's own on-disk `Help/` tree — the single source the image
    /// builder plants — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        use std::fs;

        let help_root = alloc::format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = alloc::format!("{help_root}/{locale}/unmount.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-f, --force`", "`-h, -?, --help`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/unmount.md must document {switch}"
                );
            }
        }
    }

    #[test]
    fn a_listing_failure_surfaces_as_service() {
        let mut fixture = table();
        fixture.fail = Some(Errno::NotFound);
        let detacher = MemDetacher::new();
        let err = Recorder::new();
        assert_eq!(
            run_args(&["usb1"], &fixture, &detacher, &err),
            Err(UnmountError::Service(Errno::NotFound))
        );
        assert!(detacher.detached.borrow().is_empty());
    }
}
