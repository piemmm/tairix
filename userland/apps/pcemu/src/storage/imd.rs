//! ImageDisk (`.IMD`) archives: every track in its own format, with each
//! sector's ID, deleted-data mark and CRC error kept.
//!
//! The file is untrusted. The parser is total over any bytes, refuses a
//! track it cannot represent, and bounds what an archive expands to, since
//! a sector of one fill byte stands for up to 8 KiB.

use super::floppy::{Density, Encoding, Sector, SectorId, Track, MAX_SECTORS};
use super::MediaError;
use alloc::vec::Vec;

/// The most an archive may expand to: more than any floppy format holds.
const MAX_EXPANDED: usize = 4 << 20;
/// The most tracks an archive may hold: 128 cylinders of two sides.
const MAX_TRACKS: usize = 256;

/// Why an archive was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImdError {
    /// It does not begin `IMD `.
    NotImd,
    /// It ends inside a header or a sector.
    Truncated,
    /// A field holds a value no archive can.
    Malformed,
    /// It holds more than any floppy could.
    TooLarge,
}

#[derive(Clone, Debug)]
struct Recorded {
    cylinder: u8,
    head: u8,
    track: Track,
    /// Each sector's data: where it starts in the archive's store, or
    /// `None` when the archive marks it unavailable.
    data: [Option<u32>; MAX_SECTORS],
}

/// A parsed archive.
#[derive(Clone, Debug)]
pub struct Image {
    tracks: Vec<Recorded>,
    store: Vec<u8>,
}

/// Reads the archive a byte at a time, refusing to run past its end.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, ImdError> {
        let byte = *self.bytes.get(self.at).ok_or(ImdError::Truncated)?;
        self.at += 1;
        Ok(byte)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ImdError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(ImdError::Truncated)?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }
}

/// The size code of a sector of `bytes`, if it is one a controller can
/// record.
fn size_code(bytes: u16) -> Result<u8, ImdError> {
    (0..=6u8)
        .find(|&n| 128u16 << n == bytes)
        .ok_or(ImdError::Malformed)
}

/// What an ImageDisk archive begins with.
pub const MAGIC: &[u8; 4] = b"IMD ";

impl Image {
    /// Parses an archive.
    pub fn parse(bytes: &[u8]) -> Result<Self, ImdError> {
        if !bytes.starts_with(MAGIC) {
            return Err(ImdError::NotImd);
        }
        let comment_end = bytes
            .iter()
            .position(|&b| b == 0x1A)
            .ok_or(ImdError::Truncated)?;
        let mut reader = Reader {
            bytes,
            at: comment_end + 1,
        };
        let mut image = Self {
            tracks: Vec::new(),
            store: Vec::new(),
        };
        while !reader.done() {
            if image.tracks.len() == MAX_TRACKS {
                return Err(ImdError::TooLarge);
            }
            let track = image.parse_track(&mut reader)?;
            if image
                .tracks
                .iter()
                .any(|t| (t.cylinder, t.head) == (track.cylinder, track.head))
            {
                return Err(ImdError::Malformed);
            }
            image
                .tracks
                .try_reserve(1)
                .map_err(|_| ImdError::TooLarge)?;
            image.tracks.push(track);
        }
        Ok(image)
    }

