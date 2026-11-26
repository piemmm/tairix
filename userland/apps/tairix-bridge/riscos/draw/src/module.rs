//! The Draw module, 1.22 as RISC OS 5.30 has it. Each SWI calls `DrawV`
//! with R8 its index, so a claimant may answer any; the vector's default
//! does the work, and the calls that are `Draw_ProcessPath` with set flags
//! call the vector again as `Draw_ProcessPath` with R1 and R7 made, so a
//! claimant sees that call too, as on RISC OS.
//!
//! The floating-point forms make their registers as the integer ones do
//! and call `Draw_ProcessPath`'s floating-point form, which refuses, as
//! RISC OS 5.30's does. RISC OS's `Draw_FillClippedFP` returns with V set
//! and R0 pointing at the path rather than at an error; here it refuses as
//! the others do.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::module::{Builtin, Resource, Swis, VectorDefault, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::mouse::Bounds;
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::chunk;
use tairix_riscos_kernel::vectors;

use crate::fill::Surface;
use crate::process::special::{COUNT, FILL, FILL_EACH};
use crate::process::{self, flag, Call, Clip, NO_DASH, PLOTTED};
use crate::raster::class;
use crate::stage::{Guest, Refusal, Span, Stop};

/// Its SWIs, in their chunk's order.
const NAMES: &[&str] = &[
    "ProcessPath",
    "ProcessPathFP",
    "Fill",
    "FillFP",
    "Stroke",
    "StrokeFP",
    "StrokePath",
    "StrokePathFP",
    "FlattenPath",
    "FlattenPathFP",
    "TransformPath",
    "TransformPathFP",
    "FillClipped",
    "FillClippedFP",
    "StrokeClipped",
    "StrokeClippedFP",
];

/// The calls, by their index.
pub(crate) mod call {
    pub(crate) const PROCESS_PATH: u32 = 0;
    pub(super) const PROCESS_PATH_FP: u32 = 1;
    pub(crate) const FILL: u32 = 2;
    pub(super) const FILL_FP: u32 = 3;
    pub(crate) const STROKE: u32 = 4;
    pub(super) const STROKE_FP: u32 = 5;
    pub(super) const STROKE_PATH: u32 = 6;
    pub(super) const STROKE_PATH_FP: u32 = 7;
    pub(super) const FLATTEN_PATH: u32 = 8;
    pub(super) const FLATTEN_PATH_FP: u32 = 9;
    pub(super) const TRANSFORM_PATH: u32 =
        tairix_riscos_kernel::swi::draw::TRANSFORM_PATH - tairix_riscos_kernel::swi::chunk::DRAW;
    pub(super) const TRANSFORM_PATH_FP: u32 = 11;
    pub(super) const FILL_CLIPPED: u32 = 12;
    pub(super) const FILL_CLIPPED_FP: u32 = 13;
    pub(super) const STROKE_CLIPPED: u32 = 14;
    pub(super) const STROKE_CLIPPED_FP: u32 = 15;
}

/// Its messages, and their file, kept at its workspace's start.
const MESSAGES: &[u8] = include_bytes!("../resources/Draw/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.Draw.Messages\0");

/// The fill styles a call gives for nought: a fill's, the thinnest line's
/// and a thick line's, which fills as a fill does.
pub(crate) const FILL_STYLE: u32 = class::INTERIOR_BOUNDARY | class::INTERIOR;
const THIN_STYLE: u32 = class::EXTERIOR_BOUNDARY | class::INTERIOR_BOUNDARY;
const THICK_STYLE: u32 = FILL_STYLE;
/// The style bits the convenience calls take; a stroke's top bit plots its
/// subpaths all at once.
const STYLE_BITS: u32 = 0xFF;
const ALL_AT_ONCE: u32 = 1 << 31;
/// R5 of the clipped fill's second call, which RISC OS sets with R4 its
/// workspace.
const CLIP_WORDS: u32 = 12;

