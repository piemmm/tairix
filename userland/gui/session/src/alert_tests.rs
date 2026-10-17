//! Host tests of the desktop's alert windows: titled by who speaks, framed
//! as dialogs, sized to their content and centred, holding their owner,
//! taking the keyboard only when earned, armed against a click meant for what
//! was there, and answered once.

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode};
use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertSeverity, AlertSpec};
use tairix_wm::{Compositor, Point, Rect, Surface, WindowId};

use super::{alert_title, extent, Alerts, Answer, Speaker, ALERT_ARM_NS};
use crate::hold::Holds;
use crate::shell::DesktopShell;
use crate::tests::headless_desktop;

fn spec(buttons: &[(&str, AlertButtonRole)]) -> AlertSpec {
    let buttons: alloc::vec::Vec<AlertButton> = buttons
        .iter()
        .map(|(label, role)| AlertButton::new(label, *role).expect("a valid label"))
        .collect();
    AlertSpec::new(
        AlertSeverity::Error,
        "The folder could not be listed",
        "The volume holding it stopped answering.",
        &buttons,
    )
    .expect("a valid alert")
}

fn ok() -> AlertSpec {
    spec(&[("OK", AlertButtonRole::Default)])
}

/// A task window of the session's, standing in for an application's.
fn owner(shell: &mut DesktopShell, comp: &mut Compositor) -> WindowId {
    let surface = Surface::new(300, 200).expect("a surface");
    shell
        .open_window(comp, Point::new(10, 10), surface, "Files")
        .expect("a window")
}

fn pressed(key: NamedKeyCode) -> KeyInput {
    KeyInput::Pressed {
        key: KeyValue::Named(key),
        modifiers: Modifiers::default(),
    }
}

/// Where in alert `wm`'s client its first button's centre is.
fn first_button(alerts: &Alerts, wm: WindowId, shell: &DesktopShell, comp: &Compositor) -> Point {
    let alert = alerts
        .up
        .iter()
        .find(|alert| alert.window.wm() == wm)
        .expect("the alert is up");
    let rect = alert.window.button_rects(shell, comp)[0];
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).expect("small"),
        rect.top() + i32::try_from(rect.height / 2).expect("small"),
    )
}

#[test]
fn an_alert_is_titled_by_who_speaks_and_what_it_says() {
    assert_eq!(
        alert_title(AlertSeverity::Error, Speaker::App("Files")),
        "Error from Files"
    );
    assert_eq!(
        alert_title(AlertSeverity::Warning, Speaker::App("Paint")),
        "Warning from Paint"
    );
    assert_eq!(
        alert_title(AlertSeverity::Question, Speaker::Desktop),
        "Message from Desktop"
    );
    assert_eq!(
        alert_title(AlertSeverity::Information, Speaker::App("TextEdit")),
        "Message from TextEdit"
    );
}

#[test]
fn an_alert_is_a_content_sized_dialog_of_its_own_centred_on_the_screen() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (1, None, None),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    let window = comp.window(wm).expect("on screen");
    assert!(window.frame().is_some(), "framed as a dialog");
    assert_eq!(window.parent(), None, "it holds no window");
    let bounds = window.bounds();
    let client = comp.window_client_rect(wm).expect("a client");
    assert_eq!(
        (client.width, client.height),
        extent(
            &tairix_controls::Alert::from_spec(&ok()),
            comp.screen_rect(),
            comp.scale(),
            shell.session().active_theme()
        ),
        "placed by the extent its content needs"
    );
    assert_eq!(
        bounds.origin,
        crate::shell::prompt_origin(comp.screen_rect(), (bounds.width, bounds.height))
    );
    assert_eq!(shell.router().focused(), Some(wm), "it earned the keyboard");
}

#[test]
fn an_alert_holding_a_window_rises_with_it_and_holds_it() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (5, Some(77), Some(files)),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = holds.holder(files).expect("it holds the window");
    assert_eq!(comp.window(wm).expect("up").parent(), Some(files));
    assert!(alerts.owns(wm));
    assert_eq!(holds.holder(wm), None, "it holds that window alone");
}

