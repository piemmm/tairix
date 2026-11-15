//! The decoded instructions, kept per guest page.
//!
//! A slot holds the bits it was decoded from beside the decoding, and is
//! used only while the bits fetched there still match: an instruction fetch
//! always reads memory, so code that is rewritten, a page remapped or a slot
//! reused for another page simply decodes again. Nothing has to announce a
//! change, and no slot can run stale code.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::a32;
use crate::cond::Condition;
use crate::instruction::Instruction;
use crate::profile::{Profile, Unpredictable};
use crate::t32::{self, Position};

/// The page size the memo keys on.
const PAGE_SHIFT: u32 = 12;

/// What an instruction does on the profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Decoded {
    /// It executes as this instruction.
    Execute(Instruction),
    /// It takes the undefined instruction exception if its condition holds.
    Undefined(Condition),
}

impl Decoded {
    /// What the A32 `word` does on `profile`: undefined where the profile
    /// has no such instruction, and where the architecture leaves its
    /// encoding UNPREDICTABLE and the profile takes those as undefined.
    pub(super) fn arm(word: u32, profile: &Profile) -> Self {
        let undefined = Self::Undefined(Condition::field(word >> 28));
        let Some(decoded) = a32::decode_detail(word, profile.isa) else {
            return undefined;
        };
        let refused = profile.unpredictable == Unpredictable::Undefined
            && (!decoded.canonical || a32::unpredictable(&decoded.instruction, profile.isa));
        if refused {
            undefined
        } else {
            Self::Execute(decoded.instruction)
        }
    }

    /// What the T32 instruction `key` names does on `profile`, as
    /// [`Decoded::arm`] judges it.
    pub(super) fn thumb(key: Thumb, profile: &Profile) -> Self {
        let [first, second] = key.split();
        match t32::decode(first, second, profile.isa, key.position) {
            Some(decoded)
                if !decoded.unpredictable || profile.unpredictable == Unpredictable::Execute =>
            {
                Self::Execute(decoded.instruction)
            }
            Some(decoded) => Self::Undefined(decoded.instruction.cond),
            None => Self::Undefined(key.position.condition()),
        }
    }
}

/// What a T32 slot is keyed by: an instruction's halfwords and where it
/// stands in an `IT` block, which both decide its decoding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Thumb {
    /// The first halfword in the low half, the second above it: zero for a
    /// 16-bit instruction, so what follows one never invalidates its slot.
    pub halfwords: u32,
    pub position: Position,
}

impl Thumb {
    const fn split(self) -> [u16; 2] {
        let bytes = self.halfwords.to_le_bytes();
        [
            u16::from_le_bytes([bytes[0], bytes[1]]),
            u16::from_le_bytes([bytes[2], bytes[3]]),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct Slot<K> {
    key: K,
    decoded: Decoded,
}

/// Decoded pages of one instruction set, whose instructions are `1 << SHIFT`
/// bytes apart, each page in a fixed place chosen by its number. A place is
/// shared by every page its number selects; the slots' keys keep them apart.
pub(super) struct Memo<K, const SHIFT: u32> {
    places: Vec<Option<Box<[Slot<K>]>>>,
    /// A slot for the zero key, which a fresh page's slots start as.
    fresh: Slot<K>,
}

impl<K: Copy + PartialEq, const SHIFT: u32> Memo<K, SHIFT> {
    /// Instruction slots in a page.
    const SLOTS: usize = 1 << (PAGE_SHIFT - SHIFT);

    /// A memo holding at most `pages` decoded pages, rounded down to a power
    /// of two, whose zero key `zero` decodes as `decoded`; zero pages holds
    /// none and decodes every instruction it is asked for.
    pub(super) fn new(pages: usize, zero: K, decoded: Decoded) -> Self {
        let pages = if pages == 0 {
            0
        } else {
            1 << (usize::BITS - 1 - pages.leading_zeros())
        };
        let mut places = Vec::new();
        if places.try_reserve_exact(pages).is_ok() {
            places.resize_with(pages, || None);
        }
        Self {
            places,
            fresh: Slot { key: zero, decoded },
        }
    }

    /// The decoding of `key`, fetched at `address`, made by `decode` when the
    /// slot does not hold it.
    pub(super) fn decoded(
        &mut self,
        address: u32,
        key: K,
        decode: impl FnOnce(K) -> Decoded,
    ) -> Decoded {
        let Some(slots) = self.place(address >> PAGE_SHIFT) else {
            return decode(key);
        };
        let slot = &mut slots[(address as usize >> SHIFT) & (Self::SLOTS - 1)];
        if slot.key != key {
            *slot = Slot {
                key,
                decoded: decode(key),
            };
        }
        slot.decoded
    }

    /// The slots page `page` uses, or `None` when the memo holds no pages
    /// or the slots cannot be had.
    fn place(&mut self, page: u32) -> Option<&mut [Slot<K>]> {
        // The count is a power of two, so the place is the number's low bits.
        let index = (page as usize) & self.places.len().wrapping_sub(1);
        let place = self.places.get_mut(index)?;
        if place.is_none() {
            let mut slots = Vec::new();
            slots.try_reserve_exact(Self::SLOTS).ok()?;
            slots.resize(Self::SLOTS, self.fresh);
            *place = Some(slots.into_boxed_slice());
        }
        place.as_deref_mut()
    }

    /// Gives every decoded page's memory back.
    pub(super) fn release(&mut self) {
        self.places.iter_mut().for_each(|place| *place = None);
    }

    /// The bytes the decoded pages hold.
    pub(super) fn bytes(&self) -> usize {
        self.places
            .iter()
            .flatten()
            .map(|slots| core::mem::size_of_val(&**slots))
            .sum()
    }
}
