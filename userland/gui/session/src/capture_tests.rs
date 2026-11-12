//! Host tests of the capture controller: the chords, the bar, the overlay,
//! the countdown, what a still shows and where it goes, the options, and
//! giving the seat up to the lock.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PhysicalKey};
use tairix_abi::recorder_ipc::RecordSound;
use tairix_abi::time::Time64;
use tairix_log::{Event, EventId, FieldValue, Sink};
use tairix_raster::{Color, Surface};
use tairix_taskbar::TaskbarConfig;
use tairix_taskbar::TaskbarResponse;
use tairix_tz::CivilClock;
use tairix_wallpaper::{CaptureFolder, CaptureSettings, CaptureTimer};
use tairix_wm::{shadow_footprint, Compositor, Point, Rect, WindowId};

use super::still::keep_bounded;
use super::{claim_recording, sweep_captures, RECORDING_ENDING};
use super::{
    log_kept, option_id, BarPart, CaptureChord, CaptureController, CaptureNotice, CaptureTarget,
    CaptureWork, ChordAsk, Kept, KeptFolder, Screened, StillKept, StillShot, CAPTURE_KEPT,
    CAPTURE_KEPT_MESSAGE, CAPTURE_REFUSED, CAPTURE_TAKEN, SOUND_GROUP, TIMER_GROUP,
};
use crate::keep::tests::{striped, Table, FINISHED};
use crate::keep::{scratch_path, PNG_ENDING};
use crate::menu::{ChainAnswer, ChainOutcome};
use crate::shell::{DesktopShell, ShellOutcome};
use crate::tests::{compositor, moved, shell, shell_audited, PRIMARY_PRESS, PRIMARY_RELEASE};
use tairix_controls::AuthorityState;

const SECOND: u64 = 1_000_000_000;

/// Counts the events of one id it is handed.
struct Count(AtomicUsize, EventId);

