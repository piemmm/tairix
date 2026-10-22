//! The riders: two or three wireframe motorbikes race out of the haze onto an
//! arena marked out on the grid ahead, each laying a glowing lattice fence
//! behind it along the grid's lines and turning only where the lines cross.
//! A rider who runs into a fence, its own or another's, or out of the arena
//! goes up in a blast; the last one riding turns and rides off out of sight
//! as the fences sink into the floor.
//!
//! The riders move in step, a crossing at a time, so the arena is a board of
//! crossings each claimed once, held as one bit apiece. Each chooses its way
//! at every crossing by weighing the room it would be left, how near it
//! would come to cutting a rival off, and how likely the two are to meet head
//! on. Between crossings a rider stands exactly where its course says, so
//! the duel is the same however its frames fall.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_inline::ArrayVec;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;
use tairix_util::space::{Frame, Pose, Vec3};

use super::blast::{Blasts, Burst, Piece, Size};
use super::wire::{Camera, Corners, Hull, HullId, Ink, Lines, Models, Part, Stage};
use super::{between, pick, wrap, Rgb};

/// The riders' colours.
static INKS: [Ink; 3] = [
    Ink {
        edge: Rgb::new(255.0, 70.0, 196.0),
        face: Rgb::new(24.0, 4.0, 18.0),
    },
    Ink {
        edge: Rgb::new(186.0, 255.0, 66.0),
        face: Rgb::new(14.0, 22.0, 4.0),
    },
    Ink {
        edge: Rgb::new(255.0, 228.0, 120.0),
        face: Rgb::new(24.0, 20.0, 6.0),
    },
];

/// The arena's border.
static BORDER: Ink = Ink {
    edge: Rgb::new(190.0, 214.0, 255.0),
    face: Rgb::new(0.0, 0.0, 0.0),
};

/// The arena's crossings across and deep; they must fit the board's bits.
const COLUMNS: i32 = 12;
const ROWS: i32 = 7;

/// How fast a rider rides, in cells a second, and how many rows beyond the
/// arena it rides in from.
const RIDE: f64 = 4.2;
const ENTRY_ROWS: i32 = 4;

/// The longest a rider still riding once the duel is decided takes to ride
/// out of sight, in seconds.
const LEAVE_S: f64 = 8.0;

/// How far ahead the arena's near edge stands, in cells, once the flight has
/// flown on while the longest race is run: just short of the nearest floor in
/// sight.
const NEAREST: f64 = 2.6;

/// How far along the way to a crossing a rider has come when it strikes a
/// fence there, strikes the rival riding at it, or meets it head on.
const STRIKE_AT: f64 = 0.8;
const CLASH_AT: f64 = 0.92;
const HEAD_ON_AT: f64 = 0.5;

/// The fence: its height, its rails' heights, its lattice's pitch, and how
/// long it takes to sink away once the duel is decided.
const FENCE: f64 = 0.26;
const RAIL: f64 = 0.015;
const PITCH: f64 = 0.5;
const SINK_S: f64 = 1.6;

/// How bright the arena's border burns, and how long it takes to light.
const BORDER_ALPHA: f64 = 0.45;
const LIGHT_S: f64 = 0.6;

/// How long a rider carves a turn over, as a share of a crossing's time, and
/// how far it leans into it, in radians.
const CARVE: f64 = 0.35;
const LEAN: f64 = 0.45;

/// How much a rider values the room it keeps, closing on a rival, and riding
/// straight on; how much it fears riding at a crossing a rival may take
/// next, spread over every one that rival may take; and how much it is
/// swayed by whim, which keeps two riders from mirroring one another.
const ROOM: f64 = 1.0;
const CHASE: f64 = 0.45;
const STRAIGHT: f64 = 0.7;
const CLASH: f64 = 12.0;
const WHIM: f64 = 2.4;

/// How far past the screen's edge a rider leaving is out of sight, in pixels.
const SLACK: f64 = 80.0;

