//! DrawFile 1.62 as RISC OS 5.30 has it: a Draw file in guest memory checked
//! whole, then rendered, measured or its fonts declared, through the one
//! definition of the format `lib/drawing` holds.
//!
//! Its errors are RISC OS's numbers in TAIRiX Bridge's words. It issues no
//! service for an object it does not know, which it passes over, as RISC OS
//! 5.30's does. Text in a named font and text areas are painted by the Font
//! Manager; text in no font, or one the Font Manager refuses, is drawn in the
//! system font.

mod check;
mod render;
mod stack;
mod text_area;
mod workspace;

use tairix_drawing::riscos::format;
use tairix_drawing::riscos::units::{DRAW_UNITS_PER_OS_UNIT, DRAW_UNIT_SHIFT};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::{filetype, fs::kind};
use tairix_riscos_kernel::module::{Builtin, Command, Resource, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::msgtrans::Own;
use tairix_riscos_kernel::os::{Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::{chunk, os as kos, pdriver, X};

use crate::geometry::Matrix;
use crate::process::matrix_at;
use check::{Refusal, Stop, Within};
use render::{Asked, Renderer, FLATNESS};

const NAMES: &[&str] = &["Render", "BBox", "DeclareFonts"];

mod call {
    pub(super) const RENDER: u32 = 0;
    pub(super) const BBOX: u32 = 1;
    pub(super) const DECLARE_FONTS: u32 = 2;
}

/// `DrawFile_Render`'s flags.
mod flag {
    pub(super) const BOXES: u32 = 1;
    pub(super) const SUPPRESS: u32 = 1 << 1;
    pub(super) const FLATNESS: u32 = 1 << 2;
}

/// `DrawFile_DeclareFonts`'s flag passed on to `PDriver_DeclareFont`.
const NO_DOWNLOAD: u32 = 1;

const MESSAGES: &[u8] = include_bytes!("../../resources/DrawFile/Messages");

const RENDER_SYNTAX: &str =
    "Syntax: *Render [-file] <filename> [<a> [<b> [<c> [<d> [<e> [<f>]]]]]] [-bbox] [-suppress] [-flatness <n>]";
/// `*Render`'s arguments, as `OS_ReadArgs` reads them.
const RENDER_ARGUMENTS: &[u8] = b"file/A,a,b,c,d,e,f,bbox/S,suppress/S,flatness/K\0";

/// The DrawFile module.
pub(crate) static DRAW_FILE: Builtin = Builtin {
    title: "DrawFile",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::DRAW_FILE,
        prefix: "DrawFile",
        names: NAMES,
        handler: perform,
    }),
    services: None,
    commands: &[Command::new(
        "Render",
        (1, 12),
        "Renders a Draw file on the screen.",
        Some(RENDER_SYNTAX),
        render_command,
    )],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.DrawFile.Messages",
        file_type: filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

struct State {
    own: Own,
    workspace: u32,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = os.rma_claim(workspace::BYTES)?;
    os.write_word(regs.r[12], at)?;
    let own = workspace::MESSAGES_FILE.place(os, at)?;
    workspace::lay_out(os, at)?;
    os.hold_extension(State { own, workspace: at })
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        call::RENDER => render(os, regs),
        call::BBOX => bbox(os, regs),
        call::DECLARE_FONTS => declare_fonts(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[DRAW_FILE.title.as_bytes()])),
    }
}

fn workspace(os: &mut Os<'_>) -> u32 {
    os.extension::<State>().map_or(0, |state| state.workspace)
}

