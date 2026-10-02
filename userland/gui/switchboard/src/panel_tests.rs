//! Unit tests for the overview panel's window lifecycle and effect
//! application, driven entirely through the recording host.

use tairix_abi::driver::display::DamageRect;
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::switchboard_ipc::{CommandSection, FrameReport, SeatReport, SwitchboardRequest};
use tairix_abi::sysinfo::ProcessState;
use tairix_abi::window_ipc::{
    AppMenuItemId, AppMenuRowView, MenuOutcome, MenuRefusal, WindowRegion,
};
use tairix_abi::{Errno, ProcId, Signal};
use tairix_controls::WHEEL_STEP;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;

use super::{refusal_notice, Panel, PANEL_TITLE};
use crate::model::{build_model, OwnerBundles, PanelModel, RollingMeters, SessionReport, LOWERED};
use crate::sample::Sample;
use crate::task_menu::{task_control, task_menu};
use crate::test_host::{
    process_summary, sample_with, RecordingHost, NO_AUTHORITY, PROC_CONTROL_AUTHORITY,
};
use crate::view::{RecoveryControl, Section, SwitchboardAction, TaskControl};
use crate::wait::required_members;

/// This service's own scheduler task id in these tests.
const OWN_PID: u64 = 4242;

/// A recovery-bearing model: one stopped task the panel can act on.
fn stopped_model(pid: u64, can_force: bool) -> PanelModel {
    let sample = sample_with(alloc::vec![process_summary(
        pid,
        ProcessState::Stopped,
        b"stuck",
        None
    )]);
    let authority = if can_force {
        &PROC_CONTROL_AUTHORITY
    } else {
        &NO_AUTHORITY
    };
    build_model(
        PANEL_TITLE,
        None,
        &sample,
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        authority,
    )
}

/// A model with one live task the panel can switch to.
fn task_model(pid: u64) -> PanelModel {
    let sample = sample_with(alloc::vec![process_summary(
        pid,
        ProcessState::Running,
        b"alpha",
        None
    )]);
    build_model(
        PANEL_TITLE,
        None,
        &sample,
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        &NO_AUTHORITY,
    )
}

/// Enough sampled processes to overflow the task list at [`WINDOW`], so a
/// test can scroll it. The first row names `first_pid`, so which reading a
/// row came from is observable through the request activating it produces.
fn busy_model(first_pid: u64) -> PanelModel {
    busy_at(first_pid, None)
}

/// Forty running tasks whose CPU column reads `permille`, so a refresh from
/// one such model to another moves every row's drawn cells.
fn busy_at(first_pid: u64, permille: Option<u16>) -> PanelModel {
    let processes = (0..40)
        .map(|i| process_summary(first_pid + i, ProcessState::Running, b"task", permille))
        .collect();
    build_model(
        PANEL_TITLE,
        None,
        &sample_with(processes),
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        &NO_AUTHORITY,
    )
}

/// A model whose only reading is what the session's last frame cost, so a
/// refresh from one to another changes the System section's compositor
/// figures and nothing else any section draws.
fn frame_model(damaged_px: u64) -> PanelModel {
    let session = SessionReport {
        seat: SeatReport::HEALTHY,
        frame: Some(FrameReport {
            screen_px: 1920 * 1080,
            damaged_px,
            blended_px: 42_000,
            opaque_px: 1_100,
            dirty_rects: 3,
            present_calls: 1,
            chrome_hits: 12,
            chrome_misses: 1,
        }),
    };
    build_model(
        PANEL_TITLE,
        None,
        &Sample::default(),
        &session,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        &NO_AUTHORITY,
    )
}

/// A model with nothing in it.
fn empty_model() -> PanelModel {
    build_model(
        PANEL_TITLE,
        None,
        &Sample::default(),
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        &NO_AUTHORITY,
    )
}

fn open(panel: &mut Panel, host: &mut RecordingHost, section: CommandSection) {
    panel.open_section(host, section);
    panel.flush(host);
}

/// The window rectangle the scrolling test lays the composition out in.
const WINDOW: Rect = Rect::new(0, 0, 600, 400);

/// Scroll the open panel's active section down by `detents` wheel detents,
/// the way the compositor delivers a wheel event: resolved against the real
/// window geometry, theme metrics, and font metrics, so the offset a test
/// reads back is the one the user would have.
fn wheel(panel: &mut Panel, detents: i32) -> u64 {
    panel.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: detents * SCROLL_UNITS_PER_DETENT,
        },
        WINDOW,
        Scale::ONE,
        &Theme::dark(),
        BitmapFont::console(),
    );
    scroll_offset(panel)
}

