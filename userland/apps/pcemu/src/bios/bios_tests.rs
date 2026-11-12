use super::*;
use crate::asm::{Asm, Reg16, Reg8};
use crate::chips::keyboard::Scan;
use crate::cpu::Bus;
use crate::machine::Template;
use crate::storage::floppy::Floppy;
use crate::storage::MemoryDisk;
use crate::time::NANOS_PER_SECOND;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;

const SECOND: Nanos = NANOS_PER_SECOND;

pub(super) fn machine(template: Template) -> Machine {
    crate::bios::machine(&template.config(), [None, None]).unwrap()
}

/// A 360 KiB disk whose boot sector is `program`.
pub(super) fn boot_disk(program: impl FnOnce(&mut Asm)) -> Floppy {
    let image = crate::dos::harness::boot_image(program);
    Floppy::raw(Box::new(MemoryDisk::new(image, false).unwrap())).unwrap()
}

/// Row `row` of the colour text screen.
pub(super) fn row(m: &Machine, row: u32) -> String {
    (0..80)
        .map(|column| char::from(m.board().peek(0xB_8000 + (row * 80 + column) * 2)))
        .collect()
}

fn print(asm: &mut Asm, text: &[u8]) {
    for &byte in text {
        asm.mov_imm16(Reg16::Ax, 0x0E00 | u16::from(byte));
        asm.int(0x10);
    }
}

pub(super) use crate::dos::harness::stop;

/// Whether the CPU has stopped in the loop that starts at `at`.
pub(super) fn stopped_at(m: &Machine, segment: u16, at: core::ops::Range<u16>) -> bool {
    let regs = m.cpu().regs();
    regs.sreg(crate::cpu::Seg::Cs) == segment && at.contains(&regs.ip)
}

#[test]
fn the_post_boots_a_floppy_that_prints() {
    let mut m = machine(Template::IbmXt);
    let disk = boot_disk(|asm| {
        print(asm, b"Booted");
        stop(asm);
    });
    assert!(m.board_mut().insert_floppy(0, disk).unwrap().is_none());
    run_until(&mut m, 3 * SECOND);
    assert!(
        row(&m, 0).starts_with("TAIRiX PC BIOS    640 KB"),
        "{:?}",
        row(&m, 0)
    );
    assert!(row(&m, 2).starts_with("Booted"), "{:?}", row(&m, 2));
}

#[test]
fn with_nothing_to_boot_the_bios_says_so_and_waits_for_a_key() {
    let mut m = machine(Template::IbmXt);
    // An empty drive turns no disk past the head, so each try waits out the
    // controller's two seconds, as an XT's does.
    run_until(&mut m, 12 * SECOND);
    let shown: String = (0..25).map(|r| row(&m, r)).collect();
    assert!(
        shown.contains("No system disk. Insert one and press a key."),
        "{shown}"
    );
}

#[test]
fn a_key_reaches_int_16h_as_its_code() {
    let mut m = machine(Template::IbmXt);
    let disk = boot_disk(|asm| {
        asm.mov_imm8(Reg8::Ah, 0);
        asm.int(0x16);
        asm.mov_imm8(Reg8::Ah, 0x0E);
        asm.int(0x10);
        stop(asm);
    });
    m.board_mut().insert_floppy(0, disk).unwrap();
    run_until(&mut m, 3 * SECOND);
    m.board_mut().key(Scan::plain(0x1E), true);
    run_until(&mut m, 3 * SECOND + SECOND / 10);
    m.board_mut().key(Scan::plain(0x1E), false);
    run_until(&mut m, 3 * SECOND + SECOND / 5);
    assert_eq!(&row(&m, 2)[..1], "a");
}

/// A machine past its POST, halted with interrupts off in a boot sector
/// that does nothing more.
pub(super) fn booted(template: Template, floppy: Option<Floppy>) -> Machine {
    let mut m = machine(template);
    let disk = floppy.unwrap_or_else(|| boot_disk(stop));
    m.board_mut().insert_floppy(0, disk).unwrap();
    run_until(&mut m, 3 * SECOND);
    assert!(
        stopped_at(&m, 0, 0x7C00..0x7E00),
        "the boot sector ran: {:X?}",
        m.cpu().regs()
    );
    m
}

