use super::*;
use crate::chips::dma::testing::{program, Flat};
use crate::storage::floppy::Floppy;
use crate::storage::imd::{tests::archive, Image};
use crate::storage::{MemoryDisk, SECTOR};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

/// A raw image of `kib` KiB whose every sector begins with its own LBA.
fn raw(kib: usize) -> Floppy {
    let mut bytes = vec![0u8; kib * 1024];
    for (lba, sector) in bytes.chunks_mut(SECTOR).enumerate() {
        sector[..2].copy_from_slice(&u16::try_from(lba).unwrap().to_le_bytes());
    }
    Floppy::raw(Box::new(MemoryDisk::new(bytes, false).unwrap())).unwrap()
}

/// Drive 0's motor on, DMA and interrupts enabled, out of reset.
const DOR_ON: u8 = 0x1C;
/// The PC BIOS's SPECIFY: 6 ms steps and a 4 ms head load at 250 kb/s.
const SPECIFY: [u8; 3] = [0x03, 0xDF, 0x02];
/// A 6 ms step, as [`SPECIFY`] sets it.
const STEP: Nanos = 6 * MS;
/// A revolution at 300 RPM.
const REVOLUTION: Nanos = 200 * MS;

/// A controller with one drive, the DMA controller and memory, at a time
/// that only moves forward.
struct Rig {
    fdc: Fdc,
    dma: Dma,
    memory: Flat,
    now: Nanos,
}

impl Rig {
    fn new(interface: Interface, kind: DriveKind) -> Self {
        Self {
            fdc: Fdc::new(interface, [Some(kind), None, None, None]),
            dma: Dma::new(),
            memory: Flat::new(),
            now: 0,
        }
    }

    /// A drive of `kind` holding `floppy`, its motor started at 0, the
    /// reset's statuses sensed and the controller specified.
    fn ready(interface: Interface, kind: DriveKind, floppy: Floppy) -> Self {
        let mut rig = Self::new(interface, kind);
        rig.fdc.insert(0, floppy, 0).unwrap();
        rig.out(0x3F2, DOR_ON);
        if interface == Interface::HighDensity {
            rig.out(0x3F7, 0x02);
        }
        rig.wait(5 * MS);
        for _ in 0..4 {
            rig.command(&[0x08]);
            rig.reply();
        }
        rig.command(&SPECIFY);
        rig
    }

    fn wait(&mut self, span: Nanos) {
        self.now += span;
    }

    fn wait_until(&mut self, t: Nanos) {
        self.now = self.now.max(t);
    }

    fn out(&mut self, port: u16, value: u8) {
        self.fdc
            .output(port, value, self.now, &mut self.dma, &mut self.memory);
    }

    fn input(&mut self, port: u16) -> Option<u8> {
        self.fdc
            .input(port, self.now, &mut self.dma, &mut self.memory)
    }

    fn status(&mut self) -> u8 {
        self.input(0x3F4).unwrap()
    }

    fn command(&mut self, bytes: &[u8]) {
        for &b in bytes {
            assert_eq!(self.status() & 0xC0, 0x80, "ready for a command byte");
            self.out(0x3F5, b);
        }
    }

    fn read_result(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while self.status() & 0xC0 == 0xC0 {
            out.push(self.input(0x3F5).unwrap());
        }
        out
    }

    /// The result of a command that moves no data.
    fn reply(&mut self) -> Vec<u8> {
        self.wait(COMMAND_TIME);
        self.read_result()
    }

    /// Runs to the controller's next event.
    fn step(&mut self) -> Nanos {
        let at = self.fdc.next_event().expect("something to be due");
        self.now = at;
        self.fdc.advance(at, &mut self.dma, &mut self.memory);
        at
    }

    /// Runs event by event until the result phase.
    fn finish(&mut self) -> Vec<u8> {
        loop {
            self.step();
            if self.status() & 0xF0 == 0xD0 {
                return self.read_result();
            }
        }
    }

    /// Runs until the controller interrupts, then senses why.
    fn sense_seek(&mut self) -> Vec<u8> {
        while !self.fdc.irq() {
            self.step();
        }
        self.command(&[0x08]);
        self.reply()
    }

