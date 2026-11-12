use super::*;
use crate::machine::Template;

fn looks() -> (Scale, Theme) {
    tairix_font::install_test_transport();
    (Scale::ONE, Theme::light())
}

/// A turbo XT's bar, two diskette drives and one fixed disk, along a
/// 900-pixel window.
fn bar() -> (MachineBar, Rect) {
    let (scale, theme) = looks();
    let mut bar = MachineBar::new(&Template::TurboXt.config(), [true, false]);
    let height = MachineBar::height(scale, &theme);
    let bounds = Rect::new(0, 600, 900, height);
    bar.place(bounds, (scale, &theme));
    (bar, bounds)
}

fn click(bar: &mut MachineBar, at: Point) -> Option<MenuRequest> {
    let mut asked = None;
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        asked = asked.or(bar.on_input(&event));
    }
    asked
}

#[test]
fn every_fitted_drive_has_a_slot_along_the_bar_and_none_overlaps() {
    let (bar, bounds) = bar();
    let kinds: Vec<(Kind, u8)> = bar
        .drives
        .iter()
        .map(|drive| (drive.kind, drive.unit))
        .collect();
    assert_eq!(
        kinds,
        [(Kind::Diskette, 0), (Kind::Diskette, 1), (Kind::Fixed, 0)]
    );
    for pair in bar.drives.windows(2) {
        assert!(pair[0].slot.intersection(&pair[1].slot).is_empty());
    }
    assert!(bar
        .drives
        .iter()
        .all(|drive| drive.slot.right() <= bounds.right()));
    assert_eq!(label(Kind::Diskette, 1), "B:");
    assert_eq!(label(Kind::Diskette, 2), "Diskette 3");
    assert_eq!(label(Kind::Fixed, 1), "D:");
}

#[test]
fn a_narrow_window_shares_what_room_there_is() {
    let (scale, theme) = looks();
    let mut bar = MachineBar::new(&Template::TurboXt.config(), [true, true]);
    let bounds = Rect::new(0, 0, 320, MachineBar::height(scale, &theme));
    bar.place(bounds, (scale, &theme));
    assert!(
        bar.drives.iter().all(|drive| drive.slot.right() <= 320),
        "{:?}",
        bar.drives
    );
}

#[test]
fn a_lamp_that_changes_is_redrawn_and_one_that_does_not_is_left() {
    let (mut bar, _) = bar();
    let mut damage = Region::new();
    bar.set_lights(
        Lights {
            floppy: 0,
            fixed: 1,
        },
        &mut damage,
    );
    assert!(damage.intersects(bar.drives[2].slot), "C: lit");
    assert!(!damage.intersects(bar.drives[0].slot), "A: as it was");
    let mut damage = Region::new();
    bar.set_lights(
        Lights {
            floppy: 0,
            fixed: 1,
        },
        &mut damage,
    );
    assert!(damage.is_empty(), "nothing changed");
}

