//! Every call that changes a directory, and every process's end, leaves the
//! change on the disk rather than only in DOS's buffers: a machine switched
//! off at the prompt keeps what was done, as it does under DOS.

use super::harness::*;
use crate::asm::{Reg16, Reg8};
use crate::cpu::gpr;
use crate::machine::Template;
use crate::storage::floppy::Floppy;
use crate::storage::{BlockDevice, MediaError, MemoryDisk, SECTOR};
use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;

/// A diskette the test reads while it stays in the drive.
struct Watched(Rc<RefCell<MemoryDisk>>);

impl BlockDevice for Watched {
    fn sectors(&self) -> u64 {
        self.0.borrow().sectors()
    }

    fn read_only(&self) -> bool {
        false
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        self.0.borrow_mut().read(sector, out)
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        self.0.borrow_mut().write(sector, data)
    }
}

/// DOS booted from a watched diskette that also holds `files`.
fn watched(files: &[File<'_>]) -> (Dos, Rc<RefCell<MemoryDisk>>) {
    let shell = com(idle);
    let mut all = alloc::vec![File {
        name: b"COMMAND COM",
        bytes: &shell
    }];
    all.extend(files.iter().map(|f| File {
        name: f.name,
        bytes: f.bytes,
    }));
    let disk = Rc::new(RefCell::new(MemoryDisk::new(image(&all), false).unwrap()));
    let mut m = crate::bios::machine(&Template::IbmPc.config(), [None, None]).unwrap();
    let floppy = Floppy::raw(Box::new(Watched(Rc::clone(&disk)))).unwrap();
    m.board_mut().insert_floppy(0, floppy).unwrap();
    (Dos::shell_of(m), disk)
}

/// Whether the disk's root holds an entry named `name`.
fn listed(disk: &Rc<RefCell<MemoryDisk>>, name: &[u8; 11]) -> bool {
    root_entry(disk.borrow().bytes(), name).is_some()
}

#[test]
fn a_call_that_changes_a_directory_has_written_it_when_it_returns() {
    let (mut dos, disk) = watched(&[
        File {
            name: b"OLD     TXT",
            bytes: b"old",
        },
        File {
            name: b"GONE    TXT",
            bytes: b"x",
        },
    ]);
    let made = dos.asciz(DATA + 0x60, "MADE");
    assert!(!carry(&dos.call(with_path(0x39, 0, made))));
    assert!(listed(&disk, b"MADE       "), "made");
    let file = dos.asciz(DATA, "NEW.TXT");
    let created = dos.call(|r| with_path(0x3C, 0, file)(r));
    assert!(!carry(&created), "{created:X?}");
    assert!(listed(&disk, b"NEW     TXT"), "created, before any close");
    let old = dos.asciz(DATA, "OLD.TXT");
    let renamed = dos.asciz(DATA + 0x20, "KEPT.TXT");
    let moved = dos.call(|r| {
        with_path(0x56, 0, old)(r);
        r.gpr[gpr::DI] = renamed;
    });
    assert!(!carry(&moved), "{moved:X?}");
    assert!(
        listed(&disk, b"KEPT    TXT") && !listed(&disk, b"OLD     TXT"),
        "renamed"
    );
    let gone = dos.asciz(DATA, "GONE.TXT");
    assert!(!carry(&dos.call(with_path(0x41, 0, gone))));
    assert!(!listed(&disk, b"GONE    TXT"), "deleted");
    assert!(!carry(&dos.call(with_path(0x3A, 0, made))));
    assert!(!listed(&disk, b"MADE       "), "removed");
}

#[test]
fn an_fcb_delete_and_rename_have_written_the_directory_when_they_return() {
    let (mut dos, disk) = watched(&[
        File {
            name: b"ONE     DAT",
            bytes: b"1",
        },
        File {
            name: b"TWO     DAT",
            bytes: b"2",
        },
    ]);
    let mut fcb = [0u8; 37];
    fcb[1..12].copy_from_slice(b"ONE     DAT");
    fcb[17..28].copy_from_slice(b"UNO     DAT");
    dos.put(DATA, &fcb);
    let renamed = dos.call(|r| {
        r.gpr[gpr::AX] = 0x1700;
        r.gpr[gpr::DX] = DATA;
    });
    assert_eq!(renamed.gpr[gpr::AX] & 0xFF, 0, "{renamed:X?}");
    assert!(
        listed(&disk, b"UNO     DAT") && !listed(&disk, b"ONE     DAT"),
        "renamed"
    );
    fcb[1..12].copy_from_slice(b"TWO     DAT");
    dos.put(DATA, &fcb);
    let deleted = dos.call(|r| {
        r.gpr[gpr::AX] = 0x1300;
        r.gpr[gpr::DX] = DATA;
    });
    assert_eq!(deleted.gpr[gpr::AX] & 0xFF, 0, "{deleted:X?}");
    assert!(!listed(&disk, b"TWO     DAT"), "deleted");
}

/// A program that rewrites the first record of `HELD.DAT` through an FCB,
/// which nothing closes when its process ends, then `ends` its run.
fn rewriter(ends: impl FnOnce(&mut crate::asm::Asm)) -> alloc::vec::Vec<u8> {
    com(|asm| {
        let fcb = asm.label();
        let record = asm.label();
        for (function, at) in [(0x0F, &fcb), (0x1A, &record), (0x15, &fcb)] {
            asm.mov_imm8(Reg8::Ah, function);
            asm.mov_label(Reg16::Dx, at);
            asm.int(0x21);
        }
        ends(asm);
        asm.bind(&fcb);
        let mut block = [0u8; 37];
        block[1..12].copy_from_slice(b"HELD    DAT");
        asm.bytes(&block);
        asm.bind(&record);
        let mut data = [b' '; 128];
        data[..RECORD.len()].copy_from_slice(RECORD);
        asm.bytes(&data);
    })
}

const RECORD: &[u8; 22] = b"WRITTEN BEFORE THE END";

/// The machine with `program` started as `WRITE.COM`, its EXEC under way.
fn running(program: &[u8]) -> (Dos, Rc<RefCell<MemoryDisk>>, u16) {
    let held = [b'o'; 512];
    let (mut dos, disk) = watched(&[
        File {
            name: b"WRITE   COM",
            bytes: program,
        },
        File {
            name: b"HELD    DAT",
            bytes: &held,
        },
    ]);
    make_room(&mut dos);
    let path = dos.asciz(DATA, "WRITE.COM");
    exec_block(&mut dos, DATA + 0x40);
    let end = dos.begin(&[0xCD, 0x21], |r| {
        with_path(0x4B, 0, path)(r);
        r.gpr[gpr::BX] = DATA + 0x40;
    });
    (dos, disk, end)
}

/// Whether `HELD.DAT` holds the record on the disk itself.
fn on_disk(disk: &Rc<RefCell<MemoryDisk>>) -> bool {
    root_file(disk.borrow().bytes(), b"HELD    DAT").is_some_and(|data| data.starts_with(RECORD))
}

#[test]
fn a_process_that_ends_with_a_file_unclosed_has_its_writes_on_the_disk() {
    let (mut dos, disk, _) = running(&rewriter(|asm| {
        let here = asm.label();
        asm.bind(&here);
        asm.jmp_short(&here);
    }));
    let now = dos.m.now();
    crate::bios::run_until(&mut dos.m, now + 5 * crate::time::NANOS_PER_SECOND);
    assert!(
        !on_disk(&disk),
        "a running program's record is still in the buffers"
    );
    let (mut dos, disk, end) = running(&rewriter(|asm| {
        asm.mov_imm16(Reg16::Ax, 0x4C00);
        asm.int(0x21);
    }));
    let ran = dos.finish(end);
    assert!(!carry(&ran), "{ran:X?}");
    assert!(
        on_disk(&disk),
        "the record left the buffers when the program ended"
    );
}
