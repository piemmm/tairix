//! The DOS's workspace in the drive's 2 KiB.

use crate::cpu::STACK_PAGE;
use crate::media::c1541::recording::DATA_GCR;

/// Page zero.
pub mod zp {
    /// The job queue: buffer n's job, replaced by its return code.
    pub const JOBS: u16 = 0x00;
    /// Buffer n's track and sector, two bytes a buffer.
    pub const HEADERS: u16 = 0x06;
    /// The disk's ID, its first character then its second.
    pub const DISK_ID: u16 = 0x12;

    /// The device's number, from the jumpers.
    pub const DEVICE: u16 = 0x20;
    /// `$80` while the bus is under ATN, else 0: a line state the waits
    /// compare ATN with.
    pub const ATN_MODE: u16 = 0x21;
    /// Bit 7 while this drive is the listener, bit 6 while the talker.
    pub const ADDRESSED: u16 = 0x22;
    /// The channel being served: the one the last command under ATN named,
    /// or the one whose request is being carried out.
    pub const CHANNEL: u16 = 0x23;
    /// What that command asked: the command's top nibble, `$60` data,
    /// `$E0` close, `$F0` open.
    pub const REQUEST: u16 = 0x24;
    /// The byte being received or sent.
    pub const BYTE: u16 = 0x25;
    /// Bit 7 when the byte received came with EOI, or the byte to send
    /// goes with it.
    pub const EOI: u16 = 0x26;
    /// The command buffer's length so far.
    pub const COMMAND_LENGTH: u16 = 0x27;
    /// The error: its number, track and sector.
    pub const ERROR: u16 = 0x28;
    pub const ERROR_TRACK: u16 = 0x29;
    pub const ERROR_SECTOR: u16 = 0x2A;
    /// The error message's next character to send, and its length.
    pub const MESSAGE_AT: u16 = 0x2B;
    pub const MESSAGE_LENGTH: u16 = 0x2C;
    /// A listen's request and channel left for the interface when
    /// UNLISTEN ended it, 0 for none.
    pub const PENDING: u16 = 0x2D;
    /// A pointer for walking tables longer than a page.
    pub const POINTER: u16 = 0x2E;

    /// A channel's buffer, as the file layer reads and fills it.
    pub const FILE_POINTER: u16 = 0x30;
    /// A name or pattern being matched against the directory entry at
    /// `DIRECTORY_ENTRY`: its first character and its length.
    pub const PATTERN: u16 = 0x32;
    pub const PATTERN_LENGTH: u16 = 0x34;
    /// Set once the BAM is read and the disk's ID known; cleared when the
    /// write-protect sensor changes, as it does when a disk goes in or
    /// out.
    pub const DISK_READY: u16 = 0x35;
    /// The write-protect sensor as the controller last saw it.
    pub const SENSOR: u16 = 0x36;
    /// The name's start and end in the command buffer, the type and mode
    /// its fields ask for, each 0 for none.
    pub const NAME_START: u16 = 0x37;
    pub const NAME_END: u16 = 0x38;
    pub const NAME_TYPE: u16 = 0x39;
    pub const NAME_MODE: u16 = 0x3A;
    /// The directory sector in the directory buffer, and the entry's
    /// offset in it, the buffer's page after it so the two point at the
    /// entry.
    pub const DIRECTORY_TRACK: u16 = 0x3B;
    pub const DIRECTORY_SECTOR: u16 = 0x3C;
    pub const DIRECTORY_ENTRY: u16 = 0x3D;
    pub const DIRECTORY_PAGE: u16 = 0x3E;
    /// A byte kept a moment.
    pub const COUNT: u16 = 0x3F;

