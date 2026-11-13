//! The MOS's documented interface, which the MOS places and every other ROM
//! calls: its entry points and vectors.

/// The entry points at the top of memory.
pub mod entry {
    /// GSINIT: begin reading a string.
    pub const GSINIT: u16 = 0xFFC2;
    /// GSREAD: a string's next character.
    pub const GSREAD: u16 = 0xFFC5;
    /// OSFIND: open or close a file.
    pub const OSFIND: u16 = 0xFFCE;
    /// OSGBPB: bytes to or from a file.
    pub const OSGBPB: u16 = 0xFFD1;
    /// OSBPUT: a byte to a file.
    pub const OSBPUT: u16 = 0xFFD4;
    /// OSBGET: a byte from a file.
    pub const OSBGET: u16 = 0xFFD7;
    /// OSARGS: a file's or the filing system's arguments.
    pub const OSARGS: u16 = 0xFFDA;
    /// OSFILE: a whole file.
    pub const OSFILE: u16 = 0xFFDD;
    /// OSRDCH: a character from the input stream.
    pub const OSRDCH: u16 = 0xFFE0;
    /// OSASCI: OSWRCH, a carriage return as a new line.
    pub const OSASCI: u16 = 0xFFE3;
    /// OSNEWL: a new line.
    pub const OSNEWL: u16 = 0xFFE7;
    /// OSWRCR: a carriage return.
    pub const OSWRCR: u16 = 0xFFEC;
    /// OSWRCH: a character to the output streams.
    pub const OSWRCH: u16 = 0xFFEE;
    /// OSWORD.
    pub const OSWORD: u16 = 0xFFF1;
    /// OSBYTE.
    pub const OSBYTE: u16 = 0xFFF4;
    /// OSCLI: a command line.
    pub const OSCLI: u16 = 0xFFF7;
}

/// The vectors, `&0200–&0235`.
pub mod vector {
    /// USERV: *CODE and *LINE.
    pub const USER: u16 = 0x0200;
    /// BRKV: errors.
    pub const BRK: u16 = 0x0202;
    /// IRQ1V: every interrupt.
    pub const IRQ1: u16 = 0x0204;
    /// IRQ2V: interrupts the MOS does not handle.
    pub const IRQ2: u16 = 0x0206;
    /// CLIV: OSCLI.
    pub const CLI: u16 = 0x0208;
    /// BYTEV: OSBYTE.
    pub const BYTE: u16 = 0x020A;
    /// WORDV: OSWORD.
    pub const WORD: u16 = 0x020C;
    /// WRCHV: OSWRCH.
    pub const WRCH: u16 = 0x020E;
    /// RDCHV: OSRDCH.
    pub const RDCH: u16 = 0x0210;
    /// FILEV: OSFILE.
    pub const FILE: u16 = 0x0212;
    /// ARGSV: OSARGS.
    pub const ARGS: u16 = 0x0214;
    /// BGETV: OSBGET.
    pub const BGET: u16 = 0x0216;
    /// BPUTV: OSBPUT.
    pub const BPUT: u16 = 0x0218;
    /// GBPBV: OSGBPB.
    pub const GBPB: u16 = 0x021A;
    /// FINDV: OSFIND.
    pub const FIND: u16 = 0x021C;
    /// FSCV: filing system control.
    pub const FSC: u16 = 0x021E;
    /// EVNTV: events.
    pub const EVENT: u16 = 0x0220;
    /// UPTV: the user print routine.
    pub const UPT: u16 = 0x0222;
    /// VDUV: unrecognised PLOT and VDU 23.
    pub const VDU: u16 = 0x0226;
    /// KEYV: the keyboard.
    pub const KEY: u16 = 0x0228;
    /// INSV: insert into a buffer.
    pub const INS: u16 = 0x022A;
    /// REMV: remove from a buffer.
    pub const REM: u16 = 0x022C;
    /// CNPV: count or purge a buffer.
    pub const CNP: u16 = 0x022E;
    /// The vectors there are.
    pub const COUNT: u8 = 27;
}

/// The error pointer the MOS leaves at an error's number, past its BRK.
pub const ERROR_POINTER: u16 = 0xFD;
/// Bit 7 set while an Escape is pending.
pub const ESCAPE_FLAG: u16 = 0xFF;

