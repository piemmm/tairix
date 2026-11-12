//! The kernel's data in guest memory, at segment [`SEGMENT`]: the built-in
//! device headers, the List of Lists with `NUL` inside it, the swappable data
//! area holding the calls in progress, the CDS, the DPBs, the first SFT block,
//! the block driver's units and the boot-time disk buffers. Everything a
//! program reads where MS-DOS 5 documents it is laid out as documented;
//! everything else is the kernel's own.

/// The kernel's data segment, where MS-DOS keeps IO.SYS.
pub const SEGMENT: u16 = 0x0070;

/// Bytes of a device header.
pub const HEADER: u16 = 18;
/// A built-in device: its header, then the far jumps that are its strategy
/// and interrupt entries.
const DEVICE_RECORD: u16 = 32;
/// Where a built-in device's strategy entry lies after its header.
pub const STRATEGY_JUMP: u16 = HEADER;
/// Where its interrupt entry lies.
pub const INTERRUPT_JUMP: u16 = HEADER + 5;

/// The built-in devices, in the order DOS chains them after `NUL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the devices' own names")]
pub enum Builtin {
    Con,
    Aux,
    Prn,
    Clock,
    Block,
    Com1,
    Lpt1,
    Lpt2,
    Lpt3,
    Com2,
    Com3,
    Com4,
}

impl Builtin {
    /// Every built-in device, in chain order.
    pub const ALL: [Self; 12] = [
        Self::Con,
        Self::Aux,
        Self::Prn,
        Self::Clock,
        Self::Block,
        Self::Com1,
        Self::Lpt1,
        Self::Lpt2,
        Self::Lpt3,
        Self::Com2,
        Self::Com3,
        Self::Com4,
    ];

    /// Where its header lies in [`SEGMENT`].
    #[must_use]
    pub const fn header(self) -> u16 {
        DEVICES + self as u16 * DEVICE_RECORD
    }
}

/// The built-in devices' records.
pub const DEVICES: u16 = 0x0000;
const DEVICES_END: u16 = DEVICES + 12 * DEVICE_RECORD;

/// The List of Lists, which INT 21h function `52h` points at. Its first
/// fields lie below it.
pub const SYSVARS: u16 = DEVICES_END + 0x0C;

/// The List of Lists' fields, as offsets from [`SYSVARS`].
pub mod sysvars {
    /// The sharing retry count.
    pub const SHARE_RETRIES: i16 = -0x0C;
    /// The sharing retry delay.
    pub const SHARE_DELAY: i16 = -0x0A;
    /// The current disk buffer.
    pub const CURRENT_BUFFER: i16 = -0x08;
    /// Where unread `CON` input lies in the kernel's segment.
    pub const UNREAD_CON: i16 = -0x04;
    /// The first MCB's segment.
    pub const FIRST_MCB: i16 = -0x02;
    /// The first DPB.
    pub const FIRST_DPB: i16 = 0x00;
    /// The first SFT block.
    pub const FIRST_SFT: i16 = 0x04;
    /// The active `CLOCK$` device's header.
    pub const CLOCK: i16 = 0x08;
    /// The active `CON` device's header.
    pub const CON: i16 = 0x0C;
    /// The largest sector of any block device.
    pub const MAX_SECTOR: i16 = 0x10;
    /// The disk-buffer information record.
    pub const BUFFER_INFO: i16 = 0x12;
    /// The CDS array.
    pub const CDS: i16 = 0x16;
    /// The system FCB table.
    pub const FCBS: i16 = 0x1A;
    /// FCBs protected from reuse.
    pub const PROTECTED_FCBS: i16 = 0x1E;
    /// Block devices installed.
    pub const BLOCK_DEVICES: i16 = 0x20;
    /// Drive letters available: `LASTDRIVE`.
    pub const LASTDRIVE: i16 = 0x21;
    /// The `NUL` device's header itself.
    pub const NUL: i16 = 0x22;
    /// Drives `JOIN`ed.
    pub const JOINED: i16 = 0x34;
    /// The `SETVER` list, none.
    pub const SETVER: i16 = 0x37;
    /// The `x` of `BUFFERS=x,y`.
    pub const BUFFERS: i16 = 0x3F;
    /// Its `y`, the lookahead buffers.
    pub const LOOKAHEAD: i16 = 0x41;
    /// The boot drive, `1` for A:.
    pub const BOOT_DRIVE: i16 = 0x43;
    /// Whether moves use 32-bit instructions: never, below a 386.
    pub const DWORD_MOVES: i16 = 0x44;
    /// Extended memory in KiB: none, below a 286.
    pub const EXTENDED_KIB: i16 = 0x45;
    /// The List of Lists' end.
    pub const END: i16 = 0x50;
}

