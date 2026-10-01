//! The warp starfield: stars flown through at a speed that cruises, surges
//! into warp, and settles back, the whole field turning slowly about the line
//! of flight. With warp turned off it only cruises; under reduced motion it
//! only cruises and does not turn.
//!
//! Each star is a point in a unit volume ahead of the viewer, projected with
//! perspective and drawn as the path it travelled over the frame: a dot while
//! cruising, a streak dimming to its tail in warp. Nearer stars are brighter
//! and wider; a star is respawned far away once it passes the viewer or
//! leaves the screen, and fades up from the dark as it approaches, so none
//! appears from nowhere.
//!
//! A frame repaints only where a star was and now is: the footprints drawn
//! last frame are erased and this frame's drawn, so the pixel work follows
//! the stars rather than the screen, however coarsely the damage the
//! compositor is told about is kept.

use alloc::vec::Vec;

use tairix_raster::{Canvas, ScanScratch, SUBPIXEL};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};
use tairix_wallpaper::StarfieldOptions;
use tairix_wm::{Color, Compositor, Rect, Region, Scale, Surface, WindowId};

use super::seed_from;
use tairix_theme::motion::{seconds, SceneClock};

/// Stars per million screen pixels at the field's own density, and the bounds
/// the count is kept in; a denser or sparser field scales all three, so every
/// density differs from the next on any screen.
const STARS_PER_MEGAPIXEL: u64 = 560;
const MIN_STARS: u64 = 160;
const MAX_STARS: u64 = 2_600;

/// Depth of the nearest plane a star is drawn at, and of the farthest.
const Z_NEAR: f64 = 0.015;
const Z_FAR: f64 = 1.0;

/// How far back a respawned star is placed, beneath the far plane.
const RESPAWN_DEPTH: f64 = 0.25;

/// Depth units a second while cruising, and in warp.
const CRUISE_SPEED: f64 = 0.09;
const WARP_SPEED: f64 = 1.35;

/// The flight's cycle, in seconds: cruise, surge into warp, hold, settle.
const CRUISE_S: f64 = 16.0;
const SURGE_S: f64 = 3.0;
const WARP_S: f64 = 7.0;
const SETTLE_S: f64 = 5.0;

/// Radians a second the field turns about the line of flight.
const ROLL_RATE: f64 = 0.02;

/// How far past the screen's edge a star's head may project before it is
/// respawned, in pixels: a streak entering from off screen still draws.
const EDGE_SLACK: f64 = 64.0;

/// The logical width of the faintest star and the widening of the nearest.
const BASE_WIDTH: f64 = 0.7;
const NEAR_WIDTH: f64 = 2.3;

/// The fraction of a streak's head brightness its tail keeps, out of 255.
const TAIL_FLOOR: u32 = 38;

/// Star colours by temperature and how common each is, out of 100: blue-white
/// giants, white, warm white, yellow, and the rare orange.
const TINTS: [((u8, u8, u8), u64); 5] = [
    ((0xC4, 0xD4, 0xFF), 30),
    ((0xFF, 0xFF, 0xFF), 40),
    ((0xFF, 0xF1, 0xDC), 20),
    ((0xFF, 0xDA, 0xA8), 8),
    ((0xFF, 0xB8, 0x9A), 2),
];

/// The colour every star leans towards in warp.
const WARP_TINT: (u8, u8, u8) = (0xA8, 0xC8, 0xFF);

/// One star: where it is, and how it looks.
#[derive(Copy, Clone, Debug)]
struct Star {
    x: f64,
    y: f64,
    z: f64,
    tint: (u8, u8, u8),
    /// Its own brightness, `0.55..=1.0`, so the field is not uniform.
    glow: f64,
}

/// One star as this frame draws it.
#[derive(Copy, Clone, Debug)]
struct Streak {
    head: (f64, f64),
    tail: (f64, f64),
    width: f64,
    color: Color,
    footprint: Rect,
}

