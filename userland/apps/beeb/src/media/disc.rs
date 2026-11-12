//! Floppy discs as the drive sees them: sides of tracks, each a ring of FM
//! recording holding sectors where a formatter put them — an ID field the
//! controller searches for, then the data field it transfers.
//!
//! SSD and DSD images, the plain sector dumps of single- and double-sided
//! Acorn DFS discs, decode to ten 256-byte sectors per track numbered 0–9 and
//! placed as the DFS formats a track. They encode back to the image's own
//! length, longer only as far as a guest wrote past it, reporting any track
//! holding what a sector dump cannot record.

use alloc::vec;
use alloc::vec::Vec;

/// Bytes in a DFS sector.
pub const SECTOR_BYTES: usize = DFS_SECTOR as usize;
/// Sectors on a DFS track.
pub const SECTORS_PER_TRACK: usize = 10;
/// Bytes a DFS track holds in a sector dump.
pub const TRACK_BYTES: usize = SECTOR_BYTES * SECTORS_PER_TRACK;
/// The innermost track a drive's head reaches: 80 tracks, and the few spare
/// beyond.
pub const LAST_TRACK: u8 = 83;
/// The most tracks a disc has.
pub const MAX_TRACKS: usize = LAST_TRACK as usize + 1;

/// Bytes of FM recording around a track: one revolution at 300 rpm, 64 µs a
/// byte.
pub const REVOLUTION_BYTES: u16 = 3125;
/// An ID field: its address mark, the track, head, sector and size bytes, and
/// its CRC.
pub const ID_FIELD_BYTES: u16 = 7;
/// From an ID field's address mark to its data field's: the ID field, then
/// the 8271's fixed gap 2 of eleven fill and six sync bytes.
pub const ID_TO_DATA_MARK: u16 = ID_FIELD_BYTES + 17;
/// A data field's address mark, before its data, and its CRC, after.
pub const DATA_MARK_BYTES: u16 = 1;
/// The CRC closing a field.
pub const CRC_BYTES: u16 = 2;
/// What a gap holds between its sync bytes and the fields.
pub const GAP_FILL: u8 = 0xFF;

/// A DFS sector's data.
const DFS_SECTOR: u16 = 256;
/// The DFS format's gap 1 and gap 3, each ending in its six sync bytes.
const DFS_GAP_1: u16 = 16 + 6;
const DFS_GAP_3: u16 = 21 + 6;
/// From one DFS sector's ID field to the next's.
const DFS_STRIDE: u16 = ID_TO_DATA_MARK + DATA_MARK_BYTES + DFS_SECTOR + CRC_BYTES + DFS_GAP_3;
/// The bytes of a data field whose ID field gives size code `code`.
#[must_use]
pub const fn field_size(code: u8) -> usize {
    128 << (code & 0x07)
}

/// The longest data field a size code names.
pub const LONGEST_FIELD: usize = field_size(0x07);
/// The shortest data field a sector holds: every one starts as a field of a
/// size code's length, and a write cut short keeps the longer of the old and
/// what it wrote.
const SHORTEST_FIELD: usize = field_size(0);
/// The most sectors that fit around a track, each the shortest it can be.
const MAX_SECTORS: usize = REVOLUTION_BYTES as usize
    / ((ID_TO_DATA_MARK + DATA_MARK_BYTES + CRC_BYTES) as usize + SHORTEST_FIELD);

/// The size code of a 256-byte sector: 128 << code.
const DFS_SIZE_CODE: u8 = 1;

/// The address marks opening a data field and a deleted one.
const DATA_MARK: u8 = 0xFB;
const DELETED_DATA_MARK: u8 = 0xF8;

/// An ID field: what the controller compares while it looks for a sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectorId {
    /// The track the field claims.
    pub track: u8,
    /// The head the field claims.
    pub head: u8,
    /// The sector number.
    pub sector: u8,
    /// The data field's size, `128 << size`.
    pub size: u8,
}

