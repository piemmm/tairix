//! The C library's statics, which follow the kernel's in the library's: where
//! each thing lies, counted from the library statics' start.
//!
//! What a client's code reaches by offset is where RISC OS 5.30 has it —
//! `errno`, the stream blocks, the `ctype` table — as are the buffers and
//! tables whose addresses the library hands out and the random number
//! generator's state; the rest lies in what those leave free.

/// The C statics' size.
pub(crate) const SIZE: u32 = 0xB48;
/// Where they start in the library's statics: after the kernel's.
pub(crate) const START: u32 = crate::kernel::statics::SIZE;

/// Where each thing lies, from the library statics' start.
pub(crate) mod at {
    use super::START;

    /// `errno`.
    pub(crate) const ERRNO: u32 = START;
    /// The streams' blocks, `stdin`, `stdout` and `stderr` first.
    pub(crate) const STREAMS: u32 = START + 0x004;
    /// When the program started, by `OS_ReadMonotonicTime`: what `clock`
    /// counts from.
    pub(crate) const CLOCK_START: u32 = START + 0x284;
    /// The `ctype` table: a byte for each character, EOF's before it.
    pub(crate) const CTYPE: u32 = START + 0x290;
    /// `HUGE_VAL`, as the FPA stores a double.
    pub(crate) const HUGE_VAL: u32 = START + 0x390;
    /// The heap: where its first arena starts, the part of its last not yet
    /// made blocks, the bytes freed since its free blocks were last merged,
    /// which free lists have blocks, and the free lists.
    pub(crate) const HEAP_FIRST: u32 = START + 0x398;
    pub(crate) const HEAP_TOP: u32 = START + 0x39C;
    pub(crate) const HEAP_END: u32 = START + 0x3A0;
    pub(crate) const HEAP_FREED: u32 = START + 0x3A4;
    pub(crate) const HEAP_LIST_MAP: u32 = START + 0x3A8;
    pub(crate) const HEAP_LISTS: u32 = START + 0x3B0;
    /// The territory's upper and lower case tables the `CTYPE` category
    /// converts by; nought in the C locale.
    pub(crate) const UPPER_TABLE: u32 = START + 0x494;
    pub(crate) const LOWER_TABLE: u32 = START + 0x498;
    /// What the time functions lend the OS: a five-byte time, a territory's
    /// ordinals, a format code, and the text a territory writes.
    pub(crate) const FIVE_BYTE_TIME: u32 = START + 0x49C;
    pub(crate) const ORDINALS: u32 = START + 0x4A4;
    pub(crate) const FORMAT_CODE: u32 = START + 0x4C0;
    pub(crate) const TERRITORY_TEXT: u32 = START + 0x4C4;
    /// Each category's territory, nought for the C locale: `LC_COLLATE`,
    /// `LC_CTYPE`, `LC_MONETARY`, `LC_NUMERIC` and `LC_TIME`'s.
    pub(crate) const CATEGORIES: u32 = START + 0x53C;
    /// The `struct lconv` `localeconv` answers.
    pub(crate) const LCONV: u32 = START + 0x550;
    /// The C locale's strings `localeconv` points at: ".", then "".
    pub(crate) const C_STRINGS: u32 = START + 0x61C;
    /// The buffer `setlocale` writes its answer in.
    pub(crate) const LOCALE_NAME: u32 = START + 0x620;
    /// Each signal's handler, signal 1's first.
    pub(crate) const SIGNALS: u32 = START + 0x64C;
    /// How many names `tmpnam` has made, and the buffer it answers in when
    /// given none.
    pub(crate) const TMPNAM_COUNT: u32 = START + 0x674;
    pub(crate) const TMPNAM_TEXT: u32 = START + 0x680;
    /// The random number generator: whether it is seeded, the seed it was
    /// given mixed, its words, and its two indexes, counted from one.
    pub(crate) const RANDOM_SEEDED: u32 = START + 0x6D4;
    pub(crate) const RANDOM_SEED: u32 = START + 0x6D8;
    pub(crate) const RANDOM_STATE: u32 = START + 0x6DC;
    pub(crate) const RANDOM_INDEXES: u32 = START + 0x7B4;
    /// Where `strtok` goes on from.
    pub(crate) const TOKEN_NEXT: u32 = START + 0x7C0;
    /// The heap block `getenv` answers its values in.
    pub(crate) const GETENV: u32 = START + 0x7C4;
    /// The bytes the heap's arenas hold.
    pub(crate) const HEAP_BYTES: u32 = START + 0x7C8;
    /// The functions `atexit` registered: how many, the first of them in
    /// turn, and the heap block holding those past them.
    pub(crate) const ATEXIT_COUNT: u32 = START + 0x7CC;
    pub(crate) const ATEXIT: u32 = START + 0x7D0;
    pub(crate) const ATEXIT_MORE: u32 = START + 0x9C8;
    /// The functions `at_quick_exit` registered, laid as `atexit`'s are.
    pub(crate) const QUICK_EXIT_COUNT: u32 = START + 0x9CC;
    pub(crate) const QUICK_EXIT: u32 = START + 0x9D0;
    pub(crate) const QUICK_EXIT_MORE: u32 = START + 0xA50;
    /// The buffer `strerror` writes its message in.
    pub(crate) const ERROR_TEXT: u32 = START + 0x8EC;
    /// The text `asctime` and `ctime` answer.
    pub(crate) const TIME_TEXT: u32 = START + 0x93C;
    /// The `struct tm`s `gmtime` and `localtime` answer.
    pub(crate) const GMTIME: u32 = START + 0x980;
    pub(crate) const LOCALTIME: u32 = START + 0x9A4;
}

