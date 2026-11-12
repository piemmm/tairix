use alloc::vec::Vec;

use super::{input, output, register, result, status, Fdc, PhysicalDrive};
use crate::media::disc::{Disc, ImageFormat, SECTOR_BYTES, TRACK_BYTES};
use crate::snapshot::{self, Reader, Writer};

/// Cycles per byte under the head, and per revolution.
const BYTE: u64 = 128;
const REVOLUTION: u64 = 3125 * BYTE;
/// The DFS's timings: 24 ms steps, 20 ms settling, a 64 ms head load and
/// twelve revolutions before the idle head unloads.
const STEP: u64 = 48_000;
const SETTLE: u64 = 40_000;
const LOAD: u64 = 128_000;
/// The chip's command entry time.
const ENTRY: u64 = 400;

/// Every byte of a test disc says where it is.
fn pattern(track: usize, sector: usize, offset: usize) -> u8 {
    u8::try_from((track * 37 + sector * 11 + offset * 7) % 251).unwrap()
}

fn numbered_disc() -> Disc {
    let image: Vec<u8> = (0..80 * TRACK_BYTES)
        .map(|i| {
            pattern(
                i / TRACK_BYTES,
                i % TRACK_BYTES / SECTOR_BYTES,
                i % SECTOR_BYTES,
            )
        })
        .collect();
    Disc::decode(&image, ImageFormat::Ssd, false).unwrap()
}

fn sector_bytes(track: usize, sector: usize) -> Vec<u8> {
    (0..SECTOR_BYTES)
        .map(|offset| pattern(track, sector, offset))
        .collect()
}

/// The controller through a snapshot and back, as a machine restored at
/// `now` holds it.
fn round_trip(fdc: &Fdc, now: u64) -> Fdc {
    let mut w = Writer::new();
    w.put(fdc);
    let body = snapshot::open(&w.finish().unwrap()).unwrap();
    let mut r = Reader::new(&body);
    r.set_clock(now).unwrap();
    let restored: Fdc = r
        .get()
        .unwrap_or_else(|error| panic!("refused at {now}: {error:?}: {fdc:?}"));
    assert_eq!(&restored, fdc);
    restored
}

/// What a finished disc command left.
#[derive(Debug, PartialEq, Eq)]
struct Finished {
    result: u8,
    bytes: Vec<u8>,
    /// When INT rose with the result.
    at: u64,
}

/// A host answering the controller as the DFS does: every byte INT offers is
/// taken `latency` cycles later, and the result read when it arrives.
struct Host {
    fdc: Fdc,
    now: u64,
    latency: u64,
    /// Data requests answered before the host stops answering.
    answers: usize,
    /// Put the controller through a snapshot at every step of a command.
    snapshots: bool,
}

impl Host {
    fn new(disc: Option<Disc>) -> Self {
        let mut fdc = Fdc::new();
        fdc.set_disc(0, PhysicalDrive::Zero, disc);
        let mut host = Self::bare(fdc);
        host.immediate(0x35, &[0x0D, 0x0C, 0x0A, 0xC8]);
        host.immediate(0x35, &[0x10, 0xFF, 0xFF, 0x00]);
        host.immediate(0x35, &[0x18, 0xFF, 0xFF, 0x00]);
        host.immediate(0x3A, &[register::MODE, 0xC1]);
        host
    }

    const fn bare(fdc: Fdc) -> Self {
        Self {
            fdc,
            now: 0,
            latency: 40,
            answers: usize::MAX,
            snapshots: false,
        }
    }

    fn with_disc() -> Self {
        Self::new(Some(numbered_disc()))
    }

    fn status(&mut self) -> u8 {
        self.fdc.read(self.now, 0).unwrap()
    }

    fn issue(&mut self, command: u8, parameters: &[u8]) {
        self.fdc.write(self.now, 0, command);
        for &parameter in parameters {
            self.now += 16;
            self.fdc.write(self.now, 1, parameter);
        }
    }

    /// Run a command that does not touch the disc: busy clears with no INT,
    /// leaving a result only if it reads something.
    fn immediate(&mut self, command: u8, parameters: &[u8]) -> Option<u8> {
        self.issue(command, parameters);
        loop {
            self.now += 8;
            let status = self.status();
            assert_eq!(status & status::INT, 0, "command {command:#04x} raised INT");
            if status & status::BUSY == 0 {
                return (status & status::RESULT_FULL != 0)
                    .then(|| self.fdc.read(self.now, 1).unwrap());
            }
        }
    }

