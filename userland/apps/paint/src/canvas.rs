//! The picture being edited, held as square tiles shared copy-on-write.
//!
//! A tile is written only once its canvas holds the sole reference to it, so
//! the undo history, a save being encoded and the canvas on screen share the
//! same pixels until one of them changes. A blank canvas is a handful of
//! tiles shared across the whole grid, whatever its size.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::{flatten_row, masked_colour, IndexDepth, PictureKind, PictureSource, Rgba8};
use tairix_util::fallible;

use crate::shape::Bounds;

/// Side of a tile, in pixels.
pub const TILE: u32 = 64;

/// Longest side a picture may have: the edit decode's own bound.
pub const MAX_SIDE: u32 = tairix_sandbox::imageedit::MAX_EDIT_SIDE;

/// Most pixels a picture may hold: the edit decode's own bound.
pub const MAX_PIXELS: u64 = tairix_sandbox::imageedit::MAX_EDIT_PIXELS;

/// The allocator refused the pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutOfMemory;

/// Why a canvas could not be made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CanvasError {
    /// A side is zero or past [`MAX_SIDE`], or the picture past
    /// [`MAX_PIXELS`].
    BadSize,
    /// The palette is empty or longer than its depth indexes, or the fill
    /// does not suit the kind.
    BadPalette,
    /// The allocator refused the pixels.
    OutOfMemory,
}

impl core::fmt::Display for CanvasError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::BadSize => "the size is too large or empty",
            Self::BadPalette => "the palette cannot hold the picture",
            Self::OutOfMemory => "there is not enough memory",
        })
    }
}

impl From<OutOfMemory> for CanvasError {
    fn from(OutOfMemory: OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

/// How a canvas stores its pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// One index a pixel into `palette`, and one opacity a pixel when
    /// `masked`.
    Indexed {
        /// Bits an index occupies in the file.
        depth: IndexDepth,
        /// The colours, at most `depth.colours()` of them.
        palette: Vec<Rgba8>,
        /// Whether a mask plane accompanies the indices.
        masked: bool,
    },
    /// Four straight-alpha bytes a pixel.
    Rgba,
}

/// How a kind's pixels lie in a tile, which is all reading or writing one
/// needs: never the palette.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Planes {
    /// Four bytes a pixel.
    Rgba,
    /// An index a pixel, and an opacity a pixel when `masked`.
    Indexed {
        /// Whether the opacity plane is there.
        masked: bool,
    },
}

impl Kind {
    /// How this kind's pixels lie in a tile.
    #[must_use]
    pub const fn planes(&self) -> Planes {
        match self {
            Self::Indexed { masked, .. } => Planes::Indexed { masked: *masked },
            Self::Rgba => Planes::Rgba,
        }
    }

    /// Bytes one pixel's sample occupies.
    #[must_use]
    pub const fn sample_bytes(&self) -> usize {
        match self {
            Self::Indexed { .. } => 1,
            Self::Rgba => 4,
        }
    }

    /// Whether a mask plane accompanies the samples.
    #[must_use]
    pub const fn masked(&self) -> bool {
        matches!(self, Self::Indexed { masked: true, .. })
    }

    /// The palette, for an indexed kind.
    #[must_use]
    pub fn palette(&self) -> Option<&[Rgba8]> {
        match self {
            Self::Indexed { palette, .. } => Some(palette),
            Self::Rgba => None,
        }
    }

    /// The index depth, for an indexed kind.
    #[must_use]
    pub const fn depth(&self) -> Option<IndexDepth> {
        match self {
            Self::Indexed { depth, .. } => Some(*depth),
            Self::Rgba => None,
        }
    }

    /// Whether a pixel of this kind can be made transparent.
    #[must_use]
    pub const fn holds_transparency(&self) -> bool {
        matches!(self, Self::Rgba | Self::Indexed { masked: true, .. })
    }

    /// The colour `sample` shows as.
    #[must_use]
    pub fn colour(&self, sample: Sample) -> Rgba8 {
        match (self, sample) {
            (
                Self::Indexed {
                    palette, masked, ..
                },
                Sample::Index(index, alpha),
            ) => {
                let entry = palette.get(usize::from(index)).copied().unwrap_or([0; 4]);
                if *masked {
                    masked_colour(entry, alpha)
                } else {
                    entry
                }
            }
            (_, Sample::Rgba(colour)) => colour,
            (Self::Rgba, Sample::Index(..)) => [0; 4],
        }
    }

