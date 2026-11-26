//! Squash's SWIs held to the codec they carry and the registers RISC OS 5's
//! module answers with: whole calls and calls in pieces, crossing the host's
//! pieces, the queries, and every refusal. That the codec writes what the
//! module wrote is `lib/compress`'s to show.

use alloc::vec;
use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use super::*;
use crate::memory::layout::{APPLICATION, RISC_OS_5};
use crate::memory::Bits;
use crate::swi::os::SWI_NUMBER_TO_STRING;
use crate::swi::squash::{COMPRESS, DECOMPRESS};
use crate::swi::X;
use crate::testkit::{boot, Session};

/// The slot the tests' buffers lie in.
const SLOT: u32 = 0x10_0000;
const WORK: u32 = APPLICATION + 0x1_0000;
const INPUT: u32 = APPLICATION + 0x2_0000;
const OUTPUT: u32 = APPLICATION + 0x6_0000;
const BACK: u32 = APPLICATION + 0xB_0000;

fn session() -> Session {
    let mut s = boot(&RISC_OS_5);
    s.application(Bits::Bit32, SLOT);
    s
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

fn ok(s: &mut Session, number: u32, low: &[u32]) -> Regs {
    let (regs, _) = s.user_swi(X | number, registers(low));
    assert!(
        !regs.flag(psr::V),
        "SWI &{number:X} {}",
        s.error_text(regs.r[0]).escape_ascii()
    );
    regs
}

fn refused(s: &mut Session, number: u32, low: &[u32]) -> u32 {
    let (regs, _) = s.user_swi(X | number, registers(low));
    assert!(regs.flag(psr::V), "SWI &{number:X} refused");
    s.peek(regs.r[0])
}

/// Text, then noise: a stream that clears its table, longer than the pieces
/// the host carries a call in.
fn plain() -> Vec<u8> {
    const WORDS: [&[u8]; 3] = [b"squash ", b"group ", b"code\n"];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = Vec::new();
    while out.len() < 150_000 {
        out.extend_from_slice(WORDS[usize::try_from(next() % 3).unwrap()]);
    }
    out.extend((0..60_000).map(|_| next().to_le_bytes()[0]));
    out
}

fn len(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).unwrap()
}

#[test]
fn the_queries_answer_squash_s_sizes_and_flags_out_of_shape_are_refused() {
    let mut s = session();
    for (n, answer) in [
        (0, 12),
        (1000, 1512),
        (u32::MAX, u32::MAX),
        (0x8000_0000, 0x4000_000C),
    ] {
        let regs = ok(&mut s, COMPRESS, &[8, n]);
        assert_eq!(regs.r[..2], [0x7C00, answer]);
    }
    let regs = ok(&mut s, DECOMPRESS, &[8, 1000]);
    assert_eq!(regs.r[..2], [0x4400, u32::MAX]);
    for (number, given) in [
        (COMPRESS, 16),
        (COMPRESS, 9),
        (DECOMPRESS, 16),
        (DECOMPRESS, 12),
    ] {
        assert_eq!(
            refused(&mut s, number, &[given, WORK, INPUT, 10, OUTPUT, 1000]),
            0x924
        );
    }
    // Compressing takes the output-fits flag, and makes nothing of it.
    s.write(INPUT, b"0123456789");
    let regs = ok(&mut s, COMPRESS, &[4, WORK, INPUT, 10, OUTPUT, 1000]);
    assert_eq!(regs.r[0], 0);
}

#[test]
fn nothing_compressed_is_a_stream_that_decompresses_to_nothing() {
    let mut s = session();
    let regs = ok(&mut s, COMPRESS, &[0, WORK, INPUT, 0, OUTPUT, 12]);
    assert_eq!(regs.r[..6], [0, WORK, INPUT, 0, OUTPUT + 3, 9]);
    assert_eq!(s.read(OUTPUT, 3), squash::STREAM_MAGIC);
    let regs = ok(&mut s, DECOMPRESS, &[0, WORK, OUTPUT, 3, BACK, 100]);
    assert_eq!(regs.r[..6], [0, WORK, OUTPUT + 3, 0, BACK, 100]);
}

#[test]
fn a_whole_call_crosses_the_host_s_pieces_and_writes_the_codec_s_stream() {
    let mut s = session();
    let plain = plain();
    s.write(INPUT, &plain);
    let room = max_output(len(&plain));
    let regs = ok(
        &mut s,
        COMPRESS,
        &[0, WORK, INPUT, len(&plain), OUTPUT, room],
    );
    let made = regs.r[4] - OUTPUT;
    assert_eq!(regs.r[0], 0);
    assert_eq!(
        regs.r[1..6],
        [WORK, INPUT + len(&plain), 0, OUTPUT + made, room - made]
    );
    let stream = s.read(OUTPUT, made as usize);
    let mut work = vec![0u8; COMPRESS_WORK];
    start_compression(&mut work, Clearing::AtOnce).unwrap();
    let mut expected = vec![0u8; room as usize];
    let progress = compress(&mut work, &plain, false, &mut expected, &mut |_| {}).unwrap();
    assert_eq!(stream, expected[..progress.produced]);

    let regs = ok(
        &mut s,
        DECOMPRESS,
        &[0, WORK, OUTPUT, made, BACK, len(&plain)],
    );
    assert_eq!(
        regs.r[..6],
        [0, WORK, OUTPUT + made, 0, BACK + len(&plain), 0]
    );
    assert_eq!(s.read(BACK, plain.len()), plain);
}

