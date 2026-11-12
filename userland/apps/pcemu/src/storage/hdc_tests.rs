use super::*;
use crate::chips::dma::testing::{program, Flat};
use crate::storage::MemoryDisk;
use alloc::vec;

const KIND: DiskType = DiskType {
    cylinders: 64,
    heads: 2,
    track_to_track: 3 * MS,
    average_seek: 85 * MS,
};
/// A revolution at 3600 RPM.
const REVOLUTION: Nanos = 16_666_666;
/// The control byte the PC BIOS uses: 70 µs buffered steps.
const BUFFERED: u8 = 5;

fn rate() -> Hertz {
    crate::time::CpuClock::PC.divided::<5>()
}

/// An image whose every sector begins with its own LBA.
fn image(kind: DiskType, recording: Recording, read_only: bool) -> MemoryDisk {
    let sectors =
        usize::from(kind.cylinders) * usize::from(kind.heads) * usize::from(recording.sectors());
    let mut bytes = vec![0u8; sectors * SECTOR];
    for (lba, sector) in bytes.chunks_mut(SECTOR).enumerate() {
        sector[..4].copy_from_slice(&u32::try_from(lba).unwrap().to_le_bytes());
    }
    MemoryDisk::new(bytes, read_only).unwrap()
}

fn formatted(interleave: u8) -> LowLevel {
    LowLevel {
        interleave,
        bad_tracks: Vec::new(),
    }
}

struct Rig {
    hdc: Hdc,
    dma: Dma,
    memory: Flat,
    now: Nanos,
}

impl Rig {
    /// An adapter with drive 0 attached, DMA and interrupts enabled.
    fn new(interleave: u8) -> Self {
        let mut hdc = Hdc::new(Recording::Mfm, 0x0C, rate());
        hdc.attach(
            0,
            KIND,
            Box::new(image(KIND, Recording::Mfm, false)),
            formatted(interleave),
        )
        .unwrap();
        let mut rig = Self {
            hdc,
            dma: Dma::new(),
            memory: Flat::new(),
            now: 0,
        };
        rig.out(3, 0x03);
        rig
    }

    fn out(&mut self, offset: u16, value: u8) {
        self.hdc.output(
            0x320 + offset,
            value,
            self.now,
            &mut self.dma,
            &mut self.memory,
        );
    }

    fn input(&mut self, offset: u16) -> Option<u8> {
        self.hdc
            .input(0x320 + offset, self.now, &mut self.dma, &mut self.memory)
    }

    fn status(&mut self) -> u8 {
        self.input(1).unwrap()
    }

    fn command(&mut self, dcb: [u8; 6]) {
        self.out(2, 0);
        for byte in dcb {
            assert_eq!(
                self.status() & 0x0F,
                0x0D,
                "busy, asking for a command byte"
            );
            self.out(0, byte);
        }
    }

    /// Runs event by event until the completion status, and reads it.
    fn finish(&mut self) -> u8 {
        while self.status() & 0x0F != 0x0F {
            self.now = self.hdc.next_event().expect("the command to go on");
            self.hdc.advance(self.now, &mut self.dma, &mut self.memory);
        }
        self.input(0).unwrap()
    }

    /// Moves `bytes` to or from the host through the data port.
    fn pio_read(&mut self, bytes: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < bytes {
            if self.status() & 0x07 == 0x03 {
                out.push(self.input(0).unwrap());
            } else {
                self.now = self.hdc.next_event().expect("the data to come");
                self.hdc.advance(self.now, &mut self.dma, &mut self.memory);
            }
        }
        out
    }

    fn pio_write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            while self.status() & 0x07 != 0x01 {
                self.now = self.hdc.next_event().expect("room for the data");
                self.hdc.advance(self.now, &mut self.dma, &mut self.memory);
            }
            self.out(0, byte);
        }
    }

    /// Gives drive 0 the characteristics of [`KIND`], as a BIOS does.
    fn initialize(&mut self) {
        self.out(3, 0x02);
        self.command([0x0C, 0, 0, 0, 0, 0]);
        let [high, low] = KIND.cylinders.to_be_bytes();
        self.pio_write(&[high, low, KIND.heads, 0, 0, 0, 0, 11]);
        assert_eq!(self.finish(), 0x00);
        self.out(3, 0x03);
    }

    fn sense(&mut self, drive: u8) -> Vec<u8> {
        self.out(3, 0x02);
        self.command([0x03, drive << 5, 0, 0, 0, 0]);
        let sense = self.pio_read(4);
        assert_eq!(self.finish() & 0x02, 0);
        self.out(3, 0x03);
        sense
    }
}