/// Calls INT `vector` with the registers `set` gives, from a program that
/// halts once it returns, and hands back the registers it returned.
/// Starts `program` at `0:0600h` with the registers `set` gives, the
/// interrupts the BIOS serves unmasked as its POST leaves them, the fixed
/// disk's included; it ends halted with interrupts off. Returns where that
/// halt is.
pub(super) fn start(
    m: &mut Machine,
    program: impl FnOnce(&mut Asm),
    set: impl FnOnce(&mut crate::cpu::Regs),
) -> core::ops::Range<u16> {
    let mut asm = Asm::new(0x0600);
    program(&mut asm);
    asm.cli();
    let end = asm.here();
    let here = asm.label();
    asm.bind(&here);
    asm.hlt();
    asm.jmp_short(&here);
    for (addr, &byte) in (0x0600..).zip(&asm.finish().unwrap()) {
        m.board_mut().write(addr, byte, 0);
    }
    let at = m.board().cycles_at(m.now());
    m.board_mut().output(0x21, 0x9C, at);
    m.cpu_mut().reset();
    let regs = m.cpu_mut().regs_mut();
    regs.set_sreg(crate::cpu::Seg::Cs, 0);
    regs.ip = 0x0600;
    regs.set_sreg(crate::cpu::Seg::Ss, 0);
    regs.gpr[crate::cpu::gpr::SP] = 0x7000;
    set(regs);
    end..end + 3
}

/// Runs `program` to its end; returns the registers it ends with.
pub(super) fn run(
    m: &mut Machine,
    program: impl FnOnce(&mut Asm),
    set: impl FnOnce(&mut crate::cpu::Regs),
) -> crate::cpu::Regs {
    let end = start(m, program, set);
    let begun = m.now();
    loop {
        let now = m.now();
        run_until(m, now + SECOND / 20);
        if stopped_at(m, 0, end.clone()) {
            return *m.cpu().regs();
        }
        assert!(m.now() - begun < 20 * SECOND, "the program ended");
    }
}

/// Masks every interrupt at the 8259A, as a program may leave them, so
/// nothing but the end of a run stops a service's wait.
pub(super) fn mask_every_interrupt(m: &mut Machine) {
    let at = m.board().cycles_at(m.now());
    m.board_mut().output(0x21, 0xFF, at);
}

/// Runs the machine `runs` times for `slice` each, asserting none passes
/// its end by more than an instruction: no service holds the machine from
/// its owner. Stops early once the program halts at `end`; answers whether
/// it did.
pub(super) fn runs_end_where_asked(
    m: &mut Machine,
    end: &core::ops::Range<u16>,
    slice: crate::time::Nanos,
    runs: u32,
) -> bool {
    /// The most a run passes its end by: the instruction it ends inside.
    const OVERRUN: crate::time::Nanos = 50_000;
    for _ in 0..runs {
        if stopped_at(m, 0, end.clone()) {
            return true;
        }
        let until = m.now() + slice;
        run_until(m, until);
        assert!(
            m.now() <= until + OVERRUN,
            "a run passed its end by {} ns",
            m.now() - until
        );
    }
    stopped_at(m, 0, end.clone())
}

/// Calls INT `vector` with the registers `set` gives; returns the
/// registers it returned.
pub(super) fn call(
    m: &mut Machine,
    vector: u8,
    set: impl FnOnce(&mut crate::cpu::Regs),
) -> crate::cpu::Regs {
    run(
        m,
        |asm| {
            asm.sti();
            asm.int(vector);
        },
        set,
    )
}

/// Waits with interrupts on until the tick count has moved `ticks` on: the
/// BIOS serves the keyboard and timer meanwhile.
pub(super) fn wait_ticks(asm: &mut Asm, ticks: u8) {
    asm.sti();
    // MOV AX, [046Ch]; ADD AX, ticks; then HLT until [046Ch] - AX is no
    // longer negative.
    asm.bytes(&[0xA1, 0x6C, 0x04, 0x05, ticks, 0x00]);
    asm.bytes(&[0xF4, 0x8B, 0x1E, 0x6C, 0x04, 0x29, 0xC3, 0x78, 0xF7]);
}

