use alloc::vec;
use alloc::vec::Vec;

use tairix_riscos_arm::bus::{Abort, Bus, Width};

use super::super::layout::{APPLICATION, SHARED};
use super::super::policy::Ceiling;
use super::super::tests::{shared, Call, Recorder, Refused};
use super::super::*;

const HANDLER: u32 = 0x03F0_0000;

fn request(size: u32, max: u32) -> Create<'static> {
    Create {
        number: None,
        size,
        base: None,
        flags: 0,
        max: Some(max),
        handler: Some(HANDLER),
        workspace: None,
        title: Some(b"Test"),
        pool: 0,
        caller: Bits::Bit32,
    }
}

fn word(memory: &mut Memory, address: u32) -> Result<u32, Abort> {
    memory.bus().read(address, Width::Word, true)
}

fn store(memory: &mut Memory, address: u32, value: u32) {
    memory
        .bus()
        .write(address, Width::Word, value, true)
        .unwrap();
}

#[test]
fn an_area_is_made_grown_and_answered_for() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory
        .create_area(&request(0x2000, 0x10_0000), &mut guest)
        .unwrap();
    assert_eq!(made.number, 0x100, "the first quick number");
    assert_eq!(
        made.base,
        layout::WINDOW_TOP,
        "a 32-bit caller's above 64 MiB"
    );
    assert_eq!(made.max, 0x10_0000);
    assert_eq!(
        guest.take(),
        [
            Call::PreGrow {
                bytes: 0x2000,
                size: 0,
                block: 0
            },
            Call::PostGrow {
                bytes: 0x2000,
                size: 0x2000,
                block: 0
            },
            Call::Moved {
                bytes: 0x2000,
                area: 0x100
            },
            Call::Created(0x100),
        ]
    );
    let info = memory.area_info(made.number).unwrap();
    assert_eq!(
        (info.size, info.base, info.max, info.title),
        (0x2000, made.base, 0x10_0000, &b"Test"[..])
    );
    assert_eq!(
        info.handler,
        Some(Handler {
            bits: Bits::Bit32,
            code: HANDLER,
            workspace: made.base
        }),
        "workspace defaults to the base"
    );
    store(&mut memory, made.base + 0x1FFC, 9);
    assert_eq!(word(&mut memory, made.base + 0x1FFC), Ok(9));
    assert_eq!(
        word(&mut memory, made.base + 0x2000),
        Err(Abort::Translation)
    );
    let read = memory.read_area(0x100).unwrap();
    assert_eq!(
        (read.start, read.size, read.max),
        (made.base, 0x2000, 0x10_0000)
    );
    assert_eq!(
        memory.take_area_changes(),
        (Some(0x100), 1),
        "made, the initial grow not counted"
    );
}

#[test]
fn a_26bit_callers_area_goes_into_the_window_from_its_top() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let old = Create {
        caller: Bits::Bit26,
        ..request(0, 0x10_0000)
    };
    let first = memory.create_area(&old, &mut guest).unwrap();
    assert_eq!(first.base, SHARED.rma.start - 0x10_0000);
    let second = memory.create_area(&old, &mut guest).unwrap();
    assert_eq!(second.base, first.base - 0x10_0000);
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    assert_eq!(
        memory.slot_limit(slot),
        Some(second.base - APPLICATION),
        "a 32-bit slot stops at the window's lowest area"
    );
}

#[test]
fn numbers_are_quick_first_reused_last_in_first_out_and_checked() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let none = request(0, 0);
    let a = memory.create_area(&none, &mut guest).unwrap().number;
    let b = memory.create_area(&none, &mut guest).unwrap().number;
    assert_eq!((a, b), (0x100, 0x101));
    memory.remove_area(a, &mut guest).unwrap();
    assert_eq!(
        memory.create_area(&none, &mut guest).unwrap().number,
        a,
        "the one freed first"
    );
    for (number, error) in [
        (3, AreaError::AreaAlreadyExists),
        (7, AreaError::AreaAlreadyExists),
        (0x150, AreaError::AreaAlreadyExists),
        (0x8000_0000, AreaError::BadDynamicAreaOptions),
    ] {
        let asked = Create {
            number: Some(number),
            ..none
        };
        assert_eq!(
            memory.create_area(&asked, &mut guest),
            Err(error),
            "{number:X}"
        );
    }
    let asked = Create {
        number: Some(0x1000),
        ..none
    };
    assert_eq!(
        memory.create_area(&asked, &mut guest).unwrap().number,
        0x1000
    );
    assert_eq!(
        memory.create_area(&asked, &mut guest),
        Err(AreaError::AreaAlreadyExists)
    );
    for _ in 2..0x100 {
        memory.create_area(&none, &mut guest).unwrap();
    }
    assert_eq!(
        memory.create_area(&none, &mut guest).unwrap().number,
        0x201,
        "past the quick numbers, probed from the count of those made past them"
    );
    let untitled = Create {
        title: None,
        ..none
    };
    let made = memory.create_area(&untitled, &mut guest).unwrap().number;
    assert_eq!(memory.area_info(made).unwrap().title, b"00000202");
}

#[test]
fn a_maximum_is_clamped_to_the_clamps_and_the_ram_and_rounded() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let unbounded = Create {
        max: None,
        ..request(0, 0)
    };
    let ram = u32::try_from(memory.ram()).unwrap();
    assert_eq!(memory.create_area(&unbounded, &mut guest).unwrap().max, ram);
    let old = memory.set_clamps(Clamps {
        unbounded: 0x20_0000,
        bounded: 0x30_0000,
        sparse: 5,
    });
    assert_eq!(old.sparse, 0xFFFF_F000);
    assert_eq!(
        memory
            .set_clamps(Clamps {
                unbounded: 0,
                bounded: 0,
                sparse: 0
            })
            .sparse,
        0xFFFF_F000,
        "below 1 MiB leaves it be"
    );
    assert_eq!(
        memory.create_area(&unbounded, &mut guest).unwrap().max,
        0x20_0000
    );
    assert_eq!(
        memory
            .create_area(&request(0, 0x40_0001), &mut guest)
            .unwrap()
            .max,
        0x30_0000
    );
    assert_eq!(
        memory
            .create_area(&request(0x3000, 0x1001), &mut guest)
            .unwrap()
            .max,
        0x3000,
        "the size wins"
    );
}

#[test]
fn flags_refused_as_risc_os_5_refuses_them() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let cases = [
        (
            AreaFlags::PMP,
            0,
            Some(HANDLER),
            "a pool that needs no particular pages",
        ),
        (
            AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
            0x1000,
            Some(HANDLER),
            "a pool made with a size",
        ),
        (
            AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
            0,
            None,
            "a pool with no handler",
        ),
        (
            AreaFlags::DOUBLY_MAPPED,
            0,
            Some(HANDLER),
            "cacheable, twice, and runnable",
        ),
        (AreaFlags::ABORTABLE, 0, None, "abortable with no handler"),
        (10, 0, Some(HANDLER), "no access privilege 10"),
    ];
    for (flags, size, handler, why) in cases {
        let asked = Create {
            flags,
            handler,
            ..request(size, 0x10_0000)
        };
        assert_eq!(
            memory.create_area(&asked, &mut guest),
            Err(AreaError::BadDynamicAreaOptions),
            "{why}"
        );
    }
    let twice = Create {
        flags: AreaFlags::DOUBLY_MAPPED | 5,
        ..request(0, 0x10_0000)
    };
    assert!(
        memory.create_area(&twice, &mut guest).is_ok(),
        "never run, it may be"
    );
}