impl Sink for Count {
    fn write_event(&self, event: &Event<'_>) {
        if event.id == self.1 {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn chord(physical: PhysicalKey, ctrl: bool) -> KeyInput {
    KeyInput::Pressed {
        key: Some(KeyValue::Char('#')),
        modifiers: Modifiers {
            shift: true,
            ctrl,
            alt: false,
            meta: true,
        },
        physical,
    }
}

fn pressed(key: KeyValue, physical: PhysicalKey) -> KeyInput {
    KeyInput::Pressed {
        key: Some(key),
        modifiers: Modifiers::default(),
        physical,
    }
}

fn escape() -> KeyInput {
    pressed(KeyValue::Named(NamedKeyCode::Escape), PhysicalKey::ESCAPE)
}

fn ask(ask: ChordAsk) -> CaptureChord {
    CaptureChord {
        ask,
        clipboard: false,
    }
}

fn window_of(
    comp: &mut Compositor,
    origin: Point,
    (width, height): (u32, u32),
    color: Color,
) -> WindowId {
    let surface = Surface::filled(width, height, color.premultiply()).expect("surface");
    comp.add_window(origin, surface)
}

fn stills(controller: &mut CaptureController) -> Vec<StillShot> {
    controller
        .take_work()
        .into_iter()
        .filter_map(|work| match work {
            CaptureWork::Still(shot) => Some(shot),
            _ => None,
        })
        .collect()
}

fn click(
    controller: &mut CaptureController,
    at: Point,
    shell: &mut DesktopShell,
    comp: &mut Compositor,
) {
    controller.pointer(&moved(at.x, at.y), 0, shell, comp);
    controller.pointer(&PRIMARY_PRESS, 0, shell, comp);
    controller.pointer(&PRIMARY_RELEASE, 0, shell, comp);
}

fn press_part(
    controller: &mut CaptureController,
    part: BarPart,
    shell: &mut DesktopShell,
    comp: &mut Compositor,
) {
    let at = controller
        .bar_part_centre(part, comp)
        .expect("the bar is up");
    click(controller, at, shell, comp);
}

#[test]
fn a_chord_is_matched_by_where_the_key_is_not_what_it_types() {
    assert_eq!(
        CaptureChord::of(&chord(PhysicalKey::DIGIT_3, false)),
        Some(ask(ChordAsk::Screen))
    );
    assert_eq!(
        CaptureChord::of(&chord(PhysicalKey::DIGIT_4, true)),
        Some(CaptureChord {
            ask: ChordAsk::Area,
            clipboard: true
        })
    );
    assert_eq!(
        CaptureChord::of(&chord(PhysicalKey::DIGIT_5, false)).map(|chord| chord.ask),
        Some(ChordAsk::Bar)
    );
    let plain = pressed(KeyValue::Char('3'), PhysicalKey::DIGIT_3);
    assert_eq!(CaptureChord::of(&plain), None, "no modifiers, no chord");
    let mut alt = chord(PhysicalKey::DIGIT_3, false);
    if let KeyInput::Pressed { modifiers, .. } = &mut alt {
        modifiers.alt = true;
    }
    assert_eq!(CaptureChord::of(&alt), None, "Alt makes no chord");
    assert_eq!(CaptureChord::of(&chord(PhysicalKey::A, false)), None);
}

#[test]
fn the_screen_chord_takes_the_whole_screen_at_once_and_logs_it() {
    static TAKEN: Count = Count(AtomicUsize::new(0), CAPTURE_TAKEN);
    let mut shell = shell_audited(TaskbarConfig::bottom_bar(1920, 1080), &TAKEN);
    let mut comp = compositor();
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    let shots = stills(&mut controller);
    assert_eq!(shots.len(), 1);
    let picture = shots[0].pixels.surface();
    assert_eq!((picture.width(), picture.height()), (1920, 1080));
    assert!(!shots[0].clipboard);
    assert_eq!(shots[0].folder, KeptFolder::Desktop);
    assert_eq!(TAKEN.0.load(Ordering::Relaxed), 1);
    assert_eq!(controller.holding_name(), "idle");
}

/// Counts the [`CAPTURE_KEPT`] records naming one kind of capture.
struct KeptAs(AtomicUsize, &'static str);

impl Sink for KeptAs {
    fn write_event(&self, event: &Event<'_>) {
        let how = event.fields.iter().find(|field| field.key == "how");
        if event.id == CAPTURE_KEPT
            && event.message == CAPTURE_KEPT_MESSAGE
            && how.is_some_and(|field| field.value == FieldValue::Str(self.1))
        {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Each kind of kept capture is logged under its own name, so the system log
/// says where every capture landed.
#[test]
fn a_kept_capture_is_logged_by_what_it_was() {
    static STILLS: KeptAs = KeptAs(AtomicUsize::new(0), "still");
    static RECORDINGS: KeptAs = KeptAs(AtomicUsize::new(0), "recording");
    static LEFTOVERS: KeptAs = KeptAs(AtomicUsize::new(0), "leftover");
    for (sink, kept) in [
        (&STILLS, Kept::Still),
        (&RECORDINGS, Kept::Recording),
        (&LEFTOVERS, Kept::Leftover),
    ] {
        let shell = shell_audited(TaskbarConfig::bottom_bar(1920, 1080), sink);
        log_kept(&shell, kept);
        assert_eq!(sink.0.load(Ordering::Relaxed), 1, "{kept:?}");
    }
}

#[test]
fn the_lock_and_the_sheet_are_blanked_and_capture_chrome_left_out() {
    let (mut shell, mut comp) = (shell(), compositor());
    let lock = window_of(
        &mut comp,
        Point::new(100, 100),
        (50, 40),
        Color::rgb(200, 10, 10),
    );
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.screen(Screened {
        lock: Some(lock),
        sheet: None,
    });
    controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    let shots = stills(&mut controller);
    let pixel = shots[0].pixels.surface().pixels()[120 * 1920 + 120];
    assert_eq!(
        (pixel.r, pixel.g, pixel.b),
        (0, 0, 0),
        "the lock shows black"
    );
    let thumbnail = controller.chrome_windows();
    assert_eq!(thumbnail.len(), 1, "the floating thumbnail");
    let tap = controller
        .still_tap(comp.screen_rect(), None)
        .expect("a tap");
    assert!(
        tap.exclude.contains(&thumbnail[0]),
        "its own chrome is left out"
    );
    assert_eq!(tap.blank, [lock]);
}

/// The bar's modes are a segmented control: Left and Right choose the one
/// beside, stopping at either end, and Enter takes the one chosen.
#[test]
fn the_arrows_choose_the_bars_mode_and_enter_takes_it() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    let arrow = |code| pressed(KeyValue::Named(code), PhysicalKey::A);
    for key in [
        arrow(NamedKeyCode::Left),
        arrow(NamedKeyCode::Right),
        arrow(NamedKeyCode::Right),
    ] {
        controller.key(&key, 0, &mut shell, &mut comp);
    }
    assert_eq!(
        controller.holding_name(),
        "bar",
        "choosing a mode takes nothing"
    );
    let enter = pressed(KeyValue::Named(NamedKeyCode::Enter), PhysicalKey::A);
    controller.key(&enter, 0, &mut shell, &mut comp);
    assert_eq!(
        controller.holding_name(),
        "choosing",
        "the third mode, an area, is chosen in the overlay"
    );
}

#[test]
fn the_bar_offers_its_modes_and_commits_the_one_chosen() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "bar");
    let bar = controller.chrome_windows()[0];
    let bounds = comp.window(bar).expect("the bar").bounds();
    let work = shell.work_area(&comp);
    assert!(
        bounds.center().x.abs_diff(work.center().x) <= 1,
        "bottom centre of the work area"
    );
    assert!(bounds.bottom() < work.bottom());
    press_part(&mut controller, BarPart::Mode(2), &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    assert_eq!(
        controller.holding_name(),
        "choosing",
        "an area is chosen in the overlay"
    );
    assert!(comp.window(bar).is_none(), "the bar went");
    controller.key(&escape(), 0, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "idle");
    assert!(
        controller.take_work().is_empty(),
        "Escape discards everything"
    );
    assert!(controller.chrome_windows().is_empty());
}

#[test]
fn a_recording_mode_commits_a_recording_of_the_screen() {
    let (mut shell, mut comp) = (shell(), compositor());
    let settings = CaptureSettings {
        sound: RecordSound::Microphone,
        ..CaptureSettings::default()
    };
    let mut controller = CaptureController::new(settings);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Mode(3), &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    let work = controller.take_work();
    let [CaptureWork::Record(plan)] = work.as_slice() else {
        panic!("one recording: {work:?}");
    };
    assert_eq!(plan.area, comp.screen_rect());
    assert_eq!(plan.target, CaptureTarget::Screen);
    assert_eq!(plan.sound, RecordSound::Microphone);
    assert_eq!(plan.folder, KeptFolder::Desktop);
}

#[test]
fn an_area_is_dragged_out_and_taken() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(
        CaptureChord {
            ask: ChordAsk::Area,
            clipboard: true,
        },
        0,
        &mut shell,
        &mut comp,
    );
    assert_eq!(controller.holding_name(), "choosing");
    for event in [
        moved(300, 200),
        PRIMARY_PRESS,
        moved(100, 100),
        PRIMARY_RELEASE,
    ] {
        controller.pointer(&event, 0, &mut shell, &mut comp);
    }
    let shots = stills(&mut controller);
    let picture = shots[0].pixels.surface();
    assert_eq!((picture.width(), picture.height()), (200, 100));
    assert!(shots[0].clipboard, "Ctrl sends it to the clipboard");
}

#[test]
fn space_picks_a_window_alone_with_its_shadow() {
    let (mut shell, mut comp) = (shell(), compositor());
    let picked = window_of(
        &mut comp,
        Point::new(400, 300),
        (200, 150),
        Color::rgb(0, 200, 0),
    );
    let over = window_of(
        &mut comp,
        Point::new(560, 420),
        (100, 100),
        Color::rgb(250, 0, 250),
    );
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Area), 0, &mut shell, &mut comp);
    controller.key(
        &pressed(KeyValue::Char(' '), PhysicalKey::SPACE),
        0,
        &mut shell,
        &mut comp,
    );
    for event in [moved(450, 350), PRIMARY_PRESS] {
        controller.pointer(&event, 0, &mut shell, &mut comp);
    }
    let shots = stills(&mut controller);
    let bounds = comp.window(picked).expect("the window").bounds();
    let area =
        shadow_footprint(bounds, comp.scale(), comp.theme()).intersection(&comp.screen_rect());
    let picture = shots[0].pixels.surface();
    assert_eq!(
        (picture.width(), picture.height()),
        (area.width, area.height)
    );
    let inside = |x: i32, y: i32| {
        let (x, y) = (x - area.left(), y - area.top());
        picture.pixels()[usize::try_from(y).expect("in") * usize::try_from(area.width).expect("in")
            + usize::try_from(x).expect("in")]
    };
    let shown = inside(590, 440);
    assert_eq!(
        (shown.r, shown.g, shown.b),
        (0, 200, 0),
        "the window over it is left out"
    );
    assert!(comp.window(over).is_some());
}

#[test]
fn the_countdown_is_a_park_deadline_and_takes_the_capture_when_it_runs_out() {
    let (mut shell, mut comp) = (shell(), compositor());
    let settings = CaptureSettings {
        timer: CaptureTimer::Seconds5,
        ..CaptureSettings::default()
    };
    let mut controller = CaptureController::new(settings);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "counting");
    assert_eq!(controller.park_deadline_ns(0, u64::MAX), SECOND);
    controller.tick(SECOND, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "counting");
    assert!(stills(&mut controller).is_empty());
    controller.tick(5 * SECOND, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "idle");
    assert_eq!(stills(&mut controller).len(), 1);

    controller.chord(ask(ChordAsk::Bar), 6 * SECOND, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    controller.key(&escape(), 7 * SECOND, &mut shell, &mut comp);
    controller.tick(20 * SECOND, &mut shell, &mut comp);
    assert!(
        stills(&mut controller).is_empty(),
        "Escape while counting discards it"
    );
}

#[test]
fn a_chord_during_a_recording_stops_it_and_one_while_choosing_cancels() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.set_recording(true);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    assert!(controller.take_stop());
    assert!(controller.take_work().is_empty(), "a stop is no work");
    assert_eq!(controller.holding_name(), "idle");
    controller.set_recording(false);
    controller.chord(ask(ChordAsk::Area), 0, &mut shell, &mut comp);
    controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "idle");
    assert!(
        controller.take_work().is_empty(),
        "the second chord only cancelled"
    );
}

