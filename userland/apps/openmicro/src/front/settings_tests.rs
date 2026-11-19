use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{FieldControl, FieldGroupAction};
use tairix_geometry::{Region, Scale};
use tairix_machines::Standing;
use tairix_theme::Theme;

use super::*;
use crate::front::library::{Shipped, Socket};
use crate::machine::c64::{sid, Standard};

fn machine(fitting: Fitting) -> Machine {
    Machine::new(1, String::from("Test"), fitting)
}

fn act(settings: &mut Settings, (group, row): (usize, usize), action: FieldAction) {
    let theme = Theme::light();
    let acted = ColumnAction {
        group,
        action: FieldGroupAction { row, action },
    };
    settings.take(&acted, (Scale::ONE, &theme), &mut Region::new());
}

fn selected(settings: &Settings, (group, row): (usize, usize)) -> Option<usize> {
    match settings.form.column().groups()[group].rows()[row].control() {
        FieldControl::Combo(combo) => combo.selected(),
        other => panic!("not a list: {other:?}"),
    }
}

fn enabled(settings: &Settings, (group, row): (usize, usize)) -> bool {
    settings.form.column().groups()[group].rows()[row]
        .state()
        .enabled
}

/// The ROM row of `socket` in a dialog of `fitting`.
fn row_of(fitting: &Fitting, socket: Socket) -> usize {
    rom_sockets(fitting)
        .iter()
        .position(|(held, _, _)| *held == socket)
        .expect("the socket has a row")
}

#[test]
fn a_model_b_s_slot_takes_what_its_list_offers() {
    tairix_font::install_test_transport();
    let fitting = Fitting::model_b();
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Off, Vec::new());
    let slot = row_of(&fitting, Socket::Slot(0));
    act(
        &mut settings,
        (at::ROMS, slot),
        FieldAction::Selected { index: 1 },
    );
    let fitted = |settings: &Settings| {
        settings
            .choices()
            .fitting
            .sockets()
            .find(|(socket, _)| *socket == Socket::Slot(0))
            .map(|(_, image)| image.clone())
    };
    assert_eq!(fitted(&settings), Some(Image::Ram));
    act(
        &mut settings,
        (at::LINKS, at::MODE),
        FieldAction::Selected { index: 3 },
    );
    act(
        &mut settings,
        (at::LINKS, at::BOOT),
        FieldAction::Selected { index: 1 },
    );
    let Fitting::ModelB { links, .. } = &settings.choices().fitting else {
        panic!("a Model B");
    };
    assert_eq!((links.mode, links.shift_boots), (3, false));
}

/// Choosing *Install…* fits nothing: the socket waits for an image, its row
/// shows what it held, and the image kept is offered everywhere and fitted
/// where it was asked for.
#[test]
fn install_waits_for_an_image_and_fits_it_where_it_was_asked_for() {
    tairix_font::install_test_transport();
    let fitting = Fitting::model_b();
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Off, Vec::new());
    let slot = row_of(&fitting, Socket::Slot(8));
    let install = Socket::Slot(8).offered().len();
    act(
        &mut settings,
        (at::ROMS, slot),
        FieldAction::Selected { index: install },
    );
    assert_eq!(settings.take_install(), Some(Socket::Slot(8)));
    assert_eq!(settings.take_install(), None, "taken once");
    assert_eq!(
        selected(&settings, (at::ROMS, slot)),
        Some(0),
        "still empty"
    );
    let theme = Theme::light();
    settings.installed(
        Socket::Slot(8),
        String::from("rom.viewsheet"),
        (Scale::ONE, &theme),
        &mut Region::new(),
    );
    assert!(settings.choices().fitting.sockets().any(|(socket, image)| {
        socket == Socket::Slot(8) && *image == Image::Installed(String::from("rom.viewsheet"))
    }));
    let other = row_of(&fitting, Socket::Slot(6));
    act(
        &mut settings,
        (at::ROMS, other),
        FieldAction::Selected {
            index: Socket::Slot(6).offered().len(),
        },
    );
    assert_eq!(
        settings.take_install(),
        None,
        "the installed image is listed before Install"
    );
}

#[test]
fn an_electron_s_plus_1_rows_stand_disabled_until_it_is_fitted() {
    tairix_font::install_test_transport();
    let fitting = Fitting::electron(false);
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Off, Vec::new());
    let plus1 = row_of(&fitting, Socket::Plus1);
    assert!(!enabled(&settings, (at::ROMS, plus1)));
    act(
        &mut settings,
        (at::MACHINE, at::PLUS1),
        FieldAction::Set { on: true },
    );
    assert!(enabled(&settings, (at::ROMS, plus1)));
    assert!(settings.choices().fitting.sockets().any(|(socket, image)| {
        socket == Socket::Plus1 && *image == Image::Shipped(Shipped::Plus1)
    }));
    act(
        &mut settings,
        (at::MACHINE, at::PLUS1),
        FieldAction::Set { on: false },
    );
    assert!(!enabled(&settings, (at::ROMS, plus1)));
    assert_eq!(settings.choices().fitting, Fitting::electron(false));
}

