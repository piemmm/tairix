use alloc::string::ToString;

use tairix_theme::ThemeRegistry;

use super::*;

fn task(context: Context, line: &str) -> Task {
    Task {
        context,
        line: line.to_string(),
        slot: 640 << 10,
        screen: 0,
        paused: false,
        busy: false,
    }
}

fn labels(tasks: &Tasks) -> Vec<String> {
    tasks
        .buttons
        .iter()
        .map(|(_, button)| match button.content() {
            ButtonContent::Label(text) => text.clone(),
            _ => String::new(),
        })
        .collect()
}

fn enabled(tasks: &Tasks, act: Act) -> bool {
    tasks
        .buttons
        .iter()
        .find(|(was, _)| *was == act)
        .is_some_and(|(_, button)| button.state() == ControlState::default())
}

/// A primary click at `at`.
fn click(tasks: &mut Tasks, at: Point) -> Option<Command> {
    let themes = ThemeRegistry::with_builtins();
    let looks = (Scale::ONE, themes.active());
    let mut damage = Region::new();
    let mut acted = None;
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        acted = acted.or(tasks.on_input(&event, looks, &mut damage));
    }
    acted
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.origin.x + at(rect.width / 2),
        rect.origin.y + at(rect.height / 2),
    )
}

fn placed(tasks: &Tasks, act: Act) -> Rect {
    let themes = ThemeRegistry::with_builtins();
    let layout = tasks.layout(Scale::ONE, themes.active());
    let index = tasks
        .buttons
        .iter()
        .position(|(was, _)| *was == act)
        .unwrap();
    layout.button(index, &tasks.buttons, Scale::ONE, themes.active())
}

fn row(tasks: &Tasks, row: usize) -> Rect {
    let themes = ThemeRegistry::with_builtins();
    tasks.layout(Scale::ONE, themes.active()).row(row)
}

/// A click on the button that does `act`.
fn press(tasks: &mut Tasks, act: Act) -> Option<Command> {
    let at = centre(placed(tasks, act));
    click(tasks, at)
}

/// A click on the task shown at `shown`.
fn choose(tasks: &mut Tasks, shown: usize) {
    let at = centre(row(tasks, shown));
    click(tasks, at);
}

#[test]
fn a_command_line_is_shown_as_latin_1_held_to_what_may_be_drawn() {
    assert_eq!(shown(b"Run \xA3Notes"), "Run \u{A3}Notes");
    assert_eq!(shown(b"a\x1Bb"), "a\u{FFFD}b", "a control character");
}

#[test]
fn tasks_come_and_go_with_their_memory_and_pausing() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    tasks.launched(task(1, "BASIC"));
    tasks.launched(task(2, "Run Transfer:$.1.Game"));
    tasks.own(2, 1 << 20, 80 << 10);
    tasks.paused(2, true);
    assert_eq!(tasks.tasks()[1].slot, 1 << 20);
    assert_eq!(tasks.tasks()[1].screen, 80 << 10);
    assert!(tasks.tasks()[1].paused);
    assert!(tasks.is_paused(2));
    assert!(!tasks.is_paused(1));
    assert!(!tasks.is_paused(9), "an unknown context");
    tasks.ended(1);
    assert_eq!(tasks.tasks().len(), 1);
    assert_eq!(tasks.tasks()[0].context, 2);
    tasks.own(9, 1, 1);
    tasks.paused(9, true);
    assert_eq!(tasks.tasks().len(), 1, "an unknown context changes nothing");
}

#[test]
fn a_task_whose_program_shows_its_hourglass_is_busy_until_it_ends() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    tasks.launched(task(1, "BASIC"));
    tasks.launched(task(2, "Run Transfer:$.Game"));
    assert!(!tasks.any_busy());
    tasks.busy(2, true);
    tasks.busy(9, true);
    assert!(tasks.any_busy());
    assert_eq!(state(&tasks.tasks()[0]), "Running");
    assert_eq!(state(&tasks.tasks()[1]), "Busy");
    tasks.paused(2, true);
    assert_eq!(state(&tasks.tasks()[1]), "Paused", "held, whatever it says");
    assert!(!tasks.any_busy(), "a task held is not busy");
    tasks.paused(2, false);
    assert!(tasks.any_busy());
    tasks.ended(2);
    assert!(!tasks.any_busy(), "its hourglass goes with it");
    tasks.busy(1, true);
    tasks.clear_busy();
    assert!(!tasks.any_busy(), "none once its worker has gone");
}

