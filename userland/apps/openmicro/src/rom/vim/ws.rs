//! The editor's workspace: a language's zero page to [`zp::END`], and its
//! pages &04–&07. The text lies between OSHWM and HIMEM, outside it.

/// Zero page.
pub mod zp {
    /// The text's first byte: OSHWM.
    pub const BUF: u16 = 0x00;
    /// The gap's first byte, past the text before it.
    pub const GAP: u16 = 0x02;
    /// The gap's end: the text after it starts here.
    pub const GEND: u16 = 0x04;
    /// The text's end, where the undo record starts.
    pub const TEND: u16 = 0x06;
    /// The register's first byte, where the undo record ends.
    pub const REG: u16 = 0x08;
    /// The area's end: HIMEM, where the register ends.
    pub const LIMIT: u16 = 0x0A;
    /// The text's length.
    pub const LEN: u16 = 0x0C;
    /// The gap's place in the text: the length before it.
    pub const GOFF: u16 = 0x0E;
    /// The cursor's place in the text.
    pub const CUR: u16 = 0x10;
    /// Where the cursor's line starts.
    pub const LSTART: u16 = 0x12;
    /// Where the line at the screen's top starts.
    pub const TOP: u16 = 0x14;
    /// The cursor's line, from 1.
    pub const LINE: u16 = 0x16;
    /// The top line's number.
    pub const TOPLN: u16 = 0x18;
    /// The count typed before a command; 0 for none.
    pub const COUNT: u16 = 0x1A;
    /// Where the last change was made, and how much it left in its place.
    pub const UPOS: u16 = 0x1C;
    pub const UINS: u16 = 0x1E;
    /// Pointers.
    pub const P0: u16 = 0x20;
    pub const P1: u16 = 0x22;
    pub const P2: u16 = 0x24;
    pub const P3: u16 = 0x26;
    /// Sixteen-bit numbers.
    pub const N0: u16 = 0x28;
    pub const N1: u16 = 0x2A;
    pub const N2: u16 = 0x2C;
    /// The cursor's column in its line.
    pub const COL: u16 = 0x2E;
    /// The column a vertical move keeps to; [`super::END_COLUMN`] for the
    /// line's end.
    pub const WANT: u16 = 0x2F;
    /// The first column shown.
    pub const LEFT: u16 = 0x30;
    /// The text rows, above the status line.
    pub const ROWS: u16 = 0x31;
    /// The columns a row shows: one fewer than the mode's, so that writing
    /// a row's last never scrolls the screen.
    pub const WIDTH: u16 = 0x32;
    /// The screen mode.
    pub const MODE: u16 = 0x33;
    /// The highlighting: [`super::syntax`].
    pub const SYN: u16 = 0x34;
    /// What the file is and how it stands: [`super::file`].
    pub const FILE: u16 = 0x35;
    /// What the screen owes: [`super::show`].
    pub const SHOW: u16 = 0x36;
    /// The first row owed when [`super::show::FROM`] is set.
    pub const FROM_ROW: u16 = 0x37;
    /// The operator waiting for its motion, or 0.
    pub const OP: u16 = 0x38;
    /// The options: [`super::option`].
    pub const OPTIONS: u16 = 0x39;
    /// Whether the register holds whole lines.
    pub const LINEWISE: u16 = 0x3A;
    /// Eight-bit temporaries.
    pub const T0: u16 = 0x3B;
    pub const T1: u16 = 0x3C;
    pub const T2: u16 = 0x3D;
    pub const T3: u16 = 0x3E;
    pub const T4: u16 = 0x3F;
    /// The cursor's screen row.
    pub const CROW: u16 = 0x40;
    /// The line walker: the column reached, the screen column, the
    /// class being drawn and the lexer's state.
    pub const WCOL: u16 = 0x41;
    pub const WX: u16 = 0x42;
    pub const WCLASS: u16 = 0x43;
    pub const LEX: u16 = 0x44;
    /// The run the lexer last found: its class and its length.
    pub const RCLASS: u16 = 0x45;
    pub const RLEN: u16 = 0x46;
    /// The address a text access reaches, a bank's own where it lies in
    /// sideways RAM.
    pub const VP: u16 = 0x47;
    /// The repeat buffer's length, its replay's place, and whether a change
    /// is being recorded into it.
    pub const DOT_LEN: u16 = 0x49;
    pub const DOT_AT: u16 = 0x4A;
    pub const RECORD: u16 = 0x4B;
    /// Whether the undo record holds a change.
    pub const UNDO_OK: u16 = 0x4C;
    /// The key read last.
    pub const KEY: u16 = 0x4D;
    /// A motion's kind: [`super::motion`].
    pub const MKIND: u16 = 0x4E;
    /// The stack pointer the main loop runs at, which an error returns to.
    pub const STACK: u16 = 0x4F;
    /// The text a tokenised line is read from and written to, and its
    /// state, while BASIC is converted.
    pub const BASIC_IN: u16 = 0x50;
    pub const BASIC_OUT: u16 = 0x52;
    pub const BASIC_STATE: u16 = 0x54;
    /// The line number BASIC numbers must pass.
    pub const LAST_NUMBER: u16 = 0x55;
    /// The length of the line [`super::super::buffer::LINE_BUF`] holds.
    pub const LLEN: u16 = 0x57;
    /// Whether the line walker draws, in bit 7, or measures.
    pub const WDRAW: u16 = 0x58;
    /// The row being drawn, and the first row a redraw owes.
    pub const ROW: u16 = 0x59;
    pub const ROW_FIRST: u16 = 0x5A;
    /// A place held across a call.
    pub const HOLD: u16 = 0x5B;
    /// Where the message being built has reached, and whether the text a
    /// call carries ends it.
    pub const MSG_AT: u16 = 0x5D;
    pub const MSG_ENDS: u16 = 0x5E;
    /// What a file being read is for: [`super::reading`].
    pub const READING: u16 = 0x5F;
    /// An operator's range: its start and its end.
    pub const RSTART: u16 = 0x60;
    pub const REND: u16 = 0x62;
    /// The search's direction: 0 forward, otherwise back.
    pub const BACKWARD: u16 = 0x64;
    /// The command line being read: the place reached, whether `!`
    /// followed the word, what range was named, and its lines.
    pub const EX_AT: u16 = 0x65;
    pub const BANG: u16 = 0x66;
    pub const RANGE: u16 = 0x67;
    pub const LINE_A: u16 = 0x68;
    pub const LINE_B: u16 = 0x6A;
    /// A BASIC line being converted: the place reached in it, its length,
    /// and the column the listing has reached.
    pub const BASIC_AT: u16 = 0x6C;
    pub const BASIC_LEN: u16 = 0x6D;
    pub const BASIC_COL: u16 = 0x6E;
    /// The page HIMEM starts, where the text's memory runs on into sideways
    /// RAM.
    pub const MAIN_TOP: u16 = 0x6F;
    /// The highest zero page byte the editor uses, short of the user's
    /// &70 to &8F.
    pub const END: u16 = 0x6F;
    const _: () = assert!(BASIC_COL < MAIN_TOP && MAIN_TOP <= END && END < 0x70);
}

