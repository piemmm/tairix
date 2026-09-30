//! The tank battle: two small armoured sides roll out of the haze onto the
//! grid ahead, take up places either side of the flight's path, and trade
//! fire — turrets swinging round, shells streaking across, a hit blowing a
//! tank apart — until one side is gone or the flight is upon them, when
//! whoever is left drives off out of sight.
//!
//! The battle stands on the ground, so the flight closes on it as it plays:
//! it is set far enough ahead that the flight crosses [`BATTLE_ROOM`] cells
//! before time runs out, whatever the pace. Every move is a closed course
//! between decisions — a turn on the spot, then a drive — so a tank stands
//! wherever its course says at any instant, and every decision falls at a
//! time its own course fixed.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_inline::ArrayVec;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;
use tairix_util::space::{Frame, Pose, Vec3};

use super::blast::{Blasts, Burst, Piece, Size};
use super::wire::{Camera, Corners, Hull, HullId, Ink, Lines, Models, Part, Stage};
use super::{between, pick, wrap, Rgb};

/// The two sides' colours.
static INKS: [Ink; 2] = [
    Ink {
        edge: Rgb::new(255.0, 92.0, 76.0),
        face: Rgb::new(26.0, 5.0, 5.0),
    },
    Ink {
        edge: Rgb::new(84.0, 222.0, 255.0),
        face: Rgb::new(4.0, 18.0, 26.0),
    },
];

/// How many cells the flight crosses while the battle may last, and how far
/// ahead the nearest place stands when it has.
pub(super) const BATTLE_ROOM: f64 = 11.0;
const NEAREST: f64 = 3.0;

/// Where a side takes its places: how far either side of the flight's path,
/// and how deep within the battle, in cells.
const PLACE_ACROSS: (f64, f64) = (1.9, 4.4);
const PLACE_DEEP: (f64, f64) = (0.0, 5.0);

/// How much farther off a tank rolls in from, and how long after the battle
/// begins it may set out.
const ROLL_IN: (f64, f64) = (7.0, 10.0);
const STAGGER: (f64, f64) = (0.0, 1.4);

/// How fast a tank drives in, repositions and drives off, in cells a second,
/// and turns its hull and its turret, in radians a second.
const DRIVE: f64 = 2.2;
const SHIFT: f64 = 1.4;
const LEAVE: f64 = 2.6;
const TURN: f64 = 1.6;
const SLEW: f64 = 1.3;

/// How long a tank holds its aim before firing, how long it takes to reload,
/// how often it moves between shots, and how far.
const SETTLE_S: f64 = 0.3;
const RELOAD_S: (f64, f64) = (1.5, 2.7);
const SHIFT_CHANCE: f64 = 0.35;
const SHIFT_BY: (f64, f64) = (0.9, 2.0);

/// How fast a shell flies, in cells a second, how likely one is to strike
/// home, and how wide of its mark a miss falls, in cells.
const SHELL_SPEED: f64 = 11.0;
const HIT_CHANCE: f64 = 0.38;
const MISS_BY: (f64, f64) = (0.9, 1.7);

/// A shell's glowing length and head, in cells.
const SHELL_LENGTH: f64 = 0.4;
const SHELL_HEAD: f64 = 0.1;

/// How far the barrel kicks back when it fires, in cells, how fast it runs
/// out again, and how long the muzzle flash lasts, in seconds.
const RECOIL: f64 = 0.1;
const RECOIL_RATE: f64 = 11.0;
const FLASH_S: f64 = 0.14;

/// How long a tank takes to come out of the haze, in seconds.
const FADE_IN_S: f64 = 1.0;

/// How far past the edge of sight a tank drives before it is gone, in cells.
const BEYOND: f64 = 3.0;

/// The fewest seconds between two decisions of one tank.
const LEAST_STEP: f64 = 0.05;

