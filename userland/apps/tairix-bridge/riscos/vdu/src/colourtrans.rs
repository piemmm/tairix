//! ColourTrans 1.97: which colour a mode shows nearest, or furthest from, a
//! wanted one, the tables that translate a sprite's colours, calibration
//! and the colour models, as RISC OS 5 gives each. Each SWI calls `ColourV`
//! with R8 its offset, so a claimant may answer any of them; the module's
//! own answers are the vector's default.

mod calibration;
mod ecf;
mod fonts;
pub(crate) mod matching;
mod models;
pub(crate) mod palettes;
pub(crate) mod readwrite;
mod setting;
mod tables;

use alloc::rc::Rc;

use tairix_colour::Rgb;
use tairix_riscos_arm::psr;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{
    Builtin, Command, CommandKind, Help, Resource, Services, Swis, VectorDefault, SWI_OUT_OF_RANGE,
};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::{gbpb, read_unsigned, GBPB};
use tairix_riscos_kernel::swi::{chunk, colour_trans as swi, X};
use tairix_riscos_kernel::vectors;

use crate::colour::{gcol_of_number, number_of_gcol};
use calibration::Calibration;
use matching::{Loadings, Look};
use palettes::Against;
use tairix_riscos_kernel::swi::os::CURRENT_MODE;
use tairix_vdu::mode::var;

/// Its SWIs, in their chunk's order.
const NAMES: &[&str] = &[
    "SelectTable",
    "SelectGCOLTable",
    "ReturnGCOL",
    "SetGCOL",
    "ReturnColourNumber",
    "ReturnGCOLForMode",
    "ReturnColourNumberForMode",
    "ReturnOppGCOL",
    "SetOppGCOL",
    "ReturnOppColourNumber",
    "ReturnOppGCOLForMode",
    "ReturnOppColourNumberForMode",
    "GCOLToColourNumber",
    "ColourNumberToGCOL",
    "ReturnFontColours",
    "SetFontColours",
    "InvalidateCache",
    "SetCalibration",
    "ReadCalibration",
    "ConvertDeviceColour",
    "ConvertDevicePalette",
    "ConvertRGBToCIE",
    "ConvertCIEToRGB",
    "WriteCalibrationToFile",
    "ConvertRGBToHSV",
    "ConvertHSVToRGB",
    "ConvertRGBToCMYK",
    "ConvertCMYKToRGB",
    "ReadPalette",
    "WritePalette",
    "SetColour",
    "MiscOp",
    "WriteLoadingsToFile",
    "SetTextColour",
    "SetOppTextColour",
    "GenerateTable",
];

/// Its messages, and their file, kept at its workspace's start.
const MESSAGES: &[u8] = include_bytes!("../resources/Colours/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.Colours.Messages\0");
/// The block `MiscOp` 1 points at: RISC OS 5's is internal and
/// undocumented, and all it shows that can be known is the current mode's
/// depth, its fourteenth word.
const DESCRIPTION_AT: u32 = MESSAGES_FILE.room();
const DESCRIPTION_WORDS: u32 = 24;
const DEPTH_IN_DESCRIPTION: u32 = 4 * 13;
const WORKSPACE: u32 = DESCRIPTION_AT + 4 * DESCRIPTION_WORDS;

/// A calibration table whose lists do not each run from level nought to the
/// top.
const BAD_CALIBRATION: Error = Error {
    number: 0xA00,
    token: "BadCalib",
};

/// A CIE conversion whose answer is past a register.
const OVERFLOW: Error = Error {
    number: 0xA01,
    token: "Overflow",
};

/// `ConvertHSVToRGB`'s refusal of a colour with neither hue nor saturation.
const BAD_HSV: Error = Error {
    number: 0xA02,
    token: "BadHSV",
};

/// A deeper mode's font colour offset past the Font Manager's range, which
/// refuses it with its own number.
const BAD_FONT_OFFSET: Error = Error {
    number: 0x206,
    token: "BadFontOffset",
};

/// "`ColourTrans_MiscOp` has no such reason".
const BAD_MISC_OP: Error = Error {
    number: 0xA04,
    token: "BadMiscOp",
};

/// `ColourTrans_MiscOp`'s reasons.
mod misc_op {
    /// The loadings set, the old ones given back.
    pub(super) const SET_LOADINGS: u32 = 0;
    /// The current mode's depth, and the block describing it.
    pub(super) const DESCRIBE: u32 = 1;
    /// Answered with R0 nought: all RISC OS 5 shows of it.
    pub(super) const NOUGHT: u32 = 2;
}

