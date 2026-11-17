//! Deterministic fuzz harness for the address space: random runs of slot,
//! dynamic-area, sparse and pool calls with hostile arguments, under a
//! kernel whose handlers answer at random and sometimes call back into the
//! memory calls themselves: resizing, removing and making areas, claiming
//! sparse and pool pages.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every page in use is accounted for: the fixed ones, application
//!   space's, each live area's and each pool's, and the free pool is the
//!   ceiling less those;
//! * every page an area reports mapped reads back at its address, in both
//!   copies of a doubly mapped one;
//! * no area holds more than its maximum, no two live areas' address space
//!   overlaps, and every area's number is one a caller reads as positive and
//!   no system area's.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::bus::{Bus, Width};
use tairix_riscos_kernel::memory::layout::{RISC_OS_5, SHARED};
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::{
    Allowed, AreaFlags, Bits, Create, Guest, GuestError, Handler, Info, LogicalOp, Memory,
    PageEntry, PageQuery, PhysicalOp, Service, Slot, FREE_POOL,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 150;
/// Calls in one run.
const CALLS: usize = 160;
const PAGE: u32 = 0x1000;
/// The highest system area number; every other area's is past it.
const SYSTEM: u32 = 7;
/// Titles a request is drawn with: none, empty, ordinary, and past the
/// longest RISC OS keeps.
const TITLES: [Option<&[u8]>; 4] = [
    None,
    Some(b""),
    Some(b"Fuzz"),
    Some(b"A title much longer than thirty-one characters"),
];

/// A kernel whose every answer is drawn, and which sometimes makes a memory
/// call of its own from inside a handler.
struct Hostile {
    rng: Prng,
    depth: u32,
    areas: Vec<u32>,
    /// The area the call in progress works on, which a handler meddles with
    /// most.
    target: Option<u32>,
}

impl Hostile {
    fn chance(&mut self, one_in: usize) -> bool {
        self.rng.below(one_in) == 0
    }

    /// From inside a handler, a call that changes memory, as a handler may
    /// make.
    fn meddle(&mut self, memory: &mut Memory) {
        if self.depth > 0 || !self.chance(4) {
            return;
        }
        self.depth += 1;
        let area = match (self.target, self.chance(2)) {
            (Some(target), true) => Some(target),
            _ => (!self.areas.is_empty()).then(|| *self.rng.pick(&self.areas)),
        };
        match (self.rng.below(8), area) {
            (0 | 1, Some(area)) => {
                let bytes = if self.chance(2) {
                    drawn_change(&mut self.rng)
                } else {
                    0x1000
                };
                let _ = memory.change_area(i32::try_from(area).unwrap_or(-1), bytes, self);
            }
            (2, Some(area)) => {
                let _ = memory.remove_area(area, self);
            }
            (7, _) => {
                let first = u32::try_from(self.rng.below(600)).unwrap_or(0);
                let count = u32::try_from(self.rng.below(8)).unwrap_or(0);
                let _ = memory.reserve_pages(first, count, self.chance(2));
            }
            (6, Some(area)) => {
                // Removed, and a pool holding a page made in its place.
                let _ = memory.remove_area(area, self);
                let pool = Create {
                    size: 0,
                    flags: AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
                    pool: 4,
                    ..drawn_request(&mut self.rng)
                };
                if let Ok(made) = memory.create_area(&pool, self) {
                    let claim = PhysicalOp {
                        slot: 0,
                        page: -2,
                        flags: 0,
                    };
                    let _ = memory.pool_physical(made.number, &[claim], self);
                }
            }
            (3, _) => {
                let request = drawn_request(&mut self.rng);
                let _ = memory.create_area(&request, self);
            }
            (4, Some(area)) => {
                let base = memory.area_info(area).map_or(0, |info| info.base);
                let size = self.rng.next_u32() % 0x8000;
                let _ = memory.sparse_claim(area, base, size, self);
            }
            (5, Some(area)) => {
                let ops = drawn_physical(&mut self.rng);
                let _ = memory.pool_physical(area, &ops, self);
            }
            _ => {}
        }
        self.depth -= 1;
    }
}

impl Guest for Hostile {
    fn pre_grow(
        &mut self,
        _: &mut Memory,
        _: Handler,
        block: &mut [PageEntry],
        _: u32,
        _: u32,
    ) -> Result<Allowed, Option<GuestError>> {
        if self.chance(8) {
            return Err(if self.chance(2) {
                None
            } else {
                Some(GuestError(0x8000))
            });
        }
        if !block.is_empty() && self.chance(2) {
            for entry in block.iter_mut() {
                entry.page = Some(u32::try_from(self.rng.below(600)).unwrap_or(0));
            }
        }
        Ok(Allowed {
            reserved: self.chance(2),
        })
    }

    fn post_grow(&mut self, memory: &mut Memory, _: Handler, _: &[PageEntry], _: u32, _: u32) {
        self.meddle(memory);
    }

    fn pre_shrink(
        &mut self,
        _: &mut Memory,
        _: Handler,
        bytes: u32,
        _: u32,
    ) -> Result<u32, Option<GuestError>> {
        if self.chance(10) {
            return Err(None);
        }
        Ok(match self.rng.below(3) {
            0 => bytes,
            1 => self.rng.next_u32(),
            _ => self.rng.next_u32() % (bytes + 1),
        })
    }

    fn post_shrink(&mut self, memory: &mut Memory, _: Handler, _: u32, _: u32) {
        self.meddle(memory);
    }

    fn test_shrink(&mut self, memory: &mut Memory, _: Handler, _: u32) -> u32 {
        self.meddle(memory);
        self.rng.next_u32()
    }

    fn resize_pmp(
        &mut self,
        memory: &mut Memory,
        _: Handler,
        pages: i32,
        area: i32,
    ) -> Result<(), Option<GuestError>> {
        if let Ok(area) = u32::try_from(area) {
            let op = PhysicalOp {
                slot: u32::try_from(self.rng.below(6)).unwrap_or(0),
                page: if pages >= 0 { -2 } else { -1 },
                flags: 0,
            };
            let _ = memory.pool_physical(area, &[op], self);
        }
        if self.chance(4) {
            Err(None)
        } else {
            Ok(())
        }
    }

    fn service(&mut self, memory: &mut Memory, service: Service<'_>) -> bool {
        if matches!(service, Service::MemoryMoved { .. }) {
            self.meddle(memory);
        }
        self.chance(12)
    }

    fn moving_memory(&mut self, _: &mut Memory, _: u32) -> bool {
        self.chance(2)
    }

    fn active_object(&self) -> u32 {
        0x0200_0000
    }

    fn in_interrupt(&self) -> bool {
        false
    }
}

fn drawn_change(rng: &mut Prng) -> i32 {
    let magnitude = match rng.below(4) {
        0 => rng.below(4) * 0x1000,
        1 => rng.below(0x10_000),
        2 => rng.below(80) * 0x1000,
        _ => rng.next_u32() as usize,
    };
    let magnitude = i32::try_from(magnitude).unwrap_or(i32::MAX);
    if rng.below(2) == 0 {
        magnitude
    } else {
        magnitude.saturating_neg()
    }
}

/// One of the shapes RISC OS 5 makes, or any mix of flag bits at all.
fn drawn_flags(rng: &mut Prng) -> u32 {
    let choices = [
        0,
        AreaFlags::SHRINKABLE,
        AreaFlags::NEEDS_SPECIFIC_PAGES,
        AreaFlags::NEEDS_DMA,
        AreaFlags::SPARSE,
        AreaFlags::DOUBLY_MAPPED | AreaFlags::NOT_CACHEABLE,
        AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
        AreaFlags::DOUBLY_MAPPED,
        AreaFlags::ABORTABLE,
    ];
    let shape = if rng.below(4) == 0 {
        let bits = [
            AreaFlags::NOT_CACHEABLE,
            AreaFlags::DOUBLY_MAPPED,
            AreaFlags::NOT_USER_DRAGGABLE,
            AreaFlags::NEEDS_SPECIFIC_PAGES,
            AreaFlags::SHRINKABLE,
            AreaFlags::SPARSE,
            AreaFlags::NEEDS_DMA,
            AreaFlags::ABORTABLE,
            AreaFlags::PMP,
        ];
        bits.iter()
            .filter(|_| rng.below(3) == 0)
            .fold(0, |flags, &bit| flags | bit)
    } else {
        *rng.pick(&choices)
    };
    let privilege = u32::try_from(rng.below(16)).unwrap_or(0);
    shape | if rng.below(4) == 0 { privilege } else { 0 }
}

/// A size of a few pages, or one past every limit.
fn drawn_size(rng: &mut Prng, pages: usize) -> u32 {
    match rng.below(10) {
        0 => *rng.pick(&[u32::MAX, 0xFFFF_F001, 0x8000_0000]),
        1 => rng.next_u32(),
        _ => u32::try_from(rng.below(pages)).unwrap_or(0) * PAGE,
    }
}

fn drawn_request(rng: &mut Prng) -> Create<'static> {
    let flags = drawn_flags(rng);
    let pool = flags & AreaFlags::PMP != 0;
    Create {
        number: (rng.below(10) == 0).then(|| match rng.below(3) {
            0 => *rng.pick(&[0, SYSTEM, 0x150, 0x7FFF_FFFF, 0x8000_0000, u32::MAX]),
            _ => 0x1000 + u32::try_from(rng.below(0x100)).unwrap_or(0),
        }),
        size: if pool && rng.below(4) != 0 {
            0
        } else {
            drawn_size(rng, 24)
        },
        base: (rng.below(10) == 0).then(|| match rng.below(3) {
            0 => rng.next_u32(),
            _ => 0x0400_0000 + u32::try_from(rng.below(0x400)).unwrap_or(0) * 0x10_0000,
        }),
        flags,
        max: (rng.below(8) != 0).then(|| drawn_size(rng, 64)),
        handler: (rng.below(5) != 0).then_some(0x1234),
        workspace: None,
        title: *rng.pick(&TITLES),
        pool: if rng.below(10) == 0 {
            rng.next_u32()
        } else {
            u32::try_from(rng.below(8)).unwrap_or(0)
        },
        caller: if rng.below(2) == 0 {
            Bits::Bit26
        } else {
            Bits::Bit32
        },
    }
}

