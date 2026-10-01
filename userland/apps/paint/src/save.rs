//! Writing a document out: the format a file's name asks for, whether the
//! document can be written as it, and the encoding itself.
//!
//! A PNG or a JPEG holds one picture, so a document of several is written as
//! a sprite area or not at all: writing one of its pictures under the
//! document's name would lose the rest without a word. A picture that is to
//! become a sprite and has no sprite details of its own is given them from
//! what it is.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::SaveEndings;
use tairix_browse::media::{media_for_name, name_endings, Ending, MediaType};
use tairix_image::{
    desktop_palette, encode_jpeg, encode_png, encode_sprite_area, opaque_sprite_writes_back,
    EncodeError, IndexDepth, JpegOptions, PictureKind, PictureSource, Rgba8, SpriteInput,
    SpriteLayout, SpriteName, SpritePalette,
};
use tairix_sandbox::imagerender::ViewFormat;

use crate::canvas::{Canvas, Kind};
use crate::document::{
    free_name, sprite_area, Document, Entry, Origin, Picture, Snapshot, SpriteInfo,
};

/// What transparency is written over in a format with none: white paper.
const JPEG_BACKGROUND: [u8; 3] = [255, 255, 255];

/// A format Paint writes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaveFormat {
    /// Portable Network Graphics.
    Png,
    /// JPEG.
    Jpeg,
    /// A RISC OS sprite area.
    Sprites,
}

impl SaveFormat {
    /// Every format Paint writes.
    pub const ALL: [Self; 3] = [Self::Png, Self::Jpeg, Self::Sprites];

    /// What the format is in the desktop's one media registry.
    #[must_use]
    pub const fn media(self) -> MediaType {
        match self {
            Self::Png => MediaType::ImagePng,
            Self::Jpeg => MediaType::ImageJpeg,
            Self::Sprites => MediaType::ImageSprite,
        }
    }

    /// The extension a new file of this format is given: the registry's
    /// first for it.
    #[must_use]
    pub fn extension(self) -> &'static str {
        name_endings(self.media())
            .find_map(|(separator, code)| (separator == '.').then_some(code))
            .unwrap_or_default()
    }

    /// The format a file read as `format` is written back in, if Paint
    /// writes it.
    #[must_use]
    pub const fn of(format: ViewFormat) -> Option<Self> {
        match format {
            ViewFormat::Png => Some(Self::Png),
            ViewFormat::Jpeg => Some(Self::Jpeg),
            ViewFormat::Sprite => Some(Self::Sprites),
            _ => None,
        }
    }
}

/// Why a document cannot be written under a name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveRefusal {
    /// The name asks for a format Paint does not write; the suffix that asks.
    Unwritable(String),
    /// The format holds one picture and the document has this many entries.
    SeveralPictures(usize),
    /// The format holds a picture and the document's one entry is a sprite
    /// kept as its bytes.
    KeptSprite,
    /// A sprite kept as its bytes is not a whole number of words, so no
    /// sprite after it could be written where RISC OS reads it.
    KeptSpriteOffWord(SpriteName),
    /// A sprite file named so that nothing could open it again: the name
    /// carries neither `.spr` nor the `,ff9` file type.
    SpritesUnnamed,
}

impl core::fmt::Display for SaveRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unwritable(suffix) => write!(
                f,
                "Paint cannot write {suffix} files: give the name .png, .jpg or .spr"
            ),
            Self::SeveralPictures(count) => write!(
                f,
                "The {count} sprites can only be saved together as a sprite file: give the name .spr"
            ),
            Self::KeptSprite => f.write_str(
                "This sprite cannot be edited, so it can only be saved in a sprite file",
            ),
            Self::KeptSpriteOffWord(name) => write!(
                f,
                "The sprite {name} is damaged and cannot be written back: delete it to save"
            ),
            Self::SpritesUnnamed => {
                f.write_str("A sprite file can only be opened again if it is named .spr")
            }
        }
    }
}

