use alloc::vec::Vec;

use super::*;
use crate::media::c1541::{d64, gcr};

/// A formatted disk, every sector of track 18 its sector number, the ID
/// `AB`.
fn disk() -> Surface {
    let mut image = alloc::vec![0; 174_848];
    let bam = (1..18u8)
        .map(|t| usize::from(d64::sectors(t)))
        .sum::<usize>()
        * 256;
    image[bam + 0xA2] = b'A';
    image[bam + 0xA3] = b'B';
    d64::decode(&image).expect("a D64")
}

fn reading(zone: u8, phase: u8) -> Controls {
    Controls {
        motor: true,
        phase,
        zone,
        reading: true,
        byte_ready: true,
        write_value: 0,
    }
}

/// The bytes BYTE READY announces over `cycles` drive cycles from `from`,
/// each with its cycle, and the cycle SYNC was first seen.
fn read(
    head: &mut Head,
    disk: &mut Option<Surface>,
    from: u64,
    cycles: u64,
) -> (Vec<(u64, u8)>, Option<u64>) {
    let mut bytes = Vec::new();
    let mut sync = None;
    for cycle in from..from + cycles {
        head.advance(cycle + 1, disk);
        if head.sync() && sync.is_none() {
            sync = Some(cycle);
        }
        if head.byte_ready(cycle) {
            bytes.push((cycle, head.latched()));
        }
    }
    (bytes, sync)
}

#[test]
fn the_first_bytes_after_sync_are_sector_0_s_header() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    let (bytes, sync) = read(&mut head, &mut disk, 0, 2_000);
    let sync = sync.expect("the ring starts with a SYNC");
    assert!(sync < 100);
    let after: Vec<u8> = bytes
        .into_iter()
        .filter(|&(cycle, _)| cycle > sync)
        .map(|(_, byte)| byte)
        .collect();
    let mut header = Vec::new();
    let checksum = 0x12 ^ b'A' ^ b'B';
    gcr::encode(
        &[0x08, checksum, 0, 18, b'B', b'A', 0x0F, 0x0F],
        &mut header,
    );
    assert_eq!(after[..header.len()], header[..]);
}

#[test]
fn bytes_come_at_the_zone_s_rate() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    let (bytes, _) = read(&mut head, &mut disk, 0, 200_000);
    // A revolution holds 7142 bytes. A SYNC's five are announced as one:
    // its first eight ones complete a byte before the tenth shows SYNC.
    let syncs = 19 * 2;
    assert_eq!(bytes.len(), 7142 - syncs * 5 + syncs);
}

#[test]
fn a_cell_lasts_four_counts_of_16_less_the_zone_s_two_bits() {
    assert_eq!(
        [0, 1, 2, 3].map(cell_cycles),
        [64, 60, 56, 52],
        "crystal cycles"
    );
    assert_eq!(cell_cycles(4), cell_cycles(0));
    assert_eq!(cell_cycles(u8::MAX), cell_cycles(3));
    assert_eq!(
        [0, 1, 2, 3].map(d64::revolution_bytes),
        [6250, 6666, 7142, 7692]
    );
}

#[test]
fn a_phase_a_step_round_moves_the_head_and_opposite_coils_do_not() {
    let mut head = Head::new();
    assert_eq!(head.half_track(), 34);
    head.set_controls(reading(2, 3));
    assert_eq!(head.half_track(), 35);
    head.set_controls(reading(2, 0));
    assert_eq!(head.half_track(), 36);
    head.set_controls(reading(2, 2));
    assert_eq!(head.half_track(), 36, "two steps round pull both ways");
    head.set_controls(reading(2, 3));
    assert_eq!(head.half_track(), 35);
    let mut stopped = reading(2, 2);
    stopped.motor = false;
    head.set_controls(stopped);
    assert_eq!(head.half_track(), 35, "the stepper needs the motor");
}

#[test]
fn a_track_with_nothing_recorded_never_shows_sync() {
    let mut disk = Some(Surface::blank());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    let (_, sync) = read(&mut head, &mut disk, 0, 100_000);
    assert_eq!(sync, None);
}

#[test]
fn writing_records_the_drive_s_bytes_at_the_zone_s_rate() {
    let mut disk = Some(Surface::blank());
    let mut head = Head::new();
    let mut controls = reading(2, 2);
    controls.reading = false;
    controls.write_value = 0xA5;
    head.set_controls(controls);
    for cycle in 0..40_000 {
        head.advance(cycle + 1, &mut disk);
    }
    let surface = disk.as_ref().expect("a disk");
    assert!(surface.is_dirty());
    let track = surface.track(34).expect("written");
    assert_eq!(track.bytes().len(), d64::revolution_bytes(2));
    let written = track
        .bytes()
        .iter()
        .fold(0, |count, &byte| count + usize::from(byte == 0xA5));
    // 40 ms is 1428 bytes at zone 2's rate, the first the shift register's
    // empty byte.
    assert_eq!(written, 1427);
}

#[test]
fn reading_resumes_after_a_write_without_a_stale_noise_deadline() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    let (_, sync) = read(&mut head, &mut disk, 0, 1_000);
    assert!(sync.is_some(), "the noise timer is armed by now");
    let mut writing = reading(2, 2);
    writing.reading = false;
    head.set_controls(writing);
    for cycle in 1_000..30_000 {
        head.advance(cycle + 1, &mut disk);
    }
    head.set_controls(reading(2, 2));
    let (bytes, _) = read(&mut head, &mut disk, 30_000, 10_000);
    assert!(!bytes.is_empty());
}

/// The bytes a test feeds the head, none repeating soon.
fn fed(n: usize) -> u8 {
    (n.wrapping_mul(37) ^ (n >> 3)).to_le_bytes()[0]
}