/// The `NUL` device's header.
pub const NUL: u16 = SYSVARS + sysvars::NUL as u16;
/// The far jumps that are `NUL`'s strategy and interrupt entries.
pub const NUL_JUMPS: u16 = (SYSVARS + 0x50 + 0x0F) & !0x0F;

/// The kernel's own state: the built-in drivers', the disk buffers' and
/// the country chosen.
pub mod driver {
    use super::NUL_JUMPS;
    use crate::bios::layout::diskette;
    /// The request a built-in strategy entry was last handed.
    pub const REQUEST: u16 = NUL_JUMPS + 0x10;
    /// `CLOCK$`'s date: days since 1980-01-01.
    pub const DAYS: u16 = REQUEST + 4;
    /// A key `CON` read ahead but has not handed over: its code, then
    /// whether it is held.
    pub const CON_AHEAD: u16 = DAYS + 2;
    /// The console's column, for tab stops and the line editor.
    pub const COLUMN: u16 = CON_AHEAD + 2;
    /// Whether console output is echoed to the printer: Ctrl+P.
    pub const ECHO: u16 = COLUMN + 1;
    /// The block driver's units.
    pub const UNIT_COUNT: u16 = CON_AHEAD + 4;
    /// The unit that last used each floppy drive, for a drive two units
    /// share.
    pub const OWNERS: u16 = UNIT_COUNT + 2;
    /// The near pointers to the units' BPBs `INIT` hands DOS.
    pub const BPB_ARRAY: u16 = OWNERS + 4;
    /// The partition table `INIT` is reading: four entries of type, first
    /// sector and sectors.
    pub const SCAN: u16 = BPB_ARRAY + 2 * super::DRIVES as u16;
    /// Bytes of an entry there.
    pub const SCAN_ENTRY: u16 = 9;
    /// Each fixed disk's extended partition's first sector, zero for none,
    /// then the heads and sectors a track it is addressed by.
    pub const EXTENDED: u16 = SCAN + 4 * SCAN_ENTRY;
    /// The line editor's template: the last line typed, its length first.
    pub const TEMPLATE: u16 = EXTENDED + 12;
    /// The line a cooked read of `CON` took and has not handed over all of:
    /// its length, where the next byte is, then the line.
    pub const CON_LINE: u16 = TEMPLATE + 130;
    /// The disk buffers: the first one's segment, then how many follow it
    /// a buffer apart. `BUFFER_INFO` in the List of Lists points here.
    pub const BUFFERS: u16 = CON_LINE + 132;
    /// The count every use of a buffer stamps it with, for choosing one to
    /// reuse.
    pub const BUFFER_CLOCK: u16 = BUFFERS + 4;
    /// The segment of the buffer stamped last.
    pub const BUFFER_LAST: u16 = BUFFER_CLOCK + 2;
    /// The country chosen, by its code.
    pub const COUNTRY: u16 = BUFFER_LAST + 2;
    /// The count every use of an FCB's file stamps its entry with.
    pub const FCB_CLOCK: u16 = COUNTRY + 2;
    /// The segment of the `STACKS=` block, zero for none.
    pub const STACKS: u16 = FCB_CLOCK + 2;
    /// A sector the block driver reads into when a transfer would cross a
    /// 64 KiB boundary, or a partition table.
    pub const BOUNCE: u16 = (STACKS + 2 + 0x0F) & !0x0F;
    /// The diskette parameters the block driver keeps INT 1Eh at: the
    /// BIOS's, copied at `INIT`, their sectors a track the medium's during
    /// each transfer and as copied between them.
    pub const DISKETTE_TABLE: u16 = BOUNCE + 512;
    /// The sectors a track the copy holds between transfers.
    pub const DISKETTE_EOT: u16 = DISKETTE_TABLE + diskette::LEN;
    /// The INT 13h vector DOS's own replaced, which it goes on to.
    pub const INT13: u16 = DISKETTE_EOT + 1;
    /// The diskette parameters INT 1Eh points at while the block driver
    /// formats a track: the medium's, its sectors a track the format's.
    pub const FORMAT_TABLE: u16 = INT13 + 4;
    /// Set once the boot has given the shared folders their drives, so one
    /// shared later takes its drive at once.
    pub const SHARES_GIVEN: u16 = FORMAT_TABLE + diskette::LEN;
    /// The media byte functions `1Bh` and `1Ch` point at for a shared
    /// drive, which has no DPB to hold one.
    pub const SHARE_MEDIA: u16 = SHARES_GIVEN + 1;
    /// The state's end.
    pub const END: u16 = SHARE_MEDIA + 1;
}