/// The light of a shell and of a muzzle flash.
static SHELL: Ink = Ink {
    edge: Rgb::new(255.0, 236.0, 170.0),
    face: Rgb::new(0.0, 0.0, 0.0),
};
const MUZZLE: Rgb = Rgb::new(255.0, 222.0, 150.0);

/// The deck's height: the top of the body, where the turret stands.
const DECK: f64 = 0.24;

/// The body's side profile, rear to front along its underside and back over
/// its top, and half its width.
const PROFILE: [(f64, f64); 6] = [
    (-0.52, 0.02),
    (0.44, 0.02),
    (0.62, 0.13),
    (0.46, DECK),
    (-0.5, DECK),
    (-0.6, 0.12),
];
const HALF_WIDTH: f64 = 0.36;

/// Where the turret sits on the body, in the body's own coordinates.
const TURRET_AT: Vec3 = Vec3::new(0.0, 0.0, -0.04);

/// The turret's footprint at its base, its base and top heights, how much its
/// top narrows, and about what it narrows.
const TURRET: [(f64, f64); 6] = [
    (-0.22, -0.28),
    (0.22, -0.28),
    (0.27, 0.02),
    (0.15, 0.2),
    (-0.15, 0.2),
    (-0.27, 0.02),
];
const TURRET_HEIGHTS: (f64, f64) = (DECK, 0.4);
const TURRET_TAPER: f64 = 0.8;
const TURRET_MIDDLE: f64 = -0.04;

/// The barrel's half width, its height span, its reach, and where its shells
/// leave it, all in the turret's own coordinates.
const BARREL_HALF: f64 = 0.035;
const BARREL_HEIGHTS: (f64, f64) = (0.29, 0.36);
const BARREL_REACH: (f64, f64) = (0.19, 0.86);
const MUZZLE_AT: Vec3 = Vec3::new(
    0.0,
    f64::midpoint(BARREL_HEIGHTS.0, BARREL_HEIGHTS.1),
    BARREL_REACH.1 + 0.02,
);

/// The radar on its mast above the turret's back, its dish's half width and
/// half height, and how fast it turns, in radians a second.
const MAST: [Vec3; 2] = [
    Vec3::new(0.0, TURRET_HEIGHTS.1, -0.17),
    Vec3::new(0.0, TURRET_HEIGHTS.1 + 0.17, -0.17),
];
const DISH: (f64, f64) = (0.075, 0.03);
const RADAR_SPIN: f64 = 2.2;

/// The body: a low, sloped hull on its tracks, road wheels drawn along both
/// sides.
pub(super) fn body() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for side in [-1.0, 1.0] {
        for (z, y) in PROFILE {
            let _ = corners.try_push(Vec3::new(side * HALF_WIDTH, y, z));
        }
    }
    let mut lines = Lines::new();
    for side in [-HALF_WIDTH, HALF_WIDTH] {
        for wheel in [-0.36, -0.12, 0.12, 0.36] {
            let spoke = |at: u8| {
                let turn = TAU * f64::from(at) / 6.0;
                Vec3::new(
                    side,
                    0.1 + 0.075 * mathf::sin(turn),
                    wheel + 0.075 * mathf::cos(turn),
                )
            };
            for at in 0..6u8 {
                let _ = lines.try_push([spoke(at), spoke(at + 1)]);
            }
        }
        let _ = lines.try_push([Vec3::new(side, 0.19, -0.54), Vec3::new(side, 0.19, 0.53)]);
    }
    (corners, lines)
}

/// The turret: a sloped six-sided block, narrower at its top.
pub(super) fn turret() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for (height, taper) in [(TURRET_HEIGHTS.0, 1.0), (TURRET_HEIGHTS.1, TURRET_TAPER)] {
        for (x, z) in TURRET {
            let _ = corners.try_push(Vec3::new(
                x * taper,
                height,
                TURRET_MIDDLE + (z - TURRET_MIDDLE) * taper,
            ));
        }
    }
    (corners, Lines::new())
}

