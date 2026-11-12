use super::super::run_until;
use super::super::tests::{
    booted, call, mask_every_interrupt, poke, runs_end_where_asked, start, stopped_at,
};
use crate::asm::{Asm, Reg16};
use crate::cpu::{Bus, Regs, Seg};
use crate::machine::{Machine, Template};
use crate::time::NANOS_PER_MILLISECOND as MS;
use alloc::vec::Vec;

/// The registers as `Regs` numbers them.
const AX: u8 = 0;
const CX: u8 = 1;
const DX: u8 = 2;
const BX: u8 = 3;
const SI: u8 = 6;
const DI: u8 = 7;
const AL: u8 = 0;
const BL: u8 = 3;
const AH: u8 = 4;
const FRAME: u32 = 0xD_0000;
/// Where the tests keep their structures and buffers.
const STRUCT: u16 = 0x0900;
const LIST: u16 = 0x0920;
const OLD_LIST: u16 = 0x0930;
const BUFFER: u16 = 0x0A00;

fn ems(m: &mut Machine, set: impl FnOnce(&mut Regs)) -> Regs {
    call(m, 0x67, |r| {
        r.set_sreg(Seg::Ds, 0);
        r.set_sreg(Seg::Es, 0);
        set(r);
    })
}

/// Calls function `ax` with BX and DX set; returns its status.
fn function(m: &mut Machine, ax: u16, bx: u16, dx: u16) -> Regs {
    ems(m, |r| {
        r.set16(AX, ax);
        r.set16(BX, bx);
        r.set16(DX, dx);
    })
}

fn status(r: &Regs) -> u8 {
    r.get8(AH)
}

fn peek(m: &Machine, at: u32) -> u8 {
    m.board().peek(at)
}

fn word(m: &Machine, at: u32) -> u16 {
    u16::from_le_bytes([peek(m, at), peek(m, at + 1)])
}

fn allocate(m: &mut Machine, pages: u16) -> u16 {
    let r = function(m, 0x4300, pages, 0);
    assert_eq!(status(&r), 0);
    r.get16(DX)
}

fn map(m: &mut Machine, handle: u16, logical: u16, window: u16) -> u8 {
    status(&function(m, 0x4400 | window, logical, handle))
}

/// A board's machine with a handle of `pages`, page `n` holding `0xA0 + n`
/// at its first byte, and window 0 left showing page 0.
fn marked(pages: u16) -> (Machine, u16) {
    let mut m = booted(Template::TurboXt, None);
    let handle = allocate(&mut m, pages);
    for n in (0..pages).rev() {
        assert_eq!(map(&mut m, handle, n, 0), 0);
        poke(&mut m, FRAME, &[0xA0 + u8::try_from(n).unwrap()]);
    }
    (m, handle)
}

#[test]
fn the_post_installs_the_manager_where_programs_look_for_it() {
    let mut m = booted(Template::TurboXt, None);
    assert_eq!((word(&m, 0x19C), word(&m, 0x19E)), (0x0012, 0xF000));
    let name: Vec<u8> = (0..8).map(|i| peek(&m, 0xF_000A + i)).collect();
    assert_eq!(name, b"EMMXXXX0");
    assert_eq!(status(&function(&mut m, 0x4000, 0, 0)), 0);
    assert_eq!(function(&mut m, 0x4100, 0, 0).get16(BX), 0xD000);
    let pages = function(&mut m, 0x4200, 0, 0);
    assert_eq!((pages.get16(BX), pages.get16(DX)), (128, 128));
    assert_eq!(function(&mut m, 0x4600, 0, 0).get8(AL), 0x40);
    assert_eq!(peek(&m, FRAME), 0xFF, "every window empty");
    let bare = booted(Template::IbmXt, None);
    assert_eq!(word(&bare, 0x19E), 0, "no manager without a board");
    assert_ne!(peek(&bare, 0xF_000A), b'E');
}

