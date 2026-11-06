//! The `twins` fixture of the twin-volume QEMU vertical (the *Twins* rules of
//! `docs/src/filesystem/drives.md`). Run from the
//! scripted root shell, it waits on the mount table for the two USB sticks the
//! vertical plugs in, each a copy of one FAT32 image labelled [`LABEL`] and so
//! carrying one identity, to be attached as [`NAME`] and [`TWIN_NAME`]. It then
//! checks, printing each step's marker as it holds:
//!
//! 1. [`ATTACHED_MARKER`]: both are attached;
//! 2. [`KEYED_MARKER`]: one identity under two keys, the volume attached first
//!    published under the identity itself, and `id::<key>` naming each;
//! 3. [`FILE_IDS_MARKER`]: [`FILE`] has an id of its own on each, the two told
//!    apart by the key alone;
//! 4. [`WRITTEN_MARKER`]: what is written through each reads back from it;
//! 5. [`DETACHED_MARKER`]: each detaches by its key, the other untouched.
//!
//! A pass prints [`PASS_MARKER`] and exits `0`. A failure prints its reason
//! and parks rather than exiting: the vertical arms its end on this program's
//! audited exit, so a failed check times the run out with the reason, and the
//! last marker before it, in the transcript.
//!
//! The steps are pure and host-tested; `src/run.rs` hands them the running
//! system.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::sysinfo::MountRecord;
use tairix_abi::volume::VOLUME_ID_LEN;
use tairix_abi::{Errno, FileId, STORAGE_CATALOG};
use tairix_path::{Path, VolumeId};

/// The command word: the bundle's name, the word the vertical's script
/// types, and the `comm` the kernel attests on the fixture's audited calls.
pub const COMMAND: &str = "twins";

/// The label the sticks' image records, as a FAT volume-label field, space
/// padded: the label the volume manager names both volumes by.
pub const LABEL: [u8; 11] = *b"TWINS      ";

/// The catalog name [`LABEL`] gives the volume attached first.
pub const NAME: &str = "twins";

/// The catalog name its twin is shown under: [`NAME`] with the lowest ordinal
/// free, a spelling no label produces.
pub const TWIN_NAME: &str = "twins_2";

/// The file the image holds, whose id the fixture reads through each twin.
pub const FILE: &str = "HELLO.TXT";

/// The file the fixture writes through each twin.
pub const WRITTEN: &str = "TWIN.TXT";

/// What the fixture writes through [`NAME`] and [`TWIN_NAME`], in that order:
/// different, so a write landing on the other twin is caught.
pub const WRITTEN_TEXTS: [&[u8]; 2] = [
    b"Written through the volume attached first.",
    b"Written through its twin.",
];

/// How long the fixture waits for both volumes before it gives up: past the
/// controller's, both sticks' and both volume managers' bring-up on an
/// emulated machine.
pub const MOUNT_DEADLINE_NS: u64 = 120_000_000_000;

/// Largest file the fixture reads: past every file it writes.
pub const READ_CAP: usize = 4096;

/// The line the fixture prints once both twins are attached.
pub const ATTACHED_MARKER: &str = "TWINS ATTACHED";

/// The line it prints once it has seen one identity under two keys.
pub const KEYED_MARKER: &str = "TWINS KEYED";

/// The line it prints once [`FILE`]'s two ids are told apart.
pub const FILE_IDS_MARKER: &str = "TWINS FILE IDS";

/// The line it prints once each twin holds what was written through it.
pub const WRITTEN_MARKER: &str = "TWINS WRITTEN";

/// The line it prints once each twin is detached by its key.
pub const DETACHED_MARKER: &str = "TWINS DETACHED";

/// The line it prints once every step passed.
pub const PASS_MARKER: &str = "TWINS PASS";

/// What the fixture asks of the running system.
pub trait System {
    /// The attached volumes, as the mount table lists them.
    ///
    /// # Errors
    ///
    /// Why the table could not be read.
    fn mounts(&mut self) -> Result<Vec<MountRecord>, String>;

    /// The identity of the node at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the stat's refusal.
    fn file_id(&mut self, path: &str) -> Result<FileId, Errno>;

    /// The whole of the file at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the read's refusal.
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno>;

    /// Make the file at `path` hold `bytes`, creating it.
    ///
    /// # Errors
    ///
    /// The create's or the write's refusal.
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno>;

    /// Detach the volume published under `key`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn detach(&mut self, key: [u8; VOLUME_ID_LEN]) -> Result<(), Errno>;

