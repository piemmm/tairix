use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{Display, DisplayFormat, DisplayMode, DisplayPower};
use tairix_abi::input::{PointerButtonCode, PointerInput};
use tairix_abi::session_ipc::{SessionRequest, SessionVerdict};
use tairix_abi::time::{Duration64, Time64};
use tairix_abi::{DriverError, Errno, WAITSET_TIMEOUT_NONE};
use tairix_cursor::{CursorImage, PlacedCursor};
use tairix_display::ChannelOrder;
use tairix_display::SwitchedOff;
use tairix_geometry::{Point, Rect, Scale};
use tairix_greeter::{AccountTile, Verdict};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::Pixel;
use tairix_theme::motion::SceneClock;
use tairix_theme::{MotionInteraction, Theme, Timeline};

use super::{LoginScreen, Step};
use crate::accounts::SessionTransport;
use crate::cursor::pointer_image;
use crate::frame::{rect_of, Present, Scanout};
use crate::wait::ENERGY_SAVING_AFTER_NS;

const SECRET: &str = "open-sesame";

/// A screen large enough for the panel and a row of tiles.
fn mode() -> DisplayMode {
    DisplayMode {
        width_px: 1000,
        height_px: 600,
        stride_bytes: 1000 * 4,
        format: DisplayFormat::Bgra8888,
    }
}

/// An authority accepting exactly one account's one secret and refusing
/// everything else with a fixed lockout.
struct Authority {
    account: &'static str,
    secret: &'static str,
    lockout: Duration64,
    reachable: bool,
}

impl Authority {
    const fn accepting(account: &'static str, secret: &'static str) -> Self {
        Self {
            account,
            secret,
            lockout: Duration64::from_secs(20),
            reachable: true,
        }
    }

    const fn unreachable() -> Self {
        Self {
            account: "",
            secret: "",
            lockout: Duration64::ZERO,
            reachable: false,
        }
    }
}

impl SessionTransport for Authority {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        if !self.reachable {
            return Err(Errno::TimedOut);
        }
        let Ok(SessionRequest::Authenticate { username, password }) =
            SessionRequest::decode(request)
        else {
            return Err(Errno::OutOfRange);
        };
        if username == self.account && password == self.secret {
            SessionVerdict::Accepted.encode(reply)
        } else {
            SessionVerdict::Refused {
                retry_after: self.lockout,
            }
            .encode(reply)
        }
    }
}

fn screen(accounts: Vec<AccountTile>, authority: Authority) -> LoginScreen<Authority> {
    screen_in(accounts, authority, Theme::dark())
}

fn screen_in(
    accounts: Vec<AccountTile>,
    authority: Authority,
    theme: Theme,
) -> LoginScreen<Authority> {
    LoginScreen::new(
        Scanout::new(mode()).expect("a valid mode"),
        theme,
        Scale::ONE,
        "TAIRiX 0.0.0 (tairix)".to_string(),
        accounts,
        authority,
    )
}

/// The shipped theme with reduced motion: every animation lands at once.
fn still() -> Theme {
    let base = Theme::dark();
    Theme::new(
        base.id(),
        base.name(),
        base.appearance(),
        *base.palette(),
        *base.metrics(),
        *base.fonts(),
        base.cursors().clone(),
        base.motion().with_reduced_motion(true),
        base.density(),
        base.contrast(),
    )
}

/// The park timeout of a screen at rest at `now_ns` whose last input came at
/// zero: the wait for its display's sleep, and nothing nearer.
fn resting(now_ns: u64) -> u64 {
    ENERGY_SAVING_AFTER_NS - now_ns
}

/// A moment past every animation a round can have started.
///
/// A deadline assertion about the screen *at rest* is made here, so that
/// picking an account or being refused — both of which animate — is over
/// rather than still asking for frames.
fn settled_ns() -> u64 {
    let motion = Theme::dark().motion();
    let longest = MotionInteraction::ALL
        .iter()
        .map(|interaction| u64::from(motion.duration(*interaction)))
        .max()
        .unwrap_or(0);
    longest * 1_000_000 + 1
}

fn key(named: NamedKey) -> InputEvent {
    InputEvent::KeyPressed {
        key: Key::Named(named),
        modifiers: Modifiers::default(),
    }
}

fn typed(ch: char) -> InputEvent {
    InputEvent::KeyPressed {
        key: Key::Char(ch),
        modifiers: Modifiers::default(),
    }
}

/// The arrow the login screen draws, at the scale every test runs at.
fn arrow() -> CursorImage {
    pointer_image(Scale::ONE).expect("the built-in arrow renders")
}

/// Where the pointer starts: the middle of the screen.
fn centre() -> (i32, i32) {
    (
        i32::try_from(mode().width_px / 2).expect("a small screen"),
        i32::try_from(mode().height_px / 2).expect("a small screen"),
    )
}

/// The screen rectangle `arrow` covers with its hotspot at `(x, y)`.
fn cursor_rect(image: &CursorImage, x: i32, y: i32) -> Rect {
    let hotspot = image.hotspot();
    Rect::new(x - hotspot.x, y - hotspot.y, image.width(), image.height())
}

/// Relative motion carrying the pointer from `(fx, fy)` to `(tx, ty)`.
fn moved_from(from: (i32, i32), to: (i32, i32)) -> PointerInput {
    PointerInput::MovedBy {
        dx: to.0 - from.0,
        dy: to.1 - from.1,
    }
}

/// How bright the frame's pixel at `(x, y)` is, summed over its three
/// colour channels. The fourth byte is alpha, which is not colour.
fn brightness(frame: &[u8], x: u32, y: u32) -> u32 {
    pixel_at(frame, x, y)
        .iter()
        .take(3)
        .map(|channel| u32::from(*channel))
        .sum()
}

/// The four scan-out bytes of the frame at `(x, y)`.
fn pixel_at(frame: &[u8], x: u32, y: u32) -> &[u8] {
    let stride = usize::try_from(mode().stride_bytes).expect("a small stride");
    let at = usize::try_from(y).expect("a small screen") * stride
        + usize::try_from(x).expect("a small screen") * 4;
    &frame[at..at + 4]
}

/// Every screen position where the frame differs from the kept surface's
/// own pixel — everywhere the composer drew something over it.
///
/// The frame is that surface encoded for scan-out with the cursor sampled
/// on top, so this is exactly the arrow's ink, and empty when no pointer is
/// drawn. Only meaningful straight after a whole-screen composition: a
/// frame composed within a damage rectangle is deliberately older than the
/// surface outside it.
fn drawn_over(login: &LoginScreen<Authority>) -> Vec<(i32, i32)> {
    let order = ChannelOrder::for_format(mode().format).expect("a format the frame encodes");
    let surface = login.painted.as_ref().expect("a surface is kept");
    let frame = login.frame();
    let mut found = Vec::new();
    for y in 0..mode().height_px {
        for x in 0..mode().width_px {
            let Some(pixel) = surface.get(x, y) else {
                continue;
            };
            if pixel_at(frame, x, y) != order.encode(pixel).as_slice() {
                found.push((
                    i32::try_from(x).expect("a small screen"),
                    i32::try_from(y).expect("a small screen"),
                ));
            }
        }
    }
    found
}

/// Whether `present` hands the display every pixel of `rect`.
fn covers(present: Present, rect: Rect) -> bool {
    match present {
        Present::Nothing => false,
        Present::Whole => true,
        Present::Region(region) => {
            let Some(region) = rect_of(region) else {
                return false;
            };
            region.union(&rect) == region
        }
    }
}

