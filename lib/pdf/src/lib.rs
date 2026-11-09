//! The first-party PDF reader (`lib/pdf`).
//!
//! [`Document::open`] reads a PDF's structure: its cross-reference, rebuilt
//! by scanning where the file's own is damaged, and its page tree. Objects are
//! read only when asked for, so opening a page does not parse the objects of
//! every other. [`Page`] is one page's geometry, and [`Page::display_list`]
//! interprets its content into a [`DisplayList`]: what it paints, in order,
//! every colour in sRGB. [`Document::about`] is what the document states
//! beyond its pages. `plans/PDF.md` is the ledger of what is read and what is
//! staged.
//!
//! Every byte of a PDF is untrusted. Reading is total: no input panics, every
//! count, depth and length is held to a fixed containment bound, every stream
//! decodes within the caller's byte budget, and a refusal is a [`PdfError`],
//! never a different document. A caller reads PDFs only in a sandbox worker.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod colour;
mod content;
mod crypt;
mod display;
mod document;
mod error;
mod filter;
#[cfg(test)]
mod fixture;
mod function;
mod image;
mod lex;
mod limits;
mod memo;
mod object;
mod optional;
mod page;
mod parse;
mod pdfdoc;
mod shading;
mod xref;

pub use display::{
    DisplayList, Gradient, GradientKind, Item, Losses, Lost, Paint, Pen, Picture, Stop,
};
pub use document::{recognised, About, Document, Options};
pub use error::{PdfError, Refused};
pub use limits::MAX_PASSWORD_BYTES;
pub use page::Page;
