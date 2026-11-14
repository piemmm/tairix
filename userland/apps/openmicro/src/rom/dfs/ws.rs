//! The DFS's workspace: the filing system's zero page, the absolute
//! workspace from &0E00 to &16FF, and two private pages. The catalogue, the
//! current and library directories and drives, and the *OPT 1 and *ENABLE
//! states sit where DFS 1.20 keeps them, which software reads.

/// The zero page a filing system may use.
pub mod zp {
    /// The 8271's result once a command has ended; bit 7 set while it runs.
    /// NMI workspace, used only while the NMI routine is claimed.
    pub const NMI_RESULT: u16 = 0xA0;
    /// Bytes a read still stores before it discards the rest.
    pub const NMI_COUNT: u16 = 0xA1;

    /// A command's own temporaries.
    pub const UTILITY: u16 = 0xA8;

    /// A pointer to the caller's block or text.
    pub const BLOCK: u16 = 0xB0;
    /// A second pointer.
    pub const POINTER: u16 = 0xB2;
    /// Scratch, not kept from one call to the next.
    pub const T0: u16 = 0xB4;
    pub const T1: u16 = 0xB5;
    pub const T2: u16 = 0xB6;
    pub const T3: u16 = 0xB7;
    pub const T4: u16 = 0xB8;
    pub const T5: u16 = 0xB9;
    pub const T6: u16 = 0xBA;
    pub const T7: u16 = 0xBB;
    /// The channel being worked on: its block's offset in its page.
    pub const CHANNEL: u16 = 0xBC;
    /// A catalogue entry's offset.
    pub const ENTRY: u16 = 0xBD;
    /// The pointer inline text is printed through.
    pub const PRINT: u16 = 0xBE;

    /// The name being looked for: seven characters, space filled.
    pub const NAME: u16 = 0xC0;
    /// Its directory.
    pub const DIR: u16 = 0xC7;
    /// Its drive.
    pub const DRIVE: u16 = 0xC8;
    /// Non-zero when the name holds a wildcard.
    pub const WILD: u16 = 0xC9;
    /// OSFIND's mode while a file opens.
    pub const MODE: u16 = 0xCA;
    /// A, X and Y as a filing system call was entered with them.
    pub const SAVE_A: u16 = 0xCB;
    pub const SAVE_X: u16 = 0xCC;
    pub const SAVE_Y: u16 = 0xCD;
    /// The pointer to the channel's buffer.
    pub const BUFFER: u16 = 0xCE;
}

/// The catalogue of the drive last read: names in its first sector, the
/// rest in its second, eight bytes an entry from offset 8.
pub mod catalogue {
    /// Sector 0: the title's first eight characters, then each entry's name
    /// and directory, the directory's top bit the lock.
    pub const NAMES: u16 = 0x0E00;
    /// Sector 1: the title's last four characters, the cycle number, the
    /// entries' size, the boot option and the sector count, then each
    /// entry's addresses, length and start sector.
    pub const INFO: u16 = 0x0F00;
    /// The cycle number, in BCD.
    pub const CYCLE: u16 = INFO + 4;
    /// Eight times the number of entries.
    pub const END: u16 = INFO + 5;
    /// The boot option in bits 4 and 5, the sector count's top two bits in 0
    /// and 1.
    pub const OPTION: u16 = INFO + 6;
    /// The sector count's low byte.
    pub const SECTORS: u16 = INFO + 7;
    /// The first entry's offset, and the last one's past which none fits.
    pub const FIRST: u8 = 8;
    pub const LAST: u8 = 0xF8;
    /// An entry's name and directory, from its offset in the first sector.
    pub const NAME_LEN: u8 = 7;
    pub const ENTRY_DIR: u8 = 7;
    /// The directory byte's lock.
    pub const LOCKED: u8 = 0x80;
    /// An entry's fields from its offset in the second sector: the low two
    /// bytes of the load and execution addresses and the length, the top
    /// bits of each with the start sector's, and the start's low byte.
    pub const LOAD: u8 = 0;
    pub const EXEC: u8 = 2;
    pub const LENGTH: u8 = 4;
    pub const MIXED: u8 = 6;
    pub const START: u8 = 7;
    /// Where in the mixed byte each field's top two bits sit.
    pub const START_SHIFT: u8 = 0;
    pub const LOAD_SHIFT: u8 = 2;
    pub const LENGTH_SHIFT: u8 = 4;
    pub const EXEC_SHIFT: u8 = 6;
    /// The title's characters in each sector.
    pub const TITLE_FIRST: u8 = 8;
    pub const TITLE_LEN: u8 = 12;
    /// The sectors the catalogue itself takes.
    pub const OWN_SECTORS: u8 = 2;
}

