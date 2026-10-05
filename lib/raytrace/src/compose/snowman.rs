//! A snowman set out on the snow: two balls or three rolled from the snow
//! about it and stacked crooked, each seated on the one below, with lumps of
//! coal for its eyes, a carrot for its nose and, as often as not, sticks for
//! its arms and coal down its front and for its mouth.

use core::f64::consts::TAU;

use tairix_util::mathf;

use super::{direction, rgb, tumble, Dice, Recipe, Stage};
use crate::bark::{Bark, BarkKind};
use crate::land::Land;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::rock::Habit;
use crate::shape::Shape;
use crate::snowman::{Ball, Making, Rolled};
use crate::vector::{Frame, Pose, Vec3};

/// The prototypes a snowman plans at most: three balls, the coal of its
/// face and front, a carrot and two sticks. One that would not fit is not
/// built.
const PROTOTYPES: usize = 7;

/// A ball of the stack: its shape, and where it stands.
#[derive(Copy, Clone, Debug)]
struct Placed {
    ball: Ball,
    pose: Pose,
}

impl Placed {
    /// Where its surface lies along the world direction `toward` from its
    /// middle, and which way it faces out there.
    fn on(&self, toward: Vec3) -> (Vec3, Vec3) {
        let local = self.ball.surface(self.pose.frame.to_local(toward));
        let point = self.pose.point_to_world(local);
        (point, (point - self.pose.at).normalized())
    }
}

/// The materials a snowman is made of.
#[derive(Copy, Clone, Debug)]
struct Stuff {
    snow: usize,
    coal: usize,
    /// The lump of coal every piece of it is placed from.
    lump: u32,
}

/// A snowman at `at` on `land`, looking toward the compass angle `facing`.
pub(super) fn snowman(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (at, facing): ((f64, f64), f64),
) -> Option<()> {
    if stage.plannable() < PROTOTYPES {
        return Some(());
    }
    let balls = stacked(dice);
    let stuff = stuff(stage, dice)?;
    let placed = stack(stage, dice, land, (at, &balls), stuff.snow)?;
    let count = balls.iter().flatten().count();
    let (head, body) = (
        placed.get(count - 1)?.as_ref()?,
        placed.get(count - 2)?.as_ref()?,
    );
    let face = facing + dice.angle(-15.0, 15.0);
    face_of(stage, dice, head, (face, stuff))?;
    if dice.chance(0.6) {
        buttons(stage, dice, body, (face, stuff))?;
    }
    if dice.chance(0.7) {
        arms(stage, dice, body, face)?;
    }
    stage.claim(at, 1.3 * balls.first()?.as_ref()?.radius)
}

/// The balls of a snowman, foot first: two or three, each smaller than the
/// one it stands on, each pressed down onto what it stands on and seated
/// into the ball below as far as that press flattened them both.
fn stacked(dice: &mut Dice) -> [Option<Ball>; 3] {
    let three = dice.chance(0.8);
    let foot = dice.range(0.3, 0.5);
    let middle = foot
        * if three {
            dice.range(0.62, 0.8)
        } else {
            dice.range(0.55, 0.72)
        };
    let radii = if three {
        [foot, middle, middle * dice.range(0.6, 0.78)]
    } else {
        [foot, middle, 0.0]
    };
    let count = if three { 3 } else { 2 };
    let pressed = [
        dice.range(0.1, 0.18),
        dice.range(0.08, 0.15),
        dice.range(0.08, 0.14),
    ];
    let mut balls = [None; 3];
    for index in 0..count {
        let above = index + 1 < count;
        // The ball above flattens a disc about as broad as its own foot.
        let seat = above.then(|| {
            let ratio = radii[index + 1] / radii[index];
            0.81 * pressed[index + 1] * ratio * ratio
        });
        let making = Making {
            // A head is as often packed by hand as rolled.
            rolled: above || dice.chance(0.5),
            pressed: pressed[index],
            seat,
        };
        balls[index] = Some(Ball::made(radii[index], making, dice.wide()));
    }
    balls
}

