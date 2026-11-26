//! A stack chunk, as the PRM lays it out: its five-word header, the two
//! static offsets in the words reserved to Acorn above it, and SL 560 bytes
//! above its base.

/// The mark a chunk's first word holds.
pub(crate) const MARK: u32 = 0xF606_90FF;

/// SL lies this far above its chunk's base.
pub(crate) const LIMIT_ABOVE: u32 = 560;

/// A chunk's words.
pub(crate) mod word {
    pub(crate) const MARK: u32 = 0;
    pub(crate) const NEXT: u32 = 4;
    pub(crate) const PREVIOUS: u32 = 8;
    pub(crate) const SIZE: u32 = 12;
    pub(crate) const DEALLOCATE: u32 = 16;
    /// What relocates the library's statics, and the client's.
    pub(crate) const LIBRARY_OFFSET: u32 = 20;
    pub(crate) const CLIENT_OFFSET: u32 = 24;
}

/// The chunk whose stack limit is `sl`.
pub(crate) const fn of_limit(sl: u32) -> u32 {
    sl.wrapping_sub(LIMIT_ABOVE)
}

/// The stack limit of the chunk at `chunk`.
pub(crate) const fn limit_of(chunk: u32) -> u32 {
    chunk.wrapping_add(LIMIT_ABOVE)
}