#[test]
fn a_named_base_must_be_a_free_page_where_areas_go() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let at = |base| Create {
        base: Some(base),
        ..request(0, 0x10_0000)
    };
    assert_eq!(
        memory.create_area(&at(0x0800_0004), &mut guest),
        Err(AreaError::AreaNotOnPageBoundary)
    );
    assert_eq!(
        memory.create_area(&at(0x0080_0000), &mut guest),
        Err(AreaError::OverlappingAreas),
        "application space"
    );
    assert_eq!(
        memory.create_area(&at(SHARED.rma.start), &mut guest),
        Err(AreaError::OverlappingAreas),
        "the RMA"
    );
    assert_eq!(
        memory
            .create_area(&at(0x0800_0000), &mut guest)
            .unwrap()
            .base,
        0x0800_0000
    );
    assert_eq!(
        memory.create_area(&at(0x080F_F000), &mut guest),
        Err(AreaError::OverlappingAreas),
        "another area"
    );
}

#[test]
fn a_grow_is_all_or_nothing_and_a_shrink_takes_what_its_handler_allows() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let number = memory
        .create_area(&request(0x1000, 0x4000), &mut guest)
        .unwrap()
        .number;
    let signed = i32::try_from(number).unwrap();
    guest.take();
    assert_eq!(
        memory.change_area(signed, 0x4000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        }),
        "past the maximum by a page"
    );
    assert_eq!(
        memory.change_area(signed, 1, &mut guest),
        Ok(0x1000),
        "rounded up"
    );
    guest.take();
    guest.allow_shrink = Some(0x1000);
    assert_eq!(
        memory.change_area(signed, -0x2000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0x1000
        })
    );
    assert_eq!(
        guest.take(),
        [
            Call::PreShrink {
                bytes: 0x2000,
                size: 0x2000
            },
            Call::PostShrink {
                bytes: 0x1000,
                size: 0x1000
            },
            Call::Moved {
                bytes: -0x1000,
                area: signed
            },
        ]
    );
    guest.allow_shrink = Some(0);
    assert!(memory.change_area(signed, -0x1000, &mut guest).is_err());
    assert!(
        !guest
            .take()
            .iter()
            .any(|call| matches!(call, Call::PostShrink { .. })),
        "nothing moved, so no PostShrink"
    );
    guest.allow_shrink = None;
    assert_eq!(
        memory.change_area(signed, -0x1FFF, &mut guest),
        Ok(0x1000),
        "a shrink rounded toward none"
    );
    assert_eq!(
        memory.change_area(-1, 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        }),
        "-1 is not an area here"
    );
    guest.refuse_grow = Some(Refused::Silently);
    assert_eq!(
        memory.change_area(signed, 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        })
    );
    assert_eq!(
        memory.area_info(number).unwrap().size,
        0,
        "a refused grow moved nothing"
    );
}

#[test]
fn a_shrinkable_area_gives_its_memory_to_a_grow_that_needs_it() {
    let mut memory = Memory::new(&SHARED, Ceiling::derive(4 * 1024 * 1024)).unwrap();
    let mut guest = Recorder::new();
    let cache = Create {
        flags: AreaFlags::SHRINKABLE,
        handler: None,
        ..request(0x8_0000, 0x10_0000)
    };
    let cache = memory.create_area(&cache, &mut guest).unwrap().number;
    let free = memory.free_pages();
    let want = (free + 16) * 0x1000;
    let grower = memory
        .create_area(&request(0, 0x20_0000), &mut guest)
        .unwrap()
        .number;
    assert_eq!(
        memory.change_area(
            i32::try_from(grower).unwrap(),
            i32::try_from(want).unwrap(),
            &mut guest
        ),
        Ok(want)
    );
    assert_eq!(
        memory.area_info(cache).unwrap().size,
        0x8_0000 - 16 * 0x1000,
        "it gave what was short"
    );
    guest.test_shrink = 0x3000;
    assert_eq!(
        memory.free_area_pages(None, &mut guest),
        Ok(memory.free_pages() + 0x70),
        "no handler: all of it may go"
    );
}

#[test]
fn the_free_pool_moves_memory_to_and_from_application_space() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    memory.switch_slot(slot).unwrap();
    assert_eq!(memory.change_area(6, -0x3000, &mut guest), Ok(0x3000));
    assert_eq!(memory.slot_size(slot), Some(0x3000));
    assert_eq!(
        guest.take(),
        [
            Call::Memory { change: 0x3000 },
            Call::Moved {
                bytes: 0x3000,
                area: -1
            }
        ]
    );
    assert_eq!(memory.change_area(6, 0x1000, &mut guest), Ok(0x1000));
    assert_eq!(memory.slot_size(slot), Some(0x2000));
    let read = memory.read_area(-1).unwrap();
    assert_eq!((read.start, read.size), (APPLICATION, 0x2000));
    guest.take();
    guest.claim_service = true;
    assert_eq!(
        memory.change_area(6, 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::ApplicationInUse,
            moved: 0
        })
    );
    guest.claim_service = false;
    guest.active = APPLICATION + 0x100;
    guest.claim_upcall = false;
    assert_eq!(
        memory.change_area(6, 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::Application,
            moved: 0
        }),
        "the current active object is in application space"
    );
    guest.claim_upcall = true;
    assert_eq!(
        memory.change_area(6, 0x1000, &mut guest),
        Ok(0x1000),
        "unless the upcall is claimed"
    );
}

#[test]
fn a_handler_names_its_pages_and_another_holders_are_replaced_with_their_contents() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let other = memory
        .create_area(&request(0x1000, 0x1000), &mut guest)
        .unwrap();
    store(&mut memory, other.base, 0xFEED);
    let (frame, _) = memory
        .mmu
        .lookup(tairix_riscos_arm::mmu::Table::Global, other.base)
        .unwrap();
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&specific, &mut guest).unwrap();
    guest.take();
    guest.name_pages = Vec::from([frame.number()]);
    assert_eq!(
        memory.change_area(i32::try_from(made.number).unwrap(), 0x1000, &mut guest),
        Ok(0x1000)
    );
    let (taken, _) = memory
        .mmu
        .lookup(tairix_riscos_arm::mmu::Table::Global, made.base)
        .unwrap();
    assert_eq!(taken, frame, "the page it named");
    assert_eq!(
        word(&mut memory, other.base),
        Ok(0xFEED),
        "the other area kept its contents on a replacement"
    );
    let calls = guest.take();
    assert!(
        calls.contains(&Call::Unsafe(1)) && calls.contains(&Call::Safe(1, 1)),
        "{calls:?}"
    );
    let third = memory.create_area(&specific, &mut guest).unwrap();
    assert_eq!(
        memory.change_area(i32::try_from(third.number).unwrap(), 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::CantGetPhysicalMemory,
            moved: 0
        }),
        "now unavailable"
    );
    guest.name_pages = Vec::from([0]);
    assert_eq!(
        memory.change_area(i32::try_from(third.number).unwrap(), 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::CantGetPhysicalMemory,
            moved: 0
        }),
        "a kernel page"
    );
}