    /// Whether `sample` is one this kind stores.
    fn admits(&self, sample: Sample) -> bool {
        match (self, sample) {
            (
                Self::Indexed {
                    palette, masked, ..
                },
                Sample::Index(index, alpha),
            ) => usize::from(index) < palette.len() && (*masked || alpha == u8::MAX),
            (Self::Rgba, Sample::Rgba(_)) => true,
            _ => false,
        }
    }

    fn check(&self) -> Result<(), CanvasError> {
        match self {
            Self::Indexed { depth, palette, .. }
                if palette.is_empty() || palette.len() > depth.colours() =>
            {
                Err(CanvasError::BadPalette)
            }
            _ => Ok(()),
        }
    }
}

/// One pixel as a canvas stores it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Sample {
    /// A palette index, and how opaque the mask leaves it: 255 unmasked.
    Index(u8, u8),
    /// A colour.
    Rgba(Rgba8),
}

/// One tile's pixels, row-major across the tile's own width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tile {
    samples: Vec<u8>,
    mask: Vec<u8>,
}

impl Tile {
    /// `pixels` pixels of `kind`, each `fill`.
    fn filled(kind: &Kind, pixels: usize, fill: Sample) -> Result<Self, OutOfMemory> {
        let (samples, mask) = match fill {
            Sample::Index(index, alpha) => (
                fallible::filled(pixels, index).ok_or(OutOfMemory)?,
                if kind.masked() {
                    fallible::filled(pixels, alpha).ok_or(OutOfMemory)?
                } else {
                    Vec::new()
                },
            ),
            Sample::Rgba(colour) => {
                let mut samples = fallible::filled(pixels * 4, 0u8).ok_or(OutOfMemory)?;
                for pixel in samples.as_chunks_mut::<4>().0 {
                    *pixel = colour;
                }
                (samples, Vec::new())
            }
        };
        Ok(Self { samples, mask })
    }

    /// A copy the allocator may refuse.
    fn copy(&self) -> Result<Self, OutOfMemory> {
        let mut samples = Vec::new();
        let mut mask = Vec::new();
        if !fallible::reserve(&mut samples, self.samples.len())
            || !fallible::reserve(&mut mask, self.mask.len())
        {
            return Err(OutOfMemory);
        }
        samples.extend_from_slice(&self.samples);
        mask.extend_from_slice(&self.mask);
        Ok(Self { samples, mask })
    }

    /// The samples: an index or four RGBA bytes a pixel.
    #[must_use]
    pub fn samples(&self) -> &[u8] {
        &self.samples
    }

    /// The mask: one opacity a pixel, empty for an unmasked kind.
    #[must_use]
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    /// The samples and the mask, to write together.
    pub fn planes_mut(&mut self) -> (&mut [u8], &mut [u8]) {
        (&mut self.samples, &mut self.mask)
    }

    /// Bytes this tile holds.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.samples.len() + self.mask.len()
    }

    /// Pixel `at`, its planes laid out as `planes`.
    #[must_use]
    pub fn sample(&self, planes: Planes, at: usize) -> Sample {
        read_sample(&self.samples, &self.mask, planes, at)
    }
}

/// Pixel `at` of a tile's planes, laid out as `planes`; a pixel past them
/// reads as clear.
#[must_use]
pub fn read_sample(samples: &[u8], mask: &[u8], planes: Planes, at: usize) -> Sample {
    match planes {
        Planes::Rgba => match samples.get(at * 4..at * 4 + 4) {
            Some(&[r, g, b, a]) => Sample::Rgba([r, g, b, a]),
            _ => Sample::Rgba([0; 4]),
        },
        Planes::Indexed { masked } => Sample::Index(
            samples.get(at).copied().unwrap_or(0),
            if masked {
                mask.get(at).copied().unwrap_or(0)
            } else {
                u8::MAX
            },
        ),
    }
}

