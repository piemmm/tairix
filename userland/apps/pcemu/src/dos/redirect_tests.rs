//! A folder shared with TAIRiX DOS as a drive, served in the tests by the
//! front-end's own server over an in-memory folder.

use super::*;
use crate::asm::Reg16;
use crate::cpu::{gpr, Bus, Regs, Seg};
use crate::dos::harness::*;
use crate::folder::memhost::MemHost;
use crate::folder::server::Server;
use crate::folder::{DosName, Refusal, Share};
use alloc::boxed::Box;
use alloc::vec::Vec;
use tairix_abi::time::{DosTime, Time64};
use tairix_fat::dir::attr;

/// Where the tests' calls keep the bytes they move.
const BUFFER: u16 = DATA + 0x200;

fn dos_name(text: &str) -> DosName {
    DosName::new(name::field(text.as_bytes(), false).unwrap(), false).unwrap()
}

/// TAIRiX DOS booted with `host` as its share 0, writable where `writable`:
/// drive C:, the first after the diskettes' A: and B:.
fn shared(host: &MemHost, writable: bool) -> Dos {
    let mut m = Dos::machine(&[]);
    let label = dos_name("HOST");
    let server = Server::new(host.root(), label, writable, 0x00AB_CDEF);
    m.board_mut()
        .share(0, Share::new(label, writable, Box::new(server)));
    let mut dos = Dos::shell_of(m);
    assert_eq!(share_drive(&mut dos), Some(2));
    dos
}

fn share_drive(dos: &mut Dos) -> Option<u8> {
    dos.m.board_mut().share_mut(0).and_then(|share| share.drive)
}

/// What a handle call answered: AX, or with the carry the error.
fn outcome(regs: &Regs) -> Result<u16, u16> {
    if carry(regs) {
        Err(ax(regs))
    } else {
        Ok(ax(regs))
    }
}

/// Function AH with AL on `path`.
fn on_path(dos: &mut Dos, ah: u8, al: u8, path: &str) -> Result<u16, u16> {
    let at = dos.asciz(DATA, path);
    outcome(&dos.call(with_path(ah, al, at)))
}

/// Function `3Ch` with attributes `attributes`.
fn create(dos: &mut Dos, path: &str, attributes: u16) -> Result<u16, u16> {
    let at = dos.asciz(DATA, path);
    outcome(&dos.call(|r| {
        with_path(0x3C, 0, at)(r);
        r.gpr[gpr::CX] = attributes;
    }))
}

fn read(dos: &mut Dos, handle: u16, len: u16) -> Result<Vec<u8>, u16> {
    let regs = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3F00;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = len;
        r.gpr[gpr::DX] = BUFFER;
    });
    outcome(&regs).map(|count| dos.get(BUFFER, count))
}

fn write(dos: &mut Dos, handle: u16, bytes: &[u8]) -> Result<u16, u16> {
    dos.put(BUFFER, bytes);
    outcome(&dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = u16::try_from(bytes.len()).unwrap();
        r.gpr[gpr::DX] = BUFFER;
    }))
}

/// Function AX on `handle`.
fn on_handle(dos: &mut Dos, function: u16, handle: u16) -> Result<u16, u16> {
    outcome(&dos.call(|r| {
        r.gpr[gpr::AX] = function;
        r.gpr[gpr::BX] = handle;
    }))
}

/// The names a search for `pattern` with attributes `attributes` finds in
/// turn, with each one's attribute byte.
fn found(dos: &mut Dos, pattern: &str, attributes: u16) -> Vec<(Vec<u8>, u8)> {
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = BUFFER;
    });
    let at = dos.asciz(DATA, pattern);
    let mut names = Vec::new();
    let mut regs = dos.call(|r| {
        with_path(0x4E, 0, at)(r);
        r.gpr[gpr::CX] = attributes;
    });
    while !carry(&regs) {
        let record = dos.get(BUFFER, 43);
        let name = record[0x1E..]
            .iter()
            .take_while(|&&b| b != 0)
            .copied()
            .collect();
        names.push((name, record[0x15]));
        regs = dos.call(|r| r.gpr[gpr::AX] = 0x4F00);
    }
    assert_eq!(ax(&regs), error::NO_MORE_FILES);
    names
}