/// The format `name` asks for — its RISC OS file type or its extension, as
/// the desktop's one media registry reads either — or `None` for a name
/// that asks for neither.
///
/// # Errors
///
/// [`SaveRefusal::Unwritable`] for a name that asks for a format Paint does
/// not write.
pub fn format_named(name: &str) -> Result<Option<SaveFormat>, SaveRefusal> {
    let Some(media) = media_for_name(name) else {
        return Ok(None);
    };
    let format = SaveFormat::ALL
        .into_iter()
        .find(|format| format.media() == media);
    if format.is_none() {
        let suffix = name.rfind(['.', ',']).map_or(name, |at| &name[at..]);
        return Err(SaveRefusal::Unwritable(String::from(suffix)));
    }
    Ok(format)
}

/// The format a document of `entries`, read from `origin`, is written in
/// under `name`: what the name asks for, else what the document is.
///
/// # Errors
///
/// [`SaveRefusal`] where the document cannot be written so.
pub fn format_for(
    name: &str,
    entries: &[Entry],
    origin: Origin,
) -> Result<SaveFormat, SaveRefusal> {
    writable_as(format_named(name)?, entries, origin)
}

/// The endings a document of `entries`, read from `origin`, may be saved
/// under — every name ending of each format that can hold it, the format it
/// is first, so a name given none takes that.
///
/// # Errors
///
/// The refusal of a save in that format, where no format can hold it.
pub fn save_endings(entries: &[Entry], origin: Origin) -> Result<SaveEndings, SaveRefusal> {
    let own = natural(entries, origin);
    let formats =
        core::iter::once(own).chain(SaveFormat::ALL.into_iter().filter(|&format| format != own));
    let mut endings = SaveEndings::ANY;
    let mut refused = None;
    for format in formats {
        match writable_as(Some(format), entries, origin) {
            Ok(_) => {
                for (separator, code) in name_endings(format.media()) {
                    // An ending past the table's bound is left out, so the
                    // picker refuses it: narrower than the save, never wider.
                    let _ = endings.push(separator, code);
                }
            }
            Err(refusal) => {
                refused.get_or_insert(refusal);
            }
        }
    }
    match refused {
        Some(refusal) if endings.is_any() => Err(refusal),
        _ => Ok(endings),
    }
}

/// The format a document of `entries`, read from `origin`, is written in as
/// `named` asks, else as what it is.
fn writable_as(
    named: Option<SaveFormat>,
    entries: &[Entry],
    origin: Origin,
) -> Result<SaveFormat, SaveRefusal> {
    let format = named.unwrap_or_else(|| natural(entries, origin));
    if format == SaveFormat::Sprites {
        // A sprite area carries no signature, so its name is how it is known.
        if named.is_none() {
            return Err(SaveRefusal::SpritesUnnamed);
        }
        let damaged = entries.iter().find_map(|entry| match entry {
            Entry::Kept(kept) if !opaque_sprite_writes_back(&kept.bytes) => Some(kept.name),
            _ => None,
        });
        if let Some(name) = damaged {
            return Err(SaveRefusal::KeptSpriteOffWord(name));
        }
    } else {
        match entries {
            [Entry::Picture(_)] => {}
            [Entry::Kept(_)] => return Err(SaveRefusal::KeptSprite),
            many => return Err(SaveRefusal::SeveralPictures(many.len())),
        }
    }
    Ok(format)
}

/// Why a file Paint read is not written back over itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotWrittenBack {
    /// It is of a format Paint reads but does not write.
    Format,
    /// It was read at fewer bits a channel than it holds.
    Reduced,
    /// It held more than its picture: a colour profile, text, metadata, an
    /// animation's further frames.
    Extras,
    /// Its name asks for another format than the one it holds.
    Misnamed,
    /// A save under its own name would be refused.
    Refused(SaveRefusal),
}

