//! The seat's touch gesture recogniser.
//!
//! A touch device reports [`TouchFrame`]s: every contact on its surface, scan
//! by scan. What they mean — pointer motion, a click, a scroll, a pinch — is
//! the seat's policy, decided here once for every seat owner. A [`Recogniser`]
//! follows each surface apart, keyed by the injector the kernel stamped on its
//! frames and the injector's own index of the device, and answers
//! [`Gesture`]s.
//!
//! - **Touchpad.** One finger moves the pointer, further the faster it moves.
//!   Two fingers moving together scroll on both axes, and two moving apart or
//!   together pinch. A brief tap of one, two or three fingers clicks the
//!   primary, secondary or middle button where tapping is on; a one-finger
//!   tap's press is held a moment, so a touch that follows it drags and a
//!   second tap makes a double click. A clickpad's press is the button its
//!   fingers count.
//! - **Touchscreen.** One finger is the pointer at the place touched, pressed
//!   once it moves or rests, so a second finger landing with it begins a
//!   gesture instead of a click; two fingers scroll and pinch at their centre.
//! - A contact the device judges a palm takes part in nothing.
//!
//! The recogniser allocates nothing. Between frames it needs waking only at
//! the instant it states, [`Recogniser::deadline_ns`]; every queued frame is
//! fed before [`Recogniser::expire`] is called, so a frame that arrived before
//! a deadline is read before the deadline acts.

#![no_std]

mod fingers;
mod pad;
mod screen;
mod two;

use tairix_abi::input::PointerButtonCode;
use tairix_abi::touch::{PinchPhase, TouchExtent, TouchFrame, TouchSurface};
use tairix_input::PointerButton;

use fingers::{Fingers, Um};
use pad::Pad;
use screen::Screen;

/// The user's touch settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TouchSettings {
    /// A tap on a touchpad clicks.
    pub tap_to_click: bool,
    /// Two fingers on a touchpad move the content, as on a touchscreen,
    /// rather than the view.
    pub natural_scroll: bool,
    /// How far a touchpad finger moves the pointer, as a percentage of the
    /// standard gain.
    pub speed_percent: u16,
}

impl TouchSettings {
    /// Tapping on, natural scrolling, the standard speed.
    pub const DEFAULT: Self = Self {
        tap_to_click: true,
        natural_scroll: true,
        speed_percent: 100,
    };
}

impl Default for TouchSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A place on a touchscreen: 0 at its left or top edge, `u16::MAX` at its
/// right or bottom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfacePoint {
    /// Across.
    pub x: u16,
    /// Down.
    pub y: u16,
}

impl SurfacePoint {
    /// The pixel this place is on a screen `width` by `height` pixels that
    /// the surface spans edge to edge, rounded to the nearest.
    #[must_use]
    pub fn on_screen(self, width: u32, height: u32) -> (u32, u32) {
        (along(self.x, width), along(self.y, height))
    }
}

/// The pixel `at` of `u16::MAX` is along an axis `length` pixels long.
fn along(at: u16, length: u32) -> u32 {
    let last = u64::from(length.saturating_sub(1));
    let span = u64::from(u16::MAX);
    u32::try_from((u64::from(at) * last + span / 2) / span).unwrap_or(u32::MAX)
}

/// A button a touch surface pressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchPress {
    /// One of the surface's own buttons, which the seat maps through its
    /// button order as it does a mouse's.
    Device(PointerButtonCode),
    /// A click the fingers made — a tap, a touchscreen press, a clickpad
    /// counted by its fingers — which is already the button meant.
    Fingers(PointerButton),
}

/// A step of a pinch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pinch {
    /// Where the pinch is in its life.
    pub phase: PinchPhase,
    /// The fingers' spread relative to when the pinch began, in 16.16 fixed
    /// point.
    pub scale: u32,
    /// Where on a touchscreen the fingers' centre is; `None` on a touchpad,
    /// whose pinch is at the pointer.
    pub at: Option<SurfacePoint>,
}

