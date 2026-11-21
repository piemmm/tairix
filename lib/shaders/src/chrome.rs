//! The compositor's chrome: a window's body laid over the frame and the
//! drop shadow it casts beneath it — `tairix_raster::Body` through
//! `Pixel::laid_over`, and `tairix_raster::shadow_row`, pixel for pixel.
//!
//! Each pass lays one window over one rectangle of the frame, which its push
//! names ([`Push`]). A window's body is described in a storage buffer of
//! words the CPU fills ([`body::Layout`]), more than portable push constants
//! hold; a caster's shadow is described in the push itself, beside the
//! levels' tables laid out once ([`shadow::Tables`]). A description that
//! names geometry past a bound keeping the pass's arithmetic in a word is
//! refused rather than wrapped ([`Refused`]).

use tairix_spirv::{Admitted, BuildError, Function, Id, Op};

pub use crate::kit::Refused;
use crate::kit::{pixel_word, within_extent, within_surface, Kit, Pass, Place};

/// The binding of the frame both passes lay over, read and written.
pub const TARGET: u32 = 0;

const STRIDE: u32 = 0;
const LEFT: u32 = 1;
const TOP: u32 = 2;
pub(crate) const WIDTH: u32 = 3;
pub(crate) const HEIGHT: u32 = 4;
const DITHERED: u32 = 5;
/// The frame's push fields, which every chrome and paint pass's push starts
/// with.
pub(crate) const FRAME_FIELDS: u32 = DITHERED + 1;
/// The push constants' words.
pub const WORDS: usize = FRAME_FIELDS as usize;

/// The rectangle of the surface a chrome or paint pass lays over: the frame,
/// or any surface a pass paints.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Push {
    /// Words a row of the frame.
    pub stride: u32,
    /// The rectangle's leftmost column on the frame: where its dither starts.
    pub left: u32,
    /// The rectangle's top row on the frame.
    pub top: u32,
    /// The rectangle's width.
    pub width: u32,
    /// The rectangle's height.
    pub height: u32,
    /// Whether to round at the frame's ordered dither rather than to
    /// nearest.
    pub dithered: bool,
}

impl Push {
    /// The push constants, in the modules' order.
    #[must_use]
    pub const fn words(&self) -> [u32; WORDS] {
        let mut words = [0; WORDS];
        words[STRIDE as usize] = self.stride;
        words[LEFT as usize] = self.left;
        words[TOP as usize] = self.top;
        words[WIDTH as usize] = self.width;
        words[HEIGHT as usize] = self.height;
        words[DITHERED as usize] = self.dithered as u32;
        words
    }
}

/// One invocation's pixel on the frame, the word it sits at, and the bias it
/// rounds at.
pub(crate) struct Framed {
    pub(crate) x: Id,
    pub(crate) y: Id,
    pub(crate) at: Id,
    pub(crate) bias: Id,
}

/// The frame pixel invocation `place` lays.
pub(crate) fn framed(
    f: &mut Function<'_>,
    kit: &Kit,
    Place { x, y }: Place,
) -> Result<Framed, BuildError> {
    let frame = (
        kit.field(f, STRIDE)?,
        kit.field(f, LEFT)?,
        kit.field(f, TOP)?,
    );
    let at = kit.index(f, frame, (x, y))?;
    let x = kit.op(f, Op::I_ADD, frame.1, x)?;
    let y = kit.op(f, Op::I_ADD, frame.2, y)?;
    let dithered = kit.field(f, DITHERED)?;
    let bias = kit.bias(f, dithered, (x, y))?;
    Ok(Framed { x, y, at, bias })
}

/// One window's body: its client's pixels within their drawn extent, the
/// plate laid past them on the client's rows, its four furniture strips, the
/// client's rounded cut and the window's opacity. A pixel is what
/// `tairix_raster::Body` lays there, laid over the frame through
/// `Pixel::laid_over`; a source whose alpha is nought lays nothing.
pub mod body {
    use tairix_raster::{Pixel, RoundRect};

    use super::{
        framed, pixel_word, within_extent, within_surface, Admitted, BuildError, Id, Op, Pass,
        Refused, TARGET,
    };
    use crate::MAX_PASS_RADIUS;

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/body.spv");

    /// The binding of the client's pixels: every one the layout's client
    /// names, and at least one word.
    pub const CLIENT: u32 = 1;
    /// The binding of the furniture's pixels: every strip's, where the
    /// layout places it, and at least one word.
    pub const FURNITURE: u32 = 2;
    /// The binding of the window's [`Layout`].
    pub const LAYOUT: u32 = 3;

