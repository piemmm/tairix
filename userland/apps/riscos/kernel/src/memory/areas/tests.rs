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
    memory.mmu().read(address, Width::Word, true)
}

fn store(memory: &mut Memory, address: u32, value: u32) {
    memory
        .mmu()
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
        .mmu()
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
        .mmu()
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