/// Page &10: the operation in hand and the settings.
pub mod page {
    /// The transfer `transfer` runs: drive, direction, first sector,
    /// sectors, memory address and the bytes a read stores.
    pub const OP_DRIVE: u16 = 0x1000;
    pub const OP_WRITE: u16 = OP_DRIVE + 1;
    pub const OP_SECTOR: u16 = OP_WRITE + 1;
    pub const OP_SECTORS: u16 = OP_SECTOR + 2;
    pub const OP_ADDRESS: u16 = OP_SECTORS + 2;
    pub const OP_STORE: u16 = OP_ADDRESS + 2;
    /// The track and sector of the command in hand.
    pub const OP_TRACK: u16 = OP_STORE + 3;
    pub const OP_SECTOR_ON_TRACK: u16 = OP_TRACK + 1;
    /// Sectors and bytes the command in hand moves.
    pub const OP_COUNT: u16 = OP_SECTOR_ON_TRACK + 1;
    pub const OP_BYTES: u16 = OP_COUNT + 1;
    /// Attempts the command in hand has left.
    pub const OP_ATTEMPTS: u16 = OP_BYTES + 2;
    /// The drive's physical number and its select bits.
    pub const OP_PHYSICAL: u16 = OP_ATTEMPTS + 1;
    pub const OP_SELECT: u16 = OP_PHYSICAL + 1;
    /// The ROM that had the NMI routine before this one claimed it.
    pub const NMI_PREVIOUS: u16 = OP_SELECT + 1;
    /// Per physical drive, non-zero once its head has found track 0.
    pub const CALIBRATED: u16 = NMI_PREVIOUS + 1;
    /// The drive whose catalogue is at &0E00.
    pub const CAT_DRIVE: u16 = CALIBRATED + 2;
    /// The address of the rest of a *RUN command's line, for OSARGS 1.
    pub const TAIL: u16 = CAT_DRIVE + 1;
    /// A command's parameters as the chip is given them.
    pub const PARAMS: u16 = TAIL + 2;
    /// A second name, for *RENAME's new one: seven characters, directory,
    /// drive.
    pub const NAME2: u16 = PARAMS + 5;
    /// A file's addresses and length as four bytes each, and its first
    /// sector: the one being saved, loaded or read.
    pub const INFO_LOAD: u16 = NAME2 + 9;
    pub const INFO_EXEC: u16 = INFO_LOAD + 4;
    pub const INFO_LENGTH: u16 = INFO_EXEC + 4;
    pub const INFO_START: u16 = INFO_LENGTH + 4;
    /// Bit 7 set while names are matched as patterns, `*` and `#` wild;
    /// clear, they match only themselves.
    pub const PATTERN: u16 = INFO_START + 2;
    /// Non-zero when a name gave its directory or drive.
    pub const QUALIFIED: u16 = PATTERN + 1;
    /// Scratch for the commands.
    pub const WORK: u16 = QUALIFIED + 1;
    pub const WORK_LEN: u16 = 0x30;
    /// The channels open. With none, nothing in the channels' pages is read:
    /// software loads over them once its files are closed, as DFS 1.20
    /// lets it.
    pub const OPEN_COUNT: u16 = WORK + WORK_LEN;
    const _: () = assert!(OPEN_COUNT < MESSAGES, "page &10's fields overlap");

    /// Messages on: zero for *OPT 1 on, &FF off.
    pub const MESSAGES: u16 = 0x10C6;
    /// *ENABLE's countdown: 1 just after it, 0 for the next command, &FF
    /// otherwise.
    pub const ENABLE: u16 = 0x10C7;
    /// The current directory and drive, then the library's.
    pub const DIR: u16 = 0x10C9;
    pub const DRIVE: u16 = 0x10CA;
    pub const LIB_DIR: u16 = 0x10CB;
    pub const LIB_DRIVE: u16 = 0x10CC;
    /// The settings a soft BREAK keeps, from `MESSAGES` to `LIB_DRIVE`.
    pub const SETTINGS: u16 = MESSAGES;
    pub const SETTINGS_LEN: u8 = (LIB_DRIVE - MESSAGES + 1).to_le_bytes()[0];
}

/// The channels: handle `h` has a block of `CHANNEL_SIZE` bytes at
/// `CHANNELS + (h - FIRST_HANDLE + 1) * CHANNEL_SIZE` and a buffer page of
/// its own.
pub mod channel {
    /// The handles issued.
    pub const FIRST_HANDLE: u8 = 0x11;
    pub const LAST_HANDLE: u8 = 0x15;
    /// The blocks' page, the first block a size past its start.
    pub const CHANNELS: u16 = 0x1100;
    pub const SIZE: u8 = 0x20;
    /// The first channel's buffer page.
    pub const FIRST_BUFFER: u8 = 0x12;

    /// A block's fields, by offset: the name, directory and drive.
    pub const NAME: u8 = 0;
    pub const DIR: u8 = 7;
    pub const DRIVE: u8 = 8;
    /// The first sector.
    pub const START: u8 = 9;
    /// Sectors the file may fill before it must grow.
    pub const ROOM: u8 = 11;
    /// PTR and EXT, three bytes each.
    pub const PTR: u8 = 13;
    pub const EXT: u8 = 16;
    /// The file's sector the buffer holds.
    pub const HELD: u8 = 19;
    /// The flags below.
    pub const FLAGS: u8 = 21;
    /// The buffer's page.
    pub const BUFFER: u8 = 22;
    const _: () = assert!(BUFFER < SIZE, "a channel's fields fit its block");
    /// The block offset past the last channel's.
    pub const END: u8 = (LAST_HANDLE - FIRST_HANDLE + 2) * SIZE;
    /// The sectors a file opened for output is first given.
    pub const OUTPUT_ROOM: u8 = 64;

    /// The channel is open.
    pub const OPEN: u8 = 0x80;
    /// It may be written.
    pub const WRITABLE: u8 = 0x40;
    /// The buffer holds a sector.
    pub const VALID: u8 = 0x20;
    /// The buffer has been written since it was read.
    pub const DIRTY: u8 = 0x10;
    /// A read has met the end once.
    pub const AT_END: u8 = 0x08;
    /// EXT has changed since the catalogue last recorded it.
    pub const EXTENDED: u8 = 0x04;
}

/// The two pages of private workspace from the page &DF0 records.
pub mod private {
    /// Non-zero while the settings are kept in the bytes after it, another
    /// ROM holding the absolute workspace.
    pub const SAVED: u8 = 0;
    /// The pages claimed.
    pub const PAGES: u8 = 2;
    /// The top bit of this ROM's byte at &DF0: set while it holds the
    /// absolute workspace.
    pub const OWNED: u8 = 0x80;
}

/// The page the absolute workspace ends at, below the private pages.
pub const ABSOLUTE_END: u8 = 0x17;