    const ORIGIN_X: u32 = 0;
    const ORIGIN_Y: u32 = 1;
    const OPACITY: u32 = 2;
    const CLIENT_AT: u32 = 3;
    const DRAWN_WIDTH: u32 = 9;
    const DRAWN_HEIGHT: u32 = 10;
    const BACKDROP: u32 = 11;
    const PLATED: u32 = 12;
    const CUT_RADIUS: u32 = 13;
    const CUT_WIDTH: u32 = 14;
    const CUT_HEIGHT: u32 = 15;
    const CUT_X: u32 = 16;
    const CUT_Y: u32 = 17;
    const FURNITURE_AT: u32 = 18;
    /// A [`Strip`]'s words: its place, its extent, its base and its stride.
    const STRIP_WORDS: u32 = 6;
    /// The layout's words.
    pub const LAYOUT_WORDS: usize = 42;

    /// A rectangle of a window's own pixels: where it sits in the window's
    /// outer rectangle, its extent, and where its pixels lie in their buffer.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub struct Strip {
        /// Its leftmost column, from the window's outer left.
        pub x: u32,
        /// Its top row, from the window's outer top.
        pub y: u32,
        /// Its width; nought holds nothing.
        pub width: u32,
        /// Its height; nought holds nothing.
        pub height: u32,
        /// The word its first pixel is at in its buffer.
        pub base: u32,
        /// Words a row of its pixels.
        pub stride: u32,
    }

    impl Strip {
        /// Its words, in the module's order.
        const fn words(&self) -> [u32; STRIP_WORDS as usize] {
            [
                self.x,
                self.y,
                self.width,
                self.height,
                self.base,
                self.stride,
            ]
        }

        /// Whether its geometry is within the bounds the pass computes in.
        fn bounded(&self) -> bool {
            within_extent(&[self.x, self.y, self.width, self.height, self.stride])
                && within_surface(self.base, self.stride, self.height)
        }
    }

    /// What a window lays: everything a pixel of its body is resolved from.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Layout {
        /// Its outer top left on the frame.
        pub origin: (i32, i32),
        /// The opacity it is drawn at.
        pub opacity: u8,
        /// The columns and rows its client owns, and where the client's
        /// pixels lie in their buffer.
        pub client: Strip,
        /// How many of the client's columns and rows its pixels reach; the
        /// plate lies past them.
        pub drawn: (u32, u32),
        /// The plate laid where the client's pixels do not reach, if any.
        pub backdrop: Option<Pixel>,
        /// The rounded rectangle the client's pixels are cut to, and the
        /// client's top left within it; `None` where they are not cut.
        pub cut: Option<(RoundRect, (u32, u32))>,
        /// The furniture: its top band, its bottom band, and its side strips
        /// left and right. At a pixel more than one holds, the first does.
        pub furniture: [Strip; 4],
    }

    impl Layout {
        /// The layout's words, in the module's order.
        ///
        /// # Errors
        ///
        /// [`Refused::Radius`] for a cut rounded past [`MAX_PASS_RADIUS`], and
        /// [`Refused::Extent`] for an extent, offset or stride past
        /// `MAX_DRAWING_EXTENT` or a strip's pixels past
        /// `MAX_SURFACE_PIXELS` words.
        pub fn words(&self) -> Result<[u32; LAYOUT_WORDS], Refused> {
            let (shape, (cut_x, cut_y)) = self.cut.unwrap_or((
                RoundRect {
                    width: 0,
                    height: 0,
                    radius: 0,
                },
                (0, 0),
            ));
            if shape.radius > MAX_PASS_RADIUS {
                return Err(Refused::Radius);
            }
            let bounded = self.client.bounded()
                && self.furniture.iter().all(Strip::bounded)
                && within_extent(&[
                    self.drawn.0,
                    self.drawn.1,
                    shape.width,
                    shape.height,
                    cut_x,
                    cut_y,
                ]);
            if !bounded {
                return Err(Refused::Extent);
            }
            let mut words = [0; LAYOUT_WORDS];
            words[ORIGIN_X as usize] = self.origin.0.cast_unsigned();
            words[ORIGIN_Y as usize] = self.origin.1.cast_unsigned();
            words[OPACITY as usize] = u32::from(self.opacity);
            let at = CLIENT_AT as usize;
            words[at..at + STRIP_WORDS as usize].copy_from_slice(&self.client.words());
            words[DRAWN_WIDTH as usize] = self.drawn.0;
            words[DRAWN_HEIGHT as usize] = self.drawn.1;
            if let Some(backdrop) = self.backdrop {
                words[BACKDROP as usize] = pixel_word(backdrop);
                words[PLATED as usize] = 1;
            }
            words[CUT_RADIUS as usize] = shape.radius;
            words[CUT_WIDTH as usize] = shape.width;
            words[CUT_HEIGHT as usize] = shape.height;
            words[CUT_X as usize] = cut_x;
            words[CUT_Y as usize] = cut_y;
            for (strip, at) in self
                .furniture
                .iter()
                .zip((FURNITURE_AT as usize..).step_by(STRIP_WORDS as usize))
            {
                words[at..at + STRIP_WORDS as usize].copy_from_slice(&strip.words());
            }
            Ok(words)
        }
    }

    /// Where `(x, y)` of the window's outer rectangle falls in the strip whose
    /// words start at `at`: whether it is inside, and the word it reads.
    fn strip_at(
        f: &mut tairix_spirv::Function<'_>,
        kit: &crate::kit::Kit,
        (layout, at): (Id, u32),
        (x, y): (Id, Id),
    ) -> Result<(Id, Id), BuildError> {
        let [left, top, width, height, base, stride] = [0, 1, 2, 3, 4, 5].map(|field| at + field);
        let column = {
            let left = kit.word(f, layout, left)?;
            kit.op(f, Op::I_SUB, x, left)?
        };
        let row = {
            let top = kit.word(f, layout, top)?;
            kit.op(f, Op::I_SUB, y, top)?
        };
        let width = kit.word(f, layout, width)?;
        let height = kit.word(f, layout, height)?;
        let across = kit.test(f, Op::U_LESS_THAN, column, width)?;
        let down = kit.test(f, Op::U_LESS_THAN, row, height)?;
        let inside = f.op(Op::LOGICAL_AND, kit.boolean, &[across, down])?;
        let stride = kit.word(f, layout, stride)?;
        let base = kit.word(f, layout, base)?;
        let start = kit.op(f, Op::I_MUL, row, stride)?;
        let start = kit.op(f, Op::I_ADD, base, start)?;
        let word = kit.op(f, Op::I_ADD, start, column)?;
        Ok((inside, word))
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(super::WORDS)?;
        let target = pass.buffer(TARGET)?;
        let (client, furniture) = (pass.buffer(CLIENT)?, pass.buffer(FURNITURE)?);
        let layout = pass.buffer(LAYOUT)?;
        pass.finish((super::WIDTH, super::HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let origin_x = kit.word(f, layout, ORIGIN_X)?;
            let origin_y = kit.word(f, layout, ORIGIN_Y)?;
            let lx = kit.op(f, Op::I_SUB, framed.x, origin_x)?;
            let ly = kit.op(f, Op::I_SUB, framed.y, origin_y)?;
            let zero = f.module().uint(0)?;

            let (in_client, client_word) = strip_at(f, kit, (layout, CLIENT_AT), (lx, ly))?;
            let cx = {
                let left = kit.word(f, layout, CLIENT_AT)?;
                kit.op(f, Op::I_SUB, lx, left)?
            };
            let cy = {
                let top = kit.word(f, layout, CLIENT_AT + 1)?;
                kit.op(f, Op::I_SUB, ly, top)?
            };
            let drawn_width = kit.word(f, layout, DRAWN_WIDTH)?;
            let drawn_height = kit.word(f, layout, DRAWN_HEIGHT)?;
            let across = kit.test(f, Op::U_LESS_THAN, cx, drawn_width)?;
            let down = kit.test(f, Op::U_LESS_THAN, cy, drawn_height)?;
            let drawn = f.op(Op::LOGICAL_AND, kit.boolean, &[across, down])?;
            let read = f.op(Op::LOGICAL_AND, kit.boolean, &[in_client, drawn])?;
            let client_word = kit.select(f, read, client_word, zero)?;
            let own = kit.load(f, client, client_word)?;
            let backdrop = kit.word(f, layout, BACKDROP)?;
            let plated = kit.word(f, layout, PLATED)?;
            let plated = kit.test(f, Op::I_NOT_EQUAL, plated, zero)?;
            let client_pixel = kit.select(f, drawn, own, backdrop)?;
            let held = f.op(Op::LOGICAL_OR, kit.boolean, &[drawn, plated])?;
            let client_held = f.op(Op::LOGICAL_AND, kit.boolean, &[in_client, held])?;
            let cut_x = kit.word(f, layout, CUT_X)?;
            let cut_y = kit.word(f, layout, CUT_Y)?;
            let shape_x = kit.op(f, Op::I_ADD, cx, cut_x)?;
            let shape_y = kit.op(f, Op::I_ADD, cy, cut_y)?;
            let shape = (
                kit.word(f, layout, CUT_WIDTH)?,
                kit.word(f, layout, CUT_HEIGHT)?,
                kit.word(f, layout, CUT_RADIUS)?,
            );
            let coverage = kit.coverage(f, (shape_x, shape_y), shape)?;

            let last = FURNITURE_AT + 3 * STRIP_WORDS;
            let (mut furniture_held, mut furniture_word) =
                strip_at(f, kit, (layout, last), (lx, ly))?;
            for strip in (0..3).rev() {
                let at = FURNITURE_AT + strip * STRIP_WORDS;
                let (inside, word) = strip_at(f, kit, (layout, at), (lx, ly))?;
                furniture_word = kit.select(f, inside, word, furniture_word)?;
                furniture_held = f.op(Op::LOGICAL_OR, kit.boolean, &[inside, furniture_held])?;
            }
            let furniture_word = kit.select(f, furniture_held, furniture_word, zero)?;
            let furniture_pixel = kit.load(f, furniture, furniture_word)?;

            let full = f.module().uint(255)?;
            let opacity = kit.word(f, layout, OPACITY)?;
            let covered = kit.equal(f, coverage, full)?;
            let product = kit.op(f, Op::I_MUL, opacity, coverage)?;
            let nearest = f.module().uint(tairix_raster::ROUND_NEAREST)?;
            let weakened = kit.div255(f, product, nearest)?;
            let client_factor = kit.select(f, covered, opacity, weakened)?;
            let cut = f.op(Op::LOGICAL_NOT, kit.boolean, &[covered])?;
            let outside = f.op(Op::LOGICAL_NOT, kit.boolean, &[in_client])?;
            let gives_way = f.op(Op::LOGICAL_OR, kit.boolean, &[outside, cut])?;
            let use_furniture = f.op(Op::LOGICAL_AND, kit.boolean, &[furniture_held, gives_way])?;
            let unhidden = f.op(Op::LOGICAL_NOT, kit.boolean, &[furniture_held])?;
            let first = f.op(Op::LOGICAL_OR, kit.boolean, &[covered, unhidden])?;
            let use_client = f.op(Op::LOGICAL_AND, kit.boolean, &[client_held, first])?;
            let source = kit.select(f, use_furniture, furniture_pixel, client_pixel)?;
            let factor = kit.select(f, use_furniture, opacity, client_factor)?;
            let laid = f.op(Op::LOGICAL_OR, kit.boolean, &[use_furniture, use_client])?;
            let alpha = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, source, 24)?;
            let seen = kit.test(f, Op::I_NOT_EQUAL, alpha, zero)?;
            let lays = f.op(Op::LOGICAL_AND, kit.boolean, &[laid, seen])?;
            let skipped = f.op(Op::LOGICAL_NOT, kit.boolean, &[lays])?;
            f.if_then(skipped, |f| f.emit(Op::RETURN, &[]))?;
            let under = kit.load(f, target, framed.at)?;
            let scaled = kit.scale(f, source, factor, framed.bias)?;
            let result = kit.over(f, scaled, under, framed.bias)?;
            kit.store(f, target, framed.at, result)
        })
    }
}