    /// Run a disc command, taking or supplying each data byte, to its result.
    fn run(&mut self, command: u8, parameters: &[u8], supply: &[u8]) -> Finished {
        self.issue(command, parameters);
        let mut bytes = Vec::new();
        let mut supply = supply.iter().copied();
        let mut answered = 0;
        loop {
            if self.snapshots {
                self.fdc = round_trip(&self.fdc, self.now);
            }
            let status = self.status();
            let waiting = status & status::DATA_REQUEST != 0 && answered == self.answers;
            if !self.fdc.int() || waiting {
                let next = self.fdc.next_event().expect("a running command acts again");
                self.now = self.now.max(next);
                self.fdc.advance(self.now);
                continue;
            }
            if status & status::DATA_REQUEST != 0 {
                answered += 1;
                self.now += self.latency;
                match supply.next() {
                    Some(byte) => self.fdc.write(self.now, 4, byte),
                    None => bytes.push(self.fdc.read(self.now, 4).unwrap()),
                }
            } else if status & status::RESULT_FULL != 0 {
                let at = self.now;
                let result = self.fdc.read(self.now, 1).unwrap();
                assert_eq!(self.status() & (status::BUSY | status::INT), 0);
                return Finished { result, bytes, at };
            }
        }
    }

    fn special(&mut self, register: u8) -> u8 {
        self.immediate(0x7D, &[register]).unwrap()
    }

    fn head(&self) -> u8 {
        self.fdc.drive(PhysicalDrive::Zero).head_track()
    }

    fn sector(&mut self, track: u8, sector: u8) -> Vec<u8> {
        self.fdc
            .disc_mut(PhysicalDrive::Zero)
            .unwrap()
            .track(0, usize::from(track))
            .unwrap()
            .sectors[usize::from(sector)]
        .data
        .clone()
    }
}

#[test]
fn immediate_commands_raise_no_interrupt_and_only_the_reads_leave_a_result() {
    // A result and INT from SPECIFY or a register write sent the DFS's NMI
    // handler round with nothing to do, and the DFS lost its place.
    let mut host = Host::with_disc();
    assert_eq!(host.immediate(0x35, &[0x0D, 0x0C, 0x0A, 0xC8]), None);
    assert_eq!(host.immediate(0x3A, &[register::DRIVE_OUTPUT, 0x48]), None);
    assert_eq!(host.immediate(0x7D, &[register::DRIVE_OUTPUT]), Some(0x48));
    assert!(host.immediate(0x6C, &[]).is_some());
    assert!(!host.fdc.int());
}

#[test]
fn specify_writes_three_consecutive_special_registers() {
    let mut host = Host::with_disc();
    host.immediate(0x35, &[register::STEP_RATE, 0x11, 0x22, 0x33]);
    let set =
        [register::STEP_RATE, register::SETTLE, register::HEAD_LOAD].map(|at| host.special(at));
    assert_eq!(set, [0x11, 0x22, 0x33]);
    // The last value written passes through the scan sector register.
    assert_eq!(host.special(register::SCAN_SECTOR), 0x33);
}

#[test]
fn drive_status_reports_track_0_the_ready_lines_and_the_index_hole() {
    let mut host = Host::with_disc();
    host.now = 3 * REVOLUTION;
    let at_index = host.immediate(0x6C, &[]).unwrap();
    assert_eq!(at_index, input::TRACK_0 | input::READY_0 | input::INDEX);
    host.now = 3 * REVOLUTION + REVOLUTION / 2;
    assert_eq!(
        host.immediate(0x6C, &[]).unwrap(),
        input::TRACK_0 | input::READY_0
    );

    let mut empty = Host::new(None);
    empty.now = REVOLUTION / 2;
    // An empty drive is not ready, and its index sensor sees light.
    assert_eq!(
        empty.immediate(0x6C, &[]).unwrap(),
        input::TRACK_0 | input::INDEX
    );
}

