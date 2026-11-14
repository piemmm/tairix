//! The MOS's documented interface, which the MOS places and every other ROM
//! calls: its entry points, vectors, calls and events, the sideways ROM
//! header it reads, and the workspace a ROM may read or fill.

use crate::machine::SIDEWAYS_BASE;

/// The entry points at the top of memory.
pub mod entry {
    /// OSEVEN: raise event Y, A its parameter.
    pub const OSEVEN: u16 = 0xFFBF;
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
    /// On the Electron: the RS423 buffer has a character for an expansion.
    pub const RS423: u8 = 0x13;
    /// On the Electron: the printer buffer has a character for an expansion.
    pub const PRINTER: u8 = 0x14;
    /// On the Electron: the 100 Hz poll, Y the ROMs still to be polled.
    pub const POLL: u8 = 0x15;
    /// On the Electron: BEL, for an external sound system.
    pub const BELL: u8 = 0x16;
    /// On the Electron: the sound buffer flushed, for an external sound
    /// system.
    pub const SOUND_FLUSH: u8 = 0x17;
}

/// A sideways ROM's header, from `&8000`.
pub mod header {
    use super::SIDEWAYS_BASE;

    /// The language entry.
    pub const LANGUAGE: u16 = SIDEWAYS_BASE;
    /// The service entry.
    pub const SERVICE: u16 = SIDEWAYS_BASE + 3;
    /// The ROM type.
    pub const TYPE: u16 = SIDEWAYS_BASE + 6;
    /// The copyright string's offset from `&8000`.
    pub const COPYRIGHT: u16 = SIDEWAYS_BASE + 7;
    /// The title, then the version string.
    pub const TITLE: u16 = SIDEWAYS_BASE + 9;
}

/// A ROM type byte's bits.
pub mod rom_type {
    /// It has a service entry.
    pub const SERVICE: u8 = 0x80;
    /// It has a language entry.
    pub const LANGUAGE: u8 = 0x40;
    /// A second processor relocation address follows the copyright.
    pub const RELOCATION: u8 = 0x20;
    /// On the Electron: the language expands the firm keys.
    pub const FIRM_KEYS: u8 = 0x10;
    /// Its code is the 6502's, not 6502 BASIC.
    pub const CODE: u8 = 0x02;
}

/// The copyright every ROM built here carries.
pub const COPYRIGHT: &str = "(C)2026TAIRiX";

/// The events, by number.
pub mod event {
    /// An output buffer became empty; X the buffer.
    pub const OUTPUT_EMPTY: u8 = 0;
    /// An input buffer was full; X the buffer, Y the character refused.
    pub const INPUT_FULL: u8 = 1;
    /// A character entered an input buffer; Y the character.
    pub const CHARACTER: u8 = 2;
    /// An ADC conversion completed; Y its channel.
    pub const CONVERTED: u8 = 3;
    /// Vertical sync began.
    pub const VSYNC: u8 = 4;
    /// The interval timer crossed zero.
    pub const INTERVAL: u8 = 5;
    /// The Escape condition was set.
    pub const ESCAPE: u8 = 6;
}

/// The OSBYTE calls the ROMs make or answer, by number.
pub mod osbyte {
    /// Flush the input buffer, or with X 0 every buffer.
    pub const FLUSH_INPUT: u8 = 0x0F;
    /// Convert the first X ADC channels in turn, none for 0.
    pub const SELECT_ADC: u8 = 0x10;
    /// Convert ADC channel X next.
    pub const FORCE_ADC: u8 = 0x11;
    /// Flush buffer X.
    pub const FLUSH_BUFFER: u8 = 0x15;
    /// On the Electron: one more paged ROM to poll.
    pub const POLL_UP: u8 = 0x16;
    /// On the Electron: one paged ROM fewer to poll.
    pub const POLL_DOWN: u8 = 0x17;
    /// Close the *SPOOL and *EXEC files.
    pub const CLOSE_SPOOL_EXEC: u8 = 0x77;
    /// Scan the keyboard from internal key 16.
    pub const SCAN_FROM_16: u8 = 0x7A;
    /// The printer's driver goes dormant.
    pub const PRINTER_DORMANT: u8 = 0x7B;
    /// Acknowledge an Escape.
    pub const ACKNOWLEDGE_ESCAPE: u8 = 0x7E;
    /// Whether file X is at its end.
    pub const EOF: u8 = 0x7F;
    /// ADVAL: an ADC channel's reading, the fire buttons, or a buffer's
    /// count.
    pub const ADVAL: u8 = 0x80;
    /// INKEY.
    pub const INKEY: u8 = 0x81;
    /// The OS high-water mark.
    pub const OSHWM: u8 = 0x83;
    /// HIMEM.
    pub const HIMEM: u8 = 0x84;
    /// HIMEM in MODE X.
    pub const HIMEM_MODE: u8 = 0x85;
    /// The text cursor's position.
    pub const CURSOR_POSITION: u8 = 0x86;
    /// Issue service call X.
    pub const SERVICE: u8 = 0x8F;
    /// Kept for applications; X 128 is the Plus 1's.
    pub const APPLICATION: u8 = 0xA3;
    /// The last BREAK's type.
    pub const LAST_BREAK: u8 = 0xFD;
}

/// The OSBYTE variables a ROM reads directly: OSBYTE `n`'s at `BASE + n`.
pub mod var {
    /// Where OSBYTE `n`'s variable lives, less `n`.
    pub const BASE: u16 = 0x0190;

    /// The variable OSBYTE `n` reads.
    #[must_use]
    pub const fn of(n: u8) -> u16 {
        BASE + n as u16
    }

    /// The ADC channel converting.
    pub const ADC_CHANNEL: u16 = of(0xBC);
    /// The highest ADC channel converted.
    pub const ADC_MAXIMUM: u16 = of(0xBD);
    /// The printer's type (OSBYTE 5).
    pub const PRINTER: u16 = of(0xF5);
}

/// Page two's ADC readings and buffer flags.
pub mod page2 {
    /// The ADC channels ADVAL reads, numbered from 1.
    pub const ADC_CHANNELS: u8 = crate::rom::asm::byte(crate::machine::ANALOGUE_CHANNELS);
    /// The ADC channels' readings, low bytes, channel 1 first.
    pub const ADC_LOW: u16 = 0x02B6;
    /// The ADC channels' readings, high bytes.
    pub const ADC_HIGH: u16 = ADC_LOW + ADC_CHANNELS as u16;
    /// The channel last converted, or 0 since a conversion was forced.
    pub const ADC_LAST: u16 = ADC_HIGH + ADC_CHANNELS as u16;
    /// Each buffer's busy flag.
    pub const BUFFER_BUSY: u16 = 0x02CF;
}

/// The buffers, by number.
pub mod buffer {
    /// The keyboard's.
    pub const KEYBOARD: u8 = 0;
    /// RS423's input.
    pub const RS423_INPUT: u8 = 1;
    /// RS423's output.
    pub const RS423_OUTPUT: u8 = 2;
    /// The printer's.
    pub const PRINTER: u8 = 3;
    /// Sound channel 0's, the first of four.
    pub const SOUND: u8 = 4;
    /// The buffers there are.
    pub const COUNT: u8 = 9;
}

/// The printer's buffer busy flag: bit 7 set while its driver is dormant.
pub const PRINTER_BUSY: u16 = page2::BUFFER_BUSY + buffer::PRINTER as u16;

/// OSBYTE 5's printer types.
pub mod printer {
    /// The parallel printer.
    pub const PARALLEL: u8 = 1;
    /// The serial printer.
    pub const SERIAL: u8 = 2;
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