/// One caster's drop shadow, at either of two levels and easing between
/// them: a pixel is the shadow `tairix_raster::shadow_row` lays there. The
/// levels' kernels and notches are laid out once, in [`Tables`](shadow::Tables),
/// and each caster is described in the pass's push, by
/// [`Tables::push`](shadow::Tables::push).
pub mod shadow {
    use alloc::vec::Vec;

    use tairix_raster::{Caster, DropShadow, RoundRect, ROUND_NEAREST, SHADOW_ONE, SHADOW_SHIFT};
    use tairix_util::fallible;

    use super::{
        framed, pixel_word, within_extent, Admitted, BuildError, Function, Id, Kit, Op, Pass,
        Refused, FRAME_FIELDS as FRAME, TARGET, WORDS as FRAME_WORDS,
    };
    use crate::{MAX_PASS_COORDINATE, MAX_PASS_RADIUS};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/shadow.spv");

    /// The binding of the levels' tables, [`Tables::words`].
    pub const TABLES: u32 = 1;

    const LEFT_EDGE: u32 = FRAME;
    const TOP_EDGE: u32 = FRAME + 1;
    const RIGHT_EDGE: u32 = FRAME + 2;
    const BOTTOM_EDGE: u32 = FRAME + 3;
    const OPACITY: u32 = FRAME + 4;
    const LIFT: u32 = FRAME + 5;
    const SILHOUETTE_RADIUS: u32 = FRAME + 6;
    const SILHOUETTE_WIDTH: u32 = FRAME + 7;
    const SILHOUETTE_HEIGHT: u32 = FRAME + 8;
    /// Where the lifted level's words start; the resting level's follow.
    const LEVELS: u32 = FRAME + 9;
    const REACH: u32 = 0;
    const COLOR: u32 = 1;
    const RAMP: u32 = 2;
    const NOTCH_SIDE: u32 = 3;
    const NOTCH: u32 = 4;
    /// A level's words.
    const LEVEL_WORDS: u32 = 5;
    /// The push constants' words: the frame's rectangle, then the caster.
    pub const WORDS: usize = (LEVELS + 2 * LEVEL_WORDS) as usize;

