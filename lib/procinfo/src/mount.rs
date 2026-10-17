//! The shared mount-list and view-list walks, the namespace they join into,
//! and the `mount` row rendering.
//!
//! The service lists attached volumes ([`MountRecord`]) apart from the view
//! paths that show them ([`ViewBindingRecord`]). [`Namespace`] joins the two,
//! so every consumer resolves a path to the binding covering it and that
//! binding to its volume through one definition.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::sysinfo::{
    MountRecord, SysinfoQueryId, ViewBindingRecord, VolumeIoStatsRecord, VOLUME_ID_UNPUBLISHED,
};
use tairix_abi::Errno;

use crate::list::{field_lossy, walk_records, ListError, WalkStep};
use crate::transport::Transport;
use crate::volume::availability_marker;

/// Number of [`MountRecord`]s requested per mount-list page: the most one
/// reply holds, since a page that does not fit is refused whole.
pub const MOUNT_PAGE: u16 = tairix_abi::reply_page(MountRecord::WIRE_LEN);

/// Number of [`ViewBindingRecord`]s requested per view-list page, sized as
/// [`MOUNT_PAGE`] is.
pub const VIEW_PAGE: u16 = tairix_abi::reply_page(ViewBindingRecord::WIRE_LEN);

/// Number of [`VolumeIoStatsRecord`]s requested per page of the service
/// counters, sized as [`MOUNT_PAGE`] is.
pub const VOLUME_SERVICE_PAGE: u16 = tairix_abi::reply_page(VolumeIoStatsRecord::WIRE_LEN);

/// Page through the attached volumes and hand each decoded [`MountRecord`]
/// to `sink`, in the order the service lists them.
///
/// The query is [`SysinfoQueryId::MOUNT_LIST`], which the service serves
/// ungated: the mount table is system-wide and secret-free.
///
/// `sink` answers [`WalkStep::Continue`] to be given the next record or
/// [`WalkStep::Stop`] to end the walk there, which is how a caller bounds
/// how much of a long or hostile list it will accept. Stopping is an
/// ordinary success, so it stays distinguishable from a failure.
///
/// The walk **fails closed**: a reply whose length is not a whole number of
/// [`MountRecord::WIRE_LEN`] records, or one that would overflow the page
/// offset, is rejected rather than partially rendered.
///
/// # Errors
///
/// * [`ListError::Call`] — the transport failed, the service denied the
///   query, or the reply was structurally invalid.
/// * [`ListError::Sink`] — `sink` returned an error for some record; the
///   walk stops at that record.
pub fn for_each_mount(
    transport: &dyn Transport,
    sink: impl FnMut(&MountRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::MOUNT_LIST,
        MountRecord::WIRE_LEN,
        MOUNT_PAGE,
        MountRecord::from_bytes,
        sink,
    )
}

/// Page through each block-backed volume's cumulative service counters
/// ([`SysinfoQueryId::VOLUME_IO_STATS`], ungated) and hand each to `sink`, on
/// the terms of [`for_each_mount`]. A record joins its mount by volume id.
///
/// # Errors
///
/// As [`for_each_mount`].
pub fn for_each_volume_service(
    transport: &dyn Transport,
    sink: impl FnMut(&VolumeIoStatsRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::VOLUME_IO_STATS,
        VolumeIoStatsRecord::WIRE_LEN,
        VOLUME_SERVICE_PAGE,
        VolumeIoStatsRecord::from_bytes,
        sink,
    )
}

/// Page through the view bindings ([`SysinfoQueryId::VIEW_LIST`]) and hand
/// each to `sink`, on the terms of [`for_each_mount`].
///
/// # Errors
///
/// As [`for_each_mount`].
pub fn for_each_view(
    transport: &dyn Transport,
    sink: impl FnMut(&ViewBindingRecord) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::VIEW_LIST,
        ViewBindingRecord::WIRE_LEN,
        VIEW_PAGE,
        ViewBindingRecord::from_bytes,
        sink,
    )
}

/// The attached volumes and the view bindings onto them, read together so a
/// path resolves to the binding covering it and that binding to its volume.
#[derive(Clone, Debug, Default)]
pub struct Namespace {
    volumes: Vec<MountRecord>,
    views: Vec<ViewBindingRecord>,
    /// Indices of the volumes that published an identity, sorted by it.
    by_id: Vec<usize>,
}

