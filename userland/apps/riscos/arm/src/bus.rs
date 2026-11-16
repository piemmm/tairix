//! How the processor reaches memory.
//!
//! Every access the processor makes is naturally aligned: it applies its
//! profile's alignment rule, faulting or rotating, before the bus sees the
//! address. Whether a page may be read, written or executed, and from which
//! privilege, is the bus's to say.

/// The width of a data access.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Width {
    /// A byte.
    Byte,
    /// A halfword.
    Half,
    /// A word.
    Word,
}

impl Width {
    /// The access's size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Half => 2,
            Self::Word => 4,
        }
    }

    /// The bits of a value the access carries.
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Byte => 0xFF,
            Self::Half => 0xFFFF,
            Self::Word => u32::MAX,
        }
    }
}

/// Why the bus refused an access.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Abort {
    /// No page is mapped there.
    Translation,
    /// The page's permissions forbid the access.
    Permission,
    /// The memory system failed the access.
    External,
}

/// An instruction word and where it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fetch {
    /// The word.
    pub word: u32,
    /// Whether the word is on a host page, where a `UDF` calls the host
    /// rather than taking the undefined instruction exception.
    pub host: bool,
    /// The page of memory the word is in, by number: the physical page where
    /// the bus translates. The processor keeps its decodings by it, so each
    /// task's code at a shared address keeps its own.
    pub page: u32,
}

/// The memory a processor runs in.
pub trait Bus {
    /// The instruction word at `address`, a multiple of four, fetched with
    /// the given privilege.
    fn fetch(&mut self, address: u32, privileged: bool) -> Result<Fetch, Abort>;

    /// The `width` at `address`, a multiple of the width, zero-extended.
    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort>;

    /// Stores `value`'s low `width` at `address`, a multiple of the width.
    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort>;
}