/// A pool's physical op list: a few entries, or now and then more than
/// there are pages.
fn drawn_physical(rng: &mut Prng) -> Vec<PhysicalOp> {
    let entries = if rng.below(40) == 0 {
        5000
    } else {
        rng.below(12)
    };
    (0..entries)
        .map(|_| PhysicalOp {
            slot: u32::try_from(rng.below(10)).unwrap_or(0),
            page: *rng.pick(&[-1, -2, -3, 30, 200, 599, 100_000, i32::MIN]),
            flags: *rng.pick(&[0, 1 << 15, 1 << 7, 1 << 15 | 1 << 7]),
        })
        .collect()
}

/// Every live area's number, in enumeration order.
fn live_areas(memory: &Memory) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut after = None;
    while let Ok(Some(number)) = memory.enumerate_areas(after) {
        after = Some(number);
        if number != FREE_POOL {
            numbers.push(number);
        }
    }
    numbers
}

/// The address space an area holds: both copies of a doubly mapped one.
fn span(info: &Info<'_>) -> core::ops::Range<u64> {
    let (base, max) = (u64::from(info.base), u64::from(info.max));
    if info.flags & AreaFlags::DOUBLY_MAPPED != 0 {
        base.saturating_sub(max)..base + max
    } else {
        base..base + max
    }
}

