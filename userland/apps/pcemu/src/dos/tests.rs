//! TAIRiX DOS booted on an emulated PC from a floppy built here: a FAT12
//! volume made through `lib/fat`, its boot record TAIRiX DOS's own, and
//! programs assembled by the crate's encoder.

use super::harness::*;
use super::*;
use crate::asm::{Asm, Cond, Reg16, Reg8};
use crate::bios::run_until;
use crate::chips::keyboard::{us_key, Scan};
use crate::cpu::{flag, gpr, Bus, Regs, Seg};
use crate::machine::{Machine, Template};
use crate::time::Nanos;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use tairix_fat::dir::attr;

#[test]
fn the_boot_starts_the_shell() {
    let shell = com(|asm| {
        print(asm, b"The shell is running.\r\n$");
        stop(asm);
    });
    let m = booted(
        &[File {
            name: b"COMMAND COM",
            bytes: &shell,
        }],
        20 * SECOND,
    );
    let text = screen(&m);
    let state = state(&m);
    assert!(text.contains("Starting TAIRiX DOS..."), "{text}{state}");
    assert!(text.contains("The shell is running."), "{text}{state}");
}

#[test]
fn a_file_is_created_written_read_back_and_deleted() {
    let mut dos = Dos::boot(&[]);
    let name = dos.asciz(DATA, "A:\\NOTES.TXT");
    let created = dos.call(|r| {
        with_path(0x3C, 0, name)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&created), "{created:X?}");
    let handle = ax(&created);
    dos.put(DATA + 0x80, b"Hello, DOS.");
    let wrote = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 11;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert!(!carry(&wrote) && ax(&wrote) == 11, "{wrote:X?}");
    let closed = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = handle;
    });
    assert!(!carry(&closed), "{closed:X?}");
    let opened = dos.call(with_path(0x3D, 0, name));
    assert!(!carry(&opened), "{opened:X?}");
    let handle = ax(&opened);
    let read = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3F00;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 64;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    assert_eq!(ax(&read), 11, "{read:X?}");
    assert_eq!(dos.get(DATA + 0x100, 11), b"Hello, DOS.");
    let size = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4202;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = 0;
    });
    assert_eq!((ax(&size), size.gpr[gpr::DX]), (11, 0));
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = handle;
    });
    let attributes = dos.call(with_path(0x43, 0, name));
    assert_eq!(attributes.gpr[gpr::CX], u16::from(attr::ARCHIVE));
    let deleted = dos.call(with_path(0x41, 0, name));
    assert!(!carry(&deleted), "{deleted:X?}");
    let gone = dos.call(with_path(0x3D, 0, name));
    assert!(carry(&gone));
    assert_eq!(ax(&gone), fs::error::FILE_NOT_FOUND);
}

#[test]
fn a_directory_is_made_entered_listed_and_removed() {
    let mut dos = Dos::boot(&[]);
    let sub = dos.asciz(DATA, "WORK");
    assert!(!carry(&dos.call(with_path(0x39, 0, sub))));
    assert!(!carry(&dos.call(with_path(0x3B, 0, sub))));
    let cwd = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4700;
        r.gpr[gpr::DX] = 0;
        r.gpr[gpr::SI] = DATA + 0x100;
    });
    assert!(!carry(&cwd));
    assert_eq!(dos.get(DATA + 0x100, 5), b"WORK\0");
    let file = dos.asciz(DATA + 0x40, "INSIDE.DAT");
    let created = dos.call(|r| {
        with_path(0x3C, 0, file)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&created));
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = ax(&created);
    });
    let up = dos.asciz(DATA + 0x60, "..");
    assert!(!carry(&dos.call(with_path(0x3B, 0, up))));
    let busy = dos.call(with_path(0x3A, 0, sub));
    assert!(carry(&busy));
    assert_eq!(ax(&busy), fs::error::ACCESS_DENIED);
    let inside = dos.asciz(DATA + 0x80, "WORK\\INSIDE.DAT");
    assert!(!carry(&dos.call(with_path(0x41, 0, inside))));
    assert!(!carry(&dos.call(with_path(0x3A, 0, sub))));
    assert!(carry(&dos.call(with_path(0x3B, 0, sub))));
}

#[test]
fn a_search_finds_each_match_in_turn_through_the_dta() {
    let mut dos = Dos::boot(&[
        File {
            name: b"ALPHA   TXT",
            bytes: b"a",
        },
        File {
            name: b"BETA    TXT",
            bytes: b"bb",
        },
        File {
            name: b"GAMMA   DOC",
            bytes: b"ccc",
        },
    ]);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    let pattern = dos.asciz(DATA, "*.TXT");
    let first = dos.call(|r| {
        with_path(0x4E, 0, pattern)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&first), "{first:X?}");
    assert_eq!(dos.get(DATA + 0x100 + 0x1E, 10), b"ALPHA.TXT\0");
    assert_eq!(dos.get(DATA + 0x100 + 0x1A, 4), [1, 0, 0, 0]);
    let second = dos.call(|r| r.gpr[gpr::AX] = 0x4F00);
    assert!(!carry(&second));
    assert_eq!(dos.get(DATA + 0x100 + 0x1E, 9), b"BETA.TXT\0");
    let none = dos.call(|r| r.gpr[gpr::AX] = 0x4F00);
    assert!(carry(&none));
    assert_eq!(ax(&none), fs::error::NO_MORE_FILES);
}

#[test]
fn a_rename_moves_a_file_between_directories() {
    let mut dos = Dos::boot(&[File {
        name: b"OLD     TXT",
        bytes: b"kept",
    }]);
    let sub = dos.asciz(DATA, "DEST");
    assert!(!carry(&dos.call(with_path(0x39, 0, sub))));
    let old = dos.asciz(DATA + 0x20, "OLD.TXT");
    let new = dos.asciz(DATA + 0x40, "DEST\\NEW.TXT");
    let renamed = dos.call(|r| {
        with_path(0x56, 0, old)(r);
        r.gpr[gpr::DI] = new;
    });
    assert!(!carry(&renamed), "{renamed:X?}");
    assert!(carry(&dos.call(with_path(0x3D, 0, old))));
    let opened = dos.call(with_path(0x3D, 0, new));
    assert!(!carry(&opened));
    let read = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3F00;
        r.gpr[gpr::BX] = ax(&opened);
        r.gpr[gpr::CX] = 16;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    assert_eq!(dos.get(DATA + 0x100, ax(&read)), b"kept");
}

#[test]
fn memory_is_allocated_resized_and_freed() {
    let mut dos = Dos::boot(&[]);
    let refused = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4800;
        r.gpr[gpr::BX] = 0xFFFF;
    });
    assert!(carry(&refused));
    assert_eq!(ax(&refused), fs::error::NO_MEMORY);
    let shell = dos.psp;
    let shrunk = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4A00;
        r.gpr[gpr::BX] = 0x1000;
        r.set_sreg(Seg::Es, shell);
    });
    assert!(!carry(&shrunk), "{shrunk:X?}");
    let block = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4800;
        r.gpr[gpr::BX] = 0x100;
    });
    assert!(!carry(&block), "{block:X?}");
    let segment = ax(&block);
    assert!(segment > shell + 0x1000);
    let freed = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4900;
        r.set_sreg(Seg::Es, segment);
    });
    assert!(!carry(&freed));
    let inside = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4900;
        r.set_sreg(Seg::Es, segment + 1);
    });
    assert!(carry(&inside));
    assert_eq!(ax(&inside), fs::error::BAD_BLOCK);
}

#[test]
fn a_child_runs_and_its_return_code_comes_back() {
    let child = com(|asm| {
        asm.mov_imm16(Reg16::Ax, 0x4C2A);
        asm.int(0x21);
    });
    let mut dos = Dos::boot(&[File {
        name: b"CHILD   COM",
        bytes: &child,
    }]);
    let shell = dos.psp;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x4A00;
        r.gpr[gpr::BX] = 0x1000;
        r.set_sreg(Seg::Es, shell);
    });
    let path = dos.asciz(DATA, "CHILD.COM");
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
    assert!(!carry(&ran), "{ran:X?}\n{}", screen(&dos.m));
    let code = dos.call(|r| r.gpr[gpr::AX] = 0x4D00);
    assert_eq!(ax(&code), 0x002A);
}

#[test]
fn the_version_and_the_list_of_lists_are_where_programs_look() {
    let mut dos = Dos::boot(&[]);
    let version = dos.call(|r| r.gpr[gpr::AX] = 0x3000);
    assert_eq!(ax(&version) & 0xFF, 5);
    let lists = dos.call(|r| r.gpr[gpr::AX] = 0x5200);
    assert_eq!(lists.sreg(Seg::Es), data::SEGMENT);
    assert_eq!(lists.gpr[gpr::BX], data::SYSVARS);
    let nul = crate::cpu::linear(data::SEGMENT, data::NUL + device::header::NAME);
    let name: Vec<u8> = (nul..nul + 8).map(|at| dos.m.board().peek(at)).collect();
    assert_eq!(name, b"NUL     ");
}

/// The free clusters of the boot diskette.
fn free_clusters(dos: &mut Dos) -> u16 {
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3600;
        r.gpr[gpr::DX] = 0;
    })
    .gpr[gpr::BX]
}

#[test]
fn a_long_file_takes_its_clusters_and_gives_them_back() {
    const CHUNKS: u16 = 40;
    for config in [&b""[..], b"BUFFERS=1\r\n"] {
        let files = [File {
            name: b"CONFIG  SYS",
            bytes: config,
        }];
        let mut dos = Dos::boot(&files[..usize::from(!config.is_empty())]);
        let before = free_clusters(&mut dos);
        let name = dos.asciz(DATA, "A:\\LONG.BIN");
        let created = dos.call(|r| {
            with_path(0x3C, 0, name)(r);
            r.gpr[gpr::CX] = 0;
        });
        let handle = ax(&created);
        for chunk in 0..CHUNKS {
            dos.put(DATA + 0x400, &[u8::try_from(chunk).unwrap(); 0x400]);
            let wrote = dos.call(|r| {
                r.gpr[gpr::AX] = 0x4000;
                r.gpr[gpr::BX] = handle;
                r.gpr[gpr::CX] = 0x400;
                r.gpr[gpr::DX] = DATA + 0x400;
            });
            assert_eq!(ax(&wrote), 0x400, "{wrote:X?}");
        }
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x3E00;
            r.gpr[gpr::BX] = handle;
        });
        assert_eq!(
            before - free_clusters(&mut dos),
            CHUNKS,
            "one 1 KiB cluster a chunk"
        );
        let opened = dos.call(with_path(0x3D, 0, name));
        let handle = ax(&opened);
        for chunk in 0..CHUNKS {
            let read = dos.call(|r| {
                r.gpr[gpr::AX] = 0x3F00;
                r.gpr[gpr::BX] = handle;
                r.gpr[gpr::CX] = 0x400;
                r.gpr[gpr::DX] = DATA + 0x400;
            });
            assert_eq!(ax(&read), 0x400);
            assert_eq!(
                dos.get(DATA + 0x400, 0x400),
                [u8::try_from(chunk).unwrap(); 0x400]
            );
        }
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x3E00;
            r.gpr[gpr::BX] = handle;
        });
        let deleted = dos.call(with_path(0x41, 0, name));
        assert!(!carry(&deleted), "{deleted:X?}");
        assert_eq!(free_clusters(&mut dos), before);
    }
}