#[test]
fn a_grow_naming_pages_asks_in_chunks_of_63() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(0, 0x20_0000)
    };
    let made = memory.create_area(&specific, &mut guest).unwrap();
    guest.take();
    assert_eq!(
        memory.change_area(
            i32::try_from(made.number).unwrap(),
            100 * 0x1000,
            &mut guest
        ),
        Ok(100 * 0x1000)
    );
    let grows: Vec<Call> = guest
        .take()
        .into_iter()
        .filter(|call| matches!(call, Call::PreGrow { .. }))
        .collect();
    assert_eq!(
        grows,
        [
            Call::PreGrow {
                bytes: 63 * 0x1000,
                size: 0,
                block: 63
            },
            Call::PreGrow {
                bytes: 37 * 0x1000,
                size: 63 * 0x1000,
                block: 37
            },
        ]
    );
}

#[test]
fn a_doubly_mapped_area_shows_each_page_twice_and_its_first_copy_moves_down() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let twice = Create {
        flags: AreaFlags::DOUBLY_MAPPED | AreaFlags::NOT_CACHEABLE,
        ..request(0x2000, 0x10_0000)
    };
    let made = memory.create_area(&twice, &mut guest).unwrap();
    store(&mut memory, made.base + 0x1000, 0x2B);
    assert_eq!(
        word(&mut memory, made.base - 0x1000),
        Ok(0x2B),
        "page one in both copies"
    );
    let read = memory
        .read_area(i32::try_from(made.number).unwrap())
        .unwrap();
    assert_eq!(read.start, made.base - 0x2000, "the first copy's start");
    memory
        .change_area(i32::try_from(made.number).unwrap(), 0x1000, &mut guest)
        .unwrap();
    assert_eq!(
        word(&mut memory, made.base + 0x1000),
        Ok(0x2B),
        "the second copy stays put"
    );
    assert_eq!(
        word(&mut memory, made.base - 0x2000),
        Ok(0x2B),
        "the first moved down a page"
    );
    memory
        .change_area(i32::try_from(made.number).unwrap(), -0x2000, &mut guest)
        .unwrap();
    assert_eq!(
        word(&mut memory, made.base - 0x1000),
        Ok(0),
        "page zero, moved back up"
    );
    assert_eq!(
        word(&mut memory, made.base + 0x1000),
        Err(Abort::Translation)
    );
}

#[test]
fn areas_enumerate_by_title_case_blind_the_newest_first_among_equals() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let titled = |title: &'static [u8]| Create {
        title: Some(title),
        ..request(0, 0)
    };
    let zeta = memory
        .create_area(&titled(b"zeta"), &mut guest)
        .unwrap()
        .number;
    let alpha = memory
        .create_area(&titled(b"Alpha"), &mut guest)
        .unwrap()
        .number;
    let newer = memory
        .create_area(&titled(b"ALPHA"), &mut guest)
        .unwrap()
        .number;
    let mut order = Vec::new();
    let mut after = None;
    while let Some(next) = memory.enumerate_areas(after).unwrap() {
        order.push(next);
        after = Some(next);
    }
    assert_eq!(order, [newer, alpha, FREE_POOL, zeta]);
    assert_eq!(
        memory.enumerate_areas(Some(0x777)),
        Err(AreaError::BadDynamicArea)
    );
    let info = memory.enumerate_area_info(Some(alpha)).unwrap().unwrap();
    assert_eq!(
        info.number, zeta,
        "the free pool is the system's, so skipped"
    );
    let pool = memory.area_info(FREE_POOL).unwrap();
    assert_eq!(pool.pool, Some((memory.free_pages(), 16384)));
    assert_eq!(pool.flags, 0x0010_0032);
}

#[test]
fn renumbering_and_removing_follow_risc_os_and_refuse_its_unguarded_cases() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory
        .create_area(&request(0x1000, 0x1000), &mut guest)
        .unwrap();
    let explicit = Create {
        number: Some(0x300),
        ..request(0, 0)
    };
    memory.create_area(&explicit, &mut guest).unwrap();
    assert_eq!(
        memory.renumber_area(made.number, 0x400, &mut guest),
        Err(AreaError::IllegalRenumber),
        "a quick number"
    );
    assert_eq!(
        memory.renumber_area(0x300, 5, &mut guest),
        Err(AreaError::IllegalRenumber),
        "onto a system number"
    );
    assert_eq!(
        memory.renumber_area(0x300, 0x150, &mut guest),
        Err(AreaError::IllegalRenumber)
    );
    assert_eq!(
        memory.renumber_area(0x999, 0x400, &mut guest),
        Err(AreaError::BadDynamicArea)
    );
    let other = Create {
        number: Some(0x400),
        ..request(0, 0)
    };
    memory.create_area(&other, &mut guest).unwrap();
    assert_eq!(
        memory.renumber_area(0x300, 0x400, &mut guest),
        Err(AreaError::AreaAlreadyExists)
    );
    memory.take_area_changes();
    guest.take();
    memory.renumber_area(0x300, 0x500, &mut guest).unwrap();
    assert_eq!(guest.take(), [Call::Renumbered(0x300, 0x500)]);
    assert_eq!(
        memory.take_area_changes(),
        (Some(0x300), 8),
        "recorded under the old number"
    );
    assert!(memory.area_info(0x500).is_ok() && memory.area_info(0x300).is_err());
    assert_eq!(
        memory.remove_area(1, &mut guest),
        Err(AreaError::BadDynamicArea),
        "the RMA's number"
    );
    let free = memory.free_pages();
    memory.remove_area(made.number, &mut guest).unwrap();
    assert_eq!(memory.free_pages(), free + 1);
    assert_eq!(word(&mut memory, made.base), Err(Abort::Translation));
    assert!(guest.take().contains(&Call::Removed(made.number)));
    let (only, changes) = memory.take_area_changes();
    assert_eq!(
        (only, changes),
        (Some(made.number), 2),
        "the final shrink not counted"
    );
}

#[test]
fn locate_finds_the_regions_first_then_an_areas_reservation() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory
        .create_area(&request(0, 0x10_0000), &mut guest)
        .unwrap();
    assert_eq!(
        memory.locate(made.base + 0xF_FFFF),
        Ok(Located::Area(made.number)),
        "reserved, not mapped"
    );
    assert_eq!(
        memory.locate(SHARED.stacks.supervisor.start),
        Ok(Located::Region(3))
    );
    assert_eq!(memory.locate(0x4000), Ok(Located::Region(15)));
    assert_eq!(
        memory.locate(0x0001_0000),
        Err(AreaError::BadAddress),
        "application space is never found"
    );
    assert_eq!(
        memory.locate(SHARED.rma.start),
        Err(AreaError::BadAddress),
        "nor the kernel's parts"
    );
}