/// What one holder of `tile` is charged for it: its bytes shared out among
/// everything holding it, rounded up, so a sole holder pays the whole.
#[must_use]
pub fn charge(tile: &Arc<Tile>) -> usize {
    tile.bytes().div_ceil(Arc::strong_count(tile))
}

/// Where a tile sits: its first column and row and its extent.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TileRect {
    /// First column.
    pub x: u32,
    /// First row.
    pub y: u32,
    /// Columns it covers.
    pub width: u32,
    /// Rows it covers.
    pub height: u32,
}

impl TileRect {
    /// The pixels the tile covers.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        Bounds {
            x0: i64::from(self.x),
            y0: i64::from(self.y),
            x1: i64::from(self.x + self.width),
            y1: i64::from(self.y + self.height),
        }
    }
}

/// A picture held as tiles.
#[derive(Debug, Eq, PartialEq)]
pub struct Canvas {
    width: u32,
    height: u32,
    kind: Kind,
    tiles: Vec<Arc<Tile>>,
    across: u32,
}

/// Whether a `width`×`height` picture is one a canvas may hold.
#[must_use]
pub fn admissible(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SIDE
        && height <= MAX_SIDE
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

impl Canvas {
    /// A `width`×`height` canvas of `kind`, every pixel `fill`.
    ///
    /// Tiles of one extent are one shared tile until written, so a blank
    /// canvas costs at most four tiles of pixels whatever its size.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] for a size or palette out of bounds, or a refused
    /// allocation.
    pub fn new(width: u32, height: u32, kind: Kind, fill: Sample) -> Result<Self, CanvasError> {
        if !admissible(width, height) {
            return Err(CanvasError::BadSize);
        }
        kind.check()?;
        if !kind.admits(fill) {
            return Err(CanvasError::BadPalette);
        }
        let across = width.div_ceil(TILE);
        let down = height.div_ceil(TILE);
        let count = (across * down) as usize;
        let mut tiles = Vec::new();
        if !fallible::reserve(&mut tiles, count) {
            return Err(CanvasError::OutOfMemory);
        }
        // One tile per distinct extent: the full interior, the right column,
        // the bottom row and the corner.
        let mut shared: [Option<(u32, u32, Arc<Tile>)>; 4] = [None, None, None, None];
        for ty in 0..down {
            for tx in 0..across {
                let (w, h) = (edge(width, tx), edge(height, ty));
                let slot = usize::from(w != TILE) + 2 * usize::from(h != TILE);
                let tile = match &shared[slot] {
                    Some((sw, sh, tile)) if (*sw, *sh) == (w, h) => Arc::clone(tile),
                    _ => {
                        let tile = Arc::new(Tile::filled(&kind, (w * h) as usize, fill)?);
                        shared[slot] = Some((w, h, Arc::clone(&tile)));
                        tile
                    }
                };
                tiles.push(tile);
            }
        }
        Ok(Self {
            width,
            height,
            kind,
            tiles,
            across,
        })
    }

    /// A copy sharing every tile until one is written; the list of them is
    /// what grows with the picture, so its room is asked for, not assumed.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        let tiles =
            fallible::collected(self.tiles.len(), self.tiles.iter().cloned()).ok_or(OutOfMemory)?;
        Ok(Self {
            width: self.width,
            height: self.height,
            kind: self.kind.clone(),
            tiles,
            across: self.across,
        })
    }

    /// Width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How the pixels are stored.
    #[must_use]
    pub const fn kind(&self) -> &Kind {
        &self.kind
    }

    /// Replace the palette of an indexed canvas with `palette`, answering the
    /// one it had; `None`, changing nothing, for a canvas of another kind or a
    /// palette that could not index every pixel.
    pub fn swap_palette(&mut self, palette: Vec<Rgba8>) -> Option<Vec<Rgba8>> {
        let Kind::Indexed {
            depth,
            palette: held,
            ..
        } = &mut self.kind
        else {
            return None;
        };
        if palette.len() != held.len() || palette.len() > depth.colours() {
            return None;
        }
        Some(core::mem::replace(held, palette))
    }

    /// Give palette entry `index` the colour `colour`, in place; `false`,
    /// changing nothing, for a canvas of another kind or an entry it lacks.
    pub fn set_palette_entry(&mut self, index: u8, colour: Rgba8) -> bool {
        let Kind::Indexed { palette, .. } = &mut self.kind else {
            return false;
        };
        let Some(entry) = palette.get_mut(usize::from(index)) else {
            return false;
        };
        *entry = colour;
        true
    }

    /// How many tiles there are.
    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// The tile holding pixel `(x, y)`, which must lie on the canvas.
    #[must_use]
    pub const fn tile_index(&self, x: u32, y: u32) -> usize {
        ((y / TILE) * self.across + x / TILE) as usize
    }

    /// Where tile `index` sits.
    #[must_use]
    pub fn tile_rect(&self, index: usize) -> TileRect {
        // A grid is at most (MAX_SIDE / TILE)² tiles, far inside a u32.
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let tx = index % self.across;
        let ty = index / self.across;
        TileRect {
            x: tx * TILE,
            y: ty * TILE,
            width: edge(self.width, tx),
            height: edge(self.height, ty),
        }
    }

    /// Tile `index`.
    #[must_use]
    pub fn tile(&self, index: usize) -> &Arc<Tile> {
        &self.tiles[index]
    }

    /// Whether `tile` is shaped to stand at `index`: the samples, and the
    /// mask, that tile's pixels take in this canvas's kind.
    #[must_use]
    pub fn fits(&self, index: usize, tile: &Tile) -> bool {
        if index >= self.tiles.len() {
            return false;
        }
        let rect = self.tile_rect(index);
        let pixels = rect.width as usize * rect.height as usize;
        let mask = if self.kind.masked() { pixels } else { 0 };
        tile.samples.len() == pixels * self.kind.sample_bytes() && tile.mask.len() == mask
    }

    /// Tile `index`, to write: copied first where anything else holds it.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the copy is refused; the canvas is unchanged.
    pub fn tile_mut(&mut self, index: usize) -> Result<&mut Tile, OutOfMemory> {
        let slot = &mut self.tiles[index];
        if Arc::get_mut(slot).is_none() {
            *slot = Arc::new(slot.copy()?);
        }
        Arc::get_mut(slot).ok_or(OutOfMemory)
    }

    /// Put `tile` in place of tile `index`, answering the tile it replaces.
    ///
    /// `tile` must be one this canvas's tile `index` held, or a copy of it
    /// written since: the extent is not checked here but by construction.
    pub fn replace_tile(&mut self, index: usize, tile: Arc<Tile>) -> Arc<Tile> {
        core::mem::replace(&mut self.tiles[index], tile)
    }

    /// The pixel at `(x, y)`, or `None` off the canvas.
    #[must_use]
    pub fn sample(&self, x: u32, y: u32) -> Option<Sample> {
        if x >= self.width || y >= self.height {
            return None;
        }
        // The tile's column and row are the pixel's own over the tile side,
        // so a single pixel costs no division by the grid's width.
        let (tx, ty) = (x / TILE, y / TILE);
        let index = (ty * self.across + tx) as usize;
        let at = ((y - ty * TILE) * edge(self.width, tx) + (x - tx * TILE)) as usize;
        Some(self.tiles[index].sample(self.kind.planes(), at))
    }

    /// The colour `(x, y)` shows as, or `None` off the canvas.
    #[must_use]
    pub fn colour_at(&self, x: u32, y: u32) -> Option<Rgba8> {
        self.sample(x, y).map(|sample| self.kind.colour(sample))
    }

    /// The colours of `out.len()` pixels of row `y` from column `x`, as
    /// they show. Pixels off the canvas are left as they were.
    pub fn row_colours(&self, y: u32, x: u32, out: &mut [Rgba8]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let end = x.saturating_add(u32::try_from(out.len()).unwrap_or(u32::MAX));
        let end = end.min(self.width);
        let mut column = x;
        while column < end {
            let index = self.tile_index(column, y);
            let rect = self.tile_rect(index);
            let tile = &self.tiles[index];
            let run = (rect.x + rect.width).min(end) - column;
            let start = ((y - rect.y) * rect.width + (column - rect.x)) as usize;
            let into = &mut out[(column - x) as usize..(column - x + run) as usize];
            let (from, to) = (start, start + run as usize);
            let bytes = self.kind.sample_bytes();
            let mask = if self.kind.masked() {
                &tile.mask[from..to]
            } else {
                &[]
            };
            flatten_row(
                PictureSource::kind(self),
                &tile.samples[from * bytes..to * bytes],
                mask,
                into.as_flattened_mut(),
            );
            column += run;
        }
    }

    /// The samples of `out.len()` pixels of row `y` from column `x`. Pixels
    /// off the canvas are left as they were.
    pub fn row_samples(&self, y: u32, x: u32, out: &mut [Sample]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let end = x.saturating_add(u32::try_from(out.len()).unwrap_or(u32::MAX));
        let end = end.min(self.width);
        let planes = self.kind.planes();
        let mut column = x;
        while column < end {
            let index = self.tile_index(column, y);
            let rect = self.tile_rect(index);
            let tile = &self.tiles[index];
            let run = (rect.x + rect.width).min(end) - column;
            let start = ((y - rect.y) * rect.width + (column - rect.x)) as usize;
            let into = &mut out[(column - x) as usize..(column - x + run) as usize];
            for (at, slot) in into.iter_mut().enumerate() {
                *slot = tile.sample(planes, start + at);
            }
            column += run;
        }
    }

    /// The samples of `out.len()` pixels of column `x` from row `y` down,
    /// each tile looked up once for the run of rows it holds. Pixels off the
    /// canvas are left as they were.
    pub fn column_samples(&self, x: u32, y: u32, out: &mut [Sample]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let end = y.saturating_add(u32::try_from(out.len()).unwrap_or(u32::MAX));
        let end = end.min(self.height);
        let planes = self.kind.planes();
        let mut row = y;
        while row < end {
            let index = self.tile_index(x, row);
            let rect = self.tile_rect(index);
            let tile = &self.tiles[index];
            let run = (rect.y + rect.height).min(end) - row;
            let start = ((row - rect.y) * rect.width + (x - rect.x)) as usize;
            let into = &mut out[(row - y) as usize..(row - y + run) as usize];
            for (step, slot) in into.iter_mut().enumerate() {
                *slot = tile.sample(planes, start + step * rect.width as usize);
            }
            row += run;
        }
    }

    /// Bytes the pixels occupy, shared tiles counted each time they appear.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let pixels = self.width as usize * self.height as usize;
        pixels * (self.kind.sample_bytes() + usize::from(self.kind.masked()))
    }

    /// Bytes this canvas is charged for its tiles: each one's shared out
    /// among everything holding it, so what several hold is charged to them
    /// together and never to none.
    #[must_use]
    pub fn charged_bytes(&self) -> usize {
        self.tiles.iter().map(charge).sum()
    }

    /// Whether any pixel is less than opaque.
    #[must_use]
    pub fn has_transparency(&self) -> bool {
        match &self.kind {
            Kind::Rgba => self
                .tiles
                .iter()
                .any(|tile| tile.samples.as_chunks::<4>().0.iter().any(|p| p[3] != 255)),
            Kind::Indexed {
                palette, masked, ..
            } => {
                let mut clear = [false; 256];
                for (slot, entry) in clear.iter_mut().zip(palette) {
                    *slot = entry[3] != 255;
                }
                let any_clear = clear.contains(&true);
                self.tiles.iter().any(|tile| {
                    (*masked && tile.mask.iter().any(|&m| m != 255))
                        || (any_clear
                            && tile.samples.iter().any(|&index| clear[usize::from(index)]))
                })
            }
        }
    }

    /// Whether any pixel is partly transparent, rather than wholly opaque or
    /// wholly clear.
    #[must_use]
    pub fn has_partial_alpha(&self) -> bool {
        let partial = |value: u8| value != 0 && value != 255;
        match &self.kind {
            Kind::Rgba => self.tiles.iter().any(|tile| {
                tile.samples
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| partial(p[3]))
            }),
            // What a palette pixel shows is its entry under its mask, so a
            // translucent entry counts as much as a soft mask.
            Kind::Indexed { .. } => {
                let planes = self.kind.planes();
                self.tiles.iter().any(|tile| {
                    (0..tile.samples.len())
                        .any(|at| partial(self.kind.colour(tile.sample(planes, at))[3]))
                })
            }
        }
    }
}