/// The Draw module.
pub(crate) static DRAW: Builtin = Builtin {
    title: "Draw",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::DRAW,
        prefix: "Draw",
        names: NAMES,
        handler: perform,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[VectorDefault {
        vector: vectors::DRAW,
        code: vector,
    }],
    resources: &[Resource {
        name: "Resources.Draw.Messages",
        file_type: filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// What Draw holds while it is active.
struct State {
    own: Own,
    workspace: u32,
    clip: Clip,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(MESSAGES_FILE.room())?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.hold_extension(State {
        own,
        workspace,
        clip: Clip::default(),
    })
}

/// Its state released; the kernel frees its workspace from its private
/// word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// A Draw SWI: `DrawV` called with R8 its index, the caller's R8 kept; one
/// past the chunk's names is refused without calling it.
fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    if offset as usize >= NAMES.len() {
        return Err(out_of_range(os));
    }
    let kept = regs.r[8];
    regs.r[8] = offset;
    regs.flags = 0;
    let answered = os.call_vector(vectors::DRAW, regs, Entry::Supervisor);
    regs.r[8] = kept;
    answered?;
    if regs.flag(psr::V) {
        return Err(Fail::Error(regs.r[0]));
    }
    Ok(())
}

fn out_of_range(os: &mut Os<'_>) -> Fail {
    os.error_with(SWI_OUT_OF_RANGE, &[DRAW.title.as_bytes()])
}

/// `DrawV`'s default: the call R8 names.
fn vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[8] {
        call::PROCESS_PATH => process_path(os, regs),
        call::PROCESS_PATH_FP | call::FILL_CLIPPED_FP | call::STROKE_CLIPPED_FP => {
            Err(refuse(os, Refusal::Unsupported))
        }
        call::FILL => fill(os, regs, false),
        call::FILL_FP => fill(os, regs, true),
        call::STROKE => stroke(os, regs, false),
        call::STROKE_FP => stroke(os, regs, true),
        call::STROKE_PATH => stroke_path(os, regs, false),
        call::STROKE_PATH_FP => stroke_path(os, regs, true),
        call::FLATTEN_PATH => flatten_path(os, regs, false),
        call::FLATTEN_PATH_FP => flatten_path(os, regs, true),
        call::TRANSFORM_PATH => transform_path(os, regs, false),
        call::TRANSFORM_PATH_FP => transform_path(os, regs, true),
        call::FILL_CLIPPED => fill_clipped(os, regs),
        call::STROKE_CLIPPED => stroke_clipped(os, regs),
        _ => Err(out_of_range(os)),
    }
}

/// The error a refusal raises, in Draw's words with RISC OS's numbers.
pub(crate) const fn error_of(refusal: Refusal) -> Error {
    let (number, token) = match refusal {
        Refusal::Reserved => (0x982, "Reserved"),
        Refusal::BadElement => (0x984, "BadElement"),
        Refusal::Looped => (0x984, "Looped"),
        Refusal::Order => (0x985, "Order"),
        Refusal::MayChangeLength => (0x986, "Lengthens"),
        Refusal::OutputFull => (0x987, "Full"),
        Refusal::NeedsFlattening => (0x988, "Flatten"),
        Refusal::BadCapJoin => (0x989, "CapJoin"),
        Refusal::Overflow => (0x98A, "Overflow"),
        Refusal::Unsupported => (0x9FF, "Unsupported"),
    };
    Error { number, token }
}

fn refuse(os: &mut Os<'_>, refusal: Refusal) -> Fail {
    os.own_error(own, error_of(refusal), &[])
}

/// What processing's stop raises: Draw's refusals in its words; a
/// flattening that never ends as RISC OS's does, overflowing the stack.
pub(crate) fn raise(os: &mut Os<'_>, stop: Stop) -> Fail {
    match stop {
        Stop::Refused(refusal) => refuse(os, refusal),
        Stop::Fail(fail) => fail,
        Stop::NoRoom => os.no_memory(),
        Stop::TooDeep => Fail::Unwind(os.overflowed()),
    }
}

impl Guest for Os<'_> {
    fn read_word(&mut self, at: u32) -> Result<u32, Fail> {
        Os::read_word(self, at)
    }

    fn write_word(&mut self, at: u32, value: u32) -> Result<(), Fail> {
        Os::write_word(self, at, value)
    }

    fn plot(&mut self, spans: &[Span]) -> Result<(), Fail> {
        tairix_riscos_vdu::render::plot_foreground(self, spans)
    }

    fn yield_point(&mut self) -> Result<(), Fail> {
        Os::yield_point(self).map_err(Fail::Unwind)
    }
}