    fn read(&mut self, c: u8, h: u8, r: u8, eot: u8) {
        self.command(&[0x46, h << 2, c, h, r, 2, eot, 0x2A, 0xFF]);
    }
}

/// Channel 2 set to move `bytes` from the controller to memory at
/// `address`.
fn to_memory(rig: &mut Rig, address: u16, bytes: u16) {
    program(&mut rig.dma, 2, 0x46, u32::from(address), bytes - 1);
}

fn from_memory(rig: &mut Rig, address: u16, bytes: u16) {
    program(&mut rig.dma, 2, 0x4A, u32::from(address), bytes - 1);
}

fn lba_at(memory: &Flat, address: usize) -> u16 {
    u16::from_le_bytes([memory.0[address], memory.0[address + 1]])
}

#[test]
fn a_reset_holds_the_controller_then_reports_every_unit_in_order() {
    let mut rig = Rig::new(Interface::Standard, DriveKind::Dd525);
    rig.out(0x3F2, 0x0C);
    rig.out(0x3F2, 0x08);
    assert_eq!(rig.status(), 0, "in reset the controller answers nothing");
    rig.wait(MS);
    rig.out(0x3F2, 0x0C);
    assert!(!rig.fdc.irq());
    assert_eq!(
        rig.fdc.next_event(),
        Some(MS + 2 * POLL_TIME),
        "the ready-line poll at 250 kb/s's 4 MHz clock"
    );
    rig.step();
    for unit in 0..4u8 {
        assert!(
            rig.fdc.irq(),
            "the interrupt holds while a unit's status waits"
        );
        rig.command(&[0x08]);
        assert_eq!(rig.reply(), [0xC0 | unit, 0]);
    }
    assert!(!rig.fdc.irq());
    rig.command(&[0x08]);
    assert_eq!(rig.reply(), [0x80], "with nothing to sense it is invalid");
}

#[test]
fn a_seek_end_takes_its_units_place_ahead_of_the_others() {
    let mut rig = Rig::new(Interface::Standard, DriveKind::Dd525);
    rig.fdc.insert(0, raw(360), 0).unwrap();
    rig.out(0x3F2, DOR_ON);
    rig.command(&SPECIFY);
    rig.wait(5 * MS);
    let start = rig.now;
    rig.command(&[0x0F, 0x00, 10]);
    while rig.fdc.next_event().is_some() {
        rig.step();
    }
    assert_eq!(rig.now, start + COMMAND_TIME + 10 * STEP, "ten steps");
    let mut sensed = Vec::new();
    for _ in 0..4 {
        rig.command(&[0x08]);
        sensed.push(rig.reply());
    }
    assert_eq!(
        sensed,
        [vec![0x20, 10], vec![0xC1, 0], vec![0xC2, 0], vec![0xC3, 0]]
    );
    assert!(!rig.fdc.irq());
}

#[test]
fn a_seek_past_the_drives_last_track_leaves_the_head_at_its_stop() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.command(&[0x0F, 0x00, 50]);
    assert_eq!(
        rig.sense_seek(),
        [0x20, 50],
        "the controller counts every pulse"
    );
    rig.command(&[0x0F, 0x00, 41]);
    assert_eq!(rig.sense_seek(), [0x20, 41]);
    rig.wait_until(SPIN_UP);
    rig.command(&[0x4A, 0x00]);
    let result = rig.finish();
    assert_eq!(
        (result[0] & 0xC0, result[3]),
        (0, 32),
        "nine steps back from the stop at 41"
    );
    rig.command(&[0x0F, 0x00, 48]);
    rig.sense_seek();
    rig.command(&[0x4A, 0x00]);
    let result = rig.finish();
    assert_eq!((result[0] & 0xC0, result[3]), (0, 39), "and seven in again");
}

#[test]
fn a_recalibrate_gives_up_after_77_steps_and_a_second_one_arrives() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd35, raw(720));
    rig.command(&[0x0F, 0x00, 79]);
    rig.sense_seek();
    let start = rig.now;
    rig.command(&[0x07, 0x00]);
    while !rig.fdc.irq() {
        rig.step();
    }
    assert_eq!(
        rig.now,
        start + COMMAND_TIME + 77 * STEP,
        "77 pulses without track 0"
    );
    rig.command(&[0x08]);
    assert_eq!(rig.reply(), [0x70, 0], "equipment check");
    rig.command(&[0x04, 0x00]);
    assert_eq!(rig.reply()[0] & 0x10, 0, "the head is two tracks short");
    rig.command(&[0x07, 0x00]);
    assert_eq!(rig.sense_seek(), [0x20, 0]);
}