    fn parse_track(&mut self, reader: &mut Reader<'_>) -> Result<Recorded, ImdError> {
        let mode = reader.byte()?;
        let cylinder = reader.byte()?;
        let head_flags = reader.byte()?;
        let count = usize::from(reader.byte()?);
        let size = reader.byte()?;
        let (encoding, density) = match mode {
            0 => (Encoding::Fm, Density::High),
            1 | 2 => (Encoding::Fm, Density::Double),
            3 => (Encoding::Mfm, Density::High),
            4 | 5 => (Encoding::Mfm, Density::Double),
            _ => return Err(ImdError::Malformed),
        };
        let head = head_flags & 0x3F;
        if head > 1 || count > MAX_SECTORS || (size > 6 && size != 0xFF) {
            return Err(ImdError::Malformed);
        }
        let numbers = reader.take(count)?;
        let cylinders = if head_flags & 0x80 != 0 {
            Some(reader.take(count)?)
        } else {
            None
        };
        let heads = if head_flags & 0x40 != 0 {
            Some(reader.take(count)?)
        } else {
            None
        };
        let sizes = if size == 0xFF {
            Some(reader.take(count * 2)?)
        } else {
            None
        };
        let mut sectors = [Sector {
            id: SectorId {
                c: 0,
                h: 0,
                r: 0,
                n: 0,
            },
            deleted: false,
            bad: false,
            readable: false,
        }; MAX_SECTORS];
        let mut data = [None; MAX_SECTORS];
        for i in 0..count {
            let n = match sizes {
                Some(table) => size_code(u16::from_le_bytes([table[i * 2], table[i * 2 + 1]]))?,
                None => size,
            };
            let id = SectorId {
                c: cylinders.map_or(cylinder, |map| map[i]),
                h: heads.map_or(head, |map| map[i]),
                r: numbers[i],
                n,
            };
            let kind = reader.byte()?;
            if kind > 8 {
                return Err(ImdError::Malformed);
            }
            let len = id.bytes();
            if kind != 0 {
                let at = u32::try_from(self.store.len()).map_err(|_| ImdError::TooLarge)?;
                if self.store.len() + len > MAX_EXPANDED {
                    return Err(ImdError::TooLarge);
                }
                self.store
                    .try_reserve(len)
                    .map_err(|_| ImdError::TooLarge)?;
                if kind % 2 == 0 {
                    let fill = reader.byte()?;
                    self.store.resize(self.store.len() + len, fill);
                } else {
                    self.store.extend_from_slice(reader.take(len)?);
                }
                data[i] = Some(at);
            }
            sectors[i] = Sector {
                id,
                deleted: matches!(kind, 3 | 4 | 7 | 8),
                bad: kind >= 5,
                readable: kind != 0,
            };
        }
        Ok(Recorded {
            cylinder,
            head,
            track: Track::new(encoding, density, None, &sectors[..count]),
            data,
        })
    }

