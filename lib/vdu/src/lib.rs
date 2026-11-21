//! The Acorn VDU vocabulary the BBC Micro's MOS and RISC OS share: the control
//! codes and the parameters each takes ([`code`]), VDU 25's plot codes
//! ([`plot`]), RISC OS 5's numbered screen modes ([`mode`]), the default
//! palettes and the 256-colour modes' palette arithmetic ([`palette`]), and the
//! 8×8 system character set ([`font`]).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod code;
pub mod font;
pub mod mode;
pub mod palette;
pub mod plot;
