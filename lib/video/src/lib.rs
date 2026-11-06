//! First-party TAIRiX video (`lib/video`): the VP8 codec a screen recording
//! is made with and a lossy WebP picture is one frame of, and the WebM
//! container a recording is kept in.
//!
//! Every decoder turns untrusted bytes into frames or a typed refusal — never
//! a panic — and states the memory a stream's declared geometry costs before
//! it reserves any, so a caller bounds a decode it has not begun.
//!
//! The VP8 encoder, behind the `encode` feature, codes the desktop's own
//! pictures: it states what an encoder of a size holds before one is made,
//! and reconstructs each frame with the decoder's own code, so its reference
//! is every decoder's to the bit.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod vp8;
pub mod webm;
