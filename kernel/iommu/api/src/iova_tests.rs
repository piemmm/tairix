extern crate std;

use std::vec::Vec;

use super::*;

const PAGE: u64 = IO_PAGE_SIZE;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn base_of(block: Option<IovaBlock>) -> Option<u64> {
    block.map(|block| block.base())
}

/// The highest slot of `order` ending by `limit` that any free block holds,
/// found by looking at every one.
fn highest_slot(space: &IovaSpace, order: u32, limit: u64) -> Option<u64> {
    let size = PAGE << order;
    let limit = if limit == 0 {
        space.aperture.end
    } else {
        limit.min(space.aperture.end)
    };
    let blocks = space.orders.iter().enumerate().skip(order as usize);
    blocks
        .flat_map(|(o, held)| {
            held.blocks
                .iter()
                .map(move |(index, ())| (index << (IO_PAGE_SHIFT as usize + o), block_bytes(o)))
        })
        .filter_map(|(base, bytes)| {
            let slot = (base + bytes).min(limit).checked_sub(size)? & !(size - 1);
            (slot >= base).then_some(slot)
        })
        .max()
}

fn assert_highest_kept(space: &IovaSpace) {
    for (o, order) in space.orders.iter().enumerate() {
        let greatest = order.blocks.iter().last().map(|(index, ())| index);
        assert_eq!(order.highest, greatest, "order {o}");
    }
}

#[test]
fn blocks_come_from_the_top_naturally_aligned() {
    let mut space = IovaSpace::new(PAGE..MIB, &[]).unwrap();
    assert_eq!(base_of(space.alloc(0, 0)), Some(MIB - PAGE));
    let block = space.alloc(4, 0).unwrap();
    assert_eq!(block.bytes(), PAGE << 4);
    assert_eq!(block.base() % block.bytes(), 0);
    assert!(block.base() + block.bytes() <= MIB - PAGE);
}

#[test]
fn a_block_ends_below_the_reach_it_is_asked_for() {
    let mut space = IovaSpace::new(PAGE..(1 << 48), &[]).unwrap();
    assert_eq!(
        base_of(space.alloc(3, 4 * GIB)),
        Some(4 * GIB - (PAGE << 3))
    );
    assert_eq!(
        base_of(space.alloc(0, PAGE)),
        None,
        "nothing ends by the first page"
    );
    assert_eq!(base_of(space.alloc(0, 2 * PAGE)), Some(PAGE));
}

/// Each order's highest free block sits above a lower reach, so the search
/// below it walks the order's tree, and taking a highest block hands the
/// order's next one up.
#[test]
fn a_lower_reach_finds_the_highest_block_below_it_in_each_order() {
    let mut space = IovaSpace::new(PAGE..(1 << 40), &[]).unwrap();
    let top = space.alloc(0, 0).unwrap();
    assert_eq!(top.base(), (1 << 40) - PAGE);
    let low = space.alloc(5, 16 * MIB).unwrap();
    assert_eq!(low.base(), 16 * MIB - (PAGE << 5));
    let expected = highest_slot(&space, 5, 16 * MIB);
    let next = space.alloc(5, 16 * MIB).unwrap();
    assert_eq!(Some(next.base()), expected);
    assert_eq!(next.base(), 16 * MIB - (PAGE << 6));
    assert_highest_kept(&space);
    for block in [top, low, next] {
        space.free(block).unwrap();
        assert_highest_kept(&space);
    }
    assert_eq!(base_of(space.alloc(0, 0)), Some((1 << 40) - PAGE));
}

#[test]
fn a_reserved_window_is_never_handed_out() {
    let hole = 0xFEE0_0000..0xFEF0_0000;
    let mut space = IovaSpace::new(PAGE..4 * GIB, core::slice::from_ref(&hole)).unwrap();
    for _ in 0..24 {
        let block = space.alloc(8, 0).unwrap();
        let end = block.base() + block.bytes();
        assert!(
            end <= hole.start || block.base() >= hole.end,
            "{:#x} overlaps the hole",
            block.base()
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
        space.free(block).unwrap();
    }
    assert_eq!(space.free_bytes(), initial);
    assert_highest_kept(&space);
    let half = (top / 2 / PAGE).trailing_zeros();
    assert_eq!(base_of(space.alloc(half, 0)), Some(top - (PAGE << half)));
}

#[test]
fn buddies_merge_into_their_parent() {
    let mut space = IovaSpace::new(PAGE..(PAGE << 10), &[]).unwrap();
    let a = space.alloc(0, 0).unwrap();
    let b = space.alloc(0, 0).unwrap();
    assert_eq!(a.base() ^ b.base(), PAGE, "the top two pages are buddies");
    space.free(a).unwrap();
    space.free(b).unwrap();
    assert_eq!(base_of(space.alloc(8, 0)), Some((PAGE << 10) - (PAGE << 8)));
}

#[test]
fn an_order_past_the_space_is_refused() {
    let mut space = IovaSpace::new(PAGE..MIB, &[]).unwrap();
    assert_eq!(space.alloc(u32::try_from(ORDERS).unwrap(), 0), None);
    assert_eq!(space.alloc(u32::MAX, 0), None);
    assert_eq!(space.alloc(8, 0), None, "larger than the aperture");
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
    assert_eq!(base_of(space.alloc(0, 0)), Some(top - PAGE));
    assert!(space.alloc(40, 0).is_some());
    assert_highest_kept(&space);
}

/// Random allocations and frees always take the highest slot any free block
/// holds, never hand out overlapping blocks, never stray outside the
/// aperture or into a hole, and account every byte.
#[test]
fn random_traffic_takes_the_highest_slot_and_keeps_every_byte_accounted() {
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
    let mut live: Vec<IovaBlock> = Vec::new();
    for _ in 0..if cfg!(miri) { 150 } else { 4_000 } {
        if live.is_empty() || next() % 3 != 0 {
            let order = u32::try_from(next() % 10).unwrap();
            let limit = if next() % 4 == 0 { 1 << 31 } else { 0 };
            let expected = highest_slot(&space, order, limit);
            let block = space.alloc(order, limit);
            assert_eq!(block.as_ref().map(IovaBlock::base), expected);
            if let Some(block) = block {
                let (base, end) = (block.base(), block.base() + block.bytes());
                assert!(base >= aperture.start && end <= aperture.end);
                assert!(limit == 0 || end <= limit);
                assert!(end <= hole.start || base >= hole.end);
                assert_eq!(base % block.bytes(), 0);
                for other in &live {
                    assert!(
                        end <= other.base() || base >= other.base() + other.bytes(),
                        "blocks overlap"
                    );
                }
                live.push(block);
            }
        } else {
            let index = usize::try_from(next() % live.len() as u64).unwrap();
            space.free(live.swap_remove(index)).unwrap();
        }
        assert_highest_kept(&space);
        let held: u64 = live.iter().map(IovaBlock::bytes).sum();
        assert_eq!(space.free_bytes() + held, total);
    }
}