impl core::fmt::Display for NotWrittenBack {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Format => f.write_str("Paint does not write this kind of file back"),
            Self::Reduced => f.write_str(
                "Paint holds this file at fewer bits than it has, so does not write it back",
            ),
            Self::Extras => f.write_str(
                "This file holds more than its picture, such as a colour profile or text, so \
                 Paint does not write it back",
            ),
            Self::Misnamed => f.write_str(
                "This file's name asks for another format than it holds, so it is not written back",
            ),
            Self::Refused(refusal) => write!(f, "{refusal}"),
        }
    }
}

/// Whether the file `name`, read as `document`, may be written back over
/// itself: only where a save under its own name writes exactly the format it
/// was read as, and nothing it held was lost on the way in.
///
/// # Errors
///
/// [`NotWrittenBack`]: why not.
pub fn write_back(name: &str, document: &Document) -> Result<(), NotWrittenBack> {
    let Origin::Read(read_as) = document.origin() else {
        return Err(NotWrittenBack::Format);
    };
    let own = SaveFormat::of(read_as).ok_or(NotWrittenBack::Format)?;
    let unkept = document.unkept();
    if unkept.precision {
        return Err(NotWrittenBack::Reduced);
    }
    if unkept.extras {
        return Err(NotWrittenBack::Extras);
    }
    match format_for(name, document.entries(), document.origin()) {
        Ok(written) if written == own => Ok(()),
        Ok(_) => Err(NotWrittenBack::Misnamed),
        Err(refusal) => Err(NotWrittenBack::Refused(refusal)),
    }
}

/// What a document of `entries`, read from `origin`, is written as when its
/// name does not say: what it is.
#[must_use]
pub fn natural(entries: &[Entry], origin: Origin) -> SaveFormat {
    if sprite_area(entries, origin) {
        return SaveFormat::Sprites;
    }
    match origin {
        Origin::Read(format) => SaveFormat::of(format).unwrap_or(SaveFormat::Png),
        Origin::New => SaveFormat::Png,
    }
}

/// `snapshot` written as `format`; a sprite with no details of its own is
/// named after `name`.
///
/// # Errors
///
/// [`EncodeError`] as the encoder states it.
pub fn encode(snapshot: &Snapshot, format: SaveFormat, name: &str) -> Result<Vec<u8>, EncodeError> {
    match format {
        SaveFormat::Png | SaveFormat::Jpeg => {
            let Some(Entry::Picture(picture)) = snapshot.entries.get(snapshot.current) else {
                return Err(EncodeError::SpriteAreaEmpty);
            };
            if format == SaveFormat::Png {
                encode_png(&picture.canvas)
            } else {
                let options = JpegOptions::new(snapshot.jpeg_quality, JPEG_BACKGROUND)?;
                encode_jpeg(&picture.canvas, options)
            }
        }
        SaveFormat::Sprites => encode_sprites(&snapshot.entries, name),
    }
}

/// What `snapshot` written as `format` cannot keep, said with the save: a
/// JPEG holds no transparency, so a picture with any is laid over white, and
/// neither a PNG nor a JPEG holds a sprite's pixels other than square.
#[must_use]
pub fn lost_in(snapshot: &Snapshot, format: SaveFormat) -> Option<&'static str> {
    let Some(Entry::Picture(picture)) = snapshot.entries.get(snapshot.current) else {
        return None;
    };
    let shaped = format != SaveFormat::Sprites && picture.pixel_aspect() != (1, 1);
    let clear = format == SaveFormat::Jpeg && picture.canvas.has_transparency();
    match (clear, shaped) {
        (true, true) => Some(
            "A JPEG holds no transparency and square pixels alone, so the picture was laid \
             over white and its pixels' shape not kept",
        ),
        (true, false) => Some("A JPEG holds no transparency, so the picture was laid over white"),
        (false, true) => {
            Some("This format holds square pixels alone, so the pixels' shape was not kept")
        }
        (false, false) => None,
    }
}

