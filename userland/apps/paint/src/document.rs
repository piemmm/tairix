//! A document: the pictures one file holds, and what the file says about
//! them.
//!
//! A PNG, a JPEG, a GIF or a BMP holds one picture; an OpenRaster file one
//! picture of layers; a TIFF holds any number of pages; a RISC OS sprite area
//! holds any number of sprites, each with its name, mode and palette, and
//! keeps the sprites no editor here can read as their exact bytes so that
//! saving writes them back unchanged. Every change goes through here, so each
//! is recorded as a step of the history.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::{
    desktop_palette, Density, IndexDepth, Rgba8, SpriteMode, SpriteName, SpritePalette, Unkept,
};
use tairix_reclaim::PressureBand;
use tairix_sandbox::imageedit::{KeptReason, MAX_LAYER_NAME};
use tairix_sandbox::imagerender::ViewFormat;

use crate::canvas::{Canvas, CanvasError, Kind, OutOfMemory, Sample, Tile};
use crate::colour::{nearest, WHITE};
use crate::history::{Applied, Damage, History, Step, Unapplied};
use crate::save::{SaveFormat, SaveSettings};

/// A picture to begin.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NewPicture {
    /// Its size.
    pub size: (u32, u32),
    /// Its depth, `None` for colour.
    pub depth: Option<IndexDepth>,
    /// Whether its background is clear rather than white.
    pub transparent: bool,
}

impl NewPicture {
    /// A white picture of millions of colours at the size new ones take.
    pub const DEFAULT: Self = Self {
        size: (640, 480),
        depth: None,
        transparent: false,
    };

    /// The blank picture: white, or clear; a palette picture takes the
    /// desktop's colours for its depth.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] where it cannot be held.
    pub fn canvas(&self) -> Result<Canvas, CanvasError> {
        let (width, height) = self.size;
        let Some(depth) = self.depth else {
            let fill = if self.transparent { [0; 4] } else { WHITE };
            return Canvas::new(width, height, Kind::Rgba, Sample::Rgba(fill));
        };
        let colours = desktop_palette(depth);
        let palette = tairix_util::fallible::collected(
            colours.len(),
            colours.iter().map(|&[r, g, b]| [r, g, b, 255]),
        )
        .ok_or(CanvasError::OutOfMemory)?;
        let white = nearest(&palette, WHITE);
        let fill = Sample::Index(white, if self.transparent { 0 } else { u8::MAX });
        let kind = Kind::Indexed {
            depth,
            palette,
            masked: self.transparent,
        };
        Canvas::new(width, height, kind, fill)
    }
}

/// What a sprite says about itself beyond its pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteInfo {
    /// Its name.
    pub name: SpriteName,
    /// Its mode, which decides its depth and the shape of its pixels.
    pub mode: SpriteMode,
    /// How its colours are stated.
    pub palette: SpritePalette,
    /// Whether a direct-colour sprite carries a mask. A palette sprite's
    /// mask is its canvas's own.
    pub masked: bool,
}

impl SpriteInfo {
    /// Details for `canvas` as a sprite called `name`, which has none of its
    /// own: square pixels, the depth it has, and its colours stated as they
    /// are.
    #[must_use]
    pub fn for_canvas(name: SpriteName, canvas: &Canvas) -> Self {
        let mode = match canvas.kind().depth() {
            Some(depth) => SpriteMode::indexed(depth, (1, 1), false),
            None => SpriteMode::truecolour((1, 1), false),
        };
        Self {
            name,
            mode,
            palette: crate::save::restated(canvas.kind()),
            masked: false,
        }
    }

    /// These details made true of `canvas`, the picture `transform` made of
    /// the one they described.
    ///
    /// A new depth takes a mode of that depth and restates the palette; a
    /// quarter turn swaps the pixels' shape, where a mode of the same layout
    /// can say so.
    #[must_use]
    pub fn refit(&self, transform: crate::transform::Transform, canvas: &Canvas) -> Self {
        use crate::transform::{Transform, Turn};
        use tairix_image::SpriteLayout;
        let (x, y) = self.mode.eig();
        let eig = match transform {
            Transform::Turn(Turn::Quarter | Turn::ThreeQuarters) => (y, x),
            _ => (x, y),
        };
        let alpha = self.mode.alpha_mask();
        let mode = match (canvas.kind().depth(), self.mode.layout()) {
            (Some(depth), SpriteLayout::Indexed(held)) if held == depth => self.mode.with_eig(eig),
            (Some(depth), _) => SpriteMode::indexed(depth, eig, alpha),
            (None, SpriteLayout::Direct { .. }) => self.mode.with_eig(eig),
            (None, SpriteLayout::Indexed(_)) => SpriteMode::truecolour(eig, alpha),
        };
        let palette = if matches!(transform, Transform::Convert { .. }) {
            crate::save::restated(canvas.kind())
        } else {
            self.palette.clone()
        };
        let masked = match canvas.kind() {
            crate::canvas::Kind::Rgba => self.masked,
            crate::canvas::Kind::Indexed { masked, .. } => *masked,
        };
        Self {
            name: self.name,
            mode,
            palette,
            masked,
        }
    }
}

/// The most layers one picture holds: what OpenRaster is read with.
pub const MOST_LAYERS: usize = tairix_image::MOST_ORA_LAYERS;