/// Every screen position where `left` and `right` differ.
fn differing(left: &[u8], right: &[u8]) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    for y in 0..mode().height_px {
        for x in 0..mode().width_px {
            if pixel_at(left, x, y) != pixel_at(right, x, y) {
                found.push((
                    i32::try_from(x).expect("a small screen"),
                    i32::try_from(y).expect("a small screen"),
                ));
            }
        }
    }
    found
}

/// A colour no render of this screen produces.
const MARK: Pixel = Pixel {
    r: 1,
    g: 2,
    b: 3,
    a: 255,
};

/// A screen with its pointer and its first frame already up.
fn ready() -> LoginScreen<Authority> {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.set_pointer(arrow());
    login.repaint();
    login
}

/// Stamp [`MARK`] into the kept surface at `at`.
///
/// A render cannot produce that colour, so a surface still carrying the
/// stamp is demonstrably the one that was rendered before it — which is how
/// these tests count renders without the screen counting them for them.
fn stamp(login: &mut LoginScreen<Authority>, at: (i32, i32)) {
    let surface = login.painted.as_mut().expect("a surface is kept");
    surface.set(on_screen(at.0), on_screen(at.1), MARK);
}

/// The kept surface's own pixel at `at`, or `None` when no surface is kept.
fn kept(login: &LoginScreen<Authority>, at: (i32, i32)) -> Option<Pixel> {
    let surface = login.painted.as_ref()?;
    Some(
        surface
            .get(on_screen(at.0), on_screen(at.1))
            .expect("a pixel on the screen"),
    )
}

fn on_screen(value: i32) -> u32 {
    u32::try_from(value).expect("a coordinate on the screen")
}

/// Pick the focused tile, then type `secret` and submit it. Returns the step
/// the submitting event produced.
fn offer<T: SessionTransport>(
    screen: &mut LoginScreen<T>,
    secret: &str,
    now_ns: u64,
) -> crate::Step {
    screen.on_input(&key(NamedKey::Enter), now_ns);
    for ch in secret.chars() {
        screen.on_input(&typed(ch), now_ns);
    }
    screen.on_input(&key(NamedKey::Enter), now_ns)
}

#[test]
fn the_first_frame_covers_the_whole_screen() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    assert_eq!(login.repaint(), Present::Whole);
    assert!(
        login.frame().iter().any(|byte| *byte != 0),
        "the first frame drew something"
    );
}

#[test]
fn a_verified_secret_finishes_the_screen() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let step = offer(&mut login, SECRET, 0);
    assert!(step.verified);
    assert_eq!(
        step.answer.map(|answer| answer.verdict),
        Some(Verdict::Verified)
    );
}

/// The login screen appears out of the black it was handed the display in,
/// so the chooser is never cut onto a cleared screen.
///
/// The park loop runs it: the refresh steps the veil and the timeout is what
/// asks for the next frame, exactly as it does for every other animation.
#[test]
fn the_opening_frame_is_black_and_the_chooser_appears_out_of_it() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );

    let opening = login.begin_entry_fade(0);
    assert!(covers(opening, login.screen()), "the whole screen is shown");

    let (x, y) = (mode().width_px / 2, mode().height_px / 2);
    assert_eq!(brightness(login.frame(), x, y), 0, "and every pixel of it");
    assert!(!login.session_fade_finished(), "arriving is not leaving");

    let mut now = 0;
    let mut lightest = 0;
    let mut frames = 0u32;
    while login.park_timeout(now, None) < resting(now) {
        frames += 1;
        assert!(frames <= 1_000, "the fade never stopped asking for frames");
        now += login.park_timeout(now, None);
        login.refresh(now, None);
        let sample = brightness(login.frame(), x, y);
        assert!(sample >= lightest, "the veil darkened at frame {frames}");
        lightest = sample;
    }

    assert!(frames > 1, "it faded rather than cut, in {frames} frames");
    let arrived = login.frame().to_vec();
    login.repaint();
    assert!(
        differing(&arrived, login.frame()).is_empty(),
        "the screen it arrives at is the screen itself"
    );
}

/// A veil step re-blits the surface it already holds and never paints it
/// again.
///
/// The veil is a flat black field over everything, so painting it in would
/// have meant redrawing the chrome, both stages and every glyph and shadow to
/// change one number — the whole cost of a screen, once per frame of a fade.
/// The stamp is what proves the paint did not happen: a paint writes every
/// pixel, so a surviving stamp is a surface that was not painted.
#[test]
fn a_veil_step_reblits_the_surface_and_never_repaints_it() {
    let mut login = ready();
    login.repaint();
    stamp(&mut login, centre());
    let before = login.frame().to_vec();

    let opening = login.begin_session_fade(0);
    assert!(covers(opening, login.screen()), "the whole screen is shown");
    let Some(due) = login.session_fade_due(0) else {
        panic!("the shipped theme animates the session fade");
    };
    login.session_fade_step(due);

    assert_eq!(
        kept(&login, centre()),
        Some(MARK),
        "the painted surface was not painted again"
    );
    assert!(
        !differing(&before, login.frame()).is_empty(),
        "yet the frame on screen darkened"
    );
}

/// A paint repaints the buffer it already holds rather than allocating a
/// screen-sized one per frame.
///
/// An animated frame would otherwise map, zero and unmap a screenful of pixels
/// to draw the same picture one step on. Every paint writes every pixel, so
/// reusing the buffer holds exactly what a fresh one would.
#[test]
fn a_repaint_reuses_the_buffer_it_already_holds() {
    let mut login = ready();
    login.on_input(&key(NamedKey::Enter), 0);
    let held = login
        .painted
        .as_ref()
        .expect("a surface is kept")
        .pixels()
        .as_ptr();

    stamp(&mut login, centre());
    assert_ne!(login.on_input(&typed('x'), 0).present, Present::Nothing);

    assert_ne!(
        kept(&login, centre()),
        Some(MARK),
        "the keystroke genuinely painted the buffer again"
    );
    assert_eq!(
        login
            .painted
            .as_ref()
            .expect("a surface is kept")
            .pixels()
            .as_ptr(),
        held,
        "and it is the same buffer, not a fresh screenful"
    );
}

/// A theme with no motion has nothing to arrive out of: the screen opens on
/// the chooser, with no veil to present and no frame owed for one.
#[test]
fn a_reduced_motion_screen_opens_on_the_chooser() {
    let mut login = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );

    assert_eq!(login.begin_entry_fade(0), Present::Nothing);
    assert_eq!(
        login.park_timeout(0, None),
        resting(0),
        "nothing but the display's sleep is armed"
    );

    let opening = login.repaint();
    assert!(covers(opening, login.screen()));
    assert!(
        brightness(login.frame(), mode().width_px / 2, 0) > 0,
        "the screen it opens on is the chooser, not the black"
    );

    let mut plain = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );
    plain.repaint();
    assert!(
        differing(login.frame(), plain.frame()).is_empty(),
        "and it is the same screen as one that never faded"
    );
}

