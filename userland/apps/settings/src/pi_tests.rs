//! The Raspberry Pi category: shown only on a Pi, the CPU fan staged and
//! applied as every machine setting is, and the cooling service's reading.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::gpio::{HeaderLines, Polarity};
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::hwtree::{HwDeviceClass, HwMatchKey, HwNode, HW_NODE_ROOT, HW_NODE_ROOT_ID};
use tairix_abi::sysinfo::PlatformCompatible;
use tairix_abi::thermal_ipc::{FanFault, FanLines, FanMode, FanPolicy, ThermalStatus};
use tairix_controls::FieldControl;
use tairix_geometry::Scale;
use tairix_sysconfig::{Key, SystemConfig};
use tairix_wallpaper::DesktopSettings;

use super::{line_label, names_raspberry_pi, status_rows, temperature_text, Cooling};
use crate::registry::{strip_rows, Board, Category, Pane, StripRow};
use crate::shell::{Shell, ShellOutcome};
use crate::test_support::{captions, damage, rows, task_words, theme, value_of, WIDE};
use tairix_controls::DisclosureSet;

const STATUS: usize = 0;
const FAN: usize = 1;
const MODE: usize = 0;
const LINE: usize = 1;
const START: usize = 3;

fn board(names: &[&[u8]]) -> PlatformCompatible {
    let mut root = HwNode::new(HW_NODE_ROOT_ID, HW_NODE_ROOT, HwDeviceClass::Root);
    for name in names {
        root.push_match_key(HwMatchKey::compatible(name).unwrap())
            .unwrap();
    }
    PlatformCompatible::of(&root)
}

/// A Pi 4's header as the cooling service reports it: GPIO 2–27, with the
/// console holding 14 and 15.
fn header() -> FanLines {
    let lent = HeaderLines::span(2, 27);
    let free = HeaderLines::from_mask(lent.mask() & !HeaderLines::span(14, 15).mask());
    FanLines::new(lent, free).unwrap()
}

fn reading(celsius: i16, fault: Option<FanFault>) -> Cooling {
    Cooling::Read(ThermalStatus {
        temperature: Some(Millicelsius::from_celsius(celsius)),
        policy: FanPolicy::new(FanMode::Automatic, Some(18), Polarity::ActiveHigh, 65, 10).unwrap(),
        running: true,
        fault,
        lines: Some(header()),
    })
}

/// A shell on a Pi showing its pane, the store read and the cooling service
/// answered.
fn pi_pane(config: SystemConfig) -> Shell {
    let mut shell = Shell::new(DesktopSettings::default(), Board::RASPBERRY_PI).expect("registry");
    shell.adopt_config(Some(config));
    let mut sink = damage();
    assert!(shell.go_to_pane("raspberry-pi", WIDE, Scale::ONE, &theme(), &mut sink));
    shell.adopt_cooling(reading(52, None));
    shell.lay_out(WIDE, Scale::ONE, &theme());
    shell
}

fn pending(shell: &Shell) -> Vec<(String, String)> {
    shell.form_for_test().expect("a composed pane").pending()
}

#[test]
fn only_a_board_named_by_raspberry_pi_is_one() {
    assert!(names_raspberry_pi(&board(&[
        b"raspberrypi,4-model-b",
        b"brcm,bcm2711"
    ])));
    assert!(names_raspberry_pi(&board(&[b"raspberrypi,3-model-b-plus"])));
    assert!(!names_raspberry_pi(&board(&[b"brcm,bcm2711"])));
    assert!(!names_raspberry_pi(&board(&[b"linux,dummy-virt"])));
    assert!(!names_raspberry_pi(&PlatformCompatible::default()));
    assert_eq!(
        Board::of(&board(&[b"raspberrypi,400"])),
        Board::RASPBERRY_PI
    );
    assert_eq!(Board::of(&board(&[b"riscv-virtio"])), Board::default());
}