fn names(found: &[(Vec<u8>, u8)]) -> Vec<&[u8]> {
    found.iter().map(|(name, _)| name.as_slice()).collect()
}

#[test]
fn a_share_is_the_first_drive_free_and_a_network_drive_with_no_driver() {
    let host = MemHost::new();
    let mut dos = shared(&host, true);
    let flags = kernel16(&dos.m, cds_at(2) + cds::FLAGS);
    assert_eq!(flags, cds::NETWORK);
    let attributes = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4409;
        r.gpr[gpr::BX] = 3;
    });
    assert_eq!(
        attributes.gpr[gpr::DX],
        crate::dos::ioctl::drive_bits::REMOTE
    );
    let removable = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4408;
        r.gpr[gpr::BX] = 3;
    });
    assert_eq!(
        outcome(&removable),
        Err(error::INVALID_FUNCTION),
        "a network drive has no driver to ask, as DOS answers"
    );
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x0E00;
        r.gpr[gpr::DX] = 2;
    });
    let current = dos.call(|r| r.gpr[gpr::AX] = 0x1900);
    assert_eq!(ax(&current) & 0xFF, 2, "C: is selected");
    let directory = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4700;
        r.gpr[gpr::DX] = 0;
        r.gpr[gpr::SI] = BUFFER;
    });
    assert!(!carry(&directory));
    assert_eq!(dos.get(BUFFER, 1), [0], "at its root");
}

#[test]
fn a_file_of_the_share_is_read_and_its_end_is_where_the_host_has_it_now() {
    let host = MemHost::new().file("readme.txt", b"Hello, share.");
    let mut dos = shared(&host, true);
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\README.TXT").unwrap();
    assert_eq!(read(&mut dos, handle, 100).unwrap(), b"Hello, share.");
    assert_eq!(read(&mut dos, handle, 100).unwrap(), b"", "at its end");
    host.replace("readme.txt", b"Hello, share. Again.");
    let end = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4202;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = 0;
    });
    assert_eq!(outcome(&end), Ok(20), "the end the host has now");
    let back = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4200;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = 13;
    });
    assert_eq!(outcome(&back), Ok(13));
    assert_eq!(read(&mut dos, handle, 100).unwrap(), b" Again.");
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
    assert_eq!(read(&mut dos, handle, 1), Err(error::INVALID_HANDLE));
}

#[test]
fn a_file_made_on_the_share_holds_what_dos_wrote_and_the_stamp_it_set() {
    let host = MemHost::new();
    let mut dos = shared(&host, true);
    let handle = create(&mut dos, "C:\\NOTES.TXT", 0).unwrap();
    assert_eq!(write(&mut dos, handle, b"Hello, host."), Ok(12));
    let stamp = DosTime {
        date: (15 << 9) | (6 << 5) | 15,
        time: (12 << 11) | (30 << 5),
    };
    let set = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5701;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = stamp.time;
        r.gpr[gpr::DX] = stamp.date;
    });
    assert!(!carry(&set));
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
    assert_eq!(host.bytes("NOTES.TXT").unwrap(), b"Hello, host.");
    assert_eq!(host.modified("NOTES.TXT"), stamp.to_time64());
    let again = create(&mut dos, "C:\\NOTES.TXT", 0).unwrap();
    assert!(on_handle(&mut dos, 0x3E00, again).is_ok());
    assert_eq!(host.bytes("NOTES.TXT").unwrap(), b"", "made again, emptied");
    let new = dos.call(|r| {
        with_path(0x5B, 0, DATA)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert_eq!(
        outcome(&new),
        Err(error::FILE_EXISTS),
        "function 5Bh makes only a new file"
    );
    assert_eq!(
        create(&mut dos, "C:\\LABEL", u16::from(attr::VOLUME_ID)),
        Err(error::ACCESS_DENIED),
        "a share's label is the name it was shared under"
    );
}

#[test]
fn a_write_of_nothing_ends_the_file_where_it_stands() {
    let host = MemHost::new().file("data.bin", &[7; 100]);
    let mut dos = shared(&host, true);
    let handle = on_path(&mut dos, 0x3D, 2, "C:\\DATA.BIN").unwrap();
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x4200;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = 40;
    });
    assert_eq!(write(&mut dos, handle, &[]), Ok(0));
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
    assert_eq!(host.bytes("data.bin").unwrap(), [7; 40]);
}