/// While the machine is not off its ROMs, links and Plus 1 hold still, and
/// its name, sound and tape loading still change.
#[test]
fn what_is_fitted_is_read_only_unless_the_machine_is_off() {
    tairix_font::install_test_transport();
    let fitting = Fitting::model_b();
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Running, Vec::new());
    let slot = row_of(&fitting, Socket::Slot(0));
    assert!(!enabled(&settings, (at::ROMS, slot)));
    assert!(!enabled(&settings, (at::LINKS, at::MODE)));
    act(
        &mut settings,
        (at::ROMS, slot),
        FieldAction::Selected { index: 1 },
    );
    act(
        &mut settings,
        (at::LINKS, at::MODE),
        FieldAction::Selected { index: 0 },
    );
    assert_eq!(settings.choices().fitting, fitting, "nothing fitted moved");
    let sound = settings.sound_group();
    act(
        &mut settings,
        (sound, at::VOLUME),
        FieldAction::Settled { permille: 400 },
    );
    act(
        &mut settings,
        (sound, at::FAST_TAPE),
        FieldAction::Set { on: false },
    );
    assert_eq!(settings.choices().volume, 400);
    assert!(!settings.choices().fast_loading);
    let electron = Fitting::electron(true);
    let locked = Settings::new(&machine(electron), Standing::Suspended, Vec::new());
    assert!(!enabled(&locked, (at::MACHINE, at::PLUS1)));
}

#[test]
fn every_installed_image_is_offered_by_every_socket() {
    let installed = [String::from("rom.a"), String::from("rom.b")];
    let list = listed(Socket::Basic, &Image::Shipped(Shipped::Basic), &installed);
    assert_eq!(
        list,
        [
            Image::Shipped(Shipped::Basic),
            Image::Installed(String::from("rom.a")),
            Image::Installed(String::from("rom.b")),
        ]
    );
    let missing = Image::Installed(String::from("rom.gone"));
    assert_eq!(
        listed(Socket::Basic, &missing, &installed).last(),
        Some(&missing),
        "the image it holds even if the store has lost it"
    );
}

#[test]
fn a_commodore_s_standard_sid_and_drives_change_while_it_is_off() {
    tairix_font::install_test_transport();
    let fitting = Fitting::c64(Standard::Pal, sid::Model::Mos6581, 1);
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Off, Vec::new());
    let dos9 = row_of(&fitting, Socket::Dos(1));
    assert!(!enabled(&settings, (at::ROMS, dos9)), "device 9 not fitted");
    act(
        &mut settings,
        (at::MACHINE, at::STANDARD),
        FieldAction::Selected { index: 1 },
    );
    act(
        &mut settings,
        (at::MACHINE, at::SID),
        FieldAction::Selected { index: 1 },
    );
    act(
        &mut settings,
        (at::MACHINE, at::DRIVES + 1),
        FieldAction::Set { on: true },
    );
    act(
        &mut settings,
        (at::MACHINE, at::DRIVES),
        FieldAction::Set { on: false },
    );
    assert_eq!(
        settings.choices().fitting,
        Fitting::C64 {
            standard: Standard::Ntsc,
            sid: sid::Model::Mos8580,
            kernal: Image::Shipped(Shipped::C64Kernal),
            basic: Image::Shipped(Shipped::C64Basic),
            character: Image::Shipped(Shipped::C64Character),
            drives: [None, Some(Image::Shipped(Shipped::C1541Dos))],
        }
    );
    assert!(enabled(&settings, (at::ROMS, dos9)), "device 9 fitted now");
    assert!(!enabled(
        &settings,
        (at::ROMS, row_of(&fitting, Socket::Dos(0)))
    ));
    act(
        &mut settings,
        (at::MACHINE, at::JOYSTICK),
        FieldAction::Selected { index: 2 },
    );
    assert_eq!(settings.choices().joystick, Joystick::Off);
}

#[test]
fn a_running_commodore_s_joystick_changes_but_its_fitting_does_not() {
    tairix_font::install_test_transport();
    let fitting = Fitting::c64(Standard::Pal, sid::Model::Mos6581, 1);
    let mut settings = Settings::new(&machine(fitting.clone()), Standing::Running, Vec::new());
    for row in [at::STANDARD, at::SID, at::DRIVES, at::DRIVES + 1] {
        assert!(!enabled(&settings, (at::MACHINE, row)), "row {row}");
    }
    assert!(enabled(&settings, (at::MACHINE, at::JOYSTICK)));
    act(
        &mut settings,
        (at::MACHINE, at::STANDARD),
        FieldAction::Selected { index: 1 },
    );
    act(
        &mut settings,
        (at::MACHINE, at::DRIVES + 1),
        FieldAction::Set { on: true },
    );
    assert_eq!(settings.choices().fitting, fitting, "what is fitted stands");
    act(
        &mut settings,
        (at::MACHINE, at::JOYSTICK),
        FieldAction::Selected { index: 1 },
    );
    assert_eq!(settings.choices().joystick, Joystick::Port1);
}
