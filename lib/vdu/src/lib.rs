//! The Acorn VDU vocabulary the BBC Micro's MOS and RISC OS share: the control
//! codes and the parameters each takes ([`code`]), RISC OS 5's numbered screen
//! modes ([`mode`]), and the 8×8 system character set ([`font`]).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod code;
pub mod font;
pub mod mode;