#[test]
fn pages_are_allocated_mapped_unmapped_and_freed() {
    let (mut m, handle) = marked(4);
    assert_eq!(handle, 1, "the first after the system's handle 0");
    assert_eq!(function(&mut m, 0x4200, 0, 0).get16(BX), 124);
    assert_eq!(peek(&m, FRAME), 0xA0);
    assert_eq!(map(&mut m, handle, 2, 3), 0);
    assert_eq!(peek(&m, FRAME + 0xC000), 0xA2);
    assert_eq!(map(&mut m, handle, 4, 0), 0x8A, "past its pages");
    assert_eq!(map(&mut m, handle, 0, 4), 0x8B, "no fifth window");
    assert_eq!(map(&mut m, 9, 0, 0), 0x83, "no such handle");
    assert_eq!(map(&mut m, handle, 0xFFFF, 3), 0);
    assert_eq!(peek(&m, FRAME + 0xC000), 0xFF, "unmapped");
    assert_eq!(function(&mut m, 0x4C00, 0, handle).get16(BX), 4);
    assert_eq!(status(&function(&mut m, 0x4500, 0, handle)), 0);
    assert_eq!(peek(&m, FRAME), 0xFF, "its pages leave the frame with it");
    assert_eq!(function(&mut m, 0x4200, 0, 0).get16(BX), 128);
    assert_eq!(status(&function(&mut m, 0x4C00, 0, handle)), 0x83);
}

#[test]
fn allocation_refuses_zero_too_many_too_few_and_past_the_last_handle() {
    let mut m = booted(Template::TurboXt, None);
    assert_eq!(status(&function(&mut m, 0x4300, 0, 0)), 0x89);
    assert_eq!(status(&function(&mut m, 0x4300, 129, 0)), 0x87);
    allocate(&mut m, 128);
    assert_eq!(status(&function(&mut m, 0x4300, 1, 0)), 0x88);
    for _ in 2..=254 {
        assert_eq!(
            status(&function(&mut m, 0x5A00, 0, 0)),
            0,
            "a handle with no pages"
        );
    }
    assert_eq!(status(&function(&mut m, 0x5A00, 0, 0)), 0x85);
    assert_eq!(function(&mut m, 0x4B00, 0, 0).get16(BX), 255);
    assert_eq!(
        status(&function(&mut m, 0x4500, 0, 0)),
        0,
        "the system's handle is emptied"
    );
    assert_eq!(function(&mut m, 0x4B00, 0, 0).get16(BX), 255, "but kept");
}

#[test]
fn a_handles_map_is_saved_once_and_restored_once() {
    let (mut m, handle) = marked(2);
    assert_eq!(status(&function(&mut m, 0x4700, 0, handle)), 0);
    assert_eq!(status(&function(&mut m, 0x4700, 0, handle)), 0x8D);
    map(&mut m, handle, 1, 0);
    assert_eq!(
        status(&function(&mut m, 0x4500, 0, handle)),
        0x86,
        "a saved map pending"
    );
    assert_eq!(status(&function(&mut m, 0x4800, 0, handle)), 0);
    assert_eq!(peek(&m, FRAME), 0xA0);
    assert_eq!(status(&function(&mut m, 0x4800, 0, handle)), 0x8E);
}

#[test]
fn the_whole_page_map_is_saved_and_a_corrupt_one_refused() {
    let (mut m, handle) = marked(2);
    assert_eq!(function(&mut m, 0x4E03, 0, 0).get8(AL), 10);
    let saved = ems(&mut m, |r| {
        r.set16(AX, 0x4E00);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&saved), 0);
    map(&mut m, handle, 1, 0);
    let restore = |m: &mut Machine| {
        status(&ems(m, |r| {
            r.set16(AX, 0x4E01);
            r.set16(SI, BUFFER);
        }))
    };
    assert_eq!(restore(&mut m), 0);
    assert_eq!(peek(&m, FRAME), 0xA0);
    let byte = peek(&m, u32::from(BUFFER));
    poke(&mut m, u32::from(BUFFER), &[byte ^ 1]);
    map(&mut m, handle, 1, 0);
    assert_eq!(restore(&mut m), 0xA3);
    assert_eq!(peek(&m, FRAME), 0xA1, "nothing applied");
}

