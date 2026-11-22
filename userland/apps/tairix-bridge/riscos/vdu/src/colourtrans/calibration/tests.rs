use super::*;

/// The manual's example table.
const MANUAL: [u32; 9] = [
    2,
    2,
    2,
    0x0201_0300,
    0x0203_FDFF,
    0x0201_0300,
    0x03FC_02FF,
    0x0201_0300,
    0xFF03_02FF,
];

/// A red list of three pairs, its middle one off the line between its ends.
const THREE: [u32; 10] = [
    3,
    2,
    2,
    0x1010_1000,
    0x2040_2080,
    0xFF00_00FF,
    0,
    0x00FF_00FF,
    0,
    0xFF00_00FF,
];

fn converted(table: &[u32], device: u32) -> u32 {
    let calibration = Calibration::of_words(table.to_vec()).expect("a good table");
    calibration
        .convert(Rgb::from_riscos_word(device))
        .riscos_word()
}

#[test]
fn a_device_colour_shows_its_channels_interpolated_and_summed_as_risc_os_5_works_them() {
    for (device, standard) in [
        (0, 0x0603_0900),
        (0xFFFF_FF00, 0xFFFF_FF00),
        (0x8080_8000, 0x8582_8500),
        (0x0000_FF00, 0x0605_FF00),
        (0x40C0_2000, 0x46C0_2700),
        (0x7F7F_7F00, 0x8481_8400),
        (0x0000_8100, 0x0604_8700),
        (0x0000_C000, 0x0604_C500),
        (0x0000_FE00, 0x0604_FF00),
        (0x0100_0000, 0x0603_0800),
        (0xFEFE_FE00, 0xFFFF_FF00),
        (0x1234_5600, 0x1836_5D00),
        (0x00FF_0000, 0x07FE_0800),
        (0xFF00_0000, 0xFF05_0800),
    ] {
        assert_eq!(converted(&MANUAL, device), standard, "{device:08X}");
    }
}

#[test]
fn a_falling_part_of_a_list_is_cut_toward_minus_infinity() {
    for (device, standard) in [
        (0, 0x1010_1000),
        (0xFFFF_FF00, 0xFFFF_0000),
        (0x8080_8000, 0x9FBF_2000),
        (0x0000_8000, 0x2040_2000),
        (0x0000_8100, 0x213F_1F00),
        (0x0000_8800, 0x2E3B_1D00),
        (0x0000_AA00, 0x692A_1500),
        (0x0000_C000, 0x901F_0F00),
        (0x40C0_2000, 0x53DB_1400),
        (0x1234_5600, 0x2C64_1A00),
    ] {
        assert_eq!(converted(&THREE, device), standard, "{device:08X}");
    }
}

#[test]
fn a_list_must_run_from_nought_to_the_top_in_ascending_levels() {
    let mut bad_start = MANUAL;
    bad_start[3] = 0x0201_0310;
    let mut bad_end = MANUAL;
    bad_end[4] = 0x0203_FDFE;
    let one_pair = [1, 2, 2, 0, 0, 0xFF00_00FF, 0, 0xFF00_00FF];
    let unsorted = [
        3,
        2,
        2,
        0,
        0x8000_00FF,
        0xFF00_0080,
        0,
        0xFF00_00FF,
        0,
        0xFF00_00FF,
    ];
    let repeated = [3, 2, 2, 0, 0, 0xFF00_00FF, 0, 0xFF00_00FF, 0, 0xFF00_00FF];
    let short = [2, 2, 2, 0, 0xFF];
    for table in [
        &bad_start[..],
        &bad_end,
        &one_pair,
        &unsorted,
        &repeated,
        &short,
        &[],
    ] {
        assert_eq!(
            Calibration::of_words(table.to_vec()),
            Err(Invalid),
            "{table:X?}"
        );
    }
}

#[test]
fn a_table_keeps_its_words() {
    let calibration = Calibration::of_words(THREE.to_vec()).expect("a good table");
    assert_eq!(calibration.words(), THREE);
}

#[test]
fn a_table_s_commands_give_its_sizes_blue_first_and_its_pairs_last_first() {
    let default = commands(&DEFAULT_TABLE).expect("room");
    assert_eq!(
        default,
        b"ColourTransMapSize &2 &2 &2\nColourTransMap &FF0000FF &0 &FF00FF &0 &FFFF &0\n"
    );
    let three = commands(&THREE).expect("room");
    assert_eq!(
        three,
        b"ColourTransMapSize &2 &2 &3\nColourTransMap &FF0000FF &0 &FF00FF &0 &FF0000FF &20402080 &10101000\n"
    );
}