    /// Cylinders the archive records.
    #[must_use]
    pub fn cylinders(&self) -> u8 {
        self.tracks
            .iter()
            .map(|t| t.cylinder.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    fn recorded(&self, cylinder: u8, head: u8) -> Option<&Recorded> {
        self.tracks
            .iter()
            .find(|t| t.cylinder == cylinder && t.head == head)
    }

    /// The track at `cylinder` on `head`, if the archive has it.
    #[must_use]
    pub fn track(&self, cylinder: u8, head: u8) -> Option<Track> {
        self.recorded(cylinder, head).map(|t| t.track)
    }

    /// Reads the data of the sector in slot `slot` into `out`, as much as
    /// both hold.
    pub fn read(
        &self,
        cylinder: u8,
        head: u8,
        slot: usize,
        out: &mut [u8],
    ) -> Result<(), MediaError> {
        let track = self
            .recorded(cylinder, head)
            .ok_or(MediaError::OutOfRange)?;
        let sector = track
            .track
            .sectors()
            .get(slot)
            .ok_or(MediaError::OutOfRange)?;
        let start = track
            .data
            .get(slot)
            .copied()
            .flatten()
            .ok_or(MediaError::Failed)? as usize;
        let len = out.len().min(sector.id.bytes());
        out[..len].copy_from_slice(&self.store[start..start + len]);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A sector of a test archive: `(record, kind, data)`.
    pub(crate) type SectorRecord<'a> = (u8, u8, &'a [u8]);
    /// A track of a test archive: `(mode, cylinder, head, size code,
    /// sectors)`.
    pub(crate) type TrackRecord<'a> = (u8, u8, u8, u8, &'a [SectorRecord<'a>]);

    /// An archive of the given tracks.
    pub(crate) fn archive(tracks: &[TrackRecord<'_>]) -> Vec<u8> {
        let mut out = b"IMD 1.18: 01/01/1990 00:00:00\r\nTest disk\x1A".to_vec();
        for &(mode, cylinder, head, size, sectors) in tracks {
            out.extend([
                mode,
                cylinder,
                head,
                u8::try_from(sectors.len()).unwrap(),
                size,
            ]);
            out.extend(sectors.iter().map(|s| s.0));
            for &(_, kind, data) in sectors {
                out.push(kind);
                out.extend_from_slice(data);
            }
        }
        out
    }

    #[test]
    fn an_archive_keeps_its_ids_marks_and_compressed_sectors() {
        let full = [0x11u8; 256];
        let bytes = archive(&[(
            5,
            0,
            0,
            1,
            &[(3, 1, &full), (1, 2, &[0xE5]), (2, 4, &[0x00]), (9, 0, &[])],
        )]);
        let image = Image::parse(&bytes).unwrap();
        let track = image.track(0, 0).unwrap();
        let ids: Vec<u8> = track.sectors().iter().map(|s| s.id.r).collect();
        assert_eq!(ids, [3, 1, 2, 9], "the interleave as recorded");
        assert!(track.sectors()[2].deleted && !track.sectors()[0].deleted);
        assert!(!track.sectors()[3].readable);
        assert_eq!(
            (track.encoding, track.density),
            (Encoding::Mfm, Density::Double)
        );
        let mut out = [0u8; 256];
        image.read(0, 0, 1, &mut out).unwrap();
        assert_eq!(out, [0xE5; 256], "a compressed sector expands to its fill");
        assert_eq!(image.read(0, 0, 3, &mut out), Err(MediaError::Failed));
        assert_eq!(image.cylinders(), 1);
    }

    #[test]
    fn maps_and_size_tables_override_the_track_defaults() {
        let mut bytes = b"IMD 1.18\x1A".to_vec();
        bytes.extend([3, 5, 0xC1, 2, 0xFF, 1, 2, 7, 7, 0, 1]);
        bytes.extend(512u16.to_le_bytes());
        bytes.extend(1024u16.to_le_bytes());
        bytes.extend([2, 0xAA, 6, 0xBB]);
        let image = Image::parse(&bytes).unwrap();
        let track = image.track(5, 1).unwrap();
        assert_eq!(
            track.sectors()[0].id,
            SectorId {
                c: 7,
                h: 0,
                r: 1,
                n: 2
            }
        );
        assert_eq!(
            track.sectors()[1].id,
            SectorId {
                c: 7,
                h: 1,
                r: 2,
                n: 3
            }
        );
        assert!(track.sectors()[1].bad);
        assert_eq!(track.density, Density::High);
    }

    #[test]
    fn hostile_archives_are_refused() {
        assert_eq!(Image::parse(b"NOT AN IMAGE").err(), Some(ImdError::NotImd));
        assert_eq!(
            Image::parse(b"IMD no terminator").err(),
            Some(ImdError::Truncated)
        );
        assert_eq!(
            Image::parse(&archive(&[(9, 0, 0, 2, &[])])).err(),
            Some(ImdError::Malformed)
        );
        let mut cut = archive(&[(5, 0, 0, 2, &[(1, 1, &[0; 512])])]);
        cut.truncate(cut.len() - 10);
        assert_eq!(Image::parse(&cut).err(), Some(ImdError::Truncated));
        let twice = archive(&[(5, 0, 0, 2, &[(1, 2, &[0])]), (5, 0, 0, 2, &[(1, 2, &[0])])]);
        assert_eq!(
            Image::parse(&twice).err(),
            Some(ImdError::Malformed),
            "a track recorded twice"
        );
        let sectors: Vec<SectorRecord<'_>> = (0..64).map(|r| (r, 2u8, &[0u8][..])).collect();
        let tracks: Vec<TrackRecord<'_>> = (0..10).map(|c| (5, c, 0, 6, &sectors[..])).collect();
        assert_eq!(
            Image::parse(&archive(&tracks)).err(),
            Some(ImdError::TooLarge),
            "fill bytes cannot expand past a floppy"
        );
    }
}