#[test]
fn a_disc_taken_out_leaves_the_drive_not_ready_until_its_status_is_read_twice() {
    let mut host = Host::with_disc();
    let disc = host.fdc.set_disc(host.now, PhysicalDrive::Zero, None);
    host.fdc.set_disc(host.now, PhysicalDrive::Zero, disc);
    assert_eq!(host.run(0x53, &[0, 0, 0x21], &[]).result, result::NOT_READY);
    // The first read reports the latched state and clears it.
    assert_eq!(host.immediate(0x6C, &[]).unwrap() & input::READY_0, 0);
    assert_eq!(
        host.immediate(0x6C, &[]).unwrap() & input::READY_0,
        input::READY_0
    );
    assert_eq!(host.run(0x53, &[0, 0, 0x21], &[]).result, result::OK);
}

#[test]
fn a_seek_steps_at_the_programmed_rate_settles_and_interrupts() {
    let mut host = Host::with_disc();
    host.immediate(0x3A, &[register::DRIVE_OUTPUT, 0x48]);
    let start = host.now;
    let seek = host.run(0x69, &[5], &[]);
    assert_eq!(seek.result, result::OK);
    assert_eq!(host.head(), 5);
    assert_eq!(host.special(register::TRACK_0), 5);
    // Each step one step-time after the last, then the settling time.
    let issued = start + 16;
    assert_eq!(seek.at, issued + ENTRY + 5 * STEP + SETTLE);
}

#[test]
fn a_seek_to_track_0_steps_out_to_the_sensor_whatever_the_track_register_says() {
    let mut host = Host::with_disc();
    host.run(0x69, &[9], &[]);
    host.immediate(0x35, &[0x10, 0xFF, 0xFF, 0x00]);
    assert_eq!(host.run(0x69, &[0], &[]).result, result::OK);
    assert_eq!(host.head(), 0);
}

#[test]
fn the_chip_steps_by_its_track_register_not_by_where_the_head_is() {
    let mut host = Host::with_disc();
    host.run(0x69, &[3], &[]);
    // Told the head is at track 0 when it is at 3, a seek to 5 lands on 8.
    host.immediate(0x35, &[0x10, 0xFF, 0xFF, 0x00]);
    host.run(0x69, &[5], &[]);
    assert_eq!(host.head(), 8);
}

#[test]
fn an_id_field_naming_another_track_makes_the_chip_step_in_and_look_again() {
    let mut host = Host::with_disc();
    host.run(0x69, &[4], &[]);
    // The register says 5 with the head at 4: reading track 5 meets track
    // 4's IDs, steps once, and finds the sector.
    host.immediate(0x35, &[0x10, 0xFF, 0xFF, 0x05]);
    let read = host.run(0x53, &[5, 2, 0x21], &[]);
    assert_eq!(read.result, result::OK);
    assert_eq!(read.bytes, sector_bytes(5, 2));
    assert_eq!((host.head(), host.special(register::TRACK_0)), (5, 6));
}

#[test]
fn after_two_steps_without_its_track_a_read_gives_up() {
    let mut host = Host::with_disc();
    host.run(0x69, &[10], &[]);
    host.immediate(0x35, &[0x10, 0xFF, 0xFF, 0x00]);
    let read = host.run(0x53, &[20, 0, 0x21], &[]);
    assert_eq!(read.result, result::SECTOR_NOT_FOUND);
    // The seek counted 20 from 0; the two retries stepped twice more.
    assert_eq!(host.head(), 32);
}

#[test]
fn reading_moves_each_byte_through_int_and_ends_with_the_result() {
    let mut host = Host::with_disc();
    let read = host.run(0x53, &[7, 3, 0x23], &[]);
    assert_eq!(read.result, result::OK);
    let expected: Vec<u8> = (3..6).flat_map(|sector| sector_bytes(7, sector)).collect();
    assert_eq!(read.bytes, expected);
    assert_eq!(host.special(register::SCAN_SECTOR), 5);
}

#[test]
fn the_128_byte_single_record_command_reads_one_128_byte_sector() {
    let mut host = Host::with_disc();
    let ids: Vec<u8> = (0..16).flat_map(|sector| [2, 0, sector, 0]).collect();
    assert_eq!(
        host.run(0x63, &[2, 0x0B, 0x10, 0x00, 0x10], &ids).result,
        result::OK
    );
    let read = host.run(0x52, &[2, 5], &[]);
    assert_eq!((read.result, read.bytes.len()), (result::OK, 128));
    assert!(read.bytes.iter().all(|&byte| byte == 0xE5));
}

#[test]
fn a_byte_not_taken_before_the_next_ends_the_read_with_late_dma() {
    let mut host = Host::with_disc();
    host.latency = 200;
    let read = host.run(0x53, &[1, 0, 0x21], &[]);
    assert_eq!(read.result, result::LATE_DMA);
    assert_eq!(read.bytes.len(), 1);
}

