//! The MOS's workspace, where the Advanced User Guide documents it: what
//! software reads directly, at the addresses it reads.

/// The filing system's zero page, `&B0–&CF`.
pub mod fs {
    /// The first byte.
    pub const SCRATCH: u16 = 0xB0;
    /// The pointer inline text is printed through, where MOS 1.20 leaves
    /// one; the cassette's block pointer shares it, since no message is
    /// printed while a block moves.
    pub const PRINT_POINTER: u16 = SCRATCH + 8;
    /// The cassette's speed: the 6850's divide and word bits, 5 for 1200
    /// baud and 6 for 300.
    pub const BAUD: u16 = 0xC6;
}

/// The VDU drivers' zero page, `&D0–&E1`.
pub mod vdu_zp {
    /// The VDU status byte OSBYTE &75 reads.
    pub const STATUS: u16 = 0xD0;
    /// The mask of the pixel being plotted.
    pub const PIXEL_MASK: u16 = 0xD1;
    /// Text colour OR and EOR masks.
    pub const TEXT_OR: u16 = 0xD2;
    /// Text colour EOR mask.
    pub const TEXT_EOR: u16 = 0xD3;
    /// Graphics colour OR mask.
    pub const GRAPHICS_OR: u16 = 0xD4;
    /// Graphics colour EOR mask.
    pub const GRAPHICS_EOR: u16 = 0xD5;
    /// Address of the graphics cell being plotted.
    pub const GRAPHICS_CELL: u16 = 0xD6;
    /// Address of the text cursor's top raster line.
    pub const CURSOR_LINE: u16 = 0xD8;
    /// Temporaries.
    pub const TEMP: u16 = 0xDA;
    /// The row multiplication table's address.
    pub const ROW_TABLE: u16 = 0xE0;
}

/// The MOS's own zero page, `&E2–&FF`.
pub mod os {
    /// Cassette filing system status.
    pub const CFS_STATUS: u16 = 0xE2;
    /// Cassette options: BGET/BPUT low nibble, LOAD/SAVE high.
    pub const CFS_OPTIONS: u16 = 0xE3;
    /// GSREAD's flags: the string is quoted, a space ends it.
    pub const GS_QUOTE: u16 = 0xE4;
    /// GSREAD's character being built.
    pub const GS_CHAR: u16 = 0xE5;
    /// Scratch.
    pub const TEMP: u16 = 0xE6;
    /// Auto-repeat countdown.
    pub const REPEAT_COUNT: u16 = 0xE7;
    /// OSWORD 0's buffer pointer.
    pub const LINE_BUFFER: u16 = 0xE8;
    /// Cassette/ROM filing system critical flag.
    pub const CFS_CRITICAL: u16 = 0xEB;
    /// The last key pressed, by internal number.
    pub const KEY_LAST: u16 = 0xEC;
    /// The first key pressed, by internal number.
    pub const KEY_FIRST: u16 = 0xED;
    pub use crate::rom::os::zp::{CALL_A, CALL_X, CALL_Y, ROM, TEXT};
    /// The ROM filing system's ROM.
    pub const RFS_ROM: u16 = 0xF5;
    /// The ROM filing system's pointer, and OSRDRM's address.
    pub const RFS_POINTER: u16 = 0xF6;
    /// IRQ and buffer scratch.
    pub const SCRATCH: u16 = 0xFA;
    /// A on entry to the IRQ handler.
    pub const IRQ_A: u16 = 0xFC;
    /// The last error's text, after its BRK.
    pub const ERROR: u16 = crate::rom::os::ERROR_POINTER;
    /// Bit 7 set while an Escape is pending.
    pub const ESCAPE: u16 = crate::rom::os::ESCAPE_FLAG;
}

/// The vectors, shared with every ROM.
pub use crate::rom::os::vector;

/// The OSBYTE variables: the one OSBYTE `n` reads lives at `BASE + n`.
pub mod var {
    /// Where OSBYTE `n`'s variable lives, less `n`.
    pub const BASE: u16 = 0x0190;
    /// The first OSBYTE that reads or writes a variable outright.
    pub const FIRST: u8 = 0xA6;

    /// The variable OSBYTE `n` reads.
    #[must_use]
    pub const fn of(n: u8) -> u16 {
        BASE + n as u16
    }

