//! Names as RISC OS 5's FileSwitch resolves them, on the RAM disc: a
//! wildcard matched to the first entry its directory lists, the directory
//! read up from where it is not there and listed whatever it is; what each
//! operation makes of a wildcard nothing matches; and the temporary filing
//! system a line's prefix sets.

mod common;

use common::{block, call, info, read, save, text, FILECORE_NOT_FOUND};
use tairix_riscos_kernel::filing::NOT_FOUND;
use tairix_riscos_kernel::os::{Kernel, Os, Regs};
use tairix_riscos_kernel::swi::os::{
    fs_control, ARGS, CHANGE_DYNAMIC_AREA, CLI, FILE, FIND, FS_CONTROL, GBPB,
};

/// FileSwitch's refusals of a wildcard where none may be, and of a wipe whose
/// directory lists nothing.
const WILDCARDS: u32 = 0xFD;
const NOTHING_MATCHED: u32 = 0x416;

/// A session with a RAM disc of 256K holding the empty directory `D` and the
/// untyped file `F`.
fn disc() -> Kernel {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    call(&mut os, CHANGE_DYNAMIC_AREA, &[5, 0x4_0000]).expect("a RAM disc");
    line(&mut os, b"CDir RAM:$.D").expect("made");
    save(&mut os, b"RAM:$.F", 0xFFF, b"f").expect("saved");
    kernel
}

/// The command line `line` run: its refusal's number, if any.
fn line(os: &mut Os<'_>, line: &[u8]) -> Result<(), u32> {
    let at = text(os, line);
    call(os, CLI, &[at]).map(drop).map_err(|(number, _)| number)
}

/// `OS_FSControl` `reason` on `name`, R2 and R3 as given: its registers, or
/// its refusal's number.
fn control(os: &mut Os<'_>, reason: u32, name: &[u8], r2: u32, r3: u32) -> Result<Regs, u32> {
    let at = text(os, name);
    call(os, FS_CONTROL, &[reason, at, r2, r3]).map_err(|(number, _)| number)
}

/// What `OS_File` 5 finds as `name`: its object kind.
fn kind(os: &mut Os<'_>, name: &[u8]) -> u32 {
    info(os, name).expect("read").0
}

#[test]
fn a_wildcard_under_a_missing_directory_or_a_file_matches_nothing_found() {
    let mut kernel = disc();
    let mut os = kernel.os();
    for name in [
        &b"RAM:$.Missing.*"[..],
        b"RAM:$.F.*",
        b"RAM:$.Missing.X.*",
        b"RAM:$.F.X*.Y",
        b"RAM:$.z*.X",
    ] {
        assert_eq!(kind(&mut os, name), 0, "{}", String::from_utf8_lossy(name));
    }
    assert_eq!(kind(&mut os, b"RAM:$.F*"), 1, "the first match");
}

#[test]
fn a_wipe_of_a_wildcard_is_refused_only_where_its_directory_lists_nothing() {
    let mut kernel = disc();
    let mut os = kernel.os();
    assert_eq!(
        control(&mut os, fs_control::WIPE, b"RAM:$.z*", 0, 0).map(drop),
        Ok(())
    );
    assert_eq!(
        (kind(&mut os, b"RAM:$.F"), kind(&mut os, b"RAM:$.D")),
        (1, 2),
        "nothing matched"
    );
    assert_eq!(
        control(&mut os, fs_control::WIPE, b"RAM:$.D.*", 0, 0).map(drop),
        Err(NOTHING_MATCHED),
        "an empty directory"
    );
    for name in [&b"RAM:$.Missing.*"[..], b"RAM:$.F.*"] {
        assert_eq!(
            control(&mut os, fs_control::WIPE, name, 0, 0).map(drop),
            Err(FILECORE_NOT_FOUND),
            "FileCore will not list it: {}",
            String::from_utf8_lossy(name)
        );
    }
}

#[test]
fn a_count_of_a_wildcard_nothing_matches_counts_nothing() {
    let mut kernel = disc();
    let mut os = kernel.os();
    for name in [&b"RAM:$.z*"[..], b"RAM:$.D.*"] {
        let counted = control(&mut os, fs_control::COUNT, name, 0, 0).expect("counted");
        assert_eq!(
            (counted.r[2], counted.r[3]),
            (0, 0),
            "{}",
            String::from_utf8_lossy(name)
        );
    }
    assert_eq!(
        control(&mut os, fs_control::COUNT, b"RAM:$.Missing.*", 0, 0).map(drop),
        Err(FILECORE_NOT_FOUND)
    );
}

#[test]
fn information_on_a_wildcard_nothing_matches_is_not_found() {
    let mut kernel = disc();
    let mut os = kernel.os();
    for reason in [fs_control::INFO, fs_control::FILE_INFO] {
        assert_eq!(
            control(&mut os, reason, b"RAM:$.z*", 0, 0).map(drop),
            Err(NOT_FOUND.number)
        );
    }
}