/// How far `detents` wheel detents scroll a list at the unit scale.
fn detents_px(detents: u64) -> u64 {
    detents * u64::from(WHEEL_STEP)
}

/// The open composition's scroll offset, read straight off the panel's own
/// field: these tests are the panel's own module, so no accessor exists for
/// their sake alone.
fn scroll_offset(panel: &Panel) -> u64 {
    panel
        .view
        .as_ref()
        .expect("the panel is open")
        .scroll_offset()
}

/// Feed a bare pointer move to the open panel, the way the run loop
/// delivers a `WindowEvent::Pointer` `Moved` action.
/// A point `x` into the section's content, past the navigation rail down the
/// leading edge — so a test aiming at a row lands on one rather than on the
/// rail that chooses which list the rows belong to.
fn in_content(x: i32, y: i32) -> Point {
    Point::new(x + i32::try_from(crate::view::RAIL_WIDTH).unwrap_or(0), y)
}

fn pointer_move(panel: &mut Panel, to: Point) {
    panel.on_pointer(
        &InputEvent::PointerMoved { to },
        WINDOW,
        Scale::ONE,
        &Theme::dark(),
        BitmapFont::console(),
    );
}

#[test]
fn a_fresh_panel_is_closed_and_shows_nothing() {
    let panel = Panel::new(OWN_PID, empty_model());
    assert!(!panel.is_open());
    assert_eq!(panel.section(), None);
    assert_eq!(panel.session_report(), &SessionReport::HEALTHY);
}

#[test]
fn a_second_open_raises_the_one_window_rather_than_stacking_another() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());

    open(&mut panel, &mut host, CommandSection::Tasks);
    open(&mut panel, &mut host, CommandSection::Recovery);

    assert_eq!(host.opened, 1);
    assert_eq!(
        host.requests,
        alloc::vec![SwitchboardRequest::ActivateOwner { owner: OWN_PID }]
    );
    assert_eq!(panel.section(), Some(Section::Recovery));
    assert_eq!(host.armed(), required_members(true));
}

#[test]
fn a_refused_window_create_leaves_the_panel_closed_and_states_why() {
    let mut host = RecordingHost::new();
    host.open_refusal = Some(Errno::PermissionDenied);
    let mut panel = Panel::new(OWN_PID, empty_model());

    open(&mut panel, &mut host, CommandSection::Tasks);

    assert!(!panel.is_open());
    assert_eq!(host.opened, 0);
    assert_eq!(host.presents, 0);
    assert_eq!(host.armed(), required_members(false));
    assert_eq!(
        host.refusals,
        alloc::vec![(
            alloc::string::String::from("open the overview window"),
            Errno::PermissionDenied
        )]
    );
}

#[test]
fn closing_returns_to_headless_sampling() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);

    // The window manager's close request drives the panel's own close.
    panel.close(&mut host);

    assert!(!panel.is_open());
    assert_eq!(panel.section(), None);
    assert_eq!(host.closed, 1);
    assert_eq!(host.armed(), required_members(false));

    // A later model change draws nothing: there is no window to draw into,
    // and the panel never re-opens one on its own.
    let presents = host.presents;
    panel.refresh(&host, task_model(10));
    assert_eq!(host.presents, presents);
    assert!(!panel.is_open());
}

#[test]
fn closing_an_already_closed_panel_does_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    panel.close(&mut host);
    assert_eq!(host.closed, 0);
}

#[test]
fn a_seat_report_is_stored_for_the_callers_next_rebuild() {
    let host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    let report = SeatReport::new(3, &[11, 12]).expect("valid report");

    panel.set_seat_report(report);

    assert_eq!(panel.session_report().seat.owners(), &[11, 12]);
    assert_eq!(panel.session_report().seat.total(), 3);
    assert!(!panel.is_open());
    assert_eq!(host.opened, 0, "a seat report opens no window");
}

#[test]
fn refreshing_with_an_unchanged_model_draws_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    panel.refresh(&host, empty_model());

    assert_eq!(host.presents, presents);
}