/// The swappable data area.
pub const SDA: u16 = (driver::END + 0x0F) & !0x0F;

/// The swappable data area's fields, as offsets from [`SDA`].
pub mod sda {
    /// Set while an INT 24h handler runs.
    pub const ERROR_MODE: u16 = 0x00;
    /// The `InDOS` count.
    pub const INDOS: u16 = 0x01;
    /// The drive the current critical error is on, `FFh` for none.
    pub const ERROR_DRIVE: u16 = 0x02;
    /// The last error's locus.
    pub const LOCUS: u16 = 0x03;
    /// The last error's extended code.
    pub const ERROR: u16 = 0x04;
    /// Its suggested action.
    pub const ACTION: u16 = 0x06;
    /// Its class.
    pub const CLASS: u16 = 0x07;
    /// ES:DI for the last error.
    pub const ERROR_POINTER: u16 = 0x08;
    /// The current DTA.
    pub const DTA: u16 = 0x0C;
    /// The current PSP.
    pub const PSP: u16 = 0x10;
    /// SP kept across an INT 23h.
    pub const BREAK_SP: u16 = 0x12;
    /// The last process's return code, with its termination type.
    pub const RETURN_CODE: u16 = 0x14;
    /// The current drive.
    pub const DRIVE: u16 = 0x16;
    /// The extended break flag: `BREAK=ON`.
    pub const BREAK: u16 = 0x17;
    /// The area swapped even outside DOS.
    pub const ALWAYS: u16 = 0x18;
    /// The verify flag.
    pub const VERIFY: u16 = 0x18;
    /// The memory allocation strategy.
    pub const STRATEGY: u16 = 0x19;
    /// Set by INT 1Bh: Ctrl+Break was pressed.
    pub const BREAK_HIT: u16 = 0x1A;
    /// Calls in progress.
    pub const ROOTS: u16 = 0x1B;
    /// Frames in use.
    pub const DEPTH: u16 = 0x1C;
    /// Set while an INT 23h handler's return ends the program: the next
    /// termination is Ctrl+C's.
    pub const BREAK_END: u16 = 0x1D;
    /// The country's switch character, `/`.
    pub const SWITCH: u16 = 0x1E;
    /// The interrupted call's AX, which a call restarted after INT 23h gets
    /// back.
    pub const BREAK_AX: u16 = 0x20;
    /// SS kept across an INT 23h, beside its SP.
    pub const BREAK_SS: u16 = 0x22;
    /// The frames.
    pub const FRAMES: u16 = 0x24;
}

pub use crate::frames::FRAME;
/// Frames the calls in progress share.
pub const DEPTH: u8 = 24;
/// Calls that may be in progress at once, each with its region.
pub const LEVELS: u8 = 3;
/// Bytes of a level's region: its scratch, then its guest stack.
pub const LEVEL: u16 = 0x400;
/// The levels' regions.
pub const LEVEL_BASE: u16 = SDA + sda::FRAMES + DEPTH as u16 * FRAME;