/// The starfield screensaver.
pub(super) struct Starfield {
    stars: Vec<Star>,
    streaks: Vec<Streak>,
    /// The footprints the last frame drew, which this one erases.
    drawn: Vec<Rect>,
    damage: Region,
    scratch: ScanScratch,
    rng: NonCryptoRng,
    screen: Rect,
    size: (u32, u32),
    centre: (f64, f64),
    focal: f64,
    /// Half the spawn volume's extent across and up, in unit coordinates.
    spread: (f64, f64),
    /// Physical pixels per logical one, for star widths.
    pixel: f64,
    /// Cruise without surging or turning.
    calm: bool,
    /// Surge into warp and back, rather than only cruising.
    warp: bool,
    started_ns: u64,
    last_ns: u64,
    due_ns: u64,
}

impl Starfield {
    /// A field for a `size` screen at `scale` as `options` describe it, first
    /// drawn at `now_ns` and `calm` under reduced motion, or `None` when the
    /// heap will not give it.
    pub(super) fn new(
        size: (u32, u32),
        scale: Scale,
        (calm, options): (bool, StarfieldOptions),
        now_ns: u64,
    ) -> Option<Self> {
        let (width, height) = size;
        let pixels = u64::from(width) * u64::from(height);
        let percent = options.stars.percent();
        let density = |stars: u64| stars * percent / 100;
        let count = (pixels * density(STARS_PER_MEGAPIXEL) / 1_000_000)
            .clamp(density(MIN_STARS), density(MAX_STARS));
        let count = usize::try_from(count).ok()?;
        let half_height = f64::from(height.max(1)) / 2.0;
        let aspect = f64::from(width.max(1)) / f64::from(height.max(1));
        let mut field = Self {
            stars: Vec::new(),
            streaks: Vec::new(),
            drawn: Vec::new(),
            damage: Region::new(),
            scratch: ScanScratch::new(),
            rng: NonCryptoRng::seed_from_u64(seed_from(now_ns)),
            screen: Rect::new(0, 0, width, height),
            size,
            centre: (f64::from(width) / 2.0, half_height),
            focal: half_height,
            spread: (aspect * 1.15, 1.15),
            pixel: f64::from(scale.scale_length(1_000)) / 1_000.0,
            calm,
            warp: options.warp,
            started_ns: now_ns,
            last_ns: now_ns,
            due_ns: now_ns,
        };
        if !(fallible::reserve(&mut field.stars, count)
            && fallible::reserve(&mut field.streaks, count)
            && fallible::reserve(&mut field.drawn, count))
        {
            return None;
        }
        for _ in 0..count {
            let depth = Z_NEAR + field.rng.next_f64() * (Z_FAR - Z_NEAR);
            let star = field.spawn(depth);
            field.stars.push(star);
        }
        Some(field)
    }

    /// When the next frame is due.
    pub(super) const fn due_ns(&self) -> u64 {
        self.due_ns
    }

    /// Fly the field on to `now_ns` and draw the frame, if one is due.
    pub(super) fn advance(&mut self, now_ns: u64, wm: WindowId, compositor: &mut Compositor) {
        if now_ns < self.due_ns {
            return;
        }
        let step = now_ns
            .saturating_sub(self.last_ns)
            .min(SceneClock::MOST_FRAMES * SceneClock::FRAME_NS);
        self.last_ns = now_ns;
        self.due_ns = now_ns.saturating_add(SceneClock::FRAME_NS);
        let flight = seconds(now_ns.saturating_sub(self.started_ns));
        let (speed, roll) = match (self.calm, self.warp) {
            (true, _) => (CRUISE_SPEED, 0.0),
            (false, false) => (CRUISE_SPEED, ROLL_RATE * flight),
            (false, true) => (speed_at(flight), ROLL_RATE * flight),
        };
        self.fly(speed * seconds(step), speed, roll);

        // A field this dense has stars all over the screen, so its damage is
        // the box around them: far more rectangles than any present carries.
        let reached = self
            .drawn
            .iter()
            .chain(self.streaks.iter().map(|streak| &streak.footprint))
            .fold(Rect::EMPTY, |bounds, rect| bounds.union(rect));
        self.damage.clear();
        self.damage.add(reached);
        let Self {
            streaks,
            drawn,
            scratch,
            damage,
            size,
            ..
        } = self;
        // A kept buffer is black but for the last frame's stars.
        let kept = compositor.keeps_content(wm, *size);
        let _ = compositor.repaint_window(wm, *size, damage, |surface, rects| {
            let erased = if kept { drawn.as_slice() } else { rects };
            for rect in erased {
                erase(surface, *rect);
            }
            for streak in streaks.iter() {
                draw(surface, streak, scratch);
            }
        });
        self.drawn.clear();
        self.drawn
            .extend(self.streaks.iter().map(|streak| streak.footprint));
    }