#[test]
fn a_wildcarded_leaf_naming_what_is_made_or_deleted_is_refused() {
    let mut kernel = disc();
    let mut os = kernel.os();
    for name in [&b"RAM:$.F*"[..], b"RAM:$.#"] {
        let at = text(&mut os, name);
        for args in [
            [0, at, 0, 0, 0, 0],
            [6, at, 0, 0, 0, 0],
            [7, at, 0, 0, 0, 4],
            [8, at, 0, 0, 0, 0],
            [10, at, 0xFFF, 0, 0, 0],
            [11, at, 0xFFF, 0, 0, 4],
        ] {
            let refused = call(&mut os, FILE, &args)
                .map(drop)
                .map_err(|(number, _)| number);
            assert_eq!(refused, Err(WILDCARDS), "OS_File {}", args[0]);
        }
    }
    assert_eq!(info(&mut os, b"RAM:$.F").map(|f| f.0), Ok(1), "kept");
    let typed = text(&mut os, b"RAM:$.F*");
    call(&mut os, FILE, &[18, typed, 0xFEB]).expect("its first match typed");
    let load = info(&mut os, b"RAM:$.F").expect("read").1;
    assert_eq!((load >> 8) & 0xFFF, 0xFEB);
}

#[test]
fn a_prefix_serves_its_own_name_and_the_temporary_filing_system_the_rest() {
    let mut kernel = disc();
    let mut os = kernel.os();
    control(&mut os, fs_control::SELECT, b"Resources", 0, 0).expect("current");
    assert_eq!(kind(&mut os, b"$.F"), 0, "not on the current filing system");
    control(&mut os, fs_control::PREFIX, b"RAM:x", 0, 0).expect("temporary");
    assert_eq!(kind(&mut os, b"$.F"), 1, "on the temporary filing system");
    call(&mut os, FS_CONTROL, &[fs_control::RESTORE]).expect("restored");
    assert_eq!(kind(&mut os, b"$.F"), 0);
    assert_eq!(kind(&mut os, b"RAM:$.F"), 1);
    assert_eq!(
        kind(&mut os, b"$.F"),
        0,
        "the prefix served its own name alone"
    );
    assert_eq!(line(&mut os, b"RAM:Info $.F"), Ok(()));
    assert_eq!(
        line(&mut os, b"IfThere RAM:$.F Then RAM:$.Missing"),
        Err(NOT_FOUND.number),
        "run where the prefix put it"
    );
    assert_eq!(kind(&mut os, b"$.F"), 0, "the line's prefix ended with it");
}

/// The leaf of the temporary filing system's current directory, as
/// `OS_GBPB` 6 reads it.
fn current_directory(os: &mut Os<'_>) -> Vec<u8> {
    let at = os.rma_claim(64).expect("room");
    call(os, GBPB, &[6, 0, at]).expect("read");
    let bytes = read(os, at, 64);
    let length = usize::from(bytes[1]);
    bytes[2..2 + length].to_vec()
}

#[test]
fn what_reads_the_filing_system_reads_the_temporary_one() {
    let mut kernel = disc();
    let mut os = kernel.os();
    line(&mut os, b"Dir RAM:$.D").expect("RAM current, D its directory");
    control(&mut os, fs_control::SELECT, b"Resources", 0, 0).expect("current");
    control(&mut os, fs_control::PREFIX, b"RAM:x", 0, 0).expect("temporary");
    let ram = control(&mut os, fs_control::PRESENT, b"RAM", 0, 0)
        .expect("RAM")
        .r[1];
    assert_eq!(call(&mut os, ARGS, &[0, 0]).expect("read").r[0], ram);
    assert_eq!(current_directory(&mut os), b"D");
    let name = text(&mut os, b"$.F");
    let handle = call(&mut os, FIND, &[0x40, name]).expect("opened").r[0];
    call(&mut os, FIND, &[0, 0]).expect("the temporary filing system's files closed");
    let status = call(&mut os, ARGS, &[254, handle]).expect("read").r[0];
    assert_eq!(status & 0x800, 0x800, "the handle unallocated");
    call(&mut os, FS_CONTROL, &[fs_control::UNSET_CURRENT]).expect("unset");
    assert_eq!(current_directory(&mut os), b"\"Unset\"");
    call(&mut os, FS_CONTROL, &[fs_control::RESTORE]).expect("restored");
    line(&mut os, b"Dir RAM:$.D").expect("set");
    line(&mut os, b"Dir RAM:$").expect("D now the previous directory");
    control(&mut os, fs_control::SELECT, b"Resources", 0, 0).expect("current");
    control(&mut os, fs_control::PREFIX, b"RAM:x", 0, 0).expect("temporary");
    call(&mut os, FS_CONTROL, &[fs_control::BACK]).expect("exchanged");
    assert_eq!(
        current_directory(&mut os),
        b"D",
        "RAM's directories exchanged"
    );
}

#[test]
fn nought_names_no_filing_system() {
    let mut kernel = disc();
    let mut os = kernel.os();
    control(&mut os, fs_control::SELECT, b"RAM", 0, 0).expect("current");
    let at = block(&mut os, &[0xEE; 16]);
    call(
        &mut os,
        FS_CONTROL,
        &[fs_control::NUMBER_TO_NAME, 0, at, 16],
    )
    .expect("named");
    assert_eq!(read(&mut os, at, 2), [0, 0xEE], "an empty name");
}