fn delete(host: &mut Host, track: u8, sector: u8) {
    let track = host
        .fdc
        .disc_mut(PhysicalDrive::Zero)
        .unwrap()
        .track_mut(0, usize::from(track))
        .unwrap();
    track.sectors[usize::from(sector)].deleted = true;
}

#[test]
fn read_data_counts_a_deleted_sector_without_moving_it() {
    let mut host = Host::with_disc();
    delete(&mut host, 4, 1);
    let read = host.run(0x53, &[4, 0, 0x23], &[]);
    assert_eq!(read.result, result::DELETED);
    let expected: Vec<u8> = [0, 2]
        .into_iter()
        .flat_map(|sector| sector_bytes(4, sector))
        .collect();
    assert_eq!(read.bytes, expected);
}

#[test]
fn read_data_and_deleted_data_moves_a_deleted_sector_and_says_so() {
    let mut host = Host::with_disc();
    delete(&mut host, 4, 1);
    let read = host.run(0x57, &[4, 0, 0x23], &[]);
    assert_eq!(read.result, result::DELETED);
    assert_eq!(read.bytes.len(), 3 * SECTOR_BYTES);
}

#[test]
fn verify_checks_the_sectors_without_moving_them() {
    let mut host = Host::with_disc();
    let verify = host.run(0x5F, &[9, 0, 0x2A], &[]);
    assert_eq!((verify.result, verify.bytes.len()), (result::OK, 0));
}

#[test]
fn a_sector_not_on_the_track_is_given_up_at_the_second_index_pulse() {
    let mut host = Host::with_disc();
    host.run(0x53, &[0, 0, 0x21], &[]);
    let searching = host.now + 2 * 16 + ENTRY;
    let read = host.run(0x53, &[0, 12, 0x21], &[]);
    assert_eq!(read.result, result::SECTOR_NOT_FOUND);
    assert_eq!(read.at % REVOLUTION, 0);
    assert!((REVOLUTION..=2 * REVOLUTION).contains(&(read.at - searching)));
}

#[test]
fn a_read_at_the_wrong_length_fails_its_crc() {
    let mut host = Host::with_disc();
    let short = host.run(0x53, &[6, 0, 0x01], &[]);
    assert_eq!(short.result, result::DATA_CRC_ERROR);
    assert_eq!(short.bytes[..], sector_bytes(6, 0)[..128]);
    // Read long, the bytes run on into the recorded CRC and the gap.
    let long = host.run(0x53, &[6, 0, 0x41], &[]);
    assert_eq!(long.result, result::DATA_CRC_ERROR);
    assert_eq!(long.bytes[..256], sector_bytes(6, 0)[..]);
    assert_eq!(long.bytes[258..], [0xFF; 254]);
}

#[test]
fn a_write_replaces_the_sector_and_reads_back() {
    let mut host = Host::with_disc();
    let data: Vec<u8> = (0..=255).collect();
    assert_eq!(host.run(0x4B, &[12, 4, 0x21], &data).result, result::OK);
    assert_eq!(host.sector(12, 4), data);
    assert!(host.fdc.disc_mut(PhysicalDrive::Zero).unwrap().is_dirty());
    assert_eq!(host.run(0x53, &[12, 4, 0x21], &[]).bytes, data);
    assert_eq!(host.run(0x4F, &[12, 4, 0x21], &data).result, result::OK);
    assert_eq!(host.run(0x53, &[12, 4, 0x21], &[]).result, result::DELETED);
}

#[test]
fn a_write_the_host_falls_behind_on_leaves_the_sector_failing_its_crc() {
    let mut host = Host::with_disc();
    host.answers = 2;
    assert_eq!(
        host.run(0x4B, &[3, 3, 0x21], &[0x55; 256]).result,
        result::LATE_DMA
    );
    let mut expected = sector_bytes(3, 3);
    expected[..2].fill(0x55);
    assert_eq!(host.sector(3, 3), expected);
    host.answers = usize::MAX;
    assert_eq!(
        host.run(0x53, &[3, 3, 0x21], &[]).result,
        result::DATA_CRC_ERROR
    );
}