#[test]
fn a_sparse_area_maps_runs_where_they_are_claimed() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let sparse = Create {
        flags: AreaFlags::SPARSE,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&sparse, &mut guest).unwrap();
    let info = memory.area_info(made.number).unwrap();
    assert!(info.handler.is_none() && info.flags & AreaFlags::NOT_USER_DRAGGABLE != 0);
    guest.take();
    memory
        .sparse_claim(made.number, made.base + 0x3001, 0x1FFF, &mut guest)
        .unwrap();
    assert_eq!(
        word(&mut memory, made.base + 0x3000),
        Ok(0),
        "widened to whole pages"
    );
    assert_eq!(word(&mut memory, made.base + 0x4000), Ok(0));
    assert_eq!(
        word(&mut memory, made.base + 0x5000),
        Err(Abort::Translation)
    );
    assert_eq!(
        guest.take(),
        [
            Call::Moved {
                bytes: 0x2000,
                area: i32::try_from(made.number).unwrap()
            },
            Call::Moved {
                bytes: 0,
                area: i32::try_from(made.number).unwrap()
            }
        ]
    );
    memory
        .sparse_release(made.number, made.base + 0x3001, 0x2000, &mut guest)
        .unwrap();
    assert_eq!(
        word(&mut memory, made.base + 0x3000),
        Ok(0),
        "narrowed to whole pages: the first stays"
    );
    assert_eq!(
        word(&mut memory, made.base + 0x4000),
        Err(Abort::Translation)
    );
    assert_eq!(
        memory.sparse_claim(made.number, made.base + 0x10_0000, 0x1000, &mut guest),
        Err(AreaError::InvalidSparse),
        "past its maximum"
    );
    let plain = memory.create_area(&request(0, 0x1000), &mut guest).unwrap();
    assert_eq!(
        memory.sparse_claim(plain.number, plain.base, 0x1000, &mut guest),
        Err(AreaError::InvalidSparse)
    );
    assert_eq!(
        memory.change_area(i32::try_from(made.number).unwrap(), 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        }),
        "a sparse area is not resized"
    );
}

/// A pool of four slots and four pages' window.
fn pool(memory: &mut Memory, guest: &mut Recorder) -> Created {
    let pool = Create {
        flags: AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
        pool: 4,
        ..request(0, 0x4000)
    };
    memory.create_area(&pool, guest).unwrap()
}

const fn physical(slot: u32, page: i32, flags: u32) -> PhysicalOp {
    PhysicalOp { slot, page, flags }
}

const fn logical(window: u32, slot: Option<u32>, flags: u32) -> LogicalOp {
    LogicalOp {
        window,
        slot,
        flags,
    }
}

#[test]
fn a_pool_claims_maps_and_releases_its_own_pages() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = pool(&mut memory, &mut guest);
    let free = memory.free_pages();
    let ops = [physical(0, -2, 0), physical(1, 100, 1 << 15)];
    memory.pool_physical(made.number, &ops, &mut guest).unwrap();
    assert_eq!(memory.free_pages(), free - 2);
    assert_eq!(memory.area_info(made.number).unwrap().pool, Some((2, 4)));
    let past = [physical(4, -2, 0)];
    assert_eq!(
        memory.pool_physical(made.number, &past, &mut guest),
        Err((AreaError::BadPageNumber, 0))
    );
    memory
        .pool_logical(made.number, &[logical(3, Some(1), 0)])
        .unwrap();
    store(&mut memory, made.base + 0x3000, 0x77);
    memory
        .pool_logical(made.number, &[logical(0, Some(1), 1 << 15)])
        .unwrap();
    assert_eq!(
        word(&mut memory, made.base),
        Ok(0x77),
        "moved, never mapped twice"
    );
    assert_eq!(
        word(&mut memory, made.base + 0x3000),
        Err(Abort::Translation)
    );
    let mut queries = [PageQuery {
        page: Some(100),
        ..PageQuery::default()
    }];
    memory.pool_query(made.number, &mut queries).unwrap();
    assert_eq!(
        (queries[0].slot, queries[0].window, queries[0].flags),
        (Some(1), Some(0), 1 << 15)
    );
    let release = [physical(1, -1, 0)];
    assert_eq!(
        memory.pool_physical(made.number, &release, &mut guest),
        Err((AreaError::NotAllMoved, 0)),
        "a mapped page is not released"
    );
    memory
        .pool_logical(made.number, &[logical(0, None, 0)])
        .unwrap();
    memory
        .pool_physical(made.number, &release, &mut guest)
        .unwrap();
    assert_eq!(memory.free_pages(), free - 1);
}

#[test]
fn a_pools_page_list_resizes_and_its_change_and_removal_go_to_its_handler() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = pool(&mut memory, &mut guest);
    let free = memory.free_pages();
    memory
        .pool_physical(made.number, &[physical(0, -2, 0)], &mut guest)
        .unwrap();
    assert_eq!(memory.free_pages(), free - 1);
    assert_eq!(
        memory.pool_resize(made.number, -3),
        Ok(3),
        "the empty slots from the end"
    );
    assert_eq!(
        memory.pool_resize(made.number, -1),
        Ok(0),
        "stopping at a filled one"
    );
    assert_eq!(memory.pool_resize(made.number, 2), Ok(2));
    guest.take();
    memory
        .change_area(i32::try_from(made.number).unwrap(), 0x2000, &mut guest)
        .unwrap();
    assert_eq!(
        guest.take(),
        [Call::ResizePmp {
            pages: 2,
            area: i32::try_from(made.number).unwrap()
        }]
    );
    memory.remove_area(made.number, &mut guest).unwrap();
    assert_eq!(memory.free_pages(), free, "its last page released");
}

#[test]
fn a_change_made_from_an_interrupt_or_inside_another_is_refused() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory.create_area(&request(0, 0x1000), &mut guest).unwrap();
    guest.interrupt = true;
    assert_eq!(
        memory.change_area(i32::try_from(made.number).unwrap(), 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        })
    );
}

#[test]
fn an_area_its_handler_grows_again_while_it_is_removed_stays_whole() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory
        .create_area(&request(0x2000, 0x10_0000), &mut guest)
        .unwrap();
    let free = memory.free_pages();
    guest.regrow = Some((i32::try_from(made.number).unwrap(), 0x1000));
    assert_eq!(
        memory.remove_area(made.number, &mut guest),
        Err(AreaError::NotAllMoved)
    );
    assert_eq!(
        memory.area_info(made.number).unwrap().size,
        0x1000,
        "its page is still its own"
    );
    assert_eq!(memory.free_pages(), free + 1);
    memory.remove_area(made.number, &mut guest).unwrap();
    assert_eq!(memory.free_pages(), free + 2, "nothing left behind");
}

