//! The region the front-end shares with its worker: two frame slots for each
//! display adapter fitted, each as large as the adapter's monitor shows.
//!
//! Both sides derive the layout from the machine's configuration, so neither
//! takes the other's word for where a slot lies. The worker writes a slot only
//! while it is its own; it names the slot once the frame is in it, and the
//! front-end copies the frame out and hands the slot back.

use crate::video::Adapter;

/// Slots for each adapter: one being written while the front-end copies the
/// other out.
pub const SLOTS_PER_ADAPTER: usize = 2;

/// Most adapters a machine has: one monochrome and one colour.
pub const MAX_ADAPTERS: usize = 2;

/// Most slots a layout has.
pub const MAX_SLOTS: usize = SLOTS_PER_ADAPTER * MAX_ADAPTERS;

/// Bytes a pixel takes in a slot: red, green, blue and alpha.
pub const PIXEL: usize = 4;

/// One slot of the region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The fitted adapter, counted in order, whose frames it holds.
    pub adapter: u8,
    /// Where it begins in the region.
    pub offset: usize,
    /// The widest frame it holds, in dots.
    pub width: u32,
    /// The tallest, in lines.
    pub height: u32,
}

impl Slot {
    /// Bytes a frame of `width` by `height` takes, if the slot holds one so
    /// large.
    #[must_use]
    pub fn frame_bytes(&self, width: u32, height: u32) -> Option<usize> {
        if width == 0 || height == 0 || width > self.width || height > self.height {
            return None;
        }
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(PIXEL)
    }

    /// Bytes the slot spans.
    #[must_use]
    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize * PIXEL
    }

    /// Whether the slot spans nothing, which no adapter's does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where each slot lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    slots: [Option<Slot>; MAX_SLOTS],
    len: usize,
}

impl Layout {
    /// The layout for a machine with the adapters `video` lists.
    #[must_use]
    pub fn of(video: &[Option<Adapter>; MAX_ADAPTERS]) -> Self {
        let mut slots = [None; MAX_SLOTS];
        let mut len = 0;
        let fitted = video.iter().flatten().zip(0u8..);
        for (place, (adapter, n)) in slots.chunks_mut(SLOTS_PER_ADAPTER).zip(fitted) {
            let (width, height) = adapter.largest_frame();
            for slot in place {
                let next = Slot {
                    adapter: n,
                    offset: len,
                    width,
                    height,
                };
                len += next.len();
                *slot = Some(next);
            }
        }
        Self { slots, len }
    }

    /// Bytes the region spans.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the region spans nothing: a machine with no display.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Slot `n`, if the layout has one.
    #[must_use]
    pub fn slot(&self, n: u8) -> Option<Slot> {
        self.slots.get(usize::from(n)).copied().flatten()
    }

    /// Every slot, numbered.
    pub fn slots(&self) -> impl Iterator<Item = (u8, Slot)> + '_ {
        (0u8..)
            .zip(self.slots.iter())
            .filter_map(|(n, slot)| slot.map(|slot| (n, slot)))
    }
}