#[test]
fn writes_to_a_protected_disc_are_refused() {
    let image = numbered_disc().encode().image;
    let mut host = Host::new(Some(Disc::decode(&image, ImageFormat::Ssd, true).unwrap()));
    assert_eq!(
        host.run(0x4B, &[1, 1, 0x21], &[0; 256]).result,
        result::WRITE_PROTECTED
    );
    assert_eq!(
        host.run(0x63, &[1, 0x15, 0x2A, 0, 0x10], &[0; 40]).result,
        result::WRITE_PROTECTED
    );
    assert_eq!(host.sector(1, 1), sector_bytes(1, 1));
}

#[test]
fn read_id_starts_at_the_index_and_returns_the_ids_in_turn() {
    let mut host = Host::with_disc();
    let ids = host.run(0x5B, &[8, 0, 3], &[]);
    assert_eq!(ids.result, result::OK);
    assert_eq!(ids.bytes, [8, 0, 0, 1, 8, 0, 1, 1, 8, 0, 2, 1]);
}

#[test]
fn a_format_lays_down_the_supplied_ids_from_the_index_and_ends_at_the_next() {
    let mut host = Host::with_disc();
    let ids: Vec<u8> = (0..10).flat_map(|sector| [20, 0, sector, 1]).collect();
    let format = host.run(0x63, &[20, 0x15, 0x2A, 0x00, 0x10], &ids);
    assert_eq!(format.result, result::OK);
    assert_eq!(format.at % REVOLUTION, 0);
    let track = host
        .fdc
        .disc_mut(PhysicalDrive::Zero)
        .unwrap()
        .track(0, 20)
        .unwrap()
        .clone();
    let positions: Vec<u16> = track.sectors.iter().map(|sector| sector.position).collect();
    // The DFS's own gaps lay a track as an image decodes one.
    assert_eq!(
        positions,
        [22, 332, 642, 952, 1262, 1572, 1882, 2192, 2502, 2812]
    );
    assert!(track
        .sectors
        .iter()
        .all(|sector| sector.data == [0xE5; 256]));
    assert_eq!(host.run(0x53, &[20, 9, 0x21], &[]).bytes, [0xE5; 256]);
}

#[test]
fn a_format_running_past_the_index_writes_over_its_first_sectors() {
    let mut host = Host::with_disc();
    let ids: Vec<u8> = (0..11).flat_map(|sector| [30, 0, sector, 1]).collect();
    assert_eq!(
        host.run(0x63, &[30, 0x15, 0x2B, 0x00, 0x10], &ids).result,
        result::OK
    );
    let track = host
        .fdc
        .disc_mut(PhysicalDrive::Zero)
        .unwrap()
        .track(0, 30)
        .unwrap()
        .clone();
    let numbers: Vec<u8> = track
        .sectors
        .iter()
        .map(|sector| sector.id.sector)
        .collect();
    assert_eq!(numbers, (1..11).collect::<Vec<u8>>());
}

#[test]
fn the_idle_head_unloads_and_the_drive_deselects_after_the_index_count() {
    let mut host = Host::with_disc();
    let read = host.run(0x53, &[0, 0, 0x21], &[]);
    assert!(host.fdc.head_loaded(PhysicalDrive::Zero));
    let unload = (read.at / REVOLUTION + 12) * REVOLUTION;
    host.fdc.advance(unload - 1);
    assert!(host.fdc.head_loaded(PhysicalDrive::Zero));
    host.fdc.advance(unload);
    assert!(!host.fdc.head_loaded(PhysicalDrive::Zero));

    let mut host = Host::with_disc();
    host.immediate(0x35, &[register::STEP_RATE, 0x0C, 0x0A, 0xF8]);
    host.run(0x53, &[0, 0, 0x21], &[]);
    host.fdc.advance(host.now + 100 * REVOLUTION);
    assert!(
        host.fdc.head_loaded(PhysicalDrive::Zero),
        "an index count of 15 keeps the head loaded"
    );
}

#[test]
fn a_read_with_the_head_unloaded_waits_for_it_to_load() {
    let mut host = Host::with_disc();
    let start = host.now;
    let read = host.run(0x53, &[0, 0, 0x21], &[]);
    assert!(read.at >= start + ENTRY + LOAD);
}

#[test]
fn a_command_for_another_drive_drops_the_head() {
    let mut host = Host::with_disc();
    host.run(0x53, &[0, 0, 0x21], &[]);
    assert!(host.fdc.head_loaded(PhysicalDrive::Zero));
    host.immediate(0xAC, &[]);
    assert!(!host.fdc.head_loaded(PhysicalDrive::Zero));
}