/// A command block for `op` at an address.
fn dcb(op: u8, head: u8, cylinder: u16, sector: u8, count: u8, control: u8) -> [u8; 6] {
    let [high, low] = cylinder.to_be_bytes();
    [op, head, (high << 6) | sector, low, count, control]
}

fn lba_at(memory: &Flat, address: usize) -> u32 {
    u32::from_le_bytes(memory.0[address..address + 4].try_into().unwrap())
}

#[test]
fn a_command_goes_from_its_block_to_its_status() {
    let mut rig = Rig::new(3);
    assert_eq!(rig.status(), 0);
    rig.command([0x00, 0, 0, 0, 0, 0]);
    assert_eq!(rig.status(), 0x08, "busy on the command");
    assert!(!rig.hdc.irq());
    rig.now = rig.hdc.next_event().unwrap();
    assert_eq!(rig.now, COMMAND_TIME);
    assert_eq!(rig.status(), 0x2F, "the status byte, and the interrupt");
    assert!(rig.hdc.irq());
    assert_eq!(rig.input(0), Some(0x00));
    assert_eq!(rig.status(), 0);
    assert!(!rig.hdc.irq());
}

#[test]
fn a_missing_drive_is_not_ready() {
    let mut rig = Rig::new(3);
    rig.command([0x00, 0x20, 0, 0, 0, 0]);
    assert_eq!(rig.finish(), 0x22, "drive 1, with an error");
    assert_eq!(rig.sense(1), [0x04, 0x20, 0, 0]);
    assert_eq!(rig.sense(1), [0x00, 0x20, 0, 0], "sensing clears it");
}

#[test]
fn an_unknown_opcode_is_an_invalid_command() {
    let mut rig = Rig::new(3);
    rig.command([0x02, 0, 0, 0, 0, 0]);
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(rig.sense(0)[0], 0x20, "no address to report");
}

#[test]
fn a_dma_read_bursts_each_sector_at_the_transfer_rate() {
    let mut rig = Rig::new(3);
    program(&mut rig.dma, 3, 0x47, 0x1000, 2 * 512 - 1);
    rig.command(dcb(0x08, 0, 0, 0, 2, BUFFERED));
    let mut previous = None;
    let mut gaps = Vec::new();
    while rig.status() & 0x0F != 0x0F {
        rig.now = rig.hdc.next_event().unwrap();
        rig.hdc.advance(rig.now, &mut rig.dma, &mut rig.memory);
        if rig.dma.take_transfers() == 1 {
            if let Some(p) = previous {
                gaps.push(rig.now - p);
            }
            previous = Some(rig.now);
        }
    }
    assert!(rig.hdc.irq());
    assert_eq!(rig.input(0), Some(0x00));
    assert_eq!(
        (lba_at(&rig.memory, 0x1000), lba_at(&rig.memory, 0x1200)),
        (0, 1)
    );
    assert_eq!(gaps.len(), 1023);
    assert!(
        gaps.iter().filter(|&&gap| gap > 2000).count() == 1,
        "one wait between the sectors"
    );
    assert!(
        gaps.iter()
            .all(|&gap| gap > 2000 || (1047..=1048).contains(&gap)),
        "a transfer every five clocks"
    );
}

/// The time a whole-track read takes at `interleave`.
fn track_read(interleave: u8) -> Nanos {
    let mut rig = Rig::new(interleave);
    program(&mut rig.dma, 3, 0x47, 0, 17 * 512 - 1);
    rig.command(dcb(0x08, 0, 0, 0, 17, BUFFERED));
    rig.finish();
    rig.now
}

#[test]
fn too_tight_an_interleave_waits_a_revolution_for_each_sector() {
    let three = track_read(3);
    let two = track_read(2);
    let one = track_read(1);
    assert!(
        three < 4 * REVOLUTION,
        "three revolutions read the track: {three}"
    );
    assert!(
        two > 16 * REVOLUTION,
        "the controller misses the sector two slots on: {two}"
    );
    assert!(one > 16 * REVOLUTION, "and the next one: {one}");
}

