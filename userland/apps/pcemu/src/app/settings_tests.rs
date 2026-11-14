use super::*;
use crate::board::Board;
use crate::cpu::CpuModel;
use crate::machine::Template;
use tairix_controls::FieldLayout;
use tairix_geometry::Point;
use tairix_input::{Key, Modifiers, NamedKey, PointerButton};
use tairix_machines::Library;

fn machine(template: Template) -> Machine {
    let mut library = Library::<Machine>::new();
    let id = library.mint().unwrap();
    library
        .add(Machine {
            id,
            name: String::from("Test"),
            config: template.config(),
            disks: [None, None],
            clock_offset: 0,
            display: tairix_crt::Tube::default(),
        })
        .unwrap();
    library.machines()[0].clone()
}

const TEMPLATES: [Template; 4] = [
    Template::IbmPc,
    Template::IbmXt,
    Template::TurboXt,
    Template::Workstation186,
];

#[test]
fn a_templates_own_choices_make_its_own_configuration() {
    for template in TEMPLATES {
        let machine = machine(template);
        let choices = Choices::of(&machine);
        assert_eq!(
            choices.configured(&machine.config),
            machine.config,
            "{template:?}"
        );
    }
}

#[test]
fn every_choice_of_every_setting_is_one_the_board_takes() {
    for template in TEMPLATES {
        let base = machine(template);
        let start = Choices::of(&base);
        let mut tried = Vec::new();
        let mut each = |change: &dyn Fn(&mut Choices)| {
            let mut choices = start.clone();
            change(&mut choices);
            tried.push(choices.configured(&base.config));
        };
        for n in 0..PROCESSORS.len() {
            each(&|c| c.processor = n);
        }
        for n in 0..SPEEDS.len() {
            each(&|c| c.speed = n);
        }
        for n in 0..MEMORY.len() {
            each(&|c| c.memory = n);
        }
        for n in 0..EXPANDED.len() {
            each(&|c| c.expanded = n);
        }
        for card in 0..CARDS.len() {
            for monitor in 0..monitors(card).len() {
                each(&|c| {
                    c.card = card;
                    c.monitor = monitor;
                });
            }
        }
        for n in 0..SOUNDS.len() {
            each(&|c| c.sound = n);
        }
        for a in 0..DRIVES.len() {
            for b in 0..DRIVES.len() {
                each(&|c| c.drives = [a, b]);
            }
        }
        each(&|c| {
            c.game = !c.game;
            c.clock = !c.clock;
            c.coprocessor = !c.coprocessor;
        });
        for config in tried {
            assert!(Board::new(&config).is_ok(), "{template:?}: {config:?}");
        }
    }
}

/// Where the control of `group`'s `row` is drawn.
fn control_at(settings: &Settings, (group, row): (usize, usize), theme: &Theme) -> Point {
    let (_, place) = settings.form.place(Scale::ONE, theme);
    let layout = settings
        .form
        .column()
        .layouts(place)
        .into_iter()
        .find_map(|(at, layout)| (at == group).then_some(layout))
        .expect("the group is laid out");
    let seated = &settings.form.column().groups()[group];
    let bounds = seated
        .row_rect(row, layout, Scale::ONE, theme)
        .expect("the row is drawn");
    seated.rows()[row]
        .control_rect(FieldLayout { bounds, ..layout }, Scale::ONE, theme)
        .expect("the row seats its control")
        .center()
}

/// Where the open choice list of `group` shows its choice `index`.
fn choice_at(settings: &Settings, group: usize, index: usize, theme: &Theme) -> Point {
    let (_, place) = settings.form.place(Scale::ONE, theme);
    let popup = settings
        .form
        .column()
        .layouts(place)
        .into_iter()
        .find_map(|(at, layout)| (at == group).then_some(layout.popup))
        .expect("the group is laid out");
    assert!(!popup.is_empty(), "the open list is placed");
    let combo = settings.form.column().groups()[group]
        .rows()
        .iter()
        .find_map(|row| match row.control() {
            FieldControl::Combo(combo) if combo.is_expanded() => Some(combo),
            _ => None,
        })
        .expect("a list is open");
    combo
        .menu()
        .row_rect(index, popup, Scale::ONE, theme)
        .expect("the choice is drawn")
        .center()
}

fn pointer(settings: &mut Settings, event: InputEvent, theme: &Theme) -> Option<Answer> {
    settings.on_input(&event, 0, (Scale::ONE, theme), &mut Region::new())
}

fn click(settings: &mut Settings, at: Point, theme: &Theme) -> Option<Answer> {
    let primary = PointerButton::Primary;
    pointer(settings, InputEvent::PointerMoved { to: at }, theme);
    pointer(
        settings,
        InputEvent::PointerPressed { button: primary },
        theme,
    );
    pointer(
        settings,
        InputEvent::PointerReleased { button: primary },
        theme,
    )
}

fn switch_on(settings: &Settings, (group, row): (usize, usize)) -> bool {
    match settings.form.column().groups()[group].rows()[row].control() {
        FieldControl::Toggle(toggle) => toggle.is_on(),
        other => panic!("not a switch: {other:?}"),
    }
}