/// What a touch meant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gesture {
    /// Move the pointer by `(dx, dy)` pixels: a touchpad finger's motion at
    /// the user's speed.
    MovedBy {
        /// Rightward.
        dx: i32,
        /// Downward.
        dy: i32,
    },
    /// Put the pointer at this place on a touchscreen.
    MovedTo(SurfacePoint),
    /// A button went down at the pointer.
    Pressed(TouchPress),
    /// A button came up at the pointer.
    Released(TouchPress),
    /// Scroll by `(dx, dy)` scroll units at the pointer, positive toward
    /// the end on each axis.
    Scrolled {
        /// Across.
        dx: i32,
        /// Down.
        dy: i32,
    },
    /// A pinch.
    Pinch(Pinch),
}

/// The most surfaces followed at once. A bound on what injectors can make the
/// seat hold, not a capacity: a machine has one or two, and the least recently
/// fed is let go when another arrives.
const SURFACES_MAX: usize = 8;

/// The size a touchpad that states none is taken to be: a laptop's, in tenths
/// of a millimetre.
const TOUCHPAD_EXTENT: TouchExtent = TouchExtent {
    width: 1_000,
    height: 650,
};

/// The size a touchscreen is taken to be when neither it nor the seat states
/// one: a 15.6-inch panel's.
const SCREEN_EXTENT: TouchExtent = TouchExtent {
    width: 3_440,
    height: 1_940,
};

/// The seat's touch gesture recogniser.
#[derive(Clone, Debug)]
pub struct Recogniser {
    settings: TouchSettings,
    screen: TouchExtent,
    tracks: [Option<Track>; SURFACES_MAX],
    fed: u64,
}

impl Recogniser {
    /// A recogniser following no surface yet.
    #[must_use]
    pub const fn new(settings: TouchSettings) -> Self {
        Self {
            settings,
            screen: TouchExtent {
                width: 0,
                height: 0,
            },
            tracks: [const { None }; SURFACES_MAX],
            fed: 0,
        }
    }

    /// Apply the user's touch settings from the next touch on.
    pub fn set_settings(&mut self, settings: TouchSettings) {
        self.settings = settings;
    }

    /// The screen a touchscreen that states no size of its own covers:
    /// `width` by `height` pixels drawn at `dpi` dots an inch.
    pub fn set_screen(&mut self, width: u32, height: u32, dpi: u32) {
        let dpi = u64::from(dpi.max(1));
        let tenths_of_mm = |px: u32| u16::try_from(u64::from(px) * 254 / dpi).unwrap_or(u16::MAX);
        self.screen = TouchExtent {
            width: tenths_of_mm(width),
            height: tenths_of_mm(height),
        };
    }

    /// Read `frame`, answering what it meant through `out`.
    pub fn feed(&mut self, frame: &TouchFrame, out: &mut dyn FnMut(Gesture)) {
        self.fed += 1;
        let index = self.slot_for(frame, out);
        let screen = self.screen;
        let settings = self.settings;
        let fed = self.fed;
        let track = self.tracks[index].get_or_insert_with(|| Track::new(frame, screen));
        track.feed(frame, settings, fed, out);
        if track.is_idle() {
            self.tracks[index] = None;
        }
    }

    /// The instant the recogniser must be woken at, when a frame does not
    /// come first: a held tap's release or a touchscreen's waiting press.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.tracks
            .iter()
            .flatten()
            .filter_map(Track::deadline_ns)
            .min()
    }

    /// Act on every deadline at or before `now_ns`.
    pub fn expire(&mut self, now_ns: u64, out: &mut dyn FnMut(Gesture)) {
        for slot in &mut self.tracks {
            if let Some(track) = slot {
                track.expire(now_ns, out);
                if track.is_idle() {
                    *slot = None;
                }
            }
        }
    }

    /// Let go of every surface, releasing what it held and undoing a pinch:
    /// the seat changed hands, or its touch channel was lost.
    pub fn reset(&mut self, out: &mut dyn FnMut(Gesture)) {
        for slot in &mut self.tracks {
            if let Some(mut track) = slot.take() {
                track.end(out);
            }
        }
    }

    /// The slot `frame`'s surface is followed in: its own, a free one, or the
    /// least recently fed, let go first. A surface that changed its kind or
    /// size is a new surface.
    fn slot_for(&mut self, frame: &TouchFrame, out: &mut dyn FnMut(Gesture)) -> usize {
        let held = self
            .tracks
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|track| track.names(frame)));
        let index = held
            .or_else(|| self.tracks.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                self.tracks
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.as_ref().map_or(0, |track| track.fed))
                    .map_or(0, |(index, _)| index)
            });
        let unchanged = self.tracks[index]
            .as_ref()
            .is_some_and(|track| track.names(frame) && track.same_surface(frame));
        if !unchanged {
            if let Some(mut track) = self.tracks[index].take() {
                track.end(out);
            }
        }
        index
    }
}

