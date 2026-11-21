//! The furniture painter's primitives: a rounded plate, a wash, a ring, a
//! shape's coverage, an ink laid through a coverage plane, and a picture laid
//! rounded or toned — each `tairix_raster`'s own `Surface` call, pixel for
//! pixel.
//!
//! Every pass lays over the rectangle of a surface its push names
//! ([`Push`]), within the shape it paints; what it paints follows in the
//! push, built and bounded by the pass's own description type, which refuses
//! geometry past a bound that keeps its arithmetic in a word ([`Refused`]).
//! A plane — a mask, a shape's coverage — is a storage buffer of words, one a
//! pixel, the value in its low byte.

use tairix_raster::MAX_DRAWING_EXTENT;
use tairix_spirv::{Admitted, BuildError, Function, Id, Op};

use crate::chrome::{framed, Framed, FRAME_FIELDS as FRAME, HEIGHT, WIDTH};
pub use crate::chrome::{Push, Refused};
use crate::kit::{pixel_word, within_extent, within_surface, Kit, Pass, Place};
use crate::MAX_PASS_RADIUS;

/// The binding of the surface every pass lays over, read and written.
pub const TARGET: u32 = 0;

/// Whether `target`'s rectangle lies within the `width` × `height`
/// rectangle at (`x`, `y`) of the same surface.
fn within(target: &Push, (x, y, width, height): (u32, u32, u32, u32)) -> bool {
    let reaches = |start: u32, extent: u32| u64::from(start) + u64::from(extent);
    target.left >= x
        && target.top >= y
        && reaches(target.left, target.width) <= reaches(x, width)
        && reaches(target.top, target.height) <= reaches(y, height)
}

/// The words of a push: `target`'s rectangle, then `fields` in order.
fn push_words<const N: usize>(target: &Push, fields: &[u32]) -> [u32; N] {
    let mut words = [0; N];
    let frame = target.words();
    for (slot, word) in words.iter_mut().zip(frame.iter().chain(fields)) {
        *slot = *word;
    }
    words
}

/// The word of `plane` at `index` where `held`, else its first word, which
/// every plane has: the low byte of what was read.
fn plane_byte(
    f: &mut Function<'_>,
    kit: &Kit,
    (plane, index): (Id, Id),
    held: Id,
) -> Result<Id, BuildError> {
    let zero = f.module().uint(0)?;
    let index = kit.select(f, held, index, zero)?;
    let word = kit.load(f, plane, index)?;
    kit.op_by(f, Op::BITWISE_AND, word, 0xff)
}

/// Whether (`x`, `y`) of the surface lies in the `width` × `height`
/// rectangle whose top left is at the words `origin`, which may be negative,
/// and where it lies in it.
fn placed(
    f: &mut Function<'_>,
    kit: &Kit,
    (x, y): (Id, Id),
    (left, top, width, height): (Id, Id, Id, Id),
) -> Result<(Id, Id, Id), BuildError> {
    let column = kit.op(f, Op::I_SUB, x, left)?;
    let row = kit.op(f, Op::I_SUB, y, top)?;
    let across = kit.test(f, Op::U_LESS_THAN, column, width)?;
    let down = kit.test(f, Op::U_LESS_THAN, row, height)?;
    let inside = f.op(Op::LOGICAL_AND, kit.boolean, &[across, down])?;
    Ok((inside, column, row))
}

/// Lay `source` at `factor`/255 over the target pixel `framed` names,
/// `Pixel::laid_over` rounding at its bias.
fn lay(
    f: &mut Function<'_>,
    kit: &Kit,
    (target, framed): (Id, &Framed),
    (source, factor): (Id, Id),
) -> Result<(), BuildError> {
    let under = kit.load(f, target, framed.at)?;
    let scaled = kit.scale(f, source, factor, framed.bias)?;
    let result = kit.over(f, scaled, under, framed.bias)?;
    kit.store(f, target, framed.at, result)
}

/// A rounded rectangle composited over a surface at each pixel's coverage, or
/// laid down replacing it, mixed toward it on an arc:
/// `Surface::fill_round_rect` and `Surface::set_round_rect`.
pub mod plate {
    use tairix_raster::Color;

    use super::{
        framed, pixel_word, push_words, within, within_extent, Admitted, BuildError, Op, Pass,
        Push, Refused, FRAME, HEIGHT, MAX_PASS_RADIUS, TARGET, WIDTH,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/plate.spv");

    const X: u32 = FRAME;
    const Y: u32 = FRAME + 1;
    const PLATE_WIDTH: u32 = FRAME + 2;
    const PLATE_HEIGHT: u32 = FRAME + 3;
    const RADIUS: u32 = FRAME + 4;
    const SOURCE: u32 = FRAME + 5;
    const REPLACE: u32 = FRAME + 6;
    /// The push constants' words.
    pub const WORDS: usize = (REPLACE + 1) as usize;

    /// A rounded rectangle of a surface and how it is laid.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Plate {
        /// Its leftmost column.
        pub x: u32,
        /// Its top row.
        pub y: u32,
        /// Its width.
        pub width: u32,
        /// Its height.
        pub height: u32,
        /// The radius its corners round by, before the clamp to half its
        /// shorter side.
        pub radius: u32,
        /// Its colour.
        pub color: Color,
        /// Whether it replaces what it covers rather than compositing over
        /// it.
        pub replace: bool,
    }

    impl Plate {
        /// The push laying the plate over `target`'s rectangle of it.
        ///
        /// # Errors
        ///
        /// [`Refused::Radius`] past [`MAX_PASS_RADIUS`], and
        /// [`Refused::Extent`] for a plate past `MAX_DRAWING_EXTENT` or a
        /// rectangle not within it.
        pub fn push(&self, target: &Push) -> Result<[u32; WORDS], Refused> {
            if self.radius > MAX_PASS_RADIUS {
                return Err(Refused::Radius);
            }
            let rect = (self.x, self.y, self.width, self.height);
            if !within_extent(&[self.width, self.height]) || !within(target, rect) {
                return Err(Refused::Extent);
            }
            Ok(push_words(
                target,
                &[
                    self.x,
                    self.y,
                    self.width,
                    self.height,
                    self.radius,
                    pixel_word(self.color.premultiply()),
                    u32::from(self.replace),
                ],
            ))
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let (left, top) = (kit.field(f, X)?, kit.field(f, Y)?);
            let lx = kit.op(f, Op::I_SUB, framed.x, left)?;
            let ly = kit.op(f, Op::I_SUB, framed.y, top)?;
            let shape = (
                kit.field(f, PLATE_WIDTH)?,
                kit.field(f, PLATE_HEIGHT)?,
                kit.field(f, RADIUS)?,
            );
            let coverage = kit.coverage(f, (lx, ly), shape)?;
            let source = kit.field(f, SOURCE)?;
            let zero = f.module().uint(0)?;
            let replace = kit.field(f, REPLACE)?;
            let replaces = kit.test(f, Op::I_NOT_EQUAL, replace, zero)?;
            let under = kit.load(f, target, framed.at)?;
            f.if_else(
                replaces,
                |f| {
                    let mixed = kit.mix(f, (under, source), coverage, framed.bias)?;
                    kit.store(f, target, framed.at, mixed)
                },
                |f| {
                    let scaled = kit.scale(f, source, coverage, framed.bias)?;
                    let laid = kit.over(f, scaled, under, framed.bias)?;
                    kit.store(f, target, framed.at, laid)
                },
            )
        })
    }
}

