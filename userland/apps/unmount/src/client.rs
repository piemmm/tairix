//! The `unmount` engine: resolve the named volume through the System
//! Information API's mount and view listings, then hand its identity to the
//! kernel's detach path.

use alloc::format;
use alloc::string::String;

use tairix_abi::stdinfo::{Human, Severity, StdInfoKind, StdInfoRecord, Suggestion};
use tairix_abi::sysinfo::{MountAvailability, MountRecord, VOLUME_ID_LEN};
use tairix_abi::{Errno, STORAGE_CATALOG};
use tairix_help::{own_short_help, HelpSource};
use tairix_procinfo::{availability_marker, named_volume, Namespace, Transport, Unnamed};
use tairix_util::quote::quote_if_needed;

use crate::command::Command;
use crate::error::{Retention, UnmountError};
use crate::io::{Detacher, Output};

/// The one-line usage banner, the short help's fallback when the bundled
/// help document is unavailable.
pub const USAGE: &str = "usage: unmount [-f | --force] [--] NAME";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "unmount";

/// The facts the resolver needs about the matched mount: the volume's
/// stable identity, how the listing shows it, and its catalog name.
struct Resolved {
    volume_id: [u8; VOLUME_ID_LEN],
    availability: MountAvailability,
    catalog: Option<String>,
}

impl Resolved {
    fn of(volume: &MountRecord) -> Self {
        let catalog = volume
            .target_bytes()
            .strip_prefix(format!("/{STORAGE_CATALOG}/").as_bytes())
            .filter(|name| !name.is_empty() && !name.contains(&b'/'))
            .and_then(|name| core::str::from_utf8(name).ok())
            .map(String::from);
        Self {
            volume_id: volume.volume_id(),
            availability: volume.availability(),
            catalog,
        }
    }

    /// What the volume's writes had to do with `errno`, the kernel's refusal
    /// of a plain detach: the one refusal its listed state earns.
    fn retention(&self, errno: Errno) -> Option<Retention> {
        self.availability
            .plain_detach_refusal()
            .filter(|&(refused, _)| refused == errno)
            .map(|(_, retention)| retention)
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
///   `retention` names the refusal a plain detach of a vanished or
///   conflicted volume earns by design, which only a force gets past (the
///   `--force` suggestion is also emitted on fd 3, additive and ignorable).
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
            let retention = if force {
                None
            } else {
                resolved.retention(errno)
            };
            if let Some(retention) = retention {
                emit_force_suggestion(err, &name, &resolved, retention);
            }
            Err(UnmountError::Detach {
                name,
                errno,
                retention,
            })
        }
    }
}

/// Resolve `name` against the namespace by the rules every tool naming a
/// volume shares ([`named_volume`]): a durable root, a path, a catalog word,
/// or the file a volume was attached from, the caller's own over another's.
fn resolve(
    name: &str,
    detacher: &dyn Detacher,
    transport: &dyn Transport,
) -> Result<Resolved, UnmountError> {
    let namespace = Namespace::read(transport)?;
    named_volume(&namespace, name, detacher)
        .map(Resolved::of)
        .map_err(|why| unnamed(name, why))
}

/// `unmount`'s refusal of `name`, which names no volume for `why`.
fn unnamed(name: &str, why: Unnamed) -> UnmountError {
    let name = String::from(name);
    match why {
        Unnamed::NotFound => UnmountError::NotFound(name),
        Unnamed::NotShown => UnmountError::NotShown(name),
        Unnamed::Path(errno) => UnmountError::Path { name, errno },
        Unnamed::Ambiguous => UnmountError::Ambiguous(name),
        Unnamed::NameCut => UnmountError::NameCut(name),
        Unnamed::NoVolume => UnmountError::NotDetachable(name),
    }
}