#[test]
fn a_write_reads_back() {
    let mut rig = Rig::new(3);
    rig.memory.0[0x4000..0x4400].fill(0x5A);
    program(&mut rig.dma, 3, 0x4B, 0x4000, 1024 - 1);
    rig.command(dcb(0x0A, 1, 9, 4, 2, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    program(&mut rig.dma, 3, 0x47, 0x6000, 1024 - 1);
    rig.command(dcb(0x08, 1, 9, 4, 2, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert!(rig.memory.0[0x6000..0x6400].iter().all(|&b| b == 0x5A));
}

#[test]
fn a_transfer_runs_on_across_heads_and_cylinders() {
    let mut rig = Rig::new(3);
    rig.initialize();
    program(&mut rig.dma, 3, 0x47, 0, 2 * 512 - 1);
    rig.command(dcb(0x08, 1, 0, 16, 2, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert_eq!(
        (lba_at(&rig.memory, 0), lba_at(&rig.memory, 0x200)),
        (33, 34)
    );
}

#[test]
fn unbuffered_steps_take_three_milliseconds_a_track() {
    let mut rig = Rig::new(3);
    rig.command(dcb(0x0B, 0, 63, 0, 0, 0));
    rig.finish();
    assert_eq!(rig.now, COMMAND_TIME + 63 * 3 * MS);
    let start = rig.now;
    rig.command(dcb(0x0B, 0, 0, 0, 0, BUFFERED));
    rig.finish();
    let buffered = rig.now - start - COMMAND_TIME;
    assert!(
        buffered < 63 * 3 * MS && buffered > KIND.average_seek,
        "the drive's own pace across the disk: {buffered}"
    );
}

#[test]
fn the_characteristics_bound_the_addresses() {
    let mut rig = Rig::new(3);
    rig.out(3, 0x02);
    rig.command([0x0C, 0, 0, 0, 0, 0]);
    rig.pio_write(&[0x00, 0x20, 2, 0, 0, 0, 0, 11]);
    assert_eq!(rig.finish(), 0x00);
    rig.command(dcb(0x08, 0, 32, 0, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(
        rig.sense(0),
        [0xA1, 0x00, 0x00, 32],
        "an illegal address, and where"
    );
}

#[test]
fn past_the_drive_a_seek_misses_and_a_head_reads_nothing() {
    let mut rig = Rig::new(3);
    program(&mut rig.dma, 3, 0x47, 0, 512 - 1);
    rig.command(dcb(0x08, 0, 100, 0, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(rig.sense(0)[0], 0x95, "the heads stopped at cylinder 63");
    rig.command(dcb(0x08, 5, 1, 0, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(rig.sense(0)[..2], [0x92, 0x05]);
}

#[test]
fn a_format_writes_the_buffer_and_can_mark_a_track_bad() {
    let mut rig = Rig::new(3);
    rig.out(3, 0x02);
    rig.command([0x0F, 0, 0, 0, 0, 0]);
    rig.pio_write(&[0xC3; 512]);
    assert_eq!(rig.finish(), 0x00);
    rig.out(3, 0x03);
    rig.command(dcb(0x06, 1, 2, 0, 4, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert_eq!(rig.hdc.low_level(0).unwrap().interleave, 4);
    program(&mut rig.dma, 3, 0x47, 0, 512 - 1);
    rig.command(dcb(0x08, 1, 2, 7, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert!(rig.memory.0[..512].iter().all(|&b| b == 0xC3));
    rig.command(dcb(0x07, 1, 2, 0, 4, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert_eq!(rig.hdc.low_level(0).unwrap().bad_tracks, [2 * 2 + 1]);
    program(&mut rig.dma, 3, 0x47, 0, 512 - 1);
    rig.command(dcb(0x08, 1, 2, 7, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(rig.sense(0)[0], 0x99, "a bad track");
    rig.command(dcb(0x06, 1, 2, 0, 3, BUFFERED));
    assert_eq!(rig.finish(), 0x00);
    assert!(rig.hdc.low_level(0).unwrap().bad_tracks.is_empty());
}

#[test]
fn a_stalled_dma_channel_holds_the_transfer_until_it_answers() {
    let mut rig = Rig::new(3);
    program(&mut rig.dma, 3, 0x47, 0, 512 - 1);
    rig.dma.write_port(0x0A, 0x07);
    rig.command(dcb(0x08, 0, 0, 0, 1, BUFFERED));
    while rig.hdc.next_event().is_some() {
        rig.now = rig.hdc.next_event().unwrap();
        rig.hdc.advance(rig.now, &mut rig.dma, &mut rig.memory);
    }
    assert_eq!(rig.status(), 0x18, "busy, requesting DMA");
    rig.dma.write_port(0x0A, 0x03);
    rig.hdc.advance(rig.now, &mut rig.dma, &mut rig.memory);
    assert_eq!(rig.finish(), 0x00);
    assert_eq!(lba_at(&rig.memory, 0), 0);
}

#[test]
fn without_dma_the_host_takes_each_byte_through_the_data_port() {
    let mut rig = Rig::new(3);
    rig.out(3, 0x02);
    rig.command(dcb(0xE5, 0, 1, 3, 1, BUFFERED));
    let long = rig.pio_read(SECTOR + ECC);
    assert_eq!(rig.finish(), 0x00);
    assert_eq!(
        u32::from_le_bytes(long[..4].try_into().unwrap()),
        2 * 17 + 3
    );
    assert_eq!(&long[SECTOR..], &[0; ECC], "READ LONG's ECC is not kept");
}

#[test]
fn a_read_only_image_is_a_write_fault() {
    let mut hdc = Hdc::new(Recording::Mfm, 0, rate());
    hdc.attach(
        0,
        KIND,
        Box::new(image(KIND, Recording::Mfm, true)),
        formatted(3),
    )
    .unwrap();
    let mut rig = Rig {
        hdc,
        dma: Dma::new(),
        memory: Flat::new(),
        now: 0,
    };
    rig.out(3, 0x03);
    program(&mut rig.dma, 3, 0x4B, 0, 512 - 1);
    rig.command(dcb(0x0A, 0, 0, 0, 1, BUFFERED));
    assert_eq!(rig.finish(), 0x02);
    assert_eq!(rig.sense(0)[0], 0x83);
}

#[test]
fn a_reset_drops_the_command_and_its_interrupt() {
    let mut rig = Rig::new(3);
    rig.command([0x00, 0, 0, 0, 0, 0]);
    rig.now = rig.hdc.next_event().unwrap();
    rig.status();
    assert!(rig.hdc.irq());
    rig.out(1, 0);
    assert!(!rig.hdc.irq());
    assert_eq!(rig.status(), 0);
}

#[test]
fn the_jumpers_name_the_drive_types() {
    let mut rig = Rig::new(3);
    assert_eq!(rig.input(2), Some(0x0C));
    assert_eq!(rig.input(3), None, "the mask register is write-only");
}

#[test]
fn a_drive_attaches_only_as_its_image_and_geometry_allow() {
    let mut hdc = Hdc::new(Recording::Rll, 0, rate());
    assert_eq!(
        hdc.attach(
            0,
            KIND,
            Box::new(image(KIND, Recording::Mfm, false)),
            formatted(3)
        ),
        Err(AttachError::Size)
    );
    let huge = DiskType {
        cylinders: 2000,
        ..KIND
    };
    assert_eq!(
        hdc.attach(
            0,
            huge,
            Box::new(image(KIND, Recording::Rll, false)),
            formatted(3)
        ),
        Err(AttachError::Geometry)
    );
    let bad = LowLevel {
        interleave: 3,
        bad_tracks: vec![128],
    };
    assert_eq!(
        hdc.attach(0, KIND, Box::new(image(KIND, Recording::Rll, false)), bad),
        Err(AttachError::BadTrack)
    );
    assert_eq!(
        hdc.attach(
            2,
            KIND,
            Box::new(image(KIND, Recording::Rll, false)),
            formatted(3)
        ),
        Err(AttachError::NoSuchDrive)
    );
    assert_eq!(
        hdc.attach(
            1,
            KIND,
            Box::new(image(KIND, Recording::Rll, false)),
            formatted(3)
        ),
        Ok(())
    );
}

#[test]
fn the_interleave_lays_logical_sectors_apart() {
    assert_eq!(
        &interleaved(17, 3)[..17],
        &[0, 3, 6, 9, 12, 15, 1, 4, 7, 10, 13, 16, 2, 5, 8, 11, 14]
    );
    assert_eq!(&interleaved(17, 1)[..17], &(0..17).collect::<Vec<u8>>()[..]);
    assert_eq!(
        &interleaved(17, 17)[..17],
        &interleaved(17, 1)[..17],
        "a whole track's interleave is none"
    );
    let mut seen: Vec<u8> = interleaved(26, 4)[..26].to_vec();
    seen.sort_unstable();
    assert_eq!(seen, (0..26).collect::<Vec<u8>>(), "every slot once");
}