/// One sector: where it lies, its ID field and its data field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sector {
    /// Bytes from the index to the ID field's address mark, below
    /// `REVOLUTION_BYTES`.
    pub position: u16,
    /// The ID field.
    pub id: SectorId,
    /// The data field's contents.
    pub data: Vec<u8>,
    /// The data field carries a deleted-data mark.
    pub deleted: bool,
    /// The data field's recorded CRC does not match it, as a write cut short
    /// leaves one.
    pub crc_error: bool,
}

impl Sector {
    /// The data field's byte `offset` as the head reads it, the data then its
    /// recorded CRC then gap fill: a read longer than the field runs on into
    /// what follows it.
    #[must_use]
    pub fn field_byte(&self, offset: usize) -> u8 {
        match offset.checked_sub(self.data.len()) {
            None => self.data[offset],
            Some(past @ 0..=1) => self.recorded_crc()[past],
            Some(_) => GAP_FILL,
        }
    }

    /// The CRC recorded after the data, wrong when `crc_error` says so.
    #[must_use]
    pub(crate) fn recorded_crc(&self) -> [u8; 2] {
        let crc = field_crc(self.deleted, self.data.iter().copied());
        if self.crc_error { !crc } else { crc }.to_be_bytes()
    }

    /// Bytes from the ID field's address mark to the end of the data field.
    #[must_use]
    pub(crate) fn extent(&self) -> usize {
        usize::from(ID_TO_DATA_MARK + DATA_MARK_BYTES + CRC_BYTES) + self.data.len()
    }
}

/// The CRC an FM data field carries: CRC-16/CCITT (x¹⁶ + x¹² + x⁵ + 1, from
/// all ones, most significant bit first) over its address mark and its data.
pub fn field_crc(deleted: bool, data: impl IntoIterator<Item = u8>) -> u16 {
    let mark = if deleted {
        DELETED_DATA_MARK
    } else {
        DATA_MARK
    };
    core::iter::once(mark).chain(data).fold(0xFFFF, crc_ccitt)
}

/// One byte into a CRC-16/CCITT.
fn crc_ccitt(crc: u16, byte: u8) -> u16 {
    (0..8).fold(crc ^ (u16::from(byte) << 8), |crc, _| {
        if crc & 0x8000 == 0 {
            crc << 1
        } else {
            (crc << 1) ^ 0x1021
        }
    })
}

/// One track's sectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    /// The sectors.
    pub sectors: Vec<Sector>,
}

impl Track {
    /// A track as the DFS formats one, its data from a sector dump's slice,
    /// zero-padded where the dump ends.
    fn dfs(track: usize, side: usize, bytes: &[u8]) -> Self {
        let sectors = (0..SECTORS_PER_TRACK)
            .zip((DFS_GAP_1..).step_by(usize::from(DFS_STRIDE)))
            .map(|(sector, position)| {
                let mut data = vec![0; SECTOR_BYTES];
                if let Some(source) = bytes.get(sector * SECTOR_BYTES..) {
                    let len = source.len().min(SECTOR_BYTES);
                    data[..len].copy_from_slice(&source[..len]);
                }
                Sector {
                    position,
                    id: SectorId {
                        track: u8::try_from(track).unwrap_or(u8::MAX),
                        head: u8::try_from(side).unwrap_or(0),
                        sector: u8::try_from(sector).unwrap_or(0),
                        size: DFS_SIZE_CODE,
                    },
                    data,
                    deleted: false,
                    crc_error: false,
                }
            })
            .collect();
        Self { sectors }
    }

