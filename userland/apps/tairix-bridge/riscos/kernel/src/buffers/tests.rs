//! The buffer manager held to RISC OS 5's BufferManager 0.39: the kernel's
//! buffers it holds, handles, flags, thresholds, registered memory, the
//! vectors' byte and block forms, waking, owners, events, `UpCall`s, the
//! service routine, and the kernel's own buffers it passes on.

use alloc::vec;
use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use super::*;
use crate::cli::tests::{cli, plain};
use crate::events::number::{INPUT_FULL as EVENT_INPUT_FULL, OUTPUT_EMPTY as EVENT_OUTPUT_EMPTY};
use crate::msgtrans::tests::swi;
use crate::os::Outcome;
use crate::swi::buffer::{
    CREATE, DEREGISTER, GET_INFO, INTERNAL_INFO, LINK_DEVICE, MODIFY_FLAGS, REGISTER, REMOVE,
    THRESHOLD, UNLINK_DEVICE,
};
use crate::swi::os::{BYTE, CALL_A_VECTOR, CLAIM};
use crate::swi::X;
use crate::testkit::asm;
use crate::testkit::{Session, CODE, DATA};
use crate::vectors::Vector;

/// Where `OS_CallAVector` is called from, so its flags can be set first.
const CALLER: u32 = CODE;
/// The guest routines the tests link, claim with and refuse with.
const WAKE: u32 = CODE + 0x100;
const OWNER: u32 = CODE + 0x200;
const REFUSER: u32 = CODE + 0x300;
const CLAIMANT: u32 = CODE + 0x400;
/// The refuser's error block.
const REFUSAL: u32 = DATA + 0x700;
const REFUSAL_NUMBER: u32 = 0x1234;
/// What the routines record: the next free word's address, then the words.
const LOG: u32 = DATA + 0x800;
/// Blocks moved in and out.
const BYTES: u32 = DATA + 0x2000;
const OUT: u32 = DATA + 0x3000;
/// Memory a test registers as a buffer.
const OWN_MEMORY: u32 = DATA + 0x5000;

/// R9 to R11, which a recording routine uses and puts back.
const SCRATCH: u16 = 0b1110_0000_0000;

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

fn ok(s: &mut Session, number: u32, low: &[u32]) -> Regs {
    match swi(s, number, registers(low)) {
        Ok(regs) => regs,
        Err((error, text)) => panic!("SWI &{number:X}: &{error:X} {}", text.escape_ascii()),
    }
}

/// The error number `SWI number` refuses with.
fn refused(s: &mut Session, number: u32, low: &[u32]) -> u32 {
    match swi(s, number, registers(low)) {
        Ok(_) => panic!("SWI &{number:X} refused"),
        Err((error, _)) => error,
    }
}

fn create(s: &mut Session, flags: u32, size: u32) -> u32 {
    ok(s, CREATE, &[flags, size, ANY_HANDLE]).r[0]
}

/// `Buffer_GetInfo`: R0 to R6.
fn info(s: &mut Session, handle: u32) -> [u32; 7] {
    let regs = ok(s, GET_INFO, &[handle]);
    core::array::from_fn(|n| regs.r[n])
}

fn flags(s: &mut Session, handle: u32) -> u32 {
    info(s, handle)[0]
}

fn used(s: &mut Session, handle: u32) -> u32 {
    info(s, handle)[6]
}

/// `vector` called through `OS_CallAVector` from User mode with `low` in R0
/// up and `flags` set: what it answers, V and R0 its error.
fn vector(s: &mut Session, vector: Vector, flags: u32, low: &[u32]) -> Regs {
    s.poke(
        CALLER,
        &[
            asm::set_flags(flags),
            asm::swi(X | CALL_A_VECTOR),
            asm::b(CALLER + 8, CALLER + 8),
        ],
    );
    let mut r = registers(low);
    r[9] = vector.0;
    s.enter(crate::memory::Bits::Bit32, CALLER, r);
    assert_eq!(s.resume(), Outcome::Budget, "the call returned");
    s.regs()
}

/// A byte into `handle`'s buffer through `InsV`: whether it went in.
fn insert(s: &mut Session, handle: u32, byte: u8) -> bool {
    let regs = vector(s, vectors::INS, 0, &[u32::from(byte), handle]);
    assert!(!regs.flag(psr::V), "InsV gives no error");
    !regs.flag(psr::C)
}