#[test]
fn another_board_has_no_raspberry_pi_category_and_no_route_to_it() {
    let rows = strip_rows(&DisclosureSet::closed(), "", Board::default());
    assert!(!rows.contains(&StripRow::Category(Category::RaspberryPi)));
    assert!(strip_rows(&DisclosureSet::closed(), "cpu fan", Board::default()).is_empty());
    assert_eq!(Pane::named("raspberry-pi", Board::default()), None);
    assert_eq!(Pane::launched(&["raspberry-pi"], Board::default()), None);
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("registry");
    let mut sink = damage();
    assert!(!shell.go_to_pane("raspberry-pi", WIDE, Scale::ONE, &theme(), &mut sink));
    assert!(!shell.go_to(Pane::RaspberryPi, WIDE, Scale::ONE, &theme(), &mut sink));
}

#[test]
fn a_raspberry_pi_shows_the_category_and_finds_its_fan() {
    let rows = strip_rows(&DisclosureSet::closed(), "", Board::RASPBERRY_PI);
    assert!(rows.contains(&StripRow::Category(Category::RaspberryPi)));
    assert_eq!(
        strip_rows(&DisclosureSet::closed(), "cpu fan", Board::RASPBERRY_PI),
        [StripRow::Category(Category::RaspberryPi)]
    );
    assert_eq!(
        Pane::launched(&["raspberry-pi"], Board::RASPBERRY_PI),
        Some(Pane::RaspberryPi)
    );
}

#[test]
fn the_pane_states_the_reading_above_the_fan_s_policy() {
    let shell = pi_pane(SystemConfig::default());
    assert_eq!(captions(&shell), ["STATUS", "CPU FAN"]);
    let all = rows(&shell);
    assert_eq!(value_of(&all[0]), "52.0 °C");
    assert_eq!(value_of(&all[1]), "Running");
}

#[test]
fn a_reading_not_yet_taken_or_unanswered_is_stated_never_invented() {
    let unread = status_rows(&Cooling::Unread);
    assert!(matches!(unread[0].control(), FieldControl::Unmeasured(_)));
    let unanswered = status_rows(&Cooling::Unanswered);
    assert!(matches!(
        unanswered[1].control(),
        FieldControl::Unmeasured(_)
    ));
    let faulted = status_rows(&reading(70, Some(FanFault::SensorFailed)));
    assert!(faulted[1]
        .description()
        .is_some_and(|said| said.contains("cannot be read")));
}

#[test]
fn the_line_row_offers_the_free_lines_and_names_those_in_use() {
    let shell = pi_pane(SystemConfig::default());
    let form = shell.form_for_test().expect("a form");
    let row = &form.groups()[FAN].rows()[LINE];
    let FieldControl::Combo(combo) = row.control() else {
        panic!("a choice of lines");
    };
    assert_eq!(combo.choices()[0], "None");
    assert!(combo.choices().contains(&"GPIO 18 (pin 12)".to_string()));
    assert!(!combo
        .choices()
        .iter()
        .any(|choice| choice.starts_with("GPIO 14 ")));
    assert!(row
        .description()
        .is_some_and(|said| said.contains("GPIO 14 (pin 8), GPIO 15 (pin 10)")));
}

#[test]
fn the_fan_is_staged_and_applied_through_one_configure_run() {
    let mut shell = pi_pane(SystemConfig::default());
    assert!(shell.choose_for_test(FAN, MODE, 2), "Automatic");
    // None, then GPIO 2, 3, 4 … 13, 16, 17, 18: the console's pair is not
    // offered.
    let gpio18 = 1 + (2..=13).count() + 2;
    assert!(shell.choose_for_test(FAN, LINE, gpio18));
    assert_eq!(
        pending(&shell),
        vec![
            (Key::CoolingFanMode.name().to_string(), "auto".to_string()),
            (Key::CoolingFanLine.name().to_string(), "18".to_string()),
        ]
    );
    let theme = theme();
    let last = shell.action_rects(WIDE, Scale::ONE, &theme).len() - 1;
    let ShellOutcome::Authorise(task) = crate::test_support::press_band(&mut shell, last) else {
        panic!("Apply asks for the elevated run");
    };
    assert_eq!(
        task_words(&task),
        ["cooling.fan.mode", "auto", "cooling.fan.line", "18"]
    );
}