/// The extent of the tile at grid position `at` along a side `length` long.
const fn edge(length: u32, at: u32) -> u32 {
    let start = at * TILE;
    if length - start < TILE {
        length - start
    } else {
        TILE
    }
}

/// Write `sample` as pixel `at` of a tile's planes; one the planes cannot
/// store is left unwritten.
pub fn write_sample(samples: &mut [u8], mask: &mut [u8], at: usize, sample: Sample) {
    match sample {
        Sample::Rgba(colour) => {
            if let Some(pixel) = samples.get_mut(at * 4..at * 4 + 4) {
                pixel.copy_from_slice(&colour);
            }
        }
        Sample::Index(index, alpha) => {
            if let Some(slot) = samples.get_mut(at) {
                *slot = index;
            }
            if let Some(slot) = mask.get_mut(at) {
                *slot = alpha;
            }
        }
    }
}

impl PictureSource for Canvas {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn kind(&self) -> PictureKind<'_> {
        match &self.kind {
            Kind::Indexed {
                depth,
                palette,
                masked,
            } => PictureKind::Indexed {
                depth: *depth,
                palette,
                masked: *masked,
            },
            Kind::Rgba => PictureKind::Rgba,
        }
    }

    fn read_row(&self, y: u32, samples: &mut [u8], mask: &mut [u8]) {
        if y >= self.height {
            return;
        }
        let bytes = self.kind.sample_bytes();
        let mut column = 0;
        while column < self.width {
            let index = self.tile_index(column, y);
            let rect = self.tile_rect(index);
            let tile = &self.tiles[index];
            let start = ((y - rect.y) * rect.width) as usize;
            let run = rect.width as usize;
            let at = column as usize;
            if let (Some(into), Some(from)) = (
                samples.get_mut(at * bytes..(at + run) * bytes),
                tile.samples.get(start * bytes..(start + run) * bytes),
            ) {
                into.copy_from_slice(from);
            }
            if let (Some(into), Some(from)) = (
                mask.get_mut(at..at + run),
                tile.mask.get(start..start + run),
            ) {
                into.copy_from_slice(from);
            }
            column += rect.width;
        }
    }
}