#[test]
fn a_diskette_drive_asks_for_its_menu_and_a_fixed_disk_asks_nothing() {
    let (mut bar, _) = bar();
    let a = bar.drives[0].slot;
    assert_eq!(
        click(&mut bar, a.center()),
        Some(MenuRequest {
            menu: Menu::Drive {
                unit: 0,
                holds: false
            },
            anchor: a,
        })
    );
    bar.set_held(
        1,
        Some((String::from("GAMES.IMG"), true)),
        &mut Region::new(),
    );
    let b = bar.drives[1].slot;
    assert!(matches!(
        click(&mut bar, b.center()),
        Some(MenuRequest {
            menu: Menu::Drive {
                unit: 1,
                holds: true
            },
            ..
        })
    ));
    let c = bar.drives[2].slot.center();
    assert_eq!(click(&mut bar, c), None);
    bar.on_input(&InputEvent::PointerMoved { to: a.center() });
    bar.on_input(&InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    assert!(bar.is_pressing(), "its release is the bar's");
    bar.on_input(&InputEvent::PointerMoved { to: b.center() });
    assert_eq!(
        bar.on_input(&InputEvent::PointerReleased {
            button: PointerButton::Primary
        }),
        None,
        "let go over another drive"
    );
    assert!(!bar.is_pressing());
}

#[test]
fn a_drop_goes_to_the_diskette_drive_under_it_or_else_the_first() {
    let (bar, bounds) = bar();
    assert_eq!(bar.diskette_at(bar.drives[1].slot.center()), Some(1));
    assert_eq!(
        bar.diskette_at(Point::new(10, 10)),
        Some(0),
        "over the picture"
    );
    let mut config = Template::IbmPc.config();
    config.floppy = None;
    let none = MachineBar::new(&config, [false, false]);
    assert_eq!(none.diskette_at(bounds.center()), None);
}

#[test]
fn the_bar_paints_what_it_holds() {
    let (scale, theme) = looks();
    let (mut bar, bounds) = bar();
    bar.set_held(
        0,
        Some((String::from("DOS.IMA"), false)),
        &mut Region::new(),
    );
    bar.set_lights(
        Lights {
            floppy: 1,
            fixed: 1,
        },
        &mut Region::new(),
    );
    let mut surface = Surface::new(900, u32::try_from(bounds.bottom()).unwrap()).unwrap();
    bar.render(&mut surface, (scale, &theme));
    let lamp = bar.drives[0].slot;
    let lit = Color::from(theme.palette().success).premultiply();
    let found = (lamp.top()..lamp.bottom()).any(|y| {
        (lamp.left()..lamp.left() + 30)
            .any(|x| surface.get(u32::try_from(x).unwrap(), u32::try_from(y).unwrap()) == Some(lit))
    });
    assert!(found, "A:'s lamp is lit green");
}

#[test]
fn the_machines_own_slot_ends_the_bar_and_asks_for_its_menu() {
    let (mut bar, bounds) = bar();
    let machine = bar.machine;
    assert_eq!(machine.right(), bounds.right());
    assert!(bar
        .drives
        .iter()
        .all(|drive| drive.slot.right() <= machine.left()));
    assert_eq!(
        click(&mut bar, machine.center()),
        Some(MenuRequest {
            menu: Menu::Machine,
            anchor: machine
        })
    );
}

#[test]
fn a_paused_machine_says_so_on_its_slot() {
    let (mut bar, _) = bar();
    let mut damage = Region::new();
    bar.set_paused(true, &mut damage);
    assert!(damage.intersects(bar.machine));
    let mut damage = Region::new();
    bar.set_paused(true, &mut damage);
    assert!(damage.is_empty(), "already said");
}

#[test]
fn a_machine_being_kept_says_so_and_asks_for_nothing() {
    let (mut bar, _) = bar();
    let mut damage = Region::new();
    bar.set_saving(&mut damage);
    assert!(damage.intersects(bar.machine));
    let (machine, a) = (bar.machine.center(), bar.drives[0].slot.center());
    assert_eq!(click(&mut bar, machine), None);
    assert_eq!(click(&mut bar, a), None);
}

fn shared(name: &str, drive: Option<u8>, read_only: bool) -> Shared {
    Shared {
        name: String::from(name),
        drive,
        read_only,
    }
}

#[test]
fn a_share_takes_a_slot_after_the_drives_and_asks_for_its_own_menu() {
    let (scale, theme) = looks();
    let (mut bar, bounds) = bar();
    let mut damage = Region::new();
    bar.set_share(
        1,
        Some(shared("WORK", None, false)),
        (scale, &theme),
        &mut damage,
    );
    bar.set_share(
        0,
        Some(shared("HOST", Some(3), true)),
        (scale, &theme),
        &mut damage,
    );
    assert!(damage.rects().contains(&bounds), "laid out again");
    let kinds: Vec<(Kind, u8)> = bar
        .drives
        .iter()
        .map(|drive| (drive.kind, drive.unit))
        .collect();
    assert_eq!(
        kinds,
        [
            (Kind::Diskette, 0),
            (Kind::Diskette, 1),
            (Kind::Fixed, 0),
            (Kind::Share(Some(3)), 0),
            (Kind::Share(None), 1)
        ],
        "in slot order, after the machine's own drives"
    );
    for pair in bar.drives.windows(2) {
        assert!(pair[0].slot.intersection(&pair[1].slot).is_empty());
    }
    assert!(bar
        .drives
        .iter()
        .all(|drive| drive.slot.right() <= bar.machine.left()));
    let share = bar.drives[3].slot;
    let asked = click(&mut bar, Point::new(share.left() + 4, share.top() + 2));
    assert_eq!(
        asked,
        Some(MenuRequest {
            menu: Menu::Share { slot: 0 },
            anchor: share
        })
    );
    let mut damage = Region::new();
    bar.set_share(0, None, (scale, &theme), &mut damage);
    assert_eq!(bar.drives.len(), 4, "no longer shared");
    bar.set_share(0, None, (scale, &theme), &mut damage);
    assert_eq!(
        bar.drives.len(),
        4,
        "stopping a share not there changes nothing"
    );
}

#[test]
fn a_share_says_its_drive_or_that_it_has_none() {
    let (scale, theme) = looks();
    let (mut bar, _) = bar();
    let mut damage = Region::new();
    bar.set_share(
        0,
        Some(shared("HOST", Some(3), false)),
        (scale, &theme),
        &mut damage,
    );
    bar.set_share(
        1,
        Some(shared("KEPT", Some(4), true)),
        (scale, &theme),
        &mut damage,
    );
    bar.set_share(
        2,
        Some(shared("WAITING", None, false)),
        (scale, &theme),
        &mut damage,
    );
    let shares: Vec<(String, StatusTone)> = bar.drives[3..].iter().map(shown).collect();
    assert_eq!(
        shares,
        [
            (String::from("D: HOST"), StatusTone::Ink),
            (String::from("E: KEPT (read-only)"), StatusTone::Ink),
            (String::from("WAITING (no drive)"), StatusTone::Muted)
        ]
    );
    let lights = Lights {
        floppy: 0xF,
        fixed: 0x3,
    };
    assert!(
        bar.drives[3..].iter().all(|drive| !lit(lights, drive)),
        "a share has no lamp"
    );
}
