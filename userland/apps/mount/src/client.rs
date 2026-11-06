//! The mount engine: list the mount table through the System Information
//! API, or mount a disk image through the kernel.

use alloc::format;
use alloc::string::String;

use tairix_abi::imageattach::{ImageAccess, ImageAttach, ImageAttachReply};
use tairix_abi::netattach::Presentation;
use tairix_abi::sysinfo::{MountAvailability, MountRecord, MOUNT_TARGET_MAX};
use tairix_abi::volume::{is_binding_path, ReplayAnswer, VolumeReplayRequest};
use tairix_abi::{Errno, STORAGE_CATALOG};
use tairix_help::{own_short_help, HelpSource};
use tairix_procinfo::{named_volume, render_binding, Namespace, Output, Transport};
use tairix_util::output::Output as Stream;
use tairix_util::quote::{quote_if_needed, quote_subject};

use crate::command::{Command, HeldAnswer, Listing, MountRequest};
use crate::error::MountError;
use crate::io::Mounter;

/// The usage line, the short help's fallback when the bundled help document
/// is unavailable.
pub const USAGE: &str = "usage: mount [-lrvw] [-t TYPE] [-o OPTIONS] [--] [IMAGE [TARGET]]
       mount [-v] --list-held | --replay-held NAME | --discard-held NAME";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "mount";

/// Run one [`Command`].
///
/// A [`Command::List`] writes one line per view binding its [`Listing`]
/// shows to `out`. A [`Command::Mount`] mounts the image privately for the
/// caller through `mounter`, silently unless `-v` asked where it went; a
/// volume mounted read-only that was not asked to be is warned of on `err`, as
/// util-linux warns of a write-protected source, and `-w` makes that a
/// failure. A [`Command::Answer`] hands the kernel the key of the volume its
/// name names, which must hold writes awaiting an answer, silently unless `-v`
/// asked what became of them.
///
/// # Errors
///
/// * [`MountError::BadName`] — the kernel's naming policy keeps the name
///   asked for.
/// * [`MountError::WriteProtected`], [`MountError::Refused`],
///   [`MountError::AlreadyMounted`], [`MountError::Mount`],
///   [`MountError::Target`] — the mount failed.
/// * [`MountError::Unresolved`], [`MountError::NotHeld`],
///   [`MountError::Answer`] — the answer was not carried out.
/// * [`MountError::Service`] — listing the mount table failed.
/// * [`MountError::Output`] — writing the terminal failed.
pub fn run(
    command: Command,
    locale: Option<&str>,
    mounter: &dyn Mounter,
    transport: &dyn Transport,
    help: &dyn HelpSource,
    out: &dyn Stream,
    err: &dyn Stream,
) -> Result<(), MountError> {
    match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            out.write_all(&bytes).map_err(MountError::Output)
        }
        Command::List(listing) => list(transport, &listing, out),
        Command::Mount(request) => mount(&request, mounter, out, err),
        Command::Answer(asked) => answer_held(&asked, mounter, transport, out),
    }
}

/// Read the namespace and write one line per view binding `listing` shows,
/// from the volume each shows; one showing none is of type `none`.
fn list(transport: &dyn Transport, listing: &Listing, out: &dyn Stream) -> Result<(), MountError> {
    let namespace = Namespace::read(transport)?;
    for view in namespace.views() {
        let volume = namespace.volume_of(view);
        let held = volume
            .is_some_and(|record| record.availability() == MountAvailability::ReplayUnconfirmed);
        let fstype = volume.map_or(&b"none"[..], MountRecord::fstype_bytes);
        if (held || !listing.held) && listing.types.shows(fstype) {
            out.write_line(&render_binding(view, volume))
                .map_err(MountError::Output)?;
        }
    }
    Ok(())
}

