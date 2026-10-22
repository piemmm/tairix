//! Host tests of the screensaver: what covers the screen, the pointer it
//! hides, the saver it hosts and the reasons it ends one, and the display it
//! switches off and wakes.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::desktop::{Appearance, Contrast, Density};
use tairix_abi::driver::display::{
    DamageList, DamageRect, DisplayFormat, DisplayMode, DisplayPower, RefreshRate,
};
use tairix_abi::saver::{
    Choice, ChoiceValue, Choices, OptionKey, SaverFeeds, SaverOption, SaverOptionKind,
};
use tairix_abi::saver_ipc::{SaverMessage, WallReading, SAVER_START_DEADLINE_NS};
use tairix_abi::sysinfo::MemoryBand;
use tairix_abi::window_ipc::{SaverOrigin, SaverStanding};
use tairix_abi::{BundleId, DriverError, Errno, APPINFO_FLAG_FEED_MACHINE};
use tairix_display::Presenter;
use tairix_log::{DiscardSink, Sink};
use tairix_reclaim::PressureBand;
use tairix_wallpaper::{SaverValues, TaskNames};
use tairix_wm::{Color, Compositor, Point, Scale, Surface};

use super::fixtures::{Launches, Pipes, Records};
use super::{
    ceilings, saver_start, Ended, Fault, Saver, SaverContext, SaverEnd, SaverLaunch, SaverSetup,
    Screensaver, SwitchedOff, Waking, PREVIEW_STEADY_NS, SAVER_DISMISSED, SAVER_ENDED, SAVER_SHOWN,
};
use crate::tests::compositor;

type Saving = Screensaver<Pipes, DiscardSink>;

/// A panel that records the switches asked of it.
struct Panel {
    can_switch: bool,
    refuse: Option<DriverError>,
    switches: Vec<DisplayPower>,
}

impl Panel {
    fn switchable() -> Self {
        Self {
            can_switch: true,
            refuse: None,
            switches: Vec::new(),
        }
    }

    fn fixed() -> Self {
        Self {
            can_switch: false,
            ..Self::switchable()
        }
    }
}

impl Presenter for Panel {
    fn mode(&self) -> DisplayMode {
        DisplayMode {
            width_px: 1920,
            height_px: 1080,
            stride_bytes: 1920 * 4,
            format: DisplayFormat::Rgba8888,
        }
    }

    fn ready(&self) -> bool {
        true
    }

    fn present(&mut self, _frame: &[u8], _damage: &[DamageRect]) -> Result<(), DriverError> {
        Ok(())
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if !self.can_switch {
            return Err(DriverError::Unsupported);
        }
        if let Some(refusal) = self.refuse {
            return Err(refusal);
        }
        self.switches.push(power);
        Ok(())
    }
}

/// A saver bundle offered as Waves, declaring `feeds` and one choice.
fn saver(feeds: u32) -> Saver {
    let choice = |value: &str, label: &str| Choice {
        value: ChoiceValue::new(value).expect("a word"),
        label: BoundedText::new(label).expect("a label"),
    };
    let detail = SaverOption::new(
        OptionKey::new("detail").expect("a key"),
        (
            BoundedText::new("Detail").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Choice {
            choices: Choices::new(&[choice("simple", "Simple"), choice("maximum", "Maximum")])
                .expect("choices"),
            default: 0,
        },
    )
    .expect("an option");
    Saver {
        choice: String::from("os.tairix.saver.waves"),
        id: BundleId::new("os.tairix.saver.waves").ok(),
        bundle: String::from("/System/Screensavers/Waves.app"),
        origin: SaverOrigin::System,
        title: String::from("Waves"),
        purpose: String::new(),
        feeds: SaverFeeds::from_bits(feeds).expect("feeds"),
        parallel: false,
        large_memory: false,
        options: alloc::vec![detail],
        standing: SaverStanding::Offered,
    }
}

fn context(values: &SaverValues) -> SaverContext<'_> {
    SaverContext {
        screen: (1920, 1080),
        scale: Scale::ONE,
        refresh: RefreshRate::ASSUMED,
        look: (
            Appearance::Dark,
            None,
            Contrast::Normal,
            Density::Normal,
            false,
        ),
        seed: [3; tairix_abi::saver_ipc::SAVER_SEED_LEN],
        pressure: PressureBand::Normal,
        machine: (4, 8 << 30),
        identity: ("ada", "lovelace"),
        slides: 0,
        values,
        names: TaskNames::Own,
    }
}