/// A straight colour washed over a rectangle, rounding into each pixel once
/// at its dither: ramped from a top colour to a bottom one down the
/// rectangle's rows, at a weight a pixel from up to two planes —
/// `Surface::fill_vertical_gradient`, `Surface::wash_region` and
/// `Surface::wash_polygon_subpixel`.
pub mod wash {
    use tairix_raster::Color;

    use super::{
        framed, plane_byte, push_words, within, within_extent, within_surface, Admitted,
        BuildError, Op, Pass, Push, Refused, FRAME, HEIGHT, TARGET, WIDTH,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/wash.spv");

    /// The binding of the planes the weights are read from.
    pub const PLANES: u32 = 1;

    const X: u32 = FRAME;
    const Y: u32 = FRAME + 1;
    const LAST: u32 = FRAME + 2;
    const TOP_COLOR: u32 = FRAME + 3;
    const BOTTOM_COLOR: u32 = FRAME + 4;
    const PLANE_COUNT: u32 = FRAME + 5;
    const FIRST_BASE: u32 = FRAME + 6;
    const FIRST_STRIDE: u32 = FRAME + 7;
    const SECOND_BASE: u32 = FRAME + 8;
    const SECOND_STRIDE: u32 = FRAME + 9;
    /// The push constants' words.
    pub const WORDS: usize = (SECOND_STRIDE + 1) as usize;

    /// A plane of weights in the [`PLANES`] buffer: the word its first
    /// pixel, the wash's top left, is at, and words a row.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub struct Plane {
        /// The word of its first pixel.
        pub base: u32,
        /// Words a row.
        pub stride: u32,
    }

    /// A wash over a rectangle of a surface.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Wash {
        /// The rectangle's leftmost column.
        pub x: u32,
        /// Its top row.
        pub y: u32,
        /// Its width.
        pub width: u32,
        /// Its height: the rows the colour ramps down.
        pub height: u32,
        /// The colour of its first row.
        pub top: Color,
        /// The colour of its last row.
        pub bottom: Color,
        /// The planes of weights its pixels are washed at, each from the
        /// rectangle's top left: none for a gradient, a mask for a masked
        /// wash, and a shape's coverage then a mask for a washed shape.
        pub planes: [Option<Plane>; 2],
    }

    impl Wash {
        /// The push washing `target`'s rectangle of it.
        ///
        /// # Errors
        ///
        /// [`Refused::Extent`] for a wash past `MAX_DRAWING_EXTENT`, a
        /// rectangle not within it, or a second plane without a first.
        pub fn push(&self, target: &Push) -> Result<[u32; WORDS], Refused> {
            let rect = (self.x, self.y, self.width, self.height);
            let strides = self.planes.map(|plane| plane.unwrap_or_default().stride);
            let held = self
                .planes
                .iter()
                .flatten()
                .all(|plane| within_surface(plane.base, plane.stride, self.height));
            if !within_extent(&[self.width, self.height, strides[0], strides[1]])
                || !held
                || !within(target, rect)
                || (self.planes[0].is_none() && self.planes[1].is_some())
            {
                return Err(Refused::Extent);
            }
            let [first, second] = self.planes.map(Option::unwrap_or_default);
            let count = self.planes.iter().flatten().count();
            Ok(push_words(
                target,
                &[
                    self.x,
                    self.y,
                    self.height.saturating_sub(1),
                    color_word(self.top),
                    color_word(self.bottom),
                    u32::try_from(count).unwrap_or(0),
                    first.base,
                    first.stride,
                    second.base,
                    second.stride,
                ],
            ))
        }
    }

    /// A straight colour as one word, red in its low byte.
    const fn color_word(color: Color) -> u32 {
        u32::from_le_bytes([color.r, color.g, color.b, color.a])
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        let planes = pass.buffer(PLANES)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let (left, top) = (kit.field(f, X)?, kit.field(f, Y)?);
            let lx = kit.op(f, Op::I_SUB, framed.x, left)?;
            let ly = kit.op(f, Op::I_SUB, framed.y, top)?;
            let ends = (kit.field(f, TOP_COLOR)?, kit.field(f, BOTTOM_COLOR)?);
            let last = kit.field(f, LAST)?;
            let colour = kit.ramp(f, ends, (ly, last))?;
            let alpha = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, colour, 24)?;
            let count = kit.field(f, PLANE_COUNT)?;
            let mut weights = [alpha; 2];
            for (weight, (base, stride, at_least)) in weights.iter_mut().zip([
                (FIRST_BASE, FIRST_STRIDE, 1),
                (SECOND_BASE, SECOND_STRIDE, 2),
            ]) {
                let reach = f.module().uint(at_least)?;
                let held = kit.test(f, Op::U_GREATER_THAN_EQUAL, count, reach)?;
                let stride = kit.field(f, stride)?;
                let base = kit.field(f, base)?;
                let start = kit.op(f, Op::I_MUL, ly, stride)?;
                let start = kit.op(f, Op::I_ADD, base, start)?;
                let index = kit.op(f, Op::I_ADD, start, lx)?;
                let byte = plane_byte(f, kit, (planes, index), held)?;
                let full = f.module().uint(255)?;
                *weight = kit.select(f, held, byte, full)?;
            }
            let two = f.module().uint(2)?;
            let both = kit.test(f, Op::U_GREATER_THAN_EQUAL, count, two)?;
            let joined = kit.scale8(f, weights[0], weights[1])?;
            let inner = kit.select(f, both, joined, weights[0])?;
            let weight = kit.scale8(f, alpha, inner)?;
            let zero = f.module().uint(0)?;
            let none = kit.equal(f, weight, zero)?;
            f.if_then(none, |f| f.emit(Op::RETURN, &[]))?;
            let under = kit.load(f, target, framed.at)?;
            let washed = kit.wash(f, (colour, weight), under, framed.bias)?;
            kit.store(f, target, framed.at, washed)
        })
    }
}

/// An ink laid through a plane of coverage at each pixel, at nearest: a
/// glyph laid through its row's coverage (`cover_span`), a mask tinted
/// (`Surface::blit_tinted`), a covered shape (`Surface::fill_polygon_subpixel`
/// with [`coverage`]'s plane).
pub mod mask {
    use tairix_raster::Color;

    use super::{
        framed, lay, placed, plane_byte, push_words, within_extent, within_surface, Admitted,
        BuildError, Op, Pass, Push, Refused, FRAME, HEIGHT, TARGET, WIDTH,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/mask.spv");

    /// The binding of the plane of coverage.
    pub const PLANE: u32 = 1;

    const MASK_X: u32 = FRAME;
    const MASK_Y: u32 = FRAME + 1;
    const MASK_WIDTH: u32 = FRAME + 2;
    const MASK_HEIGHT: u32 = FRAME + 3;
    const BASE: u32 = FRAME + 4;
    const STRIDE: u32 = FRAME + 5;
    const INK: u32 = FRAME + 6;
    /// The push constants' words.
    pub const WORDS: usize = (INK + 1) as usize;

    /// A plane of coverage placed on a surface, and the ink laid through it.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Mask {
        /// The plane's leftmost column on the surface, which may lie left of
        /// it.
        pub x: i32,
        /// Its top row on the surface, which may lie above it.
        pub y: i32,
        /// Its width.
        pub width: u32,
        /// Its height.
        pub height: u32,
        /// The word of its first pixel in the [`PLANE`] buffer.
        pub base: u32,
        /// Words a row.
        pub stride: u32,
        /// The ink.
        pub ink: Color,
    }

