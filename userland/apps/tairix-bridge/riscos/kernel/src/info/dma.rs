//! `OS_Memory` 19: a DMA transfer's buffer translated for its driver.
//!
//! The caller's input function hands over the buffer's regions one by one;
//! each is cut at page boundaries and translated, and the pieces go to its
//! output function joined wherever they are logically and physically
//! contiguous and alike. Nothing caches memory here, but the processor says
//! it does, so a DMA write's partial cache lines at a region's ends are marked
//! for a bounce buffer, as RISC OS 5 marks them: a line shared with other data
//! could be written back over what the transfer wrote.

use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_arm::psr;

use crate::error::BAD_ADDRESS;
use crate::memory::AreaFlags;
use crate::os::{Entry, Fail, Os, Regs, Target};

use crate::error::BAD_PARAMETERS;

/// The input function gives physical addresses.
const PHYSICAL_INPUT: u32 = 1 << 8;
/// The transfer writes to RAM.
const DMA_WRITES: u32 = 1 << 9;
/// The transfer is over: the regions are walked again, nothing output.
const COMPLETE: u32 = 1 << 10;
/// The functions use 64-bit physical addresses.
const WIDE: u32 = 1 << 11;
/// A region's flag: a bounce buffer is or must be used.
const BOUNCE: u32 = 1;

/// One translated piece of the buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Piece {
    logical: u32,
    physical: u64,
    length: u32,
    flags: u32,
}

impl Piece {
    /// Whether `next` carries straight on from this, alike.
    const fn joins(&self, next: &Self) -> bool {
        self.flags == next.flags
            && self.logical.wrapping_add(self.length) == next.logical
            && self.physical + self.length as u64 == next.physical
    }
}

/// The calls made so far: their registers' values for the next.
struct Walk {
    flags: u32,
    workspace: u32,
    input: Target,
    output: Target,
    input_state: u32,
    output_state: u32,
    /// The piece waiting to be joined or output.
    pending: Option<Piece>,
}