/// The barrel: a long, square rod.
pub(super) fn barrel() -> (Corners, Lines) {
    let mut corners = Corners::new();
    for x in [-BARREL_HALF, BARREL_HALF] {
        for y in [BARREL_HEIGHTS.0, BARREL_HEIGHTS.1] {
            for z in [BARREL_REACH.0, BARREL_REACH.1] {
                let _ = corners.try_push(Vec3::new(x, y, z));
            }
        }
    }
    (corners, Lines::new())
}

/// Whether a camera at `seen`, in the turret's own coordinates, stands
/// ahead of the turret's front face, where the barrel is in front of it.
fn before_turret(seen: Vec3) -> bool {
    let (x0, z0) = TURRET[3];
    let (low, high) = TURRET_HEIGHTS;
    let base = Vec3::new(x0, low, z0);
    let top = Vec3::new(
        x0 * TURRET_TAPER,
        high,
        TURRET_MIDDLE + (z0 - TURRET_MIDDLE) * TURRET_TAPER,
    );
    let normal = (top - base).cross(Vec3::new(-1.0, 0.0, 0.0));
    normal.dot(seen - base) > 0.0
}

/// A course: a turn on the spot from `yaw.0` to `yaw.1` over the times
/// `turn`, then a drive from `from` to `to` over the times `drive`.
#[derive(Copy, Clone, Debug)]
struct Leg {
    from: Vec3,
    to: Vec3,
    yaw: (f64, f64),
    turn: (f64, f64),
    drive: (f64, f64),
}

impl Leg {
    /// Standing at `at` facing `yaw` from `time` on.
    const fn still(at: Vec3, yaw: f64, time: f64) -> Self {
        Self {
            from: at,
            to: at,
            yaw: (yaw, yaw),
            turn: (time, time),
            drive: (time, time),
        }
    }

    /// From where this course stands at `time`, turn to face `to` and drive
    /// there at `speed`.
    fn towards(&self, time: f64, to: Vec3, speed: f64) -> Self {
        let (from, yaw) = self.at(time);
        let heading = if (to - from).length() > 1e-6 {
            mathf::atan2(to.x - from.x, to.z - from.z)
        } else {
            yaw
        };
        let facing = yaw + wrap(heading - yaw);
        let turned = time + mathf::fabs(facing - yaw) / TURN;
        let arrives = turned + (to - from).length() / speed;
        Self {
            from,
            to,
            yaw: (yaw, facing),
            turn: (time, turned),
            drive: (turned, arrives),
        }
    }

    /// From where this course stands at `time`, turn on the spot to `yaw`.
    fn facing(&self, time: f64, yaw: f64) -> Self {
        let (at, now) = self.at(time);
        let facing = now + wrap(yaw - now);
        let turned = time + mathf::fabs(facing - now) / TURN;
        Self {
            from: at,
            to: at,
            yaw: (now, facing),
            turn: (time, turned),
            drive: (turned, turned),
        }
    }

    /// Where the tank stands at `time`, and which way it faces.
    fn at(&self, time: f64) -> (Vec3, f64) {
        let yaw = self.yaw.0 + (self.yaw.1 - self.yaw.0) * eased(time, self.turn);
        let at = self.from.lerp(self.to, eased(time, self.drive));
        (at, yaw)
    }

    const fn ends(&self) -> f64 {
        self.drive.1
    }
}

/// The turret's swing: from `from` to `to` radians off the hull's own
/// heading, over the times `span`.
#[derive(Copy, Clone, Debug)]
struct Sweep {
    from: f64,
    to: f64,
    span: (f64, f64),
}

impl Sweep {
    fn at(&self, time: f64) -> f64 {
        self.from + (self.to - self.from) * eased(time, self.span)
    }
}

/// What a tank is about.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Plan {
    /// Rolling in to its place.
    Arriving,
    /// About to swing its turret onto the nearest enemy.
    Aiming,
    /// Its aim held, about to fire.
    Firing,
    /// Reloading, perhaps moving.
    Reloading,
    /// Driving off out of sight.
    Leaving,
}