    /// The cassette filing system's timeout counter.
    pub const CFS_TIMEOUT: u16 = of(0xB0);
    /// The input source: 0 keyboard, 1 RS423.
    pub const INPUT_SOURCE: u16 = of(0xB1);
    /// Keyboard semaphore.
    pub const KEYBOARD_SEMAPHORE: u16 = of(0xB2);
    /// Primary OSHWM.
    pub const PRIMARY_OSHWM: u16 = of(0xB3);
    /// Current OSHWM.
    pub const OSHWM: u16 = of(0xB4);
    /// RS423 mode.
    pub const RS423_MODE: u16 = of(0xB5);
    /// How many font zones OSBYTE 20 last exploded.
    pub const FONT_STATE: u16 = of(0xB6);
    /// Cassette or ROM filing system.
    pub const CFS_OR_RFS: u16 = of(0xB7);
    /// The video ULA control register copy.
    pub const ULA_CONTROL: u16 = of(0xB8);
    /// The video ULA palette copy.
    pub const ULA_PALETTE: u16 = of(0xB9);
    /// The ROM paged in at the last BRK.
    pub const BRK_ROM: u16 = of(0xBA);
    /// BASIC's ROM.
    pub const BASIC_ROM: u16 = of(0xBB);
    /// The ADC channel converting.
    pub const ADC_CHANNEL: u16 = of(0xBC);
    /// The highest ADC channel sampled.
    pub const ADC_MAXIMUM: u16 = of(0xBD);
    /// The ADC conversion type.
    pub const ADC_TYPE: u16 = of(0xBE);
    /// The 6850 control register copy.
    pub const ACIA_CONTROL: u16 = of(0xC0);
    /// Flash countdown.
    pub const FLASH_COUNT: u16 = of(0xC1);
    /// The mark state's duration, the space state's following it.
    pub const FLASH_MARK: u16 = of(0xC2);
    /// Auto-repeat delay.
    pub const REPEAT_DELAY: u16 = of(0xC4);
    /// Auto-repeat period.
    pub const REPEAT_PERIOD: u16 = of(0xC5);
    /// *EXEC's file handle.
    pub const EXEC_HANDLE: u16 = of(0xC6);
    /// *SPOOL's file handle.
    pub const SPOOL_HANDLE: u16 = of(0xC7);
    /// ESCAPE and BREAK effects.
    pub const BREAK_EFFECT: u16 = of(0xC8);
    /// Keyboard status: the lock keys and modifiers.
    pub const KEYBOARD_STATUS: u16 = of(0xCA);
    /// RS423 handshake extent.
    pub const RS423_HANDSHAKE: u16 = of(0xCB);
    /// RS423 input suppression.
    pub const RS423_SUPPRESS: u16 = of(0xCC);
    /// Sound suppressed.
    pub const SOUND_SUPPRESS: u16 = of(0xD2);
    /// BELL channel.
    pub const BELL_CHANNEL: u16 = of(0xD3);
    /// BELL envelope or amplitude.
    pub const BELL_AMPLITUDE: u16 = of(0xD4);
    /// BELL pitch.
    pub const BELL_PITCH: u16 = of(0xD5);
    /// BELL duration.
    pub const BELL_DURATION: u16 = of(0xD6);
    /// Startup message and !BOOT options.
    pub const STARTUP: u16 = of(0xD7);
    /// Soft key string length left.
    pub const SOFT_KEY_LENGTH: u16 = of(0xD8);
    /// Lines printed since the last page halt.
    pub const PAGED_LINES: u16 = of(0xD9);
    /// Parameters still wanted by the VDU queue, negated.
    pub const VDU_QUEUE: u16 = of(0xDA);
    /// The TAB key's code.
    pub const TAB: u16 = of(0xDB);
    /// The ESCAPE character.
    pub const ESCAPE_CHAR: u16 = of(0xDC);
    /// Interpretation of input codes &C0–&FF, four zones.
    pub const INPUT_ZONES: u16 = of(0xDD);
    /// Interpretation of function keys and their shifted forms, four zones.
    pub const KEY_ZONES: u16 = of(0xE1);
    /// The ESCAPE key's action: 0 sets the Escape condition.
    pub const ESCAPE_ACTION: u16 = of(0xE5);
    /// The ESCAPE condition's effects.
    pub const ESCAPE_EFFECTS: u16 = of(0xE6);
    /// The User VIA's IRQ mask.
    pub const USER_VIA_MASK: u16 = of(0xE7);
    /// The 6850's IRQ mask.
    pub const ACIA_MASK: u16 = of(0xE8);
    /// The System VIA's IRQ mask.
    pub const SYSTEM_VIA_MASK: u16 = of(0xE9);
    /// Output streams (OSBYTE 3).
    pub const OUTPUT_STREAMS: u16 = of(0xEC);
    /// Cursor editing (OSBYTE 4).
    pub const CURSOR_EDITING: u16 = of(0xED);
    /// The country.
    pub const COUNTRY: u16 = of(0xF0);
    /// The serial ULA control register copy.
    pub const SERIAL_ULA: u16 = of(0xF2);
    /// Which TIME copy is current.
    pub const TIME_SWITCH: u16 = of(0xF3);
    /// Printer destination (OSBYTE 5).
    pub const PRINTER: u16 = of(0xF5);
    /// Printer ignore character (OSBYTE 6).
    pub const PRINTER_IGNORE: u16 = of(0xF6);
    /// The BREAK intercept: a JMP and its address.
    pub const BREAK_INTERCEPT: u16 = of(0xF7);
    /// The current language's ROM.
    pub const LANGUAGE: u16 = of(0xFC);
    /// The last BREAK: 0 soft, 1 power-on, 2 hard.
    pub const LAST_BREAK: u16 = of(0xFD);
    /// The keyboard links' startup options.
    pub const STARTUP_OPTIONS: u16 = of(0xFF);
}