/// A level's scratch, as offsets from its region. Each routine that keeps
/// bytes here has its own area, so routines nested in one call never share
/// one.
pub mod level {
    /// The device request header its calls are made with.
    pub const REQUEST: u16 = 0x000;
    /// The directory entry last found, and where it lies.
    pub const FOUND: u16 = 0x020;
    /// `CLOCK$`'s six-byte record.
    pub const CLOCK: u16 = 0x04A;
    /// The first path buffer.
    pub const PATH: u16 = 0x050;
    /// The second, for a rename's new name.
    pub const PATH2: u16 = 0x0D0;
    /// The line editor's line and state: both path buffers, which no call
    /// reading a line uses.
    pub const LINE: u16 = PATH;
    /// Bytes the line editor keeps.
    pub const LINE_LEN: u16 = 0x100;
    /// An FCB-format name the call works with.
    pub const NAME: u16 = 0x150;
    /// The byte a character routine moves, and the one it echoes.
    pub const BYTE: u16 = 0x15C;
    /// The BPB a block driver hands over.
    pub const BPB: u16 = 0x160;
    /// EXEC's copy of the image's header.
    pub const HEADER: u16 = 0x180;
    /// The media-ID record a label change hands its drive's driver: the
    /// header's buffer, which no call making or deleting a label uses.
    pub const MEDIA_ID: u16 = HEADER;
    /// A directory entry the call keeps while it looks up another.
    pub const KEPT: u16 = 0x1A0;
    /// The drives the call has checked its media of, a bit each.
    pub const CHECKED: u16 = 0x1EC;
    /// Where the call's caller's registers lie.
    pub const CALLER: u16 = 0x1F0;
    /// The ROM entry the call returns to its caller through; `0` for one
    /// that never returns.
    pub const EXIT: u16 = 0x1F4;
    /// The frame the call's root is.
    pub const ROOT: u16 = 0x1F6;
    /// Set once a critical error of the call is failed: the call keeps its
    /// error, and the last error it records is the failure's.
    pub const FAILED: u16 = 0x1F7;
    /// A word just below the guest stack, set as the call begins: changed,
    /// the stack has overrun into the call's own records.
    pub const CANARY: u16 = 0x1FE;
    /// The guest stack's lowest byte.
    pub const STACK: u16 = 0x200;

    const _: () = assert!(FOUND + 0x2A <= CLOCK && CLOCK + 6 <= PATH && PATH + 0x80 <= PATH2);
    const _: () = assert!(LINE + LINE_LEN <= NAME && PATH2 + 0x80 <= NAME);
    const _: () = assert!(NAME + 11 <= BYTE && BYTE + 2 <= BPB && BPB + 25 <= HEADER);
    const _: () =
        assert!(HEADER + 0x1C <= KEPT && KEPT + 32 <= CHECKED && ROOT < FAILED && FAILED < CANARY);
    const _: () = assert!(MEDIA_ID + crate::dos::device::generic::media::LEN <= KEPT);
    const _: () = assert!(CANARY + 2 == STACK && STACK < super::LEVEL);
}

/// The swappable data area's end.
pub const SDA_END: u16 = LEVEL_BASE + LEVELS as u16 * LEVEL;

/// Drive letters the kernel can name.
pub const DRIVES: u8 = 26;

/// The CDS array.
pub const CDS: u16 = SDA_END;
/// Bytes of a CDS entry.
pub const CDS_ENTRY: u16 = 0x58;

/// A CDS entry's fields.
pub mod cds {
    /// The current directory, as ASCIZ `C:\...`.
    pub const PATH: u16 = 0x00;
    /// Bytes the path may take.
    pub const PATH_LEN: u16 = 0x43;
    /// The drive's attributes.
    pub const FLAGS: u16 = 0x43;
    /// The drive's DPB.
    pub const DPB: u16 = 0x45;
    /// The current directory's first cluster: `0` the root, [`UNKNOWN`]
    /// once a change of medium leaves it to be looked up again. A network
    /// drive's ID is the double word here.
    pub const CLUSTER: u16 = 0x49;
    pub const UNKNOWN: u16 = 0xFFFF;
    /// The network ID's high word.
    pub const ID_HIGH: u16 = 0x4B;
    /// The word `INT 21h AX=5F03h` keeps with a network drive.
    pub const USER_WORD: u16 = 0x4D;
    /// Where the root's backslash lies in the path.
    pub const ROOT: u16 = 0x4F;
    /// After the root's offset: the drive's type, its file system's header
    /// and the file system's own two bytes, which end the entry.
    const TAIL: u16 = 1 + 4 + 2;

