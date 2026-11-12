//! Volumes the emulator makes for TAIRiX DOS: a FAT12 or FAT16 volume of
//! the shape `lib/fat` plans, its boot record TAIRiX DOS's own, holding a
//! tree of files laid out from the volume's first cluster.

use crate::storage::{BlockDevice, MemoryDisk, SECTOR};
use alloc::vec;
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::dir::{attr, ShortEntry, DOT, DOTDOT, ENTRY};
use tairix_fat::stamp::{Stamp, Stamps};
use tairix_fat::{format, name, table};

/// A file to put on a volume.
#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    /// Its path from the root, its components 8.3 names apart by `\`; the
    /// directories on it are made.
    pub path: &'a str,
    /// Its bytes.
    pub bytes: &'a [u8],
    /// Its attributes, beside the archive bit every new file has.
    pub attributes: u8,
}

/// Why no volume could be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeError {
    /// A path component is no 8.3 name, or names a file and a directory.
    Name,
    /// The files do not fit the volume's clusters.
    Full,
    /// The root directory has no room for another entry.
    Root,
    /// The volume is FAT32's, which DOS does not read.
    Kind,
    /// The boot record could not be assembled.
    Boot,
    /// The device failed a write.
    Device,
}

/// A sector's bytes, as a volume's offsets count them.
const SECTOR_BYTES: u64 = SECTOR as u64;

/// A directory being laid out: its entries, and where it lies.
struct Directory {
    /// Its parent's index; the root is its own.
    parent: usize,
    /// Its first cluster, `0` for the root.
    cluster: u32,
    entries: Vec<Item>,
}

/// An entry of a directory.
struct Item {
    name: [u8; 11],
    content: Content,
}

/// What an entry stands for.
enum Content {
    /// The file `file` of those asked for, from `cluster`.
    File { file: usize, cluster: u32 },
    /// A directory of the tree.
    Directory(usize),
    /// The volume's label.
    Label,
}