/// What a picture's first layer is called.
pub const BACKGROUND: &str = "Background";

/// One layer of a picture: its pixels, what it is called, and how it lies
/// over the layers beneath it.
#[derive(Debug, Eq, PartialEq)]
pub struct Layer {
    /// Its pixels.
    pub canvas: Canvas,
    /// What it is called.
    pub name: String,
    /// How much of it shows, out of 255.
    pub opacity: u8,
    /// Whether it shows at all.
    pub visible: bool,
}

impl Layer {
    /// A layer of `canvas` called `name`, wholly showing.
    #[must_use]
    pub const fn new(canvas: Canvas, name: String) -> Self {
        Self {
            canvas,
            name,
            opacity: u8::MAX,
            visible: true,
        }
    }

    /// A copy sharing its pixels until either is written.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        let mut name = String::new();
        name.try_reserve_exact(self.name.len())
            .map_err(|_| OutOfMemory)?;
        name.push_str(&self.name);
        Ok(Self {
            canvas: self.canvas.try_clone()?,
            name,
            opacity: self.opacity,
            visible: self.visible,
        })
    }

    /// Whether it lays nothing over what is beneath and changes nothing it
    /// covers: shown wholly, its own pixels all that show.
    fn plain(&self) -> bool {
        self.visible && self.opacity == u8::MAX
    }

    /// Whether anything of it shows: shown, and not wholly faint.
    #[must_use]
    pub const fn shows(&self) -> bool {
        self.visible && self.opacity > 0
    }
}

/// One picture of a document: one layer or more, the bottom first, one of
/// them the layer painting lands on.
///
/// Every layer is the picture's size and kind; a palette picture, whose
/// pixels are entries of one palette, holds one.
#[derive(Debug, Eq, PartialEq)]
pub struct Picture {
    layers: Vec<Layer>,
    active: usize,
    /// Its sprite details, when it is, or is to be, a sprite.
    pub sprite: Option<SpriteInfo>,
    /// How densely its pixels are laid out, where its file stated it.
    pub density: Option<Density>,
}

impl Picture {
    /// A picture of one layer, `canvas`, that is not a sprite.
    #[must_use]
    pub fn plain(canvas: Canvas) -> Self {
        Self {
            layers: alloc::vec![Layer::new(canvas, String::from(BACKGROUND))],
            active: 0,
            sprite: None,
            density: None,
        }
    }

    /// A picture of `layers`, the bottom first, painting landing on layer
    /// `active`; `None` for none, more than [`MOST_LAYERS`], layers of
    /// different sizes or kinds or named past `MAX_LAYER_NAME`, more than one
    /// of a palette, or an active layer past the last.
    #[must_use]
    pub fn layered(layers: Vec<Layer>, active: usize) -> Option<Self> {
        let first = layers.first()?;
        let shape = (
            first.canvas.width(),
            first.canvas.height(),
            first.canvas.kind(),
        );
        let alike = layers.iter().all(|layer| {
            let own = (
                layer.canvas.width(),
                layer.canvas.height(),
                layer.canvas.kind(),
            );
            own == shape && layer.name.len() <= MAX_LAYER_NAME
        });
        let palette = shape.2.palette().is_some();
        let fits = layers.len() <= MOST_LAYERS && (!palette || layers.len() == 1);
        (alike && fits && active < layers.len()).then_some(Self {
            layers,
            active,
            sprite: None,
            density: None,
        })
    }

    /// The pixels painting lands on: the active layer's.
    #[must_use]
    pub fn canvas(&self) -> &Canvas {
        &self.layers[self.active].canvas
    }

    /// The pixels painting lands on, to write.
    pub fn canvas_mut(&mut self) -> &mut Canvas {
        &mut self.layers[self.active].canvas
    }

    /// The layers, the bottom first.
    #[must_use]
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The layers, to change their pixels or how they show.
    pub(crate) fn layers_mut(&mut self) -> &mut [Layer] {
        &mut self.layers
    }

    /// Which layer painting lands on.
    #[must_use]
    pub const fn active(&self) -> usize {
        self.active
    }

    /// Paint on layer `index`, answering whether it exists.
    pub fn set_active(&mut self, index: usize) -> bool {
        let exists = index < self.layers.len();
        if exists {
            self.active = index;
        }
        exists
    }