/// Cover the screen black at `now_ns`, asserting it covered it.
fn start_blank(saver: &mut Saving, comp: &mut Compositor, now_ns: u64) {
    let setup = SaverSetup::<Launches> {
        ground: None,
        launch: None,
    };
    assert_eq!(saver.start(setup, comp, now_ns), Ok(true));
}

/// Host `bundle` over pipes held in memory, answering the pipes.
fn host<S: Sink + Clone>(
    saver: &mut Screensaver<Pipes, S>,
    bundle: &Saver,
    comp: &mut Compositor,
    now_ns: u64,
) -> Pipes {
    let values = SaverValues::default();
    let mut launches = Launches::default();
    let setup = SaverSetup {
        ground: None,
        launch: Some(SaverLaunch {
            saver: bundle,
            context: context(&values),
            launcher: &mut launches,
        }),
    };
    assert_eq!(saver.start(setup, comp, now_ns), Ok(true));
    assert_eq!(launches.entries, ["/System/Screensavers/Waves.app/Run"]);
    launches.pipes.pop().expect("launched")
}

fn centre(compositor: &Compositor) -> Point {
    let screen = compositor.screen_rect();
    Point::new(
        screen.left() + i32::try_from(screen.width / 2).expect("small"),
        screen.top() + i32::try_from(screen.height / 2).expect("small"),
    )
}

/// Flush what the screensaver queued for the saver.
fn flush<S: Sink + Clone>(saver: &mut Screensaver<Pipes, S>, comp: &mut Compositor) {
    assert!(saver.on_writable(0, comp).is_none());
}

#[test]
fn a_screensaver_covers_the_screen_until_it_is_dismissed() {
    let mut comp = compositor();
    let behind = comp.add_window(Point::new(0, 0), Surface::new(64, 64).expect("a surface"));
    let mut saver = Saving::new(DiscardSink);
    start_blank(&mut saver, &mut comp, 0);
    assert!(saver.is_shown());
    let over = comp.window_at(centre(&comp)).expect("something is on top");
    assert_ne!(over, behind);
    assert_eq!(comp.window_at(Point::new(1, 1)), Some(over), "every pixel");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    assert!(!saver.is_shown());
    assert_eq!(comp.window_at(Point::new(1, 1)), Some(behind));
    assert_eq!(
        saver.dismiss(&mut comp, None),
        Ok(false),
        "nothing left to dismiss"
    );
}

#[test]
fn a_screensaver_hides_the_pointer_until_it_goes() {
    let mut comp = compositor();
    crate::tests::shell().refresh_cursor(&mut comp);
    assert!(comp.cursor_bounds().is_some(), "a pointer to hide");
    let mut saver = Saving::new(DiscardSink);
    let _pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    assert!(saver.window().is_some());
    assert_eq!(comp.cursor_bounds(), None, "no pointer over it");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    assert!(comp.cursor_bounds().is_some(), "the pointer is back");
}

fn saver_bundle() -> Saver {
    saver(0)
}

#[test]
fn a_window_raised_behind_the_screensaver_goes_back_beneath_it() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    start_blank(&mut saver, &mut comp, 0);
    let saver_window = comp.window_at(Point::new(1, 1));
    let late = comp.add_window(Point::new(0, 0), Surface::new(64, 64).expect("a surface"));
    comp.raise(late);
    saver.keep_topmost(&mut comp);
    assert_eq!(comp.window_at(Point::new(1, 1)), saver_window);
}

#[test]
fn a_dimmed_screensaver_is_the_ground_it_was_given() {
    let mut comp = compositor();
    let screen = comp.screen_rect();
    let mut ground = Surface::new(screen.width, screen.height).expect("a ground");
    ground.fill(Color::rgb(200, 200, 200));
    let mut saver = Saving::new(DiscardSink);
    let setup = SaverSetup::<Launches> {
        ground: Some(ground),
        launch: None,
    };
    assert_eq!(saver.start(setup, &mut comp, 0), Ok(true));
    assert!(saver.is_shown());
    assert_eq!(
        saver.park_deadline_ns(0, u64::MAX),
        u64::MAX,
        "nothing to advance"
    );
}