/// The most crossings one fence passes: as many as the board has bits, which
/// every crossing of the arena must fit.
const MAX_POSTS: usize = 128;
const _: () = assert!(
    COLUMNS * ROWS <= 128,
    "the arena's crossings fit the board's bits"
);

/// The seat's height: the top of the bike's body, where the rider sits.
const SEAT: f64 = 0.37;

/// The bike's side profile, rear to front along its underside and back over
/// its top, and half its width; the rider's, and half theirs.
const BODY: [(f64, f64); 6] = [
    (-0.2, 0.14),
    (0.14, 0.14),
    (0.16, 0.3),
    (0.06, SEAT),
    (-0.14, SEAT),
    (-0.2, 0.27),
];
const BODY_HALF: f64 = 0.075;
const RIDER: [(f64, f64); 6] = [
    (-0.13, SEAT),
    (0.04, SEAT),
    (0.14, 0.5),
    (0.12, 0.58),
    (0.02, 0.61),
    (-0.1, 0.5),
];
const RIDER_HALF: f64 = 0.065;

/// The wheels: their hubs along the bike, their radius and so their hubs'
/// height over the floor they stand on, their sides and spokes; and the
/// planes beyond which each wheel stands clear of the body.
const HUBS: [f64; 2] = [0.34, -0.34];
const WHEEL: f64 = 0.14;
const HUB_HEIGHT: f64 = WHEEL;
const WHEEL_SIDES: u8 = 12;
const SPOKES: u8 = 5;
const FRONT_CLEAR: f64 = 0.17;
const REAR_CLEAR: f64 = -0.2;

/// The fork's and the swingarm's reach, and the handlebar's.
const FORK_TOP: (f64, f64) = (0.19, 0.43);
const FORK_HALF: f64 = 0.045;
const ARM_END: (f64, f64) = (-0.21, 0.2);
const ARM_HALF: f64 = 0.05;
const BAR_HALF: f64 = 0.13;

/// The bike's body: tank, engine and seat in one wedge.
pub(super) fn body() -> (Corners, Lines) {
    (prism(&BODY, BODY_HALF), Lines::new())
}

/// The rider, crouched over the tank.
pub(super) fn rider() -> (Corners, Lines) {
    (prism(&RIDER, RIDER_HALF), Lines::new())
}

/// The corners of `profile`, given as `(along, up)`, set `half` either side.
fn prism(profile: &[(f64, f64)], half: f64) -> Corners {
    let mut corners = Corners::new();
    for side in [-half, half] {
        for &(z, y) in profile {
            let _ = corners.try_push(Vec3::new(side, y, z));
        }
    }
    corners
}

/// A way along the grid's lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Way {
    Away,
    Right,
    Toward,
    Left,
}

impl Way {
    const fn step(self) -> (i32, i32) {
        match self {
            Self::Away => (0, 1),
            Self::Right => (1, 0),
            Self::Toward => (0, -1),
            Self::Left => (-1, 0),
        }
    }

    /// The heading a bike riding this way faces.
    const fn yaw(self) -> f64 {
        match self {
            Self::Away => 0.0,
            Self::Right => FRAC_PI_2,
            Self::Toward => PI,
            Self::Left => -FRAC_PI_2,
        }
    }

    /// The way a quarter turn to the right, and to the left.
    const fn right(self) -> Self {
        match self {
            Self::Away => Self::Right,
            Self::Right => Self::Toward,
            Self::Toward => Self::Left,
            Self::Left => Self::Away,
        }
    }

    const fn left(self) -> Self {
        self.right().right().right()
    }
}

/// A crossing of the arena, counted from its near left corner; one outside it
/// is where a rider rides in or out.
type Node = (i32, i32);

/// A crossing of the arena as its fences keep it, in two bytes: a fence lies
/// wholly within the arena, so every crossing it passes fits.
type Post = (i8, i8);

/// Crossing `node` as a fence keeps it, or `None` for one outside the arena.
fn post(node: Node) -> Option<Post> {
    bit(node)?;
    Some((i8::try_from(node.0).ok()?, i8::try_from(node.1).ok()?))
}