    /// The picture's size.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.canvas().width(), self.canvas().height())
    }

    /// What its pixels are: every layer's alike.
    #[must_use]
    pub fn kind(&self) -> &Kind {
        self.canvas().kind()
    }

    /// The same picture's metadata — layer names, how each shows, the active
    /// layer, sprite details, density — over `canvases`, one a layer;
    /// `None` where they are not one a layer, alike in size and kind.
    #[must_use]
    pub fn with_canvases(&self, canvases: Vec<Canvas>) -> Option<Self> {
        if canvases.len() != self.layers.len() {
            return None;
        }
        let mut layers = Vec::new();
        layers.try_reserve_exact(canvases.len()).ok()?;
        for (canvas, held) in canvases.into_iter().zip(&self.layers) {
            let mut name = String::new();
            name.try_reserve_exact(held.name.len()).ok()?;
            name.push_str(&held.name);
            layers.push(Layer {
                canvas,
                name,
                opacity: held.opacity,
                visible: held.visible,
            });
        }
        self.with_layers(layers, self.active)
    }

    /// The same picture with `layers` in place of its own and layer `active`
    /// painted on.
    #[must_use]
    pub fn with_layers(&self, layers: Vec<Layer>, active: usize) -> Option<Self> {
        let mut picture = Self::layered(layers, active)?;
        picture.sprite.clone_from(&self.sprite);
        picture.density = self.density;
        Some(picture)
    }

    /// A copy sharing its pixels until either is written.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        let mut layers = Vec::new();
        layers
            .try_reserve_exact(self.layers.len())
            .map_err(|_| OutOfMemory)?;
        for layer in &self.layers {
            layers.push(layer.try_clone()?);
        }
        Ok(Self {
            layers,
            active: self.active,
            sprite: self.sprite.clone(),
            density: self.density,
        })
    }

    /// Whether something of every layer of `range` shows; `false` where one
    /// is missing.
    #[must_use]
    pub fn shows(&self, range: core::ops::Range<usize>) -> bool {
        self.layers
            .get(range)
            .is_some_and(|layers| layers.iter().all(Layer::shows))
    }

    /// Whether it shows exactly its one layer's pixels, so its layers need
    /// no composing.
    #[must_use]
    pub fn single(&self) -> bool {
        self.layers.len() == 1 && self.layers[0].plain()
    }

    /// Bytes its layers are charged: what they share, shared out.
    pub(crate) fn charged_bytes(&self) -> usize {
        self.layers
            .iter()
            .map(|layer| layer.canvas.charged_bytes())
            .sum()
    }

    /// Bytes its layers occupy.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.layers.iter().map(|layer| layer.canvas.bytes()).sum()
    }

    /// A pixel's shape, width to height: square unless the sprite's mode
    /// says otherwise.
    #[must_use]
    pub fn pixel_aspect(&self) -> (u32, u32) {
        self.sprite
            .as_ref()
            .map_or((1, 1), |sprite| sprite.mode.pixel_aspect())
    }

    /// The colour the layers show together at `(x, y)`, the layer painted
    /// on showing `active` there.
    #[must_use]
    pub fn shown_at(&self, (x, y): (u32, u32), active: Rgba8) -> Rgba8 {
        if self.single() {
            return active;
        }
        let (mut out, mut scratch) = ([[0; 4]], [[0; 4]]);
        let shown = Some((self.active, core::slice::from_ref(&active)));
        crate::compose::compose_run(
            &self.layers,
            shown,
            &mut out,
            &mut scratch,
            |canvas, into| {
                into[0] = canvas.colour_at(x, y).unwrap_or([0; 4]);
            },
        );
        out[0]
    }

    /// The picture as its layers show together, as one canvas.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the canvas cannot be had.
    pub fn flattened(&self) -> Result<Canvas, OutOfMemory> {
        crate::compose::flatten(&self.layers)
    }

    /// Why `layer` could not join the stack at `index`, if it could not.
    fn refuses(&self, index: usize, layer: &Layer) -> Option<LayerRefusal> {
        let shape = (self.size(), self.kind());
        let alike = (
            (layer.canvas.width(), layer.canvas.height()),
            layer.canvas.kind(),
        ) == shape;
        if index > self.layers.len() {
            Some(LayerRefusal::NoSuchLayer)
        } else if self.kind().palette().is_some() {
            Some(LayerRefusal::Palette)
        } else if self.layers.len() >= MOST_LAYERS {
            Some(LayerRefusal::Full)
        } else if !alike || layer.name.len() > MAX_LAYER_NAME {
            Some(LayerRefusal::Unlike)
        } else {
            None
        }
    }

    /// Hold the room one more layer takes, so adding it cannot then fail for
    /// memory.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub(crate) fn reserve_layer(&mut self) -> Result<(), OutOfMemory> {
        self.layers.try_reserve(1).map_err(|_| OutOfMemory)
    }

    /// Add `layer` at `index` and paint on it.
    ///
    /// # Errors
    ///
    /// The [`LayerRefusal`], `layer` handed back; nothing changed.
    pub(crate) fn insert_layer(
        &mut self,
        index: usize,
        layer: Layer,
    ) -> Result<(), (LayerRefusal, Layer)> {
        if let Some(refusal) = self.refuses(index, &layer) {
            return Err((refusal, layer));
        }
        if self.reserve_layer().is_err() {
            return Err((LayerRefusal::OutOfMemory, layer));
        }
        self.layers.insert(index, layer);
        self.active = index;
        Ok(())
    }

    /// Take layer `index` out, painting on the one that takes its place.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]: no such layer, or the last.
    pub(crate) fn remove_layer(&mut self, index: usize) -> Result<Layer, LayerRefusal> {
        if index >= self.layers.len() {
            return Err(LayerRefusal::NoSuchLayer);
        }
        if self.layers.len() == 1 {
            return Err(LayerRefusal::LastLayer);
        }
        let layer = self.layers.remove(index);
        self.active = index.min(self.layers.len() - 1);
        Ok(layer)
    }

    /// Move layer `from` to `to`, painting on it there.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal::NoSuchLayer`] where either is past the last.
    pub(crate) fn move_layer(&mut self, from: usize, to: usize) -> Result<(), LayerRefusal> {
        if from >= self.layers.len() || to >= self.layers.len() {
            return Err(LayerRefusal::NoSuchLayer);
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
        self.active = to;
        Ok(())
    }

    /// Show layer `index` as `shown` says, painting on it, answering how it
    /// showed before.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]: no such layer, a name too long, or a palette
    /// picture's layer shown other than wholly.
    pub(crate) fn show_layer(&mut self, index: usize, shown: Shown) -> Result<Shown, LayerRefusal> {
        let palette = self.kind().palette().is_some();
        let layer = self
            .layers
            .get_mut(index)
            .ok_or(LayerRefusal::NoSuchLayer)?;
        if shown.name.len() > MAX_LAYER_NAME {
            return Err(LayerRefusal::Unlike);
        }
        if palette && (shown.opacity, shown.visible) != (u8::MAX, true) {
            return Err(LayerRefusal::Palette);
        }
        self.active = index;
        Ok(Shown {
            name: core::mem::replace(&mut layer.name, shown.name),
            opacity: core::mem::replace(&mut layer.opacity, shown.opacity),
            visible: core::mem::replace(&mut layer.visible, shown.visible),
        })
    }
}

