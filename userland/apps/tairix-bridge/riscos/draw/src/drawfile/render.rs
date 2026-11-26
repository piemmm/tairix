//! A checked Draw file rendered an object at a time, each through the SWIs
//! RISC OS 5.30's DrawFile calls for it, so the Draw module, ColourTrans,
//! the sprite operations and whatever claims their vectors see the calls
//! RISC OS's does. An object's error is kept and rendering goes on; the
//! first is the call's.
//!
//! - **Paths** are filled then stroked by `Draw_Fill` and `Draw_Stroke`,
//!   each colour set by `ColourTrans_SetGCOL`, the flatness 128 unless the
//!   call gives one.
//! - **Text** in a named font is painted by the Font Manager: found at its
//!   sizes, coloured through ColourTrans, painted in OS units through the
//!   render's matrix, its own after it, and let go. Text in no font, or one
//!   the Font Manager refuses, the refusal kept, is drawn in the system font
//!   a character at a time into a work sprite's mask, then plotted through it.
//! - **Sprites** are plotted by `OS_SpriteOp` 56 through a table
//!   ColourTrans selects, scaled to fill their bounds, or through their own
//!   matrix; a **JPEG** by `JPEG_PlotTransformed`.
//! - **Text areas** are laid out and painted through the Font Manager.
//!
//! An object whose bounds miss the clipping box, in draw units, is passed
//! over; with the boxes flag each object's bounds are drawn dotted in red
//! first.

use tairix_drawing::riscos::format::{
    self, dash_pattern_bytes, Object, Objects, DASHED, DASH_COUNT, FIXED_ONE, FRACTION_BITS, GROUP,
    JPEG, JPEG_DATA, JPEG_LENGTH, JPEG_MATRIX, KERNED, NO_COLOUR, OBJECT_BOUNDS, OBJECT_HEADER,
    PATH, PATH_DASHES, PATH_FILL, PATH_OUTLINE, PATH_STYLE, PATH_WIDTH, RIGHT_TO_LEFT, SPRITE,
    TAGGED, TEXT, TEXT_AREA, TEXT_BACKGROUND, TEXT_BASE, TEXT_CHARACTERS, TEXT_COLOUR, TEXT_SIZES,
    TEXT_STYLE, TRANSFORMED_SPRITE, TRANSFORMED_TEXT, TRANSFORMED_TEXT_FIELDS,
    TRANSFORMED_TEXT_FLAGS, WINDING_EVEN_ODD,
};
use tairix_drawing::riscos::units::{
    DRAW_UNITS_PER_OS_UNIT, DRAW_UNITS_PER_POINT, DRAW_UNIT_SHIFT, SIXTEENTHS_PER_POINT,
};
use tairix_image::{sprite_control, SPRITE_HEADER_LEN};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::colour_trans::{BARE_SPRITE, CURRENT, SPRITE_POINTER, USE_ECF};
use tairix_riscos_kernel::swi::{chunk, colour_trans, font, jpeg, os as kos, sprite_op, X};
use tairix_util::endian::{le_i32, le_u32};
use tairix_vdu::code::{Control, Operation};
use tairix_vdu::mode::var;
use tairix_vdu::plot;

use super::check::Fonts;
use super::stack::{self, Depth, Level};
use super::text_area::{self, Area};
use super::workspace;
use crate::geometry::{Matrix, Point};
use crate::module::{call, FILL_STYLE};
use crate::raster::{class, winding};
use crate::thicken::{Style, Triangle};