#[test]
fn a_call_whose_root_index_was_written_over_fails_and_dos_goes_on() {
    use crate::frames::Context;
    let mut dos = Dos::boot(&[]);
    {
        let mut g = crate::bios::guest::Guest::new(&mut dos.m);
        let mut k = engine::Kernel::new(&mut g);
        k.set_sda8(data::sda::ROOTS, 1);
        k.set_sda8(data::sda::DEPTH, 1);
        let root = engine::Frame::of(&int21::Int21 {
            function: 0x30,
            stage: 0,
            value: 0,
        });
        k.store(0, &root);
        let at = k.scratch(data::level::ROOT);
        k.set8(at, 5);
        engine::resume(&mut k);
        assert_eq!(
            (k.sda8(data::sda::ROOTS), k.sda8(data::sda::DEPTH)),
            (0, 0),
            "the call was dropped"
        );
    }
    let version = dos.call(|r| r.gpr[gpr::AX] = 0x3000);
    assert_eq!(ax(&version), 0x0005, "the next call is answered");
}

#[test]
fn a_dpb_whose_fat_cannot_hold_its_clusters_is_refused() {
    let mut dos = Dos::boot(&[]);
    let mut g = crate::bios::guest::Guest::new(&mut dos.m);
    let mut k = engine::Kernel::new(&mut g);
    assert!(k.dpb(0).is_some());
    k.set16(fs::dpb_at(0) + data::dpb::FAT_SECTORS, 1);
    assert_eq!(k.dpb(0), None);
}

#[test]
fn a_fat16_dpb_of_every_size_fat16_holds_is_taken() {
    let mut dos = Dos::boot(&[]);
    let mut g = crate::bios::guest::Guest::new(&mut dos.m);
    let mut k = engine::Kernel::new(&mut g);
    let at = fs::dpb_at(0);
    k.set16(at + data::dpb::FAT_SECTORS, 256);
    for max_cluster in [4087u16, 15_918, 65_525] {
        k.set16(at + data::dpb::MAX_CLUSTER, max_cluster);
        assert!(
            k.dpb(0).is_some(),
            "{max_cluster} clusters and one, a FAT16 volume's"
        );
    }
    k.set16(at + data::dpb::MAX_CLUSTER, 65_526);
    assert_eq!(k.dpb(0), None, "past what FAT16 counts");
}

#[test]
fn a_bad_drive_and_a_bad_handle_are_refused() {
    let mut dos = Dos::boot(&[]);
    let path = dos.asciz(DATA, "Q:\\NONE.TXT");
    let refused = dos.call(with_path(0x3D, 0, path));
    assert!(carry(&refused));
    assert_eq!(ax(&refused), fs::error::INVALID_DRIVE);
    let handle = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = 19;
    });
    assert!(carry(&handle));
    assert_eq!(ax(&handle), fs::error::INVALID_HANDLE);
    let error = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5900;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(ax(&error), fs::error::INVALID_HANDLE);
}

#[test]
fn the_free_space_of_the_boot_diskette_is_counted() {
    let mut dos = Dos::boot(&[File {
        name: b"DATA    BIN",
        bytes: &[0x55; 3000],
    }]);
    let free = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3600;
        r.gpr[gpr::DX] = 0;
    });
    let bpb = diskette();
    let clusters = u16::try_from(bpb.max_cluster() - 1).unwrap();
    assert_eq!(ax(&free), 2);
    assert_eq!(free.gpr[gpr::CX], 512);
    assert_eq!(free.gpr[gpr::DX], clusters);
    let used = 1 + 3;
    assert_eq!(free.gpr[gpr::BX], clusters - used);
}

/// Makes INT 21h typing `keys` — each pressed and let go — while the call
/// waits.
fn typed(dos: &mut Dos, keys: &[Scan], set: impl FnOnce(&mut Regs)) -> Regs {
    let events: Vec<(Scan, bool)> = keys
        .iter()
        .flat_map(|&key| [(key, true), (key, false)])
        .collect();
    dos.run(&[0xCD, 0x21], set, &events)
}

/// The keys that type `text`, none of which needs Shift.
fn keys_of(text: &str) -> Vec<Scan> {
    text.bytes()
        .map(|byte| match us_key(byte) {
            Some((key, false)) => key,
            _ => panic!("no key types {byte:?} unshifted"),
        })
        .collect()
}

/// `INT 25h`, then the flags it leaves on the stack dropped without loading
/// them, so the carry is the call's.
const ABSOLUTE_READ: [u8; 4] = [0xCD, 0x25, 0x44, 0x44];

const ENTER: Scan = Scan::plain(0x1C);
const BACKSPACE: Scan = Scan::plain(0x0E);
const F3: Scan = Scan::plain(0x3D);

/// Where the recording INT 24h handler lies in the shell's segment, the
/// bytes of codes it has kept, and the codes, a word each.
const HANDLER: u16 = DATA + 0x300;
const KEPT: u16 = DATA + 0x340;
const CODES: u16 = DATA + 0x342;

/// Installs a recording INT 24h handler: it keeps each error's code, then
/// fails the call, or when `asks` answers with the digit typed: `1` retry,
/// `3` fail.
fn recording_handler(dos: &mut Dos, asks: bool) {
    let [kept_low, kept_high] = KEPT.to_le_bytes();
    let [codes_low, codes_high] = CODES.to_le_bytes();
    let mut handler = vec![
        0x53, 0x2E, 0x8B, 0x1E, kept_low, kept_high, 0x2E, 0x89, 0xBF, codes_low, codes_high, 0x2E,
        0x83, 0x06, kept_low, kept_high, 0x02, 0x5B,
    ];
    let answer: &[u8] = if asks {
        &[0xB4, 0x00, 0xCD, 0x16, 0x2C, b'0']
    } else {
        &[0xB0, 0x03]
    };
    handler.extend_from_slice(answer);
    handler.push(0xCF);
    dos.put(HANDLER, &handler);
    dos.put(KEPT, &[0, 0]);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x2524;
        r.gpr[gpr::DX] = HANDLER;
    });
}

/// How many codes the recording handler has kept.
fn kept_codes(m: &Machine, psp: u16) -> u16 {
    let byte = |at| m.board().peek(crate::cpu::linear(psp, at));
    u16::from_le_bytes([byte(KEPT), byte(KEPT + 1)]) / 2
}

/// The codes the recording handler has kept, in order.
fn critical_codes(dos: &Dos) -> Vec<u8> {
    let kept = kept_codes(&dos.m, dos.psp);
    dos.get(CODES, kept * 2).into_iter().step_by(2).collect()
}

/// A character driver named `FAILDEV` whose every request but `INIT`
/// fails with a general failure.
fn failing_driver() -> Vec<u8> {
    let mut image = vec![0u8; 0x60];
    image[..18].copy_from_slice(&[
        0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x80, 0x12, 0x00, 0x1E, 0x00, b'F', b'A', b'I', b'L', b'D',
        b'E', b'V', b' ',
    ]);
    let code: &[u8] = &[
        0x2E, 0x89, 0x1E, 0x50, 0x00, // mov cs:[50h], bx
        0x2E, 0x8C, 0x06, 0x52, 0x00, // mov cs:[52h], es
        0xCB, // retf
        0x90, // nop
        0x1E, // push ds
        0x53, // push bx
        0x2E, 0xC5, 0x1E, 0x50, 0x00, // lds bx, cs:[50h]
        0x80, 0x7F, 0x02, 0x00, // cmp byte [bx+2], 0
        0x75, 0x0F, // jne fail
        0xC7, 0x47, 0x0E, 0x60, 0x00, // mov word [bx+0Eh], 60h
        0x8C, 0x4F, 0x10, // mov [bx+10h], cs
        0xC7, 0x47, 0x03, 0x00, 0x01, // mov word [bx+3], 0100h
        0xEB, 0x05, // jmp out
        0xC7, 0x47, 0x03, 0x0C, 0x81, // fail: mov word [bx+3], 810Ch
        0x5B, // out: pop bx
        0x1F, // pop ds
        0xCB, // retf
    ];
    image[0x12..0x12 + code.len()].copy_from_slice(code);
    image
}

#[test]
fn a_file_made_again_is_emptied_in_its_own_entry_with_one_buffer() {
    let config = b"BUFFERS=1\r\n";
    let mut dos = Dos::boot(&[File {
        name: b"CONFIG  SYS",
        bytes: config,
    }]);
    let name = dos.asciz(DATA, "TWICE.DAT");
    for round in 0..2 {
        let made = dos.call(|r| {
            with_path(0x3C, 0, name)(r);
            r.gpr[gpr::CX] = 0;
        });
        assert!(!carry(&made), "round {round}: {made:X?}");
        let handle = ax(&made);
        dos.put(DATA + 0x100, &[0xA5; 1500]);
        if round == 0 {
            dos.call(|r| {
                r.gpr[gpr::AX] = 0x4000;
                r.gpr[gpr::BX] = handle;
                r.gpr[gpr::CX] = 1500;
                r.gpr[gpr::DX] = DATA + 0x100;
            });
        }
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x3E00;
            r.gpr[gpr::BX] = handle;
        });
    }
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x200;
    });
    let every = dos.asciz(DATA + 0x40, "*.*");
    let first = dos.call(|r| {
        with_path(0x4E, 0, every)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&first));
    let mut names = Vec::new();
    loop {
        let name: Vec<u8> = dos
            .get(DATA + 0x200 + 0x1E, 13)
            .into_iter()
            .take_while(|&b| b != 0)
            .collect();
        let size = u32::from_le_bytes(dos.get(DATA + 0x200 + 0x1A, 4).try_into().unwrap());
        names.push((String::from_utf8(name).unwrap(), size));
        if carry(&dos.call(|r| r.gpr[gpr::AX] = 0x4F00)) {
            break;
        }
    }
    assert_eq!(
        names,
        [
            (
                "COMMAND.COM".into(),
                u32::try_from(com(idle).len()).unwrap()
            ),
            ("CONFIG.SYS".into(), 11),
            ("TWICE.DAT".into(), 0)
        ]
    );
    let free = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3600;
        r.gpr[gpr::DX] = 0;
    });
    let clusters = u16::try_from(diskette().max_cluster() - 1).unwrap();
    assert_eq!(free.gpr[gpr::BX], clusters - 2);
}

#[test]
fn line_input_is_edited_echoed_and_kept_as_the_template() {
    let mut dos = Dos::boot(&[]);
    dos.put(DATA, &[20]);
    let mut keys = keys_of("abc");
    keys.push(BACKSPACE);
    keys.extend(keys_of("d"));
    keys.push(ENTER);
    let line = typed(&mut dos, &keys, |r| {
        r.gpr[gpr::AX] = 0x0A00;
        r.gpr[gpr::DX] = DATA;
    });
    assert_eq!(
        dos.get(DATA + 1, 5),
        [3, b'a', b'b', b'd', b'\r'],
        "{line:X?}"
    );
    assert!(screen(&dos.m).contains("abd"), "{}", screen(&dos.m));
    typed(&mut dos, &[F3, ENTER], |r| {
        r.gpr[gpr::AX] = 0x0A00;
        r.gpr[gpr::DX] = DATA;
    });
    assert_eq!(dos.get(DATA + 1, 5), [3, b'a', b'b', b'd', b'\r']);
}