#[test]
fn a_temperature_slider_stop_is_a_whole_degree_of_its_range() {
    let mut shell = pi_pane(SystemConfig::default());
    assert!(shell.choose_for_test(FAN, START, 5));
    assert_eq!(
        pending(&shell),
        vec![(Key::CoolingFanStart.name().to_string(), "45".to_string())]
    );
    assert!(
        !shell.choose_for_test(FAN, START, 46),
        "past 85 °C stages nothing"
    );
}

#[test]
fn a_new_temperature_moves_one_reading_and_keeps_what_was_staged() {
    let mut shell = pi_pane(SystemConfig::default());
    assert!(shell.choose_for_test(FAN, MODE, 1), "On");
    shell.adopt_cooling(reading(61, None));
    assert_eq!(value_of(&rows(&shell)[0]), "61.0 °C");
    assert_eq!(pending(&shell).len(), 1, "the working copy stands");
    let form = shell.form_for_test().expect("a form");
    assert_eq!(form.groups()[STATUS].rows().len(), 2);
}

#[test]
fn line_labels_name_the_header_pin() {
    assert_eq!(line_label(18), "GPIO 18 (pin 12)");
    assert_eq!(line_label(2), "GPIO 2 (pin 3)");
    assert_eq!(line_label(27), "GPIO 27 (pin 13)");
    assert_eq!(line_label(40), "GPIO 40");
    let mut pins: Vec<u8> = super::HEADER_PINS.to_vec();
    pins.sort_unstable();
    pins.dedup();
    assert_eq!(pins.len(), 28, "each GPIO has its own pin");
    assert!(pins.iter().all(|pin| (3..=40).contains(pin)));
}

#[test]
fn a_header_not_served_offers_no_line_but_the_one_set() {
    let mut config = SystemConfig::default();
    config
        .set(Key::CoolingFanLine, "18")
        .expect("a header line");
    let mut shell = pi_pane(config);
    let Cooling::Read(status) = reading(50, None) else {
        unreachable!();
    };
    shell.adopt_cooling(Cooling::Read(ThermalStatus {
        lines: None,
        ..status
    }));
    let form = shell.form_for_test().expect("a form");
    let row = &form.groups()[FAN].rows()[LINE];
    let FieldControl::Combo(combo) = row.control() else {
        panic!("a choice of lines");
    };
    assert_eq!(combo.choices(), ["None", "GPIO 18 (pin 12)"]);
    assert_eq!(combo.selected_text(), Some("GPIO 18 (pin 12)"));
}

/// A reading that reshapes the pane rebuilds its rows from the working copy,
/// so nothing staged is lost.
#[test]
fn a_reading_that_reshapes_the_pane_keeps_what_was_staged() {
    let mut shell = pi_pane(SystemConfig::default());
    assert!(shell.choose_for_test(FAN, MODE, 1), "On");
    shell.adopt_cooling(reading(52, Some(FanFault::LineUnavailable)));
    assert_eq!(
        pending(&shell),
        vec![(Key::CoolingFanMode.name().to_string(), "on".to_string())]
    );
    assert!(rows(&shell)[1]
        .description()
        .is_some_and(|said| said.contains("would not lend")));
}

#[test]
fn temperatures_read_to_a_tenth_of_a_degree() {
    assert_eq!(temperature_text(487), "48.7 °C");
    assert_eq!(temperature_text(-5), "-0.5 °C");
    assert_eq!(temperature_text(0), "0.0 °C");
}

#[test]
fn only_a_change_of_temperature_is_a_reading_moved_in_place() {
    assert!(reading(50, None).only_temperature_moves(&reading(51, None)));
    assert!(
        !reading(50, None).only_temperature_moves(&reading(50, Some(FanFault::LineUnavailable)))
    );
    assert!(!Cooling::Unread.only_temperature_moves(&reading(50, None)));
}
