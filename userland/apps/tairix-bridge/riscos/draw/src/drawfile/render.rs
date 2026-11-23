//! A checked Draw file rendered an object at a time, each through the SWIs
//! RISC OS 5.30's DrawFile calls for it, so the Draw module, ColourTrans,
//! the sprite operations and whatever claims their vectors see the calls
//! RISC OS's does. An object's error is kept and rendering goes on; the
//! first is the call's.
//!
//! - **Paths** are filled then stroked by `Draw_Fill` and `Draw_Stroke`,
//!   each colour set by `ColourTrans_SetGCOL`, the flatness 128 unless the
//!   call gives one.
//! - **Text** in the system font is drawn a character at a time into a work
//!   sprite's mask, then plotted through it; text in a named font asks the
//!   Font Manager for the font first, the system font standing in for one
//!   it does not give.
//! - **Sprites** are plotted by `OS_SpriteOp` 56 through a table
//!   ColourTrans selects, scaled to fill their bounds, or through their own
//!   matrix; a **JPEG** by `JPEG_PlotTransformed`.
//! - **Text areas** are read through their commands, then painted through
//!   the Font Manager.
//!
//! An object whose bounds miss the clipping box, in draw units, is passed
//! over; with the boxes flag each object's bounds are drawn dotted in red
//! first.

use tairix_drawing::riscos::format::{
    self, Object, Objects, DASHED, GROUP, JPEG, NO_COLOUR, OBJECT_HEADER, PATH, PATH_FIELDS,
    SPRITE, TAGGED, TEXT, TEXT_AREA, TRANSFORMED_SPRITE, TRANSFORMED_TEXT, WINDING_EVEN_ODD,
};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{chunk, colour_trans, font, jpeg, os as kos, sprite_op, X};

use super::check::{Fonts, GROUP_OBJECTS, TAGGED_OBJECT};
use super::stack::{self, Depth, Level, Taken};
use super::text_area::{self, Area};
use super::workspace;
use crate::geometry::{Matrix, Point};
use crate::module::call;

/// The flatness a render takes but where the call gives one.
pub(super) const FLATNESS: u32 = 128;
/// The fill style a path is filled with, and stroked with.
const FILL_STYLE: u32 = 0x30;
const STROKE_STYLE: u32 = 0x38;
/// The Draw module's even-odd winding rule.
const EVEN_ODD: u32 = 2;
/// The mitre limit every path is stroked with: ten widths.
const MITRE_LIMIT: u32 = 0xA_0000;
/// A triangle cap's sizes are in sixteenths of the line's width in a file,
/// in 256ths in the Draw module's block.
const TRIANGLE_SCALE: u32 = 16;
/// `ColourTrans_SetGCOL`'s flags: error-diffused colours where the mode
/// shows too few.
const USE_ECFS: u32 = 0x100;
/// R1 for an `OS_SpriteOp` by pointer, which names no area.
const NO_AREA: u32 = 0x100;
/// The `OS_SpriteOp` reasons DrawFile calls, each by pointer.
const READ_INFO: u32 = sprite_op::BY_POINTER | sprite_op::READ_INFO;
const PLOT_TRANSFORMED: u32 = sprite_op::BY_POINTER | sprite_op::PUT_TRANSFORMED;
const OUTPUT_TO_MASK: u32 = sprite_op::BY_POINTER | sprite_op::SWITCH_TO_MASK;
/// `OS_SpriteOp` 56's plot action: through the sprite's mask.
const THROUGH_MASK: u32 = 8;
/// `ColourTrans_SelectTable`'s R0 for a sprite R1 points at, and R5's flag
/// saying so.
const SPRITE_SOURCE: u32 = 0x100;
const SPRITE_POINTER: u32 = 1;
/// The current mode and palette, to `ColourTrans_SelectTable`.
const CURRENT: u32 = u32::MAX;
/// A sprite's header: where it holds its image's offset, its mode, and how
/// long a header with no palette is.
const SPRITE_IMAGE: u32 = 32;
const SPRITE_MODE: u32 = 40;
const SPRITE_HEADER: u32 = 44;
/// The mode variables of a mode's eigen factors.
const X_EIG: u32 = 4;
const Y_EIG: u32 = 5;
/// A JPEG object's fields before its matrix, and where its length is.
const JPEG_MATRIX: usize = OBJECT_HEADER + 16;
const JPEG_LENGTH: usize = JPEG_MATRIX + format::MATRIX;
/// `JPEG_PlotTransformed`'s flag: R2 is a matrix.
const JPEG_BY_MATRIX: u32 = 1;
/// A text's fields after its header: its colours, style, sizes and base
/// point, then its characters.
const TEXT_FIELDS: usize = 28;
/// A transformed text's matrix and flags come first.
const TRANSFORMED_TEXT_FIELDS: usize = format::MATRIX + 4;
/// The box flag's colour, red, and the dot pattern it is drawn in.
const BOX_COLOUR: u32 = 0x0000_DD00;
const BOX_PATTERN: [u8; 10] = [23, 6, 0xCC, 0, 0xCC, 0, 0xCC, 0, 0xCC, 0];
/// `PLOT` 4, a move, and 21, a dotted line with both ends.
const MOVE: u32 = 4;
const DOTTED: u32 = 21;
/// Draw units to the OS unit, as a shift.
const UNIT_SHIFT: u32 = 8;