    /// The half-track the head is on.
    pub const HALF_TRACK: u16 = 0x40;
    /// Controller ticks the motor still needs to come up to speed.
    pub const SPIN_UP: u16 = 0x41;
    /// Controller ticks before an idle motor stops.
    pub const SPIN_DOWN: u16 = 0x42;
    /// The job being done: its buffer's number, track and sector.
    pub const JOB_BUFFER: u16 = 0x43;
    pub const JOB_TRACK: u16 = 0x44;
    pub const JOB_SECTOR: u16 = 0x45;
    /// The job's buffer.
    pub const BUFFER: u16 = 0x46;
    /// The GCR stream's next byte: the buffer, then the overflow.
    pub const STREAM: u16 = 0x48;
    /// A group of five GCR bytes and the four bytes they record.
    pub const GCR: u16 = 0x4A;
    pub const PLAIN: u16 = 0x4F;
    /// The header found: its eight bytes.
    pub const HEADER: u16 = 0x53;
    /// The data block's checksum.
    pub const CHECKSUM: u16 = 0x5B;
    /// Headers still to look at before giving up.
    pub const TRIES: u16 = 0x5C;
    /// A block group's first byte in the buffer.
    pub const INDEX: u16 = 0x5D;
    /// A header's ten GCR bytes as they come off the disk.
    pub const RAW: u16 = 0x60;
    /// A verify's outcome so far.
    pub const RESULT: u16 = 0x6A;
    /// The job's sector's header as the disk holds it in GCR.
    pub const EXPECTED: u16 = 0x6B;