/// A verified secret takes the screen to black before the process leaves,
/// so the desktop coming up out of the same black reads as one movement.
#[test]
fn a_verified_secret_fades_the_screen_to_black() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    assert!(offer(&mut login, SECRET, 0).verified);

    // Typing a secret takes longer than picking the account animates, so the
    // screen the fade covers is a settled one.
    let start = settled_ns();
    login.refresh(start, None);

    let opening = login.begin_session_fade(start);
    assert_ne!(opening, Present::Nothing, "the fade has a first frame");
    assert!(!login.session_fade_finished());

    let (x, y) = (mode().width_px / 2, mode().height_px / 2);
    let mut darkest = 3 * 255u32;
    let mut now = start;
    let mut frames = 0u32;
    while let Some(due) = login.session_fade_due(now) {
        frames += 1;
        assert!(
            frames <= login.session_fade_budget(),
            "the fade asked for more frames than it can need"
        );
        now += due;
        login.session_fade_step(now);
        let sample = brightness(login.frame(), x, y);
        assert!(sample <= darkest, "the veil lightened at frame {frames}");
        darkest = sample;
    }

    assert!(login.session_fade_finished(), "the screen is black");
    assert_eq!(darkest, 0, "and every channel of it is");
    assert!(frames > 1, "it faded rather than cut, in {frames} frames");
}

/// The fade ends on the clock and its own budget alone.
///
/// Nothing about it reads the display's answer, so a present the display
/// refuses cannot keep a successful login from leaving; and a clock that
/// stopped, or a seat that reads ready forever, runs the budget out instead
/// of spinning.
#[test]
fn the_fade_ends_on_the_clock_and_the_budget_alone() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    assert!(offer(&mut login, SECRET, 0).verified);

    // Every frame's present dropped on the floor, as a refusing display
    // would leave it.
    let _ = login.begin_session_fade(0);
    let span = settled_ns();
    let _ = login.session_fade_step(span);
    assert!(
        login.session_fade_finished(),
        "the clock alone took it to black"
    );
    assert_eq!(
        login.session_fade_due(span),
        None,
        "and it asks for no more"
    );

    let mut stuck = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    stuck.repaint();
    let _ = stuck.begin_session_fade(0);
    for _ in 0..stuck.session_fade_budget() {
        let _ = stuck.session_fade_step(0);
    }
    assert!(
        !stuck.session_fade_finished(),
        "a clock that never advances never finishes the fade — the budget is\n         what lets the login leave anyway"
    );
}

/// Once the screen has begun leaving, the decision is made: input is not
/// answered, and nothing it would have changed is drawn.
#[test]
fn input_during_the_fade_is_ignored() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    assert!(offer(&mut login, SECRET, 0).verified);
    let _ = login.begin_session_fade(0);

    for event in [key(NamedKey::Escape), typed('x'), key(NamedKey::Enter)] {
        let step = login.on_input(&event, 0);
        assert_eq!(
            step.present,
            Present::Nothing,
            "{event:?} painted something"
        );
        assert!(!step.verified);
        assert!(step.answer.is_none(), "{event:?} reached the authority");
    }
}

/// A reduced-motion theme has nothing to fade: the screen leaves at once,
/// with no extra frame presented.
#[test]
fn a_reduced_motion_fade_leaves_at_once() {
    let mut login = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );
    login.repaint();
    assert!(offer(&mut login, SECRET, 0).verified);

    assert_eq!(login.begin_session_fade(0), Present::Nothing);
    assert!(login.session_fade_finished());
    assert_eq!(login.session_fade_due(0), None);
}

/// A verified screen, its pointer drawn and its whole frame composed, at
/// the moment before it begins leaving.
fn about_to_leave() -> (LoginScreen<Authority>, u64) {
    let mut login = ready();
    assert!(offer(&mut login, SECRET, 0).verified);
    // Typing a secret takes longer than picking the account animates, so
    // what the fade covers is a settled screen.
    let start = settled_ns();
    login.refresh(start, None);
    login.repaint();
    (login, start)
}

/// The pointer leaves with the screen it belonged to. From the first veiled
/// frame nothing is drawn for it: the verdict is given, input is no longer
/// answered, and a bright arrow over the black would point at nothing.
#[test]
fn the_pointer_is_gone_from_the_first_veiled_frame() {
    let (mut login, start) = about_to_leave();
    assert!(
        !drawn_over(&login).is_empty(),
        "the arrow is on the frame to begin with"
    );

    login.begin_session_fade(start);

    assert_eq!(
        drawn_over(&login),
        Vec::new(),
        "the veiled frame is the veiled surface and nothing over it"
    );
}

/// The frame the pointer leaves on repaints where it sat, so no arrow can
/// be left burned into the presented bytes.
#[test]
fn the_frame_the_pointer_leaves_on_repaints_where_it_sat() {
    let (mut login, start) = about_to_leave();
    let sat = cursor_rect(&arrow(), centre().0, centre().1).intersection(&login.screen());
    assert!(!sat.is_empty(), "the arrow is on the screen");
    let ink = drawn_over(&login);
    let before = login.frame().to_vec();

    let opening = login.begin_session_fade(start);

    assert!(
        covers(opening, sat),
        "{opening:?} does not present the {sat:?} the arrow sat on"
    );
    // The veil opens fully transparent, so the only pixels this frame can
    // change are the ones the arrow was inking — and it changes all of them.
    assert_eq!(
        differing(&before, login.frame()),
        ink,
        "the frame changed somewhere other than where the arrow was"
    );
}

/// The pointer still tracks while the screen leaves; it is only not drawn.
/// Nothing is presented for a move nobody can see.
#[test]
fn a_move_during_the_fade_presents_nothing_and_still_tracks_the_pointer() {
    let (mut login, start) = about_to_leave();
    login.begin_session_fade(start);
    let veiled = login.frame().to_vec();

    let corner = (30, 30);
    let step = login.on_pointer(&moved_from(centre(), corner), start);

    assert_eq!(step.present, Present::Nothing, "a move nobody can see");
    assert_eq!(login.frame(), veiled.as_slice(), "and nothing was drawn");
    assert_eq!(login.cursor.at(), Point::new(corner.0, corner.1));
    assert_eq!(
        login.pointer.as_ref().map(PlacedCursor::bounds),
        Some(cursor_rect(&arrow(), corner.0, corner.1)),
        "the artwork followed the position it is not drawn at"
    );
}

/// Only the screen leaving hides the pointer. A screen animating for any
/// other reason — a refused attempt shaking, a lockout counting down —
/// draws it exactly where it sits, as it always did.
#[test]
fn an_unveiled_screen_still_draws_its_pointer() {
    let mut login = ready();
    let sits = cursor_rect(&arrow(), centre().0, centre().1);
    let confined = |ink: &[(i32, i32)]| {
        assert!(!ink.is_empty(), "the arrow is drawn");
        for (x, y) in ink {
            assert!(
                sits.contains(Point::new(*x, *y)),
                "({x}, {y}) is drawn outside the cursor at {sits:?}"
            );
        }
    };
    confined(&drawn_over(&login));

    assert!(!offer(&mut login, "wrong", 0).verified);
    login.refresh(Timeline::FRAME_NS, None);
    login.repaint();
    confined(&drawn_over(&login));
}

/// A reduced-motion theme has no veil to present, so the screen leaves on
/// the frame it was already showing — pointer and all. There is no veiled
/// frame for the arrow to be absent from.
#[test]
fn a_reduced_motion_fade_leaves_the_frame_as_it_was() {
    let mut login = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );
    login.set_pointer(arrow());
    login.repaint();
    assert!(offer(&mut login, SECRET, 0).verified);
    login.repaint();
    let showing = login.frame().to_vec();

    assert_eq!(login.begin_session_fade(0), Present::Nothing);
    assert_eq!(login.frame(), showing.as_slice());
    assert!(
        !drawn_over(&login).is_empty(),
        "the arrow is still on the frame the screen leaves on"
    );
}