#[test]
fn bytes_fed_at_each_byte_ready_are_written_end_to_end_without_a_slip() {
    let mut disk = Some(Surface::blank());
    let mut head = Head::new();
    let mut controls = reading(3, 2);
    controls.reading = false;
    head.set_controls(controls);
    // More bytes than the write clock takes to fall a whole cell behind the
    // ring's, fed as the DOS feeds port A a few cycles after BYTE READY,
    // each write to VIA2 handing the head its controls again.
    let count = 6000;
    let mut next = 0;
    let mut answer_at = None;
    let mut cycle = 0;
    while next < count {
        head.advance(cycle + 1, &mut disk);
        if head.byte_ready(cycle) {
            answer_at = Some(cycle + 4);
        }
        if answer_at == Some(cycle) {
            controls.write_value = fed(next);
            head.set_controls(controls);
            next += 1;
        }
        cycle += 1;
    }
    let track = disk
        .as_ref()
        .and_then(|surface| surface.track(34))
        .expect("written");
    let ring: Vec<bool> = (0..track.len()).map(|bit| track.bit(bit)).collect();
    // Each byte fed is shifted out after the one being written when it came.
    let expected: Vec<bool> = (0..count - 2)
        .flat_map(|n| (0..8).rev().map(move |bit| fed(n) >> bit & 1 != 0))
        .collect();
    let start = (0..ring.len())
        .find(|&at| (0..64).all(|bit| ring[(at + bit) % ring.len()] == expected[bit]))
        .expect("the first bytes fed are on the track");
    let slip = (0..expected.len()).find(|&bit| ring[(start + bit) % ring.len()] != expected[bit]);
    assert_eq!(slip, None, "the bits written from the first fed on");
}

/// `head` written into a snapshot and read back.
fn reloaded(head: &Head) -> Result<Head, crate::snapshot::SnapshotError> {
    let mut w = snapshot::Writer::new();
    w.put(head);
    let body = snapshot::open(&w.finish().expect("fits")).expect("opens");
    snapshot::Reader::new(&body).get::<Head>()
}

#[test]
fn a_deadline_already_past_falls_due_at_once_and_time_runs_on() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    head.advance(100, &mut disk);
    head.noise_at = head.now - 1;
    head.advance(200, &mut disk);
    assert_eq!(
        head.now,
        200 * CRYSTAL_PER_CYCLE,
        "the head reached the cycle asked"
    );
    head.next_reversal = Reversal::At(head.now - 1);
    head.advance(300, &mut disk);
    assert_eq!(
        head.now,
        300 * CRYSTAL_PER_CYCLE,
        "a real reversal past due fired once"
    );
}

#[test]
fn a_snapshot_with_a_past_noise_deadline_or_a_write_off_any_ring_is_refused() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    head.advance(100, &mut disk);
    assert_eq!(reloaded(&head).as_ref(), Ok(&head));
    let mut stale = head.clone();
    stale.noise_at = 0;
    assert!(reloaded(&stale).is_err(), "noise due before now");
    let mut astray = head;
    astray.write_at = Some(usize::MAX);
    assert!(reloaded(&astray).is_err(), "a write past any ring");
}

#[test]
fn a_write_position_off_the_ring_wraps_onto_it() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(Controls {
        reading: false,
        ..reading(2, 2)
    });
    head.write_at = Some(usize::MAX);
    head.advance(200, &mut disk);
    assert!(head.write_at.is_some_and(|at| at < MAX_TRACK_BYTES * 8));
}

#[test]
fn a_disk_changed_under_the_head_is_read_afresh() {
    let mut disk: Option<Surface> = None;
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    head.advance(100, &mut disk);
    assert_eq!(
        head.next_reversal,
        Reversal::Never,
        "an empty drive records nothing"
    );
    let mut disk = Some(self::disk());
    head.disk_changed();
    assert_eq!(head.next_reversal, Reversal::Unknown);
    let (_, sync) = read(&mut head, &mut disk, 100, 2_000);
    assert!(sync.is_some(), "the new disk's SYNC is seen");
}

#[test]
fn a_reversal_in_the_cell_under_the_head_is_still_ahead_of_it() {
    let mut ring = crate::media::c1541::Track::new(alloc::vec![0; 6250], 0).expect("a ring");
    ring.set_bit(10, true);
    let mut rings = alloc::vec![None; crate::media::c1541::HALF_TRACKS];
    rings[usize::from(HOME)] = Some(ring);
    let disk = Surface::new(rings, crate::media::c1541::Format::G64).expect("a disk");
    let mut head = Head::new();
    head.controls = reading(0, HOME & 3);
    head.angle = 10 * REVOLUTION / 50_000;
    assert_eq!(
        head.find_reversal(Some(&disk)),
        Some(head.now + FILTER_DELAY),
        "cell 10's reversal, a filter delay on"
    );
}

/// The regression: every write to VIA2 forgot the next reversal, and the
/// scan that found it again ran a bit at a time round a ring with none.
#[test]
fn a_write_to_via2_that_moves_nothing_keeps_the_next_reversal() {
    let mut disk = Some(disk());
    let mut head = Head::new();
    head.set_controls(reading(2, 2));
    head.advance(100, &mut disk);
    let cached = Reversal::At(head.now + 5);
    head.next_reversal = cached;
    head.set_controls(Controls {
        write_value: 0x55,
        ..reading(2, 2)
    });
    assert_eq!(head.next_reversal, cached, "the flux ahead is unchanged");
    head.set_controls(reading(2, 3));
    assert_eq!(
        head.next_reversal,
        Reversal::Unknown,
        "a step moves the head"
    );
}