/// One tank.
#[derive(Copy, Clone, Debug)]
struct Tank {
    side: usize,
    ink: &'static Ink,
    alive: bool,
    gone: bool,
    leg: Leg,
    turret: Sweep,
    plan: Plan,
    /// When it decides what to do next.
    next: f64,
    /// Whom it aims at.
    target: usize,
    fired: Option<f64>,
    radar: f64,
}

impl Tank {
    /// Where it stands at `time`, and which way its hull and its turret face.
    fn at(&self, time: f64) -> (Vec3, f64, f64) {
        let (at, yaw) = self.leg.at(time);
        (at, yaw, self.turret.at(time))
    }

    /// Its body's, turret's and barrel's poses at `time`.
    fn poses(&self, time: f64) -> [Pose; 3] {
        let (at, yaw, turret) = self.at(time);
        let hull = Frame::turned(yaw, 0.0);
        let body = Pose::new(at, hull);
        let facing = Frame::turned(yaw + turret, 0.0);
        let turret = Pose::new(at + hull.to_world(TURRET_AT), facing);
        let kick = self.fired.map_or(0.0, |fired| {
            let since = time - fired;
            if since >= 0.0 {
                RECOIL * mathf::exp(-RECOIL_RATE * since)
            } else {
                0.0
            }
        });
        let barrel = Pose::new(turret.at - facing.z * kick, facing);
        [body, turret, barrel]
    }

    fn fighting(&self) -> bool {
        self.alive && !self.gone
    }
}

/// A shell in flight: where it left and where it lands, over the times
/// `span`, and whom it strikes, if it strikes home.
#[derive(Copy, Clone, Debug)]
struct Shell {
    from: Vec3,
    to: Vec3,
    span: (f64, f64),
    target: Option<usize>,
}

/// One battle.
#[derive(Debug)]
pub(super) struct Battle {
    rng: NonCryptoRng,
    tanks: ArrayVec<Tank, 4>,
    shells: ArrayVec<Shell, 8>,
    start: f64,
    /// When whoever is still fighting gives up and leaves.
    closing: f64,
    /// How wide the screen is against its height: how far across the flight
    /// sees at a given depth.
    aspect: f64,
}

impl Battle {
    /// A battle beginning at `start` on the grid ahead of `camera`, played
    /// from `rng`, closing once the flight going `speed` cells a second has
    /// crossed [`BATTLE_ROOM`] cells.
    pub(super) fn new(mut rng: NonCryptoRng, start: f64, camera: &Camera, speed: f64) -> Self {
        let view = camera.view();
        let aspect = f64::from(view.width) / f64::from(view.height);
        let ahead = camera.position().z + NEAREST + BATTLE_ROOM;
        let sizes: [u8; 2] = match pick(&mut rng, 20) {
            0..=7 => [1, 1],
            8..=11 => [2, 1],
            12..=15 => [1, 2],
            _ => [2, 2],
        };
        let mut tanks = ArrayVec::new();
        for (side, (&count, ink)) in sizes.iter().zip(&INKS).enumerate() {
            let across = if side == 0 { -1.0 } else { 1.0 };
            for slot in 0..count {
                let deep = PLACE_DEEP.0
                    + (PLACE_DEEP.1 - PLACE_DEEP.0) * (f64::from(slot) + rng.next_f64()) / 2.0;
                let place = Vec3::new(across * between(&mut rng, PLACE_ACROSS), 0.0, ahead + deep);
                let spawn = place
                    + Vec3::new(
                        across * 0.8 * rng.next_f64(),
                        0.0,
                        between(&mut rng, ROLL_IN),
                    );
                let setting_out = start + between(&mut rng, STAGGER);
                let leg = Leg::still(spawn, PI, setting_out).towards(setting_out, place, DRIVE);
                let _ = tanks.try_push(Tank {
                    side,
                    ink,
                    alive: true,
                    gone: false,
                    leg,
                    turret: Sweep {
                        from: 0.0,
                        to: 0.0,
                        span: (start, start),
                    },
                    plan: Plan::Arriving,
                    next: leg.ends(),
                    target: 0,
                    fired: None,
                    radar: rng.next_f64() * TAU,
                });
            }
        }
        Self {
            rng,
            tanks,
            shells: ArrayVec::new(),
            start,
            closing: start + BATTLE_ROOM / speed.max(1e-3),
            aspect,
        }
    }

