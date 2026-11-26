//! MimeMap 0.19: `MimeMap_Translate` and `*MimeMap` between RISC OS file
//! types, their names, MIME types and extensions, answered from TAIRiX's own
//! type registry (`lib/browse`) where RISC OS 5's reads a mappings file. A
//! type's name is the system's, as FileSwitch names it. `*ReadMimeMap` has
//! nothing to read again.

use alloc::vec::Vec;

use tairix_abi::fs::Filetype;
use tairix_browse::media::{self, MediaType};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{Builtin, Command, Resource, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::chunk::MIME_MAP as CHUNK;

use crate::fileswitch::types;

/// MimeMap's messages, in TAIRiX Bridge's own words.
const MESSAGES: &[u8] = include_bytes!("../resources/MimeMap/Messages");

/// The messages as MessageTrans opens them.
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.MimeMap.Messages\0");

/// A form the SWI takes or answers in that it does not know.
const UNKNOWN_FORM: Error = Error {
    number: 0xB0_0001,
    token: "UnknownForm",
};

/// A type, name, MIME type or extension the registry maps nothing to.
const NO_MAPPING: Error = Error {
    number: 0xB0_0002,
    token: "NoMapping",
};

/// The longest MIME type or extension the SWI reads.
const MOST: usize = 255;

/// MimeMap.
pub static MIME_MAP: Builtin = Builtin {
    title: "MimeMap",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "MimeMap",
        names: &["Translate"],
        handler: swi,
    }),
    services: None,
    commands: &[
        Command::new(
            "MimeMap",
            (0, 1),
            "*MimeMap shows the MIME type, RISC OS file type and extensions TAIRiX maps a file type, extension, MIME type or file type name to, or with nothing given every mapping it knows.",
            Some("Syntax: *MimeMap [&<file type> | .<extension> | <MIME type> | <file type name>]"),
            show,
        ),
        Command::new(
            "ReadMimeMap",
            (0, 0),
            "*ReadMimeMap reads the mappings again: TAIRiX's are built in, so there is nothing new to read.",
            Some("Syntax: *ReadMimeMap"),
            reread,
        ),
    ],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.MimeMap.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// What MimeMap holds while it is active.
struct State {
    own: Own,
    workspace: u32,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn fail(os: &mut Os<'_>, error: Error) -> Fail {
    os.own_error(own, error, &[])
}

fn init(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(MESSAGES_FILE.room())?;
    let held = MESSAGES_FILE
        .place(os, workspace)
        .and_then(|own| os.hold_extension(State { own, workspace }));
    if let Err(failure) = held {
        os.rma_free(workspace)?;
        return Err(failure);
    }
    Ok(())
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    let workspace = os.extension::<State>().map(|state| state.workspace);
    os.release_extension::<State>();
    match workspace {
        Some(workspace) => os.rma_free(workspace),
        None => Ok(()),
    }
}

/// How the SWI names a type, in R0 and R2.
#[derive(Clone, Copy)]
enum Form {
    /// A RISC OS file type's number.
    Type,
    /// A file type's name, as the system names it.
    Name,
    /// A MIME type.
    Mime,
    /// An extension, its dot optional.
    Extension,
}

impl Form {
    const fn of(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Type),
            1 => Some(Self::Name),
            2 => Some(Self::Mime),
            3 => Some(Self::Extension),
            _ => None,
        }
    }
}

/// A mapping the registry holds: a media type and the RISC OS file type its
/// file has, where it has one.
#[derive(Clone, Copy)]
struct Mapping {
    media: MediaType,
    filetype: Option<Filetype>,
}

impl Mapping {
    /// The mapping of a file of type `filetype`.
    fn of_type(filetype: Filetype) -> Option<Self> {
        media::media_for_filetype(filetype).map(|media| Self {
            media,
            filetype: Some(filetype),
        })
    }

    /// The mapping of `media`, its file type the one a new file of it is
    /// given.
    fn of_media(media: MediaType) -> Self {
        Self {
            media,
            filetype: media::canonical_filetype(media),
        }
    }

    /// Its first extension, without its dot.
    fn extension(self) -> Option<&'static str> {
        self.extensions().next()
    }

    fn extensions(self) -> impl Iterator<Item = &'static str> {
        media::name_endings(self.media)
            .filter(|&(mark, _)| mark == '.')
            .map(|(_, extension)| extension)
    }
}

/// The mapping a type named by `name`, as the system names types, has.
fn of_name(os: &mut Os<'_>, name: &[u8]) -> Result<Option<Mapping>, Fail> {
    let filetype = types::type_named(os, name)?.and_then(|kind| u16::try_from(kind).ok());
    Ok(filetype.and_then(Filetype::new).and_then(Mapping::of_type))
}

