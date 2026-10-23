//! Tests for General: the machine readings, the staged-apply model, and the
//! system task an apply asks the desktop for.
//!
//! The desktop is injected by driving the shell's own outcome — the shell
//! asks for a task and adopts what it is told came of it — so the whole
//! staged model is exercised with no desktop, no account database, and no
//! spawn anywhere near it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::sysinfo::{SystemIdentity, Uptime};
use tairix_abi::time::{Duration64, Time64, WallClockReading, WallTimeState};
use tairix_geometry::{to_i32, Point, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::Surface;
use tairix_sysconfig::{CacheMode, LoginType, Switch, SystemConfig};
use tairix_wallpaper::DesktopSettings;

use crate::facts::MachineFacts;
use crate::registry::Board;
use crate::shell::{Authorised, Shell, ShellOutcome};
use crate::test_support::{band_line, damage, opaque, stated, task_words, theme, WIDE};
use tairix_abi::window_ipc::SystemTask;

/// A shell showing `pane`, with the machine's store already read.
fn showing_with(pane: &str, config: SystemConfig) -> Shell {
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
    shell.adopt_config(Some(config));
    let mut sink = damage();
    assert!(
        shell.go_to_pane(pane, WIDE, Scale::ONE, &theme(), &mut sink),
        "the registry carries the pane"
    );
    shell.lay_out(WIDE, Scale::ONE, &theme());
    shell
}

/// Choose the second value of group `group`'s row `row`.
fn choose_next(shell: &mut Shell, group: usize, row: usize) {
    assert!(
        shell.choose_for_test(group, row, 1),
        "a machine row stages its change"
    );
}

/// Press the band's command at `index` (`0` Revert, `1` Apply), where the
/// band actually drew it.
fn press_command(shell: &mut Shell, index: usize) -> ShellOutcome {
    let theme = theme();
    let rects = shell.action_rects(WIDE, Scale::ONE, &theme);
    let rect = *rects.get(index).expect("the band drew its commands");
    let at = Point::new(
        rect.left() + to_i32(rect.width / 2),
        rect.top() + to_i32(rect.height / 2),
    );
    let mut sink = damage();
    let mut concluded = ShellOutcome::Idle;
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        let acted = shell.on_pointer(&event, WIDE, Scale::ONE, &theme, &mut sink);
        if acted.changed() {
            concluded = acted;
        }
    }
    concluded
}

/// Press the band's acting command, which is always its trailing one —
/// a staged band's Apply, or the single command a pane with no working
/// copy offers.
fn press_action(shell: &mut Shell) -> ShellOutcome {
    let last = shell
        .action_rects(WIDE, Scale::ONE, &theme())
        .len()
        .saturating_sub(1);
    press_command(shell, last)
}

/// Press the band's acting command and hand back the task it asked the
/// desktop for.
fn apply(shell: &mut Shell) -> SystemTask {
    let ShellOutcome::Authorise(task) = press_action(shell) else {
        panic!("the band asks the desktop for a task");
    };
    *task
}

// --- The staged model ---------------------------------------------------

#[test]
fn a_staged_row_edits_a_working_copy_and_asks_for_nothing() {
    // The whole point of a staged pane: a choice is not a write, so nothing
    // is asked of the desktop and no store is touched until Apply.
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    assert!(!shell.authorising(), "a choice asks for nothing");
    let form = shell.form_for_test().expect("a composed pane");
    assert_eq!(
        form.pending(),
        alloc::vec![(
            tairix_sysconfig::Key::LoginType.name().to_string(),
            "text".to_string()
        )]
    );
}

#[test]
fn reverting_puts_the_working_copy_back() {
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    assert_eq!(shell.form_for_test().expect("a form").pending().len(), 1);

    // The leading command in the band is Revert.
    press_command(&mut shell, 0);
    assert!(shell.form_for_test().expect("a form").pending().is_empty());
}

#[test]
fn applying_asks_the_desktop_to_configure_every_staged_key_at_once() {
    let mut shell = showing_with("caching", SystemConfig::default());
    // Two rows, so the one task carries both changes together.
    choose_next(&mut shell, 0, 0);
    choose_next(&mut shell, 1, 0);
    assert_eq!(shell.form_for_test().expect("a form").pending().len(), 2);

    let task = apply(&mut shell);
    assert!(shell.authorising(), "the desktop is asked");
    assert!(matches!(task, SystemTask::Configure(_)));
    // One invocation carrying every changed key, so the document is
    // rendered once and cannot be left half written.
    assert_eq!(
        task_words(&task),
        ["cache.all", "off", "cache.filesystem", "off"]
    );
}