    /// A job the interface runs: its code, and tries left.
    pub const RETRY_CODE: u16 = 0x76;
    pub const RETRIES: u16 = 0x77;
    /// The first block of the program last loaded, track 0 for none.
    pub const LAST_TRACK: u16 = 0x78;
    pub const LAST_SECTOR: u16 = 0x79;
    /// The listing's patterns in its copy of the command, a start and an
    /// end each, their count, `ALL_ENTRIES` where none was given, and the
    /// kind it lists, 0 for any.
    pub const PATTERN_STARTS: u16 = 0x7A;
    pub const PATTERN_ENDS: u16 = 0x7F;
    pub const PATTERN_COUNT: u16 = 0x84;
    pub const LISTING_KIND: u16 = 0x85;
    /// A bit a pattern: set where it has a wildcard, and so lists every
    /// entry it matches; set once one without has listed its one entry.
    pub const PATTERNS_WILD: u16 = 0x86;
    pub const PATTERNS_USED: u16 = 0x92;
    /// A pattern's start while it is read, and the one being matched.
    pub const PATTERN_FROM: u16 = 0x87;
    pub const PATTERN_INDEX: u16 = 0x88;
    /// Set where a pattern matched the entry.
    pub const MATCHED: u16 = 0x89;
    /// A listing chunk as it is put together: the next line's offset, the
    /// lines so far, and a line's spaces before its name.
    pub const LINE_AT: u16 = 0x8A;
    pub const LINES: u16 = 0x8B;
    pub const LEADING: u16 = 0x8C;
    /// Set once a line's name has its closing quote.
    pub const QUOTED: u16 = 0x8D;
    /// Name characters still to put.
    pub const REMAINING: u16 = 0x8E;
    /// The free blocks counted, low byte first.
    pub const FREE: u16 = 0x8F;
    /// The kind an entry looked for must be, 0 for any.
    pub const FIND_KIND: u16 = 0x91;
    /// A block being found in the BAM: its track and sector, the track's
    /// sectors, the way the search moves from the directory's track, set
    /// once it has moved to the other side, and a caller's X kept.
    pub const ALLOC_TRACK: u16 = 0x93;
    pub const ALLOC_SECTOR: u16 = 0x94;
    pub const ALLOC_SECTORS: u16 = 0x95;
    pub const ALLOC_STEP: u16 = 0x96;
    pub const ALLOC_SWITCHED: u16 = 0x97;
    pub const ALLOC_X: u16 = 0x98;
    /// A block's bit in the BAM: its byte's offset in the track's bitmap,
    /// and its mask.
    pub const ALLOC_BYTE: u16 = 0xA8;
    pub const ALLOC_MASK: u16 = 0xA9;
    /// A chain being walked: set where its blocks are marked used, clear
    /// where freed, and the block reached.
    pub const CHAIN_USES: u16 = 0xAA;
    pub const CHAIN_TRACK: u16 = 0xAB;
    pub const CHAIN_SECTOR: u16 = 0xAC;
    /// The listing's file while a chunk of it is put together.
    pub const LISTING_FILE: u16 = 0xAD;
    /// A bit a track, set once a block is taken from it, until the BAM is
    /// next read or written whole.
    pub const TOUCHED: u16 = 0xAE;
    /// A format's outcome as a job's return code, the tries left at the
    /// track being laid out, the sectors still to write and the next
    /// header's offset in the header table, and the track's gap after each
    /// sector and its tail.
    pub const FORMAT_RESULT: u16 = 0xB3;
    pub const FORMAT_TRIES: u16 = 0xB4;
    pub const FORMAT_SECTORS: u16 = 0xB5;
    pub const FORMAT_HEADER: u16 = 0xB6;
    pub const FORMAT_GAP: u16 = 0xB7;
    pub const FORMAT_TAIL: u16 = 0xB8;
    /// The directory buffer's start, and its number, `NO_FILE` until a
    /// request asks for one.
    pub const DIRECTORY_AT: u16 = 0xB9;
    pub const DIRECTORY_HELD: u16 = 0xBB;
    /// The name of the entry being matched.
    pub const NAME_AT: u16 = 0xBC;
    /// The entry being listed, copied.
    pub const LISTED: u16 = 0xBE;
    /// A block command's numbers, and a number as its digits are read
    /// with bit 7 set once it outgrows a byte.
    pub const NUMBERS: u16 = 0xDE;
    pub const VALUE: u16 = 0xE2;
    pub const OVERFLOW: u16 = 0xE3;
    /// A memory command's address, the bytes an `M-R` reply has left to
    /// send, and bit 7 set while it has its RETURN still to send.
    pub const MEMORY_AT: u16 = 0xE4;
    pub const MEMORY_LEFT: u16 = 0xE6;
    pub const MEMORY_STATE: u16 = 0xE8;
    /// The record length a name's `L` field asks for, 0 for none.
    pub const NAME_RECORD: u16 = 0xE9;
    /// A relative file's arithmetic: a product or a dividend, then the
    /// quotient, and a factor, 24 bits each; a multiplier or a divisor, and
    /// the remainder.
    pub const REL_WORK: u16 = 0xEA;
    pub const REL_FACTOR: u16 = 0xED;
    pub const REL_OPERAND: u16 = 0xF0;
    pub const REL_REMAINDER: u16 = 0xF1;
    /// A data block wanted by its index in the file; a new data block's
    /// track and sector, the spare a block opening a side sector is followed
    /// by, track 0 for none, and a new side sector's; a count; the blocks an
    /// extension needs; the first side sector's block; and bit 7 set while an
    /// extension adds a single block.
    pub const REL_WANT: u16 = 0xF2;
    pub const REL_NEW: u16 = 0xF4;
    pub const REL_SPARE: u16 = 0xF6;
    pub const REL_SIDE_NEW: u16 = 0xF8;
    pub const REL_COUNT: u16 = 0xFA;
    pub const REL_NEEDED: u16 = 0xFB;
    pub const REL_FIRST_SIDE: u16 = 0xFD;
    pub const REL_SINGLE: u16 = 0xFF;
    /// A command's names in the command buffer, a start and an end each,
    /// their count, and the first after `=`, 0 for none.
    pub const NAME_STARTS: u16 = 0x99;
    pub const NAME_ENDS: u16 = 0x9E;
    pub const NAME_COUNT: u16 = 0xA3;
    pub const NAME_EQUALS: u16 = 0xA4;
    /// The files a command has done, and the buffers it reads and writes
    /// through.
    pub const DONE_COUNT: u16 = 0xA5;
    pub const SOURCE: u16 = 0xA6;
    pub const TARGET: u16 = 0xA7;
}

/// TOUCHED's bytes: a bit for each track number.
pub const TOUCHED_BYTES: u8 = (crate::media::c1541::layout::TRACKS + 1).div_ceil(8);

/// The numbers a block command keeps.
pub const BLOCK_NUMBERS: u8 = 4;

/// `PATTERN_COUNT` where a listing names no pattern at all.
pub const ALL_ENTRIES: u8 = 0xFF;
/// The patterns a listing keeps; more are ignored.
pub const PATTERNS: u8 = 5;