/// The streams' blocks: how many, and each one's bytes.
pub(crate) const STREAMS: u32 = 16;
pub(crate) const STREAM_BYTES: u32 = 40;
/// The `ctype` table's bytes.
pub(crate) const CTYPE_BYTES: u32 = 256;
/// The heap's free lists, and the words of the map of those with blocks.
pub(crate) const HEAP_LIST_COUNT: u32 = 57;
pub(crate) const HEAP_LIST_MAP_WORDS: u32 = HEAP_LIST_COUNT.div_ceil(32);
/// The categories there are.
pub(crate) const CATEGORY_COUNT: u32 = 5;
/// The `struct lconv`'s bytes: ten string pointers, then eight characters.
pub(crate) const LCONV_BYTES: u32 = 48;
/// The C locale's strings' bytes.
pub(crate) const C_STRINGS_BYTES: u32 = 4;
/// `setlocale`'s buffer's bytes.
pub(crate) const LOCALE_NAME_BYTES: u32 = 44;
/// The signals there are, numbered from one.
pub(crate) const SIGNAL_COUNT: u32 = 10;
/// The functions an exit's list keeps in the statics; the rest go to the
/// heap.
pub(crate) const EXIT_LIST_HERE: u32 = 32;
/// The random number generator's words.
pub(crate) const RANDOM_WORDS: u32 = 54;
/// `strerror`'s buffer's bytes.
pub(crate) const ERROR_TEXT_BYTES: u32 = 80;
/// `asctime`'s text's bytes: the longest any `struct tm` makes, each of its
/// six numbers eleven characters, and the terminator.
pub(crate) const TIME_TEXT_BYTES: u32 = 68;
/// A `struct tm`'s words.
pub(crate) const TM_WORDS: u32 = 9;
/// `L_tmpnam`: the bytes a `tmpnam` name takes, its terminator counted.
pub(crate) const L_TMPNAM: u32 = 32;
/// A territory's ordinals' words: centiseconds to year.
pub(crate) const ORDINAL_WORDS: u32 = 7;
/// The bytes a territory may write a time's text in.
pub(crate) const TERRITORY_TEXT_BYTES: u32 = 120;

/// Each field's start and size, in order: none overlaps the next, and the
/// last ends within the statics, which the build checks.
const LAYOUT: [(u32, u32); 42] = [
    (START, 0),
    (at::ERRNO, 4),
    (at::STREAMS, STREAMS * STREAM_BYTES),
    (at::CLOCK_START, 4),
    (at::CTYPE, CTYPE_BYTES),
    (at::HUGE_VAL, 8),
    (at::HEAP_FIRST, 4),
    (at::HEAP_TOP, 4),
    (at::HEAP_END, 4),
    (at::HEAP_FREED, 4),
    (at::HEAP_LIST_MAP, 4 * HEAP_LIST_MAP_WORDS),
    (at::HEAP_LISTS, 4 * HEAP_LIST_COUNT),
    (at::UPPER_TABLE, 4),
    (at::LOWER_TABLE, 4),
    (at::FIVE_BYTE_TIME, 5),
    (at::ORDINALS, 4 * ORDINAL_WORDS),
    (at::FORMAT_CODE, 4),
    (at::TERRITORY_TEXT, TERRITORY_TEXT_BYTES),
    (at::CATEGORIES, 4 * CATEGORY_COUNT),
    (at::LCONV, LCONV_BYTES),
    (at::C_STRINGS, C_STRINGS_BYTES),
    (at::LOCALE_NAME, LOCALE_NAME_BYTES),
    (at::SIGNALS, 4 * SIGNAL_COUNT),
    (at::TMPNAM_COUNT, 4),
    (at::TMPNAM_TEXT, L_TMPNAM),
    (at::RANDOM_SEEDED, 4),
    (at::RANDOM_SEED, 4),
    (at::RANDOM_STATE, 4 * RANDOM_WORDS),
    (at::RANDOM_INDEXES, 8),
    (at::TOKEN_NEXT, 4),
    (at::GETENV, 4),
    (at::HEAP_BYTES, 4),
    (at::ATEXIT_COUNT, 4),
    (at::ATEXIT, 4 * EXIT_LIST_HERE),
    (at::ERROR_TEXT, ERROR_TEXT_BYTES),
    (at::TIME_TEXT, TIME_TEXT_BYTES),
    (at::GMTIME, 4 * TM_WORDS),
    (at::LOCALTIME, 4 * TM_WORDS),
    (at::ATEXIT_MORE, 4),
    (at::QUICK_EXIT_COUNT, 4),
    (at::QUICK_EXIT, 4 * EXIT_LIST_HERE),
    (at::QUICK_EXIT_MORE, 4),
];

const _: () = {
    let mut n = 1;
    while n < LAYOUT.len() {
        let (start, size) = LAYOUT[n - 1];
        assert!(start + size <= LAYOUT[n].0, "the C statics' fields overlap");
        n += 1;
    }
    let (start, size) = LAYOUT[LAYOUT.len() - 1];
    assert!(start + size <= START + SIZE, "past the C statics");
};