#[test]
fn a_task_the_desktop_would_not_ask_keeps_the_working_copy() {
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);

    shell.adopt_authorisation(Authorised::Refused(String::from(
        "Something else holds the screen. Try again once it has gone.",
    )));
    assert!(!shell.authorising());
    assert!(band_line(&shell).contains("Something else holds the screen"));
    // The working copy stands, so the reader applies again rather than
    // restaging the change.
    assert_eq!(shell.form_for_test().expect("a form").pending().len(), 1);
}

#[test]
fn a_task_that_ran_re_reads_the_store() {
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);

    shell.adopt_authorisation(Authorised::Ran(0));
    assert!(!shell.authorising());
    // Persist-then-adopt: what is durable is whatever the store now says,
    // so the window asks for it again rather than declaring its own
    // working copy the truth.
    assert!(shell.config_wanted());

    shell.adopt_config(Some(SystemConfig {
        login_type: LoginType::Text,
        ..SystemConfig::default()
    }));
    assert!(shell.form_for_test().expect("a form").pending().is_empty());
}

#[test]
fn a_run_that_did_not_take_the_change_is_not_reported_as_applied() {
    // A non-zero exit is the tool refusing, which is not a success however
    // cleanly the account was allowed.
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);

    shell.adopt_authorisation(Authorised::Ran(2));
    assert!(!shell.authorising(), "the task is over");
    assert!(band_line(&shell).contains("did not accept the change"));
    assert_eq!(shell.form_for_test().expect("a form").pending().len(), 1);
    assert!(!shell.config_wanted(), "nothing was written to re-read");
}

#[test]
fn the_date_and_time_pane_asks_the_desktop_to_open_date_and_time() {
    let mut shell = showing_with("date-time", SystemConfig::default());
    assert_eq!(apply(&mut shell), SystemTask::SetClock);
    shell.adopt_authorisation(Authorised::Launched);
    assert!(!shell.authorising());
}

#[test]
fn a_declined_prompt_changes_nothing_anywhere() {
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);
    let said = band_line(&shell);
    shell.adopt_authorisation(Authorised::Declined);
    assert!(!shell.authorising());
    assert!(!shell.config_wanted());
    assert_eq!(shell.form_for_test().expect("a form").pending().len(), 1);
    assert_eq!(band_line(&shell), said, "nothing to say");
}

/// The desktop's prompt holds the seat, so the window's own input is never
/// filtered for it here: what reaches the pane is the pane's.
#[test]
fn the_window_keeps_its_own_input_while_the_desktop_asks() {
    let mut shell = showing_with("caching", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);
    let staged = shell.form_for_test().expect("a form").pending();
    press_command(&mut shell, 0);
    assert_ne!(
        shell.form_for_test().expect("a form").pending(),
        staged,
        "Revert reached the pane"
    );
    assert!(shell.authorising(), "and left the task with the desktop");
}