    // MS-DOS 4.00's `curdir_list`, field for field.
    const _: () = assert!(
        PATH + PATH_LEN == FLAGS
            && FLAGS + 2 == DPB
            && DPB + 4 == CLUSTER
            && CLUSTER + 2 == ID_HIGH
            && ID_HIGH + 2 == USER_WORD
            && USER_WORD + 2 == ROOT
            && ROOT + 2 + TAIL == super::CDS_ENTRY
    );
    /// A drive the network redirector serves.
    pub const NETWORK: u16 = 0x8000;
    /// A physical drive.
    pub const PHYSICAL: u16 = 0x4000;
    /// A drive `SUBST` makes of a directory.
    pub const SUBST: u16 = 0x1000;
}

/// The DPBs, one per drive.
pub const DPBS: u16 = CDS + DRIVES as u16 * CDS_ENTRY;
/// Bytes of a DPB.
pub const DPB_SIZE: u16 = 0x21;

/// A DPB's fields.
pub mod dpb {
    /// The drive, `0` for A:.
    pub const DRIVE: u16 = 0x00;
    /// Its unit in its driver.
    pub const UNIT: u16 = 0x01;
    /// Bytes per sector.
    pub const SECTOR: u16 = 0x02;
    /// Sectors per cluster, less one.
    pub const CLUSTER_MASK: u16 = 0x04;
    /// The shift from clusters to sectors.
    pub const CLUSTER_SHIFT: u16 = 0x05;
    /// Reserved sectors.
    pub const RESERVED: u16 = 0x06;
    /// Copies of the FAT.
    pub const FATS: u16 = 0x08;
    /// Root directory entries.
    pub const ROOT_ENTRIES: u16 = 0x09;
    /// The first data sector.
    pub const DATA: u16 = 0x0B;
    /// The highest cluster.
    pub const MAX_CLUSTER: u16 = 0x0D;
    /// Sectors of one FAT.
    pub const FAT_SECTORS: u16 = 0x0F;
    /// The root directory's first sector.
    pub const ROOT: u16 = 0x11;
    /// The driver's header.
    pub const DRIVER: u16 = 0x13;
    /// The medium's descriptor.
    pub const MEDIA: u16 = 0x17;
    /// `FFh` while the medium must be checked before it is used.
    pub const ACCESSED: u16 = 0x18;
    /// The next DPB.
    pub const NEXT: u16 = 0x19;
    /// Where the search for a free cluster starts.
    pub const FREE_HINT: u16 = 0x1D;
    /// Free clusters, `FFFFh` while unknown.
    pub const FREE: u16 = 0x1F;
}

/// The first SFT block: its header, then its entries.
pub const SFT: u16 = (DPBS + DRIVES as u16 * DPB_SIZE + 0x0F) & !0x0F;
/// Entries in the first block, as DOS keeps there.
pub const SFT_FIRST: u16 = 5;
/// Bytes of an SFT block's header: the next block, then its entries.
pub const SFT_HEADER: u16 = 6;
/// Bytes of an SFT entry.
pub const SFT_ENTRY: u16 = 0x3B;

