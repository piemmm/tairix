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

/// The deepest one resolution goes: a chain of references, with those it
/// follows to read a stream's length, filters and object stream.
pub(crate) const MAX_RESOLVE_DEPTH: usize = 32;

/// How many times over what a file holds — its bytes, and what its object
/// streams decode to — reading its objects may lex, as may a repair's scan
/// of its bytes: each is lexed about once, so past it the file was built to
/// make its objects read one another's bytes again.
pub(crate) const MAX_LEX_PASSES: usize = 16;

/// The deepest the page tree nests.
pub(crate) const MAX_PAGE_TREE_DEPTH: usize = 64;

/// The most pages a document holds.
pub(crate) const MAX_PAGES: usize = 100_000;

/// The most filters one stream chains.
pub(crate) const MAX_FILTERS: usize = 8;

/// The longest password tried, in bytes: past what any revision hashes of
/// one, and a bound on what preparing it costs. A longer password opens
/// nothing.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// The most kids the page tree may list, all nodes together: a tree past it
/// is refused rather than walked.
pub(crate) const MAX_PAGE_TREE_KIDS: usize = 2 * MAX_PAGES;

/// The largest magnitude a page box coordinate may have.
pub(crate) const MAX_BOX_COORDINATE: f64 = 1.0e6;

/// The most content bytes a page's interpretation reads: its own, and a
/// form's again each time it is painted.
pub(crate) const MAX_CONTENT_BYTES: usize = 1 << 28;

/// The most items a page's display list holds, of every kind.
pub(crate) const MAX_ITEMS: usize = 1 << 20;

/// The most steps a document's evaluations take, its pages together: what
/// colour through tint transforms and shadings may cost, and the optional
/// content tests deciding what is shown.
pub(crate) const MAX_EVALUATION_STEPS: u64 = 1 << 30;

/// The most functions one function is built of, one it shares counted each
/// time it is met.
pub(crate) const MAX_FUNCTION_NODES: usize = 4096;

/// The most expressions one visibility expression is built of, one it
/// shares counted each time it is met.
pub(crate) const MAX_EXPRESSION_NODES: usize = 4096;

/// The largest user unit a page may scale by: Acrobat's own limit.
pub(crate) const MAX_USER_UNIT: f64 = 75_000.0;

/// How far into the file its `%PDF-` header may lie.
pub(crate) const HEADER_WINDOW: usize = 1024;