#[test]
fn the_newest_notes_are_kept_and_the_window_stops_growing() {
    let themes = ThemeRegistry::with_builtins();
    let looks = (Scale::ONE, themes.active());
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    for index in 0..NOTES_MOST {
        tasks.note(format!("note {index}"));
    }
    let full = tasks.extent(looks.0, looks.1);
    for index in NOTES_MOST..3 * NOTES_MOST {
        tasks.note(format!("note {index}"));
    }
    assert_eq!(tasks.extent(looks.0, looks.1), full, "never taller");
    let newest: Vec<String> = (2 * NOTES_MOST..3 * NOTES_MOST)
        .map(|index| format!("note {index}"))
        .collect();
    assert!(tasks.notes.iter().eq(newest.iter()));
}

#[test]
fn the_boot_s_first_failures_are_said_and_the_rest_counted_once_it_ends() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    for index in 0..BOOT_SAID_MOST + 3 {
        tasks.boot_failed(&format!("Obey {index}"), "File not found");
    }
    tasks.boot_omitted(10);
    assert_eq!(
        tasks.notes.len(),
        BOOT_SAID_MOST,
        "the rest counted meanwhile"
    );
    tasks.boot_ended();
    assert_eq!(tasks.notes[0], "Obey 0: File not found");
    assert_eq!(
        tasks.notes.back().map(String::as_str),
        Some("and 13 more failures")
    );
    tasks.boot_ended();
    assert_eq!(tasks.notes.len(), BOOT_SAID_MOST + 1, "said once");

    let mut quiet = Tasks::new(Kind::Isolated, 32 << 20, false);
    quiet.boot_failed("Obey", "Bad command");
    quiet.boot_ended();
    assert_eq!(quiet.notes.len(), 1, "nothing more to count");
}

#[test]
fn the_shared_areas_are_stated_once_told_beside_the_ram() {
    let mut tasks = Tasks::new(Kind::Isolated, 32 << 20, false);
    assert_eq!(tasks.facts().len(), 1, "the RAM alone");
    tasks.shared(Shared {
        system_heap: 32 << 10,
        rma: 64 << 10,
        sprites: 0,
        font_cache: 0,
        ram_disc: 0,
        made: 0,
        free: 30 << 20,
    });
    let facts = tasks.facts();
    assert_eq!(facts.len(), 8);
    assert_eq!(facts[0], ("RAM", size(32 << 20)));
    assert_eq!(facts[7], ("Free", size(30 << 20)));
    assert_eq!(tasks.title(), "TAIRiX Bridge — isolated session");
}

#[test]
fn the_shared_session_command_is_offered_only_where_allowed() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    assert!(labels(&tasks).contains(&String::from("Run in Shared Session…")));
    tasks.offer_shared(false);
    assert!(!labels(&tasks).contains(&String::from("Run in Shared Session…")));
}

#[test]
fn a_task_is_chosen_by_its_row_and_pausing_and_quitting_act_on_it() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    tasks.launched(task(1, "BASIC"));
    tasks.launched(task(2, "Run Transfer:$.1.Game"));
    assert!(!enabled(&tasks, Act::Quit), "no task chosen");
    assert_eq!(press(&mut tasks, Act::Quit), None);
    choose(&mut tasks, 1);
    assert!(enabled(&tasks, Act::Quit));
    assert_eq!(press(&mut tasks, Act::Pause), Some(Command::Pause(2, true)));
    tasks.paused(2, true);
    assert!(labels(&tasks).contains(&String::from("Resume")));
    assert_eq!(
        press(&mut tasks, Act::Pause),
        Some(Command::Pause(2, false))
    );
    assert_eq!(press(&mut tasks, Act::Quit), Some(Command::Quit(2)));
    tasks.ended(2);
    assert!(!enabled(&tasks, Act::Quit), "its task gone");
}

#[test]
fn the_window_s_own_commands_ask_their_way() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    for (act, command) in [
        (Act::Run, Command::Run),
        (Act::RunShared, Command::RunShared),
        (Act::Settings, Command::Settings),
    ] {
        assert_eq!(press(&mut tasks, act), Some(command));
    }
}

#[test]
fn many_tasks_are_scrolled_to() {
    let mut tasks = Tasks::new(Kind::Shared, 64 << 20, true);
    for context in 1..=20 {
        tasks.launched(task(context, "BASIC"));
    }
    assert_eq!(tasks.shown_tasks().count(), SHOWN_MOST);
    let themes = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    for _ in 0..30 {
        tasks.on_input(
            &InputEvent::PointerScrolled { dx: 0, dy: 1 },
            (Scale::ONE, themes.active()),
            &mut damage,
        );
    }
    assert_eq!(tasks.top, 20 - SHOWN_MOST, "held to the last");
    assert_eq!(
        tasks.shown_tasks().last().map(|task| task.context),
        Some(20)
    );
    for context in 1..=20 {
        tasks.ended(context);
    }
    assert_eq!(tasks.top, 0);
}