/// The next byte of `handle`'s buffer through `RemV`, taken unless
/// `examining`.
fn remove(s: &mut Session, handle: u32, examining: bool) -> Option<u8> {
    let flags = if examining { psr::V } else { 0 };
    let regs = vector(s, vectors::REM, flags, &[0, handle]);
    assert!(!regs.flag(psr::V), "RemV gives no error");
    (!regs.flag(psr::C)).then(|| {
        assert_eq!(regs.r[0], regs.r[2], "R0 and R2 both the byte");
        regs.byte(0)
    })
}

/// `bytes` into `handle`'s buffer as a block: R2, R3 and C.
fn insert_block(s: &mut Session, handle: u32, bytes: &[u8]) -> (u32, u32, bool) {
    s.write(BYTES, bytes);
    let regs = vector(s, vectors::INS, 0, &[0, handle | BLOCK, BYTES, len(bytes)]);
    assert!(!regs.flag(psr::V), "InsV gives no error");
    (regs.r[2], regs.r[3], regs.flag(psr::C))
}

/// `count` bytes of `handle`'s buffer as a block to [`OUT`]: R2, R3, C and
/// the bytes moved.
fn remove_block(
    s: &mut Session,
    handle: u32,
    count: u32,
    examining: bool,
) -> (u32, u32, bool, Vec<u8>) {
    let flags = if examining { psr::V } else { 0 };
    let regs = vector(s, vectors::REM, flags, &[0, handle | BLOCK, OUT, count]);
    assert!(!regs.flag(psr::V), "RemV gives no error");
    let moved = s.read(OUT, (regs.r[2] - OUT) as usize);
    (regs.r[2], regs.r[3], regs.flag(psr::C), moved)
}

fn purge(s: &mut Session, handle: u32) {
    let regs = vector(s, vectors::CNP, psr::V, &[0, handle]);
    assert!(!regs.flag(psr::V), "a purge gives no error");
}

fn len(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).expect("a short block")
}

/// Code at `at` that adds `registers` and the CPSR to the log and returns
/// with every register and flag as it came: a vector claimant passing the
/// call on.
fn recorder(s: &mut Session, at: u32, registers: &[u32]) {
    let mut code = vec![asm::push(SCRATCH), 0, asm::ldr(11, 10, 0)];
    code.extend(registers.iter().map(|&r| asm::str_post(r, 11, 4)));
    code.extend([
        asm::mrs(9),
        asm::str_post(9, 11, 4),
        asm::str(11, 10, 0),
        asm::pull(SCRATCH),
        asm::mov_pc_lr(),
    ]);
    let literal = 4 * u32::try_from(code.len()).expect("a short routine");
    code[1] = asm::ldr(10, 15, u16::try_from(literal - 12).expect("near"));
    code.push(LOG);
    s.poke(at, &code);
    s.poke(LOG, &[LOG + 4]);
}

/// What has been recorded since, cleared.
fn logged(s: &mut Session) -> Vec<u32> {
    let end = s.peek(LOG);
    let words = (LOG + 4..end).step_by(4).map(|at| s.peek(at)).collect();
    s.poke(LOG, &[LOG + 4]);
    words
}

/// Code at [`REFUSER`] returning V set with an error of its own.
fn refuser(s: &mut Session) {
    s.poke(REFUSAL, &[REFUSAL_NUMBER]);
    s.write(REFUSAL + 4, b"Kept\0");
    s.poke(
        REFUSER,
        &[
            asm::set_flags(psr::V),
            asm::ldr(0, 15, 0),
            asm::mov_pc_lr(),
            REFUSAL,
        ],
    );
}

/// The mode and interrupt mask a recorded CPSR shows.
fn entered(cpsr: u32) -> (u32, bool) {
    (cpsr & 0x1F, cpsr & psr::I != 0)
}

const SUPERVISOR_32: u32 = 0x13;