#[test]
fn refreshing_with_a_changed_model_redraws_and_keeps_the_section() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Recovery);
    let presents = host.presents;

    panel.refresh(&host, stopped_model(7, false));
    panel.flush(&mut host);

    assert_eq!(host.presents, presents + 1);
    assert_eq!(panel.section(), Some(Section::Recovery));
}

#[test]
fn a_reading_from_another_subject_repaints_the_rail_and_not_the_window() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Recovery);

    // A task appears. The rail states every subject's reading whichever one
    // is on show, so this is on screen even from Recovery — but it is one
    // column of it, never the window.
    panel.refresh(&host, task_model(10));
    panel.flush(&mut host);

    let rect = host.last_presented_rect().expect("the rail moved");
    assert!(
        rect.width_px < host.bounds.2,
        "another subject's reading costs the rail, never the client: {rect:?}"
    );
    assert_eq!(panel.section(), Some(Section::Recovery));
}

#[test]
fn a_fresh_frame_reading_costs_the_rail_and_not_the_window() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, frame_model(3_200));
    open(&mut panel, &mut host, CommandSection::Tasks);

    panel.refresh(&host, frame_model(6_400));
    panel.flush(&mut host);

    // The session reports a frame per compositor frame, and the display
    // path's own trace rides the rail, so each one costs that column — which
    // is why it must never cost the client.
    let rect = host.last_presented_rect().expect("the rail moved");
    assert!(
        rect.width_px < host.bounds.2,
        "a frame reading costs the rail, never the client: {rect:?}"
    );
}

#[test]
fn a_refresh_keeps_the_users_place_and_shows_the_new_reading() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    assert_eq!(wheel(&mut panel, 4), detents_px(4));

    panel.refresh(&host, busy_model(200));
    panel.flush(&mut host);

    assert_eq!(panel.section(), Some(Section::Tasks));
    assert_eq!(
        scroll_offset(&panel),
        detents_px(4),
        "a live refresh must not snap the list back to the top"
    );

    // The model really is the new reading's: the first task it holds is the
    // process the refreshed sample put there, not the one it replaced.
    let action = first_task(&panel, TaskControl::Switch);
    panel.act(&mut host, action, &NO_AUTHORITY);
    assert_eq!(
        host.requests,
        alloc::vec![SwitchboardRequest::ActivateOwner { owner: 200 }]
    );
}

#[test]
fn a_task_action_asks_the_session_to_activate_that_owner() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);

    let action = first_task(&panel, TaskControl::Switch);
    panel.act(&mut host, action, &NO_AUTHORITY);

    assert_eq!(
        host.requests,
        alloc::vec![SwitchboardRequest::ActivateOwner { owner: 10 }]
    );
    assert!(host.signals.is_empty());
}

#[test]
fn a_restart_action_asks_the_session_to_relaunch_that_owner() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, stopped_model(7, false));

    panel.act(
        &mut host,
        SwitchboardAction::Recovery {
            index: 0,
            control: RecoveryControl::Restart,
        },
        &NO_AUTHORITY,
    );

    assert_eq!(
        host.requests,
        alloc::vec![SwitchboardRequest::RestartOwner { owner: 7 }]
    );
}

#[test]
fn a_force_action_signals_the_owner_when_authorised() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, stopped_model(7, true));

    panel.act(
        &mut host,
        SwitchboardAction::Recovery {
            index: 0,
            control: RecoveryControl::Force,
        },
        &PROC_CONTROL_AUTHORITY,
    );

    assert_eq!(host.signals, alloc::vec![(7, Signal::Kill)]);
    assert!(host.refusals.is_empty());
}

/// Lowering a task asks the host for the one lowered level, for that task.
#[test]
fn lowering_a_task_asks_the_host_for_the_lowered_level() {
    let sample = sample_with(alloc::vec![process_summary(
        10,
        ProcessState::Running,
        b"alpha",
        None
    )]);
    let model = build_model(
        PANEL_TITLE,
        None,
        &sample,
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        &PROC_CONTROL_AUTHORITY,
    );
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, model);

    let action = first_task(&panel, TaskControl::LowerPriority);
    panel.act(&mut host, action, &PROC_CONTROL_AUTHORITY);

    assert_eq!(host.priorities, alloc::vec![(10, LOWERED)]);
    assert!(host.refusals.is_empty());
}

