use super::*;
use crate::app::library::{Library, StoredDisk};
use crate::dos::setup::default_disk;
use crate::machine::Template;
use crate::storage::hdc::Recording;
use tairix_input::Modifiers;

fn library_of(count: usize) -> Library {
    let made = default_disk(Recording::Rll, 1).unwrap();
    let disk = StoredDisk {
        kind: made.kind,
        format: made.low_level,
    };
    let mut library = Library::new();
    let kinds = [
        ("PC", Template::IbmPc),
        ("XT", Template::IbmXt),
        ("Turbo", Template::TurboXt),
    ];
    for (name, template) in kinds.iter().cycle().take(count) {
        let id = library.mint().unwrap();
        let disks = [template.config().fixed.map(|_| disk.clone()), None];
        let machine = Machine {
            id,
            name: String::from(*name),
            config: template.config(),
            disks,
            clock_offset: 0,
            display: tairix_crt::Tube::default(),
        };
        library.add(machine).unwrap();
    }
    library
}

fn library() -> Library {
    library_of(3)
}

fn theme() -> (Scale, Theme) {
    tairix_font::install_test_transport();
    (Scale::ONE, Theme::light())
}

fn setup() -> (Shelf, Scale, Theme) {
    let (scale, theme) = theme();
    let mut shelf = Shelf::new();
    shelf.set_size((800, 600), (scale, &theme));
    shelf.show(
        library().machines(),
        |id| {
            if id == 2 {
                Standing::Running
            } else {
                Standing::Off
            }
        },
        (scale, &theme),
        &mut Region::new(),
    );
    (shelf, scale, theme)
}

fn feed(shelf: &mut Shelf, event: &InputEvent, now_ns: u64) -> Option<Request> {
    let (scale, theme) = theme();
    let interval = Duration64::from_millis(500);
    shelf.on_input(
        event,
        (now_ns, interval),
        (scale, &theme),
        &mut Region::new(),
    )
}

fn key(shelf: &mut Shelf, key: Key, ctrl: bool) -> Option<Request> {
    let modifiers = Modifiers {
        ctrl,
        ..Modifiers::default()
    };
    feed(shelf, &InputEvent::KeyPressed { key, modifiers }, 0)
}

fn click(shelf: &mut Shelf, at: Point, now_ns: u64) -> Option<Request> {
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
        asked = asked.or(feed(shelf, &event, now_ns));
    }
    asked
}

/// Turn the wheel `detents` toward the end over the cards.
fn wheel(shelf: &mut Shelf, detents: i32) {
    let over = shelf.layout().area.center();
    feed(shelf, &InputEvent::PointerMoved { to: over }, 0);
    let dy = detents * tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    feed(shelf, &InputEvent::PointerScrolled { dx: 0, dy }, 0);
}

#[test]
fn a_machine_is_described_by_its_parts() {
    let library = library();
    assert_eq!(
        describe(&library.machines()[1]),
        "8088 at 4.77 MHz \u{b7} 640 KB \u{b7} CGA \u{b7} 1 hard disk \u{b7} AdLib"
    );
    let mut machine = library.machines()[2].clone();
    machine.disks = [None, None];
    assert!(
        !describe(&machine).contains("hard disk"),
        "{}",
        describe(&machine)
    );
    machine.config.coprocessor = true;
    assert!(
        describe(&machine).contains("MHz + 8087 \u{b7}"),
        "{}",
        describe(&machine)
    );
}

#[test]
fn the_first_machine_is_chosen_and_the_commands_follow_the_choice() {
    let (mut shelf, ..) = setup();
    assert_eq!(shelf.selected(), Some((1, Standing::Off)));
    assert_eq!(key(&mut shelf, Key::Named(NamedKey::Right), false), None);
    assert_eq!(
        shelf.selected(),
        Some((2, Standing::Running)),
        "the running one"
    );
    assert_eq!(
        key(&mut shelf, Key::Named(NamedKey::Delete), false),
        None,
        "a running machine is not deleted under its window"
    );
    assert_eq!(
        key(&mut shelf, Key::Named(NamedKey::Enter), false),
        Some(Request::Start(2))
    );
    key(&mut shelf, Key::Named(NamedKey::Left), false);
    assert_eq!(
        key(&mut shelf, Key::Named(NamedKey::Delete), false),
        Some(Request::Delete(1))
    );
    assert_eq!(key(&mut shelf, Key::Char('n'), true), Some(Request::New));
    key(&mut shelf, Key::Named(NamedKey::Left), false);
    assert_eq!(
        shelf.selected(),
        Some((1, Standing::Off)),
        "no card before the first"
    );
}