#[test]
fn it_holds_the_kernel_s_mouse_serial_and_printer_buffers_in_risc_os_5_s_order() {
    let mut s = plain();
    for (handle, id, flags, size) in [
        (MOUSE, 0, 4, 64),
        (SERIAL_IN, 0x2C, 0xC, 256),
        (SERIAL_OUT, 0x58, 2, 192),
        (PRINTER, 0x84, 2, 1024),
    ] {
        let [got_flags, start, end, insert, remove, free, used] = info(&mut s, handle);
        assert_eq!((got_flags, end - start), (flags, size), "buffer {handle}");
        assert_eq!((insert, remove, free, used), (0, 0, size - 1, 0));
        let internal = ok(&mut s, INTERNAL_INFO, &[handle]);
        assert_eq!(internal.r[0], id, "buffer {handle}'s internal ID");
        assert_ne!(internal.r[1], 0, "a service routine");
        assert_ne!(internal.r[2], 0, "its workspace");
    }
    for keyboard_or_sound in [0, 4, 5, 6, 7, 8] {
        assert_eq!(
            refused(&mut s, GET_INFO, &[keyboard_or_sound]),
            NOT_KNOWN.number,
            "buffer {keyboard_or_sound} is the kernel's own"
        );
    }
    let first = create(&mut s, 0, 16);
    assert_eq!(
        ok(&mut s, INTERNAL_INFO, &[first]).r[0],
        0xB0,
        "the fifth record"
    );
}

#[test]
fn a_handle_is_the_lowest_unused_from_256_or_the_one_asked_for() {
    let mut s = plain();
    assert_eq!(create(&mut s, 0xF0, 16), FIRST_HANDLE);
    assert_eq!(flags(&mut s, FIRST_HANDLE), 0x10, "five flag bits kept");
    assert_eq!(create(&mut s, 0, 16), FIRST_HANDLE + 1);
    ok(&mut s, REMOVE, &[FIRST_HANDLE]);
    assert_eq!(
        create(&mut s, 0, 8),
        FIRST_HANDLE,
        "a removed handle reused"
    );
    assert_eq!(ok(&mut s, CREATE, &[0, 8, 5]).r[0], 5);
    assert_eq!(ok(&mut s, CREATE, &[0, 8, 0xFFFF_FFFE]).r[0], 0xFFFF_FFFE);
    assert_eq!(refused(&mut s, CREATE, &[0, 8, 5]), HANDLE_IN_USE.number);
    assert_eq!(
        refused(&mut s, CREATE, &[0, 8, MOUSE]),
        HANDLE_IN_USE.number
    );
    assert_eq!(
        refused(&mut s, CREATE, &[0, 0, ANY_HANDLE]),
        TOO_SMALL.number
    );
}

#[test]
fn the_lowest_unused_handle_is_found_past_a_long_run_and_in_a_gap_in_it() {
    let mut s = plain();
    for n in 0..300 {
        assert_eq!(create(&mut s, 0, 8), FIRST_HANDLE + n);
    }
    ok(&mut s, REMOVE, &[FIRST_HANDLE + 150]);
    assert_eq!(create(&mut s, 0, 8), FIRST_HANDLE + 150, "the gap");
    assert_eq!(
        ok(&mut s, CREATE, &[0, 8, FIRST_HANDLE + 301]).r[0],
        FIRST_HANDLE + 301
    );
    assert_eq!(create(&mut s, 0, 8), FIRST_HANDLE + 300, "the run's end");
    assert_eq!(
        create(&mut s, 0, 8),
        FIRST_HANDLE + 302,
        "past one asked for"
    );
}

#[test]
fn an_owner_letting_go_as_the_manager_dies_reaches_none_of_its_swis() {
    let mut s = plain();
    let (first, second) = (create(&mut s, 0, 8), create(&mut s, 0, 8));
    // The first's owner tries to remove the buffer its private word names
    // as it lets go, keeping the flags the call left at LOG; the second's
    // lets go.
    s.poke(
        OWNER,
        &[
            asm::push(1 << 14 | 1 << 9 | 1 << 10),
            asm::mov_reg(0, 8),
            asm::swi(X | REMOVE),
            asm::mrs(9),
            asm::ldr(10, 15, 12),
            asm::str(9, 10, 0),
            asm::pull(1 << 14 | 1 << 9 | 1 << 10),
            asm::set_flags(0),
            asm::mov_pc_lr(),
            LOG,
        ],
    );
    s.poke(OWNER + 0x40, &[asm::set_flags(0), asm::mov_pc_lr()]);
    s.poke(LOG, &[0]);
    ok(&mut s, LINK_DEVICE, &[first, 0, OWNER, second, 0]);
    ok(&mut s, LINK_DEVICE, &[second, 0, OWNER + 0x40, 0, 0]);
    cli(&mut s, "RMKill BufferManager").expect("killed");
    assert_ne!(s.peek(LOG) & psr::V, 0, "the removal refused as it dies");
}

