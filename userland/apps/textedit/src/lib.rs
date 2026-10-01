//! The `TextEdit.app` engine: the document, its history, its two displays,
//! and the state of one window, host-tested and free of both windows and
//! I/O (`plans/TEXTEDIT.md`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod detect;
pub mod document;
pub mod editor;
pub mod find;
pub mod hex;
pub mod highlight;
pub mod history;
pub mod layout;
pub mod load;
pub mod paint;
pub mod selection;
pub mod text;
pub mod view;
