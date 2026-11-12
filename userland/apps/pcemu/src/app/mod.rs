//! The application above the machine session: a machine's screen in its
//! window, what the window's input means to the machine, the disks it is
//! served from, the sound it plays and the text it copies and types
//! (`plans/PCEMU.md` §3).
//!
//! Pure, so `Run` only joins it to the live window, the application's store
//! and the audio service.

pub mod address;
pub mod assistant;
pub mod bar;
pub mod debugger;
pub mod disk;
pub mod input;
pub mod keeping;
pub mod library;
pub mod media;
pub mod menu;
pub mod picture;
pub mod screen;
pub mod settings;
pub mod shelf;
pub mod sound;
pub mod state;
pub mod text;