    /// Record a new data field over sector `index`'s: a field longer than the
    /// old one runs over the ID fields that follow it, and their sectors are
    /// gone.
    pub fn write_data(&mut self, index: usize, data: Vec<u8>, deleted: bool, crc_error: bool) {
        let Some(sector) = self.sectors.get_mut(index) else {
            return;
        };
        sector.data = data;
        sector.deleted = deleted;
        sector.crc_error = crc_error;
        let start = usize::from(sector.position);
        let end = start + sector.extent();
        let revolution = usize::from(REVOLUTION_BYTES);
        let mut at = 0;
        self.sectors.retain(|other| {
            let here = at;
            at += 1;
            let position = usize::from(other.position);
            here == index || !((start < position && position < end) || position + revolution < end)
        });
    }
}

/// How an image was laid out on its file, which is how it is written back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    /// Single-sided: tracks in order.
    Ssd,
    /// Double-sided: side 0 and side 1 of each track alternating.
    Dsd,
}

impl ImageFormat {
    const fn sides(self) -> usize {
        match self {
            Self::Ssd => 1,
            Self::Dsd => 2,
        }
    }

    /// The largest image of the format: every side of every track.
    const fn max_bytes(self) -> usize {
        self.sides() * MAX_TRACKS * TRACK_BYTES
    }
}

/// Why an image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscError {
    /// The file is empty.
    Empty,
    /// The file is larger than any disc of its format.
    TooLarge,
}

/// A disc in a drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disc {
    sides: Vec<Vec<Track>>,
    write_protected: bool,
    /// Tracks written since the disc was last saved, per side.
    dirty: Vec<Vec<bool>>,
    /// The length of the image it was decoded from, or last saved as.
    image_len: usize,
}

/// A disc encoded back into its image format, and where the format could not
/// keep all it held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// The image, as long as the one decoded or last saved, or as far past
    /// that as a guest wrote.
    pub image: Vec<u8>,
    /// The first side and track holding what a sector dump cannot record:
    /// other than ten 256-byte sectors numbered 0–9 with the track's own
    /// number, or one with a deleted-data mark or a CRC error.
    pub lost: Option<(usize, usize)>,
}

impl Disc {
    /// Decode an image of `format`. A file that ends part way through a disc
    /// is the start of one, the rest formatted as the DFS formats it and
    /// holding zeros, as a sector dump's missing tail means; the track count
    /// is the image's, rounded up to whole tracks and at least 40.
    pub fn decode(
        bytes: &[u8],
        format: ImageFormat,
        write_protected: bool,
    ) -> Result<Self, DiscError> {
        if bytes.is_empty() {
            return Err(DiscError::Empty);
        }
        if bytes.len() > format.max_bytes() {
            return Err(DiscError::TooLarge);
        }
        let sides = format.sides();
        let tracks = bytes.len().div_ceil(TRACK_BYTES * sides).max(40);
        let mut disc = Self {
            sides: vec![Vec::with_capacity(tracks); sides],
            write_protected,
            dirty: vec![vec![false; tracks]; sides],
            image_len: bytes.len(),
        };
        for track in 0..tracks {
            for side in 0..sides {
                let start = (track * sides + side) * TRACK_BYTES;
                disc.sides[side].push(Track::dfs(track, side, bytes.get(start..).unwrap_or(&[])));
            }
        }
        Ok(disc)
    }

