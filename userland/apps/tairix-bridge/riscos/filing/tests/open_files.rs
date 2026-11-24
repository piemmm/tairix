//! Files held open through FileSwitch, on FileCore's RAM disc and on a
//! filing system of a program's own, as RISC OS 5 holds them: a file open to
//! be written is open on no other handle, a locked one is opened for update
//! only to be read, its catalogue stays its entry's, and a path through a
//! missing directory or a file reads as nothing there.

mod common;

use common::{block, call, read, save, text, FILECORE_NOT_FOUND};
use tairix_riscos_filing::filecore::{code, numbered};
use tairix_riscos_filing::fileswitch::error;
use tairix_riscos_filing::ramfs;
use tairix_riscos_kernel::os::{Kernel, Os};
use tairix_riscos_kernel::swi::os::find::{
    INPUT as OPEN_IN, MUST_OPEN, OUTPUT as OPEN_OUT, UPDATE as OPEN_UP,
};
use tairix_riscos_kernel::swi::os::{fs_control, ARGS, FILE, FIND, FS_CONTROL, GBPB};

/// FileCore's "file open" beside RamFS's number, and FileSwitch's own "not
/// open for update" and "exists, and is not what was asked for".
const FILE_OPEN: u32 = numbered(ramfs::NUMBER as u32, code::OPEN);
const NOT_FOUND: u32 = tairix_riscos_kernel::filing::NOT_FOUND.number;
const NOT_FOR_UPDATE: u32 = error::NOT_FOR_UPDATE.number;
const WRONG_KIND: u32 = error::WRONG_KIND.number;

/// A session with a RAM disc of 256K, current.
fn with_disc() -> Kernel {
    disc_in(common::kernel())
}

/// `kernel` given a RAM disc of 256K, current.
fn disc_in(mut kernel: Kernel) -> Kernel {
    let mut os = kernel.os();
    common::ram_disc(&mut os);
    let ram = text(&mut os, b"RAM");
    call(&mut os, tairix_riscos_kernel::swi::os::CLI, &[ram]).expect("current");
    kernel
}

/// `name` opened with `mode`: its handle, or the refusal's number.
fn open(os: &mut Os<'_>, mode: u32, name: &[u8]) -> Result<u32, u32> {
    let name = text(os, name);
    call(os, FIND, &[mode, name])
        .map(|regs| regs.r[0])
        .map_err(|(number, _)| number)
}

fn close(os: &mut Os<'_>, handle: u32) {
    call(os, FIND, &[0, handle]).expect("closed");
}

/// What `OS_File` 20 reads of `name`: its kind, length, attributes and type.
fn catalogue(os: &mut Os<'_>, name: &[u8]) -> (u32, u32, u32, u32) {
    let name = text(os, name);
    let regs = call(os, FILE, &[20, name]).expect("read");
    (regs.r[0], regs.r[4], regs.r[5], regs.r[6])
}

fn lock(os: &mut Os<'_>, name: &[u8], attributes: u32) {
    let name = text(os, name);
    call(os, FILE, &[4, name, 0, 0, 0, attributes]).expect("attributes set");
}

#[test]
fn a_file_open_to_be_written_is_open_on_no_other_handle() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.U", 0xFFF, b"x").expect("saved");
    let first = open(&mut os, OPEN_IN, b"RAM:$.U").expect("read");
    let second = open(&mut os, OPEN_IN, b"RAM:$.U").expect("read again");
    assert_eq!(open(&mut os, OPEN_UP, b"RAM:$.U"), Err(FILE_OPEN));
    assert_eq!(open(&mut os, OPEN_OUT, b"RAM:$.U"), Err(FILE_OPEN));
    close(&mut os, second);
    close(&mut os, first);
    let writing = open(&mut os, OPEN_UP, b"RAM:$.U").expect("updated");
    assert_eq!(open(&mut os, OPEN_IN, b"RAM:$.U"), Err(FILE_OPEN));
    assert_eq!(open(&mut os, OPEN_UP, b"RAM:$.U"), Err(FILE_OPEN));
    close(&mut os, writing);
    assert!(
        open(&mut os, OPEN_UP, b"RAM:$.U").is_ok(),
        "free once closed"
    );
}

