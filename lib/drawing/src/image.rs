//! A raster picture placed in a drawing: its pixels, the rectangle they fill
//! in its own space, and the bytes it was read from where a writer can embed
//! them as they came.
//!
//! The pixels are held as a pyramid (`tairix_raster::Pyramid`), premultiplied
//! and shared, so every render of every tile, and every step of history, takes
//! them for the price of a pointer, and a picture drawn far smaller is drawn
//! from a level that averages it rather than aliasing it.
//!
//! A picture's bytes are never parsed outside the reader, which runs in the
//! parser sandbox: what a writer needs to say of them is held beside them as
//! facts the decode stated ([`Encoding`]).

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_collections::{try_arc, OrderedMap, SharedSlice};
use tairix_image::{
    DecodeError, DecodeLimits, DensityUnit, ImageFormat, JpegFrame, MAX_SPRITE_EIG,
};
use tairix_raster::{Pyramid, Surface, MAX_SURFACE_PIXELS};

use crate::limits::{self, MAX_IMAGE_SIDE, MAX_IMAGE_SOURCE_BYTES, MAX_TOTAL_IMAGE_PIXELS};
use crate::{Drawing, DrawingError, Kind, Node, ReadError};

/// The encoded form a picture's pixels were read from, and what it states
/// that a writer passing the bytes on must say of them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Encoding {
    /// PNG.
    Png,
    /// JPEG (JFIF), its raster as stored and how that turns into the picture.
    Jpeg(JpegFrame),
    /// One RISC OS sprite, control block first, its pixels each `2^eig` OS
    /// units across and down.
    Sprite {
        /// The eigen factors across and down.
        eig: (u8, u8),
    },
}

impl Encoding {
    /// Its media type, as a data URL names it; `None` for a sprite, which no
    /// web format embeds.
    #[must_use]
    pub const fn media_type(self) -> Option<&'static str> {
        match self {
            Self::Png => Some("image/png"),
            Self::Jpeg(_) => Some("image/jpeg"),
            Self::Sprite { .. } => None,
        }
    }
}

/// The bytes a picture was read from, and their encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    /// How they are encoded.
    pub encoding: Encoding,
    /// The bytes themselves.
    pub bytes: SharedSlice<u8>,
}

/// A raster picture laid over a rectangle of its own space.
#[derive(Clone, Debug)]
pub struct Image {
    pyramid: Arc<Pyramid>,
    place: Bounds,
    source: Option<Source>,
}

impl Image {
    /// `pixels`, premultiplied, laid over `place`, kept with the `source`
    /// they were decoded from where there is one.
    ///
    /// # Errors
    ///
    /// As [`shared`](Self::shared), and [`DrawingError::OutOfMemory`] when
    /// its levels cannot be held.
    pub fn new(
        pixels: Surface,
        place: Bounds,
        source: Option<Source>,
    ) -> Result<Self, DrawingError> {
        check_parts(pixels.width(), pixels.height(), place, source.as_ref())?;
        let pyramid = Pyramid::new(pixels).ok_or(DrawingError::OutOfMemory)?;
        Self::shared(try_arc(pyramid)?, place, source)
    }

    /// The picture `pyramid` holds laid over `place`, sharing its pixels.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for an empty picture, a rectangle not
    /// finite, within the drawing's coordinates and of some size, or a source
    /// whose facts the pixels contradict; [`DrawingError::TooComplex`] for a
    /// picture past the pixels a surface holds or a source past
    /// [`MAX_IMAGE_SOURCE_BYTES`].
    pub fn shared(
        pyramid: Arc<Pyramid>,
        place: Bounds,
        source: Option<Source>,
    ) -> Result<Self, DrawingError> {
        let base = pyramid.base();
        check_parts(base.width(), base.height(), place, source.as_ref())?;
        Ok(Self {
            pyramid,
            place,
            source,
        })
    }

    /// This picture laid over `place` instead.
    ///
    /// # Errors
    ///
    /// As [`shared`](Self::shared), for the rectangle.
    pub fn placed(&self, place: Bounds) -> Result<Self, DrawingError> {
        Self::shared(self.pyramid.clone(), place, self.source.clone())
    }

    /// Its pixels, premultiplied.
    #[must_use]
    pub fn pixels(&self) -> &Surface {
        self.pyramid.base()
    }

