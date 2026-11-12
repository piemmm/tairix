//! The fixed disk a new machine comes with: TAIRiX DOS set up on a 30 MB
//! drive as DOS 5's SETUP leaves one — one active FAT16 partition over all
//! of the drive the BIOS offers, the system in the root with `CONFIG.SYS`
//! and `AUTOEXEC.BAT`, the utilities and drivers in `C:\DOS` — on a sparse
//! medium, so the disk costs only what it holds.

use super::volume::{self, File, VolumeError};
use crate::asm::AsmError;
use crate::storage::hdc::{DiskType, LowLevel, Recording};
use crate::storage::sparse::{Region, SparseDisk};
use crate::storage::{BlockDevice, SECTOR};
use crate::time::NANOS_PER_MILLISECOND as MS;
use alloc::string::String;
use alloc::vec::Vec;
use tairix_abi::time::{CivilTime, DosTime};
use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::format::{self, Medium, Shape};
use tairix_fat::stamp::Stamp;
use tairix_partition::mbr::{self, Entry, Geometry};

/// The Seagate ST-238R, the RLL drive of 30 MB the machines with an RLL
/// controller come with.
pub const ST_238R: DiskType = DiskType {
    cylinders: 615,
    heads: 4,
    track_to_track: 20 * MS,
    average_seek: 65 * MS,
};

/// The Seagate ST-4038, the MFM drive of 30 MB the machines with an MFM
/// controller come with.
pub const ST_4038: DiskType = DiskType {
    cylinders: 733,
    heads: 5,
    track_to_track: 8 * MS,
    average_seek: 40 * MS,
};

/// The interleave the drives are formatted with: the best the XT's
/// controller keeps up with.
pub const INTERLEAVE: u8 = 3;

/// The disk's label.
pub const LABEL: [u8; 11] = *b"TAIRIX DOS ";

/// The stamp the system's files carry: 11 June 1991, 05:00, when DOS 5
/// shipped.
#[must_use]
pub fn released() -> Stamp {
    let civil = CivilTime {
        year: 1991,
        month: 6,
        day: 11,
        hour: 5,
        minute: 0,
        second: 0,
    };
    DosTime::of_civil(&civil).map_or(Stamp::NONE, |words| Stamp {
        date: words.date,
        time: words.time,
        centis: 0,
    })
}

/// What `CONFIG.SYS` holds: DOS 5's SETUP's figures for a PC.
pub const CONFIG_SYS: &[u8] = b"FILES=30\r\nBUFFERS=20\r\n";
/// What `AUTOEXEC.BAT` holds: the utilities on the path and the mouse's
/// driver loaded, as the machines each have a mouse.
pub const AUTOEXEC_BAT: &[u8] = b"@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\DOS\r\nC:\\DOS\\MOUSE\r\n";

/// The prompt [`AUTOEXEC_BAT`] leaves the shell at: `PROMPT $P$G` in the
/// root of `C:`.
pub const ROOT_PROMPT: &[u8] = b"C:\\>";

/// A machine's fixed disk as it comes: its drive, its low-level format,
/// and what it holds.
pub struct DefaultDisk {
    /// The drive.
    pub kind: DiskType,
    /// Its low-level format, which its owner keeps beside the image.
    pub low_level: LowLevel,
    /// What it holds.
    pub device: SparseDisk,
}

/// Why no default disk could be made: a defect of the system's own, for
/// the system's files and the drive are fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// A program or record did not assemble.
    Code(AsmError),
    /// The partition could not be planned or written.
    Partition,
    /// The volume could not be made.
    Volume(VolumeError),
}

/// The drive a disk is made for on a controller recording `recording`.
#[must_use]
pub const fn drive_for(recording: Recording) -> DiskType {
    match recording {
        Recording::Rll => ST_238R,
        Recording::Mfm => ST_4038,
    }
}