/// Mount the image `request` names, privately for the caller.
fn mount(
    request: &MountRequest,
    mounter: &dyn Mounter,
    out: &dyn Stream,
    err: &dyn Stream,
) -> Result<(), MountError> {
    // Every directory is resolved as util-linux canonicalises a mount point,
    // so a refusal of it is named as its own, as resolved, and never the image's.
    let refused_target = |target: &str, errno| MountError::Target {
        target: String::from(target),
        errno,
    };
    let target = match &request.target {
        Some(asked) => {
            let resolved = mounter
                .canonical(asked)
                .map_err(|errno| refused_target(asked, errno))?;
            if !is_binding_path(&resolved) {
                let errno = if resolved.len() > MOUNT_TARGET_MAX {
                    Errno::LengthOutOfRange
                } else {
                    Errno::OutOfRange
                };
                return Err(refused_target(&resolved, errno));
            }
            Some(resolved)
        }
        None => None,
    };
    let attach = ImageAttach {
        name: request.name.as_deref(),
        presentation: Presentation::Private,
        access: request.access,
        target: target.as_deref(),
        expects: request.expects,
        adopt: false,
        file_name: None,
    };
    let image = || request.image.clone();
    let volume = match mounter.attach(&request.image, &attach) {
        Ok(ImageAttachReply::Attached(volume)) => volume,
        // An attach answered so made no binding at `target`: the failure
        // names where it was asked to go and where it is.
        Ok(ImageAttachReply::AlreadyAttached(volume)) => {
            let at = catalog_path(volume.name());
            let asked = target
                .clone()
                .or_else(|| request.name.as_deref().map(catalog_path))
                .unwrap_or_else(|| at.clone());
            return Err(MountError::AlreadyMounted {
                image: image(),
                asked,
                at,
            });
        }
        Ok(ImageAttachReply::Refused(why)) => {
            return Err(MountError::Refused {
                image: image(),
                why,
            })
        }
        Ok(ImageAttachReply::TargetRefused(errno)) => {
            return Err(refused_target(target.as_deref().unwrap_or_default(), errno))
        }
        Ok(ImageAttachReply::NameRefused(errno)) => {
            return Err(match request.name.as_deref() {
                // The kernel's naming policy keeps it for the volumes it names.
                Some(name) if errno == Errno::OutOfRange => MountError::BadName(name.into()),
                Some(name) => MountError::Target {
                    target: catalog_path(name),
                    errno,
                },
                None => MountError::Mount {
                    image: image(),
                    errno,
                },
            });
        }
        Err(Errno::ReadOnly) if request.access == ImageAccess::Writable => {
            return Err(MountError::WriteProtected { image: image() });
        }
        Err(errno) => {
            return Err(MountError::Mount {
                image: image(),
                errno,
            })
        }
    };
    let at = target.unwrap_or_else(|| catalog_path(volume.name()));
    if request.access == ImageAccess::WritableWherePossible && volume.read_only() {
        let warning = format!(
            "mount: {}: WARNING: source write-protected, mounted read-only.",
            quote_subject(&at)
        );
        err.write_line(&warning).map_err(MountError::Output)?;
    }
    if request.verbose {
        let said = format!(
            "mount: {} mounted on {}.",
            quote_if_needed(&request.image),
            quote_if_needed(&at)
        );
        out.write_line(&said).map_err(MountError::Output)?;
    }
    Ok(())
}

/// Answer for the writes the volume `asked` names holds — written to its
/// medium, or given up — as the kernel decides who may, silently unless `-v`
/// asked what became of them. The volume is named as `unmount` names one, and
/// must be listed holding writes awaiting an answer.
fn answer_held(
    asked: &HeldAnswer,
    mounter: &dyn Mounter,
    transport: &dyn Transport,
    out: &dyn Stream,
) -> Result<(), MountError> {
    let namespace = Namespace::read(transport)?;
    let name = || asked.name.clone();
    let volume = named_volume(&namespace, &asked.name, mounter)
        .map_err(|why| MountError::Unresolved { name: name(), why })?;
    if volume.availability() != MountAvailability::ReplayUnconfirmed {
        return Err(MountError::NotHeld(name()));
    }
    let request = VolumeReplayRequest {
        volume_id: volume.volume_id(),
        answer: asked.answer,
    };
    mounter
        .replay(request)
        .map_err(|errno| MountError::Answer {
            name: name(),
            answer: asked.answer,
            errno,
        })?;
    if asked.verbose {
        let done = match asked.answer {
            ReplayAnswer::Write => "written",
            ReplayAnswer::Discard => "discarded",
        };
        let said = format!(
            "mount: {}: held writes {done}.",
            quote_if_needed(&asked.name)
        );
        out.write_line(&said).map_err(MountError::Output)?;
    }
    Ok(())
}

