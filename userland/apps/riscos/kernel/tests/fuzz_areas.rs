//! Deterministic fuzz harness for the address space: random runs of slot,
//! dynamic-area, sparse and pool calls with hostile arguments, under a
//! kernel whose handlers answer at random and sometimes call back into the
//! memory calls themselves.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every page in use is accounted for: the fixed ones, application
//!   space's, each area's and each pool's, and the free pool is the ceiling
//!   less those;
//! * every page an area reports mapped reads back at its address, in both
//!   copies of a doubly mapped one.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::bus::{Bus, Width};
use tairix_riscos_kernel::memory::layout::{RISC_OS_5, SHARED};
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::{
    AreaFlags, Bits, Create, Granted, Guest, GuestError, Handler, LogicalOp, Memory, PageEntry,
    PageQuery, PhysicalOp, Service, Slot, FREE_POOL,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 150;
/// Calls in one run.
const CALLS: usize = 160;
const PAGE: u32 = 0x1000;

/// A kernel whose every answer is drawn, and which sometimes makes a memory
/// call of its own from inside a handler.
struct Hostile {
    rng: Prng,
    depth: u32,
    areas: Vec<u32>,
}

impl Hostile {
    fn chance(&mut self, one_in: usize) -> bool {
        self.rng.below(one_in) == 0
    }

    /// From inside a handler, a call that changes memory, as a handler may
    /// make.
    fn meddle(&mut self, memory: &mut Memory) {
        if self.depth > 0 || !self.chance(6) || self.areas.is_empty() {
            return;
        }
        self.depth += 1;
        let area = *self.rng.pick(&self.areas);
        let bytes = drawn_change(&mut self.rng);
        let _ = memory.change_area(i32::try_from(area).unwrap_or(-1), bytes, self);
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
    ) -> Result<Granted, Option<GuestError>> {
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
        Ok(Granted {
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

    fn test_shrink(&mut self, _: &mut Memory, _: Handler, _: u32) -> u32 {
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

    fn service(&mut self, _: &mut Memory, _: Service<'_>) -> bool {
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
    let privilege = u32::try_from(rng.below(11)).unwrap_or(0);
    *rng.pick(&choices) | if rng.below(4) == 0 { privilege } else { 0 }
}

/// Every page in use and every area's mapping, as the invariants require.
fn check(memory: &mut Memory, fixed: u32, slots: &[Slot], areas: &[u32], step: usize) {
    let mut accounted = fixed;
    for &slot in slots {
        accounted += memory.slot_size(slot).unwrap_or(0) / PAGE;
    }
    let mut mapped = Vec::new();
    for &number in areas {
        let Ok(info) = memory.area_info(number) else {
            continue;
        };
        let flags = AreaFlags(info.flags);
        match info.pool {
            Some((pages, _)) => accounted += pages,
            None => accounted += info.size / PAGE,
        }
        if info.pool.is_none() && flags.0 & AreaFlags::SPARSE == 0 {
            for page in 0..info.size / PAGE {
                mapped.push(info.base + page * PAGE);
                if flags.0 & AreaFlags::DOUBLY_MAPPED != 0 {
                    mapped.push(info.base - info.size + page * PAGE);
                }
            }
        }
    }
    let used = memory.mmu().used();
    assert_eq!(used, accounted, "step {step}: pages in use");
    assert_eq!(
        memory.free_pages(),
        u32::try_from(memory.ram() / u64::from(PAGE))
            .unwrap_or(0)
            .saturating_sub(used),
        "step {step}: the free pool"
    );
    for address in mapped {
        assert!(
            memory.mmu().read(address, Width::Word, true).is_ok(),
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
    let usable = (16 + rng.below(48)) as u64 * 1024 * 1024;
    let mut memory = Memory::new(layout, Ceiling::derive(usable)).expect("a session");
    let fixed = memory.mmu().used();
    let mut guest = Hostile {
        rng: Prng::new(rng.next_u64()),
        depth: 0,
        areas: Vec::new(),
    };
    let mut slots = Vec::new();
    let mut areas: Vec<u32> = Vec::new();
    for step in 0..CALLS {
        guest.areas.clone_from(&areas);
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
                let flags = drawn_flags(rng);
                let pool = flags & AreaFlags::PMP != 0;
                let request = Create {
                    number: None,
                    size: if pool {
                        0
                    } else {
                        u32::try_from(rng.below(24)).unwrap_or(0) * PAGE
                    },
                    base: None,
                    flags,
                    max: Some(u32::try_from(rng.below(64)).unwrap_or(0) * PAGE),
                    handler: (rng.below(5) != 0).then_some(0x1234),
                    workspace: None,
                    title: Some(b"Fuzz"),
                    pool: u32::try_from(rng.below(8)).unwrap_or(0),
                    caller: if rng.below(2) == 0 {
                        Bits::Bit26
                    } else {
                        Bits::Bit32
                    },
                };
                if let Ok(made) = memory.create_area(&request, &mut guest) {
                    areas.push(made.number);
                }
            }
            5 | 6 if !areas.is_empty() => {
                let area = *rng.pick(&areas);
                let _ = memory.change_area(
                    i32::try_from(area).unwrap_or(-1),
                    drawn_change(rng),
                    &mut guest,
                );
            }
            7 => {
                let _ = memory.change_area(
                    i32::try_from(FREE_POOL).unwrap_or(6),
                    drawn_change(rng),
                    &mut guest,
                );
            }
            8 if !areas.is_empty() => {
                let index = rng.below(areas.len());
                if memory.remove_area(areas[index], &mut guest).is_ok() {
                    areas.remove(index);
                }
            }
            9 if !areas.is_empty() => {
                let area = *rng.pick(&areas);
                let new = 0x1000 + u32::try_from(rng.below(0x100)).unwrap_or(0);
                if memory.renumber_area(area, new, &mut guest).is_ok() {
                    for slot in &mut areas {
                        if *slot == area {
                            *slot = new;
                        }
                    }
                }
            }
            10 if !areas.is_empty() => {
                let area = *rng.pick(&areas);
                let base = memory.area_info(area).map_or(0, |info| info.base);
                let offset = rng.next_u32() % 0x5_0000;
                let size = rng.next_u32() % 0x2_0000;
                if rng.below(2) == 0 {
                    let _ = memory.sparse_claim(area, base.wrapping_add(offset), size, &mut guest);
                } else {
                    let _ =
                        memory.sparse_release(area, base.wrapping_add(offset), size, &mut guest);
                }
            }
            11 if !areas.is_empty() => {
                let area = *rng.pick(&areas);
                let ops: Vec<PhysicalOp> = (0..rng.below(4))
                    .map(|_| PhysicalOp {
                        slot: u32::try_from(rng.below(10)).unwrap_or(0),
                        page: *rng.pick(&[-1, -2, 30, 200, 599, 100_000]),
                        flags: if rng.below(2) == 0 { 1 << 15 } else { 0 },
                    })
                    .collect();
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
                let mut after = None;
                for _ in 0..64 {
                    match memory.enumerate_areas(after) {
                        Ok(Some(next)) => after = Some(next),
                        _ => break,
                    }
                }
                let _ = memory.take_area_changes();
            }
            _ => {
                let address = rng.next_u32() & !3;
                let _ = memory
                    .mmu()
                    .write(address, Width::Word, rng.next_u32(), rng.below(2) == 0);
                let _ = memory.mmu().read(address, Width::Word, true);
            }
        }
        check(&mut memory, fixed, &slots, &areas, step);
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