    /// Its pixels at every halving, shared.
    #[must_use]
    pub const fn pyramid(&self) -> &Arc<Pyramid> {
        &self.pyramid
    }

    /// The rectangle its pixels fill, in its own space.
    #[must_use]
    pub const fn place(&self) -> Bounds {
        self.place
    }

    /// The bytes it was read from, where a writer can embed them.
    #[must_use]
    pub const fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// The map from its pixels onto its rectangle, a pixel to the unit and
    /// the first row at the top.
    #[must_use]
    pub fn to_place(&self) -> Affine {
        let base = self.pyramid.base();
        Affine::scale(
            self.place.width() / f64::from(base.width()),
            self.place.height() / f64::from(base.height()),
        )
        .then(Affine::translate(self.place.min.x, self.place.min.y))
    }

    /// Whether every value is one a drawing may hold.
    pub(crate) fn check(&self) -> Result<(), DrawingError> {
        let base = self.pyramid.base();
        check_parts(
            base.width(),
            base.height(),
            self.place,
            self.source.as_ref(),
        )
    }

    /// Its rectangle as a path, in its own space: what its outline view, a
    /// clip it makes and its selection frame trace.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the path cannot be held.
    pub fn outline(&self) -> Result<tairix_bezier::Path, DrawingError> {
        crate::shape::Shape::Rect(crate::shape::RectShape {
            x: self.place.min.x,
            y: self.place.min.y,
            width: self.place.width(),
            height: self.place.height(),
            rx: 0.0,
            ry: 0.0,
        })
        .to_path()
    }

    /// Whether `point`, in its own space, falls on its rectangle.
    #[must_use]
    pub fn covers(&self, point: Point) -> bool {
        self.place.contains(point)
    }
}

impl PartialEq for Image {
    /// The same pixels laid over the same rectangle from the same source; a
    /// shared picture is the same without its pixels being read.
    fn eq(&self, other: &Self) -> bool {
        self.place == other.place
            && self.source == other.source
            && (Arc::ptr_eq(&self.pyramid, &other.pyramid) || {
                let (mine, theirs) = (self.pixels(), other.pixels());
                mine.width() == theirs.width()
                    && mine.height() == theirs.height()
                    && mine.pixels() == theirs.pixels()
            })
    }
}

/// Whether a picture of `width`×`height` pixels over `place`, from `source`,
/// is one a drawing may hold.
fn check_parts(
    width: u32,
    height: u32,
    place: Bounds,
    source: Option<&Source>,
) -> Result<(), DrawingError> {
    if width == 0 || height == 0 {
        return Err(DrawingError::Invalid);
    }
    if width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
        || usize::try_from(u64::from(width) * u64::from(height))
            .map_or(true, |pixels| pixels > MAX_SURFACE_PIXELS)
    {
        return Err(DrawingError::TooComplex);
    }
    let corners = [place.min.x, place.min.y, place.max.x, place.max.y];
    if !corners.into_iter().all(limits::coordinate) || place.width() <= 0.0 || place.height() <= 0.0
    {
        return Err(DrawingError::Invalid);
    }
    let Some(source) = source else {
        return Ok(());
    };
    if source.bytes.is_empty() {
        return Err(DrawingError::Invalid);
    }
    if source.bytes.len() > MAX_IMAGE_SOURCE_BYTES {
        return Err(DrawingError::TooComplex);
    }
    let stated = match source.encoding {
        Encoding::Png => true,
        Encoding::Jpeg(frame) => {
            matches!(frame.components, 1 | 3)
                && frame
                    .orientation
                    .picture_size(frame.stored.0, frame.stored.1)
                    == (width, height)
        }
        Encoding::Sprite { eig } => eig.0 <= MAX_SPRITE_EIG && eig.1 <= MAX_SPRITE_EIG,
    };
    if stated {
        Ok(())
    } else {
        Err(DrawingError::Invalid)
    }
}

/// Whether `drawing`, with `adding` beside it, keeps its pictures within
/// [`MAX_TOTAL_IMAGE_PIXELS`], each counted once however many objects show
/// it, as a reader counts what it reads back: what an edit that brings
/// pictures in holds itself to, so no reader refuses a drawing written for
/// the pixels its pictures hold.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when its pictures cannot be listed.
pub fn pictures_fit(drawing: &Drawing, adding: &[Arc<Node>]) -> Result<bool, DrawingError> {
    fits_within(drawing, adding, MAX_TOTAL_IMAGE_PIXELS)
}