/// The crossing a fence keeps as `post`.
fn node((column, row): Post) -> Node {
    (i32::from(column), i32::from(row))
}

/// How a rider is faring.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Fate {
    Riding,
    /// Due to strike something at this time.
    Doomed(f64),
    /// Blown apart at this time, where it stood.
    Wrecked(f64),
    Gone,
}

/// One rider.
#[derive(Clone, Debug)]
struct Rider {
    ink: &'static Ink,
    /// The crossing it last reached, and the way it rides on from there.
    node: Node,
    way: Way,
    /// The way it rode before it last turned there.
    came: Way,
    fate: Fate,
    /// Whether it lays its fence, and the crossings that fence has passed; the
    /// fence runs on from the last of them to the rider while it lays one.
    laying: bool,
    fence: ArrayVec<Post, MAX_POSTS>,
    /// Where its fence ends once it stops laying.
    fence_end: Option<(f64, f64)>,
    /// How keenly it closes on a rival rather than keeping room.
    boldness: f64,
}

/// One duel.
#[derive(Debug)]
pub(super) struct Duel {
    rng: NonCryptoRng,
    riders: ArrayVec<Rider, 3>,
    /// Where the arena's near left crossing lies, across and ahead.
    origin: (f64, f64),
    /// The crossings claimed, a bit apiece.
    board: u128,
    start: f64,
    /// When the riders last reached a crossing.
    clock: f64,
    /// When the arena lit, and when the duel was decided.
    lit: Option<f64>,
    decided: Option<f64>,
}

/// Every crossing of the arena, a bit apiece.
const EVERY: u128 = (1u128 << (COLUMNS * ROWS)) - 1;

/// The crossings at the arena's left and right edges.
const LEFT_EDGE: u128 = column_mask(0);
const RIGHT_EDGE: u128 = column_mask(COLUMNS - 1);

/// Every crossing in column `column`.
const fn column_mask(column: i32) -> u128 {
    let mut mask: u128 = 0;
    let mut row = 0;
    while row < ROWS {
        mask |= 1 << (row * COLUMNS + column);
        row += 1;
    }
    mask
}

/// The crossing next to `node` the way `way` leads.
const fn ahead((column, row): Node, way: Way) -> Node {
    let (dx, dy) = way.step();
    (column + dx, row + dy)
}

/// Where crossing `(column, row)` of an arena whose near left crossing lies
/// at `origin` lies on the floor, across and ahead.
fn place(origin: (f64, f64), (column, row): Node) -> (f64, f64) {
    (origin.0 + f64::from(column), origin.1 + f64::from(row))
}

/// Crossing `node`'s bit, or `None` outside the arena.
fn bit((column, row): Node) -> Option<u128> {
    ((0..COLUMNS).contains(&column) && (0..ROWS).contains(&row))
        .then(|| 1 << (row * COLUMNS + column))
}

/// How many free crossings can be reached from `from`, over the arena's
/// crossings `free` holds: grown a step in all four ways at a time.
fn room(from: u128, free: u128) -> u32 {
    let mut reached = from & free;
    loop {
        let grown = reached
            | ((reached & !RIGHT_EDGE) << 1)
            | ((reached & !LEFT_EDGE) >> 1)
            | (reached << COLUMNS)
            | (reached >> COLUMNS);
        let grown = grown & free;
        if grown == reached {
            return reached.count_ones();
        }
        reached = grown;
    }
}