#[test]
fn nothing_resizes_an_area_while_a_grow_handler_runs() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let victim = memory
        .create_area(&request(0x1000, 0x1000), &mut guest)
        .unwrap();
    let grower = memory.create_area(&request(0, 0x1000), &mut guest).unwrap();
    guest.remove_inside = Some(victim.number);
    assert_eq!(
        memory.change_area(i32::try_from(grower.number).unwrap(), 0x1000, &mut guest),
        Ok(0x1000)
    );
    assert_eq!(guest.nested, [Err(AreaError::NotAllMoved)]);
    assert_eq!(memory.area_info(victim.number).unwrap().size, 0x1000);
    guest.remove_inside = None;
    let sparse = Create {
        flags: AreaFlags::SPARSE,
        ..request(0, 0x1000)
    };
    let sparse = memory.create_area(&sparse, &mut guest).unwrap();
    memory.areas.resizing = true;
    assert_eq!(
        memory.sparse_claim(sparse.number, sparse.base, 0x1000, &mut guest),
        Err(AreaError::NotAllMoved)
    );
    assert_eq!(
        memory.create_area(&request(0x1000, 0x1000), &mut guest),
        Err(AreaError::NotAllMoved)
    );
    memory.areas.resizing = false;
}

#[test]
fn a_refused_initial_grow_frees_what_it_took_and_asks_no_handler_again() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let free = memory.free_pages();
    guest.refuse_after = Some(1);
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(100 * 0x1000, 0x20_0000)
    };
    assert_eq!(
        memory.create_area(&specific, &mut guest),
        Err(AreaError::NotAllMoved)
    );
    let calls = guest.take();
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::PreGrow { .. }))
            .count(),
        2,
        "one chunk granted, the next refused"
    );
    assert!(!calls
        .iter()
        .any(|call| matches!(call, Call::PreShrink { .. } | Call::PostShrink { .. })));
    assert_eq!(memory.free_pages(), free, "the granted chunk's pages freed");
    assert_eq!(
        memory.enumerate_areas(None),
        Ok(Some(FREE_POOL)),
        "and no area left"
    );
}

#[test]
fn risc_os_5s_layout_places_areas_past_its_heaps_or_below_a_lowered_limit() {
    let mut memory = Memory::new(&layout::RISC_OS_5, Ceiling::derive(1024 * 1024 * 1024)).unwrap();
    let mut guest = Recorder::new();
    let first = memory
        .create_area(&request(0, 0x10_0000), &mut guest)
        .unwrap();
    assert_eq!(
        first.base,
        layout::RISC_OS_5.system_heap.end,
        "past the RMA and system heap"
    );
    memory.set_application_limit(0x0800_0000).unwrap();
    let below = memory
        .create_area(&request(0, 0x10_0000), &mut guest)
        .unwrap();
    assert_eq!(
        below.base, 0x0800_0000,
        "between the lowered limit and the RMA"
    );
}

/// A session whose free pool runs short: a quarter of 4 MiB.
fn small() -> Memory {
    Memory::new(&SHARED, Ceiling::derive(4 * 1024 * 1024)).unwrap()
}

fn named(number: u32) -> i32 {
    i32::try_from(number).unwrap()
}

/// A shrinkable area with a handler.
fn cache(size: u32) -> Create<'static> {
    Create {
        flags: AreaFlags::SHRINKABLE,
        ..request(size, 0x10_0000)
    }
}

fn pages_of(memory: &Memory, number: u32) -> u32 {
    memory.area_info(number).unwrap().size / 0x1000
}

/// Every page the areas account for: a pool's held pages, any other's
/// mapped ones.
fn held_by_areas(memory: &Memory) -> u32 {
    let mut held = 0;
    let mut after = None;
    while let Ok(Some(number)) = memory.enumerate_areas(after) {
        after = Some(number);
        if number != FREE_POOL {
            let info = memory.area_info(number).unwrap();
            held += info.pool.map_or(info.size / 0x1000, |(pages, _)| pages);
        }
    }
    held
}

fn frame_at(memory: &Memory, address: u32) -> Option<u32> {
    memory
        .mmu
        .lookup(Table::Global, address)
        .map(|(frame, _)| frame.number())
}

#[test]
fn an_initial_size_past_any_maximum_is_refused_before_anything_is_reserved() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let free = memory.free_pages();
    for (size, max, flags) in [
        (u32::MAX, None, 0),
        (0xFFFF_F001, Some(0x10_0000), 0),
        (0x0410_0000, None, 0),
        (u32::MAX, None, AreaFlags::SPARSE),
    ] {
        let huge = Create {
            flags,
            max,
            ..request(size, 0)
        };
        assert_eq!(
            memory.create_area(&huge, &mut guest),
            Err(AreaError::NotAllMoved),
            "{size:#x}"
        );
    }
    assert!(guest.take().is_empty(), "no handler or service asked");
    assert_eq!(memory.free_pages(), free);
    assert_eq!(memory.enumerate_areas(None), Ok(Some(FREE_POOL)));
    let made = memory.create_area(&request(0, 0x1000), &mut guest).unwrap();
    assert_eq!(
        (made.number, made.base),
        (0x100, layout::WINDOW_TOP),
        "no number or address space kept"
    );
}

#[test]
fn a_grow_is_held_to_its_maximum_however_its_room_was_made() {
    let mut memory = small();
    let mut guest = Recorder::new();
    memory.create_area(&cache(0x4_0000), &mut guest).unwrap();
    memory.create_area(&cache(0x4_0000), &mut guest).unwrap();
    let max = (memory.free_pages() + 16) * 0x1000;
    let grower = memory.create_area(&request(0, max), &mut guest).unwrap();
    guest.regrow = Some((named(grower.number), 0x1000));
    assert_eq!(
        memory.change_area(named(grower.number), named(max), &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        })
    );
    assert!(guest.regrow.is_none(), "a cache's handler grew it first");
    assert_eq!(pages_of(&memory, grower.number), 1, "by that page alone");
}

#[test]
fn a_sparse_claim_maps_around_a_page_claimed_while_it_made_room() {
    let mut memory = small();
    let mut guest = Recorder::new();
    let kernel = memory.pages_in_use();
    memory.create_area(&cache(0x4_0000), &mut guest).unwrap();
    memory.create_area(&cache(0x4_0000), &mut guest).unwrap();
    let sparse = Create {
        flags: AreaFlags::SPARSE,
        ..request(0, 0x10_0000)
    };
    let sparse = memory.create_area(&sparse, &mut guest).unwrap();
    let filler = (memory.free_pages() - 2) * 0x1000;
    memory
        .create_area(&request(filler, filler), &mut guest)
        .unwrap();
    guest.claim_inside = Some((sparse.number, sparse.base + 0x2000, 0x1000));
    memory
        .sparse_claim(sparse.number, sparse.base, 0x4000, &mut guest)
        .unwrap();
    assert!(guest.claim_inside.is_none(), "a cache's handler claimed it");
    assert_eq!(pages_of(&memory, sparse.number), 4);
    assert_eq!(
        memory.pages_in_use(),
        kernel + held_by_areas(&memory),
        "no page lost"
    );
}