#[test]
fn a_manager_killed_with_many_buffers_gives_back_all_it_claimed() {
    let mut s = plain();
    let used = |s: &mut Session| {
        let free = ok(s, crate::swi::os::MODULE, &[5]).r[3];
        ok(s, crate::swi::os::READ_DYNAMIC_AREA, &[1]).r[1] - free
    };
    cli(&mut s, "RMKill BufferManager").expect("killed");
    let before = used(&mut s);
    cli(&mut s, "RMReInit BufferManager").expect("started");
    for _ in 0..300 {
        create(&mut s, 0, 8);
    }
    cli(&mut s, "RMKill BufferManager").expect("killed with them");
    assert_eq!(
        used(&mut s),
        before,
        "every record, buffer and its workspace freed"
    );
}

#[test]
fn a_buffer_the_rma_cannot_hold_is_refused_and_takes_no_handle() {
    let mut s = plain();
    let error = refused(&mut s, CREATE, &[0, 0x7FFF_FFF0, ANY_HANDLE]);
    assert_eq!(error, crate::kheap::KernelHeap::Rma.full().number);
    assert_eq!(create(&mut s, 0, 8), FIRST_HANDLE, "nothing was kept");
}

#[test]
fn every_call_refuses_a_handle_no_buffer_has() {
    let mut s = plain();
    for number in [
        REMOVE,
        DEREGISTER,
        MODIFY_FLAGS,
        LINK_DEVICE,
        UNLINK_DEVICE,
        GET_INFO,
        THRESHOLD,
        INTERNAL_INFO,
    ] {
        assert_eq!(
            refused(&mut s, number, &[0x999]),
            NOT_KNOWN.number,
            "SWI &{number:X}"
        );
    }
    assert_eq!(
        refused(&mut s, crate::swi::chunk::BUFFER + 10, &[]),
        SWI_OUT_OF_RANGE.number
    );
}

#[test]
fn modify_flags_ands_then_eors_and_keeps_every_bit() {
    let mut s = plain();
    let handle = create(&mut s, 3, 8);
    let regs = ok(&mut s, MODIFY_FLAGS, &[handle, 0x40, 0xFFFF_FFFE]);
    assert_eq!(regs.r[..3], [handle, 3, 0x42]);
    assert_eq!(flags(&mut s, handle), 0x42);
}

#[test]
fn threshold_answers_the_one_it_had_and_minus_one_only_reads() {
    let mut s = plain();
    let handle = create(&mut s, 0, 8);
    assert_eq!(ok(&mut s, THRESHOLD, &[handle, 5]).r[1], 0, "none at first");
    assert_eq!(ok(&mut s, THRESHOLD, &[handle, READ]).r[1], 5);
    assert_eq!(
        ok(&mut s, THRESHOLD, &[handle, 0x1_2345]).r[1],
        5,
        "read left it"
    );
    assert_eq!(ok(&mut s, THRESHOLD, &[handle, READ]).r[1], 0x1_2345);
}

#[test]
fn registered_memory_is_the_caller_s_and_a_removal_s_free_of_it_fails_after() {
    let mut s = plain();
    assert_eq!(
        refused(&mut s, REGISTER, &[0, OWN_MEMORY, OWN_MEMORY, ANY_HANDLE]),
        TOO_SMALL.number
    );
    let handle = ok(
        &mut s,
        REGISTER,
        &[0, OWN_MEMORY, OWN_MEMORY + 16, ANY_HANDLE],
    )
    .r[0];
    assert_eq!(info(&mut s, handle)[1..3], [OWN_MEMORY, OWN_MEMORY + 16]);
    ok(&mut s, DEREGISTER, &[handle]);
    assert_eq!(refused(&mut s, GET_INFO, &[handle]), NOT_KNOWN.number);
    let handle = ok(
        &mut s,
        REGISTER,
        &[0, OWN_MEMORY, OWN_MEMORY + 16, ANY_HANDLE],
    )
    .r[0];
    let error = refused(&mut s, REMOVE, &[handle]);
    assert_eq!(error, 0x185, "the RMA has no such block");
    assert_eq!(
        refused(&mut s, GET_INFO, &[handle]),
        NOT_KNOWN.number,
        "gone all the same"
    );
}