#[test]
fn a_host_name_dos_could_not_hold_is_reached_by_its_alias() {
    let host = MemHost::new()
        .file("Long Report.text", b"aliased")
        .file("short.txt", b"own");
    let mut dos = shared(&host, true);
    let listed = found(&mut dos, "C:\\*.*", 0);
    assert_eq!(names(&listed), [&b"LONGRE~1.TEX"[..], b"SHORT.TXT"]);
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\LONGRE~1.TEX").unwrap();
    assert_eq!(read(&mut dos, handle, 20).unwrap(), b"aliased");
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
}

#[test]
fn a_directory_of_the_share_is_made_entered_listed_and_removed() {
    let host = MemHost::new();
    let mut dos = shared(&host, true);
    assert!(on_path(&mut dos, 0x39, 0, "C:\\SUB").is_ok());
    assert!(on_path(&mut dos, 0x3B, 0, "C:\\SUB").is_ok());
    let directory = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4700;
        r.gpr[gpr::DX] = 3;
        r.gpr[gpr::SI] = BUFFER;
    });
    assert!(!carry(&directory));
    assert_eq!(dos.get(BUFFER, 4), b"SUB\0");
    let made = create(&mut dos, "C:INNER.TXT", 0).unwrap();
    assert!(on_handle(&mut dos, 0x3E00, made).is_ok());
    assert_eq!(host.names("SUB"), ["INNER.TXT"]);
    let listed = found(&mut dos, "C:*.*", u16::from(attr::DIRECTORY));
    assert_eq!(names(&listed), [&b"."[..], b"..", b"INNER.TXT"]);
    assert_eq!(listed[0].1, attr::DIRECTORY);
    assert_eq!(
        on_path(&mut dos, 0x3A, 0, "C:\\SUB"),
        Err(error::CURRENT_DIRECTORY)
    );
    assert!(on_path(&mut dos, 0x3B, 0, "C:\\").is_ok());
    assert_eq!(
        on_path(&mut dos, 0x3A, 0, "C:\\SUB"),
        Err(error::ACCESS_DENIED),
        "a directory holding a file stays"
    );
    assert!(on_path(&mut dos, 0x41, 0, "C:\\SUB\\INNER.TXT").is_ok());
    assert!(on_path(&mut dos, 0x3A, 0, "C:\\SUB").is_ok());
    assert!(host.names("").is_empty());
    assert_eq!(
        on_path(&mut dos, 0x3B, 0, "C:\\SUB"),
        Err(error::PATH_NOT_FOUND)
    );
}

#[test]
fn a_file_is_renamed_within_the_share_and_never_off_it() {
    let host = MemHost::new().file("a.txt", b"a").dir("sub");
    let mut dos = shared(&host, true);
    let from = dos.asciz(DATA, "C:\\A.TXT");
    let to = dos.asciz(DATA + 0x80, "C:\\SUB\\B.TXT");
    let renamed = dos.call(|r| {
        with_path(0x56, 0, from)(r);
        r.gpr[gpr::DI] = to;
    });
    assert!(!carry(&renamed), "{renamed:X?}");
    assert_eq!(host.bytes("sub/B.TXT").unwrap(), b"a");
    let from = dos.asciz(DATA, "C:\\SUB\\B.TXT");
    let to = dos.asciz(DATA + 0x80, "A:\\B.TXT");
    let across = dos.call(|r| {
        with_path(0x56, 0, from)(r);
        r.gpr[gpr::DI] = to;
    });
    assert_eq!(outcome(&across), Err(error::NOT_SAME_DEVICE));
}