#[test]
fn a_command_with_no_drive_selected_finds_no_drive_ready() {
    let mut host = Host::with_disc();
    assert_eq!(host.run(0x13, &[0, 0, 0x21], &[]).result, result::NOT_READY);
}

#[test]
fn reset_abandons_the_command_drops_the_outputs_and_returns_to_dma_mode() {
    let mut host = Host::with_disc();
    host.issue(0x53, &[0, 0, 0x21]);
    host.fdc.advance(host.now + 3 * REVOLUTION / 2);
    host.fdc.write(host.now, 2, 1);
    assert_eq!(host.status(), 0);
    // Held in reset, the chip ignores commands.
    host.fdc.write(host.now, 0, 0x6C);
    assert_eq!(host.status(), 0);
    host.fdc.write(host.now, 2, 0);
    assert_eq!(host.special(register::DRIVE_OUTPUT), output::SELECT_0);
    assert_eq!(host.special(register::MODE), 0xC0);
}

#[test]
fn in_dma_mode_nothing_answers_the_data_requests_and_the_read_runs_late() {
    let mut host = Host::with_disc();
    host.immediate(0x3A, &[register::MODE, 0xC0]);
    let read = host.run(0x53, &[0, 0, 0x21], &[]);
    assert_eq!((read.result, read.bytes.len()), (result::LATE_DMA, 0));
}

#[test]
fn a_scan_ends_with_late_dma_for_want_of_the_key_dma_would_fetch() {
    let mut host = Host::with_disc();
    assert_eq!(
        host.run(0x40, &[0, 0, 0x21, 0x01, 0x04], &[]).result,
        result::LATE_DMA
    );
}

#[test]
fn seeks_step_past_the_surfaces_bad_tracks() {
    let mut host = Host::with_disc();
    host.immediate(0x35, &[0x10, 0x03, 0x05, 0x00]);
    host.run(0x69, &[3], &[]);
    assert_eq!(host.head(), 4);
    host.run(0x69, &[4], &[]);
    assert_eq!(host.head(), 6);
}

#[test]
fn the_drive_outputs_show_the_step_direction_and_the_low_write_current() {
    let mut host = Host::with_disc();
    host.run(0x69, &[50], &[]);
    let out = host.special(register::DRIVE_OUTPUT);
    assert_eq!(
        out & (output::DIRECTION | output::LOW_CURRENT),
        output::DIRECTION | output::LOW_CURRENT
    );
    host.run(0x69, &[10], &[]);
    assert_eq!(
        host.special(register::DRIVE_OUTPUT) & (output::DIRECTION | output::LOW_CURRENT),
        0
    );
}

#[test]
fn the_reset_register_address_is_not_readable() {
    let mut fdc = Fdc::new();
    assert_eq!(fdc.read(0, 2), None);
    assert_eq!(fdc.read(0, 3), None);
}

#[test]
fn taking_the_disc_out_mid_read_ends_it_not_ready() {
    let mut host = Host::with_disc();
    host.issue(0x53, &[0, 0, 0x29]);
    host.now += REVOLUTION / 2;
    let disc = host.fdc.set_disc(host.now, PhysicalDrive::Zero, None);
    assert!(disc.is_some());
    assert!(host.fdc.int());
    assert_eq!(host.fdc.read(host.now, 1), Some(result::NOT_READY));
}

#[test]
fn single_actuator_mode_keeps_one_track_register_for_both_drives() {
    let mut fdc = Fdc::new();
    fdc.set_disc(0, PhysicalDrive::One, Some(numbered_disc()));
    let mut host = Host::bare(fdc);
    host.immediate(0x3A, &[register::MODE, 0xC3]);
    host.immediate(0x35, &[register::STEP_RATE, 0x0C, 0x0A, 0xC8]);
    host.immediate(0x3A, &[register::TRACK_0, 7]);
    host.run(0xA9, &[9], &[]);
    assert_eq!(host.fdc.drive(PhysicalDrive::One).head_track(), 2);
    assert_eq!(host.special(register::TRACK_0), 9);
}

