//! Backing the desktop's [`InputSource`] with a live device channel.
//!
//! [`DesktopShell`](crate::DesktopShell) drives the desktop by
//! [`pump`](crate::DesktopShell::pump)ing an injected [`InputSource`] — the
//! one seam through which pointer events reach the desktop. This module is the
//! *live* backing for that seam: [`DeviceInputSource`] reads framed
//! [`PointerInput`] records from a kernel input channel and decodes each into
//! the desktop's `lib/input` [`InputEvent`] vocabulary the window manager and
//! taskbar route.
//!
//! The seat channel is deliberately **screen-independent**: a driver injects
//! relative displacements ([`PointerInput::MovedBy`]) and resolved button
//! edges, because only the seat owner — this desktop session, which owns the
//! compositor — knows the screen's pixel extent. [`DeviceInputSource`] is
//! where that policy lives: it holds the absolute pointer position, starts it
//! at the screen's centre, accumulates each displacement with saturating
//! arithmetic, and clamps the result to the screen rectangle, so the pointer
//! can never leave the screen no matter what a (compromised) injector sends.
//! Construction refuses an empty screen outright (fail closed). It is also
//! where the user's pointer policy is applied — the button order and the
//! speed — so no surface above it ever sees the unmapped form, and where a
//! wheel's turning is accelerated.
//!
//! Touch frames arrive through a second seam, [`TouchInputChannel`], and pass
//! through the seat's gesture recogniser (`lib/touch`): a touchpad moves the
//! same pointer a mouse does, a touchscreen puts it where it is touched, and
//! the clicks, scrolls and pinches they make join the stream as the mouse's
//! do. Every source's press and release reaches the desktop as it happens:
//! a source that never released — its driver died mid-press — is undone by
//! the next release of that button from anywhere, rather than holding it down
//! for every other device.
//!
//! The raw bytes arrive through an injected [`PointerInputChannel`] seam — a
//! capability-checked kernel input channel on a running system, an in-memory
//! queue in tests — so this `userland/gui` crate holds no
//! input capability of its own and the decode runs above the device, not
//! inside it. Every record is validated by
//! [`PointerInput::from_bytes`] before it becomes an [`InputEvent`]; a
//! malformed record surfaces its [`Errno`] and the shell's
//! [`pump`](crate::DesktopShell::pump) stops without misinterpreting the bytes.
//!
//! [`InputSource`]: crate::InputSource
//! [`InputEvent`]: tairix_wm::InputEvent

use alloc::collections::VecDeque;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::input::{PointerButtonCode, PointerInput};
use tairix_abi::touch::TouchFrame;
use tairix_abi::Errno;
use tairix_touch::{Gesture, Recogniser, SurfacePoint, TouchPress, TouchSettings};
use tairix_wallpaper::{PointerSpeed, PrimaryButton};
use tairix_wm::{InputEvent, Point, PointerButton, Rect, Scale};

use crate::shell::InputSource;

/// A source of framed [`PointerInput`] record bytes from the kernel.
///
/// On a running system this is a capability-checked kernel input channel that
/// hands the desktop one [`PointerInput::WIRE_LEN`]-byte record at a time;
/// tests back it with an in-memory queue. It deals only in
/// raw bytes: decoding and validating them is [`DeviceInputSource`]'s job, so
/// the channel itself need not understand the wire format.
pub trait PointerInputChannel {
    /// Take the next pending record's bytes, or `None` when the channel is
    /// momentarily drained.
    ///
    /// # Errors
    ///
    /// Returns the kernel boundary's [`Errno`] when the channel itself faults
    /// (for example it was closed). The bytes are not interpreted here; a
    /// short or corrupt record is the decoder's concern, not the channel's.
    fn next_record(&mut self) -> Result<Option<[u8; PointerInput::WIRE_LEN]>, Errno>;
}

/// A source of framed [`TouchFrame`] bytes from the kernel: the seat's touch
/// channel on a running system, an in-memory queue in tests.
pub trait TouchInputChannel {
    /// Take the next pending frame's bytes, or `None` when the channel is
    /// momentarily drained.
    ///
    /// # Errors
    ///
    /// The kernel boundary's [`Errno`] when the channel itself faults.
    fn next_frame(&mut self) -> Result<Option<[u8; TouchFrame::WIRE_LEN]>, Errno>;
}

/// An [`InputSource`] that decodes [`PointerInput`] records from a
/// [`PointerInputChannel`] and touch frames from a [`TouchInputChannel`], and
/// resolves both against the screen.
///
/// Wrap a channel with [`new`](Self::new), handing it the compositor's
/// screen rectangle, then give the source to
/// [`DesktopShell::pump`](crate::DesktopShell::pump): each
/// [`poll`](InputSource::poll) reads one record from the channel, decodes
/// it, and — for motion — advances the held pointer position, clamped to
/// the screen.
#[derive(Clone, Debug)]
pub struct DeviceInputSource<C, T> {
    channel: C,
    touch: T,
    /// The screen rectangle every accumulated position is clamped into.
    screen: Rect,
    /// The current absolute pointer position; motion records advance it.
    pointer: Point,
    /// Which physical button is primary.
    primary: PrimaryButton,
    /// A button order chosen while a button was held, applied once none is:
    /// a press and its release must map through the same order, or the
    /// release would name a button that was never pressed.
    pending_primary: Option<PrimaryButton>,
    /// How many sources hold each physical button, by
    /// [`PointerButtonCode`]: a new button order waits until none does.
    held: [u8; 3],
    /// How many sources hold each desktop button, by [`PointerButton`]: the
    /// pointer is quiet only once none does.
    pressed: [u8; 3],
    /// When a button last went down or came up, or the wheel last turned, as
    /// the wake reading it saw.
    gesture_ns: Option<u64>,
    /// How far the pointer moves for a reported displacement.
    speed: PointerSpeed,
    /// The part of a scaled displacement too small to move a whole count
    /// yet, per axis, in hundredths of a count, so slow motion is not lost.
    carry: (i64, i64),
    /// How fast the wheel has been turning, per axis.
    wheel: (WheelAxis, WheelAxis),
    /// What the touch frames mean.
    recogniser: Recogniser,
    /// What the frames read so far meant and the desktop has not yet been
    /// handed.
    gestures: VecDeque<Gesture>,
}