#[test]
fn a_window_gone_before_its_countdown_ends_is_refused_and_said() {
    static REFUSED: Count = Count(AtomicUsize::new(0), CAPTURE_REFUSED);
    let mut shell = shell_audited(TaskbarConfig::bottom_bar(1920, 1080), &REFUSED);
    let mut comp = compositor();
    let window = window_of(
        &mut comp,
        Point::new(400, 300),
        (200, 150),
        Color::rgb(0, 200, 0),
    );
    let settings = CaptureSettings {
        timer: CaptureTimer::Seconds5,
        ..CaptureSettings::default()
    };
    let mut controller = CaptureController::new(settings);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Mode(1), &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    for event in [moved(450, 350), PRIMARY_PRESS] {
        controller.pointer(&event, 0, &mut shell, &mut comp);
    }
    assert_eq!(controller.holding_name(), "counting");
    comp.remove(window);
    controller.tick(5 * SECOND, &mut shell, &mut comp);
    assert!(matches!(
        controller.take_work().as_slice(),
        [CaptureWork::Notice(CaptureNotice::Gone {
            target: CaptureTarget::Window,
            recording: false
        })]
    ));
    assert_eq!(REFUSED.0.load(Ordering::Relaxed), 1);
}

#[test]
fn an_option_chosen_is_written_once_and_stands_while_it_is_written() {
    let (mut shell, mut comp) = (shell(), compositor());
    let stored = CaptureSettings::default();
    let mut controller = CaptureController::new(stored);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Options, &mut shell, &mut comp);
    let (model, placement) = controller.take_menu().expect("the Options menu");
    assert_eq!(model.title(), "Options");
    assert!(placement.anchor.bottom() <= comp.screen_rect().bottom());
    let five = option_id(TIMER_GROUP, 1).expect("an id");
    assert!(model.rows().iter().any(|row| row.id() == Some(five)));
    let kept = controller.menu_answered(&ChainAnswer::Ended(ChainOutcome::Chosen(five)));
    assert!(
        kept.is_none(),
        "a choice that ended the menu restates nothing"
    );
    assert_eq!(controller.settings().timer, CaptureTimer::Seconds5);
    controller.adopt(&stored, false);
    assert_eq!(
        controller.settings().timer,
        CaptureTimer::Seconds5,
        "not yet written"
    );
    assert_eq!(
        controller.take_settled().map(|settled| settled.timer),
        Some(CaptureTimer::Seconds5)
    );
    assert_eq!(controller.take_settled(), None, "written once");
    controller.adopt(&stored, true);
    assert_eq!(
        controller.settings().timer,
        CaptureTimer::Seconds5,
        "stands while publishing"
    );
    controller.adopt(&stored, false);
    assert_eq!(
        controller.settings().timer,
        CaptureTimer::Off,
        "a refused write comes back"
    );
    assert_eq!(
        controller.holding_name(),
        "bar",
        "the bar stays up under its menu"
    );
}