/// The commands that set the loadings and the calibration, which a saved
/// desktop runs.
const LOADINGS_COMMAND: &str = "ColourTransLoadings";
const MAP_SIZE_COMMAND: &str = "ColourTransMapSize";
const MAP_COMMAND: &str = "ColourTransMap";
const MAP_SYNTAX: &str = "Syntax: *ColourTransMap <pair> ...";
/// The most pairs one `*ColourTransMap` takes.
const MAP_MOST: u8 = 255;

/// ColourTrans.
pub(crate) static COLOUR_TRANS: Builtin = Builtin {
    title: "ColourTrans",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::COLOUR_TRANS,
        prefix: "ColourTrans",
        names: NAMES,
        handler: perform,
    }),
    services: Some(Services {
        numbers: &[
            service::MODE_CHANGE,
            service::WIMP_SAVE_DESKTOP,
            service::SWITCHING_OUTPUT_TO_SPRITE,
        ],
        handler: serve,
    }),
    commands: &[
        Command {
            name: LOADINGS_COMMAND,
            kind: CommandKind::Command,
            min: 3,
            max: 3,
            translate: 0b111,
            help: Help::Text(
                "Sets how much an error in red, green and blue each counts when a colour is matched.",
            ),
            syntax: Some("Syntax: *ColourTransLoadings <red> <green> <blue>"),
            run: Some(loadings_command),
        },
        Command {
            name: MAP_SIZE_COMMAND,
            kind: CommandKind::Command,
            min: 3,
            max: 3,
            translate: 0b111,
            help: Help::Text(
                "Sets how many pairs the *ColourTransMap commands after it give for blue, green and red.",
            ),
            syntax: Some("Syntax: *ColourTransMapSize <blue> <green> <red>"),
            run: Some(calibration::map_size_command),
        },
        Command {
            name: MAP_COMMAND,
            kind: CommandKind::Command,
            min: 1,
            max: MAP_MOST,
            translate: 0,
            help: Help::Text(
                "Gives pairs, the last first, toward the screen's calibration, set once all *ColourTransMapSize asked for are given.",
            ),
            syntax: Some(MAP_SYNTAX),
            run: Some(calibration::map_command),
        },
    ],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[VectorDefault {
        vector: vectors::COLOUR,
        code: vector,
    }],
    resources: &[Resource {
        name: "Resources.Colours.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// What ColourTrans holds while it is active.
pub(crate) struct State {
    own: Own,
    workspace: u32,
    loadings: Loadings,
    /// The current mode's colours as first read since they were last made
    /// stale: a palette changed since is not seen until then.
    current: Option<Against>,
    /// The 32K tables built, the oldest first.
    tables: alloc::vec::Vec<tables::ThirtyTwoK>,
    /// The screen's calibration and its RMA copy; none for the default.
    calibration: Option<calibration::Installed>,
    /// The counts `*ColourTransMap` gives, the table's order, and the pairs
    /// it has given toward them, a line at a time.
    map_size: [u32; 3],
    map_given: alloc::vec::Vec<u32>,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.write_bytes(
        workspace + DESCRIPTION_AT,
        &[0; 4 * DESCRIPTION_WORDS as usize],
    )?;
    os.hold_extension(State {
        own,
        workspace,
        loadings: Loadings::DEFAULT,
        current: None,
        tables: alloc::vec::Vec::new(),
        calibration: None,
        map_size: [0; 3],
        map_given: alloc::vec::Vec::new(),
    })
}

/// Its blocks freed but its workspace, which the kernel frees from its
/// private word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    let Some(state) = os.release_extension::<State>() else {
        return Ok(());
    };
    let tables = state.tables.iter().map(|table| table.at);
    for block in tables.chain(state.calibration.map(|installed| installed.block)) {
        os.free_rma_quietly(block);
    }
    Ok(())
}

/// A ColourTrans SWI: `ColourV` called with R8 its offset and C set, the
/// caller's R8 kept and its flags not passed on.
fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let kept = regs.r[8];
    regs.r[8] = offset;
    regs.flags = psr::C;
    let answered = os.call_vector(vectors::COLOUR, regs, Entry::Supervisor);
    regs.r[8] = kept;
    answered?;
    if regs.flag(psr::V) {
        return Err(Fail::Error(regs.r[0]));
    }
    Ok(())
}