/// What a render asks.
#[derive(Clone, Copy, Debug)]
pub(super) struct Asked {
    pub(super) boxes: bool,
    pub(super) suppress: bool,
    pub(super) flatness: u32,
    /// The matrix R3 gave, read, and its address; the module's identity
    /// where none was given.
    pub(super) matrix: Matrix,
    pub(super) matrix_at: u32,
    /// The clipping box, in draw units.
    pub(super) clip: Option<[i32; 4]>,
}

/// Renders a checked file.
pub(super) struct Renderer<'f> {
    bytes: &'f [u8],
    file: u32,
    fonts: &'f Fonts,
    asked: Asked,
    workspace: u32,
    /// The first error an object gave.
    pub(super) first: Option<u32>,
    depth: Depth,
}

impl<'f> Renderer<'f> {
    pub(super) fn new(
        bytes: &'f [u8],
        file: u32,
        fonts: &'f Fonts,
        asked: Asked,
        workspace: u32,
    ) -> Self {
        Self {
            bytes,
            file,
            fonts,
            asked,
            workspace,
            first: None,
            depth: Depth::default(),
        }
    }

    /// Renders every object.
    ///
    /// # Errors
    ///
    /// What unwinds: an object's error is kept in [`Self::first`] instead.
    pub(super) fn render(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let objects = &self.bytes[format::HEADER..];
        self.objects(os, objects, format::HEADER)
    }

    fn objects(&mut self, os: &mut Os<'_>, run: &'f [u8], at: usize) -> Result<(), Fail> {
        for object in Objects::new(run, at).map_while(Result::ok) {
            self.object(os, object, true)?;
        }
        Ok(())
    }

    fn address(&self, at: usize) -> u32 {
        self.file
            .wrapping_add(u32::try_from(at).unwrap_or(u32::MAX))
    }

    /// One `level` deeper, or the supervisor stack's overflow.
    fn deeper(&mut self, os: &mut Os<'_>, level: Level) -> Result<Taken, Fail> {
        self.depth
            .enter(stack::RENDER, level)
            .ok_or_else(|| Fail::Unwind(os.overflowed()))
    }

    /// `done`'s error kept if it is the first; what unwinds passed on.
    fn keep(&mut self, os: &mut Os<'_>, done: Result<(), Fail>) -> Result<(), Fail> {
        match done {
            Ok(()) => Ok(()),
            Err(Fail::Error(block)) => {
                if self.first.is_none() {
                    self.first = Some(workspace::keep_error(os, self.workspace, block)?);
                }
                Ok(())
            }
            Err(unwind) => Err(unwind),
        }
    }

