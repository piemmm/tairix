//! How densely a picture's pixels are laid out, kept in the terms its file
//! states it, so a picture written back to the format it was read from says
//! exactly what it said before.

/// What a [`Density`] counts pixels per.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DensityUnit {
    /// No physical unit: the figures state only the pixels' shape.
    Aspect,
    /// Pixels per inch.
    Inch,
    /// Pixels per centimetre.
    Centimetre,
    /// Pixels per metre.
    Metre,
}

impl DensityUnit {
    /// Metres per unit as a ratio, or `None` for a unit with no length.
    const fn metres(self) -> Option<(u128, u128)> {
        match self {
            Self::Aspect => None,
            Self::Inch => Some((127, 5000)),
            Self::Centimetre => Some((1, 100)),
            Self::Metre => Some((1, 1)),
        }
    }
}

/// Pixels per unit across and down, each a ratio of two non-zero whole
/// numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Density {
    across: (u32, u32),
    down: (u32, u32),
    unit: DensityUnit,
}

impl Density {
    /// `across` and `down` pixels per `unit`, each a numerator and a
    /// denominator; `None` where any part is zero.
    #[must_use]
    pub const fn new(across: (u32, u32), down: (u32, u32), unit: DensityUnit) -> Option<Self> {
        if across.0 == 0 || across.1 == 0 || down.0 == 0 || down.1 == 0 {
            return None;
        }
        Some(Self { across, down, unit })
    }

    /// Whole pixels per `unit` across and down; `None` where either is zero.
    #[must_use]
    pub const fn whole(across: u32, down: u32, unit: DensityUnit) -> Option<Self> {
        Self::new((across, 1), (down, 1), unit)
    }

    /// Pixels per unit across, as a numerator and a denominator.
    #[must_use]
    pub const fn across(&self) -> (u32, u32) {
        self.across
    }

    /// Pixels per unit down.
    #[must_use]
    pub const fn down(&self) -> (u32, u32) {
        self.down
    }

    /// What the figures count pixels per.
    #[must_use]
    pub const fn unit(&self) -> DensityUnit {
        self.unit
    }

    /// The figures as whole pixels per `unit`, rounded to the nearest, or
    /// `None` where they cannot be: a physical density stated as a bare shape
    /// or the reverse, a figure that rounds to nothing, or one past `u32`.
    /// Asked for a shape, a shape's two figures keep their proportion in the
    /// smallest whole numbers that state it.
    #[must_use]
    pub fn whole_in(&self, unit: DensityUnit) -> Option<(u32, u32)> {
        match (self.unit.metres(), unit.metres()) {
            (None, None) => self.shape(),
            (Some(from), Some(to)) => Some((
                convert(self.across, from, to)?,
                convert(self.down, from, to)?,
            )),
            _ => None,
        }
    }

    /// The pixels' shape, whatever the unit: the figures across and down in
    /// proportion, in the smallest whole numbers that state it, or `None`
    /// where those pass `u32`.
    #[must_use]
    pub fn shape(&self) -> Option<(u32, u32)> {
        let across = u128::from(self.across.0) * u128::from(self.down.1);
        let down = u128::from(self.down.0) * u128::from(self.across.1);
        let common = gcd(across, down);
        Some((
            u32::try_from(across / common).ok()?,
            u32::try_from(down / common).ok()?,
        ))
    }
}

impl Density {
    /// The same density stated exactly per `unit`, or `None` where its
    /// figures would not fit `u32` or the units are of different kinds.
    #[must_use]
    pub fn exact_in(&self, unit: DensityUnit) -> Option<Self> {
        let (from, to) = match (self.unit.metres(), unit.metres()) {
            (Some(from), Some(to)) => (from, to),
            (None, None) => return Some(Self { unit, ..*self }),
            _ => return None,
        };
        let restate = |value: (u32, u32)| {
            let numerator = u128::from(value.0) * to.0 * from.1;
            let denominator = u128::from(value.1) * to.1 * from.0;
            let common = gcd(numerator, denominator);
            Some((
                u32::try_from(numerator / common).ok()?,
                u32::try_from(denominator / common).ok()?,
            ))
        };
        Self::new(restate(self.across)?, restate(self.down)?, unit)
    }
}

/// What a file's density field states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Stated {
    /// A density the picture keeps: none for square pixels with no unit,
    /// which is what no density means.
    Kept(Option<Density>),
    /// One it cannot: a zero figure, or a unit the format does not define.
    Unkept,
}

impl Stated {
    /// What `across` and `down` per `unit` state, `unit` being `None` where
    /// the file names one its format does not define.
    pub(crate) fn of(across: (u32, u32), down: (u32, u32), unit: Option<DensityUnit>) -> Self {
        let Some(unit) = unit else {
            return Self::Unkept;
        };
        match Density::new(across, down, unit) {
            Some(density) if unit == DensityUnit::Aspect && density.shape() == Some((1, 1)) => {
                Self::Kept(None)
            }
            Some(density) => Self::Kept(Some(density)),
            None => Self::Unkept,
        }
    }
}

/// `value` pixels per a unit of `from` metres, as whole pixels per a unit of
/// `to` metres, rounded to the nearest.
fn convert(value: (u32, u32), from: (u128, u128), to: (u128, u128)) -> Option<u32> {
    let numerator = u128::from(value.0) * to.0 * from.1;
    let denominator = u128::from(value.1) * to.1 * from.0;
    let rounded = (numerator + denominator / 2) / denominator;
    u32::try_from(rounded).ok().filter(|&whole| whole > 0)
}

const fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let rest = a % b;
        a = b;
        b = rest;
    }
    if a == 0 {
        1
    } else {
        a
    }
}

#[cfg(test)]
#[path = "density_tests.rs"]
mod tests;