    /// Where one level's tables lie in [`Tables::words`].
    struct Placed {
        reach: u32,
        color: u32,
        ramp: u32,
        /// Each notch held: the radius it is cut by, its side, and its first
        /// word.
        notches: Vec<(u32, u32, u32)>,
    }

    /// The two levels' kernels and notch tiles as the pass reads them, laid
    /// out once for as long as the levels stand: a theme or scale changing,
    /// or a notch built or dropped, lays them out afresh.
    pub struct Tables {
        words: Vec<u32>,
        levels: [Placed; 2],
    }

    impl Tables {
        /// The tables of `lifted` and `resting`. Word nought is a nought,
        /// where a level holding no table points, so a masked read of a level
        /// that casts nothing stays within the buffer.
        ///
        /// # Errors
        ///
        /// [`Refused::OutOfMemory`] when the allocator refuses the words.
        pub fn new(lifted: &DropShadow, resting: &DropShadow) -> Result<Self, Refused> {
            let count: usize = [lifted, resting]
                .iter()
                .map(|level| {
                    level.ramp().len()
                        + level
                            .notches()
                            .map(|(_, notch)| notch.values.len())
                            .sum::<usize>()
                })
                .sum();
            let mut words = Vec::new();
            if !fallible::reserve(&mut words, count + 1) {
                return Err(Refused::OutOfMemory);
            }
            words.push(0);
            let mut place = |level: &DropShadow| -> Result<Placed, Refused> {
                let at =
                    |words: &Vec<u32>| u32::try_from(words.len()).map_err(|_| Refused::OutOfMemory);
                let ramp = if level.ramp().is_empty() {
                    0
                } else {
                    at(&words)?
                };
                words.extend_from_slice(level.ramp());
                let mut notches = Vec::new();
                if !fallible::reserve(&mut notches, level.notches().count()) {
                    return Err(Refused::OutOfMemory);
                }
                for (radius, notch) in level.notches() {
                    notches.push((radius, notch.side, at(&words)?));
                    words.extend(notch.values.iter().map(|&value| u32::from(value)));
                }
                Ok(Placed {
                    reach: level.reach(),
                    color: pixel_word(level.color()),
                    ramp,
                    notches,
                })
            };
            let levels = [place(lifted)?, place(resting)?];
            Ok(Self { words, levels })
        }

