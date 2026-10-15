//! Filesystem placement for persisted segments.
//!
//! The journal hands each closed segment to a [`SegmentStore`](tairix_log::SegmentStore).
//! The production sink writes every segment as its own file under
//! `/System/Logs/<stream>/`, named by the segment's id and the boot that
//! wrote it. A segment is immutable: the sink creates its file exclusively and
//! never rewrites one, and a restarted journal numbers each stream's segments
//! on from the highest already on disk (SYSLOG §6, §10).
//!
//! The path derivation is pure and lives here so it is host-tested independent
//! of any syscall; the sink that actually opens and writes the file over the
//! `tairix-rt` filesystem wrappers is the service binary's concern (the staged
//! follow-on that binds the ingress endpoint and drives the dispatch core over
//! real syscalls — SYSLOG §15).

use alloc::string::String;

use tairix_abi::fs::DirEntries;
use tairix_abi::{BootId, BOOT_ID_HEX_LEN};
use tairix_log::{SegmentError, SegmentReader, Stream};

/// The machine-wide log root. `/System/Logs` is one of the only writable paths
/// beneath the read-only `/System`, mounted `noexec`.
pub const LOGS_ROOT: &str = "/System/Logs";

/// Absolute path of the per-installation **log-attestation key** the journal
/// principal reads at startup to seal `audit`/`security` segments
/// (`PREREQUISITES.md` P-E). A secret, owner-read/write-only under the
/// read-only `/System/Security/Keys` (the installer/image writes it there).
pub const LOG_ATTESTATION_KEY_PATH: &str = "/System/Security/Keys/LogAttestation";

/// Absolute path of the per-installation **machine-id** the journal reads at
/// startup to bind each stream's hash-chain genesis to this installation
/// (SYSLOG §7.1). Non-secret, world-readable public identity (the TAIRiX
/// equivalent of `/etc/machine-id`); the installer/image provisions it, so a
/// missing file means an unprovisioned system.
pub const MACHINE_ID_PATH: &str = "/System/Security/MachineId";

/// Derive where a closed segment image belongs on disk — its containing
/// per-stream directory and its `/System/Logs/<stream>/<id>.seg` file path —
/// by parsing the segment's own self-checksummed header for the stream and
/// segment id, never a caller-supplied placement.
///
/// This is the pure half of the filesystem sink: it is host-testable without
/// any syscall, and the freestanding `Run` binary's `SegmentStore` only adds
/// the `fs_*` calls that create the directory and write the bytes to the
/// returned `(directory, file)` paths.
///
/// # Errors
///
/// The [`SegmentError`] from [`SegmentReader::open`] if `bytes` is not a valid
/// segment image (a truncated or corrupt header) — fail closed, never writing
/// a segment to a guessed path.
pub fn segment_placement_for(bytes: &[u8]) -> Result<(String, String), SegmentError> {
    let reader = SegmentReader::open(bytes)?;
    let header = reader.header();
    Ok((
        stream_directory(header.stream),
        segment_path(header.stream, header.segment_id, &header.boot_id),
    ))
}

/// The per-stream subdirectory name.
///
/// This is the stream's canonical name (the same spelling fed to the stream
/// genesis and used by the rich renderers), so the on-disk directory name and
/// the integrity label are one definition and cannot drift.
#[must_use]
pub fn stream_dir(stream: Stream) -> &'static str {
    stream.name()
}

/// The length of a segment's file name: the 16-digit id, `-`, the boot id in
/// hex, and `.seg`.
const SEGMENT_NAME_LEN: usize = 16 + 1 + BOOT_ID_HEX_LEN + 4;

/// The path a segment of `stream` with id `segment_id`, written in boot
/// `boot`, is stored at: `/System/Logs/<stream>/<segment_id>-<boot>.seg`, both
/// in lowercase hex and the id zero-padded to 16 digits. Ids rise across
/// restarts, so a lexical listing is chronological.
#[must_use]
pub fn segment_path(stream: Stream, segment_id: u64, boot: &BootId) -> String {
    let mut boot_hex = [0u8; BOOT_ID_HEX_LEN];
    alloc::format!(
        "{LOGS_ROOT}/{}/{segment_id:016x}-{}.seg",
        stream_dir(stream),
        boot.write_hex(&mut boot_hex)
    )
}

/// The segment id a file name in a stream directory spells, or `None` for a
/// name no segment is stored under.
fn segment_id_of(name: &[u8]) -> Option<u64> {
    let id = name.get(..16)?;
    let lower_hex = |bytes: &[u8]| {
        bytes
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    };
    let shaped = name.len() == SEGMENT_NAME_LEN
        && name[16] == b'-'
        && name.ends_with(b".seg")
        && lower_hex(id)
        && lower_hex(&name[17..17 + BOOT_ID_HEX_LEN]);
    if !shaped {
        return None;
    }
    u64::from_str_radix(core::str::from_utf8(id).ok()?, 16).ok()
}