/// A hosted saver is started from its manifest's options, given the values
/// the user chose or their defaults, and held to its ceilings.
#[test]
fn a_saver_is_started_with_its_options_and_held_to_its_ceilings() {
    let bundle = saver(0);
    let mut values = SaverValues::default();
    let id = bundle.id.expect("an identifier");
    assert!(values.set(
        &id,
        &bundle.options[0],
        tairix_abi::saver::SaverValue::Choice(ChoiceValue::new("maximum").expect("a word")),
    ));
    let start = saver_start(&bundle, &context(&values), 77).expect("a start");
    assert_eq!(start.now_ns, 77);
    assert_eq!(
        start
            .option("detail")
            .map(|value| value == bundle.options[0].default_value()),
        Some(false)
    );
    assert_eq!(start.account.as_str(), "ada");
    assert_eq!(start.threads, 1, "not a parallel saver");
    let (memory, threads) = ceilings(&bundle, start.region.len(), (4, 8 << 30));
    assert_eq!(start.memory, memory);
    assert_eq!(threads, 1);
    let mut parallel = saver(0);
    parallel.parallel = true;
    parallel.large_memory = true;
    let (large, cores) = ceilings(&parallel, start.region.len(), (4, 8 << 30));
    assert_eq!(cores, 4);
    assert!(large > memory);
    let (held, _) = ceilings(&parallel, start.region.len(), (4, 1 << 30));
    assert_eq!(held, 1 << 29, "never more than half the machine");
}

/// A saver that cannot be started leaves the screen covered black, and says
/// why.
#[test]
fn a_saver_that_cannot_be_started_leaves_the_screen_black() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    let bundle = saver_bundle();
    let values = SaverValues::default();
    let mut launches = Launches {
        refuse: Some(Errno::NotFound),
        ..Launches::default()
    };
    let setup = SaverSetup {
        ground: None,
        launch: Some(SaverLaunch {
            saver: &bundle,
            context: context(&values),
            launcher: &mut launches,
        }),
    };
    let refused = saver.start(setup, &mut comp, 0).expect_err("refused");
    assert_eq!(refused.title, "Waves");
    assert_eq!(refused.ended, Ended::Refused(Errno::NotFound));
    assert!(saver.is_shown(), "the screen is still covered");
}

/// A saver that misbehaves is ended, the screen laid black, and the end
/// said; a saver missing its start deadline likewise.
#[test]
fn a_saver_that_misbehaves_or_never_starts_is_ended_and_the_screen_blacked() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    let pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    flush(&mut saver, &mut comp);
    pipes.say(&SaverMessage::Presented {
        serial: 9,
        damage: None,
        next: tairix_abi::saver_ipc::NextFrame::Idle,
    });
    let mut errands = super::fixtures::Errands::default();
    let ended = saver
        .on_readable(1, &mut comp, &mut errands)
        .expect("ended");
    assert_eq!(ended.ended, Ended::Faulted(Fault::OutOfTurn));
    assert_eq!(ended.code, Some(0), "reaped");
    assert!(saver.is_shown(), "the screen stays covered");
    assert_eq!(
        saver.park_deadline_ns(1, u64::MAX),
        u64::MAX,
        "nothing hosted"
    );

    let mut late = Saving::new(DiscardSink);
    let _pipes = host(&mut late, &saver_bundle(), &mut comp, 0);
    let wall = WallReading::unset(0);
    assert!(late.advance(1, &wall, &mut comp).is_none());
    assert_eq!(
        late.park_deadline_ns(1, u64::MAX),
        SAVER_START_DEADLINE_NS - 1,
        "woken for the start deadline"
    );
    let ended = late
        .advance(SAVER_START_DEADLINE_NS, &wall, &mut comp)
        .expect("ended");
    assert_eq!(ended.ended, Ended::Faulted(Fault::NeverReady));
}

