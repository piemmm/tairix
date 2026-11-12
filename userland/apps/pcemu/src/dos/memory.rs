//! Memory as DOS arranges it: the chain of memory control blocks from the
//! segment the List of Lists names to the top of conventional memory, each
//! block owned by a PSP or free; and the PSP and environment a program gets.

use super::data::{sda, sysvars, SYSVARS};
use super::engine::{Far, Kernel};
use super::fs::error;

/// An MCB's fields.
pub mod mcb {
    /// `M`, or `Z` for the chain's last.
    pub const KIND: u16 = 0x00;
    /// The owner's PSP; `0` free, `8` DOS's own.
    pub const OWNER: u16 = 0x01;
    /// Paragraphs the block holds after its MCB.
    pub const SIZE: u16 = 0x03;
    /// The owner's name, eight bytes, for its main block.
    pub const NAME: u16 = 0x08;
    /// A block before the last.
    pub const MORE: u8 = b'M';
    /// The last.
    pub const LAST: u8 = b'Z';
    /// A free block's owner.
    pub const FREE: u16 = 0;
    /// The owner of blocks DOS keeps for itself.
    pub const DOS: u16 = 8;
}

/// The allocation strategies of function `58h`.
pub mod strategy {
    /// The lowest block big enough.
    pub const FIRST: u8 = 0;
    /// The smallest block big enough.
    pub const BEST: u8 = 1;
    /// The highest block big enough, its top taken.
    pub const LAST: u8 = 2;
}

/// Blocks the kernel walks before it counts the chain as damaged.
const MAX_BLOCKS: u32 = 0xA000;

/// A memory control block, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Its MCB's segment; the block's own starts a paragraph on.
    pub at: u16,
    /// `M` or `Z`.
    pub kind: u8,
    /// Its owner's PSP.
    pub owner: u16,
    /// Its paragraphs.
    pub size: u16,
}

impl Block {
    /// The block's own segment.
    #[must_use]
    pub const fn segment(self) -> u16 {
        self.at.wrapping_add(1)
    }

    /// The segment after it: the next MCB's.
    #[must_use]
    pub const fn end(self) -> u16 {
        self.at.wrapping_add(1).wrapping_add(self.size)
    }
}

/// Why a block could not be had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The chain is damaged.
    Trashed,
    /// No block is big enough; the largest has this many paragraphs.
    Short(u16),
    /// The segment names no block.
    NoBlock,
}

impl Refusal {
    /// The extended error it is.
    #[must_use]
    pub const fn error(self) -> u16 {
        match self {
            Self::Trashed => error::ARENA_TRASHED,
            Self::Short(_) => error::NO_MEMORY,
            Self::NoBlock => error::BAD_BLOCK,
        }
    }
}

