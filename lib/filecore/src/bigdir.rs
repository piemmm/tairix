//! Big directories (ADFS E+/F+): the variable-length `SBPr`/`oven`
//! format with a name heap.
//!
//! Layout: a 28-byte header (sequence byte, version `0,0,0`, `SBPr`,
//! name length, directory size, entry count, name-heap size, parent,
//! then the directory name padded to a word), an array of 28-byte
//! entries, the name heap (each name `CR`-terminated and word-padded),
//! and an 8-byte tail (`oven`, sequence byte, two zero bytes, check
//! byte) at the very end of the directory. Reference: G. Holdsworth,
//! "Guide To Disc Formats".
//!
//! The engine streams through a [`DirStore`] — byte-addressed access to
//! the directory object — so a directory of any size is handled without
//! a size-proportional buffer.

use crate::bytes::{get_u32, put_u32};
use crate::dir::{
    accumulate_bytes, accumulate_words, bcd_increment, entries_in_order, fold_check, Object,
    MAX_NAME_LEN,
};
use crate::{Error, Fault, Structure};
use tairix_fsmeta::preset::acorn;

/// What a directory failing its checks is.
const BROKEN: Fault = Fault::Broken(Structure::Directory);

/// The `SBPr` header marker.
pub const BIG_START: [u8; 4] = *b"SBPr";
/// The `oven` tail marker.
pub const BIG_END: [u8; 4] = *b"oven";
/// Byte size of the fixed part of the header.
pub const BIG_HEADER_SIZE: u32 = 28;
/// Byte size of one big-directory entry.
pub const BIG_ENTRY_SIZE: u32 = 28;
/// Byte size of the tail.
pub const BIG_TAIL_SIZE: u32 = 8;
/// Big directories are sized in whole 2048-byte units.
pub const BIG_DIR_GRAIN: u32 = 2048;

/// Byte-addressed access to one directory object's bytes.
///
/// Implemented by the driver over the allocation map, so directory
/// logic never sees fragmentation.
pub trait DirStore {
    /// What the store's disc fails with.
    type Error;

    /// Read `buf.len()` bytes at `offset` within the directory object.
    ///
    /// # Errors
    ///
    /// * The directory broken if the range leaves the object.
    /// * The store's failure.
    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Error<Self::Error>>;

    /// Write `data` at `offset` within the directory object.
    ///
    /// # Errors
    ///
    /// * The directory broken if the range leaves the object.
    /// * The store's failure.
    fn write_at(&mut self, offset: u32, data: &[u8]) -> Result<(), Error<Self::Error>>;
}

/// A parsed big-directory header.
#[derive(Copy, Clone)]
pub struct BigHeader {
    /// Sequence byte.
    pub masseq: u8,
    /// Directory-name length in bytes (excluding the `CR` terminator).
    pub name_len: u32,
    /// Directory size in bytes (a multiple of [`BIG_DIR_GRAIN`]).
    pub size: u32,
    /// Number of entries.
    pub entries: u32,
    /// Name-heap size in bytes.
    pub names_size: u32,
    /// Parent indirect disc address.
    pub parent: u32,
}

impl BigHeader {
    /// Offset of the entry array (header + word-padded name + `CR`).
    #[must_use]
    pub fn entries_offset(&self) -> u32 {
        BIG_HEADER_SIZE + pad_name(self.name_len)
    }

    /// Offset of the name heap.
    #[must_use]
    pub fn heap_offset(&self) -> u32 {
        self.entries_offset() + self.entries * BIG_ENTRY_SIZE
    }

    /// Offset one past the used extent (header, name, entries, heap).
    #[must_use]
    pub fn used_end(&self) -> u32 {
        self.heap_offset() + self.names_size
    }

    /// Offset of the tail.
    #[must_use]
    pub fn tail_offset(&self) -> u32 {
        self.size - BIG_TAIL_SIZE
    }

    /// Offset of the table just below the tail that holds, in entry order,
    /// each entry's disc address, as RISC OS 5 keeps it.
    #[must_use]
    pub fn table_offset(&self) -> u32 {
        self.tail_offset().saturating_sub(4 * self.entries)
    }

    fn encode(&self) -> [u8; BIG_HEADER_SIZE as usize] {
        let mut raw = [0u8; BIG_HEADER_SIZE as usize];
        raw[0] = self.masseq;
        raw[4..8].copy_from_slice(&BIG_START);
        put_u32(&mut raw, 8, self.name_len);
        put_u32(&mut raw, 12, self.size);
        put_u32(&mut raw, 16, self.entries);
        put_u32(&mut raw, 20, self.names_size);
        put_u32(&mut raw, 24, self.parent);
        raw
    }
}

