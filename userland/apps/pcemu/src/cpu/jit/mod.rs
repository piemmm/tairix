//! The translator's front end (`plans/PCEMU.md` PC18.2): a run of the guest's
//! code decoded once and lowered to `lib/codegen`'s IR, which leaves the
//! processor exactly as the interpreter would — its registers and flags, its
//! cycle count, its bus interface unit — at every point the block can stop.
//!
//! A block begins at a step boundary in native mode with TF clear, holds at
//! most [`MAX_INSTRUCTIONS`], and ends at a control transfer, at an
//! instruction that sets IF or inhibits interrupts, and before one it does
//! not translate: port I/O, interrupt entry and return, `HLT`, `ESC` and
//! `WAIT`, the host call, `LOCK`, a repeated string instruction, what loads
//! CS other than a jump, POPF, BOUND, ENTER, and the NEC extensions. An
//! instruction whose memory access the page map does not admit, or whose
//! division would fault, leaves the block before it, unchanged, for the
//! interpreter to take.
//!
//! The function takes the frame's, the page map's and the helper table's
//! addresses and answers an [`Exit`].

pub mod cache;
pub mod compiler;
#[cfg(any(test, feature = "test-util"))]
pub mod differential;
pub mod dispatch;
mod emit;
pub mod frame;
pub mod helper;
#[cfg(any(test, feature = "test-util"))]
pub mod interpreted;
mod lower;
pub mod native;
mod ops;
pub mod page;

pub use frame::Frame;
pub use page::PageMap;

use alloc::vec::Vec;

use tairix_codegen::ir::Function;

use super::decode::{decode, Fetch, Op, LONGEST};
use super::linear;
use super::model::CpuModel;
use lower::{Lower, Prefixes, Unit};

/// Instructions a block holds at most.
pub const MAX_INSTRUCTIONS: usize = 32;

/// Bytes a block's instructions take at most: each as long as one can be,
/// behind as many prefixes as one may carry.
pub const MOST_BLOCK_BYTES: usize = MAX_INSTRUCTIONS * (lower::MAX_PREFIXES + LONGEST);

/// Bytes of code a block compiles to at most: one that would take more is
/// left to the interpreter. The heaviest block measured, thirty-two `PUSHA`s
/// each behind fourteen prefixes, compiles to some 230 KB.
pub const MOST_CODE_BYTES: usize = 256 * 1024;

/// Why a block returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// The cycle limit was reached.
    Limit = 0,
    /// The next instruction is the interpreter's to take.
    Step = 1,
    /// The block ended where another may begin.
    Continue = 2,
}

impl Exit {
    /// The exit a block's answer names.
    #[must_use]
    pub const fn of(answer: u64) -> Option<Self> {
        match answer {
            0 => Some(Self::Limit),
            1 => Some(Self::Step),
            2 => Some(Self::Continue),
            _ => None,
        }
    }
}

/// What a block is decoded from.
pub trait Code {
    /// The byte at physical `addr`, where it is in memory a block may be
    /// translated from: RAM or ROM, but neither a card's, nor what the
    /// 80186's control block overlays, nor the emulator's own ROM, whose
    /// host calls are the interpreter's.
    fn byte(&self, addr: u32) -> Option<u8>;
}

/// A translated block.
#[derive(Clone, Debug)]
pub struct Block {
    /// The function.
    pub func: Function,
    /// Where the block begins: its code segment and offset.
    pub cs: u16,
    /// The offset.
    pub ip: u16,
    /// The pages its bytes lie in, and those the queue reads ahead into
    /// beyond its last instruction: a store to any of them invalidates it.
    pub pages: Vec<u32>,
    /// Instructions it holds.
    pub instructions: usize,
}

struct Reader<'a> {
    code: &'a dyn Code,
    cs: u16,
    ip: u16,
    ok: bool,
}

impl Fetch for Reader<'_> {
    fn fetch(&mut self) -> u8 {
        let byte = self.code.byte(linear(self.cs, self.ip));
        self.ip = self.ip.wrapping_add(1);
        self.ok &= byte.is_some();
        byte.unwrap_or(0)
    }
}