#[test]
fn an_alert_answers_only_once_its_buttons_have_armed() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (9, Some(77), Some(files)),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    let at = first_button(&alerts, wm, &shell, &comp);
    assert_eq!(
        alerts.click(wm, at, 1_000, &mut holds, &mut shell, &mut comp),
        None,
        "not before the serve loop has seen it"
    );
    alerts.stamp(1_000);
    alerts.stamp(5_000_000_000);
    assert_eq!(
        alerts.click(
            wm,
            at,
            1_000 + ALERT_ARM_NS - 1,
            &mut holds,
            &mut shell,
            &mut comp
        ),
        None,
        "not within the arm delay, which the first stamp started"
    );
    assert_eq!(
        alerts.key(
            wm,
            &pressed(NamedKeyCode::Enter),
            1_000 + ALERT_ARM_NS - 1,
            &mut holds,
            &mut shell,
            &mut comp
        ),
        None,
        "nor by a key typed into what was there"
    );
    assert_eq!(
        alerts.click(
            wm,
            at,
            1_000 + ALERT_ARM_NS,
            &mut holds,
            &mut shell,
            &mut comp
        ),
        Some(Answer {
            alert_id: 9,
            index: 0
        })
    );
    assert!(comp.window(wm).is_none(), "an answered alert is down");
    assert_eq!(holds.holder(files), None, "and holds nothing");
    assert_eq!(
        alerts.click(wm, at, u64::MAX, &mut holds, &mut shell, &mut comp),
        None,
        "and answers no more"
    );
}

#[test]
fn escape_answers_with_the_cancel_button() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (3, None, None),
        (
            &spec(&[
                ("Cancel", AlertButtonRole::Cancel),
                ("Replace", AlertButtonRole::Default),
            ]),
            "Message from Files"
        ),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    alerts.stamp(0);
    assert_eq!(
        alerts.key(
            wm,
            &pressed(NamedKeyCode::Escape),
            ALERT_ARM_NS,
            &mut holds,
            &mut shell,
            &mut comp
        ),
        Some(Answer {
            alert_id: 3,
            index: 0
        })
    );
}

#[test]
fn an_alert_that_has_not_earned_the_keyboard_waits_where_its_window_is() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    // The user moves on to another window over the one the alert is about.
    let surface = Surface::new(640, 480).expect("a surface");
    let work = shell
        .open_window(&mut comp, Point::new(0, 0), surface, "Work")
        .expect("a window");
    assert_eq!(shell.router().focused(), Some(work));
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (2, Some(77), Some(files)),
        (&ok(), "Error from Files"),
        (false, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = holds.holder(files).expect("up");
    assert_eq!(
        shell.router().focused(),
        Some(work),
        "the keys stay with the work"
    );
    let centre = comp.window(wm).expect("up").bounds();
    let mid = Point::new(
        centre.left() + i32::try_from(centre.width / 2).expect("small"),
        centre.top() + i32::try_from(centre.height / 2).expect("small"),
    );
    assert_eq!(
        comp.window_at(mid),
        Some(work),
        "it waits with its window, beneath the work"
    );
}

#[test]
fn while_the_seat_is_held_an_alert_opens_beneath_the_holder() {
    let (mut shell, mut comp) = headless_desktop();
    let surface = Surface::new(640, 480).expect("a surface");
    let lock = shell
        .open_window(&mut comp, Point::new(0, 0), surface, "Lock")
        .expect("a window");
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (4, None, None),
        (&ok(), "Error from Files"),
        (true, true),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    assert_eq!(shell.router().focused(), Some(lock), "nothing is taken");
    let bounds = comp.window(wm).expect("up").bounds();
    assert_eq!(comp.window_at(bounds.origin), Some(lock));
}

/// Answering an older alert leaves the newest of a window's in front: the
/// one a press on that window hands the keyboard to.
#[test]
fn the_newest_alert_holding_a_window_stays_its_front_one() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    for id in [1, 2, 3] {
        assert!(alerts.open(
            (id, Some(77), Some(files)),
            (&ok(), "Error from Files"),
            (true, false),
            &mut holds,
            &mut shell,
            &mut comp
        ));
    }
    let newest = holds.holder(files).expect("up");
    let oldest = alerts.up[0].window.wm();
    alerts.stamp(0);
    let at = first_button(&alerts, oldest, &shell, &comp);
    assert_eq!(
        alerts.click(oldest, at, ALERT_ARM_NS, &mut holds, &mut shell, &mut comp),
        Some(Answer {
            alert_id: 1,
            index: 0
        })
    );
    assert_eq!(holds.holder(files), Some(newest));
}