#[test]
fn calls_in_pieces_carry_on_through_the_workspace_as_the_codec_does() {
    let mut s = session();
    let plain = plain();
    s.write(INPUT, &plain);
    let mut work = vec![0u8; COMPRESS_WORK];
    let mut first = true;
    let (mut taken, mut made) = (0u32, 0u32);
    loop {
        let left = len(&plain) - taken;
        let (give, more) = if left > 1000 { (1000, 2) } else { (left, 0) };
        let given = if first { more } else { more | 1 };
        let regs = ok(
            &mut s,
            COMPRESS,
            &[given, WORK, INPUT + taken, give, OUTPUT + made, 700],
        );
        if first {
            start_compression(&mut work, Clearing::for_first_call(give, 700, more != 0)).unwrap();
            first = false;
        }
        let mut out = [0u8; 700];
        let from = taken as usize;
        let progress = compress(
            &mut work,
            &plain[from..from + give as usize],
            more != 0,
            &mut out,
            &mut |_| {},
        )
        .unwrap();
        let status = match progress.status {
            Status::Done => 0,
            Status::Input => 1,
            Status::Output => 2,
        };
        assert_eq!(
            regs.r[..6],
            [
                status,
                WORK,
                INPUT + taken + len32(progress.consumed),
                give - len32(progress.consumed),
                OUTPUT + made + len32(progress.produced),
                700 - len32(progress.produced),
            ]
        );
        assert_eq!(
            s.read(OUTPUT + made, progress.produced),
            out[..progress.produced]
        );
        taken += len32(progress.consumed);
        made += len32(progress.produced);
        if status == 0 {
            break;
        }
    }
    // Decompressed in pieces of thirteen bytes into room for seven.
    let (mut taken, mut back) = (0u32, 0u32);
    let mut first = true;
    loop {
        let left = made - taken;
        let (give, more) = if left > 13 { (13, 2) } else { (left, 0) };
        let given = if first { more } else { more | 1 };
        first = false;
        let regs = ok(
            &mut s,
            DECOMPRESS,
            &[given, WORK, OUTPUT + taken, give, BACK + back, 7],
        );
        taken = regs.r[2] - OUTPUT;
        back = regs.r[4] - BACK;
        if regs.r[0] == 0 {
            break;
        }
    }
    assert_eq!(s.read(BACK, plain.len()), plain);
}

#[test]
fn a_workspace_the_call_cannot_write_is_refused_before_any_output() {
    let mut s = session();
    s.write(INPUT, b"0123456789");
    s.write(BACK, b"Read only\0");
    // A dynamic area every mode may read and none may write.
    let area = ok(
        &mut s,
        crate::swi::os::DYNAMIC_AREA,
        &[0, u32::MAX, 0x8000, u32::MAX, 3, 0x8000, 0, 0, BACK],
    );
    let work = area.r[3];
    s.write(OUTPUT, &[0xAA; 16]);
    assert_eq!(
        refused(&mut s, COMPRESS, &[0, work, INPUT, 10, OUTPUT, 1000]),
        0x921
    );
    assert_eq!(s.read(OUTPUT, 16), [0xAA; 16], "nothing written");
}

#[test]
fn what_the_module_refuses_it_refuses_with_its_own_numbers() {
    let mut s = session();
    s.write(INPUT, b"0123456789");
    // A workspace off a word boundary; an output its room runs off the
    // memory there is; a workspace holding no operation to carry on.
    assert_eq!(
        refused(&mut s, COMPRESS, &[0, WORK + 2, INPUT, 10, OUTPUT, 1000]),
        0x923
    );
    assert_eq!(
        refused(&mut s, COMPRESS, &[0, WORK, INPUT, 10, OUTPUT, 0x7FFF_FFFF]),
        0x921
    );
    assert_eq!(
        refused(&mut s, COMPRESS, &[0, WORK, INPUT, SLOT, OUTPUT, 10]),
        0x921
    );
    s.write(WORK, &[0; 8]);
    assert_eq!(
        refused(&mut s, COMPRESS, &[1, WORK, INPUT, 10, OUTPUT, 1000]),
        0x923
    );
    assert_eq!(
        refused(&mut s, DECOMPRESS, &[1, WORK, INPUT, 10, BACK, 1000]),
        0x923
    );
    // Bytes Squash did not compress, and no bytes at all.
    assert_eq!(
        refused(&mut s, DECOMPRESS, &[0, WORK, INPUT, 10, BACK, 1000]),
        0x922
    );
    assert_eq!(
        refused(&mut s, DECOMPRESS, &[0, WORK, INPUT, 0, BACK, 1000]),
        0x922
    );
    // A refused call leaves the workspace as it was: the operation before it
    // carries on.
    let regs = ok(&mut s, COMPRESS, &[2, WORK, INPUT, 10, OUTPUT, 1000]);
    assert_eq!(regs.r[0], 1);
    assert_eq!(
        refused(&mut s, COMPRESS, &[1, WORK + 2, INPUT, 0, OUTPUT, 1000]),
        0x923
    );
    let regs = ok(&mut s, COMPRESS, &[1, WORK, INPUT, 0, regs.r[4], 1000]);
    assert_eq!(regs.r[0], 0);
    let made = regs.r[4] - OUTPUT;
    let regs = ok(&mut s, DECOMPRESS, &[0, WORK, OUTPUT, made, BACK, 1000]);
    assert_eq!(s.read(BACK, (regs.r[4] - BACK) as usize), b"0123456789");
}

#[test]
fn its_swis_are_named_as_squash_names_them() {
    let mut s = session();
    let names = [
        (COMPRESS, "Squash_Compress"),
        (DECOMPRESS, "Squash_Decompress"),
        (DECOMPRESS + 1, "Squash_2"),
    ];
    for (number, name) in names {
        let regs = ok(&mut s, SWI_NUMBER_TO_STRING, &[number, BACK, 64]);
        assert_eq!(
            s.read(BACK, regs.r[2] as usize - 1),
            name.as_bytes(),
            "{number:x}"
        );
    }
}
