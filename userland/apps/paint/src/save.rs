//! Writing a document out: the format a file's name asks for, whether the
//! document can be written as it, what that format cannot keep of it, and the
//! encoding itself.
//!
//! A PNG, a JPEG, a GIF, a BMP or an OpenRaster file holds one picture, so a
//! document of several is written as a TIFF's pages or a sprite area or not
//! at all: writing one of its pictures under the document's name would lose
//! the rest without a word. OpenRaster alone keeps a picture's layers; every
//! other format is written the layers laid together. A picture that is to
//! become a sprite and has no sprite details of its own is given them from
//! what it is.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::SaveEndings;
use tairix_browse::media::{media_for_name, name_endings, Ending, MediaType};
use tairix_image::{
    desktop_palette, encode_bmp, encode_gif, encode_jpeg, encode_ora, encode_png,
    encode_sprite_area, encode_tiff, opaque_sprite_writes_back, Density, DensityUnit, EncodeError,
    GifOptions, IndexDepth, JpegOptions, OraLayerSource, PictureKind, PictureSource, Rgba8,
    SpriteInput, SpriteLayout, SpriteName, SpritePalette, TiffOptions, Written,
};
use tairix_sandbox::imagerender::ViewFormat;

use crate::canvas::{Canvas, Kind, OutOfMemory};
use crate::document::{
    free_name, sprite_area, Document, Entry, Origin, Picture, Snapshot, SpriteInfo,
};
use crate::quantize::palette_for;
use crate::transform::{apply, indexed, Transform, TransformError};

/// What transparency is written over in a format with none: white paper.
const JPEG_BACKGROUND: [u8; 3] = [255, 255, 255];

/// The longest side of an OpenRaster file's thumbnail, as its specification
/// bounds it.
const THUMBNAIL_SIDE: u32 = 256;

/// A format Paint writes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaveFormat {
    /// Portable Network Graphics.
    Png,
    /// JPEG.
    Jpeg,
    /// Graphics Interchange Format.
    Gif,
    /// Windows bitmap.
    Bmp,
    /// Tag Image File Format: any number of pages.
    Tiff,
    /// A RISC OS sprite area.
    Sprites,
    /// OpenRaster: one picture, its layers kept.
    OpenRaster,
}

impl SaveFormat {
    /// Every format Paint writes, in the order a choice lists them.
    pub const ALL: [Self; 7] = [
        Self::Png,
        Self::Jpeg,
        Self::Gif,
        Self::Bmp,
        Self::Tiff,
        Self::Sprites,
        Self::OpenRaster,
    ];

    /// What the format is in the desktop's one media registry.
    #[must_use]
    pub const fn media(self) -> MediaType {
        match self {
            Self::Png => MediaType::ImagePng,
            Self::Jpeg => MediaType::ImageJpeg,
            Self::Gif => MediaType::ImageGif,
            Self::Bmp => MediaType::ImageBmp,
            Self::Tiff => MediaType::ImageTiff,
            Self::Sprites => MediaType::ImageSprite,
            Self::OpenRaster => MediaType::ImageOpenRaster,
        }
    }

    /// The format as a choice names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Sprites => "Sprite file",
            Self::OpenRaster => "OpenRaster",
        }
    }

    /// Whether it holds any number of pictures rather than one.
    #[must_use]
    pub const fn holds_several(self) -> bool {
        matches!(self, Self::Tiff | Self::Sprites)
    }

    /// Whether a picture made for it may be stored at `depth`, `None` being
    /// colour: a JPEG and OpenRaster's layers have no palette and a GIF
    /// nothing else.
    #[must_use]
    pub const fn admits(self, depth: Option<IndexDepth>) -> bool {
        match self {
            Self::Jpeg | Self::OpenRaster => depth.is_none(),
            Self::Gif => depth.is_some(),
            _ => true,
        }
    }

    /// Whether a picture made for it may have a clear background.
    #[must_use]
    pub const fn holds_transparency(self) -> bool {
        !matches!(self, Self::Jpeg)
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
            ViewFormat::Gif => Some(Self::Gif),
            ViewFormat::Bmp => Some(Self::Bmp),
            ViewFormat::Tiff => Some(Self::Tiff),
            ViewFormat::Sprite => Some(Self::Sprites),
            ViewFormat::OpenRaster => Some(Self::OpenRaster),
            _ => None,
        }
    }
}

