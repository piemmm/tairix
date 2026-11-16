use super::*;
use crate::media::crt::{decode, tests::image};

fn cartridge(hardware: u16, exrom: u8, game: u8, chips: &[(u16, u16, &[u8])]) -> Cartridge {
    Cartridge::new(&decode(&image(hardware, exrom, game, chips)).expect("a CRT")).expect("emulated")
}

/// A bank whose every byte is `fill`.
fn bank(fill: u8) -> alloc::vec::Vec<u8> {
    alloc::vec![fill; BANK_BYTES]
}

#[test]
fn a_generic_cartridge_maps_as_its_image_says() {
    let mut eight = cartridge(0, 0, 1, &[(0, 0x8000, &bank(0x11))]);
    assert_eq!(
        eight.lines(0),
        memory::Cartridge {
            game: true,
            exrom: false
        }
    );
    assert_eq!(eight.read_roml(0x8123, 0), Some(0x11));
    assert_eq!(eight.read_romh(0xA000), None, "no ROMH chip");
    let mut sixteen = cartridge(0, 0, 0, &[(0, 0x8000, &[0x22; 0x4000])]);
    assert_eq!(sixteen.read_roml(0x9FFF, 0), Some(0x22));
    assert_eq!(
        sixteen.read_romh(0xA000),
        Some(0x22),
        "the chip's second half"
    );
    let ultimax = cartridge(0, 1, 0, &[(0, 0xE000, &bank(0x33))]);
    assert!(ultimax.lines(0).ultimax());
    assert_eq!(ultimax.read_romh(0xFFFC), Some(0x33));
}

#[test]
fn ocean_banks_through_de00_with_romh_mirroring_roml() {
    let banks: alloc::vec::Vec<_> = (0..16u8).map(bank).collect();
    let chips: alloc::vec::Vec<(u16, u16, &[u8])> = banks
        .iter()
        .enumerate()
        .map(|(n, bytes)| (u16::try_from(n).expect("few"), 0x8000, bytes.as_slice()))
        .collect();
    let mut ocean = cartridge(5, 0, 0, &chips);
    assert_eq!(
        ocean.lines(0),
        memory::Cartridge {
            game: false,
            exrom: false
        },
        "16K"
    );
    ocean.write_io(0xDE00, 0x80 | 5);
    assert_eq!(ocean.read_roml(0x8000, 0), Some(5));
    assert_eq!(ocean.read_romh(0xA000), Some(5));
}

#[test]
fn simons_basic_s_upper_half_comes_in_on_any_write_to_de00_and_goes_on_a_read() {
    let mut simons = cartridge(
        4,
        0,
        1,
        &[(0, 0x8000, &bank(0x11)), (0, 0xA000, &bank(0x22))],
    );
    let lines = |game| memory::Cartridge { game, exrom: false };
    assert_eq!(simons.lines(0), lines(true), "8K from reset");
    for value in [0x00, 0x01, 0xFE] {
        simons.write_io(0xDE00, value);
        assert_eq!(
            simons.lines(0),
            lines(false),
            "16K after writing {value:02X}"
        );
        assert_eq!(simons.read_romh(0xA000), Some(0x22));
        assert_eq!(simons.read_io(0xDE00, 0), None);
        assert_eq!(simons.lines(0), lines(true), "8K after a read");
    }
}

#[test]
fn magic_desk_disables_itself_with_bit_7() {
    let mut desk = cartridge(19, 0, 1, &[(0, 0x8000, &bank(1)), (1, 0x8000, &bank(2))]);
    desk.write_io(0xDE00, 1);
    assert_eq!(desk.read_roml(0x8000, 0), Some(2));
    desk.write_io(0xDE00, 0x80);
    assert_eq!(desk.lines(0), memory::Cartridge::NONE);
}

#[test]
fn fun_play_scrambles_its_bank_bits_and_86_switches_it_off() {
    let banks: alloc::vec::Vec<_> = (0..16u8).map(bank).collect();
    let chips: alloc::vec::Vec<(u16, u16, &[u8])> = banks
        .iter()
        .enumerate()
        .map(|(n, bytes)| (u16::try_from(n).expect("few"), 0x8000, bytes.as_slice()))
        .collect();
    let mut fun = cartridge(7, 0, 1, &chips);
    fun.write_io(0xDE00, 0x09);
    assert_eq!(fun.read_roml(0x8000, 0), Some(9), "bits 3–5 then bit 0");
    fun.write_io(0xDE00, 0x86);
    assert_eq!(fun.lines(0), memory::Cartridge::NONE);
}