#[test]
fn a_dma_read_moves_each_byte_as_it_passes_and_ends_at_terminal_count() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0x1000, 2 * 512);
    rig.read(0, 0, 3, 9);
    let mut previous: Option<Nanos> = None;
    let mut bytes = 0;
    let result = loop {
        let at = rig.step();
        let moved = rig.dma.take_transfers();
        assert!(moved <= 1, "one byte at a time");
        if moved == 1 {
            if let Some(gap) = previous.map(|p| at - p).filter(|&gap| gap < 100_000) {
                assert_eq!(gap, 32_000, "a byte every 32 µs at 250 kb/s");
            }
            previous = Some(at);
            bytes += 1;
        }
        if rig.status() & 0xF0 == 0xD0 {
            break rig.read_result();
        }
    };
    assert_eq!(bytes, 1024);
    assert_eq!(
        result,
        [0x00, 0x00, 0x00, 0, 0, 5, 2],
        "normal termination after sectors 3 and 4"
    );
    assert_eq!(lba_at(&rig.memory, 0x1000), 2);
    assert_eq!(lba_at(&rig.memory, 0x1200), 3);
}

#[test]
fn reading_past_the_last_sector_without_terminal_count_ends_the_cylinder() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0, 4 * 512);
    rig.read(0, 0, 8, 9);
    assert_eq!(rig.finish(), [0x40, 0x80, 0x00, 1, 0, 1, 2]);
    assert_eq!(lba_at(&rig.memory, 0x200), 8);
}

#[test]
fn a_multitrack_read_continues_on_the_second_side() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0x2000, 2 * 512);
    rig.command(&[0xC6, 0x00, 0, 0, 9, 2, 9, 0x2A, 0xFF]);
    assert_eq!(
        rig.finish(),
        [0x04, 0x00, 0x00, 0, 1, 2, 2],
        "ended on head 1 with sector 2 next"
    );
    assert_eq!(lba_at(&rig.memory, 0x2000), 8);
    assert_eq!(lba_at(&rig.memory, 0x2200), 9, "head 1's first sector");
}

#[test]
fn a_medium_reads_only_at_its_drives_rate() {
    let mut rig = Rig::ready(Interface::HighDensity, DriveKind::Hd525, raw(360));
    rig.wait_until(SPIN_UP);
    rig.out(0x3F7, 0x00);
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    let result = rig.finish();
    assert_eq!(
        (result[0] & 0xC0, result[1]),
        (0x40, 0x01),
        "500 kb/s cannot read double density"
    );
    rig.out(0x3F7, 0x01);
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    assert_eq!(
        rig.finish()[0] & 0xC0,
        0,
        "at 300 kb/s a 1.2 MiB drive reads it"
    );
}

#[test]
fn a_forty_track_disk_in_an_eighty_track_drive_sits_on_the_even_tracks() {
    let mut rig = Rig::ready(Interface::HighDensity, DriveKind::Hd525, raw(360));
    rig.out(0x3F7, 0x01);
    rig.command(&[0x0F, 0x00, 2]);
    rig.sense_seek();
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0x3000, 512);
    rig.read(1, 0, 1, 9);
    assert_eq!(rig.finish()[0] & 0xC0, 0);
    assert_eq!(
        lba_at(&rig.memory, 0x3000),
        18,
        "physical track 2 is the disk's cylinder 1"
    );
    rig.command(&[0x0F, 0x00, 3]);
    rig.sense_seek();
    rig.command(&[0x4A, 0x00]);
    assert_eq!(
        rig.finish()[1],
        0x01,
        "between the disk's tracks there is nothing"
    );
}

#[test]
fn a_forty_track_drive_cannot_read_an_eighty_track_disk() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(720));
    rig.wait_until(SPIN_UP);
    rig.command(&[0x4A, 0x00]);
    let result = rig.finish();
    assert_eq!((result[0] & 0xC0, result[1]), (0x40, 0x01));
}

