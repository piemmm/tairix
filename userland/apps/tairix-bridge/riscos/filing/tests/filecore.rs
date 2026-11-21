//! FileCore's SWIs, as a client calls them with its instance in R8: here the
//! RAM disc's, the instance RamFS holds.

mod common;

use common::{block, call, read, text, FILECORE_NOT_FOUND};
use tairix_riscos_filing::filecore::swi::{
    CREATE, DESCRIBE_DISC, DISC_FORMAT, DISC_OP, DISC_OP_64, DRIVES, FEATURES, FLOPPY_STRUCTURE,
    FREE_SPACE, FREE_SPACE_64, LAYOUT_STRUCTURE, MISC_OP, SECTOR_DISC_OP,
};
use tairix_riscos_filing::filecore::{code, numbered, MODULE_NUMBER};
use tairix_riscos_filing::fileswitch::error;
use tairix_riscos_filing::ramfs::{self, State};
use tairix_riscos_filing::session::SLOT_BYTES;
use tairix_riscos_kernel::os::{Kernel, Os};
use tairix_riscos_kernel::swi::os::{fs_control, FS_CONTROL, READ_DYNAMIC_AREA};
use tairix_riscos_kernel::swi::X;

/// FileCore's own "bad parameters".
const BAD_PARMS: u32 = numbered(MODULE_NUMBER, code::BAD_PARMS);

/// The numbers FileCore, RamFS and FileSwitch share with RISC OS — FileCore's
/// SWIs, the errors FileCore and RamFS raise, RamFS's filing system and
/// FileSwitch's own errors — are RISC OS's. Every other test names them, so
/// each is pinned to its value here, once.
#[test]
fn the_numbers_shared_with_risc_os_are_its_own() {
    let system = u32::from(ramfs::NUMBER);
    let pinned: &[(&str, u32, u32)] = &[
        ("FileCore_DiscOp", DISC_OP, 0x4_0540),
        ("FileCore_Create", CREATE, 0x4_0541),
        ("FileCore_Drives", DRIVES, 0x4_0542),
        ("FileCore_FreeSpace", FREE_SPACE, 0x4_0543),
        ("FileCore_FloppyStructure", FLOPPY_STRUCTURE, 0x4_0544),
        ("FileCore_DescribeDisc", DESCRIBE_DISC, 0x4_0545),
        ("FileCore_DiscFormat", DISC_FORMAT, 0x4_0547),
        ("FileCore_LayoutStructure", LAYOUT_STRUCTURE, 0x4_0548),
        ("FileCore_MiscOp", MISC_OP, 0x4_0549),
        ("FileCore_SectorDiscOp", SECTOR_DISC_OP, 0x4_054A),
        ("FileCore_FreeSpace64", FREE_SPACE_64, 0x4_054B),
        ("FileCore_DiscOp64", DISC_OP_64, 0x4_054C),
        ("FileCore_Features", FEATURES, 0x4_054D),
        ("XOS_ValidateAddress", VALIDATE_ADDRESS, 0x2_003A),
        ("FileCore's bad parameters", BAD_PARMS, 0x1_08A1),
        ("RamFS's filing system", system, 0x17),
        ("RamFS's not found", FILECORE_NOT_FOUND, 0x1_17D6),
        ("RamFS's file open", numbered(system, code::OPEN), 0x1_17C2),
        ("FileSwitch's wildcards", error::WILDCARDS.number, 0xFD),
        (
            "FileSwitch's nothing matched",
            error::NOTHING_MATCHED.number,
            0x416,
        ),
        (
            "FileSwitch's not for update",
            error::NOT_FOR_UPDATE.number,
            0xC1,
        ),
        ("FileSwitch's wrong kind", error::WRONG_KIND.number, 0xAF),
        ("a session's application slot", SLOT_BYTES, 0x10_0000),
    ];
    for &(name, number, value) in pinned {
        assert_eq!(number, value, "{name}");
    }
}

/// A session with a RAM disc of 256K, and FileCore's instance for it.
fn session() -> (Kernel, u32) {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    common::ram_disc(&mut os);
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
        Err(BAD_PARMS),
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
    assert_eq!(words[3], ramfs::NUMBER, "RamFS's filing system number");
}

/// A client's entries are offsets from its module's base, added as the
/// processor adds them: from a base above what they name they reach round the
/// top of the address space to it, as RamFS's do whenever its workspace lies
/// below its module.
#[test]
fn a_client_s_entries_reach_round_the_address_space_from_its_base() {
    let (mut kernel, _) = session();
    let mut os = kernel.os();
    let title = text(&mut os, b"Wrapped");
    let routine = block(&mut os, &[0; 4]);
    let base = 0xFFFF_FF00_u32;
    let from_base = |at: u32| at.wrapping_sub(base);
    let words = [
        0x88 << 24,
        from_base(title),
        from_base(title),
        from_base(routine),
        from_base(routine),
    ];
    let descriptor = block(&mut os, &words.map(u32::to_le_bytes).concat());
    let private_word = block(&mut os, &[0; 4]);
    let created = call(&mut os, CREATE, &[descriptor, base, private_word, 1]).expect("created");
    let described = with_instance(&mut os, MISC_OP, created.r[0], &[6]).expect("described");
    let bytes = read(&mut os, described[0] + 4, 16);
    let entries: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&word| u32::from_le_bytes(word))
        .collect();
    assert_eq!(entries, [title, title, routine, routine]);
}

/// `XOS_ValidateAddress`, the vetting SWI a caller with no controller of its
/// own hands `FileCore_DiscFormat`.
const VALIDATE_ADDRESS: u32 = X | tairix_riscos_kernel::swi::os::VALIDATE_ADDRESS;

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
    assert_eq!(refused.map_err(|(number, _)| number).err(), Some(BAD_PARMS));
}

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
        Err(BAD_PARMS),
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
        Err(BAD_PARMS),
        "more disc than an image file's offsets reach"
    );
}
