//! The Raspberry Pi category: shown only on a Pi, the CPU fan staged and
//! applied as every machine setting is, and the cooling service's reading.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::gpio::{HeaderLines, Polarity};
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::sysinfo::PlatformCompatible;
use tairix_abi::thermal_ipc::{FanFault, FanLines, FanMode, FanPolicy, ThermalStatus};
use tairix_controls::testkit::{covers, keystroke};
use tairix_controls::FieldControl;
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, NamedKey, PointerButton};
use tairix_sysconfig::{Key, SystemConfig};
use tairix_wallpaper::DesktopSettings;

use super::{line_label, names_raspberry_pi, status_rows, temperature_text, Cooling};
use crate::registry::{strip_rows, Board, Category, Pane, StripRow};
use crate::shell::{Shell, ShellOutcome};
use crate::test_support::{
    captions, click, damage, platform, raspberry_pi, rows, task_words, theme, value_of, WIDE,
};
use tairix_controls::DisclosureSet;

const STATUS: usize = 0;
const FAN: usize = 1;
const MODE: usize = 0;
const LINE: usize = 1;
const START: usize = 3;

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
    let mut shell = Shell::new(DesktopSettings::default(), raspberry_pi()).expect("registry");
    shell.adopt_config(Some(config));
    let mut sink = damage();
    assert!(shell.go_to_pane("raspberry-pi", WIDE, Scale::ONE, &theme(), &mut sink));
    shell.adopt_cooling(reading(52, None), (WIDE, Scale::ONE, &theme()), &mut sink);
    shell.lay_out(WIDE, Scale::ONE, &theme());
    shell
}

fn pending(shell: &Shell) -> Vec<(String, String)> {
    shell.form_for_test().expect("a composed pane").pending()
}