/// A FAT12 or FAT16 volume `bpb` describes, labelled `label` when given,
/// holding `files` — each entry stamped `stamp` — and the TAIRiX DOS boot
/// record, as an image.
///
/// # Errors
///
/// [`VolumeError`] when the files cannot be laid out on it.
pub fn volume(
    bpb: &Bpb,
    label: Option<[u8; 11]>,
    files: &[File<'_>],
    stamp: Stamp,
) -> Result<Vec<u8>, VolumeError> {
    let sectors = usize::try_from(bpb.total_sectors).map_err(|_| VolumeError::Full)?;
    let bytes = sectors.checked_mul(SECTOR).ok_or(VolumeError::Full)?;
    let mut disk = MemoryDisk::new(vec![0u8; bytes], false).ok_or(VolumeError::Full)?;
    build(bpb, label, files, stamp, &mut disk)?;
    Ok(disk.into_bytes())
}

/// The same laid out on `device`: its system area and every sector a file
/// or directory takes written, the rest of its data area left as it was,
/// so a blank sparse device holds only what the volume does.
///
/// # Errors
///
/// [`VolumeError`] when the files cannot be laid out on it, or the device
/// is too small for the volume or fails a write.
pub fn build(
    bpb: &Bpb,
    label: Option<[u8; 11]>,
    files: &[File<'_>],
    stamp: Stamp,
    device: &mut dyn BlockDevice,
) -> Result<(), VolumeError> {
    if bpb.kind == Kind::Fat32 {
        return Err(VolumeError::Kind);
    }
    if bpb.sector_bytes() != SECTOR_BYTES || device.sectors() < u64::from(bpb.total_sectors) {
        return Err(VolumeError::Full);
    }
    let boot = super::boot_record(bpb).map_err(|_| VolumeError::Boot)?;
    let mut tree = vec![Directory {
        parent: 0,
        cluster: 0,
        entries: Vec::new(),
    }];
    if let Some(label) = label {
        tree[0].entries.push(Item {
            name: label,
            content: Content::Label,
        });
    }
    for (index, file) in files.iter().enumerate() {
        place(&mut tree, index, file.path)?;
    }
    let fat_bytes = usize::try_from(bpb.fat_bytes()).map_err(|_| VolumeError::Full)?;
    let root_bytes = usize::try_from(bpb.root_bytes()).map_err(|_| VolumeError::Full)?;
    let mut layout = Layout {
        bpb,
        fat: vec![0u8; fat_bytes],
        root: vec![0u8; root_bytes],
        device,
        next: 2,
    };
    for directory in tree.iter_mut().skip(1) {
        directory.cluster = layout.allocate((directory.entries.len() + 2) * ENTRY)?;
    }
    for directory in &mut tree {
        for item in &mut directory.entries {
            if let Content::File { file, cluster } = &mut item.content {
                *cluster = layout.allocate(files[*file].bytes.len())?;
            }
        }
    }
    for n in 0..tree.len() {
        layout.write_directory(&tree, n, files, stamp)?;
    }
    for directory in &tree {
        for item in &directory.entries {
            if let Content::File { file, cluster } = item.content {
                layout.write_data(cluster, files[file].bytes)?;
            }
        }
    }
    layout.write_system(&boot)
}

/// Puts file `index` at `path` into the tree, its directories made.
fn place(tree: &mut Vec<Directory>, index: usize, path: &str) -> Result<(), VolumeError> {
    let mut parts = path.split('\\').filter(|part| !part.is_empty()).peekable();
    let mut at = 0;
    while let Some(part) = parts.next() {
        let field = name::field(part.as_bytes())
            .filter(|f| *f != DOT && *f != DOTDOT)
            .ok_or(VolumeError::Name)?;
        let found = tree[at].entries.iter().position(|item| item.name == field);
        if parts.peek().is_none() {
            if found.is_some() {
                return Err(VolumeError::Name);
            }
            tree[at].entries.push(Item {
                name: field,
                content: Content::File {
                    file: index,
                    cluster: 0,
                },
            });
            return Ok(());
        }
        at = match found.map(|n| &tree[at].entries[n].content) {
            Some(Content::Directory(child)) => *child,
            Some(Content::File { .. } | Content::Label) => return Err(VolumeError::Name),
            None => {
                let child = tree.len();
                tree.push(Directory {
                    parent: at,
                    cluster: 0,
                    entries: Vec::new(),
                });
                tree[at].entries.push(Item {
                    name: field,
                    content: Content::Directory(child),
                });
                child
            }
        };
    }
    Err(VolumeError::Name)
}

/// The volume's clusters as they are handed out, each run of them chained
/// in the table held here; the root directory is held here too, and both
/// are written with the rest of the system area once all is laid out.
struct Layout<'a> {
    bpb: &'a Bpb,
    fat: Vec<u8>,
    root: Vec<u8>,
    device: &'a mut dyn BlockDevice,
    next: u32,
}

impl Layout<'_> {
    /// Chains enough clusters for `bytes`: the first, or `0` for none.
    fn allocate(&mut self, bytes: usize) -> Result<u32, VolumeError> {
        let cluster_bytes =
            usize::try_from(self.bpb.cluster_bytes()).map_err(|_| VolumeError::Full)?;
        let count = u32::try_from(bytes.div_ceil(cluster_bytes)).map_err(|_| VolumeError::Full)?;
        if count == 0 {
            return Ok(0);
        }
        let first = self.next;
        let last = first + count - 1;
        if last > self.bpb.max_cluster() {
            return Err(VolumeError::Full);
        }
        for cluster in first..=last {
            let link = if cluster == last {
                table::end(self.bpb.kind)
            } else {
                cluster + 1
            };
            table::set(self.bpb.kind, &mut self.fat, cluster, link);
        }
        self.next = last + 1;
        Ok(first)
    }

    /// `bytes` written from the volume's sector `first` on, the last sector
    /// filled out with zeros.
    fn write_sectors(&mut self, first: u64, bytes: &[u8]) -> Result<(), VolumeError> {
        for (n, piece) in (0u64..).zip(bytes.chunks(SECTOR)) {
            let mut sector = [0u8; SECTOR];
            sector[..piece.len()].copy_from_slice(piece);
            self.device
                .write(first + n, &sector)
                .map_err(|_| VolumeError::Device)?;
        }
        Ok(())
    }

    /// Writes `bytes` from `cluster` on, its clusters a run.
    fn write_data(&mut self, cluster: u32, bytes: &[u8]) -> Result<(), VolumeError> {
        let Some(start) = self.bpb.cluster_start(cluster) else {
            return Ok(());
        };
        self.write_sectors(start / SECTOR_BYTES, bytes)
    }

    /// Writes directory `n`'s entries, a subdirectory's `.` and `..` first,
    /// its clusters' rest zeros; the root's into the root held here.
    fn write_directory(
        &mut self,
        tree: &[Directory],
        n: usize,
        files: &[File<'_>],
        stamp: Stamp,
    ) -> Result<(), VolumeError> {
        let directory = &tree[n];
        let stamps = Stamps {
            created: stamp,
            accessed: stamp.date,
            written: Stamp { centis: 0, ..stamp },
        };
        let entry = |name: [u8; 11], attributes: u8, cluster: u32, size: u32| {
            ShortEntry {
                name,
                attributes,
                stamps,
                cluster,
                size,
                ..ShortEntry::default()
            }
            .encode()
        };
        let mut raw = Vec::new();
        if n != 0 {
            let parent = if directory.parent == 0 {
                0
            } else {
                tree[directory.parent].cluster
            };
            raw.extend_from_slice(&entry(DOT, attr::DIRECTORY, directory.cluster, 0));
            raw.extend_from_slice(&entry(DOTDOT, attr::DIRECTORY, parent, 0));
        }
        for item in &directory.entries {
            raw.extend_from_slice(&match item.content {
                Content::Label => entry(item.name, attr::VOLUME_ID, 0, 0),
                Content::Directory(child) => {
                    entry(item.name, attr::DIRECTORY, tree[child].cluster, 0)
                }
                Content::File { file, cluster } => {
                    let size =
                        u32::try_from(files[file].bytes.len()).map_err(|_| VolumeError::Full)?;
                    entry(
                        item.name,
                        attr::ARCHIVE | files[file].attributes,
                        cluster,
                        size,
                    )
                }
            });
        }
        if n == 0 {
            if raw.len() > usize::from(self.bpb.root_entries) * ENTRY {
                return Err(VolumeError::Root);
            }
            self.root[..raw.len()].copy_from_slice(&raw);
            return Ok(());
        }
        let cluster_bytes =
            usize::try_from(self.bpb.cluster_bytes()).map_err(|_| VolumeError::Full)?;
        raw.resize(raw.len().div_ceil(cluster_bytes) * cluster_bytes, 0);
        self.write_data(directory.cluster, &raw)
    }

    /// The system area written: the boot record and what `lib/fat` puts in
    /// a new volume's other reserved sectors, then every copy of the table
    /// and the root as they are held here.
    fn write_system(&mut self, boot: &[u8; SECTOR]) -> Result<(), VolumeError> {
        let bpb = self.bpb;
        let fat_start = bpb.fat_start() / SECTOR_BYTES;
        let fat_sectors = u64::from(bpb.fat_sectors);
        let fat_end = fat_start + fat_sectors * u64::from(bpb.fats);
        let [media, end] = table::head(bpb.kind, bpb.media);
        table::set(bpb.kind, &mut self.fat, 0, media);
        table::set(bpb.kind, &mut self.fat, 1, end);
        for n in 0..format::system_sectors(bpb) {
            let mut sector = [0u8; SECTOR];
            let held = if (fat_start..fat_end).contains(&n) {
                Some((&self.fat, (n - fat_start) % fat_sectors))
            } else if n >= fat_end {
                Some((&self.root, n - fat_end))
            } else {
                None
            };
            match held {
                Some((bytes, index)) => {
                    let at = usize::try_from(index).map_err(|_| VolumeError::Full)? * SECTOR;
                    let piece = bytes.get(at..).unwrap_or_default();
                    let len = piece.len().min(SECTOR);
                    sector[..len].copy_from_slice(&piece[..len]);
                }
                None => format::system_sector(bpb, boot, n, &mut sector),
            }
            self.device
                .write(n, &sector)
                .map_err(|_| VolumeError::Device)?;
        }
        Ok(())
    }
}