#[test]
fn a_force_action_is_never_attempted_without_the_capability() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, stopped_model(7, false));

    panel.act(
        &mut host,
        SwitchboardAction::Recovery {
            index: 0,
            control: RecoveryControl::Force,
        },
        &NO_AUTHORITY,
    );

    assert!(host.signals.is_empty());
    assert!(host.requests.is_empty());
}

#[test]
fn a_force_action_on_an_id_beyond_the_syscall_width_is_refused_not_truncated() {
    // Past the signed range no pid exists, so a sample claiming one is
    // refused rather than folded onto a different, arbitrary process.
    let beyond = i64::MAX.cast_unsigned() + 1;
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, stopped_model(beyond, true));

    panel.act(
        &mut host,
        SwitchboardAction::Recovery {
            index: 0,
            control: RecoveryControl::Force,
        },
        &PROC_CONTROL_AUTHORITY,
    );

    assert!(host.signals.is_empty());
    assert_eq!(
        host.refusals,
        alloc::vec![(
            alloc::string::String::from("force that task to quit"),
            Errno::OutOfRange
        )]
    );
}

#[test]
fn a_refused_action_is_stated_and_the_panel_stays_open() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);
    host.request_refusal = Some(Errno::NotFound);

    let action = first_task(&panel, TaskControl::Switch);
    panel.act(&mut host, action, &NO_AUTHORITY);

    assert_eq!(
        host.refused_actions(),
        alloc::vec!["switch to that task's window"]
    );
    assert!(panel.is_open());
}

#[test]
fn a_refused_present_is_stated_and_the_panel_stays_open() {
    let mut host = RecordingHost::new();
    host.present_refusal = Some(Errno::NoSpace);
    let mut panel = Panel::new(OWN_PID, empty_model());

    open(&mut panel, &mut host, CommandSection::Tasks);

    assert!(panel.is_open());
    assert_eq!(host.presents, 0);
    assert_eq!(
        host.refused_actions(),
        alloc::vec!["redraw the overview window"]
    );
}

#[test]
fn a_scroll_action_changes_nothing_outside_the_panel() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);

    panel.act(
        &mut host,
        SwitchboardAction::Scrolled { offset: 2 },
        &NO_AUTHORITY,
    );

    assert!(host.requests.is_empty());
    assert!(host.signals.is_empty());
    assert!(panel.is_open());
}

/// The notice is the reason alone: the reporter that writes it adds the
/// program's name and the line's end, so neither is said twice.
#[test]
fn a_refusal_notice_names_the_action_and_the_refusal() {
    assert_eq!(
        refusal_notice("restart that task", Errno::PermissionDenied),
        "could not restart that task (permission denied)"
    );
}

#[test]
fn the_first_flush_after_opening_always_presents() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());

    panel.open_section(&mut host, CommandSection::Tasks);
    panel.flush(&mut host);

    assert_eq!(host.presents, 1);
}

#[test]
fn flushing_a_closed_panel_presents_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());

    panel.flush(&mut host);

    assert_eq!(host.presents, 0);
}

#[test]
fn flushing_an_unchanged_panel_presents_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    panel.flush(&mut host);

    assert_eq!(host.presents, presents);
}

#[test]
fn repeated_unchanged_flushes_present_only_once_in_total() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    panel.open_section(&mut host, CommandSection::Tasks);

    // The first flush presents the initial paint; every later one, with
    // nothing having changed in between, must not.
    for _ in 0..5 {
        panel.flush(&mut host);
    }

    assert_eq!(host.presents, 1);
}

#[test]
fn a_pointer_move_that_reaches_the_composition_unchanged_presents_nothing() {
    // Regression test for the reported defect: a pointer that reports the
    // same position again crosses no control and leaves the composition
    // byte-for-byte what it was, so the panel that used to redraw on every
    // delivered event no longer may.
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);
    pointer_move(&mut panel, Point::new(5, 5));
    panel.flush(&mut host);
    let presents = host.presents;

    pointer_move(&mut panel, Point::new(5, 5));
    panel.flush(&mut host);

    assert_eq!(host.presents, presents);
}

#[test]
fn a_scroll_that_changes_the_composition_presents_exactly_once() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    assert_eq!(wheel(&mut panel, 4), detents_px(4));
    panel.flush(&mut host);

    assert_eq!(host.presents, presents + 1);
}