    impl Mask {
        /// The push laying the ink through the plane over `target`'s
        /// rectangle.
        ///
        /// # Errors
        ///
        /// [`Refused::Extent`] for a plane past `MAX_DRAWING_EXTENT` or more
        /// pixels than a surface holds.
        pub fn push(&self, target: &Push) -> Result<[u32; WORDS], Refused> {
            if !within_extent(&[self.width, self.height, self.stride])
                || !within_surface(self.base, self.stride, self.height)
            {
                return Err(Refused::Extent);
            }
            Ok(push_words(
                target,
                &[
                    self.x.cast_unsigned(),
                    self.y.cast_unsigned(),
                    self.width,
                    self.height,
                    self.base,
                    self.stride,
                    super::pixel_word(self.ink.premultiply()),
                ],
            ))
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        let plane = pass.buffer(PLANE)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let rect = (
                kit.field(f, MASK_X)?,
                kit.field(f, MASK_Y)?,
                kit.field(f, MASK_WIDTH)?,
                kit.field(f, MASK_HEIGHT)?,
            );
            let (inside, column, row) = placed(f, kit, (framed.x, framed.y), rect)?;
            let stride = kit.field(f, STRIDE)?;
            let base = kit.field(f, BASE)?;
            let start = kit.op(f, Op::I_MUL, row, stride)?;
            let start = kit.op(f, Op::I_ADD, base, start)?;
            let index = kit.op(f, Op::I_ADD, start, column)?;
            let level = plane_byte(f, kit, (plane, index), inside)?;
            let zero = f.module().uint(0)?;
            let seen = kit.test(f, Op::I_NOT_EQUAL, level, zero)?;
            let lays = f.op(Op::LOGICAL_AND, kit.boolean, &[inside, seen])?;
            let skipped = f.op(Op::LOGICAL_NOT, kit.boolean, &[lays])?;
            f.if_then(skipped, |f| f.emit(Op::RETURN, &[]))?;
            let ink = kit.field(f, INK)?;
            lay(f, kit, (target, &framed), (ink, level))
        })
    }
}

/// A picture laid over a surface, cut to a rounded rectangle
/// (`Surface::blit_rounded`) or toned toward its luma and weakened
/// (`Surface::blit_desaturated`), at nearest.
pub mod picture {
    use super::{
        framed, lay, placed, push_words, within_extent, within_surface, Admitted, BuildError, Op,
        Pass, Push, Refused, FRAME, HEIGHT, MAX_PASS_RADIUS, TARGET, WIDTH,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/picture.spv");

    /// The binding of the picture's pixels.
    pub const SOURCE: u32 = 1;

    const PICTURE_X: u32 = FRAME;
    const PICTURE_Y: u32 = FRAME + 1;
    const PICTURE_WIDTH: u32 = FRAME + 2;
    const PICTURE_HEIGHT: u32 = FRAME + 3;
    const BASE: u32 = FRAME + 4;
    const STRIDE: u32 = FRAME + 5;
    const ROUNDED: u32 = FRAME + 6;
    const RADIUS: u32 = FRAME + 7;
    const SATURATION: u32 = FRAME + 8;
    const STRENGTH: u32 = FRAME + 9;
    /// The push constants' words.
    pub const WORDS: usize = (STRENGTH + 1) as usize;

    /// How a picture is laid.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum Laid {
        /// Cut to the rounded rectangle it spans, its corners rounding by a
        /// radius before the clamp to half its shorter side.
        Rounded(u32),
        /// Toned toward its luma, keeping `saturation`/255 of its colour, and
        /// laid at `strength`/255 of itself.
        Toned {
            /// The colour kept.
            saturation: u8,
            /// The share of each pixel laid.
            strength: u8,
        },
    }

    /// A picture placed on a surface.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Picture {
        /// Its leftmost column on the surface, which may lie left of it.
        pub x: i32,
        /// Its top row on the surface, which may lie above it.
        pub y: i32,
        /// Its width.
        pub width: u32,
        /// Its height.
        pub height: u32,
        /// The word of its first pixel in the [`SOURCE`] buffer.
        pub base: u32,
        /// Words a row.
        pub stride: u32,
        /// How it is laid.
        pub laid: Laid,
    }

    impl Picture {
        /// The push laying the picture over `target`'s rectangle.
        ///
        /// # Errors
        ///
        /// [`Refused::Radius`] past [`MAX_PASS_RADIUS`], and
        /// [`Refused::Extent`] for a picture past `MAX_DRAWING_EXTENT` or more
        /// pixels than a surface holds.
        pub fn push(&self, target: &Push) -> Result<[u32; WORDS], Refused> {
            if !within_extent(&[self.width, self.height, self.stride])
                || !within_surface(self.base, self.stride, self.height)
            {
                return Err(Refused::Extent);
            }
            let (rounded, radius, saturation, strength) = match self.laid {
                Laid::Rounded(radius) if radius > MAX_PASS_RADIUS => return Err(Refused::Radius),
                Laid::Rounded(radius) => (1, radius, 255, 255),
                Laid::Toned {
                    saturation,
                    strength,
                } => (0, 0, saturation, strength),
            };
            Ok(push_words(
                target,
                &[
                    self.x.cast_unsigned(),
                    self.y.cast_unsigned(),
                    self.width,
                    self.height,
                    self.base,
                    self.stride,
                    rounded,
                    radius,
                    u32::from(saturation),
                    u32::from(strength),
                ],
            ))
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        let source = pass.buffer(SOURCE)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let (width, height) = (kit.field(f, PICTURE_WIDTH)?, kit.field(f, PICTURE_HEIGHT)?);
            let rect = (
                kit.field(f, PICTURE_X)?,
                kit.field(f, PICTURE_Y)?,
                width,
                height,
            );
            let (inside, column, row) = placed(f, kit, (framed.x, framed.y), rect)?;
            let zero = f.module().uint(0)?;
            let stride = kit.field(f, STRIDE)?;
            let base = kit.field(f, BASE)?;
            let start = kit.op(f, Op::I_MUL, row, stride)?;
            let start = kit.op(f, Op::I_ADD, base, start)?;
            let index = kit.op(f, Op::I_ADD, start, column)?;
            let index = kit.select(f, inside, index, zero)?;
            let pixel = kit.load(f, source, index)?;
            let alpha = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, pixel, 24)?;
            let radius = kit.field(f, RADIUS)?;
            let coverage = kit.coverage(f, (column, row), (width, height, radius))?;
            let rounded = kit.field(f, ROUNDED)?;
            let rounds = kit.test(f, Op::I_NOT_EQUAL, rounded, zero)?;
            let laid = kit.local(f, pixel)?;
            let is_toned = f.op(Op::LOGICAL_NOT, kit.boolean, &[rounds])?;
            f.if_then(is_toned, |f| {
                let saturation = kit.field(f, SATURATION)?;
                let desaturated = kit.desaturate(f, pixel, saturation)?;
                f.emit(Op::STORE, &[laid, desaturated])
            })?;
            let strength = kit.field(f, STRENGTH)?;
            let source = f.op(Op::LOAD, kit.uint, &[laid])?;
            let factor = kit.select(f, rounds, coverage, strength)?;
            let seen = kit.test(f, Op::I_NOT_EQUAL, alpha, zero)?;
            let cut = kit.equal(f, coverage, zero)?;
            let cut = f.op(Op::LOGICAL_AND, kit.boolean, &[rounds, cut])?;
            let uncut = f.op(Op::LOGICAL_NOT, kit.boolean, &[cut])?;
            let lays = f.op(Op::LOGICAL_AND, kit.boolean, &[inside, seen])?;
            let lays = f.op(Op::LOGICAL_AND, kit.boolean, &[lays, uncut])?;
            let skipped = f.op(Op::LOGICAL_NOT, kit.boolean, &[lays])?;
            f.if_then(skipped, |f| f.emit(Op::RETURN, &[]))?;
            lay(f, kit, (target, &framed), (source, factor))
        })
    }
}

