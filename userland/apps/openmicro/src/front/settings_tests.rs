use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{FieldControl, FieldGroupAction};
use tairix_geometry::{Region, Scale};
use tairix_machines::Standing;
use tairix_theme::Theme;

use super::*;
use crate::front::library::{Shipped, Socket};

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
    let slot = row_of(&fitting, Socket::Slot(7));
    let install = Socket::Slot(7).offered().len();
    act(
        &mut settings,
        (at::ROMS, slot),
        FieldAction::Selected { index: install },
    );
    assert_eq!(settings.take_install(), Some(Socket::Slot(7)));
    assert_eq!(settings.take_install(), None, "taken once");
    assert_eq!(
        selected(&settings, (at::ROMS, slot)),
        Some(0),
        "still empty"
    );
    let theme = Theme::light();
    settings.installed(
        Socket::Slot(7),
        String::from("rom.viewsheet"),
        (Scale::ONE, &theme),
        &mut Region::new(),
    );
    assert!(settings.choices().fitting.sockets().any(|(socket, image)| {
        socket == Socket::Slot(7) && *image == Image::Installed(String::from("rom.viewsheet"))
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
    assert!(!settings.choices().fast_tape);
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