/// The snow, the coal and the lump of coal a snowman is made of, as
/// materials and a prototype of `stage`'s.
fn stuff(stage: &mut Stage, dice: &mut Dice) -> Option<Stuff> {
    let rolled = Rolled {
        snow: rgb(0xEC_EF_F4),
        taken: [rgb(0x4E_3E_30), rgb(0x8A_7A_4C), rgb(0x6A_4A_2C)],
        streaked: dice.range(0.15, 0.6),
        seed: dice.seed(),
    };
    let snow = stage.material(
        Material::new(Pigment::Rolled(rolled), Finish::Matte).with_relief(Relief::grain(
            0.05,
            25.0,
            dice.seed(),
        )),
    )?;
    let coal = stage.material(
        Material::new(
            Pigment::Solid(rgb(0x14_14_17)),
            Finish::Coated { roughness: 0.3 },
        )
        .with_relief(Relief::grain(0.08, 150.0, dice.seed())),
    )?;
    // Coal breaks along fresh faces all round, and nothing has worn them.
    let habit = Habit {
        squash: dice.range(0.55, 0.8),
        elongation: dice.range(0.65, 0.9),
        fractures: 6,
        cleaved: false,
    };
    let lump = stage.plan(&Recipe::Rock {
        habit,
        wear: 0.0,
        seed: dice.wide(),
    })?;
    Some(Stuff { snow, coal, lump })
}

/// Set `balls` out at `at` on `land` in `snow`, each seated on the one below,
/// leaning a little and set off its middle as hands set it: where each
/// stands.
fn stack(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (at, balls): ((f64, f64), &[Option<Ball>; 3]),
    snow: usize,
) -> Option<[Option<Placed>; 3]> {
    let ground = land.height(&stage.fields, at.0, at.1);
    let normal = land.normal(&stage.fields, at.0, at.1);
    let mut placed = [None; 3];
    let mut below: Option<Placed> = None;
    for (slot, ball) in placed.iter_mut().zip(balls.iter().flatten()) {
        let lean = if below.is_some() {
            dice.angle(1.5, 7.0)
        } else {
            0.0
        };
        let frame = Frame::turned(dice.range(0.0, TAU), lean);
        let foot = ball.foot();
        let rests = match below {
            None => {
                // Pressed into the snow, and down far enough on a slope that
                // its level foot leaves no gap on the downhill side.
                let breadth = mathf::sqrt((ball.radius * ball.radius - foot * foot).max(0.0));
                let slope = mathf::sqrt((1.0 - normal.y * normal.y).max(0.0)) / normal.y.max(0.2);
                Vec3::new(
                    at.0,
                    ground - dice.range(0.01, 0.03) - breadth * slope,
                    at.1,
                )
            }
            Some(under) => {
                let seat = under.ball.seat()?;
                let disc =
                    mathf::sqrt((under.ball.radius * under.ball.radius - seat * seat).max(0.0));
                let off = dice.range(0.0, 0.25) * disc;
                let way = dice.range(0.0, TAU);
                under.pose.point_to_world(Vec3::new(0.0, seat, 0.0))
                    + Vec3::new(mathf::sin(way) * off, 0.0, mathf::cos(way) * off)
            }
        };
        let pose = Pose::new(rests - frame.y * foot, frame);
        let prototype = stage.plan(&Recipe::Snowball { ball: *ball })?;
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale: 1.0,
                key: dice.seed(),
            },
            snow,
            pose,
            false,
        )?;
        let here = Placed { ball: *ball, pose };
        *slot = Some(here);
        below = Some(here);
    }
    Some(placed)
}

/// A lump of `stuff`'s coal `size` across pressed into a ball at `point`,
/// its face turned `out`, sunk a share of itself in.
fn coal(
    stage: &mut Stage,
    dice: &mut Dice,
    stuff: Stuff,
    (point, out, size): (Vec3, Vec3, f64),
) -> Option<()> {
    let pose = Pose::new(point - out * (dice.range(0.3, 0.55) * size), tumble(dice));
    stage
        .add(
            Shape::Instance {
                prototype: stuff.lump,
                pose,
                scale: size,
                key: dice.seed(),
            },
            stuff.coal,
            pose,
            false,
        )
        .map(|_| ())
}