#[test]
fn the_lock_taking_the_seat_takes_every_piece_of_chrome_down() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    let _ = controller.take_work();
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    let chrome = controller.chrome_windows();
    assert_eq!(chrome.len(), 2, "the thumbnail and the bar");
    controller.abandon(&mut shell, &mut comp);
    assert_eq!(controller.holding_name(), "idle");
    assert!(chrome.iter().all(|window| comp.window(*window).is_none()));
    assert!(controller.take_work().is_empty());
}

#[test]
fn a_still_goes_where_the_options_say_and_past_the_clipboards_bound_to_the_folder() {
    let shot = |clipboard| StillShot {
        pixels: {
            let mut pixels = super::CaptureBuffer::new(7, 5).expect("pixels");
            *pixels.surface_mut() = striped(7, 5);
            pixels
        },
        density: super::still_density(tairix_wm::Scale::ONE),
        clipboard,
        folder: KeptFolder::of(CaptureFolder::Home, false),
    };
    let mut files = Table::default();
    let saved = keep_bounded(
        &mut files,
        Some("/Users/ada"),
        &shot(false),
        Some(FINISHED),
        1 << 20,
    );
    assert!(
        matches!(&saved, Ok(StillKept::Saved(path))
            if path == "/Users/ada/Home/Pictures/Screenshot 2026-10-01 at 14.03.07.png"),
        "{saved:?}"
    );
    let clipped = keep_bounded(
        &mut files,
        Some("/Users/ada"),
        &shot(true),
        Some(FINISHED),
        1 << 20,
    );
    assert!(
        matches!(clipped, Ok(StillKept::Clipboard(_))),
        "{clipped:?}"
    );
    let oversize = keep_bounded(
        &mut files,
        Some("/Users/ada"),
        &shot(true),
        Some(FINISHED),
        8,
    );
    assert!(
        matches!(&oversize, Ok(StillKept::Oversize(path)) if path.ends_with("14.03.07 2.png")),
        "{oversize:?}"
    );
    let (title, _) = CaptureNotice::SavedInstead(String::from("x")).text();
    assert_eq!(title, "Screenshot saved instead");
}

