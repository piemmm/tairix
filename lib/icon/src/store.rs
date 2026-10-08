//! The persistent thumbnail store: a picture decoded once per version of a
//! file, kept across runs in one blob of the file manager's own bulk store
//! (`plans/FILES-INTERACTION.md` FI25).
//!
//! A key is the file's identity and content generation with the way its
//! format is read, so a changed file is a different key and a stale picture
//! cannot be served; a file whose volume keeps no generation is never stored.
//! The blob is a header naming the side, a table of slot headers, and the
//! slots: a key hashes to a four-way set, a lookup reads that set's headers and
//! then one payload, and an insert takes an empty way or the set's oldest. A
//! slot header carries a checksum over itself and its payload, written after
//! the payload, so a torn write — a crash mid-slot, or two instances writing
//! one way at once — reads as an empty way; a header for another side or
//! version reformats the blob.

use tairix_abi::fs::FileId;
use tairix_geometry::Rect;
use tairix_hash::FastHash;

use crate::picture::Fitted;
use crate::thumbnail::{ArtworkDocument, DocumentStamp, Reading};

/// Positioned I/O on the blob the store lives in.
pub trait StoreFile {
    /// Fill all of `into` from `offset`, answering whether it was filled.
    fn read_exact_at(&mut self, offset: u64, into: &mut [u8]) -> bool;

    /// Write all of `from` at `offset`, answering whether it was written.
    fn write_all_at(&mut self, offset: u64, from: &[u8]) -> bool;

    /// Make the blob `len` bytes long, answering whether it is.
    fn set_len(&mut self, len: u64) -> bool;

    /// The blob's length in bytes, or `None` when it cannot be learned.
    fn byte_len(&mut self) -> Option<u64>;
}

/// What a stored picture is the picture of: one version of one file, read one
/// way.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StoreKey {
    id: FileId,
    content_gen: u64,
    reading: Reading,
}

impl StoreKey {
    /// The key of the version `stamp` names, read as `reading`, or `None` for
    /// a file whose volume names no exact version of it.
    #[must_use]
    pub fn of(stamp: DocumentStamp, reading: Reading) -> Option<Self> {
        (stamp.content_gen != 0 && !stamp.id.is_none()).then_some(Self {
            id: stamp.id,
            content_gen: stamp.content_gen,
            reading,
        })
    }

    fn reading_byte(self) -> u8 {
        match self.reading {
            Reading::Signature => 1,
            Reading::Sprite => 2,
        }
    }

    /// The set this key lives in, of `sets`. Stable across runs: the seed is
    /// fixed, because a picture stored by one run is found by the next.
    fn set(self, sets: u32) -> u64 {
        let mut bytes = [0u8; 33];
        bytes[..16].copy_from_slice(&self.id.volume);
        bytes[16..24].copy_from_slice(&self.id.node.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.content_gen.to_le_bytes());
        bytes[32] = self.reading_byte();
        FastHash::hash_bytes(SET_SEED, &bytes) % u64::from(sets.max(1))
    }
}

/// The store's own constants: the blob layout below is this version's.
const MAGIC: [u8; 8] = *b"TXTHUMBS";
const VERSION: u32 = 1;
const WAYS: u32 = 4;
const SET_SEED: u64 = 0x7468_756d_6273_746f;
const HEADER_LEN: usize = 64;
const SLOT_HEADER_LEN: usize = 64;
/// Where the payloads begin past the table: a page boundary, so a payload
/// write never shares a page with the table a lookup reads.
const PAYLOAD_ALIGN: u64 = 4096;

// Slot header field offsets.
const S_VOLUME: usize = 0;
const S_NODE: usize = 16;
const S_GEN: usize = 24;
const S_READING: usize = 32;
const S_BOUNDS: usize = 36;
const S_ORDER: usize = 52;
const S_CRC: usize = 60;

/// A store of pictures `side` pixels square in one blob.
pub struct ThumbnailStore<F: StoreFile> {
    file: F,
    side: u32,
    sets: u32,
    payload_len: usize,
    payload_base: u64,
}

/// A set's raw slot headers, and each as it decodes.
type SetHeaders = ([[u8; SLOT_HEADER_LEN]; WAYS as usize], [Way; WAYS as usize]);

/// One way of a set, as its header reads.
#[derive(Copy, Clone)]
struct Way {
    key: Option<StoreKey>,
    bounds: Rect,
    order: u64,
    crc: u32,
}