/// The rest of page two.
pub mod page2 {
    /// *TV's vertical shift.
    pub const TV_SHIFT: u16 = 0x0290;
    /// *TV's interlace setting.
    pub const TV_INTERLACE: u16 = 0x0291;
    /// The two five-byte TIME clocks, most significant byte first.
    pub const CLOCKS: u16 = 0x0292;
    /// The interval timer.
    pub const INTERVAL: u16 = 0x029C;
    /// Each sideways ROM's type byte, or zero.
    pub const ROM_TYPES: u16 = 0x02A1;
    /// INKEY's countdown.
    pub const INKEY_COUNT: u16 = 0x02B1;
    /// OSWORD 0's maximum line length, lowest and highest characters.
    pub const LINE_LIMITS: u16 = 0x02B3;
    /// The ADC channels' results, low bytes then high.
    pub const ADC_LOW: u16 = 0x02B6;
    /// The ADC channels' result high bytes.
    pub const ADC_HIGH: u16 = 0x02BA;
    /// The channel last converted.
    pub const ADC_LAST: u16 = 0x02BE;
    /// Each event's enable count.
    pub const EVENTS: u16 = 0x02BF;
    /// The soft key being expanded: its offset.
    pub const SOFT_KEY_POINTER: u16 = 0x02C9;
    /// Each buffer's busy flag.
    pub const BUFFER_BUSY: u16 = 0x02CF;
    /// Each buffer's start offset.
    pub const BUFFER_START: u16 = 0x02D8;
    /// Each buffer's end offset.
    pub const BUFFER_END: u16 = 0x02E1;
    /// The OSFILE block *LOAD, *SAVE and the like build.
    pub const OSFILE_BLOCK: u16 = 0x02EE;
}