/// Where `volume` is in the catalog.
fn catalog_path(name: &str) -> String {
    format!("/{STORAGE_CATALOG}/{name}")
}

#[cfg(test)]
mod tests {
    use super::{run, USAGE};
    use crate::command::{parse, Command, Listing};
    use crate::error::MountError;
    use crate::io::Mounter;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::imageattach::{ImageAccess, ImageAttachReply, ImageRefusal};
    use tairix_abi::netattach::{AttachReply, Presentation};
    use tairix_abi::sysinfo::{
        MountAvailability, MountRecord, MountVolumeState, PageRequest, SysinfoQueryId,
        SysinfoRequestHeader, ViewBindingRecord, MOUNT_TARGET_MAX,
    };
    use tairix_abi::volume::{ReplayAnswer, VolumeFsType, VolumeFsTypes, VolumeReplayRequest};
    use tairix_abi::Errno;
    use tairix_help::{HelpSource, SourceError};
    use tairix_procinfo::{Caller, Transport};
    use tairix_util::output::Output;

    /// An in-memory `sysinfod` answering the mount and view lists from a
    /// fixture, decoding the request as the real service does.
    struct Fixture {
        records: Vec<MountRecord>,
        views: Vec<ViewBindingRecord>,
        fail: Option<Errno>,
        seen: RefCell<Vec<SysinfoQueryId>>,
    }

    impl Fixture {
        fn new(records: Vec<MountRecord>, views: Vec<ViewBindingRecord>) -> Self {
            Self {
                records,
                views,
                fail: None,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn empty() -> Self {
            Self::new(Vec::new(), Vec::new())
        }
    }

    impl Transport for Fixture {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            let header = SysinfoRequestHeader::from_bytes(request)?;
            self.seen.borrow_mut().push(header.query);
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
                    .map(|r| r.to_le_bytes().to_vec())
                    .collect(),
                SysinfoQueryId::VIEW_LIST => self
                    .views
                    .iter()
                    .map(|v| v.to_le_bytes().to_vec())
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

    /// Captures what is written.
    #[derive(Default)]
    struct Recorder {
        bytes: RefCell<Vec<u8>>,
    }

    impl Recorder {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).expect("utf-8")
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// An output seam that always fails.
    struct FailingOutput;

    impl Output for FailingOutput {
        fn write_all(&self, _bytes: &[u8]) -> Result<(), Errno> {
            Err(Errno::PermissionDenied)
        }
    }

    /// No bundled help: the short help falls back to the usage line.
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

    /// One image attach as the kernel received it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Asked {
        image: String,
        name: Option<String>,
        presentation: Presentation,
        access: ImageAccess,
        target: Option<String>,
        expects: Option<VolumeFsTypes>,
        adopt: bool,
    }

    /// Records every attach and answer for held writes, answering each as
    /// `answer` and `replay` say.
    struct MemMounter {
        answer: Result<ImageAttachReply, Errno>,
        asked: RefCell<Vec<Asked>>,
        replay: Result<(), Errno>,
        replayed: RefCell<Vec<VolumeReplayRequest>>,
    }

    impl MemMounter {
        fn answering(answer: Result<ImageAttachReply, Errno>) -> Self {
            Self {
                answer,
                asked: RefCell::new(Vec::new()),
                replay: Ok(()),
                replayed: RefCell::new(Vec::new()),
            }
        }

        fn attaching(name: &str) -> Self {
            Self::answering(Ok(ImageAttachReply::Attached(volume(name, false))))
        }

        /// One whose kernel answers every answer for held writes `replay`.
        fn replaying(replay: Result<(), Errno>) -> Self {
            Self {
                replay,
                ..Self::attaching("x")
            }
        }
    }

    impl Caller for MemMounter {
        fn user(&self) -> Option<u32> {
            Some(1000)
        }

        /// As `realpath` resolves in `/Users/ada/Home`, where nothing named
        /// `missing` exists.
        fn canonical(&self, path: &str) -> Result<String, Errno> {
            if path.rsplit('/').next() == Some("missing") {
                return Err(Errno::NotFound);
            }
            if path == "lnk" {
                // A link whose stored target holds a tab.
                return Ok(String::from("/Users/ada/Home/new\tdir"));
            }
            Ok(if path.starts_with('/') {
                String::from(path)
            } else {
                alloc::format!("/Users/ada/Home/{path}")
            })
        }
    }

    impl Mounter for MemMounter {
        fn attach(
            &self,
            image: &str,
            attach: &tairix_abi::imageattach::ImageAttach<'_>,
        ) -> Result<ImageAttachReply, Errno> {
            self.asked.borrow_mut().push(Asked {
                image: image.to_string(),
                name: attach.name.map(String::from),
                presentation: attach.presentation,
                access: attach.access,
                target: attach.target.map(String::from),
                expects: attach.expects,
                adopt: attach.adopt,
            });
            self.answer
        }

        fn replay(&self, request: VolumeReplayRequest) -> Result<(), Errno> {
            self.replayed.borrow_mut().push(request);
            self.replay
        }
    }

    fn volume(name: &str, read_only: bool) -> AttachReply {
        AttachReply::new([7; 16], name, read_only).expect("a valid name")
    }

    fn record(source: &[u8], target: &[u8], flags: MountFlags, id: u8) -> MountRecord {
        MountRecord::new(
            source,
            target,
            b"affs",
            flags,
            MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::Available,
                medium: None,
            },
            [id; 16],
        )
        .expect("record")
    }