/// The error a refusal raises.
fn raise(os: &mut Os<'_>, refusal: Refusal) -> Fail {
    let hex = |v: usize| tairix_riscos_kernel::hex(u32::try_from(v).unwrap_or(u32::MAX));
    match refusal {
        Refusal::NotDraw => os.own_error(own, error(0x20C00, "NotDraw"), &[]),
        Refusal::Version(major) => {
            let number = decimal(major);
            os.own_error(own, error(0x20C01, "Version"), &[number.as_bytes()])
        }
        Refusal::TwoFontTables { at } => {
            os.own_error(own, error(0x20C02, "FontTables"), &[hex(at).as_bytes()])
        }
        Refusal::NoSuchFont { number, at } => {
            let number = decimal(number);
            os.own_error(
                own,
                error(0x20C03, "NoFont"),
                &[number.as_bytes(), hex(at).as_bytes()],
            )
        }
        Refusal::TooBig { within, after } => {
            let (number, token) = match (within, after) {
                (Within::File, None) => (0x20C05, "TooBig"),
                (Within::File, Some(_)) => (0x20C05, "TooBigAt"),
                (Within::Group, _) => (0x20C06, "TooBigGroup"),
                (Within::Tagged, _) => (0x20C07, "TooBigTagged"),
            };
            let at = hex(after.unwrap_or(0));
            os.own_error(own, error(number, token), &[at.as_bytes()])
        }
    }
}

/// A text area's refusal raised.
fn raise_area(os: &mut Os<'_>, refusal: text_area::Refusal) -> Fail {
    match refusal {
        text_area::Refusal::Command(command) => {
            os.own_error(own, error(0x20C08, "AreaCommand"), &[&[command]])
        }
        text_area::Refusal::UndefinedFont(number) => {
            let number = decimal(number);
            os.own_error(own, error(0x20C09, "AreaFont"), &[number.as_bytes()])
        }
        text_area::Refusal::Version(version) => {
            let version = decimal(version);
            os.own_error(own, error(0x20C0A, "AreaVersion"), &[version.as_bytes()])
        }
        text_area::Refusal::NoVersion => os.own_error(own, error(0x20C0B, "AreaNoVersion"), &[]),
        text_area::Refusal::NoRoom => os.no_memory(),
    }
}

const fn error(number: u32, token: &'static str) -> Error {
    Error { number, token }
}

fn decimal(value: u32) -> tairix_riscos_kernel::Decimal {
    tairix_riscos_kernel::Decimal::of(value.cast_signed())
}

/// The file R1 and R2 give, read whole, and its fonts if it is sound.
fn checked(os: &mut Os<'_>, regs: &Regs) -> Result<(alloc::vec::Vec<u8>, check::Fonts), Fail> {
    let length = usize::try_from(regs.r[2]).unwrap_or(usize::MAX);
    let bytes = os.read_vec(regs.r[1], length)?;
    match check::check(&bytes) {
        Ok(fonts) => Ok((bytes, fonts)),
        Err(Stop::Refused(refusal)) => Err(raise(os, refusal)),
        Err(Stop::NoRoom) => Err(os.no_memory()),
        Err(Stop::Overflowed) => Err(Fail::Unwind(os.overflowed())),
    }
}

/// The matrix R3 gives, and its address; the identity in the workspace for
/// none.
fn matrix_of(os: &mut Os<'_>, at: u32, workspace: u32) -> Result<(Matrix, u32), Fail> {
    let address = if at == 0 {
        workspace + workspace::IDENTITY
    } else {
        at
    };
    Ok((matrix_at(os, at)?, address))
}

/// `DrawFile_Render`.
fn render(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (bytes, fonts) = checked(os, regs)?;
    render::ask_font_cache(os)?;
    let at = workspace(os);
    let (matrix, matrix_at) = matrix_of(os, regs.r[3], at)?;
    let clip = if regs.r[4] == 0 {
        None
    } else {
        let mut words = [0; 4];
        os.read_words(regs.r[4], &mut words)?;
        let [x0, y0, x1, y1] = words.map(|v| v.cast_signed().wrapping_shl(DRAW_UNIT_SHIFT));
        Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
    };
    let flags = regs.r[0];
    let asked = Asked {
        boxes: flags & flag::BOXES != 0,
        suppress: flags & flag::SUPPRESS != 0,
        flatness: if flags & flag::FLATNESS != 0 {
            regs.r[5]
        } else {
            FLATNESS
        },
        matrix,
        matrix_at,
        clip,
    };
    let mut renderer = Renderer::new(&bytes, regs.r[1], &fonts, asked, at);
    renderer.render(os)?;
    match renderer.first {
        Some(block) => Err(Fail::Error(block)),
        None => Ok(()),
    }
}