    /// Print `line` on standard output.
    ///
    /// # Errors
    ///
    /// Why it could not be printed.
    fn say(&mut self, line: &str) -> Result<(), String>;
}

/// Where the volume shown as `name` is attached: its catalog entry.
#[must_use]
pub fn mount_point(name: &str) -> String {
    format!("/{STORAGE_CATALOG}/{name}")
}

/// The durable root of the volume published under `key`: `id::<key>/`.
#[must_use]
pub fn id_root(key: [u8; VOLUME_ID_LEN]) -> String {
    Path::of_volume(VolumeId::from_bytes(key)).to_string()
}

/// The records of the volumes attached as [`NAME`] and [`TWIN_NAME`] among
/// `records`, in that order, once both are.
#[must_use]
pub fn attached(records: &[MountRecord]) -> Option<(MountRecord, MountRecord)> {
    let at = |name: &str| {
        let target = mount_point(name);
        records
            .iter()
            .find(|record| record.target_bytes() == target.as_bytes())
            .copied()
    };
    Some((at(NAME)?, at(TWIN_NAME)?))
}

/// Every step after the wait, over the volumes attached as [`NAME`] (`first`)
/// and [`TWIN_NAME`] (`twin`), each printing its marker once it holds.
///
/// # Errors
///
/// The first step that did not hold, and why.
pub fn run_steps(
    system: &mut dyn System,
    first: &MountRecord,
    twin: &MountRecord,
) -> Result<(), String> {
    system.say(&format!(
        "{ATTACHED_MARKER}: {} and {}\n",
        mount_point(NAME),
        mount_point(TWIN_NAME)
    ))?;
    keyed(system, first, twin)?;
    file_ids(system, first, twin)?;
    written(system)?;
    detached(system, first, twin)?;
    system.say(&pass_line())
}

/// One identity under two keys: the volume attached first published under the
/// identity itself, its twin under a key of its own, and `id::<key>` naming
/// each.
fn keyed(system: &mut dyn System, first: &MountRecord, twin: &MountRecord) -> Result<(), String> {
    let identity = first.identity();
    if twin.identity() != identity {
        return Err(format!(
            "twins: {TWIN_NAME} carries another identity than {NAME}"
        ));
    }
    if first.volume_id() != identity {
        return Err(format!(
            "twins: {NAME} is not published under the identity it carries"
        ));
    }
    if twin.volume_id() == identity {
        return Err(format!(
            "twins: {TWIN_NAME} is published under the identity {NAME} holds"
        ));
    }
    for (record, name) in [(first, NAME), (twin, TWIN_NAME)] {
        let root = id_root(record.volume_id());
        if id_of(system, &root)? != id_of(system, &mount_point(name))? {
            return Err(format!("twins: {root} names another volume than {name}"));
        }
    }
    system.say(&format!(
        "{KEYED_MARKER}: one identity under two keys, the first published under it\n"
    ))
}

/// [`FILE`]'s id on each twin: its own, the two told apart by the key alone.
fn file_ids(
    system: &mut dyn System,
    first: &MountRecord,
    twin: &MountRecord,
) -> Result<(), String> {
    let ours = id_of(system, &format!("{}/{FILE}", mount_point(NAME)))?;
    let theirs = id_of(system, &format!("{}/{FILE}", mount_point(TWIN_NAME)))?;
    if ours == theirs {
        return Err(format!("twins: {FILE} has one id on both twins"));
    }
    if ours.volume != first.volume_id() || theirs.volume != twin.volume_id() {
        return Err(format!(
            "twins: {FILE}'s id names another volume than the one it is on"
        ));
    }
    if ours.node != theirs.node {
        return Err(format!(
            "twins: {FILE} is not one node of two identical volumes"
        ));
    }
    system.say(&format!(
        "{FILE_IDS_MARKER}: {FILE} is a file of its own on each\n"
    ))
}

/// A write through each twin, each read back holding what was written through
/// it.
fn written(system: &mut dyn System) -> Result<(), String> {
    let paths = [NAME, TWIN_NAME].map(|name| format!("{}/{WRITTEN}", mount_point(name)));
    for (path, text) in paths.iter().zip(WRITTEN_TEXTS) {
        system
            .write(path, text)
            .map_err(|err| format!("twins: {path}: not written: {err:?}"))?;
    }
    for (path, text) in paths.iter().zip(WRITTEN_TEXTS) {
        let bytes = system
            .read(path)
            .map_err(|err| format!("twins: {path}: unreadable: {err:?}"))?;
        if bytes != text {
            return Err(format!(
                "twins: {path} does not hold what was written through it"
            ));
        }
    }
    system.say(&format!(
        "{WRITTEN_MARKER}: each holds what was written through it\n"
    ))
}