/// Where output goes now, as a fill sees it: nowhere, where it cannot show
/// graphics.
fn surface(os: &mut Os<'_>) -> Surface {
    match tairix_riscos_vdu::render::surface(os) {
        Some(surface) => Surface::of(&surface),
        None => Surface {
            eig: (0, 0),
            origin: (0, 0),
            window: Bounds {
                left: 0,
                bottom: 0,
                right: -1,
                top: -1,
            },
        },
    }
}

/// `Draw_ProcessPath`.
fn process_path(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut r = [0; 8];
    r.copy_from_slice(&regs.r[..8]);
    let request = Call::of(&r);
    let surface = surface(os);
    let mut clip = os
        .extension::<State>()
        .map(|state| core::mem::take(&mut state.clip))
        .unwrap_or_default();
    let room = os.ceiling_bytes();
    let processed = process::process(os, &request, surface, &mut clip, room);
    if let Some(state) = os.extension::<State>() {
        state.clip = clip;
    }
    match processed {
        Ok(r0) => {
            regs.r[0] = r0;
            Ok(())
        }
        Err(stop) => Err(raise(os, stop)),
    }
}

/// `regs` passed to `DrawV` again as `Draw_ProcessPath`, or its floating-point
/// form, with `made` made of them; the registers it left.
fn again(
    os: &mut Os<'_>,
    regs: &Regs,
    fp: bool,
    made: impl FnOnce(&mut [u32; 13]),
) -> Result<Regs, Fail> {
    let mut inner = *regs;
    made(&mut inner.r);
    inner.r[8] = if fp {
        call::PROCESS_PATH_FP
    } else {
        call::PROCESS_PATH
    };
    inner.flags = 0;
    os.call_vector(vectors::DRAW, &mut inner, Entry::Supervisor)?;
    if inner.flag(psr::V) {
        return Err(Fail::Error(inner.r[0]));
    }
    Ok(inner)
}

/// A convenience call's fill style, refused with reserved bits outside
/// `allowed`.
fn style_of(os: &mut Os<'_>, style: u32, allowed: u32) -> Result<u32, Fail> {
    if style & !allowed != 0 {
        return Err(refuse(os, Refusal::Reserved));
    }
    Ok(style)
}

/// `Draw_Fill`: `Draw_ProcessPath` closing, flattening and filling all at once.
fn fill(os: &mut Os<'_>, regs: &mut Regs, fp: bool) -> Result<(), Fail> {
    let style = match style_of(os, regs.r[1], STYLE_BITS)? {
        0 => FILL_STYLE,
        given => given,
    };
    let done = again(os, regs, fp, |r| {
        r[1] = flag::CLOSE | flag::FLATTEN | style;
        r[6] = NO_DASH;
        r[7] = FILL;
    })?;
    regs.r[0] = done.r[0];
    Ok(())
}

/// The R1 and R7 a stroke of `style` and `thickness` passes on: the
/// thinnest line flattened, a thick one thickened and flattened again.
fn stroke_flags(style: u32, thickness: u32) -> (u32, u32) {
    let plot = if style & ALL_AT_ONCE != 0 {
        FILL
    } else {
        FILL_EACH
    };
    let style = style & STYLE_BITS;
    let flags = if thickness == 0 {
        flag::FLATTEN | flag::THICKEN | if style == 0 { THIN_STYLE } else { style }
    } else {
        flag::FLATTEN
            | flag::THICKEN
            | flag::REFLATTEN
            | if style == 0 { THICK_STYLE } else { style }
    };
    (flags, plot)
}

/// `Draw_Stroke`: `Draw_ProcessPath` thickening and filling a subpath at a time.
fn stroke(os: &mut Os<'_>, regs: &mut Regs, fp: bool) -> Result<(), Fail> {
    let style = style_of(os, regs.r[1], STYLE_BITS | ALL_AT_ONCE)?;
    let (flags, plot) = stroke_flags(style, regs.r[4]);
    let done = again(os, regs, fp, |r| {
        r[1] = flags;
        r[7] = plot;
    })?;
    regs.r[0] = done.r[0];
    Ok(())
}