pub(super) fn poke(m: &mut Machine, addr: u32, bytes: &[u8]) {
    for (at, &byte) in (addr..).zip(bytes) {
        m.board_mut().write(at, byte, 0);
    }
}

pub(super) fn carry(regs: &crate::cpu::Regs) -> bool {
    regs.flag(crate::cpu::flag::CF)
}

/// A raw image of `kib` KiB whose every sector begins with its own LBA.
pub(super) fn marked(kib: usize) -> Floppy {
    let mut bytes = vec![0u8; kib * 1024];
    for (lba, sector) in bytes.chunks_mut(512).enumerate() {
        sector[..2].copy_from_slice(&u16::try_from(lba).unwrap().to_le_bytes());
    }
    let mut asm = Asm::new(0x7C00);
    stop(&mut asm);
    let code = asm.finish().unwrap();
    bytes[..code.len()].copy_from_slice(&code);
    bytes[0x1FE] = 0x55;
    bytes[0x1FF] = 0xAA;
    Floppy::raw(Box::new(MemoryDisk::new(bytes, false).unwrap())).unwrap()
}

fn lba_at(m: &Machine, addr: u32) -> u16 {
    u16::from_le_bytes([m.board().peek(addr), m.board().peek(addr + 1)])
}

#[test]
fn int_13h_reads_writes_and_verifies_diskette_sectors() {
    let mut m = booted(Template::IbmXt, Some(marked(360)));
    let read = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0203);
        r.set16(1, 0x0102);
        r.set16(2, 0x0100);
        r.set16(3, 0x8000);
        r.set_sreg(crate::cpu::Seg::Es, 0);
    });
    assert!(!carry(&read), "status {:02X}", read.get8(4));
    assert_eq!(read.get16(0), 0x0003, "three sectors, no error");
    // Cylinder 1, head 1, sectors 2–4: LBAs 28 to 30.
    assert_eq!(
        [lba_at(&m, 0x8000), lba_at(&m, 0x8200), lba_at(&m, 0x8400)],
        [28, 29, 30]
    );
    for addr in 0x9000..0x9200 {
        m.board_mut().write(addr, 0x3C, 0);
    }
    let write = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0301);
        r.set16(1, 0x0005);
        r.set16(2, 0x0000);
        r.set16(3, 0x9000);
    });
    assert!(!carry(&write));
    let again = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0201);
        r.set16(1, 0x0005);
        r.set16(2, 0x0000);
        r.set16(3, 0xA000);
    });
    assert!(!carry(&again));
    assert_eq!(m.board().peek(0xA000), 0x3C);
    let verify = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0409);
        r.set16(1, 0x0001);
        r.set16(2, 0x0000);
    });
    assert!(!carry(&verify));
}

#[test]
fn a_transfer_across_a_dma_page_is_refused() {
    let mut m = booted(Template::IbmXt, Some(marked(360)));
    let regs = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0202);
        r.set16(1, 0x0001);
        r.set16(2, 0x0000);
        r.set16(3, 0xFF00);
    });
    assert!(carry(&regs));
    assert_eq!(regs.get8(4), 0x09);
}

#[test]
fn a_high_density_drive_finds_a_double_density_disk() {
    let mut m = booted(Template::TurboXt, Some(marked(720)));
    let regs = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0201);
        r.set16(1, 0x0103);
        r.set16(2, 0x0000);
        r.set16(3, 0x8000);
    });
    assert!(!carry(&regs), "status {:02X}", regs.get8(4));
    assert_eq!(
        lba_at(&m, 0x8000),
        2 * 9 + 2,
        "cylinder 1, sector 3 of a 720 KiB disk"
    );
    let types = call(&mut m, 0x13, |r| r.set16(0, 0x0800));
    assert_eq!(
        (types.get8(3), types.get8(5), types.get8(1)),
        (4, 79, 18),
        "a 1.44 MiB drive, as the firmware records it"
    );
    assert_eq!(types.get8(2), 2, "two drives");
    let line = call(&mut m, 0x13, |r| r.set16(0, 0x1500));
    assert_eq!(line.get8(4), 2, "a drive with a change line");
}