impl Namespace {
    /// Read the volume list, then the view list.
    ///
    /// They are two queries, so a volume attached or detached between them can
    /// leave a binding naming no listed volume; that binding then resolves to
    /// no volume rather than to a guess.
    ///
    /// # Errors
    ///
    /// As [`for_each_mount`].
    pub fn read(transport: &dyn Transport) -> Result<Self, ListError> {
        let mut volumes = Vec::new();
        for_each_mount(transport, |volume| {
            volumes.push(*volume);
            Ok(WalkStep::Continue)
        })?;
        let mut views = Vec::new();
        for_each_view(transport, |view| {
            views.push(*view);
            Ok(WalkStep::Continue)
        })?;
        Ok(Self::new(volumes, views))
    }

    /// The namespace `volumes` and `views` describe, each in service order.
    #[must_use]
    pub fn new(volumes: Vec<MountRecord>, views: Vec<ViewBindingRecord>) -> Self {
        let mut by_id: Vec<usize> = (0..volumes.len())
            .filter(|&at| volumes[at].volume_id() != VOLUME_ID_UNPUBLISHED)
            .collect();
        by_id.sort_unstable_by_key(|&at| volumes[at].volume_id());
        Self {
            volumes,
            views,
            by_id,
        }
    }

    /// The attached volumes, in service order.
    #[must_use]
    pub fn volumes(&self) -> &[MountRecord] {
        &self.volumes
    }

    /// The view bindings, in mount-table order.
    #[must_use]
    pub fn views(&self) -> &[ViewBindingRecord] {
        &self.views
    }

    /// The volume `view` shows, or `None` when it shows none (the `/Storage`
    /// catalog) or one this namespace does not list.
    #[must_use]
    pub fn volume_of(&self, view: &ViewBindingRecord) -> Option<&MountRecord> {
        if !view.shows_volume() {
            return None;
        }
        let at = self
            .by_id
            .binary_search_by_key(&view.volume_id(), |&at| self.volumes[at].volume_id())
            .ok()?;
        Some(&self.volumes[self.by_id[at]])
    }

    /// The binding covering the absolute `path`: the longest view path that
    /// is `path` or one of its ancestors, compared a whole component at a
    /// time.
    #[must_use]
    pub fn covering(&self, path: &[u8]) -> Option<&ViewBindingRecord> {
        self.views
            .iter()
            .filter(|view| covers(view.target_bytes(), path))
            .max_by_key(|view| view.target_bytes().len())
    }

    /// The binding covering `path` and the volume it shows. `None` when the
    /// covering binding shows no listed volume: the volume above it does not
    /// answer for bytes that would not land on it.
    #[must_use]
    pub fn volume_at(&self, path: &[u8]) -> Option<(&ViewBindingRecord, &MountRecord)> {
        let view = self.covering(path)?;
        Some((view, self.volume_of(view)?))
    }
}

/// Whether the view path `target` is the absolute `path` or one of its
/// ancestors.
fn covers(target: &[u8], path: &[u8]) -> bool {
    if target == b"/" {
        return path.first() == Some(&b'/');
    }
    path.strip_prefix(target)
        .is_some_and(|rest| rest.first().is_none_or(|&byte| byte == b'/'))
}