/// A canvas being filled a row at a time, top to bottom: how a decoded
/// picture and a transform's result are built.
#[derive(Debug)]
pub struct CanvasBuilder {
    canvas: Canvas,
    /// Where rows may land: the picture, or the part of it built within.
    area: Bounds,
}

impl CanvasBuilder {
    /// A builder of a `width`×`height` canvas of `kind`, its pixels written
    /// by [`row`](Self::row); rows never written stay `fill`.
    ///
    /// # Errors
    ///
    /// [`CanvasError`], as [`Canvas::new`].
    pub fn new(width: u32, height: u32, kind: Kind, fill: Sample) -> Result<Self, CanvasError> {
        Self::within(width, height, kind, fill, Bounds::picture(width, height))
    }

    /// A builder as [`new`](Self::new) whose rows land within `area` alone:
    /// the tiles beyond it stay one shared `fill`, so a layer smaller than its
    /// picture costs what it covers.
    ///
    /// # Errors
    ///
    /// [`CanvasError`], as [`Canvas::new`].
    pub fn within(
        width: u32,
        height: u32,
        kind: Kind,
        fill: Sample,
        area: Bounds,
    ) -> Result<Self, CanvasError> {
        let mut canvas = Canvas::new(width, height, kind, fill)?;
        let area = area.intersection(&Bounds::picture(width, height));
        // Every tile written gets its own pixels now, so writing rows never
        // copies.
        for index in 0..canvas.tile_count() {
            if !canvas
                .tile_rect(index)
                .bounds()
                .intersection(&area)
                .is_empty()
            {
                canvas.tile_mut(index)?;
            }
        }
        Ok(Self { canvas, area })
    }