/// Whether `drawing` and `adding` show no more than `most` pixels of
/// pictures, each counted once.
fn fits_within(drawing: &Drawing, adding: &[Arc<Node>], most: u64) -> Result<bool, DrawingError> {
    let layers = drawing.layers();
    let shown = adding
        .iter()
        .fold(layers.summary().picture_pixels, |sum, node| {
            sum.saturating_add(node.picture_pixels())
        });
    if shown <= most {
        return Ok(true);
    }
    // Shown more often than the bound allows: count each picture once.
    let mut pictures = Vec::new();
    for node in layers.iter().chain(adding) {
        list_pictures(node, &mut pictures)?;
    }
    pictures.sort_unstable_by_key(|&(at, _)| at);
    pictures.dedup_by_key(|&mut (at, _)| at);
    let held = pictures
        .iter()
        .fold(0_u64, |sum, &(_, pixels)| sum.saturating_add(pixels));
    Ok(held <= most)
}

/// Every picture `node` shows, by where its pixels are held, with its pixel
/// count, once for each object showing it.
fn list_pictures(node: &Node, into: &mut Vec<(usize, u64)>) -> Result<(), DrawingError> {
    if node.picture_pixels() == 0 {
        return Ok(());
    }
    match node.kind() {
        Kind::Image { image } => {
            let pixels = image.pixels();
            let count = u64::from(pixels.width()) * u64::from(pixels.height());
            tairix_util::fallible::try_push(into, (Arc::as_ptr(image.pyramid()).addr(), count))
                .map_err(|_| DrawingError::OutOfMemory)
        }
        Kind::Group { children, clip } => {
            for child in children.iter().chain(clip) {
                list_pictures(child, into)?;
            }
            Ok(())
        }
        Kind::Shape { .. } | Kind::Text { .. } => Ok(()),
    }
}

/// What a picture's pixels need of a format that writes them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Survey {
    /// No pixel lets anything through.
    pub(crate) opaque: bool,
    /// Each pixel is wholly shown or wholly not.
    pub(crate) binary: bool,
    /// Each pixel is a grey.
    pub(crate) grey: bool,
}

/// What `pixels` need, read once.
pub(crate) fn survey(pixels: &Surface) -> Survey {
    let mut survey = Survey {
        opaque: true,
        binary: true,
        grey: true,
    };
    for pixel in pixels.pixels() {
        survey.opaque &= pixel.a == u8::MAX;
        survey.binary &= pixel.a == u8::MAX || pixel.a == 0;
        // A premultiplied grey's channels are equal as its straight ones are.
        survey.grey &= pixel.r == pixel.g && pixel.g == pixel.b;
    }
    survey
}

/// What every picture a drawing holds is decoded under: a side no longer than
/// a drawing's picture may have, the pixels a surface holds, and a progressive
/// JPEG's store for a frame that large. The same for each, so which pictures a
/// document loses never depends on the order they come in.
const DECODE_LIMITS: DecodeLimits = DecodeLimits::new(
    MAX_IMAGE_SIDE,
    MAX_IMAGE_SIDE,
    MAX_SURFACE_PIXELS as u64,
    MAX_SURFACE_PIXELS as u64 * tairix_image::PROGRESSIVE_420_BYTES_PER_PIXEL,
);

/// A picture decoded for a drawing: its pixels at every halving and what it
/// was read from.
#[derive(Clone)]
pub(crate) struct Picture {
    pub(crate) pyramid: Arc<Pyramid>,
    pub(crate) source: Option<Source>,
}

impl Picture {
    /// This picture laid over `place`.
    pub(crate) fn placed(&self, place: Bounds) -> Result<Image, DrawingError> {
        Image::shared(self.pyramid.clone(), place, self.source.clone())
    }

    /// Its width and height in pixels.
    pub(crate) fn size(&self) -> (u32, u32) {
        let base = self.pyramid.base();
        (base.width(), base.height())
    }
}

/// The pictures one document embeds, each decoded once however many objects
/// show it, and the pixels its drawing may yet hold.
pub(crate) struct Pictures<'a> {
    /// By the bytes that encode it; `None` for one that could not be read.
    decoded: OrderedMap<&'a [u8], Option<Picture>>,
    pixels_left: u64,
}