impl<C, T> DeviceInputSource<C, T> {
    /// Build a device input source over the pointer `channel` and the `touch`
    /// channel, resolving motion against `screen` (the compositor's pixel
    /// rectangle). The pointer starts at the screen's centre.
    ///
    /// # Errors
    ///
    /// Returns [`Errno::OutOfRange`] when `screen` is empty: a screenless
    /// source could never establish a valid pointer position, so it is
    /// refused at construction rather than misbehaving later (fail closed).
    pub fn new(channel: C, touch: T, screen: Rect) -> Result<Self, Errno> {
        if screen.is_empty() {
            return Err(Errno::OutOfRange);
        }
        let centre = Point::new(
            screen.left().saturating_add_unsigned(screen.width / 2),
            screen.top().saturating_add_unsigned(screen.height / 2),
        );
        Ok(Self {
            channel,
            touch,
            screen,
            pointer: centre,
            primary: PrimaryButton::Left,
            pending_primary: None,
            held: [0; 3],
            pressed: [0; 3],
            gesture_ns: None,
            speed: PointerSpeed::NORMAL,
            carry: (0, 0),
            wheel: (WheelAxis::new(), WheelAxis::new()),
            recogniser: Recogniser::new(TouchSettings::DEFAULT),
            gestures: VecDeque::new(),
        })
    }

    /// Apply the user's pointer policy: which button is primary, and how far
    /// the pointer moves for a reported displacement.
    ///
    /// A new button order waits until no button is held.
    pub fn set_policy(&mut self, primary: PrimaryButton, speed: PointerSpeed) {
        if speed != self.speed {
            self.speed = speed;
            self.carry = (0, 0);
        }
        if self.held == [0; 3] {
            self.primary = primary;
            self.pending_primary = None;
        } else {
            self.pending_primary = Some(primary);
        }
    }

    /// Apply the user's touch settings from the next touch on.
    pub fn set_touch(&mut self, settings: TouchSettings) {
        self.recogniser.set_settings(settings);
    }

    /// The density the screen is drawn at: a touchscreen that states no size
    /// of its own covers the screen, whose size this gives.
    pub fn set_density(&mut self, scale: Scale) {
        self.recogniser
            .set_screen(self.screen.width, self.screen.height, scale.dpi());
    }

