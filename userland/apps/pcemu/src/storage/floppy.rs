//! Floppy media as a drive's head finds them: tracks of sectors, each with
//! the ID field the controller searches for, recorded at a density and in
//! an encoding. The drive's speed decides which data rate reads a density:
//! a double-density disk is 250 kb/s at 300 RPM and 300 kb/s at 360.
//!
//! A raw image has the standard layout its size names; an ImageDisk archive
//! keeps each track's own, and is write-protected.

use super::imd::Image;
use super::{BlockDevice, MediaError, SECTOR};
use alloc::boxed::Box;

/// How a track is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Single density.
    Fm,
    /// Double density.
    Mfm,
}

/// How densely a track is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    /// Double density: 360 KiB and 720 KiB media.
    Double,
    /// High density: 1.2 MiB and 1.44 MiB media, read at 500 kb/s in the
    /// drives made for them.
    High,
}

/// A sector's ID field: cylinder, head, record and size code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectorId {
    /// The cylinder the ID names.
    pub c: u8,
    /// The head the ID names.
    pub h: u8,
    /// The record number.
    pub r: u8,
    /// The size code: 128 << n bytes.
    pub n: u8,
}

impl SectorId {
    /// Bytes of data the size code names, at most 8 KiB.
    #[must_use]
    pub const fn bytes(self) -> usize {
        128 << if self.n > 6 { 6 } else { self.n }
    }
}

/// A sector as the controller finds it on its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sector {
    /// Its ID field.
    pub id: SectorId,
    /// Written with a deleted-data address mark.
    pub deleted: bool,
    /// Its data fails the CRC.
    pub bad: bool,
    /// Whether its data field exists at all.
    pub readable: bool,
}

/// Most sectors a track holds.
pub const MAX_SECTORS: usize = 64;

/// A track: how it is recorded and its sectors in the order they pass the
/// head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Track {
    /// The encoding.
    pub encoding: Encoding,
    /// The density.
    pub density: Density,
    /// The gap between sectors, where the format fixes it.
    pub gap: Option<u8>,
    sectors: [Sector; MAX_SECTORS],
    count: u8,
}

impl Track {
    /// A track of `sectors`, at most [`MAX_SECTORS`] of them.
    #[must_use]
    pub fn new(encoding: Encoding, density: Density, gap: Option<u8>, sectors: &[Sector]) -> Self {
        let empty = Sector {
            id: SectorId {
                c: 0,
                h: 0,
                r: 0,
                n: 0,
            },
            deleted: false,
            bad: false,
            readable: false,
        };
        let mut track = Self {
            encoding,
            density,
            gap,
            sectors: [empty; MAX_SECTORS],
            count: 0,
        };
        for (slot, sector) in track.sectors.iter_mut().zip(sectors) {
            *slot = *sector;
            track.count += 1;
        }
        track
    }

    /// The sectors, in the order they pass the head.
    #[must_use]
    pub fn sectors(&self) -> &[Sector] {
        &self.sectors[..usize::from(self.count)]
    }
}

/// A standard raw format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// Cylinders.
    pub cylinders: u8,
    /// Sides.
    pub heads: u8,
    /// Sectors a track, numbered from 1, each 512 bytes.
    pub sectors: u8,
    /// The density.
    pub density: Density,
}

impl Format {
    /// The format a raw image of `bytes` is.
    #[must_use]
    pub fn of_size(bytes: u64) -> Option<Self> {
        let (cylinders, heads, sectors, density) = match bytes / 1024 {
            160 => (40, 1, 8, Density::Double),
            180 => (40, 1, 9, Density::Double),
            320 => (40, 2, 8, Density::Double),
            360 => (40, 2, 9, Density::Double),
            720 => (80, 2, 9, Density::Double),
            1200 => (80, 2, 15, Density::High),
            1440 => (80, 2, 18, Density::High),
            _ => return None,
        };
        bytes.is_multiple_of(1024).then_some(Self {
            cylinders,
            heads,
            sectors,
            density,
        })
    }

    /// Bytes an image of this format holds.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.cylinders as u64 * self.heads as u64 * self.sectors as u64 * SECTOR as u64
    }

    /// The gap a PC formats between this format's sectors.
    #[must_use]
    pub const fn gap(self) -> u8 {
        match self.sectors {
            15 => 0x54,
            18 => 0x6C,
            _ => 0x50,
        }
    }
}

/// A floppy disk.
pub enum Floppy {
    /// A raw image, written through to its device.
    Raw {
        /// Its layout.
        format: Format,
        /// Its sectors.
        device: Box<dyn BlockDevice>,
    },
    /// An ImageDisk archive.
    Archive(Image),
}

impl core::fmt::Debug for Floppy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Raw { format, .. } => f
                .debug_struct("Raw")
                .field("format", format)
                .finish_non_exhaustive(),
            Self::Archive(image) => f.debug_tuple("Archive").field(image).finish(),
        }
    }
}

impl Floppy {
    /// A raw image on `device`, if its size is a standard format's.
    #[must_use]
    pub fn raw(device: Box<dyn BlockDevice>) -> Option<Self> {
        let format = Format::of_size(device.sectors().checked_mul(SECTOR as u64)?)?;
        Some(Self::Raw { format, device })
    }

    /// Cylinders recorded on the medium.
    #[must_use]
    pub fn cylinders(&self) -> u8 {
        match self {
            Self::Raw { format, .. } => format.cylinders,
            Self::Archive(image) => image.cylinders(),
        }
    }

    /// Whether writes are refused: an archive always, a raw image as its
    /// device says.
    #[must_use]
    pub fn write_protected(&self) -> bool {
        match self {
            Self::Raw { device, .. } => device.read_only(),
            Self::Archive(_) => true,
        }
    }