#[test]
fn a_file_the_host_keeps_from_change_is_read_only_to_dos() {
    let host = MemHost::new().file("kept.txt", b"kept").locked("kept.txt");
    let mut dos = shared(&host, true);
    assert_eq!(
        on_path(&mut dos, 0x43, 0, "C:\\KEPT.TXT"),
        Ok(u16::from(attr::ARCHIVE | attr::READ_ONLY))
    );
    let denied = Err(error::ACCESS_DENIED);
    assert_eq!(on_path(&mut dos, 0x3D, 1, "C:\\KEPT.TXT"), denied);
    assert_eq!(on_path(&mut dos, 0x41, 0, "C:\\KEPT.TXT"), denied);
    assert_eq!(create(&mut dos, "C:\\KEPT.TXT", 0), denied);
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\KEPT.TXT").unwrap();
    assert_eq!(read(&mut dos, handle, 10).unwrap(), b"kept");
    assert_eq!(write(&mut dos, handle, b"x"), denied, "opened to read");
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
}

#[test]
fn a_share_given_to_read_refuses_every_change() {
    let host = MemHost::new().file("seen.txt", b"seen");
    let mut dos = shared(&host, false);
    let denied = Err(error::ACCESS_DENIED);
    assert_eq!(create(&mut dos, "C:\\NEW.TXT", 0), denied);
    assert_eq!(on_path(&mut dos, 0x39, 0, "C:\\DIR"), denied);
    assert_eq!(on_path(&mut dos, 0x41, 0, "C:\\SEEN.TXT"), denied);
    assert_eq!(on_path(&mut dos, 0x3D, 2, "C:\\SEEN.TXT"), denied);
    let listed = found(&mut dos, "C:\\*.*", 0);
    assert_eq!(
        listed,
        [(b"SEEN.TXT".to_vec(), attr::ARCHIVE | attr::READ_ONLY)]
    );
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\SEEN.TXT").unwrap();
    let stamped = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5701;
        r.gpr[gpr::BX] = handle;
    });
    assert_eq!(outcome(&stamped), denied, "nor are its stamps kept");
    assert_eq!(read(&mut dos, handle, 10).unwrap(), b"seen");
    assert!(on_handle(&mut dos, 0x3E00, handle).is_ok());
    assert_eq!(host.names(""), ["seen.txt"]);
}

#[test]
fn the_space_of_a_share_is_its_volumes_and_it_has_no_dpb() {
    let host = MemHost::new().file("used.bin", &[0; 5 * 512]);
    let mut dos = shared(&host, true);
    let free = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3600;
        r.gpr[gpr::DX] = 3;
    });
    assert_eq!(
        (
            free.gpr[gpr::AX],
            free.gpr[gpr::BX],
            free.gpr[gpr::CX],
            free.gpr[gpr::DX]
        ),
        (1, 4096 - 5, 512, 4096),
        "the in-memory volume's 4096 blocks of 512 bytes"
    );
    let info = dos.call(|r| {
        r.gpr[gpr::AX] = 0x1C00;
        r.gpr[gpr::DX] = 3;
    });
    assert_eq!(
        (
            info.gpr[gpr::AX] & 0xFF,
            info.gpr[gpr::CX],
            info.gpr[gpr::DX]
        ),
        (1, 512, 4096)
    );
    let media = crate::cpu::linear(info.sreg(Seg::Ds), info.gpr[gpr::BX]);
    assert_eq!(dos.m.board().peek(media), tairix_fat::format::FIXED_MEDIA);
    let dpb = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3200;
        r.gpr[gpr::DX] = 3;
    });
    assert_eq!(dpb.gpr[gpr::AX] & 0xFF, 0xFF, "no DPB to point at");
}

#[test]
fn the_label_of_a_share_is_the_name_it_was_shared_under() {
    let host = MemHost::new().file("file.txt", b"");
    let mut dos = shared(&host, true);
    let labels = found(&mut dos, "C:\\*.*", u16::from(attr::VOLUME_ID));
    assert_eq!(
        labels,
        [(b"HOST".to_vec(), attr::VOLUME_ID)],
        "the label alone"
    );
    let all = found(
        &mut dos,
        "C:\\*.*",
        u16::from(attr::VOLUME_ID | attr::DIRECTORY),
    );
    assert_eq!(
        names(&all),
        [&b"HOST"[..], b"FILE.TXT"],
        "then the root's entries"
    );
}