impl Duel {
    /// A duel beginning at `start` on the grid ahead of `camera`, played from
    /// `rng`, set far enough ahead for the flight going `speed` cells a
    /// second to be short of the arena when the longest race is run.
    pub(super) fn new(mut rng: NonCryptoRng, start: f64, camera: &Camera, speed: f64) -> Self {
        let near = mathf::floor(camera.position().z + NEAREST + speed * Self::longest());
        let origin = (-f64::from(COLUMNS - 1) / 2.0, near);
        let count = if rng.next_f64() < 0.3 { 3 } else { 2 };
        let lanes: &[(i32, i32)] = if count == 3 {
            &[(1, 3), (4, 7), (8, 10)]
        } else {
            &[(1, 4), (7, 10)]
        };
        let mut riders = ArrayVec::new();
        let mut behind: i32 = 0;
        for (index, &(least, most)) in lanes.iter().enumerate() {
            let spread = usize::try_from(most - least + 1).unwrap_or(1);
            let column = least + i32::try_from(pick(&mut rng, spread)).unwrap_or(0);
            // Each rides in a crossing or two behind the last, so no two
            // reach the arena, or anything in it, in the same step.
            let _ = riders.try_push(Rider {
                ink: &INKS[index % INKS.len()],
                node: (column, ROWS - 1 + ENTRY_ROWS + behind),
                way: Way::Toward,
                came: Way::Toward,
                fate: Fate::Riding,
                laying: false,
                fence: ArrayVec::new(),
                fence_end: None,
                boldness: between(&mut rng, (0.3, 1.0)),
            });
            behind += 1 + i32::try_from(pick(&mut rng, 2)).unwrap_or(0);
        }
        Self {
            rng,
            riders,
            origin,
            board: 0,
            start,
            clock: start,
            lit: None,
            decided: None,
        }
    }

    /// How long one crossing takes.
    const fn step() -> f64 {
        1.0 / RIDE
    }

    /// About the longest the riders race in the arena, in seconds: two of
    /// them claiming a crossing apiece at every step until none is left.
    fn longest() -> f64 {
        f64::from(COLUMNS * ROWS) / 2.0 * Self::step()
    }

    /// Whether the duel is over by `time`: nobody riding, and every fence
    /// sunk.
    pub(super) fn is_over(&self, time: f64) -> bool {
        self.decided.is_some_and(|decided| time >= decided + SINK_S)
            && self
                .riders
                .iter()
                .all(|rider| matches!(rider.fate, Fate::Wrecked(_) | Fate::Gone))
    }

    /// Where crossing `node` lies on the floor, across and ahead.
    fn place(&self, node: Node) -> (f64, f64) {
        place(self.origin, node)
    }

    /// Play the duel on to `time`: every crossing reached and every crash due
    /// by then, in the order they fall.
    pub(super) fn advance(
        &mut self,
        time: f64,
        camera: &Camera,
        models: &Models,
        blasts: &mut Blasts,
    ) {
        loop {
            let arrives = self.clock + Self::step();
            let crash = self
                .riders
                .iter()
                .enumerate()
                .filter_map(|(at, rider)| match rider.fate {
                    Fate::Doomed(when) => Some((at, when)),
                    _ => None,
                })
                .min_by(|a, b| a.1.total_cmp(&b.1));
            match crash {
                Some((rider, when)) if when <= time && when <= arrives => {
                    self.crash(rider, when, models, blasts);
                }
                _ if arrives <= time
                    && self.riders.iter().any(|rider| rider.fate == Fate::Riding) =>
                {
                    self.arrive(arrives, camera);
                }
                _ => break,
            }
        }
    }

    /// Blow rider `index` apart where it stands at `when`.
    fn crash(&mut self, index: usize, when: f64, models: &Models, blasts: &mut Blasts) {
        let Some((pose, _)) = self.riders.get(index).map(|rider| self.pose(rider, when)) else {
            return;
        };
        let Some(rider) = self.riders.get_mut(index) else {
            return;
        };
        rider.fate = Fate::Wrecked(when);
        if rider.laying {
            rider.laying = false;
            rider.fence_end = Some((pose.at.x, pose.at.z));
        }
        let ink = rider.ink;
        let mut pieces: ArrayVec<Piece, 12> = ArrayVec::new();
        let faces = models.get(HullId::BikeBody).map_or(0, Hull::face_count);
        for face in 0..faces.min(8) {
            let _ = pieces.try_push(Piece {
                hull: HullId::BikeBody,
                face: u8::try_from(face).ok(),
                pose,
            });
        }
        let _ = pieces.try_push(Piece {
            hull: HullId::BikeRider,
            face: None,
            pose,
        });
        blasts.spawn(
            Burst {
                at: Vec3::new(pose.at.x, 0.0, pose.at.z),
                start: when,
                size: Size::Wreck,
                ink,
            },
            &pieces,
            models,
            &mut self.rng,
        );
        self.settle(when);
    }

