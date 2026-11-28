//! The Font Manager, 3.80 as RISC OS 5.30 has it: its SWIs, its state and
//! its errors, which are RISC OS's numbers in TAIRiX Bridge's words.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::module::{Builtin, Resource, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::chunk;

use crate::buffer::Output;
use crate::catalogue::Catalogue;
use crate::colours::{Colours, Palette};
use crate::glyph_names::GlyphNames;
use crate::handles::Handles;
use crate::settings::Settings;
use crate::store::Store;
use crate::{
    buffer, colours, commands, desktop, find, listing, makebitmap, measure, paint, settings,
};

/// Its SWIs, in their chunk's order.
pub(crate) const NAMES: &[&str] = &[
    "CacheAddr",
    "FindFont",
    "LoseFont",
    "ReadDefn",
    "ReadInfo",
    "StringWidth",
    "Paint",
    "Caret",
    "ConverttoOS",
    "Converttopoints",
    "SetFont",
    "CurrentFont",
    "FutureFont",
    "FindCaret",
    "CharBBox",
    "ReadScaleFactor",
    "SetScaleFactor",
    "ListFonts",
    "SetFontColours",
    "SetPalette",
    "ReadThresholds",
    "SetThresholds",
    "FindCaretJ",
    "StringBBox",
    "ReadColourTable",
    "MakeBitmap",
    "UnCacheFile",
    "SetFontMax",
    "ReadFontMax",
    "ReadFontPrefix",
    "SwitchOutputToBuffer",
    "ReadFontMetrics",
    "DecodeMenu",
    "ScanString",
    "SetColourTable",
    "CurrentRGB",
    "FutureRGB",
    "ReadEncodingFilename",
    "FindField",
    "ApplyFields",
    "LookupFont",
    "EnumerateCharacters",
];

/// The calls, by their index; those other modules call by the numbers the
/// kernel gives them.
pub(crate) mod call {
    use tairix_riscos_kernel::swi::chunk::FONT;
    use tairix_riscos_kernel::swi::font;

    pub(crate) const CACHE_ADDR: u32 = font::CACHE_ADDR - FONT;
    pub(crate) const FIND_FONT: u32 = font::FIND_FONT - FONT;
    pub(crate) const LOSE_FONT: u32 = font::LOSE_FONT - FONT;
    pub(crate) const READ_DEFN: u32 = 0x03;
    pub(crate) const READ_INFO: u32 = 0x04;
    pub(crate) const STRING_WIDTH: u32 = 0x05;
    pub(crate) const PAINT: u32 = font::PAINT - FONT;
    pub(crate) const CARET: u32 = 0x07;
    pub(crate) const CONVERT_TO_OS: u32 = 0x08;
    pub(crate) const CONVERT_TO_POINTS: u32 = 0x09;
    pub(crate) const SET_FONT: u32 = 0x0A;
    pub(crate) const CURRENT_FONT: u32 = 0x0B;
    pub(crate) const FUTURE_FONT: u32 = 0x0C;
    pub(crate) const FIND_CARET: u32 = 0x0D;
    pub(crate) const CHAR_BBOX: u32 = 0x0E;
    pub(crate) const READ_SCALE_FACTOR: u32 = 0x0F;
    pub(crate) const SET_SCALE_FACTOR: u32 = 0x10;
    pub(crate) const LIST_FONTS: u32 = 0x11;
    pub(crate) const SET_FONT_COLOURS: u32 = font::SET_FONT_COLOURS - FONT;
    pub(crate) const SET_PALETTE: u32 = font::SET_PALETTE - FONT;
    pub(crate) const READ_THRESHOLDS: u32 = 0x14;
    pub(crate) const SET_THRESHOLDS: u32 = 0x15;
    pub(crate) const FIND_CARET_J: u32 = 0x16;
    pub(crate) const STRING_BBOX: u32 = 0x17;
    pub(crate) const READ_COLOUR_TABLE: u32 = 0x18;
    pub(crate) const MAKE_BITMAP: u32 = font::MAKE_BITMAP - FONT;
    pub(crate) const UNCACHE_FILE: u32 = 0x1A;
    pub(crate) const SET_FONT_MAX: u32 = 0x1B;
    pub(crate) const READ_FONT_MAX: u32 = 0x1C;
    pub(crate) const READ_FONT_PREFIX: u32 = 0x1D;
    pub(crate) const SWITCH_OUTPUT_TO_BUFFER: u32 = font::SWITCH_OUTPUT_TO_BUFFER - FONT;
    pub(crate) const READ_FONT_METRICS: u32 = 0x1F;
    pub(crate) const DECODE_MENU: u32 = 0x20;
    pub(crate) const SCAN_STRING: u32 = font::SCAN_STRING - FONT;
    pub(crate) const SET_COLOUR_TABLE: u32 = font::SET_COLOUR_TABLE - FONT;
    pub(crate) const CURRENT_RGB: u32 = 0x23;
    pub(crate) const FUTURE_RGB: u32 = 0x24;
    pub(crate) const READ_ENCODING_FILENAME: u32 = 0x25;
    pub(crate) const FIND_FIELD: u32 = 0x26;
    pub(crate) const APPLY_FIELDS: u32 = 0x27;
    pub(crate) const LOOKUP_FONT: u32 = 0x28;
    pub(crate) const ENUMERATE_CHARACTERS: u32 = 0x29;
}