    fn view(target: &[u8], subtree: &[u8], flags: MountFlags, id: u8) -> ViewBindingRecord {
        ViewBindingRecord::new(target, subtree, flags, [id; 16]).expect("view")
    }

    /// Run `args` against `mounter` and `fixture`: the outcome, stdout and
    /// stderr.
    fn ran(
        args: &[&str],
        mounter: &MemMounter,
        fixture: &Fixture,
    ) -> (Result<(), MountError>, String, String) {
        let (out, err) = (Recorder::default(), Recorder::default());
        let command = parse(args).expect("a valid command");
        let outcome = run(command, None, mounter, fixture, &NoHelp, &out, &err);
        (outcome, out.text(), err.text())
    }

    #[test]
    fn help_without_a_document_prints_the_usage_line() {
        let out = Recorder::default();
        let fixture = Fixture::empty();
        let mounter = MemMounter::attaching("x");
        assert_eq!(
            run(
                Command::Help,
                None,
                &mounter,
                &fixture,
                &NoHelp,
                &out,
                &FailingOutput
            ),
            Ok(())
        );
        assert_eq!(out.text(), alloc::format!("{USAGE}\n"));
        assert!(fixture.seen.borrow().is_empty());
        assert!(mounter.asked.borrow().is_empty());
    }

