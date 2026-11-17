//! `OS_Heap`'s heaps over a plain memory: RISC OS 5's placement, merging and
//! resizing, and a heap the guest has overwritten refused, never followed.

use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A memory of `words` words at `BASE`.
struct Ram {
    words: Vec<u32>,
}

const BASE: u32 = 0x1_0000;

impl Ram {
    fn new(bytes: u32) -> Self {
        Self {
            words: vec![0; bytes as usize / 4],
        }
    }

    fn index(&self, address: u32) -> Option<usize> {
        let offset = address.checked_sub(BASE)?;
        (offset % 4 == 0 && (offset as usize / 4) < self.words.len()).then_some(offset as usize / 4)
    }
}

impl Space for Ram {
    fn read(&mut self, address: u32) -> Option<u32> {
        self.index(address).map(|i| self.words[i])
    }

    fn write(&mut self, address: u32, value: u32) -> Option<()> {
        let i = self.index(address)?;
        self.words[i] = value;
        Some(())
    }

    fn valid(&mut self, start: u32, end: u64) -> bool {
        u64::from(start) >= u64::from(BASE) && end <= u64::from(BASE) + 4 * self.words.len() as u64
    }
}

fn heap(size: u32) -> Ram {
    let mut ram = Ram::new(size);
    assert_eq!(initialise(&mut ram, BASE, size), Ok(()));
    ram
}

fn open(ram: &mut Ram) -> Heap {
    match Heap::open(ram, BASE) {
        Ok(heap) => heap,
        Err(error) => panic!("open: {error:?}"),
    }
}

#[test]
fn a_new_heap_describes_its_whole_room() {
    let mut ram = heap(4096);
    assert_eq!(ram.read(BASE), Some(MAGIC));
    let h = open(&mut ram);
    assert_eq!(h.describe(&mut ram), Ok((4096 - 16 - 4, 4096 - 16)));
    assert_eq!(
        initialise(&mut ram, BASE, 20),
        Err(HeapError::FailInit),
        "too small"
    );
}

#[test]
fn claims_round_to_words_and_take_from_the_top_area() {
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    let a = h.get(&mut ram, 1).unwrap_or(0);
    assert_eq!(a, BASE + 20, "past the descriptor and the size word");
    assert_eq!(h.block_size(&mut ram, a), Ok(8));
    let b = h.get(&mut ram, 10).unwrap_or(0);
    assert_eq!(b, a + 8);
    assert_eq!(
        h.block_size(&mut ram, b),
        Ok(16),
        "(10 + 7) rounded down to a word"
    );
    assert_eq!(h.get(&mut ram, 0), Err(HeapError::NoRoom));
    assert_eq!(h.get(&mut ram, 0x8000_0000), Err(HeapError::NoRoom));
}

#[test]
fn freeing_merges_neighbours_and_gives_the_top_back() {
    let mut ram = heap(4096);
    let mut heap = open(&mut ram);
    let a = heap.get(&mut ram, 16).unwrap_or(0);
    let b = heap.get(&mut ram, 16).unwrap_or(0);
    let c = heap.get(&mut ram, 16).unwrap_or(0);
    assert_eq!(heap.free(&mut ram, a), Ok(()));
    assert_eq!(heap.free(&mut ram, b), Ok(()), "merges with a");
    let (_, free_before) = heap.describe(&mut ram).unwrap_or((0, 0));
    assert_eq!(
        heap.free(&mut ram, c),
        Ok(()),
        "merges everything back into the top"
    );
    let (largest, total) = heap.describe(&mut ram).unwrap_or((0, 0));
    assert_eq!(total, 4096 - 16);
    assert_eq!(largest, 4096 - 16 - 4);
    assert!(free_before < total);
    assert_eq!(
        heap.free(&mut ram, a),
        Err(HeapError::NotABlock),
        "a double free"
    );
    let d = heap.get(&mut ram, 16).unwrap_or(0);
    assert_eq!(
        heap.free(&mut ram, d + 4),
        Err(HeapError::NotABlock),
        "an interior pointer"
    );
}

#[test]
fn a_fitting_free_block_is_split_from_its_top_first_fit() {
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    let a = h.get(&mut ram, 60).unwrap_or(0);
    let _guard = h.get(&mut ram, 4).unwrap_or(0);
    assert_eq!(h.free(&mut ram, a), Ok(()));
    let b = h.get(&mut ram, 12).unwrap_or(0);
    assert_eq!(b, a + 64 - 16, "the allocation is the free block's top");
    let c = h.get(&mut ram, 44).unwrap_or(0);
    assert_eq!(c, a, "a remnant of 4 is absorbed");
    assert_eq!(h.block_size(&mut ram, c), Ok(48));
}