/// The bits of [`zp::FILE`].
pub mod file {
    /// The text is a BASIC program, tokenised when written.
    pub const BASIC: u8 = 0x01;
    /// Its lines end in LF when written.
    pub const LF: u8 = 0x02;
    /// Its lines end in CR LF when written.
    pub const CRLF: u8 = 0x04;
    /// Its lines end in LF CR when written, as `*SPOOL` writes them.
    pub const LFCR: u8 = 0x10;
    /// It names a file not yet written.
    pub const NEW: u8 = 0x08;
    /// It has changed since it was read or written.
    pub const MODIFIED: u8 = 0x80;
}

/// The values of [`zp::READING`].
pub mod reading {
    /// A file opened and being decoded: a failure leaves an empty text
    /// without a name.
    pub const OPENED: u8 = 0;
    /// A file read into the text: a failure leaves the text as it was.
    pub const INTO_TEXT: u8 = 1;
    /// A file being opened whose catalogue entry or bytes the filing system
    /// is still reading: an error drops its name, so nothing writes the
    /// empty text over the file.
    pub const LOADING: u8 = 0x80;
}

/// The bits of [`zp::SHOW`]: what the next refresh draws.
pub mod show {
    /// Every row.
    pub const ALL: u8 = 0x80;
    /// The rows from [`super::zp::FROM_ROW`] down.
    pub const FROM: u8 = 0x40;
    /// The cursor's row.
    pub const LINE: u8 = 0x20;
    /// The status line's message, kept until a key is read.
    pub const MESSAGE: u8 = 0x10;
    /// Insert mode, which the status line names.
    pub const INSERT: u8 = 0x08;
}

/// The bits of [`zp::OPTIONS`].
pub mod option {
    /// `number`: line numbers before each row.
    pub const NUMBER: u8 = 0x80;
}

/// The values of [`zp::SYN`].
pub mod syntax {
    /// No highlighting.
    pub const OFF: u8 = 0;
    /// BASIC statements, as a `!BOOT` file holds them.
    pub const STATEMENTS: u8 = 1;
    /// A BASIC program: statements after line numbers.
    pub const PROGRAM: u8 = 2;
}