/// The ring between a rounded rectangle and its concentric inset, washed
/// solid or lit by the upper-left key light (`Surface::wash_ring`), or laid
/// as a plate's rim (`Surface::frame_ring`).
pub mod ring {
    use tairix_raster::{Color, Ring, RingInk};

    use super::{
        framed, pixel_word, push_words, within, within_extent, Admitted, BuildError, Function, Id,
        Kit, Op, Pass, Push, Refused, FRAME, HEIGHT, MAX_PASS_RADIUS, TARGET, WIDTH,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/ring.spv");

    const X: u32 = FRAME;
    const Y: u32 = FRAME + 1;
    const RING_WIDTH: u32 = FRAME + 2;
    const RING_HEIGHT: u32 = FRAME + 3;
    const TOP_RADIUS: u32 = FRAME + 4;
    const BOTTOM_RADIUS: u32 = FRAME + 5;
    const THICKNESS: u32 = FRAME + 6;
    const MODE: u32 = FRAME + 7;
    const FIRST_INK: u32 = FRAME + 8;
    const SECOND_INK: u32 = FRAME + 9;
    /// The push constants' words.
    pub const WORDS: usize = (SECOND_INK + 1) as usize;

    const SOLID: u32 = 0;
    const BEVEL: u32 = 1;
    const FRAMED: u32 = 2;

    /// What a ring is laid in.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum Laid {
        /// Washed over what is there.
        Washed(RingInk),
        /// Laid as the rim of the plate it encloses, in a colour.
        Framed(Color),
    }