/// An SFT entry's fields.
pub mod sft {
    /// Handles referring to it.
    pub const HANDLES: u16 = 0x00;
    /// The mode it was opened with.
    pub const MODE: u16 = 0x02;
    /// The file's attribute byte.
    pub const ATTRIBUTE: u16 = 0x04;
    /// The device information word.
    pub const INFO: u16 = 0x05;
    /// The device's header, or the file's DPB.
    pub const DEVICE: u16 = 0x07;
    /// The file's first cluster.
    pub const FIRST: u16 = 0x0B;
    /// Its time.
    pub const TIME: u16 = 0x0D;
    /// Its date.
    pub const DATE: u16 = 0x0F;
    /// Its size.
    pub const SIZE: u16 = 0x11;
    /// The position.
    pub const POSITION: u16 = 0x15;
    /// The cluster within the file last reached.
    pub const RELATIVE: u16 = 0x19;
    /// The sector holding its directory entry.
    pub const DIR_SECTOR: u16 = 0x1B;
    /// The entry's index in it.
    pub const DIR_INDEX: u16 = 0x1F;
    /// The name, as an FCB holds it.
    pub const NAME: u16 = 0x20;
    /// The owner's PSP.
    pub const OWNER: u16 = 0x31;
    /// The cluster last reached.
    pub const CLUSTER: u16 = 0x35;
    /// An FCB entry's last use, for choosing one to reuse; sharing's field,
    /// which nothing else fills without `SHARE`.
    pub const USED: u16 = 0x2F;
    /// The mode word's bit for an entry an FCB opened.
    pub const FCB_MODE: u16 = 0x8000;
}

/// The block driver's units.
pub const UNITS: u16 = (SFT + SFT_HEADER + SFT_FIRST * SFT_ENTRY + 0x0F) & !0x0F;
/// Bytes of a unit.
pub const UNIT: u16 = 0x60;
/// Units the block driver can have.
pub const MAX_UNITS: u8 = DRIVES;

/// A unit's fields.
pub mod unit {
    /// The BIOS drive.
    pub const DRIVE: u16 = 0x00;
    /// Its kind, what its drive reports, and what it knows of its medium.
    pub const FLAGS: u16 = 0x01;
    /// Heads the BIOS addresses the drive by.
    pub const HEADS: u16 = 0x02;
    /// Sectors per track.
    pub const SECTORS: u16 = 0x03;
    /// Where the volume starts on the drive.
    pub const START: u16 = 0x04;
    /// The volume's sectors.
    pub const TOTAL: u16 = 0x08;
    /// The volume's parameter block, as DOS's BPB holds it.
    pub const BPB: u16 = 0x0C;
    /// When the medium was last reached, in ticks.
    pub const TOUCHED: u16 = 0x28;
    /// The volume's serial when last read, to tell one medium from another.
    pub const SERIAL: u16 = 0x2C;
    /// Its label then, ASCIZ.
    pub const LABEL: u16 = 0x30;
    /// A floppy drive's INT 13h type; zero for a fixed disk.
    pub const KIND: u16 = 0x3C;
    /// The parameter block the drive's media take unless told otherwise: a
    /// floppy drive's standard medium's, a partition's own, or the last one
    /// set as the drive's.
    pub const RECOMMENDED: u16 = 0x40;
    /// The unit is a floppy drive's.
    pub const FLOPPY: u8 = 0x01;
    /// The drive's change line works.
    pub const CHANGE_LINE: u8 = 0x02;
    /// The medium last learned declared a serial, so it can be told from
    /// another of its format.
    pub const IDENTIFIED: u8 = 0x04;
    /// A change of medium was seen that DOS has not yet been told of: the
    /// line it came on is clear again, so the mark keeps it.
    pub const CHANGED: u8 = 0x08;
    /// [`BPB`] was set by a program formatting the medium: building a BPB
    /// answers it, whatever the medium holds.
    pub const FAKE_BPB: u8 = 0x10;
    /// The medium may have been formatted since DOS last checked it: the
    /// next check takes nothing on trust.
    pub const FORMATTED: u8 = 0x20;
    /// No volume DOS can read was found on the partition: its access flag
    /// reads clear until a BPB is learned for it or a program sets it.
    pub const UNFORMATTED: u8 = 0x40;
}
const _: () = assert!(unit::RECOMMENDED as usize + tairix_fat::boot::DOS_BPB <= UNIT as usize);

/// Bytes of a disk buffer: its header, then a sector.
pub const BUFFER: u16 = 0x220;
/// Bytes of a buffer's header.
pub const BUFFER_HEADER: u16 = 0x20;
/// The largest sector a buffer holds, and so the largest a drive may have.
pub const SECTOR_MAX: u16 = BUFFER - BUFFER_HEADER;
/// The disk buffers the boot reads `CONFIG.SYS` through, in a block of the
/// arena given back once `BUFFERS=` has its own.
pub const BOOT_BUFFER_COUNT: u16 = 3;