#[test]
fn line_input_drops_a_first_lf_breaks_the_screen_line_at_another_and_rubs_out_at_del() {
    const CTRL: Scan = Scan::plain(0x1D);
    const F: Scan = Scan::plain(0x21);
    let mut dos = Dos::boot(&[]);
    dos.int(0x10, |r| r.gpr[gpr::AX] = 0x0003);
    dos.put(DATA, &[20]);
    let controlled = |key: Scan| [(CTRL, true), (key, true), (key, false), (CTRL, false)];
    let plain = |text: &str| {
        keys_of(text)
            .into_iter()
            .flat_map(|key| [(key, true), (key, false)])
            .collect::<Vec<_>>()
    };
    let mut events = controlled(ENTER).to_vec();
    events.extend(plain("ab"));
    events.extend(controlled(ENTER));
    events.extend(controlled(F));
    events.extend(plain("c"));
    events.extend(controlled(BACKSPACE));
    events.extend(plain("d"));
    events.extend([(ENTER, true), (ENTER, false)]);
    dos.run(
        &[0xCD, 0x21],
        |r| {
            r.gpr[gpr::AX] = 0x0A00;
            r.gpr[gpr::DX] = DATA;
        },
        &events,
    );
    assert_eq!(
        dos.get(DATA + 1, 5),
        [3, b'a', b'b', b'd', b'\r'],
        "Ctrl+F passed over, Ctrl+Backspace a rub-out"
    );
    assert!(screen(&dos.m).starts_with("ab\nd\n"), "{}", screen(&dos.m));
}

#[test]
fn line_input_from_a_file_is_edited_and_echoed_as_if_typed_and_its_end_ends_a_line() {
    let mut dos = Dos::boot(&[File {
        name: b"LINES   TXT",
        bytes: b"\nab\ncd\x08e\rlast",
    }]);
    let path = dos.asciz(DATA + 0x40, "LINES.TXT");
    let opened = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3D00;
        r.gpr[gpr::DX] = path;
    });
    assert!(!carry(&opened), "{opened:X?}");
    let moved = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4600;
        r.gpr[gpr::BX] = ax(&opened);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&moved), "{moved:X?}");
    dos.int(0x10, |r| r.gpr[gpr::AX] = 0x0003);
    let read = |dos: &mut Dos| {
        dos.put(DATA, &[20]);
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x0A00;
            r.gpr[gpr::DX] = DATA;
        });
        dos.get(DATA + 1, 6)
    };
    assert_eq!(read(&mut dos), [4, b'a', b'b', b'c', b'e', b'\r']);
    assert!(
        screen(&dos.m).starts_with("ab\nce\n"),
        "echoed, the first LF dropped, the next a new screen line:\n{}",
        screen(&dos.m)
    );
    assert_eq!(
        read(&mut dos),
        [4, b'l', b'a', b's', b't', b'\r'],
        "the file's end ends its last line"
    );
    assert_eq!(read(&mut dos)[..2], [0, b'\r'], "and every line after it");
}

#[test]
fn a_key_read_with_echo_comes_back_as_typed_without_enter() {
    let mut dos = Dos::boot(&[]);
    let got = typed(&mut dos, &keys_of("q"), |r| r.gpr[gpr::AX] = 0x0100);
    assert_eq!(ax(&got).to_le_bytes()[0], b'q');
    assert!(screen(&dos.m).contains('q'), "{}", screen(&dos.m));
    let quiet = typed(&mut dos, &keys_of("z"), |r| r.gpr[gpr::AX] = 0x0800);
    assert_eq!(ax(&quiet).to_le_bytes()[0], b'z');
    assert!(!screen(&dos.m).contains('z'), "{}", screen(&dos.m));
}

#[test]
fn ctrl_c_at_a_childs_prompt_ends_it_through_the_default_int_23h() {
    let child = com(|asm| {
        asm.mov_imm8(Reg8::Ah, 0x01);
        asm.int(0x21);
        asm.mov_imm16(Reg16::Ax, 0x4C00);
        asm.int(0x21);
    });
    let mut dos = Dos::boot(&[File {
        name: b"WAIT    COM",
        bytes: &child,
    }]);
    make_room(&mut dos);
    let path = dos.asciz(DATA, "WAIT.COM");
    exec_block(&mut dos, DATA + 0x40);
    let ctrl = Scan::plain(0x1D);
    let c = Scan::plain(0x2E);
    let ran = dos.run(
        &[0xCD, 0x21],
        |r| {
            with_path(0x4B, 0, path)(r);
            r.gpr[gpr::BX] = DATA + 0x40;
        },
        &[(ctrl, true), (c, true), (c, false), (ctrl, false)],
    );
    assert!(!carry(&ran), "{ran:X?}");
    let code = dos.call(|r| r.gpr[gpr::AX] = 0x4D00);
    assert_eq!(ax(&code), 0x0100, "the child ended by Ctrl+C");
    assert!(screen(&dos.m).contains("^C"), "{}", screen(&dos.m));
}

#[test]
fn a_character_device_error_is_raised_as_int_24h() {
    let driver = failing_driver();
    let config = b"DEVICE=FAIL.SYS\r\n";
    let mut dos = Dos::boot(&[
        File {
            name: b"CONFIG  SYS",
            bytes: config,
        },
        File {
            name: b"FAIL    SYS",
            bytes: &driver,
        },
    ]);
    recording_handler(&mut dos, false);
    let name = dos.asciz(DATA, "FAILDEV");
    let opened = dos.call(with_path(0x3D, 1, name));
    assert!(!carry(&opened), "the driver is in the chain: {opened:X?}");
    dos.put(DATA + 0x80, b"x");
    let wrote = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = ax(&opened);
        r.gpr[gpr::CX] = 1;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert!(carry(&wrote), "{wrote:X?}");
    assert_eq!(ax(&wrote), 0x13 + u16::from(device::error::GENERAL));
    assert_eq!(critical_codes(&dos), [device::error::GENERAL]);
}

#[test]
fn an_empty_drive_is_not_ready_each_time_it_is_asked() {
    // The first try times out with the controller still in the command, so
    // until the driver resets after it every later call met a busy
    // controller: a general failure.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    let name = dos.asciz(DATA, "B:\\ANY.TXT");
    for _ in 0..3 {
        let refused = dos.call(with_path(0x3D, 0, name));
        assert!(carry(&refused), "{refused:X?}");
    }
    assert_eq!(critical_codes(&dos), [device::error::NOT_READY; 3]);
}

#[test]
fn config_sys_sets_the_tables_and_reports_what_it_cannot_take() {
    let config = b"FILES=20\r\nBUFFERS=10,2\r\nLASTDRIVE=M\r\nBREAK=ON\r\nFCBS=8,2\r\nNONSENSE=1\r\nFILES=3\r\n";
    let dos = Dos::boot(&[File {
        name: b"CONFIG  SYS",
        bytes: config,
    }]);
    let text = screen(&dos.m);
    assert!(
        text.contains("Unrecognized command in CONFIG.SYS"),
        "{text}"
    );
    assert!(text.contains("Error in CONFIG.SYS line 6"), "{text}");
    assert!(text.contains("Error in CONFIG.SYS line 7"), "{text}");
    let var = |field: i16| u16::try_from(i32::from(data::SYSVARS) + i32::from(field)).unwrap();
    let byte = |offset: u16| {
        dos.m
            .board()
            .peek(u32::from(data::SEGMENT) * 16 + u32::from(offset))
    };
    assert_eq!(byte(var(data::sysvars::LASTDRIVE)), 13);
    assert_eq!(kernel16(&dos.m, var(data::sysvars::BUFFERS)), 10);
    assert_eq!(kernel16(&dos.m, var(data::sysvars::LOOKAHEAD)), 2);
    assert_eq!(kernel16(&dos.m, var(data::sysvars::PROTECTED_FCBS)), 2);
    assert_eq!(byte(data::SDA + data::sda::BREAK), 1);
    let second = kernel16(&dos.m, data::SFT + 2);
    let count = dos.m.board().peek(u32::from(second) * 16 + 4);
    assert_eq!(count, 15, "FILES=20 adds fifteen entries to the first five");
}

#[test]
fn a_cp_m_call_through_the_psp_reaches_int_21h() {
    let mut dos = Dos::boot(&[]);
    let call = 5u16.wrapping_sub(STUB + 2 + 3);
    let [low, high] = call.to_le_bytes();
    let answer = dos.run(
        &[0xB1, 0x19, 0xE8, low, high],
        |r| r.gpr[gpr::AX] = 0xFFFF,
        &[],
    );
    assert_eq!(ax(&answer) & 0xFF, 0, "the current drive, A:");
}

#[test]
fn an_fcb_file_is_made_written_read_sized_renamed_and_deleted() {
    let mut dos = Dos::boot(&[]);
    let fcb = DATA;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    fcb_at(&mut dos, fcb, b"RECORDS DAT");
    assert_eq!(fcb_call(&mut dos, 0x16, fcb), 0, "made");
    for record in 0..3u8 {
        dos.put(DATA + 0x100, &[b'A' + record; 128]);
        assert_eq!(fcb_call(&mut dos, 0x15, fcb), 0, "record {record} written");
    }
    assert_eq!(fcb_call(&mut dos, 0x10, fcb), 0, "closed");
    fcb_at(&mut dos, fcb, b"RECORDS DAT");
    assert_eq!(fcb_call(&mut dos, 0x0F, fcb), 0, "opened");
    assert_eq!(dos.get(fcb + 0x10, 4), 384u32.to_le_bytes(), "its size");
    assert_eq!(
        dos.get(fcb + 0x0E, 2),
        128u16.to_le_bytes(),
        "the default record"
    );
    dos.put(fcb + 0x20, &[0]);
    assert_eq!(fcb_call(&mut dos, 0x14, fcb), 0);
    assert_eq!(dos.get(DATA + 0x100, 2), b"AA");
    dos.put(fcb + 0x21, &2u32.to_le_bytes());
    assert_eq!(fcb_call(&mut dos, 0x21, fcb), 0, "random read");
    assert_eq!(dos.get(DATA + 0x100, 2), b"CC");
    dos.put(fcb + 0x21, &3u32.to_le_bytes());
    assert_eq!(fcb_call(&mut dos, 0x21, fcb), 1, "past the end");
    assert_eq!(fcb_call(&mut dos, 0x10, fcb), 0);
    fcb_at(&mut dos, fcb, b"RECORDS DAT");
    dos.put(fcb + 0x0E, &64u16.to_le_bytes());
    assert_eq!(fcb_call(&mut dos, 0x23, fcb), 0);
    assert_eq!(
        dos.get(fcb + 0x21, 3),
        [6, 0, 0],
        "384 bytes are six 64-byte records"
    );
    let mut rename = [0u8; 37];
    rename[1..12].copy_from_slice(b"RECORDS DAT");
    rename[0x11..0x1C].copy_from_slice(b"????????BAK");
    dos.put(fcb, &rename);
    assert_eq!(fcb_call(&mut dos, 0x17, fcb), 0, "renamed");
    fcb_at(&mut dos, fcb, b"????????BAK");
    assert_eq!(fcb_call(&mut dos, 0x11, fcb), 0, "found");
    assert_eq!(dos.get(DATA + 0x101, 11), b"RECORDS BAK");
    assert_eq!(fcb_call(&mut dos, 0x13, fcb), 0, "deleted");
    assert_eq!(fcb_call(&mut dos, 0x11, fcb), 0xFF, "gone");
}

