use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// The documented configurations, mode = EXROM GAME CHAREN HIRAM LORAM, each
/// with what `$1000`, `$8000`, `$A000`, `$C000`, `$D000` and `$E000` reach.
const TABLE: [(u8, [Chip; 6]); 32] = {
    use Chip::{Basic as B, Character as C, Io as I, Kernal as K, Open as O, Ram as R};
    use Chip::{RomHigh as H, RomLow as L};
    [
        (31, [R, R, B, R, I, K]),
        (30, [R, R, R, R, I, K]),
        (29, [R, R, R, R, I, R]),
        (28, [R, R, R, R, R, R]),
        (27, [R, R, B, R, C, K]),
        (26, [R, R, R, R, C, K]),
        (25, [R, R, R, R, C, R]),
        (24, [R, R, R, R, R, R]),
        (23, [O, L, O, O, I, H]),
        (22, [O, L, O, O, I, H]),
        (21, [O, L, O, O, I, H]),
        (20, [O, L, O, O, I, H]),
        (19, [O, L, O, O, I, H]),
        (18, [O, L, O, O, I, H]),
        (17, [O, L, O, O, I, H]),
        (16, [O, L, O, O, I, H]),
        (15, [R, L, B, R, I, K]),
        (14, [R, R, R, R, I, K]),
        (13, [R, R, R, R, I, R]),
        (12, [R, R, R, R, R, R]),
        (11, [R, L, B, R, C, K]),
        (10, [R, R, R, R, C, K]),
        (9, [R, R, R, R, C, R]),
        (8, [R, R, R, R, R, R]),
        (7, [R, L, H, R, I, K]),
        (6, [R, R, H, R, I, K]),
        (5, [R, R, R, R, I, R]),
        (4, [R, R, R, R, R, R]),
        (3, [R, L, H, R, C, K]),
        (2, [R, R, H, R, C, K]),
        (1, [R, R, R, R, R, R]),
        (0, [R, R, R, R, R, R]),
    ]
};

/// Each table region's 4 KiB pages.
const REGIONS: [&[u8]; 6] = [
    &[1, 2, 3, 4, 5, 6, 7],
    &[8, 9],
    &[0xA, 0xB],
    &[0xC],
    &[0xD],
    &[0xE, 0xF],
];

#[test]
fn every_configuration_maps_its_pages_as_the_pla_does() {
    for (mode, regions) in TABLE {
        let cartridge = Cartridge {
            game: mode & 0x08 != 0,
            exrom: mode & 0x10 != 0,
        };
        let banking = mode & line::BANKING;
        assert_eq!(chip(banking, cartridge, 0), Chip::Ram, "mode {mode} page 0");
        for (pages, &want) in REGIONS.iter().zip(&regions) {
            for &page in *pages {
                assert_eq!(
                    chip(banking, cartridge, page),
                    want,
                    "mode {mode} page {page:X}"
                );
            }
        }
    }
}

fn byte(value: usize) -> u8 {
    u8::try_from(value).expect("a byte")
}

fn memory() -> Memory {
    let basic: Vec<u8> = (0..BASIC_BYTES).map(|i| byte(i % 251)).collect();
    let kernal: Vec<u8> = (0..KERNAL_BYTES).map(|i| byte(i % 241) ^ 0x5A).collect();
    let character: Vec<u8> = (0..CHARACTER_BYTES).map(|i| byte(i % 239) ^ 0xA5).collect();
    Memory::new(&basic, &kernal, &character).expect("the images fit")
}

#[test]
fn a_rom_image_of_another_size_is_refused() {
    let short = vec![0; BASIC_BYTES - 1];
    let kernal = vec![0; KERNAL_BYTES];
    let character = vec![0; CHARACTER_BYTES];
    assert_eq!(
        Memory::new(&short, &kernal, &character).map(|_| ()),
        Err(RomError::BadSize)
    );
}

/// The byte the CPU reads at `addr`, I/O aside.
fn read(memory: &Memory, addr: u16) -> u8 {
    memory.read_chip(memory.cpu_chip(addr), addr)
}

#[test]
fn the_pull_ups_map_the_roms_and_io_in_at_power_on() {
    let memory = memory();
    assert_eq!(memory.port.banking(), line::BANKING);
    assert_eq!(memory.cpu_chip(0xA000), Chip::Basic);
    assert_eq!(memory.cpu_chip(0xD000), Chip::Io);
    assert_eq!(memory.cpu_chip(0xFFFC), Chip::Kernal);
    assert_eq!(read(&memory, 0xA001), 1);
    assert_eq!(read(&memory, 0xE000), 0x5A);
}

