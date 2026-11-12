use super::*;
use crate::app::library::Library;
use crate::board::Board;
use crate::machine::Template;
use tairix_input::Modifiers;

fn machine(template: Template) -> Machine {
    let mut library = Library::new();
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

#[test]
fn the_coprocessor_toggle_fits_or_removes_the_8087() {
    tairix_font::install_test_transport();
    let (scale, theme) = (Scale::ONE, Theme::light());
    let machine = machine(Template::IbmXt);
    assert!(!machine.config.coprocessor);
    let mut settings = Settings::new(&machine, Standing::Off);
    let (bounds, _) = settings.layout(scale, &theme);
    let fit = FieldGroupAction {
        row: at::COPROCESSOR,
        action: FieldAction::Set { on: true },
    };
    settings.take(at::MACHINE, &fit, bounds, &mut Region::new());
    assert!(settings.choices().configured(&machine.config).coprocessor);
}

#[test]
fn a_choice_made_is_kept_and_a_new_card_offers_its_own_monitors() {
    tairix_font::install_test_transport();
    let (scale, theme) = (Scale::ONE, Theme::light());
    let machine = machine(Template::IbmXt);
    let mut settings = Settings::new(&machine, Standing::Off);
    let (bounds, _) = settings.layout(scale, &theme);
    let mut damage = Region::new();
    let mono = FieldGroupAction {
        row: at::CARD,
        action: FieldAction::Selected { index: 1 },
    };
    settings.take(at::PARTS, &mono, bounds, &mut damage);
    let offered = settings.groups[at::PARTS].rows()[at::MONITOR]
        .control()
        .clone();
    assert!(matches!(offered, FieldControl::Combo(_)));
    let amber = FieldGroupAction {
        row: at::MONITOR,
        action: FieldAction::Selected { index: 1 },
    };
    settings.take(at::PARTS, &amber, bounds, &mut damage);
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
    let machine = machine(Template::TurboXt);
    for standing in [Standing::Saving, Standing::Suspended] {
        let mut settings = Settings::new(&machine, standing);
        let (bounds, _) = settings.layout(Scale::ONE, &Theme::light());
        let changed = FieldGroupAction {
            row: at::PROCESSOR,
            action: FieldAction::Selected { index: 0 },
        };
        settings.take(at::MACHINE, &changed, bounds, &mut Region::new());
        assert_eq!(settings.choices(), &Choices::of(&machine), "{standing:?}");
        assert_eq!(settings.answer(1), Answer::Cancel, "{standing:?}");
    }
    let mut settings = Settings::new(&machine, Standing::Running);
    let (bounds, _) = settings.layout(Scale::ONE, &Theme::light());
    let mut damage = Region::new();
    let changed = FieldGroupAction {
        row: at::PROCESSOR,
        action: FieldAction::Selected { index: 0 },
    };
    settings.take(at::MACHINE, &changed, bounds, &mut damage);
    assert_eq!(settings.choices(), &Choices::of(&machine));
    assert_eq!(settings.answer(0), Answer::Cancel, "its one button closes");
    let mut damage = Region::new();
    let escape = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    };
    assert_eq!(
        settings.on_input(&escape, 0, (Scale::ONE, &Theme::light()), &mut damage),
        Some(Answer::Cancel)
    );
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