/// How a layer shows, beside its pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shown {
    /// What it is called.
    pub name: String,
    /// How much of it shows, out of 255.
    pub opacity: u8,
    /// Whether it shows at all.
    pub visible: bool,
}

impl Shown {
    /// How `layer` shows now.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when its name cannot be copied.
    pub fn of(layer: &Layer) -> Result<Self, OutOfMemory> {
        let mut name = String::new();
        name.try_reserve_exact(layer.name.len())
            .map_err(|_| OutOfMemory)?;
        name.push_str(&layer.name);
        Ok(Self {
            name,
            opacity: layer.opacity,
            visible: layer.visible,
        })
    }
}

/// Why a picture's layers could not be changed so.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayerRefusal {
    /// A picture always holds one layer.
    LastLayer,
    /// There is no such layer.
    NoSuchLayer,
    /// A picture holds at most [`MOST_LAYERS`] layers.
    Full,
    /// A palette picture holds one layer, shown wholly.
    Palette,
    /// The layer is not the picture's size or kind, or its name is longer
    /// than a layer's may be.
    Unlike,
    /// The allocator refused the room.
    OutOfMemory,
}

impl core::fmt::Display for LayerRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LastLayer => f.write_str("A picture keeps at least one layer"),
            Self::NoSuchLayer => f.write_str("There is no such layer"),
            Self::Full => write!(f, "A picture holds at most {MOST_LAYERS} layers"),
            Self::Palette => f.write_str(
                "A palette picture holds one layer: convert it to millions of colours first",
            ),
            Self::Unlike => write!(f, "A layer's name holds at most {MAX_LAYER_NAME} bytes"),
            Self::OutOfMemory => f.write_str("There is not enough memory to change the layers"),
        }
    }
}

/// A name no layer of `layers` has: `Layer` and the least number from
/// their count up that makes it so.
///
/// # Errors
///
/// [`OutOfMemory`] when the name cannot be held.
pub fn new_layer_name(layers: &[Layer]) -> Result<String, OutOfMemory> {
    // Of the count + 1 numbers from there, at most count are taken.
    let first = layers.len() + 1;
    let number = (first..=first + layers.len())
        .find(|&number| {
            !layers.iter().any(|layer| {
                layer
                    .name
                    .strip_prefix("Layer ")
                    .is_some_and(|rest| rest.parse() == Ok(number))
            })
        })
        .unwrap_or(first);
    let mut name = String::new();
    name.try_reserve_exact(20).map_err(|_| OutOfMemory)?;
    let _ = core::fmt::write(&mut name, format_args!("Layer {number}"));
    Ok(name)
}

/// `name` with ` copy` after it, cut at a character's boundary to the
/// longest a layer's name may be.
///
/// # Errors
///
/// [`OutOfMemory`] when the name cannot be held.
pub fn copy_name(name: &str) -> Result<String, OutOfMemory> {
    const COPY: &str = " copy";
    let mut keep = name.len().min(MAX_LAYER_NAME - COPY.len());
    while !name.is_char_boundary(keep) {
        keep -= 1;
    }
    let mut copy = String::new();
    copy.try_reserve_exact(keep + COPY.len())
        .map_err(|_| OutOfMemory)?;
    copy.push_str(&name[..keep]);
    copy.push_str(COPY);
    Ok(copy)
}

/// A sprite kept as its bytes, because it cannot be edited here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Kept {
    /// Its name.
    pub name: SpriteName,
    /// Why it cannot be edited.
    pub reason: KeptReason,
    /// Its bytes, exactly as the file held them.
    pub bytes: Arc<Vec<u8>>,
}

/// One entry of a document.
#[derive(Debug, Eq, PartialEq)]
pub enum Entry {
    /// A picture.
    Picture(Picture),
    /// A sprite kept as its bytes.
    Kept(Kept),
}

