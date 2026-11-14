//! The machine library the emulators keep their machines in: the record of
//! every machine, the state each leaves behind, the library window that shows
//! them, how each stands, the suspends that hold a window open until its
//! machine is kept, the new-machine assistant and a card's menu
//! (`plans/BEEB.md` §6, `plans/PCEMU.md` §3).
//!
//! An application gives what is its own: a machine's fields beyond its
//! number and name, its blob names, its models and the bounds of its store.
//! With the `rt` feature, `store` carries out the library's blob work on a
//! worker and `window` puts the library window and its dialogs on the
//! desktop.
#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod assistant;
pub mod keeping;
pub mod menu;
pub mod record;
pub mod shelf;
pub mod standing;
pub mod state;
#[cfg(all(freestanding, feature = "rt"))]
pub mod store;
#[cfg(all(freestanding, feature = "rt"))]
pub mod window;

pub use record::{Found, Kept, Landed, Library, LibraryError, Loaded, Record, Writes};
pub use standing::Standing;