#[test]
fn a_locked_file_opened_for_update_is_opened_only_to_be_read() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.L", 0xFFF, b"x").expect("saved");
    lock(&mut os, b"RAM:$.L", 0xB);
    let updated = open(&mut os, OPEN_UP, b"RAM:$.L").expect("opened");
    let again = open(&mut os, OPEN_UP, b"RAM:$.L").expect("opened again: neither may write");
    let truncated = open(&mut os, OPEN_OUT, b"RAM:$.L").expect("opened to be written afresh");
    let extent = call(&mut os, ARGS, &[2, truncated]).expect("its extent").r[2];
    assert_eq!(extent, 0, "emptied as far as the handle goes");
    let data = block(&mut os, b"abcd");
    for handle in [updated, truncated] {
        let refusal = call(&mut os, GBPB, &[2, handle, data, 4])
            .err()
            .map(|(number, _)| number);
        assert_eq!(refusal, Some(NOT_FOR_UPDATE));
        let refusal = call(&mut os, ARGS, &[3, handle, 0])
            .err()
            .map(|(number, _)| number);
        assert_eq!(refusal, Some(NOT_FOR_UPDATE));
    }
    for handle in [truncated, again, updated] {
        close(&mut os, handle);
    }
    assert_eq!(
        catalogue(&mut os, b"RAM:$.L"),
        (1, 1, 0xB, 0xFFF),
        "as it was"
    );
    let name = text(&mut os, b"RAM:$.L");
    let loaded = block(&mut os, &[0; 4]);
    call(&mut os, FILE, &[255, name, loaded, 0]).expect("loaded");
    assert_eq!(read(&mut os, loaded, 1), b"x");
}

#[test]
fn catalogue_changes_made_while_a_file_is_open_stand_once_it_closes() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.V", 0xFFF, b"x").expect("saved");
    let handle = open(&mut os, OPEN_UP, b"RAM:$.V").expect("opened");
    let name = text(&mut os, b"RAM:$.V");
    call(&mut os, FILE, &[18, name, 0xFEB]).expect("typed");
    lock(&mut os, b"RAM:$.V", 0x13);
    let data = block(&mut os, b"abcd");
    call(&mut os, GBPB, &[2, handle, data, 4]).expect("written");
    close(&mut os, handle);
    assert_eq!(
        catalogue(&mut os, b"RAM:$.V"),
        (1, 4, 0x13, 0xFEB),
        "the type and attributes the entry was given, the length the file was written to"
    );
}

#[test]
fn an_open_object_is_not_renamed_but_the_directory_holding_it_may_be() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    for directory in [&b"RAM:$.D"[..], b"RAM:$.H", b"RAM:$.K"] {
        let name = text(&mut os, directory);
        call(&mut os, FILE, &[8, name, 0, 0, 0]).expect("made");
    }
    save(&mut os, b"RAM:$.R", 0xFFF, b"x").expect("saved");
    save(&mut os, b"RAM:$.D.F", 0xFFF, b"x").expect("saved");
    let file = open(&mut os, OPEN_UP, b"RAM:$.R").expect("opened");
    let rename = fs_control::RENAME;
    let renamed = |os: &mut Os<'_>, from: &[u8], to: &[u8]| {
        let (from, to) = (text(os, from), text(os, to));
        call(os, FS_CONTROL, &[rename, from, to])
            .err()
            .map(|(number, _)| number)
    };
    assert_eq!(renamed(&mut os, b"RAM:$.R", b"RAM:$.R2"), Some(FILE_OPEN));
    close(&mut os, file);
    let directory = open(&mut os, OPEN_IN, b"RAM:$.K").expect("a directory opened to be read");
    assert_eq!(renamed(&mut os, b"RAM:$.K", b"RAM:$.K2"), Some(FILE_OPEN));
    close(&mut os, directory);
    let inside = open(&mut os, OPEN_UP, b"RAM:$.D.F").expect("opened");
    assert_eq!(renamed(&mut os, b"RAM:$.D", b"RAM:$.E"), None);
    assert_eq!(renamed(&mut os, b"RAM:$.E", b"RAM:$.H.E"), None);
    let data = block(&mut os, b"abcd");
    call(&mut os, GBPB, &[2, inside, data, 4]).expect("written where it now is");
    close(&mut os, inside);
    assert_eq!(catalogue(&mut os, b"RAM:$.H.E.F"), (1, 4, 3, 0xFFF));
}