impl Entry {
    /// A copy sharing its pixels until either is written.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        Ok(match self {
            Self::Picture(picture) => Self::Picture(picture.try_clone()?),
            Self::Kept(kept) => Self::Kept(kept.clone()),
        })
    }

    /// Bytes this entry is charged: what it shares, shared out among its
    /// holders.
    pub(crate) fn charged_bytes(&self) -> usize {
        match self {
            Self::Picture(picture) => picture.charged_bytes(),
            Self::Kept(kept) => kept.bytes.len().div_ceil(Arc::strong_count(&kept.bytes)),
        }
    }

    /// Bytes the entry occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        match self {
            Self::Picture(picture) => picture.bytes(),
            Self::Kept(kept) => kept.bytes.len(),
        }
    }

    /// Its sprite name, when it has one.
    #[must_use]
    pub fn name(&self) -> Option<&SpriteName> {
        match self {
            Self::Picture(picture) => picture.sprite.as_ref().map(|sprite| &sprite.name),
            Self::Kept(kept) => Some(&kept.name),
        }
    }

    /// The picture, for an entry that is one.
    #[must_use]
    pub const fn picture(&self) -> Option<&Picture> {
        match self {
            Self::Picture(picture) => Some(picture),
            Self::Kept(_) => None,
        }
    }
}

/// Whether `entries`, read from `origin`, are a sprite area: read from or
/// made as one, holding a sprite's name, or several pictures of a document
/// that is not a TIFF's pages.
#[must_use]
pub fn sprite_area(entries: &[Entry], origin: Origin) -> bool {
    if matches!(
        origin,
        Origin::Read(ViewFormat::Sprite) | Origin::New(SaveFormat::Sprites)
    ) || entries.iter().any(|entry| entry.name().is_some())
    {
        return true;
    }
    !origin.is_pages() && entries.len() > 1
}

/// Whether `entries`, read from `origin`, are a TIFF's pages.
#[must_use]
pub fn pages(entries: &[Entry], origin: Origin) -> bool {
    origin.is_pages() && !sprite_area(entries, origin)
}

/// What a document was read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Nothing: it was made here, to be saved as this format.
    New(SaveFormat),
    /// A file of this format.
    Read(ViewFormat),
}

impl Origin {
    /// Whether a document of this origin is a TIFF's pages.
    #[must_use]
    pub const fn is_pages(self) -> bool {
        matches!(
            self,
            Self::Read(ViewFormat::Tiff) | Self::New(SaveFormat::Tiff)
        )
    }
}

/// A document frozen for a save, sharing its pixels with the live one.
#[derive(Debug)]
pub struct Snapshot {
    /// Its entries.
    pub entries: Vec<Entry>,
    /// The entry showing when it was frozen.
    pub current: usize,
    /// What it was read from.
    pub origin: Origin,
    /// How each format that offers a choice is written.
    pub settings: SaveSettings,
}

/// Why a change to the list of entries was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ListRefusal {
    /// A document always holds one entry.
    LastEntry,
    /// There is no such entry.
    NoSuchEntry,
    /// A document holds at most this many entries.
    Full,
    /// The allocator refused the room.
    OutOfMemory,
}

impl core::fmt::Display for ListRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LastEntry => f.write_str("A sprite file keeps at least one sprite"),
            Self::NoSuchEntry => f.write_str("There is no such sprite"),
            Self::Full => write!(f, "A sprite file here holds at most {MAX_ENTRIES} sprites"),
            Self::OutOfMemory => f.write_str("There is not enough memory to change the sprites"),
        }
    }
}

/// Most entries a document holds: what a sprite area opened for editing may.
pub const MAX_ENTRIES: usize = tairix_sandbox::imageedit::MAX_EDIT_ENTRIES as usize;

/// The name a sprite takes when nothing better is to hand.
pub const SPRITE_STEM: &str = "sprite";

/// Why a name typed for a sprite was refused.
pub const NAME_REFUSAL: &str =
    "A sprite's name is one to twelve letters, digits or symbols, with no spaces";

/// A name from `base` that matches none of `taken`: `base` itself when that
/// is free, else `base` cut short to make room for the least number that is.
/// An empty `base` stands for [`SPRITE_STEM`].
///
/// One pass over `taken` marks the numbers already standing for `base`, so
/// the cost is the names held, not the names held times the numbers tried.
/// `None` only when `taken` holds more names than a document can.
#[must_use]
pub fn free_name<'a>(
    base: &SpriteName,
    taken: impl IntoIterator<Item = &'a SpriteName>,
) -> Option<SpriteName> {
    const NUMBERS: usize = MAX_ENTRIES + 1;
    const DIGITS: usize = NUMBERS.ilog10() as usize + 1;
    let base = match base.as_bytes() {
        [] => SPRITE_STEM.as_bytes(),
        bytes => bytes,
    };
    let stem = |digits: usize| &base[..base.len().min(SpriteName::MAX_LEN - digits)];
    let mut used = [0u64; (NUMBERS + 1).div_ceil(64)];
    let mut whole = false;
    for held in taken {
        let held = held.as_bytes();
        whole |= held.eq_ignore_ascii_case(base);
        // Read from where a stem cut for that many digits ends, so digits in
        // the base itself are never taken for a number.
        for digits in 1..=DIGITS {
            let stem = stem(digits);
            let Some((front, numeral)) = held.split_at_checked(stem.len()) else {
                continue;
            };
            let well_formed = numeral.len() == digits
                && numeral.first().is_some_and(|&first| first != b'0')
                && numeral.iter().all(u8::is_ascii_digit);
            if well_formed && front.eq_ignore_ascii_case(stem) {
                let number = numeral
                    .iter()
                    .fold(0, |value, &digit| value * 10 + usize::from(digit - b'0'));
                if number <= NUMBERS {
                    used[number / 64] |= 1 << (number % 64);
                }
            }
        }
    }
    if !whole {
        return SpriteName::from_bytes(base);
    }
    let number = (1..=NUMBERS).find(|&number| used[number / 64] & (1 << (number % 64)) == 0)?;
    let digits = number.ilog10() as usize + 1;
    let stem = stem(digits);
    let mut name = [0u8; SpriteName::MAX_LEN];
    let (head, tail) = name.split_at_mut(stem.len());
    head.copy_from_slice(stem);
    let mut rest = number;
    for slot in tail[..digits].iter_mut().rev() {
        *slot = b"0123456789"[rest % 10];
        rest /= 10;
    }
    SpriteName::from_bytes(&name[..stem.len() + digits])
}