#[test]
fn a_refusal_puts_its_lockout_on_the_screen_and_keeps_asking() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let step = offer(&mut login, "wrong", 0);
    assert!(!step.verified);
    assert_eq!(
        step.answer.map(|answer| answer.verdict),
        Some(Verdict::Refused)
    );
    assert_eq!(
        step.answer.map(|answer| answer.retry_after),
        Some(Duration64::from_secs(20))
    );
    assert!(
        login.notice().contains("20"),
        "the notice presents the lockout, got {:?}",
        login.notice()
    );
}

#[test]
fn a_lockout_counts_down_on_the_screen_and_clears() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    offer(&mut login, "wrong", 0);

    let after_ten = 10 * 1_000_000_000;
    let step = login.refresh(after_ten, None);
    assert_ne!(step.present, Present::Nothing, "the countdown repainted");
    assert!(login.notice().contains("10"));

    let expired = 21 * 1_000_000_000;
    login.refresh(expired, None);
    assert!(
        !login.notice().contains("10"),
        "the lockout cleared, got {:?}",
        login.notice()
    );
}

#[test]
fn an_unreachable_authority_keeps_the_surface_alive() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::unreachable(),
    );
    login.repaint();
    let step = offer(&mut login, SECRET, 0);
    assert!(!step.verified, "no answer is never a pass");
    assert_eq!(
        step.answer.map(|answer| answer.verdict),
        Some(Verdict::Unreachable)
    );
    assert_eq!(
        step.answer.map(|answer| answer.retry_after),
        Some(Duration64::ZERO),
        "an unanswerable attempt is not a lockout"
    );
    assert_eq!(
        login.park_timeout(settled_ns(), None),
        0,
        "the refusal's shake still owes the frame that ends it"
    );
    login.refresh(settled_ns(), None);
    assert_eq!(
        login.park_timeout(settled_ns(), None),
        resting(settled_ns()),
        "nothing is counting down, so only the display's sleep is armed"
    );

    let again = offer(&mut login, SECRET, 0);
    assert!(!again.verified, "the surface is still asking");
}

#[test]
fn no_accounts_still_reaches_the_authority_by_name() {
    let mut login = screen(Vec::new(), Authority::accepting("ann", SECRET));
    login.repaint();

    // The lone tile leads to a typed login name, then to the secret.
    login.on_input(&key(NamedKey::Enter), 0);
    for ch in "ann".chars() {
        login.on_input(&typed(ch), 0);
    }
    login.on_input(&key(NamedKey::Enter), 0);
    for ch in SECRET.chars() {
        login.on_input(&typed(ch), 0);
    }
    let step = login.on_input(&key(NamedKey::Enter), 0);
    assert!(step.verified);
}

#[test]
fn an_idle_screen_arms_only_its_sleep_and_a_clocked_one_wakes_at_the_minute() {
    let login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    assert_eq!(login.park_timeout(0, None), ENERGY_SAVING_AFTER_NS);

    // Twenty seconds past a minute boundary, so forty seconds to the next.
    let twenty_past = Time64::from_secs(1_700_000_060);
    assert_eq!(login.park_timeout(0, Some(twenty_past)), 40_000_000_000);
}

#[test]
fn a_running_lockout_is_the_nearer_deadline() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    offer(&mut login, "wrong", 0);
    let twenty_past = Time64::from_secs(1_700_000_060);
    assert_eq!(
        login.park_timeout(0, Some(twenty_past)),
        Timeline::FRAME_NS,
        "the refusal's shake is the nearest thing owing a frame"
    );
    assert_eq!(
        login.park_timeout(settled_ns(), Some(twenty_past)),
        0,
        "a shake whose span ran out still owes the frame that ends it"
    );
    login.refresh(settled_ns(), Some(twenty_past));
    assert_eq!(
        login.park_timeout(settled_ns(), Some(twenty_past)),
        1_000_000_000,
        "once it has settled, the lockout ticks before the minute turns"
    );
}

#[test]
fn a_keystroke_presents_only_what_it_changed() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    login.on_input(&key(NamedKey::Enter), 0);

    let step = login.on_input(&typed('x'), 0);
    let Present::Region(region) = step.present else {
        panic!("a keystroke touches the field, not the screen: {step:?}");
    };
    assert!(region.width_px < mode().width_px);
    assert!(region.height_px < mode().height_px);
}

#[test]
fn a_wake_with_nothing_to_do_presents_nothing() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let noon = Time64::from_secs(1_700_000_060);
    assert_ne!(login.refresh(0, Some(noon)).present, Present::Nothing);
    assert_eq!(
        login.refresh(0, Some(noon)).present,
        Present::Nothing,
        "the same minute a second time changes nothing"
    );
}

#[test]
fn the_ribbon_is_drawn_behind_the_column() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let plain = login.frame().to_vec();

    assert!(login.raise_ribbon(0));
    assert_eq!(login.repaint(), Present::Whole);
    assert_ne!(login.frame(), plain.as_slice());
    let lit = (0..mode().height_px)
        .flat_map(|y| (0..mode().width_px).map(move |x| (x, y)))
        .filter(|(x, y)| brightness(login.frame(), *x, *y) > 0)
        .count();
    assert!(lit > 0, "the ribbon's light reaches the frame");
}

#[test]
fn the_pointer_is_drawn_over_the_surface_and_only_where_it_sits() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let bare = login.frame().to_vec();

    let image = arrow();
    let sits = cursor_rect(&image, centre().0, centre().1);
    login.set_pointer(image);
    assert_eq!(login.repaint(), Present::Whole);

    let changed = differing(&bare, login.frame());
    assert!(
        !changed.is_empty(),
        "a whole-screen repaint draws the pointer"
    );
    for (x, y) in changed {
        assert!(
            sits.contains(Point::new(x, y)),
            "({x}, {y}) changed outside the cursor at {sits:?}"
        );
    }
}

/// The region `step` presented, or a failure naming what came instead.
fn presented(step: crate::Step) -> Rect {
    let Present::Region(region) = step.present else {
        panic!("a sub-screen change is a region present, got {step:?}");
    };
    rect_of(region).expect("a region on the screen")
}

#[test]
fn a_move_presents_the_old_and_new_cursor_rectangles_and_nothing_larger() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    let image = arrow();
    login.set_pointer(image.clone());
    login.repaint();

    // Onto the backdrop first, so the only thing the next move changes is
    // the pointer itself.
    let corner = (30, 30);
    login.on_pointer(&moved_from(centre(), corner), 0);

    let along = (70, 30);
    let step = login.on_pointer(&moved_from(corner, along), 0);
    let expected = cursor_rect(&image, corner.0, corner.1)
        .union(&cursor_rect(&image, along.0, along.1))
        .intersection(&login.screen());
    assert_eq!(presented(step), expected);

    // And into the corner, where the union runs off the screen: what is
    // presented is the part that is on it.
    let step = login.on_pointer(&moved_from(along, (0, 0)), 0);
    let clipped = cursor_rect(&image, along.0, along.1)
        .union(&cursor_rect(&image, 0, 0))
        .intersection(&login.screen());
    assert_eq!(presented(step), clipped);
    assert_eq!(
        login.screen().union(&clipped),
        login.screen(),
        "the clipped union reaches past the screen"
    );
}