/// Its messages, and their file.
const MESSAGES: &[u8] = include_bytes!("../resources/FontManager/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.Fonts.Messages\0");

/// The Font Manager.
pub(crate) static FONT_MANAGER: Builtin = Builtin {
    title: "FontManager",
    help_name: Some("Font Manager"),
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::FONT,
        prefix: "Font",
        names: NAMES,
        handler: perform,
    }),
    services: None,
    commands: &commands::COMMANDS,
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.Fonts.Messages",
        file_type: filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// What the Font Manager keeps between calls.
pub(crate) struct State {
    own: Own,
    pub(crate) handles: Handles,
    pub(crate) settings: Settings,
    /// The thresholds and the pseudo palette.
    pub(crate) palette: Palette,
    /// The colours and font the next paint starts with.
    pub(crate) current: Colours,
    /// What they would be after the string last measured.
    pub(crate) future: Colours,
    /// The fonts and encodings `Font$Path` reached when last read.
    pub(crate) catalogue: Option<Catalogue>,
    /// The fonts' data.
    pub(crate) store: Store,
    /// The glyph lists, read when a UTF-8 font first needs them.
    pub(crate) names: Option<Rc<GlyphNames>>,
    /// `Font_DecodeMenu`'s last answer, which a selection of nothing gives
    /// again.
    pub(crate) decoded: Vec<u8>,
    /// Where paints go while output is switched to a buffer.
    pub(crate) output: Option<Output>,
}

impl tairix_riscos_kernel::Reclaims for State {
    fn reclaim(&mut self) {
        self.store.reclaim();
    }
}

pub(crate) fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// `work` done with the Font Manager's state, which is there while its
/// SWIs can be called.
pub(crate) fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error_with(SWI_OUT_OF_RANGE, &[FONT_MANAGER.title.as_bytes()])),
    }
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(MESSAGES_FILE.room())?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    let settings = Settings::configured(os)?;
    let (pressure, log) = os.cache_context();
    let store = Store::new(os.ceiling_bytes(), os.cache_key(), pressure, log);
    os.hold_cache_extension(State {
        own,
        handles: Handles::new(),
        settings,
        palette: Palette::INITIAL,
        current: Colours::INITIAL,
        future: Colours::INITIAL,
        catalogue: None,
        store,
        names: None,
        decoded: Vec::new(),
        output: None,
    })?;
    find::set_path_defaults(os)?;
    let desktop = os.desktop_font();
    desktop::set_variables(os, desktop)?;
    settings::make_cache_area(os)
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.release_extension::<State>();
    settings::remove_cache_area(os)
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        call::CACHE_ADDR => settings::cache_addr(os, regs),
        call::FIND_FONT => find::find_font(os, regs),
        call::LOSE_FONT => find::lose_font(os, regs),
        call::READ_DEFN => find::read_defn(os, regs),
        call::READ_INFO => measure::read_info(os, regs),
        call::STRING_WIDTH => measure::string_width(os, regs),
        call::PAINT => paint::paint(os, regs),
        call::CARET => paint::caret(os, regs),
        call::CONVERT_TO_OS => settings::convert_to_os(os, regs),
        call::CONVERT_TO_POINTS => settings::convert_to_points(os, regs),
        call::SET_FONT => find::set_font(os, regs),
        call::CURRENT_FONT => colours::current_font(os, regs, false),
        call::FUTURE_FONT => colours::current_font(os, regs, true),
        call::FIND_CARET => measure::find_caret(os, regs, false),
        call::CHAR_BBOX => measure::char_bbox(os, regs),
        call::READ_SCALE_FACTOR => settings::read_scale_factor(os, regs),
        call::SET_SCALE_FACTOR => settings::set_scale_factor(os, regs),
        call::LIST_FONTS => listing::list_fonts(os, regs),
        call::SET_FONT_COLOURS => colours::set_font_colours(os, regs),
        call::SET_PALETTE => colours::set_palette(os, regs),
        call::READ_THRESHOLDS => colours::read_thresholds(os, regs),
        call::SET_THRESHOLDS => colours::set_thresholds(os, regs),
        call::FIND_CARET_J => measure::find_caret(os, regs, true),
        call::STRING_BBOX => measure::string_bbox(os, regs),
        call::READ_COLOUR_TABLE => colours::read_colour_table(os, regs),
        call::MAKE_BITMAP => makebitmap::make_bitmap(os, regs),
        call::UNCACHE_FILE => settings::uncache_file(os, regs),
        call::SET_FONT_MAX => settings::set_font_max(os, regs),
        call::READ_FONT_MAX => settings::read_font_max(os, regs),
        call::READ_FONT_PREFIX => find::read_font_prefix(os, regs),
        call::SWITCH_OUTPUT_TO_BUFFER => buffer::switch_output_to_buffer(os, regs),
        call::READ_FONT_METRICS => measure::read_font_metrics(os, regs),
        call::DECODE_MENU => listing::decode_menu(os, regs),
        call::SCAN_STRING => measure::scan_string(os, regs),
        call::SET_COLOUR_TABLE => colours::set_colour_table(os, regs),
        call::CURRENT_RGB => colours::rgb(os, regs, false),
        call::FUTURE_RGB => colours::rgb(os, regs, true),
        call::READ_ENCODING_FILENAME => find::read_encoding_filename(os, regs),
        call::FIND_FIELD => find::find_field(os, regs),
        call::APPLY_FIELDS => find::apply_fields(os, regs),
        call::LOOKUP_FONT => find::lookup_font(os, regs),
        call::ENUMERATE_CHARACTERS => measure::enumerate_characters(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[FONT_MANAGER.title.as_bytes()])),
    }
}