#[test]
fn a_partial_map_saves_the_windows_it_names() {
    let (mut m, handle) = marked(2);
    assert_eq!(function(&mut m, 0x4F02, 2, 0).get8(AL), 12);
    assert_eq!(status(&function(&mut m, 0x4F02, 5, 0)), 0x8B);
    map(&mut m, handle, 1, 3);
    poke(&mut m, u32::from(STRUCT), &[2, 0, 0x00, 0xD0, 0x00, 0xDC]);
    let get = ems(&mut m, |r| {
        r.set16(AX, 0x4F00);
        r.set16(SI, STRUCT);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&get), 0);
    map(&mut m, handle, 0xFFFF, 0);
    map(&mut m, handle, 0xFFFF, 3);
    let set = ems(&mut m, |r| {
        r.set16(AX, 0x4F01);
        r.set16(SI, BUFFER);
    });
    assert_eq!(status(&set), 0);
    assert_eq!((peek(&m, FRAME), peek(&m, FRAME + 0xC000)), (0xA0, 0xA1));
    poke(&mut m, u32::from(STRUCT), &[1, 0, 0x00, 0xD2]);
    let bad = ems(&mut m, |r| {
        r.set16(AX, 0x4F00);
        r.set16(SI, STRUCT);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&bad), 0x8B, "D200h is no window");
}

#[test]
fn several_pages_map_at_once_by_number_or_segment_up_to_the_first_bad_one() {
    let (mut m, handle) = marked(3);
    poke(&mut m, u32::from(LIST), &[1, 0, 1, 0, 2, 0, 3, 0]);
    let by_number = ems(&mut m, |r| {
        r.set16(AX, 0x5000);
        r.set16(CX, 2);
        r.set16(DX, handle);
        r.set16(SI, LIST);
    });
    assert_eq!(status(&by_number), 0);
    assert_eq!(
        (peek(&m, FRAME + 0x4000), peek(&m, FRAME + 0xC000)),
        (0xA1, 0xA2)
    );
    poke(
        &mut m,
        u32::from(LIST),
        &[0, 0, 0x00, 0xD4, 2, 0, 0x00, 0xD3],
    );
    let by_segment = ems(&mut m, |r| {
        r.set16(AX, 0x5001);
        r.set16(CX, 2);
        r.set16(DX, handle);
        r.set16(SI, LIST);
    });
    assert_eq!(status(&by_segment), 0x8B, "D300h is no window");
    assert_eq!(
        peek(&m, FRAME + 0x4000),
        0xA0,
        "the entry before it was mapped"
    );
    let too_many = ems(&mut m, |r| {
        r.set16(AX, 0x5000);
        r.set16(CX, 5);
        r.set16(DX, handle);
        r.set16(SI, LIST);
    });
    assert_eq!(status(&too_many), 0x8B);
}

#[test]
fn reallocation_grows_and_shrinks_and_reports_the_old_count_when_refused() {
    let (mut m, handle) = marked(2);
    map(&mut m, handle, 1, 1);
    let grown = function(&mut m, 0x5100, 5, handle);
    assert_eq!((status(&grown), grown.get16(BX)), (0, 5));
    assert_eq!(peek(&m, FRAME), 0xA0, "kept pages keep their contents");
    let shrunk = function(&mut m, 0x5100, 1, handle);
    assert_eq!((status(&shrunk), shrunk.get16(BX)), (0, 1));
    assert_eq!(
        peek(&m, FRAME + 0x4000),
        0xFF,
        "a freed page leaves the frame"
    );
    let refused = function(&mut m, 0x5100, 200, handle);
    assert_eq!((status(&refused), refused.get16(BX)), (0x87, 1));
    let emptied = function(&mut m, 0x5100, 0, handle);
    assert_eq!((status(&emptied), emptied.get16(BX)), (0, 0));
    assert_eq!(
        function(&mut m, 0x4C00, 0, handle).get16(BX),
        0,
        "still its handle"
    );
}

