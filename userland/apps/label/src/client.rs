//! The `label` engine: resolve the named volume through the System
//! Information API's mount and view listings, then show its title or hand its
//! key and the new title to the kernel's retitle path.

use alloc::format;
use alloc::string::String;

use tairix_abi::driver::filesystem::VolumeTitle;
use tairix_abi::sysinfo::{MountRecord, VOLUME_ID_UNPUBLISHED};
use tairix_abi::Errno;
use tairix_help::{own_short_help, HelpSource};
use tairix_procinfo::{named_volume, Namespace, Transport, Unnamed};
use tairix_util::quote::{output_style, Quoted};

use crate::command::Command;
use crate::error::LabelError;
use crate::io::{Output, Retitler};

/// The one-line usage banner, the short help's fallback when the bundled
/// help document is unavailable.
pub const USAGE: &str = "usage: label [option...] [--] NAME [TITLE]";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "label";

/// Run a parsed `label` command against the injected seams. A title shown is
/// written on its own line, as a name from elsewhere is written to standard
/// output; a title set writes nothing, as `e2label` does.
///
/// # Errors
///
/// [`LabelError`]: a name naming no volume, a title that is none, a volume
/// whose title is withheld or that publishes no key, the kernel's refusal, or
/// a listing or stream that failed.
pub fn run(
    command: Command,
    locale: Option<&str>,
    transport: &dyn Transport,
    retitler: &dyn Retitler,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), LabelError> {
    match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            out.write_all(&bytes).map_err(LabelError::Output)
        }
        Command::Version => {
            let line = format!("{OWN_WORD} (TAIRiX) {}\n", env!("CARGO_PKG_VERSION"));
            out.write_all(line.as_bytes()).map_err(LabelError::Output)
        }
        Command::Show { name } => {
            let namespace = Namespace::read(transport)?;
            let volume = resolve(&namespace, &name, retitler)?;
            if volume.is_withheld() {
                return Err(LabelError::Withheld(name));
            }
            let title = volume.title();
            let shown = Quoted::new(title.as_str(), output_style(out.at_terminal()));
            out.write_all(format!("{shown}\n").as_bytes())
                .map_err(LabelError::Output)
        }
        Command::Set { name, title } => retitle(name, title, transport, retitler),
    }
}

/// Title the volume `name` names `title`, the title checked before anything
/// is asked.
fn retitle(
    name: String,
    title: String,
    transport: &dyn Transport,
    retitler: &dyn Retitler,
) -> Result<(), LabelError> {
    let checked = match VolumeTitle::new(&title) {
        Ok(checked) => checked,
        Err(Errno::LengthOutOfRange) => return Err(LabelError::TitleTooLong(title)),
        Err(_) => return Err(LabelError::TitleControl(title)),
    };
    let namespace = Namespace::read(transport)?;
    let volume_id = resolve(&namespace, &name, retitler)?.volume_id();
    if volume_id == VOLUME_ID_UNPUBLISHED {
        return Err(LabelError::NotTitleable(name));
    }
    retitler
        .retitle(volume_id, &checked)
        .map_err(|errno| LabelError::Retitle { name, errno })
}