#[test]
fn a_move_that_changes_no_control_state_renders_the_surface_once_in_total() {
    let mut login = ready();
    // Away from the centred panel, so nothing the sweep passes over is a
    // tile whose focus would honestly change.
    let start = (30, 30);
    login.on_pointer(&moved_from(centre(), start), 0);
    stamp(&mut login, start);

    // The stream a hand resting on the mouse produces.
    let mut from = start;
    for step in 1..=50 {
        let to = (start.0 + step, start.1);
        assert_ne!(
            login.on_pointer(&moved_from(from, to), 0).present,
            Present::Nothing
        );
        from = to;
    }

    assert_eq!(
        kept(&login, start),
        Some(MARK),
        "the surface was rendered again for a move that changed nothing"
    );
}

#[test]
fn the_frame_after_a_move_is_what_a_full_repaint_would_have_drawn() {
    let spot = (240, 180);
    let mut moved = ready();
    moved.on_pointer(&moved_from(centre(), spot), 0);

    let mut fresh = ready();
    fresh.on_pointer(&moved_from(centre(), spot), 0);
    assert_eq!(fresh.repaint(), Present::Whole);

    let differences = differing(moved.frame(), fresh.frame());
    assert!(
        differences.is_empty(),
        "{} pixels differ from a full repaint, first at {:?}",
        differences.len(),
        differences.first()
    );
}

/// What a drain does with a burst: apply every record, merge what each one
/// changed, and hand the display that one present.
#[test]
fn a_run_of_moves_merges_into_one_present_covering_every_one_of_them() {
    let mut login = ready();
    let image = arrow();
    let start = (100, 100);
    login.on_pointer(&moved_from(centre(), start), 0);

    let mut merged = Present::Nothing;
    let mut from = start;
    for step in 1..=8 {
        let to = (start.0 + step * 3, start.1);
        let present = login.on_pointer(&moved_from(from, to), 0).present;
        merged = merged.merged(present, login.scanout.mode());
        from = to;
    }

    let expected = cursor_rect(&image, start.0, start.1)
        .union(&cursor_rect(&image, from.0, from.1))
        .intersection(&login.screen());
    let Present::Region(region) = merged else {
        panic!("a run across the backdrop is one sub-screen region, got {merged:?}");
    };
    assert_eq!(
        Rect::new(
            i32::try_from(region.x).expect("on screen"),
            i32::try_from(region.y).expect("on screen"),
            region.width_px,
            region.height_px,
        ),
        expected
    );
}

#[test]
fn a_move_onto_the_field_presents_the_field_and_the_pointer_together() {
    let mut login = ready();
    let image = arrow();
    login.on_input(&key(NamedKey::Enter), 0);

    let away = (5, 5);
    login.on_pointer(&moved_from(centre(), away), 0);
    let field = login
        .surface
        .field_rect(login.screen(), login.scale, &login.theme);
    let onto = (field.origin.x + 2, field.origin.y + 2);

    let step = login.on_pointer(&moved_from(away, onto), 0);
    let expected = cursor_rect(&image, away.0, away.1)
        .union(&cursor_rect(&image, onto.0, onto.1))
        .union(&field)
        .intersection(&login.screen());
    assert_eq!(presented(step), expected);
}

#[test]
fn a_keystroke_and_a_clock_tick_each_rebuild_the_surface() {
    let mut login = ready();
    login.on_input(&key(NamedKey::Enter), 0);
    stamp(&mut login, centre());
    assert_ne!(login.on_input(&typed('x'), 0).present, Present::Nothing);
    assert_ne!(kept(&login, centre()), Some(MARK), "a keystroke");

    let mut login = ready();
    stamp(&mut login, centre());
    let noon = Time64::from_secs(1_700_000_060);
    assert_ne!(login.refresh(0, Some(noon)).present, Present::Nothing);
    assert_ne!(kept(&login, centre()), Some(MARK), "a clock tick");
}

#[test]
fn a_verdict_and_the_countdown_it_starts_each_rebuild_the_surface() {
    let mut login = ready();
    login.on_input(&key(NamedKey::Enter), 0);
    for ch in "wrong".chars() {
        login.on_input(&typed(ch), 0);
    }

    stamp(&mut login, centre());
    let verdict = login.on_input(&key(NamedKey::Enter), 0);
    assert_eq!(
        verdict.answer.map(|answer| answer.verdict),
        Some(Verdict::Refused)
    );
    assert_ne!(verdict.present, Present::Nothing);
    assert_ne!(kept(&login, centre()), Some(MARK), "a verdict");

    stamp(&mut login, centre());
    let counted = login.refresh(10 * 1_000_000_000, None);
    assert_ne!(counted.present, Present::Nothing);
    assert_ne!(
        kept(&login, centre()),
        Some(MARK),
        "a lockout counting down"
    );
}

#[test]
fn a_raised_ribbon_owes_a_fresh_paint() {
    let mut login = ready();
    stamp(&mut login, centre());

    assert!(login.raise_ribbon(0));
    assert!(
        login.paint_owed,
        "the ribbon is behind the surface, so the paint is owed again"
    );
    assert_eq!(login.repaint(), Present::Whole);
    assert_ne!(kept(&login, centre()), Some(MARK));
}

/// Installed pointer artwork is sampled over the kept surface rather than
/// painted into it, so it appears on the next frame without the screen being
/// rendered again — and the pixels behind it are still there to be restored
/// when it moves off them.
#[test]
fn an_installed_pointer_draws_over_the_kept_surface_without_rebuilding_it() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let bare = login.frame().to_vec();
    stamp(&mut login, centre());

    let image = arrow();
    login.set_pointer(image.clone());
    assert_eq!(
        kept(&login, centre()),
        Some(MARK),
        "the pointer is not part of the surface"
    );

    assert_eq!(login.repaint(), Present::Whole);
    let sits = cursor_rect(&image, centre().0, centre().1);
    let changed = differing(&bare, login.frame());
    assert!(!changed.is_empty(), "the new pointer is on the frame");
    for (x, y) in changed {
        assert!(
            sits.contains(Point::new(x, y)) || (x, y) == centre(),
            "({x}, {y}) changed outside the cursor at {sits:?}"
        );
    }
}

#[test]
fn a_moved_pointer_leaves_nothing_painted_behind_it() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let bare = login.frame().to_vec();

    let image = arrow();
    login.set_pointer(image.clone());
    login.repaint();
    let corner = (30, 30);
    login.on_pointer(&moved_from(centre(), corner), 0);

    let sits = cursor_rect(&image, corner.0, corner.1);
    let changed = differing(&bare, login.frame());
    assert!(!changed.is_empty(), "the pointer is drawn where it landed");
    for (x, y) in changed {
        assert!(
            sits.contains(Point::new(x, y)),
            "({x}, {y}) still differs after the pointer left it"
        );
    }
}

#[test]
fn motion_that_moves_nothing_presents_nothing() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.set_pointer(arrow());
    login.repaint();
    login.on_pointer(&moved_from(centre(), (30, 30)), 0);

    let step = login.on_pointer(&PointerInput::MovedBy { dx: 0, dy: 0 }, 0);
    assert_eq!(step.present, Present::Nothing);

    // Nor does motion the screen edge swallows.
    let step = login.on_pointer(&PointerInput::MovedBy { dx: -1000, dy: 0 }, 0);
    assert_ne!(
        step.present,
        Present::Nothing,
        "the pointer reached the edge"
    );
    let step = login.on_pointer(&PointerInput::MovedBy { dx: -1000, dy: 0 }, 0);
    assert_eq!(step.present, Present::Nothing, "and stayed there");
}

