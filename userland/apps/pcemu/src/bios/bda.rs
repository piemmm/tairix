//! The BIOS data area at `0040:0000`: the firmware's state, where DOS and
//! programs read and patch it.

pub const SEGMENT: u16 = 0x40;
/// The serial ports the POST found, COM1 first.
pub const COM_PORTS: u16 = 0x00;
/// The parallel ports the POST found, LPT1 first.
pub const LPT_PORTS: u16 = 0x08;
pub const EQUIPMENT: u16 = 0x10;
pub const MEMORY_KIB: u16 = 0x13;
pub const SHIFT_FLAGS: u16 = 0x17;
pub const SHIFT_FLAGS_2: u16 = 0x18;
/// The code typed so far on the keypad with Alt held.
pub const ALT_KEYPAD: u16 = 0x19;
pub const KEY_HEAD: u16 = 0x1A;
pub const KEY_TAIL: u16 = 0x1C;
/// Bit 7: the diskette interrupt came. Bits 0–3: a drive that need not be
/// recalibrated.
pub const SEEK_STATUS: u16 = 0x3E;
/// Bits 0–3: a drive's motor is on. Bit 7: the operation writes.
pub const MOTOR_STATUS: u16 = 0x3F;
/// Ticks until the motors stop.
pub const MOTOR_COUNT: u16 = 0x40;
pub const DISKETTE_STATUS: u16 = 0x41;
/// The controller's last result bytes, seven.
pub const FDC_RESULT: u16 = 0x42;
pub const VIDEO_MODE: u16 = 0x49;
pub const COLUMNS: u16 = 0x4A;
pub const PAGE_SIZE: u16 = 0x4C;
pub const PAGE_START: u16 = 0x4E;
/// Each page's cursor, eight words of column then row.
pub const CURSOR: u16 = 0x50;
pub const CURSOR_SHAPE: u16 = 0x60;
pub const ACTIVE_PAGE: u16 = 0x62;
pub const CRTC_PORT: u16 = 0x63;
pub const MODE_REGISTER: u16 = 0x65;
pub const PALETTE: u16 = 0x66;
/// Where the option ROM scan has reached: an offset, then a segment.
pub const OPTION_ROM: u16 = 0x67;
pub const TICKS: u16 = 0x6C;
pub const MIDNIGHT: u16 = 0x70;
/// Bit 7: Ctrl+Break was pressed.
pub const BREAK: u16 = 0x71;
/// [`WARM`] asks the next POST for a warm start.
pub const RESET_FLAG: u16 = 0x72;
/// The reset flag a warm start leaves, as Ctrl+Alt+Del does.
pub const WARM: u16 = 0x1234;
pub const FIXED_STATUS: u16 = 0x74;
pub const FIXED_COUNT: u16 = 0x75;
pub const LPT_TIMEOUT: u16 = 0x78;
pub const COM_TIMEOUT: u16 = 0x7C;
pub const KEY_START: u16 = 0x80;
pub const KEY_END: u16 = 0x82;
/// The EGA's and VGA's text rows, less one.
pub const ROWS: u16 = 0x84;
/// Scan lines a character takes.
pub const CHAR_HEIGHT: u16 = 0x85;
/// Bit 7: the last mode set kept the memory. Bits 5–6: the card's memory.
/// Bit 1: a monochrome display. Bit 0: cursor emulation off.
pub const EGA_INFO: u16 = 0x87;
/// The EGA's configuration switches, low nibble; the VGA keeps there the
/// EGA it emulates at 350 lines.
pub const EGA_SWITCHES: u16 = 0x88;
/// The VGA's mode-set options: bits 7 and 4 the next text mode's lines
/// (`00` 350, `01` 400, `10` 200), bit 3 palette loading off, bit 2 a
/// monochrome display, bit 1 grey summing, bit 0 the VGA active.
pub const VGA_FLAGS: u16 = 0x89;
/// The entry in the display combination table naming the displays.
pub const DCC_INDEX: u16 = 0x8A;
/// The data rate last selected: bits 6–7, as the CCR's low two bits.
pub const DATA_RATE: u16 = 0x8B;
/// Each drive's media state: bits 0–2 what is known of the medium, bit 4
/// that it is established, bit 5 double stepping, bits 6–7 its data rate.
pub const MEDIA_STATE: u16 = 0x90;
/// Drives 0 and 1's present cylinders.
pub const CYLINDER: u16 = 0x94;
/// The EGA's and VGA's save pointer table, as a far pointer.
pub const SAVE_POINTERS: u16 = 0xA8;

/// The print-screen status byte at `0050:0000`.
pub const PRINT_STATUS_SEGMENT: u16 = 0x50;