#[test]
fn a_withdrawn_alert_comes_down() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (6, Some(77), Some(files)),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    alerts.withdraw(6, &mut holds, &mut shell, &mut comp);
    assert!(comp.window(wm).is_none());
    assert_eq!(holds.holder(files), None);
}

#[test]
fn each_further_alert_is_set_down_from_the_last() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    for id in [1, 2] {
        assert!(alerts.open(
            (id, None, None),
            (&ok(), "Error from Files"),
            (true, false),
            &mut holds,
            &mut shell,
            &mut comp
        ));
    }
    let origin = |index: usize| {
        comp.window(alerts.up[index].window.wm())
            .expect("up")
            .bounds()
            .origin
    };
    let (first, second) = (origin(0), origin(1));
    assert!(
        second.x > first.x && second.y > first.y,
        "{first:?} {second:?}"
    );
}

#[test]
fn a_change_of_density_fits_an_alert_again() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (1, None, None),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    let before: Rect = comp.window(wm).expect("up").bounds();
    assert!(comp.set_scale(tairix_wm::Scale::from_percent(150).expect("in range")));
    alerts.repaint(&shell, &mut comp);
    let after = comp.window(wm).expect("up").bounds();
    assert!(after.width > before.width);
    let screen = comp.screen_rect();
    assert!(after.right() <= screen.right() && after.bottom() <= screen.bottom());
}

#[test]
fn a_desktop_notice_holds_nothing_and_its_answer_is_relayed_to_no_one() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.notify(&ok(), "Error from Paint", &mut holds, &mut shell, &mut comp));
    let wm = alerts.up[0].window.wm();
    assert!(alerts.owns(wm));
    assert_eq!(holds.release(wm), None, "a notice holds no window");
    alerts.withdraw(0, &mut holds, &mut shell, &mut comp);
    assert!(
        alerts.owns(wm),
        "no client's withdrawal takes a notice down"
    );

    alerts.stamp(0);
    let at = first_button(&alerts, wm, &shell, &comp);
    assert_eq!(
        alerts.click(wm, at, ALERT_ARM_NS, &mut holds, &mut shell, &mut comp),
        None,
        "answered, and owed to no application"
    );
    assert!(comp.window(wm).is_none(), "an answered notice is down");
    assert!(!alerts.owns(wm));
}

#[test]
fn a_run_of_notices_gives_way_oldest_first() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (3, None, None),
        (&ok(), "Error from Files"),
        (false, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let mut notices = alloc::vec::Vec::new();
    for _ in 0..=super::DESKTOP_NOTICES_MAX {
        assert!(alerts.notify(
            &ok(),
            "Error from Desktop",
            &mut holds,
            &mut shell,
            &mut comp
        ));
        notices.push(alerts.up.last().expect("up").window.wm());
    }
    assert!(!alerts.owns(notices[0]), "the oldest notice gave way");
    assert!(notices[1..].iter().all(|wm| alerts.owns(*wm)));
    assert_eq!(
        alerts.up.len(),
        super::DESKTOP_NOTICES_MAX + 1,
        "the client's alert stays"
    );
}

/// A window the user is working in, over everything else.
fn work(shell: &mut DesktopShell, comp: &mut Compositor) -> WindowId {
    let surface = Surface::new(640, 480).expect("a surface");
    shell
        .open_window(comp, Point::new(0, 0), surface, "Work")
        .expect("a window")
}

