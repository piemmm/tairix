//! The clock screensaver: the time, and — as its options say — the date and
//! who is signed in where, moved about the screen each minute so no pixel
//! stays lit for long.
//!
//! The time is the icon bar's own reading and spelling
//! ([`SessionClock`](crate::clock::SessionClock)), so the two never disagree,
//! and it ticks on the same minute. At each tick the block fades out, moves,
//! and fades back in over the theme's stage transition; under reduced motion
//! that is nothing, and it simply moves.

use alloc::format;
use alloc::string::String;

use tairix_abi::time::{WallClockReading, NANOS_PER_MILLI};
use tairix_browse::format_date;
use tairix_font::BitmapFont;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::{MotionInteraction, TextRole, Theme};
use tairix_wallpaper::ClockOptions;
use tairix_wm::{Color, Compositor, Point, Rect, Region, Scale, Surface, WindowId};

use super::seed_from;
use super::telling::{lettered_rect, DateSpelling, Telling};
use tairix_theme::motion::SceneClock;

/// Who is signed in, and where, as the clock screensaver names them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaverIdentity {
    /// The account's login name; empty when unknown.
    pub user: String,
    /// The machine's name; empty when unknown.
    pub host: String,
}

impl SaverIdentity {
    /// The line naming who and where; empty when neither is known.
    fn line(&self) -> String {
        match (self.user.is_empty(), self.host.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.user.clone(),
            (true, false) => self.host.clone(),
            (false, false) => format!("{} \u{b7} {}", self.user, self.host),
        }
    }
}

/// The time's type height as a share of the screen's.
const TIME_SHARE: u32 = 6;

/// The least the time is drawn at, in logical pixels.
const TIME_MIN_LOGICAL: u32 = 48;

/// The time's ink, and the quieter lines' beneath it: both read against the
/// black the screensaver always is, whatever the desktop's appearance.
const TIME_INK: Color = Color::rgb(0xF2, 0xF2, 0xF4);
const DETAIL_INK: Color = Color::rgb(0x9A, 0x9A, 0xA2);

/// An in-flight move of the block: the old one fading out, then the new one
/// fading in at its new place.
struct Move {
    started_ns: u64,
    /// The block shown once the old one is gone, and where; taken when it is.
    next: Option<(Option<Surface>, Point)>,
}

/// The clock screensaver.
pub(super) struct ClockFace {
    telling: Telling,
    identity: String,
    /// The time's, the date's, and the identity line's type.
    fonts: [BitmapFont; 3],
    /// The lines composed together, or `None` when there is nothing to show.
    block: Option<Surface>,
    at: Point,
    strength: u8,
    moving: Option<Move>,
    /// How long the block takes to fade out, and to fade back in; zero when
    /// it moves at once.
    fade_ns: u64,
    rng: NonCryptoRng,
    screen: (u32, u32),
    damage: Region,
    due_ns: u64,
}

impl ClockFace {
    /// A face for a `screen` at `scale`, telling `wall` as of `now_ns` with the
    /// lines `options` ask for.
    pub(super) fn new(
        identity: &SaverIdentity,
        theme: &Theme,
        (scale, screen): (Scale, (u32, u32)),
        (wall, now_ns): (Option<WallClockReading>, u64),
        options: ClockOptions,
    ) -> Self {
        let display = BitmapFont::for_role(theme.fonts(), TextRole::Display, scale);
        let time_px = (screen.1 / TIME_SHARE).max(scale.scale_length(TIME_MIN_LOGICAL));
        let face =
            |px: u32| BitmapFont::new(display.family(), px.max(1)).with_weight(display.weight());
        let spelling: DateSpelling = format_date;
        let mut face = Self {
            telling: Telling::new(options.date.then_some(spelling), (wall, now_ns)),
            identity: if options.identity {
                identity.line()
            } else {
                String::new()
            },
            fonts: [face(time_px), face(time_px / 4), face(time_px * 3 / 16)],
            block: None,
            at: Point::ORIGIN,
            strength: u8::MAX,
            moving: None,
            fade_ns: u64::from(theme.motion().duration(MotionInteraction::StageTransition))
                * NANOS_PER_MILLI,
            rng: NonCryptoRng::seed_from_u64(seed_from(now_ns)),
            screen,
            damage: Region::new(),
            due_ns: now_ns,
        };
        let block = face.compose();
        face.at = face.place(block.as_ref());
        face.block = block;
        face.due_ns = face.telling.tick_ns();
        face
    }

    /// When the next frame, or the next minute, is due.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Draw the face whole onto `surface`, as it stands.
    pub(super) fn paint(&self, surface: &mut Surface) {
        if let Some(block) = &self.block {
            surface.blit_faded(self.at.x, self.at.y, block, self.strength);
        }
    }

    /// Step the face to `now_ns`: a frame of a move in flight, or the minute
    /// turning. `wall` is asked for the time only at a turn.
    pub(super) fn advance(
        &mut self,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        wall: &mut dyn FnMut() -> Option<WallClockReading>,
    ) {
        if now_ns < self.due_ns {
            return;
        }
        if self.moving.is_none() {
            self.turn(now_ns, wall);
            if self.moving.is_none() {
                self.repaint(wm, compositor);
                return;
            }
        }
        self.step_move(now_ns, wm, compositor);
    }