const _: () = assert!(
    zp::PATTERN_STARTS + PATTERNS as u16 == zp::PATTERN_ENDS
        && zp::PATTERN_ENDS + PATTERNS as u16 == zp::PATTERN_COUNT
        && zp::NAME_STARTS + PATTERNS as u16 == zp::NAME_ENDS
        && zp::NAME_ENDS + PATTERNS as u16 == zp::NAME_COUNT
        && zp::EXPECTED + 10 <= zp::RETRY_CODE
        && zp::TOUCHED + TOUCHED_BYTES as u16 <= zp::FORMAT_RESULT
        && zp::FORMAT_TAIL < zp::DIRECTORY_AT
        && zp::LISTED + crate::media::c1541::layout::entry::BYTES as u16 <= zp::NUMBERS
        && zp::NUMBERS + BLOCK_NUMBERS as u16 <= zp::VALUE
        && zp::MEMORY_STATE < zp::NAME_RECORD
        && zp::REL_FIRST_SIDE + 2 == zp::REL_SINGLE
        && zp::REL_SINGLE < 0x100,
    "page zero's tables apart"
);

/// The stack's top, below the overflow so the two never meet.
pub const STACK_TOP: u8 = 0xBA;
/// The GCR block's bytes past its buffer's page, in the stack page.
pub const OVERFLOW: u16 = 0x01BB;
/// Its room: what of a data block's GCR passes the buffer's page.
pub const OVERFLOW_BYTES: u8 = (DATA_GCR - 0x100).to_le_bytes()[0];
/// The first buffer; buffer n is n pages on.
pub const BUFFERS: u16 = 0x0300;

const _: () = assert!(
    STACK_PAGE + STACK_TOP as u16 + 1 == OVERFLOW
        && OVERFLOW + OVERFLOW_BYTES as u16 == 0x0200
        && 0x0100 + OVERFLOW_BYTES as u16 == DATA_GCR,
    "the overflow fills the stack page above the stack, holding what of a block passes its buffer"
);

