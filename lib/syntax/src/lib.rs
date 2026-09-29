//! TAIRiX document syntax: what a text editor needs to colour and check a
//! document without parsing it itself (`plans/TEXTEDIT.md`).
//!
//! * [`Format`] — the closed set of formats.
//! * [`lex_line`] — one total, bounded lexer per language family, a line at
//!   a time: any bytes and any [`LineState`] word yield well-formed
//!   [`Span`]s and never panic.
//! * [`store_for_name`] and [`format_for_head`] — detection.
//! * [`validate()`] — what the system would make of a settings store, in its
//!   own parser's words.
//!
//! Every consumer runs this crate in the parser sandbox: its input is an
//! untrusted document.

#![no_std]
// `Format::COUNT` is the compiler's own count of its variants, so the list
// `Format::ALL` cannot fall out of step with the enum.
#![feature(variant_count)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod clike;
mod css;
mod data;
mod detect;
mod format;
mod lex;
mod markdown;
mod markup;
mod script;
mod stores;
mod validate;

#[cfg(test)]
mod tests;

pub use detect::{format_for_head, store_for_name};
pub use format::Format;
pub use lex::{lex_line, LineState, Span, MAX_LEX_LINE};
pub use validate::{validate, Diagnostic, Severity, MAX_DIAGNOSTICS, MAX_STORE_LEN};
