//! Draw, the desktop vector editor (`plans/DRAW.md`): the engine a document
//! window runs, host-testable apart from the runtime that hosts it.
//!
//! A window holds one [`document::Document`] — a drawing and its history —
//! seen through a [`viewport::Viewport`], edited by [`edit`]'s operations,
//! and painted from tiles rendered off the event loop ([`tiles`]).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::disallowed_methods)]

extern crate alloc;

mod bars;
pub mod compute;
pub mod document;
pub mod edit;
pub mod glyphs;
pub mod input;
pub mod inspector;
pub mod layout;
pub mod menu;
pub mod paint;
pub mod snap;
pub mod tiles;
pub mod tools;
pub mod view;
pub mod viewport;
pub mod window;