impl Kernel<'_, '_> {
    /// The first MCB's segment.
    pub fn first_mcb(&mut self) -> u16 {
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::FIRST_MCB)).unwrap_or(0);
        self.get16(at)
    }

    /// The MCB at `at`, if it is one.
    pub fn block(&mut self, at: u16) -> Option<Block> {
        let kind = self.g.read8(at, mcb::KIND);
        matches!(kind, mcb::MORE | mcb::LAST).then(|| Block {
            at,
            kind,
            owner: self.g.read16(at, mcb::OWNER),
            size: self.g.read16(at, mcb::SIZE),
        })
    }

    fn set_block(&mut self, block: Block) {
        self.g.write8(block.at, mcb::KIND, block.kind);
        self.g.write16(block.at, mcb::OWNER, block.owner);
        self.g.write16(block.at, mcb::SIZE, block.size);
    }

    /// The block at `at`, a run of free blocks from it merged into one as DOS
    /// merges them when it walks: `None` if `at` or one merged is no block.
    fn merged(&mut self, at: u16) -> Option<Block> {
        let mut block = self.block(at)?;
        while block.kind == mcb::MORE && block.owner == mcb::FREE {
            let next = self.block(block.end())?;
            if next.owner != mcb::FREE {
                break;
            }
            block.size = block.size.checked_add(next.size)?.checked_add(1)?;
            block.kind = next.kind;
            self.set_block(block);
        }
        Some(block)
    }

    /// Hands each block of the chain to `visit`, free runs merged, until it
    /// answers `false` or the chain ends: `None` for a damaged chain met
    /// before then.
    fn walk(&mut self, mut visit: impl FnMut(&mut Self, Block) -> bool) -> Option<()> {
        let mut at = self.first_mcb();
        for _ in 0..MAX_BLOCKS {
            let block = self.merged(at)?;
            if !visit(self, block) || block.kind == mcb::LAST {
                return Some(());
            }
            at = block.end();
        }
        None
    }

    /// Allocates `paragraphs` for `owner` by `rule`: the block's segment.
    pub fn allocate(&mut self, paragraphs: u16, owner: u16, rule: u8) -> Result<u16, Refusal> {
        let rule = rule & 0x3F;
        let mut chosen: Option<Block> = None;
        let mut largest = 0;
        self.walk(|_, block| {
            if block.owner == mcb::FREE {
                largest = largest.max(block.size);
                let better = match (rule, chosen) {
                    (_, None) | (strategy::LAST, Some(_)) => true,
                    (strategy::BEST, Some(kept)) => block.size < kept.size,
                    _ => false,
                };
                if block.size >= paragraphs && better {
                    chosen = Some(block);
                }
            }
            true
        })
        .ok_or(Refusal::Trashed)?;
        let block = chosen.ok_or(Refusal::Short(largest))?;
        Ok(self.take(block, paragraphs, owner, rule == strategy::LAST))
    }

    /// Takes `paragraphs` of the free `block` for `owner`, from its top when
    /// `high`, leaving the rest free: the taken block's segment.
    fn take(&mut self, block: Block, paragraphs: u16, owner: u16, high: bool) -> u16 {
        let rest = block.size - paragraphs;
        if rest == 0 {
            self.set_block(Block { owner, ..block });
            return block.segment();
        }
        let rest = rest - 1;
        if high {
            let free = Block {
                kind: mcb::MORE,
                size: rest,
                ..block
            };
            let taken = Block {
                at: block.at.wrapping_add(1).wrapping_add(rest),
                kind: block.kind,
                owner,
                size: paragraphs,
            };
            self.set_block(free);
            self.set_block(taken);
            taken.segment()
        } else {
            let taken = Block {
                kind: mcb::MORE,
                owner,
                size: paragraphs,
                ..block
            };
            let free = Block {
                at: taken.end(),
                kind: block.kind,
                owner: mcb::FREE,
                size: rest,
            };
            self.set_block(taken);
            self.set_block(free);
            taken.segment()
        }
    }

    /// The block whose own segment is `segment`, if the chain has it.
    fn find_block(&mut self, segment: u16) -> Result<Block, Refusal> {
        let mut found = None;
        self.walk(|_, block| {
            if block.segment() == segment {
                found = Some(block);
            }
            found.is_none()
        })
        .ok_or(Refusal::Trashed)?;
        found.ok_or(Refusal::NoBlock)
    }

    /// Frees the block at `segment`.
    pub fn free(&mut self, segment: u16) -> Result<(), Refusal> {
        let block = self.find_block(segment)?;
        self.set_block(Block {
            owner: mcb::FREE,
            ..block
        });
        Ok(())
    }

    /// Resizes the block at `segment` to `paragraphs`, growing into a free
    /// block after it; refused, it names the most it could have.
    pub fn resize(&mut self, segment: u16, paragraphs: u16) -> Result<(), Refusal> {
        let block = self.find_block(segment)?;
        if paragraphs <= block.size {
            if paragraphs < block.size {
                let rest = block.size - paragraphs - 1;
                let kept = Block {
                    kind: mcb::MORE,
                    size: paragraphs,
                    ..block
                };
                let free = Block {
                    at: kept.end(),
                    kind: block.kind,
                    owner: mcb::FREE,
                    size: rest,
                };
                self.set_block(kept);
                self.set_block(free);
            }
            return Ok(());
        }
        let next = (block.kind == mcb::MORE)
            .then(|| self.block(block.end()))
            .flatten();
        let available = match next {
            Some(n) if n.owner == mcb::FREE => block.size.saturating_add(n.size).saturating_add(1),
            _ => block.size,
        };
        let Some(n) = next.filter(|n| n.owner == mcb::FREE && available >= paragraphs) else {
            return Err(Refusal::Short(available));
        };
        let merged = Block {
            kind: n.kind,
            size: available,
            ..block
        };
        self.set_block(merged);
        if available > paragraphs {
            self.resize(segment, paragraphs)?;
        }
        Ok(())
    }

    /// Frees every block `owner` holds.
    pub fn free_owned(&mut self, owner: u16) {
        // A damaged chain ends the freeing where the damage starts; the next
        // allocation reports it.
        let _ = self.walk(|k, block| {
            if block.owner == owner {
                k.set_block(Block {
                    owner: mcb::FREE,
                    ..block
                });
            }
            true
        });
    }

    /// Names the block at `segment` after its program: `name`'s first
    /// eight bytes.
    pub fn name_block(&mut self, segment: u16, name: &[u8]) {
        let mut field = [0u8; 8];
        for (slot, &byte) in field.iter_mut().zip(name) {
            *slot = byte;
        }
        self.g
            .write_bytes(segment.wrapping_sub(1), mcb::NAME, &field);
    }

    /// The allocation strategy.
    pub fn strategy(&mut self) -> u8 {
        self.sda8(sda::STRATEGY)
    }
}

