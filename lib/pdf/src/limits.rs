//! The containment bounds: fixed validation bounds on untrusted input, never
//! capacities. A PDF past one is refused as [`TooComplex`], never truncated
//! into a different document.
//!
//! [`TooComplex`]: crate::PdfError::TooComplex

/// The deepest arrays and dictionaries nest.
pub(crate) const MAX_NESTING: usize = 64;

/// The most objects a cross-reference holds: PDF's own limit.
pub(crate) const MAX_OBJECTS: u32 = 8_388_607;

/// The most objects one object stream holds.
pub(crate) const MAX_OBJECT_STREAM_OBJECTS: usize = 100_000;

/// The longest chain of incremental updates followed through `/Prev`.
pub(crate) const MAX_XREF_SECTIONS: usize = 4096;

/// The longest chain of references one resolution follows.
pub(crate) const MAX_RESOLVE_DEPTH: usize = 32;

/// The deepest the page tree nests.
pub(crate) const MAX_PAGE_TREE_DEPTH: usize = 64;

/// The most pages a document holds.
pub(crate) const MAX_PAGES: usize = 100_000;

/// The most filters one stream chains.
pub(crate) const MAX_FILTERS: usize = 8;

/// The most kids the page tree may list, all nodes together: a tree past it
/// is refused rather than walked.
pub(crate) const MAX_PAGE_TREE_KIDS: usize = 2 * MAX_PAGES;

/// The largest magnitude a page box coordinate may have.
pub(crate) const MAX_BOX_COORDINATE: f64 = 1.0e6;

/// The most steps a document's functions take, its pages together: what
/// colour through tint transforms and shadings may cost.
pub(crate) const MAX_FUNCTION_STEPS: u64 = 1 << 30;

/// The largest user unit a page may scale by: Acrobat's own limit.
pub(crate) const MAX_USER_UNIT: f64 = 75_000.0;

/// How far into the file its `%PDF-` header may lie.
pub(crate) const HEADER_WINDOW: usize = 1024;
