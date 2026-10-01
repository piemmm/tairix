//! A document: the pictures one file holds, and what the file says about
//! them.
//!
//! A PNG or a JPEG holds one picture; a RISC OS sprite area holds any number,
//! each with its name, mode and palette, and keeps the sprites no editor here
//! can read as their exact bytes so that saving writes them back unchanged.
//! Every change goes through here, so each is recorded as a step of the
//! history.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::{
    desktop_palette, IndexDepth, Rgba8, SpriteMode, SpriteName, SpritePalette, Unkept,
};
use tairix_reclaim::PressureBand;
use tairix_sandbox::imageedit::KeptReason;
use tairix_sandbox::imagerender::ViewFormat;

use crate::canvas::{Canvas, CanvasError, Kind, OutOfMemory, Sample, Tile};
use crate::colour::{nearest, WHITE};
use crate::history::{Applied, Damage, History, Step, Unapplied};

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

/// One picture of a document.
#[derive(Debug, Eq, PartialEq)]
pub struct Picture {
    /// Its pixels.
    pub canvas: Canvas,
    /// Its sprite details, when it is, or is to be, a sprite.
    pub sprite: Option<SpriteInfo>,
}

impl Picture {
    /// A picture of `canvas` that is not a sprite.
    #[must_use]
    pub const fn plain(canvas: Canvas) -> Self {
        Self {
            canvas,
            sprite: None,
        }
    }

    /// A copy sharing its pixels until either is written.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        Ok(Self {
            canvas: self.canvas.try_clone()?,
            sprite: self.sprite.clone(),
        })
    }

    /// A pixel's shape, width to height: square unless the sprite's mode
    /// says otherwise.
    #[must_use]
    pub fn pixel_aspect(&self) -> (u32, u32) {
        self.sprite
            .as_ref()
            .map_or((1, 1), |sprite| sprite.mode.pixel_aspect())
    }
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
            Self::Picture(picture) => picture.canvas.charged_bytes(),
            Self::Kept(kept) => kept.bytes.len().div_ceil(Arc::strong_count(&kept.bytes)),
        }
    }

    /// Bytes the entry occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        match self {
            Self::Picture(picture) => picture.canvas.bytes(),
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

/// Whether `entries`, read from `origin`, are a sprite area: read from one,
/// or holding anything that only a sprite area can.
#[must_use]
pub fn sprite_area(entries: &[Entry], origin: Origin) -> bool {
    origin == Origin::Read(ViewFormat::Sprite)
        || entries.len() > 1
        || entries.iter().any(|entry| entry.name().is_some())
}

/// What a document was read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Nothing: it was made here.
    New,
    /// A file of this format.
    Read(ViewFormat),
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
    /// The JPEG quality a save as JPEG uses.
    pub jpeg_quality: u8,
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
    jpeg_quality: u8,
}

impl Document {
    /// A new document of one picture.
    #[must_use]
    pub fn new(picture: Picture) -> Self {
        Self::built(
            alloc::vec![Entry::Picture(picture)],
            Origin::New,
            Unkept::default(),
        )
    }

    /// A document of `entries` read from a file of `origin`, which held what
    /// `unkept` says they do not; `None` for no entries or more than a
    /// document holds.
    #[must_use]
    pub fn of(entries: Vec<Entry>, origin: Origin, unkept: Unkept) -> Option<Self> {
        (!entries.is_empty() && entries.len() <= MAX_ENTRIES)
            .then(|| Self::built(entries, origin, unkept))
    }

    fn built(entries: Vec<Entry>, origin: Origin, unkept: Unkept) -> Self {
        Self {
            entries,
            current: 0,
            origin,
            unkept,
            history: History::new(),
            generation: 0,
            jpeg_quality: tairix_image::JpegOptions::DEFAULT_QUALITY,
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
            Entry::Picture(picture) => Some(&mut picture.canvas),
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

    /// The JPEG quality a save as JPEG uses.
    #[must_use]
    pub const fn jpeg_quality(&self) -> u8 {
        self.jpeg_quality
    }

    /// Save as JPEG at `quality`, which the caller has checked.
    pub fn set_jpeg_quality(&mut self, quality: u8) {
        self.jpeg_quality = quality;
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
            jpeg_quality: self.jpeg_quality,
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
        if !tiles.is_empty() {
            let entry = self.current;
            self.record(Step::Tiles { entry, tiles });
        }
    }

    /// Put the tiles a worker wrote in place in the showing entry, recording
    /// the step that takes them back out; `false`, changing nothing, for tiles
    /// of a canvas of another shape.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the step cannot be recorded; nothing changed.
    pub fn adopt_tiles(&mut self, tiles: Vec<(usize, Arc<Tile>)>) -> Result<bool, OutOfMemory> {
        self.history.reserve()?;
        let Some(canvas) = self.canvas_mut() else {
            return Ok(false);
        };
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
        self.record_tiles(before);
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
            Some(palette) => match held.canvas.swap_palette(palette) {
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