#[test]
fn a_still_records_the_desktops_density() {
    let density = super::still_density(tairix_wm::Scale::ONE).expect("a density");
    assert_eq!(
        density.whole_in(tairix_image::DensityUnit::Inch),
        Some((96, 96))
    );
}

#[test]
fn the_recording_indicator_is_the_captures_own_and_stops_the_recording() {
    let mut controller = CaptureController::new(CaptureSettings::default());
    let pressed = ShellOutcome::Taskbar(TaskbarResponse::StopScreenRecording);
    assert!(controller.claims(&pressed));
    assert!(!controller.take_stop(), "no recording, nothing to stop");
    controller.set_recording(true);
    assert!(controller.claims(&pressed));
    assert!(controller.take_stop());
    assert!(!controller.claims(&ShellOutcome::Taskbar(TaskbarResponse::Ignored)));
}

#[test]
fn kept_folders_follow_the_options_and_a_recording_never_goes_on_the_clipboard() {
    assert_eq!(
        KeptFolder::of(CaptureFolder::Home, false),
        KeptFolder::Pictures
    );
    assert_eq!(
        KeptFolder::of(CaptureFolder::Home, true),
        KeptFolder::Videos
    );
    assert_eq!(
        KeptFolder::of(CaptureFolder::Clipboard, true),
        KeptFolder::Desktop
    );
    assert_eq!(KeptFolder::Videos.folders(), ["Home", "Videos"]);
}