    /// `object` rendered; `boxed` where its own bounds are tested against
    /// the clip and drawn, as all but a tagged object's object's are.
    fn object(&mut self, os: &mut Os<'_>, object: Object<'f>, boxed: bool) -> Result<(), Fail> {
        let known = matches!(
            object.kind,
            PATH | TEXT
                | TRANSFORMED_TEXT
                | SPRITE
                | TRANSFORMED_SPRITE
                | JPEG
                | GROUP
                | TAGGED
                | TEXT_AREA
        );
        if !known {
            return Ok(());
        }
        if boxed {
            let bounds = self.bounds(object);
            if !self.visible(bounds) {
                return Ok(());
            }
            if self.asked.boxes {
                let drawn = draw_box(os, bounds);
                self.keep(os, drawn)?;
            }
        }
        match object.kind {
            GROUP => {
                let run = object.bytes.get(GROUP_OBJECTS..).unwrap_or(&[]);
                let taken = self.deeper(os, Level::Group)?;
                let drawn = self.objects(os, run, object.at + GROUP_OBJECTS);
                self.depth.leave(taken);
                return drawn;
            }
            TAGGED => {
                let wrapped = object.bytes.get(TAGGED_OBJECT..).unwrap_or(&[]);
                if let Some(Ok(inner)) = Objects::new(wrapped, object.at + TAGGED_OBJECT).next() {
                    let taken = self.deeper(os, Level::Tagged)?;
                    let drawn = self.object(os, inner, false);
                    self.depth.leave(taken);
                    return drawn;
                }
                return Ok(());
            }
            _ if self.asked.suppress => return Ok(()),
            _ => {}
        }
        let drawn = match object.kind {
            PATH => self.path(os, object),
            TEXT => self.text(os, object, None),
            TRANSFORMED_TEXT => {
                let matrix = matrix_in(object.bytes, OBJECT_HEADER);
                self.text(os, object, Some(matrix))
            }
            SPRITE => self.sprite(os, object),
            TRANSFORMED_SPRITE => self.transformed_sprite(os, object),
            JPEG => self.jpeg(os, object),
            _ => text_area(os, object),
        };
        self.keep(os, drawn)
    }

    /// An object's bounds, through the render's matrix as `DrawFile_BBox`
    /// takes a file's.
    fn bounds(&self, object: Object<'_>) -> [i32; 4] {
        let word = |at| format::int(object.bytes, at).unwrap_or(0);
        transformed_box(&self.asked.matrix, [word(8), word(12), word(16), word(20)])
    }

    fn visible(&self, [x0, y0, x1, y1]: [i32; 4]) -> bool {
        self.asked
            .clip
            .is_none_or(|[cx0, cy0, cx1, cy1]| x1 >= cx0 && x0 <= cx1 && y1 >= cy0 && y0 <= cy1)
    }

    fn path(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let field = |at| format::word(object.bytes, OBJECT_HEADER + at).unwrap_or(0);
        let (fill, outline, width, style) = (field(0), field(4), field(8), field(12));
        let mut path_at = OBJECT_HEADER + PATH_FIELDS;
        let mut dash = 0;
        if style & DASHED != 0 {
            dash = self.address(object.at + path_at);
            let count = format::word(object.bytes, path_at + 4).unwrap_or(0);
            path_at += 8 + 4 * usize::try_from(count).unwrap_or(0);
        }
        let path = self.address(object.at + path_at);
        if fill != NO_COLOUR {
            set_colour(os, fill, USE_ECFS)?;
            let rule = if style & WINDING_EVEN_ODD != 0 {
                EVEN_ODD
            } else {
                0
            };
            // R5 and R6 are as RISC OS's DrawFile leaves them: its workspace
            // and the object's bounds, which a claimant of DrawV sees.
            let mut regs = Regs::default();
            regs.r[..7].copy_from_slice(&[
                path,
                FILL_STYLE | rule,
                self.asked.matrix_at,
                self.asked.flatness,
                0,
                self.workspace + workspace::CAP_JOIN,
                self.address(object.at + 8),
            ]);
            os.perform(X | (chunk::DRAW + call::FILL), &mut regs)?;
        }
        if outline != NO_COLOUR {
            set_colour(os, outline, USE_ECFS)?;
            let block = self.workspace + workspace::CAP_JOIN;
            write_words(os, block, &cap_join_block(style))?;
            let mut regs = Regs::default();
            regs.r[..7].copy_from_slice(&[
                path,
                STROKE_STYLE,
                self.asked.matrix_at,
                self.asked.flatness,
                width,
                block,
                dash,
            ]);
            os.perform(X | (chunk::DRAW + call::STROKE), &mut regs)?;
        }
        Ok(())
    }