/// A document being edited.
#[derive(Debug)]
pub struct Document {
    entries: Vec<Entry>,
    current: usize,
    origin: Origin,
    unkept: Unkept,
    history: History,
    generation: u64,
    settings: SaveSettings,
}

impl Document {
    /// A new document of one picture, to be saved as a PNG.
    #[must_use]
    pub fn new(picture: Picture) -> Self {
        Self::new_as(picture, SaveFormat::Png)
    }

    /// A new document of one picture, to be saved as `format`.
    #[must_use]
    pub fn new_as(picture: Picture, format: SaveFormat) -> Self {
        Self::built(
            alloc::vec![Entry::Picture(picture)],
            Origin::New(format),
            Unkept::default(),
            SaveSettings::default(),
        )
    }

    /// A document of `entries` read from a file of `origin`, which held what
    /// `unkept` says they do not and is written again with `settings`; `None`
    /// for no entries or more than a document holds.
    #[must_use]
    pub fn of(
        entries: Vec<Entry>,
        origin: Origin,
        unkept: Unkept,
        settings: SaveSettings,
    ) -> Option<Self> {
        (!entries.is_empty() && entries.len() <= MAX_ENTRIES)
            .then(|| Self::built(entries, origin, unkept, settings))
    }

    fn built(entries: Vec<Entry>, origin: Origin, unkept: Unkept, settings: SaveSettings) -> Self {
        Self {
            entries,
            current: 0,
            origin,
            unkept,
            history: History::new(),
            generation: 0,
            settings,
        }
    }

    /// Every entry.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every entry, the document given up for them.
    #[must_use]
    pub fn into_entries(self) -> Vec<Entry> {
        self.entries
    }

    /// Which entry is showing.
    #[must_use]
    pub const fn current(&self) -> usize {
        self.current
    }

    /// The entry showing.
    #[must_use]
    pub fn entry(&self) -> &Entry {
        &self.entries[self.current]
    }

    /// The picture showing, unless the entry showing is kept as its bytes.
    #[must_use]
    pub fn picture(&self) -> Option<&Picture> {
        self.entry().picture()
    }

    /// The canvas showing, to draw on; what is drawn is recorded with
    /// [`record_tiles`](Self::record_tiles).
    pub fn canvas_mut(&mut self) -> Option<&mut Canvas> {
        match &mut self.entries[self.current] {
            Entry::Picture(picture) => Some(picture.canvas_mut()),
            Entry::Kept(_) => None,
        }
    }

    /// Show entry `index`, answering whether it exists.
    pub fn select(&mut self, index: usize) -> bool {
        if index < self.entries.len() {
            self.current = index;
            true
        } else {
            false
        }
    }

    /// What the document was read from.
    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    /// What its file held that it does not, so writing it back over that
    /// file would lose.
    #[must_use]
    pub const fn unkept(&self) -> Unkept {
        self.unkept
    }

    /// Whether it is a sprite area ([`sprite_area`]).
    #[must_use]
    pub fn is_sprite_area(&self) -> bool {
        sprite_area(&self.entries, self.origin)
    }

    /// Whether it is a TIFF's pages ([`pages`]).
    #[must_use]
    pub fn is_pages(&self) -> bool {
        pages(&self.entries, self.origin)
    }

    /// How each format that offers a choice is written.
    #[must_use]
    pub const fn settings(&self) -> SaveSettings {
        self.settings
    }

    /// Write it with `settings` from now on, which the caller has checked.
    pub fn set_settings(&mut self, settings: SaveSettings) {
        self.settings = settings;
    }

    /// The generation: it changes with every change, undo and redo.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the document differs from its file.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Whether there is anything to undo and anything to redo.
    #[must_use]
    pub fn can_undo_redo(&self) -> (bool, bool) {
        (self.history.can_undo(), self.history.can_redo())
    }

    /// The document of `generation` reached its file.
    pub fn saved(&mut self, generation: u64) {
        if generation == self.generation {
            self.history.mark_saved();
        } else {
            self.history.forget_saved();
        }
    }