/// The flatness a render takes but where the call gives one.
pub(super) const FLATNESS: u32 = 128;
/// The fill style a path is stroked with: all of it but its exterior.
const STROKE_STYLE: u32 = class::EXTERIOR_BOUNDARY | class::INTERIOR_BOUNDARY | class::INTERIOR;
/// A file's parts of a line's width, in the parts the Draw module's block
/// holds a triangle cap's sizes in.
const TRIANGLE_SCALE: u32 = Triangle::UNIT / format::TRIANGLE_UNIT;
/// The `OS_SpriteOp` reasons DrawFile calls, each by pointer.
const READ_INFO: u32 = sprite_op::BY_POINTER | sprite_op::READ_INFO;
const PLOT_TRANSFORMED: u32 = sprite_op::BY_POINTER | sprite_op::PUT_TRANSFORMED;
const OUTPUT_TO_MASK: u32 = sprite_op::BY_POINTER | sprite_op::SWITCH_TO_MASK;
/// A transformed text's kerning and right-to-left flags, and how far up
/// `Font_Paint`'s flags they move.
const TEXT_FLAGS: u32 = KERNED | RIGHT_TO_LEFT;
const PAINT_TEXT_FLAGS_SHIFT: u32 = font::flag::KERN.trailing_zeros();
// Each lands on the flag of `Font_Paint`'s that does as it asks.
const _: () = assert!(
    (TEXT_FLAGS << PAINT_TEXT_FLAGS_SHIFT) == (font::flag::KERN | font::flag::RIGHT_TO_LEFT)
);
/// A text's sizes are in draw units, the Font Manager's in sixteenths of
/// a point.
const DRAW_UNITS_A_SIXTEENTH: i32 = (DRAW_UNITS_PER_POINT / SIXTEENTHS_PER_POINT).cast_signed();
/// What system-font text writes into the work sprite's mask before each
/// character, as `OS_WriteN` gives it: `GCOL 3,0`.
const MASK_GCOL: [u8; 3] = [Control::GraphicsColour as u8, 3, 0];
/// The work sprite's character cell across and up, in OS units.
const CELL: (u32, u32) = {
    let mode = &tairix_vdu::mode::MODES[workspace::MODE as usize];
    (workspace::SIDE << mode.x_eig, workspace::SIDE << mode.y_eig)
};
/// Where the character is then written from: the cell's top, `MOVE 0,32`.
const MASK_MOVE: (u32, u32) = (0, CELL.1);
/// What each draw unit of a text's size scales the cell by, across and up,
/// in a matrix's fixed point.
const CELL_SCALE: (i32, i32) = (
    FIXED_ONE / (DRAW_UNITS_PER_OS_UNIT * CELL.0).cast_signed(),
    FIXED_ONE / (DRAW_UNITS_PER_OS_UNIT * CELL.1).cast_signed(),
);
/// The box flag's colour, red, and the dot pattern it is drawn in.
const BOX_COLOUR: u32 = 0x0000_DD00;
const BOX_PATTERN: [u8; 10] = [
    Control::Miscellaneous as u8,
    Operation::LinePattern as u8,
    0xCC,
    0,
    0xCC,
    0,
    0xCC,
    0,
    0xCC,
    0,
];
/// The `PLOT` codes of a move, and of a dotted line with both its ends.
const MOVE: u8 = plot::LINE | plot::ABSOLUTE;
const DOTTED: u8 = plot::DOTTED | plot::ABSOLUTE | plot::FOREGROUND;

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
        }
    }

    /// Renders every object.
    ///
    /// # Errors
    ///
    /// What unwinds: an object's error is kept in [`Self::first`] instead.
    pub(super) fn render(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let objects = Objects::new(&self.bytes[format::HEADER..], format::HEADER);
        self.objects(os, objects, Depth::default())
    }

    fn objects(&mut self, os: &mut Os<'_>, objects: Objects<'f>, depth: Depth) -> Result<(), Fail> {
        for object in objects.map_while(Result::ok) {
            self.object(os, object, true, depth)?;
        }
        Ok(())
    }

    fn address(&self, at: usize) -> u32 {
        self.file
            .wrapping_add(u32::try_from(at).unwrap_or(u32::MAX))
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
    fn object(
        &mut self,
        os: &mut Os<'_>,
        object: Object<'f>,
        boxed: bool,
        depth: Depth,
    ) -> Result<(), Fail> {
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
                let deeper = depth.deeper(os, stack::RENDER, Level::Group)?;
                return self.objects(os, stack::group_objects(object), deeper);
            }
            TAGGED => {
                let Some(Ok(inner)) = stack::tagged_object(object) else {
                    return Ok(());
                };
                let deeper = depth.deeper(os, stack::RENDER, Level::Tagged)?;
                return self.object(os, inner, false, deeper);
            }
            _ if self.asked.suppress => return Ok(()),
            _ => {}
        }
        let drawn = match object.kind {
            PATH => self.path(os, object),
            TEXT => self.text(os, object, None),
            TRANSFORMED_TEXT => {
                let matrix = matrix_in(object.bytes, OBJECT_HEADER);
                let flags = le_u32(object.bytes, TRANSFORMED_TEXT_FLAGS).unwrap_or(0);
                self.text(os, object, Some((matrix, flags)))
            }
            SPRITE => self.sprite(os, object),
            TRANSFORMED_SPRITE => self.transformed_sprite(os, object),
            JPEG => self.jpeg(os, object),
            _ => self.text_area(os, object),
        };
        self.keep(os, drawn)
    }

    /// An object's bounds, through the render's matrix as `DrawFile_BBox`
    /// takes a file's.
    fn bounds(&self, object: Object<'_>) -> [i32; 4] {
        transformed_box(&self.asked.matrix, box_at(object.bytes, OBJECT_BOUNDS))
    }

    fn visible(&self, [x0, y0, x1, y1]: [i32; 4]) -> bool {
        self.asked
            .clip
            .is_none_or(|[cx0, cy0, cx1, cy1]| x1 >= cx0 && x0 <= cx1 && y1 >= cy0 && y0 <= cy1)
    }

    fn path(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let field = |at| le_u32(object.bytes, at).unwrap_or(0);
        let (fill, outline, width, style) = (
            field(PATH_FILL),
            field(PATH_OUTLINE),
            field(PATH_WIDTH),
            field(PATH_STYLE),
        );
        let dashed = style & DASHED != 0;
        let count = dashed.then(|| field(PATH_DASHES + DASH_COUNT));
        let Some(path_at) = path_offset(object.at, count) else {
            return Ok(());
        };
        let dash = if dashed {
            self.address(object.at + PATH_DASHES)
        } else {
            0
        };
        let path = self.address(path_at);
        if fill != NO_COLOUR {
            set_colour(os, fill, USE_ECF)?;
            let rule = if style & WINDING_EVEN_ODD != 0 {
                winding::EVEN_ODD
            } else {
                winding::NON_ZERO
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
                self.address(object.at + OBJECT_BOUNDS),
            ]);
            os.perform(X | (chunk::DRAW + call::FILL), &mut regs)?;
        }
        if outline != NO_COLOUR {
            set_colour(os, outline, USE_ECF)?;
            let block = self.workspace + workspace::CAP_JOIN;
            os.write_words(block, &cap_join_block(style))?;
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

    /// A text or transformed text, `own` the latter's matrix and flags:
    /// painted by the Font Manager in its named font, or in the system font
    /// where it names none or the Font Manager refuses it, the refusal kept.
    fn text(
        &mut self,
        os: &mut Os<'_>,
        object: Object<'_>,
        own: Option<(Matrix, u32)>,
    ) -> Result<(), Fail> {
        let shift = if own.is_some() {
            TRANSFORMED_TEXT_FIELDS
        } else {
            0
        };
        let field = |at| le_i32(object.bytes, shift + at).unwrap_or(0);
        let text = Text {
            foreground: field(TEXT_COLOUR).cast_unsigned(),
            background: field(TEXT_BACKGROUND).cast_unsigned(),
            sizes: (field(TEXT_SIZES), field(TEXT_SIZES + 4)),
            at: Point::new(field(TEXT_BASE), field(TEXT_BASE + 4)),
            string: self.address(object.at + shift + TEXT_CHARACTERS),
        };
        let number = field(TEXT_STYLE).cast_unsigned() & format::FONT_NUMBER;
        if let Some(name) = self.fonts.named(number).filter(|_| number != 0) {
            match self.font_text(os, self.address(name), &text, own) {
                Ok(()) => return Ok(()),
                Err(Fail::Error(block)) => self.keep(os, Err(Fail::Error(block)))?,
                Err(unwind) => return Err(unwind),
            }
        }
        let characters = terminated(object.bytes, shift + TEXT_CHARACTERS);
        let through = compose(&self.asked.matrix, own.as_ref().map(|(matrix, _)| matrix));
        self.system_text(
            os,
            characters,
            text.foreground,
            text.sizes,
            text.at,
            &through,
        )
    }

    /// `text` painted in the font named at `name`: found at its sizes, its
    /// colours set through ColourTrans, painted, and let go even where
    /// painting is refused.
    fn font_text(
        &mut self,
        os: &mut Os<'_>,
        name: u32,
        text: &Text,
        own: Option<(Matrix, u32)>,
    ) -> Result<(), Fail> {
        let sixteenths = |size: i32| (size / DRAW_UNITS_A_SIXTEENTH).cast_unsigned();
        let mut find = Regs::default();
        find.r[1..4].copy_from_slice(&[name, sixteenths(text.sizes.0), sixteenths(text.sizes.1)]);
        os.perform(X | font::FIND_FONT, &mut find)?;
        let handle = find.r[0];
        let painted = self.paint_text(os, handle, text, own);
        let mut release = Regs::default();
        release.r[0] = handle;
        let lost = os.perform(X | font::LOSE_FONT, &mut release);
        painted.and(lost)
    }

    /// `text` painted in `handle` at its base point through the render's
    /// matrix, in OS units, its own matrix after the render's where it has one.
    fn paint_text(
        &mut self,
        os: &mut Os<'_>,
        handle: u32,
        text: &Text,
        own: Option<(Matrix, u32)>,
    ) -> Result<(), Fail> {
        let mut colours = Regs::default();
        colours.r[..4].copy_from_slice(&[
            handle,
            text.background,
            text.foreground,
            font::MOST_OFFSET.cast_unsigned(),
        ]);
        os.perform(X | colour_trans::SET_FONT_COLOURS, &mut colours)?;
        let render = &self.asked.matrix;
        let painted = match own {
            None => Matrix {
                e: 0,
                f: 0,
                ..*render
            },
            Some((matrix, _)) => {
                let moved = os_units(render.apply_truncating(Point::new(matrix.e, matrix.f)));
                Matrix {
                    e: moved.x,
                    f: moved.y,
                    ..compose(render, Some(&matrix))
                }
            }
        };
        let matrix_at = self.workspace + workspace::MATRIX;
        os.write_words(matrix_at, &painted.words())?;
        let at = os_units(render.apply_truncating(text.at));
        let flags = own.map_or(0, |(_, flags)| {
            (flags & TEXT_FLAGS) << PAINT_TEXT_FLAGS_SHIFT
        });
        let mut paint = Regs::default();
        paint.r[..7].copy_from_slice(&[
            handle,
            text.string,
            font::flag::OS_UNITS | font::flag::MATRIX | font::flag::HANDLE | flags,
            at.x.cast_unsigned(),
            at.y.cast_unsigned(),
            0,
            matrix_at,
        ]);
        os.perform(X | font::PAINT, &mut paint)
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
            let drawn = self.mask_character(os, character);
            let mut back = Regs::default();
            back.r[..4].copy_from_slice(&switch.r[..4]);
            os.perform(X | kos::SPRITE_OP, &mut back)?;
            drawn?;
            let cell = Matrix {
                a: sx.wrapping_mul(CELL_SCALE.0),
                b: 0,
                c: 0,
                d: sy.wrapping_mul(CELL_SCALE.1),
                e: at.x.wrapping_add(n.wrapping_mul(sx)),
                f: at.y.wrapping_sub(sy >> workspace::SIDE_SHIFT),
            };
            self.plot_sprite(os, sprite, &compose(through, Some(&cell)), Source::Palette)?;
        }
        Ok(())
    }

    /// `character` written into the work sprite's mask as RISC OS 5.30's
    /// DrawFile writes it: `GCOL 3,0` by `OS_WriteN`, a move by `OS_Plot`,
    /// the character by `OS_WriteC`.
    fn mask_character(&mut self, os: &mut Os<'_>, character: u8) -> Result<(), Fail> {
        let gcol = self.workspace + workspace::MASK_GCOL;
        os.write_bytes(gcol, &MASK_GCOL)?;
        let mut write = Regs::default();
        write.r[..2].copy_from_slice(&[gcol, u32::try_from(MASK_GCOL.len()).unwrap_or(0)]);
        os.perform(X | kos::WRITE_N, &mut write)?;
        plot(os, MOVE, MASK_MOVE.0, MASK_MOVE.1)?;
        write_c(os, character)
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
            Source::Palette => (BARE_SPRITE, sprite, SPRITE_POINTER),
            Source::Mode(mode) => (mode, CURRENT, matrix_at),
        };
        select.r[..6].copy_from_slice(&[r0, r1, CURRENT, CURRENT, table, r5]);
        os.perform(X | colour_trans::SELECT_TABLE, &mut select)?;
        os.write_words(matrix_at, &matrix.words())?;
        let mut put = Regs::default();
        put.r[..8].copy_from_slice(&[
            PLOT_TRANSFORMED,
            sprite_op::NO_AREA,
            sprite,
            0,
            0,
            sprite_op::THROUGH_MASK,
            matrix_at,
            table,
        ]);
        os.perform(X | kos::SPRITE_OP, &mut put)
    }

    /// The source a sprite's colours are translated from: its own palette,
    /// or its mode's.
    fn source_of(os: &mut Os<'_>, sprite: u32) -> Result<Source, Fail> {
        let image = os.read_word(sprite.wrapping_add(sprite_control::IMAGE))?;
        if image > SPRITE_HEADER_LEN {
            return Ok(Source::Palette);
        }
        Ok(Source::Mode(
            os.read_word(sprite.wrapping_add(sprite_control::MODE))?,
        ))
    }

    /// A sprite scaled to fill its bounds.
    fn sprite(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let sprite = self.address(object.at + OBJECT_HEADER);
        let mut info = Regs::default();
        info.r[..3].copy_from_slice(&[READ_INFO, sprite_op::NO_AREA, sprite]);
        os.perform(X | kos::SPRITE_OP, &mut info)?;
        let (width, height, mode) = (info.r[3], info.r[4], info.r[6]);
        let (x_eig, y_eig) = (
            mode_variable(os, mode, var::X_EIG)?,
            mode_variable(os, mode, var::Y_EIG)?,
        );
        let [x0, y0, x1, y1] = box_at(object.bytes, OBJECT_BOUNDS);
        let scale = |span: i32, pixels: u32, eig: u32| {
            let units = i64::from(pixels) << eig.min(8) << DRAW_UNIT_SHIFT;
            if units == 0 {
                return 0;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a scale kept to a word as RISC OS keeps it"
            )]
            let scale = ((i64::from(span) << FRACTION_BITS) / units) as i32;
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

    /// A text area, painted through the Font Manager.
    fn text_area(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let places = text_area::Places {
            buffer: self.workspace + workspace::TEXT,
            block: self.workspace + workspace::COORDINATES,
            matrix: self.workspace + workspace::MATRIX,
            name: self.workspace + workspace::NAME,
            name_bytes: workspace::NAME_BYTES as usize,
        };
        text_area::paint(os, &Area::of(object), &self.asked.matrix, places)
    }

    /// A JPEG through its own matrix.
    fn jpeg(&mut self, os: &mut Os<'_>, object: Object<'_>) -> Result<(), Fail> {
        let own = matrix_in(object.bytes, JPEG_MATRIX);
        let length = le_u32(object.bytes, JPEG_LENGTH).unwrap_or(0);
        let matrix_at = self.workspace + workspace::MATRIX;
        os.write_words(matrix_at, &compose(&self.asked.matrix, Some(&own)).words())?;
        let mut regs = Regs::default();
        let data = self.address(object.at + JPEG_DATA);
        // R1's parallelogram bit clear: R2 is a matrix.
        regs.r[..4].copy_from_slice(&[data, 0, matrix_at, length]);
        os.perform(X | jpeg::PLOT_TRANSFORMED, &mut regs)
    }
}