/// What a session ended mid-capture left is named at the next start for when
/// it last changed, with no date where that was never set — every scratch file
/// of a capture folder, however it is numbered — and the empty file a recorder
/// that died before writing leaves is removed rather than named.
#[test]
fn a_capture_cut_short_is_named_at_the_next_start_and_an_empty_one_removed() {
    let mut files = Table::default();
    let desktop = "/Users/ada/Desktop";
    let videos = "/Users/ada/Home/Videos";
    let still = scratch_path(desktop, PNG_ENDING, 1);
    let recording = scratch_path(videos, RECORDING_ENDING, 1);
    let later = scratch_path(videos, RECORDING_ENDING, 3);
    let empty = scratch_path(desktop, RECORDING_ENDING, 2);
    files.files.insert(still.clone(), alloc::vec![1, 2, 3]);
    // 2026-10-01 14:03:07 UTC.
    files
        .changed
        .insert(still, Time64::from_secs(1_790_863_387));
    files.files.insert(recording, alloc::vec![4; 8]);
    files.files.insert(later, alloc::vec![5; 8]);
    files.files.insert(empty.clone(), Vec::new());

    let swept = sweep_captures(&mut files, Some("/Users/ada"), &CivilClock::UTC);

    assert_eq!(
        swept,
        [
            alloc::format!("{desktop}/Screenshot 2026-10-01 at 14.03.07.png"),
            alloc::format!("{videos}/Screen Recording.webm"),
            alloc::format!("{videos}/Screen Recording 2.webm"),
        ]
    );
    assert!(!files.files.contains_key(&empty), "nothing to name");
    assert_eq!(files.files.len(), 3, "no scratch file left behind");
}

/// A recording's file is claimed afresh, and one an earlier recording left —
/// its naming failed, so it may be its only copy — is never taken over.
#[test]
fn a_recording_never_takes_over_a_file_left_before_it() {
    let mut files = Table::default();
    let videos = "/Users/ada/Home/Videos";
    let left = scratch_path(videos, RECORDING_ENDING, 1);
    files.files.insert(left.clone(), alloc::vec![9; 16]);
    let (claimed, ()) = claim_recording(
        &mut files,
        Some("/Users/ada"),
        KeptFolder::Videos,
        |files, path| files.write_scratch(path, &[]),
    )
    .expect("claimed");
    assert_eq!(claimed, scratch_path(videos, RECORDING_ENDING, 2));
    assert_eq!(
        files.files.get(&left).map(Vec::len),
        Some(16),
        "the earlier file is as it was"
    );
}

