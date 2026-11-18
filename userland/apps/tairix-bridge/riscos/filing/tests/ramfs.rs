//! RamFS over FileCore, through the SWIs a program calls: the RAM disc made
//! and refused as RISC OS 5 makes and refuses it, its files placed where RISC
//! OS 5 was recorded placing them, and its errors numbered beside `RAM:`.

mod common;

use common::{block, call, info, read, save, text};
use tairix_riscos_kernel::os::{Kernel, Os};
use tairix_riscos_kernel::swi::os::{
    fs_control, ARGS, CHANGE_DYNAMIC_AREA, FILE, FIND, FS_CONTROL, GBPB,
};

/// The RAM disc's area.
const AREA: u32 = 5;

/// `bytes` added to the RAM disc's area, or taken from it where negative:
/// the bytes moved, or the refusal's number.
fn resize(os: &mut Os<'_>, bytes: i32) -> Result<u32, u32> {
    call(os, CHANGE_DYNAMIC_AREA, &[AREA, bytes.cast_unsigned()])
        .map(|regs| regs.r[1])
        .map_err(|(number, _)| number)
}

/// A session with a RAM disc of `bytes`.
fn with_disc(bytes: i32) -> Kernel {
    let mut kernel = common::kernel();
    assert_eq!(resize(&mut kernel.os(), bytes), Ok(bytes.cast_unsigned()));
    kernel
}

/// The free bytes `OS_FSControl` 49 gives for the RAM disc.
fn free(os: &mut Os<'_>) -> u32 {
    let name = text(os, b"RAM:");
    call(os, FS_CONTROL, &[fs_control::FREE_SPACE, name])
        .expect("its free space")
        .r[0]
}

/// The error number `name`'s `OS_File` 5 refuses with, or its kind.
fn kind(os: &mut Os<'_>, name: &[u8]) -> Result<u32, u32> {
    info(os, name)
        .map(|(kind, ..)| kind)
        .map_err(|(number, _)| number)
}

#[test]
fn an_empty_area_is_no_filing_system_until_it_grows() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    assert_eq!(kind(&mut os, b"RAM:$"), Err(0xF8), "RAM: is not there");
    assert_eq!(resize(&mut os, 0x4_0000), Ok(0x4_0000));
    assert_eq!(kind(&mut os, b"RAM:$"), Ok(2));
    assert_eq!(resize(&mut os, -0x4_0000), Ok(0x4_0000));
    assert_eq!(kind(&mut os, b"RAM:$"), Err(0xF8), "gone with its area");
}

#[test]
fn a_small_fresh_disc_is_a_floppy_of_one_zone() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    assert_eq!(info(&mut os, b"RAM:$"), Ok((2, 0xFFFF_CE00, 0, 0x800)));
    assert_eq!(free(&mut os), 0x3_F000);
}

#[test]
fn a_fresh_disc_of_a_megabyte_is_a_hard_disc() {
    let mut kernel = with_disc(0x10_0000);
    let mut os = kernel.os();
    assert_eq!(
        info(&mut os, b"RAM:$"),
        Ok((2, 0xFFFF_CD00, 0, 0)),
        "the boot block's length"
    );
    assert_eq!(free(&mut os), 0xF_D800);
}

/// `name` opened for input: its handle.
fn open_in(os: &mut Os<'_>, name: &[u8]) -> u32 {
    let name = text(os, name);
    call(os, FIND, &[0x40, name]).expect("opened").r[0]
}