    /// The document frozen as it is now.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the list of entries cannot be copied.
    pub fn snapshot(&self) -> Result<Snapshot, OutOfMemory> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| OutOfMemory)?;
        for entry in &self.entries {
            entries.push(entry.try_clone()?);
        }
        Ok(Snapshot {
            entries,
            current: self.current,
            origin: self.origin,
            settings: self.settings,
        })
    }

    /// Bytes the pixels of every entry occupy.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries.iter().map(Entry::bytes).sum()
    }

    /// Size the history for memory pressure `band`.
    pub fn adopt_pressure(&mut self, band: PressureBand) {
        let bytes = self.bytes();
        self.history.adopt_pressure(band, bytes);
    }

    /// Make room to record the next change, so one made once this answers is
    /// always recorded.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when there is none: the change should not be made.
    pub fn reserve(&mut self) -> Result<(), OutOfMemory> {
        self.history.reserve()
    }

    fn record(&mut self, step: Step) {
        self.generation += 1;
        let entries = &self.entries;
        self.history
            .record(step, || entries.iter().map(Entry::bytes).sum());
    }

    /// Record that the showing entry's `tiles` were written, each the tile as
    /// it stood before: a change made after [`reserve`](Self::reserve).
    pub fn record_tiles(&mut self, tiles: Vec<(usize, Arc<Tile>)>) {
        let Some(layer) = self.picture().map(Picture::active) else {
            return;
        };
        if !tiles.is_empty() {
            let entry = self.current;
            self.record(Step::Tiles {
                entry,
                layer,
                tiles,
            });
        }
    }

    /// Put the tiles a worker wrote in place in layer `layer` of the showing
    /// entry, recording the step that takes them back out; `false`, changing
    /// nothing, for tiles of a canvas of another shape or a layer that is
    /// not there.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the step cannot be recorded; nothing changed.
    pub fn adopt_tiles(
        &mut self,
        layer: usize,
        tiles: Vec<(usize, Arc<Tile>)>,
    ) -> Result<bool, OutOfMemory> {
        self.history.reserve()?;
        let entry = self.current;
        let Entry::Picture(picture) = &mut self.entries[entry] else {
            return Ok(false);
        };
        let Some(target) = picture.layers_mut().get_mut(layer) else {
            return Ok(false);
        };
        let canvas = &mut target.canvas;
        if tiles.iter().any(|(index, tile)| !canvas.fits(*index, tile)) {
            return Ok(false);
        }
        let mut before = Vec::new();
        before
            .try_reserve_exact(tiles.len())
            .map_err(|_| OutOfMemory)?;
        for (index, tile) in tiles {
            before.push((index, canvas.replace_tile(index, tile)));
        }
        if !before.is_empty() {
            self.record(Step::Tiles {
                entry,
                layer,
                tiles: before,
            });
        }
        Ok(true)
    }

    /// Replace the showing entry's picture with `picture`; `false`,
    /// changing nothing, where the entry is kept as its bytes.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the step cannot be recorded; nothing changed.
    pub fn replace_picture(&mut self, picture: Picture) -> Result<bool, OutOfMemory> {
        self.history.reserve()?;
        let entry = self.current;
        let Entry::Picture(held) = &mut self.entries[entry] else {
            return Ok(false);
        };
        let old = core::mem::replace(held, picture);
        self.record(Step::Picture {
            entry,
            picture: old,
        });
        Ok(true)
    }

    /// The showing entry's picture, to change its layers.
    fn layered_mut(&mut self) -> Result<(usize, &mut Picture), LayerRefusal> {
        self.history
            .reserve()
            .map_err(|OutOfMemory| LayerRefusal::OutOfMemory)?;
        let entry = self.current;
        match &mut self.entries[entry] {
            Entry::Picture(picture) => Ok((entry, picture)),
            Entry::Kept(_) => Err(LayerRefusal::NoSuchLayer),
        }
    }

    /// Add `layer` to the showing picture at `index`, and paint on it.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]; nothing changed.
    pub fn insert_layer(&mut self, index: usize, layer: Layer) -> Result<(), LayerRefusal> {
        let (entry, picture) = self.layered_mut()?;
        picture
            .insert_layer(index, layer)
            .map_err(|(refusal, _)| refusal)?;
        self.record(Step::LayerInserted {
            entry,
            layer: index,
        });
        Ok(())
    }

    /// Take layer `index` out of the showing picture, painting on the one
    /// that takes its place.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]; nothing changed.
    pub fn remove_layer(&mut self, index: usize) -> Result<(), LayerRefusal> {
        let (entry, picture) = self.layered_mut()?;
        let removed = picture.remove_layer(index)?;
        self.record(Step::LayerRemoved {
            entry,
            layer: index,
            removed,
        });
        Ok(())
    }

    /// Move the showing picture's layer `from` to `to`, painting on it there.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]; nothing changed.
    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), LayerRefusal> {
        let (entry, picture) = self.layered_mut()?;
        picture.move_layer(from, to)?;
        if from != to {
            self.record(Step::LayerMoved {
                entry,
                from: to,
                to: from,
            });
        }
        Ok(())
    }

    /// Show the showing picture's layer `index` as `shown` says, painting on
    /// it; a layer already shown so takes no step to undo.
    ///
    /// # Errors
    ///
    /// [`LayerRefusal`]; nothing changed.
    pub fn show_layer(&mut self, index: usize, shown: Shown) -> Result<(), LayerRefusal> {
        let (entry, picture) = self.layered_mut()?;
        let unchanged = picture.layers().get(index).is_some_and(|layer| {
            (&layer.name, layer.opacity, layer.visible)
                == (&shown.name, shown.opacity, shown.visible)
        });
        if unchanged {
            picture.set_active(index);
            return Ok(());
        }
        let before = picture.show_layer(index, shown)?;
        self.record(Step::LayerShown {
            entry,
            layer: index,
            shown: before,
        });
        Ok(())
    }

    /// Paint on the showing picture's layer `index`, answering whether it
    /// exists: where painting lands is not itself a change.
    pub fn select_layer(&mut self, index: usize) -> bool {
        match &mut self.entries[self.current] {
            Entry::Picture(picture) => picture.set_active(index),
            Entry::Kept(_) => false,
        }
    }

    /// Give the showing entry `palette`, which must be as long as the one it
    /// has, and `sprite` details; `false`, changing nothing, where the entry
    /// has no such palette.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the step cannot be recorded; nothing changed.
    pub fn set_details(
        &mut self,
        palette: Option<Vec<Rgba8>>,
        sprite: Option<SpriteInfo>,
    ) -> Result<bool, OutOfMemory> {
        self.history.reserve()?;
        let entry = self.current;
        let Entry::Picture(held) = &mut self.entries[entry] else {
            return Ok(false);
        };
        let palette = match palette {
            Some(palette) => match held.canvas_mut().swap_palette(palette) {
                Some(old) => Some(old),
                None => return Ok(false),
            },
            None => None,
        };
        let sprite = core::mem::replace(&mut held.sprite, sprite);
        self.record(Step::Details {
            entry,
            palette,
            sprite,
        });
        Ok(true)
    }

    /// Add `entry` at `index` and show it.
    ///
    /// # Errors
    ///
    /// [`ListRefusal`] where it could not be added; nothing changed.
    pub fn insert(&mut self, index: usize, entry: Entry) -> Result<(), ListRefusal> {
        if index > self.entries.len() {
            return Err(ListRefusal::NoSuchEntry);
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(ListRefusal::Full);
        }
        if self.entries.try_reserve(1).is_err() || self.history.reserve().is_err() {
            return Err(ListRefusal::OutOfMemory);
        }
        self.entries.insert(index, entry);
        self.current = index;
        self.record(Step::Inserted { index });
        Ok(())
    }

    /// Remove entry `index`, showing the one that takes its place.
    ///
    /// # Errors
    ///
    /// [`ListRefusal`] where it could not be removed; nothing changed.
    pub fn remove(&mut self, index: usize) -> Result<(), ListRefusal> {
        if index >= self.entries.len() {
            return Err(ListRefusal::NoSuchEntry);
        }
        if self.entries.len() == 1 {
            return Err(ListRefusal::LastEntry);
        }
        self.history
            .reserve()
            .map_err(|OutOfMemory| ListRefusal::OutOfMemory)?;
        let entry = self.entries.remove(index);
        self.current = index.min(self.entries.len() - 1);
        self.record(Step::Removed { index, entry });
        Ok(())
    }

    /// Move entry `from` to `to`, showing it there.
    ///
    /// # Errors
    ///
    /// [`ListRefusal`] where it could not be moved; nothing changed.
    pub fn move_entry(&mut self, from: usize, to: usize) -> Result<(), ListRefusal> {
        if from >= self.entries.len() || to >= self.entries.len() {
            return Err(ListRefusal::NoSuchEntry);
        }
        if from == to {
            return Ok(());
        }
        self.history
            .reserve()
            .map_err(|OutOfMemory| ListRefusal::OutOfMemory)?;
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        self.current = to;
        self.record(Step::Moved { from: to, to: from });
        Ok(())
    }

    /// Undo the newest change, showing the entry it was made to.
    ///
    /// # Errors
    ///
    /// [`Unapplied`]: nothing to undo, or no memory for it.
    pub fn undo(&mut self) -> Result<Applied, Unapplied> {
        let applied = self.history.undo(&mut self.entries)?;
        Ok(self.adopt(applied))
    }

    /// Redo the newest undone change, showing the entry it was made to.
    ///
    /// # Errors
    ///
    /// [`Unapplied`]: nothing to redo, or no memory for it.
    pub fn redo(&mut self) -> Result<Applied, Unapplied> {
        let applied = self.history.redo(&mut self.entries)?;
        Ok(self.adopt(applied))
    }

    /// Show what `applied` changed; a change to another entry than the one
    /// showing changes everything shown.
    fn adopt(&mut self, applied: Applied) -> Applied {
        self.generation += 1;
        let shown = applied.entry.min(self.entries.len().saturating_sub(1));
        let moved = shown != self.current;
        self.current = shown;
        if moved {
            Applied {
                entry: shown,
                damage: Damage::List,
            }
        } else {
            applied
        }
    }

    /// The names its sprites have.
    pub fn names(&self) -> impl Iterator<Item = &SpriteName> {
        self.entries.iter().filter_map(Entry::name)
    }

    /// Whether a sprite called `name` is already in the document, other than
    /// entry `except`.
    #[must_use]
    pub fn names_taken(&self, name: &SpriteName, except: Option<usize>) -> bool {
        self.entries.iter().enumerate().any(|(index, entry)| {
            Some(index) != except && entry.name().is_some_and(|held| held.matches(name))
        })
    }

    /// The index of the sprite called `name`, matched without regard to case.
    #[must_use]
    pub fn find(&self, name: &SpriteName) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.name().is_some_and(|held| held.matches(name)))
    }

    /// How many steps can be undone.
    #[must_use]
    pub fn history_depth(&self) -> usize {
        self.history.depth()
    }
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