    /// A ring within a rectangle of a surface.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Placed {
        /// The rectangle's leftmost column.
        pub x: u32,
        /// Its top row.
        pub y: u32,
        /// Its width.
        pub width: u32,
        /// Its height.
        pub height: u32,
        /// The ring.
        pub ring: Ring,
        /// What it is laid in.
        pub laid: Laid,
    }

    impl Placed {
        /// The push laying the ring over `target`'s rectangle of it.
        ///
        /// # Errors
        ///
        /// [`Refused::Radius`] for a radius or thickness past
        /// [`MAX_PASS_RADIUS`], and [`Refused::Extent`] for a rectangle past
        /// `MAX_DRAWING_EXTENT` or not within this one.
        pub fn push(&self, target: &Push) -> Result<[u32; WORDS], Refused> {
            let ring = self.ring;
            if [ring.top_radius, ring.bottom_radius, ring.thickness]
                .iter()
                .any(|&length| length > MAX_PASS_RADIUS)
            {
                return Err(Refused::Radius);
            }
            let rect = (self.x, self.y, self.width, self.height);
            if !within_extent(&[self.width, self.height]) || !within(target, rect) {
                return Err(Refused::Extent);
            }
            let straight = |color: Color| u32::from_le_bytes([color.r, color.g, color.b, color.a]);
            let (mode, first, second) = match self.laid {
                Laid::Washed(RingInk::Solid(color)) => (SOLID, straight(color), 0),
                Laid::Washed(RingInk::Bevel { light, shade }) => {
                    (BEVEL, straight(light), straight(shade))
                }
                Laid::Framed(color) => (FRAMED, pixel_word(color.premultiply()), 0),
            };
            Ok(push_words(
                target,
                &[
                    self.x,
                    self.y,
                    self.width,
                    self.height,
                    ring.top_radius,
                    ring.bottom_radius,
                    ring.thickness,
                    mode,
                    first,
                    second,
                ],
            ))
        }
    }

    /// A radius clamped to half the shorter side of a `width` × `height`
    /// rectangle.
    fn clamped(
        f: &mut Function<'_>,
        kit: &Kit,
        radius: Id,
        (width, height): (Id, Id),
    ) -> Result<Id, BuildError> {
        let half_width = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, width, 1)?;
        let half_height = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, height, 1)?;
        let narrower = kit.min(f, radius, half_width)?;
        kit.min(f, narrower, half_height)
    }

    /// The coverage at (`lx`, `ly`) of a `width` × `height` rectangle whose
    /// top corners round by the clamped `top` and bottom ones by `bottom`:
    /// each corner the matching quadrant of a square of twice its radius.
    fn shape(
        f: &mut Function<'_>,
        kit: &Kit,
        (lx, ly): (Id, Id),
        (width, height, top, bottom): (Id, Id, Id, Id),
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let in_top = kit.test(f, Op::U_LESS_THAN, ly, top)?;
        let bottom_band = kit.op(f, Op::I_SUB, height, bottom)?;
        let rounds_below = kit.test(f, Op::I_NOT_EQUAL, bottom, zero)?;
        let low = kit.test(f, Op::U_GREATER_THAN_EQUAL, ly, bottom_band)?;
        let in_bottom = f.op(Op::LOGICAL_AND, kit.boolean, &[rounds_below, low])?;
        let below = kit.select(f, in_bottom, bottom, zero)?;
        let radius = kit.select(f, in_top, top, below)?;
        let side = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, radius, 1)?;
        let upper = kit.test(f, Op::U_LESS_THAN, ly, radius)?;
        let lower_shift = kit.op(f, Op::I_SUB, height, side)?;
        let lowered = kit.op(f, Op::I_SUB, ly, lower_shift)?;
        let qy = kit.select(f, upper, ly, lowered)?;
        let left = kit.test(f, Op::U_LESS_THAN, lx, radius)?;
        let right_band = kit.op(f, Op::I_SUB, width, radius)?;
        let right = kit.test(f, Op::U_GREATER_THAN_EQUAL, lx, right_band)?;
        let right_shift = kit.op(f, Op::I_SUB, width, side)?;
        let shifted = kit.op(f, Op::I_SUB, lx, right_shift)?;
        let qx = kit.select(f, left, lx, shifted)?;
        let corner_column = f.op(Op::LOGICAL_OR, kit.boolean, &[left, right])?;
        let curved = kit.test(f, Op::I_NOT_EQUAL, radius, zero)?;
        let corner = f.op(Op::LOGICAL_AND, kit.boolean, &[corner_column, curved])?;
        let coverage = kit.coverage(f, (qx, qy), (side, side, radius))?;
        let full = f.module().uint(255)?;
        kit.select(f, corner, coverage, full)
    }

    /// `tairix_raster`'s ring tone at (`lx`, `ly`): how squarely the outward
    /// normal faces the upper-left key light, as a signed word in
    /// `-255..=255`.
    fn tone(
        f: &mut Function<'_>,
        kit: &Kit,
        (lx, ly): (Id, Id),
        (width, height, margin_top, margin_bottom): (Id, Id, Id, Id),
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let doubled = |f: &mut Function<'_>, value| kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, value, 1);
        let (wide, tall) = (doubled(f, width)?, doubled(f, height)?);
        let px = doubled(f, lx)?;
        let px = kit.op_by(f, Op::I_ADD, px, 1)?;
        let py = doubled(f, ly)?;
        let py = kit.op_by(f, Op::I_ADD, py, 1)?;
        let upper = kit.test(f, Op::U_LESS_THAN, py, height)?;
        let margin = kit.select(f, upper, margin_top, margin_bottom)?;
        let side = doubled(f, margin)?;
        let top = doubled(f, margin_top)?;
        let bottom_margin = doubled(f, margin_bottom)?;
        let bottom = kit.saturating_sub(f, tall, bottom_margin)?;
        let clamp = |f: &mut Function<'_>, value, low, high| -> Result<Id, BuildError> {
            let high = f.glsl(tairix_spirv::GlslStd450::U_MAX, kit.uint, &[high, low])?;
            let raised = f.glsl(tairix_spirv::GlslStd450::U_MAX, kit.uint, &[value, low])?;
            kit.min(f, raised, high)
        };
        let far = kit.saturating_sub(f, wide, side)?;
        let across = clamp(f, px, side, far)?;
        let nx = kit.op(f, Op::I_SUB, px, across)?;
        let down = clamp(f, py, top, bottom)?;
        let ny = kit.op(f, Op::I_SUB, py, down)?;
        let nx2 = kit.op(f, Op::I_MUL, nx, nx)?;
        let ny2 = kit.op(f, Op::I_MUL, ny, ny)?;
        let squared = kit.op(f, Op::I_ADD, nx2, ny2)?;
        let length = kit.isqrt_scaled(f, squared, 8)?;
        let facing = kit.op(f, Op::I_SUB, zero, nx)?;
        let facing = kit.op(f, Op::I_SUB, facing, ny)?;
        let scaled = kit.op_by(f, Op::I_MUL, facing, 255 * 256)?;
        let one = f.module().uint(1)?;
        let flat = kit.equal(f, length, zero)?;
        let divisor = kit.select(f, flat, one, length)?;
        let tone = kit.op(f, Op::S_DIV, scaled, divisor)?;
        let most = f.module().uint(255)?;
        let least = f.module().uint(0u32.wrapping_sub(255))?;
        let high = kit.test(f, Op::S_GREATER_THAN, tone, most)?;
        let tone = kit.select(f, high, most, tone)?;
        let low = kit.test(f, Op::S_LESS_THAN, tone, least)?;
        let tone = kit.select(f, low, least, tone)?;
        kit.select(f, flat, zero, tone)
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let (left, top) = (kit.field(f, X)?, kit.field(f, Y)?);
            let lx = kit.op(f, Op::I_SUB, framed.x, left)?;
            let ly = kit.op(f, Op::I_SUB, framed.y, top)?;
            let (width, height) = (kit.field(f, RING_WIDTH)?, kit.field(f, RING_HEIGHT)?);
            let size = (width, height);
            let top_radius = kit.field(f, TOP_RADIUS)?;
            let top_radius = clamped(f, kit, top_radius, size)?;
            let bottom_radius = kit.field(f, BOTTOM_RADIUS)?;
            let bottom_radius = clamped(f, kit, bottom_radius, size)?;
            let thickness = kit.field(f, THICKNESS)?;
            let outer = shape(f, kit, (lx, ly), (width, height, top_radius, bottom_radius))?;
            let zero = f.module().uint(0)?;
            let twice = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, thickness, 1)?;
            let inner_width = kit.op(f, Op::I_SUB, width, twice)?;
            let inner_height = kit.op(f, Op::I_SUB, height, twice)?;
            let fits_across = kit.test(f, Op::U_GREATER_THAN, width, twice)?;
            let fits_down = kit.test(f, Op::U_GREATER_THAN, height, twice)?;
            let inset = f.op(Op::LOGICAL_AND, kit.boolean, &[fits_across, fits_down])?;
            let inner_size = (inner_width, inner_height);
            let inner_top = kit.saturating_sub(f, top_radius, thickness)?;
            let inner_top = clamped(f, kit, inner_top, inner_size)?;
            let inner_bottom = kit.saturating_sub(f, bottom_radius, thickness)?;
            let inner_bottom = clamped(f, kit, inner_bottom, inner_size)?;
            let ix = kit.op(f, Op::I_SUB, lx, thickness)?;
            let iy = kit.op(f, Op::I_SUB, ly, thickness)?;
            let across = kit.test(f, Op::U_LESS_THAN, ix, inner_width)?;
            let down = kit.test(f, Op::U_LESS_THAN, iy, inner_height)?;
            let after_left = kit.test(f, Op::U_GREATER_THAN_EQUAL, lx, thickness)?;
            let after_top = kit.test(f, Op::U_GREATER_THAN_EQUAL, ly, thickness)?;
            let mut within = inset;
            for condition in [across, down, after_left, after_top] {
                within = f.op(Op::LOGICAL_AND, kit.boolean, &[within, condition])?;
            }
            let inner = shape(
                f,
                kit,
                (ix, iy),
                (inner_width, inner_height, inner_top, inner_bottom),
            )?;
            let inner = kit.min(f, inner, outer)?;
            let inner = kit.select(f, within, inner, zero)?;
            let mode = kit.field(f, MODE)?;
            let framing = f.module().uint(FRAMED)?;
            let is_frame = kit.equal(f, mode, framing)?;
            let first = kit.field(f, FIRST_INK)?;
            f.if_then(is_frame, |f| {
                let edge = kit.mix(f, (zero, first), outer, framed.bias)?;
                let under = kit.load(f, target, framed.at)?;
                let rimmed = kit.mix(f, (edge, under), inner, framed.bias)?;
                kit.store(f, target, framed.at, rimmed)?;
                f.emit(Op::RETURN, &[])
            })?;
            let band = kit.saturating_sub(f, outer, inner)?;
            let bevel = f.module().uint(BEVEL)?;
            let bevels = kit.equal(f, mode, bevel)?;
            let banded = kit.test(f, Op::I_NOT_EQUAL, band, zero)?;
            let shading = f.op(Op::LOGICAL_AND, kit.boolean, &[bevels, banded])?;
            // Only a bevel's band is lit, and the tone is the costliest term.
            let toned = kit.local(f, zero)?;
            f.if_then(shading, |f| {
                let margin_top = f.glsl(
                    tairix_spirv::GlslStd450::U_MAX,
                    kit.uint,
                    &[top_radius, thickness],
                )?;
                let margin_bottom = f.glsl(
                    tairix_spirv::GlslStd450::U_MAX,
                    kit.uint,
                    &[bottom_radius, thickness],
                )?;
                let lit = tone(f, kit, (lx, ly), (width, height, margin_top, margin_bottom))?;
                f.emit(Op::STORE, &[toned, lit])
            })?;
            let lit = f.op(Op::LOAD, kit.uint, &[toned])?;
            let facing = kit.test(f, Op::S_GREATER_THAN_EQUAL, lit, zero)?;
            let away = kit.op(f, Op::I_SUB, zero, lit)?;
            let strength = kit.select(f, facing, lit, away)?;
            let second = kit.field(f, SECOND_INK)?;
            let toned = kit.select(f, facing, first, second)?;
            let product = kit.op(f, Op::I_MUL, band, strength)?;
            let nearest = f.module().uint(tairix_raster::ROUND_NEAREST)?;
            let shaded = kit.div255(f, product, nearest)?;
            let solid = f.module().uint(SOLID)?;
            let plain = kit.equal(f, mode, solid)?;
            let colour = kit.select(f, plain, first, toned)?;
            let weight = kit.select(f, plain, band, shaded)?;
            let ink_alpha = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, colour, 24)?;
            let product = kit.op(f, Op::I_MUL, ink_alpha, weight)?;
            let alpha = kit.div255(f, product, nearest)?;
            let unweighted = kit.equal(f, weight, zero)?;
            let clear = kit.equal(f, ink_alpha, zero)?;
            let skipped = f.op(Op::LOGICAL_OR, kit.boolean, &[unweighted, clear])?;
            f.if_then(skipped, |f| f.emit(Op::RETURN, &[]))?;
            let under = kit.load(f, target, framed.at)?;
            let washed = kit.wash(f, (colour, alpha), under, framed.bias)?;
            kit.store(f, target, framed.at, washed)
        })
    }
}