#[test]
fn files_are_placed_as_risc_os_5_placed_them() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    for (name, size, free_after, allocated) in [
        (&b"RAM:$.A"[..], 100, 0x3_E800, 0x400),
        (b"RAM:$.B", 1000, 0x3_E800, 0x400),
        (b"RAM:$.C", 2000, 0x3_E000, 0x800),
        (b"RAM:$.D", 5000, 0x3_CC00, 0x1400),
        (b"RAM:$.F", 1, 0x3_C400, 0x400),
        (b"RAM:$.G", 0, 0x3_C400, 0),
    ] {
        save(&mut os, name, 0xFFD, &vec![0x5A; size]).expect("saved");
        assert_eq!(free(&mut os), free_after, "{name:?}");
        let handle = open_in(&mut os, name);
        assert_eq!(
            call(&mut os, ARGS, &[4, handle]).expect("allocated").r[2],
            allocated
        );
        call(&mut os, FIND, &[0, handle]).expect("closed");
    }
    let root = text(&mut os, b"RAM:$");
    let out = os.rma_claim(512).expect("room");
    let listed = call(&mut os, GBPB, &[11, root, out, 8, 0, 512, 0]).expect("listed");
    assert_eq!(listed.r[3], 6);
    let records = read(&mut os, out, 512);
    let sins: Vec<u32> = records
        .chunks(32)
        .take(6)
        .map(|record| u32::from_le_bytes(record[20..24].try_into().expect("a word")))
        .collect();
    assert_eq!(
        sins,
        [0x401, 0x402, 0x500, 0x600, 0x701, 1],
        "each one's address"
    );
}

#[test]
fn the_disc_refuses_a_resize_while_it_holds_a_file() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Held", 0xFFD, b"kept").expect("saved");
    assert_eq!(resize(&mut os, 0x4_0000), Err(0x1C3));
    assert_eq!(resize(&mut os, -0x4_0000), Err(0x1C3));
    assert_eq!(kind(&mut os, b"RAM:$.Held"), Ok(1), "the file stays");
    let name = text(&mut os, b"RAM:$.Held");
    call(&mut os, FILE, &[6, name]).expect("deleted");
    assert_eq!(resize(&mut os, 0x4_0000), Ok(0x4_0000), "empty, it grows");
    assert_eq!(free(&mut os), 0x7_F000, "a fresh disc the new size");
}

#[test]
fn ram_disc_errors_are_numbered_beside_its_filing_system() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Locked", 0xFFD, b"x").expect("saved");
    let name = text(&mut os, b"RAM:$.Locked");
    call(&mut os, FILE, &[4, name, 0, 0, 0, 0x0B]).expect("locked");
    assert_eq!(
        call(&mut os, FILE, &[6, name]).map(drop).map_err(|e| e.0),
        Err(0x1_17C3)
    );
    assert_eq!(
        save(&mut os, b"RAM:$.Nowhere.File", 0xFFD, b"x").map_err(|e| e.0),
        Err(0x1_17D6),
        "no such directory"
    );
    save(&mut os, b"RAM:$.Unread", 0xFFD, b"x").expect("saved");
    let name = text(&mut os, b"RAM:$.Unread");
    call(&mut os, FILE, &[4, name, 0, 0, 0, 0x02]).expect("write only");
    assert_eq!(
        call(&mut os, FIND, &[0x40, name])
            .map(drop)
            .map_err(|e| e.0),
        Err(0x1_17BD)
    );
    let dir = text(&mut os, b"RAM:$.Full");
    call(&mut os, FILE, &[8, dir]).expect("a directory");
    save(&mut os, b"RAM:$.Full.In", 0xFFD, b"x").expect("saved");
    assert_eq!(
        call(&mut os, FILE, &[6, dir]).map(drop).map_err(|e| e.0),
        Err(0x1_17B4)
    );
    save(&mut os, b"RAM:$.Open", 0xFFD, b"x").expect("saved");
    let handle = open_in(&mut os, b"RAM:$.Open");
    let name = text(&mut os, b"RAM:$.Open");
    assert_eq!(
        call(&mut os, FILE, &[6, name]).map(drop).map_err(|e| e.0),
        Err(0x1_17C2)
    );
    call(&mut os, FIND, &[0, handle]).expect("closed");
}