#[test]
fn an_fcb_named_in_lower_case_or_by_a_pattern_keeps_its_own_file_open() {
    let mut dos = Dos::boot(&[File {
        name: b"FOUND   DAT",
        bytes: &[b'F'; 256],
    }]);
    let fcb = DATA;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    fcb_at(&mut dos, fcb, b"records dat");
    assert_eq!(fcb_call(&mut dos, 0x16, fcb), 0, "made");
    for record in 0..3u8 {
        dos.put(DATA + 0x100, &[b'A' + record; 128]);
        assert_eq!(fcb_call(&mut dos, 0x15, fcb), 0, "record {record} written");
    }
    assert_eq!(fcb_call(&mut dos, 0x10, fcb), 0, "closed");
    let name = dos.asciz(DATA + 0x200, "RECORDS.DAT");
    let opened = dos.call(with_path(0x3D, 0, name));
    assert!(!carry(&opened), "{opened:X?}");
    let read = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3F00;
        r.gpr[gpr::BX] = ax(&opened);
        r.gpr[gpr::CX] = 0x200;
        r.gpr[gpr::DX] = DATA + 0x300;
    });
    assert_eq!(ax(&read), 384, "every record in the one file");
    let held = dos.get(DATA + 0x300, 384);
    assert!(held
        .chunks(128)
        .zip(b"ABC")
        .all(|(record, &b)| record.iter().all(|&x| x == b)));
    fcb_at(&mut dos, fcb, b"FOUND   D?T");
    assert_eq!(fcb_call(&mut dos, 0x0F, fcb), 0, "opened by a pattern");
    for record in 0..2 {
        dos.put(DATA + 0x100, &[0; 128]);
        assert_eq!(fcb_call(&mut dos, 0x14, fcb), 0, "record {record} read");
        assert_eq!(dos.get(DATA + 0x100, 128), [b'F'; 128]);
    }
    assert_eq!(fcb_call(&mut dos, 0x14, fcb), 1, "at its end");
}

#[test]
fn an_fcb_search_keeps_its_place_where_dos_keeps_it_and_stays_on_its_drive() {
    let mut dos = Dos::boot(&[
        File {
            name: b"ALPHA   TXT",
            bytes: b"a",
        },
        File {
            name: b"BETA    TXT",
            bytes: b"bb",
        },
    ]);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    let attributes = attr::HIDDEN | attr::SYSTEM | attr::DIRECTORY;
    let mut search = [0u8; 44];
    search[0] = 0xFF;
    search[6] = attributes;
    search[8..19].copy_from_slice(b"????????TXT");
    dos.put(DATA, &search);
    let word = |dos: &Dos, at: u16| u16::from_le_bytes([dos.get(at, 1)[0], dos.get(at + 1, 1)[0]]);
    assert_eq!(fcb_call(&mut dos, 0x11, DATA), 0);
    assert_eq!(dos.get(DATA + 0x100 + 8, 11), b"ALPHA   TXT");
    // DOS 3 and later keep the entry's index 14h into an extended FCB, where
    // their CHKDSK reads it to find the entry it fixes.
    assert_eq!(
        word(&dos, DATA + 0x14),
        1,
        "COMMAND.COM is the root's first entry"
    );
    assert_eq!(dos.get(DATA + 7 + 0x0C, 1), [attributes]);
    assert_eq!(word(&dos, DATA + 7 + 0x0F), 0, "the root's cluster");
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x0E00;
        r.gpr[gpr::DX] = 1;
    });
    assert_eq!(
        fcb_call(&mut dos, 0x12, DATA),
        0,
        "the search goes on on A:, not the current B:"
    );
    assert_eq!(dos.get(DATA + 0x100 + 8, 11), b"BETA    TXT");
    assert_eq!(word(&dos, DATA + 0x14), 2);
    assert_eq!(fcb_call(&mut dos, 0x12, DATA), 0xFF);
}

#[test]
fn a_file_name_is_parsed_into_an_fcb() {
    let mut dos = Dos::boot(&[]);
    dos.put(DATA, b"  b:report*.d  \r");
    let parsed = dos.call(|r| {
        r.gpr[gpr::AX] = 0x2901;
        r.gpr[gpr::SI] = DATA;
        r.gpr[gpr::DI] = DATA + 0x40;
    });
    assert_eq!(ax(&parsed) & 0xFF, 1, "a wildcard");
    assert_eq!(dos.get(DATA + 0x40, 12), b"\x02REPORT??D  ");
    assert_eq!(parsed.gpr[gpr::SI], DATA + 13);
}

#[test]
fn ioctl_reports_a_devices_and_a_files_information() {
    let mut dos = Dos::boot(&[File {
        name: b"PLAIN   TXT",
        bytes: b"x",
    }]);
    let console = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4400;
        r.gpr[gpr::BX] = 1;
    });
    assert!(!carry(&console));
    assert_eq!(
        console.gpr[gpr::DX],
        0x00D3,
        "a device, the console, not at its end: {console:X?}"
    );
    let name = dos.asciz(DATA, "PLAIN.TXT");
    let opened = dos.call(with_path(0x3D, 0, name));
    let file = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4400;
        r.gpr[gpr::BX] = ax(&opened);
    });
    assert_eq!(
        file.gpr[gpr::DX],
        0x0040,
        "a file on drive A:, not written: {file:X?}"
    );
    let removable = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4408;
        r.gpr[gpr::BX] = 1;
    });
    assert!(!carry(&removable), "{removable:X?}");
    assert_eq!(ax(&removable), 0, "a diskette is removable");
    let raw = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4401;
        r.gpr[gpr::BX] = 1;
        r.gpr[gpr::DX] = 0x00A0;
    });
    assert!(!carry(&raw));
}

#[test]
fn a_drives_attributes_say_when_subst_or_the_network_made_it() {
    let mut dos = Dos::boot(&[]);
    let attributes = |dos: &mut Dos| {
        let regs = dos.call(|r| {
            r.gpr[gpr::AX] = 0x4409;
            r.gpr[gpr::BX] = 1;
        });
        (carry(&regs), regs.gpr[gpr::DX])
    };
    let (failed, local) = attributes(&mut dos);
    assert!(
        !failed && local & (ioctl::drive_bits::REMOTE | ioctl::drive_bits::SUBST) == 0,
        "{local:04X}"
    );
    let flags = fs::cds_at(0) + data::cds::FLAGS;
    let physical = peek16(&dos.m, data::SEGMENT, flags);
    poke16(
        &mut dos.m,
        data::SEGMENT,
        flags,
        physical | data::cds::SUBST,
    );
    assert_eq!(
        attributes(&mut dos),
        (false, local | ioctl::drive_bits::SUBST)
    );
    poke16(
        &mut dos.m,
        data::SEGMENT,
        flags,
        data::cds::NETWORK | data::cds::PHYSICAL,
    );
    assert_eq!(
        attributes(&mut dos),
        (false, ioctl::drive_bits::REMOTE),
        "a network drive has no driver of its own"
    );
    poke16(&mut dos.m, data::SEGMENT, flags, physical);
}

#[test]
fn the_date_and_time_are_set_and_read_back() {
    let mut dos = Dos::boot(&[]);
    let set = dos.call(|r| {
        r.gpr[gpr::AX] = 0x2B00;
        r.gpr[gpr::CX] = 2025;
        r.gpr[gpr::DX] = 0x060F;
    });
    assert_eq!(ax(&set) & 0xFF, 0);
    let date = dos.call(|r| r.gpr[gpr::AX] = 0x2A00);
    assert_eq!(date.gpr[gpr::CX], 2025);
    assert_eq!(date.gpr[gpr::DX], 0x060F);
    assert_eq!(ax(&date) & 0xFF, 0, "15 June 2025 was a Sunday");
    let bad = dos.call(|r| {
        r.gpr[gpr::AX] = 0x2B00;
        r.gpr[gpr::CX] = 2025;
        r.gpr[gpr::DX] = 0x021E;
    });
    assert_eq!(ax(&bad) & 0xFF, 0xFF, "30 February");
    let time = dos.call(|r| {
        r.gpr[gpr::AX] = 0x2D00;
        r.gpr[gpr::CX] = 0x0D2D;
        r.gpr[gpr::DX] = 0x1E00;
    });
    assert_eq!(ax(&time) & 0xFF, 0);
    let now = dos.call(|r| r.gpr[gpr::AX] = 0x2C00);
    assert_eq!(now.gpr[gpr::CX], 0x0D2D, "13:45");
}

#[test]
fn a_machine_resumed_days_later_has_moved_on_to_the_day_and_time() {
    let mut dos = Dos::boot(&[]);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x2B00;
        r.gpr[gpr::CX] = 2025;
        r.gpr[gpr::DX] = 0x0C1F;
    });
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x2D00;
        r.gpr[gpr::CX] = 0x1700;
        r.gpr[gpr::DX] = 0x0000;
    });
    // Suspended for two days, an hour and a half.
    let away = (2 * 86_400 + 5_400) * crate::time::NANOS_PER_SECOND;
    crate::bios::resumed(&mut dos.m, away);
    let date = dos.call(|r| r.gpr[gpr::AX] = 0x2A00);
    assert_eq!(
        (date.gpr[gpr::CX], date.gpr[gpr::DX]),
        (2026, 0x0103),
        "31 December 2025 at 23:00, then three midnights"
    );
    let time = dos.call(|r| r.gpr[gpr::AX] = 0x2C00);
    assert_eq!(time.gpr[gpr::CX], 0x001E, "00:30");
}

#[test]
fn country_information_comes_from_the_chosen_country() {
    let mut dos = Dos::boot(&[File {
        name: b"CONFIG  SYS",
        bytes: b"COUNTRY=044\r\n",
    }]);
    let info = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3800;
        r.gpr[gpr::DX] = DATA;
    });
    assert_eq!(info.gpr[gpr::BX], 44);
    assert_eq!(dos.get(DATA, 2), [1, 0], "day, month, year");
    assert_eq!(dos.get(DATA + 2, 2), [0x9C, 0], "the pound");
    let upper = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6520;
        r.gpr[gpr::DX] = u16::from(0x84u8);
    });
    assert_eq!(
        upper.gpr[gpr::DX] & 0xFF,
        0x8E,
        "a diaeresis keeps its capital"
    );
    let yes = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6523;
        r.gpr[gpr::DX] = u16::from(b'y');
    });
    assert_eq!(ax(&yes), 1);
    let table = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6502;
        r.gpr[gpr::BX] = 0xFFFF;
        r.gpr[gpr::DX] = 0xFFFF;
        r.gpr[gpr::CX] = 5;
        r.gpr[gpr::DI] = DATA + 0x40;
    });
    assert!(!carry(&table));
    assert_eq!(
        dos.get(DATA + 0x40, 5),
        [2, 0x80, 0x04, 0x00, 0xF0],
        "F000:0480"
    );
}

#[test]
fn int_25h_reads_the_boot_record() {
    let mut dos = Dos::boot(&[]);
    let read = dos.run(
        &ABSOLUTE_READ,
        |r| {
            r.gpr[gpr::AX] = 0;
            r.gpr[gpr::CX] = 1;
            r.gpr[gpr::DX] = 0;
            r.gpr[gpr::BX] = DATA;
        },
        &[],
    );
    assert!(!carry(&read), "{read:X?}");
    assert_eq!(dos.get(DATA + 3, 8), boot::OEM);
}

#[test]
fn handles_are_duplicated_and_their_table_grown() {
    let mut dos = Dos::boot(&[]);
    let copy = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4500;
        r.gpr[gpr::BX] = 1;
    });
    assert!(!carry(&copy));
    assert_eq!(ax(&copy), 5, "the first free handle");
    let forced = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4600;
        r.gpr[gpr::BX] = 1;
        r.gpr[gpr::CX] = 7;
    });
    assert!(!carry(&forced));
    make_room(&mut dos);
    let grown = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6700;
        r.gpr[gpr::BX] = 40;
    });
    assert!(!carry(&grown), "{grown:X?}");
    assert_eq!(dos.get(0x32, 2), 40u16.to_le_bytes());
    assert_eq!(dos.get(0x34, 2), [0, 0], "a block of its own");
    let size = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6700;
        r.gpr[gpr::BX] = 20;
    });
    assert!(!carry(&size));
    assert_eq!(dos.get(0x34, 2), 0x18u16.to_le_bytes(), "back in the PSP");
    let wrote = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = 7;
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&wrote), "the forced copy survived the moves");
}