/// `DrawFile_BBox`: the header's box through the matrix, in draw units.
fn bbox(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (bytes, _) = checked(os, regs)?;
    let at = workspace(os);
    let (matrix, _) = matrix_of(os, regs.r[3], at)?;
    let bounds = render::transformed_box(&matrix, render::box_at(&bytes, format::HEADER_BOUNDS));
    os.write_words(regs.r[4], &bounds.map(i32::cast_unsigned))
}

/// `DrawFile_DeclareFonts`: each font the file uses declared to the printer
/// driver, in the file's order — the font table's, and each text area's —
/// R0's bit 0 asking that none be downloaded.
fn declare_fonts(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (bytes, fonts) = checked(os, regs)?;
    render::ask_font_cache(os)?;
    let objects = format::Objects::new(&bytes[format::HEADER..], format::HEADER);
    let mut declaring = Declaring {
        file: regs.r[1],
        flags: regs.r[0] & NO_DOWNLOAD,
        fonts: &fonts,
        name: workspace(os) + workspace::NAME,
    };
    declaring.run(os, objects, stack::Depth::default())
}

/// What declaring a file's fonts needs.
struct Declaring<'f> {
    file: u32,
    flags: u32,
    fonts: &'f check::Fonts,
    /// Where a text area's font's name is written out.
    name: u32,
}