/// The block of `model`'s code at `cs:ip`, where its first instruction is
/// one the translator takes.
#[must_use]
pub fn translate(model: CpuModel, cs: u16, ip: u16, code: &dyn Code) -> Option<Block> {
    let units = decode_block(model, cs, ip, code);
    if units.is_empty() {
        return None;
    }
    let full = units.len() == MAX_INSTRUCTIONS;
    let span = covered(ip, &units);
    let code_pages = pages_of(cs, ip, span);
    let pages = pages_of(cs, ip, span + u16::from(model.queue_len()));
    let mut lower = Lower::new(model, cs, ip, &code_pages)?;
    for (n, unit) in units.iter().enumerate() {
        for k in 0..unit.prefixes.count {
            let at = unit.prefixes.first_ip.wrapping_add(u16::from(k));
            lower.prefix(at, unit.prefixes.after(k + 1));
        }
        let ended = ops::lower(&mut lower, unit);
        if ended {
            return finish(lower, cs, ip, pages, n + 1);
        }
    }
    // The block runs into an instruction it does not hold: one the
    // interpreter takes, unless the block merely filled.
    lower.exit(if full { Exit::Continue } else { Exit::Step });
    finish(lower, cs, ip, pages, units.len())
}

fn finish(lower: Lower, cs: u16, ip: u16, pages: Vec<u32>, instructions: usize) -> Option<Block> {
    let func = lower.finish().ok()?;
    Some(Block {
        func,
        cs,
        ip,
        pages,
        instructions,
    })
}

/// The block `request` asks for, translated from its bytes alone.
#[must_use]
pub fn translate_request(request: &cache::Request) -> Option<Block> {
    translate(request.model, request.key.cs, request.key.ip, request)
}

/// The bytes from `cs:ip` the block there would hold — its instructions'
/// and their prefixes' — where one begins there.
#[must_use]
pub fn span(model: CpuModel, cs: u16, ip: u16, code: &dyn Code) -> Option<u16> {
    let units = decode_block(model, cs, ip, code);
    (!units.is_empty()).then(|| covered(ip, &units))
}

/// The bytes from `ip` `units` hold, each beginning where the one before
/// ends.
fn covered(ip: u16, units: &[Unit]) -> u16 {
    units.last().map_or(0, |last| last.next.wrapping_sub(ip))
}

/// The pages the `len` bytes from `cs:ip` lie in, each once.
fn pages_of(cs: u16, ip: u16, len: u16) -> Vec<u32> {
    let mut pages: Vec<u32> = Vec::new();
    for i in 0..len {
        let page = linear(cs, ip.wrapping_add(i)) >> page::PAGE_BITS;
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    pages
}

/// The units of the block at `cs:ip`: each instruction with its prefixes,
/// up to the first the translator does not take or the one that ends it.
fn decode_block(model: CpuModel, cs: u16, ip: u16, code: &dyn Code) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut at = ip;
    while units.len() < MAX_INSTRUCTIONS {
        let Some(unit) = decode_unit(model, cs, at, code) else {
            break;
        };
        let ends = ops::ends_block(model, &unit);
        if !ops::takes(model, &unit) {
            break;
        }
        at = unit.next;
        units.push(unit);
        if ends {
            break;
        }
    }
    units
}

/// The instruction at `cs:ip` with any prefixes ahead of it.
fn decode_unit(model: CpuModel, cs: u16, ip: u16, code: &dyn Code) -> Option<Unit> {
    let mut prefixes = Prefixes::default();
    let mut at = ip;
    loop {
        let mut reader = Reader {
            code,
            cs,
            ip: at,
            ok: true,
        };
        let insn = decode(model, false, &mut reader);
        if !reader.ok {
            return None;
        }
        let Op::Prefix(prefix) = insn.op else {
            return Some(Unit {
                insn,
                ip: at,
                next: at.wrapping_add(u16::from(insn.len)),
                prefixes,
            });
        };
        // A longer run of prefixes is the interpreter's.
        if !prefixes.push(prefix, at) {
            return None;
        }
        at = at.wrapping_add(1);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