impl Os<'_> {
    /// `OS_Memory` 19.
    pub(crate) fn memory_dma(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let flags = regs.r[0] >> 8 << 8;
        if flags >> 12 != 0 {
            return Err(self.error(BAD_PARAMETERS));
        }
        let bits = self.caller_bits();
        let mut walk = Walk {
            flags,
            workspace: regs.r[1],
            input: Target {
                address: regs.r[3],
                bits,
            },
            output: Target {
                address: regs.r[5],
                bits,
            },
            input_state: regs.r[2],
            output_state: regs.r[4],
            pending: None,
        };
        let result = self.dma_walk(&mut walk);
        regs.r[2] = walk.input_state;
        regs.r[4] = walk.output_state;
        result
    }

    fn dma_walk(&mut self, walk: &mut Walk) -> Result<(), Fail> {
        loop {
            let mut call = Regs::default();
            call.r[9] = walk.input_state;
            call.r[12] = walk.workspace;
            self.dma_call(walk.input, &mut call)?;
            walk.input_state = call.r[9];
            let (start, length, given) = if walk.flags & WIDE == 0 {
                (u64::from(call.r[0]), call.r[1], call.r[2])
            } else {
                (
                    u64::from(call.r[0]) | u64::from(call.r[1]) << 32,
                    call.r[3],
                    call.r[2],
                )
            };
            if length == 0 {
                return self.dma_flush(walk);
            }
            self.dma_translate(walk, start, length, given & BOUNCE)?;
        }
    }

    /// Cuts the region at `start` of `length` bytes into pieces at page
    /// boundaries and the partial cache lines a DMA write leaves.
    fn dma_translate(
        &mut self,
        walk: &mut Walk,
        start: u64,
        length: u32,
        given: u32,
    ) -> Result<(), Fail> {
        let line = self
            .world
            .processor
            .cpu()
            .cache(0)
            .map_or(1, |cache| u64::from(cache.line));
        let end = start + u64::from(length);
        let mut at = start;
        while at < end {
            let page_end = (at / u64::from(PAGE_SIZE) + 1) * u64::from(PAGE_SIZE);
            let (logical, physical, cacheable) = self.dma_place(walk.flags, at)?;
            let mut stop = page_end.min(end);
            let mut flags = given;
            if walk.flags & DMA_WRITES != 0 && given == 0 && cacheable {
                let line_end = (at / line + 1) * line;
                if !at.is_multiple_of(line) || line_end > end {
                    stop = stop.min(line_end);
                    flags = BOUNCE;
                } else {
                    stop = stop.min(end - end % line);
                }
            }
            let length = u32::try_from(stop - at).map_err(|_| self.error(BAD_PARAMETERS))?;
            self.dma_add(
                walk,
                Piece {
                    logical,
                    physical,
                    length,
                    flags,
                },
            )?;
            at = stop;
            self.yield_point()?;
        }
        Ok(())
    }

    /// The logical and physical address of the byte at `address`, as the
    /// input gives it, and whether its page is cacheable.
    fn dma_place(&mut self, flags: u32, address: u64) -> Result<(u32, u64, bool), Fail> {
        let page_size = u64::from(PAGE_SIZE);
        let offset = u32::try_from(address % page_size).unwrap_or(0);
        let (page, logical) = if flags & PHYSICAL_INPUT != 0 {
            let page = u32::try_from(address / page_size)
                .ok()
                .filter(|&page| page < self.memory.ceiling_pages())
                .ok_or_else(|| self.error(BAD_ADDRESS))?;
            let logical = match self.memory.page_map(page) {
                Some((logical, _)) if logical != u32::MAX => logical + offset,
                _ => u32::MAX,
            };
            (page, logical)
        } else {
            let logical = u32::try_from(address).map_err(|_| self.error(BAD_ADDRESS))?;
            let page = self
                .memory
                .page_at(logical)
                .ok_or_else(|| self.error(BAD_ADDRESS))?;
            (page, logical)
        };
        let page_flags = self.memory.page_map(page).map_or(0, |(_, flags)| flags);
        let physical = u64::from(page) * page_size + u64::from(offset);
        Ok((
            logical,
            physical,
            page_flags & AreaFlags::NOT_CACHEABLE == 0,
        ))
    }

    /// Joins `piece` to the one waiting, or outputs that and keeps this.
    fn dma_add(&mut self, walk: &mut Walk, piece: Piece) -> Result<(), Fail> {
        match &mut walk.pending {
            Some(pending) if pending.joins(&piece) => {
                pending.length += piece.length;
                Ok(())
            }
            _ => {
                self.dma_flush(walk)?;
                walk.pending = Some(piece);
                Ok(())
            }
        }
    }

    /// Outputs the piece waiting, if any; a finished transfer outputs none.
    fn dma_flush(&mut self, walk: &mut Walk) -> Result<(), Fail> {
        let Some(piece) = walk.pending.take() else {
            return Ok(());
        };
        if walk.flags & COMPLETE != 0 {
            return Ok(());
        }
        let mut call = Regs::default();
        call.r[0] = piece.logical;
        let (low, high) = (
            (piece.physical & 0xFFFF_FFFF) as u32,
            (piece.physical >> 32) as u32,
        );
        if walk.flags & WIDE == 0 {
            call.r[1..4].copy_from_slice(&[low, piece.length, piece.flags]);
        } else {
            call.r[1..5].copy_from_slice(&[low, high, piece.flags, piece.length]);
        }
        call.r[9] = walk.output_state;
        call.r[12] = walk.workspace;
        self.dma_call(walk.output, &mut call)?;
        walk.output_state = call.r[9];
        Ok(())
    }

    /// Calls one of the caller's functions as RISC OS 5 does: in SVC mode,
    /// interrupts enabled. An error it gives ends the transfer.
    fn dma_call(&mut self, target: Target, regs: &mut Regs) -> Result<(), Fail> {
        self.call(target, regs, Entry::SupervisorIrq)?;
        if regs.flags & psr::V != 0 {
            return Err(Fail::Error(regs.r[0]));
        }
        Ok(())
    }
}