#[test]
fn handles_are_named_uniquely_and_found_by_name() {
    let mut m = booted(Template::TurboXt, None);
    let first = allocate(&mut m, 1);
    let second = allocate(&mut m, 1);
    poke(&mut m, u32::from(STRUCT), b"OVERLAYS");
    let name = |m: &mut Machine, handle| {
        status(&ems(m, |r| {
            r.set16(AX, 0x5301);
            r.set16(DX, handle);
            r.set16(SI, STRUCT);
        }))
    };
    assert_eq!(name(&mut m, first), 0);
    assert_eq!(name(&mut m, second), 0xA1, "taken");
    let read = ems(&mut m, |r| {
        r.set16(AX, 0x5300);
        r.set16(DX, first);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&read), 0);
    let got: Vec<u8> = (0..8).map(|i| peek(&m, u32::from(BUFFER) + i)).collect();
    assert_eq!(got, b"OVERLAYS");
    let found = ems(&mut m, |r| {
        r.set16(AX, 0x5401);
        r.set16(SI, STRUCT);
    });
    assert_eq!((status(&found), found.get16(DX)), (0, first));
    poke(&mut m, u32::from(STRUCT), b"NOTTHERE");
    assert_eq!(
        status(&ems(&mut m, |r| {
            r.set16(AX, 0x5401);
            r.set16(SI, STRUCT);
        })),
        0xA0
    );
    poke(&mut m, u32::from(STRUCT), &[0; 8]);
    assert_eq!(
        status(&ems(&mut m, |r| {
            r.set16(AX, 0x5401);
            r.set16(SI, STRUCT);
        })),
        0xA1,
        "a null name names nothing"
    );
    let listed = ems(&mut m, |r| {
        r.set16(AX, 0x5400);
        r.set16(DI, BUFFER);
    });
    assert_eq!(listed.get8(AL), 3, "the system's handle and both");
    assert_eq!(word(&m, u32::from(BUFFER) + 10), first);
    assert_eq!(function(&mut m, 0x5402, 0, 0).get16(BX), 255);
}

#[test]
fn handles_are_volatile_only() {
    let mut m = booted(Template::TurboXt, None);
    let handle = allocate(&mut m, 1);
    assert_eq!(function(&mut m, 0x5200, 0, handle).get8(AL), 0);
    let set = |m: &mut Machine, attribute| {
        status(&ems(m, |r| {
            r.set16(AX, 0x5201);
            r.set16(DX, handle);
            r.set8(BL, attribute);
        }))
    };
    assert_eq!(set(&mut m, 0), 0);
    assert_eq!(set(&mut m, 1), 0x91);
    assert_eq!(set(&mut m, 2), 0x90);
    assert_eq!(function(&mut m, 0x5202, 0, 0).get8(AL), 0);
}

/// A move or exchange structure: length, then each side's type, handle,
/// offset and segment or page.
fn region(len: u32, source: [u16; 4], destination: [u16; 4]) -> Vec<u8> {
    let mut bytes = len.to_le_bytes().to_vec();
    for side in [source, destination] {
        bytes.push(u8::try_from(side[0]).unwrap());
        for field in &side[1..] {
            bytes.extend(field.to_le_bytes());
        }
    }
    bytes
}

fn moved(m: &mut Machine, exchange: bool, len: u32, source: [u16; 4], destination: [u16; 4]) -> u8 {
    poke(m, u32::from(STRUCT), &region(len, source, destination));
    status(&ems(m, |r| {
        r.set16(AX, 0x5700 | u16::from(exchange));
        r.set16(SI, STRUCT);
    }))
}

#[test]
fn regions_move_and_exchange_between_conventional_and_expanded_memory() {
    let (mut m, handle) = marked(3);
    let text: Vec<u8> = (0..40_000u32)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    poke(&mut m, 0x2_0000, &text);
    let before = m.now();
    assert_eq!(
        moved(
            &mut m,
            false,
            40_000,
            [0, 0, 0, 0x2000],
            [1, handle, 0x10, 0]
        ),
        0
    );
    assert!(m.now() - before >= 40 * MS, "a move takes its time");
    map(&mut m, handle, 2, 1);
    assert_eq!(
        peek(&m, FRAME + 0x4000 + (0x10 + 39_999 - 0x8000)),
        text[39_999],
        "across both page boundaries"
    );
    assert_eq!(
        moved(
            &mut m,
            false,
            40_000,
            [1, handle, 0x10, 0],
            [0, 0, 0, 0x3000]
        ),
        0
    );
    let back: Vec<u8> = (0..40_000).map(|n| peek(&m, 0x3_0000 + n)).collect();
    assert_eq!(back, text);
    poke(&mut m, 0x4_0000, &[1, 2, 3, 4]);
    poke(&mut m, 0x4_0010, &[5, 6, 7, 8]);
    assert_eq!(
        moved(&mut m, true, 4, [0, 0, 0, 0x4000], [0, 0, 0x10, 0x4000]),
        0
    );
    assert_eq!((peek(&m, 0x4_0000), peek(&m, 0x4_0010)), (5, 1));
}