/// Every page in use and every area's mapping and place, as the invariants
/// require.
fn check(memory: &mut Memory, fixed: u32, slots: &[Slot], step: usize) {
    let mut accounted = fixed;
    for &slot in slots {
        accounted += memory.slot_size(slot).unwrap_or(0) / PAGE;
    }
    let mut mapped = Vec::new();
    let mut spans = Vec::new();
    for number in live_areas(memory) {
        let info = memory.area_info(number).expect("a live area answers");
        assert!(
            number > SYSTEM && i32::try_from(number).is_ok(),
            "step {step}: area {number:#X} numbered"
        );
        assert!(
            info.size <= info.max,
            "step {step}: area {number:#X} past its maximum"
        );
        match info.pool {
            Some((pages, _)) => accounted += pages,
            None => accounted += info.size / PAGE,
        }
        if info.max != 0 {
            spans.push(span(&info));
        }
        if info.pool.is_none() && info.flags & AreaFlags::SPARSE == 0 {
            for page in 0..info.size / PAGE {
                mapped.push(info.base + page * PAGE);
                if info.flags & AreaFlags::DOUBLY_MAPPED != 0 {
                    mapped.push(info.base - info.size + page * PAGE);
                }
            }
        }
    }
    let used = memory.pages_in_use();
    assert_eq!(used, accounted, "step {step}: pages in use");
    assert_eq!(
        memory.free_pages(),
        u32::try_from(memory.ram() / u64::from(PAGE))
            .unwrap_or(0)
            .saturating_sub(used),
        "step {step}: the free pool"
    );
    spans.sort_by_key(|span| span.start);
    for pair in spans.windows(2) {
        assert!(
            pair[0].end <= pair[1].start,
            "step {step}: areas at {:#X?} and {:#X?} overlap",
            pair[0],
            pair[1]
        );
    }
    for address in mapped {
        assert!(
            memory.bus().read(address, Width::Word, true).is_ok(),
            "step {step}: {address:#X} unmapped"
        );
    }
}

