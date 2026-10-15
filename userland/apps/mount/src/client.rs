//! The mount engine: list the mount table through the System Information
//! API, or hand a parsed attach request to the kernel.

use tairix_procinfo::{render_binding, Namespace, Output, Transport};

use crate::command::Command;
use crate::error::MountError;
use crate::io::{MountSpec, Mounter};

/// The usage banner printed by [`Command::Help`].
pub const USAGE: &str = "\
usage: mount [-r] [-t TYPE] [-o OPTIONS] [--] [SOURCE TARGET]

  (no operands)        list the mounted filesystems
  SOURCE TARGET        mount SOURCE at TARGET (needs CAP_FS_MOUNT)
  -r, --read-only      mount read-only (same as -o ro)
  -t, --types TYPE     filesystem type (probed when omitted)
  -o, --options LIST   comma-separated: ro,rw,noexec
  -h, --help           show this message

`--` ends option parsing: every later argument is an operand.";

/// Run one [`Command`].
///
/// A [`Command::List`] reads the attached volumes and the view bindings onto
/// them through `transport` (the ungated `MOUNT_LIST` and `VIEW_LIST`
/// queries) and writes one line per binding to `out`. A [`Command::Mount`] hands the parsed request to
/// `mounter`; the capability gate lives in the kernel, not here, so a denied mount comes back as
/// [`MountError::Mount`]. `mount` writes nothing on a successful attach.
///
/// # Errors
///
/// * [`MountError::Mount`] — the kernel refused or failed the attach (e.g. a
///   missing `CAP_FS_MOUNT`, an unknown source, or a bad superblock).
/// * [`MountError::Service`] — the transport failed or the reply did not
///   decode against `sysinfo-v1` while listing.
/// * [`MountError::Output`] — writing the terminal failed.
pub fn run(
    command: Command,
    mounter: &dyn Mounter,
    transport: &dyn Transport,
    out: &dyn Output,
) -> Result<(), MountError> {
    match command {
        Command::Help => out.write_line(USAGE).map_err(MountError::Output),
        Command::List => run_list(transport, out),
        Command::Mount(request) => {
            let spec = MountSpec {
                source: &request.source,
                target: &request.target,
                fstype: request.fstype.as_deref(),
                flags: request.flags,
            };
            mounter.mount(&spec).map_err(MountError::Mount)
        }
    }
}