#[test]
fn an_alert_holding_no_window_that_has_not_earned_the_keyboard_opens_beneath_the_work() {
    let (mut shell, mut comp) = headless_desktop();
    let work = work(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (5, None, None),
        (&ok(), "Error from Files"),
        (false, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    assert_eq!(shell.router().focused(), Some(work));
    let bounds = comp.window(wm).expect("up").bounds();
    assert_eq!(
        comp.window_at(bounds.origin),
        Some(work),
        "it does not take the front"
    );
}

#[test]
fn answering_the_front_alert_on_a_window_hands_the_keyboard_to_the_next() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    for id in [1, 2] {
        assert!(alerts.open(
            (id, Some(77), Some(files)),
            (&ok(), "Error from Files"),
            (true, false),
            &mut holds,
            &mut shell,
            &mut comp
        ));
    }
    let (behind, front) = (alerts.up[0].window.wm(), alerts.up[1].window.wm());
    assert_eq!(shell.router().focused(), Some(front));
    alerts.stamp(0);
    assert!(alerts
        .key(
            front,
            &pressed(NamedKeyCode::Enter),
            ALERT_ARM_NS,
            &mut holds,
            &mut shell,
            &mut comp
        )
        .is_some());
    assert_eq!(
        shell.router().focused(),
        Some(behind),
        "the window is still held, so its keys go to the alert holding it"
    );
}

#[test]
fn a_notice_answered_gives_the_keyboard_back_to_the_window_it_took_it_from() {
    let (mut shell, mut comp) = headless_desktop();
    let work = work(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.notify(
        &ok(),
        "Error from the Desktop",
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    assert_eq!(shell.router().focused(), Some(wm));
    alerts.stamp(0);
    assert!(alerts
        .key(
            wm,
            &pressed(NamedKeyCode::Enter),
            ALERT_ARM_NS,
            &mut holds,
            &mut shell,
            &mut comp
        )
        .is_none());
    assert_eq!(shell.router().focused(), Some(work));
}

#[test]
fn an_alert_whose_window_is_gone_leaves_the_keyboard_on_the_desktop() {
    let (mut shell, mut comp) = headless_desktop();
    let files = owner(&mut shell, &mut comp);
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    assert!(alerts.open(
        (6, Some(77), Some(files)),
        (&ok(), "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    assert_eq!(shell.router().focused(), Some(wm));
    assert!(comp.remove(files));
    alerts.withdraw(6, &mut holds, &mut shell, &mut comp);
    assert_eq!(
        shell.router().focused(),
        None,
        "never a window that is gone"
    );
}

#[test]
fn input_records_what_it_changed_and_the_drained_run_paints_it_once() {
    let (mut shell, mut comp) = headless_desktop();
    let mut alerts = Alerts::default();
    let mut holds = Holds::new();
    let two = spec(&[
        ("Cancel", AlertButtonRole::Cancel),
        ("Retry", AlertButtonRole::Default),
    ]);
    assert!(alerts.open(
        (7, None, None),
        (&two, "Error from Files"),
        (true, false),
        &mut holds,
        &mut shell,
        &mut comp
    ));
    let wm = alerts.up[0].window.wm();
    assert!(alerts
        .key(
            wm,
            &pressed(NamedKeyCode::Tab),
            0,
            &mut holds,
            &mut shell,
            &mut comp
        )
        .is_none());
    assert!(
        !alerts.up[0].window.owed.is_empty(),
        "moving the focus owes the buttons a paint"
    );
    alerts.paint_owed(&shell, &mut comp);
    assert!(alerts.up[0].window.owed.is_empty());
}

#[test]
fn no_application_speaks_as_the_desktop() {
    use super::app_speaker;
    let bundle = "/Users/ann/Applications/Desktop.app";
    for name in [
        "Desktop",
        " desktop ",
        "DESKTOP",
        "",
        "Desk\u{200b}top",
        "\u{202e}potkseD",
    ] {
        assert_eq!(
            alert_title(AlertSeverity::Question, app_speaker(name, bundle)),
            "Message from /Users/ann/Applications/Desktop.app",
            "{name:?}"
        );
    }
    assert_eq!(
        alert_title(AlertSeverity::Error, app_speaker("Files", bundle)),
        "Error from Files"
    );
    assert_eq!(
        super::dialog_title(app_speaker("Desk\u{200b}top", bundle), "Authenticate"),
        "/Users/ann/Applications/Desktop.app \u{2014} Authenticate",
        "nor does a dialog"
    );
}