#[test]
fn a_disk_coming_up_to_speed_reads_nothing() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    let result = rig.finish();
    assert_eq!((result[0] & 0xC0, result[1]), (0x40, 0x01));
    assert_eq!(
        rig.now,
        2 * REVOLUTION,
        "two index pulses passed while it spun up"
    );
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    assert_eq!(rig.finish()[0] & 0xC0, 0);
}

#[test]
fn a_stopped_motor_stalls_a_read_until_it_turns() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    rig.out(0x3F2, 0x0C);
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    rig.wait(5000 * MS);
    assert_eq!(rig.status() & 0xC0, 0, "no index pulses: the command waits");
    assert_eq!(rig.fdc.next_event(), None);
    let on = rig.now;
    rig.out(0x3F2, DOR_ON);
    assert_eq!(
        rig.finish()[1],
        0x01,
        "the search counts index pulses as the disk spins up"
    );
    assert_eq!(rig.now, on + 2 * REVOLUTION);
}

#[test]
fn an_archive_is_write_protected() {
    let data = [0x42u8; 512];
    let image = Image::parse(&archive(&[(5, 0, 0, 2, &[(1, 1, &data)])])).unwrap();
    let mut rig = Rig::ready(
        Interface::Standard,
        DriveKind::Dd525,
        Floppy::Archive(image),
    );
    rig.command(&[0x04, 0x00]);
    assert_eq!(rig.reply(), [0x78], "protected, ready, track 0, two-sided");
    rig.wait_until(SPIN_UP);
    from_memory(&mut rig, 0x4000, 512);
    rig.command(&[0x45, 0x00, 0, 0, 1, 2, 1, 0x2A, 0xFF]);
    assert!(!rig.fdc.irq());
    assert_eq!(&rig.reply()[..3], &[0x40, 0x02, 0x00], "refused at once");
}

#[test]
fn a_write_reads_back_and_terminal_count_pads_its_sector() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    rig.memory.0[0x5000..0x5000 + 600].fill(0xA7);
    from_memory(&mut rig, 0x5000, 600);
    rig.command(&[0x45, 0x00, 0, 0, 4, 2, 9, 0x2A, 0xFF]);
    assert_eq!(rig.finish(), [0x00, 0x00, 0x00, 0, 0, 6, 2]);
    to_memory(&mut rig, 0x6000, 1024);
    rig.read(0, 0, 4, 9);
    rig.finish();
    assert!(rig.memory.0[0x6000..0x6200 + 88].iter().all(|&b| b == 0xA7));
    assert!(
        rig.memory.0[0x6200 + 88..0x6400].iter().all(|&b| b == 0),
        "the rest of the sector was written as zeros"
    );
}

#[test]
fn a_raw_image_refuses_a_deleted_mark_and_a_foreign_format() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    from_memory(&mut rig, 0, 512);
    rig.command(&[0x49, 0x00, 0, 0, 1, 2, 9, 0x2A, 0xFF]);
    assert_eq!(&rig.reply()[..2], &[0x40, 0x02]);
    rig.command(&[0x4D, 0x00, 2, 8, 0x50, 0xF6]);
    assert_eq!(
        &rig.reply()[..2],
        &[0x40, 0x02],
        "eight sectors on a nine-sector image"
    );
}

fn format_ids(rig: &mut Rig, records: &[u8]) {
    for (i, &r) in records.iter().enumerate() {
        rig.memory.0[0x7000 + i * 4..0x7000 + i * 4 + 4].copy_from_slice(&[0, 0, r, 2]);
    }
    from_memory(rig, 0x7000, u16::try_from(records.len() * 4).unwrap());
}

#[test]
fn a_format_fills_every_sector_from_one_index_to_the_next() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    format_ids(&mut rig, &[1, 3, 5, 7, 9, 2, 4, 6, 8]);
    rig.command(&[0x4D, 0x00, 2, 9, 0x50, 0xF6]);
    assert_eq!(rig.finish()[0] & 0xC0, 0);
    assert_eq!(
        rig.now,
        4 * REVOLUTION,
        "the index after the head loaded, and the next"
    );
    to_memory(&mut rig, 0x8000, 9 * 512);
    rig.read(0, 0, 1, 9);
    rig.finish();
    assert!(rig.memory.0[0x8000..0x8000 + 9 * 512]
        .iter()
        .all(|&b| b == 0xF6));
}