impl<F: StoreFile> ThumbnailStore<F> {
    /// The store in `file` for pictures `side` pixels square, drawn by
    /// decoder `revision`, within `ceiling` bytes: adopted when its header is
    /// this layout's for that side, revision and size and the blob is as long
    /// as the layout, and formatted afresh otherwise. `None` when the ceiling
    /// holds no set, or the blob refuses the format.
    pub fn open(mut file: F, side: u32, revision: u32, ceiling: u64) -> Option<Self> {
        let payload_len = usize::try_from(side).ok()?.checked_pow(2)?.checked_mul(4)?;
        let per_slot = u64::try_from(SLOT_HEADER_LEN + payload_len).ok()?;
        let room = ceiling.checked_sub(HEADER_LEN as u64 + PAYLOAD_ALIGN)?;
        let sets = u32::try_from(room / per_slot / u64::from(WAYS)).ok()?;
        if side == 0 || sets == 0 {
            return None;
        }
        let table_len = u64::from(sets) * u64::from(WAYS) * SLOT_HEADER_LEN as u64;
        let payload_base = (HEADER_LEN as u64 + table_len).div_ceil(PAYLOAD_ALIGN) * PAYLOAD_ALIGN;
        let header = Self::header(side, revision, sets);
        let total =
            payload_base + u64::from(sets) * u64::from(WAYS) * u64::try_from(payload_len).ok()?;
        let mut held = [0u8; HEADER_LEN];
        if !file.read_exact_at(0, &mut held) || held != header || file.byte_len()? < total {
            // Emptied first, so nothing of another layout survives the change.
            if !file.set_len(0) || !file.set_len(total) || !file.write_all_at(0, &header) {
                return None;
            }
        }
        Some(Self {
            file,
            side,
            sets,
            payload_len,
            payload_base,
        })
    }

    /// The side, in pixels, of every picture this store holds.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// The picture of the open `document`, read as `reading`: the stored one
    /// when this version of it is held, else what `decode` produces — kept
    /// only when the document is still the same version once decoded, so a
    /// decode that raced a write is drawn but never stored.
    pub fn serve(
        &mut self,
        reading: Reading,
        document: &mut dyn ArtworkDocument,
        decode: impl FnOnce(&mut dyn ArtworkDocument) -> Option<Fitted>,
    ) -> Option<Fitted> {
        let stamp = document.stamp();
        let Some(key) = StoreKey::of(stamp, reading) else {
            return decode(document);
        };
        if let Some(found) = self.find(key) {
            return Some(found);
        }
        let fitted = decode(document)?;
        if document.restamp() == Some(stamp) {
            self.insert(key, &fitted);
        }
        Some(fitted)
    }

    /// The picture stored under `key`, if a way of its set holds it whole.
    pub fn find(&mut self, key: StoreKey) -> Option<Fitted> {
        let set = key.set(self.sets);
        let (headers, ways) = self.read_set(set)?;
        let (way, found) = ways
            .iter()
            .enumerate()
            .find(|(_, way)| way.key == Some(key))?;
        let mut pixels = tairix_util::fallible::filled(self.payload_len, 0u8)?;
        if !self
            .file
            .read_exact_at(self.payload_at(set, way)?, &mut pixels)
        {
            return None;
        }
        (slot_crc(&headers[way], &pixels) == found.crc).then_some(Fitted {
            pixels,
            bounds: found.bounds,
        })
    }