#[test]
fn a_temporary_and_an_extended_open_make_their_files() {
    let mut dos = Dos::boot(&[]);
    dos.put(DATA, b"A:\\\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
    let temp = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5A00;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = DATA;
    });
    assert!(!carry(&temp), "{temp:X?}");
    let name = dos.get(DATA + 3, 9);
    assert!(
        name[..8].iter().all(u8::is_ascii_hexdigit) && name[8] == 0,
        "{name:?}"
    );
    let path = dos.asciz(DATA + 0x40, "NEW.DAT");
    let extended = |dos: &mut Dos, action: u8| {
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x6C00;
            r.gpr[gpr::BX] = 0x0002;
            r.gpr[gpr::CX] = 0;
            r.gpr[gpr::DX] = u16::from(action);
            r.gpr[gpr::SI] = path;
        })
    };
    let made = extended(&mut dos, 0x12);
    assert_eq!((carry(&made), made.gpr[gpr::CX]), (false, 2), "created");
    let replaced = extended(&mut dos, 0x12);
    assert_eq!(
        (carry(&replaced), replaced.gpr[gpr::CX]),
        (false, 3),
        "replaced"
    );
    let opened = extended(&mut dos, 0x01);
    assert_eq!((carry(&opened), opened.gpr[gpr::CX]), (false, 1), "opened");
    let refused = extended(&mut dos, 0x10);
    assert!(carry(&refused));
    assert_eq!(ax(&refused), fs::error::FILE_EXISTS);
}

#[test]
fn a_files_stamp_is_set_and_kept_past_its_close() {
    let mut dos = Dos::boot(&[File {
        name: b"STAMP   TXT",
        bytes: b"z",
    }]);
    let name = dos.asciz(DATA, "STAMP.TXT");
    let opened = dos.call(with_path(0x3D, 2, name));
    let handle = ax(&opened);
    let set = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5701;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 0x6000;
        r.gpr[gpr::DX] = 0x5A21;
    });
    assert!(!carry(&set));
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = handle;
    });
    let opened = dos.call(with_path(0x3D, 0, name));
    let read = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5700;
        r.gpr[gpr::BX] = ax(&opened);
    });
    assert_eq!((read.gpr[gpr::CX], read.gpr[gpr::DX]), (0x6000, 0x5A21));
}

#[test]
fn a_path_is_made_canonical() {
    let mut dos = Dos::boot(&[]);
    let from = dos.asciz(DATA, "a:sub\\..\\notes.txt");
    let named = dos.call(|r| {
        r.gpr[gpr::AX] = 0x6000;
        r.gpr[gpr::SI] = from;
        r.gpr[gpr::DI] = DATA + 0x80;
    });
    assert!(!carry(&named));
    assert_eq!(dos.get(DATA + 0x80, 13), b"A:\\NOTES.TXT\0");
    let device = dos.asciz(DATA, "con");
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x6000;
        r.gpr[gpr::SI] = device;
        r.gpr[gpr::DI] = DATA + 0x80;
    });
    assert_eq!(dos.get(DATA + 0x80, 7), b"A:/CON\0");
}

#[test]
fn the_multiplex_interrupt_answers_the_kernels_own_check() {
    let mut dos = Dos::boot(&[]);
    let installed = dos.int(0x2F, |r| r.gpr[gpr::AX] = 0x1200);
    assert_eq!(ax(&installed) & 0xFF, 0xFF);
    let share = dos.int(0x2F, |r| r.gpr[gpr::AX] = 0x1000);
    assert_eq!(ax(&share) & 0xFF, 0, "SHARE is not installed");
}

#[test]
fn a_volume_holds_its_subdirectories_and_label_for_dos_to_read() {
    let files = [
        volume::File {
            path: "COMMAND.COM",
            bytes: &com(idle),
            attributes: 0,
        },
        volume::File {
            path: "DOS\\TOOLS\\NOTE.TXT",
            bytes: b"deep",
            attributes: 0,
        },
        volume::File {
            path: "DOS\\HIDDEN.SYS",
            bytes: b"h",
            attributes: attr::HIDDEN,
        },
    ];
    let image = volume::volume(
        &diskette(),
        Some(*b"TAIRIX DOS "),
        &files,
        tairix_fat::stamp::Stamp::NONE,
    )
    .unwrap();
    let config = Template::IbmPc.config();
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut().insert_floppy(0, floppy(image)).unwrap();
    let mut deadline = SECOND;
    let psp = loop {
        run_until(&mut m, deadline);
        let psp = kernel16(&m, data::SDA + data::sda::PSP);
        if m.cpu().regs().sreg(Seg::Cs) == psp && (0x101..0x104).contains(&m.cpu().regs().ip) {
            break psp;
        }
        assert!(deadline < 30 * SECOND, "the shell never ran");
        deadline += SECOND / 2;
    };
    let mut dos = Dos { m, psp };
    let path = dos.asciz(DATA, "A:\\DOS\\TOOLS\\NOTE.TXT");
    let opened = dos.call(with_path(0x3D, 0, path));
    assert!(!carry(&opened), "{opened:X?}");
    let read = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3F00;
        r.gpr[gpr::BX] = ax(&opened);
        r.gpr[gpr::CX] = 16;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert_eq!(dos.get(DATA + 0x80, ax(&read)), b"deep");
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x100;
    });
    let label = dos.asciz(DATA + 0x40, "A:\\*.*");
    let found = dos.call(|r| {
        with_path(0x4E, 0, label)(r);
        r.gpr[gpr::CX] = u16::from(attr::VOLUME_ID);
    });
    assert!(!carry(&found));
    assert_eq!(dos.get(DATA + 0x100 + 0x1E, 12), b"TAIRIX D.OS\0");
    let hidden = dos.asciz(DATA + 0x40, "A:\\DOS\\*.*");
    let plain = dos.call(|r| {
        with_path(0x4E, 0, hidden)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(carry(&plain), "a hidden file is found only when asked for");
    let asked = dos.call(|r| {
        with_path(0x4E, 0, hidden)(r);
        r.gpr[gpr::CX] = u16::from(attr::HIDDEN);
    });
    assert!(!carry(&asked));
    assert_eq!(dos.get(DATA + 0x100 + 0x1E, 11), b"HIDDEN.SYS\0");
}

#[test]
fn a_volume_refuses_what_it_cannot_hold() {
    let bpb = diskette();
    let stamp = tairix_fat::stamp::Stamp::NONE;
    let bad = [volume::File {
        path: "TOO-LONG-NAME.TXT",
        bytes: b"",
        attributes: 0,
    }];
    assert_eq!(
        volume::volume(&bpb, None, &bad, stamp),
        Err(volume::VolumeError::Name)
    );
    let twice = [
        volume::File {
            path: "SAME.TXT",
            bytes: b"a",
            attributes: 0,
        },
        volume::File {
            path: "SAME.TXT",
            bytes: b"b",
            attributes: 0,
        },
    ];
    assert_eq!(
        volume::volume(&bpb, None, &twice, stamp),
        Err(volume::VolumeError::Name)
    );
    let big = vec![0u8; 400 * 1024];
    let full = [volume::File {
        path: "BIG.BIN",
        bytes: &big,
        attributes: 0,
    }];
    assert_eq!(
        volume::volume(&bpb, None, &full, stamp),
        Err(volume::VolumeError::Full)
    );
    let names: Vec<String> = (0..113).map(|n| alloc::format!("F{n}.TXT")).collect();
    let many: Vec<volume::File<'_>> = names
        .iter()
        .map(|path| volume::File {
            path,
            bytes: b"",
            attributes: 0,
        })
        .collect();
    assert_eq!(
        volume::volume(&bpb, None, &many, stamp),
        Err(volume::VolumeError::Root)
    );
}

#[test]
fn config_sys_lines_are_read_as_dos_reads_them() {
    assert_eq!(directive(b"  files = 30  "), Directive::Files(30));
    assert_eq!(directive(b"BUFFERS=20,4"), Directive::Buffers(20, 4));
    assert_eq!(directive(b"BUFFERS=20"), Directive::Buffers(20, 1));
    assert_eq!(directive(b"BUFFERS=0"), Directive::Invalid);
    assert_eq!(directive(b"BUFFERS=20,9"), Directive::Invalid);
    assert_eq!(directive(b"FILES=7"), Directive::Invalid);
    assert_eq!(directive(b"FILES=256"), Directive::Invalid);
    assert_eq!(directive(b"FCBS=4,5"), Directive::Invalid);
    assert_eq!(directive(b"FCBS=8,2"), Directive::Fcbs(8, 2));
    assert_eq!(directive(b"lastdrive=z"), Directive::LastDrive(26));
    assert_eq!(directive(b"LASTDRIVE=ZZ"), Directive::Invalid);
    assert_eq!(directive(b"STACKS=9,256"), Directive::Stacks(9, 256));
    assert_eq!(directive(b"STACKS=0,0"), Directive::Stacks(0, 0));
    assert_eq!(directive(b"STACKS=4,256"), Directive::Invalid);
    assert_eq!(directive(b"STACKS=64,512"), Directive::Stacks(64, 512));
    assert_eq!(directive(b"STACKS=65,128"), Directive::Invalid);
    assert_eq!(directive(b"STACKS=8,31"), Directive::Invalid);
    assert_eq!(directive(b"STACKS=8,513"), Directive::Invalid);
    assert_eq!(directive(b"STACKS=8"), Directive::Invalid);
    assert_eq!(
        directive(b"COUNTRY=049,437,C:\\DOS\\COUNTRY.SYS"),
        Directive::Country(49)
    );
    assert_eq!(directive(b"COUNTRY=049,850"), Directive::Invalid);
    assert_eq!(directive(b"BREAK=on"), Directive::Break(true));
    assert_eq!(directive(b"BREAK=maybe"), Directive::Invalid);
    assert_eq!(
        directive(b"DEVICE=C:\\DOS\\ANSI.SYS /X"),
        Directive::Device(b"C:\\DOS\\ANSI.SYS /X", 7)
    );
    assert_eq!(
        directive(b"DEVICEHIGH SIZE=1A0 ANSI.SYS"),
        Directive::Device(b"ANSI.SYS", 20)
    );
    assert_eq!(
        directive(b"SHELL=C:\\COMMAND.COM C:\\ /E:512 /P"),
        Directive::Shell(b"C:\\COMMAND.COM C:\\ /E:512 /P")
    );
    assert_eq!(
        directive(b"INSTALL=C:\\DOS\\SHARE.EXE"),
        Directive::Install(b"C:\\DOS\\SHARE.EXE")
    );
    assert_eq!(directive(b"DOS=HIGH,UMB"), Directive::Dos(true));
    assert_eq!(directive(b"DOS=LOW"), Directive::Dos(false));
    assert_eq!(directive(b"REM nothing here"), Directive::Remark);
    assert_eq!(directive(b"; nor here"), Directive::Remark);
    assert_eq!(directive(b""), Directive::Remark);
    assert_eq!(directive(b"SWITCHES=/F"), Directive::Unknown);
}

/// A screen cell: its character and attribute.
fn cell(dos: &Dos, row: u32, column: u32) -> (u8, u8) {
    let at = 0xB_8000 + (row * 80 + column) * 2;
    (dos.m.board().peek(at), dos.m.board().peek(at + 1))
}

/// Writes `text` to the standard output.
fn write_out(dos: &mut Dos, text: &[u8]) {
    dos.put(DATA + 0x200, text);
    let wrote = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = 1;
        r.gpr[gpr::CX] = u16::try_from(text.len()).unwrap();
        r.gpr[gpr::DX] = DATA + 0x200;
    });
    assert!(!carry(&wrote), "{wrote:X?}");
}