    /// Decide the duel at `when` once nobody is left riding, or one rider is
    /// and nobody is still riding in: the fences begin to sink, and the one
    /// left rides off.
    fn settle(&mut self, when: f64) {
        if self.decided.is_some() {
            return;
        }
        let racing = self
            .riders
            .iter()
            .filter(|rider| matches!(rider.fate, Fate::Riding | Fate::Doomed(_)))
            .count();
        let entering = self
            .riders
            .iter()
            .any(|rider| rider.fate == Fate::Riding && !rider.laying);
        if racing == 0 || (racing == 1 && !entering) {
            self.decided = Some(when);
            let origin = self.origin;
            for rider in &mut self.riders {
                if rider.laying {
                    rider.laying = false;
                    rider.fence_end = Some(place(origin, rider.node));
                }
            }
        }
    }

    /// Every rider still riding reaches its next crossing at `when`, and
    /// chooses its way on.
    fn arrive(&mut self, when: f64, camera: &Camera) {
        self.clock = when;
        for rider in &mut self.riders {
            if rider.fate != Fate::Riding {
                continue;
            }
            let (dx, dy) = rider.way.step();
            rider.node = (rider.node.0 + dx, rider.node.1 + dy);
            rider.came = rider.way;
            if self.decided.is_none() {
                if let Some(claim) = bit(rider.node) {
                    if !rider.laying && rider.fence.is_empty() {
                        rider.laying = true;
                    }
                    self.board |= claim;
                }
                if let Some(post) = rider.laying.then(|| post(rider.node)).flatten() {
                    if rider.fence.last() != Some(&post) {
                        let _ = rider.fence.try_push(post);
                    }
                }
            }
        }
        let in_arena = self
            .riders
            .iter()
            .filter(|rider| rider.fate == Fate::Riding)
            .all(|rider| rider.laying);
        if self.lit.is_none() && in_arena {
            self.lit = Some(when);
        }
        self.settle(when);
        if let Some(decided) = self.decided {
            self.ride_off(when, decided, camera);
            return;
        }
        self.choose(when);
    }

    /// Choose every riding rider's way on from the crossing it has reached,
    /// and doom any that cannot avoid a fence, the arena's edge or a rival.
    fn choose(&mut self, when: f64) {
        let mut heads: ArrayVec<(Node, Way, bool), 3> = ArrayVec::new();
        for rider in &self.riders {
            let _ = heads.try_push((rider.node, rider.way, rider.fate == Fate::Riding));
        }
        let free = EVERY & !self.board;
        let mut ways: ArrayVec<Option<Way>, 3> = ArrayVec::new();
        for (index, rider) in self.riders.iter().enumerate() {
            if rider.fate != Fate::Riding || !rider.laying {
                let _ = ways.try_push(None);
                continue;
            }
            let rivals = heads
                .iter()
                .enumerate()
                .filter(|(other, head)| *other != index && head.2)
                .map(|(_, head)| *head);
            let mut best: Option<(f64, Way)> = None;
            for way in [rider.way, rider.way.left(), rider.way.right()] {
                let to = ahead(rider.node, way);
                let Some(claim) = bit(to).filter(|claim| free & claim != 0) else {
                    continue;
                };
                let mut score = ROOM * f64::from(room(claim, free));
                for (head, heading, _) in rivals.clone() {
                    let straight = ahead(head, heading);
                    let apart = f64::from((to.0 - straight.0).abs() + (to.1 - straight.1).abs());
                    score -= CHASE * rider.boldness * apart;
                    let open = [heading, heading.left(), heading.right()]
                        .map(|turn| ahead(head, turn))
                        .into_iter()
                        .filter(|next| bit(*next).is_some_and(|claim| free & claim != 0));
                    let (mut count, mut shared) = (0u8, false);
                    for next in open {
                        count += 1;
                        shared |= next == to;
                    }
                    if shared {
                        score -= CLASH / f64::from(count);
                    }
                }
                if way == rider.way {
                    score += STRAIGHT;
                }
                score += WHIM * self.rng.next_f64();
                if best.is_none_or(|(held, _)| score > held) {
                    best = Some((score, way));
                }
            }
            let _ = ways.try_push(Some(best.map_or(rider.way, |(_, way)| way)));
        }
        for (rider, way) in self.riders.iter_mut().zip(&ways) {
            if let Some(way) = way {
                rider.way = *way;
            }
        }
        self.doom(when, free);
    }