#[test]
fn a_pointer_that_would_not_rasterise_leaves_a_working_screen_with_none_drawn() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    let bare = login.frame().to_vec();

    // No `set_pointer`: the screen still tracks and hit-tests, draws no
    // cursor, and asks for no paint it does not owe.
    let step = login.on_pointer(&moved_from(centre(), (30, 30)), 0);
    assert_eq!(step.present, Present::Nothing);
    assert_eq!(login.frame(), bare.as_slice(), "nothing was drawn for it");
    assert_eq!(login.repaint(), Present::Whole);
    assert_eq!(login.frame(), bare.as_slice());

    assert!(
        offer(&mut login, SECRET, 0).verified,
        "and it still logs in"
    );
}

/// A button report expands to a move *and* a transition, and the two are
/// merged into one frame. On the backdrop neither changes a pixel, so the
/// merge must not invent damage of its own.
#[test]
fn a_button_where_nothing_answers_it_presents_nothing() {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.set_pointer(arrow());
    login.repaint();
    login.on_pointer(&moved_from(centre(), (30, 30)), 0);
    let bare = login.frame().to_vec();

    for report in [
        PointerInput::Pressed(PointerButtonCode::Primary),
        PointerInput::Released(PointerButtonCode::Primary),
        PointerInput::Scrolled { dx: 0, dy: 3 },
    ] {
        let step = login.on_pointer(&report, 0);
        assert_eq!(step.present, Present::Nothing, "{report:?} changed nothing");
        assert!(!step.verified);
    }
    assert_eq!(login.frame(), bare.as_slice());
}

/// A touchscreen points and clicks as the mouse does: a finger puts the
/// pointer where it lands, its press waits on a deadline the park honours, and
/// a scroll or a pinch finds nothing on the authentication surface to act on.
#[test]
fn a_touch_points_and_clicks_as_the_mouse_does() {
    use tairix_abi::touch::{Contact, TouchButtons, TouchExtent, TouchFrame, TouchSurface};
    use tairix_input::PointerButton;
    use tairix_touch::{Gesture, SurfacePoint, TouchPress};

    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    login.set_pointer(arrow());
    login.repaint();
    let mut frame = TouchFrame::new(
        0,
        TouchSurface::Screen,
        TouchButtons::NONE,
        TouchExtent::default(),
    );
    frame
        .push(Contact::finger(1, 0, 0))
        .expect("room for a contact");
    let mut gestures = Vec::new();
    login.feed_touch(&frame.stamped(1, 0), &mut gestures);
    assert_eq!(gestures, [Gesture::MovedTo(SurfacePoint { x: 0, y: 0 })]);
    let step = login.on_gesture(gestures[0], 0);
    assert_ne!(
        step.present,
        Present::Nothing,
        "the pointer moved to the corner"
    );

    let hold_ns = 100_000_000;
    assert!(
        login.park_timeout(0, None) <= hold_ns,
        "the press's wait is parked on"
    );
    gestures.clear();
    login.expire_touch(hold_ns, &mut gestures);
    let press = Gesture::Pressed(TouchPress::Fingers(PointerButton::Primary));
    assert_eq!(gestures, [press]);
    let pressed = login.on_gesture(press, hold_ns);
    assert_eq!(
        pressed.present,
        Present::Nothing,
        "the backdrop answers no press"
    );
    assert!(!pressed.verified);
    for nothing in [
        Gesture::Scrolled { dx: 0, dy: 120 },
        Gesture::Pinch(tairix_touch::Pinch {
            phase: tairix_abi::touch::PinchPhase::Begin,
            scale: tairix_abi::touch::PINCH_SCALE_ONE,
            at: None,
        }),
    ] {
        assert_eq!(login.on_gesture(nothing, hold_ns).present, Present::Nothing);
    }
}

#[test]
fn the_account_the_authority_was_asked_about_is_the_one_picked() {
    let mut login = screen(
        vec![
            AccountTile::new("Ann Example", "ann"),
            AccountTile::new("Bo Example", "bo"),
        ],
        Authority::accepting("bo", SECRET),
    );
    login.repaint();
    login.on_input(&key(NamedKey::Tab), 0);
    login.on_input(&key(NamedKey::Enter), 0);
    for ch in SECRET.chars() {
        login.on_input(&typed(ch), 0);
    }
    let step = login.on_input(&key(NamedKey::Enter), 0);
    assert!(step.verified);
}

/// An idle screen with nothing animating waits for its display's sleep and
/// for nothing else.
#[test]
fn an_idle_screen_waits_only_for_its_sleep() {
    let login = screen(
        vec![
            AccountTile::new("Ann Example", "ann"),
            AccountTile::new("Bo Example", "bo"),
        ],
        Authority::accepting("ann", SECRET),
    );
    assert_eq!(login.park_timeout(0, None), ENERGY_SAVING_AFTER_NS);
    // After a first paint, still idle.
    let mut login = login;
    login.repaint();
    assert_eq!(login.park_timeout(0, None), ENERGY_SAVING_AFTER_NS);
}

/// A focus change arms a short timeout; successive refreshes present
/// shrinking-or-equal damage covering the tiles; once settled the timeout
/// returns to idle.
#[test]
fn a_focus_change_arms_motion_and_settles_cleanly() {
    let mut login = screen(
        vec![
            AccountTile::new("Ann Example", "ann"),
            AccountTile::new("Bo Example", "bo"),
        ],
        Authority::accepting("ann", SECRET),
    );
    login.repaint();
    assert_eq!(login.park_timeout(0, None), resting(0));

    let step = login.on_input(&key(NamedKey::Tab), 0);
    assert_ne!(step.present, Present::Nothing, "focus move presents");

    let millis = Theme::dark()
        .motion()
        .duration(tairix_theme::MotionInteraction::SelectionChange);
    let span_ns = u64::from(millis) * 1_000_000;
    assert!(millis > 0);

    let timeout = login.park_timeout(0, None);
    assert!(
        timeout <= span_ns,
        "motion arms a short timeout, got {timeout}"
    );

    let mut now = 0u64;
    let step_ns = (span_ns / 8).max(1);
    let mut saw_present = false;
    loop {
        now = now.saturating_add(step_ns);
        let step = login.refresh(now, None);
        match step.present {
            Present::Nothing => {}
            Present::Region(_) | Present::Whole => saw_present = true,
        }
        if login.park_timeout(now, None) == resting(now) {
            break;
        }
        assert!(now <= span_ns.saturating_mul(2), "fade did not settle");
    }
    assert!(saw_present, "at least one refresh presented fade damage");
    assert_eq!(login.park_timeout(now, None), resting(now));

    // A refresh that finds nothing to do still presents nothing.
    let quiet = login.refresh(now.saturating_add(1), None);
    assert_eq!(quiet.present, Present::Nothing);
}

/// A second of the monotonic clock.
const SEC: u64 = 1_000_000_000;

/// When a screen whose last input came at zero is owed its sleep.
const SLEPT: u64 = ENERGY_SAVING_AFTER_NS;

/// What a display was shown or asked, in order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Seen {
    /// A frame presented whole, and whether every pixel of it was black.
    Present {
        black: bool,
    },
    Power(DisplayPower),
}

/// A display that records what it was shown and asked, and refuses as told.
struct Panel {
    can_switch: bool,
    refuse: Option<DriverError>,
    seen: Vec<Seen>,
}