/// Starts a move of `len` bytes from `2000:0000` into the handle's pages,
/// every interrupt masked; answers where its program halts.
fn begin_move(m: &mut Machine, handle: u16, len: u32) -> core::ops::Range<u16> {
    poke(
        m,
        u32::from(STRUCT),
        &region(len, [0, 0, 0, 0x2000], [1, handle, 0, 0]),
    );
    let end = start(
        m,
        |asm| asm.int(0x67),
        |r| {
            r.set_sreg(Seg::Ds, 0);
            r.set16(AX, 0x5700);
            r.set16(SI, STRUCT);
        },
    );
    mask_every_interrupt(m);
    end
}

#[test]
fn a_moves_time_passes_a_run_at_a_time_and_no_run_passes_its_end() {
    let (mut m, handle) = marked(3);
    let end = begin_move(&mut m, handle, 40_000);
    let begun = m.now();
    assert!(
        runs_end_where_asked(&mut m, &end, MS, 1000),
        "the move ended"
    );
    assert!(m.now() - begun >= 40 * MS, "and took its time");
    assert_eq!(status(m.cpu().regs()), 0);
}

#[test]
fn a_wait_the_guest_wrote_over_holds_no_run_past_its_end() {
    let (mut m, handle) = marked(3);
    let end = begin_move(&mut m, handle, 40_000);
    assert!(!runs_end_where_asked(&mut m, &end, MS, 1), "the move waits");
    // The stub's state sits at SS:SP; its first eight bytes are the move's
    // deadline, now the end of time.
    let regs = *m.cpu().regs();
    let state = crate::cpu::linear(regs.sreg(Seg::Ss), regs.sp());
    poke(&mut m, state, &[0xFF; 8]);
    assert!(
        !runs_end_where_asked(&mut m, &end, MS, 100),
        "never done, never holding a run"
    );
}

#[test]
fn an_overlapping_move_completes_and_says_so_where_an_exchange_refuses() {
    let (mut m, handle) = marked(2);
    poke(&mut m, 0x2_0000, &[1, 2, 3, 4, 5, 6]);
    assert_eq!(
        moved(&mut m, false, 4, [0, 0, 0, 0x2000], [0, 0, 2, 0x2000]),
        0x92
    );
    let after: Vec<u8> = (0..6).map(|n| peek(&m, 0x2_0000 + n)).collect();
    assert_eq!(after, [1, 2, 1, 2, 3, 4], "an intact copy of the source");
    assert_eq!(
        moved(&mut m, true, 4, [0, 0, 0, 0x2000], [0, 0, 2, 0x2000]),
        0x97
    );
    assert_eq!(
        moved(&mut m, false, 100, [1, handle, 0, 0], [1, handle, 50, 0]),
        0x92
    );
    assert_eq!(
        moved(&mut m, true, 100, [1, handle, 0, 0], [1, handle, 50, 0]),
        0x97
    );
}

#[test]
fn a_move_refuses_what_the_specification_forbids() {
    let (mut m, handle) = marked(2);
    let conventional = [0, 0, 0, 0x2000];
    assert_eq!(
        moved(&mut m, false, 0x10_0001, conventional, [1, handle, 0, 0]),
        0x96
    );
    assert_eq!(
        moved(&mut m, false, 16, conventional, [1, handle, 0x4000, 0]),
        0x95
    );
    assert_eq!(moved(&mut m, false, 16, [2, 0, 0, 0], conventional), 0x98);
    assert_eq!(
        moved(&mut m, false, 0x20, [0, 0, 0xFFF0, 0xFFFF], conventional),
        0xA2
    );
    assert_eq!(
        moved(&mut m, false, 0x8001, conventional, [1, handle, 0, 0]),
        0x93
    );
    assert_eq!(
        moved(&mut m, false, 16, conventional, [1, handle, 0, 2]),
        0x8A
    );
    assert_eq!(moved(&mut m, false, 16, conventional, [1, 9, 0, 0]), 0x83);
    assert_eq!(
        moved(&mut m, false, 16, [0, 0, 0, 0xD000], [1, handle, 0, 0]),
        0x94,
        "window 0 shows page 0"
    );
    assert_eq!(
        moved(&mut m, false, 0, conventional, [1, 9, 0, 0]),
        0,
        "nothing to move"
    );
}