    /// Doom every rider about to ride into a claimed crossing, off the
    /// arena, into the crossing a rival rides at, or head on into one; a
    /// rider still riding in is in play once the crossing it rides at is the
    /// arena's.
    fn doom(&mut self, when: f64, free: u128) {
        let step = Self::step();
        let mut heads: ArrayVec<(Node, Node, bool), 3> = ArrayVec::new();
        for rider in &self.riders {
            let to = ahead(rider.node, rider.way);
            let in_play = rider.laying || bit(to).is_some();
            let _ = heads.try_push((rider.node, to, rider.fate == Fate::Riding && in_play));
        }
        for (index, rider) in self.riders.iter_mut().enumerate() {
            let Some(&(from, to, racing)) = heads.get(index) else {
                continue;
            };
            if !racing {
                continue;
            }
            let rivals = heads
                .iter()
                .enumerate()
                .filter(|(other, head)| *other != index && head.2);
            let mut strike: Option<f64> = None;
            if bit(to).is_none_or(|claim| free & claim == 0) {
                strike = Some(STRIKE_AT);
            }
            for (_, &(rival_from, rival_to, _)) in rivals {
                if rival_to == from && rival_from == to {
                    strike = Some(strike.map_or(HEAD_ON_AT, |held| held.min(HEAD_ON_AT)));
                } else if rival_to == to {
                    strike = Some(strike.map_or(CLASH_AT, |held| held.min(CLASH_AT)));
                }
            }
            if let Some(share) = strike {
                rider.fate = Fate::Doomed(when + step * share);
            }
        }
    }

    /// Send whoever still rides once the duel is decided off out of sight:
    /// turned across the grid towards the arena's nearer side, and gone once
    /// past the edge of sight.
    fn ride_off(&mut self, when: f64, decided: f64, camera: &Camera) {
        let middle = f64::from(COLUMNS - 1) / 2.0;
        let origin = self.origin;
        for rider in &mut self.riders {
            if rider.fate != Fate::Riding {
                continue;
            }
            let (x, z) = place(origin, rider.node);
            let at = Vec3::new(x, 0.0, z);
            if matches!(rider.way, Way::Away | Way::Toward) {
                rider.way = if f64::from(rider.node.0) < middle {
                    Way::Left
                } else {
                    Way::Right
                };
            }
            let unseen = !camera.sees(at, SLACK) && bit(rider.node).is_none();
            if (when > decided + Self::step() && unseen) || when > decided + LEAVE_S {
                rider.fate = Fate::Gone;
            }
        }
    }

    /// Where `rider` stands at `time` and how it is turned and leaning.
    fn pose(&self, rider: &Rider, time: f64) -> (Pose, f64) {
        let stopped = match rider.fate {
            Fate::Wrecked(when) => when,
            _ => time,
        };
        let since = mathf::clamp((stopped - self.clock) / Self::step(), 0.0, 1.0);
        let (x, z) = self.place(rider.node);
        let (dx, dy) = rider.way.step();
        let at = Vec3::new(x + f64::from(dx) * since, 0.0, z + f64::from(dy) * since);
        let turning = rider.way != rider.came;
        let carve = if turning {
            mathf::smoothstep(since / CARVE)
        } else {
            1.0
        };
        let from = rider.came.yaw();
        let yaw = from + wrap(rider.way.yaw() - from) * carve;
        let lean = if turning {
            let sense = if rider.way == rider.came.right() {
                -1.0
            } else {
                1.0
            };
            sense * LEAN * mathf::sin(PI * carve)
        } else {
            0.0
        };
        let heading = Frame::turned(yaw, 0.0);
        let frame = heading.rotated_by(Frame::about(heading.z, lean));
        (Pose::new(at, frame), since)
    }