/// Read the namespace and render one line per view binding, from the volume
/// each shows.
fn run_list(transport: &dyn Transport, out: &dyn Output) -> Result<(), MountError> {
    let namespace = Namespace::read(transport)?;
    for view in namespace.views() {
        out.write_line(&render_binding(view, namespace.volume_of(view)))
            .map_err(MountError::Output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run, USAGE};
    use crate::command::{parse, Command};
    use crate::error::MountError;
    use crate::io::{MountSpec, Mounter};
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::sysinfo::{
        MountAvailability, MountRecord, MountVolumeState, PageRequest, SysinfoQueryId,
        SysinfoRequestHeader, ViewBindingRecord,
    };
    use tairix_abi::Errno;
    use tairix_procinfo::{Output, Transport};

    /// An in-memory `sysinfod` stand-in answering the mount and view lists
    /// from a fixture, decoding the request the same way the real service
    /// does.
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

    /// Captures rendered lines.
    struct Recorder {
        lines: RefCell<Vec<String>>,
    }
    impl Recorder {
        fn new() -> Self {
            Self {
                lines: RefCell::new(Vec::new()),
            }
        }
        fn lines(&self) -> Vec<String> {
            self.lines.borrow().clone()
        }
    }
    impl Output for Recorder {
        fn write_line(&self, line: &str) -> Result<(), Errno> {
            self.lines.borrow_mut().push(line.to_string());
            Ok(())
        }
    }

    /// An output seam that always fails.
    struct FailingOutput;
    impl Output for FailingOutput {
        fn write_line(&self, _line: &str) -> Result<(), Errno> {
            Err(Errno::PermissionDenied)
        }
    }

    /// One recorded attach request.
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Attached {
        source: String,
        target: String,
        fstype: Option<String>,
        flags: MountFlags,
    }

    /// Records every attach request, optionally failing it.
    struct MemMounter {
        fail: Option<Errno>,
        attached: RefCell<Vec<Attached>>,
    }
    impl MemMounter {
        fn new() -> Self {
            Self {
                fail: None,
                attached: RefCell::new(Vec::new()),
            }
        }
        fn failing(errno: Errno) -> Self {
            Self {
                fail: Some(errno),
                attached: RefCell::new(Vec::new()),
            }
        }
    }
    impl Mounter for MemMounter {
        fn mount(&self, spec: &MountSpec<'_>) -> Result<(), Errno> {
            if let Some(errno) = self.fail {
                return Err(errno);
            }
            self.attached.borrow_mut().push(Attached {
                source: spec.source.to_string(),
                target: spec.target.to_string(),
                fstype: spec.fstype.map(String::from),
                flags: spec.flags,
            });
            Ok(())
        }
    }

    fn record(
        source: &[u8],
        target: &[u8],
        fstype: &[u8],
        flags: MountFlags,
        id: u8,
    ) -> MountRecord {
        MountRecord::new(
            source,
            target,
            fstype,
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

    #[test]
    fn help_prints_usage_and_touches_no_query() {
        let mounter = MemMounter::new();
        let fixture = Fixture::new(Vec::new(), Vec::new());
        let out = Recorder::new();
        assert_eq!(run(Command::Help, &mounter, &fixture, &out), Ok(()));
        assert_eq!(out.lines(), alloc::vec![USAGE.to_string()]);
        assert!(fixture.seen.borrow().is_empty());
    }

    #[test]
    fn list_renders_one_line_per_binding_and_routes_the_queries() {
        let mounter = MemMounter::new();
        let fixture = Fixture::new(
            alloc::vec![
                record(b"rootfs", b"/", b"arxfs", MountFlags::READ_ONLY, 1),
                record(b"data", b"/Storage/data", b"arxfs", MountFlags::NOEXEC, 2),
            ],
            alloc::vec![
                view(b"/", b"/", MountFlags::READ_ONLY, 1),
                view(b"/System/Logs", b"/System/Logs", MountFlags::READ_ONLY, 1),
                view(b"/Storage/data", b"/", MountFlags::NOEXEC, 2),
            ],
        );
        let out = Recorder::new();
        assert_eq!(run(Command::List, &mounter, &fixture, &out), Ok(()));
        assert_eq!(
            out.lines(),
            [
                "rootfs on / type arxfs (ro)",
                "rootfs[/System/Logs] on /System/Logs type arxfs (ro)",
                "data on /Storage/data type arxfs (rw,noexec)",
            ]
        );
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::MOUNT_LIST, SysinfoQueryId::VIEW_LIST]
        );
        // Listing attaches nothing.
        assert!(mounter.attached.borrow().is_empty());
    }

    #[test]
    fn empty_table_lists_nothing() {
        let mounter = MemMounter::new();
        let fixture = Fixture::new(Vec::new(), Vec::new());
        let out = Recorder::new();
        assert_eq!(run(Command::List, &mounter, &fixture, &out), Ok(()));
        assert!(out.lines().is_empty());
    }

    #[test]
    fn a_list_transport_error_surfaces_as_service() {
        let mounter = MemMounter::new();
        let mut fixture = Fixture::new(Vec::new(), Vec::new());
        fixture.fail = Some(Errno::NotFound);
        let out = Recorder::new();
        assert_eq!(
            run(Command::List, &mounter, &fixture, &out),
            Err(MountError::Service(Errno::NotFound))
        );
    }

    #[test]
    fn a_list_output_failure_surfaces() {
        let mounter = MemMounter::new();
        let fixture = Fixture::new(
            alloc::vec![record(b"rootfs", b"/", b"arxfs", MountFlags::default(), 1)],
            alloc::vec![view(b"/", b"/", MountFlags::default(), 1)],
        );
        assert_eq!(
            run(Command::List, &mounter, &fixture, &FailingOutput),
            Err(MountError::Output(Errno::PermissionDenied))
        );
    }

    fn run_mount(args: &[&str], mounter: &MemMounter) -> Result<(), MountError> {
        let fixture = Fixture::new(Vec::new(), Vec::new());
        let out = Recorder::new();
        run(parse(args).expect("valid command"), mounter, &fixture, &out)
    }

    #[test]
    fn a_mount_request_reaches_the_kernel() {
        let mounter = MemMounter::new();
        assert_eq!(
            run_mount(&["-r", "-t", "arxfs", "vol", "/Storage/vol"], &mounter),
            Ok(())
        );
        let attached = mounter.attached.borrow();
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].source, "vol");
        assert_eq!(attached[0].target, "/Storage/vol");
        assert_eq!(attached[0].fstype.as_deref(), Some("arxfs"));
        assert!(attached[0].flags.contains(MountFlags::READ_ONLY));
    }

    #[test]
    fn a_denied_mount_surfaces_as_mount_permission_denied() {
        // The kernel is the policy point: a missing CAP_FS_MOUNT is its call
        // to make, surfaced here as Mount(PermissionDenied).
        let mounter = MemMounter::failing(Errno::PermissionDenied);
        assert_eq!(
            run_mount(&["vol", "/Storage/vol"], &mounter),
            Err(MountError::Mount(Errno::PermissionDenied))
        );
        assert!(mounter.attached.borrow().is_empty());
    }

    #[test]
    fn a_mount_with_no_fstype_passes_none() {
        let mounter = MemMounter::new();
        assert_eq!(run_mount(&["vol", "/Storage/vol"], &mounter), Ok(()));
        assert_eq!(mounter.attached.borrow()[0].fstype, None);
    }

    #[test]
    fn a_help_write_failure_surfaces() {
        let mounter = MemMounter::new();
        let fixture = Fixture::new(Vec::new(), Vec::new());
        assert_eq!(
            run(Command::Help, &mounter, &fixture, &FailingOutput),
            Err(MountError::Output(Errno::PermissionDenied))
        );
    }
}