/// The face pressed into `head`, looking toward `face`: two lumps of coal
/// for its eyes, a carrot pushed in for its nose, drooping as it may, and,
/// as often as not, a smile of smaller lumps.
fn face_of(
    stage: &mut Stage,
    dice: &mut Dice,
    head: &Placed,
    (face, stuff): (f64, Stuff),
) -> Option<()> {
    let apart = dice.angle(17.0, 24.0);
    let raised = dice.angle(10.0, 20.0);
    let size = dice.range(0.018, 0.026);
    for side in [-1.0, 1.0] {
        let (point, out) = head.on(direction(
            face,
            side * apart,
            raised + dice.angle(-2.0, 2.0),
        ));
        let own = size * dice.range(0.85, 1.15);
        coal(stage, dice, stuff, (point, out, own))?;
    }
    let length = dice.range(0.09, 0.16);
    let pattern = carrot_skin(dice.seed());
    let skin = stage.material(
        Material::new(
            Pigment::Bark(pattern.clone()),
            Finish::Coated { roughness: 0.6 },
        )
        .with_relief(Relief::Bark {
            bark: pattern,
            depth: 0.0012,
        }),
    )?;
    let carrot = stage.plan(&Recipe::Carrot {
        length,
        radius: dice.range(0.011, 0.018),
        skin: u16::try_from(skin).ok()?,
        seed: dice.wide(),
    })?;
    let (point, _) = head.on(direction(face, 0.0, dice.angle(-3.0, 6.0)));
    let out = direction(face, dice.angle(-8.0, 8.0), dice.angle(-12.0, 3.0));
    let pose = Pose::new(point - out * (0.25 * length), pointing(out));
    stage.add(
        Shape::Instance {
            prototype: carrot,
            pose,
            scale: 1.0,
            key: dice.seed(),
        },
        skin,
        pose,
        false,
    )?;
    if dice.chance(0.5) {
        let pieces = dice.count(5, 7);
        let spread = dice.angle(22.0, 32.0);
        for piece in 0..pieces {
            let across = 2.0 * f64::from(piece) / f64::from(pieces - 1) - 1.0;
            let lowered = dice.angle(20.0, 24.0) - dice.angle(5.0, 8.0) * across * across;
            let (point, out) = head.on(direction(face, across * spread, -lowered));
            let own = dice.range(0.008, 0.013);
            coal(stage, dice, stuff, (point, out, own))?;
        }
    }
    Some(())
}

/// A carrot's skin, its pattern drawn under `seed`.
fn carrot_skin(seed: u32) -> Bark {
    Bark {
        kind: BarkKind::Taproot,
        light: rgb(0xE6_6E_1A),
        dark: rgb(0xA0_42_12),
        accent: rgb(0xF2_94_46),
        rise: 0.0,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed,
    }
}

/// Lumps of coal down the front of `body`, the way it faces.
fn buttons(
    stage: &mut Stage,
    dice: &mut Dice,
    body: &Placed,
    (face, stuff): (f64, Stuff),
) -> Option<()> {
    let count = dice.count(2, 4);
    let (top, bottom) = (dice.angle(30.0, 45.0), dice.angle(-20.0, -5.0));
    let size = dice.range(0.02, 0.03);
    for button in 0..count {
        let down = f64::from(button) / f64::from(count - 1);
        let (point, out) = body.on(direction(
            face,
            dice.angle(-4.0, 4.0),
            top + (bottom - top) * down,
        ));
        let own = size * dice.range(0.85, 1.15);
        coal(stage, dice, stuff, (point, out, own))?;
    }
    Some(())
}

/// Two sticks pushed into the sides of `body`, which faces `face`, held out
/// as arms: raised, level or drooping, each a stick of its own.
fn arms(stage: &mut Stage, dice: &mut Dice, body: &Placed, face: f64) -> Option<()> {
    let bark = Bark {
        kind: BarkKind::Smooth,
        light: rgb(0x6E_5E_4E),
        dark: rgb(0x3C_32_2A),
        accent: rgb(0x7E_70_5E),
        rise: 0.0,
        snow: 0.7,
        moss: 0.0,
        bare: 0.0,
        seed: dice.seed(),
    };
    let material = stage.material(
        Material::new(
            Pigment::Bark(bark.clone()),
            Finish::Coated { roughness: 0.9 },
        )
        .with_relief(Relief::Bark {
            bark,
            depth: 0.0008,
        }),
    )?;
    let stick_bark = u16::try_from(material).ok()?;
    for side in [-1.0, 1.0] {
        let length = dice.range(0.45, 0.8);
        let stick = stage.plan(&Recipe::Stick {
            length,
            radius: dice.range(0.012, 0.022),
            bark: stick_bark,
            seed: dice.wide(),
        })?;
        let round = side * dice.angle(75.0, 105.0);
        let (point, _) = body.on(direction(face, round, dice.angle(0.0, 20.0)));
        let out = direction(
            face,
            round + side * dice.angle(-10.0, 20.0),
            dice.angle(-10.0, 40.0),
        );
        let pose = Pose::new(point - out * (0.12 * length), pointing(out));
        stage.add(
            Shape::Instance {
                prototype: stick,
                pose,
                scale: 1.0,
                key: dice.seed(),
            },
            material,
            pose,
            false,
        )?;
    }
    Some(())
}

/// The frame whose z points along the unit `out`, its y as near up as that
/// allows: what a piece grown along its own z and bending in its y is
/// placed in.
fn pointing(out: Vec3) -> Frame {
    let reference = if out.y.abs() < 0.95 {
        Vec3::UP
    } else {
        Vec3::new(1.0, 0.0, 0.0)
    };
    let x = reference.cross(out).normalized();
    Frame {
        x,
        y: out.cross(x),
        z: out,
    }
}

#[cfg(test)]
#[path = "snowman_tests.rs"]
mod tests;