/// `ColourV`'s default: the call R8 names, one past the SWIs refused as a
/// SWI past them is.
fn vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match chunk::COLOUR_TRANS.wrapping_add(regs.r[8]) {
        swi::SELECT_TABLE => tables::select_table(os, regs, false),
        swi::GENERATE_TABLE => tables::select_table(os, regs, true),
        swi::SELECT_GCOL_TABLE => tables::select_gcol_table(os, regs),
        swi::RETURN_GCOL => matched(os, regs, Of::Current, Look::Nearest, Answer::Gcol),
        swi::SET_GCOL => setting::set_gcol(os, regs, Look::Nearest),
        swi::SET_OPP_GCOL => setting::set_gcol(os, regs, Look::Furthest),
        swi::SET_COLOUR => setting::set_colour_number(os, regs),
        swi::SET_TEXT_COLOUR => setting::set_text_colour(os, regs, Look::Nearest),
        swi::SET_OPP_TEXT_COLOUR => setting::set_text_colour(os, regs, Look::Furthest),
        swi::RETURN_COLOUR_NUMBER => matched(os, regs, Of::Current, Look::Nearest, Answer::Number),
        swi::RETURN_GCOL_FOR_MODE => matched(os, regs, Of::Named, Look::Nearest, Answer::Gcol),
        swi::RETURN_COLOUR_NUMBER_FOR_MODE => {
            matched(os, regs, Of::Named, Look::Nearest, Answer::Number)
        }
        swi::RETURN_OPP_GCOL => matched(os, regs, Of::Current, Look::Furthest, Answer::Gcol),
        swi::RETURN_OPP_COLOUR_NUMBER => {
            matched(os, regs, Of::Current, Look::Furthest, Answer::Number)
        }
        swi::RETURN_OPP_GCOL_FOR_MODE => matched(os, regs, Of::Named, Look::Furthest, Answer::Gcol),
        swi::RETURN_OPP_COLOUR_NUMBER_FOR_MODE => {
            matched(os, regs, Of::Named, Look::Furthest, Answer::Number)
        }
        swi::GCOL_TO_COLOUR_NUMBER => {
            regs.r[0] = u32::from(number_of_gcol(regs.r[0].to_le_bytes()[0]));
            Ok(())
        }
        swi::COLOUR_NUMBER_TO_GCOL => {
            regs.r[0] = u32::from(gcol_of_number(regs.r[0].to_le_bytes()[0]));
            Ok(())
        }
        swi::READ_PALETTE => readwrite::read_palette(os, regs),
        swi::WRITE_PALETTE => readwrite::write_palette(os, regs),
        swi::RETURN_FONT_COLOURS => fonts::return_font_colours(os, regs),
        swi::SET_FONT_COLOURS => fonts::set_font_colours(os, regs),
        swi::SET_CALIBRATION => calibration::set(os, regs),
        swi::READ_CALIBRATION => calibration::read_back(os, regs),
        swi::CONVERT_DEVICE_COLOUR => calibration::convert_colour(os, regs),
        swi::CONVERT_DEVICE_PALETTE => calibration::convert_palette(os, regs),
        swi::WRITE_CALIBRATION_TO_FILE => calibration::write_to_file(os, regs),
        swi::CONVERT_RGB_TO_CIE => converted(os, regs, models::rgb_to_cie),
        swi::CONVERT_CIE_TO_RGB => converted(os, regs, models::cie_to_rgb),
        swi::CONVERT_RGB_TO_HSV => {
            give_signed(regs, &models::rgb_to_hsv(signed(regs, 0)));
            Ok(())
        }
        swi::CONVERT_HSV_TO_RGB => match models::hsv_to_rgb(signed(regs, 0)) {
            Some(rgb) => {
                give_signed(regs, &rgb);
                Ok(())
            }
            None => Err(os.own_error(own, BAD_HSV, &[])),
        },
        swi::CONVERT_RGB_TO_CMYK => {
            give_signed(regs, &models::rgb_to_cmyk(signed(regs, 0)));
            Ok(())
        }
        swi::CONVERT_CMYK_TO_RGB => {
            give_signed(regs, &models::cmyk_to_rgb(signed(regs, 0)));
            Ok(())
        }
        swi::INVALIDATE_CACHE => invalidate_cache(os),
        swi::MISC_OP => misc_op(os, regs),
        swi::WRITE_LOADINGS_TO_FILE => write_loadings(os, regs.r[1]),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[COLOUR_TRANS.title.as_bytes()])),
    }
}