#[test]
fn bytes_go_in_and_come_out_in_order_and_a_buffer_holds_one_fewer_than_its_size() {
    let mut s = plain();
    let handle = create(&mut s, 0, 4);
    for byte in *b"abc" {
        assert!(insert(&mut s, handle, byte));
    }
    assert!(
        !insert(&mut s, handle, b'd'),
        "three is all four bytes hold"
    );
    assert_eq!(remove(&mut s, handle, true), Some(b'a'), "examined");
    assert_eq!(used(&mut s, handle), 3, "and left");
    assert_eq!(remove(&mut s, handle, false), Some(b'a'));
    assert!(insert(&mut s, handle, b'd'), "wrapping");
    let taken: Vec<u8> = (0..3)
        .filter_map(|_| remove(&mut s, handle, false))
        .collect();
    assert_eq!(taken, b"bcd");
    assert_eq!(remove(&mut s, handle, false), None, "empty");
}

#[test]
fn blocks_move_as_far_as_the_buffer_allows_across_its_end() {
    let mut s = plain();
    let handle = create(&mut s, 0, 8);
    assert_eq!(
        insert_block(&mut s, handle, b"0123456789"),
        (BYTES + 7, 3, true)
    );
    let (end, left, short, moved) = remove_block(&mut s, handle, 5, false);
    assert_eq!(
        (end, left, short, moved.as_slice()),
        (OUT + 5, 0, false, &b"01234"[..])
    );
    assert_eq!(insert_block(&mut s, handle, b"abcd"), (BYTES + 4, 0, false));
    let (_, left, short, moved) = remove_block(&mut s, handle, 10, true);
    assert_eq!(
        (left, short, moved.as_slice()),
        (4, true, &b"56abcd"[..]),
        "examined"
    );
    let (_, left, short, moved) = remove_block(&mut s, handle, 10, false);
    assert_eq!((left, short, moved.as_slice()), (4, true, &b"56abcd"[..]));
    assert!(remove_block(&mut s, handle, 2, false).2, "empty");
}

#[test]
fn counts_purges_and_free_space_through_cnpv() {
    let mut s = plain();
    let handle = create(&mut s, 0, 600);
    insert_block(&mut s, handle, &[7; 300]);
    let count = vector(&mut s, vectors::CNP, 0, &[0, handle]);
    assert_eq!((count.r[1], count.r[2]), (300 % 256, 1), "bytes held");
    let room = vector(&mut s, vectors::CNP, psr::C, &[0, handle]);
    assert_eq!((room.r[1], room.r[2]), (299 % 256, 1), "free space");
    purge(&mut s, handle);
    assert_eq!(
        info(&mut s, handle)[3..7],
        [0, 0, 599, 0],
        "emptied, offsets at nought"
    );
}

#[test]
fn the_first_byte_into_a_dormant_buffer_wakes_its_device() {
    let mut s = plain();
    recorder(&mut s, WAKE, &[0, 8, 12]);
    recorder(&mut s, OWNER, &[]);
    let handle = create(&mut s, 0, 8);
    ok(&mut s, LINK_DEVICE, &[handle, WAKE, OWNER, 0x1234, 0x5678]);
    assert!(insert(&mut s, handle, b'a'));
    let woken = logged(&mut s);
    assert_eq!(woken[..3], [handle, 0x1234, 0x5678]);
    assert_eq!(
        entered(woken[3]),
        (SUPERVISOR_32, true),
        "SVC, interrupts off"
    );
    assert_eq!(flags(&mut s, handle) & flag::AWAKE, flag::AWAKE);
    assert!(insert(&mut s, handle, b'b'));
    assert!(logged(&mut s).is_empty(), "awake already");
    remove(&mut s, handle, false);
    remove(&mut s, handle, false);
    assert_eq!(
        flags(&mut s, handle) & flag::AWAKE,
        0,
        "emptied: dormant again"
    );
    assert_eq!(insert_block(&mut s, handle, b""), (BYTES, 0, false));
    assert!(logged(&mut s).is_empty(), "nothing went in");
    insert_block(&mut s, handle, b"xyz");
    assert_eq!(logged(&mut s).len(), 4, "a block wakes it once");
    purge(&mut s, handle);
    assert_eq!(
        flags(&mut s, handle) & flag::AWAKE,
        0,
        "a purge sends it to sleep"
    );
    insert(&mut s, handle, b'q');
    assert_eq!(logged(&mut s).len(), 4);
}