/// Each twin detached by its key, the other served until its own turn, and the
/// mount table naming neither once both are gone.
fn detached(
    system: &mut dyn System,
    first: &MountRecord,
    twin: &MountRecord,
) -> Result<(), String> {
    detach(system, twin, TWIN_NAME)?;
    let kept = format!("{}/{FILE}", mount_point(NAME));
    id_of(system, &kept).map_err(|why| format!("{why}, once {TWIN_NAME} was detached"))?;
    detach(system, first, NAME)?;
    let keys = [first.volume_id(), twin.volume_id()];
    let gone = |record: &MountRecord| !keys.contains(&record.volume_id());
    if !system.mounts()?.iter().all(gone) {
        return Err(String::from(
            "twins: the mount table still lists a detached twin",
        ));
    }
    system.say(&format!("{DETACHED_MARKER}: each detached by its key\n"))
}

/// Detach the volume `record` describes, shown as `name`, by its key, and see
/// it gone under its name and its key.
fn detach(system: &mut dyn System, record: &MountRecord, name: &str) -> Result<(), String> {
    let key = record.volume_id();
    system
        .detach(key)
        .map_err(|err| format!("twins: {name} was not detached by its key: {err:?}"))?;
    for gone in [mount_point(name), id_root(key)] {
        if system.file_id(&gone).is_ok() {
            return Err(format!(
                "twins: {gone} is still there once {name} is detached"
            ));
        }
    }
    Ok(())
}

/// The identity of the node at `path`, its refusal stated.
fn id_of(system: &mut dyn System, path: &str) -> Result<FileId, String> {
    system
        .file_id(path)
        .map_err(|err| format!("twins: {path}: {err:?}"))
}

