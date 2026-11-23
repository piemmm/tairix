//! Points and the transformation matrix, in the Draw module's units: a
//! path's user units, mapped by the matrix into internal Draw units, 256 to
//! the OS unit.

/// A point of a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Turned a right angle clockwise.
    #[must_use]
    pub(crate) const fn turned(self) -> Self {
        Self::new(self.y, self.x.wrapping_neg())
    }
}

/// Sums and differences of points wrap, as the processor's do.
impl core::ops::Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x.wrapping_add(other.x), self.y.wrapping_add(other.y))
    }
}

impl core::ops::Sub for Point {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x.wrapping_sub(other.x), self.y.wrapping_sub(other.y))
    }
}

impl core::ops::Neg for Point {
    type Output = Self;

    fn neg(self) -> Self {
        Self::new(self.x.wrapping_neg(), self.y.wrapping_neg())
    }
}

/// The matrix: `a` to `d` with sixteen bits of fraction, `e` and `f` in
/// internal Draw units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matrix {
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub e: i32,
    pub f: i32,
}

/// One in a matrix entry.
pub const ONE: i32 = 1 << 16;

impl Matrix {
    /// The matrix a null pointer stands for.
    pub const IDENTITY: Self = Self {
        a: ONE,
        b: 0,
        c: 0,
        d: ONE,
        e: 0,
        f: 0,
    };

    /// The six words a matrix block holds, in their order.
    #[must_use]
    pub const fn from_words(words: [u32; 6]) -> Self {
        Self {
            a: words[0].cast_signed(),
            b: words[1].cast_signed(),
            c: words[2].cast_signed(),
            d: words[3].cast_signed(),
            e: words[4].cast_signed(),
            f: words[5].cast_signed(),
        }
    }

    /// The words of its block.
    #[must_use]
    pub const fn words(&self) -> [u32; 6] {
        [
            self.a.cast_unsigned(),
            self.b.cast_unsigned(),
            self.c.cast_unsigned(),
            self.d.cast_unsigned(),
            self.e.cast_unsigned(),
            self.f.cast_unsigned(),
        ]
    }

    /// `p` mapped as the Draw module maps a path's points: the products
    /// summed exactly, rounded half up to a unit, the translation added;
    /// `None` where the result leaves a word.
    #[must_use]
    pub fn apply(&self, p: Point) -> Option<Point> {
        let axis = |m: i32, n: i32, t: i32| {
            let sum = i64::from(m) * i64::from(p.x) + i64::from(n) * i64::from(p.y);
            i32::try_from(((sum + 0x8000) >> 16) + i64::from(t)).ok()
        };
        Some(Point::new(
            axis(self.a, self.c, self.e)?,
            axis(self.b, self.d, self.f)?,
        ))
    }

    /// `p` mapped as DrawFile maps a bounding box's corner: each product
    /// cut toward nought on its own, then the translation added, wrapping as
    /// the processor's sums do.
    #[must_use]
    pub fn apply_truncating(&self, p: Point) -> Point {
        let term = |m: i32, v: i32| -> i32 {
            let product = i64::from(m) * i64::from(v);
            let cut = if product < 0 {
                -((-product) >> 16)
            } else {
                product >> 16
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a corner's term keeps its low word, as the processor's arithmetic does"
            )]
            let cut = cut as i32;
            cut
        };
        Point::new(
            term(self.a, p.x)
                .wrapping_add(term(self.c, p.y))
                .wrapping_add(self.e),
            term(self.b, p.x)
                .wrapping_add(term(self.d, p.y))
                .wrapping_add(self.f),
        )
    }

    /// Whether it changes no point.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

/// The length of `(dx, dy)` as the Draw module measures one: the square
/// root of the sum of squares, rounded down.
#[must_use]
pub(crate) fn length_of(dx: i64, dy: i64) -> u64 {
    let squares = u128::from(dx.unsigned_abs()).pow(2) + u128::from(dy.unsigned_abs()).pow(2);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the root of the squares of two 64-bit numbers fits 65 bits, and a path's \
                  differences are of 33"
    )]
    let root = squares.isqrt() as u64;
    root
}

#[cfg(test)]
mod tests;
