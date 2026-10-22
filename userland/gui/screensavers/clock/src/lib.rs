//! Clock, a TAIRiX screensaver: the time, and — as its options say — the date
//! and who is signed in where, moved about the screen each minute so no pixel
//! stays lit for long.
//!
//! The time and date are the host's reading, spelled as the icon bar spells
//! them, so the two never disagree, and the face asks for its next frame when
//! that reading next changes. At each change the block fades out, moves, and
//! fades back in over the theme's stage transition; under reduced motion it
//! simply moves.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use tairix_abi::time::NANOS_PER_MILLI;
use tairix_abi::Errno;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Region};
use tairix_raster::{Color, Surface};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_saver::{erase, footprint, DateShown, Frame, Host, NextFrame, Scene, Setup, Told};
use tairix_theme::motion::SceneClock;
use tairix_theme::{MotionInteraction, TextRole};

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
pub struct ClockFace {
    /// The time and date the block was last composed from; `None` before the
    /// first frame.
    shown: Option<Told>,
    /// The date told beneath the time, if one is.
    date: DateShown,
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
    frame_ns: u64,
}

impl ClockFace {
    /// The face `setup` describes.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for an option the host did not send as the
    /// manifest declares it.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let start = setup.start;
        let theme = setup.theme();
        let scale = setup.scale;
        let screen = (start.region.width(), start.region.height());
        let display = BitmapFont::for_role(theme.fonts(), TextRole::Display, scale);
        let time_px = (screen.1 / TIME_SHARE).max(scale.scale_length(TIME_MIN_LOGICAL));
        let face =
            |px: u32| BitmapFont::new(display.family(), px.max(1)).with_weight(display.weight());
        Ok(Self {
            shown: None,
            date: if setup.switch("date")? {
                DateShown::Numeric
            } else {
                DateShown::Hidden
            },
            identity: if setup.switch("identity")? {
                identity_line(start.account.as_str(), start.machine.as_str())
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
            rng: NonCryptoRng::seed_from_u64(setup.seed),
            screen,
            frame_ns: SceneClock::frame_on(start.refresh),
        })
    }

    /// The reading changed: compose it and set the block moving to a new
    /// place, or move it at once on the first frame and under reduced motion.
    fn turn(&mut self, told: Told, now_ns: u64, damage: &mut Region) {
        let next = self.compose(&told);
        let to = self.place(next.as_ref());
        let first = self.shown.is_none();
        self.shown = Some(told);
        if first || self.fade_ns == 0 {
            damage.add(footprint(self.block.as_ref(), self.at));
            self.block = next;
            self.at = to;
            return;
        }
        self.moving = Some(Move {
            started_ns: now_ns,
            next: Some((next, to)),
        });
    }

    /// One frame of the move in flight.
    fn step_move(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(moving) = self.moving.as_mut() else {
            return;
        };
        let elapsed = now_ns.saturating_sub(moving.started_ns);
        let fade = self.fade_ns;
        if elapsed < fade {
            self.strength = fade_strength(fade - elapsed, fade);
            return;
        }
        if let Some((next, to)) = moving.next.take() {
            damage.add(footprint(self.block.as_ref(), self.at));
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

    /// Repaint where the block was, gathered in `damage`, and where it now is.
    fn repaint(&self, surface: &mut Surface, damage: &mut Region) {
        damage.add(footprint(self.block.as_ref(), self.at));
        for rect in damage.rects() {
            erase(surface, *rect);
        }
        if let Some(block) = &self.block {
            surface.blit_faded(self.at.x, self.at.y, block, self.strength);
        }
    }

    /// The face's lines composed into one transparent block, centred on each
    /// other; `None` when there is no line to show or no surface to hold
    /// them.
    fn compose(&self, told: &Told) -> Option<Surface> {
        let lines = [
            (told.time(), self.fonts[0], TIME_INK),
            (told.date(), self.fonts[1], DETAIL_INK),
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

impl Scene for ClockFace {
    /// Show the frame's reading: the first at once, a changed one by moving
    /// the block, and a frame of a move in flight.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        let now_ns = frame.now_ns;
        if self.moving.is_none() {
            let told = Told::of(frame.wall, self.date);
            if self.shown == Some(told) {
                return NextFrame::At(frame.reading_changes_ns(self.frame_ns));
            }
            self.turn(told, now_ns, damage);
        }
        self.step_move(now_ns, damage);
        self.repaint(surface, damage);
        if self.moving.is_some() {
            NextFrame::At(now_ns.saturating_add(self.frame_ns))
        } else {
            NextFrame::At(frame.reading_changes_ns(self.frame_ns))
        }
    }
}

/// The line naming who is signed in and where; empty when neither is known.
fn identity_line(user: &str, host: &str) -> String {
    match (user.is_empty(), host.is_empty()) {
        (true, true) => String::new(),
        (false, true) => String::from(user),
        (true, false) => String::from(host),
        (false, false) => format!("{user} \u{b7} {host}"),
    }
}

/// The strength of a fade `into` nanoseconds of its `span`, which is not
/// zero.
fn fade_strength(into: u64, span: u64) -> u8 {
    u8::try_from(into.min(span) * 255 / span).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests;