/// One surface followed.
#[derive(Clone, Debug)]
struct Track {
    source: u64,
    device: u16,
    surface: TouchSurface,
    extent: TouchExtent,
    size: Um,
    fed: u64,
    /// The newest frame's time: a frame stamped earlier is read at it, so
    /// every interval is non-negative.
    now_ns: u64,
    fingers: Fingers,
    kind: Kind,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Pad(Pad),
    Screen(Screen),
}

impl Track {
    fn new(frame: &TouchFrame, screen: TouchExtent) -> Self {
        let surface = frame.surface();
        let extent = frame.extent();
        let fallback = if surface.is_direct() {
            if screen.width == 0 || screen.height == 0 {
                SCREEN_EXTENT
            } else {
                screen
            }
        } else {
            TOUCHPAD_EXTENT
        };
        let stated = |stated: u16, fallback: u16| {
            i64::from(if stated == 0 { fallback } else { stated }) * 100
        };
        Self {
            source: frame.source(),
            device: frame.device(),
            surface,
            extent,
            size: Um::new(
                stated(extent.width, fallback.width),
                stated(extent.height, fallback.height),
            ),
            fed: 0,
            now_ns: frame.time_ns(),
            fingers: Fingers::new(),
            kind: match surface {
                TouchSurface::Screen => Kind::Screen(Screen::new()),
                TouchSurface::Touchpad => Kind::Pad(Pad::new(false)),
                TouchSurface::Clickpad => Kind::Pad(Pad::new(true)),
            },
        }
    }

    fn names(&self, frame: &TouchFrame) -> bool {
        self.source == frame.source() && self.device == frame.device()
    }

    fn same_surface(&self, frame: &TouchFrame) -> bool {
        self.surface == frame.surface() && self.extent == frame.extent()
    }

    fn feed(
        &mut self,
        frame: &TouchFrame,
        settings: TouchSettings,
        fed: u64,
        out: &mut dyn FnMut(Gesture),
    ) {
        self.fed = fed;
        let now_ns = frame.time_ns().max(self.now_ns);
        self.expire(now_ns, out);
        self.now_ns = now_ns;
        self.fingers.update(frame, self.size);
        match &mut self.kind {
            Kind::Pad(pad) => pad.frame(&self.fingers, frame.buttons(), now_ns, settings, out),
            Kind::Screen(screen) => screen.frame(&self.fingers, now_ns, out),
        }
        self.fingers.prune();
    }

    const fn deadline_ns(&self) -> Option<u64> {
        match &self.kind {
            Kind::Pad(pad) => pad.deadline_ns(),
            Kind::Screen(screen) => screen.deadline_ns(),
        }
    }

    fn expire(&mut self, now_ns: u64, out: &mut dyn FnMut(Gesture)) {
        match &mut self.kind {
            Kind::Pad(pad) => pad.expire(now_ns, out),
            Kind::Screen(screen) => screen.expire(now_ns, out),
        }
    }

    fn end(&mut self, out: &mut dyn FnMut(Gesture)) {
        match &mut self.kind {
            Kind::Pad(pad) => pad.end(out),
            Kind::Screen(screen) => screen.end(out),
        }
    }

    fn is_idle(&self) -> bool {
        self.fingers.is_empty()
            && match &self.kind {
                Kind::Pad(pad) => pad.is_idle(),
                Kind::Screen(screen) => screen.is_idle(),
            }
    }
}

#[cfg(test)]
mod tests;