/// A scan-converted shape's coverage written to a plane, one word a pixel of
/// its window: the converter's own pieces of each row, walked a pixel at a
/// time and resolved under its fill rule, as `CoverageRows::row` does.
pub mod coverage {
    use alloc::vec::Vec;

    use tairix_raster::{CoverageRows, FillRule, SCAN_UNIT};
    use tairix_util::fallible;

    use super::{
        Admitted, BuildError, Function, Id, Kit, Op, Pass, Place, Refused, MAX_DRAWING_EXTENT,
    };

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/coverage.spv");

    /// The binding of the plane written, the window's width a row.
    pub const PLANE: u32 = 0;
    /// The binding of the shape's pieces, [`Shape::words`].
    pub const PIECES: u32 = 1;

    const LEFT: u32 = 0;
    const WIDTH: u32 = 1;
    const HEIGHT: u32 = 2;
    const EVEN_ODD: u32 = 3;
    /// The push constants' words.
    pub const WORDS: usize = (EVEN_ODD + 1) as usize;

    /// The widest and tallest a shape's window may be, and the furthest one
    /// of its pieces may run across a row, in pixels: a run times a row's
    /// rise stays in a signed word.
    pub const MAX_PASS_SHAPE: u32 = 1 << 14;

    /// The most pieces one row may hold: their summed cover, scaled to a
    /// pixel's area, stays in a signed word.
    pub const MAX_ROW_PIECES: usize = 1 << 12;

    /// A piece's words: its upper end, its lower end, its winding.
    const PIECE_WORDS: u32 = 5;

    /// A shape laid out for the pass: its window on the drawing, its rule,
    /// and each row's pieces.
    pub struct Shape {
        window: (u32, u32, u32, u32),
        even_odd: bool,
        words: Vec<u32>,
    }

    impl Shape {
        /// The shape `rows` covers over the `(x0, x1, y0, y1)` window
        /// `CoverageRows::bounds` answered for the surface it is drawn on.
        ///
        /// # Errors
        ///
        /// [`Refused::Extent`] for a window past [`MAX_PASS_SHAPE`], a piece
        /// running further than it across its row, or a row of more than
        /// [`MAX_ROW_PIECES`] pieces; [`Refused::OutOfMemory`].
        pub fn new(
            rows: &CoverageRows<'_>,
            (x0, x1, y0, y1): (u32, u32, u32, u32),
        ) -> Result<Self, Refused> {
            let (width, height) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
            if width > MAX_PASS_SHAPE || height > MAX_PASS_SHAPE || x1 > MAX_DRAWING_EXTENT {
                return Err(Refused::Extent);
            }
            let reach = i64::from(MAX_PASS_SHAPE) * i64::from(SCAN_UNIT);
            let offsets = usize::try_from(height).map_err(|_| Refused::Extent)? + 1;
            let mut pieces = 0usize;
            for y in y0..y1 {
                let count = rows.pieces(y).count();
                if count > MAX_ROW_PIECES {
                    return Err(Refused::Extent);
                }
                pieces += count;
            }
            let mut words = Vec::new();
            if !fallible::reserve(&mut words, offsets + pieces * PIECE_WORDS as usize) {
                return Err(Refused::OutOfMemory);
            }
            words.resize(offsets, 0);
            let mut laid = 0u32;
            for (row, y) in (y0..y1).enumerate() {
                for piece in rows.pieces(y) {
                    if (piece.to.0 - piece.from.0).abs() > reach {
                        return Err(Refused::Extent);
                    }
                    for field in [piece.from.0, piece.from.1, piece.to.0, piece.to.1] {
                        let field = i32::try_from(field).map_err(|_| Refused::Extent)?;
                        words.push(field.cast_unsigned());
                    }
                    words.push(piece.winding.cast_unsigned());
                    laid += 1;
                }
                if let Some(slot) = words.get_mut(row + 1) {
                    *slot = laid;
                }
            }
            Ok(Self {
                window: (x0, y0, width, height),
                even_odd: matches!(rows.rule(), FillRule::EvenOdd),
                words,
            })
        }

        /// The words, bound at [`PIECES`]: each row's first piece, then the
        /// pieces.
        #[must_use]
        pub fn words(&self) -> &[u32] {
            &self.words
        }

        /// The window covered, `(x, y, width, height)` on the drawing: the
        /// plane's extent.
        #[must_use]
        pub const fn window(&self) -> (u32, u32, u32, u32) {
            self.window
        }

        /// The push.
        #[must_use]
        pub const fn push(&self) -> [u32; WORDS] {
            let (x, _, width, height) = self.window;
            let mut words = [0; WORDS];
            words[LEFT as usize] = x;
            words[WIDTH as usize] = width;
            words[HEIGHT as usize] = height;
            words[EVEN_ODD as usize] = self.even_odd as u32;
            words
        }
    }

    /// `numerator / denominator` rounded half away from zero, both signed,
    /// and nought for a denominator of nought.
    fn rounded_div(
        f: &mut Function<'_>,
        kit: &Kit,
        numerator: Id,
        denominator: Id,
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let negative = kit.test(f, Op::S_LESS_THAN, denominator, zero)?;
        let flipped_numerator = kit.op(f, Op::I_SUB, zero, numerator)?;
        let flipped_denominator = kit.op(f, Op::I_SUB, zero, denominator)?;
        let numerator = kit.select(f, negative, flipped_numerator, numerator)?;
        let denominator = kit.select(f, negative, flipped_denominator, denominator)?;
        let half = kit.op_by(f, Op::S_DIV, denominator, 2)?;
        let below = kit.test(f, Op::S_LESS_THAN, numerator, zero)?;
        let lowered = kit.op(f, Op::I_SUB, numerator, half)?;
        let raised = kit.op(f, Op::I_ADD, numerator, half)?;
        let biased = kit.select(f, below, lowered, raised)?;
        let none = kit.equal(f, denominator, zero)?;
        let one = f.module().uint(1)?;
        let divisor = kit.select(f, none, one, denominator)?;
        let quotient = kit.op(f, Op::S_DIV, biased, divisor)?;
        kit.select(f, none, zero, quotient)
    }

    /// A piece of one row, travelling downward, relative to the window.
    #[derive(Copy, Clone)]
    struct Piece {
        from: (Id, Id),
        to: (Id, Id),
    }

    /// Where `piece` sits at `x`, held within its own rows.
    fn y_at(f: &mut Function<'_>, kit: &Kit, piece: Piece, x: Id) -> Result<Id, BuildError> {
        let run = kit.op(f, Op::I_SUB, piece.to.0, piece.from.0)?;
        let rise = kit.op(f, Op::I_SUB, piece.to.1, piece.from.1)?;
        let along = kit.op(f, Op::I_SUB, x, piece.from.0)?;
        let product = kit.op(f, Op::I_MUL, rise, along)?;
        let step = rounded_div(f, kit, product, run)?;
        let y = kit.op(f, Op::I_ADD, piece.from.1, step)?;
        let early = kit.test(f, Op::S_LESS_THAN, y, piece.from.1)?;
        let y = kit.select(f, early, piece.from.1, y)?;
        let late = kit.test(f, Op::S_GREATER_THAN, y, piece.to.1)?;
        let y = kit.select(f, late, piece.to.1, y)?;
        let zero = f.module().uint(0)?;
        let vertical = kit.equal(f, run, zero)?;
        kit.select(f, vertical, piece.from.1, y)
    }