fn ansi() -> Dos {
    let driver = ansi_sys();
    Dos::boot(&[
        File {
            name: b"CONFIG  SYS",
            bytes: b"DEVICE=ANSI.SYS\r\n",
        },
        File {
            name: b"ANSI    SYS",
            bytes: &driver,
        },
    ])
}

#[test]
fn ansi_sys_places_colours_and_erases_text() {
    let mut dos = ansi();
    write_out(&mut dos, b"\x1b[2J\x1b[5;10H\x1b[1;33;44mHello\x1b[0m!");
    let word: Vec<(u8, u8)> = (9..15).map(|column| cell(&dos, 4, column)).collect();
    assert_eq!(
        word,
        [
            (b'H', 0x1E),
            (b'e', 0x1E),
            (b'l', 0x1E),
            (b'l', 0x1E),
            (b'o', 0x1E),
            (b'!', 0x07)
        ],
        "{}",
        screen(&dos.m)
    );
    assert_eq!(cell(&dos, 0, 0), (b' ', 0x07), "the screen was cleared");
    write_out(&mut dos, b"\x1b[s\x1b[1;1HTop\x1b[u+\x1b[5;1H\x1b[K");
    assert_eq!(cell(&dos, 0, 1), (b'o', 0x07));
    assert_eq!(
        (0..20)
            .map(|column| cell(&dos, 4, column).0)
            .collect::<Vec<_>>(),
        [b' '; 20],
        "the line was erased"
    );
}

#[test]
fn ansi_sys_reports_the_cursor_and_types_a_reassigned_key() {
    let mut dos = ansi();
    write_out(&mut dos, b"\x1b[2J\x1b[3;7H\x1b[6n");
    let reply: Vec<u8> = (0..9)
        .map(|_| ax(&dos.call(|r| r.gpr[gpr::AX] = 0x0700)).to_le_bytes()[0])
        .collect();
    assert_eq!(reply, b"\x1b[03;07R\r");
    write_out(&mut dos, b"\x1b[97;\"hey\"p");
    let a = keys_of("a");
    let mut typed = Vec::new();
    let first = typed_char(&mut dos, &a);
    typed.push(first);
    for _ in 0..2 {
        let got = dos.call(|r| r.gpr[gpr::AX] = 0x0700);
        typed.push(ax(&got).to_le_bytes()[0]);
    }
    assert_eq!(typed, b"hey");
}

#[test]
fn ansi_sys_answers_its_check_and_gives_and_takes_the_display() {
    let mut plain = Dos::boot(&[]);
    let asked = plain.int(0x2F, |r| r.gpr[gpr::AX] = 0x1A00);
    assert_ne!(ax(&asked) & 0xFF, 0xFF, "no ANSI.SYS, no answer");
    let mut dos = ansi();
    let asked = dos.int(0x2F, |r| r.gpr[gpr::AX] = 0x1A00);
    assert_eq!(ax(&asked) & 0xFF, 0xFF, "{asked:X?}");
    let name = dos.asciz(DATA, "CON");
    let opened = dos.call(with_path(0x3D, 0x02, name));
    assert!(!carry(&opened), "{opened:X?}");
    let (handle, packet) = (ax(&opened), DATA + 0x20);
    let ioctl = |dos: &mut Dos, function: u8| {
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x440C;
            r.gpr[gpr::BX] = handle;
            r.gpr[gpr::CX] = u16::from_le_bytes([function, 0x03]);
            r.gpr[gpr::DX] = packet;
        })
    };
    dos.put(packet, &[0, 0, 14, 0]);
    let got = ioctl(&mut dos, 0x7F);
    assert!(!carry(&got), "{got:X?}");
    let info = dos.get(packet, 18);
    let word = |at: usize| u16::from_le_bytes([info[at], info[at + 1]]);
    assert_eq!(info[6], 1, "a text mode");
    assert_eq!(
        [word(4), word(8), word(10), word(12), word(14), word(16)],
        [0, 16, 0xFFFF, 0xFFFF, 80, 25],
        "the CGA's mode 3, blinking"
    );
    dos.put(packet + 16, &43u16.to_le_bytes());
    let refused = ioctl(&mut dos, 0x5F);
    assert_eq!(
        (carry(&refused), ax(&refused)),
        (true, 0x1D),
        "a CGA has no 43 lines: {refused:X?}"
    );
    dos.put(packet + 16, &25u16.to_le_bytes());
    dos.put(packet + 4, &1u16.to_le_bytes());
    let refused = ioctl(&mut dos, 0x5F);
    assert_eq!(
        (carry(&refused), ax(&refused)),
        (true, 0x14),
        "nor a settable blink: {refused:X?}"
    );
    dos.put(packet, &[1]);
    let refused = ioctl(&mut dos, 0x7F);
    assert_eq!(
        (carry(&refused), ax(&refused)),
        (true, 0x14),
        "a record of another level: {refused:X?}"
    );
    let refused = ioctl(&mut dos, 0x6A);
    assert_eq!(
        (carry(&refused), ax(&refused)),
        (true, 0x16),
        "code pages are not ANSI.SYS's: {refused:X?}"
    );
}

/// Reads a byte with function `07h`, typing `keys` while it waits.
fn typed_char(dos: &mut Dos, keys: &[Scan]) -> u8 {
    let got = typed(dos, keys, |r| r.gpr[gpr::AX] = 0x0700);
    ax(&got).to_le_bytes()[0]
}

/// A word at `segment:offset`.
fn peek16(m: &Machine, segment: u16, offset: u16) -> u16 {
    let at = crate::cpu::linear(segment, offset);
    u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)])
}

/// Sets the word at `segment:offset`.
fn poke16(m: &mut Machine, segment: u16, offset: u16, value: u16) {
    let [low, high] = value.to_le_bytes();
    m.board_mut()
        .write(crate::cpu::linear(segment, offset), low, 0);
    m.board_mut()
        .write(crate::cpu::linear(segment, offset.wrapping_add(1)), high, 0);
}

/// A system booted with `STACKS=8,128`, and the segment of its stacks.
fn with_stacks() -> (Dos, u16) {
    let dos = Dos::boot(&[File {
        name: b"CONFIG  SYS",
        bytes: b"STACKS=8,128\r\n",
    }]);
    let block = kernel16(&dos.m, data::driver::STACKS);
    assert_ne!(block, 0);
    (dos, block)
}

/// Where the timer's handler is kept among the vectors the stacks replaced.
const TIMER_KEPT: u16 = stacks::field::VECTORS + 4;

/// Points the stacks' timer at `code` in the shell's segment, which goes on
/// to the handler the stacks replaced.
fn timer_through(dos: &mut Dos, block: u16, mut code: Vec<u8>) {
    const HANDLER: u16 = 0x0800;
    let offset = peek16(&dos.m, block, TIMER_KEPT);
    let segment = peek16(&dos.m, block, TIMER_KEPT + 2);
    code.push(0xEA);
    code.extend_from_slice(&offset.to_le_bytes());
    code.extend_from_slice(&segment.to_le_bytes());
    dos.put(HANDLER, &code);
    let psp = dos.psp;
    poke16(&mut dos.m, block, TIMER_KEPT, HANDLER);
    poke16(&mut dos.m, block, TIMER_KEPT + 2, psp);
}

/// Runs the idle shell for `span` of emulated time.
fn idle_for(dos: &mut Dos, span: Nanos) {
    let now = dos.m.now();
    run_until(&mut dos.m, now + span);
}

#[test]
fn stacks_run_each_hardware_interrupt_on_a_stack_of_dos_s_own() {
    const SEEN: u16 = 0x0840;
    let (mut dos, block) = with_stacks();
    let timer = (peek16(&dos.m, 0, 0x08 * 4), peek16(&dos.m, 0, 0x08 * 4 + 2));
    assert_eq!(
        timer,
        (rom::entry::STACKS + rom::entry::STACKS_STRIDE, 0xF000)
    );
    let (segment_at, pointer_at) = (SEEN.to_le_bytes(), (SEEN + 2).to_le_bytes());
    // push ax; mov ax, ss; mov cs:[SEEN], ax; mov ax, sp; mov cs:[SEEN+2], ax; pop ax
    let keep = vec![
        0x50,
        0x8C,
        0xD0,
        0x2E,
        0xA3,
        segment_at[0],
        segment_at[1],
        0x89,
        0xE0,
        0x2E,
        0xA3,
        pointer_at[0],
        pointer_at[1],
        0x58,
    ];
    timer_through(&mut dos, block, keep);
    dos.put(SEEN, &[0; 4]);
    let ticks = dos.m.board().peek(0x46C);
    idle_for(&mut dos, SECOND / 4);
    assert_ne!(
        dos.m.board().peek(0x46C),
        ticks,
        "the BIOS's timer still ran"
    );
    let (ss, sp) = (
        peek16(&dos.m, dos.psp, SEEN),
        peek16(&dos.m, dos.psp, SEEN + 2),
    );
    assert_eq!(ss, block, "SS:SP {ss:04X}:{sp:04X}");
    let stacks = stacks::field::RECORDS + 8 * stacks::record::LEN;
    assert!((stacks..stacks + 8 * 128).contains(&sp), "SP {sp:04X}");
    for n in 0..8 {
        let taken = peek16(
            &dos.m,
            block,
            stacks::field::RECORDS + n * stacks::record::LEN,
        );
        assert_eq!(taken, 0, "stack {n} was given back");
    }
    assert_eq!(
        typed_char(&mut dos, &keys_of("k")),
        b'k',
        "the keyboard's interrupt is switched too"
    );
}

#[test]
fn an_interrupt_with_no_stack_free_halts_the_machine() {
    let (mut dos, block) = with_stacks();
    for n in 0..8 {
        poke16(
            &mut dos.m,
            block,
            stacks::field::RECORDS + n * stacks::record::LEN,
            1,
        );
    }
    idle_for(&mut dos, SECOND / 4);
    let shown = screen(&dos.m);
    assert!(
        shown.contains("Internal stack overflow") && shown.contains("System halted"),
        "{shown}"
    );
    let regs = dos.m.cpu().regs();
    assert_eq!(regs.sreg(Seg::Cs), 0xF000);
    assert!(
        !regs.flag(flag::IF),
        "the machine stopped with interrupts off"
    );
}

#[test]
fn a_stack_left_otherwise_than_it_was_taken_halts_the_machine() {
    let (mut dos, block) = with_stacks();
    // push bp; mov bp, sp; mov word [bp+8], FFFFh (the stack's number); pop bp
    timer_through(
        &mut dos,
        block,
        vec![0x55, 0x89, 0xE5, 0xC7, 0x46, 0x08, 0xFF, 0xFF, 0x5D],
    );
    idle_for(&mut dos, SECOND / 4);
    let shown = screen(&dos.m);
    assert!(
        shown.contains("Internal stack failure") && shown.contains("System halted"),
        "{shown}"
    );
}

#[test]
fn no_stacks_leaves_the_vectors_alone() {
    let dos = Dos::boot(&[]);
    assert_eq!(kernel16(&dos.m, data::driver::STACKS), 0);
    assert_eq!(peek16(&dos.m, 0, 0x08 * 4 + 2), 0xF000);
    assert_ne!(
        peek16(&dos.m, 0, 0x08 * 4),
        rom::entry::STACKS + rom::entry::STACKS_STRIDE
    );
}