#[test]
fn super_games_latches_its_register_shut() {
    let mut games = cartridge(
        8,
        0,
        0,
        &[(0, 0x8000, &[1; 0x4000]), (1, 0x8000, &[2; 0x4000])],
    );
    games.write_io(0xDF00, 0x08 | 0x01);
    assert_eq!(games.read_romh(0xA000), Some(2));
    games.write_io(0xDF00, 0x04);
    assert_eq!(
        games.read_roml(0x8000, 0),
        Some(2),
        "locked: the write did nothing"
    );
    games.reset(0);
    games.write_io(0xDF00, 0x04);
    assert_eq!(games.lines(0), memory::Cartridge::NONE);
}

#[test]
fn epyx_fastload_switches_off_unless_read() {
    let mut epyx = cartridge(10, 0, 1, &[(0, 0x8000, &bank(0x55))]);
    assert!(!epyx.lines(100).exrom, "on after reset");
    assert!(epyx.lines(600).exrom, "charged and off");
    let _ = epyx.read_io(0xDE00, 600);
    assert!(!epyx.lines(700).exrom, "an I/O1 read discharges it");
    assert_eq!(
        epyx.read_io(0xDF7F, 5000),
        Some(0x55),
        "its last page through I/O2"
    );
}

#[test]
fn system_3_and_dinamic_bank_by_the_address_of_an_io1_access() {
    let banks: alloc::vec::Vec<_> = (0..8u8).map(bank).collect();
    let chips: alloc::vec::Vec<(u16, u16, &[u8])> = banks
        .iter()
        .enumerate()
        .map(|(n, bytes)| (u16::try_from(n).expect("few"), 0x8000, bytes.as_slice()))
        .collect();
    let mut system3 = cartridge(15, 0, 1, &chips);
    system3.write_io(0xDE03, 0);
    assert_eq!(system3.read_roml(0x8000, 0), Some(3));
    let mut dinamic = cartridge(17, 0, 1, &chips);
    dinamic.write_io(0xDE05, 0);
    assert_eq!(dinamic.read_roml(0x8000, 0), Some(0), "writes do nothing");
    let _ = dinamic.read_io(0xDE05, 0);
    assert_eq!(dinamic.read_roml(0x8000, 0), Some(5));
}

#[test]
fn zaxxon_chooses_romh_by_the_half_of_roml_read() {
    let mut zaxxon = cartridge(
        18,
        0,
        0,
        &[
            (0, 0x8000, &[0x77; 0x1000]),
            (0, 0xA000, &bank(0xA0)),
            (1, 0xA000, &bank(0xA1)),
        ],
    );
    assert_eq!(
        zaxxon.read_roml(0x9234, 0),
        Some(0x77),
        "the 4K ROM repeats"
    );
    assert_eq!(zaxxon.read_romh(0xA000), Some(0xA1));
    let _ = zaxxon.read_roml(0x8000, 0);
    assert_eq!(zaxxon.read_romh(0xA000), Some(0xA0));
}

#[test]
fn an_unemulated_type_is_refused() {
    let crt = decode(&image(1, 0, 0, &[(0, 0x8000, &bank(0))])).expect("a CRT");
    assert_eq!(Cartridge::new(&crt), Err(CartridgeError::Unsupported(1)));
}

#[test]
fn a_chip_that_does_not_fit_its_slot_is_refused() {
    let new = |chips: &[(u16, u16, &[u8])]| {
        Cartridge::new(&decode(&image(0, 0, 0, chips)).expect("a CRT")).map(|_| ())
    };
    let sixteen = alloc::vec![0x22; 2 * BANK_BYTES];
    assert_eq!(new(&[(0, 0x8000, &sixteen)]), Ok(()), "ROML then ROMH");
    for address in [0xA000, 0xE000, 0x9000] {
        assert_eq!(
            new(&[(0, address, &sixteen)]),
            Err(CartridgeError::Misplaced),
            "16 KiB at {address:04X}"
        );
    }
    assert_eq!(
        new(&[(0, 0x9000, &bank(0x33)[..0x1000])]),
        Ok(()),
        "4 KiB in ROML's upper half"
    );
}