/// Emit the fd-3 `suggestion` record for a plain detach refused for what
/// the volume's writes need: the audited force-unmount is the explicit way
/// to give retained writes up, or to acknowledge lost ones. The volume is
/// named by its catalog entry, which any directory resolves, else by the
/// operand as a shell reads it back. Best-effort and additive — it never
/// changes the diagnostic, exit status, or pipeline behaviour.
fn emit_force_suggestion(
    err: &dyn Output,
    operand: &str,
    resolved: &Resolved,
    retention: Retention,
) {
    let (message, consequence) = match retention {
        Retention::Held => ("The volume holds retained uncommitted data.", "discard it"),
        Retention::Lost => (
            "The volume's uncommitted data was already lost.",
            "detach it",
        ),
    };
    let name = resolved.catalog.as_deref().unwrap_or(operand);
    let dashes = if name.starts_with('-') { "-- " } else { "" };
    let suggestion = format!(
        "Use `unmount --force {dashes}{}` to {consequence}.",
        quote_if_needed(name)
    );
    let argv: &[&str] = if dashes.is_empty() {
        &["unmount", "--force", name]
    } else {
        &["unmount", "--force", "--", name]
    };
    let ai = format!(
        "{{\"subject\":\"volume_detach\",\
         \"refusal\":{{\"availability\":\"{}\",\
         \"retained_data_would_be_discarded\":{}}},\
         \"suggestion\":{}}}",
        availability_marker(resolved.availability).unwrap_or_default(),
        retention == Retention::Held,
        Suggestion::new(argv)
    );
    let record = StdInfoRecord::new(
        OWN_WORD,
        StdInfoKind::Suggestion,
        "fs.volume_force_required",
        Severity::Info,
        Human::with_suggestion(message, &suggestion),
    )
    .with_ai(&ai);
    let line = tairix_util::output::framed(&record);
    if !line.is_empty() {
        err.info(&line);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{run, USAGE};
    use crate::command::parse;
    use crate::error::{Retention, UnmountError};
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
    use tairix_procinfo::{Caller, Transport};

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

    /// Records every detach request, optionally failing it, and resolves
    /// paths as `realpath` does from a working directory.
    struct MemDetacher {
        fail: Option<Errno>,
        detached: RefCell<Vec<Detached>>,
        cwd: &'static str,
        /// A volume's mount point beneath which nothing answers, as the
        /// fail-closed stand-in answers for a vanished volume.
        unanswering: Option<&'static str>,
        /// Links, each the path it is at and the path it names.
        links: &'static [(&'static str, &'static str)],
        canonicalised: RefCell<Vec<String>>,
    }

    impl MemDetacher {
        fn new() -> Self {
            Self {
                fail: None,
                detached: RefCell::new(Vec::new()),
                cwd: "/Users/ada",
                unanswering: None,
                links: &[],
                canonicalised: RefCell::new(Vec::new()),
            }
        }
        fn failing(errno: Errno) -> Self {
            Self {
                fail: Some(errno),
                ..Self::new()
            }
        }
        fn unanswering(self, at: &'static str) -> Self {
            Self {
                unanswering: Some(at),
                ..self
            }
        }
        fn in_dir(self, cwd: &'static str) -> Self {
            Self { cwd, ..self }
        }
        fn linked(self, links: &'static [(&'static str, &'static str)]) -> Self {
            Self { links, ..self }
        }
    }

    impl Detacher for MemDetacher {
        fn detach(&self, volume_id: [u8; 16], force: bool) -> Result<(), Errno> {
            self.detached
                .borrow_mut()
                .push(Detached { volume_id, force });
            match self.fail {
                Some(errno) => Err(errno),
                None => Ok(()),
            }
        }
    }

    impl Caller for MemDetacher {
        fn user(&self) -> Option<u32> {
            Some(VIEWER)
        }

        /// As `realpath` resolves in the working directory, where nothing
        /// named `missing` exists, `forbidden` may not be entered, and nothing
        /// beneath an unanswering mount point answers.
        fn canonical(&self, path: &str) -> Result<String, Errno> {
            self.canonicalised.borrow_mut().push(String::from(path));
            let joined = if path.starts_with('/') {
                String::from(path)
            } else {
                alloc::format!("{}/{path}", self.cwd)
            };
            let mut at: Vec<String> = Vec::new();
            for part in joined
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
            {
                if part == ".." {
                    at.pop();
                    continue;
                }
                at.push(String::from(part));
                let spelled = alloc::format!("/{}", at.join("/"));
                if let Some((_, target)) = self.links.iter().find(|(link, _)| *link == spelled) {
                    at = target
                        .split('/')
                        .filter(|part| !part.is_empty())
                        .map(String::from)
                        .collect();
                }
                let spelled = alloc::format!("/{}", at.join("/"));
                if self.unanswering.is_some_and(|dead| {
                    spelled == dead || spelled.starts_with(&alloc::format!("{dead}/"))
                }) {
                    return Err(Errno::DeviceFault);
                }
            }
            match at.last().map(String::as_str) {
                Some("missing") => Err(Errno::NotFound),
                Some("forbidden") => Err(Errno::PermissionDenied),
                _ => Ok(alloc::format!("/{}", at.join("/"))),
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

    /// A volume's durable root names that volume and no other, with a slash
    /// after it or without: a stick put in its place under the same catalog
    /// name is not it, and nothing is detached.
    #[test]
    fn a_durable_root_names_exactly_its_volume() {
        let root = alloc::format!("id::{}", tairix_path::VolumeId::from_bytes([1u8; 16]));
        for spelled in [root.clone(), alloc::format!("{root}/")] {
            let detacher = MemDetacher::new();
            run_args(
                &["--", spelled.as_str()],
                &table(),
                &detacher,
                &Recorder::new(),
            )
            .expect("detaches");
            assert_eq!(
                detacher.detached.borrow().as_slice(),
                &[Detached {
                    volume_id: [1u8; 16],
                    force: false,
                }]
            );
        }
        let swapped = Fixture::of(alloc::vec![record(
            b"usb1",
            b"/Storage/usb1",
            MountAvailability::Available,
            [9u8; 16],
        )]);
        let detacher = MemDetacher::new();
        assert!(matches!(
            run_args(
                &["--", root.as_str()],
                &swapped,
                &detacher,
                &Recorder::new()
            ),
            Err(UnmountError::NotFound(_))
        ));
        assert!(detacher.detached.borrow().is_empty());
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
                retention: Some(Retention::Held),
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
                retention: None,
            })
        );
        assert!(err.infos.borrow().is_empty());
    }

    #[test]
    fn a_failed_unmount_ends_as_util_linux_ends_one() {
        let detach = |errno| UnmountError::Detach {
            name: String::from("workbench"),
            errno,
            retention: None,
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
        let refused = |name: &str, errno, retention| {
            UnmountError::Detach {
                name: String::from(name),
                errno,
                retention,
            }
            .to_string()
        };
        assert_eq!(
            refused("workbench", Errno::Busy, None),
            alloc::format!("workbench: {}.", Errno::Busy)
        );
        let held = refused("usb2", Errno::DeviceFault, Some(Retention::Held));
        assert!(held.starts_with("usb2: volume holds") && held.ends_with('.'));
        let lost = refused("usb4", Errno::DeviceFault, Some(Retention::Lost));
        assert!(lost.contains("already lost") && lost.contains("use --force to detach it"));
        assert!(lost.ends_with('.'));
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

    fn detached(detacher: &MemDetacher) -> Vec<Detached> {
        detacher.detached.borrow().clone()
    }

    fn forced(id: u8) -> Vec<Detached> {
        alloc::vec![Detached {
            volume_id: [id; 16],
            force: true,
        }]
    }

    /// A vanished volume's mount point no longer answers, so it is matched as
    /// spelled and the dead volume is never resolved through.
    #[test]
    fn a_volume_that_no_longer_answers_is_named_by_its_mount_point() {
        for spelling in ["/Storage/usb2", "/Storage/usb2/", "/Storage/./usb2"] {
            let detacher = MemDetacher::new().unanswering("/Storage/usb2");
            let err = Recorder::new();
            assert_eq!(
                run_args(&["-f", spelling], &table(), &detacher, &err),
                Ok(()),
                "{spelling}"
            );
            assert_eq!(detached(&detacher), forced(2), "{spelling}");
            assert!(detacher.canonicalised.borrow().is_empty(), "{spelling}");
        }
        let detacher = MemDetacher::failing(Errno::DeviceFault).unanswering("/Storage/usb2");
        let err = Recorder::new();
        assert_eq!(
            run_args(&["/Storage/usb2"], &table(), &detacher, &err),
            Err(UnmountError::Detach {
                name: String::from("/Storage/usb2"),
                errno: Errno::DeviceFault,
                retention: Some(Retention::Held),
            })
        );
        assert_eq!(err.infos.borrow().len(), 1);
    }

    fn lost_image_in_home() -> Fixture {
        Fixture::of(alloc::vec![record(
            b"wb.adf",
            b"/Storage/wb",
            MountAvailability::UnavailableLost,
            [5u8; 16],
        )])
        .bound_at(b"/Users/ada/Home/wb", [5u8; 16])
    }

    #[test]
    fn a_relative_spelling_is_read_from_the_working_directory_without_resolving_it() {
        for spelling in ["Home/wb", "./Home/wb/"] {
            let detacher = MemDetacher::new().unanswering("/Users/ada/Home/wb");
            let err = Recorder::new();
            assert_eq!(
                run_args(&["-f", spelling], &lost_image_in_home(), &detacher, &err),
                Ok(()),
                "{spelling}"
            );
            assert_eq!(detached(&detacher), forced(5), "{spelling}");
            assert_eq!(
                detacher.canonicalised.borrow().as_slice(),
                [String::from(".")],
                "only the working directory was resolved"
            );
        }
    }

    #[test]
    fn dot_names_the_directory_it_stands_for() {
        let detacher = MemDetacher::new().in_dir("/Users/ada/Home/wb");
        let err = Recorder::new();
        assert_eq!(
            run_args(&["-f", "."], &lost_image_in_home(), &detacher, &err),
            Ok(())
        );
        assert_eq!(detached(&detacher), forced(5));
    }

    /// A `..` is applied to the directory the walk reached, never folded
    /// into the text before it.
    #[test]
    fn a_spelling_that_climbs_is_resolved_never_folded() {
        let fixture = table().bound_at(b"/Users/ada/usb1", [5u8; 16]);
        let detacher = MemDetacher::new().linked(&[("/Users/ada/lnk", "/Storage/x")]);
        let err = Recorder::new();
        assert_eq!(
            run_args(&["/Users/ada/lnk/../usb1"], &fixture, &detacher, &err),
            Ok(())
        );
        assert_eq!(
            detached(&detacher),
            alloc::vec![Detached {
                volume_id: [1u8; 16],
                force: false,
            }]
        );
        assert_eq!(
            detacher.canonicalised.borrow().as_slice(),
            [String::from("/Users/ada/lnk/../usb1")]
        );
    }

    fn unwell(availability: MountAvailability) -> Fixture {
        Fixture::of(alloc::vec![record(
            b"usb3",
            b"/Storage/usb3",
            availability,
            [3u8; 16],
        )])
    }

    #[test]
    fn a_live_unwell_volume_refused_busy_is_told_only_busy() {
        use alloc::string::ToString;
        for availability in [MountAvailability::Degraded, MountAvailability::Recovering] {
            let detacher = MemDetacher::failing(Errno::Busy);
            let err = Recorder::new();
            let refused = run_args(&["/Storage/usb3"], &unwell(availability), &detacher, &err)
                .expect_err("refused");
            assert_eq!(
                refused,
                UnmountError::Detach {
                    name: String::from("/Storage/usb3"),
                    errno: Errno::Busy,
                    retention: None,
                },
                "{availability:?}"
            );
            assert_eq!(
                refused.to_string(),
                alloc::format!("/Storage/usb3: {}.", Errno::Busy)
            );
            assert!(err.infos.borrow().is_empty(), "{availability:?}");
        }
    }

    #[test]
    fn a_refusal_of_authority_never_suggests_force() {
        use alloc::string::ToString;
        let detacher = MemDetacher::failing(Errno::PermissionDenied);
        let err = Recorder::new();
        let refused = run_args(&["usb2"], &table(), &detacher, &err).expect_err("refused");
        assert_eq!(
            refused,
            UnmountError::Detach {
                name: String::from("usb2"),
                errno: Errno::PermissionDenied,
                retention: None,
            }
        );
        assert_eq!(refused.status(), 1);
        assert_eq!(
            refused.to_string(),
            alloc::format!("usb2: {}.", Errno::PermissionDenied)
        );
        assert!(err.infos.borrow().is_empty());
    }

    #[test]
    fn a_refused_force_is_never_told_to_force() {
        use alloc::string::ToString;
        for errno in [Errno::PermissionDenied, Errno::DeviceFault] {
            let detacher = MemDetacher::failing(errno);
            let err = Recorder::new();
            let refused =
                run_args(&["-f", "usb2"], &table(), &detacher, &err).expect_err("refused");
            assert!(
                matches!(
                    refused,
                    UnmountError::Detach {
                        retention: None,
                        ..
                    }
                ),
                "{errno:?}"
            );
            assert!(!refused.to_string().contains("--force"), "{refused}");
            assert!(err.infos.borrow().is_empty(), "{errno:?}");
        }
    }

    #[test]
    fn a_lost_volume_is_told_its_data_is_already_lost() {
        use alloc::string::ToString;
        let fixture = Fixture::of(alloc::vec![record(
            b"usb4",
            b"/Storage/usb4",
            MountAvailability::UnavailableLost,
            [4u8; 16],
        )]);
        let detacher = MemDetacher::failing(Errno::DeviceFault);
        let err = Recorder::new();
        let refused = run_args(&["usb4"], &fixture, &detacher, &err).expect_err("refused");
        assert!(matches!(
            refused,
            UnmountError::Detach {
                retention: Some(Retention::Lost),
                ..
            }
        ));
        let text = refused.to_string();
        assert!(text.contains("already lost") && text.contains("use --force to detach it"));
        let infos = err.infos.borrow();
        assert_eq!(infos.len(), 1);
        assert!(
            infos[0].contains("\"retained_data_would_be_discarded\":false"),
            "{}",
            infos[0]
        );
        assert!(
            infos[0].contains("\"availability\":\"unavailable-lost\""),
            "{}",
            infos[0]
        );
    }

    #[test]
    fn a_conflicted_volume_is_told_force_only_for_its_held_set() {
        let conflicted = || unwell(MountAvailability::RecoveryConflict);
        let detacher = MemDetacher::failing(Errno::NotEmpty);
        let err = Recorder::new();
        let refused = run_args(&["usb3"], &conflicted(), &detacher, &err).expect_err("refused");
        assert!(matches!(
            refused,
            UnmountError::Detach {
                retention: Some(Retention::Held),
                ..
            }
        ));
        let infos = err.infos.borrow();
        assert_eq!(infos.len(), 1);
        assert!(
            infos[0].contains("\"availability\":\"recovery-conflict\""),
            "{}",
            infos[0]
        );
        assert!(
            infos[0].contains("fs.volume_force_required"),
            "{}",
            infos[0]
        );
        let busy = MemDetacher::failing(Errno::Busy);
        let quiet = Recorder::new();
        let refused = run_args(&["usb3"], &conflicted(), &busy, &quiet).expect_err("refused");
        assert!(matches!(
            refused,
            UnmountError::Detach {
                retention: None,
                ..
            }
        ));
        assert!(quiet.infos.borrow().is_empty());
    }

    /// The suggested command reads back as a shell reads it: the volume is
    /// named by its catalog entry, which any directory resolves, else by the
    /// operand quoted, with `--` before one beginning `-`.
    #[test]
    fn the_suggested_command_reads_back_as_a_shell_reads_it() {
        let lost = |source: &[u8], target: &[u8]| {
            Fixture::of(alloc::vec![record(
                source,
                target,
                MountAvailability::UnavailableLost,
                [6u8; 16],
            )])
        };
        let suggested = |fixture: &Fixture, args: &[&str]| {
            let detacher = MemDetacher::failing(Errno::DeviceFault);
            let err = Recorder::new();
            assert!(run_args(args, fixture, &detacher, &err).is_err());
            let infos = err.infos.borrow().clone();
            assert_eq!(infos.len(), 1, "{args:?}");
            infos[0].clone()
        };
        let record = suggested(&lost(b"wb.adf", b"/Storage/extras"), &["wb.adf"]);
        assert!(record.contains("unmount --force extras"), "{record}");
        // Within the source bound, and past any fixed line a record might
        // have been given.
        let long = "Amiga Workbench 1.3 Extras (1988).adf";
        let record = suggested(&lost(long.as_bytes(), b"/Users/ada/extras"), &[long]);
        assert!(record.len() > 512, "{}", record.len());
        assert!(
            record.contains(&alloc::format!("unmount --force '{long}'")),
            "{record}"
        );
        let record = suggested(&lost(b"my disk.adf", b"/Users/ada/disk"), &["my disk.adf"]);
        assert!(record.contains("unmount --force 'my disk.adf'"), "{record}");
        let record = suggested(&lost(b"-odd.adf", b"/Users/ada/odd"), &["--", "-odd.adf"]);
        assert!(record.contains("unmount --force -- -odd.adf"), "{record}");
        assert!(
            record.contains("\"argv\":[\"unmount\",\"--force\",\"--\",\"-odd.adf\"]"),
            "{record}"
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