        /// The words, bound at [`TABLES`].
        #[must_use]
        pub fn words(&self) -> &[u32] {
            &self.words
        }

        /// The push of the pass laying, over `frame`'s rectangle, the shadow
        /// `caster` casts while it stands `lift` of the way from the resting
        /// level to the lifted one.
        ///
        /// # Errors
        ///
        /// [`Refused::Coordinate`] for a caster's edge past
        /// [`MAX_PASS_COORDINATE`] of the origin, and [`Refused::Radius`] and
        /// [`Refused::Extent`] for a silhouette rounded past
        /// [`MAX_PASS_RADIUS`] or larger than `MAX_DRAWING_EXTENT`.
        pub fn push(
            &self,
            frame: &super::Push,
            caster: &Caster,
            lift: u8,
        ) -> Result<[u32; WORDS], Refused> {
            let edges = [caster.left, caster.top, caster.right, caster.bottom];
            if edges
                .iter()
                .any(|edge| edge.unsigned_abs() > MAX_PASS_COORDINATE.unsigned_abs())
            {
                return Err(Refused::Coordinate);
            }
            let silhouette = caster.silhouette.unwrap_or(RoundRect {
                width: 0,
                height: 0,
                radius: 0,
            });
            if silhouette.radius > MAX_PASS_RADIUS {
                return Err(Refused::Radius);
            }
            if !within_extent(&[silhouette.width, silhouette.height]) {
                return Err(Refused::Extent);
            }
            let corners = caster.silhouette.map_or(0, RoundRect::corner_reach);
            let mut words = [0; WORDS];
            words[..FRAME_WORDS].copy_from_slice(&frame.words());
            for (slot, edge) in words[LEFT_EDGE as usize..].iter_mut().zip(edges) {
                *slot = edge.cast_unsigned();
            }
            words[OPACITY as usize] = u32::from(caster.opacity);
            words[LIFT as usize] = u32::from(lift);
            words[SILHOUETTE_RADIUS as usize] = silhouette.radius;
            words[SILHOUETTE_WIDTH as usize] = silhouette.width;
            words[SILHOUETTE_HEIGHT as usize] = silhouette.height;
            for (level, at) in self
                .levels
                .iter()
                .zip((LEVELS..).step_by(LEVEL_WORDS as usize))
            {
                let (side, notch) = level
                    .notches
                    .iter()
                    .find(|(radius, _, _)| *radius == corners)
                    .map_or((0, 0), |&(_, side, at)| (side, at));
                let at = at as usize;
                words[at + REACH as usize] = level.reach;
                words[at + COLOR as usize] = level.color;
                words[at + RAMP as usize] = level.ramp;
                words[at + NOTCH_SIDE as usize] = side;
                words[at + NOTCH as usize] = notch;
            }
            Ok(words)
        }
    }