/// How a document is written in each format that offers a choice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SaveSettings {
    /// The JPEG quality, `1..=100`.
    pub jpeg_quality: u8,
    /// How a GIF is written.
    pub gif: GifOptions,
    /// How a TIFF is written.
    pub tiff: TiffOptions,
}

impl Default for SaveSettings {
    fn default() -> Self {
        Self {
            jpeg_quality: JpegOptions::DEFAULT_QUALITY,
            gif: GifOptions::default(),
            tiff: TiffOptions::default(),
        }
    }
}

impl SaveSettings {
    /// The settings that write a file again as `written` says it was.
    #[must_use]
    pub fn as_written(written: Written) -> Self {
        let mut settings = Self::default();
        match written {
            Written::Plain => {}
            Written::Gif(gif) => settings.gif = gif,
            Written::Tiff(tiff) => settings.tiff = tiff,
        }
        settings
    }
}

/// Something a format cannot keep of a document, said with a save.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Loss {
    /// Only OpenRaster holds layers: any other format is written them laid
    /// together.
    Layers,
    /// A JPEG holds no transparency.
    Transparency,
    /// A GIF shows each pixel or hides it.
    PartialTransparency,
    /// A GIF holds 256 colours at most.
    Colours,
    /// An OpenRaster file's layers are read back as colour.
    Palette,
    /// Only a sprite holds pixels other than square.
    PixelShape,
    /// The format cannot state the picture's density.
    Density,
    /// Only a sprite file holds a sprite's name and mode.
    SpriteDetails,
}

impl Loss {
    /// What the loss means for the document, as a person reads it.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Layers => "This format holds one layer, so the layers are laid together",
            Self::Transparency => "A JPEG holds no transparency, so the picture is laid over white",
            Self::PartialTransparency => {
                "A GIF holds no partial transparency, so each pixel is shown or clear"
            }
            Self::Colours => {
                "A GIF holds 256 colours at most, so the picture's are reduced to that"
            }
            Self::Palette => "OpenRaster reads its layers as colour, so the palette is not kept",
            Self::PixelShape => {
                "This format holds square pixels alone, so the pixels' shape is not kept"
            }
            Self::Density => "This format cannot state the picture's density, so it is not kept",
            Self::SpriteDetails => {
                "This format holds no sprite names or modes, so they are not kept"
            }
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
    /// The format holds pictures and the document holds a sprite kept as its
    /// bytes.
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
                "Paint cannot write {suffix} files: give the name .png, .jpg, .gif, .bmp, .tif, \
                 .ora or .spr"
            ),
            Self::SeveralPictures(count) => write!(
                f,
                "The {count} pictures can only be saved together, as a TIFF or a sprite file: \
                 give the name .tif or .spr"
            ),
            Self::KeptSprite => {
                f.write_str("A sprite that cannot be edited can only be saved in a sprite file")
            }
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
/// under as `format`: every name ending of it, so a name given none takes
/// the first.
///
/// # Errors
///
/// The refusal of a save in that format, where it cannot hold the document.
pub fn save_endings(
    entries: &[Entry],
    origin: Origin,
    format: SaveFormat,
) -> Result<SaveEndings, SaveRefusal> {
    writable_as(Some(format), entries, origin)?;
    let mut endings = SaveEndings::ANY;
    for (separator, code) in name_endings(format.media()) {
        // An ending past the table's bound is left out, so the picker
        // refuses it: narrower than the save, never wider.
        let _ = endings.push(separator, code);
    }
    Ok(endings)
}