/// A PSP's fields.
pub mod psp {
    /// INT 20h.
    pub const INT20: u16 = 0x00;
    /// The segment past the program's memory.
    pub const TOP: u16 = 0x02;
    /// The far call to DOS's CP/M entry.
    pub const CALL5: u16 = 0x05;
    /// The terminate address.
    pub const TERMINATE: u16 = 0x0A;
    /// The Ctrl+Break address.
    pub const BREAK: u16 = 0x0E;
    /// The critical error address.
    pub const CRITICAL: u16 = 0x12;
    /// The parent's PSP.
    pub const PARENT: u16 = 0x16;
    /// The first handle table, of [`JFT_COUNT`] handles.
    pub const JFT: u16 = 0x18;
    /// Handles the first table holds.
    pub const JFT_COUNT: u16 = 20;
    /// The handle table's size.
    pub const JFT_SIZE: u16 = 0x32;
    /// The handle table.
    pub const JFT_POINTER: u16 = 0x34;
    /// The environment's segment.
    pub const ENVIRONMENT: u16 = 0x2C;
    /// SS:SP at the last INT 21h call.
    pub const STACK: u16 = 0x2E;
    /// The previous PSP, for sharing; none.
    pub const PREVIOUS: u16 = 0x38;
    /// The DOS version `SETVER` reports.
    pub const VERSION: u16 = 0x40;
    /// INT 21h and RETF.
    pub const DISPATCH: u16 = 0x50;
    /// The first FCB.
    pub const FCB1: u16 = 0x5C;
    /// The second.
    pub const FCB2: u16 = 0x6C;
    /// The command tail.
    pub const TAIL: u16 = 0x80;
    /// The PSP's bytes: a COM program's image starts past them.
    pub const LEN: u16 = 0x100;
}

/// The far call to the CP/M entry DOS plants in every PSP: to `F01D:FEF0`,
/// which wraps to `0:00C0`, the jump at INT 30h, so the offset reads as the
/// memory a CP/M program has.
const CALL5: [u8; 5] = [0x9A, 0xF0, 0xFE, 0x1D, 0xF0];