    /// The kind being built.
    #[must_use]
    pub const fn kind(&self) -> &Kind {
        &self.canvas.kind
    }

    /// Write row `y`: `samples` as [`PictureSource::read_row`] lays them out,
    /// and `mask` one opacity a pixel for a masked kind. A row off the canvas,
    /// or planes of the wrong length, is ignored.
    pub fn row(&mut self, y: u32, samples: &[u8], mask: &[u8]) {
        if samples.len() == self.canvas.width as usize * self.canvas.kind.sample_bytes() {
            self.row_at((0, i64::from(y)), samples, mask);
        }
    }

    /// Write a row laid out as [`row`](Self::row) takes one, of any width,
    /// its first pixel at `(x, y)`: what falls off the canvas, or outside the
    /// area it was built [`within`](Self::within), is dropped, and planes
    /// that disagree in length are ignored.
    pub fn row_at(&mut self, (x, y): (i64, i64), samples: &[u8], mask: &[u8]) {
        let canvas = &mut self.canvas;
        let area = self.area;
        let bytes = canvas.kind.sample_bytes();
        let width = samples.len() / bytes;
        let planes_agree = samples.len().is_multiple_of(bytes)
            && mask.len() == if canvas.kind.masked() { width } else { 0 };
        if !planes_agree || !(area.y0..area.y1).contains(&y) {
            return;
        }
        let run_end = x.saturating_add(i64::try_from(width).unwrap_or(i64::MAX));
        let (Ok(y), Ok(mut column), Ok(end)) = (
            u32::try_from(y),
            u32::try_from(x.max(area.x0)),
            u32::try_from(run_end.min(area.x1)),
        ) else {
            return;
        };
        while column < end {
            let index = canvas.tile_index(column, y);
            let rect = canvas.tile_rect(index);
            let stop = (rect.x + rect.width).min(end);
            let run = (stop - column) as usize;
            let into = ((y - rect.y) * rect.width + (column - rect.x)) as usize;
            let from = usize::try_from(i64::from(column) - x).unwrap_or(usize::MAX);
            if let (Some(tile), Some(source)) = (
                Arc::get_mut(&mut canvas.tiles[index]),
                samples.get(from * bytes..(from + run) * bytes),
            ) {
                tile.samples[into * bytes..(into + run) * bytes].copy_from_slice(source);
                if let Some(source) = mask.get(from..from + run).filter(|_| !tile.mask.is_empty()) {
                    tile.mask[into..into + run].copy_from_slice(source);
                }
            }
            column = stop;
        }
    }