#[test]
fn an_open_file_grown_holds_the_free_space_until_it_closes() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Grows", 0xFFD, &[7; 5000]).expect("saved");
    let before = free(&mut os);
    let name = text(&mut os, b"RAM:$.Grows");
    let handle = call(&mut os, FIND, &[0xC0, name]).expect("opened").r[0];
    call(&mut os, ARGS, &[3, handle, 0x3000]).expect("extended");
    let allocated = call(&mut os, ARGS, &[4, handle]).expect("allocated").r[2];
    assert_eq!(allocated, before + 0x1400, "its own and all that was free");
    assert_eq!(free(&mut os), 0);
    call(&mut os, FIND, &[0, handle]).expect("closed");
    assert_eq!(
        free(&mut os),
        before - 0x1C00,
        "trimmed to its extent once closed"
    );
    let data = block(&mut os, &[0; 0x3000]);
    call(&mut os, FILE, &[255, name, data, 0]).expect("loaded");
    let loaded = read(&mut os, data, 0x3000);
    assert!(
        loaded[..5000].iter().all(|&b| b == 7),
        "what was there kept"
    );
}

#[test]
fn renaming_follows_risc_os_5() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    for name in [&b"RAM:$.A"[..], b"RAM:$.B"] {
        save(&mut os, name, 0xFFD, b"small").expect("saved");
    }
    let dir = text(&mut os, b"RAM:$.E");
    call(&mut os, FILE, &[8, dir]).expect("a directory");
    let before = free(&mut os);
    let (from, to) = (text(&mut os, b"RAM:$.A"), text(&mut os, b"RAM:$.E.A"));
    call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to]).expect("moved");
    assert_eq!(
        free(&mut os),
        before - 0x800,
        "out of the fragment it shared"
    );
    let (from, to) = (text(&mut os, b"RAM:$.E"), text(&mut os, b"RAM:$.E.X"));
    assert_eq!(
        call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to])
            .map(drop)
            .map_err(|e| e.0),
        Err(0x1_17B0),
        "not into itself"
    );
    let (from, to) = (text(&mut os, b"RAM:$.B"), text(&mut os, b"RAM:$.b"));
    call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to]).expect("re-spelt");
    assert_eq!(kind(&mut os, b"RAM:$.b"), Ok(1));
    let (from, to) = (text(&mut os, b"RAM:$.b"), text(&mut os, b"RAM:$.Zed"));
    call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to]).expect("renamed where it was");
    let (from, to) = (text(&mut os, b"RAM:$.E"), text(&mut os, b"RAM:$.Bee"));
    call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to])
        .expect("a directory renamed where it was");
    assert_eq!(
        kind(&mut os, b"RAM:$.Bee.A"),
        Ok(1),
        "what it holds goes with it"
    );
    assert_eq!(
        names(&mut os, b"RAM:$"),
        [&b"Bee"[..], b"Zed"],
        "each in its sorted place"
    );
}

/// The names in the directory `name`, in the order it holds them.
fn names(os: &mut Os<'_>, name: &[u8]) -> Vec<Vec<u8>> {
    let name = text(os, name);
    let room = block(os, &[0; 256]);
    let regs = call(os, GBPB, &[9, name, room, 16, 0, 256, 0]).expect("listed");
    let listed = read(os, room, 256);
    listed
        .split(|&b| b == 0)
        .take(regs.r[3] as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

#[test]
fn the_disc_is_named_and_given_a_boot_option() {
    let mut kernel = with_disc(0x4_0000);
    let mut os = kernel.os();
    let ram = text(&mut os, b"RAM:");
    let renamed = text(&mut os, b"Renamed");
    call(&mut os, FS_CONTROL, &[fs_control::NAME_DISC, ram, renamed]).expect("named");
    let name = text(&mut os, b"RAM:$");
    let buffer = os.rma_claim(256).expect("room");
    let canonical = call(
        &mut os,
        FS_CONTROL,
        &[fs_control::CANONICALISE, name, buffer, 0, 0, 256],
    )
    .expect("canonical");
    assert_eq!(canonical.r[5], 256 - 14, "RAM::Renamed.$");
    call(
        &mut os,
        FS_CONTROL,
        &[fs_control::WRITE_BOOT_OPTION, ram, 3],
    )
    .expect("set");
    let option = call(&mut os, FS_CONTROL, &[fs_control::READ_BOOT_OPTION, ram]).expect("read");
    assert_eq!(option.r[2], 3);
}
