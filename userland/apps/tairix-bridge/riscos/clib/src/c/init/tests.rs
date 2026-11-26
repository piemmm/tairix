use ::alloc::vec::Vec;

use super::*;
use crate::c::stdio::stream::Stream;
use crate::c::testing::{Client, CODE, FREE, HEAP, RETURN, SLOT_END, STATICS};
use tairix_riscos_kernel::swi::os::{MODULE, READ_DYNAMIC_AREA};
use tairix_riscos_kernel::swi::X;

const CURSOR: u32 = FREE + 0x100;
const LOG: u32 = FREE + 0x104;
/// Two functions noting 1 and 2 at a cursor's place in turn.
const NOTERS: [u32; 10] = [
    0xE3A0_0001,
    0xEA00_0001,
    0xE3A0_0002,
    0xEAFF_FFFF,
    0xE59F_C00C,
    0xE59C_1000,
    0xE481_0004,
    0xE58C_1000,
    0xE1A0_F00E,
    CURSOR,
];

/// A client whose kernel heap starts at [`HEAP`], as `_kernel_init` leaves
/// it, the C library not yet started.
fn client() -> Client {
    let mut client = Client::new();
    let statics = Statics::of_base(STATICS);
    {
        let mut os = client.session.kernel.os();
        statics.write(&mut os, kernel_at::HEAP, HEAP).expect("heap");
        statics
            .write(&mut os, kernel_at::HEAP_LIMIT, SLOT_END)
            .expect("limit");
    }
    let words: Vec<u8> = NOTERS.iter().flat_map(|w| w.to_le_bytes()).collect();
    client.put(CODE, &words);
    client.put(CURSOR, &LOG.to_le_bytes());
    client
}

#[test]
fn clib_initialise_starts_the_c_locale_the_signals_the_generator_the_heap_and_the_streams() {
    let mut client = client();
    client.put(STATICS + at::ERRNO, &[9, 0, 0, 0]);
    assert_eq!(client.answer(clib_initialise, &[]), 0);
    assert_eq!(client.word(STATICS + at::ERRNO), 0);
    assert_eq!(
        client.bytes(STATICS + at::CTYPE, 256),
        super::super::ctype::C_LOCALE
    );
    assert!((0..10).all(|n| client.word(STATICS + at::SIGNALS + 4 * n) == signal::SIG_DFL));
    assert_eq!(
        client.answer(stdlib::rand, &[]),
        0x5647_A0B0,
        "seeded with one"
    );
    assert_eq!(client.word(STATICS + at::HEAP_FIRST), HEAP);
    assert_eq!(
        client.word(STATICS + kernel_at::HEAP),
        HEAP + 8192,
        "2048 words claimed"
    );
    let alloc_routine =
        context::routine(&mut client.session.kernel.os(), Routine::HeapAlloc).expect("routine");
    assert_eq!(
        client.word(STATICS + kernel_at::ALLOC),
        alloc_routine,
        "stack chunks from the heap"
    );
    let streams = Stream::load(
        &mut client.session.kernel.os(),
        stream::stdin(Statics::of_base(STATICS)),
    )
    .expect("stdin");
    assert_eq!(
        (streams.flag, streams.extra),
        (stream::flag::READ, HEAP + 8),
        "the streams' extra blocks the heap's first"
    );
    let blocks = stream::EXTRA_BYTES * crate::c::statics::STREAMS;
    assert_eq!(client.answer(alloc::malloc, &[16]), HEAP + 8 + blocks + 8);
    let huge_val = [
        client.word(STATICS + at::HUGE_VAL),
        client.word(STATICS + at::HUGE_VAL + 4),
    ];
    assert_eq!(
        huge_val,
        [0x7FEF_FFFF, 0xFFFF_FFFF],
        "HUGE_VAL as the FPA stores a double"
    );
    let started = time::monotonic_time(&mut client.session.kernel.os()).expect("the time");
    assert_eq!(
        client.word(STATICS + at::CLOCK_START),
        started,
        "clock counts from here"
    );
}

#[test]
fn finalise_calls_what_atexit_registered_the_last_first() {
    let mut client = client();
    client.call(clib_initialise, &[]);
    assert_eq!(client.answer(atexit, &[CODE]), 0);
    assert_eq!(client.answer(atexit, &[CODE + 8]), 0);
    assert_eq!(client.answer(atexit, &[CODE]), 0);
    client.running();
    let (done, _, pc) = client.subroutine(finalise);
    assert!(done.is_ok() && pc == RETURN);
    assert_eq!(
        [client.word(LOG), client.word(LOG + 4), client.word(LOG + 8)],
        [1, 2, 1]
    );
    assert_eq!(client.word(STATICS + at::ATEXIT_COUNT), 0, "each taken off");
}

/// A function noting the R0 and R2 it is called with at the cursor.
const REGISTER_NOTER: u32 = CODE + 0x40;
const REGISTER_NOTER_WORDS: [u32; 7] = [
    0xE59F_C010,
    0xE59C_1000,
    0xE481_0004,
    0xE481_2004,
    0xE58C_1000,
    0xE1A0_F00E,
    CURSOR,
];