    /// A text or transformed text: `matrix` the latter's own.
    fn text(
        &mut self,
        os: &mut Os<'_>,
        object: Object<'_>,
        matrix: Option<Matrix>,
    ) -> Result<(), Fail> {
        let base = OBJECT_HEADER
            + if matrix.is_some() {
                TRANSFORMED_TEXT_FIELDS
            } else {
                0
            };
        let field = |at| format::int(object.bytes, base + at).unwrap_or(0);
        let colour = field(0).cast_unsigned();
        let number = field(8).cast_unsigned() & format::FONT_NUMBER;
        let sizes = (field(12), field(16));
        let at = Point::new(field(20), field(24));
        let characters = terminated(object.bytes, base + TEXT_FIELDS);
        let mut missing = Ok(());
        if number != 0 {
            missing = self.find_font(os, number, sizes);
        }
        let through = compose(&self.asked.matrix, matrix.as_ref());
        let drawn = self.system_text(os, characters, colour, sizes, at, &through);
        self.keep(os, missing)?;
        drawn
    }

    /// The Font Manager asked for font `number`; the font it gives is let
    /// go, the system font standing in, as for one it does not give.
    fn find_font(
        &mut self,
        os: &mut Os<'_>,
        number: u32,
        (sx, sy): (i32, i32),
    ) -> Result<(), Fail> {
        let Some(name) = self.fonts.named(number) else {
            return Ok(());
        };
        let sixteenths = |size: i32| (size / 40).cast_unsigned();
        let mut regs = Regs::default();
        regs.r[1..4].copy_from_slice(&[self.address(name), sixteenths(sx), sixteenths(sy)]);
        os.perform(X | font::FIND_FONT, &mut regs)?;
        let mut lose = Regs::default();
        lose.r[0] = regs.r[0];
        os.perform(X | font::LOSE_FONT, &mut lose)
    }

    /// `characters` in the system font, one at a time: each drawn into the
    /// work sprite's mask at its foot, then the sprite plotted through it
    /// at its place, `sx` wide and `sy` high.
    fn system_text(
        &mut self,
        os: &mut Os<'_>,
        characters: &[u8],
        colour: u32,
        (sx, sy): (i32, i32),
        at: Point,
        through: &Matrix,
    ) -> Result<(), Fail> {
        let area = self.workspace + workspace::AREA;
        let sprite = workspace::write_work_sprite(os, area, colour)?;
        for (n, &character) in (0i32..).zip(characters) {
            let mut switch = Regs::default();
            switch.r[..4].copy_from_slice(&[OUTPUT_TO_MASK, area, sprite, 0]);
            os.perform(X | kos::SPRITE_OP, &mut switch)?;
            let drawn = [18, 3, 0, 25, 4, 0, 0, 32, 0, character]
                .into_iter()
                .try_for_each(|byte| write_c(os, byte));
            let mut back = Regs::default();
            back.r[..4].copy_from_slice(&switch.r[..4]);
            os.perform(X | kos::SPRITE_OP, &mut back)?;
            drawn?;
            let cell = Matrix {
                a: sx.wrapping_mul(16),
                b: 0,
                c: 0,
                d: sy.wrapping_mul(8),
                e: at.x.wrapping_add(n.wrapping_mul(sx)),
                f: at.y.wrapping_sub(sy >> 3),
            };
            self.plot_sprite(os, sprite, &compose(through, Some(&cell)), Source::Palette)?;
        }
        Ok(())
    }