    /// Write row `y` from `samples`, one a pixel from the left, a tile's run
    /// at a time; a row off the canvas, or one of another width, is ignored.
    pub fn set_row(&mut self, y: u32, samples: &[Sample]) {
        let canvas = &mut self.canvas;
        if y >= canvas.height || samples.len() != canvas.width as usize {
            return;
        }
        let mut column = 0;
        while column < canvas.width {
            let index = canvas.tile_index(column, y);
            let rect = canvas.tile_rect(index);
            let start = ((y - rect.y) * rect.width) as usize;
            let run = &samples[column as usize..(column + rect.width) as usize];
            if let Some(tile) = Arc::get_mut(&mut canvas.tiles[index]) {
                for (at, sample) in (start..).zip(run) {
                    write_sample(&mut tile.samples, &mut tile.mask, at, *sample);
                }
            }
            column += rect.width;
        }
    }

    /// Set pixel `(x, y)`; one off the canvas is ignored.
    pub fn set(&mut self, x: u32, y: u32, sample: Sample) {
        let canvas = &mut self.canvas;
        if x >= canvas.width || y >= canvas.height {
            return;
        }
        let index = canvas.tile_index(x, y);
        let rect = canvas.tile_rect(index);
        let at = ((y - rect.y) * rect.width + (x - rect.x)) as usize;
        if let Some(tile) = Arc::get_mut(&mut canvas.tiles[index]) {
            write_sample(&mut tile.samples, &mut tile.mask, at, sample);
        }
    }

    /// The canvas built.
    #[must_use]
    pub fn finish(self) -> Canvas {
        self.canvas
    }
}

#[cfg(test)]
#[path = "canvas_tests.rs"]
mod tests;