#[test]
fn map_and_jump_lands_on_its_target_with_the_new_map() {
    let (mut m, handle) = marked(2);
    let mut target = Asm::new(0x0700);
    target.mov_imm16(Reg16::Dx, 0xD000);
    target.mov_to_seg(Seg::Ds, Reg16::Dx);
    target.bytes(&[0x8A, 0x1E, 0x00, 0x00]);
    target.cli();
    let halt = target.here();
    let spin = target.label();
    target.bind(&spin);
    target.hlt();
    target.jmp_short(&spin);
    poke(&mut m, 0x0700, &target.finish().unwrap());
    poke(
        &mut m,
        u32::from(STRUCT),
        &[0x00, 0x07, 0, 0, 1, 0x20, 0x09, 0, 0],
    );
    poke(&mut m, u32::from(LIST), &[1, 0, 0, 0]);
    start(
        &mut m,
        |asm| {
            asm.sti();
            asm.int(0x67);
        },
        |r| {
            r.set_sreg(Seg::Ds, 0);
            r.set16(AX, 0x5500);
            r.set16(DX, handle);
            r.set16(SI, STRUCT);
        },
    );
    let now = m.now();
    run_until(&mut m, now + 100 * MS);
    assert!(stopped_at(&m, 0, halt..halt + 3), "{:X?}", m.cpu().regs());
    let regs = m.cpu().regs();
    assert_eq!(
        (regs.get8(AH), regs.get8(BL)),
        (0, 0xA1),
        "it read page 1 through window 0"
    );
}

#[test]
fn map_and_call_maps_for_the_routine_and_restores_the_old_map_after() {
    let (mut m, handle) = marked(2);
    assert_eq!(function(&mut m, 0x5602, 0, 0).get16(BX), 32);
    let mut routine = Asm::new(0x0700);
    routine.mov_imm16(Reg16::Ax, 0xD000);
    routine.mov_to_seg(Seg::Ds, Reg16::Ax);
    routine.bytes(&[0x8A, 0x1E, 0x00, 0x00]);
    routine.retf();
    poke(&mut m, 0x0700, &routine.finish().unwrap());
    let mut call = [0u8; 22];
    call[..4].copy_from_slice(&[0x00, 0x07, 0, 0]);
    call[4] = 1;
    call[5..9].copy_from_slice(&[0x20, 0x09, 0, 0]);
    call[9] = 1;
    call[10..14].copy_from_slice(&[0x30, 0x09, 0, 0]);
    poke(&mut m, u32::from(STRUCT), &call);
    poke(&mut m, u32::from(LIST), &[1, 0, 0, 0]);
    poke(&mut m, u32::from(OLD_LIST), &[0, 0, 0, 0]);
    let regs = ems(&mut m, |r| {
        r.set16(AX, 0x5600);
        r.set16(DX, handle);
        r.set16(SI, STRUCT);
    });
    assert_eq!(regs.get8(AH), 0);
    assert_eq!(regs.get8(BL), 0xA1, "the routine saw page 1");
    assert_eq!(peek(&m, FRAME), 0xA0, "page 0 back after its return");
    assert_eq!(regs.get16(4), 0x7000, "the stack as the caller left it");
}

#[test]
fn the_mappable_windows_hardware_and_raw_pages_are_reported() {
    let (mut m, handle) = marked(1);
    let array = ems(&mut m, |r| {
        r.set16(AX, 0x5800);
        r.set16(DI, BUFFER);
    });
    assert_eq!(array.get16(CX), 4);
    let entries: Vec<u16> = (0..8)
        .map(|n| word(&m, u32::from(BUFFER) + 2 * n))
        .collect();
    assert_eq!(entries, [0xD000, 0, 0xD400, 1, 0xD800, 2, 0xDC00, 3]);
    assert_eq!(function(&mut m, 0x5801, 0, 0).get16(CX), 4);
    let info = ems(&mut m, |r| {
        r.set16(AX, 0x5900);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&info), 0);
    let fields: Vec<u16> = (0..5)
        .map(|n| word(&m, u32::from(BUFFER) + 2 * n))
        .collect();
    assert_eq!(fields, [0x400, 0, 10, 0, 0]);
    let raw = function(&mut m, 0x5901, 0, 0);
    assert_eq!((raw.get16(BX), raw.get16(DX)), (127, 128));
    let raw_handle = function(&mut m, 0x5A01, 2, 0);
    assert_eq!(status(&raw_handle), 0);
    assert_eq!(
        function(&mut m, 0x4C00, 0, raw_handle.get16(DX)).get16(BX),
        2
    );
    assert_eq!(status(&function(&mut m, 0x5A02, 0, 0)), 0x8F);
    assert_eq!(peek(&m, FRAME), 0xA0);
    assert_eq!(status(&function(&mut m, 0x5C00, 0, 0)), 0);
    assert_eq!(peek(&m, FRAME), 0xFF, "warm boot leaves the frame empty");
    assert_eq!(map(&mut m, handle, 0, 0), 0);
}