#[test]
fn a_new_disk_is_reported_before_it_is_read() {
    let mut m = booted(Template::TurboXt, Some(marked(1440)));
    m.board_mut().insert_floppy(0, marked(1440)).unwrap();
    // A disk put in a turning drive is up to speed a moment later.
    let now = m.now();
    run_until(&mut m, now + SECOND);
    let changed = call(&mut m, 0x13, |r| r.set16(0, 0x1600));
    assert!(carry(&changed));
    assert_eq!(changed.get8(4), 0x06);
    let first = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0201);
        r.set16(1, 0x0001);
        r.set16(2, 0x0000);
        r.set16(3, 0x8000);
    });
    assert_eq!(
        (carry(&first), first.get8(4)),
        (true, 0x06),
        "the read reports the change"
    );
    let second = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0201);
        r.set16(1, 0x0001);
        r.set16(2, 0x0000);
        r.set16(3, 0x8000);
    });
    assert!(!carry(&second), "status {:02X}", second.get8(4));
}

#[test]
fn the_tick_counts_eighteen_times_a_second() {
    let mut m = booted(Template::IbmXt, None);
    let before = call(&mut m, 0x1A, |r| r.set16(0, 0x0000));
    let start = m.now();
    m.cpu_mut().regs_mut().set_flag(crate::cpu::flag::IF, true);
    run_until(&mut m, start + 10 * SECOND);
    let after = call(&mut m, 0x1A, |r| r.set16(0, 0x0000));
    let ticks = |r: &crate::cpu::Regs| (u32::from(r.get16(1)) << 16) | u32::from(r.get16(2));
    let counted = ticks(&after) - ticks(&before);
    assert!(
        (180..=184).contains(&counted),
        "{counted} ticks in ten seconds"
    );
}

#[test]
fn a_tick_count_written_past_a_day_rolls_over_at_the_next_tick() {
    let mut m = booted(Template::IbmXt, None);
    call(&mut m, 0x1A, |r| r.set16(0, 0x0000));
    for (offset, byte) in (0x46Cu32..).zip([0xFF; 4]) {
        m.board_mut().write(offset, byte, 0);
    }
    let start = m.now();
    m.cpu_mut().regs_mut().set_flag(crate::cpu::flag::IF, true);
    run_until(&mut m, start + SECOND);
    let read = call(&mut m, 0x1A, |r| r.set16(0, 0x0000));
    let ticks = (u32::from(read.get16(1)) << 16) | u32::from(read.get16(2));
    assert!(ticks <= 19, "{ticks} ticks");
    assert_ne!(read.get8(0), 0, "a midnight counted");
}

#[test]
fn the_equipment_and_memory_reports_follow_the_machine() {
    let mut m = booted(Template::IbmXt, None);
    let memory = call(&mut m, 0x12, |_| {});
    assert_eq!(memory.get16(0), 640);
    let equipment = call(&mut m, 0x11, |_| {});
    assert_eq!(
        equipment.get16(0) & 0x33,
        0x21,
        "diskettes, no coprocessor, and an 80-column colour display"
    );
    assert_eq!(
        equipment.get16(0) >> 9,
        0b10_1001,
        "a printer, a game port and one serial port"
    );
    let system = call(&mut m, 0x15, |r| r.set16(0, 0x8800));
    assert_eq!(
        (carry(&system), system.get8(4)),
        (true, 0x86),
        "no extended memory on an XT"
    );
}

#[test]
fn the_equipment_report_names_a_fitted_coprocessor() {
    let mut config = Template::IbmXt.config();
    config.coprocessor = true;
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut().insert_floppy(0, boot_disk(stop)).unwrap();
    run_until(&mut m, 3 * SECOND);
    let equipment = call(&mut m, 0x11, |_| {});
    assert_eq!(equipment.get16(0) & 0x02, 0x02);
}