#[test]
fn a_path_through_a_missing_directory_or_a_file_reads_as_nothing_there() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.U", 0xFFF, b"x").expect("saved");
    for name in [&b"RAM:$.Missing.C"[..], b"RAM:$.U.C"] {
        let at = text(&mut os, name);
        for reason in [5, 17, 6] {
            let regs = call(&mut os, FILE, &[reason, at]).expect("read as nothing");
            assert_eq!(regs.r[0], 0, "OS_File {reason}");
        }
        assert_eq!(open(&mut os, OPEN_IN, name), Ok(0));
        assert_eq!(open(&mut os, OPEN_IN | MUST_OPEN, name), Err(NOT_FOUND));
        assert_eq!(open(&mut os, OPEN_UP, name), Ok(0));
        assert_eq!(
            open(&mut os, OPEN_OUT, name),
            Err(NOT_FOUND),
            "its create's refusal unreported"
        );
        let data = block(&mut os, b"abcd");
        let save = call(&mut os, FILE, &[10, at, 0xFFF, 0, data, data + 4]);
        assert_eq!(
            save.err().map(|(number, _)| number),
            Some(FILECORE_NOT_FOUND)
        );
        let create = call(&mut os, FILE, &[7, at, 0, 0, 0, 4]);
        assert_eq!(
            create.err().map(|(number, _)| number),
            Some(FILECORE_NOT_FOUND)
        );
        let made = call(&mut os, FILE, &[8, at, 0, 0, 0]);
        assert_eq!(
            made.err().map(|(number, _)| number),
            Some(FILECORE_NOT_FOUND)
        );
        let set_type = call(&mut os, FILE, &[18, at, 0xFFF]);
        assert_eq!(set_type.err().map(|(number, _)| number), Some(NOT_FOUND));
    }
    let (from, to) = (text(&mut os, b"RAM:$.U"), text(&mut os, b"RAM:$.Missing.C"));
    let moved = call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to]);
    assert_eq!(
        moved.err().map(|(number, _)| number),
        Some(FILECORE_NOT_FOUND)
    );
}

#[test]
fn an_empty_file_made_over_a_directory_is_the_wrong_kind() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    let name = text(&mut os, b"RAM:$.Dir");
    call(&mut os, FILE, &[8, name, 0, 0, 0]).expect("made");
    for args in [[7, name, 0, 0, 0, 4], [11, name, 0xFFF, 0, 0, 4]] {
        let made = call(&mut os, FILE, &args);
        assert_eq!(
            made.err().map(|(number, _)| number),
            Some(WRONG_KIND),
            "OS_File {}",
            args[0]
        );
    }
}

/// A filing system of a program's own, `TxTrunc`, whose every object is a
/// file it opens unbuffered for update and refuses to truncate, and which
/// marks the word at [`TRUNC_CLOSED`] when FileSwitch closes one:
///
/// ```text
/// base:    name - base, 0, open - base, nothing - base, nothing - base,
///          args - base, close - base, file - base, &F7, nothing - base,
///          nothing - base
/// open:    MOV r0, #&C0000000 ; MOV r1, #1 ; MOV r2, #0 ; MOV r3, #0
///          MOV r4, #0 ; MOV pc, lr
/// args:    CMP r0, #3 ; MOVNE pc, lr ; ADR r0, refusal
///          MSR CPSR_f, #&10000000 ; MOV pc, lr
/// close:   ADR r0, closed ; MOV r1, #1 ; STR r1, [r0] ; MOV pc, lr
/// file:    CMP r0, #5 ; MOVNE pc, lr ; MOV r0, #1 ; MVN r2, #&FF
///          MOV r3, #0 ; MOV r4, #0 ; MOV r5, #3 ; MOV pc, lr
/// nothing: MOV pc, lr
/// refusal: &12345, "Refused"
/// closed:  0
/// name:    "TxTrunc"
/// ```
const TRUNC: [u32; 41] = [
    0x9C,
    0,
    0x2C,
    0x88,
    0x88,
    0x44,
    0x58,
    0x68,
    0xF7,
    0x88,
    0x88,
    0xE3A0_0103,
    0xE3A0_1001,
    0xE3A0_2000,
    0xE3A0_3000,
    0xE3A0_4000,
    0xE1A0_F00E,
    0xE350_0003,
    0x11A0_F00E,
    0xE28F_0038,
    0xE328_F201,
    0xE1A0_F00E,
    0xE28F_0038,
    0xE3A0_1001,
    0xE580_1000,
    0xE1A0_F00E,
    0xE350_0005,
    0x11A0_F00E,
    0xE3A0_0001,
    0xE3E0_20FF,
    0xE3A0_3000,
    0xE3A0_4000,
    0xE3A0_5003,
    0xE1A0_F00E,
    0xE1A0_F00E,
    0x0001_2345,
    0x7566_6552,
    0x0064_6573,
    0,
    0x7254_7854,
    0x0063_6E75,
];
/// Where in [`TRUNC`] it marks a close.
const TRUNC_CLOSED: u32 = 0x98;