/// The MOS's zero page a sideways ROM reads.
pub mod zp {
    /// A of the last OSBYTE or OSWORD.
    pub const CALL_A: u16 = 0xEF;
    /// X of the last OSBYTE or OSWORD.
    pub const CALL_X: u16 = 0xF0;
    /// Y of the last OSBYTE or OSWORD.
    pub const CALL_Y: u16 = 0xF1;
    /// The text pointer GSINIT and *commands read.
    pub const TEXT: u16 = 0xF2;
    /// The ROM paged in.
    pub const ROM: u16 = 0xF4;
}

/// Page &D: the NMI routine, the extended vectors and the ROMs' private
/// workspace pages.
pub mod page_d {
    /// The NMI handler's home.
    pub const NMI: u16 = 0x0D00;
    /// The extended vectors: address and ROM, three bytes each.
    pub const EXTENDED_VECTORS: u16 = 0x0D9F;
    /// Each sideways ROM's private workspace page, by slot.
    pub const PRIVATE_WORKSPACE: u16 = 0x0DF0;
}

/// Where vector `n`'s extended entry point is: three bytes each from here.
pub const EXTENDED_ENTRIES: u16 = 0xFF00;

/// The service calls the MOS offers the sideways ROMs, by number.
pub mod service {
    /// Claim absolute workspace from &0E00.
    pub const ABSOLUTE_WORKSPACE: u8 = 1;
    /// Claim private workspace.
    pub const PRIVATE_WORKSPACE: u8 = 2;
    /// Reset: a filing system starts, and boots if Y is zero.
    pub const AUTO_BOOT: u8 = 3;
    /// A command the MOS does not know.
    pub const COMMAND: u8 = 4;
    /// An interrupt the MOS does not handle.
    pub const INTERRUPT: u8 = 5;
    /// A BRK.
    pub const BRK: u8 = 6;
    /// An OSBYTE the MOS does not know.
    pub const OSBYTE: u8 = 7;
    /// An OSWORD the MOS does not know.
    pub const OSWORD: u8 = 8;
    /// *HELP.
    pub const HELP: u8 = 9;
    /// Another ROM claims the absolute workspace.
    pub const CLAIM_WORKSPACE: u8 = 0x0A;
    /// The NMI routine is released to the ROM in Y.
    pub const NMI_RELEASE: u8 = 0x0B;
    /// Another ROM claims the NMI routine.
    pub const NMI_CLAIM: u8 = 0x0C;
    /// The ROM filing system starts a ROM.
    pub const RFS_START: u8 = 0x0D;
    /// The ROM filing system reads a byte.
    pub const RFS_BYTE: u8 = 0x0E;
    /// A filing system has claimed the vectors.
    pub const VECTORS_CLAIMED: u8 = 0x0F;
    /// The font has been exploded or imploded.
    pub const FONT: u8 = 0x11;
    /// Start filing system Y.
    pub const SELECT_FILING_SYSTEM: u8 = 0x12;
}

/// FSCV's reasons.
pub mod fsc {
    /// *OPT X,Y.
    pub const OPT: u8 = 0;
    /// Whether handle X is at its end.
    pub const EOF: u8 = 1;
    /// */ and a file to run.
    pub const SLASH: u8 = 2;
    /// A command no ROM knows.
    pub const COMMAND: u8 = 3;
    /// *RUN.
    pub const RUN: u8 = 4;
    /// *CAT.
    pub const CAT: u8 = 5;
    /// Another filing system takes over.
    pub const SHUT_DOWN: u8 = 6;
    /// The range of handles the filing system issues.
    pub const HANDLES: u8 = 7;
    /// OSCLI is about to act on a command line.
    pub const COMMAND_LINE: u8 = 8;
}

/// The filing systems OSARGS 0 names.
pub mod filing_system {
    /// The cassette at 1200 baud.
    pub const CFS_1200: u8 = 1;
    /// The cassette at 300 baud.
    pub const CFS_300: u8 = 2;
    /// The ROM filing system.
    pub const RFS: u8 = 3;
    /// The disc filing system.
    pub const DFS: u8 = 4;
}