/// Render one binding as a `source on target type fstype (options)` line,
/// the long-standing Unix `mount` listing shape, from its `volume`.
///
/// A binding of a subtree names it after the source, `source[/subtree]`, as
/// `findmnt` does, so a carved-out directory is not read as the volume's
/// root. A binding showing no volume (the `/Storage` catalog) reads `none`
/// for both.
///
/// A surprise-removed volume carries a trailing ` [unavailable-dirty]` /
/// ` [unavailable-lost]` marker, a re-inserted volume whose
/// non-mutation could not be proven carries ` [recovery-conflict]`
/// (`plans/DEVICES.md` D4c), and a live-but-unwell device carries
/// ` [degraded]` or ` [recovering]` from its reported I/O health
/// (`plans/FIX-IO.md` IO3) — additive, so a healthy volume's line is
/// byte-identical to the classic shape while an unwell, dead, or
/// conflicted one never looks healthy (`plans/DEVICES.md` D4b).
#[must_use]
pub fn render_binding(view: &ViewBindingRecord, volume: Option<&MountRecord>) -> String {
    let target = field_lossy(view.target_bytes());
    let options = render_options(view.flags());
    let Some(volume) = volume else {
        return format!("none on {target} type none ({options})");
    };
    let subtree = match view.subtree_bytes() {
        b"" | b"/" => String::new(),
        subtree => format!("[{}]", field_lossy(subtree)),
    };
    let marker = availability_marker(volume.availability())
        .map_or_else(String::new, |marker| format!(" [{marker}]"));
    format!(
        "{}{subtree} on {target} type {} ({options}){marker}",
        field_lossy(volume.source_bytes()),
        field_lossy(volume.fstype_bytes()),
    )
}

/// Render the mount-policy flags as a comma-separated option list.
///
/// The list always opens with `ro` or `rw`, then appends each restriction in
/// force, matching how a Unix `mount` listing reads.
#[must_use]
pub fn render_options(flags: MountFlags) -> String {
    let mut options = String::from(if flags.contains(MountFlags::READ_ONLY) {
        "ro"
    } else {
        "rw"
    });
    if flags.contains(MountFlags::NOEXEC) {
        options.push_str(",noexec");
    }
    options
}

#[cfg(test)]
mod tests {
    use super::{
        for_each_mount, for_each_volume_service, render_binding, render_options, Namespace,
        WalkStep, MOUNT_PAGE, VIEW_PAGE, VOLUME_SERVICE_PAGE,
    };
    use crate::list::ListError;
    use crate::request::CallError;
    use crate::transport::Transport;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::blkio::BlkMedium;
    use tairix_abi::blkio::{BlkDeviceName, BlkIoCounters};
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::sysinfo::{
        MountAvailability, MountRecord, MountVolumeState, PageRequest, SysinfoQueryId,
        SysinfoRequestHeader, ViewBindingRecord, VolumeIoStatsRecord,
    };
    use tairix_abi::Errno;

    /// An in-memory `sysinfod` stand-in answering the mount and view lists
    /// from fixed record sets, decoding each request exactly as the real
    /// service.
    struct Fixture {
        records: Vec<MountRecord>,
        views: Vec<ViewBindingRecord>,
        services: Vec<VolumeIoStatsRecord>,
        malformed: bool,
        seen: RefCell<Vec<SysinfoQueryId>>,
    }

    impl Fixture {
        fn new(records: Vec<MountRecord>) -> Self {
            Self {
                records,
                views: Vec::new(),
                services: Vec::new(),
                malformed: false,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    fn page_of<R>(
        records: &[R],
        request: &PageRequest,
        len: usize,
        encode: impl Fn(&R) -> Vec<u8>,
    ) -> Result<Vec<u8>, Errno> {
        let offset = request.offset as usize;
        if offset >= records.len() {
            return Ok(Vec::new());
        }
        let take = core::cmp::min(records.len() - offset, request.limit as usize);
        // As the service does: a page one reply cannot hold is refused.
        if take * len > tairix_abi::SYSINFO_REPLY_PAYLOAD_MAX {
            return Err(Errno::BufferTooSmall);
        }
        Ok(records[offset..offset + take]
            .iter()
            .flat_map(encode)
            .collect())
    }

    impl Transport for Fixture {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            let header = SysinfoRequestHeader::from_bytes(request)?;
            self.seen.borrow_mut().push(header.query);
            if self.malformed {
                return Ok(alloc::vec![0u8; MountRecord::WIRE_LEN + 1]);
            }
            let payload = &request[SysinfoRequestHeader::WIRE_LEN
                ..SysinfoRequestHeader::WIRE_LEN + header.payload_len as usize];
            let page = PageRequest::from_bytes(payload)?;
            match header.query {
                SysinfoQueryId::MOUNT_LIST => {
                    page_of(&self.records, &page, MountRecord::WIRE_LEN, |r| {
                        r.to_le_bytes().to_vec()
                    })
                }
                SysinfoQueryId::VIEW_LIST => {
                    page_of(&self.views, &page, ViewBindingRecord::WIRE_LEN, |r| {
                        r.to_le_bytes().to_vec()
                    })
                }
                SysinfoQueryId::VOLUME_IO_STATS => {
                    page_of(&self.services, &page, VolumeIoStatsRecord::WIRE_LEN, |r| {
                        r.to_le_bytes().to_vec()
                    })
                }
                _ => Err(Errno::NotImplemented),
            }
        }
    }

    fn volume(source: &[u8], target: &[u8], fstype: &[u8], id: u8) -> MountRecord {
        MountRecord::new(
            source,
            target,
            fstype,
            MountFlags::default(),
            MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::Available,
                medium: None,
            },
            [id; 16],
        )
        .expect("record")
    }