    /// Set the duel out as it stands at `time`.
    pub(super) fn stage(&self, time: f64, stage: &mut Stage) {
        let sunk = self
            .decided
            .map_or(0.0, |decided| mathf::smoothstep((time - decided) / SINK_S));
        if let Some(lit) = self.lit {
            let alpha = BORDER_ALPHA * mathf::smoothstep((time - lit) / LIGHT_S) * (1.0 - sunk);
            if alpha > 0.0 {
                self.border(alpha, stage);
            }
        }
        for rider in &self.riders {
            if sunk < 1.0 {
                self.fence(rider, time, sunk, stage);
            }
            if matches!(rider.fate, Fate::Riding | Fate::Doomed(_)) {
                self.bike(rider, time, stage);
            }
        }
    }

    /// The arena's border, a cell at a time so each piece takes its own place
    /// among what stands about it.
    fn border(&self, alpha: f64, stage: &mut Stage) {
        let (left, near) = self.place((0, 0));
        let (right, far) = self.place((COLUMNS - 1, ROWS - 1));
        let (left, right, near, far) = (left - 0.5, right + 0.5, near - 0.5, far + 0.5);
        let corners = [(left, near), (right, near), (right, far), (left, far)];
        for (at, &(x0, z0)) in corners.iter().enumerate() {
            let (x1, z1) = corners[(at + 1) % corners.len()];
            let length = mathf::hypot(x1 - x0, z1 - z0);
            let pieces = mathf::round_i32(mathf::ceil(length)).max(1);
            for piece in 0..pieces {
                let share = |at: i32| f64::from(at) / f64::from(pieces);
                let point = |t: f64| Vec3::new(x0 + (x1 - x0) * t, 0.012, z0 + (z1 - z0) * t);
                let (from, to) = (point(share(piece)), point(share(piece + 1)));
                stage.begin(from.lerp(to, 0.5));
                stage.push(Part::Line {
                    ends: [from, to],
                    ink: &BORDER,
                    alpha,
                });
            }
        }
    }

    /// `rider`'s fence as it stands at `time`, sunk `sunk` of the way into
    /// the floor: two rails and the lattice between them, a pitch at a time.
    fn fence(&self, rider: &Rider, time: f64, sunk: f64, stage: &mut Stage) {
        let Some(&first) = rider.fence.first() else {
            return;
        };
        let first = node(first);
        let end = rider.fence_end.or_else(|| {
            rider.laying.then(|| {
                let (pose, _) = self.pose(rider, time);
                (pose.at.x, pose.at.z)
            })
        });
        let height = FENCE * (1.0 - sunk);
        let alpha = 1.0 - sunk;
        let mut from = self.place(first);
        let corners = rider
            .fence
            .iter()
            .skip(1)
            .map(|post| self.place(node(*post)))
            .chain(end);
        let (low, high) = (RAIL, RAIL + height);
        let line = |ends| Part::Line {
            ends,
            ink: rider.ink,
            alpha,
        };
        let mut run = 0.0;
        for to in corners {
            let length = mathf::hypot(to.0 - from.0, to.1 - from.1);
            let point = |share: f64, up: f64| {
                Vec3::new(
                    from.0 + (to.0 - from.0) * share / length,
                    up,
                    from.1 + (to.1 - from.1) * share / length,
                )
            };
            // A cell of fence at a time: its two rails, and the lattice
            // zigzagging between them a pitch at a time, the zigzag's phase
            // kept along the whole fence.
            let mut along = 0.0;
            while along < length - 1e-9 {
                let reach = (along + 1.0).min(length);
                stage.begin(point(f64::midpoint(along, reach), height / 2.0));
                stage.push(line([point(along, low), point(reach, low)]));
                stage.push(line([point(along, high), point(reach, high)]));
                let mut pitch = along;
                while pitch < reach - 1e-9 {
                    let next = (pitch + PITCH).min(reach);
                    let rising =
                        mathf::round_i32(mathf::floor((run + pitch) / PITCH + 1e-9)) % 2 == 0;
                    let (start, finish) = if rising { (low, high) } else { (high, low) };
                    stage.push(line([point(pitch, start), point(next, finish)]));
                    pitch = next;
                }
                along = reach;
            }
            run += length;
            from = to;
        }
    }