#[test]
fn the_post_lists_the_parallel_port_it_finds() {
    let mut config = Template::IbmXt.config();
    config.ports.parallel = [
        Some(crate::board::ParallelPort {
            base: 0x278,
            device: crate::ports::parallel::Device::Covox,
        }),
        None,
        None,
    ];
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut().insert_floppy(0, boot_disk(stop)).unwrap();
    run_until(&mut m, 3 * SECOND);
    let word =
        |m: &Machine, at: u32| u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)]);
    assert_eq!(
        (word(&m, 0x408), word(&m, 0x40A)),
        (0x278, 0),
        "LPT1, and no LPT2"
    );
    let equipment = call(&mut m, 0x11, |_| {});
    assert_eq!(equipment.get16(0) >> 14, 1, "one printer port");
}

const DISK: crate::storage::hdc::DiskType = crate::storage::hdc::DiskType {
    cylinders: 64,
    heads: 2,
    track_to_track: 3_000_000,
    average_seek: 85_000_000,
};

/// An XT with a fixed disk whose every sector begins with its own LBA, and
/// whose first holds `boot`.
fn with_fixed_disk(boot: impl FnOnce(&mut Asm)) -> Machine {
    with_fixed_disks(1, boot)
}

/// The same with `count` such disks, from one to two.
fn with_fixed_disks(count: usize, boot: impl FnOnce(&mut Asm)) -> Machine {
    let config = Template::IbmXt.config();
    let mut m = crate::bios::machine(&config, [Some(DISK), (count > 1).then_some(DISK)]).unwrap();
    let mut bytes = vec![0u8; 64 * 2 * 17 * 512];
    for (lba, sector) in bytes.chunks_mut(512).enumerate() {
        sector[..2].copy_from_slice(&u16::try_from(lba).unwrap().to_le_bytes());
    }
    let mut asm = Asm::new(0x7C00);
    boot(&mut asm);
    let code = asm.finish().unwrap();
    bytes[..code.len()].copy_from_slice(&code);
    bytes[0x1FE] = 0x55;
    bytes[0x1FF] = 0xAA;
    let format = crate::storage::hdc::LowLevel {
        interleave: 3,
        bad_tracks: alloc::vec::Vec::new(),
    };
    for n in 0..count {
        m.board_mut()
            .attach_fixed(
                n,
                DISK,
                Box::new(MemoryDisk::new(bytes.clone(), false).unwrap()),
                format.clone(),
            )
            .unwrap();
    }
    m
}

#[test]
fn both_of_two_fixed_disks_are_counted_and_reported() {
    let mut m = with_fixed_disks(2, stop);
    run_until(&mut m, 12 * SECOND);
    assert_eq!(
        m.board().peek(0x475),
        2,
        "the second's completion carries its unit, not an error"
    );
    let second = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0800);
        r.set16(2, 0x0081);
    });
    assert!(!carry(&second), "status {:02X}", second.get8(4));
    assert_eq!((second.get8(5), second.get8(2)), (62, 2));
}

#[test]
fn a_fixed_disks_parameters_with_no_fixed_disk_bios_say_there_is_none() {
    let mut m = booted(Template::IbmPc, None);
    let asked = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0800);
        r.set16(2, 0x0080);
    });
    assert!(carry(&asked));
    assert_eq!(
        (asked.get8(4), asked.get8(2)),
        (0x01, 0),
        "an invalid call, and no fixed disks"
    );
}

#[test]
fn with_no_diskette_the_bios_boots_the_fixed_disk() {
    let mut m = with_fixed_disk(|asm| {
        print(asm, b"Fixed");
        stop(asm);
    });
    run_until(&mut m, 12 * SECOND);
    let shown: String = (0..25).map(|r| row(&m, r)).collect();
    assert!(shown.contains("Fixed"), "{shown}");
    assert_eq!(m.board().peek(0x475), 1, "one fixed disk");
}

