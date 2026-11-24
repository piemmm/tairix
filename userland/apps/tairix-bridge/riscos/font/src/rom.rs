//! `ROMFonts`: the fonts and encodings ResourceFS holds under
//! `Resources:$.Fonts`, where RISC OS keeps its own. Its Messages file lists
//! the standard families, which are TAIRiX's faces rather than files; its
//! encodings are those the encoding tables make.

use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::module::{Builtin, Resource};

/// The ROM fonts' Messages file.
const MESSAGES: &[u8] = include_bytes!("../resources/ROMFonts/Messages1");

/// The encodings, by their names in ResourceFS.
pub(crate) const ENCODINGS: [(&str, &[u8]); 17] = [
    (
        "Fonts.Encodings./Base0",
        include_bytes!("../resources/ROMFonts/Encodings/Base0"),
    ),
    (
        "Fonts.Encodings./Default",
        include_bytes!("../resources/ROMFonts/Encodings/Default"),
    ),
    (
        "Fonts.Encodings.Latin1",
        include_bytes!("../resources/ROMFonts/Encodings/Latin1"),
    ),
    (
        "Fonts.Encodings.Latin2",
        include_bytes!("../resources/ROMFonts/Encodings/Latin2"),
    ),
    (
        "Fonts.Encodings.Latin3",
        include_bytes!("../resources/ROMFonts/Encodings/Latin3"),
    ),
    (
        "Fonts.Encodings.Latin4",
        include_bytes!("../resources/ROMFonts/Encodings/Latin4"),
    ),
    (
        "Fonts.Encodings.Latin5",
        include_bytes!("../resources/ROMFonts/Encodings/Latin5"),
    ),
    (
        "Fonts.Encodings.Latin6",
        include_bytes!("../resources/ROMFonts/Encodings/Latin6"),
    ),
    (
        "Fonts.Encodings.Latin7",
        include_bytes!("../resources/ROMFonts/Encodings/Latin7"),
    ),
    (
        "Fonts.Encodings.Latin8",
        include_bytes!("../resources/ROMFonts/Encodings/Latin8"),
    ),
    (
        "Fonts.Encodings.Latin9",
        include_bytes!("../resources/ROMFonts/Encodings/Latin9"),
    ),
    (
        "Fonts.Encodings.Latin10",
        include_bytes!("../resources/ROMFonts/Encodings/Latin10"),
    ),
    (
        "Fonts.Encodings.Greek",
        include_bytes!("../resources/ROMFonts/Encodings/Greek"),
    ),
    (
        "Fonts.Encodings.Cyrillic",
        include_bytes!("../resources/ROMFonts/Encodings/Cyrillic"),
    ),
    (
        "Fonts.Encodings.Hebrew",
        include_bytes!("../resources/ROMFonts/Encodings/Hebrew"),
    ),
    (
        "Fonts.Encodings.Welsh",
        include_bytes!("../resources/ROMFonts/Encodings/Welsh"),
    ),
    (
        "Fonts.Encodings.UTF8",
        include_bytes!("../resources/ROMFonts/Encodings/UTF8"),
    ),
];

/// A text file of ResourceFS's.
const fn text(name: &'static str, data: &'static [u8]) -> Resource {
    Resource {
        name,
        file_type: filetype::TEXT,
        data,
    }
}

/// Its files: the Messages file, then the encodings.
static RESOURCES: [Resource; ENCODINGS.len() + 1] = {
    let mut resources = [const { text("Fonts.Messages1", MESSAGES) }; ENCODINGS.len() + 1];
    let mut i = 0;
    while i < ENCODINGS.len() {
        resources[i + 1] = text(ENCODINGS[i].0, ENCODINGS[i].1);
        i += 1;
    }
    resources
};

/// The `ROMFonts` module.
pub(crate) static ROM_FONTS: Builtin = Builtin {
    title: "ROMFonts",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &RESOURCES,
    code: None,
};