    /// The track at `cylinder` on `head`, if the medium has one there.
    #[must_use]
    pub fn track(&self, cylinder: u8, head: u8) -> Option<Track> {
        match self {
            Self::Raw { format, .. } => {
                if cylinder >= format.cylinders || head >= format.heads {
                    return None;
                }
                let mut sectors = [Sector {
                    id: SectorId {
                        c: 0,
                        h: 0,
                        r: 0,
                        n: 0,
                    },
                    deleted: false,
                    bad: false,
                    readable: true,
                }; MAX_SECTORS];
                for (r, sector) in (1..=format.sectors).zip(sectors.iter_mut()) {
                    sector.id = SectorId {
                        c: cylinder,
                        h: head,
                        r,
                        n: 2,
                    };
                }
                Some(Track::new(
                    Encoding::Mfm,
                    format.density,
                    Some(format.gap()),
                    &sectors[..usize::from(format.sectors)],
                ))
            }
            Self::Archive(image) => image.track(cylinder, head),
        }
    }

    /// Reads the data of the sector in slot `slot` of the track at
    /// `cylinder` and `head` into `out`, as much as both hold.
    pub fn read(
        &mut self,
        cylinder: u8,
        head: u8,
        slot: usize,
        out: &mut [u8],
    ) -> Result<(), MediaError> {
        match self {
            Self::Raw { format, device } => {
                let mut sector = [0; SECTOR];
                device.read(raw_lba(*format, cylinder, head, slot)?, &mut sector)?;
                let len = out.len().min(SECTOR);
                out[..len].copy_from_slice(&sector[..len]);
                Ok(())
            }
            Self::Archive(image) => image.read(cylinder, head, slot, out),
        }
    }

    /// Writes `data` to the sector in slot `slot`: a whole 512-byte sector
    /// of a raw image.
    pub fn write(
        &mut self,
        cylinder: u8,
        head: u8,
        slot: usize,
        data: &[u8],
    ) -> Result<(), MediaError> {
        match self {
            Self::Raw { format, device } => {
                let sector: &[u8; SECTOR] = data
                    .get(..SECTOR)
                    .and_then(|d| d.try_into().ok())
                    .ok_or(MediaError::Failed)?;
                device.write(raw_lba(*format, cylinder, head, slot)?, sector)
            }
            Self::Archive(_) => Err(MediaError::WriteProtected),
        }
    }
}

/// The sector of a raw image the slot of a track is.
fn raw_lba(format: Format, cylinder: u8, head: u8, slot: usize) -> Result<u64, MediaError> {
    if cylinder >= format.cylinders || head >= format.heads || slot >= usize::from(format.sectors) {
        return Err(MediaError::OutOfRange);
    }
    let track = u64::from(cylinder) * u64::from(format.heads) + u64::from(head);
    Ok(track * u64::from(format.sectors) + slot as u64)
}

crate::snapshot::state!(SectorId { c, h, r, n });

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryDisk;
    use alloc::vec;

    #[test]
    fn raw_images_are_known_by_their_size() {
        for (kib, layout) in [
            (160, (40, 1, 8)),
            (360, (40, 2, 9)),
            (720, (80, 2, 9)),
            (1200, (80, 2, 15)),
            (1440, (80, 2, 18)),
        ] {
            let format = Format::of_size(kib * 1024).unwrap();
            assert_eq!(
                (format.cylinders, format.heads, format.sectors),
                layout,
                "{kib} KiB"
            );
            assert_eq!(format.bytes(), kib * 1024);
        }
        assert_eq!(Format::of_size(1000 * 1024), None);
        assert_eq!(Format::of_size(360 * 1024 + 1), None);
    }

    /// A medium that says it holds `.0` sectors and holds none.
    struct Claims(u64);

    impl BlockDevice for Claims {
        fn sectors(&self) -> u64 {
            self.0
        }

        fn read_only(&self) -> bool {
            true
        }

        fn read(&mut self, _: u64, _: &mut [u8; SECTOR]) -> Result<(), MediaError> {
            Err(MediaError::Failed)
        }

        fn write(&mut self, _: u64, _: &[u8; SECTOR]) -> Result<(), MediaError> {
            Err(MediaError::WriteProtected)
        }
    }

    #[test]
    fn a_medium_claiming_more_sectors_than_bytes_can_count_is_no_diskette() {
        // Its size was multiplied out unchecked and overflowed.
        assert!(Floppy::raw(Box::new(Claims(u64::MAX))).is_none());
        assert!(Floppy::raw(Box::new(Claims(u64::MAX / 512 + 1))).is_none());
        assert!(Floppy::raw(Box::new(Claims(720))).is_some());
    }

    #[test]
    fn a_raw_track_numbers_its_sectors_from_one() {
        let mut bytes = vec![0u8; 360 * 1024];
        bytes[(2 * 9 + 3) * SECTOR] = 0xAB;
        let mut floppy = Floppy::raw(Box::new(MemoryDisk::new(bytes, false).unwrap())).unwrap();
        let track = floppy.track(1, 0).unwrap();
        assert_eq!(track.sectors().len(), 9);
        assert_eq!(
            track.sectors()[3].id,
            SectorId {
                c: 1,
                h: 0,
                r: 4,
                n: 2
            }
        );
        let mut out = [0u8; SECTOR];
        floppy.read(1, 0, 3, &mut out).unwrap();
        assert_eq!(out[0], 0xAB, "cylinder 1 head 0 is the image's third track");
        assert!(floppy.track(40, 0).is_none());
        assert_eq!(floppy.read(0, 0, 9, &mut out), Err(MediaError::OutOfRange));
    }
}