/// The mapping a MIME type or an extension spelt in `text` names.
fn of_text(text: &[u8], extension: bool) -> Option<Mapping> {
    let text = core::str::from_utf8(text).ok()?;
    if extension {
        let bare = text.strip_prefix('.').unwrap_or(text);
        media::media_for_extension(bare).map(Mapping::of_media)
    } else {
        MediaType::from_media_str(text).map(Mapping::of_media)
    }
}

/// `MimeMap_Translate`: what R1 names in R0's form, answered in R2's: a
/// type's number in R3, else a string written at R3 and ended with a zero.
fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    if offset != 0 {
        return Err(os.error_with(SWI_OUT_OF_RANGE, &[MIME_MAP.title.as_bytes()]));
    }
    let Some(form) = Form::of(regs.r[0]) else {
        return Err(fail(os, UNKNOWN_FORM));
    };
    let value = regs.r[1];
    let mapping = match form {
        Form::Type => u16::try_from(value)
            .ok()
            .and_then(Filetype::new)
            .and_then(Mapping::of_type),
        Form::Name => {
            let name = crate::fileswitch::name::read(os, value)?;
            of_name(os, &name)?
        }
        // A MIME type ends as a name does; an extension only at a zero.
        Form::Mime => of_text(&os.read_until(value, MOST, |b| b <= b' ')?.0, false),
        Form::Extension => match os.read_until(value, MOST, |b| b == 0)? {
            (text, true) => of_text(&text, true),
            (_, false) => None,
        },
    };
    let Some(mapping) = mapping else {
        return Err(fail(os, NO_MAPPING));
    };
    let answer: Vec<u8> = match Form::of(regs.r[2]) {
        // An answer in a form it does not know is no answer at all.
        None => return Ok(()),
        Some(Form::Type) => {
            let Some(filetype) = mapping.filetype else {
                return Err(fail(os, NO_MAPPING));
            };
            regs.r[3] = u32::from(filetype.get());
            return Ok(());
        }
        Some(Form::Name) => match mapping.filetype {
            Some(filetype) => name_of(os, filetype)?,
            None => return Err(fail(os, NO_MAPPING)),
        },
        Some(Form::Mime) => mapping.media.as_str().as_bytes().to_vec(),
        Some(Form::Extension) => match mapping.extension() {
            Some(extension) => extension.as_bytes().to_vec(),
            None => return Err(fail(os, NO_MAPPING)),
        },
    };
    os.write_bytes(regs.r[3], &answer)?;
    os.write_byte(regs.r[3].wrapping_add(len32(answer.len())), 0)
}

/// `filetype`'s name, as the system names it, its padding taken off.
fn name_of(os: &mut Os<'_>, filetype: Filetype) -> Result<Vec<u8>, Fail> {
    let name = types::name_of(os, u32::from(filetype.get()))?;
    let len = name
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |last| last + 1);
    Ok(name[..len].to_vec())
}

/// `*MimeMap`: the mapping a file type, extension, MIME type or file type
/// name has, or every mapping the registry holds.
fn show(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (first, at) = os.skip_spaces(regs.r[0])?;
    if first < b' ' {
        for &media in media::ALL {
            let mapping = Mapping::of_media(media);
            if mapping.filetype.is_some() || mapping.extension().is_some() {
                line(os, mapping)?;
            }
        }
        return Ok(());
    }
    let (word, _) = os.read_until(at, MOST, |b| b <= b' ')?;
    let mapping = match word.as_slice() {
        [b'&', digits @ ..] => Filetype::from_hex(digits).and_then(Mapping::of_type),
        [b'.', ..] => of_text(&word, true),
        _ if word.contains(&b'/') => of_text(&word, false),
        _ => of_name(os, &word)?,
    };
    match mapping {
        Some(mapping) => line(os, mapping),
        None => Err(fail(os, NO_MAPPING)),
    }
}

/// One mapping's line: its MIME type, then its file type and that type's
/// name, a dash for each where it has none, then its extensions.
fn line(os: &mut Os<'_>, mapping: Mapping) -> Result<(), Fail> {
    let mut text = Vec::new();
    text.extend_from_slice(mapping.media.as_str().as_bytes());
    match mapping.filetype {
        Some(filetype) => {
            text.extend_from_slice(b" &");
            text.extend_from_slice(&tairix_riscos_kernel::filing::filetype::digits(u32::from(
                filetype.get(),
            )));
            text.push(b' ');
            text.extend_from_slice(&name_of(os, filetype)?);
        }
        None => text.extend_from_slice(b" - -"),
    }
    for extension in mapping.extensions() {
        text.extend_from_slice(b" .");
        text.extend_from_slice(extension.as_bytes());
    }
    os.write_line(&text)
}

/// `*ReadMimeMap`: the registry is built in, so there is nothing to read.
#[expect(clippy::unnecessary_wraps, reason = "a command's signature")]
fn reread(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}