#[test]
fn a_new_link_asks_the_owner_and_a_refusal_keeps_the_link() {
    let mut s = plain();
    recorder(&mut s, OWNER, &[0, 1, 2, 3, 8, 12]);
    refuser(&mut s);
    let handle = create(&mut s, 0, 8);
    ok(&mut s, LINK_DEVICE, &[handle, 0, OWNER, 0x1111, 0x2222]);
    assert!(logged(&mut s).is_empty(), "nobody to ask");
    ok(&mut s, LINK_DEVICE, &[handle, 0, REFUSER, 0x3333, 0x4444]);
    assert_eq!(
        logged(&mut s)[..6],
        [handle, 0, REFUSER, 0x3333, 0x1111, 0x2222],
        "the call's registers with the owner's own"
    );
    assert_eq!(
        refused(&mut s, LINK_DEVICE, &[handle, 0, OWNER, 0, 0]),
        REFUSAL_NUMBER
    );
    assert_eq!(refused(&mut s, REMOVE, &[handle]), REFUSAL_NUMBER);
    insert(&mut s, handle, b'a');
    ok(&mut s, UNLINK_DEVICE, &[handle]);
    assert_eq!(info(&mut s, handle)[6], 0, "unlinking purges");
    ok(&mut s, UNLINK_DEVICE, &[handle]);
    ok(&mut s, LINK_DEVICE, &[handle, 0, 0, 0, 0]);
    assert_eq!(
        refused(&mut s, LINK_DEVICE, &[handle, 0, 0, 0, 0]),
        OWNER_STAYS.number
    );
    assert_eq!(refused(&mut s, REMOVE, &[handle]), OWNER_STAYS.number);
    ok(&mut s, UNLINK_DEVICE, &[handle]);
    ok(&mut s, REMOVE, &[handle]);
}

#[test]
fn input_full_and_output_empty_are_raised_with_risc_os_5_s_registers() {
    let mut s = plain();
    for event in [EVENT_OUTPUT_EMPTY, EVENT_INPUT_FULL] {
        ok(&mut s, BYTE, &[14, event]);
    }
    recorder(&mut s, CLAIMANT, &[0, 1, 2, 3]);
    ok(&mut s, CLAIM, &[vectors::EVENT.0, CLAIMANT, 0]);
    let handle = create(&mut s, flag::INPUT_FULL | flag::OUTPUT_EMPTY, 4);
    insert_block(&mut s, handle, b"abc");
    assert!(logged(&mut s).is_empty());
    assert!(!insert(&mut s, handle, b'd'));
    assert_eq!(
        logged(&mut s)[..3],
        [EVENT_INPUT_FULL, handle, u32::from(b'd')]
    );
    insert_block(&mut s, handle, b"ef");
    assert_eq!(
        logged(&mut s)[..4],
        [EVENT_INPUT_FULL, handle, BYTES, 2],
        "the handle without its block bit, what was not taken"
    );
    remove(&mut s, handle, false);
    remove(&mut s, handle, true);
    assert!(logged(&mut s).is_empty());
    remove(&mut s, handle, false);
    remove(&mut s, handle, false);
    assert_eq!(
        logged(&mut s)[..3],
        [EVENT_OUTPUT_EMPTY, handle, u32::from(b'c')]
    );
    remove(&mut s, handle, false);
    assert!(logged(&mut s).is_empty(), "not from an empty buffer");
    insert_block(&mut s, handle, b"gh");
    purge(&mut s, handle);
    assert!(logged(&mut s).is_empty(), "not on a purge");
    insert_block(&mut s, handle, b"ij");
    let (end, left, _, _) = remove_block(&mut s, handle, 3, false);
    assert_eq!(logged(&mut s)[..4], [EVENT_OUTPUT_EMPTY, handle, end, left]);
}