    fn record(source: &[u8], target: &[u8], fstype: &[u8], flags: MountFlags) -> MountRecord {
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
            [0u8; 16],
        )
        .expect("record")
    }

    fn view(target: &[u8], subtree: &[u8], flags: MountFlags, id: u8) -> ViewBindingRecord {
        ViewBindingRecord::new(target, subtree, flags, [id; 16]).expect("view")
    }

    /// The default install's namespace with a stick under `/Storage`: the
    /// root volume (1) at `/` and carved out at `/System/Logs`, the system
    /// volume (2) at `/System`, the catalog showing no volume, and the
    /// stick (3).
    fn installed() -> Namespace {
        Namespace::new(
            alloc::vec![
                volume(b"ARXFSRoot", b"/", b"arxfs", 1),
                volume(b"ARXFSSystem", b"/System", b"arxfs", 2),
                volume(b"usb1", b"/Storage/usb1", b"fat32", 3),
            ],
            alloc::vec![
                view(b"/", b"/", MountFlags::default(), 1),
                view(b"/System", b"/", MountFlags::READ_ONLY, 2),
                view(b"/System/Logs", b"/System/Logs", MountFlags::NOEXEC, 1),
                view(
                    b"/Storage",
                    b"",
                    MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
                    0
                ),
                view(b"/Storage/usb1", b"/", MountFlags::NOEXEC, 3),
            ],
        )
    }

    #[test]
    fn a_path_resolves_to_the_volume_its_covering_binding_shows() {
        let ns = installed();
        let source = |path: &[u8]| ns.volume_at(path).map(|(_, volume)| volume.source_bytes());
        assert_eq!(source(b"/System/Logs/boot"), Some(&b"ARXFSRoot"[..]));
        assert_eq!(source(b"/System/Logs"), Some(&b"ARXFSRoot"[..]));
        assert_eq!(
            source(b"/System/Commands/ls.app"),
            Some(&b"ARXFSSystem"[..])
        );
        assert_eq!(source(b"/Users/ada"), Some(&b"ARXFSRoot"[..]));
        assert_eq!(source(b"/Storage/usb1/x"), Some(&b"usb1"[..]));
        // A name sharing a prefix is not a descendant.
        assert_eq!(source(b"/Systemic"), Some(&b"ARXFSRoot"[..]));
        // The catalog stores nothing, so no volume answers for it — and the
        // volume above it does not answer in its place.
        assert_eq!(
            ns.covering(b"/Storage")
                .map(ViewBindingRecord::target_bytes),
            Some(&b"/Storage"[..])
        );
        assert_eq!(source(b"/Storage"), None);
        assert_eq!(source(b"/Storage/absent"), None);
        assert!(ns.covering(b"relative/path").is_none());
    }

    #[test]
    fn a_binding_of_an_unlisted_volume_resolves_to_none() {
        // The stick left between the two reads.
        let ns = Namespace::new(
            alloc::vec![volume(b"ARXFSRoot", b"/", b"arxfs", 1)],
            alloc::vec![
                view(b"/", b"/", MountFlags::default(), 1),
                view(b"/Storage/usb1", b"/", MountFlags::NOEXEC, 3),
            ],
        );
        assert!(ns.volume_at(b"/Storage/usb1/x").is_none());
        // A volume with no published identity joins to no binding either.
        let unpublished = Namespace::new(
            alloc::vec![volume(b"anon", b"/", b"arxfs", 0)],
            alloc::vec![view(b"/", b"/", MountFlags::default(), 0)],
        );
        assert!(unpublished.volume_at(b"/x").is_none());
    }