    /// The minute has turned: read the time and set the block moving to a new
    /// place, or move it at once under reduced motion.
    fn turn(&mut self, now_ns: u64, wall: &mut dyn FnMut() -> Option<WallClockReading>) {
        self.telling.read(wall(), now_ns);
        let next = self.compose();
        let to = self.place(next.as_ref());
        if self.fade_ns == 0 {
            self.damage.clear();
            self.damage.add(lettered_rect(self.block.as_ref(), self.at));
            self.block = next;
            self.at = to;
            self.due_ns = self.telling.tick_ns();
            return;
        }
        self.moving = Some(Move {
            started_ns: now_ns,
            next: Some((next, to)),
        });
    }

    /// One frame of the move in flight.
    fn step_move(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
        let Some(moving) = self.moving.as_mut() else {
            return;
        };
        let elapsed = now_ns.saturating_sub(moving.started_ns);
        let fade = self.fade_ns;
        self.damage.clear();
        if elapsed < fade {
            self.strength = fade_strength(fade - elapsed, fade);
        } else {
            if let Some((next, to)) = moving.next.take() {
                self.damage.add(lettered_rect(self.block.as_ref(), self.at));
                self.block = next;
                self.at = to;
            }
            let arrived = elapsed - fade;
            if arrived >= fade {
                self.strength = u8::MAX;
                self.moving = None;
            } else {
                self.strength = fade_strength(arrived, fade);
            }
        }
        self.due_ns = if self.moving.is_some() {
            now_ns.saturating_add(SceneClock::FRAME_NS)
        } else {
            self.telling.tick_ns()
        };
        self.repaint(wm, compositor);
    }

    /// Repaint where the block was, gathered in the damage, and where it now
    /// is.
    fn repaint(&mut self, wm: WindowId, compositor: &mut Compositor) {
        self.damage.add(lettered_rect(self.block.as_ref(), self.at));
        let Self {
            block,
            at,
            strength,
            damage,
            screen,
            ..
        } = self;
        let _ = compositor.repaint_window(wm, *screen, damage, |surface, rects| {
            for rect in rects {
                erase(surface, *rect);
            }
            if let Some(block) = block.as_ref() {
                surface.blit_faded(at.x, at.y, block, *strength);
            }
        });
    }

    /// The face's lines composed into one transparent block, centred on each
    /// other; `None` when there is no line to show or no surface to hold
    /// them.
    fn compose(&self) -> Option<Surface> {
        let lines = [
            (self.telling.time(), self.fonts[0], TIME_INK),
            (self.telling.date(), self.fonts[1], DETAIL_INK),
            (self.identity.as_str(), self.fonts[2], DETAIL_INK),
        ];
        let gap = self.fonts[1].line_height() / 3;
        let mut width = 0;
        let mut height = 0;
        let mut drawn = 0u32;
        for (text, font, _) in lines.iter().filter(|(text, _, _)| !text.is_empty()) {
            width = width.max(font.text_width(text));
            height += font.line_height();
            drawn += 1;
        }
        if drawn == 0 {
            return None;
        }
        let mut block = Surface::new(width.max(1), height + gap * (drawn - 1))?;
        let mut y = 0u32;
        for (text, font, ink) in lines.iter().filter(|(text, _, _)| !text.is_empty()) {
            let x = (width - font.text_width(text)) / 2;
            let _ = font.draw_text(
                &mut block,
                i32::try_from(x).unwrap_or(0),
                i32::try_from(y).unwrap_or(0),
                text,
                *ink,
            );
            y += font.line_height() + gap;
        }
        Some(block)
    }

    /// A place for `block` anywhere on the screen, clear of its edges by a
    /// margin, chosen afresh each minute; the centre when it does not fit.
    fn place(&mut self, block: Option<&Surface>) -> Point {
        let Some(block) = block else {
            return Point::ORIGIN;
        };
        let (width, height) = self.screen;
        let margin = height / 20;
        let mut spot = |room: u32, extent: u32| {
            let free = room.saturating_sub(extent + 2 * margin);
            if free == 0 {
                return i32::try_from(room.saturating_sub(extent) / 2).unwrap_or(0);
            }
            let offset = u32::try_from(self.rng.next_below(u64::from(free) + 1)).unwrap_or(0);
            i32::try_from(margin + offset).unwrap_or(0)
        };
        let x = spot(width, block.width());
        let y = spot(height, block.height());
        Point::new(x, y)
    }
}

/// The strength of a fade `into` nanoseconds of its `span`, which is not
/// zero.
fn fade_strength(into: u64, span: u64) -> u8 {
    u8::try_from(into.min(span) * 255 / span).unwrap_or(u8::MAX)
}

/// Black over `rect`.
fn erase(surface: &mut Surface, rect: Rect) {
    let Some((x, y)) = rect.surface_origin() else {
        return;
    };
    surface.fill_rect(x, y, rect.width, rect.height, Color::rgb(0, 0, 0));
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