/// The floating thumbnail of a still goes as a recording starts, so no
/// recording shows the capture's own chrome.
#[test]
fn a_recording_takes_the_floating_thumbnail_down() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    let _ = controller.take_work();
    let chrome = controller.chrome_windows();
    assert_eq!(chrome.len(), 1, "the floating thumbnail");
    controller.chord(ask(ChordAsk::Bar), SECOND, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Mode(3), &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    assert!(matches!(
        controller.take_work().as_slice(),
        [CaptureWork::Record(_)]
    ));
    assert!(comp.window(chrome[0]).is_none(), "the thumbnail went");
    assert!(controller.chrome_windows().is_empty());
}

/// An application's own tooltip is part of what the user sees, so a still
/// shows it: only the capture's own chrome is left out.
#[test]
fn an_applications_tooltip_is_shown_in_a_still() {
    let (mut shell, mut comp) = (shell(), compositor());
    let window = window_of(
        &mut comp,
        Point::new(100, 100),
        (200, 100),
        Color::rgb(10, 10, 200),
    );
    let region = crate::shell::tip_region(Rect::new(0, 0, 200, 100)).expect("a region");
    assert!(
        !shell.declare_tooltip(window, region, "Explains it", &comp),
        "nothing shows until the pointer rests"
    );
    shell.tooltip_pointer(Point::new(150, 150), 0, &comp);
    assert!(shell.tooltip_tick(10 * SECOND));
    shell.present_tooltip(&mut comp);
    let tip: Vec<WindowId> = shell.floating_windows().collect();
    assert_eq!(tip.len(), 1, "the tip's plate");
    let controller = CaptureController::new(CaptureSettings::default());
    let tap = controller
        .still_tap(comp.screen_rect(), None)
        .expect("a tap");
    assert!(
        !tap.exclude.contains(&tip[0]),
        "an application's tip is no capture chrome"
    );
}

/// A countdown plate the heap would not repaint stays the countdown's: still
/// left out of the capture, and taken down with it rather than orphaned.
#[test]
fn a_countdown_plate_refused_its_repaint_stays_the_countdowns() {
    let (mut shell, mut comp) = (shell(), compositor());
    let settings = CaptureSettings {
        timer: CaptureTimer::Seconds10,
        ..CaptureSettings::default()
    };
    let mut controller = CaptureController::new(settings);
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    press_part(&mut controller, BarPart::Primary, &mut shell, &mut comp);
    let plate = controller.chrome_windows();
    assert_eq!(plate.len(), 1, "the countdown's plate");
    tairix_fuzzseed::meter::refusing_above(4096, || {
        controller.tick(9 * SECOND, &mut shell, &mut comp);
    });
    assert_eq!(
        controller.chrome_windows(),
        plate,
        "still the countdown's own"
    );
    controller.key(&escape(), 9 * SECOND, &mut shell, &mut comp);
    assert!(comp.window(plate[0]).is_none(), "and taken down with it");
}

/// A repaint of the bar the heap refuses leaves what it owes owed, so the next
/// settle paints it rather than leaving the bar as it was.
#[test]
fn a_bar_repaint_the_heap_refused_is_painted_at_the_next_settle() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Bar), 0, &mut shell, &mut comp);
    let bar = controller.chrome_windows()[0];
    let painted = comp.window(bar).expect("the bar").bounds();
    assert!(comp.set_surface(bar, Surface::new(1, 1).expect("a pixel")));
    let right = pressed(KeyValue::Named(NamedKeyCode::Right), PhysicalKey::A);
    controller.key(&right, 0, &mut shell, &mut comp);
    tairix_fuzzseed::meter::refusing_above(16 * 1024, || {
        controller.settle(&mut shell, &mut comp);
    });
    assert_eq!(
        comp.window(bar).expect("the bar").bounds().width,
        1,
        "the refused repaint changed nothing"
    );
    controller.settle(&mut shell, &mut comp);
    assert_eq!(
        comp.window(bar).expect("the bar").bounds(),
        painted,
        "the next settle paints what was owed"
    );
}