/// The format a document of `entries`, read from `origin`, is written in as
/// `named` asks, else as what it is.
///
/// # Errors
///
/// [`SaveRefusal`] where it cannot be written so.
pub fn writable_as(
    named: Option<SaveFormat>,
    entries: &[Entry],
    origin: Origin,
) -> Result<SaveFormat, SaveRefusal> {
    let format = named.unwrap_or_else(|| natural(entries, origin));
    match format {
        SaveFormat::Sprites => {
            // A sprite area carries no signature, so its name is how it is
            // known.
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
        }
        SaveFormat::Tiff => {
            if entries.iter().any(|entry| matches!(entry, Entry::Kept(_))) {
                return Err(SaveRefusal::KeptSprite);
            }
        }
        _ => match entries {
            [Entry::Picture(_)] => {}
            [Entry::Kept(_)] => return Err(SaveRefusal::KeptSprite),
            many => return Err(SaveRefusal::SeveralPictures(many.len())),
        },
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
    /// Its colours were stated in a form Paint restates rather than keeps.
    Converted,
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
            Self::Converted => f.write_str(
                "This file's colours are stated in a form Paint does not keep, such as CMYK, so \
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
    if unkept.converted {
        return Err(NotWrittenBack::Converted);
    }
    match format_for(name, document.entries(), document.origin()) {
        Ok(written) if written == own => Ok(()),
        Ok(_) => Err(NotWrittenBack::Misnamed),
        Err(refusal) => Err(NotWrittenBack::Refused(refusal)),
    }
}

/// What a document of `entries`, read from `origin`, is written as when its
/// name does not say: what it is — a sprite area, else the format it was read
/// as or made for.
#[must_use]
pub fn natural(entries: &[Entry], origin: Origin) -> SaveFormat {
    if sprite_area(entries, origin) {
        return SaveFormat::Sprites;
    }
    match origin {
        Origin::Read(format) => SaveFormat::of(format).unwrap_or(SaveFormat::Png),
        Origin::New(format) => format,
    }
}

/// `snapshot` written as `format`; a sprite with no details of its own is
/// named after `name`.
///
/// # Errors
///
/// [`EncodeError`] as the encoder states it.
pub fn encode(snapshot: &Snapshot, format: SaveFormat, name: &str) -> Result<Vec<u8>, EncodeError> {
    let settings = snapshot.settings;
    match format {
        SaveFormat::Sprites => encode_sprites(&snapshot.entries, name),
        SaveFormat::Tiff => {
            let mut pages: Vec<Stated> = Vec::new();
            pages
                .try_reserve_exact(snapshot.entries.len())
                .map_err(|_| EncodeError::OutOfMemory)?;
            for entry in &snapshot.entries {
                let Entry::Picture(picture) = entry else {
                    return Err(EncodeError::SpriteAreaEmpty);
                };
                pages.push(Stated::flat(picture)?);
            }
            let sources = tairix_util::fallible::collected(
                pages.len(),
                pages.iter().map(|page| page as &dyn PictureSource),
            )
            .ok_or(EncodeError::OutOfMemory)?;
            encode_tiff(&sources, settings.tiff)
        }
        SaveFormat::Png
        | SaveFormat::Jpeg
        | SaveFormat::Gif
        | SaveFormat::Bmp
        | SaveFormat::OpenRaster => {
            let Some(Entry::Picture(picture)) = snapshot.entries.get(snapshot.current) else {
                return Err(EncodeError::SpriteAreaEmpty);
            };
            let source = Stated::flat(picture)?;
            match format {
                SaveFormat::Png => encode_png(&source),
                SaveFormat::Jpeg => {
                    let options = JpegOptions::new(settings.jpeg_quality, JPEG_BACKGROUND)?;
                    encode_jpeg(&source, options)
                }
                SaveFormat::Bmp => encode_bmp(&source),
                SaveFormat::OpenRaster => encode_layers(picture, &source),
                _ => encode_as_gif(&source, settings.gif),
            }
        }
    }
}

/// `picture`, whose layers laid together are `merged`, written as
/// OpenRaster: each layer whole on the canvas, and a thumbnail of the whole.
fn encode_layers(picture: &Picture, merged: &Stated) -> Result<Vec<u8>, EncodeError> {
    let layers = tairix_util::fallible::collected(
        picture.layers().len(),
        picture.layers().iter().map(|layer| OraLayerSource {
            name: &layer.name,
            picture: &layer.canvas,
            at: (0, 0),
            opacity: layer.opacity,
            visible: layer.visible,
        }),
    )
    .ok_or(EncodeError::OutOfMemory)?;
    let thumbnail = thumbnail(&merged.canvas)?;
    encode_ora(
        picture.size(),
        &layers,
        merged,
        thumbnail.as_ref().unwrap_or(&merged.canvas),
    )
}

/// `canvas` shrunk smoothly to fit a thumbnail, its shape kept; `None` where
/// it already fits.
fn thumbnail(canvas: &Canvas) -> Result<Option<Canvas>, EncodeError> {
    let (width, height) = (canvas.width(), canvas.height());
    let longest = width.max(height);
    if longest <= THUMBNAIL_SIDE {
        return Ok(None);
    }
    let fit = |side: u32| {
        let scaled = u64::from(side) * u64::from(THUMBNAIL_SIDE) / u64::from(longest);
        u32::try_from(scaled).unwrap_or(THUMBNAIL_SIDE).max(1)
    };
    let transform = Transform::Scale {
        width: fit(width),
        height: fit(height),
        smooth: true,
    };
    apply(canvas, transform).map(Some).map_err(|err| match err {
        TransformError::OutOfMemory => EncodeError::OutOfMemory,
        TransformError::BadSize | TransformError::NotApplicable => EncodeError::TooLarge,
    })
}

/// `source` written as a GIF: a colour picture reduced to the colours a GIF
/// holds first, with room left for a clear entry where it has transparency.
fn encode_as_gif(source: &Stated, options: GifOptions) -> Result<Vec<u8>, EncodeError> {
    let canvas = &source.canvas;
    if canvas.kind().palette().is_some() {
        return encode_gif(source, options);
    }
    let colours = if canvas.has_transparency() { 255 } else { 256 };
    let palette = palette_for(canvas, colours).map_err(|_| EncodeError::OutOfMemory)?;
    let reduced = indexed(canvas, IndexDepth::Eight, &palette, true).map_err(|err| match err {
        TransformError::OutOfMemory => EncodeError::OutOfMemory,
        _ => EncodeError::InvalidPalette,
    })?;
    let reduced = Stated {
        canvas: reduced,
        density: source.density,
    };
    encode_gif(&reduced, options)
}

/// What `snapshot` written as `format` cannot keep, most telling first.
///
/// # Errors
///
/// [`OutOfMemory`] when the list cannot be held, or a picture of layers
/// cannot be laid together to see what it shows.
pub fn losses(snapshot: &Snapshot, format: SaveFormat) -> Result<Vec<Loss>, OutOfMemory> {
    losses_of(&snapshot.entries, snapshot.current, format)
}

/// What `entries`, `current` showing, written as `format` cannot keep, most
/// telling first.
///
/// # Errors
///
/// [`OutOfMemory`] when the list cannot be held, or a picture of layers
/// cannot be laid together to see what it shows.
pub fn losses_of(
    entries: &[Entry],
    current: usize,
    format: SaveFormat,
) -> Result<Vec<Loss>, OutOfMemory> {
    let mut alpha = Alpha::unread(entries.len())?;
    losses_noted(entries, current, format, &mut alpha)
}

/// What a picture written shows of transparency, its layers laid together:
/// read once a picture however many formats ask.
#[derive(Copy, Clone)]
struct Alpha {
    any: bool,
    partial: bool,
}

impl Alpha {
    /// Room for what each of `count` entries holds, none read yet.
    fn unread(count: usize) -> Result<Vec<Option<Self>>, OutOfMemory> {
        tairix_util::fallible::filled(count, None).ok_or(OutOfMemory)
    }

    fn of(picture: &Picture) -> Result<Self, OutOfMemory> {
        let laid;
        let canvas = if picture.single() {
            picture.canvas()
        } else {
            laid = picture.flattened()?;
            &laid
        };
        Ok(Self {
            any: canvas.has_transparency(),
            partial: canvas.has_partial_alpha(),
        })
    }
}

/// [`losses_of`], what each entry shows of transparency read into `alpha`
/// the first time a format asks.
fn losses_noted(
    entries: &[Entry],
    current: usize,
    format: SaveFormat,
    alpha: &mut [Option<Alpha>],
) -> Result<Vec<Loss>, OutOfMemory> {
    let mut found = Vec::new();
    let written = if format.holds_several() {
        0..entries.len()
    } else {
        current..(current + 1).min(entries.len())
    };
    let mut note = |loss: Loss| {
        if !found.contains(&loss) {
            found.try_reserve(1).map_err(|_| OutOfMemory)?;
            found.push(loss);
        }
        Ok::<(), OutOfMemory>(())
    };
    for index in written {
        let Some(picture) = entries[index].picture() else {
            continue;
        };
        if !picture.single() && format != SaveFormat::OpenRaster {
            note(Loss::Layers)?;
        }
        let mut shown = || -> Result<Alpha, OutOfMemory> {
            if let Some(read) = alpha.get(index).copied().flatten() {
                return Ok(read);
            }
            let read = Alpha::of(picture)?;
            if let Some(slot) = alpha.get_mut(index) {
                *slot = Some(read);
            }
            Ok(read)
        };
        match format {
            SaveFormat::Jpeg if shown()?.any => note(Loss::Transparency)?,
            SaveFormat::Gif => {
                if picture.kind().palette().is_none() {
                    note(Loss::Colours)?;
                }
                if shown()?.partial {
                    note(Loss::PartialTransparency)?;
                }
            }
            SaveFormat::OpenRaster if picture.kind().palette().is_some() => {
                note(Loss::Palette)?;
            }
            _ => {}
        }
        if format != SaveFormat::Sprites && picture.pixel_aspect() != (1, 1) {
            note(Loss::PixelShape)?;
        }
        if format != SaveFormat::Sprites && picture.sprite.is_some() {
            note(Loss::SpriteDetails)?;
        }
        if picture
            .density
            .is_some_and(|density| !states_density(format, density))
        {
            note(Loss::Density)?;
        }
    }
    Ok(found)
}

/// Every format a document of `entries`, `current` showing and read from
/// `origin`, can be written as, each with what it would not keep: what the
/// Save As sheet offers, worked out on a worker since finding a loss reads
/// every pixel, each picture's pixels read once for every format.
///
/// # Errors
///
/// [`OutOfMemory`], as [`losses_of`].
pub fn survey(
    entries: &[Entry],
    current: usize,
    origin: Origin,
) -> Result<Vec<(SaveFormat, Vec<Loss>)>, OutOfMemory> {
    let mut offered = Vec::new();
    offered
        .try_reserve_exact(SaveFormat::ALL.len())
        .map_err(|_| OutOfMemory)?;
    let mut alpha = Alpha::unread(entries.len())?;
    for format in SaveFormat::ALL {
        if writable_as(Some(format), entries, origin).is_ok() {
            offered.push((format, losses_noted(entries, current, format, &mut alpha)?));
        }
    }
    Ok(offered)
}

/// Whether `format` can state `density` at all: a GIF only a shape, a BMP
/// only a length, a sprite and OpenRaster neither.
fn states_density(format: SaveFormat, density: Density) -> bool {
    let shape = density.unit() == DensityUnit::Aspect;
    match format {
        SaveFormat::Png | SaveFormat::Jpeg | SaveFormat::Tiff => true,
        SaveFormat::Gif => shape,
        SaveFormat::Bmp => !shape,
        SaveFormat::Sprites | SaveFormat::OpenRaster => false,
    }
}

/// The most telling of what `snapshot` written as `format` cannot keep,
/// said with the save.
///
/// # Errors
///
/// [`OutOfMemory`], as [`losses`].
pub fn lost_in(
    snapshot: &Snapshot,
    format: SaveFormat,
) -> Result<Option<&'static str>, OutOfMemory> {
    Ok(losses(snapshot, format)?.first().map(|loss| loss.message()))
}

/// A picture as one canvas — its layers laid together — with the density
/// the document holds for it, which is how every encoder reads one.
struct Stated {
    canvas: Canvas,
    density: Option<Density>,
}

impl Stated {
    fn flat(picture: &Picture) -> Result<Self, EncodeError> {
        Ok(Self {
            canvas: picture
                .flattened()
                .map_err(|OutOfMemory| EncodeError::OutOfMemory)?,
            density: picture.density,
        })
    }
}

impl PictureSource for Stated {
    fn width(&self) -> u32 {
        self.canvas.width()
    }

    fn height(&self) -> u32 {
        self.canvas.height()
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureSource::kind(&self.canvas)
    }

    fn read_row(&self, y: u32, samples: &mut [u8], mask: &mut [u8]) {
        self.canvas.read_row(y, samples, mask);
    }

    fn density(&self) -> Option<Density> {
        self.density
    }
}

/// What one entry is written as: the sprite details it is written under and
/// the pixels, seen through [`OpaquePalette`] where its palette holds what a
/// sprite palette cannot.
enum Writing<'a> {
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
    // Each picture is written as its layers show together.
    let mut flat: Vec<Option<Canvas>> = Vec::new();
    flat.try_reserve_exact(entries.len())
        .map_err(|_| EncodeError::OutOfMemory)?;
    for entry in entries {
        flat.push(match entry {
            Entry::Kept(_) => None,
            Entry::Picture(picture) => Some(
                picture
                    .flattened()
                    .map_err(|OutOfMemory| EncodeError::OutOfMemory)?,
            ),
        });
    }
    let mut written: Vec<Writing<'_>> = Vec::new();
    written
        .try_reserve_exact(entries.len())
        .map_err(|_| EncodeError::OutOfMemory)?;
    for (entry, canvas) in entries.iter().zip(&flat) {
        written.push(match (entry, canvas) {
            (Entry::Picture(picture), Some(canvas)) => {
                let sprite = sprite_details(canvas, picture.sprite.as_ref(), name, &mut taken)?;
                let source = if needs_opaque_palette(canvas) {
                    Source::Opaque(OpaquePalette::new(canvas)?)
                } else {
                    Source::Canvas(canvas)
                };
                Writing::Picture { sprite, source }
            }
            (Entry::Kept(kept), _) => Writing::Kept(&kept.bytes),
            (Entry::Picture(_), None) => return Err(EncodeError::OutOfMemory),
        });
    }
    let inputs = tairix_util::fallible::collected(
        written.len(),
        written.iter().map(|written| match written {
            Writing::Kept(bytes) => SpriteInput::Opaque(bytes),
            Writing::Picture { sprite, source } => SpriteInput::Picture {
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

/// The details a picture shown as `canvas` is written as a sprite under:
/// its own `held`, made to fit what its pixels have become, or ones made
/// from what it is.
fn sprite_details(
    canvas: &Canvas,
    held: Option<&SpriteInfo>,
    name: &str,
    taken: &mut Vec<SpriteName>,
) -> Result<SpriteInfo, EncodeError> {
    let partial = canvas.has_partial_alpha();
    let transparent = canvas.has_transparency();
    let mut sprite = if let Some(sprite) = held {
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
