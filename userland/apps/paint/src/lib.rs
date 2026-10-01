//! The `Paint.app` engine: the picture and its tiles, the tools, the undo
//! history, the transforms, a document of pictures or sprites, and the state
//! of one window, host-tested and free of both windows and I/O
//! (`plans/PAINT.md`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod canvas;
pub mod colour;
pub mod dialog;
pub mod document;
pub mod fill;
pub mod history;
pub mod layout;
pub mod load;
pub mod quantize;
pub mod render;
pub mod save;
pub mod selection;
pub mod shape;
pub mod stroke;
pub mod tool;
pub mod transform;
pub mod view;
pub mod viewport;