/// Each key of `keys` pressed and let go.
fn strokes(keys: &[Scan]) -> Vec<(Scan, bool)> {
    keys.iter()
        .flat_map(|&key| [(key, true), (key, false)])
        .collect()
}

/// A program that keeps the first 16 bytes of its command tail in the
/// BIOS's communication area, then ends.
fn tail_keeper() -> Vec<u8> {
    vec![
        0xBE, 0x80, 0x00, // mov si, 80h
        0xB8, 0x40, 0x00, // mov ax, 40h
        0x8E, 0xC0, // mov es, ax
        0xBF, 0xF0, 0x00, // mov di, F0h
        0xB9, 0x10, 0x00, // mov cx, 16
        0xFC, // cld
        0xF3, 0xA4, // rep movsb
        0xB8, 0x00, 0x4C, // mov ax, 4C00h
        0xCD, 0x21, // int 21h
    ]
}

#[test]
fn install_runs_its_program_with_its_tail_before_the_shell() {
    let keeper = tail_keeper();
    let dos = Dos::boot(&[
        File {
            name: b"CONFIG  SYS",
            bytes: b"INSTALL=A:\\KEEP.COM ARGS\r\n",
        },
        File {
            name: b"KEEP    COM",
            bytes: &keeper,
        },
    ]);
    let kept: Vec<u8> = (0..7).map(|n| dos.m.board().peek(0x4F0 + n)).collect();
    assert_eq!(kept, [5, b' ', b'A', b'R', b'G', b'S', b'\r']);
}

#[test]
fn shell_names_the_interpreter_and_its_tail() {
    let other = com(idle);
    let dos = Dos::boot(&[
        File {
            name: b"CONFIG  SYS",
            bytes: b"SHELL=A:\\OTHER.COM /E:256 /P\r\n",
        },
        File {
            name: b"OTHER   COM",
            bytes: &other,
        },
    ]);
    assert_eq!(dos.get(0x80, 12), b"\x0A /E:256 /P\r");
    let name: Vec<u8> = (0..8)
        .map(|n| dos.m.board().peek(crate::cpu::linear(dos.psp - 1, 8 + n)))
        .collect();
    assert_eq!(name, b"OTHER\0\0\0");
}

#[test]
fn a_missing_shell_is_asked_for_by_name() {
    let mut m = Dos::machine(&[File {
        name: b"CONFIG  SYS",
        bytes: b"SHELL=A:\\NOPE.COM\r\n",
    }]);
    run_while_not(&mut m, 30 * SECOND, |m| {
        screen(m).contains("Enter correct name of Command Interpreter")
    });
    assert!(
        screen(&m).contains("Bad or missing Command Interpreter"),
        "{}",
        screen(&m)
    );
    press(&mut m, &strokes(&keys_of("command.com\r")));
    let dos = Dos::shell_of(m);
    let name: Vec<u8> = (0..8)
        .map(|n| dos.m.board().peek(crate::cpu::linear(dos.psp - 1, 8 + n)))
        .collect();
    assert_eq!(name, b"COMMAND\0");
}

/// Another 360 KiB volume holding `files`: the test diskette's format, its
/// own serial.
fn stranger(files: &[File<'_>]) -> Vec<u8> {
    image_of(&formats::Diskette::K360.volume(0x2026_1110), files)
}

/// The 80186 workstation, whose drive reports a change on its line, booted
/// from the test diskette.
fn change_line_dos() -> Dos {
    Dos::shell_of(Dos::machine_with(&Template::Workstation186.config(), &[]))
}

/// Runs until the recording handler has kept `codes` codes and waits for
/// its answer.
fn until_kept(dos: &mut Dos, codes: u16) {
    let psp = dos.psp;
    run_while_not(&mut dos.m, 10 * SECOND, |m| kept_codes(m, psp) == codes);
}

/// The stub's code to create `A:\KEPT.TXT` as ASCIZ `name` names it, write
/// the four bytes at `bytes` into it and close it; the carry stands for the
/// first call that fails.
fn keep_file(name: u16, bytes: u16) -> Vec<u8> {
    let mut asm = Asm::new(STUB);
    let failed = asm.label();
    asm.mov_imm16(Reg16::Ax, 0x3C00);
    asm.zero(Reg16::Cx);
    asm.mov_imm16(Reg16::Dx, name);
    asm.int(0x21);
    asm.jcc(Cond::B, &failed);
    asm.push(Reg16::Ax);
    asm.pop(Reg16::Bx);
    asm.mov_imm16(Reg16::Ax, 0x4000);
    asm.mov_imm16(Reg16::Cx, 4);
    asm.mov_imm16(Reg16::Dx, bytes);
    asm.int(0x21);
    asm.jcc(Cond::B, &failed);
    asm.mov_imm16(Reg16::Ax, 0x3E00);
    asm.int(0x21);
    asm.bind(&failed);
    asm.finish().unwrap()
}

#[test]
fn a_new_diskette_in_a_drive_with_a_change_line_is_read_without_complaint() {
    // The BPB read after the line told of the change met the line itself,
    // and its error stood as an invalid disk change.
    let mut dos = change_line_dos();
    recording_handler(&mut dos, false);
    dos.m.board_mut().eject_floppy(0);
    let other = stranger(&[File {
        name: b"OTHER   TXT",
        bytes: b"x",
    }]);
    dos.m.board_mut().insert_floppy(0, floppy(other)).unwrap();
    let pattern = dos.asciz(DATA, "A:\\OTHER.TXT");
    let found = dos.call(|r| {
        with_path(0x4E, 0, pattern)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&found), "{found:X?}");
    assert_eq!(critical_codes(&dos), []);
}

/// Function `47h` for drive `dl`: the directory it gives, or its error.
fn current_directory(dos: &mut Dos, dl: u8) -> Result<Vec<u8>, u16> {
    let asked = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4700;
        r.gpr[gpr::DX] = u16::from(dl);
        r.gpr[gpr::SI] = DATA + 0x100;
    });
    if carry(&asked) {
        return Err(ax(&asked));
    }
    let mut text = dos.get(DATA + 0x100, 64);
    text.truncate(text.iter().position(|&b| b == 0).unwrap_or(text.len()));
    Ok(text)
}

#[test]
fn the_current_directory_outlives_a_change_to_a_disk_that_holds_it() {
    // A change of diskette sent the drive back to its root. DOS keeps the
    // directory's path, so a second disk of a set holding it is worked in
    // where the first was, and gives it up for the root only when asked for
    // it on a disk without it.
    let mut dos = change_line_dos();
    recording_handler(&mut dos, false);
    let sub = dos.asciz(DATA, "A:\\SUB");
    assert!(!carry(&dos.call(with_path(0x39, 0, sub))));
    assert!(!carry(&dos.call(with_path(0x3B, 0, sub))));
    dos.call(|r| r.gpr[gpr::AX] = 0x0D00);
    let holding = volume::volume(
        &formats::Diskette::K360.volume(0x1990_0101),
        None,
        &[volume::File {
            path: "SUB\\IN.TXT",
            bytes: b"in",
            attributes: 0,
        }],
        tairix_fat::stamp::Stamp::NONE,
    )
    .unwrap();
    dos.m.board_mut().eject_floppy(0);
    dos.m.board_mut().insert_floppy(0, floppy(holding)).unwrap();
    let name = dos.asciz(DATA, "IN.TXT");
    let opened = dos.call(with_path(0x3D, 0, name));
    assert!(
        !carry(&opened),
        "a name is found in the directory on the new disk: {opened:X?}"
    );
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = ax(&opened);
    });
    assert_eq!(current_directory(&mut dos, 1), Ok(b"SUB".to_vec()));
    dos.m.board_mut().eject_floppy(0);
    dos.m
        .board_mut()
        .insert_floppy(0, floppy(stranger(&[])))
        .unwrap();
    assert_eq!(
        current_directory(&mut dos, 0),
        Ok(Vec::new()),
        "the root of a disk with no SUB"
    );
    let cds = dos.m.board().peek(crate::cpu::linear(
        data::SEGMENT,
        fs::cds_at(0) + data::cds::PATH + 3,
    ));
    assert_eq!(cds, 0, "the drive itself is back at its root");
    assert_eq!(critical_codes(&dos), []);
}

#[test]
fn asking_an_empty_drive_its_directory_checks_the_medium() {
    // The directory was read from DOS's own records, so a program asking it
    // to learn whether a disk is there was told one was.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    assert_eq!(
        current_directory(&mut dos, 2),
        Err(device::extended(u16::from(device::error::NOT_READY)))
    );
    assert_eq!(critical_codes(&dos), [device::error::NOT_READY]);
    let why = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5900;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(ax(&why), crate::dos::fs::error::FAIL_ON_24);
    assert_eq!(
        current_directory(&mut dos, 27),
        Err(crate::dos::fs::error::INVALID_DRIVE)
    );
}