/// The kinds of motion, in [`zp::MKIND`].
pub mod motion {
    /// The range stops before the target.
    pub const EXCLUSIVE: u8 = 0;
    /// The range takes the target in.
    pub const INCLUSIVE: u8 = 1;
    /// The range is whole lines.
    pub const LINEWISE: u8 = 2;
}

/// The column [`zp::WANT`] holds for "the line's end".
pub const END_COLUMN: u8 = 0xFF;

/// The language pages.
pub mod page {
    /// A command or search line being typed.
    pub const INPUT: u16 = 0x0400;
    /// The most it holds.
    pub const INPUT_LEN: u8 = 0x7F;
    /// The file's name, ending in CR; a CR first for none.
    pub const NAME: u16 = 0x0480;
    /// The most a name holds.
    pub const NAME_LEN: u8 = 0x3F;
    /// The message the status line shows, ending in 0.
    pub const MESSAGE: u16 = 0x04C0;
    /// The most a message holds.
    pub const MESSAGE_LEN: u8 = 0x3F;
    /// The last search's text, ending in 0.
    pub const PATTERN: u16 = 0x0500;
    /// The last substitution's replacement, ending in 0.
    pub const REPLACE: u16 = 0x0540;
    /// The last change's keys, which `.` types again.
    pub const DOT: u16 = 0x0580;
    /// The most it holds.
    pub const DOT_LEN: u8 = 0x7F;
    /// A line being converted.
    pub const CONVERT: u16 = 0x0600;
    /// OSFILE's control block.
    pub const BLOCK: u16 = 0x0700;
    /// The file's load and execution addresses, kept from its reading.
    pub const LOAD: u16 = 0x0712;
    pub const EXEC: u16 = 0x0716;
    /// The language to leave for.
    pub const LEAVE_FOR: u16 = 0x071A;
    /// Set by `*VIM` before the language is entered: a name was given.
    pub const START: u16 = 0x071B;
    /// The file the editor has open, one being written; 0 for none. An
    /// error closes it.
    pub const HANDLE: u16 = 0x071C;
    /// The column the line walk stops at, and its screen column there.
    pub const WSTOP: u16 = 0x071D;
    pub const WAT: u16 = 0x071E;
    /// What a text access keeps of X and Y, and the byte it writes.
    pub const VX: u16 = 0x071F;
    pub const VY: u16 = 0x0720;
    pub const VA: u16 = 0x0721;
    /// What `wrout` keeps of X.
    pub const VW: u16 = 0x0722;
    /// The count the last change was made with.
    pub const DOT_COUNT: u16 = 0x0723;
    /// The keyboard's status byte as the editor found it.
    pub const KEYBOARD: u16 = 0x0725;
    /// How many banks of sideways RAM the text runs on into, and the banks
    /// in their order.
    pub const SEGMENTS: u16 = 0x0726;
    pub const BANKS: u16 = 0x0727;
    /// The most it holds: what sixteen-bit addresses reach past the highest
    /// HIMEM, where the sideways area starts, so every mode reaches them.
    pub const MOST_BANKS: u8 =
        (0xFF - crate::machine::SIDEWAYS_BASE.to_be_bytes()[0]) / super::super::vmem::BANK_PAGES;
    /// A move's part: how many bytes, 0 for 256, and the banks it is read
    /// from and written to.
    pub const XC: u16 = 0x0729;
    pub const SB: u16 = 0x072A;
    pub const DB: u16 = 0x072B;
    /// The digits of a number being printed, and the most there are.
    pub const DIGITS: u16 = 0x072C;
    pub const DIGITS_LEN: u8 = 5;
    /// The routines that page sideways RAM in, which run from here since a
    /// bank paged in at &8000 pages the editor out, up to [`MARK`].
    pub const RAM_CODE: u16 = 0x0731;
    /// The mark that the text survived: [`super::SURVIVED`].
    pub const MARK: u16 = 0x07FE;
    const _: () = assert!(HANDLE + 1 == WSTOP && VW + 1 == DOT_COUNT && DOT_COUNT + 2 == KEYBOARD);
    const _: () = assert!(KEYBOARD + 1 == SEGMENTS && BANKS + MOST_BANKS as u16 == XC);
    const _: () = assert!(DB + 1 == DIGITS && DIGITS + DIGITS_LEN as u16 == RAM_CODE);
}

/// What [`page::MARK`] holds while the editor's text is in memory.
pub const SURVIVED: [u8; 2] = *b"vi";
/// [`page::START`]'s values.
pub mod start {
    /// The language was entered by a BREAK, or by `*VIM` without a name.
    pub const RESUME: u8 = 0;
    /// `*VIM` named a file to edit.
    pub const NAMED: u8 = 1;
}