#[test]
fn an_area_its_handler_removes_and_replaces_leaves_the_replacement_whole() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let free = memory.free_pages();
    let made = memory
        .create_area(&request(0x2000, 0x10_0000), &mut guest)
        .unwrap();
    guest.replace_inside = Some(made.number);
    assert_eq!(memory.remove_area(made.number, &mut guest), Ok(()));
    let pool = guest.replacement.unwrap();
    assert_eq!(pool, made.number, "its number given out again");
    memory
        .pool_physical(pool, &[physical(0, -2, 0)], &mut guest)
        .unwrap();
    assert_eq!(memory.area_info(pool).unwrap().pool, Some((1, 4)));
    memory.remove_area(pool, &mut guest).unwrap();
    assert_eq!(memory.free_pages(), free, "every page back");
}

#[test]
fn a_pool_list_of_any_length_needs_room_only_for_its_empty_slots() {
    let mut memory = small();
    let mut guest = Recorder::new();
    let made = pool(&mut memory, &mut guest);
    let free = memory.free_pages();
    let ops = vec![physical(0, -2, 0); (1 << 20) + 1];
    assert_eq!(memory.pool_physical(made.number, &ops, &mut guest), Ok(()));
    assert_eq!(memory.free_pages(), free - 1, "one page, kept thereafter");
}

#[test]
fn a_refused_pool_claim_keeps_the_slots_page() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = pool(&mut memory, &mut guest);
    memory
        .pool_physical(made.number, &[physical(0, -2, 0)], &mut guest)
        .unwrap();
    let mut held = [PageQuery {
        slot: Some(0),
        ..PageQuery::default()
    }];
    memory.pool_query(made.number, &mut held).unwrap();
    let free = memory.free_pages();
    for (page, refusal) in [(-3, AreaError::BadPageNumber), (0, AreaError::NotAllMoved)] {
        assert_eq!(
            memory.pool_physical(made.number, &[physical(0, page, 0)], &mut guest),
            Err((refusal, 0)),
            "page {page}"
        );
    }
    let mut now = [PageQuery {
        slot: Some(0),
        ..PageQuery::default()
    }];
    memory.pool_query(made.number, &mut now).unwrap();
    assert_eq!(now[0].page, held[0].page, "still its page");
    assert_eq!(memory.free_pages(), free);
}

#[test]
fn a_sparse_area_asked_to_be_doubly_mapped_is_mapped_once_and_gives_its_space_back() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let both = Create {
        flags: AreaFlags::SPARSE | AreaFlags::DOUBLY_MAPPED | AreaFlags::NOT_CACHEABLE,
        ..request(0, 0x10_0000)
    };
    let first = memory.create_area(&both, &mut guest).unwrap();
    let flags = memory.area_info(first.number).unwrap().flags;
    assert_eq!(flags & AreaFlags::DOUBLY_MAPPED, 0);
    assert_eq!(first.base, layout::WINDOW_TOP, "one copy's space reserved");
    memory.remove_area(first.number, &mut guest).unwrap();
    let again = memory.create_area(&both, &mut guest).unwrap();
    assert_eq!(again.base, first.base, "and all of it given back");
}

#[test]
fn a_pool_is_never_shrinkable() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let asked = Create {
        flags: AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES | AreaFlags::SHRINKABLE,
        pool: 4,
        ..request(0, 0x4000)
    };
    let made = memory.create_area(&asked, &mut guest).unwrap();
    let flags = memory.area_info(made.number).unwrap().flags;
    assert_eq!(flags & AreaFlags::SHRINKABLE, 0);
    memory
        .pool_physical(made.number, &[physical(0, -2, 0)], &mut guest)
        .unwrap();
    memory
        .pool_logical(made.number, &[logical(0, Some(0), 0)])
        .unwrap();
    guest.take();
    let free = memory.free_pages();
    assert_eq!(
        memory.free_area_pages(None, &mut guest),
        Ok(free),
        "its mapped page is not there to take"
    );
    assert!(guest.take().is_empty(), "nor is its handler asked");
}

#[test]
fn a_walk_of_the_shrinkable_areas_ends_however_many_their_handlers_make() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    memory.create_area(&cache(0x2000), &mut guest).unwrap();
    guest.take();
    guest.breed = true;
    guest.test_shrink = 0x1000;
    let free = memory.free_pages();
    assert_eq!(memory.free_area_pages(None, &mut guest), Ok(free + 1));
    assert_eq!(
        memory.areas.count(),
        2,
        "only the area there when it began was asked"
    );
    let mut memory = small();
    let mut guest = Recorder::new();
    memory.create_area(&cache(0x4_0000), &mut guest).unwrap();
    let want = (memory.free_pages() + 0x40) * 0x1000;
    let grower = memory.create_area(&request(0, want), &mut guest).unwrap();
    guest.take();
    guest.breed = true;
    assert_eq!(
        memory.change_area(named(grower.number), named(want), &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        }),
        "the page its child took is short"
    );
    assert_eq!(
        memory.areas.count(),
        3,
        "only the area there when it began was shrunk"
    );
}

#[test]
fn room_application_space_cannot_cover_is_not_taken_from_it() {
    let mut memory = small();
    let mut guest = Recorder::new();
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    memory.switch_slot(slot).unwrap();
    memory
        .change_area(named(FREE_POOL), -0x2000, &mut guest)
        .unwrap();
    let sparse = Create {
        flags: AreaFlags::SPARSE,
        ..request(0, 0x10_0000)
    };
    let sparse = memory.create_area(&sparse, &mut guest).unwrap();
    let made = pool(&mut memory, &mut guest);
    let grower = memory.create_area(&request(0, 0x4000), &mut guest).unwrap();
    let filler = (memory.free_pages() - 1) * 0x1000;
    memory
        .create_area(&request(filler, filler), &mut guest)
        .unwrap();
    assert_eq!(
        memory.sparse_claim(sparse.number, sparse.base, 0x4000, &mut guest),
        Err(AreaError::NotAllMoved)
    );
    let claims = [0, 1, 2, 3].map(|slot| physical(slot, -2, 0));
    assert_eq!(
        memory.pool_physical(made.number, &claims, &mut guest),
        Err((AreaError::NotAllMoved, 0))
    );
    assert_eq!(
        memory.change_area(named(grower.number), 0x4000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        })
    );
    assert_eq!(
        memory.slot_size(slot),
        Some(0x2000),
        "two pages short of four"
    );
}

#[test]
fn a_slot_grow_past_what_the_slot_may_reach_shrinks_nothing() {
    let mut memory = small();
    let mut guest = Recorder::new();
    let slot = memory.create_slot(Bits::Bit26).unwrap();
    memory.switch_slot(slot).unwrap();
    let unasked = Create {
        handler: None,
        ..cache(0x8_0000)
    };
    let cached = memory.create_area(&unasked, &mut guest).unwrap();
    assert_eq!(
        memory.change_area(named(FREE_POOL), -0x01C0_0000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        }),
        "a 26-bit slot's 28 MiB less its first 32 KiB"
    );
    assert_eq!(pages_of(&memory, cached.number), 0x80);
}