#[test]
fn a_change_no_round_could_describe_presents_the_whole_client() {
    // A resize onto a fresh surface, a desktop appearance change and a
    // density change all reach the panel as window events the run loop
    // answers with `repaint_whole`; none of them is a control round, so none
    // of them can report a rectangle.
    for change in [
        |host: &mut RecordingHost| host.bounds.2 += 100,
        |host: &mut RecordingHost| host.theme = Theme::light(),
        |host: &mut RecordingHost| host.scale = Scale::from_percent(150).expect("a real scale"),
    ] {
        let mut host = RecordingHost::new();
        let mut panel = Panel::new(OWN_PID, empty_model());
        open(&mut panel, &mut host, CommandSection::Tasks);
        let presents = host.presents;

        change(&mut host);
        panel.repaint_whole();
        panel.flush(&mut host);

        assert_eq!(host.presents, presents + 1);
        assert_eq!(host.last_presented_rect(), Some(whole_client(&host)));
    }
}

#[test]
fn a_refused_present_is_reported_once_and_not_retried_by_an_unchanged_flush() {
    let mut host = RecordingHost::new();
    host.present_refusal = Some(Errno::PermissionDenied);
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);
    // `open` helper already flushed, but it failed and reported.
    assert_eq!(
        host.refused_actions(),
        alloc::vec!["redraw the overview window"]
    );

    // A second flush with nothing changed does not retry: the record was
    // updated even though the present was refused.
    panel.flush(&mut host);
    assert_eq!(
        host.refused_actions(),
        alloc::vec!["redraw the overview window"]
    );

    // An actual change owes the screen again and retries.
    panel.repaint_whole();
    panel.flush(&mut host);
    assert_eq!(
        host.refused_actions(),
        alloc::vec!["redraw the overview window", "redraw the overview window"]
    );
}

#[test]
fn refreshing_with_an_unchanged_model_then_flushing_presents_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, empty_model());
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    panel.refresh(&host, empty_model());
    panel.flush(&mut host);

    assert_eq!(host.presents, presents);
}

// --- What a present covers ------------------------------------------------

/// The client rectangle a present would cover in full, as the recording
/// host's own bounds describe it.
fn whole_client(host: &RecordingHost) -> DamageRect {
    DamageRect {
        x: 0,
        y: 0,
        width_px: host.bounds.2,
        height_px: host.bounds.3,
    }
}

#[test]
fn a_hover_presents_the_control_it_crossed_rather_than_the_window() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    assert_eq!(host.last_presented_rect(), Some(whole_client(&host)));

    pointer_move(&mut panel, in_content(40, 200));
    panel.flush(&mut host);

    let rect = host.last_presented_rect().expect("the hover presented");
    assert!(
        rect.height_px < host.bounds.3,
        "a hover repaints the row it entered, not every row: {rect:?}"
    );
}

#[test]
fn a_fresh_reading_presents_what_moved_and_not_the_client() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    // Every row's CPU cell moved — the widest a refresh of this section can
    // honestly report — and that is still short of the client, which also
    // carries the headings and the navigation rail beside the table.
    panel.refresh(&host, busy_at(100, Some(640)));
    panel.flush(&mut host);

    assert_eq!(host.presents, presents + 1);
    let rect = host.last_presented_rect().expect("a present");
    assert_ne!(
        rect,
        whole_client(&host),
        "a fresh reading costs the readings that moved, never the client"
    );
}

#[test]
fn a_reading_that_did_not_move_presents_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let presents = host.presents;

    // A distinct model value that derives the very same rows — the task
    // identities moved and no drawn cell did — so the refresh gate lets it
    // through and every section then reports nothing.
    panel.refresh(&host, busy_model(200));
    panel.flush(&mut host);

    assert_eq!(
        host.presents, presents,
        "nothing on screen moved, so nothing is presented"
    );
}

#[test]
fn a_reading_adopted_onto_released_pixels_draws_the_client_whole() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    // The session gave the retained pixels back, so there is no frame to
    // resolve a rectangle against and nothing partial could stand on them.
    host.attached = false;

    panel.refresh(&host, busy_at(100, Some(640)));
    host.attached = true;
    panel.flush(&mut host);

    assert_eq!(host.last_presented_rect(), Some(whole_client(&host)));
}

#[test]
fn a_hover_report_does_not_survive_into_the_next_present() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    pointer_move(&mut panel, in_content(40, 200));
    panel.flush(&mut host);

    // Nothing reported this time, so the account is clean and the only thing
    // that presents again is a change that owes the client whole.
    panel.repaint_whole();
    panel.flush(&mut host);

    assert_eq!(host.last_presented_rect(), Some(whole_client(&host)));
}