/// The line a pass prints.
#[must_use]
pub fn pass_line() -> String {
    format!(
        "{PASS_MARKER}: {NAME} and {TWIN_NAME} attached together under keys of their own, each \
         served, written and detached by its key\n"
    )
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;

    use tairix_abi::driver::block::{Block, BlockGeometry};
    use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
    use tairix_abi::sysinfo::{MountAvailability, MountVolumeState};
    use tairix_abi::DriverError;
    use tairix_path::Root;

    use super::*;

    /// The identity both twins carry.
    const IDENTITY: [u8; VOLUME_ID_LEN] = [7; VOLUME_ID_LEN];

    /// The key the kernel publishes the twin under.
    const TWIN_KEY: [u8; VOLUME_ID_LEN] = [9; VOLUME_ID_LEN];

    /// The markers a passing run prints, in order.
    const MARKERS: [&str; 6] = [
        ATTACHED_MARKER,
        KEYED_MARKER,
        FILE_IDS_MARKER,
        WRITTEN_MARKER,
        DETACHED_MARKER,
        PASS_MARKER,
    ];

    /// One attached volume as the fake holds it.
    struct Volume {
        name: &'static str,
        key: [u8; VOLUME_ID_LEN],
        identity: [u8; VOLUME_ID_LEN],
        files: BTreeMap<String, Vec<u8>>,
    }

    /// How the fake misbehaves, where it does.
    #[derive(Default)]
    struct Faults {
        /// `id::<key>` reaches the other twin.
        crossed_roots: bool,
        /// Every write lands on the volume attached first.
        misdirected_writes: bool,
        /// A detach answers success and leaves its volume attached.
        sticky_detach: bool,
    }

    /// A running system as far as the steps see it: the attached volumes,
    /// reached by catalog name or by key, and what was printed.
    struct Fake {
        volumes: Vec<Volume>,
        faults: Faults,
        said: Vec<String>,
    }

    impl Fake {
        /// Two twins attached as the kernel attaches them, misbehaving as
        /// `faults` says.
        fn twins(faults: Faults) -> Self {
            let volume = |name, key| Volume {
                name,
                key,
                identity: IDENTITY,
                files: BTreeMap::from([(FILE.to_string(), b"Hello".to_vec())]),
            };
            Self {
                volumes: vec![volume(NAME, IDENTITY), volume(TWIN_NAME, TWIN_KEY)],
                faults,
                said: Vec::new(),
            }
        }

        /// Every step over the twins the fake lists attached.
        fn run(&mut self) -> Result<(), String> {
            let records = self.mounts()?;
            let (first, twin) = attached(&records).ok_or("not both attached")?;
            run_steps(self, &first, &twin)
        }

        /// The volume `path` reaches and the file it names there, empty for
        /// the volume's root.
        fn resolve(&self, path: &str) -> Option<(usize, String)> {
            let parsed = tairix_path::parse(path).ok()?;
            let components = parsed.components();
            let in_catalog = components.first().map(String::as_str) == Some(STORAGE_CATALOG);
            let (at, file) = match parsed.root() {
                Root::VolumeId(id) => {
                    let at = self.volumes.iter().position(|v| v.key == *id.as_bytes())?;
                    if self.faults.crossed_roots {
                        ((at + 1) % self.volumes.len(), components)
                    } else {
                        (at, components)
                    }
                }
                Root::View if in_catalog => {
                    let name = components.get(1)?;
                    let at = self.volumes.iter().position(|v| v.name == name.as_str())?;
                    (at, &components[2..])
                }
                _ => return None,
            };
            Some((at, file.join("/")))
        }

        /// The record the mount table lists for `volume`.
        fn record(volume: &Volume) -> MountRecord {
            let state = MountVolumeState {
                usage: VolumeStats::default(),
                availability: MountAvailability::Available,
                medium: None,
            };
            MountRecord::new(
                volume.name.as_bytes(),
                mount_point(volume.name).as_bytes(),
                b"fat32",
                MountFlags::NOEXEC,
                state,
                volume.key,
            )
            .expect("a well-formed mount record")
            .with_identity(volume.identity)
        }
    }

    impl System for Fake {
        fn mounts(&mut self) -> Result<Vec<MountRecord>, String> {
            Ok(self.volumes.iter().map(Self::record).collect())
        }

        fn file_id(&mut self, path: &str) -> Result<FileId, Errno> {
            let (at, file) = self.resolve(path).ok_or(Errno::NotFound)?;
            let volume = &self.volumes[at];
            let node = match file.as_str() {
                "" => 1,
                name => {
                    volume
                        .files
                        .keys()
                        .position(|held| held == name)
                        .and_then(|index| u64::try_from(index).ok())
                        .ok_or(Errno::NotFound)?
                        + 2
                }
            };
            Ok(FileId {
                volume: volume.key,
                node,
            })
        }

        fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            let (at, file) = self.resolve(path).ok_or(Errno::NotFound)?;
            self.volumes[at]
                .files
                .get(&file)
                .cloned()
                .ok_or(Errno::NotFound)
        }

        fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
            let (at, file) = self.resolve(path).ok_or(Errno::NotFound)?;
            let at = if self.faults.misdirected_writes {
                0
            } else {
                at
            };
            self.volumes[at].files.insert(file, bytes.to_vec());
            Ok(())
        }

        fn detach(&mut self, key: [u8; VOLUME_ID_LEN]) -> Result<(), Errno> {
            let at = self
                .volumes
                .iter()
                .position(|volume| volume.key == key)
                .ok_or(Errno::NotFound)?;
            if !self.faults.sticky_detach {
                self.volumes.remove(at);
            }
            Ok(())
        }

        fn say(&mut self, line: &str) -> Result<(), String> {
            self.said.push(line.to_string());
            Ok(())
        }
    }

    /// The sticks' image as a device the probe reads.
    struct Image(Vec<u8>);

    impl Block for Image {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            let sector = tairix_test_fat32_image::SECTOR_BYTES;
            Ok(BlockGeometry {
                block_size: u32::try_from(sector).map_err(|_| DriverError::OutOfRange)?,
                block_count: u64::try_from(self.0.len() / sector)
                    .map_err(|_| DriverError::OutOfRange)?,
            })
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            let start = usize::try_from(lba)
                .ok()
                .and_then(|lba| lba.checked_mul(tairix_test_fat32_image::SECTOR_BYTES))
                .ok_or(DriverError::LengthOutOfRange)?;
            let bytes = start
                .checked_add(buf.len())
                .and_then(|end| self.0.get(start..end))
                .ok_or(DriverError::LengthOutOfRange)?;
            buf.copy_from_slice(bytes);
            Ok(())
        }

        fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
            Err(DriverError::ReadOnly)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn every_step_holds_on_twins_that_behave() {
        let mut fake = Fake::twins(Faults::default());
        assert_eq!(fake.run(), Ok(()));
        assert!(
            fake.volumes.is_empty(),
            "both twins are detached at the end"
        );
        assert_eq!(fake.said.len(), MARKERS.len());
        for (line, marker) in fake.said.iter().zip(MARKERS) {
            assert!(line.starts_with(marker) && line.ends_with('\n'), "{line}");
        }
        let (steps, pass) = fake.said.split_at(MARKERS.len() - 1);
        assert!(steps.iter().all(|line| !line.contains(PASS_MARKER)));
        assert_eq!(pass, [pass_line()]);
    }

    #[test]
    fn the_twins_count_as_attached_only_once_both_are() {
        let mut fake = Fake::twins(Faults::default());
        let both = fake.mounts().expect("listed");
        assert!(attached(&both).is_some());
        assert_eq!(attached(&both[..1]), None, "the first alone");
        assert_eq!(attached(&both[1..]), None, "the twin alone");
    }

    /// A step that fails says why and prints no marker of its own, so the
    /// transcript's last marker names the step that held last.
    #[test]
    fn twins_published_under_one_key_fail_at_their_own_step() {
        let mut fake = Fake::twins(Faults::default());
        fake.volumes[1].key = IDENTITY;
        let why = fake.run().expect_err("one key for two volumes");
        assert!(why.contains(TWIN_NAME), "{why}");
        assert_eq!(fake.said.len(), 1, "{:?}", fake.said);
    }

    #[test]
    fn a_first_volume_not_published_under_its_identity_fails() {
        let mut fake = Fake::twins(Faults::default());
        fake.volumes[0].key = [3; VOLUME_ID_LEN];
        let why = fake.run().expect_err("the first holds a key of its own");
        assert!(why.contains("not published under the identity"), "{why}");
    }

    #[test]
    fn volumes_of_two_identities_are_no_twins() {
        let mut fake = Fake::twins(Faults::default());
        fake.volumes[1].identity = [5; VOLUME_ID_LEN];
        let why = fake.run().expect_err("not twins at all");
        assert!(why.contains("another identity"), "{why}");
    }

    #[test]
    fn an_id_root_naming_the_other_twin_fails() {
        let mut fake = Fake::twins(Faults {
            crossed_roots: true,
            ..Faults::default()
        });
        let why = fake.run().expect_err("id:: crossed over");
        assert!(why.contains("names another volume"), "{why}");
    }

    #[test]
    fn a_write_landing_on_the_other_twin_fails() {
        let mut fake = Fake::twins(Faults {
            misdirected_writes: true,
            ..Faults::default()
        });
        let why = fake.run().expect_err("both writes on one twin");
        let first = format!("{}/{WRITTEN}", mount_point(NAME));
        assert!(why.contains(&first), "{why}");
        assert_eq!(fake.said.len(), 3, "{:?}", fake.said);
    }

    #[test]
    fn a_detach_that_leaves_its_volume_fails() {
        let mut fake = Fake::twins(Faults {
            sticky_detach: true,
            ..Faults::default()
        });
        let why = fake.run().expect_err("a detach that took nothing");
        assert!(why.contains("still there"), "{why}");
    }

    /// The names the fixture waits for are the ones the volume manager's own
    /// policy gives the sticks' image: its label, read where the probe reads
    /// it, for the volume attached first, and that name's first twin name for
    /// the second.
    #[test]
    fn the_names_waited_for_are_the_ones_the_sticks_image_is_given() {
        use tairix_abi::blkio::BlkTransport;
        use tairix_fsprobe::name::{candidates, twin, NameBase};
        use tairix_fsprobe::{recognise, Recognised};

        let image = Image(tairix_test_fat32_image::build_labelled_image(&LABEL));
        let mut device = tairix_bytedev::ByteDevice::new(image).expect("a device of sectors");
        let Ok(Some(Recognised::Volume(volume))) = recognise(&mut device) else {
            panic!("the sticks' image is a recognised volume");
        };
        let base = NameBase::of(volume.label(), Some(BlkTransport::Usb));
        let name = candidates(base, volume.identity).next().expect("a name");
        assert_eq!(name.as_bytes(), NAME.as_bytes());
        let second = twin(name.as_bytes(), 2).expect("a twin name");
        assert_eq!(second.as_bytes(), TWIN_NAME.as_bytes());
    }

    #[test]
    fn the_file_read_through_each_twin_is_the_one_the_image_holds() {
        assert_eq!(FILE.as_bytes(), tairix_test_fat32_image::PLANTED_FILE_NAME);
    }

    /// [`COMMAND`] is the manifest source's `name`, so the script, the end's
    /// keying and the composed bundle cannot spell it differently.
    #[test]
    fn command_matches_the_manifest_source_name() {
        let manifest = include_str!("../AppInfo.toml");
        let named = manifest.lines().any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                key.trim() == "name" && value.trim() == format!("{COMMAND:?}")
            })
        });
        assert!(named, "AppInfo.toml `name` must be {COMMAND:?}");
    }
}
