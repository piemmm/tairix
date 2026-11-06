//! The screen-capture vertical's contract (`plans/CAPTURE.md` CP11): the
//! desktop's capture records the freestanding gate (`src/main.rs`) keys on,
//! generated from their own definitions by `build.rs`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

include!(concat!(env!("OUT_DIR"), "/contract.rs"));