#[test]
fn int_13h_reads_a_fixed_disk_and_reports_its_geometry() {
    let mut m = with_fixed_disk(stop);
    run_until(&mut m, 12 * SECOND);
    let geometry = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0800);
        r.set16(2, 0x0080);
    });
    assert!(!carry(&geometry));
    assert_eq!(
        (
            geometry.get8(5),
            geometry.get8(1) & 0x3F,
            geometry.get8(6),
            geometry.get8(2)
        ),
        (62, 17, 1, 1)
    );
    let read = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0202);
        r.set16(1, 0x0305);
        r.set16(2, 0x0180);
        r.set16(3, 0x8000);
        r.set_sreg(crate::cpu::Seg::Es, 0);
    });
    assert!(!carry(&read), "status {:02X}", read.get8(4));
    // Cylinder 3, head 1, sectors 5 and 6: LBAs 7 × 17 + 4 and + 5.
    assert_eq!(
        [lba_at(&m, 0x8000), lba_at(&m, 0x8200)],
        [7 * 17 + 4, 7 * 17 + 5]
    );
    let past = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0201);
        r.set16(1, 0x0001);
        r.set16(2, 0x0580);
        r.set16(3, 0x8000);
    });
    assert_eq!(
        (carry(&past), past.get8(4)),
        (true, 0x01),
        "a head past the characteristics the BIOS gave is an illegal address"
    );
    let floppy = call(&mut m, 0x13, |r| r.set16(0, 0x0800));
    assert!(
        !carry(&floppy),
        "a diskette's call still reaches the diskette service"
    );
    assert_eq!(floppy.get8(3), 1, "the 360 KiB drive");
}

#[test]
fn the_cga_draws_text_in_graphics_and_reads_it_back() {
    let mut m = booted(Template::IbmXt, None);
    call(&mut m, 0x10, |r| r.set16(0, 0x0004));
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0941);
        r.set16(3, 0x0002);
        r.set16(1, 1);
    });
    let read = call(&mut m, 0x10, |r| r.set16(0, 0x0800));
    assert_eq!(read.get8(0), b'A');
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0C03);
        r.set16(1, 319);
        r.set16(2, 199);
    });
    let pixel = call(&mut m, 0x10, |r| {
        r.set16(0, 0x0D00);
        r.set16(1, 319);
        r.set16(2, 199);
    });
    assert_eq!(pixel.get8(0), 3);
}

#[test]
fn a_mode_set_reads_only_the_crt_controllers_values_through_int_1dh() {
    let mut m = booted(Template::IbmXt, None);
    let rom = 0xF_0000 + u32::from(layout::table::VIDEO);
    let mut copy = [0u8; 64];
    for (at, byte) in (rom..).zip(copy.iter_mut()) {
        *byte = m.board().peek(at);
    }
    poke(&mut m, 0x8000, &copy);
    poke(&mut m, 0x8040, &[0xFF; 24]);
    poke(&mut m, 0x1D * 4, &[0x00, 0x00, 0x00, 0x08]);
    call(&mut m, 0x10, |r| r.set16(0, 0x0003));
    let peek16 =
        |m: &Machine, at: u32| u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)]);
    assert_eq!(peek16(&m, 0x44A), 80, "the mode's columns");
    assert_eq!(peek16(&m, 0x44C), 0x1000, "its page's length");
    assert_eq!(m.board().peek(0x465), 0x29, "its mode register");
}

#[test]
fn the_teletype_scrolls_at_the_bottom_with_the_cursors_attribute() {
    let mut m = booted(Template::IbmXt, None);
    call(&mut m, 0x10, |r| r.set16(0, 0x0003));
    call(&mut m, 0x10, |r| {
        r.set16(0, 0x0200);
        r.set16(2, 0x1800);
    });
    call(&mut m, 0x10, |r| r.set16(0, 0x0E58));
    call(&mut m, 0x10, |r| r.set16(0, 0x0E0A));
    assert_eq!(&row(&m, 23)[..1], "X", "the line moved up");
    assert_eq!(&row(&m, 24)[..1], " ");
}