impl<'a> Pictures<'a> {
    pub(crate) fn new() -> Self {
        Self {
            decoded: OrderedMap::new(),
            pixels_left: MAX_TOTAL_IMAGE_PIXELS,
        }
    }

    /// The picture `encoded` stands for, decoded by `decode` the first time
    /// it is met; `None` for one that cannot be read or held, which the
    /// drawing loses.
    ///
    /// # Errors
    ///
    /// [`ReadError::TooComplex`] once the document's pictures hold more than
    /// a drawing's may, and [`ReadError::OutOfMemory`] when one cannot be
    /// held.
    pub(crate) fn picture(
        &mut self,
        encoded: &'a [u8],
        decode: impl FnOnce(&DecodeLimits) -> Result<(Surface, Option<Source>), DecodeError>,
    ) -> Result<Option<Picture>, ReadError> {
        if let Some(known) = self.decoded.get(&encoded) {
            return Ok(known.clone());
        }
        let picture = match decode(&DECODE_LIMITS) {
            Ok((pixels, source)) => {
                let count = u64::from(pixels.width()) * u64::from(pixels.height());
                if count > self.pixels_left {
                    return Err(ReadError::TooComplex);
                }
                self.pixels_left -= count;
                let pyramid = Pyramid::new(pixels).ok_or(ReadError::OutOfMemory)?;
                Some(Picture {
                    pyramid: try_arc(pyramid).map_err(|_| ReadError::OutOfMemory)?,
                    source,
                })
            }
            Err(DecodeError::OutOfMemory) => return Err(ReadError::OutOfMemory),
            Err(_) => None,
        };
        self.decoded
            .try_insert(encoded, picture.clone())
            .map_err(|_| ReadError::OutOfMemory)?;
        Ok(picture)
    }
}

/// `bytes`, a PNG or a JPEG by its content, decoded under `limits`, kept with
/// the bytes and what they state.
///
/// # Errors
///
/// [`DecodeError::UnknownFormat`] for any other content, and whatever the
/// decode refuses.
pub(crate) fn decode_encoded(
    bytes: SharedSlice<u8>,
    limits: &DecodeLimits,
) -> Result<(Surface, Option<Source>), DecodeError> {
    let encoding = match tairix_image::sniff(&bytes) {
        Some(ImageFormat::Png) => Encoding::Png,
        Some(ImageFormat::Jpeg) => Encoding::Jpeg(tairix_image::jpeg_frame(&bytes)?),
        _ => return Err(DecodeError::UnknownFormat),
    };
    let image = tairix_image::decode(&bytes, limits)?;
    let pixels = Surface::from_rgba8(image.width(), image.height(), image.pixels())
        .ok_or(DecodeError::OutOfMemory)?;
    Ok((pixels, Some(Source { encoding, bytes })))
}