/// Heap bytes a name of `len` occupies (`CR` terminator, word padded).
fn pad_name(len: u32) -> u32 {
    (len + 1 + 3) & !3
}

/// The big-directory engine: a validated header plus streaming
/// operations over a [`DirStore`].
pub struct BigDir {
    /// The validated header.
    pub header: BigHeader,
}

impl BigDir {
    /// Read and validate the directory rooted in `store`.
    ///
    /// `object_limit` is the directory object's allocation; the header
    /// size must fit inside it (the allocation may round up past the
    /// directory, and a held node id may carry a stale pre-growth size,
    /// so equality is deliberately not required).
    ///
    /// # Errors
    ///
    /// The directory broken on any structural violation (markers,
    /// version, sizes, sequence mismatch, or check byte).
    pub fn load<S: DirStore>(store: &mut S, object_limit: u32) -> Result<Self, Error<S::Error>> {
        let mut raw = [0u8; BIG_HEADER_SIZE as usize];
        store.read_at(0, &mut raw)?;
        if raw[1] != 0 || raw[2] != 0 || raw[3] != 0 || raw[4..8] != BIG_START {
            return Err(BROKEN.into());
        }
        let header = BigHeader {
            masseq: raw[0],
            name_len: get_u32(&raw, 8),
            size: get_u32(&raw, 12),
            entries: get_u32(&raw, 16),
            names_size: get_u32(&raw, 20),
            parent: get_u32(&raw, 24),
        };
        if header.size > object_limit
            || header.size < BIG_DIR_GRAIN
            || !header.size.is_multiple_of(BIG_DIR_GRAIN)
        {
            return Err(BROKEN.into());
        }
        if header.name_len as usize > MAX_NAME_LEN
            || header.entries > header.size / BIG_ENTRY_SIZE
            || header.names_size > header.size
            || header.used_end() > header.tail_offset()
        {
            return Err(BROKEN.into());
        }
        let mut tail = [0u8; BIG_TAIL_SIZE as usize];
        store.read_at(header.tail_offset(), &mut tail)?;
        if tail[..4] != BIG_END || tail[4] != header.masseq || tail[5] != 0 || tail[6] != 0 {
            return Err(BROKEN.into());
        }
        let dir = Self { header };
        // A zero check byte is accepted as "unchecked" (some imaging
        // tools write it so); anything else must match.
        let stored = tail[7];
        if stored != 0 && stored != dir.check_byte(store)? {
            return Err(BROKEN.into());
        }
        if !entries_in_order(header.entries, |index| dir.entry(store, index))? {
            return Err(BROKEN.into());
        }
        Ok(dir)
    }

    /// Compute the directory check byte: words then bytes over the used
    /// extent, then the tail's marker word and the three bytes before
    /// the check byte itself.
    fn check_byte<S: DirStore>(&self, store: &mut S) -> Result<u8, Error<S::Error>> {
        let mut check = 0u32;
        let mut offset = 0u32;
        let used = self.header.used_end();
        let mut buf = [0u8; 512];
        while offset < used {
            let take = (used - offset).min(512);
            store.read_at(offset, &mut buf[..take as usize])?;
            check = accumulate_words(&buf[..take as usize], check);
            offset += take;
        }
        let mut tail = [0u8; BIG_TAIL_SIZE as usize];
        store.read_at(self.header.tail_offset(), &mut tail)?;
        check = accumulate_words(&tail[..4], check);
        check = accumulate_bytes(&tail[4..7], check);
        Ok(fold_check(check))
    }

    /// Decode the entry at `index`.
    ///
    /// # Errors
    ///
    /// The directory broken if the entry's name pointer or length
    /// leaves the heap.
    pub fn entry<S: DirStore>(
        &self,
        store: &mut S,
        index: u32,
    ) -> Result<Option<Object>, Error<S::Error>> {
        if index >= self.header.entries {
            return Ok(None);
        }
        let at = self.header.entries_offset() + index * BIG_ENTRY_SIZE;
        let mut raw = [0u8; BIG_ENTRY_SIZE as usize];
        store.read_at(at, &mut raw)?;
        let name_len = get_u32(&raw, 20);
        let name_ptr = get_u32(&raw, 24);
        if name_len == 0
            || name_len as usize > MAX_NAME_LEN
            || name_ptr
                .checked_add(name_len)
                .is_none_or(|end| end > self.header.names_size)
        {
            return Err(BROKEN.into());
        }
        let mut object = Object {
            name: [0; MAX_NAME_LEN],
            name_len: name_len as usize,
            load: get_u32(&raw, 0),
            exec: get_u32(&raw, 4),
            size: get_u32(&raw, 8),
            indaddr: get_u32(&raw, 12),
            attr: acorn::attr_from_filecore(get_u32(&raw, 16).to_le_bytes()[0]),
        };
        store.read_at(
            self.header.heap_offset() + name_ptr,
            &mut object.name[..name_len as usize],
        )?;
        Ok(Some(object))
    }

