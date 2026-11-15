//! The RISC OS emulator's kernel (`plans/RISCOS-EMULATOR.md` §3, §4).
//!
//! - [`memory`]: the guest address space: its two layouts, the memory
//!   policy, task slots and the dynamic-area manager, over the processor's
//!   MMU.
//! - [`corpus`]: the recorded observation corpus's scenarios and fixtures.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod corpus;
pub mod memory;