    #[test]
    fn list_renders_one_line_per_binding_and_routes_the_queries() {
        let fixture = Fixture::new(
            alloc::vec![
                record(b"rootfs", b"/", MountFlags::READ_ONLY, 1),
                record(b"wb.adf", b"/Storage/workbench", MountFlags::NOEXEC, 2),
            ],
            alloc::vec![
                view(b"/", b"/", MountFlags::READ_ONLY, 1),
                view(b"/Storage/workbench", b"/", MountFlags::NOEXEC, 2),
            ],
        );
        let mounter = MemMounter::attaching("x");
        let out = Recorder::default();
        assert_eq!(
            run(
                Command::List(Listing::default()),
                None,
                &mounter,
                &fixture,
                &NoHelp,
                &out,
                &FailingOutput
            ),
            Ok(())
        );
        assert_eq!(
            out.text(),
            "rootfs on / type affs (ro)\nwb.adf on /Storage/workbench type affs (rw,noexec)\n"
        );
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::MOUNT_LIST, SysinfoQueryId::VIEW_LIST]
        );
        assert!(mounter.asked.borrow().is_empty(), "listing mounts nothing");
    }

    #[test]
    fn a_listing_names_only_the_types_t_asks_for() {
        let fixture = Fixture::new(
            alloc::vec![
                record(b"rootfs", b"/", MountFlags::READ_ONLY, 1),
                record(b"wb.adf", b"/Storage/workbench", MountFlags::NOEXEC, 2),
            ],
            alloc::vec![
                view(b"/", b"/", MountFlags::READ_ONLY, 1),
                view(b"/Storage", b"", MountFlags::NOEXEC, 0),
                view(b"/Storage/workbench", b"/", MountFlags::NOEXEC, 2),
            ],
        );
        let mounter = MemMounter::attaching("x");
        let (outcome, out, _) = ran(&["-t", "noaffs"], &mounter, &fixture);
        assert_eq!(outcome, Ok(()));
        assert_eq!(out, "none on /Storage type none (rw,noexec)\n");
        for args in [&["-v", "-t", "nonone"][..], &["-l", "-t", "AFFS"][..]] {
            let (_, out, _) = ran(args, &mounter, &fixture);
            assert_eq!(
                out,
                "rootfs on / type affs (ro)\nwb.adf on /Storage/workbench type affs (rw,noexec)\n",
                "{args:?}"
            );
        }
        assert!(mounter.asked.borrow().is_empty(), "listing mounts nothing");
    }

    #[test]
    fn a_listing_that_fails_says_where() {
        let mut fixture = Fixture::empty();
        fixture.fail = Some(Errno::NotFound);
        let mounter = MemMounter::attaching("x");
        let out = Recorder::default();
        assert_eq!(
            run(
                Command::List(Listing::default()),
                None,
                &mounter,
                &fixture,
                &NoHelp,
                &out,
                &out
            ),
            Err(MountError::Service(Errno::NotFound))
        );
        let listed = Fixture::new(
            alloc::vec![record(b"rootfs", b"/", MountFlags::default(), 1)],
            alloc::vec![view(b"/", b"/", MountFlags::default(), 1)],
        );
        assert_eq!(
            run(
                Command::List(Listing::default()),
                None,
                &mounter,
                &listed,
                &NoHelp,
                &FailingOutput,
                &out
            ),
            Err(MountError::Output(Errno::PermissionDenied))
        );
    }

    #[test]
    fn an_image_is_mounted_privately_and_silently() {
        let mounter = MemMounter::attaching("workbench");
        let (outcome, out, err) = ran(&["-t", "affs", "wb.adf"], &mounter, &Fixture::empty());
        assert_eq!(outcome, Ok(()));
        assert_eq!((out.as_str(), err.as_str()), ("", ""));
        assert_eq!(
            mounter.asked.borrow().as_slice(),
            [Asked {
                image: String::from("wb.adf"),
                name: None,
                presentation: Presentation::Private,
                access: ImageAccess::WritableWherePossible,
                target: None,
                expects: VolumeFsTypes::of([VolumeFsType::Affs]),
                // An image mounted already is reported, never taken up for
                // this session too.
                adopt: false,
            }]
        );
    }

    #[test]
    fn a_target_names_the_volume_or_shows_it_where_it_says() {
        let mounter = MemMounter::attaching("workbench");
        let fixture = Fixture::empty();
        for (target, name, shown) in [
            ("/Storage/workbench", Some("workbench"), None),
            ("/Users/ada/Home/wb", None, Some("/Users/ada/Home/wb")),
            ("wb", None, Some("/Users/ada/Home/wb")),
        ] {
            mounter.asked.borrow_mut().clear();
            let (outcome, ..) = ran(&["wb.adf", target], &mounter, &fixture);
            assert_eq!(outcome, Ok(()), "{target}");
            let asked = mounter.asked.borrow();
            assert_eq!(asked[0].name.as_deref(), name, "{target}");
            assert_eq!(asked[0].target.as_deref(), shown, "{target}");
        }
        assert_eq!(
            ran(&["wb.adf", "missing"], &mounter, &fixture).0,
            Err(MountError::Target {
                target: String::from("missing"),
                errno: Errno::NotFound,
            })
        );
    }

    /// A refusal of the directory is the directory's, however it was given
    /// and whichever side refused it, and never the image's.
    #[test]
    fn a_refused_directory_is_named_as_itself() {
        let fixture = Fixture::empty();
        let target = |target: &str, errno| {
            Err(MountError::Target {
                target: String::from(target),
                errno,
            })
        };
        let mounter = MemMounter::attaching("workbench");
        assert_eq!(
            ran(&["wb.adf", "/Users/ada/Home/missing"], &mounter, &fixture).0,
            target("/Users/ada/Home/missing", Errno::NotFound)
        );
        let deep = alloc::format!("/Users/ada/Home/{}", "d".repeat(MOUNT_TARGET_MAX));
        assert_eq!(
            ran(&["wb.adf", &deep], &mounter, &fixture).0,
            target(&deep, Errno::LengthOutOfRange),
            "too long to be listed where it would be bound"
        );
        assert!(mounter.asked.borrow().is_empty(), "nothing was attached");
        let refusing = MemMounter::answering(Ok(ImageAttachReply::TargetRefused(Errno::NotEmpty)));
        let (outcome, _, err) = ran(&["wb.adf", "full"], &refusing, &fixture);
        assert_eq!(
            outcome,
            target("/Users/ada/Home/full", Errno::NotEmpty),
            "named as resolved, as util-linux names a mount point"
        );
        assert!(err.is_empty(), "the caller reports it once");
        let failure = outcome.expect_err("refused");
        assert_eq!(failure.status(), 32);
        assert_eq!(
            failure.to_string(),
            alloc::format!("/Users/ada/Home/full: {}.", Errno::NotEmpty)
        );
    }

    /// A directory that resolves to a spelling no binding is listed with,
    /// though within the bound, is out of range, never too long.
    #[test]
    fn a_target_holding_a_control_character_is_refused_out_of_range() {
        let fixture = Fixture::empty();
        let mounter = MemMounter::attaching("workbench");
        assert_eq!(
            ran(&["wb.adf", "lnk"], &mounter, &fixture).0,
            Err(MountError::Target {
                target: String::from("/Users/ada/Home/new\tdir"),
                errno: Errno::OutOfRange,
            })
        );
        assert!(mounter.asked.borrow().is_empty(), "nothing was attached");
    }

    /// A name the kernel's naming policy keeps for the volumes it names is a
    /// command-line error, as a name no volume may take is.
    #[test]
    fn a_name_the_policy_keeps_is_refused_as_a_name() {
        let fixture = Fixture::empty();
        let mounter = MemMounter::answering(Ok(ImageAttachReply::NameRefused(Errno::OutOfRange)));
        let refused = ran(&["-o", "name=users", "wb.adf"], &mounter, &fixture).0;
        assert_eq!(refused, Err(MountError::BadName(String::from("users"))));
        assert!(refused.expect_err("refused").misused());
    }

    #[test]
    fn a_name_in_use_is_refused_as_where_it_would_be_shown() {
        let fixture = Fixture::empty();
        let mounter =
            MemMounter::answering(Ok(ImageAttachReply::NameRefused(Errno::AlreadyExists)));
        let refused = ran(&["wb.adf", "/Storage/usb1"], &mounter, &fixture).0;
        assert_eq!(
            refused,
            Err(MountError::Target {
                target: String::from("/Storage/usb1"),
                errno: Errno::AlreadyExists,
            })
        );
        assert_eq!(refused.expect_err("refused").status(), 32);
    }

    #[test]
    fn a_refusal_to_open_the_image_is_the_images_though_a_name_was_asked() {
        let fixture = Fixture::empty();
        let mounter = MemMounter::answering(Err(Errno::OutOfRange));
        assert_eq!(
            ran(&["Work:wb.adf", "/Storage/workbench"], &mounter, &fixture).0,
            Err(MountError::Mount {
                image: String::from("Work:wb.adf"),
                errno: Errno::OutOfRange,
            })
        );
    }

    #[test]
    fn verbose_says_where_it_went() {
        let mounter = MemMounter::attaching("workbench");
        let fixture = Fixture::empty();
        let (_, out, _) = ran(&["-v", "wb.adf"], &mounter, &fixture);
        assert_eq!(out, "mount: wb.adf mounted on /Storage/workbench.\n");
        let (_, out, _) = ran(&["-v", "wb.adf", "/Users/ada/Home/wb"], &mounter, &fixture);
        assert_eq!(out, "mount: wb.adf mounted on /Users/ada/Home/wb.\n");
    }

    #[test]
    fn a_volume_mounted_read_only_unasked_is_warned_of() {
        let mounter = MemMounter::answering(Ok(ImageAttachReply::Attached(volume("disc", true))));
        let fixture = Fixture::empty();
        let (outcome, out, err) = ran(&["disc.iso"], &mounter, &fixture);
        assert_eq!(outcome, Ok(()));
        assert_eq!(out, "");
        assert_eq!(
            err,
            "mount: /Storage/disc: WARNING: source write-protected, mounted read-only.\n"
        );
        let (_, _, err) = ran(&["-r", "disc.iso"], &mounter, &fixture);
        assert_eq!(err, "", "asked for, so nothing to warn of");
        assert!(
            fixture.seen.borrow().is_empty(),
            "the kernel's answer says so"
        );
    }

    #[test]
    fn w_mounts_read_write_or_fails() {
        let mounter = MemMounter::answering(Err(Errno::ReadOnly));
        let (outcome, out, err) = ran(&["-w", "disc.iso"], &mounter, &Fixture::empty());
        let failed = outcome.expect_err("write-protected");
        assert_eq!(
            failed.to_string(),
            "disc.iso is write-protected but explicit read-write mode requested."
        );
        assert_eq!(failed.status(), 32);
        assert_eq!((out.as_str(), err.as_str()), ("", ""));
        assert_eq!(mounter.asked.borrow()[0].access, ImageAccess::Writable);
        let (outcome, ..) = ran(&["disc.iso"], &mounter, &Fixture::empty());
        assert_eq!(
            outcome,
            Err(MountError::Mount {
                image: String::from("disc.iso"),
                errno: Errno::ReadOnly,
            }),
            "unforced, a refusal is the kernel's"
        );
    }

    #[test]
    fn a_refused_image_says_why() {
        let mounter =
            MemMounter::answering(Ok(ImageAttachReply::Refused(ImageRefusal::Partitioned)));
        let (outcome, ..) = ran(&["work.hdf"], &mounter, &Fixture::empty());
        let refused = outcome.expect_err("refused");
        assert_eq!(
            refused.to_string(),
            "work.hdf: it holds a partitioned disk, whose partitions are not mounted."
        );
        assert_eq!(refused.status(), 32);
    }

    #[test]
    fn an_image_mounted_already_is_a_failure_naming_where_it_was_asked_to_go_and_is() {
        let mounter = MemMounter::answering(Ok(ImageAttachReply::AlreadyAttached(volume(
            "workbench",
            false,
        ))));
        for (args, asked) in [
            (&["wb.adf"][..], "/Storage/workbench"),
            (&["wb.adf", "/Users/ada/Home/wb"], "/Users/ada/Home/wb"),
            (&["wb.adf", "/Storage/work"], "/Storage/work"),
        ] {
            let (outcome, ..) = ran(args, &mounter, &Fixture::empty());
            let already = outcome.expect_err("already mounted");
            assert_eq!(
                already.to_string(),
                alloc::format!("{asked}: wb.adf already mounted on /Storage/workbench."),
                "{args:?}"
            );
            assert_eq!(already.status(), 32);
        }
    }

    #[test]
    fn a_kernel_refusal_is_the_mount_s_failure() {
        let mounter = MemMounter::answering(Err(Errno::Busy));
        let (outcome, ..) = ran(&["wb.adf"], &mounter, &Fixture::empty());
        assert_eq!(
            outcome,
            Err(MountError::Mount {
                image: String::from("wb.adf"),
                errno: Errno::Busy,
            })
        );
    }

    #[test]
    fn a_terminal_that_fails_says_so() {
        let mounter = MemMounter::attaching("workbench");
        let fixture = Fixture::empty();
        let command = parse(&["-v", "wb.adf"]).expect("valid");
        assert_eq!(
            run(
                command,
                None,
                &mounter,
                &fixture,
                &NoHelp,
                &FailingOutput,
                &FailingOutput
            ),
            Err(MountError::Output(Errno::PermissionDenied))
        );
    }

    /// A stick holding writes awaiting an answer, published as `[3; 16]` and
    /// shown at `/Storage/usb1`, beside an image holding none.
    fn held_fixture() -> Fixture {
        let held = MountRecord::new(
            b"usb1",
            b"/Storage/usb1",
            b"fat32",
            MountFlags::READ_ONLY,
            MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::ReplayUnconfirmed,
                medium: None,
            },
            [3; 16],
        )
        .expect("record");
        Fixture::new(
            alloc::vec![
                record(b"wb.adf", b"/Storage/workbench", MountFlags::NOEXEC, 2),
                held,
            ],
            alloc::vec![
                view(b"/Storage/workbench", b"/", MountFlags::NOEXEC, 2),
                view(b"/Storage/usb1", b"/", MountFlags::READ_ONLY, 3),
            ],
        )
    }

    #[test]
    fn the_held_listing_shows_only_the_volumes_holding_writes() {
        let mounter = MemMounter::attaching("x");
        let (outcome, out, _) = ran(&["--list-held"], &mounter, &held_fixture());
        assert_eq!(outcome, Ok(()));
        assert_eq!(
            out,
            "usb1 on /Storage/usb1 type fat32 (ro) [replay-unconfirmed]\n"
        );
        assert!(mounter.asked.borrow().is_empty(), "listing mounts nothing");
    }

    /// An answer hands the kernel the named volume's key and what becomes of
    /// its writes, silently, the volume named as `unmount` names one.
    #[test]
    fn a_held_answer_hands_the_kernel_the_volumes_key() {
        let mounter = MemMounter::replaying(Ok(()));
        let fixture = held_fixture();
        for (args, answer) in [
            (&["--replay-held", "usb1"][..], ReplayAnswer::Write),
            (&["--discard-held", "/Storage/usb1"], ReplayAnswer::Discard),
        ] {
            let (outcome, out, err) = ran(args, &mounter, &fixture);
            assert_eq!(outcome, Ok(()), "{args:?}");
            assert_eq!((out.as_str(), err.as_str()), ("", ""), "silent on success");
            assert_eq!(
                mounter.replayed.borrow().last(),
                Some(&VolumeReplayRequest {
                    volume_id: [3; 16],
                    answer,
                })
            );
        }
        let (_, out, _) = ran(&["-v", "--replay-held", "usb1"], &mounter, &fixture);
        assert_eq!(out, "mount: usb1: held writes written.\n");
        assert!(mounter.asked.borrow().is_empty(), "nothing was mounted");
    }

    /// A name naming no volume, a volume holding no writes and the kernel's
    /// refusal each say why, exiting as util-linux does: `1` for an answer the
    /// caller may not give, `32` for one not carried out.
    #[test]
    fn a_held_answer_not_carried_out_says_why() {
        let fixture = held_fixture();
        let taking = MemMounter::replaying(Ok(()));
        let unknown = ran(&["--replay-held", "usb9"], &taking, &fixture)
            .0
            .expect_err("refused");
        assert_eq!(unknown.to_string(), "usb9: not mounted.");
        assert_eq!(unknown.status(), 32);
        let healthy = ran(&["--discard-held", "workbench"], &taking, &fixture)
            .0
            .expect_err("refused");
        assert_eq!(
            healthy.to_string(),
            "workbench: holds no writes awaiting an answer."
        );
        assert_eq!(healthy.status(), 32);
        assert!(
            taking.replayed.borrow().is_empty(),
            "the kernel is asked nothing"
        );
        let denied = MemMounter::replaying(Err(Errno::PermissionDenied));
        let refused = ran(&["--discard-held", "usb1"], &denied, &fixture)
            .0
            .expect_err("refused");
        assert_eq!(refused.status(), 1, "an answer the caller may not give");
        assert!(
            !refused.misused(),
            "a sentence alone, with no hint to --help"
        );
        assert_eq!(
            refused.to_string(),
            alloc::format!(
                "usb1: held writes not discarded: {}.",
                Errno::PermissionDenied
            )
        );
        let kept = MemMounter::replaying(Err(Errno::ReadOnly));
        let changed = ran(&["--replay-held", "usb1"], &kept, &fixture)
            .0
            .expect_err("refused");
        assert_eq!(changed.status(), 32);
        assert_eq!(
            changed.to_string(),
            "usb1: the disk no longer matches the one the writes were made on; they are kept, \
             and the volume is read-only."
        );
    }

    /// Every locale's `OPTIONS` documents exactly the switches this parser
    /// accepts, read from the bundle's own `Help/` tree — the one copy the
    /// image builder plants.
    #[test]
    fn help_documents_the_parser_switches() {
        extern crate std;
        use std::fs;

        let help_root = alloc::format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = alloc::format!("{help_root}/{locale}/mount.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-r, --read-only`",
                "`-w, --rw, --read-write`",
                "`-t, --types <list>`",
                "`-o, --options <list>`",
                "`-v, --verbose`",
                "`-l, --show-labels`",
                "`--list-held`",
                "`--replay-held <name>`",
                "`--discard-held <name>`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/mount.md must document {switch}"
                );
            }
        }
    }
}