#[test]
fn a_suspended_machine_is_started_copied_or_deleted_and_one_being_kept_is_not() {
    let (mut shelf, scale, theme) = setup();
    let library = library();
    let standing = |id| match id {
        1 => Standing::Suspended,
        2 => Standing::Saving,
        _ => Standing::Off,
    };
    shelf.show(
        library.machines(),
        standing,
        (scale, &theme),
        &mut Region::new(),
    );
    assert_eq!(shelf.selected(), Some((1, Standing::Suspended)));
    assert_eq!(
        key(&mut shelf, Key::Named(NamedKey::Enter), false),
        Some(Request::Start(1)),
        "it resumes"
    );
    assert_eq!(
        key(&mut shelf, Key::Named(NamedKey::Delete), false),
        Some(Request::Delete(1))
    );
    key(&mut shelf, Key::Named(NamedKey::Right), false);
    assert_eq!(shelf.selected(), Some((2, Standing::Saving)));
    for refused in [NamedKey::Enter, NamedKey::Delete] {
        assert_eq!(
            key(&mut shelf, Key::Named(refused), false),
            None,
            "{refused:?}"
        );
    }
    let second = shelf.layout().cards[1].center();
    click(&mut shelf, second, 1_000);
    assert_eq!(
        click(&mut shelf, second, 1_100),
        None,
        "nor started twice-clicked"
    );
    let start = shelf.layout().commands[1].center();
    assert_eq!(click(&mut shelf, start, 9_000_000_000), None);
}

#[test]
fn a_click_chooses_a_card_and_a_double_click_starts_it() {
    let (mut shelf, ..) = setup();
    let third = shelf.layout().cards[2].center();
    assert_eq!(click(&mut shelf, third, 1_000), None);
    assert_eq!(shelf.selected(), Some((3, Standing::Off)));
    assert_eq!(
        click(&mut shelf, third, 1_100_000_000),
        None,
        "too slow to pair"
    );
    assert_eq!(
        click(&mut shelf, third, 1_200_000_000),
        Some(Request::Start(3))
    );
    let settings = shelf.layout().commands[2].center();
    assert_eq!(
        click(&mut shelf, settings, 9_000_000_000),
        Some(Request::Settings(3))
    );
}

#[test]
fn the_choice_survives_the_machines_changing_and_none_is_offered_of_none() {
    let (mut shelf, scale, theme) = setup();
    key(&mut shelf, Key::Named(NamedKey::Right), false);
    let mut library = library();
    library.remove(1);
    shelf.show(
        library.machines(),
        |_| Standing::Off,
        (scale, &theme),
        &mut Region::new(),
    );
    assert_eq!(
        shelf.selected(),
        Some((2, Standing::Off)),
        "the chosen machine, now first"
    );
    shelf.show(&[], |_| Standing::Off, (scale, &theme), &mut Region::new());
    assert_eq!(shelf.selected(), None);
    let start = shelf.layout().commands[1].center();
    assert_eq!(
        click(&mut shelf, start, 1),
        None,
        "Start offers nothing with nothing chosen"
    );
    let new = shelf.layout().commands[0].center();
    assert_eq!(click(&mut shelf, new, 9_000_000_000), Some(Request::New));
}

#[test]
fn every_card_lies_inside_the_window_and_none_overlaps_the_next() {
    let (shelf, scale, theme) = setup();
    let layout = shelf.layout();
    for pair in layout.cards.windows(2) {
        assert!(pair[0].intersection(&pair[1]).is_empty(), "{pair:?}");
    }
    assert!(layout.cards.iter().all(|card| card.right() <= 800));
    assert_eq!(layout.bar, None, "three cards need no scrolling");
    let (w, h) = Shelf::picture_size(scale, &theme).unwrap();
    assert!(w > h, "a screen is wider than tall: {w}x{h}");
    let drawn = shelf.cards[0]
        .preview
        .thumbnail_bounds(layout.cards[0], scale, &theme);
    assert_eq!(
        (drawn.width, drawn.height),
        (w, h),
        "the size every card draws"
    );
    let mut surface = Surface::new(800, 600).unwrap();
    let blank = Surface::new(w, h).unwrap();
    shelf.render(&mut surface, (scale, &theme), |_| Some(&blank));
}