/// `bytes`, one RISC OS sprite, decoded under `limits`, kept with its own
/// bytes and the eigen factors it is drawn at.
///
/// # Errors
///
/// Whatever the decode refuses.
pub(crate) fn decode_sprite(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<(Surface, Option<Source>), DecodeError> {
    let sprite = tairix_image::decode_sprite(bytes, limits)?;
    let pixels = Surface::from_rgba8(
        sprite.image.width(),
        sprite.image.height(),
        sprite.image.pixels(),
    )
    .ok_or(DecodeError::OutOfMemory)?;
    let own = usize::try_from(sprite.length)
        .ok()
        .and_then(|length| bytes.get(..length))
        .ok_or(DecodeError::SpriteTruncated)?;
    let kept = SharedSlice::try_from_slice(own).map_err(|_| DecodeError::OutOfMemory)?;
    let source = Source {
        encoding: Encoding::Sprite { eig: sprite.eig },
        bytes: kept,
    };
    Ok((pixels, Some(source)))
}

/// A picture file a user places in a drawing, decoded: its pixels, the bytes
/// it came as where a writer can pass them on, and the size it states its
/// pixels at.
#[derive(Debug)]
pub struct PlacedPicture {
    /// Its pixels, premultiplied.
    pub pixels: Surface,
    /// The bytes it came as, for a PNG or a JPEG.
    pub source: Option<Source>,
    /// The size its pixels span in drawing pixels: their own at the density
    /// the file states, a drawing pixel each where it states none.
    pub natural: Point,
}

/// `bytes`, any still picture `tairix_image` reads, by its content — the
/// first page or sprite of a file of several — decoded for placing in a
/// drawing. Run only in the parser sandbox: the bytes are untrusted.
///
/// # Errors
///
/// [`ReadError::Unrecognised`] for bytes that are no picture,
/// [`ReadError::Malformed`] for a broken one, [`ReadError::TooComplex`] for
/// one past the pixels a drawing's picture holds, and
/// [`ReadError::OutOfMemory`] when it cannot be held.
pub fn decode_placed(bytes: &[u8]) -> Result<PlacedPicture, ReadError> {
    use tairix_image::{NativeDocument, SpriteEntry};
    let format = tairix_image::sniff(bytes).ok_or(ReadError::Unrecognised)?;
    let limits = DECODE_LIMITS;
    let refused = |error: DecodeError| match error {
        DecodeError::OutOfMemory => ReadError::OutOfMemory,
        DecodeError::PixelCountExceedsLimit
        | DecodeError::WidthExceedsLimit
        | DecodeError::HeightExceedsLimit => ReadError::TooComplex,
        _ => ReadError::Malformed,
    };
    let opened = tairix_image::open_native(format, bytes, &limits).map_err(refused)?;
    let picture = match opened {
        NativeDocument::Picture { picture, .. } => picture,
        NativeDocument::Pages { mut pages, .. } => pages
            .page(0)
            .map_err(refused)?
            .ok_or(ReadError::Malformed)?,
        NativeDocument::Sprites(mut sprites) => match sprites.sprite(0).map_err(refused)? {
            Some(SpriteEntry::Picture(sprite)) => {
                let density = sprite.mode.density();
                sprite.picture.with_density(density)
            }
            _ => return Err(ReadError::Malformed),
        },
        NativeDocument::Layers { .. } => {
            let flat = tairix_image::decode_as(format, bytes, &limits).map_err(refused)?;
            tairix_image::Picture::rgba(flat.width(), flat.height(), flat.into_pixels())
                .map_err(|_| ReadError::Malformed)?
        }
    };
    let (width, height) = (picture.width(), picture.height());
    let natural = natural_size((width, height), picture.density());
    let rgba = picture.to_rgba().ok_or(ReadError::OutOfMemory)?;
    drop(picture);
    let pixels = Surface::from_rgba8(width, height, &rgba).ok_or(ReadError::OutOfMemory)?;
    let encoding = match format {
        ImageFormat::Png => Some(Encoding::Png),
        ImageFormat::Jpeg => Some(Encoding::Jpeg(
            tairix_image::jpeg_frame(bytes).map_err(refused)?,
        )),
        _ => None,
    };
    let source = match encoding {
        Some(encoding) if bytes.len() <= MAX_IMAGE_SOURCE_BYTES => Some(Source {
            encoding,
            bytes: SharedSlice::try_from_slice(bytes).map_err(|_| ReadError::OutOfMemory)?,
        }),
        _ => None,
    };
    Ok(PlacedPicture {
        pixels,
        source,
        natural,
    })
}

/// Whether a picture placed at its own size may span `natural`, its width and
/// height in drawing pixels: of some size, and within a drawing's
/// coordinates.
#[must_use]
pub fn placeable(natural: Point) -> bool {
    [natural.x, natural.y]
        .into_iter()
        .all(|side| limits::coordinate(side) && side > 0.0)
}

/// The size, in drawing pixels, `pixels` span at `density`: the inch's
/// ninety-six drawing pixels at the density a length unit states, and its
/// pixels' shape where it states only that. A density is advisory, so one it
/// states none of, or one that would lay it past a drawing, leaves the
/// picture its own pixels.
fn natural_size((width, height): (u32, u32), density: Option<tairix_image::Density>) -> Point {
    let own = Point::new(f64::from(width), f64::from(height));
    let Some(density) = density else {
        return own;
    };
    let ratio =
        |(numerator, denominator): (u32, u32)| f64::from(numerator) / f64::from(denominator);
    let stated = match density.exact_in(DensityUnit::Inch) {
        Some(inch) => {
            let per_inch = f64::from(crate::drawing::PIXELS_PER_INCH);
            Point::new(
                own.x * per_inch / ratio(inch.across()),
                own.y * per_inch / ratio(inch.down()),
            )
        }
        None => Point::new(
            own.x,
            own.y * ratio(density.across()) / ratio(density.down()),
        ),
    };
    if placeable(stated) {
        stated
    } else {
        own
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