impl Panel {
    fn switchable() -> Self {
        Self {
            can_switch: true,
            refuse: None,
            seen: Vec::new(),
        }
    }

    fn fixed() -> Self {
        Self {
            can_switch: false,
            ..Self::switchable()
        }
    }

    fn switches(&self) -> Vec<DisplayPower> {
        self.seen
            .iter()
            .filter_map(|seen| match seen {
                Seen::Power(power) => Some(*power),
                Seen::Present { .. } => None,
            })
            .collect()
    }
}

impl Display for Panel {
    fn mode_info(&self) -> Result<DisplayMode, DriverError> {
        Ok(mode())
    }

    fn present(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        let black = frame
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [0, 0, 0]);
        self.seen.push(Seen::Present { black });
        Ok(())
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if !self.can_switch {
            return Err(DriverError::Unsupported);
        }
        if let Some(refusal) = self.refuse {
            return Err(refusal);
        }
        self.seen.push(Seen::Power(power));
        Ok(())
    }
}

/// A screen standing over the ribbon with its chrome told and its first frame
/// composed, and no pointer drawn over either.
fn ribboned() -> LoginScreen<Authority> {
    let mut login = screen(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
    );
    assert!(login.raise_ribbon(0), "the heap gives a ribbon");
    login.refresh(0, None);
    login.repaint();
    login
}

#[test]
fn a_screen_left_alone_puts_its_display_to_sleep_after_thirty_minutes() {
    let mut login = ready();
    let mut panel = Panel::switchable();
    assert_eq!(login.sleep_if_idle(&mut panel, SLEPT - 1), None, "not yet");
    assert!(!login.is_asleep());
    assert!(panel.seen.is_empty());

    assert_eq!(
        login.sleep_if_idle(&mut panel, SLEPT),
        Some(SwitchedOff::Off)
    );
    assert!(login.is_asleep());
    assert_eq!(
        panel.seen,
        [
            Seen::Present { black: true },
            Seen::Power(DisplayPower::Off)
        ],
        "the screen goes black, then the display off, so it wakes on black"
    );
    assert_eq!(
        login.park_timeout(SLEPT, None),
        WAITSET_TIMEOUT_NONE,
        "and arms nothing"
    );
    let noon = Time64::from_secs(1_700_000_060);
    assert_eq!(
        login.refresh(SLEPT + 60 * SEC, Some(noon)).present,
        Present::Nothing,
        "a sleeping screen presents nothing, not even the minute turning"
    );
    assert_eq!(
        login.sleep_if_idle(&mut panel, u64::MAX),
        None,
        "asleep already"
    );
}

/// Any input puts the sleep off by the whole wait again.
#[test]
fn input_puts_the_sleep_off_by_the_whole_wait() {
    let mut login = ready();
    let mut panel = Panel::switchable();
    login.on_pointer(&PointerInput::Scrolled { dx: 0, dy: 1 }, 10 * SEC);
    assert_eq!(login.sleep_if_idle(&mut panel, SLEPT), None);
    assert_eq!(login.park_timeout(SLEPT, None), 10 * SEC);
    assert_eq!(
        login.sleep_if_idle(&mut panel, SLEPT + 10 * SEC),
        Some(SwitchedOff::Off)
    );
}

/// The wait starts from the moment the screen comes up, not from the clock's
/// zero.
#[test]
fn the_wait_for_sleep_starts_when_the_screen_opens() {
    let mut login = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );
    let opened = 100 * SEC;
    assert!(covers(login.open(opened, None), login.screen()));
    assert_eq!(login.park_timeout(opened, None), ENERGY_SAVING_AFTER_NS);
    assert_eq!(login.sleep_if_idle(&mut Panel::switchable(), SLEPT), None);
}

/// The gesture that wakes the screen reaches nothing behind it: the display is
/// switched back on, and the screen arrives out of black as it first did.
#[test]
fn input_while_asleep_reaches_nothing_and_wakes_the_display() {
    let mut login = ready();
    let mut panel = Panel::switchable();
    let _ = login.sleep_if_idle(&mut panel, SLEPT);
    let woke = SLEPT + 10 * SEC;

    assert_eq!(login.on_input(&key(NamedKey::Enter), woke), Step::quiet());
    let opened = login
        .wake(&mut panel, woke, None)
        .expect("the display wakes");
    assert!(covers(opened, login.screen()));
    assert!(!login.is_asleep());
    assert_eq!(panel.switches(), [DisplayPower::Off, DisplayPower::On]);
    assert_eq!(
        login.surface.selected_account(),
        None,
        "the waking key picked no account"
    );
    let (x, y) = (mode().width_px / 4, mode().height_px / 4);
    assert_eq!(
        brightness(login.frame(), x, y),
        0,
        "it arrives out of black"
    );
    assert_eq!(
        login.wake(&mut panel, woke, None),
        Ok(Present::Nothing),
        "a wake with no input owed presents nothing"
    );
    assert!(login.park_timeout(woke, None) < ENERGY_SAVING_AFTER_NS);
}

/// A secret half typed when the screen went to sleep does not outlive it:
/// the screen wakes on the chooser, and what was typed is gone.
#[test]
fn a_secret_half_typed_when_the_screen_sleeps_is_erased() {
    let mut login = ready();
    login.on_input(&key(NamedKey::Enter), 0);
    for ch in "open-".chars() {
        login.on_input(&typed(ch), 0);
    }
    let mut panel = Panel::switchable();
    let _ = login.sleep_if_idle(&mut panel, SLEPT);
    let woke = SLEPT + SEC;
    login.on_input(&key(NamedKey::Enter), woke);
    let _ = login.wake(&mut panel, woke, None);

    assert_eq!(
        login.surface.selected_account(),
        None,
        "back on the chooser"
    );
    for ch in "sesame".chars() {
        login.on_input(&typed(ch), woke);
    }
    let step = login.on_input(&key(NamedKey::Enter), woke);
    assert!(
        !step.verified && step.answer.is_none(),
        "the rest of the secret did not complete what was typed before"
    );
    assert_eq!(login.surface.selected_account(), Some("ann"));
}

/// A display with no power control is kept black instead, and wakes without
/// anything to switch back on.
#[test]
fn a_display_that_cannot_switch_off_sleeps_black() {
    let mut login = ribboned();
    let mut panel = Panel::fixed();
    assert_eq!(
        login.sleep_if_idle(&mut panel, SLEPT),
        Some(SwitchedOff::Blanked)
    );
    assert!(login.is_asleep());
    assert_eq!(panel.seen, [Seen::Present { black: true }]);
    assert!(
        login
            .frame()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [0, 0, 0]),
        "the whole frame is black"
    );
    assert_eq!(login.park_timeout(SLEPT, None), WAITSET_TIMEOUT_NONE);
    assert_eq!(
        login.refresh(SLEPT + SceneClock::FRAME_NS, None).present,
        Present::Nothing,
        "nor does the ribbon move behind the black"
    );

    let woke = SLEPT + SEC;
    login.on_pointer(&PointerInput::MovedBy { dx: 3, dy: 0 }, woke);
    let opened = login
        .wake(&mut panel, woke, None)
        .expect("a black screen wakes");
    assert!(covers(opened, login.screen()));
    assert!(panel.switches().is_empty(), "nothing to switch back on");
}