    /// The caster's geometry as one invocation reads it.
    struct Geometry {
        left: Id,
        top: Id,
        right: Id,
        bottom: Id,
        opacity: Id,
        /// Whether the caster casts anything at all at either level.
        casts: Id,
        /// How much of this invocation's pixel the silhouette covers.
        covered: Id,
    }

    /// The caster's geometry, and how much of pixel (`x`, `y`) its silhouette
    /// covers: what both levels share.
    fn geometry(f: &mut Function<'_>, kit: &Kit, (x, y): (Id, Id)) -> Result<Geometry, BuildError> {
        let left = kit.field(f, LEFT_EDGE)?;
        let top = kit.field(f, TOP_EDGE)?;
        let right = kit.field(f, RIGHT_EDGE)?;
        let bottom = kit.field(f, BOTTOM_EDGE)?;
        let opacity = kit.field(f, OPACITY)?;
        let zero = f.module().uint(0)?;
        let wide = kit.test(f, Op::S_LESS_THAN, left, right)?;
        let tall = kit.test(f, Op::S_LESS_THAN, top, bottom)?;
        let seen = kit.test(f, Op::I_NOT_EQUAL, opacity, zero)?;
        let area = f.op(Op::LOGICAL_AND, kit.boolean, &[wide, tall])?;
        let casts = f.op(Op::LOGICAL_AND, kit.boolean, &[area, seen])?;
        let after_left = kit.test(f, Op::S_GREATER_THAN_EQUAL, x, left)?;
        let before_right = kit.test(f, Op::S_LESS_THAN, x, right)?;
        let after_top = kit.test(f, Op::S_GREATER_THAN_EQUAL, y, top)?;
        let before_bottom = kit.test(f, Op::S_LESS_THAN, y, bottom)?;
        let across = f.op(Op::LOGICAL_AND, kit.boolean, &[after_left, before_right])?;
        let down = f.op(Op::LOGICAL_AND, kit.boolean, &[after_top, before_bottom])?;
        let within = f.op(Op::LOGICAL_AND, kit.boolean, &[across, down])?;
        let lx = kit.op(f, Op::I_SUB, x, left)?;
        let ly = kit.op(f, Op::I_SUB, y, top)?;
        let silhouette = (
            kit.field(f, SILHOUETTE_WIDTH)?,
            kit.field(f, SILHOUETTE_HEIGHT)?,
            kit.field(f, SILHOUETTE_RADIUS)?,
        );
        let coverage = kit.coverage(f, (lx, ly), silhouette)?;
        let covered = kit.select(f, within, coverage, zero)?;
        Ok(Geometry {
            left,
            top,
            right,
            bottom,
            opacity,
            casts,
            covered,
        })
    }

    /// One level's words, read where they start in the push.
    struct Level {
        reach: Id,
        color: Id,
        ramp: Id,
        side: Id,
        notch: Id,
    }

    fn level(f: &mut Function<'_>, kit: &Kit, at: u32) -> Result<Level, BuildError> {
        Ok(Level {
            reach: kit.field(f, at + REACH)?,
            color: kit.field(f, at + COLOR)?,
            ramp: kit.field(f, at + RAMP)?,
            side: kit.field(f, at + NOTCH_SIDE)?,
            notch: kit.field(f, at + NOTCH)?,
        })
    }