    /// Find the entry named `name`, returning its index and object, by
    /// binary search on the order [`Self::load`] proved.
    ///
    /// # Errors
    ///
    /// Propagates decode errors from [`Self::entry`].
    pub fn find<S: DirStore>(
        &self,
        store: &mut S,
        name: &[u8],
    ) -> Result<Option<(u32, Object)>, Error<S::Error>> {
        let index = self.place_of(store, name)?;
        let Some(prior) = index.checked_sub(1) else {
            return Ok(None);
        };
        let object = self.entry(store, prior)?.ok_or(BROKEN)?;
        Ok(acorn::names_match(object.name(), name).then_some((prior, object)))
    }

    /// The index of the first entry that sorts after `name`.
    ///
    /// # Errors
    ///
    /// Propagates decode errors from [`Self::entry`].
    pub fn place_of<S: DirStore>(
        &self,
        store: &mut S,
        name: &[u8],
    ) -> Result<u32, Error<S::Error>> {
        let (mut low, mut high) = (0u32, self.header.entries);
        while low < high {
            let mid = low + (high - low) / 2;
            let object = self.entry(store, mid)?.ok_or(BROKEN)?;
            if acorn::name_order(object.name(), name) == core::cmp::Ordering::Greater {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        Ok(low)
    }

    /// Insert `object` in sorted position.
    ///
    /// # Errors
    ///
    /// * [`Fault::Exists`] if a same-named entry exists.
    /// * [`Fault::DirectoryFull`] if the directory's current size
    ///   cannot hold the new entry and name (the caller grows the
    ///   directory object and retries).
    pub fn insert<S: DirStore>(
        &mut self,
        store: &mut S,
        object: &Object,
    ) -> Result<(), Error<S::Error>> {
        if self.find(store, object.name())?.is_some() {
            return Err(Fault::Exists.into());
        }
        // The name was validated against `MAX_NAME_LEN`, so it fits.
        let name_len = u32::try_from(object.name_len).unwrap_or(u32::MAX);
        let grown = BIG_ENTRY_SIZE + pad_name(name_len) + 4;
        if self.header.used_end() + grown > self.header.table_offset() {
            return Err(Fault::DirectoryFull.into());
        }
        let index = self.place_of(store, object.name())?;
        // The name joins the heap where the entry it goes before keeps its
        // own, so the heap stays in the entries' order, as RISC OS 5 keeps it.
        let name_ptr = if index < self.header.entries {
            self.name_ptr(store, index)?
        } else {
            self.header.names_size
        };
        let padded_len = pad_name(name_len);
        let gap_at = self.header.entries_offset() + index * BIG_ENTRY_SIZE;
        let used = self.header.used_end();
        move_region(store, gap_at, gap_at + BIG_ENTRY_SIZE, used - gap_at)?;
        let heap_offset = self.header.heap_offset() + BIG_ENTRY_SIZE;
        let name_at = heap_offset + name_ptr;
        move_region(
            store,
            name_at,
            name_at + padded_len,
            self.header.names_size - name_ptr,
        )?;
        let mut padded = [0u8; MAX_NAME_LEN + 4];
        padded[..object.name_len].copy_from_slice(object.name());
        padded[object.name_len] = 0x0D;
        store.write_at(name_at, &padded[..padded_len as usize])?;
        let entries_at = self.header.entries_offset();
        let moved = |ptr: u32| (ptr >= name_ptr).then(|| ptr + padded_len);
        rebase_names(store, entries_at, 0..index, moved)?;
        rebase_names(store, entries_at, index + 1..self.header.entries + 1, moved)?;
        // Write the new entry.
        let mut raw = [0u8; BIG_ENTRY_SIZE as usize];
        put_u32(&mut raw, 0, object.load);
        put_u32(&mut raw, 4, object.exec);
        put_u32(&mut raw, 8, object.size);
        put_u32(&mut raw, 12, object.indaddr);
        put_u32(
            &mut raw,
            16,
            u32::from(acorn::attr_to_filecore(object.attr)),
        );
        put_u32(&mut raw, 20, name_len);
        put_u32(&mut raw, 24, name_ptr);
        store.write_at(gap_at, &raw)?;
        let table = self.header.table_offset();
        move_region(store, table, table - 4, 4 * index)?;
        store.write_at(table - 4 + 4 * index, &object.indaddr.to_le_bytes())?;
        self.header.entries += 1;
        self.header.names_size += padded_len;
        self.seal(store)
    }

    /// Where the entry at `index` keeps its name in the heap.
    fn name_ptr<S: DirStore>(&self, store: &mut S, index: u32) -> Result<u32, Error<S::Error>> {
        let mut raw = [0u8; 4];
        store.read_at(
            self.header.entries_offset() + index * BIG_ENTRY_SIZE + 24,
            &mut raw,
        )?;
        Ok(get_u32(&raw, 0))
    }

    /// Remove the entry at `index`, compacting the name heap.
    ///
    /// # Errors
    ///
    /// The directory broken if `index` is not a live entry, or its name's
    /// padding runs past the heap.
    pub fn remove<S: DirStore>(
        &mut self,
        store: &mut S,
        index: u32,
    ) -> Result<(), Error<S::Error>> {
        let Some(object) = self.entry(store, index)? else {
            return Err(BROKEN.into());
        };
        let mut raw = [0u8; BIG_ENTRY_SIZE as usize];
        let at = self.header.entries_offset() + index * BIG_ENTRY_SIZE;
        store.read_at(at, &mut raw)?;
        let name_ptr = get_u32(&raw, 24);
        let padded = pad_name(u32::try_from(object.name_len).unwrap_or(u32::MAX));
        if name_ptr
            .checked_add(padded)
            .is_none_or(|end| end > self.header.names_size)
        {
            return Err(BROKEN.into());
        }
        // Close the entry gap (moves later entries and the heap down).
        let used = self.header.used_end();
        let after = at + BIG_ENTRY_SIZE;
        move_region(store, after, at, used - after)?;
        // Compact the heap over the removed name.
        let heap_offset = self.header.heap_offset() - BIG_ENTRY_SIZE;
        let name_at = heap_offset + name_ptr;
        let heap_end = heap_offset + self.header.names_size;
        move_region(
            store,
            name_at + padded,
            name_at,
            heap_end - (name_at + padded),
        )?;
        let table = self.header.table_offset();
        move_region(store, table, table + 4, 4 * index)?;
        store.write_at(table, &[0; 4])?;
        self.header.entries -= 1;
        self.header.names_size -= padded;
        rebase_names(
            store,
            self.header.entries_offset(),
            0..self.header.entries,
            |ptr| (ptr > name_ptr).then(|| ptr - padded),
        )?;
        self.seal(store)
    }

    /// Rewrite the metadata (load/exec/size/indaddr/attributes) of the
    /// entry at `index`, keeping its name.
    ///
    /// # Errors
    ///
    /// The directory broken if `index` is not a live entry.
    pub fn update<S: DirStore>(
        &mut self,
        store: &mut S,
        index: u32,
        object: &Object,
    ) -> Result<(), Error<S::Error>> {
        self.put_fields(store, index, object)?;
        self.seal(store)
    }

    /// As [`Self::update`], and give the entry `object`'s spelling of its
    /// name. The two names must match ignoring case, so the entry keeps its
    /// place in the sort order and its name its length, and the heap is
    /// rewritten in place.
    ///
    /// # Errors
    ///
    /// The directory broken if `index` is not a live entry or the names
    /// do not match ignoring case.
    pub fn update_respelt<S: DirStore>(
        &mut self,
        store: &mut S,
        index: u32,
        object: &Object,
    ) -> Result<(), Error<S::Error>> {
        let stored = self.entry(store, index)?.ok_or(BROKEN)?;
        if !acorn::names_match(stored.name(), object.name()) {
            return Err(BROKEN.into());
        }
        let ptr = self.name_ptr(store, index)?;
        store.write_at(self.header.heap_offset() + ptr, object.name())?;
        self.put_fields(store, index, object)?;
        self.seal(store)
    }

    /// Write `object`'s metadata fields into the entry at `index`.
    fn put_fields<S: DirStore>(
        &self,
        store: &mut S,
        index: u32,
        object: &Object,
    ) -> Result<(), Error<S::Error>> {
        if index >= self.header.entries {
            return Err(BROKEN.into());
        }
        let at = self.header.entries_offset() + index * BIG_ENTRY_SIZE;
        let mut raw = [0u8; 20];
        put_u32(&mut raw, 0, object.load);
        put_u32(&mut raw, 4, object.exec);
        put_u32(&mut raw, 8, object.size);
        put_u32(&mut raw, 12, object.indaddr);
        put_u32(
            &mut raw,
            16,
            u32::from(acorn::attr_to_filecore(object.attr)),
        );
        store.write_at(at, &raw)?;
        store.write_at(
            self.header.table_offset() + 4 * index,
            &object.indaddr.to_le_bytes(),
        )
    }

    /// Rename the directory itself, as renaming or moving its entry does on
    /// RISC OS 5: the stored name rewritten over the old one with a NUL
    /// terminator, the entries and heap moved to follow it, what they vacate
    /// cleared, and `parent` taken as its parent, all in one sealing.
    ///
    /// # Errors
    ///
    /// * [`Fault::NameTooLong`] if `name` is empty or exceeds
    ///   [`MAX_NAME_LEN`].
    /// * [`Fault::DirectoryFull`] if the directory cannot hold the longer
    ///   name.
    /// * The store's failure.
    pub fn rename<S: DirStore>(
        &mut self,
        store: &mut S,
        name: &[u8],
        parent: u32,
    ) -> Result<(), Error<S::Error>> {
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(Fault::NameTooLong.into());
        }
        let name_len = u32::try_from(name.len()).unwrap_or(u32::MAX);
        let old_field = pad_name(self.header.name_len);
        let new_field = pad_name(name_len);
        let body = self.header.used_end() - self.header.entries_offset();
        if BIG_HEADER_SIZE + new_field + body > self.header.table_offset() {
            return Err(Fault::DirectoryFull.into());
        }
        move_region(
            store,
            BIG_HEADER_SIZE + old_field,
            BIG_HEADER_SIZE + new_field,
            body,
        )?;
        let mut field = [0u8; MAX_NAME_LEN + 4];
        field[..name.len()].copy_from_slice(name);
        store.write_at(BIG_HEADER_SIZE, &field[..new_field as usize])?;
        if new_field < old_field {
            let vacated = (old_field - new_field) as usize;
            let end = BIG_HEADER_SIZE + new_field + body;
            store.write_at(end, &[0u8; MAX_NAME_LEN + 4][..vacated])?;
        }
        self.header.name_len = name_len;
        self.header.parent = parent;
        self.seal(store)
    }

    /// Repoint the directory's parent.
    ///
    /// # Errors
    ///
    /// Propagates store I/O errors.
    pub fn set_parent<S: DirStore>(
        &mut self,
        store: &mut S,
        parent: u32,
    ) -> Result<(), Error<S::Error>> {
        self.header.parent = parent;
        self.seal(store)
    }

    /// Grow the directory to `new_size` (the object has already been
    /// extended that far): the tail moves to the new end and the space
    /// between the used extent and the tail is zeroed.
    ///
    /// # Errors
    ///
    /// * [`Fault::Unsupported`] if `new_size` is not a
    ///   larger multiple of [`BIG_DIR_GRAIN`].
    /// * The store's failure.
    pub fn grow<S: DirStore>(
        &mut self,
        store: &mut S,
        new_size: u32,
    ) -> Result<(), Error<S::Error>> {
        if new_size <= self.header.size || !new_size.is_multiple_of(BIG_DIR_GRAIN) {
            return Err(Fault::Unsupported.into());
        }
        let old_table = self.header.table_offset();
        let table_len = 4 * self.header.entries;
        self.header.size = new_size;
        let new_table = self.header.table_offset();
        move_region(store, old_table, new_table, table_len)?;
        // Zero what the old table and tail held, up to the new table.
        let zeroes = [0u8; 512];
        let mut offset = old_table;
        while offset < new_table {
            let take = (new_table - offset).min(512);
            store.write_at(offset, &zeroes[..take as usize])?;
            offset += take;
        }
        self.seal(store)
    }

    /// Bump the sequence number, rewrite header and tail, and refresh
    /// the check byte.
    fn seal<S: DirStore>(&mut self, store: &mut S) -> Result<(), Error<S::Error>> {
        self.header.masseq = bcd_increment(self.header.masseq);
        store.write_at(0, &self.header.encode())?;
        let mut tail = [0u8; BIG_TAIL_SIZE as usize];
        tail[..4].copy_from_slice(&BIG_END);
        tail[4] = self.header.masseq;
        store.write_at(self.header.tail_offset(), &tail)?;
        let check = self.check_byte(store)?;
        store.write_at(self.header.tail_offset() + 7, &[check])
    }

    /// Write a fresh, empty big directory of `size` bytes named `name`
    /// into `store`.
    ///
    /// # Errors
    ///
    /// * [`Fault::NameTooLong`] if the name exceeds
    ///   [`MAX_NAME_LEN`] or `size` cannot hold it.
    /// * The store's failure.
    pub fn initialise<S: DirStore>(
        store: &mut S,
        size: u32,
        name: &[u8],
        parent: u32,
    ) -> Result<(), Error<S::Error>> {
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(Fault::NameTooLong.into());
        }
        let header = BigHeader {
            masseq: 0,
            name_len: u32::try_from(name.len()).unwrap_or(u32::MAX),
            size,
            entries: 0,
            names_size: 0,
            parent,
        };
        if size < BIG_DIR_GRAIN
            || !size.is_multiple_of(BIG_DIR_GRAIN)
            || header.used_end() > header.tail_offset()
        {
            return Err(Fault::Unsupported.into());
        }
        // Zero the whole directory, then lay down header, name, tail.
        let zeroes = [0u8; 512];
        let mut offset = 0u32;
        while offset < size {
            let take = (size - offset).min(512);
            store.write_at(offset, &zeroes[..take as usize])?;
            offset += take;
        }
        store.write_at(0, &header.encode())?;
        let mut padded = [0u8; MAX_NAME_LEN + 4];
        let padded_len = pad_name(header.name_len) as usize;
        padded[..name.len()].copy_from_slice(name);
        padded[name.len()] = 0x0D;
        store.write_at(BIG_HEADER_SIZE, &padded[..padded_len])?;
        let mut tail = [0u8; BIG_TAIL_SIZE as usize];
        tail[..4].copy_from_slice(&BIG_END);
        tail[4] = header.masseq;
        store.write_at(header.tail_offset(), &tail)?;
        let dir = Self { header };
        let check = dir.check_byte(store)?;
        store.write_at(header.tail_offset() + 7, &[check])
    }
}