/// A buffer's header fields.
pub mod buffer {
    /// The drive, `FFh` while it holds nothing.
    pub const DRIVE: u16 = 0x00;
    /// What it holds and whether it must be written back.
    pub const FLAGS: u16 = 0x01;
    /// The sector it holds.
    pub const SECTOR: u16 = 0x02;
    /// Copies to write back: a FAT's count.
    pub const COPIES: u16 = 0x06;
    /// Sectors between the copies.
    pub const STRIDE: u16 = 0x07;
    /// When it was last used, for choosing one to reuse.
    pub const USED: u16 = 0x09;
    /// The sector's bytes.
    pub const DATA: u16 = super::BUFFER_HEADER;
    /// It must be written back.
    pub const DIRTY: u8 = 0x40;
    /// It holds part of a FAT.
    pub const FAT: u8 = 0x02;
}

/// The kernel's own PSP, the shell's parent.
pub const DOS_PSP: u16 = (UNITS + MAX_UNITS as u16 * UNIT + 0x0F) & !0x0F;

/// What the boot keeps between the programs it runs: its progress through
/// `CONFIG.SYS`, the settings it found there, and the shell's line.
pub const BOOT: u16 = DOS_PSP + 0x100;

/// The boot's fields, as offsets from [`BOOT`].
pub mod boot {
    /// How far the boot has gone.
    pub const STAGE: u16 = 0x00;
    /// The next line of `CONFIG.SYS` to take, as an offset into its text.
    pub const LINE: u16 = 0x02;
    /// That line's number, for messages.
    pub const LINE_NUMBER: u16 = 0x04;
    /// The segment holding `CONFIG.SYS`'s text, `0` for none.
    pub const TEXT: u16 = 0x06;
    /// Its length.
    pub const TEXT_LEN: u16 = 0x08;
    /// `FILES=`.
    pub const FILES: u16 = 0x0A;
    /// `BUFFERS=`, and its lookahead count.
    pub const BUFFERS: u16 = 0x0B;
    /// The lookahead buffers `BUFFERS=` names.
    pub const LOOKAHEAD: u16 = 0x0C;
    /// `FCBS=`, and the entries protected from reuse.
    pub const FCBS: u16 = 0x0D;
    /// The protected FCB entries.
    pub const PROTECTED: u16 = 0x0E;
    /// `LASTDRIVE=`, as a count of drives.
    pub const LASTDRIVE: u16 = 0x0F;
    /// `STACKS=`: how many.
    pub const STACKS: u16 = 0x10;
    /// Their size.
    pub const STACK_SIZE: u16 = 0x12;
    /// The next drive letter a block driver's units take.
    pub const NEXT_DRIVE: u16 = 0x14;
    /// The boot buffers' block.
    pub const BOOT_BUFFERS: u16 = 0x16;
    /// The BIOS drive the machine booted from.
    pub const BIOS_DRIVE: u16 = 0x18;
    /// The register frame a program the boot runs ends into.
    pub const FRAME: u16 = 0x20;
    /// The EXEC parameter block.
    pub const BLOCK: u16 = 0x38;
    /// An empty FCB, the one a program the boot runs is given.
    pub const FCB: u16 = 0x48;
    /// The shell's line: its path, then its arguments.
    pub const SHELL: u16 = 0x58;
    /// The command tail a program the boot runs is given.
    pub const TAIL: u16 = 0xD8;
    /// A message the boot is writing.
    pub const MESSAGE: u16 = 0x158;
    /// Bytes the boot keeps.
    pub const LEN: u16 = 0x1C0;
}

/// The kernel's fixed data's end; the arena follows it.
pub const END: u16 = (BOOT + boot::LEN + 0x0F) & !0x0F;

/// Bytes of an SFT block of `count` entries.
#[must_use]
pub const fn sft_block(count: u16) -> u16 {
    SFT_HEADER + count * SFT_ENTRY
}

const _: () = assert!(SYSVARS as i32 + sysvars::END as i32 <= NUL_JUMPS as i32);
const _: () = assert!(END.is_multiple_of(16) && DOS_PSP.is_multiple_of(16));