#[test]
fn discarded_pixels_are_redrawn_whole() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, busy_model(100));
    open(&mut panel, &mut host, CommandSection::Tasks);
    pointer_move(&mut panel, in_content(40, 200));

    panel.repaint_whole();
    panel.flush(&mut host);

    assert_eq!(
        host.last_presented_rect(),
        Some(whole_client(&host)),
        "the session gave back the whole window's pixels, so the whole window is drawn"
    );
}

// --- A task's menu ----------------------------------------------------------

/// `control` chosen for the first task `panel`'s model holds, by identity.
fn first_task(panel: &Panel, control: TaskControl) -> SwitchboardAction {
    SwitchboardAction::Task {
        proc_id: panel.model().model.tasks[0].proc_id,
        control,
    }
}

/// Running tasks `pids`, in that order, built under `authority`.
fn running_model(pids: &[u64], authority: &dyn tairix_abi::CapabilityQuery) -> PanelModel {
    let processes = pids
        .iter()
        .map(|pid| process_summary(*pid, ProcessState::Running, b"task", None))
        .collect();
    build_model(
        PANEL_TITLE,
        None,
        &sample_with(processes),
        &SessionReport::HEALTHY,
        &OwnerBundles::new(),
        &mut RollingMeters::new(),
        authority,
    )
}

/// The identity of the task `pid` in `model`.
fn proc_id_of(model: &PanelModel, pid: u64) -> ProcId {
    (0..model.model.tasks.len())
        .find(|&index| model.task_owner(index) == Some(pid))
        .map(|index| model.model.tasks[index].proc_id)
        .expect("the model holds that task")
}

/// Ask the open panel for `pid`'s menu at `(40, 80)`, answering the open id
/// the desktop accepted it under.
fn ask_menu(panel: &mut Panel, host: &mut RecordingHost, pid: u64) -> u64 {
    let proc_id = proc_id_of(panel.model(), pid);
    panel.act(
        host,
        SwitchboardAction::TaskMenu {
            proc_id,
            anchor: Rect::new(40, 80, 0, 0),
        },
        &NO_AUTHORITY,
    );
    u64::try_from(host.menus.len()).expect("a small count")
}

/// The id of the row the last opened menu declares for `control`.
fn row_of(host: &RecordingHost, control: TaskControl) -> AppMenuItemId {
    let (_, menu) = host.menus.last().expect("a menu was opened");
    menu.rows()
        .find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if task_control(item.id) == Some(control) => Some(item.id),
            _ => None,
        })
        .expect("the menu declares that command")
}

#[test]
fn a_task_menu_is_asked_of_the_desktop_where_the_reader_asked_for_it() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);

    let _ = ask_menu(&mut panel, &mut host, 10);

    let task = &panel.model().model.tasks[0];
    assert_eq!(
        host.menus,
        alloc::vec![(
            WindowRegion::new(40, 80, 0, 0).expect("a valid anchor"),
            task_menu(task).expect("the rows fit the menu bounds"),
        )],
        "one open, at the press, carrying the task's own rows"
    );
    assert!(host.refusals.is_empty());
}

#[test]
fn a_chosen_row_acts_on_the_task_its_menu_was_opened_on_after_the_rows_move() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10, 20], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);

    // Samples go on landing while the menu is up, and this one puts another
    // task where the chosen one was.
    panel.refresh(&host, running_model(&[20, 30, 10], &PROC_CONTROL_AUTHORITY));
    let force = row_of(&host, TaskControl::ForceQuit);
    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(force),
        &PROC_CONTROL_AUTHORITY,
    );

    assert_eq!(host.signals, alloc::vec![(10, Signal::Kill)]);
}

#[test]
fn a_command_the_task_no_longer_permits_is_not_carried_out() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);
    let pause = row_of(&host, TaskControl::Pause);

    // Someone else paused it while the menu was up.
    let paused = sample_with(alloc::vec![process_summary(
        10,
        ProcessState::Stopped,
        b"task",
        None
    )]);
    panel.refresh(
        &host,
        build_model(
            PANEL_TITLE,
            None,
            &paused,
            &SessionReport::HEALTHY,
            &OwnerBundles::new(),
            &mut RollingMeters::new(),
            &PROC_CONTROL_AUTHORITY,
        ),
    );
    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(pause),
        &PROC_CONTROL_AUTHORITY,
    );

    assert!(host.signals.is_empty(), "the verdict held now decides");
}

