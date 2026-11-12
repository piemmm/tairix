use super::*;
use crate::dos::engine::Kernel;
use crate::machine::{Machine, Template};

/// A machine whose memory from `first` holds one free block to `top`.
fn arena(first: u16, top: u16) -> Machine {
    let config = Template::IbmPc.config();
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    let mut g = crate::bios::guest::Guest::new(&mut m);
    let mut k = Kernel::new(&mut g);
    let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::FIRST_MCB)).unwrap();
    k.set16(at, first);
    k.g.write8(first, mcb::KIND, mcb::LAST);
    k.g.write16(first, mcb::OWNER, mcb::FREE);
    k.g.write16(first, mcb::SIZE, top - first - 1);
    m
}

fn with_kernel<T>(m: &mut Machine, f: impl FnOnce(&mut Kernel<'_, '_>) -> T) -> T {
    let mut g = crate::bios::guest::Guest::new(m);
    let mut k = Kernel::new(&mut g);
    f(&mut k)
}

#[test]
fn an_allocation_splits_the_free_block_and_a_free_merges_it_back() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let a = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        assert_eq!(a, 0x1001);
        let b = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        assert_eq!(b, 0x1102);
        k.free(a).unwrap();
        k.free(b).unwrap();
        let whole = k.allocate(0x0FFF, 0x0800, strategy::FIRST).unwrap();
        assert_eq!(whole, 0x1001);
    });
}

#[test]
fn a_request_too_large_reports_the_largest_free_block() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        assert_eq!(
            k.allocate(0x2000, 0x0800, strategy::FIRST),
            Err(Refusal::Short(0x0FFF))
        );
    });
}

#[test]
fn last_fit_takes_the_top_of_the_block() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let high = k.allocate(0x10, 0x0800, strategy::LAST).unwrap();
        assert_eq!(high, 0x2000 - 0x10);
    });
}

#[test]
fn a_damaged_chain_is_refused_not_walked() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        k.g.write8(0x1000, mcb::KIND, b'X');
        assert_eq!(
            k.allocate(0x10, 0x0800, strategy::FIRST),
            Err(Refusal::Trashed)
        );
    });
}

#[test]
fn a_block_resized_up_takes_the_free_block_after_it() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let a = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        k.resize(a, 0x200).unwrap();
        assert_eq!(k.block(a - 1).unwrap().size, 0x200);
        assert_eq!(k.resize(a, 0x2000), Err(Refusal::Short(0x0FFF)));
    });
}

#[test]
fn each_strategy_picks_its_own_free_block() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let mut held = [0u16; 5];
        for (slot, size) in held.iter_mut().zip([0x100, 0x40, 0x100, 0x20, 0x100]) {
            *slot = k.allocate(size, 0x0800, strategy::FIRST).unwrap();
        }
        k.free(held[1]).unwrap();
        k.free(held[3]).unwrap();
        let first = k.allocate(0x10, 0x0900, strategy::FIRST).unwrap();
        assert_eq!(first, held[1]);
        k.free(first).unwrap();
        let best = k.allocate(0x10, 0x0900, strategy::BEST).unwrap();
        assert_eq!(best, held[3]);
        k.free(best).unwrap();
        assert_eq!(k.allocate(0x30, 0x0900, strategy::BEST).unwrap(), held[1]);
        assert_eq!(
            k.allocate(0x10, 0x0900, strategy::LAST).unwrap(),
            0x2000 - 0x10
        );
    });
}

#[test]
fn an_owners_blocks_are_freed_together_and_no_others() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let a = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        let b = k.allocate(0x100, 0x0900, strategy::FIRST).unwrap();
        let c = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        k.free_owned(0x0800);
        assert_eq!(k.block(a - 1).unwrap().owner, mcb::FREE);
        assert_eq!(k.block(b - 1).unwrap().owner, 0x0900);
        assert_eq!(k.block(c - 1).unwrap().owner, mcb::FREE);
    });
}

#[test]
fn a_block_outside_the_chain_cannot_be_freed() {
    let mut m = arena(0x1000, 0x2000);
    with_kernel(&mut m, |k| {
        let a = k.allocate(0x100, 0x0800, strategy::FIRST).unwrap();
        let fake = a + 0x10;
        k.g.write8(fake - 1, mcb::KIND, mcb::MORE);
        k.g.write16(fake - 1, mcb::OWNER, 0x0800);
        k.g.write16(fake - 1, mcb::SIZE, 0x10);
        assert_eq!(k.free(fake), Err(Refusal::NoBlock));
        assert_eq!(k.block(fake - 1).unwrap().owner, 0x0800);
    });
}