    /// `rider`'s bike as it stands at `time`: its wheels, body and rider in
    /// the order the camera sees them.
    fn bike(&self, rider: &Rider, time: f64, stage: &mut Stage) {
        let (pose, _) = self.pose(rider, time);
        let seen = pose.point_to_local(stage.camera().position());
        let ink = rider.ink;
        let rolled = (time - self.start) * RIDE / WHEEL;
        stage.begin(pose.at + Vec3::UP * 0.3);
        let front_near = seen.z > FRONT_CLEAR;
        let rear_near = seen.z < REAR_CLEAR;
        if !front_near {
            wheel(&pose, 0, rolled, ink, stage);
        }
        if !rear_near {
            wheel(&pose, 1, rolled, ink, stage);
        }
        // A bike leaning away through a turn can tip the seat's plane above
        // the camera, and then its body hides its rider.
        let seated = if seen.y > SEAT {
            [HullId::BikeBody, HullId::BikeRider]
        } else {
            [HullId::BikeRider, HullId::BikeBody]
        };
        for hull in seated {
            stage.push(Part::Solid {
                hull,
                pose,
                ink,
                alpha: 1.0,
            });
        }
        if front_near {
            wheel(&pose, 0, rolled, ink, stage);
        }
        if rear_near {
            wheel(&pose, 1, rolled, ink, stage);
        }
    }
}

/// Wheel `which` of a bike at `pose` — the front, or the rear — turned
/// `rolled` radians, with the fork and handlebar or the swingarm that
/// holds it.
fn wheel(pose: &Pose, which: usize, rolled: f64, ink: &'static Ink, stage: &mut Stage) {
    let along = HUBS[which % HUBS.len()];
    let hub = pose.point_to_world(Vec3::new(0.0, HUB_HEIGHT, along));
    let plane = Frame {
        x: pose.frame.z,
        y: pose.frame.y,
        z: -pose.frame.x,
    };
    stage.push(Part::Ring {
        centre: hub,
        frame: plane,
        radius: WHEEL,
        sides: WHEEL_SIDES,
        ink,
        alpha: 1.0,
    });
    for spoke in 0..SPOKES {
        let turn = -rolled + TAU * f64::from(spoke) / f64::from(SPOKES);
        let rim = hub + (plane.x * mathf::cos(turn) + plane.y * mathf::sin(turn)) * (WHEEL * 0.92);
        stage.push(Part::Line {
            ends: [hub, rim],
            ink,
            alpha: 1.0,
        });
    }
    let line = |from: Vec3, to: Vec3| Part::Line {
        ends: [pose.point_to_world(from), pose.point_to_world(to)],
        ink,
        alpha: 1.0,
    };
    if which == 0 {
        for side in [-FORK_HALF, FORK_HALF] {
            stage.push(line(
                Vec3::new(side, HUB_HEIGHT, along),
                Vec3::new(side, FORK_TOP.1, FORK_TOP.0),
            ));
        }
        stage.push(line(
            Vec3::new(-BAR_HALF, FORK_TOP.1, FORK_TOP.0),
            Vec3::new(BAR_HALF, FORK_TOP.1, FORK_TOP.0),
        ));
    } else {
        for side in [-ARM_HALF, ARM_HALF] {
            stage.push(line(
                Vec3::new(side, HUB_HEIGHT, along),
                Vec3::new(side, ARM_END.1, ARM_END.0),
            ));
        }
    }
}

#[cfg(test)]
#[path = "riders_tests.rs"]
mod tests;