#[expect(clippy::too_many_lines, reason = "one arm per call the harness draws")]
fn check_run(rng: &mut Prng) {
    let layout = if rng.below(3) == 0 {
        &RISC_OS_5
    } else {
        &SHARED
    };
    let usable = (4 + rng.below(60)) as u64 * 1024 * 1024;
    let mut memory = Memory::new(layout, Ceiling::derive(usable)).expect("a session");
    let fixed = memory.pages_in_use();
    let mut guest = Hostile {
        rng: Prng::new(rng.next_u64()),
        depth: 0,
        areas: Vec::new(),
        target: None,
    };
    let mut slots = Vec::new();
    for step in 0..CALLS {
        let areas = live_areas(&memory);
        guest.areas.clone_from(&areas);
        guest.target = (!areas.is_empty()).then(|| *rng.pick(&areas));
        match rng.below(14) {
            0 => {
                let bits = if rng.below(3) == 0 {
                    Bits::Bit26
                } else {
                    Bits::Bit32
                };
                if let Ok(slot) = memory.create_slot(bits) {
                    slots.push(slot);
                    let _ = memory.switch_slot(slot);
                }
            }
            1 if !slots.is_empty() => {
                let slot = *rng.pick(&slots);
                let _ = memory.set_slot_size(slot, rng.next_u32() % 0x80_0000);
            }
            2 if slots.len() > 1 => {
                let index = rng.below(slots.len());
                if memory.destroy_slot(slots[index]).is_ok() {
                    slots.remove(index);
                }
            }
            3 | 4 => {
                let request = drawn_request(rng);
                let _ = memory.create_area(&request, &mut guest);
            }
            5 | 6 if !areas.is_empty() => {
                let area = guest.target.unwrap_or(FREE_POOL);
                let bytes = match memory.area_info(area) {
                    // Up to its maximum: the grow a nested one may push past.
                    Ok(info) if rng.below(3) == 0 => {
                        i32::try_from(info.max.saturating_sub(info.size)).unwrap_or(i32::MAX)
                    }
                    _ => drawn_change(rng),
                };
                let _ = memory.change_area(i32::try_from(area).unwrap_or(-1), bytes, &mut guest);
            }
            7 => {
                let _ = memory.change_area(
                    i32::try_from(FREE_POOL).unwrap_or(6),
                    drawn_change(rng),
                    &mut guest,
                );
            }
            8 if !areas.is_empty() => {
                let _ = memory.remove_area(guest.target.unwrap_or(FREE_POOL), &mut guest);
            }
            9 if !areas.is_empty() => {
                let area = *rng.pick(&areas);
                let new = match rng.below(4) {
                    0 => rng.next_u32(),
                    _ => 0x1000 + u32::try_from(rng.below(0x100)).unwrap_or(0),
                };
                let _ = memory.renumber_area(area, new, &mut guest);
            }
            10 if !areas.is_empty() => {
                let area = guest.target.unwrap_or(FREE_POOL);
                let base = memory.area_info(area).map_or(0, |info| info.base);
                let offset = rng.next_u32() % 0x5_0000;
                let size = drawn_size(rng, 32);
                if rng.below(2) == 0 {
                    let _ = memory.sparse_claim(area, base.wrapping_add(offset), size, &mut guest);
                } else {
                    let _ =
                        memory.sparse_release(area, base.wrapping_add(offset), size, &mut guest);
                }
            }
            11 if !areas.is_empty() => {
                let area = guest.target.unwrap_or(FREE_POOL);
                let ops = drawn_physical(rng);
                let _ = memory.pool_physical(area, &ops, &mut guest);
                let logical = [LogicalOp {
                    window: u32::try_from(rng.below(70)).unwrap_or(0),
                    slot: (rng.below(3) != 0).then(|| u32::try_from(rng.below(10)).unwrap_or(0)),
                    flags: rng.next_u32(),
                }];
                let _ = memory.pool_logical(area, &logical);
                let _ = memory.pool_resize(area, drawn_change(rng) % 8);
                let mut queries = [PageQuery {
                    page: Some(u32::try_from(rng.below(600)).unwrap_or(0)),
                    ..PageQuery::default()
                }];
                let _ = memory.pool_query(area, &mut queries);
            }
            12 => {
                let _ = memory.locate(rng.next_u32());
                let _ = memory.free_area_pages(None, &mut guest);
                let _ = memory.read_area(i32::try_from(rng.below(0x300)).unwrap_or(0) - 1);
                let _ = memory.take_area_changes();
            }
            _ => {
                let address = rng.next_u32() & !3;
                let _ = memory
                    .bus()
                    .write(address, Width::Word, rng.next_u32(), rng.below(2) == 0);
                let _ = memory.bus().read(address, Width::Word, true);
            }
        }
        check(&mut memory, fixed, &slots, step);
    }
}

#[test]
fn any_run_of_memory_calls_keeps_every_page_accounted_for() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_run_of_memory_calls_keeps_every_page_accounted_for",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        check_run(&mut rng);
        runs += 1;
    }
}