#[test]
fn an_openout_whose_truncation_is_refused_leaves_nothing_open() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    let bytes: Vec<u8> = TRUNC.iter().flat_map(|word| word.to_le_bytes()).collect();
    let base = block(&mut os, &bytes);
    call(&mut os, FS_CONTROL, &[fs_control::ADD, base, 0, 0]).expect("added");
    let refusal = open(&mut os, OPEN_OUT, b"TxTrunc:$.f");
    assert_eq!(refusal, Err(0x1_2345), "the truncation's refusal");
    assert_eq!(
        os.read_word(base + TRUNC_CLOSED).ok(),
        Some(1),
        "the file it opened closed again"
    );
    let name = text(&mut os, b"TxTrunc");
    call(&mut os, FS_CONTROL, &[fs_control::REMOVE, name]).expect("removed");
}

#[test]
fn an_open_whose_buffer_there_is_no_room_for_leaves_nothing_open() {
    let mut kernel = disc_in(common::kernel_in(16 * 1024 * 1024));
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.U", 0xFFF, b"x").expect("saved");
    let name = text(&mut os, b"RAM:$.U");
    let mut held = Vec::new();
    for size in (6..=20).rev().map(|bits| 1u32 << bits) {
        while let Ok(at) = os.rma_claim(size) {
            held.push(at);
        }
    }
    let refusal = call(&mut os, FIND, &[OPEN_UP, name]).map(|regs| regs.r[0]);
    assert!(refusal.is_err(), "no room for its buffer: {refusal:?}");
    for at in held {
        os.rma_free(at).expect("freed");
    }
    assert!(
        open(&mut os, OPEN_UP, b"RAM:$.U").is_ok(),
        "nothing left open by the refusal"
    );
}

#[test]
fn open_files_are_counted_up_from_the_first_for_a_handle_of_nought_or_below() {
    let mut kernel = with_disc();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.A", 0xFFF, b"a").expect("saved");
    save(&mut os, b"RAM:$.B", 0xFFF, b"b").expect("saved");
    let one = open(&mut os, OPEN_IN, b"RAM:$.A").expect("opened");
    let other = open(&mut os, OPEN_IN, b"RAM:$.B").expect("opened");
    let (first, second) = (one.min(other), one.max(other));
    let next = |os: &mut Os<'_>, after: u32| {
        let regs = call(os, FS_CONTROL, &[fs_control::NEXT_OPEN, after]).expect("counted");
        (regs.r[1], regs.r[2])
    };
    let (handle, info) = next(&mut os, u32::MAX);
    assert_eq!(handle, first, "-1 starts from the first");
    assert_eq!(
        info & 0xFF,
        u32::from(ramfs::NUMBER),
        "its filing system's information word"
    );
    assert_eq!(next(&mut os, 0).0, first, "and so does nought");
    assert_eq!(next(&mut os, first).0, second);
    assert_eq!(next(&mut os, second).0, u32::MAX, "-1 after the last");
    close(&mut os, second);
    close(&mut os, first);
}