    /// Move every star `travel` nearer at `speed` and project it into this
    /// frame's streaks, the field turned `roll` radians.
    fn fly(&mut self, travel: f64, speed: f64, roll: f64) {
        let (sin, cos) = (mathf::sin(roll), mathf::cos(roll));
        let warp = ((speed - CRUISE_SPEED) / (WARP_SPEED - CRUISE_SPEED)).clamp(0.0, 1.0);
        self.streaks.clear();
        for index in 0..self.stars.len() {
            let mut star = self.stars[index];
            star.z -= travel;
            if star.z <= Z_NEAR {
                star = self.respawn();
            }
            let (x, y) = (star.x * cos - star.y * sin, star.x * sin + star.y * cos);
            let head = self.project(x, y, star.z);
            if !self.on_screen(head) {
                self.stars[index] = self.respawn();
                continue;
            }
            self.stars[index] = star;
            let tail = self.project(x, y, (star.z + travel).min(Z_FAR));
            if let Some(streak) = self.streak(&star, head, tail, warp) {
                self.streaks.push(streak);
            }
        }
    }

    /// How `star` draws with its head at `head` and its tail at `tail`, or
    /// `None` when it is too faint to draw or wholly off screen.
    fn streak(&self, star: &Star, head: (f64, f64), tail: (f64, f64), warp: f64) -> Option<Streak> {
        let near = 1.0 - star.z / Z_FAR;
        let brightness = star.glow * near * mathf::sqrt(near) * (1.0 + 0.35 * warp);
        let alpha = to_channel(brightness * 255.0);
        if alpha == 0 {
            return None;
        }
        let width = self.pixel * (BASE_WIDTH + NEAR_WIDTH * near * near);
        let reach = width / 2.0 + 1.0;
        let left = mathf::floor(head.0.min(tail.0) - reach);
        let top = mathf::floor(head.1.min(tail.1) - reach);
        let right = mathf::ceil(head.0.max(tail.0) + reach);
        let bottom = mathf::ceil(head.1.max(tail.1) + reach);
        let footprint = Rect::new(
            mathf::round_i32(left),
            mathf::round_i32(top),
            span(right - left),
            span(bottom - top),
        )
        .intersection(&self.screen);
        if footprint.is_empty() {
            return None;
        }
        let (r, g, b) = mix(star.tint, WARP_TINT, 0.35 * warp);
        Some(Streak {
            head,
            tail,
            width,
            color: Color::rgba(r, g, b, alpha),
            footprint,
        })
    }

    /// Where a point at `(x, y)` and depth `z` lands on the screen.
    fn project(&self, x: f64, y: f64, z: f64) -> (f64, f64) {
        (
            self.centre.0 + x / z * self.focal,
            self.centre.1 + y / z * self.focal,
        )
    }

    /// Whether a projected point is within reach of the screen.
    fn on_screen(&self, (x, y): (f64, f64)) -> bool {
        let (width, height) = (f64::from(self.size.0), f64::from(self.size.1));
        (-EDGE_SLACK..=width + EDGE_SLACK).contains(&x)
            && (-EDGE_SLACK..=height + EDGE_SLACK).contains(&y)
    }

    /// A star to replace one that passed the viewer or left the screen: far
    /// off again, at a depth of its own so the field never arrives in waves.
    fn respawn(&mut self) -> Star {
        let depth = Z_FAR - self.rng.next_f64() * RESPAWN_DEPTH;
        self.spawn(depth)
    }

    /// A new star at depth `z`, anywhere across the spawn volume.
    fn spawn(&mut self, z: f64) -> Star {
        let x = (self.rng.next_f64() * 2.0 - 1.0) * self.spread.0;
        let y = (self.rng.next_f64() * 2.0 - 1.0) * self.spread.1;
        let mut pick = self.rng.next_below(100);
        let mut tint = TINTS[0].0;
        for (colour, share) in TINTS {
            if pick < share {
                tint = colour;
                break;
            }
            pick -= share;
        }
        Star {
            x,
            y,
            z,
            tint,
            glow: 0.55 + 0.45 * self.rng.next_f64(),
        }
    }
}