#[test]
fn blocks_grow_in_place_downward_or_by_moving_and_keep_their_bytes() {
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    let a = h.get(&mut ram, 8).unwrap_or(0);
    ram.write(a, 0xA1);
    ram.write(a + 4, 0xA2);
    let grown = h.extend_block(&mut ram, a, 8).unwrap_or(None).unwrap_or(0);
    assert_eq!(grown, a, "into the top area in place");
    assert_eq!(h.block_size(&mut ram, a), Ok(20));
    let b = h.get(&mut ram, 8).unwrap_or(0);
    let moved = h.extend_block(&mut ram, a, 64).unwrap_or(None).unwrap_or(0);
    assert_ne!(moved, a, "blocked by b, it moves");
    assert_eq!(ram.read(moved), Some(0xA1));
    assert_eq!(ram.read(moved + 4), Some(0xA2));
    assert_eq!(h.free(&mut ram, b), Ok(()));
    let shrunk = h.extend_block(&mut ram, moved, -40).unwrap_or(None);
    assert_eq!(shrunk, Some(moved));
    assert_eq!(
        h.extend_block(&mut ram, moved, -1000),
        Ok(None),
        "shrunk away"
    );
}

#[test]
fn the_heap_grows_and_shrinks_only_to_its_high_water_mark() {
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    assert_eq!(h.extend_heap(&mut ram, -1000), Ok(-1000));
    let _a = h.get(&mut ram, 100).unwrap_or(0);
    let result = h.extend_heap(&mut ram, -4000);
    assert!(matches!(result, Err(HeapError::ExcessiveShrink { .. })));
    assert_eq!(
        h.extend_heap(&mut ram, 1 << 20),
        Err(HeapError::BadExtend),
        "past the memory"
    );
}

#[test]
fn aligned_claims_are_aligned_and_cross_no_boundary() {
    let mut ram = heap(1 << 14);
    let mut h = open(&mut ram);
    let a = h.get_aligned(&mut ram, 100, 256, 0, BASE).unwrap_or(0);
    assert_eq!((a - BASE) % 256, 0);
    let b = h.get_aligned(&mut ram, 200, 16, 256, BASE).unwrap_or(0);
    assert_eq!(
        (b - BASE) / 256,
        (b - BASE + 199) / 256,
        "within one 256-byte span"
    );
    assert_eq!(
        h.get_aligned(&mut ram, 100, 12, 0, BASE),
        Err(HeapError::NoRoom),
        "not a power of two"
    );
    assert_eq!(
        h.get_aligned(&mut ram, 100, 64, 32, BASE),
        Err(HeapError::NoRoom),
        "a boundary below the alignment"
    );
    for odd in [3, 5, 6] {
        assert_eq!(
            h.get_aligned(&mut ram, 8, odd, 0, BASE),
            Err(HeapError::NoRoom),
            "{odd}: not a power of two"
        );
    }
    let small = [0, 1, 2].map(|alignment| h.get_aligned(&mut ram, 8, alignment, 0, BASE));
    for claimed in small {
        let at = claimed.unwrap_or(0);
        assert_eq!((at - BASE) % 4, 0, "none, one or two counts as four");
        assert_eq!(h.free(&mut ram, at), Ok(()));
    }
    assert_eq!(h.free(&mut ram, a), Ok(()));
    assert_eq!(h.free(&mut ram, b), Ok(()));
}

#[test]
fn an_overwritten_heap_is_refused_rather_than_followed() {
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    let a = h.get(&mut ram, 16).unwrap_or(0);
    let b = h.get(&mut ram, 16).unwrap_or(0);
    assert_eq!(h.free(&mut ram, a), Ok(()));
    // A free list that loops back on itself.
    ram.write(a - 4, 0u32.wrapping_sub(8));
    let h = open(&mut ram);
    assert_eq!(h.describe(&mut ram), Err(HeapError::BadLink));
    // A block whose size is zero.
    let mut ram = heap(4096);
    let mut h = open(&mut ram);
    let a = h.get(&mut ram, 16).unwrap_or(0);
    let c = h.get(&mut ram, 16).unwrap_or(0);
    ram.write(a - 4, 0);
    assert_eq!(h.free(&mut ram, c), Err(HeapError::BadLink));
    let _ = b;
    // A descriptor whose high-water mark passes its end.
    let mut ram = heap(4096);
    ram.write(BASE + 8, 8192);
    assert!(Heap::open(&mut ram, BASE).is_err());
    ram.write(BASE, 0);
    assert_eq!(
        Heap::open(&mut ram, BASE).err(),
        Some(HeapError::BadDescriptor)
    );
}