#[test]
fn a_share_withdrawn_cuts_its_open_files_off_and_its_drive_goes() {
    let host = MemHost::new().file("open.txt", b"open");
    let mut dos = shared(&host, true);
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\OPEN.TXT").unwrap();
    super::detach(&mut dos.m, 0);
    dos.m.board_mut().unshare(0);
    assert_eq!(read(&mut dos, handle, 4), Err(error::NAME_DELETED));
    assert!(
        on_handle(&mut dos, 0x3E00, handle).is_ok(),
        "closed all the same"
    );
    assert_eq!(kernel16(&dos.m, cds_at(2) + cds::FLAGS), 0);
    assert_eq!(
        on_path(&mut dos, 0x3D, 0, "C:\\OPEN.TXT"),
        Err(error::INVALID_DRIVE)
    );
}

#[test]
fn a_share_attached_after_the_boot_takes_its_drive_at_once() {
    let host = MemHost::new().file("late.txt", b"late");
    let mut dos = Dos::boot(&[]);
    let label = dos_name("LATE");
    let server = Server::new(host.root(), label, true, 9);
    dos.m
        .board_mut()
        .share(1, Share::new(label, true, Box::new(server)));
    super::attach(&mut dos.m);
    assert_eq!(super::unannounced(&mut dos.m), Some((1, Some(2))));
    assert_eq!(super::unannounced(&mut dos.m), None, "told once");
    let handle = on_path(&mut dos, 0x3D, 0, "C:\\LATE.TXT").unwrap();
    assert_eq!(read(&mut dos, handle, 10).unwrap(), b"late");
}

#[test]
fn a_program_on_the_share_runs() {
    let child = com(|asm| {
        asm.mov_imm16(Reg16::Ax, 0x4C2A);
        asm.int(0x21);
    });
    let host = MemHost::new().file("child.com", &child);
    let mut dos = shared(&host, true);
    let shell = dos.psp;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x4A00;
        r.gpr[gpr::BX] = 0x1000;
        r.set_sreg(Seg::Es, shell);
    });
    let path = dos.asciz(DATA, "C:\\CHILD.COM");
    let block = DATA + 0x40;
    let tail = DATA + 0x60;
    dos.put(tail, &[0, 0x0D]);
    let mut parameters = [0u8; 14];
    parameters[2..4].copy_from_slice(&tail.to_le_bytes());
    parameters[4..6].copy_from_slice(&shell.to_le_bytes());
    parameters[6..8].copy_from_slice(&(DATA + 0x70).to_le_bytes());
    parameters[8..10].copy_from_slice(&shell.to_le_bytes());
    parameters[10..12].copy_from_slice(&(DATA + 0x70).to_le_bytes());
    parameters[12..14].copy_from_slice(&shell.to_le_bytes());
    dos.put(block, &parameters);
    let ran = dos.call(|r| {
        with_path(0x4B, 0, path)(r);
        r.gpr[gpr::BX] = block;
    });
    assert!(!carry(&ran), "{ran:X?}");
    let code = dos.call(|r| r.gpr[gpr::AX] = 0x4D00);
    assert_eq!(ax(&code), 0x002A);
}

