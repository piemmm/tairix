use tairix_wallpaper::{DesktopSettings, PointerTrail};
use tairix_wm::{Compositor, ENLARGED_SIDE_PX};

use super::AidPolicy;
use crate::shell::DesktopShell;
use crate::switchuser::NO_DEADLINE_NS;
use crate::tests::{compositor, moved, shell};

const MS: u64 = 1_000_000;

fn every_aid() -> AidPolicy {
    AidPolicy {
        shake: true,
        trail: PointerTrail::Long,
        locate: true,
    }
}

/// A shell showing its pointer at the centre of the screen, with `policy`.
fn desktop(policy: AidPolicy) -> (DesktopShell, Compositor) {
    let mut shell = shell();
    let mut comp = compositor();
    shell.set_pointer_aids(policy);
    let _ = shell.apply(moved(960, 540), &mut comp, 0);
    shell.refresh_cursor(&mut comp);
    shell.advance_pointer_aids(0, &mut comp);
    comp.composite();
    (shell, comp)
}

/// Shake the pointer across 120 pixels and back `strokes` times, 60 ms a
/// stroke, stepping the aids each frame; answer the instant it ends at.
fn shake(shell: &mut DesktopShell, comp: &mut Compositor, from_ns: u64, strokes: u32) -> u64 {
    let mut now = from_ns;
    for index in 0..strokes {
        let (from, to) = if index % 2 == 0 {
            (900, 1020)
        } else {
            (1020, 900)
        };
        for step in 1..=4 {
            now += 15 * MS;
            let x = from + (to - from) * step / 4;
            let _ = shell.apply(moved(x, 540), comp, now);
            shell.advance_pointer_aids(now, comp);
        }
    }
    now
}

/// Step the aids a frame at a time from `from_ns` for `span_ns`.
fn frames(shell: &mut DesktopShell, comp: &mut Compositor, from_ns: u64, span_ns: u64) -> u64 {
    let mut now = from_ns;
    while now < from_ns + span_ns {
        now += 16 * MS;
        shell.advance_pointer_aids(now, comp);
    }
    now
}

#[test]
fn the_policy_is_read_from_the_settings_with_shake_alone_on_by_default() {
    assert_eq!(
        AidPolicy::of(&DesktopSettings::default()),
        AidPolicy {
            shake: true,
            trail: PointerTrail::Off,
            locate: false,
        }
    );
}

#[test]
fn a_shaken_pointer_grows_on_screen_and_comes_home() {
    let (mut shell, mut comp) = desktop(every_aid());
    let native = comp.cursor_bounds().expect("a pointer is shown");
    let end = shake(&mut shell, &mut comp, 0, 6);
    let grown = frames(&mut shell, &mut comp, end, 200 * MS);
    assert_eq!(
        comp.cursor_bounds().expect("a pointer is shown").width,
        ENLARGED_SIDE_PX
    );
    frames(&mut shell, &mut comp, grown, 1_500 * MS);
    let home = comp.cursor_bounds().expect("a pointer is shown");
    assert_eq!(
        (home.width, home.height),
        (native.width, native.height),
        "back at its own size"
    );
}

#[test]
fn with_shake_off_the_pointer_never_grows() {
    let (mut shell, mut comp) = desktop(AidPolicy::NONE);
    let native = comp.cursor_bounds().expect("a pointer is shown");
    let end = shake(&mut shell, &mut comp, 0, 6);
    frames(&mut shell, &mut comp, end, 200 * MS);
    assert_eq!(comp.cursor_bounds(), Some(native));
}

#[test]
fn a_lone_ctrl_sends_rings_only_when_asked_for() {
    let (mut shell, mut comp) = desktop(AidPolicy::NONE);
    shell.locate_pointer(10 * MS);
    shell.advance_pointer_aids(20 * MS, &mut comp);
    assert!(
        !comp.has_damage(),
        "nothing is drawn for an aid not asked for"
    );

    let (mut shell, mut comp) = desktop(every_aid());
    shell.locate_pointer(10 * MS);
    shell.advance_pointer_aids(20 * MS, &mut comp);
    assert!(comp.has_damage(), "the rings are drawn");
    assert!(
        shell.pointer_aids_park_deadline_ns(20 * MS, NO_DEADLINE_NS) < NO_DEADLINE_NS,
        "and ask for the frames that move them"
    );
    let done = frames(&mut shell, &mut comp, 20 * MS, 1_000 * MS);
    comp.composite();
    shell.advance_pointer_aids(done + 16 * MS, &mut comp);
    assert!(!comp.has_damage(), "once they have gone nothing is owed");
}

#[test]
fn a_trail_asks_for_frames_only_until_it_catches_up() {
    let (mut shell, mut comp) = desktop(every_aid());
    let mut now = 0;
    for step in 1..=20 {
        now += 8 * MS;
        let _ = shell.apply(moved(960 + step * 6, 540), &mut comp, now);
        shell.advance_pointer_aids(now, &mut comp);
    }
    assert!(shell.pointer_aids_park_deadline_ns(now, NO_DEADLINE_NS) < NO_DEADLINE_NS);
    let rested = frames(&mut shell, &mut comp, now, 600 * MS);
    assert_eq!(
        shell.pointer_aids_park_deadline_ns(rested, NO_DEADLINE_NS),
        NO_DEADLINE_NS
    );
}

#[test]
fn an_idle_desktop_with_every_aid_on_parks_indefinitely() {
    let (mut shell, mut comp) = desktop(every_aid());
    frames(&mut shell, &mut comp, 0, 100 * MS);
    assert_eq!(
        shell.pointer_aids_park_deadline_ns(100 * MS, NO_DEADLINE_NS),
        NO_DEADLINE_NS
    );
}

#[test]
fn the_screensaver_takes_every_aid_away_and_the_pointer_returns_as_itself() {
    let (mut shell, mut comp) = desktop(every_aid());
    let native = comp.cursor_bounds().expect("a pointer is shown");
    let end = shake(&mut shell, &mut comp, 0, 6);
    shell.locate_pointer(end);
    let now = frames(&mut shell, &mut comp, end, 100 * MS);
    assert!(comp.set_cursor_hidden(true));
    shell.advance_pointer_aids(now + 16 * MS, &mut comp);
    assert_eq!(
        shell.pointer_aids_park_deadline_ns(now + 16 * MS, NO_DEADLINE_NS),
        NO_DEADLINE_NS,
        "nothing is animated behind the screensaver"
    );
    assert!(comp.set_cursor_hidden(false));
    let shown = comp.cursor_bounds().expect("a pointer is shown");
    assert_eq!((shown.width, shown.height), (native.width, native.height));
}