    /// Accumulate into the `cover` and `area` locals what `piece`, of
    /// winding `sign`, adds to the pixel `column` of a window `right`
    /// sub-units wide and `cells` pixels: the converter's clip and cell walk,
    /// only as far as that pixel's own cell.
    fn accumulate(
        f: &mut Function<'_>,
        kit: &Kit,
        (cover, area): (Id, Id),
        (piece, sign): (Piece, Id),
        (column, right, cells): (Id, Id, Id),
    ) -> Result<(), BuildError> {
        let unit = f.module().uint(SCAN_UNIT)?;
        let zero = f.module().uint(0)?;
        let add = |f: &mut Function<'_>, slot: Id, value: Id| -> Result<(), BuildError> {
            let held = f.op(Op::LOAD, kit.uint, &[slot])?;
            let sum = kit.op(f, Op::I_ADD, held, value)?;
            f.emit(Op::STORE, &[slot, sum])
        };
        let signed = |f: &mut Function<'_>, value: Id| kit.op(f, Op::I_MUL, sign, value);
        let height = kit.op(f, Op::I_SUB, piece.to.1, piece.from.1)?;
        let vertical = kit.equal(f, piece.from.0, piece.to.0)?;
        let last_cell = kit.op_by(f, Op::I_SUB, cells, 1)?;
        let cell_of = |f: &mut Function<'_>, x: Id| -> Result<Id, BuildError> {
            let cell = kit.op(f, Op::U_DIV, x, unit)?;
            kit.min(f, cell, last_cell)
        };
        f.if_else(
            vertical,
            |f| {
                let left_of = kit.test(f, Op::S_LESS_THAN, piece.from.0, zero)?;
                let right_of = kit.test(f, Op::S_GREATER_THAN_EQUAL, piece.from.0, right)?;
                let cell = cell_of(f, piece.from.0)?;
                let reached = kit.test(f, Op::U_LESS_THAN_EQUAL, cell, column)?;
                let onto = kit.equal(f, cell, column)?;
                let inside = f.op(Op::LOGICAL_NOT, kit.boolean, &[right_of])?;
                let counted = f.op(Op::LOGICAL_AND, kit.boolean, &[inside, reached])?;
                let counted = f.op(Op::LOGICAL_OR, kit.boolean, &[left_of, counted])?;
                let rise = signed(f, height)?;
                let rise = kit.select(f, counted, rise, zero)?;
                add(f, cover, rise)?;
                let base = kit.op(f, Op::I_MUL, cell, unit)?;
                let offset = kit.op(f, Op::I_SUB, piece.from.0, base)?;
                let offset = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, offset, 1)?;
                let swept = kit.op(f, Op::I_MUL, height, offset)?;
                let swept = signed(f, swept)?;
                let own = f.op(Op::LOGICAL_AND, kit.boolean, &[inside, onto])?;
                let unclipped = f.op(Op::LOGICAL_NOT, kit.boolean, &[left_of])?;
                let own = f.op(Op::LOGICAL_AND, kit.boolean, &[own, unclipped])?;
                let swept = kit.select(f, own, swept, zero)?;
                add(f, area, swept)
            },
            |f| {
                let ascending = kit.test(f, Op::S_LESS_THAN, piece.from.0, piece.to.0)?;
                let low = kit.select(f, ascending, piece.from.0, piece.to.0)?;
                let high = kit.select(f, ascending, piece.to.0, piece.from.0)?;
                let gone_left = kit.test(f, Op::S_LESS_THAN_EQUAL, high, zero)?;
                let gone_right = kit.test(f, Op::S_GREATER_THAN_EQUAL, low, right)?;
                let carried_whole = signed(f, height)?;
                let carried_whole = kit.select(f, gone_left, carried_whole, zero)?;
                add(f, cover, carried_whole)?;
                let gone = f.op(Op::LOGICAL_OR, kit.boolean, &[gone_left, gone_right])?;
                let walks = f.op(Op::LOGICAL_NOT, kit.boolean, &[gone])?;
                f.if_then(walks, |f| {
                    walk(
                        f,
                        kit,
                        (cover, area),
                        (piece, sign, ascending),
                        (column, right, cells),
                        (low, high),
                    )
                })
            },
        )
    }

    /// [`accumulate`] for a piece that is neither vertical nor wholly beside
    /// the window: clipped to it, the part left of it carried, then walked as
    /// far as the pixel `column`'s own cell.
    fn walk(
        f: &mut Function<'_>,
        kit: &Kit,
        (cover, area): (Id, Id),
        (piece, sign, ascending): (Piece, Id, Id),
        (column, right, cells): (Id, Id, Id),
        (low, high): (Id, Id),
    ) -> Result<(), BuildError> {
        let unit = f.module().uint(SCAN_UNIT)?;
        let zero = f.module().uint(0)?;
        let add = |f: &mut Function<'_>, slot: Id, value: Id| -> Result<(), BuildError> {
            let held = f.op(Op::LOAD, kit.uint, &[slot])?;
            let sum = kit.op(f, Op::I_ADD, held, value)?;
            f.emit(Op::STORE, &[slot, sum])
        };
        let signed = |f: &mut Function<'_>, value: Id| kit.op(f, Op::I_MUL, sign, value);
        let last_cell = kit.op_by(f, Op::I_SUB, cells, 1)?;
        let cell_of = |f: &mut Function<'_>, x: Id| -> Result<Id, BuildError> {
            let cell = kit.op(f, Op::U_DIV, x, unit)?;
            kit.min(f, cell, last_cell)
        };
        // Clipped at the window's left edge, the part left of it carried.
        let clips_left = kit.test(f, Op::S_LESS_THAN, low, zero)?;
        let at_left = y_at(f, kit, piece, zero)?;
        let carried_up = kit.op(f, Op::I_SUB, at_left, piece.from.1)?;
        let carried_down = kit.op(f, Op::I_SUB, piece.to.1, at_left)?;
        let carried = kit.select(f, ascending, carried_up, carried_down)?;
        let carried = signed(f, carried)?;
        let carried = kit.select(f, clips_left, carried, zero)?;
        add(f, cover, carried)?;
        let from_x = kit.select(f, ascending, zero, piece.from.0)?;
        let from_y = kit.select(f, ascending, at_left, piece.from.1)?;
        let to_x = kit.select(f, ascending, piece.to.0, zero)?;
        let to_y = kit.select(f, ascending, piece.to.1, at_left)?;
        let left_clipped = Piece {
            from: (
                kit.select(f, clips_left, from_x, piece.from.0)?,
                kit.select(f, clips_left, from_y, piece.from.1)?,
            ),
            to: (
                kit.select(f, clips_left, to_x, piece.to.0)?,
                kit.select(f, clips_left, to_y, piece.to.1)?,
            ),
        };
        // Clipped at its right edge, the part right of it dropped.
        let clips_right = kit.test(f, Op::S_GREATER_THAN, high, right)?;
        let at_right = y_at(f, kit, left_clipped, right)?;
        let to_x = kit.select(f, ascending, right, left_clipped.to.0)?;
        let to_y = kit.select(f, ascending, at_right, left_clipped.to.1)?;
        let from_x = kit.select(f, ascending, left_clipped.from.0, right)?;
        let from_y = kit.select(f, ascending, left_clipped.from.1, at_right)?;
        let walked = Piece {
            from: (
                kit.select(f, clips_right, from_x, left_clipped.from.0)?,
                kit.select(f, clips_right, from_y, left_clipped.from.1)?,
            ),
            to: (
                kit.select(f, clips_right, to_x, left_clipped.to.0)?,
                kit.select(f, clips_right, to_y, left_clipped.to.1)?,
            ),
        };
        let first = cell_of(f, walked.from.0)?;
        let last = cell_of(f, walked.to.0)?;
        let base = kit.op(f, Op::I_MUL, column, unit)?;
        let next = kit.op(f, Op::I_ADD, base, unit)?;
        let at_base = y_at(f, kit, walked, base)?;
        let at_next = y_at(f, kit, walked, next)?;
        // The cells up to this pixel's own hold every part of the
        // piece left of its right boundary.
        let lowest = kit.min(f, first, last)?;
        let highest = f.glsl(tairix_spirv::GlslStd450::U_MAX, kit.uint, &[first, last])?;
        let all = kit.test(f, Op::U_LESS_THAN_EQUAL, highest, column)?;
        let none = kit.test(f, Op::U_GREATER_THAN, lowest, column)?;
        let rising_part = kit.op(f, Op::I_SUB, at_next, walked.from.1)?;
        let falling_part = kit.op(f, Op::I_SUB, walked.to.1, at_next)?;
        let part = kit.select(f, ascending, rising_part, falling_part)?;
        let whole = kit.op(f, Op::I_SUB, walked.to.1, walked.from.1)?;
        let part = kit.select(f, all, whole, part)?;
        let part = kit.select(f, none, zero, part)?;
        let part = signed(f, part)?;
        add(f, cover, part)?;
        // This pixel's own cell: the piece from where it enters to where
        // it leaves.
        let at_first = kit.equal(f, column, first)?;
        let at_last = kit.equal(f, column, last)?;
        let entry_x = kit.select(f, ascending, base, next)?;
        let entry_y = kit.select(f, ascending, at_base, at_next)?;
        let exit_x = kit.select(f, ascending, next, base)?;
        let exit_y = kit.select(f, ascending, at_next, at_base)?;
        let entry_x = kit.select(f, at_first, walked.from.0, entry_x)?;
        let entry_y = kit.select(f, at_first, walked.from.1, entry_y)?;
        let exit_x = kit.select(f, at_last, walked.to.0, exit_x)?;
        let exit_y = kit.select(f, at_last, walked.to.1, exit_y)?;
        let below_high = kit.test(f, Op::U_LESS_THAN_EQUAL, column, highest)?;
        let above_low = kit.test(f, Op::U_GREATER_THAN_EQUAL, column, lowest)?;
        let own = f.op(Op::LOGICAL_AND, kit.boolean, &[below_high, above_low])?;
        let rise = kit.op(f, Op::I_SUB, exit_y, entry_y)?;
        let entered = kit.op(f, Op::I_SUB, entry_x, base)?;
        let left_at = kit.op(f, Op::I_SUB, exit_x, base)?;
        let breadth = kit.op(f, Op::I_ADD, entered, left_at)?;
        let swept = kit.op(f, Op::I_MUL, rise, breadth)?;
        let swept = signed(f, swept)?;
        let swept = kit.select(f, own, swept, zero)?;
        add(f, area, swept)
    }

    /// `tairix_raster`'s fill rule: the alpha a pixel that accumulated
    /// `signed` area takes.
    fn rule_alpha(
        f: &mut Function<'_>,
        kit: &Kit,
        signed: Id,
        even_odd: Id,
    ) -> Result<Id, BuildError> {
        let unit = SCAN_UNIT;
        let full = f.module().uint(2 * unit * unit)?;
        let twice = f.module().uint(4 * unit * unit)?;
        let zero = f.module().uint(0)?;
        let negative = kit.test(f, Op::S_LESS_THAN, signed, zero)?;
        let flipped = kit.op(f, Op::I_SUB, zero, signed)?;
        let magnitude = kit.select(f, negative, flipped, signed)?;
        let non_zero = kit.min(f, magnitude, full)?;
        let wrapped = kit.op(f, Op::S_MOD, signed, twice)?;
        let folded = kit.op(f, Op::I_SUB, twice, wrapped)?;
        let past = kit.test(f, Op::S_GREATER_THAN, wrapped, full)?;
        let odd = kit.select(f, past, folded, wrapped)?;
        let on = kit.test(f, Op::I_NOT_EQUAL, even_odd, zero)?;
        let covered = kit.select(f, on, odd, non_zero)?;
        let weighed = kit.op_by(f, Op::I_MUL, covered, 255)?;
        let rounded = kit.op_by(f, Op::I_ADD, weighed, unit * unit)?;
        let level = kit.op(f, Op::U_DIV, rounded, full)?;
        let most = f.module().uint(255)?;
        kit.min(f, level, most)
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let plane = pass.buffer(PLANE)?;
        let pieces = pass.buffer(PIECES)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let unit = f.module().uint(SCAN_UNIT)?;
            let left = kit.field(f, LEFT)?;
            let cells = kit.field(f, WIDTH)?;
            let origin = kit.op(f, Op::I_MUL, left, unit)?;
            let right = kit.op(f, Op::I_MUL, cells, unit)?;
            let first = kit.load(f, pieces, y)?;
            let next_row = kit.op_by(f, Op::I_ADD, y, 1)?;
            let after = kit.load(f, pieces, next_row)?;
            let count = kit.op(f, Op::I_SUB, after, first)?;
            let height = kit.field(f, HEIGHT)?;
            let table = kit.op_by(f, Op::I_ADD, height, 1)?;
            let zero = f.module().uint(0)?;
            let (cover, area) = (kit.local(f, zero)?, kit.local(f, zero)?);
            kit.repeat(f, count, |f, step| {
                let index = kit.op(f, Op::I_ADD, first, step)?;
                let start = kit.op_by(f, Op::I_MUL, index, PIECE_WORDS)?;
                let start = kit.op(f, Op::I_ADD, table, start)?;
                let mut fields = [start; PIECE_WORDS as usize];
                for (slot, offset) in fields.iter_mut().zip(0u32..) {
                    let at = kit.op_by(f, Op::I_ADD, start, offset)?;
                    *slot = kit.load(f, pieces, at)?;
                }
                let [from_x, from_y, to_x, to_y, winding] = fields;
                let piece = Piece {
                    from: (kit.op(f, Op::I_SUB, from_x, origin)?, from_y),
                    to: (kit.op(f, Op::I_SUB, to_x, origin)?, to_y),
                };
                accumulate(f, kit, (cover, area), (piece, winding), (x, right, cells))
            })?;
            let cover = f.op(Op::LOAD, kit.uint, &[cover])?;
            let area = f.op(Op::LOAD, kit.uint, &[area])?;
            let scaled = kit.op_by(f, Op::I_MUL, cover, 2 * SCAN_UNIT)?;
            let signed = kit.op(f, Op::I_SUB, scaled, area)?;
            let even_odd = kit.field(f, EVEN_ODD)?;
            let alpha = rule_alpha(f, kit, signed, even_odd)?;
            let start = kit.op(f, Op::I_MUL, y, cells)?;
            let at = kit.op(f, Op::I_ADD, start, x)?;
            kit.store(f, plane, at, alpha)
        })
    }
}