/// One task is asked at a time: Apply while the desktop is asking asks
/// nothing more, and once the task is over the next Apply asks again.
#[test]
fn one_task_is_asked_at_a_time() {
    let mut shell = showing_with("login-startup", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let _ = apply(&mut shell);
    assert!(
        !matches!(press_action(&mut shell), ShellOutcome::Authorise(_)),
        "a task outstanding asks for no second"
    );
    shell.adopt_authorisation(Authorised::Declined);
    assert!(matches!(apply(&mut shell), SystemTask::Configure(_)));
}

// --- The readings -------------------------------------------------------

#[test]
fn a_machine_row_with_no_reading_says_so_rather_than_showing_a_default() {
    // An unread store is not a store of defaults: showing one would be a
    // value the reader could not account for and could not have set.
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
    let mut sink = damage();
    shell.go_to_pane("caching", WIDE, Scale::ONE, &theme(), &mut sink);
    assert!(shell.config_wanted(), "the pane asks for the reading");
    let form = shell.form_for_test().expect("a composed pane");
    assert!(
        form.pending().is_empty(),
        "there is nothing to apply against a store that was never read"
    );
}

#[test]
fn the_master_switch_restates_the_rows_it_is_a_ceiling_over() {
    // Turning caching off for the machine leaves each class's own value
    // standing — that is what the store says — but a row that only said
    // `Automatic` would read as a cache that is running.
    let mut shell = showing_with("caching", SystemConfig::default());
    let before = pane_text(&shell);
    choose_next(&mut shell, 0, 0);
    let after = pane_text(&shell);
    assert!(
        after.contains("no effect") && !before.contains("no effect"),
        "the ceiling is stated on the rows it takes away"
    );
}

/// Every word the pane on show draws in its rows, for a test that asks what
/// a reader would actually see.
fn pane_text(shell: &Shell) -> String {
    let Some(form) = shell.form_for_test() else {
        return String::new();
    };
    let mut text = String::new();
    for group in form.groups() {
        for row in group.rows() {
            text.push_str(row.label());
            text.push(' ');
            if let Some(description) = row.description() {
                text.push_str(description);
                text.push(' ');
            }
        }
    }
    text
}

#[test]
fn the_about_pane_states_every_reading_it_could_not_take() {
    // Each query is its own, so one refusal costs one reading — and the
    // row says so rather than borrowing a neighbour's success.
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
    let mut sink = damage();
    shell.go_to_pane("about", WIDE, Scale::ONE, &theme(), &mut sink);
    let unmeasured = stated(&shell);
    assert_eq!(unmeasured.len(), 7, "one row per About reading");
    assert!(
        unmeasured.iter().all(|value| value == "not measured"),
        "a reading that did not arrive is stated, never fabricated: {unmeasured:?}"
    );

    // One reading lands; only that row moves, because each is its own.
    shell.adopt_machine(MachineFacts {
        memory_bytes: Some(2 << 30),
        ..MachineFacts::default()
    });
    let partly = stated(&shell);
    assert_eq!(partly[6], "2.0 GiB");
    assert!(partly[..6].iter().all(|value| value == "not measured"));

    // And the rest.
    shell.adopt_machine(MachineFacts {
        identity: Some(
            SystemIdentity::new(*b"0123456789abcdef", 0, 1, 0, b"tairix").expect("an identity"),
        ),
        uptime: Some(Uptime {
            since_boot: Duration64::from_nanos(90 * 60 * 1_000_000_000),
            boot_time: Time64::UNIX_EPOCH,
        }),
        cpus: Vec::new(),
        memory_bytes: Some(2 << 30),
        clock: None,
    });
    let measured = stated(&shell);
    assert_eq!(measured[0], "tairix");
    assert_eq!(measured[1], "30313233343536373839616263646566");
    assert_eq!(measured[2], "0.1.0");
    assert_eq!(measured[3], "1:30");
    // No processor was reported, so the pane says so rather than naming one.
    assert_eq!(measured[4], "not measured");
    assert_eq!(measured[5], "not measured");
}

/// A fact column paints its readings, not only its labels: two different
/// readings of the same pane must reach different pixels, which they cannot
/// while the plate is laid out with no slot for a reading to be drawn in.
#[test]
fn the_fact_panes_paint_their_readings() {
    let theme = theme();
    let paint = |pane: &str, facts: MachineFacts| {
        let mut shell =
            Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
        let mut sink = damage();
        assert!(shell.go_to_pane(pane, WIDE, Scale::ONE, &theme, &mut sink));
        shell.adopt_machine(facts);
        shell.lay_out(WIDE, Scale::ONE, &theme);
        let mut surface = Surface::new(WIDE.width, WIDE.height).expect("a surface");
        shell.render(
            &mut surface,
            WIDE,
            Scale::ONE,
            opaque(&theme),
            &mut NoArtwork,
        );
        surface.pixels().to_vec()
    };
    let measured = MachineFacts {
        memory_bytes: Some(2 << 30),
        clock: Some(WallClockReading::UNSET),
        ..MachineFacts::default()
    };
    for pane in ["about", "date-time"] {
        let unmeasured = paint(pane, MachineFacts::default());
        assert!(
            unmeasured != paint(pane, measured.clone()),
            "{pane} paints none of its readings"
        );
    }
}

#[test]
fn a_clock_that_was_never_set_says_so_rather_than_showing_the_epoch() {
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
    let mut sink = damage();
    shell.go_to_pane("date-time", WIDE, Scale::ONE, &theme(), &mut sink);
    shell.adopt_machine(MachineFacts {
        clock: Some(WallClockReading::UNSET),
        ..MachineFacts::default()
    });
    let unset = stated(&shell);
    assert_eq!(unset[0], "not set");
    assert_eq!(unset[1], "UTC", "no zone store has been delegated");
    assert_eq!(unset[2], "nothing yet");

    // A clock somebody set says where the reading came from, so a user can
    // tell a validated network sample from this machine's own chip.
    shell.adopt_machine(MachineFacts {
        clock: Some(WallClockReading::new(
            Time64::from_secs(1_700_000_000),
            WallTimeState::Trusted,
        )),
        ..MachineFacts::default()
    });
    let set = stated(&shell);
    assert_eq!(set[2], "the network");
    assert_ne!(set[0], "not set");
}

/// The clock is stated in the user's zone and region, from the store the
/// desktop delegated, and follows a new choice of either.
#[test]
fn the_clock_is_read_in_the_users_zone_and_region() {
    let mut shell = Shell::new(DesktopSettings::default(), Board::default()).expect("a registry");
    let mut sink = damage();
    shell.go_to_pane("date-time", WIDE, Scale::ONE, &theme(), &mut sink);
    shell.adopt_machine(MachineFacts {
        clock: Some(WallClockReading::new(
            // 2023-11-14 22:13:20 UTC.
            Time64::from_secs(1_700_000_000),
            WallTimeState::Trusted,
        )),
        ..MachineFacts::default()
    });
    shell.adopt_zones(Some(crate::test_support::zones()));
    let stated_now = stated(&shell);
    assert_eq!(stated_now[0], "Tuesday 14 November 2023 22:13");
    assert_eq!(stated_now[1], "London, United Kingdom, GMT (UTC+00:00)");

    shell.adopt_settings(DesktopSettings {
        time_zone: tairix_wallpaper::ZoneName::new("America/New_York"),
        region: tairix_wallpaper::RegionCode::new("US"),
        ..DesktopSettings::default()
    });
    let stated_now = stated(&shell);
    assert_eq!(stated_now[0], "Tuesday 14 November 2023 5:13 PM");
    assert_eq!(stated_now[1], "New York, United States, EST (UTC-05:00)");
}

#[test]
fn a_store_whose_master_switch_is_off_still_reports_each_class_value() {
    let shell = showing_with(
        "caching",
        SystemConfig {
            cache_all: Switch::Off,
            cache_block: CacheMode::Off,
            ..SystemConfig::default()
        },
    );
    let form = shell.form_for_test().expect("a composed pane");
    // Nothing differs from what is in effect, because nothing was edited:
    // the ceiling is a reading, not a staged change.
    assert!(form.pending().is_empty());
}

/// The window's bare ground is the icon bar's glass and a strip row laid on
/// it is solid, and nothing anywhere is more see-through than the glass. The
/// system's prompt is the desktop's, so asking for a task draws nothing into
/// this window.
#[test]
fn the_window_is_glass_and_asking_for_a_task_draws_nothing_into_it() {
    use tairix_raster::Color;
    use tairix_theme::ThemeRegistry;

    use crate::frame::WINDOW_GROUND;
    use crate::registry::{Category, StripRow};

    let mut shell = showing_with("caching", SystemConfig::default());
    choose_next(&mut shell, 0, 0);
    let themes = ThemeRegistry::with_builtins();
    let grounds = themes.grounds(WINDOW_GROUND);
    let p = *grounds.window.palette();
    let mut surface = Surface::new(WIDE.width, WIDE.height).expect("a surface");
    shell.render(&mut surface, WIDE, Scale::ONE, grounds, &mut NoArtwork);

    let resting = shell
        .rows()
        .iter()
        .position(|row| *row == StripRow::Category(Category::Sound))
        .expect("the strip lists Sound");
    let row = shell
        .strip_row_rect(resting, WIDE, Scale::ONE, grounds.window)
        .expect("the strip seats Sound");
    let x = u32::try_from(row.right()).expect("on the surface") - 8;
    let y = u32::try_from(row.top()).expect("on the surface") + row.height / 2;
    assert_eq!(
        surface.get(x, y),
        Some(Color::from(p.surface).premultiply()),
        "a resting strip row lets the desktop through"
    );

    let weakest = surface.pixels().iter().map(|pixel| pixel.a).min();
    assert_eq!(
        weakest,
        Some(p.chrome_alpha),
        "something is thinner than the glass"
    );
    let _ = apply(&mut shell);
    assert!(shell.authorising());
    let mut asked = Surface::new(WIDE.width, WIDE.height).expect("a surface");
    shell.render(&mut asked, WIDE, Scale::ONE, grounds, &mut NoArtwork);
    assert_eq!(
        asked.pixels(),
        surface.pixels(),
        "drawn into no part of this window"
    );
}