#[test]
fn an_unformatted_track_has_no_sector_and_no_id_to_find() {
    let mut fdc = Fdc::new();
    let mut disc = numbered_disc();
    disc.track_mut(0, 0).unwrap().sectors.clear();
    fdc.set_disc(0, PhysicalDrive::Zero, Some(disc));
    let mut host = Host::bare(fdc);
    host.immediate(0x35, &[register::STEP_RATE, 0x0C, 0x0A, 0xC8]);
    host.immediate(0x3A, &[register::MODE, 0xC1]);
    assert_eq!(
        host.run(0x53, &[0, 0, 0x21], &[]).result,
        result::SECTOR_NOT_FOUND
    );
    assert_eq!(
        host.run(0x5B, &[0, 0, 1], &[]).result,
        result::SECTOR_NOT_FOUND
    );
}

#[test]
fn a_programmed_gap_5_puts_an_index_mark_before_gap_1() {
    let mut host = Host::with_disc();
    let ids: Vec<u8> = (0..5).flat_map(|sector| [40, 0, sector, 1]).collect();
    assert_eq!(
        host.run(0x63, &[40, 0x15, 0x25, 0x20, 0x10], &ids).result,
        result::OK
    );
    let track = host
        .fdc
        .disc_mut(PhysicalDrive::Zero)
        .unwrap()
        .track(0, 40)
        .unwrap()
        .clone();
    // Gap 5 of 32 and its sync, the index mark, then gap 1 of 16 and its sync.
    assert_eq!(track.sectors[0].position, 32 + 6 + 1 + 16 + 6);
}

#[test]
fn every_kind_of_command_runs_the_same_through_a_snapshot_at_each_step() {
    let commands = |snapshots: bool| {
        let mut host = Host::with_disc();
        host.snapshots = snapshots;
        let data: Vec<u8> = (0..=255).rev().collect();
        let ids: Vec<u8> = (0..11).flat_map(|sector| [30, 0, sector, 1]).collect();
        let finished = [
            host.run(0x53, &[7, 3, 0x23], &[]),
            host.run(0x4B, &[12, 4, 0x21], &data),
            host.run(0x4F, &[12, 5, 0x21], &data),
            host.run(0x57, &[12, 4, 0x22], &[]),
            host.run(0x5B, &[8, 0, 3], &[]),
            host.run(0x5F, &[9, 0, 0x22], &[]),
            host.run(0x63, &[30, 0x15, 0x2B, 0x00, 0x10], &ids),
            host.run(0x53, &[40, 99, 0x21], &[]),
            host.run(0x69, &[2], &[]),
        ];
        host.now += 20 * REVOLUTION;
        host.fdc.advance(host.now);
        (finished, host.fdc)
    };
    let (plain, plain_fdc) = commands(false);
    let (through, through_fdc) = commands(true);
    assert_eq!(plain, through);
    assert_eq!(plain_fdc, through_fdc);
    assert!(plain
        .iter()
        .any(|finished| finished.result == result::SECTOR_NOT_FOUND));
}

/// The controller through a snapshot restored at `clock`, or its refusal.
fn restored_at(fdc: &Fdc, clock: u64) -> Result<Fdc, snapshot::SnapshotError> {
    let mut w = Writer::new();
    w.put(fdc);
    let body = snapshot::open(&w.finish().unwrap()).unwrap();
    let mut r = Reader::new(&body);
    r.set_clock(clock).unwrap();
    r.get()
}

/// The regression: a restored command's times were bounded only from above,
/// so one due long before the clock, or a search allowed to run past its two
/// revolutions, replayed every revolution in between.
#[test]
fn a_restored_command_due_before_the_clock_or_searching_too_long_is_refused() {
    let mut host = Host::with_disc();
    host.issue(0x53, &[7, 3, 0x23]);
    let due = host.fdc.op_due.expect("the command is under way");
    assert!(restored_at(&host.fdc, host.now).is_ok());
    assert_eq!(
        restored_at(&host.fdc, due + 1).err(),
        Some(snapshot::SnapshotError::Malformed),
        "an event already past"
    );
    let searching = |deadline: u64| {
        let mut fdc = host.fdc.clone();
        if let Some(op) = &mut fdc.operation {
            op.stage = super::Stage::Searching { deadline };
        }
        fdc
    };
    assert!(restored_at(
        &searching(due + super::SEARCH_INDEXES * REVOLUTION),
        host.now
    )
    .is_ok());
    assert!(restored_at(
        &searching(due + super::SEARCH_INDEXES * REVOLUTION + 1),
        host.now
    )
    .is_err());
    assert!(
        restored_at(&searching(due - 1), host.now).is_err(),
        "giving up before it began"
    );
}