    /// Encode the disc back into its image format: each track's sectors 0–9
    /// in order, a missing sector as zeros, to the image's length.
    #[must_use]
    pub fn encode(&self) -> Encoded {
        let len = self.encoded_len();
        let mut image = Vec::with_capacity(len);
        let mut lost = None;
        'dump: for track in 0..self.tracks() {
            for (side, tracks) in self.sides.iter().enumerate() {
                if image.len() >= len {
                    break 'dump;
                }
                let mut bytes = [0u8; TRACK_BYTES];
                if let Some(held) = tracks.get(track) {
                    held.dump(&mut bytes);
                    if lost.is_none() && !held.is_dump(track) {
                        lost = Some((side, track));
                    }
                }
                let keep = (len - image.len()).min(TRACK_BYTES);
                image.extend_from_slice(&bytes[..keep]);
            }
        }
        Encoded { image, lost }
    }

    /// The bytes the image runs to: its length when decoded or last saved,
    /// or the end of the furthest track written past that.
    fn encoded_len(&self) -> usize {
        let sides = self.sides.len();
        let written = (0..self.tracks())
            .rev()
            .flat_map(|track| (0..sides).rev().map(move |side| (track, side)))
            .find(|&(track, side)| self.dirty[side][track])
            .map_or(0, |(track, side)| (track * sides + side + 1) * TRACK_BYTES);
        self.image_len.max(written)
    }

    /// How many sides it has.
    #[must_use]
    pub fn sides(&self) -> usize {
        self.sides.len()
    }

    /// How many tracks each side has.
    #[must_use]
    pub fn tracks(&self) -> usize {
        self.sides.first().map_or(0, Vec::len)
    }

    /// Whether the write-protect notch is covered.
    #[must_use]
    pub const fn write_protected(&self) -> bool {
        self.write_protected
    }

    /// A track, if the side has it.
    #[must_use]
    pub fn track(&self, side: usize, track: usize) -> Option<&Track> {
        self.sides.get(side)?.get(track)
    }

    /// A track to write to, if the side has it; it is marked written.
    pub fn track_mut(&mut self, side: usize, track: usize) -> Option<&mut Track> {
        let slot = self.dirty.get_mut(side)?.get_mut(track)?;
        *slot = true;
        self.sides.get_mut(side)?.get_mut(track)
    }

    /// Whether any track has been written since the disc was last saved.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.iter().flatten().any(|&dirty| dirty)
    }

    /// Note that the disc has been saved as [`encode`](Self::encode) gave it.
    pub fn mark_saved(&mut self) {
        self.image_len = self.encoded_len();
        for side in &mut self.dirty {
            side.fill(false);
        }
    }
}

impl Track {
    /// Write the track's sectors 0–9 into `bytes` as a sector dump holds
    /// them, each as far as its data runs.
    fn dump(&self, bytes: &mut [u8; TRACK_BYTES]) {
        for sector in &self.sectors {
            let number = usize::from(sector.id.sector);
            if number < SECTORS_PER_TRACK {
                let data = &sector.data[..sector.data.len().min(SECTOR_BYTES)];
                bytes[number * SECTOR_BYTES..][..data.len()].copy_from_slice(data);
            }
        }
    }

    /// Whether a sector dump records the whole track numbered `number`: ten
    /// intact 256-byte sectors numbered 0–9, each naming the track.
    fn is_dump(&self, number: usize) -> bool {
        let mut seen = 0u16;
        self.sectors.len() == SECTORS_PER_TRACK
            && self.sectors.iter().all(|sector| {
                let id = sector.id;
                let bit = 1u16.checked_shl(u32::from(id.sector)).unwrap_or(0);
                let fresh = seen & bit == 0;
                seen |= bit;
                fresh
                    && usize::from(id.sector) < SECTORS_PER_TRACK
                    && usize::from(id.track) == number
                    && id.size == DFS_SIZE_CODE
                    && sector.data.len() == SECTOR_BYTES
                    && !sector.deleted
                    && !sector.crc_error
            })
    }
}

crate::snapshot::fields!(SectorId {
    track,
    head,
    sector,
    size
});

impl crate::snapshot::Save for Sector {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.position);
        w.put(&self.id);
        w.bytes(&self.data);
        w.put(&self.deleted);
        w.put(&self.crc_error);
    }
}

impl crate::snapshot::Load for Sector {
    /// Its ID field within the revolution, and a data field no shorter than
    /// any sector's and no longer than any size code's.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let position = r.get()?;
        let id = r.get()?;
        let data = r.bytes_at_most(LONGEST_FIELD)?;
        crate::snapshot::Reader::check(
            position < REVOLUTION_BYTES && data.len() >= SHORTEST_FIELD,
        )?;
        Ok(Self {
            position,
            id,
            data: data.to_vec(),
            deleted: r.get()?,
            crc_error: r.get()?,
        })
    }
}