    /// Keep `picture` under `key`, in an empty way of its set or in place of
    /// the set's oldest. A picture not this store's shape is refused.
    pub fn insert(&mut self, key: StoreKey, picture: &Fitted) -> bool {
        let square = Rect::new(0, 0, self.side, self.side);
        if picture.pixels.len() != self.payload_len
            || picture.bounds.is_empty()
            || picture.bounds.intersection(&square) != picture.bounds
        {
            return false;
        }
        let set = key.set(self.sets);
        let Some((_, ways)) = self.read_set(set) else {
            return false;
        };
        let newest = ways.iter().map(|way| way.order).max().unwrap_or(0);
        let way = ways
            .iter()
            .position(|way| way.key == Some(key))
            .or_else(|| ways.iter().position(|way| way.key.is_none()))
            .or_else(|| {
                ways.iter()
                    .enumerate()
                    .min_by_key(|(_, way)| way.order)
                    .map(|(index, _)| index)
            })
            .unwrap_or(0);
        let mut header = [0u8; SLOT_HEADER_LEN];
        header[S_VOLUME..S_VOLUME + 16].copy_from_slice(&key.id.volume);
        header[S_NODE..S_NODE + 8].copy_from_slice(&key.id.node.to_le_bytes());
        header[S_GEN..S_GEN + 8].copy_from_slice(&key.content_gen.to_le_bytes());
        header[S_READING] = key.reading_byte();
        let bounds = picture.bounds;
        for (at, value) in [
            bounds.left().unsigned_abs(),
            bounds.top().unsigned_abs(),
            bounds.width,
            bounds.height,
        ]
        .into_iter()
        .enumerate()
        {
            let field = S_BOUNDS + at * 4;
            header[field..field + 4].copy_from_slice(&value.to_le_bytes());
        }
        header[S_ORDER..S_ORDER + 8].copy_from_slice(&newest.saturating_add(1).to_le_bytes());
        let crc = slot_crc(&header, &picture.pixels);
        header[S_CRC..S_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        // The payload goes down before the header that vouches for it, so a
        // write cut short between them leaves a header whose checksum fails.
        let Some(payload_at) = self.payload_at(set, way) else {
            return false;
        };
        self.file.write_all_at(payload_at, &picture.pixels)
            && self.file.write_all_at(Self::header_at(set, way), &header)
    }

    /// The header this version writes for pictures `side` pixels square in
    /// `sets` sets.
    fn header(side: u32, revision: u32, sets: u32) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(&MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&side.to_le_bytes());
        header[16..20].copy_from_slice(&revision.to_le_bytes());
        header[20..24].copy_from_slice(&sets.to_le_bytes());
        header[24..28].copy_from_slice(&WAYS.to_le_bytes());
        let crc = tairix_crc32c::checksum(&header[..28]);
        header[28..32].copy_from_slice(&crc.to_le_bytes());
        header
    }

    /// Read the headers of `set`'s ways, raw and decoded.
    fn read_set(&mut self, set: u64) -> Option<SetHeaders> {
        let mut raw = [[0u8; SLOT_HEADER_LEN]; WAYS as usize];
        if !self
            .file
            .read_exact_at(Self::header_at(set, 0), raw.as_flattened_mut())
        {
            return None;
        }
        let ways = raw.map(|header| decode_way(&header, self.side));
        Some((raw, ways))
    }

    fn header_at(set: u64, way: usize) -> u64 {
        HEADER_LEN as u64 + (set * u64::from(WAYS) + way as u64) * SLOT_HEADER_LEN as u64
    }

    fn payload_at(&self, set: u64, way: usize) -> Option<u64> {
        let slot = set * u64::from(WAYS) + u64::try_from(way).ok()?;
        Some(self.payload_base + slot * u64::try_from(self.payload_len).ok()?)
    }
}

/// One way's header decoded: an empty way, or a malformed one, holds no key.
fn decode_way(header: &[u8; SLOT_HEADER_LEN], side: u32) -> Way {
    let empty = Way {
        key: None,
        bounds: Rect::EMPTY,
        order: 0,
        crc: 0,
    };
    let u32_at = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let u64_at = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&header[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    let reading = match header[S_READING] {
        1 => Reading::Signature,
        2 => Reading::Sprite,
        _ => return empty,
    };
    let (Ok(x), Ok(y)) = (
        i32::try_from(u32_at(S_BOUNDS)),
        i32::try_from(u32_at(S_BOUNDS + 4)),
    ) else {
        return empty;
    };
    let bounds = Rect::new(x, y, u32_at(S_BOUNDS + 8), u32_at(S_BOUNDS + 12));
    if bounds.is_empty() || bounds.intersection(&Rect::new(0, 0, side, side)) != bounds {
        return empty;
    }
    let mut volume = [0u8; 16];
    volume.copy_from_slice(&header[S_VOLUME..S_VOLUME + 16]);
    Way {
        key: Some(StoreKey {
            id: FileId {
                volume,
                node: u64_at(S_NODE),
            },
            content_gen: u64_at(S_GEN),
            reading,
        }),
        bounds,
        order: u64_at(S_ORDER),
        crc: u32_at(S_CRC),
    }
}

/// The checksum a slot header carries: over the header up to the checksum
/// field and the payload it vouches for.
fn slot_crc(header: &[u8; SLOT_HEADER_LEN], payload: &[u8]) -> u32 {
    !tairix_crc32c::update(
        tairix_crc32c::update(0xFFFF_FFFF, &header[..S_CRC]),
        payload,
    )
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
