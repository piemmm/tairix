//! `STACKS=`: hardware interrupts switched to a stack of DOS's own before
//! their handlers run, so a handler never runs on what little stack the
//! program it interrupted had left. The stacks, their bookkeeping and the
//! vectors they replaced are an arena block of DOS's.

use super::data::driver;
use super::engine::Kernel;
use super::memory::{mcb, strategy};
use super::rom::entry;
use crate::asm::Reg16;
use crate::bios::guest::Guest;
use crate::bios::layout::SEGMENT as ROM;
use crate::cpu::Seg;

/// How many vectors are switched.
pub const HOOKED_COUNT: u16 = 8;

/// The vectors switched, as MS-DOS switches them on a machine with one
/// interrupt controller: the NMI and IRQ 0 to 6.
const HOOKED: [u8; HOOKED_COUNT as usize] = [0x02, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E];

/// The stacks `STACKS=` may ask for, and the bytes it may ask for each.
pub const COUNTS: core::ops::RangeInclusive<u8> = 8..=64;
pub const SIZES: core::ops::RangeInclusive<u16> = 32..=512;

/// The block's fields.
pub(super) mod field {
    /// Stacks.
    pub const COUNT: u16 = 0x00;
    /// Bytes of each.
    pub const SIZE: u16 = 0x02;
    /// The stack the next search starts at.
    pub const NEXT: u16 = 0x04;
    /// The vectors replaced, in [`super::HOOKED`] order.
    pub const VECTORS: u16 = 0x06;
    /// A record per stack: whether it is taken, then the SS:SP its
    /// interrupt came from.
    pub const RECORDS: u16 = VECTORS + 4 * super::HOOKED_COUNT;
}

/// A record's fields.
pub(super) mod record {
    pub const TAKEN: u16 = 0;
    pub const SP: u16 = 2;
    pub const SS: u16 = 4;
    pub const LEN: u16 = 6;
}

/// The block's layout, read from it and checked, as a program may have
/// written over it.
struct Layout {
    segment: u16,
    count: u16,
    size: u16,
}

impl Layout {
    fn read(k: &mut Kernel<'_, '_>) -> Option<Self> {
        let segment = k.get16(driver::STACKS);
        let count = k.g.read16(segment, field::COUNT);
        let size = k.g.read16(segment, field::SIZE);
        let shaped = segment != 0
            && (1..=u16::from(*COUNTS.end())).contains(&count)
            && SIZES.contains(&size)
            && size.is_multiple_of(2);
        shaped.then_some(Self {
            segment,
            count,
            size,
        })
    }

    /// The offset just past stack `n`.
    fn top(&self, n: u16) -> u16 {
        record_at(self.count) + (n + 1) * self.size
    }
}

/// Where stack `n`'s record lies.
const fn record_at(n: u16) -> u16 {
    field::RECORDS + n * record::LEN
}

/// The paragraphs a block of `count` stacks of `size` bytes takes.
fn paragraphs(count: u8, size: u16) -> u16 {
    let bytes = u32::from(field::RECORDS) + u32::from(count) * u32::from(record::LEN + size);
    u16::try_from(bytes.div_ceil(16)).unwrap_or(u16::MAX)
}

/// Makes `count` stacks of `size` bytes and points the hooked vectors at
/// the ROM entries that switch to them; `false` when memory is short.
pub fn install(k: &mut Kernel<'_, '_>, count: u8, size: u16) -> bool {
    if count == 0 {
        return true;
    }
    let size = size & !1;
    let Ok(segment) = k.allocate(paragraphs(count, size), mcb::DOS, strategy::FIRST) else {
        return false;
    };
    k.name_block(segment, b"SD");
    k.g.write16(segment, field::COUNT, u16::from(count));
    k.g.write16(segment, field::SIZE, size);
    k.g.write16(segment, field::NEXT, 0);
    for n in 0..u16::from(count) {
        k.g.write16(segment, record_at(n) + record::TAKEN, 0);
    }
    for (n, vector) in (0u16..).zip(HOOKED) {
        let at = u16::from(vector) * 4;
        let (offset, seg) = (k.g.read16(0, at), k.g.read16(0, at + 2));
        k.g.write16(segment, field::VECTORS + n * 4, offset);
        k.g.write16(segment, field::VECTORS + n * 4 + 2, seg);
        k.g.write16(0, at, entry::STACKS + n * entry::STACKS_STRIDE);
        k.g.write16(0, at + 2, ROM);
    }
    k.set16(driver::STACKS, segment);
    true
}

/// Hooked vector `hooked` was taken: its handler is called on a free
/// stack, returning to the ROM's way back. With none free, the ROM entry
/// goes on to halt the machine, as DOS does.
pub fn enter(g: &mut Guest<'_>, hooked: u16) {
    let mut k = Kernel::new(g);
    let layout = Layout::read(&mut k).filter(|_| hooked < HOOKED_COUNT);
    let Some(layout) = layout else {
        return k.g.jump(ROM, entry::STACK_FAILURE);
    };
    let vector = field::VECTORS + hooked * 4;
    let first = k.g.read16(layout.segment, field::NEXT) % layout.count;
    let Some(n) = (0..layout.count)
        .map(|step| (first + step) % layout.count)
        .find(|&n| k.g.read16(layout.segment, record_at(n) + record::TAKEN) == 0)
    else {
        return;
    };
    let at = record_at(n);
    let (ss, sp) = (k.g.seg(Seg::Ss), k.g.r16(Reg16::Sp));
    k.g.write16(layout.segment, at + record::TAKEN, 1);
    k.g.write16(layout.segment, at + record::SP, sp);
    k.g.write16(layout.segment, at + record::SS, ss);
    k.g.write16(layout.segment, field::NEXT, (n + 1) % layout.count);
    let (offset, segment) = (
        k.g.read16(layout.segment, vector),
        k.g.read16(layout.segment, vector + 2),
    );
    let flags = k.g.regs().flags;
    k.g.set_stack(layout.segment, layout.top(n));
    k.g.push(n);
    k.g.push(flags);
    k.g.push(ROM);
    k.g.push(entry::STACK_OUT);
    k.g.jump(segment, offset);
}

/// A handler called on one of the stacks has returned: the stack it ran on
/// is given back and the interrupted SS:SP restored, for the ROM entry's
/// IRET. A stack left otherwise than it was taken halts the machine.
pub fn leave(g: &mut Guest<'_>) {
    let mut k = Kernel::new(g);
    let (ss, sp) = (k.g.seg(Seg::Ss), k.g.r16(Reg16::Sp));
    let restored = Layout::read(&mut k).and_then(|layout| {
        let n = k.g.read16(ss, sp);
        if ss != layout.segment || n >= layout.count || sp != layout.top(n) - 2 {
            return None;
        }
        let at = record_at(n);
        if k.g.read16(layout.segment, at + record::TAKEN) == 0 {
            return None;
        }
        k.g.write16(layout.segment, at + record::TAKEN, 0);
        Some((
            k.g.read16(layout.segment, at + record::SS),
            k.g.read16(layout.segment, at + record::SP),
        ))
    });
    match restored {
        Some((ss, sp)) => k.g.set_stack(ss, sp),
        None => k.g.jump(ROM, entry::STACK_FAILURE),
    }
}