/// Machine reports are wanted only by a ready saver given the machine feed,
/// on an awake display.
#[test]
fn only_a_ready_saver_given_the_machine_feed_wants_reports() {
    let mut comp = compositor();
    let mut plain = Saving::new(DiscardSink);
    let pipes = host(&mut plain, &saver(0), &mut comp, 0);
    pipes.say(&SaverMessage::Ready);
    let mut errands = super::fixtures::Errands::default();
    assert!(plain.on_readable(0, &mut comp, &mut errands).is_none());
    assert!(!plain.wants_machine_reports());

    let mut fed = Saving::new(DiscardSink);
    let pipes = host(&mut fed, &saver(APPINFO_FLAG_FEED_MACHINE), &mut comp, 0);
    assert!(!fed.wants_machine_reports(), "not before it is ready");
    pipes.say(&SaverMessage::Ready);
    assert!(fed.on_readable(0, &mut comp, &mut errands).is_none());
    assert!(fed.wants_machine_reports());
}

#[test]
fn a_preview_holds_through_motion_for_its_first_moment_only() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    let setup = SaverSetup::<Launches> {
        ground: None,
        launch: None,
    };
    assert_eq!(saver.start_preview(setup, &mut comp, 100), Ok(true));
    assert!(!saver.woken_by(Waking::Moved, 100));
    assert!(!saver.woken_by(Waking::Moved, 100 + PREVIEW_STEADY_NS - 1));
    assert!(saver.woken_by(Waking::Moved, 100 + PREVIEW_STEADY_NS));
    assert!(saver.woken_by(Waking::Acted, 100), "a deliberate gesture");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));

    start_blank(&mut saver, &mut comp, 100);
    assert!(
        saver.woken_by(Waking::Moved, 100),
        "a screensaver the idle deadline started keeps no grace"
    );
}

/// A preview asked for over a screensaver already up changes nothing: not
/// the saver, and not the rule that takes it down.
#[test]
fn a_preview_over_a_running_screensaver_leaves_it_as_it_is() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    assert!(saver.would_start());
    start_blank(&mut saver, &mut comp, 0);
    assert!(
        !saver.would_start(),
        "so the embedder prepares nothing over the screensaver up"
    );
    let window = saver.window();
    let setup = SaverSetup::<Launches> {
        ground: None,
        launch: None,
    };
    assert_eq!(saver.start_preview(setup, &mut comp, 0), Ok(true));
    assert_eq!(saver.window(), window);
    assert!(saver.woken_by(Waking::Moved, 0));
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    assert!(
        saver.would_start(),
        "taken down, the next start covers anew"
    );
}

#[test]
fn a_display_that_can_switch_off_goes_dark_and_wakes_before_the_screensaver_goes() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Saving::new(DiscardSink);
    let _pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    assert!(saver.is_dark());
    assert_eq!(
        saver.saver_descriptors(),
        (None, false, false),
        "a dark screen lets its saver go"
    );
    assert_eq!(
        saver.park_deadline_ns(0, u64::MAX),
        u64::MAX,
        "a dark screen draws nothing and arms nothing"
    );
    assert_eq!(saver.switch_display_off(&mut comp, Some(&mut panel)), None);

    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
    assert!(!saver.is_dark());
    assert!(!saver.is_shown());
}

/// A display with no power control is not left lit and moving: the screen
/// goes black and still, and its saver is ended.
#[test]
fn a_display_that_cannot_switch_off_is_kept_black_and_still() {
    let mut comp = compositor();
    let mut panel = Panel::fixed();
    let mut saver = Saving::new(DiscardSink);
    let _pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Blanked)
    );
    assert!(!saver.is_dark(), "a lit screen is still presented");
    assert!(saver.is_shown());
    assert_eq!(saver.park_deadline_ns(0, u64::MAX), u64::MAX);
    comp.composite();
    assert_eq!(comp.frame()[..4], [0, 0, 0, 255], "black");
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert!(panel.switches.is_empty(), "nothing to switch back on");
}

#[test]
fn a_refused_switch_is_stated_and_the_screen_kept_black() {
    let mut comp = compositor();
    let mut panel = Panel {
        refuse: Some(DriverError::DeviceFault),
        ..Panel::switchable()
    };
    let mut saver = Saving::new(DiscardSink);
    start_blank(&mut saver, &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Refused(DriverError::DeviceFault))
    );
    assert!(!saver.is_dark());
}
/// A display that will not light again keeps the screensaver up and dark:
/// presenting to a screen nobody can see helps nobody, and the next input
/// asks again.
#[test]
fn a_display_that_will_not_wake_keeps_the_screensaver_up_and_asks_again() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Saving::new(DiscardSink);
    start_blank(&mut saver, &mut comp, 0);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    panel.refuse = Some(DriverError::DeviceFault);
    assert_eq!(
        saver.dismiss(&mut comp, Some(&mut panel)),
        Err(DriverError::DeviceFault)
    );
    assert!(saver.is_shown());
    assert!(saver.is_dark());
    panel.refuse = None;
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert!(!saver.is_shown());
}