/// What one entry is written as: the sprite details it is written under and
/// the pixels, seen through [`OpaquePalette`] where its palette holds what a
/// sprite palette cannot.
enum Written<'a> {
    Picture {
        sprite: SpriteInfo,
        source: Source<'a>,
    },
    Kept(&'a [u8]),
}

enum Source<'a> {
    Canvas(&'a Canvas),
    Opaque(OpaquePalette<'a>),
}

impl Source<'_> {
    fn picture(&self) -> &dyn PictureSource {
        match self {
            Self::Canvas(canvas) => *canvas,
            Self::Opaque(opaque) => opaque,
        }
    }
}

fn encode_sprites(entries: &[Entry], name: &str) -> Result<Vec<u8>, EncodeError> {
    let mut taken: Vec<SpriteName> = tairix_util::fallible::collected(
        entries.len(),
        entries.iter().filter_map(Entry::name).copied(),
    )
    .ok_or(EncodeError::OutOfMemory)?;
    let mut written: Vec<Written<'_>> = Vec::new();
    written
        .try_reserve_exact(entries.len())
        .map_err(|_| EncodeError::OutOfMemory)?;
    for entry in entries {
        written.push(match entry {
            Entry::Kept(kept) => Written::Kept(&kept.bytes),
            Entry::Picture(picture) => {
                let sprite = sprite_details(picture, name, &mut taken)?;
                let source = if needs_opaque_palette(&picture.canvas) {
                    Source::Opaque(OpaquePalette::new(&picture.canvas)?)
                } else {
                    Source::Canvas(&picture.canvas)
                };
                Written::Picture { sprite, source }
            }
        });
    }
    let inputs = tairix_util::fallible::collected(
        written.len(),
        written.iter().map(|written| match written {
            Written::Kept(bytes) => SpriteInput::Opaque(bytes),
            Written::Picture { sprite, source } => SpriteInput::Picture {
                name: sprite.name,
                mode: sprite.mode,
                palette: &sprite.palette,
                masked: sprite.masked,
                source: source.picture(),
            },
        }),
    )
    .ok_or(EncodeError::OutOfMemory)?;
    encode_sprite_area(&inputs)
}

/// Whether a palette picture's palette holds a translucent colour, which a
/// sprite palette cannot: its opacity is written in the mask instead.
fn needs_opaque_palette(canvas: &Canvas) -> bool {
    canvas
        .kind()
        .palette()
        .is_some_and(|palette| palette.iter().any(|entry| entry[3] != u8::MAX))
}

/// The details `picture` is written as a sprite under: its own, made to fit
/// what its pixels have become, or ones made from what it is.
fn sprite_details(
    picture: &Picture,
    name: &str,
    taken: &mut Vec<SpriteName>,
) -> Result<SpriteInfo, EncodeError> {
    let canvas = &picture.canvas;
    let partial = canvas.has_partial_alpha();
    let transparent = canvas.has_transparency();
    let mut sprite = if let Some(sprite) = &picture.sprite {
        sprite.clone()
    } else {
        let name = unique_name(name, taken).ok_or(EncodeError::TooLarge)?;
        taken.try_reserve(1).map_err(|_| EncodeError::OutOfMemory)?;
        taken.push(name);
        SpriteInfo::for_canvas(name, canvas)
    };
    sprite.palette = palette_form(canvas, &sprite);
    match canvas.kind() {
        Kind::Indexed { masked, .. } => sprite.masked = *masked || needs_opaque_palette(canvas),
        Kind::Rgba => sprite.masked |= transparent,
    }
    let alpha_channel = matches!(
        sprite.mode.layout(),
        SpriteLayout::Direct {
            alpha_channel: true,
            ..
        }
    );
    if partial && sprite.masked && !alpha_channel && !sprite.mode.alpha_mask() {
        sprite.mode = sprite.mode.with_alpha_mask(true);
    }
    Ok(sprite)
}