#[test]
fn cards_past_the_window_scroll_into_it_by_wheel_and_by_choice() {
    let (scale, theme) = theme();
    let mut shelf = Shelf::new();
    shelf.set_size((800, 600), (scale, &theme));
    shelf.show(
        library_of(20).machines(),
        |_| Standing::Off,
        (scale, &theme),
        &mut Region::new(),
    );
    let area = shelf.layout().area;
    let bar = shelf.layout().bar.expect("twenty cards overflow");
    assert!(bar.left() >= area.right(), "the bar is beside the cards");
    let last = *shelf.layout().cards.last().unwrap();
    assert!(
        last.bottom() > area.bottom(),
        "the last card is out of sight"
    );

    wheel(&mut shelf, 3);
    let moved = shelf.layout().cards[0].top();
    assert!(
        moved < area.top(),
        "the wheel scrolls the cards up: {moved}"
    );

    for _ in 0..20 {
        key(&mut shelf, Key::Named(NamedKey::Right), false);
    }
    assert_eq!(shelf.selected().map(|(id, _)| id), Some(20));
    let chosen = *shelf.layout().cards.last().unwrap();
    assert!(
        chosen.top() >= area.top() && chosen.bottom() <= area.bottom(),
        "the chosen card is brought wholly into view: {chosen:?} in {area:?}"
    );

    shelf.set_size((800, 4_000), (scale, &theme));
    assert_eq!(
        shelf.layout().bar,
        None,
        "a window tall enough needs no scrolling"
    );
    assert_eq!(
        shelf.layout().cards[0].top(),
        area.top(),
        "and is scrolled back"
    );
}

#[test]
fn a_press_on_a_card_hidden_under_the_commands_chooses_nothing() {
    let (scale, theme) = theme();
    let mut shelf = Shelf::new();
    shelf.set_size((800, 600), (scale, &theme));
    shelf.show(
        library_of(20).machines(),
        |_| Standing::Off,
        (scale, &theme),
        &mut Region::new(),
    );
    wheel(&mut shelf, 2);
    let area = shelf.layout().area;
    let hidden = shelf.layout().cards[1];
    assert!(hidden.top() < area.top(), "{hidden:?} {area:?}");
    let above = Point::new(hidden.center().x, area.top() - 2);
    assert!(
        hidden.contains(above),
        "the card reaches under the commands"
    );
    click(&mut shelf, above, 1);
    assert_eq!(
        shelf.selected().map(|(id, _)| id),
        Some(1),
        "the first stays chosen"
    );
}

#[test]
fn showing_the_machines_again_damages_only_the_cards_that_changed() {
    let (mut shelf, scale, theme) = setup();
    let library = library();
    let mut damage = Region::new();
    shelf.show(
        library.machines(),
        |id| {
            if id == 3 {
                Standing::Running
            } else {
                Standing::Off
            }
        },
        (scale, &theme),
        &mut damage,
    );
    let cards = &shelf.layout().cards;
    assert!(damage.intersects(cards[1]), "the machine that stopped");
    assert!(damage.intersects(cards[2]), "the machine that started");
    assert!(!damage.intersects(cards[0]), "the one that did neither");
    assert!(damage.intersects(shelf.layout().status));
    assert_eq!(shelf.card_bounds(2), Some(cards[1]));
    assert_eq!(shelf.card_bounds(99), None);

    let mut fewer = library.clone();
    fewer.remove(3);
    let mut damage = Region::new();
    shelf.show(
        fewer.machines(),
        |_| Standing::Off,
        (scale, &theme),
        &mut damage,
    );
    let area = shelf.layout().area;
    assert!(
        damage
            .rects()
            .iter()
            .any(|rect| rect.contains(area.center())),
        "a machine gone redraws the cards"
    );
}

#[test]
fn a_right_press_on_a_card_chooses_it_and_asks_for_its_menu_there() {
    let (mut shelf, ..) = setup();
    let second = shelf.layout().cards[1].center();
    feed(&mut shelf, &InputEvent::PointerMoved { to: second }, 0);
    let asked = feed(
        &mut shelf,
        &InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        },
        0,
    );
    let id = shelf
        .selected()
        .map(|(id, _)| id)
        .expect("a card is chosen");
    assert_eq!(
        asked,
        Some(Request::Menu { id, at: second }),
        "the card the press is on, and the press's place"
    );
    assert_eq!(
        shelf.selected(),
        Some((id, Standing::Running)),
        "the second card, whatever it runs"
    );
    let off_the_cards = Point::new(2, 2);
    feed(
        &mut shelf,
        &InputEvent::PointerMoved { to: off_the_cards },
        0,
    );
    assert_eq!(
        feed(
            &mut shelf,
            &InputEvent::PointerPressed {
                button: PointerButton::Secondary,
            },
            0,
        ),
        None
    );
}