/// A text object's fields DrawFile paints it by.
#[derive(Clone, Copy, Debug)]
struct Text {
    foreground: u32,
    background: u32,
    /// Its width and height, in draw units.
    sizes: (i32, i32),
    /// Its base point.
    at: Point,
    /// Where its characters are.
    string: u32,
}

/// The Font Manager's cache asked after, as RISC OS 5.30's DrawFile asks
/// before rendering a file or declaring its fonts, the answer unused even
/// when there is no Font Manager to give one.
///
/// # Errors
///
/// What unwinds.
pub(super) fn ask_font_cache(os: &mut Os<'_>) -> Result<(), Fail> {
    os.perform_quietly(X | font::CACHE_ADDR, &mut Regs::default())?;
    Ok(())
}

/// A point in draw units in OS units, cut toward nought.
fn os_units(p: Point) -> Point {
    let units = DRAW_UNITS_PER_OS_UNIT.cast_signed();
    Point::new(p.x / units, p.y / units)
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
    let units = |v: i32| (v >> DRAW_UNIT_SHIFT).cast_unsigned();
    plot(os, MOVE, units(x0), units(y1))?;
    for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        plot(os, DOTTED, units(x), units(y))?;
    }
    Ok(())
}

/// The matrix six words of `bytes` from `at` hold, nought for a word past
/// them.
fn matrix_in(bytes: &[u8], at: usize) -> Matrix {
    Matrix::from_words(format::matrix_words(bytes, at).map(|word| word.unwrap_or(0)))
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
        let product =
            ((i64::from(m) * i64::from(n) + i64::from(FIXED_ONE / 2)) >> FRACTION_BITS) as i32;
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

/// The box at `at` of `bytes`, in draw units: its low corner, then its high
/// one, a word of it nought where `bytes` end before it.
pub(super) fn box_at(bytes: &[u8], at: usize) -> [i32; 4] {
    core::array::from_fn(|i| le_i32(bytes, at.saturating_add(4 * i)).unwrap_or(0))
}

/// Where the path object at `at` in the file holds its path, past its dash
/// pattern of `count` lengths where it has one; `None` where no 32-bit
/// offset reaches that, refusing the object as a file's reader does, alike
/// on every target.
fn path_offset(at: usize, count: Option<u32>) -> Option<usize> {
    let pattern = match count {
        Some(count) => dash_pattern_bytes(usize::try_from(count).ok()?)?,
        None => 0,
    };
    let offset = at.checked_add(PATH_DASHES)?.checked_add(pattern)?;
    u32::try_from(offset).is_ok().then_some(offset)
}

/// The Draw module's cap and join block for path style `style`.
fn cap_join_block(style: u32) -> [u32; 4] {
    use tairix_drawing::riscos::format::{
        CODE, END_CAP_SHIFT, MITRE_LIMIT, START_CAP_SHIFT, TRIANGLE_LENGTH_SHIFT, TRIANGLE_SIZE,
        TRIANGLE_WIDTH_SHIFT,
    };
    let size = |shift: u32| ((style >> shift) & TRIANGLE_SIZE) * TRIANGLE_SCALE;
    let triangle = Triangle {
        width: size(TRIANGLE_WIDTH_SHIFT),
        length: size(TRIANGLE_LENGTH_SHIFT),
    };
    Style {
        join: style & CODE,
        end_cap: (style >> END_CAP_SHIFT) & CODE,
        start_cap: (style >> START_CAP_SHIFT) & CODE,
        mitre: (MITRE_LIMIT << FRACTION_BITS).cast_signed(),
        end_triangle: triangle,
        start_triangle: triangle,
    }
    .words()
}

/// The bytes of `bytes` from `at` to the next nought.
pub(super) fn terminated(bytes: &[u8], at: usize) -> &[u8] {
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

fn plot(os: &mut Os<'_>, code: u8, x: u32, y: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[u32::from(code), x, y]);
    os.perform(X | kos::PLOT, &mut regs)
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
        // The style of a recorded file: round caps and a round join, triangle
        // sizes in the top half.
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

    #[test]
    fn a_dash_pattern_no_offset_reaches_refuses_its_path_on_every_target() {
        assert_eq!(path_offset(100, None), Some(100 + 40));
        assert_eq!(path_offset(100, Some(3)), Some(100 + 40 + 8 + 4 * 3));
        // Past any 32-bit offset on a 64-bit host, and past a usize on a
        // 32-bit one.
        assert_eq!(path_offset(100, Some(u32::MAX)), None);
        assert_eq!(path_offset(usize::MAX - 8, None), None);
    }
}
