//! BASIC's workspace: the words below and above ARGP, at the start of the
//! application space, where BASIC V keeps what a program and the code it
//! calls reach. The offsets below ARGP are BASIC V's as recorded on RISC OS
//! 5; those above it are the interpreter's own registers, which no
//! documented interface names.

/// The application space's start, where a RISC OS application is entered.
pub const BASE: u32 = 0x8000;

/// ARGP: the workspace's anchor, R8 to code `CALL` enters.
pub const ARGP: u32 = BASE + 0x700;

/// Where a program starts in BASIC V while nothing has moved it.
pub const PAGE_DEFAULT: u32 = BASE + 0xF00;

/// Where one starts in BASIC VI, whose workspace is two kilobytes longer,
/// as recorded.
pub const PAGE_DEFAULT_VI: u32 = BASE + 0x1700;

/// A word or block of the workspace, by its offset from ARGP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot(pub i32);

impl Slot {
    /// Its address.
    #[must_use]
    pub const fn at(self) -> u32 {
        ARGP.wrapping_add_signed(self.0)
    }
}

/// The string accumulator: 256 bytes, the error handler's buffer too.
pub const STRACC: Slot = Slot(-0x600);
/// The free string lists: the head of list `i` holds blocks of `4(i + 1)`
/// bytes, each block's first word the next.
pub const FREE_LISTS: Slot = Slot(-0x300);
/// The free lists there are: one for each word count a string's block has.
pub const FREE_LIST_COUNT: u32 = 64;
/// The variable chains' heads, one for each first character of a name from
/// [`FIRST_HEAD`] to [`LAST_HEAD`], indexed by it from [`FREE_LISTS`]'s base.
pub const HEADS: Slot = FREE_LISTS;
/// The procedures' chain, indexed as the character `@` would be.
pub const PROC_HEAD: u8 = 0x40;
/// The functions' chain, indexed as the character `|` would be.
pub const FN_HEAD: u8 = 0x7C;
/// The first chain's index.
pub const FIRST_HEAD: u8 = PROC_HEAD;
/// The last chain's index.
pub const LAST_HEAD: u8 = FN_HEAD;
/// `COUNT`.
pub const TALLY: Slot = Slot(-0x108);
/// `WIDTH` less one.
pub const WIDTHLOC: Slot = Slot(-0x104);
/// `@%`.
pub const AT_PERCENT: Slot = Slot(-0x100);
/// `A%`; `B%` to `Z%` follow a word apart.
pub const RESIDENT: Slot = Slot(-0xFC);
/// `PAGE`.
pub const PAGE: Slot = Slot(-0x94);
/// `TOP`.
pub const TOP: Slot = Slot(-0x90);
/// The free space's start: `END`, the variables' end.
pub const FSA: Slot = Slot(-0x8C);
/// `LOMEM`.
pub const LOMEM: Slot = Slot(-0x88);
/// `HIMEM`: the stack's top.
pub const HIMEM: Slot = Slot(-0x84);
/// The exception flags, a byte each, as recorded: Escape's, then `TRACE`'s.
pub const ESCWORD: Slot = Slot(-0x70);
/// `TRACE TO`'s file handle, then the local arrays', installed libraries',
/// transient libraries' and overlay's pointers.
pub const TRACEF: Slot = Slot(-0x68);
/// The local arrays' list.
pub const LOCALARLIST: Slot = Slot(-0x64);
/// The installed libraries' list.
pub const INSTALLLIST: Slot = Slot(-0x60);
/// The transient libraries' list.
pub const LIBRARYLIST: Slot = Slot(-0x5C);
/// The overlay structure.
pub const OVERPTR: Slot = Slot(-0x58);
/// The memory limit BASIC runs to.
pub const MEMLIMIT: Slot = Slot(-0x54);
/// The environment handlers BASIC replaced, each a code, workspace and buffer
/// word: the exit and `UpCall` handlers' workspace, as BASIC V gives them.
pub const SAVED_HANDLERS: Slot = Slot(-0x50);

/// The escape flag in [`ESCWORD`].
pub const ESCAPE: u32 = 0x80;
/// The trace flag in [`ESCWORD`], set while any `TRACE` is on.
pub const TRACING: u32 = 0x8000;

/// The line typed at the prompt, as read.
pub const TYPED: Slot = Slot(-0x500);
/// It tokenised, where an immediate line runs from.
pub const IMMEDIATE: Slot = Slot(-0x400);
/// A line's room in either.
pub const LINE_ROOM: u32 = 256;

/// The interpreter's registers: what a resumed slice carries on from.
pub mod reg {
    use super::Slot;

    /// What the machine is doing: [`crate::machine::Mode`]'s number.
    pub const MODE: Slot = Slot(0x00);
    /// The program pointer: the next byte to interpret.
    pub const PTR: Slot = Slot(0x04);
    /// The current line's start, its CR; zero for an immediate line.
    pub const LINE: Slot = Slot(0x08);
    /// The BASIC stack's pointer while guest code runs.
    pub const SP: Slot = Slot(0x0C);
    /// `ERR`.
    pub const ERR: Slot = Slot(0x10);
    /// `ERL`.
    pub const ERL: Slot = Slot(0x14);
    /// The `ON ERROR` statement's text, zero for none.
    pub const ON_ERROR: Slot = Slot(0x18);
    /// The stack's pointer an `ON ERROR` resumes with.
    pub const ON_ERROR_SP: Slot = Slot(0x1C);
    /// The next `DATA` item, zero before the first `READ`.
    pub const DATA: Slot = Slot(0x20);
    /// The interpreter's switches: [`crate::machine::flag`]'s bits.
    pub const FLAGS: Slot = Slot(0x24);
    /// `LISTO`.
    pub const LISTO: Slot = Slot(0x28);
    /// The line `TRACE` traces below, or zero for every line.
    pub const TRACE_LIMIT: Slot = Slot(0x2C);
    /// `RND`'s seed, five bytes.
    pub const SEED: Slot = Slot(0x30);
    /// The `-quit` or `QUIT` exit code, or `ERROR EXT`'s block.
    pub const EXIT_CODE: Slot = Slot(0x3C);
    /// The line the `ON ERROR` statement is on, zero in an immediate line.
    pub const ON_ERROR_LINE: Slot = Slot(0x40);
    /// The program's first line's number's high byte, which `NEW` kept for
    /// `OLD`.
    pub const OLD: Slot = Slot(0x44);
    /// The line `AUTO` numbers next.
    pub const AUTO: Slot = Slot(0x48);
    /// `AUTO`'s step, nought while it is off.
    pub const AUTO_STEP: Slot = Slot(0x4C);
    /// The assembler's `OPT`.
    pub const OPT: Slot = Slot(0x50);
    /// The last error, as RISC OS lays an error block: its number, then its
    /// text to a zero.
    pub const ERROR: Slot = Slot(0x400);
    /// The last error's room.
    pub const ERROR_ROOM: u32 = 256;
}

/// The workspace's end: where `PAGE` starts while nothing has moved it.
pub const END: u32 = PAGE_DEFAULT;

const _: () = assert!(reg::ERROR.0 + reg::ERROR_ROOM.cast_signed() <= (END - ARGP).cast_signed());
const _: () = assert!(TYPED.0 + LINE_ROOM.cast_signed() <= IMMEDIATE.0);
const _: () = assert!(IMMEDIATE.0 + LINE_ROOM.cast_signed() <= FREE_LISTS.0);
const _: () = assert!(STRACC.0 + 256 <= TYPED.0);