/// Where a call's output pointer sends the path: nought counts it.
const fn output_of(pointer: u32) -> u32 {
    if pointer == 0 {
        COUNT
    } else {
        pointer
    }
}

/// `Draw_StrokePath`: the stroke's outline written to R1's buffer.
fn stroke_path(os: &mut Os<'_>, regs: &mut Regs, fp: bool) -> Result<(), Fail> {
    let flags = if regs.r[4] == 0 {
        flag::FLATTEN | flag::THICKEN
    } else {
        flag::FLATTEN | flag::THICKEN | flag::REFLATTEN
    };
    let output = output_of(regs.r[1]);
    let done = again(os, regs, fp, |r| {
        r[1] = flags;
        r[7] = output;
    })?;
    regs.r[0] = done.r[0];
    Ok(())
}

/// `Draw_FlattenPath`: R2 the flatness, the matrix none.
fn flatten_path(os: &mut Os<'_>, regs: &mut Regs, fp: bool) -> Result<(), Fail> {
    let flags = if fp {
        flag::FLATTEN | flag::FLOAT
    } else {
        flag::FLATTEN
    };
    let (output, flatness) = (output_of(regs.r[1]), regs.r[2]);
    let done = again(os, regs, fp, |r| {
        r[1] = flags;
        r[2] = 0;
        r[3] = flatness;
        r[7] = output;
    })?;
    regs.r[0] = done.r[0];
    Ok(())
}

/// `Draw_TransformPath`: R1 nought rewrites the path in place.
fn transform_path(os: &mut Os<'_>, regs: &mut Regs, fp: bool) -> Result<(), Fail> {
    let output = regs.r[1];
    let done = again(os, regs, fp, |r| {
        r[1] = 0;
        r[6] = NO_DASH;
        r[7] = output;
    })?;
    regs.r[0] = done.r[0];
    Ok(())
}

/// The clip block at `at`: its path, fill style and matrix.
fn clip_block(os: &mut Os<'_>, at: u32) -> Result<[u32; 3], Fail> {
    let mut words = [0; 3];
    os.read_words(at, &mut words)?;
    Ok(words)
}

/// The clip made from the clip block at `at`, as both clipped calls make
/// it.
fn make_clip(os: &mut Os<'_>, regs: &Regs, at: u32) -> Result<(), Fail> {
    let [path, style, matrix] = clip_block(os, at)?;
    let style = if style == 0 { FILL_STYLE } else { style };
    again(os, regs, false, |r| {
        r[0] = path;
        r[1] = flag::CLOSE | flag::FLATTEN | flag::MAKE_CLIP | style;
        r[2] = matrix;
        r[6] = NO_DASH;
        r[7] = FILL;
    })?;
    Ok(())
}

/// `Draw_FillClipped`: R4 the clip block; the path filled within the clip's
/// pixels. R0 is left the path's address.
fn fill_clipped(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    make_clip(os, regs, regs.r[4])?;
    let style = if regs.r[1] == 0 {
        FILL_STYLE
    } else {
        regs.r[1]
    };
    let workspace = os.extension::<State>().map_or(0, |state| state.workspace);
    again(os, regs, false, |r| {
        r[1] = flag::CLOSE | flag::FLATTEN | flag::WITHIN_CLIP | style;
        r[4] = workspace;
        r[5] = CLIP_WORDS;
        r[6] = NO_DASH;
        r[7] = FILL;
    })?;
    Ok(())
}

/// `Draw_StrokeClipped`: R7 the clip block; the path stroked within the
/// clip's pixels.
fn stroke_clipped(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    make_clip(os, regs, regs.r[7])?;
    let style = style_of(os, regs.r[1], STYLE_BITS | ALL_AT_ONCE)?;
    let (flags, plot) = stroke_flags(style, regs.r[4]);
    again(os, regs, false, |r| {
        r[1] = flags | flag::WITHIN_CLIP;
        r[7] = plot;
    })?;
    regs.r[0] = PLOTTED;
    Ok(())
}
