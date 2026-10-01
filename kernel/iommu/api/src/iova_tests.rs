extern crate std;

use std::vec::Vec;

use super::*;

const PAGE: u64 = IO_PAGE_SIZE;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

#[test]
fn blocks_come_from_the_top_naturally_aligned() {
    let mut space = IovaSpace::new(PAGE..MIB, &[]).unwrap();
    assert_eq!(space.alloc(0, 0), Some(MIB - PAGE));
    let block = space.alloc(4, 0).unwrap();
    assert_eq!(block % (PAGE << 4), 0);
    assert!(block + (PAGE << 4) <= MIB - PAGE);
}

#[test]
fn a_block_ends_below_the_reach_it_is_asked_for() {
    let mut space = IovaSpace::new(PAGE..(1 << 48), &[]).unwrap();
    let block = space.alloc(3, 4 * GIB).unwrap();
    assert_eq!(block, 4 * GIB - (PAGE << 3));
    assert_eq!(space.alloc(0, PAGE), None, "nothing ends by the first page");
    assert_eq!(space.alloc(0, 2 * PAGE), Some(PAGE));
}

#[test]
fn a_reserved_window_is_never_handed_out() {
    let hole = 0xFEE0_0000..0xFEF0_0000;
    let mut space = IovaSpace::new(PAGE..4 * GIB, core::slice::from_ref(&hole)).unwrap();
    for _ in 0..24 {
        let block = space.alloc(8, 0).unwrap();
        let end = block + MIB;
        assert!(
            end <= hole.start || block >= hole.end,
            "{block:#x} overlaps the hole"
        );
    }
}

#[test]
fn overlapping_and_outlying_reservations_merge() {
    let reserved = [
        0x3000..0x6000,
        0x5000..0x8000,
        0x10_0000..0x20_0000,
        0x0..0x2000,
    ];
    let space = IovaSpace::new(PAGE..0x10_0000, &reserved).unwrap();
    assert_eq!(space.free_bytes(), 0x10_0000 - 0x8000 + 0x1000);
}

#[test]
fn freeing_everything_merges_back_to_the_original_space() {
    // Small enough for the interpreter to walk every block.
    let top = if cfg!(miri) { 2 * MIB } else { 64 * MIB };
    let aperture = PAGE..top;
    let mut space = IovaSpace::new(aperture.clone(), &[]).unwrap();
    let initial = space.free_bytes();
    let mut live = Vec::new();
    while let Some(block) = space.alloc(2, 0) {
        live.push(block);
    }
    assert!(space.free_bytes() < PAGE << 2);
    for block in live {
        space.free(block, 2).unwrap();
    }
    assert_eq!(space.free_bytes(), initial);
    let half = (top / 2 / PAGE).trailing_zeros();
    assert_eq!(space.alloc(half, 0), Some(top - (PAGE << half)));
}

#[test]
fn buddies_merge_into_their_parent() {
    let mut space = IovaSpace::new(PAGE..(PAGE << 10), &[]).unwrap();
    let a = space.alloc(0, 0).unwrap();
    let b = space.alloc(0, 0).unwrap();
    assert_eq!(a ^ b, PAGE, "the top two pages are buddies");
    space.free(a, 0).unwrap();
    space.free(b, 0).unwrap();
    assert_eq!(space.alloc(8, 0), Some((PAGE << 10) - (PAGE << 8)));
}

#[test]
fn a_bad_return_is_refused_as_a_value() {
    let mut space = IovaSpace::new(PAGE..MIB, &[]).unwrap();
    let block = space.alloc(1, 0).unwrap();
    assert_eq!(space.free(block + PAGE, 1), Err(IovaError::NotAllocated));
    assert_eq!(space.free(0, 0), Err(IovaError::NotAllocated));
    assert_eq!(space.free(MIB, 0), Err(IovaError::NotAllocated));
    assert_eq!(space.free(PAGE, 60), Err(IovaError::NotAllocated));
    space.free(block, 1).unwrap();
    assert_eq!(
        space.free(block, 1),
        Err(IovaError::NotAllocated),
        "after merging upward"
    );
    assert_eq!(
        space.free(block, 0),
        Err(IovaError::NotAllocated),
        "half of a free block"
    );

    let pair = space.alloc(1, 0).unwrap();
    space.free(pair + PAGE, 0).unwrap();
    assert_eq!(
        space.free(pair, 1),
        Err(IovaError::NotAllocated),
        "a free half inside it"
    );
}

#[test]
fn a_bad_aperture_or_reservation_is_refused() {
    assert_eq!(
        IovaSpace::new(0..MIB, &[]).err(),
        Some(IovaError::BadAperture)
    );
    assert_eq!(
        IovaSpace::new(PAGE..PAGE, &[]).err(),
        Some(IovaError::BadAperture)
    );
    assert_eq!(
        IovaSpace::new(PAGE + 1..MIB, &[]).err(),
        Some(IovaError::BadAperture)
    );
    for bad in [0x2000..0x2000, 0x2000..0x2800] {
        assert_eq!(
            IovaSpace::new(PAGE..MIB, core::slice::from_ref(&bad)).err(),
            Some(IovaError::BadReservation)
        );
    }
}

#[test]
fn the_top_of_a_64_bit_space_is_reachable() {
    let top = !(PAGE - 1);
    let mut space = IovaSpace::new(PAGE..top, &[]).unwrap();
    assert_eq!(space.alloc(0, 0), Some(top - PAGE));
    assert!(space.alloc(40, 0).is_some());
}

/// Random allocations and frees never hand out overlapping blocks, never
/// stray outside the aperture or into a hole, and account every byte.
#[test]
fn random_traffic_keeps_every_block_disjoint_and_every_byte_accounted() {
    let aperture = PAGE..(1 << 32);
    let hole = 0xFEE0_0000..0xFEF0_0000;
    let mut space = IovaSpace::new(aperture.clone(), core::slice::from_ref(&hole)).unwrap();
    let total = space.free_bytes();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut live: Vec<(u64, u32)> = Vec::new();
    for _ in 0..if cfg!(miri) { 150 } else { 4_000 } {
        if live.is_empty() || next() % 3 != 0 {
            let order = u32::try_from(next() % 10).unwrap();
            let limit = if next() % 4 == 0 { 1 << 31 } else { 0 };
            if let Some(base) = space.alloc(order, limit) {
                let end = base + (PAGE << order);
                assert!(base >= aperture.start && end <= aperture.end);
                assert!(limit == 0 || end <= limit);
                assert!(end <= hole.start || base >= hole.end);
                assert_eq!(base % (PAGE << order), 0);
                for &(other, other_order) in &live {
                    let other_end = other + (PAGE << other_order);
                    assert!(end <= other || base >= other_end, "blocks overlap");
                }
                live.push((base, order));
            }
        } else {
            let index = usize::try_from(next() % live.len() as u64).unwrap();
            let (base, order) = live.swap_remove(index);
            space.free(base, order).unwrap();
        }
        let held: u64 = live.iter().map(|&(_, order)| PAGE << order).sum();
        assert_eq!(space.free_bytes() + held, total);
    }
}