/// The sectors of a drive of `kind`'s every cylinder on a controller
/// recording `recording`, and those of the cylinders the BIOS offers: all
/// but the last, which it keeps for the controller's diagnostics.
#[must_use]
pub fn sizes(kind: DiskType, recording: Recording) -> (u32, u32) {
    let cylinder = u32::from(kind.heads) * u32::from(recording.sectors());
    let cylinders = u32::from(kind.cylinders);
    (cylinders * cylinder, cylinders.saturating_sub(1) * cylinder)
}

/// A master boot record with one active partition of `sectors` from
/// `start`, its type the one DOS gives a volume of `kind` there, its CHS
/// addresses those of `geometry`.
///
/// # Errors
///
/// [`SetupError`] where the code does not assemble or the entry cannot be
/// written for the geometry.
pub fn partitioned(
    kind: Kind,
    start: u32,
    sectors: u32,
    geometry: Option<Geometry>,
) -> Result<[u8; SECTOR], SetupError> {
    let entry = Entry {
        type_byte: mbr::fat_type(kind, start, sectors, geometry),
        active: true,
        start_lba: start,
        sectors,
    };
    let mut record = super::boot::master_boot_record().map_err(SetupError::Code)?;
    let slot = mbr::encode_slot(&entry, geometry).map_err(|_| SetupError::Partition)?;
    let table = mbr::PARTITION_TABLE_OFFSET;
    record[table..table + mbr::PARTITION_ENTRY_LEN].copy_from_slice(&slot);
    Ok(record)
}

/// The files the disk holds: the shell and the startup files in the root,
/// every utility and driver in `C:\DOS`.
fn files() -> Result<Vec<(String, Vec<u8>)>, SetupError> {
    let mut out = alloc::vec![
        (
            String::from("COMMAND.COM"),
            super::command_com().map_err(SetupError::Code)?
        ),
        (String::from("CONFIG.SYS"), CONFIG_SYS.to_vec()),
        (String::from("AUTOEXEC.BAT"), AUTOEXEC_BAT.to_vec()),
        (String::from("DOS\\ANSI.SYS"), super::ansi_sys()),
        (
            String::from("DOS\\MOUSE.COM"),
            super::mouse_com().map_err(SetupError::Code)?
        ),
    ];
    for (name, image) in super::utilities().map_err(SetupError::Code)? {
        out.push((alloc::format!("DOS\\{name}"), image));
    }
    Ok(out)
}

/// The disk a machine whose controller records `recording` comes with,
/// its volume's serial `serial`.
///
/// # Errors
///
/// [`SetupError`], a defect of the system's own.
pub fn default_disk(recording: Recording, serial: u32) -> Result<DefaultDisk, SetupError> {
    let kind = drive_for(recording);
    let track = u32::from(recording.sectors());
    let (total, offered) = sizes(kind, recording);
    let (start, sectors) = (track, offered - track);
    let medium = Medium {
        sectors,
        bytes_per_sector: 512,
        sectors_per_track: u16::from(recording.sectors()),
        heads: u16::from(kind.heads),
        hidden_sectors: start,
        media: format::FIXED_MEDIA,
    };
    let shape = Shape {
        kind: None,
        sectors_per_cluster: None,
        root_entries: 0,
        serial,
        label: Some(LABEL),
    };
    let bpb: Bpb = format::plan(&medium, &shape).map_err(|_| SetupError::Partition)?;
    let mut device = SparseDisk::new(u64::from(total));
    let record = partitioned(
        bpb.kind,
        start,
        sectors,
        Geometry::new(kind.heads, recording.sectors()),
    )?;
    device
        .write(0, &record)
        .map_err(|_| SetupError::Partition)?;
    let owned = files()?;
    let listed: Vec<File<'_>> = owned
        .iter()
        .map(|(path, bytes)| File {
            path,
            bytes,
            attributes: 0,
        })
        .collect();
    let mut region = Region {
        device: &mut device,
        first: u64::from(start),
        sectors: u64::from(sectors),
    };
    volume::build(&bpb, Some(LABEL), &listed, released(), &mut region)
        .map_err(SetupError::Volume)?;
    Ok(DefaultDisk {
        kind,
        low_level: LowLevel {
            interleave: INTERLEAVE,
            bad_tracks: Vec::new(),
        },
        device,
    })
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