    /// `park_ns` shortened to the instant a touch acts without a frame — a
    /// held tap's release, a touchscreen's waiting press — or left as it is.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        crate::switchuser::park_within(
            park_ns,
            self.recogniser
                .deadline_ns()
                .map(|due| due.saturating_sub(now_ns)),
        )
    }

    /// Whether a touch has an instant at or before `now_ns` to act at: the
    /// seat input a wait that timed out for it is served as.
    #[must_use]
    pub fn touch_due(&self, now_ns: u64) -> bool {
        self.recogniser
            .deadline_ns()
            .is_some_and(|due| due <= now_ns)
    }

    /// The underlying pointer channel.
    pub const fn channel(&self) -> &C {
        &self.channel
    }

    /// The underlying pointer channel, mutably.
    pub fn channel_mut(&mut self) -> &mut C {
        &mut self.channel
    }

    /// Consume the source, returning the pointer and touch channels it
    /// wrapped.
    pub fn into_channels(self) -> (C, T) {
        (self.channel, self.touch)
    }

    /// The current absolute pointer position.
    #[must_use]
    pub const fn pointer(&self) -> Point {
        self.pointer
    }

    /// Whether no button is held, and none has gone down or come up and the
    /// wheel has not turned, in a wake at or after `since_ns`: whether the
    /// pointer made no gesture a modifier could have qualified.
    ///
    /// Every record of one wake is read at one instant, and the pointer's
    /// before the keyboard's, so a gesture made in the very wake a key was
    /// counts as made with it: what this may wrongly answer is `false`, never
    /// `true`.
    #[must_use]
    pub fn quiet_since(&self, since_ns: u64) -> bool {
        self.pressed == [0; 3] && self.gesture_ns.is_none_or(|made| made < since_ns)
    }

    /// Advance the pointer by one displacement, saturating and clamping so
    /// the result always lies on the screen — a hostile or faulty injector
    /// can pin the pointer to an edge, never move it off-screen or wrap it.
    fn displace(&mut self, dx: i32, dy: i32) -> Point {
        let max_x = self.screen.right() - 1;
        let max_y = self.screen.bottom() - 1;
        self.pointer = Point::new(
            self.pointer
                .x
                .saturating_add(dx)
                .clamp(self.screen.left(), max_x),
            self.pointer
                .y
                .saturating_add(dy)
                .clamp(self.screen.top(), max_y),
        );
        self.pointer
    }

    /// Put the pointer at a touchscreen's `place`, the screen spanning the
    /// surface edge to edge.
    fn place(&mut self, place: SurfacePoint) -> Point {
        self.pointer = self.on_screen(place);
        self.pointer
    }

    fn on_screen(&self, place: SurfacePoint) -> Point {
        let (x, y) = place.on_screen(self.screen.width, self.screen.height);
        Point::new(
            self.screen.left().saturating_add_unsigned(x),
            self.screen.top().saturating_add_unsigned(y),
        )
    }

    /// A physical button went down: held for the button order, pressed to the
    /// desktop through it.
    fn press_physical(&mut self, code: PointerButtonCode, now_ns: u64) -> InputEvent {
        let held = &mut self.held[code_index(code)];
        *held = held.saturating_add(1);
        self.press(pointer_button(code, self.primary), now_ns)
    }

    /// A physical button came up; once none is held a button order chosen
    /// meanwhile takes effect.
    fn release_physical(&mut self, code: PointerButtonCode, now_ns: u64) -> InputEvent {
        let mapped = pointer_button(code, self.primary);
        let held = &mut self.held[code_index(code)];
        *held = held.saturating_sub(1);
        if self.held == [0; 3] {
            if let Some(primary) = self.pending_primary.take() {
                self.primary = primary;
            }
        }
        self.release(mapped, now_ns)
    }

    fn press(&mut self, button: PointerButton, now_ns: u64) -> InputEvent {
        self.gesture_ns = Some(now_ns);
        let pressed = &mut self.pressed[button_index(button)];
        *pressed = pressed.saturating_add(1);
        InputEvent::PointerPressed { button }
    }

    /// A release reaches the desktop however many sources still hold the
    /// button, so one whose source can no longer release it is still undone.
    fn release(&mut self, button: PointerButton, now_ns: u64) -> InputEvent {
        self.gesture_ns = Some(now_ns);
        let pressed = &mut self.pressed[button_index(button)];
        *pressed = pressed.saturating_sub(1);
        InputEvent::PointerReleased { button }
    }

    fn pointer_event(&mut self, record: PointerInput, now_ns: u64) -> InputEvent {
        match record {
            PointerInput::MovedBy { dx, dy } => {
                let percent = self.speed.percent();
                let dx = scaled(dx, &mut self.carry.0, percent);
                let dy = scaled(dy, &mut self.carry.1, percent);
                InputEvent::PointerMoved {
                    to: self.displace(dx, dy),
                }
            }
            PointerInput::Pressed(code) => self.press_physical(code, now_ns),
            PointerInput::Released(code) => self.release_physical(code, now_ns),
            // A scroll is a delta at the current pointer position, not a
            // move, and a gesture a held modifier qualifies, as a click is.
            PointerInput::Scrolled { dx, dy } => {
                self.gesture_ns = Some(now_ns);
                InputEvent::PointerScrolled {
                    dx: self.wheel.0.accelerated(dx, now_ns),
                    dy: self.wheel.1.accelerated(dy, now_ns),
                }
            }
        }
    }

    /// What a touch `gesture` is to the desktop. A touchpad's motion is
    /// already at the user's speed and its scroll already in proportion to
    /// the fingers' travel, so neither is scaled or accelerated again.
    fn gesture_event(&mut self, gesture: Gesture, now_ns: u64) -> InputEvent {
        match gesture {
            Gesture::MovedBy { dx, dy } => InputEvent::PointerMoved {
                to: self.displace(dx, dy),
            },
            Gesture::MovedTo(place) => InputEvent::PointerMoved {
                to: self.place(place),
            },
            Gesture::Pressed(TouchPress::Device(code)) => self.press_physical(code, now_ns),
            Gesture::Released(TouchPress::Device(code)) => self.release_physical(code, now_ns),
            Gesture::Pressed(TouchPress::Fingers(button)) => self.press(button, now_ns),
            Gesture::Released(TouchPress::Fingers(button)) => self.release(button, now_ns),
            Gesture::Scrolled { dx, dy } => {
                self.gesture_ns = Some(now_ns);
                InputEvent::PointerScrolled { dx, dy }
            }
            Gesture::Pinch(pinch) => {
                self.gesture_ns = Some(now_ns);
                InputEvent::Pinch {
                    phase: pinch.phase,
                    scale: pinch.scale,
                    at: pinch.at.map_or(self.pointer, |at| self.on_screen(at)),
                }
            }
        }
    }
}

const fn code_index(code: PointerButtonCode) -> usize {
    match code {
        PointerButtonCode::Primary => 0,
        PointerButtonCode::Secondary => 1,
        PointerButtonCode::Middle => 2,
    }
}

const fn button_index(button: PointerButton) -> usize {
    match button {
        PointerButton::Primary => 0,
        PointerButton::Secondary => 1,
        PointerButton::Middle => 2,
    }
}

/// Map a physical [`PointerButtonCode`] to the desktop's [`PointerButton`]
/// under the button order `primary`.
///
/// The two enumerations are deliberately separate — the first is the frozen
/// ABI wire code, the second the `lib/input` routing vocabulary — and this is
/// the single place the desktop crosses between them.
const fn pointer_button(code: PointerButtonCode, primary: PrimaryButton) -> PointerButton {
    match (code, primary) {
        (PointerButtonCode::Primary, PrimaryButton::Left)
        | (PointerButtonCode::Secondary, PrimaryButton::Right) => PointerButton::Primary,
        (PointerButtonCode::Secondary, PrimaryButton::Left)
        | (PointerButtonCode::Primary, PrimaryButton::Right) => PointerButton::Secondary,
        (PointerButtonCode::Middle, _) => PointerButton::Middle,
    }
}