/// A repaint of the overlay the heap refuses is painted at the next settle.
#[test]
fn an_overlay_repaint_the_heap_refused_is_painted_at_the_next_settle() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.chord(ask(ChordAsk::Area), 0, &mut shell, &mut comp);
    let overlay = controller.chrome_windows()[0];
    assert!(comp.set_surface(overlay, Surface::new(1, 1).expect("a pixel")));
    for event in [moved(300, 200), PRIMARY_PRESS, moved(100, 100)] {
        controller.pointer(&event, 0, &mut shell, &mut comp);
    }
    tairix_fuzzseed::meter::refusing_above(1 << 20, || {
        controller.settle(&mut shell, &mut comp);
    });
    assert_eq!(
        comp.window(overlay).expect("the overlay").bounds().width,
        1,
        "the refused repaint changed nothing"
    );
    controller.settle(&mut shell, &mut comp);
    assert_eq!(
        comp.window(overlay).expect("the overlay").bounds(),
        comp.screen_rect(),
        "the next settle paints what was owed"
    );
}

/// An overlay the heap cannot show says the controls could not be shown, not
/// that a screenshot was not taken, since none was being.
#[test]
fn an_overlay_that_cannot_be_shown_says_so() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    tairix_fuzzseed::meter::refusing_above(1 << 20, || {
        controller.chord(ask(ChordAsk::Area), 0, &mut shell, &mut comp);
    });
    assert_eq!(controller.holding_name(), "idle");
    assert!(matches!(
        controller.take_work().as_slice(),
        [CaptureWork::Notice(CaptureNotice::NoControls)]
    ));
    let (title, _) = CaptureNotice::NoControls.text();
    assert_eq!(title, "Capture not available");
}

/// A chord during a recording stops it however short the heap: a stop is a
/// flag, never work the heap must find room for.
#[test]
fn a_recording_is_stopped_however_short_the_heap() {
    let (mut shell, mut comp) = (shell(), compositor());
    let mut controller = CaptureController::new(CaptureSettings::default());
    controller.set_recording(true);
    tairix_fuzzseed::meter::refusing_after(0, || {
        controller.chord(ask(ChordAsk::Screen), 0, &mut shell, &mut comp);
    });
    assert!(controller.take_stop());
    assert!(!controller.take_stop(), "taken once");
}

/// Every sound choice is offered plainly: none wears the Authority Mark, since
/// one the account may not record itself is asked of an administrator rather
/// than refused.
#[test]
fn the_sound_rows_carry_no_authority_mark() {
    let model = super::options_menu(CaptureSettings::default(), true);
    let sound: Vec<_> = (0..RecordSound::ALL.len())
        .map(|place| option_id(SOUND_GROUP, place).expect("an id"))
        .collect();
    let rows: Vec<_> = model
        .rows()
        .iter()
        .filter(|row| row.id().is_some_and(|id| sound.contains(&id)))
        .collect();
    assert_eq!(rows.len(), RecordSound::ALL.len());
    let chosen = CaptureSettings::default().sound;
    for (row, choice) in rows.into_iter().zip(RecordSound::ALL) {
        let state = row.drawn().state();
        assert_eq!(state.authority, AuthorityState::Allowed);
        // The choice in force is the one row a pick would not change.
        assert_eq!(state.enabled, choice != chosen, "{choice:?}");
        assert!(
            row.tip().is_none(),
            "no refusal is explained, since none is made"
        );
    }
}
