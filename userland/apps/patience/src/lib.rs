//! Patience: Klondike, FreeCell and Spider, as the host-tested model its
//! `Run` binary composes.
#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod analysis;
pub mod art;
pub mod card;
pub mod deal;
#[cfg(test)]
mod fixture;
pub mod game;
mod input;
pub mod layout;
pub mod menu;
pub mod motion;
pub mod paint;
pub mod play;
pub mod prefs;
pub mod saved;
pub mod score;
pub mod solver;
pub mod stats;
pub mod store;
pub mod table;
pub mod variant;