    /// Sprite `sprite` plotted through `matrix`, its colours through the
    /// table ColourTrans selects for it.
    fn plot_sprite(
        &mut self,
        os: &mut Os<'_>,
        sprite: u32,
        matrix: &Matrix,
        source: Source,
    ) -> Result<(), Fail> {
        let table = self.workspace + workspace::TABLE;
        let matrix_at = self.workspace + workspace::MATRIX;
        let mut select = Regs::default();
        let (r0, r1, r5) = match source {
            Source::Palette => (SPRITE_SOURCE, sprite, SPRITE_POINTER),
            Source::Mode(mode) => (mode, CURRENT, matrix_at),
        };
        select.r[..6].copy_from_slice(&[r0, r1, CURRENT, CURRENT, table, r5]);
        os.perform(X | colour_trans::SELECT_TABLE, &mut select)?;
        write_words(os, matrix_at, &matrix.words())?;
        let mut put = Regs::default();
        put.r[..8].copy_from_slice(&[
            PLOT_TRANSFORMED,
            NO_AREA,
            sprite,
            0,
            0,
            THROUGH_MASK,
            matrix_at,
            table,
        ]);
        os.perform(X | kos::SPRITE_OP, &mut put)
    }

    /// The source a sprite's colours are translated from: its own palette,
    /// or its mode's.
    fn source_of(os: &mut Os<'_>, sprite: u32) -> Result<Source, Fail> {
        let image = os.read_word(sprite.wrapping_add(SPRITE_IMAGE))?;
        if image > SPRITE_HEADER {
            return Ok(Source::Palette);
        }
        Ok(Source::Mode(
            os.read_word(sprite.wrapping_add(SPRITE_MODE))?,
        ))
    }

    /// A sprite scaled to fill its bounds.
    fn sprite(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let sprite = self.address(object.at + OBJECT_HEADER);
        let mut info = Regs::default();
        info.r[..3].copy_from_slice(&[READ_INFO, NO_AREA, sprite]);
        os.perform(X | kos::SPRITE_OP, &mut info)?;
        let (width, height, mode) = (info.r[3], info.r[4], info.r[6]);
        let (x_eig, y_eig) = (
            mode_variable(os, mode, X_EIG)?,
            mode_variable(os, mode, Y_EIG)?,
        );
        let word = |at| format::int(object.bytes, at).unwrap_or(0);
        let [x0, y0, x1, y1] = [word(8), word(12), word(16), word(20)];
        let scale = |span: i32, pixels: u32, eig: u32| {
            let units = i64::from(pixels) << eig.min(8) << UNIT_SHIFT;
            if units == 0 {
                return 0;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a scale kept to a word as RISC OS keeps it"
            )]
            let scale = ((i64::from(span) << 16) / units) as i32;
            scale
        };
        let fill = Matrix {
            a: scale(x1.wrapping_sub(x0), width, x_eig),
            b: 0,
            c: 0,
            d: scale(y1.wrapping_sub(y0), height, y_eig),
            e: x0,
            f: y0,
        };
        let source = Self::source_of(os, sprite)?;
        let through = compose(&self.asked.matrix, Some(&fill));
        self.plot_sprite(os, sprite, &through, source)
    }

    /// A sprite through its own matrix.
    fn transformed_sprite(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let own = matrix_in(object.bytes, OBJECT_HEADER);
        let sprite = self.address(object.at + OBJECT_HEADER + format::MATRIX);
        let source = Self::source_of(os, sprite)?;
        let through = compose(&self.asked.matrix, Some(&own));
        self.plot_sprite(os, sprite, &through, source)
    }

    /// A JPEG through its own matrix.
    fn jpeg(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let own = matrix_in(object.bytes, JPEG_MATRIX);
        let length = format::word(object.bytes, JPEG_LENGTH).unwrap_or(0);
        let matrix_at = self.workspace + workspace::MATRIX;
        write_words(
            os,
            matrix_at,
            &compose(&self.asked.matrix, Some(&own)).words(),
        )?;
        let mut regs = Regs::default();
        let data = self.address(object.at + JPEG_LENGTH + 4);
        regs.r[..4].copy_from_slice(&[data, JPEG_BY_MATRIX, matrix_at, length]);
        os.perform(X | jpeg::PLOT_TRANSFORMED, &mut regs)
    }
}