/// The VDU variables OSBYTE &A0 reads, `&0300–&037F`.
pub mod vdu {
    /// The graphics window: left, bottom, right, top, in pixels.
    pub const GRAPHICS_WINDOW: u16 = 0x0300;
    /// The text window: left, bottom, right, top, in characters.
    pub const TEXT_WINDOW: u16 = 0x0308;
    /// Text window left column.
    pub const TEXT_LEFT: u16 = 0x0308;
    /// Text window bottom row.
    pub const TEXT_BOTTOM: u16 = 0x0309;
    /// Text window right column.
    pub const TEXT_RIGHT: u16 = 0x030A;
    /// Text window top row.
    pub const TEXT_TOP: u16 = 0x030B;
    /// The graphics origin, external coordinates.
    pub const ORIGIN: u16 = 0x030C;
    /// The graphics cursor, external coordinates.
    pub const GRAPHICS_CURSOR: u16 = 0x0310;
    /// The previous graphics cursor, internal coordinates.
    pub const OLD_GRAPHICS_CURSOR: u16 = 0x0314;
    /// The text cursor's column on the screen.
    pub const CURSOR_X: u16 = 0x0318;
    /// The text cursor's row on the screen.
    pub const CURSOR_Y: u16 = 0x0319;
    /// The line within its character cell, from the top, of the pixel whose
    /// address was last found.
    pub const PIXEL_LINE: u16 = 0x031A;
    /// The VDU queue's parameters, the last at `QUEUE_END`.
    pub const QUEUE: u16 = 0x031B;
    /// The VDU queue's last byte.
    pub const QUEUE_END: u16 = 0x0323;
    /// A PLOT's destination point, internal coordinates.
    pub const PLOT_POINT: u16 = 0x0320;
    /// The graphics cursor, internal coordinates.
    pub const GRAPHICS_INTERNAL: u16 = 0x0324;
    /// Line drawing: the two deltas.
    pub const DELTAS: u16 = 0x0328;
    /// Graphics scratch.
    pub const SCRATCH: u16 = 0x0330;
    /// The text cursor's address for the 6845.
    pub const CURSOR_ADDRESS: u16 = 0x034A;
    /// The text window's width in bytes.
    pub const WINDOW_WIDTH: u16 = 0x034C;
    /// The bottom of screen memory, high byte.
    pub const SCREEN_BOTTOM: u16 = 0x034E;
    /// Bytes per character.
    pub const BYTES_PER_CHARACTER: u16 = 0x034F;
    /// Where the 6845 starts displaying.
    pub const SCREEN_START: u16 = 0x0350;
    /// Bytes per character row.
    pub const BYTES_PER_ROW: u16 = 0x0352;
    /// Screen memory's size, high byte.
    pub const SCREEN_SIZE: u16 = 0x0354;
    /// The MODE.
    pub const MODE: u16 = 0x0355;
    /// The memory map type: 0 20K, 1 16K, 2 10K, 3 8K, 4 1K.
    pub const MAP_TYPE: u16 = 0x0356;
    /// Text foreground colour.
    pub const TEXT_FOREGROUND: u16 = 0x0357;
    /// Text background colour.
    pub const TEXT_BACKGROUND: u16 = 0x0358;
    /// Graphics foreground colour.
    pub const GRAPHICS_FOREGROUND: u16 = 0x0359;
    /// Graphics background colour.
    pub const GRAPHICS_BACKGROUND: u16 = 0x035A;
    /// Foreground plot action.
    pub const FOREGROUND_ACTION: u16 = 0x035B;
    /// Background plot action.
    pub const BACKGROUND_ACTION: u16 = 0x035C;
    /// A general jump vector.
    pub const JUMP: u16 = 0x035D;
    /// The cursor start register's last value.
    pub const CURSOR_START: u16 = 0x035F;
    /// Logical colours, less one.
    pub const COLOURS: u16 = 0x0360;
    /// Pixels per byte, less one.
    pub const PIXELS_PER_BYTE: u16 = 0x0361;
    /// The left-hand pixel's colour mask.
    pub const LEFT_MASK: u16 = 0x0362;
    /// The right-hand pixel's colour mask.
    pub const RIGHT_MASK: u16 = 0x0363;
    /// While copy editing, the input position's column.
    pub const EDIT_X: u16 = 0x0364;
    /// While copy editing, the input position's row.
    pub const EDIT_Y: u16 = 0x0365;
    /// The teletext output cursor character.
    pub const TELETEXT_CURSOR: u16 = 0x0366;
    /// Which font zones are exploded.
    pub const FONT_FLAGS: u16 = 0x0367;
    /// Each 32-character zone's definitions, high byte, from code 32.
    pub const FONT_ZONES: u16 = 0x0368;
    /// The palette: each logical colour's physical one.
    pub const PALETTE: u16 = 0x036F;
}

/// The cassette and ROM filing systems' workspace, `&0380–&03DF`.
pub mod cfs {
    /// The header of the file BPUT writes.
    pub const BPUT_HEADER: u16 = 0x0380;
    /// The offset into the BPUT buffer.
    pub const BPUT_OFFSET: u16 = 0x039D;
    /// The offset into the BGET buffer.
    pub const BGET_OFFSET: u16 = 0x039E;
    /// The name of the file open for BGET.
    pub const BGET_NAME: u16 = 0x03A7;
    /// The header of the last block read.
    pub const HEADER: u16 = 0x03B2;
    /// The gap between blocks of a sequential file.
    pub const GAP: u16 = 0x03D1;
    /// The name being searched for.
    pub const SEARCH_NAME: u16 = 0x03D2;
    /// The block BGET reads next.
    pub const BGET_BLOCK: u16 = 0x03DD;
    /// The last block's flags.
    pub const LAST_FLAGS: u16 = 0x03DF;
}

/// The buffers.
pub mod buffer {
    /// The keyboard buffer.
    pub const KEYBOARD: u16 = 0x03E0;
    /// The sound workspace.
    pub const SOUND_WORKSPACE: u16 = 0x0800;
    /// The four sound channel queues.
    pub const SOUND: u16 = 0x0840;
    /// The printer buffer.
    pub const PRINTER: u16 = 0x0880;
    /// Envelopes 1–4.
    pub const ENVELOPES: u16 = 0x08C0;
    /// The RS423 output buffer, and the cassette's BPUT block.
    pub const RS423_OUTPUT: u16 = 0x0900;
    /// The speech buffer.
    pub const SPEECH: u16 = 0x09C0;
    /// The RS423, cassette and ROM input buffer.
    pub const INPUT: u16 = 0x0A00;
    /// The soft keys: offsets, then the strings.
    pub const SOFT_KEYS: u16 = 0x0B00;
    /// Definitions of characters 128–159.
    pub const FONT_RAM: u16 = 0x0C00;
}

/// Page &0D.
/// Page &D, shared with every ROM.
pub use crate::rom::os::page_d;