    /// The kernel's running sum up to the signed offset `d`: nothing before
    /// the reach, all of it past.
    fn cum(
        f: &mut Function<'_>,
        kit: &Kit,
        (tables, level): (Id, &Level),
        d: Id,
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let low = kit.op(f, Op::I_SUB, zero, level.reach)?;
        let before = kit.test(f, Op::S_LESS_THAN, d, low)?;
        let index = kit.op(f, Op::I_ADD, d, level.reach)?;
        let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, level.reach, 1)?;
        let taps = kit.op_by(f, Op::I_ADD, doubled, 1)?;
        let past = kit.test(f, Op::U_GREATER_THAN_EQUAL, index, taps)?;
        let outside = f.op(Op::LOGICAL_OR, kit.boolean, &[before, past])?;
        let held = kit.select(f, outside, zero, index)?;
        let entry = kit.op(f, Op::I_ADD, level.ramp, held)?;
        let value = kit.load(f, tables, entry)?;
        let one = f.module().uint(SHADOW_ONE)?;
        let value = kit.select(f, past, one, value)?;
        kit.select(f, before, zero, value)
    }

    /// The notch's value at signed `row` and `column` of its tile, nought
    /// outside it.
    fn notch_at(
        f: &mut Function<'_>,
        kit: &Kit,
        (tables, level): (Id, &Level),
        (row, column): (Id, Id),
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let mut within = [zero; 2];
        for (within, value) in within.iter_mut().zip([row, column]) {
            let low = kit.test(f, Op::S_GREATER_THAN_EQUAL, value, zero)?;
            let high = kit.test(f, Op::S_LESS_THAN, value, level.side)?;
            *within = f.op(Op::LOGICAL_AND, kit.boolean, &[low, high])?;
        }
        let inside = f.op(Op::LOGICAL_AND, kit.boolean, &within)?;
        let start = kit.op(f, Op::I_MUL, row, level.side)?;
        let offset = kit.op(f, Op::I_ADD, start, column)?;
        let offset = kit.select(f, inside, offset, zero)?;
        let entry = kit.op(f, Op::I_ADD, level.notch, offset)?;
        let value = kit.load(f, tables, entry)?;
        kit.select(f, inside, value, zero)
    }

    /// `tairix_raster`'s shadow strength of `intensity` at a pixel the
    /// silhouette covers by `covered`, drawn at `opacity`.
    fn strength(
        f: &mut Function<'_>,
        kit: &Kit,
        intensity: Id,
        (covered, opacity): (Id, Id),
    ) -> Result<Id, BuildError> {
        let one = f.module().uint(SHADOW_ONE)?;
        let held = kit.min(f, intensity, one)?;
        let weighed = kit.op_by(f, Op::I_MUL, held, 255)?;
        let rounded = kit.op_by(f, Op::I_ADD, weighed, SHADOW_ONE / 2)?;
        let level = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, rounded, SHADOW_SHIFT)?;
        let full = f.module().uint(255)?;
        let outside = kit.op(f, Op::I_SUB, full, covered)?;
        let nearest = f.module().uint(ROUND_NEAREST)?;
        let product = kit.op(f, Op::I_MUL, level, outside)?;
        let shaded = kit.div255(f, product, nearest)?;
        let product = kit.op(f, Op::I_MUL, shaded, opacity)?;
        kit.div255(f, product, nearest)
    }

    /// The pixel one level lays at (`x`, `y`) for its `share` of the lift,
    /// transparent where it casts nothing: its colour scaled by its weight
    /// there and the share, rounding at `bias`. A level the lift gives no
    /// share is not evaluated at all.
    fn part(
        f: &mut Function<'_>,
        kit: &Kit,
        (tables, level, caster): (Id, &Level, &Geometry),
        (x, y, bias): (Id, Id, Id),
        (share, cast): (Id, Id),
    ) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        let laid = kit.local(f, zero)?;
        f.if_then(cast, |f| {
            let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, level.reach, 1)?;
            let reaching = kit.test(f, Op::I_NOT_EQUAL, level.reach, zero)?;
            let below = kit.op(f, Op::I_ADD, caster.bottom, doubled)?;
            let started = kit.test(f, Op::S_GREATER_THAN_EQUAL, y, caster.top)?;
            let unfinished = kit.test(f, Op::S_LESS_THAN, y, below)?;
            let lifted_y = kit.op(f, Op::I_SUB, y, level.reach)?;
            let from_top = kit.op(f, Op::I_SUB, lifted_y, caster.top)?;
            let from_bottom = kit.op(f, Op::I_SUB, lifted_y, caster.bottom)?;
            let to_top = cum(f, kit, (tables, level), from_top)?;
            let to_bottom = cum(f, kit, (tables, level), from_bottom)?;
            let vertical = kit.saturating_sub(f, to_top, to_bottom)?;
            let rising = kit.test(f, Op::I_NOT_EQUAL, vertical, zero)?;
            let x_left = kit.op(f, Op::I_SUB, x, caster.left)?;
            let x_right = kit.op(f, Op::I_SUB, x, caster.right)?;
            let to_left = cum(f, kit, (tables, level), x_left)?;
            let to_right = cum(f, kit, (tables, level), x_right)?;
            let horizontal = kit.saturating_sub(f, to_left, to_right)?;
            let product = kit.op(f, Op::I_MUL, horizontal, vertical)?;
            let rounded = kit.op_by(f, Op::I_ADD, product, SHADOW_ONE / 2)?;
            let whole = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, rounded, SHADOW_SHIFT)?;
            let from_left = kit.op(f, Op::I_ADD, x_left, level.reach)?;
            let edge = kit.op(f, Op::I_ADD, caster.right, level.reach)?;
            let edge = kit.op_by(f, Op::I_SUB, edge, 1)?;
            let from_right = kit.op(f, Op::I_SUB, edge, x)?;
            let below_end = kit.op_by(f, Op::I_SUB, below, 1)?;
            let rows = [
                kit.op(f, Op::I_SUB, y, caster.top)?,
                kit.op(f, Op::I_SUB, below_end, y)?,
            ];
            let mut notches = zero;
            for row in rows {
                for column in [from_left, from_right] {
                    let value = notch_at(f, kit, (tables, level), (row, column))?;
                    notches = kit.op(f, Op::I_ADD, notches, value)?;
                }
            }
            let intensity = kit.saturating_sub(f, whole, notches)?;
            let weight = strength(f, kit, intensity, (caster.covered, caster.opacity))?;
            let product = kit.op(f, Op::I_MUL, weight, share)?;
            let nearest = f.module().uint(ROUND_NEAREST)?;
            let weight = kit.div255(f, product, nearest)?;
            let pixel = kit.scale(f, level.color, weight, bias)?;
            let mut casting = caster.casts;
            for condition in [reaching, started, unfinished, rising] {
                casting = f.op(Op::LOGICAL_AND, kit.boolean, &[casting, condition])?;
            }
            let pixel = kit.select(f, casting, pixel, zero)?;
            f.emit(Op::STORE, &[laid, pixel])
        })?;
        f.op(Op::LOAD, kit.uint, &[laid])
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let target = pass.buffer(TARGET)?;
        let tables = pass.buffer(TABLES)?;
        pass.finish((super::WIDTH, super::HEIGHT), |f, kit, place| {
            let framed = framed(f, kit, place)?;
            let (x, y, bias) = (framed.x, framed.y, framed.bias);
            let caster = geometry(f, kit, (x, y))?;
            let (zero, full) = (f.module().uint(0)?, f.module().uint(255)?);
            // The silhouette's covered pixels darken at neither level.
            let hidden = kit.equal(f, caster.covered, full)?;
            f.if_then(hidden, |f| f.emit(Op::RETURN, &[]))?;
            let lift = kit.field(f, LIFT)?;
            let rest = kit.op(f, Op::I_SUB, full, lift)?;
            let raised = kit.test(f, Op::I_NOT_EQUAL, lift, zero)?;
            let lowered = kit.test(f, Op::I_NOT_EQUAL, lift, full)?;
            let lifted = level(f, kit, LEVELS)?;
            let resting = level(f, kit, LEVELS + LEVEL_WORDS)?;
            let high = part(
                f,
                kit,
                (tables, &lifted, &caster),
                (x, y, bias),
                (lift, raised),
            )?;
            let low = part(
                f,
                kit,
                (tables, &resting, &caster),
                (x, y, bias),
                (rest, lowered),
            )?;
            let (high, low) = (kit.channels(f, high)?, kit.channels(f, low)?);
            let mut sum = high;
            for ((sum, high), low) in sum.iter_mut().zip(high).zip(low) {
                let added = kit.op(f, Op::I_ADD, high, low)?;
                *sum = kit.min(f, added, full)?;
            }
            let pixel = kit.pack(f, sum)?;
            let clear = kit.equal(f, sum[3], zero)?;
            f.if_then(clear, |f| f.emit(Op::RETURN, &[]))?;
            let under = kit.load(f, target, framed.at)?;
            let result = kit.over(f, pixel, under, bias)?;
            kit.store(f, target, framed.at, result)
        })
    }
}