/// Where a sprite's colours come from.
#[derive(Clone, Copy, Debug)]
enum Source {
    Palette,
    Mode(u32),
}

/// The bounds drawn as a dotted red rectangle, in OS units.
fn draw_box(os: &mut Os<'_>, [x0, y0, x1, y1]: [i32; 4]) -> Result<(), Fail> {
    set_colour(os, BOX_COLOUR, 0)?;
    for byte in BOX_PATTERN {
        write_c(os, byte)?;
    }
    let units = |v: i32| (v >> UNIT_SHIFT).cast_unsigned();
    plot(os, MOVE, units(x0), units(y1))?;
    for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        plot(os, DOTTED, units(x), units(y))?;
    }
    Ok(())
}

/// A text area: its colours set through the Font Manager, then its
/// commands read, as RISC OS's are.
fn text_area(os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
    let area = Area::of(object);
    let mut regs = Regs::default();
    regs.r[..4].copy_from_slice(&[
        0,
        area.background,
        area.foreground,
        text_area::COLOUR_OFFSET,
    ]);
    os.perform(X | font::SET_FONT_COLOURS, &mut regs)?;
    area.read()
        .map_err(|refusal| text_area::refuse(os, refusal))
}

/// The matrix six words of `bytes` from `at` hold.
fn matrix_in(bytes: &[u8], at: usize) -> Matrix {
    let mut words = [0; 6];
    for (i, word) in words.iter_mut().enumerate() {
        *word = format::word(bytes, at + 4 * i).unwrap_or(0);
    }
    Matrix::from_words(words)
}

/// `inner` then `outer`: a point through the first, then the second, each
/// product rounded half up as the Draw module rounds a point.
fn compose(outer: &Matrix, inner: Option<&Matrix>) -> Matrix {
    let Some(inner) = inner else {
        return *outer;
    };
    if outer.is_identity() {
        return *inner;
    }
    let mul = |m: i32, n: i32| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a product kept to a word as RISC OS keeps it"
        )]
        let product = ((i64::from(m) * i64::from(n) + 0x8000) >> 16) as i32;
        product
    };
    let translate = outer
        .apply(Point::new(inner.e, inner.f))
        .unwrap_or(Point::new(inner.e, inner.f));
    Matrix {
        a: mul(outer.a, inner.a).wrapping_add(mul(outer.c, inner.b)),
        b: mul(outer.b, inner.a).wrapping_add(mul(outer.d, inner.b)),
        c: mul(outer.a, inner.c).wrapping_add(mul(outer.c, inner.d)),
        d: mul(outer.b, inner.c).wrapping_add(mul(outer.d, inner.d)),
        e: translate.x,
        f: translate.y,
    }
}

/// A box through `matrix`, each corner's products cut toward nought on
/// their own, the corners' extent taken.
pub(super) fn transformed_box(matrix: &Matrix, [x0, y0, x1, y1]: [i32; 4]) -> [i32; 4] {
    let (lx, hx, ly, hy) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
    let corners = [(lx, ly), (hx, ly), (lx, hy), (hx, hy)]
        .map(|(x, y)| matrix.apply_truncating(Point::new(x, y)));
    let mut extent = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for p in corners {
        extent = [
            extent[0].min(p.x),
            extent[1].min(p.y),
            extent[2].max(p.x),
            extent[3].max(p.y),
        ];
    }
    extent
}

