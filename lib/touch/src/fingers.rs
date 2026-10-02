//! The contacts a surface has down, measured in micrometres.

use tairix_abi::touch::{Contact, ContactKind, ContactPhase, TouchFrame, TOUCH_CONTACTS_MAX};

use crate::SurfacePoint;

/// A position or displacement on a surface, in micrometres.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Um {
    pub x: i64,
    pub y: i64,
}

impl Um {
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }

    pub const fn plus(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }

    pub const fn minus(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }

    const fn length_squared(self) -> u64 {
        self.x.unsigned_abs() * self.x.unsigned_abs()
            + self.y.unsigned_abs() * self.y.unsigned_abs()
    }

    pub fn length(self) -> i64 {
        i64::try_from(self.length_squared().isqrt()).unwrap_or(i64::MAX)
    }

    /// Whether this displacement reaches further than `radius`.
    pub const fn exceeds(self, radius: i64) -> bool {
        self.length_squared() > radius.unsigned_abs() * radius.unsigned_abs()
    }
}

/// One contact the surface has down, or lifted in this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Finger {
    pub id: u16,
    pub landed: Um,
    pub at: Um,
    /// Where it was in the frame before, so `at - was` is this frame's motion.
    pub was: Um,
    pub place: SurfacePoint,
    /// Once judged a palm, it takes part in nothing until it lifts.
    pub palm: bool,
    /// Still on the surface after this frame.
    pub down: bool,
}

impl Finger {
    /// Whether this is a finger on the surface, as every gesture counts one.
    pub const fn touching(&self) -> bool {
        self.down && !self.palm
    }

    /// How far it moved in this frame.
    pub const fn moved(&self) -> Um {
        self.at.minus(self.was)
    }

    /// How far it has moved since it landed.
    pub const fn travelled(&self) -> Um {
        self.at.minus(self.landed)
    }
}

const NO_FINGER: Finger = Finger {
    id: 0,
    landed: Um::new(0, 0),
    at: Um::new(0, 0),
    was: Um::new(0, 0),
    place: SurfacePoint { x: 0, y: 0 },
    palm: false,
    down: false,
};

/// Room for every contact of the frame before, all lost, and a whole frame of
/// new ones: a lost contact is kept until the frame that loses it is read.
const HELD_MAX: usize = 2 * TOUCH_CONTACTS_MAX;

/// The two fingers of a two-finger gesture, as one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pair {
    pub centre: Um,
    pub span: i64,
    pub place: SurfacePoint,
}

/// The contacts a surface has down.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fingers {
    list: [Finger; HELD_MAX],
    len: usize,
}

impl Fingers {
    pub const fn new() -> Self {
        Self {
            list: [NO_FINGER; HELD_MAX],
            len: 0,
        }
    }

    /// Read `frame` over a surface `size` across: its contacts move, land
    /// and lift, and one it no longer names has lifted where it last was.
    pub fn update(&mut self, frame: &TouchFrame, size: Um) {
        let mut named = [false; HELD_MAX];
        for finger in &mut self.list[..self.len] {
            finger.was = finger.at;
        }
        for contact in frame.contacts() {
            let at = micrometres(*contact, size);
            let place = SurfacePoint {
                x: contact.x,
                y: contact.y,
            };
            let down = contact.phase == ContactPhase::Down;
            let palm = contact.kind == ContactKind::Palm;
            if let Some(index) = self.list[..self.len]
                .iter()
                .position(|finger| finger.id == contact.id)
            {
                let finger = &mut self.list[index];
                finger.at = at;
                finger.place = place;
                finger.palm |= palm;
                finger.down = down;
                named[index] = true;
            } else if down && self.len < HELD_MAX {
                self.list[self.len] = Finger {
                    id: contact.id,
                    landed: at,
                    at,
                    was: at,
                    place,
                    palm,
                    down: true,
                };
                named[self.len] = true;
                self.len += 1;
            }
        }
        for (finger, named) in self.list[..self.len].iter_mut().zip(named) {
            finger.down &= named;
        }
    }

    /// Forget every contact that lifted in the frame just read.
    pub fn prune(&mut self) {
        let mut kept = 0;
        for index in 0..self.len {
            if self.list[index].down {
                self.list[kept] = self.list[index];
                kept += 1;
            }
        }
        self.len = kept;
    }

    /// Every contact down, or lifting in the frame just read.
    pub fn all(&self) -> &[Finger] {
        &self.list[..self.len]
    }

    /// The fingers on the surface.
    pub fn touching(&self) -> impl Iterator<Item = &Finger> {
        self.all().iter().filter(|finger| finger.touching())
    }

    pub fn count(&self) -> usize {
        self.touching().count()
    }

    pub fn find(&self, id: u16) -> Option<&Finger> {
        self.all().iter().find(|finger| finger.id == id)
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The two fingers on the surface, when exactly two are.
    pub fn pair(&self) -> Option<Pair> {
        let mut touching = self.touching();
        let (first, second) = (touching.next()?, touching.next()?);
        if touching.next().is_some() {
            return None;
        }
        Some(Pair {
            centre: Um::new(
                first.at.x.midpoint(second.at.x),
                first.at.y.midpoint(second.at.y),
            ),
            span: first.at.minus(second.at).length(),
            place: SurfacePoint {
                x: first.place.x.midpoint(second.place.x),
                y: first.place.y.midpoint(second.place.y),
            },
        })
    }
}

/// Where `contact` is on a surface `size` across.
fn micrometres(contact: Contact, size: Um) -> Um {
    let span = i64::from(u16::MAX);
    Um::new(
        i64::from(contact.x) * size.x / span,
        i64::from(contact.y) * size.y / span,
    )
}