    #[test]
    fn read_walks_the_volumes_then_the_views() {
        let ns = installed();
        let mut fixture = Fixture::new(ns.volumes().to_vec());
        fixture.views = ns.views().to_vec();
        let read = Namespace::read(&fixture).expect("both lists");
        assert_eq!(read.volumes(), ns.volumes());
        assert_eq!(read.views(), ns.views());
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::MOUNT_LIST, SysinfoQueryId::VIEW_LIST]
        );
        assert!(
            usize::from(VIEW_PAGE) * ViewBindingRecord::WIRE_LEN
                <= tairix_abi::SYSINFO_REPLY_PAYLOAD_MAX
        );
    }

    #[test]
    fn the_storage_medium_survives_the_client_decode() {
        // The medium the service reported reaches the consumer through the
        // paging walk, so a caller renders the drive it is really on.
        let record = MountRecord::new(
            b"usb1",
            b"/Storage/usb1",
            b"fat32",
            MountFlags::NOEXEC,
            MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::Available,
                medium: Some(BlkMedium::UsbFlash),
            },
            [0u8; 16],
        )
        .expect("record");
        let fixture = Fixture::new(alloc::vec![record]);
        let seen = collect(&fixture).expect("walk");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].medium(), Some(BlkMedium::UsbFlash));
    }

    /// A machine with more mounts than one reply holds still lists them all:
    /// each page asks for no more than a reply carries, where a page sized
    /// past it is refused whole and the walk would fail.
    #[test]
    fn a_mount_table_longer_than_one_reply_is_walked_whole() {
        let records: Vec<MountRecord> = (0..100)
            .map(|_| record(b"vol", b"/Storage/vol", b"arxfs", MountFlags::NOEXEC))
            .collect();
        let fixture = Fixture::new(records);
        assert_eq!(collect(&fixture).expect("every page answered").len(), 100);
        assert!(
            usize::from(MOUNT_PAGE) * MountRecord::WIRE_LEN
                <= tairix_abi::SYSINFO_REPLY_PAYLOAD_MAX
        );
    }

    fn collect(fixture: &Fixture) -> Result<Vec<MountRecord>, ListError> {
        let seen = RefCell::new(Vec::new());
        for_each_mount(fixture, |r| {
            seen.borrow_mut().push(*r);
            Ok(WalkStep::Continue)
        })?;
        Ok(seen.into_inner())
    }

    #[test]
    fn walk_routes_the_mount_query_and_yields_records() {
        let fixture = Fixture::new(alloc::vec![
            record(b"rootfs", b"/", b"arxfs", MountFlags::READ_ONLY),
            record(b"data", b"/Storage/data", b"arxfs", MountFlags::default()),
        ]);
        let got = collect(&fixture).expect("ok");
        assert_eq!(got.len(), 2);
        assert_eq!(
            fixture.seen.borrow().as_slice(),
            &[SysinfoQueryId::MOUNT_LIST]
        );
    }

    #[test]
    fn walk_pages_until_a_short_page() {
        let mut records = Vec::new();
        for _ in 0..=MOUNT_PAGE {
            records.push(record(b"v", b"/Storage/v", b"arxfs", MountFlags::default()));
        }
        let fixture = Fixture::new(records);
        let got = collect(&fixture).expect("ok");
        assert_eq!(got.len(), usize::from(MOUNT_PAGE) + 1);
        assert_eq!(fixture.seen.borrow().len(), 2);
    }

    #[test]
    fn empty_table_yields_nothing() {
        let fixture = Fixture::new(Vec::new());
        assert!(collect(&fixture).expect("ok").is_empty());
        assert_eq!(fixture.seen.borrow().len(), 1);
    }

    #[test]
    fn malformed_reply_fails_closed() {
        let mut fixture = Fixture::new(alloc::vec![record(
            b"rootfs",
            b"/",
            b"arxfs",
            MountFlags::default()
        )]);
        fixture.malformed = true;
        assert_eq!(
            collect(&fixture),
            Err(ListError::Call(CallError::Service(Errno::BadMagic)))
        );
    }

    #[test]
    fn sink_error_stops_the_walk() {
        let fixture = Fixture::new(alloc::vec![
            record(b"a", b"/a", b"arxfs", MountFlags::default()),
            record(b"b", b"/b", b"arxfs", MountFlags::default()),
        ]);
        let count = RefCell::new(0usize);
        let result = for_each_mount(&fixture, |_| {
            *count.borrow_mut() += 1;
            Err(Errno::NotFound)
        });
        assert_eq!(result, Err(ListError::Sink(Errno::NotFound)));
        assert_eq!(*count.borrow(), 1);
    }

    #[test]
    fn render_writes_the_classic_mount_line() {
        let ns = installed();
        let lines: Vec<_> = ns
            .views()
            .iter()
            .map(|view| render_binding(view, ns.volume_of(view)))
            .collect();
        assert_eq!(
            lines,
            [
                "ARXFSRoot on / type arxfs (rw)",
                "ARXFSSystem on /System type arxfs (ro)",
                "ARXFSRoot[/System/Logs] on /System/Logs type arxfs (rw,noexec)",
                "none on /Storage type none (ro,noexec)",
                "usb1 on /Storage/usb1 type fat32 (rw,noexec)",
            ]
        );
    }

    #[test]
    fn an_unavailable_volume_is_marked_in_the_render() {
        for (availability, marker) in [
            (MountAvailability::UnavailableDirty, "[unavailable-dirty]"),
            (MountAvailability::RecoveryConflict, "[recovery-conflict]"),
            (MountAvailability::UnavailableLost, "[unavailable-lost]"),
        ] {
            let record = MountRecord::new(
                b"usb1",
                b"/Storage/usb1",
                b"fat32",
                MountFlags::NOEXEC,
                MountVolumeState {
                    usage: VolumeStats::default(),
                    availability,
                    medium: None,
                },
                [7u8; 16],
            )
            .expect("record");
            let line = render_binding(
                &view(b"/Storage/usb1", b"/", MountFlags::NOEXEC, 7),
                Some(&record),
            );
            assert_eq!(
                line,
                alloc::format!("usb1 on /Storage/usb1 type fat32 (rw,noexec) {marker}")
            );
        }
    }

    #[test]
    fn render_options_orders_ro_then_restrictions() {
        assert_eq!(render_options(MountFlags::default()), "rw");
        assert_eq!(render_options(MountFlags::READ_ONLY), "ro");
        assert_eq!(
            render_options(MountFlags::NOEXEC.union(MountFlags::READ_ONLY)),
            "ro,noexec"
        );
    }

    #[test]
    fn render_is_lossy_on_invalid_bytes() {
        let line = render_binding(
            &view(b"/", b"/", MountFlags::default(), 1),
            Some(&volume(&[0xFF, 0xFE], b"/", b"arxfs", 1)),
        );
        assert!(line.contains('\u{FFFD}'));
    }

    /// Every volume's service counters are walked, across pages, in the
    /// service's order — and a page is never larger than one reply holds.
    #[test]
    fn the_service_counters_are_walked_whole_across_pages() {
        let count = usize::from(VOLUME_SERVICE_PAGE) + 3;
        let mut fixture = Fixture::new(Vec::new());
        fixture.services = (0..count)
            .map(|i| {
                VolumeIoStatsRecord::new(
                    [u8::try_from(i % 256).expect("byte"); 16],
                    u64::try_from(i).expect("dev"),
                    BlkIoCounters {
                        read_bytes: u64::try_from(i).expect("bytes"),
                        ..BlkIoCounters::default()
                    },
                    BlkDeviceName::new("disk"),
                )
            })
            .collect();
        let mut seen = Vec::new();
        for_each_volume_service(&fixture, |record| {
            seen.push(record.dev());
            Ok(WalkStep::Continue)
        })
        .expect("walked");
        assert_eq!(seen.len(), count);
        assert!(seen.iter().enumerate().all(|(i, dev)| *dev == i as u64));
        assert!(fixture
            .seen
            .borrow()
            .iter()
            .all(|query| *query == SysinfoQueryId::VOLUME_IO_STATS));
    }
}