/// `delta` counts scaled to `percent`, carrying the remainder in `carry`.
///
/// Truncating toward zero keeps the two directions symmetric, and the carry
/// stays below one count, so however slowly the mouse moves the pointer
/// eventually follows.
fn scaled(delta: i32, carry: &mut i64, percent: u16) -> i32 {
    let total = carry.saturating_add(i64::from(delta) * i64::from(percent));
    let moved = total / 100;
    *carry = total - moved * 100;
    i32::try_from(moved).unwrap_or(if moved < 0 { i32::MIN } else { i32::MAX })
}

/// How far back a wheel's turning is measured.
const WHEEL_WINDOW_NS: u64 = 200_000_000;

/// The detent rate, per second, a deliberate line-by-line turn stays within.
/// A faster turn is multiplied in proportion to how much faster it is.
const WHEEL_STEADY_RATE: u64 = 8;

/// The most a fast spin multiplies a detent by, as a percentage.
const WHEEL_MAX_GAIN_PERCENT: u64 = 600;

/// Reports remembered per axis: more than a hand turns a wheel in the window.
const WHEEL_REPORTS: usize = 16;

/// One axis of a wheel's recent turning, in one direction.
///
/// The rate is measured in scroll units, so a fine wheel's small steps
/// accelerate exactly as a detent wheel's detents would, and *between
/// drains*: reports read in one drain merge into one sample, so turns a busy
/// session reads together are never mistaken for a fast spin — only turning
/// that is seen to keep up across separate drains accelerates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WheelAxis {
    /// When each remembered report was drained, and its units, oldest first.
    recent: [(u64, u32); WHEEL_REPORTS],
    len: usize,
    /// Whether the remembered turning was toward the start.
    backwards: bool,
    /// Scroll units short of a whole one, in hundredths.
    carry: i64,
}

impl WheelAxis {
    const fn new() -> Self {
        Self {
            recent: [(0, 0); WHEEL_REPORTS],
            len: 0,
            backwards: false,
            carry: 0,
        }
    }

    /// The accelerated scroll units `units` drained at `now_ns` are worth.
    fn accelerated(&mut self, units: i32, now_ns: u64) -> i32 {
        if units == 0 {
            return 0;
        }
        let backwards = units < 0;
        if backwards != self.backwards {
            *self = Self::new();
            self.backwards = backwards;
        }
        self.remember(units.unsigned_abs(), now_ns);
        let gain = self.gain_percent();
        scaled(units, &mut self.carry, gain)
    }

    /// Remember `units` drained at `now_ns`, forgetting what the window has
    /// left behind.
    fn remember(&mut self, units: u32, now_ns: u64) {
        let cutoff = now_ns.saturating_sub(WHEEL_WINDOW_NS);
        let stale = self.recent[..self.len]
            .iter()
            .take_while(|(at, _)| *at < cutoff)
            .count();
        self.recent.copy_within(stale..self.len, 0);
        self.len -= stale;
        if let Some((at, held)) = self.recent[..self.len].last_mut() {
            if *at == now_ns {
                *held = held.saturating_add(units);
                return;
            }
        }
        if self.len == WHEEL_REPORTS {
            self.recent.copy_within(1.., 0);
            self.len -= 1;
        }
        self.recent[self.len] = (now_ns, units);
        self.len += 1;
    }

    /// What the remembered turning multiplies a turn by, as a percentage:
    /// nothing until the rate across separate drains passes a deliberate
    /// turn's, then in proportion to it, up to the ceiling.
    fn gain_percent(&self) -> u16 {
        let held = &self.recent[..self.len];
        let (Some(&(first, first_units)), Some(&(last, _))) = (held.first(), held.last()) else {
            return 100;
        };
        let span = last.saturating_sub(first);
        if span == 0 {
            return 100;
        }
        let since_first = held
            .iter()
            .map(|&(_, units)| u128::from(units))
            .sum::<u128>()
            .saturating_sub(u128::from(first_units));
        let steady =
            u128::from(WHEEL_STEADY_RATE) * u128::from(SCROLL_UNITS_PER_DETENT.unsigned_abs());
        let gain = since_first * 1_000_000_000 * 100 / (u128::from(span) * steady);
        u16::try_from(gain.clamp(100, u128::from(WHEEL_MAX_GAIN_PERCENT))).unwrap_or(100)
    }
}