/// A refusal to switch off is named, and the screen is kept black in its
/// place.
#[test]
fn a_refused_switch_is_named_and_the_screen_kept_black() {
    let mut login = ready();
    let mut panel = Panel {
        refuse: Some(DriverError::DeviceFault),
        ..Panel::switchable()
    };
    assert_eq!(
        login.sleep_if_idle(&mut panel, SLEPT),
        Some(SwitchedOff::Refused(DriverError::DeviceFault))
    );
    assert!(login.is_asleep());
    assert_eq!(panel.seen, [Seen::Present { black: true }]);
}

/// A display that will not light again keeps the screen asleep, and it is the
/// next input — not the next wake of the loop — that asks again.
#[test]
fn a_display_that_will_not_wake_stays_asleep_and_the_next_input_asks_again() {
    let mut login = ready();
    let mut panel = Panel::switchable();
    let _ = login.sleep_if_idle(&mut panel, SLEPT);
    panel.refuse = Some(DriverError::DeviceFault);
    let woke = SLEPT + SEC;
    login.on_input(&key(NamedKey::Enter), woke);
    assert_eq!(
        login.wake(&mut panel, woke, None),
        Err(DriverError::DeviceFault)
    );
    assert!(login.is_asleep());
    assert_eq!(login.wake(&mut panel, woke, None), Ok(Present::Nothing));
    assert_eq!(login.park_timeout(woke, None), WAITSET_TIMEOUT_NONE);

    panel.refuse = None;
    login.on_input(&key(NamedKey::Enter), woke + SEC);
    assert!(login
        .wake(&mut panel, woke + SEC, None)
        .is_ok_and(|opened| opened != Present::Nothing));
    assert!(!login.is_asleep());
}

/// The pointer comes back where the hand put it while the screen slept.
#[test]
fn the_pointer_follows_the_hand_while_the_screen_sleeps() {
    let mut login = ready();
    let mut panel = Panel::switchable();
    let _ = login.sleep_if_idle(&mut panel, SLEPT);
    let step = login.on_pointer(&moved_from(centre(), (30, 30)), SLEPT + SEC);
    assert_eq!(step.present, Present::Nothing);
    assert_eq!(login.cursor.at(), Point::new(30, 30));
}

/// A frame of the ribbon re-composes what the ribbon moved and paints nothing
/// of the column: the stamp in the kept surface survives it.
#[test]
fn a_ribbon_frame_recomposes_what_it_moved_and_paints_nothing() {
    let mut login = ribboned();
    let before = login.frame().to_vec();
    stamp(&mut login, (0, 0));
    let step = login.refresh(SceneClock::FRAME_NS, None);
    assert_ne!(step.present, Present::Nothing, "the ribbon moved");
    assert_ne!(step.present, Present::Whole, "and only where it moved");
    assert_eq!(
        kept(&login, (0, 0)),
        Some(MARK),
        "the column was not painted"
    );
    for (x, y) in differing(&before, login.frame()) {
        assert!(
            covers(step.present, Rect::new(x, y, 1, 1)),
            "({x}, {y}) changed outside the present"
        );
    }
}

/// However many frames of the ribbon have gone out, the frame on screen is the
/// one a whole composition of the screen as it stands would draw.
#[test]
fn a_ribbon_frame_leaves_no_stale_pixel() {
    let mut login = ribboned();
    for frame in 1..=6 {
        login.refresh(frame * SceneClock::FRAME_NS, None);
    }
    let composed = login.frame().to_vec();
    login.repaint();
    assert!(differing(&composed, login.frame()).is_empty());
}

/// Nothing of the ribbon's light reaches the column: wherever the surface
/// leaves the column bare, the frame shows the ribbon's dark sky.
#[test]
fn the_ribbon_keeps_the_column_dark() {
    let mut login = ribboned();
    for frame in 1..=6 {
        login.refresh(frame * 30 * SceneClock::FRAME_NS, None);
    }
    let column = login.surface.column_rect(login.screen(), Scale::ONE);
    let painted = login.painted.as_ref().expect("a surface is kept");
    let bare = (0..mode().height_px)
        .flat_map(|y| (0..mode().width_px).map(move |x| (x, y)))
        .filter(|(x, y)| {
            let at = Point::new(
                i32::try_from(*x).expect("a small screen"),
                i32::try_from(*y).expect("a small screen"),
            );
            column.contains(at) && painted.get(*x, *y).is_some_and(|pixel| pixel.a == 0)
        });
    let mut checked = 0;
    for (x, y) in bare {
        checked += 1;
        assert_eq!(brightness(login.frame(), x, y), 0, "light at ({x}, {y})");
    }
    assert!(checked > 0, "the column has bare ground to check");
}

#[test]
fn the_ribbon_asks_for_its_frames_and_holds_still_under_reduced_motion() {
    let login = ribboned();
    assert_eq!(login.park_timeout(0, None), SceneClock::FRAME_NS);

    let mut calm = screen_in(
        vec![AccountTile::new("Ann Example", "ann")],
        Authority::accepting("ann", SECRET),
        still(),
    );
    assert!(calm.raise_ribbon(0));
    calm.refresh(0, None);
    calm.repaint();
    assert_eq!(
        calm.park_timeout(0, None),
        resting(0),
        "a still ribbon arms no frame"
    );
    assert_eq!(
        calm.refresh(10 * SceneClock::FRAME_NS, None).present,
        Present::Nothing
    );
}

/// The authority meters each login name on its own, so a lockout one account
/// earned neither shows on another's prompt nor stops that one's secret from
/// reaching the authority.
#[test]
fn a_lockout_on_one_account_does_not_hold_back_another() {
    let mut login = screen(
        vec![
            AccountTile::new("Ann Example", "ann"),
            AccountTile::new("Bo Example", "bo"),
        ],
        Authority::accepting("bo", SECRET),
    );
    login.repaint();
    let refused = offer(&mut login, "wrong", 0);
    assert_eq!(
        refused.answer.map(|answer| answer.retry_after),
        Some(Duration64::from_secs(20))
    );

    login.on_input(&key(NamedKey::Escape), SEC);
    login.on_input(&key(NamedKey::Tab), SEC);
    login.on_input(&key(NamedKey::Enter), SEC);
    assert_eq!(login.surface.selected_account(), Some("bo"));
    login.refresh(2 * SEC, None);
    assert!(
        !login.notice().contains("try again"),
        "ann's lockout is on bo's prompt: {:?}",
        login.notice()
    );
    for ch in SECRET.chars() {
        login.on_input(&typed(ch), 2 * SEC);
    }
    assert!(login.on_input(&key(NamedKey::Enter), 2 * SEC).verified);
}

/// Going back to the account that was locked out finds its lockout still
/// counting.
#[test]
fn a_lockout_is_still_counting_when_its_account_is_picked_again() {
    let mut login = screen(
        vec![
            AccountTile::new("Ann Example", "ann"),
            AccountTile::new("Bo Example", "bo"),
        ],
        Authority::accepting("bo", SECRET),
    );
    login.repaint();
    offer(&mut login, "wrong", 0);
    login.on_input(&key(NamedKey::Escape), SEC);
    login.refresh(2 * SEC, None);
    assert!(!login.notice().contains("try again"), "not on the chooser");
    login.on_input(&key(NamedKey::Enter), 3 * SEC);
    assert_eq!(login.surface.selected_account(), Some("ann"));
    login.refresh(4 * SEC, None);
    assert!(
        login.notice().contains("16"),
        "ann's lockout counts on: {:?}",
        login.notice()
    );
}
