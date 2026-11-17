//! Zero page (`plans/RISCOS-EMULATOR.md` §3): the kernel workspace software
//! reads directly, from &FFFF0000 and, for a 26-bit task, from zero.

/// The processor vectors.
pub const VECTORS: u32 = 0x000;
/// The escape condition, in bit 6.
pub const ESC_STATUS: u32 = 0x104;
/// Whether a reset is refused.
pub const CANNOT_RESET: u32 = 0x107;
/// Non-zero while an interrupt is handled.
pub const IRQSEMA: u32 = 0x108;
/// The monotonic clock, in centiseconds.
pub const METROGNOME: u32 = 0x10C;
/// The debugger's workspace, and its size.
pub const DEBUGGER_SPACE: u32 = 0x300;
pub const DEBUGGER_SPACE_SIZE: u32 = 8 * 16;
/// The table the processor vectors load their handlers' addresses from.
pub const PROCESSOR_VECTOR_TABLE: u32 = 0x400;
/// The input redirection's handle.
pub const REDIRECT_IN: u32 = 0xAE1;
/// The output redirection's handle.
pub const REDIRECT_OUT: u32 = 0xAE2;
/// The C library's counter.
pub const CLIB_COUNTER: u32 = 0xFE8;
/// The RISC OS library's word.
pub const RISCOS_LIB_WORD: u32 = 0xFEC;
/// The C library's word.
pub const CLIB_WORD: u32 = 0xFF0;
/// The floating-point emulator's anchor.
pub const FPE_ANCHOR: u32 = 0xFF4;
/// The Wimp's current task's domain.
pub const DOMAIN_ID: u32 = 0xFF8;