#[test]
fn a_grow_refused_by_application_space_says_why() {
    let mut memory = small();
    let mut guest = Recorder::new();
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    memory.switch_slot(slot).unwrap();
    memory
        .change_area(named(FREE_POOL), -0x4000, &mut guest)
        .unwrap();
    let want = (memory.free_pages() + 1) * 0x1000;
    let grower = memory.create_area(&request(0, want), &mut guest).unwrap();
    guest.claim_service = true;
    assert_eq!(
        memory.change_area(named(grower.number), named(want), &mut guest),
        Err(Failure {
            error: AreaError::ApplicationInUse,
            moved: 0
        })
    );
    assert_eq!(memory.slot_size(slot), Some(0x4000));
}

#[test]
fn only_pages_another_holds_are_announced_each_paired_with_its_replacement() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let other = memory
        .create_area(&request(0x2000, 0x2000), &mut guest)
        .unwrap();
    let held = [other.base, other.base + 0x1000].map(|address| frame_at(&memory, address).unwrap());
    let free = memory.mmu.next_free(Frame::new(0)).unwrap().number();
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&specific, &mut guest).unwrap();
    guest.name_pages = Vec::from([held[1], free, held[0]]);
    assert_eq!(
        memory.change_area(named(made.number), 0x3000, &mut guest),
        Ok(0x3000)
    );
    let entry = |page: u32, logical: u32| PageEntry {
        page: Some(page),
        logical,
        physical: page * 0x1000,
    };
    let before = Vec::from([
        entry(held[1], other.base + 0x1000),
        entry(held[0], other.base),
    ]);
    assert_eq!(guest.announced, core::slice::from_ref(&before));
    assert_eq!(guest.swapped.len(), 1);
    let (old, new) = &guest.swapped[0];
    assert_eq!(old, &before);
    assert_eq!(new.len(), 2);
    for (old, new) in old.iter().zip(new) {
        assert_eq!(new.logical, old.logical, "where the page was");
        assert_eq!(frame_at(&memory, new.logical), new.page);
        assert!(!guest.name_pages.contains(&new.page.unwrap()), "none named");
    }
    let mine = [0, 0x1000, 0x2000].map(|offset| frame_at(&memory, made.base + offset).unwrap());
    assert_eq!(mine, [held[1], free, held[0]], "each where it was named");
}

#[test]
fn a_grow_whose_mapping_fails_leaves_the_area_and_the_free_pool_as_they_were() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory
        .create_area(&request(0x1000, 0x10_0000), &mut guest)
        .unwrap();
    store(&mut memory, made.base, 0x55);
    let squatter = memory.mmu.allocate(FrameKind::Memory).unwrap();
    memory
        .mmu
        .map(
            Table::Global,
            made.base + 0x2000,
            squatter,
            made_access(&memory, made.base),
        )
        .unwrap();
    let free = memory.free_pages();
    assert_eq!(
        memory.change_area(named(made.number), 0x3000, &mut guest),
        Err(Failure {
            error: AreaError::NotAllMoved,
            moved: 0
        })
    );
    assert_eq!(pages_of(&memory, made.number), 1);
    assert_eq!(memory.free_pages(), free, "every page it took given back");
    assert_eq!(word(&mut memory, made.base), Ok(0x55));
    assert_eq!(
        word(&mut memory, made.base + 0x1000),
        Err(Abort::Translation),
        "the page mapped before the failure undone"
    );
    let twice = Create {
        flags: AreaFlags::DOUBLY_MAPPED | AreaFlags::NOT_CACHEABLE,
        ..request(0x1000, 0x10_0000)
    };
    let twice = memory.create_area(&twice, &mut guest).unwrap();
    store(&mut memory, twice.base, 0x66);
    memory
        .mmu
        .map(
            Table::Global,
            twice.base + 0x1000,
            squatter,
            made_access(&memory, twice.base),
        )
        .unwrap();
    assert!(memory
        .change_area(named(twice.number), 0x1000, &mut guest)
        .is_err());
    assert_eq!(
        word(&mut memory, twice.base - 0x1000),
        Ok(0x66),
        "its first copy moved back up"
    );
    assert_eq!(
        word(&mut memory, twice.base - 0x2000),
        Err(Abort::Translation)
    );
}

/// The access an area's page at `address` is mapped with.
fn made_access(memory: &Memory, address: u32) -> Access {
    memory.mmu.lookup(Table::Global, address).unwrap().1
}

#[test]
fn a_dma_areas_handler_is_shown_the_pages_chosen_and_may_refuse_them() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let dma = Create {
        flags: AreaFlags::NEEDS_DMA,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&dma, &mut guest).unwrap();
    guest.take();
    assert_eq!(
        memory.change_area(named(made.number), 0x2000, &mut guest),
        Ok(0x2000)
    );
    let calls = guest.take();
    assert!(
        calls.contains(&Call::PreGrow {
            bytes: 0x2000,
            size: 0,
            block: 2
        }),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::Unsafe(_) | Call::Fiq(_))),
        "free pages need no announcing"
    );
    guest.refuse_grow = Some(Refused::Silently);
    let free = memory.free_pages();
    assert!(memory
        .change_area(named(made.number), 0x1000, &mut guest)
        .is_err());
    assert_eq!(memory.free_pages(), free);
    assert_eq!(pages_of(&memory, made.number), 2);
}

#[test]
fn a_replaced_page_carries_its_watches() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let other = memory
        .create_area(&request(0x1000, 0x1000), &mut guest)
        .unwrap();
    let frame = Frame::new(frame_at(&memory, other.base).unwrap());
    let watcher = tairix_riscos_arm::mmu::Watcher::new(0).unwrap();
    memory.mmu.watch(frame, 1, watcher).unwrap();
    memory.mmu.take_written(watcher, |_, _| {});
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&specific, &mut guest).unwrap();
    guest.name_pages = Vec::from([frame.number()]);
    memory
        .change_area(named(made.number), 0x1000, &mut guest)
        .unwrap();
    let mut written = Vec::new();
    memory
        .mmu
        .take_written(watcher, |frame, granules| written.push((frame, granules)));
    let replacement = Frame::new(frame_at(&memory, other.base).unwrap());
    assert_eq!(written, [(replacement, 1)], "its contents changed frame");
    store(&mut memory, other.base, 1);
    written.clear();
    memory
        .mmu
        .take_written(watcher, |frame, granules| written.push((frame, granules)));
    assert_eq!(written, [(replacement, 1)], "and stores to it are seen");
}

#[test]
fn numbers_past_those_a_caller_reads_as_positive_are_never_given_out() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    memory.areas.quick.clear();
    memory.areas.probe_from = 0x7FFF_FFFE;
    let made = memory.create_area(&request(0, 0), &mut guest).unwrap();
    assert_eq!(made.number, 0x7FFF_FFFF);
    assert_eq!(
        memory.create_area(&request(0, 0), &mut guest),
        Err(AreaError::CantAllocateArea)
    );
}

