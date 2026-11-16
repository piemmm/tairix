use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::media::c1541::{d64, gcr};

/// A DOS that starts the motor on track 18, waits for SYNC and stores the
/// next eight bytes BYTE READY announces at $0300, waiting with BVC as the
/// 1541's DOS does.
fn header_reader() -> Vec<u8> {
    let program = [
        0xA9, 0x6F, 0x8D, 0x02, 0x1C, // VIA2 DDRB: phase, motor, LED, zone out
        0xA9, 0x46, 0x8D, 0x00, 0x1C, // motor on, zone 2, phase 2
        0xA9, 0xEE, 0x8D, 0x0C, 0x1C, // PCR: reading, BYTE READY enabled
        0xA9, 0x01, 0x8D, 0x0B, 0x1C, // ACR: port A latched
        0xA2, 0x00, // LDX #0
        0xAD, 0x00, 0x1C, 0x30, 0xFB, // wait for SYNC
        0xB8, // CLV
        0x50, 0xFE, // BVC *
        0xB8, // CLV
        0xAD, 0x01, 0x1C, // LDA $1C01
        0x9D, 0x00, 0x03, // STA $0300,X
        0xE8, 0xE0, 0x08, 0xD0, 0xF2, // INX : CPX #8 : BNE
        0x4C, 0x2A, 0xC0, // JMP *
    ];
    let mut dos = vec![0xEA; DOS_BYTES];
    dos[..program.len()].copy_from_slice(&program);
    dos[0x3FFC..0x3FFE].copy_from_slice(&[0x00, 0xC0]);
    dos
}

fn disk() -> Surface {
    let mut image = vec![0; 174_848];
    let bam = (1..18u8)
        .map(|t| usize::from(d64::sectors(t)))
        .sum::<usize>()
        * 256;
    image[bam + 0xA2] = b'A';
    image[bam + 0xA3] = b'B';
    d64::decode(&image).expect("a D64")
}

#[test]
fn the_dos_reads_a_header_through_byte_ready_and_so() {
    let mut drive = Drive::new(&header_reader(), Unit::Eight).expect("a 16 KiB DOS");
    drive.insert(disk());
    drive.run_until(5_000);
    assert!(drive.motor());
    let mut header = Vec::new();
    gcr::encode(
        &[0x08, 0x12 ^ b'A' ^ b'B', 0, 18, b'B', b'A', 0x0F, 0x0F],
        &mut header,
    );
    let read: Vec<u8> = (0..8).map(|i| drive.ram(0x0300 + i)).collect();
    assert_eq!(read, header[..8]);
}

#[test]
fn the_drive_answers_atn_on_data_until_it_acknowledges() {
    let mut drive = Drive::new(&vec![0xEA; DOS_BYTES], Unit::Eight).expect("a DOS");
    drive.run_until(10);
    let atn = Lines {
        atn: true,
        ..Lines::default()
    };
    drive.set_others(atn);
    assert!(drive.holds().data, "ATN unacknowledged");
    drive.set_others(Lines::default());
    assert!(
        !drive.holds().data || drive.holds().clock,
        "released with ATN"
    );
}

#[test]
fn the_bus_monitor_records_the_vias_accesses_while_asked() {
    let program = [
        0xA9, 0x1A, 0x8D, 0x02, 0x18, // LDA #$1A : STA $1802
        0xAD, 0x00, 0x1C, // LDA $1C00
        0x8D, 0x00, 0x03, // STA $0300
        0x4C, 0x0B, 0xC0, // JMP *
    ];
    let mut dos = vec![0xEA; DOS_BYTES];
    dos[..program.len()].copy_from_slice(&program);
    dos[0x3FFC..0x3FFE].copy_from_slice(&[0x00, 0xC0]);
    let mut unasked = Drive::new(&dos, Unit::Eight).expect("a DOS");
    unasked.run_until(100);
    assert_eq!(
        unasked.take_io_trace().count(),
        0,
        "nothing recorded unasked"
    );
    let mut drive = Drive::new(&dos, Unit::Eight).expect("a DOS");
    drive.monitor_io(true);
    drive.run_until(100);
    let trace: Vec<IoAccess> = drive.take_io_trace().collect();
    let seen: Vec<(u16, bool)> = trace
        .iter()
        .map(|access| (access.addr, access.write))
        .collect();
    assert_eq!(seen, [(0x1802, true), (0x1C00, false)]);
    assert_eq!(trace[0].value, 0x1A);
    assert!(
        trace[0].cycle < trace[1].cycle,
        "each at the cycle the VIA saw it"
    );
}

#[test]
fn a_dos_of_another_size_is_refused() {
    assert!(matches!(
        Drive::new(&[0; 100], Unit::Nine),
        Err(RomError::BadSize)
    ));
}