/// Page two.
pub mod page {
    /// The command channel's buffer: a command or a file's name.
    pub const COMMAND: u16 = 0x0200;
    /// Its room; a longer command is refused.
    pub const COMMAND_BYTES: u8 = 42;
    /// The error message as the channel sends it.
    pub const MESSAGE: u16 = COMMAND + COMMAND_BYTES as u16;
    /// Its room.
    pub const MESSAGE_BYTES: u8 = 40;
    /// Each data channel's file, as the buffer it holds, [`NO_FILE`](super::NO_FILE)
    /// for none.
    pub const CHANNEL_FILE: u16 = MESSAGE + MESSAGE_BYTES as u16;
    /// The data channels, every one but the command channel.
    pub const DATA_CHANNELS: u8 = 15;
    /// Each open file's state, a byte a channel buffer: its mode, `FREE`
    /// for a buffer no file holds; the offset of its next byte, the byte
    /// it holds ready to send and of the block's last; the block after
    /// this one as it reads, or this block as it writes; the byte held,
    /// with bit 7 set where EOI goes with it.
    pub const FILE_MODE: u16 = CHANNEL_FILE + DATA_CHANNELS as u16;
    pub const FILE_AT: u16 = FILE_MODE + super::CHANNEL_BUFFERS as u16;
    pub const FILE_LAST: u16 = FILE_AT + super::CHANNEL_BUFFERS as u16;
    pub const FILE_TRACK: u16 = FILE_LAST + super::CHANNEL_BUFFERS as u16;
    pub const FILE_SECTOR: u16 = FILE_TRACK + super::CHANNEL_BUFFERS as u16;
    pub const FILE_BYTE: u16 = FILE_SECTOR + super::CHANNEL_BUFFERS as u16;
    pub const FILE_EOI: u16 = FILE_BYTE + super::CHANNEL_BUFFERS as u16;
    /// A written file's directory entry, its first block, the blocks it has
    /// so far, its type byte once closed, and the file a replace frees,
    /// track 0 for none.
    pub const FILE_ENTRY_TRACK: u16 = FILE_EOI + super::CHANNEL_BUFFERS as u16;
    pub const FILE_ENTRY_SECTOR: u16 = FILE_ENTRY_TRACK + super::CHANNEL_BUFFERS as u16;
    pub const FILE_ENTRY: u16 = FILE_ENTRY_SECTOR + super::CHANNEL_BUFFERS as u16;
    pub const FILE_FIRST_TRACK: u16 = FILE_ENTRY + super::CHANNEL_BUFFERS as u16;
    pub const FILE_FIRST_SECTOR: u16 = FILE_FIRST_TRACK + super::CHANNEL_BUFFERS as u16;
    pub const FILE_BLOCKS: u16 = FILE_FIRST_SECTOR + super::CHANNEL_BUFFERS as u16;
    pub const FILE_BLOCKS_HIGH: u16 = FILE_BLOCKS + super::CHANNEL_BUFFERS as u16;
    pub const FILE_KIND: u16 = FILE_BLOCKS_HIGH + super::CHANNEL_BUFFERS as u16;
    pub const FILE_OLD_TRACK: u16 = FILE_KIND + super::CHANNEL_BUFFERS as u16;
    pub const FILE_OLD_SECTOR: u16 = FILE_OLD_TRACK + super::CHANNEL_BUFFERS as u16;
    /// The track the BAM last recorded the written file as filling, 0 for
    /// none yet.
    pub const FILE_FILL: u16 = FILE_OLD_SECTOR + super::CHANNEL_BUFFERS as u16;
    /// The listing's command, kept while later commands reuse the
    /// command buffer.
    pub const LISTING_TEXT: u16 = FILE_FILL + super::CHANNEL_BUFFERS as u16;
    /// The first block of each file a copy reads, found before its own
    /// file is made, by the source's name's number less one.
    pub const COPY_TRACKS: u16 = LISTING_TEXT + COMMAND_BYTES as u16;
    pub const COPY_SECTORS: u16 = COPY_TRACKS + super::COPY_SOURCES as u16;
    /// The relative file open, as its channel's buffer, [`super::NO_FILE`]
    /// for none, and its side sector's buffer.
    pub const REL_FILE: u16 = COPY_SECTORS + super::COPY_SOURCES as u16;
    pub const REL_SIDE: u16 = REL_FILE + 1;
    /// Its record length; the record and the position in it its next byte
    /// is at, each from 0; the records its blocks hold whole; its data
    /// blocks and side sectors.
    pub const REL_LENGTH: u16 = REL_SIDE + 1;
    pub const REL_RECORD: u16 = REL_LENGTH + 1;
    pub const REL_POSITION: u16 = REL_RECORD + 2;
    pub const REL_RECORDS: u16 = REL_POSITION + 1;
    pub const REL_BLOCKS: u16 = REL_RECORDS + 2;
    pub const REL_SIDES: u16 = REL_BLOCKS + 2;
    /// The data block its buffer holds by its index, and the side sector
    /// its other holds; what is to be written back, and whether the file
    /// changed since it opened.
    pub const REL_HELD: u16 = REL_SIDES + 1;
    pub const REL_SIDE_HELD: u16 = REL_HELD + 2;
    pub const REL_DIRTY: u16 = REL_SIDE_HELD + 1;
    /// The read's state, and the position of the last byte a read of the
    /// record sends; the cursor: the next byte's data block by its index
    /// and its offset in the block's data.
    pub const REL_STATE: u16 = REL_DIRTY + 1;
    pub const REL_END: u16 = REL_STATE + 1;
    pub const REL_INDEX: u16 = REL_END + 1;
    pub const REL_OFFSET: u16 = REL_INDEX + 2;
}

/// The files a copy reads at most: every name it keeps but the first.
pub const COPY_SOURCES: u8 = PATTERNS - 1;

/// `CHANNEL_FILE` for a channel with no file open.
pub const NO_FILE: u8 = 0xFF;

const _: () = assert!(
    page::REL_OFFSET < BUFFERS,
    "page two's areas within page two"
);

/// The buffer the BAM is kept in; the others serve files, direct access
/// and the directory alike.
pub const BAM_BUFFER: u8 = 4;
pub const CHANNEL_BUFFERS: u8 = 4;

/// Buffer `number`'s first byte.
pub const fn buffer(number: u8) -> u16 {
    BUFFERS + 0x100 * number as u16
}

/// The BAM's buffer.
pub const BAM: u16 = buffer(BAM_BUFFER);
