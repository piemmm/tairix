//! The BASIC stack's records. The stack descends from `HIMEM` and holds
//! everything the machine is part way through: the values and operators of
//! expressions being evaluated, what each statement waiting on one does with
//! its value, and the frames of loops, routines and `LOCAL`s. Each record
//! starts with a header word at its lowest address — its kind, a byte of its
//! own and its length, or for a block too long for the word nought there and
//! its length in the next word — so the machine can tell what it pops, and
//! refuses a stack it did not leave as "Corruption of stack". A program may
//! write over any of it: every count, index and address a record carries is
//! checked before use.
//!
//! The layout is the interpreter's own; BASIC V's frames are not a documented
//! interface. Code `CALL` enters may push below the pointer it is given and
//! must leave what is above it alone, as on RISC OS.

/// What a record is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Kind {
    /// An integer value.
    Int = 1,
    /// A real value: the dialect's bytes.
    Real = 2,
    /// A string value: its length in the header, its bytes after it.
    Str = 3,
    /// A binary operator waiting for its right operand.
    Binary = 8,
    /// An operation on the factor being read: a unary operator, a function
    /// of a factor or a binary indirection's left operand.
    Factor = 9,
    /// An open bracket.
    Bracket = 10,
    /// A function's bracketed arguments being read.
    Arguments = 11,
    /// An expression's start: what was below it waits on its value.
    Expression = 12,
    /// A statement waiting on a value, and what it carries.
    Continuation = 16,
    /// A `FOR` loop.
    For = 32,
    /// A `REPEAT` loop.
    Repeat = 33,
    /// A `WHILE` loop.
    While = 34,
    /// A `GOSUB`'s return.
    Gosub = 35,
    /// A procedure's return.
    Proc = 36,
    /// A function's return, the expression it was called from below it.
    Fn = 37,
    /// A value saved by `LOCAL` or a parameter, to be put back.
    Local = 38,
    /// An `ON ERROR` saved by `LOCAL ERROR`.
    LocalError = 39,
    /// A `DATA` pointer saved by `LOCAL DATA`.
    LocalData = 40,
    /// A block a `DIM ... LOCAL` made on the stack.
    LocalBlock = 41,
    /// Guest code the machine left for: a `CALL`, a `USR`, a SWI or a
    /// command, which the machine resumes after.
    Guest = 48,
    /// The machine's work for a routine-table call from guest code, which it
    /// returns to when done.
    Routine = 49,
}

impl Kind {
    /// The kind a header names, none for one no record has.
    pub(crate) const fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Int,
            2 => Self::Real,
            3 => Self::Str,
            8 => Self::Binary,
            9 => Self::Factor,
            10 => Self::Bracket,
            11 => Self::Arguments,
            12 => Self::Expression,
            16 => Self::Continuation,
            32 => Self::For,
            33 => Self::Repeat,
            34 => Self::While,
            35 => Self::Gosub,
            36 => Self::Proc,
            37 => Self::Fn,
            38 => Self::Local,
            39 => Self::LocalError,
            40 => Self::LocalData,
            41 => Self::LocalBlock,
            48 => Self::Guest,
            49 => Self::Routine,
            _ => return None,
        })
    }

    /// Whether it is a value.
    pub(crate) const fn is_value(self) -> bool {
        matches!(self, Self::Int | Self::Real | Self::Str)
    }
}

/// A record's header word: its kind, a byte of its own, and its length.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Header {
    /// What it is.
    pub(crate) kind: Kind,
    /// Its own byte.
    pub(crate) aux: u8,
    /// Its length in bytes, the header's word among them.
    pub(crate) length: u32,
}

impl Header {
    /// The header word of a record of `kind` with `aux` and `length` bytes,
    /// at most [`SHORT_MOST`].
    pub(crate) const fn word(kind: Kind, aux: u8, length: u32) -> u32 {
        kind as u32 | (aux as u32) << 8 | length << 16
    }

    /// The header word of a record of `kind` with `aux` whose length, which
    /// may be past [`SHORT_MOST`], is the word after it.
    pub(crate) const fn long(kind: Kind, aux: u8) -> u32 {
        Self::word(kind, aux, 0)
    }

    /// The header `word` is, none where it is no record's; `next` reads the
    /// word after it, which a long record's length is.
    pub(crate) fn of<E>(
        word: u32,
        next: impl FnOnce() -> Result<u32, E>,
    ) -> Result<Option<Self>, E> {
        let [kind, aux, ..] = word.to_le_bytes();
        let Some(kind) = Kind::of(kind) else {
            return Ok(None);
        };
        let (length, least) = match word >> 16 {
            0 => (next()?, LONG_HEADER),
            short => (short, 4),
        };
        Ok((length >= least && length.is_multiple_of(4)).then_some(Self { kind, aux, length }))
    }
}

/// The longest record a header word's own field holds.
pub(crate) const SHORT_MOST: u32 = 0xFFFC;

/// The bytes a long record's header and length take.
pub(crate) const LONG_HEADER: u32 = 8;

/// The most words a fixed record carries after its header.
pub(crate) const MOST_WORDS: usize = 16;

/// The bytes a string record of `length` bytes takes.
pub(crate) const fn string_record(length: u32) -> u32 {
    4 + length.next_multiple_of(4)
}

#[cfg(test)]
mod tests {
    use super::{string_record, Header, Kind, LONG_HEADER, SHORT_MOST};

    /// The header `word` is, a short record's: its next word unread.
    fn short(word: u32) -> Option<Header> {
        Header::of(word, || Err("a short record's next word read")).expect("not read")
    }

    #[test]
    fn a_header_holds_its_kind_byte_and_length_and_refuses_what_none_could_be() {
        let word = Header::word(Kind::Continuation, 0xAB, 24);
        assert_eq!(
            short(word),
            Some(Header {
                kind: Kind::Continuation,
                aux: 0xAB,
                length: 24
            })
        );
        assert_eq!(short(0), None, "no kind");
        assert_eq!(short(Header::word(Kind::Int, 0, 6)), None, "unaligned");
        assert_eq!(short(0x0008_0005), None, "an unknown kind");
        assert_eq!(
            short(Header::word(Kind::Int, 0, SHORT_MOST)).map(|h| h.length),
            Some(SHORT_MOST)
        );
    }

    #[test]
    fn a_long_record_s_length_is_its_next_word_however_long() {
        let long = |next: u32| Header::of(Header::long(Kind::LocalBlock, 1), || Ok::<_, ()>(next));
        assert_eq!(
            long(0x10_0008),
            Ok(Some(Header {
                kind: Kind::LocalBlock,
                aux: 1,
                length: 0x10_0008
            })),
            "past what the header word holds"
        );
        assert_eq!(
            long(LONG_HEADER).map(|h| h.map(|h| h.length)),
            Ok(Some(LONG_HEADER))
        );
        assert_eq!(long(4), Ok(None), "too short to hold its own length");
        assert_eq!(long(0x1_0002), Ok(None), "unaligned");
        assert_eq!(
            Header::of(Header::long(Kind::LocalBlock, 0), || Err("unreadable")),
            Err("unreadable"),
            "the next word's fault is the walk's"
        );
    }

    #[test]
    fn a_string_record_rounds_its_bytes_to_a_word() {
        assert_eq!(string_record(0), 4);
        assert_eq!(string_record(1), 8);
        assert_eq!(string_record(4), 8);
        assert_eq!(string_record(255), 260);
    }
}