#[test]
fn a_diskette_put_back_under_held_writes_takes_them_and_a_stranger_does_not() {
    // Holding sectors it had not written, DOS asked about the change for as
    // long as the line stayed up, which only a read clears; the driver now
    // tells the volume it holds from another by its boot sector.
    let mut dos = change_line_dos();
    recording_handler(&mut dos, true);
    let name = dos.asciz(DATA, "A:\\KEPT.TXT");
    let created = dos.call(|r| {
        with_path(0x3C, 0, name)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&created), "{created:X?}");
    let handle = ax(&created);
    dos.put(DATA + 0x80, b"held");
    let wrote = dos.call(|r| {
        r.gpr[gpr::AX] = 0x4000;
        r.gpr[gpr::BX] = handle;
        r.gpr[gpr::CX] = 4;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert_eq!(ax(&wrote), 4, "{wrote:X?}");
    let held = dos.m.board_mut().eject_floppy(0).unwrap();
    let other = stranger(&[]);
    dos.m
        .board_mut()
        .insert_floppy(0, floppy(other.clone()))
        .unwrap();
    let end = dos.begin(&[0xCD, 0x21], |r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = handle;
    });
    until_kept(&mut dos, 1);
    let mut taken_out = dos.m.board_mut().eject_floppy(0).unwrap();
    dos.m.board_mut().insert_floppy(0, held).unwrap();
    press(&mut dos.m, &strokes(&keys_of("1")));
    let closed = dos.finish(end);
    assert!(!carry(&closed), "{closed:X?}");
    assert_eq!(critical_codes(&dos), [device::error::DISK_CHANGE]);
    assert!(
        diskette_bytes(&mut taken_out) == other,
        "nothing reached the stranger"
    );
    let mut written = dos.m.board_mut().eject_floppy(0).unwrap();
    assert_eq!(
        root_file(&diskette_bytes(&mut written), b"KEPT    TXT").as_deref(),
        Some(&b"held"[..])
    );
}

#[test]
fn a_write_met_by_a_changed_diskette_never_reaches_it() {
    // A failed call was tried again after a reset, which found the line
    // clear: a write carried on onto whatever diskette had been put in.
    let mut dos = change_line_dos();
    recording_handler(&mut dos, true);
    let volume = diskette_bytes(&mut dos.m.board_mut().eject_floppy(0).unwrap());
    dos.m
        .board_mut()
        .insert_floppy(0, protected_floppy(volume.clone()))
        .unwrap();
    let name = dos.asciz(DATA, "A:\\KEPT.TXT");
    dos.put(DATA + 0x80, b"held");
    let end = dos.begin(&keep_file(name, DATA + 0x80), |_| {});
    until_kept(&mut dos, 1);
    dos.m.board_mut().eject_floppy(0);
    let other = stranger(&[]);
    dos.m
        .board_mut()
        .insert_floppy(0, floppy(other.clone()))
        .unwrap();
    press(&mut dos.m, &strokes(&keys_of("1")));
    until_kept(&mut dos, 2);
    let mut taken_out = dos.m.board_mut().eject_floppy(0).unwrap();
    dos.m.board_mut().insert_floppy(0, floppy(volume)).unwrap();
    press(&mut dos.m, &strokes(&keys_of("1")));
    let kept = dos.finish(end);
    assert!(!carry(&kept), "{kept:X?}");
    assert_eq!(
        critical_codes(&dos),
        [device::error::WRITE_PROTECT, device::error::DISK_CHANGE]
    );
    assert!(
        diskette_bytes(&mut taken_out) == other,
        "nothing reached the stranger"
    );
    let mut written = dos.m.board_mut().eject_floppy(0).unwrap();
    assert_eq!(
        root_file(&diskette_bytes(&mut written), b"KEPT    TXT").as_deref(),
        Some(&b"held"[..])
    );
}

#[test]
fn a_call_failed_at_its_critical_error_keeps_its_code_and_says_why() {
    // The caller gets the device's error, as from DOS 3.1 on, and only the
    // last error tells it the call was failed on INT 24h, which no call
    // recorded before.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    let name = dos.asciz(DATA, "B:\\ANY.TXT");
    let refused = dos.call(with_path(0x3D, 0, name));
    assert!(carry(&refused), "{refused:X?}");
    assert_eq!(
        ax(&refused),
        device::extended(u16::from(device::error::NOT_READY))
    );
    let why = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5900;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(ax(&why), crate::dos::fs::error::FAIL_ON_24);
    assert_eq!(why.gpr[gpr::BX] >> 8, 11, "the medium's class");
    let missing = dos.asciz(DATA + 0x20, "A:\\NONE.TXT");
    let absent = dos.call(with_path(0x3D, 0, missing));
    assert!(carry(&absent), "{absent:X?}");
    let why = dos.call(|r| {
        r.gpr[gpr::AX] = 0x5900;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(
        ax(&why),
        crate::dos::fs::error::FILE_NOT_FOUND,
        "the next call's error is its own"
    );
}

#[test]
fn a_sector_that_reads_at_its_second_try_is_read() {
    // The BPB read had no retry: a medium the drive read only at a second
    // try was a data error at once.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    dos.m.board_mut().eject_floppy(0);
    let other = stranger(&[File {
        name: b"OTHER   TXT",
        bytes: b"x",
    }]);
    dos.m
        .board_mut()
        .insert_floppy(0, unreliable_floppy(other, |_, before| before == 0))
        .unwrap();
    let now = dos.m.now();
    run_until(&mut dos.m, now + 3 * SECOND);
    let pattern = dos.asciz(DATA, "A:\\OTHER.TXT");
    let found = dos.call(|r| {
        with_path(0x4E, 0, pattern)(r);
        r.gpr[gpr::CX] = 0;
    });
    assert!(!carry(&found), "{found:X?}");
    assert_eq!(critical_codes(&dos), []);
}

#[test]
fn setting_a_media_id_keeps_the_units_geometry() {
    // The new ID's boot sector was learned whole, its parameter block taken
    // for the medium's however little DOS could use it: here a size of
    // none, after which every sector was past the volume's end.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    let mut volume = diskette_bytes(&mut dos.m.board_mut().eject_floppy(0).unwrap());
    volume[19..21].fill(0);
    volume[32..36].fill(0);
    dos.m.board_mut().insert_floppy(0, floppy(volume)).unwrap();
    let mut packet = [0u8; 0x19];
    packet[2..6].copy_from_slice(&0x2026_1110u32.to_le_bytes());
    packet[6..0x11].copy_from_slice(b"RENAMED    ");
    packet[0x11..].copy_from_slice(b"FAT12   ");
    dos.put(DATA, &packet);
    let set = dos.call(|r| {
        r.gpr[gpr::AX] = 0x440D;
        r.gpr[gpr::BX] = 1;
        r.gpr[gpr::CX] = 0x0846;
        r.gpr[gpr::DX] = DATA;
    });
    assert!(!carry(&set), "{set:X?}");
    let read = dos.run(
        &ABSOLUTE_READ,
        |r| {
            r.gpr[gpr::AX] = 0;
            r.gpr[gpr::CX] = 1;
            r.gpr[gpr::DX] = 700;
            r.gpr[gpr::BX] = DATA + 0x100;
        },
        &[],
    );
    assert!(!carry(&read), "{read:X?}");
    assert_eq!(critical_codes(&dos), []);
}

#[test]
fn dos_starts_its_serial_ports_at_2400_bits_a_second() {
    // They were left at the power-on divisor, which counts as 65,536: under
    // two bits a second, so a line written to AUX took minutes.
    let mut dos = Dos::boot(&[]);
    let board = dos.m.board_mut();
    let lcr = board.input(0x3FB, 0);
    board.output(0x3FB, lcr | 0x80, 0);
    let divisor = u16::from_le_bytes([board.input(0x3F8, 0), board.input(0x3F9, 0)]);
    board.output(0x3FB, lcr, 0);
    assert_eq!(divisor, 48, "115 200 over 2400");
    assert_eq!(lcr & 0x3F, 0x03, "eight data bits, one stop bit, no parity");
}

/// The label the boot record of drive `bl` holds, through generic IOCTL.
fn media_label(dos: &mut Dos, bl: u16) -> Vec<u8> {
    let asked = dos.call(|r| {
        r.gpr[gpr::AX] = 0x440D;
        r.gpr[gpr::BX] = bl;
        r.gpr[gpr::CX] = 0x0866;
        r.gpr[gpr::DX] = DATA + 0x180;
    });
    assert!(!carry(&asked), "{asked:X?}");
    dos.get(DATA + 0x180 + 6, 11)
}

/// The label a search of drive A finds, if any.
fn directory_label(dos: &mut Dos) -> Option<Vec<u8>> {
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1A00;
        r.gpr[gpr::DX] = DATA + 0x1C0;
    });
    let pattern = dos.asciz(DATA + 0x40, "A:\\*.*");
    let found = dos.call(|r| {
        with_path(0x4E, 0, pattern)(r);
        r.gpr[gpr::CX] = 0x08;
    });
    if carry(&found) {
        return None;
    }
    let name = dos.get(DATA + 0x1C0 + 0x1E, 13);
    Some(name[..name.iter().position(|&b| b == 0).unwrap_or(13)].to_vec())
}

#[test]
fn a_volume_label_is_made_one_a_directory_and_deleted_through_an_fcb() {
    // A file made with the label attribute was refused, and an FCB delete
    // of one passed it by, so no program could label a disk.
    let mut dos = Dos::boot(&[]);
    let name = dos.asciz(DATA, "A:\\MY DISK");
    let made = dos.call(|r| {
        with_path(0x5B, 0, name)(r);
        r.gpr[gpr::CX] = 0x08;
    });
    assert!(!carry(&made), "{made:X?}");
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x3E00;
        r.gpr[gpr::BX] = ax(&made);
    });
    assert_eq!(directory_label(&mut dos).as_deref(), Some(&b"MY DISK"[..]));
    assert_eq!(
        media_label(&mut dos, 1),
        b"MY DISK    ",
        "the boot record carries it too"
    );
    let other = dos.asciz(DATA, "A:\\OTHER");
    let second = dos.call(|r| {
        with_path(0x3C, 0, other)(r);
        r.gpr[gpr::CX] = 0x08;
    });
    assert!(
        carry(&second) && ax(&second) == fs::error::ACCESS_DENIED,
        "one label a directory: {second:X?}"
    );
    let mut fcb = [0u8; 44];
    fcb[0] = 0xFF;
    fcb[6] = 0x08;
    fcb[8..19].copy_from_slice(b"???????????");
    dos.put(DATA + 0x80, &fcb);
    let deleted = dos.call(|r| {
        r.gpr[gpr::AX] = 0x1300;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert_eq!(deleted.gpr[gpr::AX] & 0xFF, 0, "{deleted:X?}");
    assert_eq!(directory_label(&mut dos), None);
    assert_eq!(media_label(&mut dos, 1), b"NO NAME    ");
    fcb[8..19].copy_from_slice(b"FROM FCB   ");
    dos.put(DATA + 0x80, &fcb);
    let created = dos.call(|r| {
        r.gpr[gpr::AX] = 0x1600;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert_eq!(created.gpr[gpr::AX] & 0xFF, 0, "{created:X?}");
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x1000;
        r.gpr[gpr::DX] = DATA + 0x80;
    });
    assert_eq!(directory_label(&mut dos).as_deref(), Some(&b"FROM FCB"[..]));
}

#[test]
fn a_high_density_track_is_read_whole_in_one_transfer() {
    // The BIOS reads on to the other head after the sector its table's
    // sectors a track name, nine as the BIOS has it, so a transfer past a
    // high-density track's ninth sector took the other side's.
    let config = Template::Workstation186.config();
    let shell = com(idle);
    let mut image = image_of(
        &formats::Diskette::K1440.volume(1),
        &[File {
            name: b"COMMAND COM",
            bytes: &shell,
        }],
    );
    for (lba, sector) in image.chunks_mut(512).enumerate().skip(36).take(18) {
        sector.fill(u8::try_from(lba).unwrap());
    }
    let mut dos = Dos::shell_of(bare_on(&config, image));
    let read = dos.run(
        &ABSOLUTE_READ,
        |r| {
            r.gpr[gpr::AX] = 0;
            r.gpr[gpr::CX] = 18;
            r.gpr[gpr::DX] = 36;
            r.gpr[gpr::BX] = DATA + 0x200;
        },
        &[],
    );
    assert!(!carry(&read), "{read:X?}");
    for (lba, sector) in (36..).zip(dos.get(DATA + 0x200, 18 * 512).chunks(512)) {
        assert!(
            sector.iter().all(|&b| usize::from(b) == lba),
            "sector {lba} holds {}",
            sector[0]
        );
    }
    let word =
        |at: u32| u32::from(dos.m.board().peek(at)) | u32::from(dos.m.board().peek(at + 1)) << 8;
    let table = (word(0x1E * 4 + 2) << 4) + word(0x1E * 4);
    assert_eq!(
        dos.m.board().peek(table + 4),
        9,
        "INT 1Eh's table keeps the BIOS's nine between transfers"
    );
}

#[test]
fn a_track_a_program_formats_through_the_bios_is_a_change_dos_sees() {
    // DOS heard of no format a program asked the BIOS for, so within two
    // seconds of its last access it took the disk for the one it had read.
    let mut dos = Dos::boot(&[]);
    recording_handler(&mut dos, false);
    let free = |dos: &mut Dos| {
        dos.call(|r| {
            r.gpr[gpr::AX] = 0x3600;
            r.gpr[gpr::DX] = 1;
        })
    };
    assert_eq!(free(&mut dos).gpr[gpr::DX], 354);
    let ids: Vec<u8> = (1..=9).flat_map(|sector| [0, 0, sector, 2]).collect();
    dos.put(DATA + 0x200, &ids);
    let formatted = dos.int(0x13, |r| {
        r.gpr[gpr::AX] = 0x0509;
        r.gpr[gpr::CX] = 0x0001;
        r.gpr[gpr::DX] = 0x0000;
        r.gpr[gpr::BX] = DATA + 0x200;
    });
    assert!(!carry(&formatted), "{formatted:X?}");
    free(&mut dos);
    assert_eq!(
        critical_codes(&dos),
        [7],
        "the formatted disk is of no format DOS knows"
    );
}