#[test]
fn int_13h_formats_a_diskette_track() {
    let mut m = booted(Template::IbmXt, Some(marked(360)));
    let ids: alloc::vec::Vec<u8> = (1..=9).flat_map(|sector| [1, 0, sector, 2]).collect();
    poke(&mut m, 0x5000, &ids);
    let formatted = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0509);
        r.set16(1, 0x0100);
        r.set16(2, 0x0000);
        r.set_sreg(crate::cpu::Seg::Es, 0);
        r.set16(3, 0x5000);
    });
    assert!(!carry(&formatted), "status {:02X}", formatted.get8(4));
    let read = |m: &mut Machine, cx: u16, dx: u16| {
        let regs = call(m, 0x13, |r| {
            r.set16(0, 0x0201);
            r.set16(1, cx);
            r.set16(2, dx);
            r.set_sreg(crate::cpu::Seg::Es, 0);
            r.set16(3, 0x6000);
        });
        assert!(!carry(&regs), "status {:02X}", regs.get8(4));
    };
    read(&mut m, 0x0105, 0x0000);
    assert!(
        (0..512).all(|i| m.board().peek(0x6000 + i) == 0xF6),
        "the parameter table's fill byte"
    );
    read(&mut m, 0x0101, 0x0100);
    assert_eq!(lba_at(&m, 0x6000), 27, "the other side kept its data");
}

#[test]
fn int_13h_sets_a_media_type_for_a_disk_the_drive_holds() {
    let mut m = booted(Template::TurboXt, Some(marked(1440)));
    let set = |m: &mut Machine, dx: u16, cx: u16| {
        call(m, 0x13, |r| {
            r.set16(0, 0x1800);
            r.set16(1, cx);
            r.set16(2, dx);
        })
    };
    let held = set(&mut m, 0x0000, 0x4F12);
    assert!(!carry(&held), "status {:02X}", held.get8(4));
    let table = (u32::from(held.sreg(crate::cpu::Seg::Es)) << 4) + u32::from(held.get16(7));
    assert_eq!(m.board().peek(table + 4), 18, "the medium's parameters");
    let empty = set(&mut m, 0x0001, 0x4F0F);
    assert!(carry(&empty));
    assert_eq!(empty.get8(4), 0x80, "the 1.2 MiB drive holds no disk");
    let unknown = set(&mut m, 0x0000, 0x4F0A);
    assert!(carry(&unknown));
    assert_eq!(unknown.get8(4), 0x0C, "no medium of ten sectors a track");
    let mut xt = booted(Template::IbmXt, None);
    xt.board_mut().eject_floppy(0);
    let unseen = set(&mut xt, 0x0000, 0x2709);
    assert!(
        !carry(&unseen),
        "a drive without a change line cannot say it is empty"
    );
}

#[test]
fn int_13h_reports_the_xts_diskette_drive() {
    let mut m = booted(Template::IbmXt, Some(marked(360)));
    let drive = call(&mut m, 0x13, |r| {
        r.set16(0, 0x0800);
        r.set16(2, 0x0000);
    });
    assert!(!carry(&drive));
    assert_eq!(
        (
            drive.get8(3),
            drive.get8(5),
            drive.get8(1),
            drive.get8(6),
            drive.get8(2)
        ),
        (1, 39, 9, 1, 1),
        "a 360 KiB drive: 40 cylinders of 9 sectors, two heads, one drive"
    );
    let table = (u32::from(drive.sreg(crate::cpu::Seg::Es)) << 4) + u32::from(drive.get16(7));
    assert_eq!(m.board().peek(table + 4), 9, "its media's parameters");
}

#[test]
fn the_bell_sounds_for_half_a_second() {
    let mut m = booted(Template::IbmXt, None);
    let end = start(
        &mut m,
        |asm| {
            asm.sti();
            asm.int(0x10);
        },
        |r| {
            r.set16(0, 0x0E07);
            r.set16(3, 0x0000);
        },
    );
    let begun = m.now();
    run_until(&mut m, begun + SECOND / 5);
    let at = m.board().cycles_at(m.now());
    assert_eq!(
        m.board_mut().input(0x61, at) & 0x03,
        0x03,
        "the speaker sounds"
    );
    while !stopped_at(&m, 0, end.clone()) {
        let now = m.now();
        run_until(&mut m, now + SECOND / 100);
        assert!(m.now() - begun < SECOND, "the bell ended");
    }
    let tick = SECOND * 10 / 182;
    let took = m.now() - begun;
    assert!((8 * tick..=10 * tick).contains(&took), "{took} ns");
    let at = m.board().cycles_at(m.now());
    assert_eq!(m.board_mut().input(0x61, at) & 0x03, 0, "and stops");
}