impl<C: PointerInputChannel, T: TouchInputChannel> InputSource for DeviceInputSource<C, T> {
    fn poll(&mut self, now_ns: u64) -> Result<Option<InputEvent>, Errno> {
        loop {
            if let Some(gesture) = self.gestures.pop_front() {
                return Ok(Some(self.gesture_event(gesture, now_ns)));
            }
            if let Some(bytes) = self.channel.next_record()? {
                let record = PointerInput::from_bytes(&bytes)?;
                return Ok(Some(self.pointer_event(record, now_ns)));
            }
            if let Some(bytes) = self.touch.next_frame()? {
                let frame = TouchFrame::from_bytes(&bytes)?;
                let gestures = &mut self.gestures;
                self.recogniser
                    .feed(&frame, &mut |gesture| gestures.push_back(gesture));
            } else {
                // Every queued frame has been read, so a deadline acts only
                // on touches that truly arrived before it.
                let gestures = &mut self.gestures;
                self.recogniser
                    .expire(now_ns, &mut |gesture| gestures.push_back(gesture));
                if self.gestures.is_empty() {
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceInputSource, PointerInputChannel, TouchInputChannel, WheelAxis, WHEEL_WINDOW_NS,
    };
    use crate::InputSource;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    use tairix_abi::input::{PointerButtonCode, PointerInput};
    use tairix_abi::time::NANOS_PER_MILLI as MS;
    use tairix_abi::touch::{Contact, TouchButtons, TouchExtent, TouchFrame, TouchSurface};
    use tairix_abi::Errno;
    use tairix_wallpaper::{PointerSpeed, PrimaryButton};
    use tairix_wm::{InputEvent, PinchPhase, Point, PointerButton, Rect, Scale};

    /// The screen the tests resolve motion against: 640×480 at the origin,
    /// so the pointer starts at its centre (320, 240).
    const SCREEN: Rect = Rect::new(0, 0, 640, 480);

    /// An in-memory channel that yields queued records, optionally faulting.
    struct QueueChannel {
        records: VecDeque<[u8; PointerInput::WIRE_LEN]>,
        fault: Option<Errno>,
    }

    impl QueueChannel {
        fn new(events: &[PointerInput]) -> Self {
            Self {
                records: events.iter().map(PointerInput::to_le_bytes).collect(),
                fault: None,
            }
        }

        fn push_raw(&mut self, bytes: [u8; PointerInput::WIRE_LEN]) {
            self.records.push_back(bytes);
        }

        fn fault_with(&mut self, errno: Errno) {
            self.fault = Some(errno);
        }
    }

    impl PointerInputChannel for QueueChannel {
        fn next_record(&mut self) -> Result<Option<[u8; PointerInput::WIRE_LEN]>, Errno> {
            if let Some(errno) = self.fault.take() {
                return Err(errno);
            }
            Ok(self.records.pop_front())
        }
    }

    /// An in-memory touch channel that yields queued frames.
    #[derive(Default)]
    struct TouchQueue {
        frames: VecDeque<[u8; TouchFrame::WIRE_LEN]>,
    }

    impl TouchInputChannel for TouchQueue {
        fn next_frame(&mut self) -> Result<Option<[u8; TouchFrame::WIRE_LEN]>, Errno> {
            Ok(self.frames.pop_front())
        }
    }

    type TestSource = DeviceInputSource<QueueChannel, TouchQueue>;

    fn source(events: &[PointerInput]) -> TestSource {
        DeviceInputSource::new(QueueChannel::new(events), TouchQueue::default(), SCREEN)
            .expect("non-empty screen")
    }

    #[test]
    fn empty_screen_is_refused_at_construction() {
        assert_eq!(
            DeviceInputSource::new(QueueChannel::new(&[]), TouchQueue::default(), Rect::EMPTY)
                .map(|_| ()),
            Err(Errno::OutOfRange)
        );
    }

    /// D446: Ctrl with the wheel counted as a lone Ctrl, so releasing it
    /// showed the locate rings — a scroll stamped no gesture time.
    #[test]
    fn a_wheel_turn_is_a_gesture_a_lone_ctrl_must_not_have_seen() {
        let mut source = source(&[PointerInput::Scrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        }]);
        assert!(source.quiet_since(0));
        assert!(source.poll(100).is_ok());
        assert!(!source.quiet_since(50), "turned since Ctrl went down");
        assert!(!source.quiet_since(100), "turned in the same wake");
        assert!(source.quiet_since(101), "but nothing since");
    }

    #[test]
    fn buttons_are_quiet_only_when_none_is_held_or_changed_since() {
        let mut source = source(&[
            PointerInput::Pressed(PointerButtonCode::Primary),
            PointerInput::Released(PointerButtonCode::Primary),
        ]);
        assert!(source.quiet_since(0), "nothing has happened yet");
        assert!(source.poll(100).is_ok());
        assert!(!source.quiet_since(50), "held");
        assert!(source.poll(200).is_ok());
        assert!(!source.quiet_since(150), "released since");
        assert!(!source.quiet_since(200), "released in the same wake");
        assert!(source.quiet_since(201));
    }

    #[test]
    fn pointer_starts_at_the_screen_centre() {
        let source = source(&[]);
        assert_eq!(source.pointer(), Point::new(320, 240));
    }

    #[test]
    fn displacements_accumulate_into_an_absolute_position() {
        let mut source = source(&[
            PointerInput::MovedBy { dx: 12, dy: -5 },
            PointerInput::MovedBy { dx: -2, dy: 0 },
        ]);
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(332, 235)
            }))
        );
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(330, 235)
            }))
        );
        assert_eq!(source.poll(0), Ok(None));
    }

    #[test]
    fn motion_is_clamped_to_the_screen() {
        // A displacement past every edge — including i32 extremes, which
        // must saturate rather than wrap — pins the pointer to the edge.
        let mut source = source(&[
            PointerInput::MovedBy {
                dx: i32::MIN,
                dy: i32::MIN,
            },
            PointerInput::MovedBy {
                dx: i32::MAX,
                dy: i32::MAX,
            },
        ]);
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(0, 0)
            }))
        );
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(639, 479)
            }))
        );
    }

    #[test]
    fn decodes_each_button_for_press_and_release() {
        let mut events = Vec::new();
        for code in [
            PointerButtonCode::Primary,
            PointerButtonCode::Secondary,
            PointerButtonCode::Middle,
        ] {
            events.push(PointerInput::Pressed(code));
            events.push(PointerInput::Released(code));
        }
        let mut source = source(&events);
        for button in [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
        ] {
            assert_eq!(source.poll(0), Ok(Some(pressed(button))));
            assert_eq!(source.poll(0), Ok(Some(released(button))));
        }
    }

    /// A release reaches the desktop whatever was pressed before it, so a
    /// press whose source died before releasing is undone by the next one.
    #[test]
    fn every_release_reaches_the_desktop() {
        let mut source = source(&[
            PointerInput::Released(PointerButtonCode::Secondary),
            PointerInput::Pressed(PointerButtonCode::Middle),
        ]);
        assert_eq!(source.poll(0), Ok(Some(released(PointerButton::Secondary))));
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Middle))));
        assert_eq!(source.poll(0), Ok(None));
    }

    #[test]
    fn buttons_do_not_move_the_pointer() {
        let mut source = source(&[PointerInput::Pressed(PointerButtonCode::Primary)]);
        let before = source.pointer();
        assert!(matches!(
            source.poll(0),
            Ok(Some(InputEvent::PointerPressed { .. }))
        ));
        assert_eq!(source.pointer(), before);
    }

    #[test]
    fn a_scroll_keeps_its_units_and_does_not_move_the_pointer() {
        let mut source = source(&[PointerInput::Scrolled {
            dx: -DETENT,
            dy: 4 * DETENT,
        }]);
        let before = source.pointer();
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerScrolled {
                dx: -DETENT,
                dy: 4 * DETENT
            }))
        );
        assert_eq!(source.pointer(), before);
    }

    const DETENT: i32 = SCROLL_UNITS_PER_DETENT;

    /// What each of `steps` turns of `units`, one drained every `apart_ns`
    /// nanoseconds, is worth once accelerated.
    fn turned_by(units: i32, steps: usize, apart_ns: u64) -> Vec<i32> {
        let mut axis = WheelAxis::new();
        (0..steps)
            .map(|at| axis.accelerated(units, 10_000 * MS + at as u64 * apart_ns))
            .collect()
    }

    /// [`turned_by`] for whole detents `apart_ms` milliseconds apart.
    fn turned(detents: usize, apart_ms: u64) -> Vec<i32> {
        turned_by(DETENT, detents, apart_ms * MS)
    }

    #[test]
    fn a_deliberate_turn_moves_one_detent_at_a_time() {
        assert!(turned(6, 250).iter().all(|&units| units == DETENT));
        assert!(turned(6, 125).iter().all(|&units| units == DETENT));
    }

    #[test]
    fn a_faster_turn_is_multiplied_in_proportion_and_capped() {
        // Twenty detents a second is two and a half times a deliberate turn.
        let twenty = turned(8, 50);
        assert_eq!(twenty[0], DETENT, "the first detent has nothing to measure");
        assert_eq!(*twenty.last().expect("a turn"), DETENT * 5 / 2);
        // A hundred a second is past the ceiling.
        let spin = turned(12, 10);
        assert_eq!(*spin.last().expect("a spin"), DETENT * 6);
        assert!(spin.windows(2).all(|pair| pair[0] <= pair[1]), "{spin:?}");
    }

    #[test]
    fn a_fine_wheel_accelerates_exactly_as_its_detents_would() {
        // An eighth of a detent every 6.25 ms is the same turning as a detent
        // every 50 ms — twenty detents a second, two and a half times steady
        // — so once each is in steady state an eighth step is worth an eighth
        // of a detent step. Only how soon steady state is reached differs:
        // the fine wheel reports eight times as often.
        let fine = turned_by(DETENT / 8, 64, 50 * MS / 8);
        let coarse = turned(8, 50);
        let fine_step = *fine.last().expect("a turn");
        let coarse_step = *coarse.last().expect("a turn");
        assert_eq!(coarse_step, DETENT * 5 / 2);
        assert!(
            (fine_step * 8 - coarse_step).abs() <= 8,
            "an eighth step {fine_step} against a detent step {coarse_step}"
        );
        // A slow fine turn is never rounded away: every step is delivered.
        let slow = turned_by(15, 8, 250 * MS);
        assert_eq!(slow, alloc::vec![15; 8]);
    }

    #[test]
    fn detents_drained_together_are_not_taken_for_a_spin() {
        let mut axis = WheelAxis::new();
        let now = 10_000 * MS;
        assert_eq!(axis.accelerated(DETENT, now), DETENT);
        assert_eq!(
            axis.accelerated(DETENT, now),
            DETENT,
            "one drain is one sample"
        );
        assert_eq!(axis.accelerated(2 * DETENT, now), 2 * DETENT);
    }

    #[test]
    fn a_reversal_or_a_pause_starts_the_turn_afresh() {
        let mut axis = WheelAxis::new();
        let mut now = 10_000 * MS;
        for _ in 0..10 {
            axis.accelerated(DETENT, now);
            now += 10 * MS;
        }
        assert!(
            axis.accelerated(DETENT, now) > DETENT,
            "the spin was accelerated"
        );
        assert_eq!(
            axis.accelerated(-DETENT, now + 10 * MS),
            -DETENT,
            "a reversal is afresh"
        );
        for _ in 0..10 {
            axis.accelerated(-DETENT, now);
            now += 10 * MS;
        }
        assert_eq!(
            axis.accelerated(-DETENT, now + WHEEL_WINDOW_NS + MS),
            -DETENT,
            "and so is a pause longer than the window"
        );
    }

    #[test]
    fn malformed_record_surfaces_bad_magic() {
        let mut channel = QueueChannel::new(&[]);
        channel.push_raw([0u8; PointerInput::WIRE_LEN]);
        let mut source = DeviceInputSource::new(channel, TouchQueue::default(), SCREEN)
            .expect("non-empty screen");
        // An all-zero record has the wrong magic and must be refused, never
        // misinterpreted.
        assert_eq!(source.poll(0), Err(Errno::BadMagic));
    }

    #[test]
    fn channel_fault_propagates() {
        let mut channel = QueueChannel::new(&[PointerInput::MovedBy { dx: 1, dy: 2 }]);
        channel.fault_with(Errno::NotFound);
        let mut source = DeviceInputSource::new(channel, TouchQueue::default(), SCREEN)
            .expect("non-empty screen");
        assert_eq!(source.poll(0), Err(Errno::NotFound));
        // After the one-shot fault clears, the queued record still decodes.
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(321, 242)
            }))
        );
    }

    #[test]
    fn into_channels_returns_the_wrapped_channels() {
        let source = source(&[PointerInput::MovedBy { dx: 0, dy: 0 }]);
        let (channel, touch) = source.into_channels();
        assert_eq!(channel.records.len(), 1);
        assert!(touch.frames.is_empty());
    }

    /// Queue one frame of `contacts` on a `surface` that states no size, as
    /// the kernel stamps it at `at_ms`.
    fn touch(source: &mut TestSource, surface: TouchSurface, at_ms: u64, contacts: &[Contact]) {
        let mut frame = TouchFrame::new(0, surface, TouchButtons::NONE, TouchExtent::default());
        for &contact in contacts {
            frame.push(contact).expect("room for the contact");
        }
        source
            .touch
            .frames
            .push_back(frame.stamped(1, at_ms * MS).to_le_bytes());
    }

    fn drain(source: &mut TestSource, now_ms: u64) -> Vec<InputEvent> {
        core::iter::from_fn(|| source.poll(now_ms * MS).expect("a sound stream")).collect()
    }

    #[test]
    fn a_touchscreen_puts_the_pointer_where_it_is_touched() {
        let mut source = source(&[]);
        touch(
            &mut source,
            TouchSurface::Screen,
            0,
            &[Contact::finger(1, 0, 0)],
        );
        assert_eq!(
            drain(&mut source, 0),
            [InputEvent::PointerMoved {
                to: Point::new(0, 0)
            }]
        );
        touch(
            &mut source,
            TouchSurface::Screen,
            200,
            &[Contact::finger(1, u16::MAX, u16::MAX)],
        );
        assert_eq!(
            drain(&mut source, 200),
            [
                pressed(PointerButton::Primary),
                InputEvent::PointerMoved {
                    to: Point::new(639, 479)
                }
            ],
            "the far corner, pressed once it moved"
        );
        touch(&mut source, TouchSurface::Screen, 210, &[]);
        assert_eq!(drain(&mut source, 210), [released(PointerButton::Primary)]);
    }

    #[test]
    fn a_touchscreens_waiting_press_acts_at_its_deadline_and_not_before() {
        let mut source = source(&[]);
        touch(
            &mut source,
            TouchSurface::Screen,
            0,
            &[Contact::finger(1, 100, 100)],
        );
        let _ = drain(&mut source, 0);
        assert!(!source.touch_due(99 * MS));
        assert!(source.touch_due(100 * MS));
        assert_eq!(source.park_deadline_ns(40 * MS, u64::MAX), 60 * MS);
        assert_eq!(drain(&mut source, 99), []);
        assert_eq!(drain(&mut source, 100), [pressed(PointerButton::Primary)]);
        assert!(!source.touch_due(u64::MAX), "nothing left to wait for");
        assert!(!source.quiet_since(0), "a touch press is a gesture");
    }

    /// Each source's presses reach the desktop as they happen; the pointer is
    /// quiet again only once every source has let go.
    #[test]
    fn a_tap_and_the_mouse_each_press_and_release_as_they_happen() {
        let mut source = source(&[PointerInput::Pressed(PointerButtonCode::Primary)]);
        assert_eq!(drain(&mut source, 0), [pressed(PointerButton::Primary)]);
        touch(
            &mut source,
            TouchSurface::Touchpad,
            10,
            &[Contact::finger(1, 100, 100)],
        );
        touch(&mut source, TouchSurface::Touchpad, 40, &[]);
        assert_eq!(drain(&mut source, 40), [pressed(PointerButton::Primary)]);
        assert_eq!(
            drain(&mut source, 1_000),
            [released(PointerButton::Primary)]
        );
        assert!(!source.quiet_since(0), "the mouse still holds it");
        source
            .channel
            .records
            .push_back(PointerInput::Released(PointerButtonCode::Primary).to_le_bytes());
        assert_eq!(
            drain(&mut source, 1_000),
            [released(PointerButton::Primary)]
        );
        assert!(source.quiet_since(1_000 * MS + 1));
    }

    #[test]
    fn a_two_finger_scroll_arrives_unaccelerated_and_a_pinch_at_its_place() {
        let mut source = source(&[]);
        let pair = |y: u16| [Contact::finger(1, 1_000, y), Contact::finger(2, 9_000, y)];
        touch(&mut source, TouchSurface::Touchpad, 0, &pair(10_000));
        let mut events = Vec::new();
        for frame in 1..=10u16 {
            touch(
                &mut source,
                TouchSurface::Touchpad,
                u64::from(frame),
                &pair(10_000 - 500 * frame),
            );
            events.extend(drain(&mut source, u64::from(frame)));
        }
        let scrolled: i32 = events
            .iter()
            .map(|event| match event {
                InputEvent::PointerScrolled { dy, .. } => *dy,
                _ => 0,
            })
            .sum();
        assert!(scrolled > 0, "fingers up move the content up: {events:?}");
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, InputEvent::PointerMoved { .. })),
            "two fingers move no pointer"
        );

        // A touchscreen pinch is at its fingers' centre, mapped to the screen.
        let mut screen = source_with_density(Scale::ONE);
        let spread = |half: u16| {
            [
                Contact::finger(1, 32_767 - half, 32_767),
                Contact::finger(2, 32_767 + half, 32_767),
            ]
        };
        touch(&mut screen, TouchSurface::Screen, 0, &spread(4_000));
        touch(&mut screen, TouchSurface::Screen, 10, &spread(8_000));
        let events = drain(&mut screen, 10);
        assert!(
            events.iter().any(|event| matches!(
                event,
                InputEvent::Pinch {
                    phase: PinchPhase::Begin,
                    at,
                    ..
                } if *at == Point::new(319, 239)
            )),
            "{events:?}"
        );
    }

    fn source_with_density(scale: Scale) -> TestSource {
        let mut source = source(&[]);
        source.set_density(scale);
        source
    }

    #[test]
    fn a_touchscreen_without_its_own_size_is_measured_by_the_screens_density() {
        // 640 pixels at 96 DPI are 169 mm, so a sixty-fifth of the way across
        // is 2.6 mm and moves past the press slop; at 384 DPI it is 0.6 mm.
        for (scale, presses) in [
            (Scale::ONE, true),
            (Scale::from_percent(400).expect("a scale"), false),
        ] {
            let mut source = source_with_density(scale);
            touch(
                &mut source,
                TouchSurface::Screen,
                0,
                &[Contact::finger(1, 0, 0)],
            );
            touch(
                &mut source,
                TouchSurface::Screen,
                10,
                &[Contact::finger(1, 1_000, 0)],
            );
            let events = drain(&mut source, 10);
            assert_eq!(
                events.contains(&pressed(PointerButton::Primary)),
                presses,
                "{scale:?}: {events:?}"
            );
        }
    }

    const fn pressed(button: PointerButton) -> InputEvent {
        InputEvent::PointerPressed { button }
    }

    const fn released(button: PointerButton) -> InputEvent {
        InputEvent::PointerReleased { button }
    }

    #[test]
    fn a_left_handed_order_swaps_the_two_main_buttons() {
        let mut source = source(&[
            PointerInput::Pressed(PointerButtonCode::Primary),
            PointerInput::Released(PointerButtonCode::Primary),
            PointerInput::Pressed(PointerButtonCode::Secondary),
            PointerInput::Released(PointerButtonCode::Secondary),
            PointerInput::Pressed(PointerButtonCode::Middle),
        ]);
        source.set_policy(PrimaryButton::Right, PointerSpeed::NORMAL);
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Secondary))));
        assert_eq!(source.poll(0), Ok(Some(released(PointerButton::Secondary))));
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Primary))));
        assert_eq!(source.poll(0), Ok(Some(released(PointerButton::Primary))));
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Middle))));
    }

    /// A button order chosen mid-press waits for the release, so the release
    /// names the button that was pressed.
    #[test]
    fn a_new_button_order_waits_until_no_button_is_held() {
        let mut source = source(&[
            PointerInput::Pressed(PointerButtonCode::Primary),
            PointerInput::Released(PointerButtonCode::Primary),
            PointerInput::Pressed(PointerButtonCode::Primary),
        ]);
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Primary))));
        source.set_policy(PrimaryButton::Right, PointerSpeed::NORMAL);
        assert_eq!(source.poll(0), Ok(Some(released(PointerButton::Primary))));
        assert_eq!(source.poll(0), Ok(Some(pressed(PointerButton::Secondary))));
    }

    #[test]
    fn a_speed_scales_motion_and_carries_what_is_too_small_to_move() {
        let moves = [PointerInput::MovedBy { dx: 1, dy: -1 }; 4];
        let mut slow = source(&moves);
        slow.set_policy(
            PrimaryButton::Left,
            PointerSpeed::from_percent(50).expect("a speed"),
        );
        let mut at = Point::new(320, 240);
        for _ in 0..4 {
            if let Ok(Some(InputEvent::PointerMoved { to })) = slow.poll(0) {
                at = to;
            }
        }
        assert_eq!(at, Point::new(322, 238), "four half-counts move two whole");

        let mut fast = source(&[PointerInput::MovedBy { dx: 10, dy: 0 }]);
        fast.set_policy(
            PrimaryButton::Left,
            PointerSpeed::from_percent(300).expect("a speed"),
        );
        assert_eq!(
            fast.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(350, 240)
            }))
        );
    }

    #[test]
    fn a_huge_scaled_displacement_saturates_rather_than_wrapping() {
        let mut source = source(&[PointerInput::MovedBy {
            dx: i32::MAX,
            dy: i32::MIN,
        }]);
        source.set_policy(PrimaryButton::Left, PointerSpeed::MAX);
        assert_eq!(
            source.poll(0),
            Ok(Some(InputEvent::PointerMoved {
                to: Point::new(639, 0)
            }))
        );
    }
}
