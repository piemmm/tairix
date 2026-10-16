//! The classic ext2/ext3 block map: twelve direct pointers in `i_block`,
//! then a single, a double and a triple indirect block.
//!
//! Lookups report runs of physically consecutive pointers so a read is one
//! device request per run; allocation fills a hole run from one contiguous
//! data allocation; truncation frees from the end of the file in bounded
//! passes. Block I/O and allocation belong to the filesystem, reached through
//! [`MapStore`].

use tairix_abi::DriverError;

use crate::bytes::scratch;
use crate::extent::{Mapping, Truncated};

/// Direct pointers ahead of the indirect ones.
const DIRECT_SLOTS: usize = 12;
const DIRECT: u64 = DIRECT_SLOTS as u64;
/// `i_block` slots of the single, double and triple indirect blocks.
const SINGLE_SLOT: usize = 12;
const DOUBLE_SLOT: usize = 13;
const TRIPLE_SLOT: usize = 14;

/// The filesystem services the map needs.
pub(crate) trait MapStore {
    /// Pointers one indirect block holds.
    fn ptrs_per_block(&self) -> u64;
    /// Read indirect block `block`.
    fn read_ptrs(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write indirect block `block`.
    fn write_ptrs(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError>;
    /// Allocate up to `want` consecutive data blocks near `goal`, all below
    /// 2³² (a classic pointer is 32 bits), returning the first and how many.
    fn alloc_data(&mut self, goal: u64, want: u32) -> Result<(u64, u32), DriverError>;
    /// Allocate one indirect block near `goal`, below 2³².
    fn alloc_ptr_block(&mut self, goal: u64) -> Result<u64, DriverError>;
    /// Release `len` data blocks from `start`.
    fn free_data(&mut self, start: u64, len: u64);
    /// Release one indirect block.
    fn free_ptr_block(&mut self, block: u64);
}

fn get(buf: &[u8], i: usize) -> u32 {
    buf.get(i * 4..i * 4 + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn set(buf: &mut [u8], i: usize, v: u32) -> Result<(), DriverError> {
    let slot = buf
        .get_mut(i * 4..i * 4 + 4)
        .ok_or(DriverError::DeviceFault)?;
    slot.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn narrow(block: u64) -> Result<u32, DriverError> {
    u32::try_from(block).map_err(|_| DriverError::DeviceFault)
}

/// Where a logical block sits: the `i_block` slot of its tree, the tree's
/// height (0 for a direct pointer), and its index below that slot.
#[derive(Copy, Clone)]
struct Place {
    slot: usize,
    height: u32,
    index: u64,
}

/// The highest logical block the map can address, exclusive.
pub(crate) fn limit(ppb: u64) -> u64 {
    DIRECT
        .saturating_add(ppb)
        .saturating_add(ppb.saturating_mul(ppb))
        .saturating_add(ppb.saturating_mul(ppb).saturating_mul(ppb))
}

fn place(ppb: u64, logical: u64) -> Option<Place> {
    if logical < DIRECT {
        return usize::try_from(logical).ok().map(|slot| Place {
            slot,
            height: 0,
            index: 0,
        });
    }
    let mut rem = logical - DIRECT;
    let mut span = ppb;
    for (slot, height) in [(SINGLE_SLOT, 1), (DOUBLE_SLOT, 2), (TRIPLE_SLOT, 3)] {
        if rem < span {
            return Some(Place {
                slot,
                height,
                index: rem,
            });
        }
        rem -= span;
        span = span.checked_mul(ppb)?;
    }
    None
}

fn span(ppb: u64, height: u32) -> u64 {
    ppb.saturating_pow(height)
}

/// Resolve logical block `logical`.
pub(crate) fn lookup<S: MapStore>(
    iblock: &[u8],
    store: &mut S,
    logical: u64,
) -> Result<Mapping, DriverError> {
    let ppb = store.ptrs_per_block();
    let Some(at) = place(ppb, logical) else {
        return Ok(Mapping::Hole { len: None });
    };
    if at.height == 0 {
        return Ok(run_in(iblock, at.slot, DIRECT_SLOTS));
    }
    let mut ptr = get(iblock, at.slot);
    let mut index = at.index;
    let mut buf = scratch(usize::try_from(ppb * 4).map_err(|_| DriverError::DeviceFault)?)?;
    for height in (1..=at.height).rev() {
        let below = span(ppb, height - 1);
        if ptr == 0 {
            return Ok(Mapping::Hole {
                len: Some(span(ppb, height) - index),
            });
        }
        store.read_ptrs(u64::from(ptr), &mut buf)?;
        let i = usize::try_from(index / below).map_err(|_| DriverError::DeviceFault)?;
        if height == 1 {
            return Ok(run_in(&buf, i, buf.len() / 4));
        }
        ptr = get(&buf, i);
        index %= below;
    }
    Err(DriverError::DeviceFault)
}

/// The run of consecutive pointers, or of holes, starting at `i` in a leaf
/// array of `n` pointers.
fn run_in(buf: &[u8], i: usize, n: usize) -> Mapping {
    let first = get(buf, i);
    let mut len = 1usize;
    if first == 0 {
        while i + len < n && get(buf, i + len) == 0 {
            len += 1;
        }
        return Mapping::Hole {
            len: Some(len as u64),
        };
    }
    while i + len < n && u64::from(get(buf, i + len)) == u64::from(first) + len as u64 {
        len += 1;
    }
    Mapping::Mapped {
        phys: u64::from(first),
        len: len as u64,
        unwritten: false,
    }
}

/// What one allocation mapped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Allocated {
    /// The first data block.
    pub(crate) phys: u64,
    /// Logical blocks mapped from the one asked for.
    pub(crate) len: u32,
    /// Indirect blocks allocated to reach them.
    pub(crate) meta: u32,
}

/// Map up to `want` logical blocks from `logical`, a hole, to freshly
/// allocated data within the one pointer array `logical` lies in.
///
/// Each block is placed where the file most likely continues, as ext2 places
/// it: just past the nearest mapped pointer before it in the same array, else
/// just past the indirect block holding the array, else near `goal`.
pub(crate) fn alloc<S: MapStore>(
    iblock: &mut [u8],
    store: &mut S,
    logical: u64,
    want: u32,
    goal: u64,
) -> Result<Allocated, DriverError> {
    let ppb = store.ptrs_per_block();
    let at = place(ppb, logical).ok_or(DriverError::LengthOutOfRange)?;
    let mut meta = 0u32;
    let near_direct = after_mapped(iblock, at.slot).unwrap_or(goal);
    if at.height == 0 {
        let room = holes_from(iblock, at.slot, DIRECT_SLOTS);
        if room == 0 {
            return Err(DriverError::DeviceFault);
        }
        let (phys, len) = store.alloc_data(near_direct, want.min(room))?;
        fill(iblock, at.slot, phys, len)?;
        return Ok(Allocated { phys, len, meta });
    }
    let block_bytes = usize::try_from(ppb * 4).map_err(|_| DriverError::DeviceFault)?;
    let mut buf = scratch(block_bytes)?;
    // Walk down, creating each missing indirect block on the way.
    let mut ptr = get(iblock, at.slot);
    if ptr == 0 {
        ptr = narrow(store.alloc_ptr_block(near_direct)?)?;
        meta += 1;
        store.write_ptrs(u64::from(ptr), &buf)?;
        set(iblock, at.slot, ptr)?;
    }
    let mut index = at.index;
    for height in (1..=at.height).rev() {
        let below = span(ppb, height - 1);
        store.read_ptrs(u64::from(ptr), &mut buf)?;
        let i = usize::try_from(index / below).map_err(|_| DriverError::DeviceFault)?;
        let near = after_mapped(&buf, i).unwrap_or(u64::from(ptr) + 1);
        if height == 1 {
            let room = holes_from(&buf, i, buf.len() / 4);
            if room == 0 {
                return Err(DriverError::DeviceFault);
            }
            let (phys, len) = store.alloc_data(near, want.min(room))?;
            fill(&mut buf, i, phys, len)?;
            store.write_ptrs(u64::from(ptr), &buf)?;
            return Ok(Allocated { phys, len, meta });
        }
        let mut child = get(&buf, i);
        if child == 0 {
            child = narrow(store.alloc_ptr_block(near)?)?;
            meta += 1;
            set(&mut buf, i, child)?;
            store.write_ptrs(u64::from(ptr), &buf)?;
            buf.fill(0);
            store.write_ptrs(u64::from(child), &buf)?;
        }
        ptr = child;
        index %= below;
    }
    Err(DriverError::DeviceFault)
}

/// The block just past the nearest mapped pointer before slot `i` of `ptrs`.
fn after_mapped(ptrs: &[u8], i: usize) -> Option<u64> {
    (0..i)
        .rev()
        .map(|slot| get(ptrs, slot))
        .find(|&ptr| ptr != 0)
        .map(|ptr| u64::from(ptr) + 1)
}

/// Consecutive zero pointers from `i` in an array of `n`, as an allocation
/// request bound.
fn holes_from(buf: &[u8], i: usize, n: usize) -> u32 {
    let mut len = 0u32;
    while i + (len as usize) < n && get(buf, i + len as usize) == 0 {
        len += 1;
    }
    len
}

fn fill(buf: &mut [u8], i: usize, phys: u64, len: u32) -> Result<(), DriverError> {
    for k in 0..len {
        set(buf, i + k as usize, narrow(phys + u64::from(k))?)?;
    }
    Ok(())
}

/// Release every mapping at or past logical block `from`, from the end of the
/// file, stopping after `budget` released runs or indirect blocks.
pub(crate) fn truncate<S: MapStore>(
    iblock: &mut [u8],
    store: &mut S,
    from: u64,
    budget: u32,
) -> Result<Truncated, DriverError> {
    let ppb = store.ptrs_per_block();
    let mut out = Truncated::default();
    let mut left = budget.max(1);
    let trees = [
        (TRIPLE_SLOT, 3, DIRECT + ppb + ppb * ppb),
        (DOUBLE_SLOT, 2, DIRECT + ppb),
        (SINGLE_SLOT, 1, DIRECT),
    ];
    for (slot, height, start) in trees {
        let ptr = get(iblock, slot);
        if ptr == 0 {
            continue;
        }
        if left == 0 {
            return Ok(out);
        }
        match trim_tree(
            store,
            u64::from(ptr),
            height,
            start,
            from,
            &mut left,
            &mut out,
        )? {
            Trimmed::Emptied => {
                store.free_ptr_block(u64::from(ptr));
                out.nodes += 1;
                left = left.saturating_sub(1);
                set(iblock, slot, 0)?;
            }
            Trimmed::Kept => {}
            Trimmed::Stopped => return Ok(out),
        }
    }
    let first = usize::try_from(from.min(DIRECT)).map_err(|_| DriverError::DeviceFault)?;
    for slot in (first..DIRECT_SLOTS).rev() {
        let ptr = get(iblock, slot);
        if ptr == 0 {
            continue;
        }
        if left == 0 {
            return Ok(out);
        }
        store.free_data(u64::from(ptr), 1);
        out.data += 1;
        left -= 1;
        set(iblock, slot, 0)?;
    }
    out.done = true;
    Ok(out)
}

enum Trimmed {
    /// Nothing at or past the cut remains, nor anything before it: free me.
    Emptied,
    /// Nothing at or past the cut remains; something before it does.
    Kept,
    /// The budget ran out with work left.
    Stopped,
}

/// Trim the subtree of `height` rooted at indirect block `block`, whose first
/// logical block is `start`, to `from`.
fn trim_tree<S: MapStore>(
    store: &mut S,
    block: u64,
    height: u32,
    start: u64,
    from: u64,
    left: &mut u32,
    out: &mut Truncated,
) -> Result<Trimmed, DriverError> {
    let ppb = store.ptrs_per_block();
    let below = span(ppb, height - 1);
    if start + below * ppb <= from {
        return Ok(Trimmed::Kept);
    }
    let mut buf = scratch(usize::try_from(ppb * 4).map_err(|_| DriverError::DeviceFault)?)?;
    store.read_ptrs(block, &mut buf)?;
    let n = buf.len() / 4;
    let mut changed = false;
    let mut stopped = false;
    let mut i = n;
    while i > 0 {
        i -= 1;
        let child_start = start + i as u64 * below;
        if child_start + below <= from {
            break;
        }
        let ptr = get(&buf, i);
        if ptr == 0 {
            continue;
        }
        if *left == 0 {
            stopped = true;
            break;
        }
        if height == 1 {
            // Coalesce a run of consecutive data pointers into one release.
            let mut j = i;
            while j > 0
                && start + (j as u64 - 1) * below >= from
                && get(&buf, j - 1) != 0
                && u64::from(get(&buf, j - 1)) + 1 == u64::from(get(&buf, j))
            {
                j -= 1;
            }
            let len = (i - j + 1) as u64;
            store.free_data(u64::from(get(&buf, j)), len);
            out.data += len;
            *left -= 1;
            for k in j..=i {
                set(&mut buf, k, 0)?;
            }
            changed = true;
            i = j;
            continue;
        }
        match trim_tree(
            store,
            u64::from(ptr),
            height - 1,
            child_start,
            from,
            left,
            out,
        )? {
            Trimmed::Emptied => {
                store.free_ptr_block(u64::from(ptr));
                out.nodes += 1;
                *left = left.saturating_sub(1);
                set(&mut buf, i, 0)?;
                changed = true;
            }
            Trimmed::Kept => {}
            Trimmed::Stopped => {
                stopped = true;
                break;
            }
        }
    }
    if !stopped && (0..n).all(|k| get(&buf, k) == 0) {
        return Ok(Trimmed::Emptied);
    }
    if changed {
        store.write_ptrs(block, &buf)?;
    }
    Ok(if stopped {
        Trimmed::Stopped
    } else {
        Trimmed::Kept
    })
}

#[cfg(test)]
#[path = "blockmap_tests.rs"]
mod tests;