/// The Draw module's cap and join block for path style `style`.
fn cap_join_block(style: u32) -> [u32; 4] {
    use tairix_drawing::riscos::format::{
        CODE, END_CAP_SHIFT, START_CAP_SHIFT, TRIANGLE_LENGTH_SHIFT, TRIANGLE_SIZE,
        TRIANGLE_WIDTH_SHIFT,
    };
    let join = style & CODE;
    let end = (style >> END_CAP_SHIFT) & CODE;
    let start = (style >> START_CAP_SHIFT) & CODE;
    let width = ((style >> TRIANGLE_WIDTH_SHIFT) & TRIANGLE_SIZE) * TRIANGLE_SCALE;
    let length = ((style >> TRIANGLE_LENGTH_SHIFT) & TRIANGLE_SIZE) * TRIANGLE_SCALE;
    let triangle = (length << 16) | width;
    [
        join | (end << 8) | (start << 16),
        MITRE_LIMIT,
        triangle,
        triangle,
    ]
}

/// The bytes of `bytes` from `at` to the next nought.
fn terminated(bytes: &[u8], at: usize) -> &[u8] {
    let rest = bytes.get(at..).unwrap_or(&[]);
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    &rest[..end]
}

fn mode_variable(os: &mut Os<'_>, mode: u32, variable: u32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[mode, variable]);
    os.perform(X | kos::READ_MODE_VARIABLE, &mut regs)?;
    Ok(regs.r[2])
}

fn set_colour(os: &mut Os<'_>, colour: u32, flags: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = colour;
    regs.r[3] = flags;
    os.perform(X | colour_trans::SET_GCOL, &mut regs)
}

fn write_c(os: &mut Os<'_>, byte: u8) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = u32::from(byte);
    os.perform(X | kos::WRITE_C, &mut regs)
}

fn plot(os: &mut Os<'_>, code: u32, x: u32, y: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[code, x, y]);
    os.perform(X | kos::PLOT, &mut regs)
}

pub(super) fn write_words(os: &mut Os<'_>, at: u32, words: &[u32]) -> Result<(), Fail> {
    for (i, &word) in (0u32..).zip(words) {
        os.write_word(at.wrapping_add(4 * i), word)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_through_a_turn_takes_its_corners_extent_as_risc_os_did() {
        let turn = Matrix {
            a: 0xB505,
            b: 0xB505,
            c: -0xB505,
            d: 0xB505,
            e: 0,
            f: 0,
        };
        assert_eq!(transformed_box(&turn, [50, 60, 10, 20]), [-35, 21, 21, 77]);
        let stretch = Matrix {
            a: 0x2_0000,
            d: 0x8000,
            e: 1000,
            f: -2000,
            ..Matrix::IDENTITY
        };
        assert_eq!(
            transformed_box(&stretch, [10, 20, 50, 60]),
            [1020, -1990, 1100, -1970]
        );
        assert_eq!(
            transformed_box(&turn, [0, 0, 100_000, 80000]),
            [-56568, 0, 70710, 127_278]
        );
    }

    #[test]
    fn a_path_style_makes_the_cap_and_join_block_risc_os_made() {
        // The style of a recorded file: a round start cap, butt end cap,
        // round join, triangle sizes in the top half.
        assert_eq!(
            cap_join_block(0x2010_0055),
            [0x0001_0101, 0xA_0000, 0x0200_0100, 0x0200_0100]
        );
        assert_eq!(cap_join_block(0), [0, 0xA_0000, 0, 0]);
    }

    #[test]
    fn composing_with_the_identity_is_the_other_matrix() {
        let m = Matrix {
            a: 3,
            b: 4,
            c: 5,
            d: 6,
            e: 7,
            f: 8,
        };
        assert_eq!(compose(&Matrix::IDENTITY, Some(&m)), m);
        assert_eq!(compose(&m, None), m);
        let double = Matrix {
            a: 0x2_0000,
            d: 0x2_0000,
            ..Matrix::IDENTITY
        };
        let shifted = Matrix {
            e: 100,
            f: -50,
            ..Matrix::IDENTITY
        };
        assert_eq!(
            compose(&double, Some(&shifted)),
            Matrix {
                e: 200,
                f: -100,
                ..double
            }
        );
    }
}
