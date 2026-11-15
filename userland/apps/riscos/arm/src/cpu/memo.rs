//! The decoded instructions, kept per guest page.
//!
//! A slot holds the word it was decoded from beside the decoding, and is
//! used only while the word fetched there still matches: an instruction
//! fetch always reads memory, so code that is rewritten, a page remapped or
//! a slot reused for another page simply decodes again. Nothing has to
//! announce a change, and no slot can run stale code.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::a32::{self, Instruction};
use crate::cond::Condition;
use crate::profile::{Profile, Unpredictable};

/// Instruction slots in a page: a 4 KiB page of words.
pub(super) const SLOTS: usize = 1024;
/// The page size the memo keys on.
const PAGE_SHIFT: u32 = 12;

/// What a word does on the profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Decoded {
    /// It executes as this instruction.
    Execute(Instruction),
    /// It takes the undefined instruction exception if its condition passes.
    Undefined(Condition),
}

impl Decoded {
    /// What `word` does on `profile`: undefined where the profile has no
    /// such instruction, and where the architecture leaves its encoding
    /// UNPREDICTABLE and the profile takes those as undefined.
    pub(super) fn of(word: u32, profile: &Profile) -> Self {
        let undefined = Self::Undefined(Condition::field(word >> 28));
        let Some(decoded) = a32::decode_detail(word, profile.isa) else {
            return undefined;
        };
        let refused = profile.unpredictable == Unpredictable::Undefined
            && (!decoded.canonical || decoded.instruction.unpredictable(profile.isa));
        if refused {
            undefined
        } else {
            Self::Execute(decoded.instruction)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    word: u32,
    decoded: Decoded,
}

/// Decoded pages, each in a fixed place chosen by its page number. A place
/// is shared by every page its number selects; the slots' words keep them
/// apart.
pub(super) struct Memo {
    places: Vec<Option<Box<[Slot]>>>,
    /// The decoding of a zero word, which a fresh page's slots start as.
    fresh: Slot,
}

impl Memo {
    /// A memo holding at most `pages` decoded pages, rounded down to a power
    /// of two; zero holds none and decodes every instruction it is asked
    /// for.
    pub(super) fn new(pages: usize, profile: &Profile) -> Self {
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
            fresh: Slot {
                word: 0,
                decoded: Decoded::of(0, profile),
            },
        }
    }

    /// What the word fetched at `address` does on `profile`.
    pub(super) fn decoded(&mut self, address: u32, word: u32, profile: &Profile) -> Decoded {
        let page = address >> PAGE_SHIFT;
        let Some(slots) = self.place(page) else {
            return Decoded::of(word, profile);
        };
        let slot = &mut slots[(address as usize >> 2) & (SLOTS - 1)];
        if slot.word != word {
            *slot = Slot {
                word,
                decoded: Decoded::of(word, profile),
            };
        }
        slot.decoded
    }

    /// The slots page `page` uses, or `None` when the memo holds no pages
    /// or the slots cannot be had.
    fn place(&mut self, page: u32) -> Option<&mut [Slot]> {
        // The count is a power of two, so the place is the number's low bits.
        let index = (page as usize) & self.places.len().wrapping_sub(1);
        let place = self.places.get_mut(index)?;
        if place.is_none() {
            let mut slots = Vec::new();
            slots.try_reserve_exact(SLOTS).ok()?;
            slots.resize(SLOTS, self.fresh);
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
        let pages = self.places.iter().filter(|place| place.is_some()).count();
        pages * SLOTS * core::mem::size_of::<Slot>()
    }
}