#[test]
fn upcalls_8_and_9_mark_each_crossing_of_the_threshold() {
    let mut s = plain();
    recorder(&mut s, CLAIMANT, &[0, 1, 2]);
    ok(&mut s, CLAIM, &[vectors::UPCALL.0, CLAIMANT, 0]);
    let handle = create(&mut s, flag::UPCALLS, 9);
    ok(&mut s, THRESHOLD, &[handle, 4]);
    insert_block(&mut s, handle, b"abcd");
    assert!(
        logged(&mut s).is_empty(),
        "free space at the threshold is not below it"
    );
    insert(&mut s, handle, b'e');
    assert_eq!(logged(&mut s)[..3], [upcall::BUFFER_FILLING, handle, 0]);
    insert(&mut s, handle, b'f');
    remove(&mut s, handle, false);
    assert!(logged(&mut s).is_empty(), "no crossing");
    remove(&mut s, handle, false);
    assert_eq!(
        logged(&mut s)[..3],
        [upcall::BUFFER_EMPTYING, handle, u32::MAX]
    );
    insert_block(&mut s, handle, b"gh");
    assert_eq!(logged(&mut s).len(), 4, "a block crosses once");
    purge(&mut s, handle);
    assert_eq!(
        logged(&mut s)[..3],
        [upcall::BUFFER_EMPTYING, handle, u32::MAX],
        "a purge too"
    );
    ok(&mut s, THRESHOLD, &[handle, 0]);
    insert_block(&mut s, handle, b"12345678");
    purge(&mut s, handle);
    assert!(logged(&mut s).is_empty(), "no threshold, no UpCalls");
    ok(&mut s, THRESHOLD, &[handle, 4]);
    ok(&mut s, MODIFY_FLAGS, &[handle, 0, !flag::UPCALLS]);
    insert_block(&mut s, handle, b"12345678");
    assert!(logged(&mut s).is_empty(), "nor without the flag");
}

/// The service routine called for the buffer with internal ID `id`, as a
/// device driver calls it: R2 to R3 and C.
fn serve(s: &mut Session, reason: u32, id: u32, r2: u32, r3: u32) -> Result<(u32, u32, bool), u32> {
    let mut os = s.kernel.os();
    let routine = os.routine_of(&BUFFER_MANAGER, SERVICE).expect("active");
    let mut regs = Regs::new([
        reason,
        id,
        r2,
        r3,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        os.world.buffers.workspace,
    ]);
    match os.call_routine(
        routine,
        crate::memory::Bits::Bit32,
        Entry::Supervisor,
        &mut regs,
    ) {
        Ok(()) => {
            assert_eq!(regs.r[..2], [reason, id], "R0 and R1 kept");
            Ok((regs.r[2], regs.r[3], regs.flag(psr::C)))
        }
        Err(Fail::Error(block)) => Err(s.peek(block)),
        Err(Fail::Unwind(unwind)) => panic!("unwound: {unwind:?}"),
    }
}

#[test]
fn the_service_routine_moves_bytes_by_internal_id_without_the_vectors() {
    let mut s = plain();
    let handle = create(&mut s, 0, 8);
    let id = ok(&mut s, INTERNAL_INFO, &[handle]).r[0];
    assert_eq!(
        serve(&mut s, reason::INSERT_BYTE, id, u32::from(b'q'), 0),
        Ok((u32::from(b'q'), 0, false))
    );
    assert_eq!(serve(&mut s, reason::USED, id, 0, 0).map(|r| r.0), Ok(1));
    assert_eq!(serve(&mut s, reason::FREE, id, 0, 0).map(|r| r.0), Ok(6));
    assert_eq!(
        serve(&mut s, reason::EXAMINE_BYTE, id, 0, 0),
        Ok((u32::from(b'q'), 0, false))
    );
    assert_eq!(
        serve(&mut s, reason::REMOVE_BYTE, id, 0, 0),
        Ok((u32::from(b'q'), 0, false))
    );
    assert_eq!(
        serve(&mut s, reason::REMOVE_BYTE, id, 77, 0),
        Ok((77, 0, true)),
        "empty: R2 kept"
    );
    s.write(BYTES, b"abcdefghij");
    assert_eq!(
        serve(&mut s, reason::INSERT_BLOCK, id, BYTES, 10),
        Ok((BYTES + 7, 3, true))
    );
    assert_eq!(
        serve(&mut s, reason::INSERT_BLOCK, id, BYTES, 2),
        Ok((BYTES, 2, true)),
        "full"
    );
    assert_eq!(
        serve(&mut s, reason::INSERT_BYTE, id, 0, 0).map(|r| r.2),
        Ok(true)
    );
    assert_eq!(
        serve(&mut s, reason::EXAMINE_BLOCK, id, OUT, 3),
        Ok((OUT + 3, 0, false))
    );
    assert_eq!(
        serve(&mut s, reason::REMOVE_BLOCK, id, OUT, 5),
        Ok((OUT + 5, 0, false))
    );
    assert_eq!(s.read(OUT, 5), b"abcde");
    s.write(BYTES, b"klmno");
    serve(&mut s, reason::INSERT_BLOCK, id, BYTES, 5).expect("in");
    let start = info(&mut s, handle)[1];
    assert_eq!(
        serve(&mut s, reason::NEXT_FILLED, id, 0, 0),
        Ok((start + 6, 2, false)),
        "from the removal point to the memory's end"
    );
    assert_eq!(
        serve(&mut s, reason::EXAMINE_BLOCK, id, OUT, 7),
        Ok((OUT + 7, 0, false))
    );
    assert_eq!(s.read(OUT, 7), b"fgklmno", "across the wrap");
    assert_eq!(
        serve(&mut s, reason::PURGE, id, 0, 0).map(|r| r.2),
        Ok(false)
    );
    assert_eq!(serve(&mut s, reason::USED, id, 0, 0).map(|r| r.0), Ok(0));
    assert_eq!(
        serve(&mut s, reason::NEXT_FILLED + 1, id, 0, 0),
        Err(crate::kheap::UNKNOWN_REASON.number)
    );
    assert_eq!(
        serve(&mut s, reason::USED, id + 4, 0, 0),
        Err(NOT_KNOWN.number)
    );
}