    /// Whether the battle is over: every tank wrecked or gone, and no shell
    /// still in flight.
    pub(super) fn is_over(&self) -> bool {
        self.shells.is_empty() && self.tanks.iter().all(|tank| !tank.fighting())
    }

    /// Play the battle on to `time`: every decision and every shell's
    /// landing due by then, in the order they fall.
    pub(super) fn advance(
        &mut self,
        time: f64,
        camera: &Camera,
        models: &Models,
        blasts: &mut Blasts,
    ) {
        loop {
            let landing = self
                .shells
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.span.1.total_cmp(&b.1.span.1))
                .map(|(at, shell)| (at, shell.span.1));
            let deciding = self
                .tanks
                .iter()
                .enumerate()
                .filter(|(_, tank)| tank.fighting())
                .min_by(|a, b| a.1.next.total_cmp(&b.1.next))
                .map(|(at, tank)| (at, tank.next));
            match (landing, deciding) {
                (Some((shell, lands)), decision)
                    if lands <= time && decision.is_none_or(|(_, due)| lands <= due) =>
                {
                    self.land(shell, models, blasts);
                }
                (_, Some((tank, due))) if due <= time => self.decide(tank, due, camera),
                _ => break,
            }
        }
    }

    /// Land shell `index`: a tank blown apart if it strikes home on one still
    /// standing, or a burst on the ground.
    fn land(&mut self, index: usize, models: &Models, blasts: &mut Blasts) {
        let Some(shell) = self.shells.remove(index) else {
            return;
        };
        let lands = shell.span.1;
        let struck = shell
            .target
            .and_then(|target| self.tanks.get_mut(target).filter(|tank| tank.fighting()));
        let Some(tank) = struck else {
            blasts.spawn(
                Burst {
                    at: Vec3::new(shell.to.x, 0.0, shell.to.z),
                    start: lands,
                    size: Size::Strike,
                    ink: &SHELL,
                },
                &[],
                models,
                &mut self.rng,
            );
            return;
        };
        tank.alive = false;
        let [body, turret, barrel] = tank.poses(lands);
        let ink = tank.ink;
        let mut pieces: ArrayVec<Piece, 12> = ArrayVec::new();
        let faces = models.get(HullId::TankBody).map_or(0, Hull::face_count);
        for face in 0..faces.min(10) {
            let _ = pieces.try_push(Piece {
                hull: HullId::TankBody,
                face: u8::try_from(face).ok(),
                pose: body,
            });
        }
        for (hull, pose) in [(HullId::TankTurret, turret), (HullId::TankBarrel, barrel)] {
            let _ = pieces.try_push(Piece {
                hull,
                face: None,
                pose,
            });
        }
        blasts.spawn(
            Burst {
                at: body.at,
                start: lands,
                size: Size::Wreck,
                ink,
            },
            &pieces,
            models,
            &mut self.rng,
        );
    }

    /// Tank `index` decides, at `due`, what to do next.
    fn decide(&mut self, index: usize, due: f64, camera: &Camera) {
        let Some(me) = self.tanks.get(index).copied() else {
            return;
        };
        let (at, yaw, turret) = me.at(due);
        let nearest = self
            .tanks
            .iter()
            .enumerate()
            .filter(|(_, tank)| tank.fighting() && tank.side != me.side)
            .map(|(other, tank)| (other, (tank.leg.at(due).0 - at).length()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(other, _)| other);
        let mut tank = me;
        match (tank.plan, nearest) {
            (Plan::Leaving, _) => tank.gone = true,
            (_, None) => self.leave(&mut tank, due, camera),
            _ if due >= self.closing => self.leave(&mut tank, due, camera),
            (Plan::Arriving, Some(_)) => {
                let facing = if tank.side == 0 {
                    FRAC_PI_2
                } else {
                    -FRAC_PI_2
                };
                tank.leg = tank.leg.facing(due, facing);
                tank.plan = Plan::Aiming;
                tank.next = tank.leg.ends();
            }
            (Plan::Aiming, Some(target)) => {
                let aim = self
                    .tanks
                    .get(target)
                    .map_or(at, |enemy| enemy.leg.at(due).0);
                let bearing = mathf::atan2(aim.x - at.x, aim.z - at.z) - yaw;
                let to = turret + wrap(bearing - turret);
                let swung = due + mathf::fabs(to - turret) / SLEW;
                tank.turret = Sweep {
                    from: turret,
                    to,
                    span: (due, swung),
                };
                tank.target = target;
                tank.plan = Plan::Firing;
                tank.next = swung + SETTLE_S;
            }
            (Plan::Firing, Some(_)) => {
                self.fire(&mut tank, due);
                tank.plan = Plan::Reloading;
                tank.next = due + between(&mut self.rng, RELOAD_S);
            }
            (Plan::Reloading, Some(_)) => {
                tank.plan = Plan::Aiming;
                tank.next = due;
                if self.rng.next_f64() < SHIFT_CHANCE {
                    let across = if tank.side == 0 { -1.0 } else { 1.0 };
                    let step = between(&mut self.rng, SHIFT_BY);
                    let turn = self.rng.next_f64() * TAU;
                    let mut to = at + Vec3::new(mathf::cos(turn), 0.0, mathf::sin(turn)) * step;
                    to.x = across * mathf::clamp(across * to.x, PLACE_ACROSS.0, PLACE_ACROSS.1);
                    tank.leg = tank.leg.towards(due, to, SHIFT);
                    tank.next = tank.leg.ends();
                }
            }
        }
        tank.next = tank.next.max(due + LEAST_STEP);
        if let Some(held) = self.tanks.get_mut(index) {
            *held = tank;
        }
    }

    /// Fire `tank`'s gun at its target at `time`: struck home, or wide of it.
    fn fire(&mut self, tank: &mut Tank, time: f64) {
        let [_, _, barrel] = tank.poses(time);
        let from = barrel.point_to_world(MUZZLE_AT);
        let Some(enemy) = self.tanks.get(tank.target).copied() else {
            return;
        };
        let hits = self.rng.next_f64() < HIT_CHANCE;
        // Led by where the enemy will be once the shell has flown there.
        let mut to = enemy.leg.at(time).0;
        for _ in 0..2 {
            let lands = time + (to - from).length() / SHELL_SPEED;
            to = enemy.leg.at(lands).0;
        }
        let to = if hits {
            to + Vec3::UP * 0.18
        } else {
            let turn = self.rng.next_f64() * TAU;
            let wide = between(&mut self.rng, MISS_BY);
            to + Vec3::new(mathf::cos(turn), 0.0, mathf::sin(turn)) * wide
        };
        let flight = (to - from).length() / SHELL_SPEED;
        if self
            .shells
            .try_push(Shell {
                from,
                to,
                span: (time, time + flight),
                target: hits.then_some(tank.target),
            })
            .is_ok()
        {
            tank.fired = Some(time);
        }
    }

    /// Send `tank` off out of sight from where it stands at `time`: away from
    /// the flight's path, past the edge of what `camera` sees.
    fn leave(&self, tank: &mut Tank, time: f64, camera: &Camera) {
        let (at, _) = tank.leg.at(time);
        let depth = (at.z - camera.position().z).max(1.0);
        let across = if at.x < camera.position().x {
            -1.0
        } else {
            1.0
        };
        let off = camera.position().x + across * (depth * self.aspect / 2.0 + BEYOND);
        tank.leg = tank.leg.towards(time, Vec3::new(off, 0.0, at.z), LEAVE);
        tank.plan = Plan::Leaving;
        tank.next = tank.leg.ends();
    }

    /// Set the battle out as it stands at `time`.
    pub(super) fn stage(&self, time: f64, stage: &mut Stage) {
        let camera = stage.camera().position();
        let alpha = mathf::smoothstep((time - self.start) / FADE_IN_S);
        for tank in self.tanks.iter().filter(|tank| tank.fighting()) {
            let [body, turret, barrel] = tank.poses(time);
            let ink = tank.ink;
            let solid = |hull, pose| Part::Solid {
                hull,
                pose,
                ink,
                alpha,
            };
            stage.begin(body.at);
            // The camera flies above every deck, so the body never hides the
            // turret standing on it.
            stage.push(solid(HullId::TankBody, body));
            let (first, second) = if before_turret(turret.point_to_local(camera)) {
                (
                    solid(HullId::TankTurret, turret),
                    solid(HullId::TankBarrel, barrel),
                )
            } else {
                (
                    solid(HullId::TankBarrel, barrel),
                    solid(HullId::TankTurret, turret),
                )
            };
            stage.push(first);
            stage.push(second);
            let dish = Frame::turned(tank.radar + RADAR_SPIN * time, 0.0);
            let top = turret.point_to_world(MAST[1]);
            let corner =
                |x: f64, y: f64| top + turret.frame.to_world(dish.to_world(Vec3::new(x, y, 0.0)));
            let (w, h) = DISH;
            stage.push(Part::Line {
                ends: [turret.point_to_world(MAST[0]), top],
                ink,
                alpha,
            });
            for (from, to) in [
                ((-w, -h), (w, -h)),
                ((w, -h), (w, h)),
                ((w, h), (-w, h)),
                ((-w, h), (-w, -h)),
            ] {
                stage.push(Part::Line {
                    ends: [corner(from.0, from.1), corner(to.0, to.1)],
                    ink,
                    alpha,
                });
            }
            if let Some(fired) = tank
                .fired
                .filter(|fired| (0.0..FLASH_S).contains(&(time - fired)))
            {
                let muzzle = barrel.point_to_world(MUZZLE_AT);
                stage.push(Part::Glow {
                    base: muzzle,
                    tip: muzzle + barrel.frame.z * 0.25,
                    radius: 0.14,
                    light: MUZZLE,
                    alpha: 1.0 - (time - fired) / FLASH_S,
                });
            }
        }
        for shell in &self.shells {
            let flown = progress(time, shell.span);
            let head = shell.from.lerp(shell.to, flown);
            let heading = (shell.to - shell.from).normalized();
            let tail = head - heading * SHELL_LENGTH.min((head - shell.from).length());
            stage.begin(head);
            stage.push(Part::Line {
                ends: [tail, head],
                ink: &SHELL,
                alpha: 1.0,
            });
            stage.push(Part::Glow {
                base: head,
                tip: head,
                radius: SHELL_HEAD,
                light: SHELL.edge,
                alpha: 1.0,
            });
        }
    }
}

/// How far through `span` `time` stands, eased in and out, held to `0..=1`.
fn eased(time: f64, span: (f64, f64)) -> f64 {
    mathf::smoothstep(progress(time, span))
}

/// How far through `span` `time` stands, held to `0..=1`; a span with no
/// length is wholly past once it begins.
fn progress(time: f64, (from, to): (f64, f64)) -> f64 {
    if to <= from {
        return if time >= from { 1.0 } else { 0.0 };
    }
    mathf::clamp((time - from) / (to - from), 0.0, 1.0)
}

#[cfg(test)]
#[path = "tanks_tests.rs"]
mod tests;