#[test]
fn a_change_asked_from_an_interrupt_calls_no_service() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let made = memory.create_area(&request(0, 0x1000), &mut guest).unwrap();
    guest.take();
    guest.interrupt = true;
    assert!(memory
        .change_area(named(made.number), 0x1000, &mut guest)
        .is_err());
    assert!(guest.take().is_empty());
}

#[test]
fn the_areas_one_session_may_hold_are_bounded() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    for _ in 0..super::MOST_AREAS {
        memory.create_area(&request(0, 0), &mut guest).unwrap();
    }
    assert_eq!(
        memory.create_area(&request(0, 0), &mut guest),
        Err(AreaError::CantAllocateArea)
    );
    memory.remove_area(0x100, &mut guest).unwrap();
    assert!(memory.create_area(&request(0, 0), &mut guest).is_ok());
}

#[test]
fn reserved_pages_are_marked_kept_through_use_and_refused_twice() {
    let mut memory = shared();
    let page = memory
        .contiguous_free(2, 0, 0, u64::from(u32::MAX))
        .unwrap();
    assert_eq!(memory.reserve_pages(page, 2, true), Ok(()));
    assert_eq!(
        memory.page_map(page).map(|(_, flags)| flags & RESERVED),
        Some(RESERVED)
    );
    assert_eq!(
        memory.reserve_pages(page + 1, 1, true),
        Err(ReserveError::Reserved),
        "one of them is reserved"
    );
    assert_eq!(
        memory.contiguous_free(1, 0, 0, u64::from(u32::MAX)),
        Some(page + 2),
        "never recommended"
    );
    assert_eq!(
        memory.reserve_pages(page, 0, true),
        Ok(()),
        "an empty range"
    );
    let past = memory.ceiling_pages();
    assert_eq!(
        memory.reserve_pages(past, 1, true),
        Err(ReserveError::Range)
    );
    assert_eq!(
        memory.reserve_pages(u32::MAX, 2, false),
        Err(ReserveError::Range)
    );
    assert_eq!(memory.reserve_pages(page, 2, false), Ok(()));
    assert_eq!(
        memory.reserve_pages(page, 2, false),
        Ok(()),
        "released twice"
    );
    assert!(!memory.page_reserved(page));
}

#[test]
fn a_recommendation_keeps_to_its_alignment_and_physical_range() {
    let memory = shared();
    let any = u64::from(u32::MAX);
    let aligned = memory.contiguous_free(1, 20, 0, any).unwrap();
    assert_eq!(aligned % 256, 0, "a megabyte boundary");
    let from = memory.contiguous_free(3, 0, 0x40_0000, any).unwrap();
    assert!(from >= 0x400, "at or past 4 MiB");
    assert_eq!(
        memory.contiguous_free(1, 0, 0x40_0000, 0x40_0FFE),
        None,
        "no whole page lies in the range"
    );
    assert_eq!(
        memory.contiguous_free(1, 0, 0x40_0000, 0x40_0FFF),
        Some(0x400)
    );
    assert_eq!(
        memory.contiguous_free(1, 0, 1 << 32, u64::MAX),
        None,
        "no page past 4 GiB"
    );
}

#[test]
fn a_reserved_page_is_taken_only_by_a_handler_saying_so_and_locked_only_knowingly() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let page = memory
        .contiguous_free(1, 0, 0, u64::from(u32::MAX))
        .unwrap();
    memory.reserve_pages(page, 1, true).unwrap();
    let specific = Create {
        flags: AreaFlags::NEEDS_SPECIFIC_PAGES,
        ..request(0, 0x10_0000)
    };
    let made = memory.create_area(&specific, &mut guest).unwrap();
    guest.name_pages = Vec::from([page]);
    let number = i32::try_from(made.number).unwrap();
    assert_eq!(
        memory.change_area(number, 0x1000, &mut guest),
        Err(Failure {
            error: AreaError::PageReserved,
            moved: 0
        })
    );
    guest.take_reserved = true;
    assert_eq!(memory.change_area(number, 0x1000, &mut guest), Ok(0x1000));
    memory.change_area(number, -0x1000, &mut guest).unwrap();
    memory.remove_area(made.number, &mut guest).unwrap();
    assert!(
        memory.page_reserved(page),
        "the reservation outlasts the claim"
    );
    let pool = pool(&mut memory, &mut guest);
    let named = i32::try_from(page).unwrap();
    assert_eq!(
        memory.pool_physical(pool.number, &[physical(0, named, 1 << 15)], &mut guest),
        Err((AreaError::PageReserved, 0)),
        "a lock not knowing it is reserved"
    );
    let lock = 1 << 15 | RESERVED;
    memory
        .pool_physical(pool.number, &[physical(0, named, lock)], &mut guest)
        .unwrap();
    memory
        .pool_physical(pool.number, &[physical(0, -1, 0)], &mut guest)
        .unwrap();
    memory
        .pool_physical(pool.number, &[physical(1, named, 0)], &mut guest)
        .unwrap();
    memory
        .pool_physical(pool.number, &[physical(2, -2, 0)], &mut guest)
        .unwrap();
    let mut queries = [PageQuery {
        slot: Some(2),
        ..PageQuery::default()
    }];
    memory.pool_query(pool.number, &mut queries).unwrap();
    assert!(
        queries[0]
            .page
            .is_some_and(|chosen| !memory.page_reserved(chosen)),
        "a page the kernel chooses is one no one reserved"
    );
}

/// Every area numbers enumeration finds, the free pool's among them.
fn enumerated(memory: &Memory) -> Vec<u32> {
    let mut found = Vec::new();
    let mut after = None;
    while let Some(number) = memory.enumerate_areas(after).unwrap() {
        found.push(number);
        after = Some(number);
    }
    found
}

#[test]
fn a_parked_area_is_found_by_nothing_and_its_number_is_free_until_it_is_given_back() {
    let mut memory = shared();
    let mut guest = Recorder::new();
    let screen = Create {
        number: Some(2),
        ..request(0x1000, 0x10_0000)
    };
    let first = memory.create_kernel_area(&screen, &mut guest).unwrap();
    store(&mut memory, first.base, 0x1111);
    let parked = memory.park_area(first.number).unwrap();
    assert_eq!(
        memory.area_info(first.number).err(),
        Some(AreaError::BadDynamicArea)
    );
    assert!(!enumerated(&memory).contains(&first.number));
    assert_eq!(word(&mut memory, first.base), Ok(0x1111), "its pages kept");
    let second = memory.create_kernel_area(&screen, &mut guest).unwrap();
    assert_eq!(second.number, first.number);
    assert_ne!(second.base, first.base, "its addresses still reserved");
    assert_eq!(
        memory.unpark_area(&parked),
        Err(AreaError::AreaAlreadyExists),
        "its number taken"
    );
    let other = memory.park_area(second.number).unwrap();
    memory.unpark_area(&parked).unwrap();
    assert_eq!(memory.area_info(first.number).unwrap().base, first.base);
    assert!(enumerated(&memory).contains(&first.number));
    assert_eq!(word(&mut memory, first.base), Ok(0x1111));
    assert_eq!(
        memory.unpark_area(&other),
        Err(AreaError::AreaAlreadyExists)
    );
}