/// The display is the screensaver's to wake even when its surface never came
/// up: a machine short of memory still sleeps its display, and still wakes it.
#[test]
fn a_display_asleep_without_a_surface_is_still_woken() {
    let mut comp = compositor();
    let mut panel = Panel::switchable();
    let mut saver = Saving::new(DiscardSink);
    assert_eq!(
        saver.switch_display_off(&mut comp, Some(&mut panel)),
        Some(SwitchedOff::Off)
    );
    assert!(saver.is_shown(), "the next input only wakes the screen");
    assert_eq!(saver.dismiss(&mut comp, Some(&mut panel)), Ok(true));
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
}

/// With no screensaver up and a display that will not switch off, the desktop
/// is left lit and live: the next input is the user's, not swallowed.
#[test]
fn a_refused_switch_without_a_surface_leaves_the_desktop_live() {
    let mut comp = compositor();
    for mut panel in [
        Panel::fixed(),
        Panel {
            refuse: Some(DriverError::DeviceFault),
            ..Panel::switchable()
        },
    ] {
        let mut saver = Saving::new(DiscardSink);
        assert!(saver
            .switch_display_off(&mut comp, Some(&mut panel))
            .is_some());
        assert!(!saver.is_shown(), "input reaches the desktop");
        assert!(!saver.is_dark());
    }
}

/// The machine's pressure reaches a saver at the depth the session reads it.
#[test]
fn every_pressure_band_reaches_a_saver_at_its_own_depth() {
    for band in [
        PressureBand::Normal,
        PressureBand::Mild,
        PressureBand::Moderate,
        PressureBand::Severe,
        PressureBand::Critical,
    ] {
        assert_eq!(super::memory_band(band).depth(), band.depth(), "{band:?}");
    }
    let values = SaverValues::default();
    let mut given = context(&values);
    given.pressure = PressureBand::Severe;
    let start = saver_start(&saver(0), &given, 0).expect("a start");
    assert_eq!(start.pressure, MemoryBand::new(3).expect("a band"));
}

/// An errand's answer reaches only the saver that asked: one landing after
/// that saver went, when another draws, is dropped.
#[test]
fn an_answer_reaches_only_the_saver_that_asked_for_it() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    let _first = host(&mut saver, &saver_bundle(), &mut comp, 0);
    let asked = saver.hosting().expect("a saver draws");
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    let second = host(&mut saver, &saver_bundle(), &mut comp, 0);
    let drawing = saver.hosting().expect("another draws");
    assert_ne!(asked, drawing);
    flush(&mut saver, &mut comp);
    let heard = second.heard().len();
    assert!(saver
        .font_answered(asked, (Ok(&[1, 2, 3]), 1), &mut comp)
        .is_none());
    assert!(saver.kept(asked, (true, 1), &mut comp).is_none());
    flush(&mut saver, &mut comp);
    assert_eq!(second.heard().len(), heard, "nothing was sent to it");
}

/// A saver whose pipes cannot be watched is ended, and the screen kept
/// black, with why.
#[test]
fn a_saver_that_cannot_be_watched_is_ended() {
    let mut comp = compositor();
    let mut saver = Saving::new(DiscardSink);
    let _pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    let ended = saver
        .unwatched(Errno::OutOfMemory, &mut comp)
        .expect("ended");
    assert_eq!(ended.ended, Ended::Unwatched(Errno::OutOfMemory));
    assert_eq!(saver.hosting(), None);
    assert!(saver.is_shown(), "the screen stays covered");
}

/// What the session says of a saver it ended names the saver and why, and
/// that the screen is black.
#[test]
fn an_ended_saver_is_said_by_name_and_reason() {
    let said = |ended, code| {
        alloc::format!(
            "{}",
            SaverEnd {
                saver: String::from("os.tairix.saver.waves"),
                title: String::from("Waves"),
                ended,
                code,
            }
        )
    };
    assert_eq!(
        said(Ended::Faulted(Fault::Late), Some(9)),
        "the screensaver Waves did not draw a frame in time; the screen is kept black"
    );
    assert_eq!(
        said(Ended::Gone, Some(3)),
        "the screensaver Waves stopped with exit code 3; the screen is kept black"
    );
    assert!(said(Ended::Refused(Errno::NotFound), None).contains("could not be started"));
}