#[test]
fn the_kernel_s_own_buffers_and_handles_nobody_has_answer_as_the_kernel_does() {
    let mut s = plain();
    assert!(insert(&mut s, 0, b'k'), "the keyboard buffer");
    assert_eq!(
        remove(&mut s, 0, true),
        Some(b'k'),
        "examined with V clear after"
    );
    let count = vector(&mut s, vectors::CNP, 0, &[0, 0]);
    assert_eq!((count.r[1], count.r[2]), (1, 0));
    purge(&mut s, 0);
    assert_eq!(remove(&mut s, 0, false), None, "purged with V clear after");
    let unknown = vector(&mut s, vectors::INS, 0, &[b'z'.into(), 0x777]);
    assert!(
        unknown.flag(psr::C) && !unknown.flag(psr::V),
        "nobody took it"
    );
    let unknown = vector(&mut s, vectors::REM, 0, &[0x55, 0x777, 0x66]);
    assert!(
        !unknown.flag(psr::C) && !unknown.flag(psr::V),
        "C clear, as RISC OS 5 leaves it"
    );
    assert_eq!((unknown.r[0], unknown.r[2]), (0x55, 0x66), "registers kept");
    let unknown = vector(&mut s, vectors::CNP, psr::V, &[0, 0x777, 0x66]);
    assert!(!unknown.flag(psr::V));
    assert_eq!(unknown.r[1..3], [0x777, 0x66], "registers kept");
}

#[test]
fn os_byte_138_passes_a_handle_of_the_manager_s_whole() {
    let mut s = plain();
    let handle = create(&mut s, 0, 8);
    assert!(handle > 0xFF);
    ok(&mut s, BYTE, &[138, handle, u32::from(b'w')]);
    assert_eq!(remove(&mut s, handle, false), Some(b'w'));
}

#[test]
fn a_killed_manager_asks_its_owners_and_a_refusal_keeps_it_running() {
    let mut s = plain();
    refuser(&mut s);
    let handle = create(&mut s, 0, 8);
    ok(&mut s, LINK_DEVICE, &[handle, 0, REFUSER, 0, 0]);
    assert!(
        cli(&mut s, "RMKill BufferManager").is_err(),
        "the owner kept it"
    );
    assert_eq!(flags(&mut s, PRINTER), 2, "still running");
    ok(&mut s, UNLINK_DEVICE, &[handle]);
    cli(&mut s, "RMKill BufferManager").expect("killed");
    assert!(
        insert(&mut s, 0, b'k'),
        "the keyboard's is the kernel's still"
    );
    assert!(!insert(&mut s, handle, b'x'), "its buffers went with it");
    cli(&mut s, "RMReInit BufferManager").expect("started again");
    assert_eq!(
        ok(&mut s, INTERNAL_INFO, &[PRINTER]).r[0],
        0x84,
        "made afresh"
    );
    assert_eq!(create(&mut s, 0, 8), FIRST_HANDLE);
}