impl crate::snapshot::Save for Track {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.sectors);
    }
}

impl crate::snapshot::Load for Track {
    /// No more sectors than fit around a track, none running into the next
    /// one's ID field round the revolution, though a single one may run on
    /// over its own.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let sectors: Vec<Sector> = r.get_at_most(MAX_SECTORS)?;
        let mut order: Vec<&Sector> = sectors.iter().collect();
        order.sort_unstable_by_key(|sector| sector.position);
        let revolution = usize::from(REVOLUTION_BYTES);
        let apart = order.len() < 2
            || order.iter().enumerate().all(|(index, sector)| {
                // The last runs on round the revolution to the first.
                let (next, wrap) = order
                    .get(index + 1)
                    .map_or((order[0], revolution), |&next| (next, 0));
                usize::from(sector.position) + sector.extent() <= usize::from(next.position) + wrap
            });
        crate::snapshot::Reader::check(apart)?;
        Ok(Self { sectors })
    }
}

impl crate::snapshot::Save for Disc {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&u32::try_from(self.sides.len()).unwrap_or(u32::MAX));
        for side in &self.sides {
            w.put(side);
        }
        for side in &self.dirty {
            w.put(side);
        }
        w.put(&self.write_protected);
        w.put(&self.image_len);
    }
}