/// A field list of one `saver` record, as the log holds it.
fn naming(saver: &str) -> Vec<(String, String)> {
    alloc::vec![(String::from("saver"), String::from(saver))]
}

/// Draw one frame of the saver at the far end of `pipes` at `now_ns`: asked
/// for, presented changing its first pixel, and taken in.
fn present<S: Sink + Clone>(
    saver: &mut Screensaver<Pipes, S>,
    pipes: &Pipes,
    (serial, now_ns): (u32, u64),
    comp: &mut Compositor,
) {
    let wall = WallReading::unset(now_ns);
    assert!(saver.advance(now_ns, &wall, comp).is_none());
    flush(saver, comp);
    let corner = DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    };
    pipes.say(&SaverMessage::Presented {
        serial,
        damage: DamageList::new(&[corner]).ok(),
        next: tairix_abi::saver_ipc::NextFrame::Soon,
    });
    let mut errands = super::fixtures::Errands::default();
    assert!(saver.on_readable(now_ns, comp, &mut errands).is_none());
}

/// A hosted saver is recorded by its identifier when its first frame reaches
/// the screen, once however many follow, and again when it is dismissed.
#[test]
fn a_saver_is_recorded_when_first_shown_and_when_dismissed() {
    let mut comp = compositor();
    let log = Records::default();
    let mut saver = Screensaver::<Pipes, Records>::new(log.clone());
    let pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    pipes.say(&SaverMessage::Ready);
    let mut errands = super::fixtures::Errands::default();
    assert!(saver.on_readable(0, &mut comp, &mut errands).is_none());
    assert!(log.of(SAVER_SHOWN).is_empty(), "nothing presented yet");
    present(&mut saver, &pipes, (1, 1), &mut comp);
    present(&mut saver, &pipes, (2, 1_000_000_000), &mut comp);
    let mut shown = naming("os.tairix.saver.waves");
    shown.push((String::from("preview"), String::from("false")));
    assert_eq!(log.of(SAVER_SHOWN), [shown]);
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));
    assert_eq!(log.of(SAVER_DISMISSED), [naming("os.tairix.saver.waves")]);
    assert!(log.of(SAVER_ENDED).is_empty());
}

/// A saver ended for misbehaving, or one that could not be started, is
/// recorded with why; neither is recorded as dismissed.
#[test]
fn a_saver_ended_or_refused_is_recorded_with_why() {
    let mut comp = compositor();
    let log = Records::default();
    let mut saver = Screensaver::<Pipes, Records>::new(log.clone());
    let pipes = host(&mut saver, &saver_bundle(), &mut comp, 0);
    flush(&mut saver, &mut comp);
    pipes.say(&SaverMessage::Presented {
        serial: 9,
        damage: None,
        next: tairix_abi::saver_ipc::NextFrame::Idle,
    });
    let mut errands = super::fixtures::Errands::default();
    assert!(saver.on_readable(1, &mut comp, &mut errands).is_some());
    assert_eq!(saver.dismiss(&mut comp, None), Ok(true));

    let bundle = saver_bundle();
    let values = SaverValues::default();
    let mut launches = Launches {
        refuse: Some(Errno::NotFound),
        ..Launches::default()
    };
    let setup = SaverSetup {
        ground: None,
        launch: Some(SaverLaunch {
            saver: &bundle,
            context: context(&values),
            launcher: &mut launches,
        }),
    };
    assert!(saver.start(setup, &mut comp, 2).is_err());

    let reason = |why: &str| {
        let mut fields = naming("os.tairix.saver.waves");
        fields.push((String::from("reason"), String::from(why)));
        fields
    };
    assert_eq!(
        log.of(SAVER_ENDED),
        [
            reason("broke the screensaver protocol"),
            reason(&alloc::format!("could not be started: {}", Errno::NotFound)),
        ]
    );
    assert!(
        log.of(SAVER_DISMISSED).is_empty(),
        "its end was recorded once"
    );
    assert!(log.of(SAVER_SHOWN).is_empty());
}