#[test]
fn a_command_on_a_task_that_went_while_its_menu_was_up_does_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10, 20], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);
    let force = row_of(&host, TaskControl::ForceQuit);

    panel.refresh(&host, running_model(&[20], &PROC_CONTROL_AUTHORITY));
    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(force),
        &PROC_CONTROL_AUTHORITY,
    );

    assert!(host.signals.is_empty(), "never the task that stayed");
}

#[test]
fn an_answer_to_a_settled_or_unknown_open_is_dropped() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);
    let force = row_of(&host, TaskControl::ForceQuit);

    panel.menu_closed(
        &mut host,
        open_id + 1,
        MenuOutcome::Chosen(force),
        &PROC_CONTROL_AUTHORITY,
    );
    assert!(host.signals.is_empty(), "an id this panel never held");

    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Dismissed,
        &PROC_CONTROL_AUTHORITY,
    );
    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(force),
        &PROC_CONTROL_AUTHORITY,
    );
    assert!(
        host.signals.is_empty(),
        "the open was answered once, and its gesture is over"
    );
    assert!(host.refusals.is_empty());
}

#[test]
fn a_row_id_the_menu_never_declared_does_nothing() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);

    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(AppMenuItemId::new(u16::MAX).expect("a valid id")),
        &PROC_CONTROL_AUTHORITY,
    );

    assert!(host.signals.is_empty());
    assert!(host.requests.is_empty());
}

#[test]
fn a_menu_the_desktop_refuses_is_stated_and_the_panel_carries_on() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);

    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Refused(MenuRefusal::SeatBusy),
        &NO_AUTHORITY,
    );

    assert_eq!(
        host.refusals,
        alloc::vec![(
            alloc::string::String::from("show that task's commands"),
            Errno::SeatBusy
        )]
    );
    assert!(panel.is_open());
}

#[test]
fn a_menu_open_the_desktop_turns_down_is_stated_and_leaves_nothing_owed() {
    let mut host = RecordingHost::new();
    host.menu_refusal = Some(Errno::NotSupported);
    let mut panel = Panel::new(OWN_PID, running_model(&[10], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);

    let proc_id = proc_id_of(panel.model(), 10);
    panel.act(
        &mut host,
        SwitchboardAction::TaskMenu {
            proc_id,
            anchor: Rect::new(40, 80, 0, 0),
        },
        &NO_AUTHORITY,
    );

    assert_eq!(
        host.refused_actions(),
        alloc::vec!["show that task's commands"]
    );
    assert!(panel.is_open(), "a refused menu is an answer, not a fault");
    let first = AppMenuItemId::for_index(0).expect("a valid id");
    panel.menu_closed(
        &mut host,
        1,
        MenuOutcome::Chosen(first),
        &PROC_CONTROL_AUTHORITY,
    );
    assert!(host.requests.is_empty(), "no open was ever owed an answer");
}

#[test]
fn closing_the_window_forgets_the_menu_it_was_owed() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, running_model(&[10], &PROC_CONTROL_AUTHORITY));
    open(&mut panel, &mut host, CommandSection::Tasks);
    let open_id = ask_menu(&mut panel, &mut host, 10);
    let force = row_of(&host, TaskControl::ForceQuit);

    panel.close(&mut host);
    open(&mut panel, &mut host, CommandSection::Tasks);
    panel.menu_closed(
        &mut host,
        open_id,
        MenuOutcome::Chosen(force),
        &PROC_CONTROL_AUTHORITY,
    );

    assert!(
        host.signals.is_empty(),
        "an answer for a window that has gone acts on nothing"
    );
}

#[test]
fn no_menu_is_asked_for_a_task_that_has_gone() {
    let mut host = RecordingHost::new();
    let mut panel = Panel::new(OWN_PID, task_model(10));
    open(&mut panel, &mut host, CommandSection::Tasks);

    panel.act(
        &mut host,
        SwitchboardAction::TaskMenu {
            proc_id: ProcId::from_raw([0xee; tairix_abi::PROC_ID_LEN]),
            anchor: Rect::new(40, 80, 0, 0),
        },
        &NO_AUTHORITY,
    );

    assert!(host.menus.is_empty());
    assert!(host.refusals.is_empty());
}
