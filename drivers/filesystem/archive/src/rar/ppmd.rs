//! PPMd variant H as RAR 3 uses it: the context model on its own heap, and
//! RAR's range decoder.
//!
//! The model lays its twelve-byte units out on a heap exactly as its
//! encoder's does, because where it runs out of memory and restarts, and
//! whether a successor points into the text area, are part of the format:
//! encoder and decoder agree on them or not at all. The heap is asked of the
//! allocator zeroed, and the model writes only what its symbols build, so
//! where pages are backed on first touch a stream declaring the format's
//! largest model holds only that. A context is twelve
//! bytes (symbol count, frequency sum, states, suffix), a state six (symbol,
//! frequency, successor); references are byte offsets, zero none.
//!
//! The model's state follows from the symbols decoded, so its offsets stay
//! within its heap by construction; every access is still checked, and one
//! out of bounds faults the stream rather than the process. Nor does the
//! model read memory it has not written, so a stream declaring model after
//! model has each laid out on the last one's heap, never another zeroed.

use core::cell::Cell;

use alloc::vec::Vec;

use tairix_util::fallible::zeroed;

use tairix_abi::DriverError;

use super::bits::Reader;

const UNIT: u32 = 12;
const MAX_FREQ: u32 = 124;
const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);
const NUM_INDEXES: usize = 38;
/// The most orders a model has.
pub(super) const MAX_ORDER: u32 = 64;
const INIT_BIN_ESC: [u32; 8] = [
    0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051,
];
const EXP_ESCAPE: [u32; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const TOP: u32 = 1 << 24;
const BOTTOM: u32 = 1 << 15;

/// The units each allocator index serves.
const INDEX_UNITS: [u32; NUM_INDEXES] = {
    let mut table = [0; NUM_INDEXES];
    let mut units = 0;
    let mut i = 0u32;
    while (i as usize) < NUM_INDEXES {
        units += if i >= 12 { 4 } else { (i >> 2) + 1 };
        table[i as usize] = units;
        i += 1;
    }
    table
};

/// The least index serving a count of units, by count less one.
const UNITS_INDEX: [usize; 128] = {
    let mut table = [0; 128];
    let mut i = 0;
    let mut k = 0u32;
    while k < 128 {
        if INDEX_UNITS[i] < k + 1 {
            i += 1;
        }
        table[k as usize] = i;
        k += 1;
    }
    table
};

/// A binary context's probability column, by its suffix's symbol count
/// less one.
const NS_TO_BS: [u32; 256] = {
    let mut table = [6; 256];
    table[0] = 0;
    table[1] = 2;
    let mut k = 2;
    while k < 11 {
        table[k] = 4;
        k += 1;
    }
    table
};

/// An escape estimator's row, by unmasked symbols less one.
const NS_TO_INDEX: [usize; 256] = {
    let mut table = [0; 256];
    table[1] = 1;
    table[2] = 2;
    let mut m = 3;
    let mut k = 1;
    let mut i = 3;
    while i < 256 {
        table[i] = m;
        k -= 1;
        if k == 0 {
            m += 1;
            k = m - 2;
        }
        i += 1;
    }
    table
};

/// The flag a symbol's high bits set in an estimator's column.
const fn high_bits(symbol: u8) -> u32 {
    if symbol < 0x40 {
        0
    } else {
        8
    }
}

/// RAR's range decoder: Subbotin's carry-less coder.
#[derive(Default)]
pub(super) struct Coder {
    low: u32,
    range: u32,
    code: u32,
}

impl Coder {
    /// A coder reading its first four bytes from `br`.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data ending before them.
    pub(super) fn start(br: &mut Reader<'_>) -> Result<Self, DriverError> {
        let mut code = 0u32;
        for _ in 0..4 {
            code = code << 8 | u32::from(br.coder_byte()?);
        }
        Ok(Self {
            low: 0,
            range: u32::MAX,
            code,
        })
    }

    fn threshold(&mut self, total: u32) -> Result<u32, DriverError> {
        if total == 0 {
            return Err(DriverError::MediumError);
        }
        self.range /= total;
        if self.range == 0 {
            return Err(DriverError::MediumError);
        }
        Ok(self.code.wrapping_sub(self.low) / self.range)
    }

    fn decode(&mut self, br: &mut Reader<'_>, start: u32, size: u32) -> Result<(), DriverError> {
        self.low = self.low.wrapping_add(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
        loop {
            if self.low ^ self.low.wrapping_add(self.range) >= TOP {
                if self.range >= BOTTOM {
                    return Ok(());
                }
                self.range = self.low.wrapping_neg() & (BOTTOM - 1);
            }
            self.code = self.code << 8 | u32::from(br.coder_byte()?);
            self.range <<= 8;
            self.low <<= 8;
        }
    }

    fn bit(&mut self, br: &mut Reader<'_>, size0: u32) -> Result<bool, DriverError> {
        if self.threshold(BIN_SCALE)? < size0 {
            self.decode(br, 0, size0)?;
            Ok(false)
        } else {
            self.decode(br, size0, BIN_SCALE - size0)?;
            Ok(true)
        }
    }
}

/// An escape estimator.
#[derive(Copy, Clone, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

/// Which estimator an escape was coded with.
#[derive(Copy, Clone)]
enum Estimator {
    Table(usize, usize),
    Dummy,
}

/// The most states a context holds: one a symbol.
const MAX_STATES: u32 = 256;

/// The model's heap: at least the span its size states, every access
/// checked against it.
struct Memory {
    bytes: Vec<u8>,
    fault: Cell<bool>,
}

impl Memory {
    /// A heap of at least `span` bytes: `heap` where it is that long — a
    /// model before this one's, whose bytes this model never reads before it
    /// writes them, kept whole so a stream changing its model's size never
    /// has one zeroed again — else a new one asked of the allocator zeroed,
    /// `heap` let go first, so the pages the model's symbols never reach are
    /// never written.
    fn new(heap: Vec<u8>, span: usize) -> Result<Self, DriverError> {
        let bytes = if heap.len() >= span {
            heap
        } else {
            drop(heap);
            zeroed(span).ok_or(DriverError::OutOfMemory)?
        };
        Ok(Self {
            bytes,
            fault: Cell::new(false),
        })
    }

    /// The `len` bytes from `at`, where the heap holds them.
    fn run(&self, at: u32, len: usize) -> Option<&[u8]> {
        let at = usize::try_from(at).ok()?;
        self.bytes.get(at..at.checked_add(len)?)
    }

    fn run_mut(&mut self, at: u32, len: usize) -> Option<&mut [u8]> {
        let at = usize::try_from(at).ok()?;
        self.bytes.get_mut(at..at.checked_add(len)?)
    }

    #[inline]
    fn read<const N: usize>(&self, at: u32) -> [u8; N] {
        if let Some(Ok(bytes)) = self.run(at, N).map(<[u8; N]>::try_from) {
            return bytes;
        }
        self.fault.set(true);
        [0; N]
    }

    #[inline]
    fn write<const N: usize>(&mut self, at: u32, bytes: [u8; N]) {
        match self.run_mut(at, N) {
            Some(run) => run.copy_from_slice(&bytes),
            None => self.fault.set(true),
        }
    }

    fn get8(&self, at: u32) -> u8 {
        self.read::<1>(at)[0]
    }

    fn get16(&self, at: u32) -> u32 {
        u16::from_le_bytes(self.read(at)).into()
    }

    fn get32(&self, at: u32) -> u32 {
        u32::from_le_bytes(self.read(at))
    }

    fn put8(&mut self, at: u32, value: u32) {
        self.write(at, [value.to_le_bytes()[0]]);
    }

    fn put16(&mut self, at: u32, value: u32) {
        let [low, high, ..] = value.to_le_bytes();
        self.write(at, [low, high]);
    }

    fn put32(&mut self, at: u32, value: u32) {
        self.write(at, value.to_le_bytes());
    }

    /// The `count` six-byte states from `at`; `None` past the heap or for
    /// more than a context holds.
    fn states(&self, at: u32, count: u32) -> Option<&[u8]> {
        if count > MAX_STATES {
            return None;
        }
        self.run(at, usize::try_from(count).ok()? * 6)
    }

    /// Copy `len` bytes from `from` to `to`; the two never overlap here.
    fn copy(&mut self, to: u32, from: u32, len: u32) {
        let (Ok(to), Ok(from), Ok(len)) = (
            usize::try_from(to),
            usize::try_from(from),
            usize::try_from(len),
        ) else {
            self.fault.set(true);
            return;
        };
        let fits = |at: usize| {
            at.checked_add(len)
                .is_some_and(|end| end <= self.bytes.len())
        };
        if fits(from) && fits(to) {
            self.bytes.copy_within(from..from + len, to);
        } else {
            self.fault.set(true);
        }
    }

    fn state(&self, at: u32) -> [u8; 6] {
        self.read(at)
    }

    fn set_state(&mut self, at: u32, state: [u8; 6]) {
        self.write(at, state);
    }

    fn swap_states(&mut self, a: u32, b: u32) {
        let (first, second) = (self.state(a), self.state(b));
        self.set_state(a, second);
        self.set_state(b, first);
    }
}

/// A PPMd variant H model.
pub(super) struct Model {
    mem: Memory,
    size: u32,
    align: u32,
    max_order: u32,
    free: [u32; NUM_INDEXES],
    text: u32,
    hi_unit: u32,
    lo_unit: u32,
    units_start: u32,
    glue_count: u32,
    order_fall: u32,
    run_length: i32,
    init_rl: i32,
    prev_success: u32,
    min_ctx: u32,
    max_ctx: u32,
    found: u32,
    bin_summ: [[u16; 64]; 128],
    see: [[See; 16]; 25],
    dummy: See,
    init_esc: u32,
    high_bits: u32,
    /// Which symbols an escape has masked: those whose stamp is current.
    masked: [u8; 256],
    stamp: u8,
    /// An escape's unmasked states: their indices, symbols and frequencies.
    candidates: [(u8, u8, u8); 256],
}

impl Model {
    /// A model of `size` bytes and `max_order` orders, laid out on `heap`,
    /// a model before it's or none.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a size or order out of range;
    /// [`DriverError::OutOfMemory`] where its heap cannot be had.
    pub(super) fn new(size: u32, max_order: u32, heap: Vec<u8>) -> Result<Self, DriverError> {
        if !(UNIT..=u32::MAX - 36).contains(&size) || !(2..=MAX_ORDER).contains(&max_order) {
            return Err(DriverError::MediumError);
        }
        let align = 4 - (size & 3);
        let span = usize::try_from(align + size + UNIT).map_err(|_| DriverError::OutOfMemory)?;
        let mut model = Self {
            mem: Memory::new(heap, span)?,
            size,
            align,
            max_order,
            free: [0; NUM_INDEXES],
            text: 0,
            hi_unit: 0,
            lo_unit: 0,
            units_start: 0,
            glue_count: 0,
            order_fall: 0,
            run_length: 0,
            init_rl: 0,
            prev_success: 0,
            min_ctx: 0,
            max_ctx: 0,
            found: 0,
            bin_summ: [[0; 64]; 128],
            see: [[See::default(); 16]; 25],
            dummy: See {
                summ: 0,
                shift: 7,
                count: 64,
            },
            init_esc: 0,
            high_bits: 0,
            masked: [0; 256],
            stamp: 0,
            candidates: [(0, 0, 0); 256],
        };
        model.restart();
        Ok(model)
    }

    /// The heap, for the next model to be laid out on.
    pub(super) fn into_heap(self) -> Vec<u8> {
        self.mem.bytes
    }

    fn restart(&mut self) {
        self.free = [0; NUM_INDEXES];
        self.text = self.align;
        self.hi_unit = self.text + self.size;
        self.units_start = self.hi_unit - self.size / 8 / UNIT * 7 * UNIT;
        self.lo_unit = self.units_start;
        self.glue_count = 0;
        self.order_fall = self.max_order;
        self.init_rl = -i32::try_from(self.max_order.min(12)).unwrap_or(12) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;
        self.hi_unit -= UNIT;
        let ctx = self.hi_unit;
        self.min_ctx = ctx;
        self.max_ctx = ctx;
        self.mem.put32(ctx + 8, 0);
        self.mem.put16(ctx, 256);
        self.mem.put16(ctx + 2, 257);
        let states = self.lo_unit;
        self.found = states;
        self.lo_unit += 128 * UNIT;
        self.mem.put32(ctx + 4, states);
        for symbol in 0..256u32 {
            let state = states + 6 * symbol;
            self.mem.put8(state, symbol);
            self.mem.put8(state + 1, 1);
            self.mem.put32(state + 2, 0);
        }
        for (i, row) in (0u32..).zip(self.bin_summ.iter_mut()) {
            for (k, &esc) in INIT_BIN_ESC.iter().enumerate() {
                let value = u16::try_from(BIN_SCALE - esc / (i + 2)).unwrap_or(0);
                for column in (k..64).step_by(8) {
                    row[column] = value;
                }
            }
        }
        for (i, row) in (0u16..).zip(self.see.iter_mut()) {
            for see in row.iter_mut() {
                see.shift = 3;
                see.summ = (5 * i + 10) << 3;
                see.count = 4;
            }
        }
    }

    fn insert_node(&mut self, node: u32, index: usize) {
        self.mem.put32(node, self.free[index]);
        self.free[index] = node;
    }

    fn remove_node(&mut self, index: usize) -> u32 {
        let node = self.free[index];
        self.free[index] = self.mem.get32(node);
        node
    }

    fn split_block(&mut self, block: u32, old_index: usize, new_index: usize) {
        let units = INDEX_UNITS[old_index] - INDEX_UNITS[new_index];
        let block = block + INDEX_UNITS[new_index] * UNIT;
        let mut index = units_index(units);
        if INDEX_UNITS[index] != units {
            index -= 1;
            let k = INDEX_UNITS[index];
            self.insert_node(block + k * UNIT, slot(units - k - 1));
        }
        self.insert_node(block, index);
    }

    fn glue_free_blocks(&mut self) {
        let head = self.align + self.size;
        let mut n = head;
        self.glue_count = 255;
        for (index, &units) in INDEX_UNITS.iter().enumerate() {
            let mut next = self.free[index];
            self.free[index] = 0;
            while next != 0 {
                let node = next;
                self.mem.put32(node + 4, n);
                self.mem.put32(n + 8, node);
                n = node;
                next = self.mem.get32(node);
                self.mem.put16(node, 0);
                self.mem.put16(node + 2, units);
                if self.mem.fault.get() {
                    return;
                }
            }
        }
        self.mem.put16(head, 1);
        self.mem.put32(head + 4, n);
        self.mem.put32(n + 8, head);
        if self.lo_unit != self.hi_unit {
            self.mem.put16(self.lo_unit, 1);
        }
        while n != head && !self.mem.fault.get() {
            let node = n;
            let mut units = self.mem.get16(node + 2);
            loop {
                let next = node + units * UNIT;
                units += self.mem.get16(next + 2);
                if self.mem.get16(next) != 0 || units >= 0x1_0000 || self.mem.fault.get() {
                    break;
                }
                let prev = self.mem.get32(next + 8);
                let after = self.mem.get32(next + 4);
                self.mem.put32(prev + 4, after);
                self.mem.put32(after + 8, prev);
                self.mem.put16(node + 2, units);
            }
            n = self.mem.get32(node + 4);
        }
        n = self.mem.get32(head + 4);
        while n != head && !self.mem.fault.get() {
            let mut node = n;
            let next = self.mem.get32(node + 4);
            let mut units = self.mem.get16(node + 2);
            while units > 128 {
                self.insert_node(node, NUM_INDEXES - 1);
                units -= 128;
                node += 128 * UNIT;
            }
            if units == 0 {
                self.mem.fault.set(true);
                return;
            }
            let mut index = units_index(units);
            if INDEX_UNITS[index] != units {
                index -= 1;
                let k = INDEX_UNITS[index];
                self.insert_node(node + k * UNIT, slot(units - k - 1));
            }
            self.insert_node(node, index);
            n = next;
        }
    }

    fn alloc_units_rare(&mut self, index: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free[index] != 0 {
                return self.remove_node(index);
            }
        }
        let mut i = index;
        loop {
            i += 1;
            if i == NUM_INDEXES {
                let bytes = INDEX_UNITS[index] * UNIT;
                self.glue_count = self.glue_count.wrapping_sub(1);
                if self.units_start.saturating_sub(self.text) > bytes {
                    self.units_start -= bytes;
                    return self.units_start;
                }
                return 0;
            }
            if self.free[i] != 0 {
                break;
            }
        }
        let block = self.remove_node(i);
        self.split_block(block, i, index);
        block
    }

    fn alloc_units(&mut self, index: usize) -> u32 {
        if self.free[index] != 0 {
            return self.remove_node(index);
        }
        let bytes = INDEX_UNITS[index] * UNIT;
        if bytes <= self.hi_unit.saturating_sub(self.lo_unit) {
            let block = self.lo_unit;
            self.lo_unit += bytes;
            return block;
        }
        self.alloc_units_rare(index)
    }

    fn shrink_units(&mut self, old: u32, old_units: u32, new_units: u32) -> u32 {
        let (old_index, new_index) = (units_index(old_units), units_index(new_units));
        if old_index == new_index {
            return old;
        }
        if self.free[new_index] != 0 {
            let block = self.remove_node(new_index);
            self.mem.copy(block, old, new_units * UNIT);
            self.insert_node(old, old_index);
            return block;
        }
        self.split_block(old, old_index, new_index);
        old
    }

    /// The state of `ctx` holding `symbol`, scanning no further than its
    /// symbol count.
    fn find(&self, ctx: u32, symbol: u8) -> u32 {
        let mut state = self.mem.get32(ctx + 4);
        for _ in 0..self.mem.get16(ctx) {
            if self.mem.get8(state) == symbol {
                return state;
            }
            state += 6;
        }
        self.mem.fault.set(true);
        state
    }

    fn create_successors(&mut self, skip: bool) -> u32 {
        let mut ctx = self.min_ctx;
        let up_branch = self.mem.get32(self.found + 2);
        let symbol = self.mem.get8(self.found);
        let mut pending = [0u32; MAX_ORDER as usize + 1];
        let mut count = 0;
        if !skip {
            pending[0] = self.found;
            count = 1;
        }
        while self.mem.get32(ctx + 8) != 0 && !self.mem.fault.get() {
            ctx = self.mem.get32(ctx + 8);
            let state = if self.mem.get16(ctx) == 1 {
                ctx + 2
            } else {
                self.find(ctx, symbol)
            };
            let successor = self.mem.get32(state + 2);
            if successor != up_branch {
                ctx = successor;
                if count == 0 {
                    return ctx;
                }
                break;
            }
            let Some(slot) = pending.get_mut(count) else {
                self.mem.fault.set(true);
                return 0;
            };
            *slot = state;
            count += 1;
        }
        let up_symbol = self.mem.get8(up_branch);
        let up_successor = up_branch + 1;
        let up_freq = if self.mem.get16(ctx) == 1 {
            self.mem.get8(ctx + 3).into()
        } else {
            let state = self.find(ctx, up_symbol);
            let cf = u32::from(self.mem.get8(state + 1)).wrapping_sub(1);
            let s0 = self
                .mem
                .get16(ctx + 2)
                .wrapping_sub(self.mem.get16(ctx))
                .wrapping_sub(cf);
            if cf.wrapping_mul(2) <= s0 {
                1 + u32::from(cf.wrapping_mul(5) > s0)
            } else if s0 == 0 {
                self.mem.fault.set(true);
                1
            } else {
                1 + (cf
                    .wrapping_mul(2)
                    .wrapping_add(s0.wrapping_mul(3))
                    .wrapping_sub(1))
                    / s0.wrapping_mul(2)
            }
        };
        while count > 0 && !self.mem.fault.get() {
            let child = if self.hi_unit != self.lo_unit {
                self.hi_unit = self.hi_unit.wrapping_sub(UNIT);
                self.hi_unit
            } else if self.free[0] != 0 {
                self.remove_node(0)
            } else {
                let block = self.alloc_units_rare(0);
                if block == 0 {
                    return 0;
                }
                block
            };
            self.mem.put16(child, 1);
            self.mem.put8(child + 2, up_symbol.into());
            self.mem.put8(child + 3, up_freq);
            self.mem.put32(child + 4, up_successor);
            self.mem.put32(child + 8, ctx);
            count -= 1;
            self.mem.put32(pending[count] + 2, child);
            ctx = child;
        }
        ctx
    }

    /// Count `symbol`, found in the current context, in its suffix too.
    fn raise_in_suffix(&mut self, symbol: u8) {
        let ctx = self.mem.get32(self.min_ctx + 8);
        if ctx == 0 {
            return;
        }
        if self.mem.get16(ctx) == 1 {
            let state = ctx + 2;
            let f = u32::from(self.mem.get8(state + 1));
            if f < 32 {
                self.mem.put8(state + 1, f + 1);
            }
            return;
        }
        let mut state = self.mem.get32(ctx + 4);
        if self.mem.get8(state) != symbol {
            state = self.find(ctx, symbol);
            let before = state.wrapping_sub(6);
            if self.mem.get8(state + 1) >= self.mem.get8(before + 1) {
                self.mem.swap_states(state, before);
                state = before;
            }
        }
        let f = u32::from(self.mem.get8(state + 1));
        if f < MAX_FREQ - 9 {
            self.mem.put8(state + 1, f + 2);
            self.mem.put16(ctx + 2, self.mem.get16(ctx + 2) + 2);
        }
    }

    /// Make room in `ctx`, of `ns1` states, for one more: a binary context
    /// given a states array, a full array moved to a larger block. `false`
    /// where the heap has no room, the model to be restarted.
    fn grow_states(&mut self, ctx: u32, ns1: u32, ns: u32) -> bool {
        if ns1 == 1 {
            let state = self.alloc_units(0);
            if state == 0 {
                return false;
            }
            let one = self.mem.state(ctx + 2);
            self.mem.set_state(state, one);
            self.mem.put32(ctx + 4, state);
            let f = u32::from(self.mem.get8(state + 1));
            let f = if f < MAX_FREQ / 4 - 1 {
                f << 1
            } else {
                MAX_FREQ - 4
            };
            self.mem.put8(state + 1, f);
            self.mem
                .put16(ctx + 2, f + self.init_esc + u32::from(ns > 3));
            return true;
        }
        if ns1 & 1 == 0 {
            let old_units = ns1 >> 1;
            let index = units_index(old_units);
            if index != UNITS_INDEX.get(slot(old_units)).copied().unwrap_or(index) {
                let block = self.alloc_units(index + 1);
                if block == 0 {
                    return false;
                }
                let old = self.mem.get32(ctx + 4);
                self.mem.copy(block, old, old_units * UNIT);
                self.insert_node(old, index);
                self.mem.put32(ctx + 4, block);
            }
        }
        let mut sf = self.mem.get16(ctx + 2);
        sf += u32::from(2 * ns1 < ns) + 2 * u32::from(4 * ns1 <= ns && sf <= 8 * ns1);
        self.mem.put16(ctx + 2, sf);
        true
    }

    fn update_model(&mut self) {
        let found = self.found;
        let symbol = self.mem.get8(found);
        let freq = u32::from(self.mem.get8(found + 1));
        let mut successor_of_found = self.mem.get32(found + 2);
        if freq < MAX_FREQ / 4 {
            self.raise_in_suffix(symbol);
        }
        if self.order_fall == 0 {
            self.min_ctx = self.create_successors(true);
            self.max_ctx = self.min_ctx;
            if self.min_ctx == 0 {
                self.restart();
                return;
            }
            self.mem.put32(self.found + 2, self.min_ctx);
            return;
        }
        self.mem.put8(self.text, symbol.into());
        self.text += 1;
        let mut successor = self.text;
        if self.text >= self.units_start {
            self.restart();
            return;
        }
        if successor_of_found != 0 {
            if successor_of_found <= successor {
                let made = self.create_successors(false);
                if made == 0 {
                    self.restart();
                    return;
                }
                successor_of_found = made;
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = successor_of_found;
                if self.max_ctx != self.min_ctx {
                    self.text = self.text.wrapping_sub(1);
                }
            }
        } else {
            self.mem.put32(found + 2, successor);
            successor_of_found = self.min_ctx;
        }
        let ns = self.mem.get16(self.min_ctx);
        let s0 = self
            .mem
            .get16(self.min_ctx + 2)
            .wrapping_sub(ns)
            .wrapping_sub(u32::from(self.mem.get8(self.found + 1)).wrapping_sub(1));
        let mut ctx = self.max_ctx;
        while ctx != self.min_ctx && !self.mem.fault.get() {
            let ns1 = self.mem.get16(ctx);
            if !self.grow_states(ctx, ns1, ns) {
                self.restart();
                return;
            }
            let sum = self.mem.get16(ctx + 2);
            let mut cf = 2 * u32::from(self.mem.get8(self.found + 1)) * (sum + 6);
            let sf = s0.wrapping_add(sum);
            if cf < sf.wrapping_mul(6) {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= sf.wrapping_mul(4));
                self.mem.put16(ctx + 2, sum + 3);
            } else {
                cf = 4
                    + u32::from(cf >= sf.wrapping_mul(9))
                    + u32::from(cf >= sf.wrapping_mul(12))
                    + u32::from(cf >= sf.wrapping_mul(15));
                self.mem.put16(ctx + 2, sum + cf);
            }
            let state = self.mem.get32(ctx + 4) + ns1 * 6;
            self.mem.put32(state + 2, successor);
            self.mem.put8(state, symbol.into());
            self.mem.put8(state + 1, cf);
            self.mem.put16(ctx, ns1 + 1);
            ctx = self.mem.get32(ctx + 8);
        }
        self.max_ctx = successor_of_found;
        self.min_ctx = successor_of_found;
    }

    fn rescale(&mut self) {
        let ctx = self.min_ctx;
        let head = self.mem.get32(ctx + 4);
        let mut state = self.found;
        let first = self.mem.state(state);
        while state != head && !self.mem.fault.get() {
            let before = self.mem.state(state.wrapping_sub(6));
            self.mem.set_state(state, before);
            state = state.wrapping_sub(6);
        }
        self.mem.set_state(state, first);
        let mut esc = i64::from(self.mem.get16(ctx + 2)) - i64::from(self.mem.get8(state + 1));
        let adder = u32::from(self.order_fall != 0);
        let f = (u32::from(self.mem.get8(state + 1)) + 4) & 0xFF;
        self.mem.put8(state + 1, f.midpoint(adder));
        let mut sum = u32::from(self.mem.get8(state + 1));
        let mut i = self.mem.get16(ctx).wrapping_sub(1);
        while i > 0 && !self.mem.fault.get() {
            state += 6;
            let f = u32::from(self.mem.get8(state + 1));
            esc -= i64::from(f);
            self.mem.put8(state + 1, f.midpoint(adder));
            sum += u32::from(self.mem.get8(state + 1));
            if self.mem.get8(state + 1) > self.mem.get8(state.wrapping_sub(6) + 1) {
                let moving = self.mem.state(state);
                let mut at = state;
                loop {
                    let before = self.mem.state(at.wrapping_sub(6));
                    self.mem.set_state(at, before);
                    at = at.wrapping_sub(6);
                    if at == head
                        || moving[1] <= self.mem.get8(at.wrapping_sub(6) + 1)
                        || self.mem.fault.get()
                    {
                        break;
                    }
                }
                self.mem.set_state(at, moving);
            }
            i -= 1;
        }
        if self.mem.get8(state + 1) == 0 {
            let num_stats = self.mem.get16(ctx);
            let mut zeros = 0;
            loop {
                zeros += 1;
                state = state.wrapping_sub(6);
                if self.mem.get8(state + 1) != 0 || zeros >= num_stats || self.mem.fault.get() {
                    break;
                }
            }
            esc += i64::from(zeros);
            self.mem.put16(ctx, num_stats - zeros);
            if self.mem.get16(ctx) == 1 {
                let mut one = self.mem.state(head);
                loop {
                    one[1] -= one[1] >> 1;
                    esc >>= 1;
                    if esc <= 1 {
                        break;
                    }
                }
                self.insert_node(head, units_index((num_stats + 1) >> 1));
                self.found = ctx + 2;
                self.mem.set_state(ctx + 2, one);
                return;
            }
            let (n0, n1) = ((num_stats + 1) >> 1, (self.mem.get16(ctx) + 1) >> 1);
            if n0 != n1 {
                let moved = self.shrink_units(head, n0, n1);
                self.mem.put32(ctx + 4, moved);
            }
        }
        let total = (i64::from(sum) + esc - (esc >> 1)).rem_euclid(0x1_0000);
        self.mem.put16(ctx + 2, u32::try_from(total).unwrap_or(0));
        self.found = self.mem.get32(ctx + 4);
    }

    fn next_context(&mut self) {
        let successor = self.mem.get32(self.found + 2);
        if self.order_fall == 0 && successor > self.text {
            self.min_ctx = successor;
            self.max_ctx = successor;
        } else {
            self.update_model();
        }
    }

    fn new_mask(&mut self) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.masked = [0; 256];
            self.stamp = 1;
        }
    }

    fn mask(&mut self, symbol: u8) {
        self.masked[usize::from(symbol)] = self.stamp;
    }

    fn escape_estimator(&mut self, num_masked: u32) -> (Estimator, u32) {
        let ctx = self.min_ctx;
        let ns = self.mem.get16(ctx);
        if ns == 256 {
            return (Estimator::Dummy, 1);
        }
        let unmasked = ns.wrapping_sub(num_masked);
        let suffix_ns = self.mem.get16(self.mem.get32(ctx + 8));
        let column = slot(
            u32::from(unmasked < suffix_ns.wrapping_sub(ns))
                + 2 * u32::from(self.mem.get16(ctx + 2) < 11 * ns)
                + 4 * u32::from(num_masked > unmasked)
                + self.high_bits,
        );
        let row = NS_TO_INDEX
            .get(slot(unmasked.wrapping_sub(1)))
            .copied()
            .unwrap_or(0);
        let Some(see) = self.see.get_mut(row).and_then(|row| row.get_mut(column)) else {
            self.mem.fault.set(true);
            return (Estimator::Dummy, 1);
        };
        let r = u32::from(see.summ >> see.shift);
        see.summ = see.summ.wrapping_sub(u16::try_from(r).unwrap_or(0));
        (Estimator::Table(row, column), r + u32::from(r == 0))
    }

    fn estimator(&mut self, which: Estimator) -> &mut See {
        match which {
            Estimator::Table(row, column) => &mut self.see[row][column],
            Estimator::Dummy => &mut self.dummy,
        }
    }

    /// Decode one symbol.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data the model cannot have coded.
    pub(super) fn decode(
        &mut self,
        coder: &mut Coder,
        br: &mut Reader<'_>,
    ) -> Result<u8, DriverError> {
        let symbol = self.decode_symbol(coder, br)?;
        if self.mem.fault.get() {
            return Err(DriverError::MediumError);
        }
        Ok(symbol)
    }

    fn decode_symbol(&mut self, coder: &mut Coder, br: &mut Reader<'_>) -> Result<u8, DriverError> {
        let first = if self.mem.get16(self.min_ctx) == 1 {
            self.decode_binary(coder, br)?
        } else {
            self.decode_first(coder, br)?
        };
        if let Some(symbol) = first {
            return Ok(symbol);
        }
        loop {
            if let Some(symbol) = self.decode_masked(coder, br)? {
                return Ok(symbol);
            }
        }
    }

    /// Decode in the current context of many states: its symbol, or `None`
    /// for an escape, its states masked.
    fn decode_first(
        &mut self,
        coder: &mut Coder,
        br: &mut Reader<'_>,
    ) -> Result<Option<u8>, DriverError> {
        let ctx = self.min_ctx;
        let ns = self.mem.get16(ctx);
        let mut state = self.mem.get32(ctx + 4);
        let summ = self.mem.get16(ctx + 2);
        let count = coder.threshold(summ)?;
        let mut high = u32::from(self.mem.get8(state + 1));
        if count < high {
            coder.decode(br, 0, high)?;
            self.found = state;
            let symbol = self.mem.get8(state);
            self.prev_success = u32::from(2 * high > summ);
            self.run_length = self
                .run_length
                .wrapping_add(i32::try_from(self.prev_success).unwrap_or(0));
            self.mem.put16(ctx + 2, summ + 4);
            self.mem.put8(state + 1, high + 4);
            if high + 4 > MAX_FREQ {
                self.rescale();
            }
            self.next_context();
            return Ok(Some(symbol));
        }
        self.prev_success = 0;
        let Some(view) = self.mem.states(state, ns) else {
            return Err(DriverError::MediumError);
        };
        let mut hit = None;
        for (k, record) in view.as_chunks::<6>().0.iter().enumerate().skip(1) {
            high += u32::from(record[1]);
            if high > count {
                hit = Some((k, u32::from(record[1])));
                break;
            }
        }
        if let Some((k, f)) = hit {
            state += 6 * u32::try_from(k).map_err(|_| DriverError::MediumError)?;
            coder.decode(br, high - f, f)?;
            self.found = state;
            let symbol = self.mem.get8(state);
            self.mem.put8(state + 1, f + 4);
            self.mem.put16(ctx + 2, self.mem.get16(ctx + 2) + 4);
            let before = state.wrapping_sub(6);
            if f + 4 > u32::from(self.mem.get8(before + 1)) {
                self.mem.swap_states(state, before);
                state = before;
                self.found = state;
                if u32::from(self.mem.get8(state + 1)) > MAX_FREQ {
                    self.rescale();
                }
            }
            self.next_context();
            return Ok(Some(symbol));
        }
        if count >= summ {
            return Err(DriverError::MediumError);
        }
        self.high_bits = high_bits(self.mem.get8(self.found));
        coder.decode(br, high, summ - high)?;
        self.new_mask();
        let states = self.mem.get32(ctx + 4);
        let Some(view) = self.mem.states(states, ns) else {
            return Err(DriverError::MediumError);
        };
        for record in view.as_chunks::<6>().0 {
            self.masked[usize::from(record[0])] = self.stamp;
        }
        Ok(None)
    }

    /// Decode in the current context of one state: its symbol, or `None`
    /// for an escape, the symbol masked.
    fn decode_binary(
        &mut self,
        coder: &mut Coder,
        br: &mut Reader<'_>,
    ) -> Result<Option<u8>, DriverError> {
        let ctx = self.min_ctx;
        let one = ctx + 2;
        let suffix = self.mem.get32(ctx + 8);
        let freq = self.mem.get8(one + 1);
        let row = usize::from(freq).wrapping_sub(1);
        self.high_bits = high_bits(self.mem.get8(self.found));
        let column = slot(
            self.prev_success
                + NS_TO_BS
                    .get(slot(self.mem.get16(suffix).wrapping_sub(1)))
                    .copied()
                    .unwrap_or(6)
                + self.high_bits
                + 2 * high_bits(self.mem.get8(one))
                + u32::try_from((self.run_length >> 26) & 0x20).unwrap_or(0),
        );
        let Some(prob) = self
            .bin_summ
            .get(row)
            .and_then(|row| row.get(column))
            .copied()
        else {
            return Err(DriverError::MediumError);
        };
        let prob32 = u32::from(prob);
        if !coder.bit(br, prob32)? {
            let raised =
                prob32 + (1 << INT_BITS) - ((prob32 + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
            self.bin_summ[row][column] = u16::try_from(raised & 0xFFFF).unwrap_or(0);
            self.found = one;
            let symbol = self.mem.get8(one);
            self.mem
                .put8(one + 1, u32::from(freq) + u32::from(freq < 128));
            self.prev_success = 1;
            self.run_length = self.run_length.wrapping_add(1);
            self.next_context();
            return Ok(Some(symbol));
        }
        let lowered = prob32 - ((prob32 + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
        self.bin_summ[row][column] = u16::try_from(lowered & 0xFFFF).unwrap_or(0);
        self.init_esc = EXP_ESCAPE[slot(lowered >> 10).min(15)];
        self.new_mask();
        let symbol = self.mem.get8(one);
        self.mask(symbol);
        self.prev_success = 0;
        Ok(None)
    }

    /// Escape to the nearest suffix holding symbols not yet masked and
    /// decode there: its symbol, or `None` for a further escape, those
    /// symbols masked too.
    fn decode_masked(
        &mut self,
        coder: &mut Coder,
        br: &mut Reader<'_>,
    ) -> Result<Option<u8>, DriverError> {
        let num_masked = self.mem.get16(self.min_ctx);
        loop {
            self.order_fall += 1;
            let suffix = self.mem.get32(self.min_ctx + 8);
            if suffix == 0 || self.mem.fault.get() {
                return Err(DriverError::MediumError);
            }
            self.min_ctx = suffix;
            if self.mem.get16(suffix) != num_masked {
                break;
            }
        }
        let ctx = self.min_ctx;
        let ns = self.mem.get16(ctx);
        let states = self.mem.get32(ctx + 4);
        let Some(view) = self.mem.states(states, ns) else {
            return Err(DriverError::MediumError);
        };
        // The unmasked states, gathered once: an escape's coding and its
        // masking walk only them.
        let mut found = 0;
        let mut high = 0u32;
        // Branchless: every state is written, and kept only by counting
        // it, as the masks fall unpredictably.
        for (index, record) in view.as_chunks::<6>().0.iter().enumerate() {
            let (symbol, freq) = (record[0], record[1]);
            let unmasked = self.masked[usize::from(symbol)] != self.stamp;
            // At most 256 states, so the index is a byte.
            self.candidates[found] = (index.to_le_bytes()[0], symbol, freq);
            found += usize::from(unmasked);
            high += u32::from(freq) * u32::from(unmasked);
        }
        let (which, esc) = self.escape_estimator(num_masked);
        let total = esc + high;
        let count = coder.threshold(total)?;
        if count < high {
            let mut acc = 0u32;
            let mut chosen = (0u8, 0u8, 0u8);
            for &candidate in &self.candidates[..found] {
                acc += u32::from(candidate.2);
                chosen = candidate;
                if acc > count {
                    break;
                }
            }
            let (symbol, f) = (chosen.1, u32::from(chosen.2));
            let chosen = states + 6 * u32::from(chosen.0);
            coder.decode(br, acc - f, f)?;
            let see = self.estimator(which);
            if u32::from(see.shift) < PERIOD_BITS {
                see.count = see.count.wrapping_sub(1);
                if see.count == 0 {
                    see.summ = see.summ.wrapping_shl(1);
                    see.count = 3u8.wrapping_shl(u32::from(see.shift));
                    see.shift += 1;
                }
            }
            self.found = chosen;
            self.mem.put16(ctx + 2, self.mem.get16(ctx + 2) + 4);
            self.mem.put8(chosen + 1, f + 4);
            if f + 4 > MAX_FREQ {
                self.rescale();
            }
            self.run_length = self.init_rl;
            self.update_model();
            return Ok(Some(symbol));
        }
        if count >= total {
            return Err(DriverError::MediumError);
        }
        coder.decode(br, high, total - high)?;
        let see = self.estimator(which);
        see.summ = see
            .summ
            .wrapping_add(u16::try_from(total & 0xFFFF).unwrap_or(0));
        for &(_, symbol, _) in &self.candidates[..found] {
            self.masked[usize::from(symbol)] = self.stamp;
        }
        Ok(None)
    }
}

/// The least allocator index serving `units`, one to 128.
fn units_index(units: u32) -> usize {
    UNITS_INDEX
        .get(slot(units.wrapping_sub(1)))
        .copied()
        .unwrap_or(NUM_INDEXES - 1)
}

fn slot(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "ppmd_tests.rs"]
mod tests;