impl crate::snapshot::Load for Disc {
    /// One side or two, each of the same number of tracks with a written
    /// flag apiece, and an image no longer than its format's.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        use crate::snapshot::Reader;
        let count = r.count_at_most(ImageFormat::Dsd.sides())?;
        Reader::check(count > 0)?;
        let mut sides = Vec::with_capacity(count);
        for _ in 0..count {
            let side: Vec<Track> = r.get_at_most(MAX_TRACKS)?;
            Reader::check(!side.is_empty())?;
            sides.push(side);
        }
        let tracks = sides[0].len();
        Reader::check(sides.iter().all(|side| side.len() == tracks))?;
        let mut dirty = Vec::with_capacity(count);
        for _ in 0..count {
            let side: Vec<bool> = r.get_at_most(tracks)?;
            Reader::check(side.len() == tracks)?;
            dirty.push(side);
        }
        let format = if count == 1 {
            ImageFormat::Ssd
        } else {
            ImageFormat::Dsd
        };
        let disc = Self {
            sides,
            write_protected: r.get()?,
            dirty,
            image_len: r.get()?,
        };
        Reader::check((1..=format.max_bytes()).contains(&disc.image_len))?;
        Ok(disc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(tracks: usize, sides: usize) -> Vec<u8> {
        (0..tracks * sides * TRACK_BYTES)
            .map(|i| u8::try_from((i * 7 + i / SECTOR_BYTES) % 251).unwrap())
            .collect()
    }

    #[test]
    fn the_crc_is_crc_16_ccitt_from_all_ones() {
        assert_eq!(b"123456789".iter().copied().fold(0xFFFF, crc_ccitt), 0x29B1);
        assert_eq!(field_crc(false, []), crc_ccitt(0xFFFF, DATA_MARK));
        assert_ne!(field_crc(false, [0; 4]), field_crc(true, [0; 4]));
    }

    #[test]
    fn an_ssd_decodes_to_ten_sectors_a_track_where_the_dfs_formats_them() {
        let image = dump(40, 1);
        let disc = Disc::decode(&image, ImageFormat::Ssd, false).unwrap();
        assert_eq!((disc.sides(), disc.tracks()), (1, 40));
        let track = disc.track(0, 3).unwrap();
        let positions: Vec<u16> = track.sectors.iter().map(|sector| sector.position).collect();
        assert_eq!(
            positions,
            [22, 332, 642, 952, 1262, 1572, 1882, 2192, 2502, 2812]
        );
        let last = &track.sectors[9];
        assert!(usize::from(last.position) + last.extent() <= usize::from(REVOLUTION_BYTES));
        assert_eq!(
            last.id,
            SectorId {
                track: 3,
                head: 0,
                sector: 9,
                size: 1
            }
        );
        assert_eq!(
            last.data[..],
            image[3 * TRACK_BYTES + 9 * SECTOR_BYTES..4 * TRACK_BYTES]
        );
        assert_eq!(disc.encode(), Encoded { image, lost: None });
    }

    #[test]
    fn a_dsd_interleaves_its_sides_and_encodes_back() {
        let image = dump(80, 2);
        let disc = Disc::decode(&image, ImageFormat::Dsd, false).unwrap();
        assert_eq!((disc.sides(), disc.tracks()), (2, 80));
        let side_1 = disc.track(1, 5).unwrap();
        assert_eq!(side_1.sectors[0].id.head, 1);
        assert_eq!(
            side_1.sectors[0].data[..],
            image[(5 * 2 + 1) * TRACK_BYTES..][..SECTOR_BYTES]
        );
        assert_eq!(disc.encode(), Encoded { image, lost: None });
    }

    #[test]
    fn a_short_image_is_the_start_of_a_forty_track_disc() {
        let disc = Disc::decode(&[0xAA; 300], ImageFormat::Ssd, false).unwrap();
        assert_eq!(disc.tracks(), 40);
        let first = &disc.track(0, 0).unwrap().sectors;
        assert_eq!(first[0].data[..], [0xAA; SECTOR_BYTES]);
        assert_eq!(first[1].data[..44], [0xAA; 44]);
        assert_eq!(first[1].data[44..], [0; SECTOR_BYTES - 44]);
        assert_eq!(
            Disc::decode(&[], ImageFormat::Ssd, false),
            Err(DiscError::Empty)
        );
        assert_eq!(
            Disc::decode(
                &vec![0; MAX_TRACKS * TRACK_BYTES + 1],
                ImageFormat::Ssd,
                false
            ),
            Err(DiscError::TooLarge)
        );
    }

    #[test]
    fn writing_marks_a_track_dirty_until_saved() {
        let mut disc = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false).unwrap();
        assert!(!disc.is_dirty());
        disc.track_mut(0, 7).unwrap();
        assert!(disc.is_dirty());
        disc.mark_saved();
        assert!(!disc.is_dirty());
        assert!(disc.track_mut(0, 40).is_none());
    }

    /// The regression: every image encoded back to whole tracks, at least
    /// forty, so a short one grew on its first save.
    #[test]
    fn an_image_saves_at_its_own_length_or_as_far_as_was_written() {
        let mut disc = Disc::decode(&[0xAA; 300], ImageFormat::Ssd, false).unwrap();
        assert_eq!(disc.encode().image, [0xAA; 300]);
        disc.track_mut(0, 2).unwrap().sectors[5].data.fill(0x55);
        let written = disc.encode().image;
        assert_eq!(written.len(), 3 * TRACK_BYTES);
        assert_eq!(written[..300], [0xAA; 300]);
        assert_eq!(
            written[2 * TRACK_BYTES + 5 * SECTOR_BYTES..][..SECTOR_BYTES],
            [0x55; SECTOR_BYTES]
        );
        disc.mark_saved();
        assert_eq!(
            disc.encode().image.len(),
            3 * TRACK_BYTES,
            "saved, it stays that long"
        );
        let mut sides = Disc::decode(&dump(40, 2), ImageFormat::Dsd, false).unwrap();
        sides.track_mut(1, 39).unwrap();
        assert_eq!(sides.encode().image.len(), 80 * TRACK_BYTES);
    }