impl Kernel<'_, '_> {
    /// Builds a PSP at `segment` whose memory ends at `top`, for `parent`,
    /// its handles copied from `parent`'s inheritable ones when `inherit`.
    pub fn create_psp(&mut self, segment: u16, top: u16, parent: u16, inherit: bool) {
        self.g.write_bytes(segment, 0, &[0; psp::LEN as usize]);
        self.g.write_bytes(segment, psp::INT20, &[0xCD, 0x20]);
        self.g.write16(segment, psp::TOP, top);
        self.g.write_bytes(segment, psp::CALL5, &CALL5);
        for (field, vector) in [
            (psp::TERMINATE, 0x22u16),
            (psp::BREAK, 0x23),
            (psp::CRITICAL, 0x24),
        ] {
            let offset = self.g.read16(0, vector * 4);
            let seg = self.g.read16(0, vector * 4 + 2);
            self.g.write16(segment, field, offset);
            self.g.write16(segment, field + 2, seg);
        }
        self.g.write16(segment, psp::PARENT, parent);
        let jft = [super::file::CLOSED; psp::JFT_COUNT as usize];
        self.g.write_bytes(segment, psp::JFT, &jft);
        self.g.write16(segment, psp::JFT_SIZE, psp::JFT_COUNT);
        self.g.write16(segment, psp::JFT_POINTER, psp::JFT);
        self.g.write16(segment, psp::JFT_POINTER + 2, segment);
        self.g.write_bytes(segment, psp::PREVIOUS, &[0xFF; 4]);
        self.g.write16(segment, psp::VERSION, 0x0005);
        self.g
            .write_bytes(segment, psp::DISPATCH, &[0xCD, 0x21, 0xCB]);
        self.g.write_bytes(segment, psp::FCB1 + 1, &[b' '; 11]);
        self.g.write_bytes(segment, psp::FCB2 + 1, &[b' '; 11]);
        self.g.write_bytes(segment, psp::TAIL, &[0, 0x0D]);
        if inherit && parent != 0 {
            self.inherit_handles(segment, parent);
        }
    }

    /// Copies `parent`'s handles to `child`, but those opened private, and
    /// counts the new references.
    fn inherit_handles(&mut self, child: u16, parent: u16) {
        let (table, size) = self.jft(parent);
        for handle in 0..size.min(psp::JFT_COUNT) {
            let at = table.add(handle);
            let index = self.g.read8(at.segment, at.offset);
            if index == super::file::CLOSED {
                continue;
            }
            let Some(entry) = self.sft(u16::from(index)) else {
                continue;
            };
            let mode = self.sft16(entry, super::data::sft::MODE);
            if mode.to_le_bytes()[0] & super::file::access::PRIVATE != 0 {
                continue;
            }
            let handles = self.sft16(entry, super::data::sft::HANDLES);
            self.set_sft16(entry, super::data::sft::HANDLES, handles.saturating_add(1));
            self.g.write8(child, psp::JFT + handle, index);
        }
    }

    /// The environment block at `segment`, as far as its double NUL: its
    /// bytes, the double NUL included, or `None` for one over 32 KiB.
    pub fn environment(&mut self, segment: u16) -> Option<alloc::vec::Vec<u8>> {
        let end = environment_end(self.g, segment)?;
        let mut out = alloc::vec![0u8; usize::from(end)];
        self.g.read_bytes(segment, 0, &mut out);
        Some(out)
    }

    /// The DTA.
    pub fn dta(&mut self) -> Far {
        let offset = self.sda16(sda::DTA);
        Far::new(self.sda16(sda::DTA + 2), offset)
    }

    /// Sets the DTA.
    pub fn set_dta(&mut self, at: Far) {
        self.set_sda16(sda::DTA, at.offset);
        self.set_sda16(sda::DTA + 2, at.segment);
    }
}

/// Where the variables of the environment block at `segment` end: just
/// past their double NUL, where DOS 3 and later put the count of strings
/// and the program's own path; `None` for a block over 32 KiB.
pub fn environment_end(g: &mut crate::bios::guest::Guest<'_>, segment: u16) -> Option<u16> {
    let mut previous = 1u8;
    for offset in 0..0x8000u16 {
        let byte = g.read8(segment, offset);
        if byte == 0 && previous == 0 {
            return Some(offset + 1);
        }
        previous = byte;
    }
    None
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
