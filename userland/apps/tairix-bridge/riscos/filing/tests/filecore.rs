//! FileCore's SWIs, as a client calls them with its instance in R8: here the
//! RAM disc's, the instance RamFS holds.

mod common;

use common::{block, call, read, text};
use tairix_riscos_filing::ramfs::State;
use tairix_riscos_kernel::os::{Kernel, Os};
use tairix_riscos_kernel::swi::os::{
    fs_control, CHANGE_DYNAMIC_AREA, FS_CONTROL, READ_DYNAMIC_AREA,
};

/// FileCore's SWIs, by their offset in its chunk.
const DISC_OP: u32 = 0x4_0540;
const DRIVES: u32 = 0x4_0542;
const FREE_SPACE: u32 = 0x4_0543;
const DESCRIBE_DISC: u32 = 0x4_0545;
const MISC_OP: u32 = 0x4_0549;
const SECTOR_DISC_OP: u32 = 0x4_054A;
const FREE_SPACE_64: u32 = 0x4_054B;
const DISC_OP_64: u32 = 0x4_054C;
const FEATURES: u32 = 0x4_054D;

/// A session with a RAM disc of 256K, and FileCore's instance for it.
fn session() -> (Kernel, u32) {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    call(&mut os, CHANGE_DYNAMIC_AREA, &[5, 0x4_0000]).expect("a RAM disc");
    let instance = os
        .extension::<State>()
        .map(|state| state.instance())
        .expect("RamFS active");
    (kernel, instance)
}

/// `swi` with `args` in R0 to R7 and the instance in R8.
fn with_instance(os: &mut Os<'_>, swi: u32, instance: u32, args: &[u32]) -> Result<[u32; 8], u32> {
    let mut all = [0; 9];
    all[..args.len()].copy_from_slice(args);
    all[8] = instance;
    call(os, swi, &all)
        .map(|regs| {
            let mut out = [0; 8];
            out.copy_from_slice(&regs.r[..8]);
            out
        })
        .map_err(|(number, _)| number)
}

/// The RAM disc's area's bytes from `at`.
fn area(os: &mut Os<'_>, at: u32, count: usize) -> Vec<u8> {
    let base = call(os, READ_DYNAMIC_AREA, &[5]).expect("area 5").r[0];
    read(os, base + at, count)
}

#[test]
fn features_and_drives_are_as_risc_os_5_gives_them() {
    let (mut kernel, instance) = session();
    let mut os = kernel.os();
    assert_eq!(
        call(&mut os, FEATURES, &[]).map(|r| r.r[0]),
        Ok(3),
        "with no instance"
    );
    let drives = with_instance(&mut os, DRIVES, instance, &[]).expect("drives");
    assert_eq!(drives[..3], [0, 1, 0], "drive 0, one floppy, no hard disc");
    assert_eq!(
        with_instance(&mut os, DRIVES, instance + 4, &[]).map(drop),
        Err(0x108A1),
        "no instance's private word there: FileCore's own BadParms"
    );
}

#[test]
fn free_space_agrees_with_fileswitchs() {
    let (mut kernel, instance) = session();
    let mut os = kernel.os();
    let disc = text(&mut os, b":0");
    let free = with_instance(&mut os, FREE_SPACE, instance, &[disc]).expect("free");
    assert_eq!(free[..2], [0x3_F000, 0x3_F000]);
    let wide = with_instance(&mut os, FREE_SPACE_64, instance, &[disc]).expect("free");
    assert_eq!(wide[..3], [0x3_F000, 0, 0x3_F000]);
    let ram = text(&mut os, b"RAM:");
    let switched = call(&mut os, FS_CONTROL, &[fs_control::FREE_SPACE, ram]).expect("free");
    assert_eq!(switched.r[0], free[0]);
}

#[test]
fn describe_disc_gives_the_record_the_disc_holds() {
    let (mut kernel, instance) = session();
    let mut os = kernel.os();
    let disc = text(&mut os, b":0");
    let out = block(&mut os, &[0xEE; 64]);
    with_instance(&mut os, DESCRIBE_DISC, instance, &[disc, out]).expect("described");
    assert_eq!(
        read(&mut os, out, 20),
        area(&mut os, 4, 20),
        "the map's record"
    );
    let missing = text(&mut os, b":Nowhere");
    assert_eq!(
        with_instance(&mut os, DESCRIBE_DISC, instance, &[missing, out]).map(drop),
        Err(0x1_17D4)
    );
}

#[test]
fn disc_ops_move_bytes_by_address_sector_and_wide_address() {
    let (mut kernel, instance) = session();
    let mut os = kernel.os();
    let buffer = block(&mut os, &[0; 1024]);
    let read_map =
        with_instance(&mut os, DISC_OP, instance, &[0, 1, 0, buffer, 1024]).expect("read");
    assert_eq!(
        (read_map[2], read_map[3], read_map[4]),
        (0x400, buffer + 1024, 0),
        "moved on past it"
    );
    assert_eq!(read(&mut os, buffer, 1024), area(&mut os, 0, 1024));
    let pattern: Vec<u8> = (0..1024u32).map(|n| n.to_le_bytes()[0] ^ 0x5A).collect();
    let source = block(&mut os, &pattern);
    with_instance(&mut os, DISC_OP, instance, &[0, 2, 0x2_0000, source, 1024]).expect("written");
    assert_eq!(
        area(&mut os, 0x2_0000, 1024),
        pattern,
        "written where it was asked"
    );
    let by_sector = block(&mut os, &[0; 1024]);
    with_instance(
        &mut os,
        SECTOR_DISC_OP,
        instance,
        &[0, 1, 0x80, by_sector, 1024],
    )
    .expect("read");
    assert_eq!(
        read(&mut os, by_sector, 1024),
        pattern,
        "sector &80 is byte &20000"
    );
    let address = block(&mut os, &[0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0]);
    let wide = block(&mut os, &[0; 1024]);
    with_instance(&mut os, DISC_OP_64, instance, &[0, 1, address, wide, 1024]).expect("read");
    assert_eq!(read(&mut os, wide, 1024), pattern);
    assert_eq!(
        read(&mut os, address + 4, 8),
        [0, 4, 2, 0, 0, 0, 0, 0],
        "moved on past it"
    );
    assert_eq!(
        with_instance(&mut os, DISC_OP, instance, &[0, 1, 1 << 29, buffer, 1024]).map(drop),
        Err(0x1_17AC),
        "a drive RamFS does not have"
    );
}