/// `N` registers from R`first`, as the signed 16.16 numbers they hold.
fn signed<const N: usize>(regs: &Regs, first: usize) -> [i32; N] {
    core::array::from_fn(|i| regs.r[first + i].cast_signed())
}

/// `values` given back in R0 onwards.
fn give_signed(regs: &mut Regs, values: &[i32]) {
    for (register, &value) in regs.r.iter_mut().zip(values) {
        *register = value.cast_unsigned();
    }
}

/// R0 to R2 converted by `model`, refused where its answer overflows.
fn converted(
    os: &mut Os<'_>,
    regs: &mut Regs,
    model: fn([i32; 3]) -> Option<[i32; 3]>,
) -> Result<(), Fail> {
    match model(signed(regs, 0)) {
        Some(answer) => {
            give_signed(regs, &answer);
            Ok(())
        }
        None => Err(os.own_error(own, OVERFLOW, &[])),
    }
}

/// The colours a match is made against.
#[derive(Clone, Copy)]
enum Of {
    /// The current mode's, as its palette showed them when last read.
    Current,
    /// Those R1 and R2 name: a mode and its palette.
    Named,
}

/// What a match answers with.
#[derive(Clone, Copy)]
enum Answer {
    Gcol,
    Number,
}

/// R0's colour matched in the colours `of` names, R0 the answer.
fn matched(
    os: &mut Os<'_>,
    regs: &mut Regs,
    of: Of,
    look: Look,
    answer: Answer,
) -> Result<(), Fail> {
    let wanted = Rgb::from_riscos_word(regs.r[0]);
    let answered = |against: &Against, loadings| match answer {
        Answer::Gcol => against.gcol(wanted, loadings, look),
        Answer::Number => against.colour_number(wanted, loadings, look),
    };
    let answer = match of {
        Of::Named if (regs.r[1], regs.r[2]) != (swi::CURRENT, swi::CURRENT) => {
            let named = palettes::named(os, regs.r[1], regs.r[2])?;
            let named = named.calibrated_by(screen_calibration(os));
            loadings(os).map(|loadings| answered(&named, loadings))
        }
        _ => current(os)?.map(|(against, loadings)| answered(against, loadings)),
    };
    if let Some(answer) = answer {
        regs.r[0] = answer;
    }
    Ok(())
}

/// The loadings, while ColourTrans holds its state.
fn loadings(os: &mut Os<'_>) -> Option<Loadings> {
    os.extension::<State>().map(|state| state.loadings)
}

/// The current mode's colours, read where they were made stale, and the
/// loadings they are matched by as they stand once read: reading them may run
/// the guest's code, which may set the loadings. None while ColourTrans holds
/// nothing.
fn current<'o>(os: &'o mut Os<'_>) -> Result<Option<(&'o Against, Loadings)>, Fail> {
    if os
        .extension::<State>()
        .is_some_and(|state| state.current.is_none())
    {
        let read = palettes::current(os)?.calibrated_by(screen_calibration(os));
        if let Some(state) = os.extension::<State>() {
            state.current = Some(read);
        }
    }
    Ok(os
        .extension::<State>()
        .and_then(|state| Some((state.current.as_ref()?, state.loadings))))
}

/// The screen's calibration; none for the default.
fn screen_calibration(os: &mut Os<'_>) -> Option<Rc<Calibration>> {
    os.extension::<State>().and_then(|state| {
        state
            .calibration
            .as_ref()
            .map(|installed| Rc::clone(&installed.table))
    })
}