/// The errors the Font Manager raises.
pub(crate) mod error {
    use tairix_riscos_kernel::error::Error;

    const fn error(number: u32, token: &'static str) -> Error {
        Error { number, token }
    }

    pub(crate) const UNNAMED_SIZE: Error = error(0xCC, "UnnamedSize");
    pub(crate) const UNDEFINED_COLOUR: Error = error(0x205, "UndefinedColour");
    pub(crate) const BAD_COLOURS: Error = error(0x206, "BadColours");
    pub(crate) const INVALID_COLOUR: Error = error(0x207, "InvalidColour");
    pub(crate) const UNDEFINED: Error = error(0x208, "Undefined");
    pub(crate) const OUT_OF_RANGE: Error = error(0x20A, "OutOfRange");
    pub(crate) const NOT_FOUND: Error = error(0x20B, "NotFound");
    pub(crate) const NO_DATA: Error = error(0x20B, "NoData");
    pub(crate) const NO_HANDLES: Error = error(0x20D, "NoHandles");
    pub(crate) const CTRL_CHAR: Error = error(0x20F, "CtrlChar");
    pub(crate) const RESERVED: Error = error(0x213, "Reserved");
    pub(crate) const BAD_MATRIX: Error = error(0x219, "BadMatrix");
    pub(crate) const ZERO_SIZE: Error = error(0x21A, "ZeroSize");
    pub(crate) const TRANSFORMED: Error = error(0x21C, "Transformed");
    pub(crate) const NO_RGB: Error = error(0x21D, "NoRGB");
    pub(crate) const NO_ENCODING: Error = error(0x21E, "NoEncoding");
    pub(crate) const ILLEGAL: Error = error(0x226, "Illegal");
    pub(crate) const NO_SUPREMACY: Error = error(0x227, "NoSupremacy");
    pub(crate) const NOT_PATH: Error = error(0x228, "NotPath");
    pub(crate) const FONTS_CLAIMED: Error = error(0x229, "FontsClaimed");
    pub(crate) const NOT_CACHE: Error = error(0x22A, "NotCache");
}

/// The error `error` raises, its text from the Font Manager's messages with
/// `arguments` in it.
pub(crate) fn raise(os: &mut Os<'_>, error: Error, arguments: &[&[u8]]) -> Fail {
    os.own_error(own, error, arguments)
}

/// A token of the Font Manager's messages.
pub(crate) fn message(os: &mut Os<'_>, token: &str) -> Result<Vec<u8>, Fail> {
    os.own_text(own, token, &[])
}