#[test]
fn a_format_naming_a_sector_twice_leaves_the_track_alone() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    format_ids(&mut rig, &[1, 2, 3, 4, 5, 6, 7, 8, 8]);
    rig.command(&[0x4D, 0x00, 2, 9, 0x50, 0xF6]);
    assert_eq!(&rig.finish()[..2], &[0x40, 0x02]);
    to_memory(&mut rig, 0x8000, 512);
    rig.read(0, 0, 2, 9);
    rig.finish();
    assert_eq!(lba_at(&rig.memory, 0x8000), 1);
}

#[test]
fn read_id_reports_the_next_sector_under_the_head() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP + 30 * MS);
    let start = rig.now;
    rig.command(&[0x4A, 0x00]);
    let result = rig.finish();
    assert_eq!((result[0], result[1], result[3], result[6]), (0, 0, 0, 2));
    assert!((2..=9).contains(&result[5]), "a sector past the first");
    assert!(rig.now < start + REVOLUTION);
}

/// An archive's track 0: sector 1, a deleted sector 2, and sector 3.
fn marked() -> Floppy {
    let sectors: [(u8, u8, &[u8]); 3] = [(1, 2, &[0x11]), (2, 4, &[0x22]), (3, 2, &[0x33])];
    Floppy::Archive(Image::parse(&archive(&[(5, 0, 0, 2, &sectors)])).unwrap())
}

#[test]
fn a_deleted_mark_ends_a_read_on_its_sector_unless_skipped() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, marked());
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0, 3 * 512);
    rig.read(0, 0, 1, 3);
    assert_eq!(
        rig.finish(),
        [0x00, 0x00, 0x40, 0, 0, 2, 2],
        "read, marked, its address not advanced"
    );
    assert_eq!((rig.memory.0[0], rig.memory.0[0x200]), (0x11, 0x22));
    to_memory(&mut rig, 0x1000, 2 * 512);
    rig.command(&[0x66, 0x00, 0, 0, 1, 2, 3, 0x2A, 0xFF]);
    assert_eq!(
        rig.finish(),
        [0x00, 0x00, 0x40, 1, 0, 1, 2],
        "skipped, then terminal count on sector 3"
    );
    assert_eq!((rig.memory.0[0x1000], rig.memory.0[0x1200]), (0x11, 0x33));
}

#[test]
fn a_scan_hits_the_sector_that_matches_memory() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    rig.memory.0[..512].fill(0x11);
    rig.memory.0[512] = 1;
    rig.memory.0[700] = 0xFF;
    from_memory(&mut rig, 0, 1024);
    rig.command(&[0x51, 0x00, 0, 0, 1, 2, 9, 0x2A, 0x01]);
    assert_eq!(
        rig.finish(),
        [0x00, 0x00, 0x08, 0, 0, 3, 2],
        "an equal hit on sector 2"
    );
}

#[test]
fn a_read_track_reads_past_short_sectors_into_the_gap() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0, 1024);
    rig.command(&[0x42, 0x00, 0, 0, 1, 3, 9, 0x2A, 0xFF]);
    assert_eq!(
        &rig.finish()[..3],
        &[0x40, 0x20, 0x20],
        "N 3 over 512-byte sectors fails its CRC"
    );
    assert_eq!(lba_at(&rig.memory, 0), 0);
    assert!(rig.memory.0[512..1024].iter().all(|&b| b == GAP_FILLER));
}

#[test]
fn the_disk_change_line_clears_on_a_step_with_a_disk_in() {
    let mut rig = Rig::ready(Interface::HighDensity, DriveKind::Hd35, raw(1440));
    assert_eq!(
        rig.input(0x3F7),
        Some(0xFF),
        "changed, the other bits undriven"
    );
    rig.command(&[0x0F, 0x00, 1]);
    rig.sense_seek();
    assert_eq!(rig.input(0x3F7), Some(0x7F));
    rig.fdc.eject(0, rig.now);
    assert_eq!(rig.input(0x3F7), Some(0xFF));
    let mut plain = Rig::ready(Interface::HighDensity, DriveKind::Dd525, raw(360));
    assert_eq!(
        plain.input(0x3F7),
        Some(0x7F),
        "a 360 KiB drive has no change line"
    );
    let mut standard = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    assert_eq!(
        standard.input(0x3F7),
        None,
        "nothing answers 3F7h on a PC adapter"
    );
}