#[test]
fn atexit_takes_any_number_and_calls_each_with_its_own_address() {
    let mut client = client();
    let words: Vec<u8> = REGISTER_NOTER_WORDS
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    client.put(REGISTER_NOTER, &words);
    client.call(clib_initialise, &[]);
    let many = 2 * EXIT_LIST_HERE + 4;
    for n in 0..many {
        let function = if n % 2 == 0 { CODE } else { REGISTER_NOTER };
        assert_eq!(client.answer(atexit, &[function]), 0, "registration {n}");
    }
    client.running();
    let (done, _, pc) = client.subroutine(finalise);
    assert!(done.is_ok() && pc == RETURN);
    let mut at = LOG;
    for n in (0..many).rev() {
        if n % 2 == 0 {
            assert_eq!(client.word(at), 1, "the noter registered {n}th");
            at += 4;
        } else {
            assert_eq!(
                [client.word(at), client.word(at + 4)],
                [REGISTER_NOTER; 2],
                "R0 and R2 at {n}"
            );
            at += 8;
        }
    }
}

#[test]
fn a_count_no_list_holds_registers_nothing_and_calls_only_what_the_list_holds() {
    let mut client = client();
    client.call(clib_initialise, &[]);
    for _ in 0..EXIT_LIST_HERE {
        assert_eq!(client.answer(atexit, &[CODE]), 0);
    }
    client.put(STATICS + at::ATEXIT_COUNT, &u32::MAX.to_le_bytes());
    assert_ne!(client.answer(atexit, &[CODE]), 0, "refused");
    client.running();
    let (done, _, pc) = client.subroutine(finalise);
    assert!(done.is_ok() && pc == RETURN);
    assert_eq!(
        client.word(CURSOR),
        LOG + 4 * EXIT_LIST_HERE,
        "each the list holds called once"
    );
    assert_eq!(client.word(STATICS + at::ATEXIT_COUNT), 0);
}

#[test]
fn a_list_block_the_heap_does_not_hold_is_replaced_not_written() {
    let mut client = client();
    client.call(clib_initialise, &[]);
    for _ in 0..EXIT_LIST_HERE {
        assert_eq!(client.answer(atexit, &[CODE]), 0);
    }
    let fake = FREE + 0x800;
    client.put(fake, &0x7FFF_FFFF_u32.to_le_bytes());
    client.put(STATICS + at::ATEXIT_MORE, &fake.to_le_bytes());
    assert_eq!(client.answer(atexit, &[CODE + 8]), 0);
    let block = client.word(STATICS + at::ATEXIT_MORE);
    assert!(
        block > HEAP && block < SLOT_END,
        "a block of the heap: {block:#x}"
    );
    assert_eq!(client.word(block), CODE + 8);
    assert_eq!(client.word(fake + 4), 0, "nothing written past the fake");
}

/// Its finalisation, through the languages `_kernel_init` was given, is
/// `tests/stdio.rs`'s to show.
#[test]
fn exit_sets_the_return_code() {
    let mut client = client();
    client.call(clib_initialise, &[]);
    let _ = client.outcome(exit, &[3]);
    assert_eq!(client.word(STATICS + kernel_at::RETURN_CODE), 3);
}

#[test]
fn the_version_is_the_library_s_own_name_with_genuine_s_number() {
    let mut client = client();
    let version = client.answer(clib_version, &[]);
    assert_eq!(
        client.string(version),
        b"TAIRiX Bridge SharedCLibrary vsn 623/32\n"
    );
}

fn rma_used(client: &mut Client) -> u32 {
    let mut os = client.session.kernel.os();
    let mut free = Regs::default();
    free.r[0] = 5;
    os.perform(X | MODULE, &mut free).expect("described");
    let mut area = Regs::default();
    area.r[0] = 1;
    os.perform(X | READ_DYNAMIC_AREA, &mut area)
        .expect("the RMA");
    area.r[1] - free.r[3]
}

#[test]
fn a_module_s_heap_lies_in_the_rma_and_its_finalisation_gives_it_back() {
    let mut client = client();
    let start = rma_used(&mut client);
    let workspace = client.session.kernel.os().rma_claim(64).expect("claimed");
    let private = FREE + 0x200;
    client.put(private, &workspace.to_le_bytes());
    client.put(STATICS + kernel_at::MODULE, &[1, 0, 0, 0]);
    client.call(clib_initialise, &[]);
    let first = client.word(STATICS + at::HEAP_FIRST);
    assert!(first >= SLOT_END, "in the RMA: {first:#x}");
    let big = client.answer(alloc::malloc, &[40_000]);
    assert!(big != 0 && big != first + 8, "a second arena");
    client.call(alloc::free, &[big]);
    assert!(rma_used(&mut client) > start);
    client.running_with(&[private]);
    let (done, _, _) = client.subroutine(clib_finalisemodule);
    assert!(done.is_ok());
    assert_eq!(
        rma_used(&mut client),
        start,
        "the heap and the workspace given back"
    );
    assert_eq!(
        client.word(private),
        0,
        "the private word cleared, so the kernel frees nothing again"
    );
}