#[test]
fn an_fcb_file_of_the_share_is_made_written_read_sized_renamed_and_deleted() {
    let host = MemHost::new();
    let mut dos = shared(&host, true);
    let fcb = DATA;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = BUFFER;
    });
    let on_share = |dos: &mut Dos, name: &[u8; 11]| {
        fcb_at(dos, fcb, name);
        dos.put(fcb, &[3]);
    };
    on_share(&mut dos, b"records dat");
    assert_eq!(fcb_call(&mut dos, 0x16, fcb), 0, "made");
    for record in 0..3u8 {
        dos.put(BUFFER, &[b'A' + record; 128]);
        assert_eq!(fcb_call(&mut dos, 0x15, fcb), 0, "record {record} written");
    }
    assert_eq!(fcb_call(&mut dos, 0x10, fcb), 0, "closed");
    let held = host.bytes("RECORDS.DAT").unwrap();
    assert_eq!(held.len(), 384);
    assert!(held[256..].iter().all(|&b| b == b'C'));
    on_share(&mut dos, b"RECORDS DAT");
    assert_eq!(fcb_call(&mut dos, 0x0F, fcb), 0, "opened");
    assert_eq!(dos.get(fcb + 0x10, 4), 384u32.to_le_bytes());
    dos.put(fcb + 0x21, &2u32.to_le_bytes());
    assert_eq!(fcb_call(&mut dos, 0x21, fcb), 0, "random read");
    assert_eq!(dos.get(BUFFER, 2), b"CC");
    assert_eq!(fcb_call(&mut dos, 0x10, fcb), 0);
    on_share(&mut dos, b"RECORDS DAT");
    dos.put(fcb + 0x0E, &64u16.to_le_bytes());
    assert_eq!(fcb_call(&mut dos, 0x23, fcb), 0);
    assert_eq!(dos.get(fcb + 0x21, 3), [6, 0, 0]);
    let mut rename = [0u8; 37];
    rename[0] = 3;
    rename[1..12].copy_from_slice(b"RECORDS DAT");
    rename[0x11..0x1C].copy_from_slice(b"????????BAK");
    dos.put(fcb, &rename);
    assert_eq!(fcb_call(&mut dos, 0x17, fcb), 0, "renamed");
    assert_eq!(host.names(""), ["RECORDS.BAK"]);
    on_share(&mut dos, b"????????BAK");
    assert_eq!(fcb_call(&mut dos, 0x11, fcb), 0, "found");
    assert_eq!(dos.get(BUFFER, 12), b"\x03RECORDS BAK");
    assert_eq!(fcb_call(&mut dos, 0x13, fcb), 0, "deleted");
    assert!(host.names("").is_empty());
    assert_eq!(fcb_call(&mut dos, 0x11, fcb), 0xFF, "gone");
}

#[test]
fn an_fcb_rename_by_a_pattern_renames_each_match_once() {
    let host = MemHost::new().file("a.txt", b"").file("b.txt", b"");
    let mut dos = shared(&host, true);
    let mut rename = [0u8; 37];
    rename[0] = 3;
    rename[1..12].copy_from_slice(b"????????TXT");
    // Each new name still matches the pattern: renamed where it lay, it is
    // not come on again.
    rename[0x11..0x1C].copy_from_slice(b"?X??????TXT");
    dos.put(DATA, &rename);
    assert_eq!(fcb_call(&mut dos, 0x17, DATA), 0);
    assert_eq!(host.names(""), ["AX.TXT", "BX.TXT"]);
}

#[test]
fn an_fcb_search_of_the_share_keeps_its_place_in_the_fcb_and_its_drive() {
    let host = MemHost::new()
        .file("alpha.txt", b"a")
        .file("beta.txt", b"bb");
    let mut dos = shared(&host, true);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = BUFFER;
    });
    let mut search = [0u8; 44];
    search[0] = 0xFF;
    search[6] = attr::HIDDEN;
    search[7] = 3;
    search[8..19].copy_from_slice(b"????????TXT");
    dos.put(DATA, &search);
    assert_eq!(fcb_call(&mut dos, 0x11, DATA), 0);
    assert_eq!(dos.get(BUFFER, 8), [0xFF, 0, 0, 0, 0, 0, attr::HIDDEN, 3]);
    assert_eq!(dos.get(BUFFER + 8, 11), b"ALPHA   TXT");
    assert_eq!(
        dos.get(DATA + 7 + 0x15, 1),
        [super::REMOTE_SEARCH | 3],
        "the search drive marked remote"
    );
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x0E00;
        r.gpr[gpr::DX] = 0;
    });
    assert_eq!(fcb_call(&mut dos, 0x12, DATA), 0, "on C: still, from A:");
    assert_eq!(dos.get(BUFFER + 8, 11), b"BETA    TXT");
    assert_eq!(
        dos.get(BUFFER + 8 + 0x1C, 4),
        2u32.to_le_bytes(),
        "its size"
    );
    assert_eq!(fcb_call(&mut dos, 0x12, DATA), 0xFF);
}