#[test]
fn the_digital_output_register_gates_the_interrupt_and_dma() {
    let mut rig = Rig::new(Interface::Standard, DriveKind::Dd525);
    rig.fdc.insert(0, raw(360), 0).unwrap();
    rig.out(0x3F2, 0x14);
    rig.wait(10 * MS);
    rig.status();
    assert!(!rig.fdc.irq(), "statuses wait, but nothing reaches IRQ 6");
    rig.out(0x3F2, DOR_ON);
    assert!(rig.fdc.irq());
    for _ in 0..4 {
        rig.command(&[0x08]);
        rig.reply();
    }
    rig.command(&SPECIFY);
    rig.out(0x3F2, 0x14);
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    assert_eq!(
        &rig.finish()[..2],
        &[0x40, 0x10],
        "no DMA request reached the 8237: overrun"
    );
}

#[test]
fn non_dma_mode_hands_the_host_each_byte_with_an_interrupt() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.command(&[0x03, 0xDF, 0x03]);
    rig.wait_until(SPIN_UP);
    rig.read(0, 0, 9, 9);
    let mut data = Vec::new();
    let result = loop {
        rig.step();
        match rig.status() & 0xF0 {
            0xF0 => {
                assert!(rig.fdc.irq(), "each byte interrupts");
                data.push(rig.input(0x3F5).unwrap());
                assert!(!rig.fdc.irq());
            }
            0xD0 => break rig.read_result(),
            _ => {}
        }
    };
    assert_eq!(data.len(), 512);
    assert_eq!(u16::from_le_bytes([data[0], data[1]]), 8);
    assert_eq!(
        result,
        [0x40, 0x80, 0x00, 1, 0, 1, 2],
        "no terminal count without DMA"
    );
}

#[test]
fn non_dma_mode_overruns_when_the_host_is_late() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.command(&[0x03, 0xDF, 0x03]);
    rig.wait_until(SPIN_UP);
    rig.read(0, 0, 1, 9);
    assert_eq!(&rig.finish()[..2], &[0x40, 0x10]);
}

#[test]
fn a_loaded_head_does_not_wait_to_load_again() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.command(&[0x03, 0xDF, 0xFE]);
    rig.wait_until(SPIN_UP);
    let start = rig.now;
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 1);
    rig.finish();
    let first = rig.now;
    assert!(first >= start + 127 * 4 * MS, "the head loaded first");
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 1);
    rig.finish();
    assert!(
        rig.now - first <= REVOLUTION + 20 * MS,
        "within a revolution"
    );
}

#[test]
fn a_reset_abandons_a_command() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd525, raw(360));
    rig.wait_until(SPIN_UP);
    to_memory(&mut rig, 0, 512);
    rig.read(0, 0, 1, 9);
    rig.wait(MS);
    rig.out(0x3F2, 0x18);
    rig.wait(MS);
    rig.out(0x3F2, DOR_ON);
    assert_eq!(rig.status(), 0x80);
    rig.wait(REVOLUTION);
    rig.status();
    assert_eq!(rig.dma.take_transfers(), 0);
}

#[test]
fn an_unknown_command_is_invalid() {
    let mut rig = Rig::ready(Interface::Standard, DriveKind::Dd35, raw(720));
    rig.command(&[0x10]);
    assert_eq!(rig.reply(), [0x80], "a µPD765A has no VERSION command");
}

#[test]
fn a_drive_that_is_not_fitted_takes_no_disk() {
    let mut fdc = Fdc::new(
        Interface::Standard,
        [Some(DriveKind::Dd525), None, None, None],
    );
    assert!(fdc.insert(1, raw(360), 0).is_err());
    assert!(fdc.insert(0, raw(360), 0).unwrap().is_none());
    assert!(
        fdc.insert(0, raw(360), 0).unwrap().is_some(),
        "a swap returns the old disk"
    );
}