#[test]
fn misc_op_6_describes_the_client() {
    let (mut kernel, instance) = session();
    let mut os = kernel.os();
    let described = with_instance(&mut os, MISC_OP, instance, &[6]).expect("described");
    let words = read(&mut os, described[0], 20);
    assert_eq!(words[3], 0x17, "RamFS's filing system number");
}

/// `FileCore_DiscFormat`.
const DISC_FORMAT: u32 = 0x4_0547;
/// `OS_ValidateAddress`, the vetting SWI a caller with no controller of its
/// own hands `FileCore_DiscFormat`.
const VALIDATE_ADDRESS: u32 = 0x2003A;

#[test]
fn disc_format_describes_each_format_as_risc_os_5_describes_it() {
    let (mut kernel, _) = session();
    let mut os = kernel.os();
    let at = block(&mut os, &[0xEE; 64]);
    // Sector size, the first gap on each side, the third gap, sectors a
    // track, density, options, side and track skew, the fill byte, tracks.
    for (format, layout) in [
        (0x80, (256, [42, 42], 57, 16, 2, 0x0C, 0, 0)),
        (0x81, (1024, [303, 32], 90, 5, 2, 0, 0, 0)),
        (0x82, (1024, [303, 32], 90, 5, 2, 0, 0, 0)),
        (0x83, (1024, [50, 50], 90, 10, 4, 0, 1, 3)),
        (0x84, (1024, [50, 50], 90, 20, 8, 0, 1, 5)),
    ] {
        call(
            &mut os,
            DISC_FORMAT,
            &[at, VALIDATE_ADDRESS, at + 64, format],
        )
        .expect("described");
        let got = read(&mut os, at, 25);
        let word = |n: usize| u32::from_le_bytes([got[n], got[n + 1], got[n + 2], got[n + 3]]);
        let (sector, gap1, gap3, per_track, density, options, side, track) = layout;
        assert_eq!(
            (word(0), [word(4), word(8)], word(12)),
            (sector, gap1, gap3),
            "{format:#X}"
        );
        assert_eq!(
            (
                got[16],
                got[17],
                got[18],
                got[21],
                got[22],
                got[23],
                word(24 - 3) >> 24
            ),
            (
                per_track,
                density,
                options,
                side,
                track,
                0xA5,
                u32::from(got[24])
            ),
            "{format:#X}"
        );
        assert_eq!(got[24], 160, "{format:#X}");
    }
    let refused = call(&mut os, DISC_FORMAT, &[at, VALIDATE_ADDRESS, at + 64, 0x85]);
    assert_eq!(refused.map_err(|(number, _)| number).err(), Some(0x108A1));
}

/// `FileCore_FloppyStructure` and `FileCore_LayoutStructure`.
const FLOPPY_STRUCTURE: u32 = 0x4_0544;
const LAYOUT_STRUCTURE: u32 = 0x4_0548;

/// An 800K E floppy's record, but for its size.
fn e_record(os: &mut Os<'_>, size: u64) -> u32 {
    let mut raw = [0u8; 64];
    raw[..12].copy_from_slice(&[10, 5, 2, 2, 15, 7, 1, 0, 0, 1, 0x20, 0x05]);
    raw[12..16].copy_from_slice(&0x203u32.to_le_bytes());
    let size = size.to_le_bytes();
    raw[16..20].copy_from_slice(&size[..4]);
    raw[36..40].copy_from_slice(&size[4..]);
    raw[22..32].copy_from_slice(b"Probe     ");
    block(os, &raw)
}

#[test]
fn a_structure_is_refused_for_a_disc_its_record_cannot_lay_out() {
    let (mut kernel, _) = session();
    let mut os = kernel.os();
    let defects = block(&mut os, &0x2000_0000u32.to_le_bytes());
    let buffer = block(&mut os, &[0; 0x1000]);
    let floppy = e_record(&mut os, 0xC_8000);
    call(&mut os, FLOPPY_STRUCTURE, &[buffer, floppy, 0, defects]).expect("an E floppy");
    let past_its_zones = e_record(&mut os, 1 << 32);
    assert_eq!(
        call(
            &mut os,
            FLOPPY_STRUCTURE,
            &[buffer, past_its_zones, 0, defects]
        )
        .map(drop)
        .map_err(|(number, _)| number),
        Err(0x108A1),
        "more disc than one zone describes"
    );
    let past_an_image = e_record(&mut os, 5 << 30);
    let name = text(&mut os, b"Probe");
    assert_eq!(
        call(
            &mut os,
            LAYOUT_STRUCTURE,
            &[past_an_image, defects, name, 0]
        )
        .map(drop)
        .map_err(|(number, _)| number),
        Err(0x108A1),
        "more disc than an image file's offsets reach"
    );
}