/// The id a stream's next segment takes, given its directory `listing` (the
/// `fs_readdir` stream): one past the highest stored, or `0` for none.
///
/// # Errors
///
/// [`Errno::OutOfRange`](tairix_abi::Errno::OutOfRange) when the highest id is
/// the last one, and a malformed listing's own error: either way the journal
/// cannot number a segment without reusing one.
pub fn next_segment_id(listing: &[u8]) -> Result<u64, tairix_abi::Errno> {
    let mut highest: Option<u64> = None;
    for entry in DirEntries::new(listing) {
        if let Some(id) = segment_id_of(entry?.name) {
            highest = Some(highest.map_or(id, |h| h.max(id)));
        }
    }
    match highest {
        None => Ok(0),
        Some(id) => id.checked_add(1).ok_or(tairix_abi::Errno::OutOfRange),
    }
}

/// The directory a stream's segments live in: `/System/Logs/<stream>`.
#[must_use]
pub fn stream_directory(stream: Stream) -> String {
    alloc::format!("{LOGS_ROOT}/{}", stream_dir(stream))
}

#[cfg(test)]
mod tests {
    use super::{
        next_segment_id, segment_id_of, segment_path, segment_placement_for, stream_dir,
        stream_directory,
    };
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;
    use tairix_abi::time::{Duration64, Time64, WallClockReading, WallTimeState};
    use tairix_abi::{
        BootId, CapabilitySummary, DirEntry, Errno, FileId, FileKind, Origin, ProcId, TrustDomain,
        BOOT_ID_LEN, ORIGIN_CONSOLE_NONE,
    };
    use tairix_log::{
        machine_id_hash, CallerContent, Journal, SegmentHeader, SegmentStore, SegmentWriter, Stamp,
        Stream, STREAM_COUNT,
    };

    fn boot(fill: u8) -> BootId {
        BootId::from_raw([fill; BOOT_ID_LEN])
    }

    /// Build a minimal (record-free) closed segment image for `stream`/`id`,
    /// so the placement derivation can be exercised on a real header.
    fn empty_segment(stream: Stream, id: u64) -> Vec<u8> {
        let header = SegmentHeader {
            stream,
            segment_id: id,
            machine_id_hash: machine_id_hash(&[0x33; 16]),
            boot_id: boot(0x5A),
            first_seq: 0,
            prev_segment_hash: [0u8; 32],
            creation_monotonic: Duration64::from_secs(1),
            creation_wall: WallClockReading::new(Time64::from_secs(1), WallTimeState::Trusted),
        };
        let mut buf = alloc::vec![0u8; 4096];
        // Runtime needs no seal; a record-free segment closes immediately.
        let writer = SegmentWriter::begin(&mut buf, &header).expect("begin");
        let finished = writer.finish(None).expect("finish");
        // `finished` borrows `buf`; copy the closed image out before it drops.
        finished.buf[..finished.len].to_vec()
    }

    /// The `fs_readdir` stream listing `names`.
    fn listing(names: &[&str]) -> Vec<u8> {
        let mut out = alloc::vec![0u8; 4096];
        let mut used = 0;
        for name in names {
            let entry = DirEntry {
                kind: FileKind::Regular,
                size: 0,
                allocated: 0,
                modified: Time64::UNIX_EPOCH,
                id: FileId::NONE,
                nlink: 1,
                content_gen: 0,
                name: name.as_bytes(),
            };
            used += entry.encode_into(&mut out[used..]).expect("fits");
        }
        out.truncate(used);
        out
    }

    /// The leaf of a stored segment's path.
    fn leaf(path: &str) -> &str {
        path.rsplit('/').next().expect("leaf")
    }

    #[test]
    fn segment_placement_reads_the_dir_and_file_from_the_header() {
        let seg = empty_segment(Stream::Runtime, 42);
        let (dir, file) = segment_placement_for(&seg).expect("valid segment");
        assert_eq!(dir, "/System/Logs/runtime");
        assert_eq!(
            file,
            "/System/Logs/runtime/000000000000002a-5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a.seg"
        );
    }

    #[test]
    fn segment_placement_fails_closed_on_a_corrupt_header() {
        // A too-short blob cannot carry a whole header, so placement fails
        // closed rather than guessing a path (the exact `SegmentError` variant
        // is the reader's concern; here it is enough that no path is minted).
        assert!(segment_placement_for(&[0u8; 8]).is_err());
    }

    #[test]
    fn stream_dir_matches_the_canonical_label() {
        assert_eq!(stream_dir(Stream::Boot), "boot");
        assert_eq!(stream_dir(Stream::Runtime), "runtime");
        assert_eq!(stream_dir(Stream::Security), "security");
        assert_eq!(stream_dir(Stream::Audit), "audit");
        assert_eq!(stream_dir(Stream::Journal), "journal");
        assert_eq!(stream_directory(Stream::Audit), "/System/Logs/audit");
    }