#[test]
fn set_0_is_the_only_alternate_set_and_keeps_a_save_area_the_system_gives() {
    let (mut m, handle) = marked(2);
    assert_eq!(
        function(&mut m, 0x5B03, 0, 0).get8(BL),
        0,
        "no hardware sets"
    );
    assert_eq!(function(&mut m, 0x5B02, 0, 0).get16(DX), 10);
    ems(&mut m, |r| {
        r.set16(AX, 0x4E00);
        r.set16(DI, BUFFER);
    });
    map(&mut m, handle, 1, 0);
    let set_one = ems(&mut m, |r| {
        r.set16(AX, 0x5B01);
        r.set8(BL, 1);
    });
    assert_eq!(status(&set_one), 0x9C);
    let set = ems(&mut m, |r| {
        r.set16(AX, 0x5B01);
        r.set8(BL, 0);
        r.set16(DI, BUFFER);
    });
    assert_eq!(status(&set), 0);
    assert_eq!(peek(&m, FRAME), 0xA0, "the area's map loaded");
    let got = function(&mut m, 0x5B00, 0, 0);
    assert_eq!(
        (got.get8(BL), got.sreg(Seg::Es), got.get16(DI)),
        (0, 0, BUFFER)
    );
    assert_eq!(
        status(&function(&mut m, 0x5B04, 0, 0)),
        0,
        "freeing set 0 is ignored"
    );
    assert_eq!(
        status(&ems(&mut m, |r| {
            r.set16(AX, 0x5B04);
            r.set8(BL, 2);
        })),
        0x9C
    );
}

#[test]
fn the_operating_systems_functions_open_and_close_under_its_key() {
    let mut m = booted(Template::TurboXt, None);
    let os = |m: &mut Machine, ax: u16, key: (u16, u16)| {
        ems(m, |r| {
            r.set16(AX, ax);
            r.set16(BX, key.0);
            r.set16(CX, key.1);
            r.set16(DI, BUFFER);
        })
    };
    let first = os(&mut m, 0x5D01, (0, 0));
    assert_eq!(status(&first), 0);
    let key = (first.get16(BX), first.get16(CX));
    assert_ne!(key, (0, 0), "the first caller is given a key");
    assert_eq!(status(&os(&mut m, 0x5900, (0, 0))), 0xA4, "closed");
    assert_eq!(status(&os(&mut m, 0x5B00, (0, 0))), 0xA4);
    assert_eq!(
        status(&os(&mut m, 0x5901, (0, 0))),
        0,
        "the raw counts stay open"
    );
    assert_eq!(
        status(&os(&mut m, 0x5D00, (key.0 ^ 1, key.1))),
        0xA4,
        "a wrong key"
    );
    let opened = os(&mut m, 0x5D00, key);
    assert_eq!(
        (status(&opened), opened.get16(BX)),
        (0, key.0),
        "no second key"
    );
    assert_eq!(status(&os(&mut m, 0x5900, (0, 0))), 0);
    assert_eq!(status(&os(&mut m, 0x5D02, key)), 0);
    let again = os(&mut m, 0x5D00, (0, 0));
    assert_eq!(status(&again), 0, "returned to as installed: keyless");
    assert_eq!(
        status(&os(&mut m, 0x5D02, (0, 0))),
        0xA4,
        "the new key is wanted"
    );
}

#[test]
fn undefined_functions_and_subfunctions_are_refused() {
    let mut m = booted(Template::TurboXt, None);
    for ax in [0x4900, 0x4A00, 0x3F00, 0x5E00, 0x6000] {
        assert_eq!(status(&function(&mut m, ax, 0, 0)), 0x84, "{ax:04X}");
    }
    for ax in [
        0x4E04, 0x4F03, 0x5002, 0x5203, 0x5302, 0x5403, 0x5502, 0x5603, 0x5702, 0x5802, 0x5902,
        0x5B09, 0x5D03,
    ] {
        assert_eq!(status(&function(&mut m, ax, 0, 0)), 0x8F, "{ax:04X}");
    }
}