/// The flight's speed `flight` seconds in, in depth units a second.
fn speed_at(flight: f64) -> f64 {
    let cycle = CRUISE_S + SURGE_S + WARP_S + SETTLE_S;
    let t = flight - mathf::floor(flight / cycle) * cycle;
    if t < CRUISE_S {
        CRUISE_SPEED
    } else if t < CRUISE_S + SURGE_S {
        lerp(
            CRUISE_SPEED,
            WARP_SPEED,
            mathf::smoothstep((t - CRUISE_S) / SURGE_S),
        )
    } else if t < CRUISE_S + SURGE_S + WARP_S {
        WARP_SPEED
    } else {
        let settled = (t - CRUISE_S - SURGE_S - WARP_S) / SETTLE_S;
        lerp(WARP_SPEED, CRUISE_SPEED, mathf::smoothstep(settled))
    }
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// `from` moved `share` of the way towards `to`.
fn mix(from: (u8, u8, u8), to: (u8, u8, u8), share: f64) -> (u8, u8, u8) {
    let channel = |a: u8, b: u8| to_channel(f64::from(a) + (f64::from(b) - f64::from(a)) * share);
    (
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

/// `value` rounded into a colour channel.
fn to_channel(value: f64) -> u8 {
    u8::try_from(mathf::round_i32(value.clamp(0.0, 255.0))).unwrap_or(u8::MAX)
}

/// A non-negative pixel extent as a width.
fn span(extent: f64) -> u32 {
    u32::try_from(mathf::round_i32(extent.max(0.0))).unwrap_or(0)
}

/// Black over `rect`, which the frame repaints.
fn erase(surface: &mut Surface, rect: Rect) {
    let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
        return;
    };
    surface.fill_rect(x, y, rect.width, rect.height, Color::rgb(0, 0, 0));
}

/// A pixel coordinate in the scan converter's sub-pixel units.
fn sub(value: f64) -> i32 {
    mathf::round_i32(value * f64::from(SUBPIXEL))
}

/// Draw one streak, held to its own footprint: a dot while it barely moved,
/// otherwise a band from tail to head brightening towards the head.
fn draw(surface: &mut Surface, streak: &Streak, scratch: &mut ScanScratch) {
    let Streak {
        head,
        tail,
        width,
        color,
        footprint,
    } = *streak;
    let (Ok(fx), Ok(fy)) = (
        u32::try_from(footprint.left()),
        u32::try_from(footprint.top()),
    ) else {
        return;
    };
    let (dx, dy) = (head.0 - tail.0, head.1 - tail.1);
    let length = mathf::hypot(dx, dy);
    let half = width / 2.0;
    surface.with_clip(fx, fy, footprint.width, footprint.height, |surface| {
        if length < width * 0.75 {
            let dot = [
                (sub(head.0 - half), sub(head.1 - half)),
                (sub(head.0 + half), sub(head.1 - half)),
                (sub(head.0 + half), sub(head.1 + half)),
                (sub(head.0 - half), sub(head.1 + half)),
            ];
            Canvas::fill_polygon_subpixel(surface, &dot, color, scratch);
            return;
        }
        let (ox, oy) = (-dy / length * half, dx / length * half);
        let band = [
            (sub(tail.0 + ox), sub(tail.1 + oy)),
            (sub(tail.0 - ox), sub(tail.1 - oy)),
            (sub(head.0 - ox), sub(head.1 - oy)),
            (sub(head.0 + ox), sub(head.1 + oy)),
        ];
        let reach = length * length;
        surface.wash_polygon_subpixel_in(
            &band,
            color,
            |x, y| {
                let along = ((f64::from(x) + 0.5 - tail.0) * dx
                    + (f64::from(y) + 0.5 - tail.1) * dy)
                    / reach;
                let strength = TAIL_FLOOR
                    + u32::from(to_channel(
                        along.clamp(0.0, 1.0) * f64::from(255 - TAIL_FLOOR),
                    ));
                u8::try_from(strength).unwrap_or(u8::MAX)
            },
            scratch,
        );
    });
}

#[cfg(test)]
#[path = "starfield_tests.rs"]
mod tests;