#[test]
fn only_a_board_named_by_raspberry_pi_is_one() {
    assert!(names_raspberry_pi(&platform(&[
        b"raspberrypi,4-model-b",
        b"brcm,bcm2711"
    ])));
    assert!(names_raspberry_pi(&platform(&[
        b"raspberrypi,3-model-b-plus"
    ])));
    assert!(!names_raspberry_pi(&platform(&[b"brcm,bcm2711"])));
    assert!(!names_raspberry_pi(&platform(&[b"linux,dummy-virt"])));
    assert!(!names_raspberry_pi(&PlatformCompatible::default()));
    assert!(Board::of(&platform(&[b"raspberrypi,400"])).shows(Category::RaspberryPi));
    assert_eq!(Board::of(&platform(&[b"riscv-virtio"])), Board::default());
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
    let rows = strip_rows(&DisclosureSet::closed(), "", raspberry_pi());
    assert!(rows.contains(&StripRow::Category(Category::RaspberryPi)));
    assert_eq!(
        strip_rows(&DisclosureSet::closed(), "cpu fan", raspberry_pi()),
        [StripRow::Category(Category::RaspberryPi)]
    );
    assert_eq!(
        Pane::launched(&["raspberry-pi"], raspberry_pi()),
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
    shell.adopt_cooling(
        reading(61, None),
        (WIDE, Scale::ONE, &theme()),
        &mut damage(),
    );
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
    shell.adopt_cooling(
        Cooling::Read(ThermalStatus {
            lines: None,
            ..status
        }),
        (WIDE, Scale::ONE, &theme()),
        &mut damage(),
    );
    let form = shell.form_for_test().expect("a form");
    let row = &form.groups()[FAN].rows()[LINE];
    let FieldControl::Combo(combo) = row.control() else {
        panic!("a choice of lines");
    };
    assert_eq!(combo.choices(), ["None", "GPIO 18 (pin 12)"]);
    assert_eq!(combo.selected_text(), Some("GPIO 18 (pin 12)"));
}

/// A reading keeps what was staged, whether it restates the STATUS plate in
/// place or rebuilds the pane.
#[test]
fn a_reading_that_reshapes_the_pane_keeps_what_was_staged() {
    let mut shell = pi_pane(SystemConfig::default());
    assert!(shell.choose_for_test(FAN, MODE, 1), "On");
    let faulted = reading(52, Some(FanFault::LineUnavailable));
    shell.adopt_cooling(faulted.clone(), (WIDE, Scale::ONE, &theme()), &mut damage());
    assert!(rows(&shell)[1]
        .description()
        .is_some_and(|said| said.contains("would not lend")));
    let Cooling::Read(status) = faulted else {
        unreachable!();
    };
    shell.adopt_cooling(
        Cooling::Read(ThermalStatus {
            lines: None,
            ..status
        }),
        (WIDE, Scale::ONE, &theme()),
        &mut damage(),
    );
    assert_eq!(
        pending(&shell),
        vec![(Key::CoolingFanMode.name().to_string(), "on".to_string())]
    );
}

#[test]
fn temperatures_read_to_a_tenth_of_a_degree() {
    assert_eq!(temperature_text(487), "48.7 °C");
    assert_eq!(temperature_text(-5), "-0.5 °C");
    assert_eq!(temperature_text(0), "0.0 °C");
}

#[test]
fn only_a_reading_of_the_same_lines_moves_in_place() {
    assert!(reading(50, None).moves_only_status(&reading(51, None)));
    assert!(reading(50, None).moves_only_status(&reading(50, Some(FanFault::LineUnavailable))));
    let Cooling::Read(status) = reading(50, None) else {
        unreachable!();
    };
    assert!(
        !reading(50, None).moves_only_status(&Cooling::Read(ThermalStatus {
            lines: None,
            ..status
        }))
    );
    assert!(!Cooling::Unread.moves_only_status(&reading(50, None)));
}

/// A reading reports what it moved and nothing more: nothing for the reading
/// already shown or one landing once the pane is left, a row for what moved
/// in it, and the pane where its rows were rebuilt.
#[test]
fn a_reading_reports_only_what_it_moved() {
    let theme = theme();
    let seat = (WIDE, Scale::ONE, &theme);
    let mut shell = pi_pane(SystemConfig::default());
    let shown = |shell: &Shell, row: usize| {
        shell
            .row_rect_for_test((STATUS, row), WIDE, Scale::ONE, &theme)
            .expect("the STATUS rows show")
    };
    let Cooling::Read(status) = reading(61, None) else {
        unreachable!();
    };
    let mut moved = damage();
    shell.adopt_cooling(reading(52, None), seat, &mut moved);
    assert!(moved.is_empty(), "the reading already shown moves nothing");
    let temperature = shown(&shell, 0);
    shell.adopt_cooling(reading(61, None), seat, &mut moved);
    assert_eq!(
        moved.bounds(),
        temperature,
        "a new temperature moves its row alone"
    );
    let fan = shown(&shell, 1);
    let mut stopped = damage();
    shell.adopt_cooling(
        Cooling::Read(ThermalStatus {
            running: false,
            ..status
        }),
        seat,
        &mut stopped,
    );
    assert_eq!(stopped.bounds(), fan, "a stopped fan moves its row alone");
    let mut unlent = damage();
    shell.adopt_cooling(
        Cooling::Read(ThermalStatus {
            running: false,
            lines: None,
            ..status
        }),
        seat,
        &mut unlent,
    );
    let column = shell.frame(WIDE, Scale::ONE, &theme).content;
    assert!(
        covers(&unlent, column),
        "a reading the line row is built from rebuilds the pane"
    );
    assert!(shell.go_to_pane("lock-screen", WIDE, Scale::ONE, &theme, &mut damage()));
    let mut away = damage();
    shell.adopt_cooling(reading(70, None), seat, &mut away);
    assert!(
        away.is_empty(),
        "a reading landing once the pane is left moves nothing"
    );
}

/// The category wears the board-and-fan badge, which the icon library names
/// for what it draws rather than for any board.
#[test]
fn the_category_wears_the_board_and_fan_badge() {
    assert_eq!(
        Category::RaspberryPi.row().map(|row| row.icon),
        Some(IconKind::BoardFan)
    );
}

/// A window tall enough to seat the whole pane.
const TALL: Rect = Rect::new(0, 0, WIDE.width, 4 * WIDE.height);

/// [`pi_pane`] on the store's defaults, laid out in [`TALL`].
fn tall_pane() -> Shell {
    let mut shell = pi_pane(SystemConfig::default());
    shell.lay_out(TALL, Scale::ONE, &theme());
    shell
}

/// Route `events` to the shell laid out in [`TALL`].
fn route(shell: &mut Shell, events: &[InputEvent]) {
    let theme = theme();
    for event in events {
        let _ = shell.on_pointer(event, TALL, Scale::ONE, &theme, &mut damage());
    }
}

/// The "Turns on at" slider.
fn start_slider(shell: &Shell) -> tairix_controls::Slider {
    let form = shell.form_for_test().expect("a form");
    let FieldControl::Slider(slider) = form.groups()[FAN].rows()[START].control() else {
        panic!("the start temperature is a slider");
    };
    slider.clone()
}

/// What the STATUS plate's fan row says beneath it, if anything.
fn fan_said(shell: &Shell) -> Option<String> {
    rows(shell)[1].description().map(String::from)
}

/// A reading landing while "Turns on at" is dragged waits for the drag to
/// settle rather than rebuilding the slider under the pointer.
#[test]
fn a_reading_landing_mid_drag_waits_for_the_drag_to_settle() {
    let theme = theme();
    let mut shell = tall_pane();
    let start = shell
        .row_control_rect_for_test((FAN, START), TALL, Scale::ONE, &theme)
        .expect("the start row shows");
    route(
        &mut shell,
        &[
            InputEvent::PointerMoved { to: start.center() },
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
        ],
    );
    assert!(
        start_slider(&shell).is_dragging(),
        "the press took the slider"
    );
    shell.adopt_cooling(
        reading(52, Some(FanFault::LineUnavailable)),
        (TALL, Scale::ONE, &theme),
        &mut damage(),
    );
    assert!(start_slider(&shell).is_dragging(), "the drag goes on");
    assert_eq!(fan_said(&shell), None, "the reading waits");
    route(
        &mut shell,
        &[InputEvent::PointerReleased {
            button: PointerButton::Primary,
        }],
    );
    assert!(!start_slider(&shell).is_dragging());
    assert!(fan_said(&shell).is_some_and(|said| said.contains("would not lend")));
}

/// A reading landing while the line list is open waits for it to close
/// rather than closing it.
#[test]
fn a_reading_landing_while_the_line_list_is_open_waits_for_it_to_close() {
    let theme = theme();
    let mut shell = tall_pane();
    let listing = |shell: &Shell| {
        shell
            .form_for_test()
            .is_some_and(crate::form::Form::is_listing)
    };
    let line = shell
        .row_control_rect_for_test((FAN, LINE), TALL, Scale::ONE, &theme)
        .expect("the line row shows");
    click(&mut shell, line.center(), TALL, &theme);
    assert!(listing(&shell), "the click opened the list");
    shell.adopt_cooling(
        reading(52, Some(FanFault::LineUnavailable)),
        (TALL, Scale::ONE, &theme),
        &mut damage(),
    );
    assert!(listing(&shell), "the list stays open");
    assert_eq!(fan_said(&shell), None, "the reading waits");
    let _ = shell.on_key(
        keystroke(tairix_input::Key::Named(NamedKey::Escape)),
        TALL,
        Scale::ONE,
        &theme,
        &mut damage(),
    );
    assert!(!listing(&shell), "Escape closed it");
    assert!(fan_said(&shell).is_some_and(|said| said.contains("would not lend")));
}

/// A drag let go of outside the pane settles where it was left, so passing
/// back over the slider afterwards moves nothing.
#[test]
fn a_drag_let_go_outside_the_pane_settles_there() {
    let theme = theme();
    let mut shell = tall_pane();
    let start = shell
        .row_control_rect_for_test((FAN, START), TALL, Scale::ONE, &theme)
        .expect("the start row shows");
    let strip = shell
        .frame(TALL, Scale::ONE, &theme)
        .sidebar
        .expect("the window seats the strip")
        .center();
    route(
        &mut shell,
        &[
            InputEvent::PointerMoved { to: start.center() },
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            InputEvent::PointerMoved { to: strip },
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
        ],
    );
    assert_eq!(
        pending(&shell),
        vec![(Key::CoolingFanStart.name().to_string(), "40".to_string())],
        "settled at the start of its track"
    );
    let settled = start_slider(&shell).value();
    route(
        &mut shell,
        &[InputEvent::PointerMoved {
            to: Point::new(start.right() - 1, start.center().y),
        }],
    );
    assert_eq!(
        start_slider(&shell).value(),
        settled,
        "passing back over it moves nothing"
    );
}

/// The line staged is always among the line row's choices and a pick draws
/// them again around it, so the row never shows a line Apply would not write.
#[test]
fn the_line_row_shows_the_line_it_stages() {
    let mut config = SystemConfig::default();
    config
        .set(Key::CoolingFanLine, "14")
        .expect("a header line");
    let mut shell = pi_pane(config);
    let line_row =
        |shell: &Shell| shell.form_for_test().expect("a form").groups()[FAN].rows()[LINE].clone();
    let row = line_row(&shell);
    let FieldControl::Combo(combo) = row.control() else {
        panic!("a choice of lines");
    };
    assert_eq!(combo.selected_text(), Some("GPIO 14 (pin 8)"));
    assert!(
        row.description()
            .is_some_and(|said| said.contains("In use and not offered: GPIO 15 (pin 10).")),
        "{:?}",
        row.description()
    );
    let gpio18 = combo
        .choices()
        .iter()
        .position(|choice| choice == "GPIO 18 (pin 12)")
        .expect("GPIO 18 is free");
    assert!(shell.choose_for_test(FAN, LINE, gpio18));
    let row = line_row(&shell);
    let FieldControl::Combo(combo) = row.control() else {
        panic!("a choice of lines");
    };
    assert_eq!(combo.selected_text(), Some("GPIO 18 (pin 12)"));
    assert!(!combo
        .choices()
        .iter()
        .any(|choice| choice.starts_with("GPIO 14 ")));
    assert!(row
        .description()
        .is_some_and(|said| said.contains("GPIO 14 (pin 8), GPIO 15 (pin 10)")));
    let last = combo.choices().len() - 1;
    assert_eq!(combo.choices()[last], "GPIO 27 (pin 13)");
    assert!(shell.choose_for_test(FAN, LINE, last));
    assert_eq!(
        pending(&shell),
        vec![(Key::CoolingFanLine.name().to_string(), "27".to_string())]
    );
}