/// How the palette of a sprite of `canvas` is stated. A stored palette is
/// what the file held, since every change to a sprite's colours restates it
/// ([`restated`]); a sprite with no stated palette shows the desktop's, so
/// one whose colours are no longer those states them in full.
fn palette_form(canvas: &Canvas, sprite: &SpriteInfo) -> SpritePalette {
    match (&sprite.palette, canvas.kind()) {
        (_, Kind::Rgba) => SpritePalette::Implied,
        (SpritePalette::Implied, kind) => restated(kind),
        (held, _) => held.clone(),
    }
}

/// How a sprite of `kind` states colours that have just changed: as no
/// palette at all where they are the desktop's, else in full.
#[must_use]
pub fn restated(kind: &Kind) -> SpritePalette {
    match kind {
        Kind::Indexed { depth, palette, .. } if !is_desktop(*depth, palette) => SpritePalette::Full,
        _ => SpritePalette::Implied,
    }
}

fn is_desktop(depth: IndexDepth, palette: &[Rgba8]) -> bool {
    let desktop = desktop_palette(depth);
    palette.len() == desktop.len()
        && palette
            .iter()
            .zip(desktop)
            .all(|(entry, rgb)| entry[..3] == rgb[..] && entry[3] == u8::MAX)
}

/// A sprite name made from a file `name`, unlike every name in `taken`: its
/// leaf without its ending, lower-cased, with `_` for what a name cannot hold.
fn unique_name(name: &str, taken: &[SpriteName]) -> Option<SpriteName> {
    let leaf = name.rsplit('/').next().unwrap_or(name);
    let mut base = [0u8; SpriteName::MAX_LEN];
    let mut len = 0;
    for (slot, ch) in base.iter_mut().zip(Ending::of(leaf).stem.chars()) {
        *slot = u8::try_from(ch)
            .ok()
            .filter(u8::is_ascii_graphic)
            .map_or(b'_', |byte| byte.to_ascii_lowercase());
        len += 1;
    }
    free_name(&SpriteName::from_bytes(&base[..len])?, taken)
}

/// A palette picture seen with its palette opaque and each entry's opacity
/// in its mask, which is how a sprite states what a translucent palette
/// entry of a PNG does.
struct OpaquePalette<'a> {
    canvas: &'a Canvas,
    palette: Vec<Rgba8>,
    alpha: Vec<u8>,
}

impl<'a> OpaquePalette<'a> {
    fn new(canvas: &'a Canvas) -> Result<Self, EncodeError> {
        let held = canvas.kind().palette().unwrap_or(&[]);
        let palette = tairix_util::fallible::collected(
            held.len(),
            held.iter()
                .map(|entry| [entry[0], entry[1], entry[2], u8::MAX]),
        )
        .ok_or(EncodeError::OutOfMemory)?;
        let alpha = tairix_util::fallible::collected(held.len(), held.iter().map(|entry| entry[3]))
            .ok_or(EncodeError::OutOfMemory)?;
        Ok(Self {
            canvas,
            palette,
            alpha,
        })
    }
}

impl PictureSource for OpaquePalette<'_> {
    fn width(&self) -> u32 {
        self.canvas.width()
    }

    fn height(&self) -> u32 {
        self.canvas.height()
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureKind::Indexed {
            depth: self.canvas.kind().depth().unwrap_or(IndexDepth::Eight),
            palette: &self.palette,
            masked: true,
        }
    }

    fn read_row(&self, y: u32, samples: &mut [u8], mask: &mut [u8]) {
        if self.canvas.kind().masked() {
            self.canvas.read_row(y, samples, mask);
        } else {
            self.canvas.read_row(y, samples, &mut []);
            mask.fill(u8::MAX);
        }
        for (opacity, &index) in mask.iter_mut().zip(samples.iter()) {
            let entry = self.alpha.get(usize::from(index)).copied().unwrap_or(0);
            *opacity = u8::try_from((u32::from(*opacity) * u32::from(entry) + 127) / 255)
                .unwrap_or(u8::MAX);
        }
    }
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;