/// The volume `name` names in `namespace`, by the rules every tool naming a
/// volume shares ([`named_volume`]).
fn resolve<'n>(
    namespace: &'n Namespace,
    name: &str,
    retitler: &dyn Retitler,
) -> Result<&'n MountRecord, LabelError> {
    named_volume(namespace, name, retitler).map_err(|why| match why {
        Unnamed::NoVolume => LabelError::NotTitleable(String::from(name)),
        why => LabelError::Unresolved {
            name: String::from(name),
            why,
        },
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{run, USAGE};
    use crate::command::parse;
    use crate::error::LabelError;
    use crate::io::{Output, Retitler};
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats, VolumeTitle};
    use tairix_abi::sysinfo::{
        MountAvailability, MountRecord, MountVolumeState, PageRequest, SysinfoQueryId,
        SysinfoRequestHeader, ViewBindingRecord,
    };
    use tairix_abi::Errno;
    use tairix_help::{HelpSource, SourceError};
    use tairix_procinfo::{Caller, Transport, Unnamed};

    /// An in-memory `sysinfod` stand-in answering the mount and view lists,
    /// each volume shown from its root at its target, beside the catalog,
    /// which shows none.
    struct Fixture {
        records: Vec<MountRecord>,
        views: Vec<ViewBindingRecord>,
        fail: Option<Errno>,
    }

    impl Fixture {
        fn of(records: Vec<MountRecord>) -> Self {
            let mut views: Vec<ViewBindingRecord> = records
                .iter()
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
            views.push(
                ViewBindingRecord::new(b"/Storage", b"", MountFlags::NOEXEC, [0; 16])
                    .expect("the catalog"),
            );
            Self {
                records,
                views,
                fail: None,
            }
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

    /// The user these tests run as.
    const VIEWER: u32 = 1000;

    /// Records every retitle it is asked, answering `answer`, and resolves an
    /// absolute path as itself.
    struct MemRetitler {
        answer: Result<(), Errno>,
        asked: RefCell<Vec<([u8; 16], String)>>,
    }

    impl MemRetitler {
        fn answering(answer: Result<(), Errno>) -> Self {
            Self {
                answer,
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Caller for MemRetitler {
        fn user(&self) -> Option<u32> {
            Some(VIEWER)
        }

        fn canonical(&self, path: &str) -> Result<String, Errno> {
            if path.starts_with('/') {
                Ok(String::from(path))
            } else {
                Err(Errno::NotFound)
            }
        }
    }

    impl Retitler for MemRetitler {
        fn retitle(&self, volume_id: [u8; 16], title: &VolumeTitle) -> Result<(), Errno> {
            self.asked
                .borrow_mut()
                .push((volume_id, String::from(title.as_str())));
            self.answer
        }
    }

    /// Captures what is written.
    struct Recorder(RefCell<Vec<u8>>);

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(())
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

    /// The volume `source` names, shown at `target`, published as `id`.
    fn record(source: &[u8], target: &[u8], id: u8) -> MountRecord {
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

    /// A stick shown as `/Storage/<name>`, titled `title`, published as `id`.
    fn stick(name: &str, title: &str, id: u8) -> MountRecord {
        let target = alloc::format!("/Storage/{name}");
        record(name.as_bytes(), target.as_bytes(), id)
            .with_title(VolumeTitle::new(title).expect("a title"))
            .with_titled(true)
    }

    /// Two sticks, one titled, and the boot volume, published under no key.
    fn table() -> Fixture {
        Fixture::of(alloc::vec![
            stick("usb1", "HOLIDAY", 1),
            stick("usb2", "", 2),
            record(b"ARXFSRoot", b"/", 0),
        ])
    }

    /// Run `args` against `fixture` and `retitler`, answering the outcome and
    /// what was written.
    fn run_args(
        args: &[&str],
        fixture: &Fixture,
        retitler: &MemRetitler,
    ) -> (Result<(), LabelError>, String) {
        let out = Recorder(RefCell::new(Vec::new()));
        let outcome = run(
            parse(args).expect("a valid command"),
            None,
            fixture,
            retitler,
            &NoHelp,
            &out,
        );
        (
            outcome,
            String::from_utf8(out.0.into_inner()).expect("text"),
        )
    }

    /// The title is shown on its own line, and a volume titled nothing shows an
    /// empty one; nothing is asked of the kernel.
    #[test]
    fn a_title_is_shown_on_its_own_line() {
        let retitler = MemRetitler::answering(Ok(()));
        assert_eq!(
            run_args(&["usb1"], &table(), &retitler),
            (Ok(()), String::from("HOLIDAY\n"))
        );
        assert_eq!(
            run_args(&["/Storage/usb2"], &table(), &retitler),
            (Ok(()), String::from("\n"))
        );
        assert!(retitler.asked.borrow().is_empty());
    }

    /// A title is handed to the kernel with the key of the volume named — by
    /// its catalog name, or by its durable root as the file manager's request
    /// to an administrator names it — writing nothing, and the empty title
    /// clears it.
    #[test]
    fn a_title_is_handed_to_the_kernel_with_the_volumes_key() {
        let retitler = MemRetitler::answering(Ok(()));
        assert_eq!(
            run_args(&["usb1", "SUMMER"], &table(), &retitler),
            (Ok(()), String::new())
        );
        let root = alloc::format!("id::{}/", tairix_path::VolumeId::from_bytes([2u8; 16]));
        let cleared = run_args(&["--", &root, ""], &table(), &retitler);
        assert_eq!(cleared, (Ok(()), String::new()));
        assert_eq!(
            *retitler.asked.borrow(),
            [([1; 16], String::from("SUMMER")), ([2; 16], String::new())]
        );
    }

    /// A title no volume could keep is refused before anything is asked.
    #[test]
    fn a_title_that_is_none_is_refused_before_anything_is_asked() {
        let retitler = MemRetitler::answering(Ok(()));
        let long = "X".repeat(253);
        assert_eq!(
            run_args(&["usb1", &long], &table(), &retitler).0,
            Err(LabelError::TitleTooLong(long.clone()))
        );
        assert_eq!(
            run_args(&["usb1", "TAB\tBED"], &table(), &retitler).0,
            Err(LabelError::TitleControl(String::from("TAB\tBED")))
        );
        assert!(retitler.asked.borrow().is_empty());
    }

    /// A name naming no volume says why, as `unmount` says it; the catalog and
    /// a volume published under no key take no title.
    #[test]
    fn a_name_naming_no_volume_a_title_belongs_to_says_why() {
        let retitler = MemRetitler::answering(Ok(()));
        let err = run_args(&["usb9", "X"], &table(), &retitler)
            .0
            .expect_err("refused");
        let expected = LabelError::Unresolved {
            name: String::from("usb9"),
            why: Unnamed::NotFound,
        };
        assert_eq!(err, expected);
        assert_eq!(alloc::format!("{err}"), "usb9: not mounted.");
        assert_eq!(err.status(), 1);
        for name in ["/Storage", "ARXFSRoot"] {
            assert_eq!(
                run_args(&[name, "X"], &table(), &retitler).0,
                Err(LabelError::NotTitleable(String::from(name))),
                "{name}"
            );
        }
        assert!(retitler.asked.borrow().is_empty());
    }

    /// The kernel's refusal is reported as its own, worded for the volume.
    #[test]
    fn the_kernels_refusal_is_reported_as_its_own() {
        for (errno, said) in [
            (Errno::PermissionDenied, "usb1: permission denied."),
            (Errno::NotSupported, "usb1: the volume takes no new title."),
            (Errno::ReadOnly, "usb1: the volume is read-only."),
            (
                Errno::OutOfRange,
                "usb1: the volume's format cannot spell the title.",
            ),
        ] {
            let retitler = MemRetitler::answering(Err(errno));
            let err = run_args(&["usb1", "holiday"], &table(), &retitler)
                .0
                .expect_err("refused");
            let expected = LabelError::Retitle {
                name: String::from("usb1"),
                errno,
            };
            assert_eq!(err, expected);
            assert_eq!(alloc::format!("{err}"), said);
            assert_eq!(err.status(), 1);
        }
    }

    /// Another user's private volume shows its catalog entry but not its title.
    #[test]
    fn a_withheld_title_is_not_shown() {
        let fixture = Fixture::of(alloc::vec![stick("theirs", "SECRET", 3).withheld()]);
        let retitler = MemRetitler::answering(Ok(()));
        let (outcome, written) = run_args(&["theirs"], &fixture, &retitler);
        assert_eq!(outcome, Err(LabelError::Withheld(String::from("theirs"))));
        assert!(written.is_empty(), "nothing is shown");
    }

    #[test]
    fn help_falls_back_to_the_usage_and_version_names_the_tool() {
        let retitler = MemRetitler::answering(Ok(()));
        let (outcome, help) = run_args(&["--help"], &table(), &retitler);
        assert_eq!(outcome, Ok(()));
        assert!(help.contains(USAGE));
        let (outcome, version) = run_args(&["--version"], &table(), &retitler);
        assert_eq!(outcome, Ok(()));
        assert_eq!(
            version,
            alloc::format!("label (TAIRiX) {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_listing_failure_surfaces_as_service() {
        let mut fixture = table();
        fixture.fail = Some(Errno::NotFound);
        let retitler = MemRetitler::answering(Ok(()));
        let err = run_args(&["usb1"], &fixture, &retitler)
            .0
            .expect_err("failed");
        assert_eq!(err, LabelError::Service(Errno::NotFound));
        assert_eq!(err.status(), 2);
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts, read from the bundle's own on-disk `Help/` tree — the
    /// single source the image builder plants.
    #[test]
    fn help_documents_the_parser_switches() {
        use std::fs;

        let help_root = alloc::format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = alloc::format!("{help_root}/{locale}/label.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-h, -?, --help`", "`--version`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/label.md must document {switch}"
                );
            }
        }
    }
}