#[test]
fn a_chip_read_at_an_address_it_does_not_answer_still_reads_a_byte() {
    let memory = memory();
    assert_eq!(memory.read_chip(Chip::Basic, 0x2001), read(&memory, 0xA001));
    assert_eq!(
        memory.read_chip(Chip::Kernal, 0x0000),
        read(&memory, 0xE000)
    );
}

#[test]
fn the_roms_read_where_mapped_and_ram_beneath_where_not() {
    let mut memory = memory();
    memory.write_ram(0xE000, 0x42);
    memory.write_ram(0xA000, 0x24);
    assert_eq!(read(&memory, 0xE000), 0x5A);
    memory.port.write(0, 0x07, 0);
    memory.port.write(1, line::LORAM, 0);
    assert_eq!(memory.cpu_chip(0xE000), Chip::Ram);
    assert_eq!(read(&memory, 0xE000), 0x42);
    assert_eq!(memory.cpu_chip(0xA000), Chip::Ram);
    assert_eq!(read(&memory, 0xA000), 0x24);
    assert_eq!(memory.cpu_chip(0xD000), Chip::Character);
    assert_eq!(read(&memory, 0xD001), 0xA5 ^ 1);
}

#[test]
fn the_kernal_s_set_up_reads_back_with_the_sense_line() {
    let mut memory = memory();
    memory.port.write(0, 0x2F, 0);
    memory.port.write(1, 0x37, 0);
    assert_eq!(memory.port.read(0, true, 0), 0x2F);
    assert_eq!(memory.port.read(1, true, 0), 0x37);
    assert_eq!(memory.port.read(1, false, 0), 0x27, "a button held");
    assert!(!memory.port.motor());
    memory.port.write(1, 0x17, 0);
    assert!(memory.port.motor());
}

#[test]
fn an_undriven_line_keeps_its_level_but_the_motor_line_is_pulled_low_and_runs() {
    let mut port = Port::new();
    port.write(0, 0xFF, 0);
    port.write(1, line::CASSETTE_WRITE | line::CASSETTE_MOTOR, 0);
    assert!(!port.motor(), "driven high");
    port.write(0, 0x00, 10);
    let read = port.read(1, true, 20);
    assert_eq!(read & line::CASSETTE_WRITE, line::CASSETTE_WRITE);
    assert_eq!(read & line::CASSETTE_MOTOR, 0);
    assert!(port.motor(), "undriven");
    assert!(Port::new().motor(), "every line an input at power-on");
}

#[test]
fn lines_6_and_7_hold_a_1_until_it_falls_off() {
    let mut port = Port::new();
    port.write(0, 0xC0, 0);
    port.write(1, 0x40, 0);
    port.write(0, 0x00, 100);
    assert_eq!(port.read(1, true, 100) & 0xC0, 0x40);
    assert_eq!(port.read(1, true, 100 + FALL_OFF_CYCLES - 1) & 0xC0, 0x40);
    assert_eq!(port.read(1, true, 100 + FALL_OFF_CYCLES) & 0xC0, 0x00);
}

#[test]
fn a_line_never_driven_reads_0() {
    let port = Port::new();
    assert_eq!(port.read(1, true, 0) & 0xC0, 0);
}

#[test]
fn the_vic_ii_sees_its_bank_and_the_character_rom_in_banks_0_and_2() {
    let mut memory = memory();
    for bank in 0..4u8 {
        let base = u16::from(bank) << 14;
        memory.write_ram(base | 0x1000, 0x10 + bank);
        memory.write_ram(base | 0x0400, 0x20 + bank);
    }
    for bank in 0..4u8 {
        assert_eq!(memory.vic_read(bank, 0x0400), 0x20 + bank);
    }
    assert_eq!(memory.vic_read(0, 0x1000), 0xA5);
    assert_eq!(memory.vic_read(2, 0x1001), 0xA5 ^ 1);
    assert_eq!(memory.vic_read(1, 0x1000), 0x11);
    assert_eq!(memory.vic_read(3, 0x1000), 0x13);
}

#[test]
fn colour_ram_keeps_four_bits_a_cell_mirrored_through_its_ten_lines() {
    let mut memory = memory();
    memory.write_colour(0xD800, 0xAB);
    assert_eq!(memory.colour(0xD800), 0x0B);
    assert_eq!(memory.colour(0xDC00), 0x0B);
}