    /// The regression: a track a dump cannot hold was silently dropped.
    #[test]
    fn a_track_a_dump_cannot_hold_is_reported() {
        let mut disc = Disc::decode(&dump(40, 2), ImageFormat::Dsd, false).unwrap();
        assert_eq!(disc.encode().lost, None);
        disc.track_mut(1, 6).unwrap().sectors[3].deleted = true;
        disc.track_mut(0, 9).unwrap().sectors.pop();
        assert_eq!(disc.encode().lost, Some((1, 6)), "the first, in dump order");
        let mut renumbered = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false).unwrap();
        renumbered.track_mut(0, 4).unwrap().sectors[0].id.sector = 11;
        assert_eq!(renumbered.encode().lost, Some((0, 4)));
        let mut moved = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false).unwrap();
        moved.track_mut(0, 4).unwrap().sectors[0].position += 3;
        assert_eq!(
            moved.encode().lost,
            None,
            "where sectors lie is no part of a dump"
        );
    }

    fn track_snapshot(track: &Track) -> Result<Track, crate::snapshot::SnapshotError> {
        let mut w = crate::snapshot::Writer::new();
        w.section(*b"TRAK", |w| w.put(track));
        let sealed = w.finish().unwrap();
        let body = crate::snapshot::open(&sealed).unwrap();
        crate::snapshot::Reader::new(&body).section(*b"TRAK", crate::snapshot::Reader::get)
    }

    /// The regression: a restored track needed only distinct positions, so
    /// thousands of sectors could lie over one another.
    #[test]
    fn a_restored_track_holds_sectors_only_as_a_track_can() {
        let dfs = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false)
            .unwrap()
            .track(0, 0)
            .unwrap()
            .clone();
        assert_eq!(track_snapshot(&dfs), Ok(dfs.clone()));
        let mut long = dfs.clone();
        long.write_data(9, vec![2; 4096], false, false);
        assert_eq!(
            long.sectors.len(),
            1,
            "a field past a revolution runs over all the rest"
        );
        assert_eq!(track_snapshot(&long), Ok(long.clone()));
        let mut overlapping = dfs.clone();
        overlapping.sectors[4].position = overlapping.sectors[3].position + 10;
        assert!(track_snapshot(&overlapping).is_err());
        let mut short = dfs;
        short.sectors[0].data.truncate(SHORTEST_FIELD - 1);
        assert!(track_snapshot(&short).is_err());
        assert_eq!(MAX_SECTORS, 20);
    }

    #[test]
    fn a_field_read_past_its_data_runs_into_its_crc_then_the_gap() {
        let mut disc = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false).unwrap();
        let sector = disc.track_mut(0, 0).unwrap().sectors[0].clone();
        let crc = field_crc(false, sector.data.iter().copied()).to_be_bytes();
        assert_eq!([sector.field_byte(256), sector.field_byte(257)], crc);
        assert_eq!(sector.field_byte(258), GAP_FILL);
        let broken = Sector {
            crc_error: true,
            ..sector
        };
        assert_ne!(broken.recorded_crc(), crc);
    }

    #[test]
    fn a_data_field_written_longer_runs_over_the_ids_after_it() {
        let mut track = Disc::decode(&dump(40, 1), ImageFormat::Ssd, false)
            .unwrap()
            .track(0, 0)
            .unwrap()
            .clone();
        track.write_data(2, vec![1; 1024], true, false);
        let numbers: Vec<u8> = track
            .sectors
            .iter()
            .map(|sector| sector.id.sector)
            .collect();
        // 1024 bytes from sector 2's ID field reach past sectors 3, 4 and 5.
        assert_eq!(numbers, [0, 1, 2, 6, 7, 8, 9]);
        assert!(track.sectors[2].deleted);
        // The last sector's, written past the index, runs over the first ones.
        track.write_data(6, vec![2; 1500], false, false);
        let numbers: Vec<u8> = track
            .sectors
            .iter()
            .map(|sector| sector.id.sector)
            .collect();
        assert_eq!(numbers, [6, 7, 8, 9]);
    }
}