    #[test]
    fn segment_paths_sort_chronologically_across_boots() {
        // Zero-padding makes a lexical sort agree with the numeric id order,
        // and the id leads, so a later boot's segment sorts after an earlier
        // boot's whatever the boot ids.
        let a = segment_path(Stream::Runtime, 9, &boot(0xff));
        let b = segment_path(Stream::Runtime, 10, &boot(0x00));
        assert!(a < b);
    }

    #[test]
    fn only_a_segment_name_spells_an_id() {
        let name = String::from(leaf(&segment_path(Stream::Runtime, 0x1f, &boot(0xab))));
        assert_eq!(segment_id_of(name.as_bytes()), Some(0x1f));
        for other in [
            "000000000000001f.seg",
            "000000000000001F-abababababababababababababababab.seg",
            "000000000000001f-abababababababababababababababab.segx",
            "000000000000001f_abababababababababababababababab.seg",
            "000000000000001f-ababababababababababababababababab.seg",
            "index",
        ] {
            assert_eq!(segment_id_of(other.as_bytes()), None, "{other}");
        }
    }

    #[test]
    fn the_next_id_follows_the_highest_on_disk() {
        let named = |id| String::from(leaf(&segment_path(Stream::Runtime, id, &boot(1))));
        assert_eq!(next_segment_id(&listing(&[])), Ok(0));
        let (low, high) = (named(3), named(0x20));
        assert_eq!(next_segment_id(&listing(&[&high, "index", &low])), Ok(0x21));
        let last = named(u64::MAX);
        assert_eq!(next_segment_id(&listing(&[&last])), Err(Errno::OutOfRange));
    }

    /// A volume's segment files, refusing to replace one as the production
    /// store's exclusive create does.
    #[derive(Default)]
    struct Volume {
        files: BTreeMap<String, Vec<u8>>,
    }

    impl Volume {
        /// The listing of `stream`'s directory.
        fn listing(&self, stream: Stream) -> Vec<u8> {
            let dir = alloc::format!("{}/", stream_directory(stream));
            let names: Vec<&str> = self
                .files
                .keys()
                .filter_map(|path| path.strip_prefix(dir.as_str()))
                .collect();
            listing(&names)
        }
    }

    impl SegmentStore for &mut Volume {
        type Error = Errno;
        fn store_segment(&mut self, bytes: &[u8]) -> Result<(), Errno> {
            let (_, path) = segment_placement_for(bytes).map_err(|_| Errno::OutOfRange)?;
            if self.files.contains_key(&path) {
                return Err(Errno::AlreadyExists);
            }
            self.files.insert(path, bytes.to_vec());
            Ok(())
        }
    }

    /// One journal run over `volume` in boot `boot`: number each stream on
    /// from the volume, commit `records` runtime records, then flush.
    fn run(volume: &mut Volume, boot_id: BootId, records: usize) {
        let mut next = [0u64; STREAM_COUNT];
        for stream in Stream::ALL {
            next[stream.as_u8() as usize] =
                next_segment_id(&volume.listing(stream)).expect("numbered");
        }
        let mut buffers: [Vec<u8>; STREAM_COUNT] = core::array::from_fn(|_| alloc::vec![0u8; 4096]);
        let [b0, b1, b2, b3, b4, b5] = &mut buffers;
        let bufs: [&mut [u8]; STREAM_COUNT] = [b0, b1, b2, b3, b4, b5].map(Vec::as_mut_slice);
        let origin = Origin::new(
            TrustDomain::Kernel,
            0,
            0,
            1,
            ProcId::KERNEL,
            CapabilitySummary::EMPTY,
            ORIGIN_CONSOLE_NONE,
        );
        let mut journal = Journal::new(
            volume,
            machine_id_hash(&[0x33; 16]),
            boot_id,
            None,
            origin,
            bufs,
        )
        .continuing_segments(next);
        for _ in 0..records {
            let admission = journal.admit(&origin, Some("net"), Some(Stream::Runtime), None, None);
            let caller = CallerContent {
                level: None,
                component: None,
                tag: None,
                event_id: None,
                requested_source: None,
                requested_stream: None,
                message: "link up",
            };
            journal
                .commit(
                    &admission,
                    Stamp {
                        cpu: 0,
                        cpu_seq: 0,
                        monotonic: Duration64::from_secs(1),
                        wall: WallClockReading::UNSET,
                    },
                    caller,
                    &[],
                    &mut [0u8; 512],
                )
                .expect("commit");
            journal.flush().expect("flush");
        }
    }

    /// A restarted journal keeps every segment the earlier run wrote and
    /// numbers its own after them.
    #[test]
    fn a_second_run_keeps_the_first_runs_segments() {
        let mut volume = Volume::default();
        run(&mut volume, boot(1), 2);
        run(&mut volume, boot(2), 2);
        let ids: Vec<u64> = volume
            .files
            .keys()
            .filter_map(|path| segment_id_of(leaf(path).as_bytes()))
            .collect();
        assert_eq!(ids, [0, 1, 2, 3], "both runs kept, in order");
    }
}