impl Declaring<'_> {
    fn run(
        &mut self,
        os: &mut Os<'_>,
        objects: format::Objects<'_>,
        depth: stack::Depth,
    ) -> Result<(), Fail> {
        for object in objects.map_while(Result::ok) {
            self.object(os, object, depth)?;
        }
        Ok(())
    }

    fn object(
        &mut self,
        os: &mut Os<'_>,
        object: format::Object<'_>,
        depth: stack::Depth,
    ) -> Result<(), Fail> {
        match object.kind {
            format::FONT_TABLE => {
                for &(_, name) in &self.fonts.names {
                    let at = self
                        .file
                        .wrapping_add(u32::try_from(name).unwrap_or(u32::MAX));
                    self.declare(os, at)?;
                }
            }
            format::TEXT_AREA => {
                let names = text_area::Area::of(object)
                    .fonts()
                    .map_err(|refusal| raise_area(os, refusal))?;
                for name in names {
                    workspace::write_name(os, self.name, workspace::NAME_BYTES as usize, name)?;
                    self.declare(os, self.name)?;
                }
            }
            format::GROUP => {
                let deeper = depth.deeper(os, stack::DECLARE, stack::Level::Group)?;
                self.run(os, stack::group_objects(object), deeper)?;
            }
            format::TAGGED => {
                if let Some(Ok(inner)) = stack::tagged_object(object) {
                    let deeper = depth.deeper(os, stack::DECLARE, stack::Level::Tagged)?;
                    self.object(os, inner, deeper)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn declare(&self, os: &mut Os<'_>, name: u32) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[0, name, self.flags]);
        os.perform(X | pdriver::DECLARE_FONT, &mut regs)
    }
}

/// The number a `*Render` argument gives, scaled by `scale`; `None` for
/// one that is no number.
fn scaled(text: &[u8], scale: i64) -> Option<i32> {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    };
    let (whole, fraction) = match digits.iter().position(|&b| b == b'.') {
        Some(point) => (&digits[..point], &digits[point + 1..]),
        None => (digits, &[][..]),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let mut value: i64 = 0;
    for &digit in whole {
        let digit = i64::from(digit.checked_sub(b'0').filter(|d| *d < 10)?);
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    let mut scaled = value.checked_mul(scale)?;
    let mut denominator: i64 = 1;
    let mut numerator: i64 = 0;
    for &digit in fraction.iter().take(9) {
        let digit = i64::from(digit.checked_sub(b'0').filter(|d| *d < 10)?);
        numerator = numerator * 10 + digit;
        denominator *= 10;
    }
    scaled = scaled.checked_add((numerator * scale + denominator / 2) / denominator)?;
    i32::try_from(if negative { -scaled } else { scaled }).ok()
}

/// `*Render`: the file loaded into the RMA and rendered, its arguments the
/// matrix's entries (the translation in OS units) and the flags.
fn render_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    const SLOTS: usize = 10;
    const ARGUMENTS_ROOM: u32 = 512;
    let template = os.rma_claim_with(RENDER_ARGUMENTS)?;
    let result = os.with_rma_block(ARGUMENTS_ROOM, |os, buffer| {
        let mut read = Regs::default();
        read.r[..4].copy_from_slice(&[template, regs.r[0], buffer, ARGUMENTS_ROOM]);
        os.perform(X | kos::READ_ARGS, &mut read)?;
        let mut slots = [0; SLOTS];
        os.read_words(buffer, &mut slots)?;
        let text = |os: &mut Os<'_>, at: u32| os.read_text(at, ARGUMENTS_ROOM as usize);
        let fixed_one = i64::from(format::FIXED_ONE);
        let draw_units = i64::from(DRAW_UNITS_PER_OS_UNIT);
        let mut matrix = Matrix::IDENTITY;
        let entries = [
            (&mut matrix.a, fixed_one),
            (&mut matrix.b, fixed_one),
            (&mut matrix.c, fixed_one),
            (&mut matrix.d, fixed_one),
            (&mut matrix.e, draw_units),
            (&mut matrix.f, draw_units),
        ];
        for ((entry, scale), &slot) in entries.into_iter().zip(&slots[1..7]) {
            if slot != 0 {
                if let Some(value) = scaled(&text(os, slot)?, scale) {
                    *entry = value;
                }
            }
        }
        let mut flags = 0;
        if slots[7] != 0 {
            flags |= flag::BOXES;
        }
        if slots[8] != 0 {
            flags |= flag::SUPPRESS;
        }
        let mut flatness = 0;
        if slots[9] != 0 {
            flags |= flag::FLATNESS;
            flatness = scaled(&text(os, slots[9])?, draw_units)
                .unwrap_or(0)
                .cast_unsigned();
        }
        let at = workspace(os);
        os.write_words(at + workspace::MATRIX, &matrix.words())?;
        load_and_render(os, slots[0], flags, at + workspace::MATRIX, flatness)
    });
    os.free_rma_quietly(template);
    result
}

/// The file named at `name` loaded into the RMA and rendered.
fn load_and_render(
    os: &mut Os<'_>,
    name: u32,
    flags: u32,
    matrix: u32,
    flatness: u32,
) -> Result<(), Fail> {
    let mut info = Regs::default();
    info.r[..2].copy_from_slice(&[kos::file::READ_INFO, name]);
    os.perform(X | kos::FILE, &mut info)?;
    let length = if info.r[0] == kind::FILE {
        info.r[4]
    } else {
        0
    };
    os.with_rma_block(length, |os, at| {
        let mut load = Regs::default();
        load.r[..4].copy_from_slice(&[kos::file::LOAD, name, at, 0]);
        os.perform(X | kos::FILE, &mut load)?;
        let mut render = Regs::default();
        render.r[..6].copy_from_slice(&[flags, at, length, matrix, 0, flatness]);
        os.perform(X | (chunk::DRAW_FILE + call::RENDER), &mut render)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_render_argument_is_scaled_and_one_past_the_arithmetic_is_no_number() {
        let fixed_one = i64::from(format::FIXED_ONE);
        assert_eq!(scaled(b"1.5", fixed_one), Some(0x1_8000));
        assert_eq!(scaled(b"-0.25", fixed_one), Some(-0x4000));
        assert_eq!(scaled(b"x", fixed_one), None);
        // The fraction's rounding carries the whole past what the sum holds.
        assert_eq!(scaled(b"140737488355327.999999999", fixed_one), None);
    }
}