/// The name pointer of each entry in `slots`, the entries starting at
/// `entries_at`, rewritten where `moved` gives it a new value, a buffer of
/// entries read and written at a time.
fn rebase_names<S: DirStore>(
    store: &mut S,
    entries_at: u32,
    slots: core::ops::Range<u32>,
    moved: impl Fn(u32) -> Option<u32>,
) -> Result<(), Error<S::Error>> {
    const CHUNK: u32 = 512 / BIG_ENTRY_SIZE;
    let mut buf = [0u8; (CHUNK * BIG_ENTRY_SIZE) as usize];
    let mut slot = slots.start;
    while slot < slots.end {
        let count = (slots.end - slot).min(CHUNK);
        let at = entries_at + slot * BIG_ENTRY_SIZE;
        let raw = &mut buf[..(count * BIG_ENTRY_SIZE) as usize];
        store.read_at(at, raw)?;
        let mut changed = false;
        for entry in raw.as_chunks_mut::<{ BIG_ENTRY_SIZE as usize }>().0 {
            if let Some(ptr) = moved(get_u32(entry, 24)) {
                put_u32(entry, 24, ptr);
                changed = true;
            }
        }
        if changed {
            store.write_at(at, raw)?;
        }
        slot += count;
    }
    Ok(())
}

/// Move `len` bytes from `src` to `dst` within the store, handling
/// overlapping ranges in either direction.
fn move_region<S: DirStore>(
    store: &mut S,
    src: u32,
    dst: u32,
    len: u32,
) -> Result<(), Error<S::Error>> {
    if len == 0 || src == dst {
        return Ok(());
    }
    let mut buf = [0u8; 512];
    if dst < src {
        let mut done = 0u32;
        while done < len {
            let take = (len - done).min(512);
            store.read_at(src + done, &mut buf[..take as usize])?;
            store.write_at(dst + done, &buf[..take as usize])?;
            done += take;
        }
    } else {
        let mut remaining = len;
        while remaining > 0 {
            let take = remaining.min(512);
            let at = remaining - take;
            store.read_at(src + at, &mut buf[..take as usize])?;
            store.write_at(dst + at, &buf[..take as usize])?;
            remaining -= take;
        }
    }
    Ok(())
}