#[test]
fn an_fcb_delete_by_a_pattern_keeps_what_the_host_keeps() {
    let host = MemHost::new()
        .file("one.tmp", b"")
        .file("two.tmp", b"")
        .file("kept.tmp", b"")
        .locked("kept.tmp");
    let mut dos = shared(&host, true);
    fcb_at(&mut dos, DATA, b"????????TMP");
    dos.put(DATA, &[3]);
    assert_eq!(fcb_call(&mut dos, 0x13, DATA), 0);
    assert_eq!(host.names(""), ["kept.tmp"]);
    assert_eq!(
        fcb_call(&mut dos, 0x13, DATA),
        0xFF,
        "nothing left to delete"
    );
}

#[test]
fn a_refusal_of_the_share_is_its_dos_error() {
    let cases = [
        (Refusal::NotFound, error::FILE_NOT_FOUND),
        (Refusal::PathNotFound, error::PATH_NOT_FOUND),
        (Refusal::Denied, error::ACCESS_DENIED),
        (Refusal::NotEmpty, error::ACCESS_DENIED),
        (Refusal::Full, error::ACCESS_DENIED),
        (Refusal::Exists, error::FILE_EXISTS),
        (Refusal::NoMore, error::NO_MORE_FILES),
        (Refusal::TooMany, error::TOO_MANY_OPEN),
        (Refusal::Handle, error::INVALID_HANDLE),
        (Refusal::Fault, error::NETWORK_ERROR),
        (Refusal::Gone, error::NAME_DELETED),
    ];
    for (refusal, error) in cases {
        assert_eq!(super::code(refusal), error, "{refusal:?}");
    }
}

#[test]
fn a_stamp_outside_dos_years_is_the_nearest_dos_holds() {
    let early = super::stamp(Time64::from_secs(0));
    assert_eq!(early, DosTime::from_time64(DosTime::EARLIEST).unwrap());
    let late = super::stamp(Time64::from_secs(i64::MAX / 2));
    assert_eq!(late, DosTime::from_time64(DosTime::LATEST).unwrap());
}

#[test]
fn the_shell_lists_types_and_copies_on_a_share_as_on_a_disk() {
    let host = MemHost::new()
        .file("readme.txt", b"From the host.\r\n")
        .dir("docs");
    let mut m = machine_of(&crate::machine::Template::IbmPc.config(), [None, None]);
    let label = dos_name("HOST");
    let server = Server::new(host.root(), label, true, 0x0D05);
    m.board_mut()
        .share(0, Share::new(label, true, Box::new(server)));
    let mut m = prompted_in(m, &diskette(), &[]);
    typed(&mut m, "dir c:");
    let text = screen(&m);
    assert!(text.contains(" Volume in drive C is HOST"), "{text}");
    assert!(text.contains(" Directory of C:\\"), "{text}");
    assert!(text.contains("README   TXT        16"), "{text}");
    assert!(text.contains("DOCS         <DIR>"), "{text}");
    assert!(text.contains("bytes free"), "{text}");
    typed(&mut m, "type c:readme.txt");
    assert!(screen(&m).contains("From the host."), "{}", screen(&m));
    typed(&mut m, "copy c:readme.txt c:docs\\copy.txt");
    assert!(screen(&m).contains("1 file(s) copied"), "{}", screen(&m));
    assert_eq!(host.bytes("docs/COPY.TXT").unwrap(), b"From the host.\r\n");
    typed(&mut m, "c:");
    typed(&mut m, "cd docs");
    typed(&mut m, "del copy.txt");
    let text = screen(&m);
    assert!(text.trim_end().ends_with("C:\\DOCS>"), "{text}");
    assert!(host.names("docs").is_empty());
}