/// `ColourTrans_InvalidateCache`: the colours made stale, and, while output
/// goes to a sprite, whether it has a palette read again, as RISC OS 5
/// reads it through `OS_SpriteOp` 37.
fn invalidate_cache(os: &mut Os<'_>) -> Result<(), Fail> {
    make_stale(os);
    let Some([_, _, sprite, _]) = crate::state::read(os, |vdu, _| vdu.context.0) else {
        return Ok(());
    };
    if sprite == 0 {
        return Ok(());
    }
    let read_palette = crate::sprite::Naming::Pointer.value() | crate::sprite::reason::PALETTE;
    let mut read = Regs::new([
        read_palette,
        sprite,
        sprite,
        u32::MAX,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.perform(X | tairix_riscos_kernel::swi::os::SPRITE_OP, &mut read)
}

/// The current mode's colours made stale, to be read again when next
/// wanted.
fn make_stale(os: &mut Os<'_>) {
    if let Some(state) = os.extension::<State>() {
        state.current = None;
    }
}

/// `ColourTrans_MiscOp`: R0 the reason.
fn misc_op(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        misc_op::SET_LOADINGS => {
            let set = Loadings(regs.r[1]);
            let old = os.extension::<State>().map_or(Loadings::DEFAULT, |state| {
                core::mem::replace(&mut state.loadings, set)
            });
            regs.r[2] = old.0;
            Ok(())
        }
        misc_op::DESCRIBE => {
            let Some(workspace) = os.extension::<State>().map(|state| state.workspace) else {
                return Ok(());
            };
            let [log2_bpp] = crate::vars::mode_variables(os, CURRENT_MODE, [var::LOG2_BPP])?;
            let log2_bpp = log2_bpp.unwrap_or(0);
            let description = workspace + DESCRIPTION_AT;
            os.write_word(description + DEPTH_IN_DESCRIPTION, log2_bpp)?;
            regs.r[2..5].copy_from_slice(&[log2_bpp, description, 0]);
            Ok(())
        }
        misc_op::NOUGHT => {
            regs.r[0] = 0;
            Ok(())
        }
        _ => Err(os.own_error(own, BAD_MISC_OP, &[])),
    }
}

/// `ColourTrans_WriteLoadingsToFile`: the command that sets the loadings
/// as they are, written to the file `handle`.
fn write_loadings(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let loadings = loadings(os).unwrap_or(Loadings::DEFAULT);
    let mut line = alloc::vec::Vec::new();
    let [red, green, blue] = loadings.channels();
    let digits = [
        tairix_riscos_kernel::hex(u32::from(red)),
        tairix_riscos_kernel::hex(u32::from(green)),
        tairix_riscos_kernel::hex(u32::from(blue)),
    ];
    if line.try_reserve(LOADINGS_COMMAND.len() + 13).is_err() {
        return Err(os.no_memory());
    }
    line.extend_from_slice(LOADINGS_COMMAND.as_bytes());
    for channel in &digits {
        line.extend_from_slice(b" &");
        line.extend_from_slice(channel.as_bytes());
    }
    line.push(b'\n');
    write_to(os, handle, &line)
}

/// `bytes` written to the open file `handle` at its pointer.
fn write_to(os: &mut Os<'_>, handle: u32, bytes: &[u8]) -> Result<(), Fail> {
    let length = len32(bytes.len());
    os.below_supervisor_stack(length, |os, at| {
        os.write_bytes(at, bytes)?;
        let mut write = Regs::default();
        write.r[..4].copy_from_slice(&[gbpb::WRITE, handle, at, length]);
        os.perform(X | GBPB, &mut write)
    })
}

/// `*ColourTransLoadings <red> <green> <blue>`: each a number, its low
/// byte the channel's loading.
fn loadings_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut values = [0; 3];
    parameters(os, regs.r[0], &mut values)?;
    if let Some(state) = os.extension::<State>() {
        state.loadings = Loadings::of_channels(values.map(|value| value.to_le_bytes()[0]));
    }
    Ok(())
}

/// A command's numbers from `at`, as many as `values` holds.
fn parameters(os: &mut Os<'_>, mut at: u32, values: &mut [u32]) -> Result<(), Fail> {
    for value in values {
        let (number, end) =
            os.number_at(at, read_unsigned::TERMINATED | read_unsigned::DECIMAL, 0)?;
        *value = number;
        at = end;
    }
    Ok(())
}

/// Its services: a mode change makes the current mode's colours stale, and
/// output switched to or from a sprite calls `InvalidateCache`, through
/// `ColourV` as RISC OS 5's does; a saved desktop is given the calibration
/// and the loadings.
fn serve(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    match regs.r[1] {
        service::MODE_CHANGE => make_stale(os),
        service::SWITCHING_OUTPUT_TO_SPRITE => {
            match os.perform(X | swi::INVALIDATE_CACHE, &mut Regs::default()) {
                Ok(()) | Err(Fail::Error(_)) => {}
                Err(Fail::Unwind(unwind)) => return Err(unwind),
            }
        }
        service::WIMP_SAVE_DESKTOP => {
            let saved = calibration::write_commands(os, regs.r[2], false)
                .and_then(|_| write_loadings(os, regs.r[2]));
            match saved {
                Ok(()) => {}
                Err(Fail::Error(block)) => regs.r[..2].copy_from_slice(&[block, 0]),
                Err(Fail::Unwind(unwind)) => return Err(unwind),
            }
        }
        _ => {}
    }
    Ok(())
}
