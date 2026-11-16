//! The 1541 DOS's layout of a disk above its sectors, as the 1541 User's
//! Guide documents it: the BAM on the directory track's first sector, the
//! directory chained from its second, and each file a chain of blocks, the
//! last naming its last byte.

/// Tracks on a disk the DOS formats and reads.
pub const TRACKS: u8 = 35;
/// The track holding the BAM and the directory.
pub const DIRECTORY_TRACK: u8 = 18;
/// The BAM's sector on the directory track.
pub const BAM_SECTOR: u8 = 0;
/// The directory's first sector.
pub const FIRST_DIRECTORY_SECTOR: u8 = 1;
/// A block's first two bytes: the next block's track and sector, or 0 and
/// the offset of the last block's last byte.
pub const LINK_BYTES: u8 = 2;
/// A block's bytes after its link.
pub const DATA_BYTES: u8 = u8::MAX - LINK_BYTES + 1;

/// The BAM's fields.
pub mod bam {
    /// The DOS version's byte, after the link to the directory.
    pub const VERSION_AT: u8 = 2;
    /// The version a 1541 formats with.
    pub const VERSION: u8 = b'A';
    /// Each track's entry, from four bytes in for track 1: its free sectors'
    /// count, then a bit a sector, set where it is free.
    pub const TRACK_ENTRY_BYTES: u8 = 4;
    /// The disk's name, padded to sixteen characters, then two padding
    /// bytes.
    pub const NAME: u8 = 0x90;
    /// The disk's ID, then a padding byte and the DOS type.
    pub const ID: u8 = 0xA2;
    /// The DOS type's place.
    pub const DOS_TYPE_AT: u8 = ID + 3;
    /// The DOS type a 1541 formats with.
    pub const DOS_TYPE: [u8; 2] = *b"2A";
    /// The header's end, past the padding after the DOS type.
    pub const HEADER_END: u8 = DOS_TYPE_AT + 2 + 4;
    /// The ID, its padding and the DOS type together, as a listing shows
    /// them.
    pub const ID_AND_TYPE_BYTES: u8 = 5;
}

/// A directory entry's fields; eight entries fill a sector.
pub mod entry {
    /// The type byte: a kind and its closed, locked and replacing bits.
    pub const TYPE: u8 = 2;
    /// The first block's track.
    pub const TRACK: u8 = 3;
    /// The first block's sector.
    pub const SECTOR: u8 = 4;
    /// The name, padded to sixteen characters.
    pub const NAME: u8 = 5;
    /// A relative file's first side sector, track then sector.
    pub const SIDE: u8 = 21;
    /// A relative file's record length.
    pub const RECORD_LENGTH: u8 = 23;
    /// The replacing file's first block, track then sector, while a
    /// replace is open.
    pub const REPLACEMENT: u8 = 28;
    /// The file's length in blocks, low byte first.
    pub const BLOCKS: u8 = 30;
    /// An entry's length.
    pub const BYTES: u8 = 32;
}

/// A name's longest.
pub const NAME_BYTES: u8 = 16;
/// The shifted space that pads a shorter name.
pub const PADDING: u8 = 0xA0;

/// A directory entry's type byte; 0 is an empty slot.
pub mod kind {
    /// A deleted file's kind.
    pub const DEL: u8 = 0;
    /// A sequential file's.
    pub const SEQ: u8 = 1;
    /// A program's.
    pub const PRG: u8 = 2;
    /// A user file's.
    pub const USR: u8 = 3;
    /// A relative file's.
    pub const REL: u8 = 4;
    /// Set once the file is closed, clear while it is written.
    pub const CLOSED: u8 = 0x80;
    /// Set where scratching is refused.
    pub const LOCKED: u8 = 0x40;
    /// Set while a file replacing this one is open.
    pub const REPLACING: u8 = 0x20;
}

/// A relative file's side sector, after its link: its number from 0, the
/// record length, every one of the file's side sectors' blocks, then the
/// blocks of the data blocks it lists, track then sector each. The last's
/// link names no block, its sector the offset of its last pointer's last
/// byte.
pub mod side {
    /// The side sector's number.
    pub const NUMBER: u8 = 2;
    /// The file's record length.
    pub const RECORD_LENGTH: u8 = 3;
    /// The file's side sectors' blocks.
    pub const TABLE: u8 = 4;
    /// The data blocks' blocks.
    pub const POINTERS: u8 = 16;
    /// The side sectors a file has at most.
    pub const MOST: u8 = 6;
    /// The data blocks a side sector lists.
    pub const LISTED: u8 = 120;
}

const _: () = assert!(
    side::RECORD_LENGTH + 1 == side::TABLE
        && side::TABLE + 2 * side::MOST == side::POINTERS
        && side::POINTERS as u16 + 2 * side::LISTED as u16 == 0x100
        && entry::SIDE + 2 == entry::RECORD_LENGTH,
    "a side sector's fields fill it in order, and an entry's follow one another"
);
