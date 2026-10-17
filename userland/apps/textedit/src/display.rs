//! The read-only display a window may show its document as beside the text
//! and hex views: an executable's code, or a picture's structure and bytes.
//! A document opens as at most one — no container magic is also a picture's
//! signature — and scrolls the same way whichever it is.

use crate::code::CodeDisplay;
use crate::data::DataDisplay;
use crate::hex::HexLayout;

/// Bytes of a document's head a display is offered by: the longest
/// signature either reads, with room to spare.
pub const HEAD_LEN: usize = 64;

/// A window's read-only display of its document.
#[derive(Debug)]
pub enum Display {
    /// An executable's instructions.
    Code(CodeDisplay),
    /// A picture's structure, then its bytes.
    Data(DataDisplay),
}

impl Display {
    /// The display a document called `name` opening with `head` offers, if
    /// any, shown at once.
    #[must_use]
    pub fn of(head: &[u8], name: &str) -> Option<Self> {
        CodeDisplay::of(head)
            .map(Self::Code)
            .or_else(|| DataDisplay::of(head, name).map(Self::Data))
    }

    /// Whether the window shows its document this way.
    #[must_use]
    pub const fn showing(&self) -> bool {
        match self {
            Self::Code(code) => code.showing(),
            Self::Data(data) => data.showing(),
        }
    }

    /// Show the document this way, or stop, answering whether that changed.
    pub fn show(&mut self, showing: bool) -> bool {
        match self {
            Self::Code(code) => code.show(showing),
            Self::Data(data) => data.show(showing),
        }
    }

    /// How many rows it scrolls through over a document of `len` bytes.
    #[must_use]
    pub fn row_count(&self, len: usize) -> usize {
        match self {
            Self::Code(code) => code.line_count(),
            Self::Data(data) => data.row_count(len),
        }
    }

    /// The first row shown.
    #[must_use]
    pub const fn top(&self) -> usize {
        match self {
            Self::Code(code) => code.top(),
            Self::Data(data) => data.top(),
        }
    }

    /// The widest row over a document of `len` bytes, in columns.
    #[must_use]
    pub fn widest(&self, len: usize) -> usize {
        match self {
            Self::Code(code) => code.widest(),
            Self::Data(data) => data.widest(HexLayout::for_len(len).width()),
        }
    }

    /// Show from row `top`, answering whether the view moved.
    pub fn set_top(&mut self, top: usize, len: usize) -> bool {
        match self {
            Self::Code(code) => code.set_top(top),
            Self::Data(data) => data.set_top(top, len),
        }
    }

    /// Scroll by `rows`, answering whether the view moved.
    pub fn scroll(&mut self, rows: isize, len: usize) -> bool {
        match self {
            Self::Code(code) => code.scroll(rows),
            Self::Data(data) => data.scroll(rows, len),
        }
    }

    /// What the status band calls it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Code(_) => "Code",
            Self::Data(_) => "Data",
        }
    }
}