#[test]
fn a_choice_list_stays_open_under_the_pointer_and_takes_the_choice_clicked() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let machine = machine(Template::IbmXt);
    let mut settings = Settings::new(&machine, Standing::Off);
    let field = control_at(&settings, (at::MACHINE, at::PROCESSOR), &theme);
    click(&mut settings, field, &theme);
    assert!(
        settings.form.column().is_listing(),
        "a click opens the list"
    );
    let v20 = choice_at(&settings, at::MACHINE, 2, &theme);
    pointer(&mut settings, InputEvent::PointerMoved { to: v20 }, &theme);
    assert!(
        settings.form.column().is_listing(),
        "the pointer moving onto the list leaves it open"
    );
    assert_eq!(click(&mut settings, v20, &theme), None);
    assert!(!settings.form.column().is_listing(), "choosing closes it");
    assert_eq!(
        settings.choices().configured(&machine.config).cpu,
        CpuModel::V20
    );
}

#[test]
fn a_press_that_takes_a_list_away_presses_nothing_beneath_it() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let machine = machine(Template::IbmXt);
    let mut settings = Settings::new(&machine, Standing::Off);
    let field = control_at(&settings, (at::PARTS, at::DRIVE_B), &theme);
    click(&mut settings, field, &theme);
    assert!(settings.form.column().is_listing());
    let (bounds, _) = settings.form.place(Scale::ONE, &theme);
    let cancel = settings
        .form
        .dialog()
        .action_rects(bounds, Scale::ONE, &theme)[0]
        .center();
    assert_eq!(
        click(&mut settings, cancel, &theme),
        None,
        "only the list closes"
    );
    assert!(!settings.form.column().is_listing());
    assert_eq!(click(&mut settings, cancel, &theme), Some(Answer::Cancel));
}

#[test]
fn a_switch_shows_what_it_is_set_to_and_a_second_click_turns_it_back() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let machine = machine(Template::IbmXt);
    assert!(!machine.config.coprocessor);
    let mut settings = Settings::new(&machine, Standing::Off);
    for (row, before) in [
        ((at::MACHINE, at::COPROCESSOR), false),
        ((at::PARTS, at::GAME), machine.config.ports.game),
        ((at::PARTS, at::CLOCK), machine.config.ports.clock),
    ] {
        let at = control_at(&settings, row, &theme);
        click(&mut settings, at, &theme);
        assert_eq!(switch_on(&settings, row), !before, "{row:?} turns");
        click(&mut settings, at, &theme);
        assert_eq!(switch_on(&settings, row), before, "{row:?} turns back");
    }
    let coprocessor = control_at(&settings, (at::MACHINE, at::COPROCESSOR), &theme);
    click(&mut settings, coprocessor, &theme);
    let config = settings.choices().configured(&machine.config);
    assert!(config.coprocessor);
    assert_eq!(config.ports.game, machine.config.ports.game);
}

#[test]
fn a_choice_made_is_kept_and_a_new_card_offers_its_own_monitors() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let machine = machine(Template::IbmXt);
    let mut settings = Settings::new(&machine, Standing::Off);
    let card = control_at(&settings, (at::PARTS, at::CARD), &theme);
    click(&mut settings, card, &theme);
    let hercules = choice_at(&settings, at::PARTS, 1, &theme);
    click(&mut settings, hercules, &theme);
    let monitor = control_at(&settings, (at::PARTS, at::MONITOR), &theme);
    click(&mut settings, monitor, &theme);
    let amber = choice_at(&settings, at::PARTS, 1, &theme);
    click(&mut settings, amber, &theme);
    let config = settings.choices().configured(&machine.config);
    assert_eq!(
        config.video,
        [Some(Adapter::Hercules(Phosphor::Amber)), None]
    );
    assert_eq!(
        settings.answer(1),
        Answer::Apply(settings.choices().clone())
    );
}

#[test]
fn a_running_or_suspended_machines_settings_change_nothing() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let machine = machine(Template::TurboXt);
    for standing in [Standing::Saving, Standing::Suspended, Standing::Running] {
        let mut settings = Settings::new(&machine, standing);
        let field = control_at(&settings, (at::MACHINE, at::PROCESSOR), &theme);
        click(&mut settings, field, &theme);
        assert!(
            !settings.form.column().is_listing(),
            "{standing:?}: no list opens"
        );
        let switch = control_at(&settings, (at::MACHINE, at::COPROCESSOR), &theme);
        click(&mut settings, switch, &theme);
        assert_eq!(
            switch_on(&settings, (at::MACHINE, at::COPROCESSOR)),
            machine.config.coprocessor,
            "{standing:?}: no switch turns"
        );
        assert_eq!(settings.choices(), &Choices::of(&machine), "{standing:?}");
        assert_eq!(settings.answer(1), Answer::Cancel, "{standing:?}");
    }
    let mut settings = Settings::new(&machine, Standing::Running);
    assert_eq!(settings.answer(0), Answer::Cancel, "its one button closes");
    let escape = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    };
    assert_eq!(pointer(&mut settings, escape, &theme), Some(Answer::Cancel));
}

#[test]
fn the_dialog_is_as_tall_as_what_it_holds_and_paints() {
    tairix_font::install_test_transport();
    let (scale, theme) = (Scale::ONE, Theme::light());
    let settings = Settings::new(&machine(Template::IbmPc), Standing::Off);
    let (width, height) = settings.extent(scale, &theme);
    assert!(height > 300 && height < 900, "{height}");
    let mut surface = Surface::new(width, height).unwrap();
    settings.render(&mut surface, scale, &theme);
}
