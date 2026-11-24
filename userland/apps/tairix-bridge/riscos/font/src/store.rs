//! The fonts' data and the glyphs painted from it, kept in the session's
//! reclaimable caches: each master's data, loaded when its font is found and
//! loaded again when it is wanted after the machine's memory pressure took it
//! back, and each glyph's levels as a paint drew them. What a call measures
//! or draws with it holds a share of, so letting the cache's copy go never
//! takes data from under a call using it.

use alloc::rc::Rc;
use core::mem::size_of;

use tairix_hash::{BuildFastHash, BuildSipHash13, HashSeed};
use tairix_log::Sink;
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureGauge, RebuildCost,
    ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity, MAP_ENTRY_OVERHEAD,
};

use crate::bitmaps::{Held as HeldFile, Raster, Wanted};
use crate::fontdata::FontData;
use crate::glyphs::Bitmap;

/// A master's data as the cache holds it.
struct Held(Rc<FontData>);

impl CachedBytes for Held {
    fn payload_bytes(&self) -> usize {
        self.0.bytes()
    }

    /// The data's own buffers wipe themselves when the last share of them
    /// goes, so the cache letting its share go leaves nothing to wipe.
    fn wipe(&mut self) {}
}

/// The cache's declaration: data parsed from the guest's font files or
/// fetched from the font service, rebuilt by a load, the guest's own, and
/// all of it let go at once when a font file is uncached.
const CANDIDATE: CacheCandidate = CacheCandidate {
    class: Some(ReclaimClass::TransformCache),
    owner: Some(ReclaimOwner::UserlandProcess("riscos-font")),
    rebuild_cost: RebuildCost::Moderate,
    sensitivity: Some(Sensitivity::UserData),
    invalidation: Some(InvalidationSource::GenerationToken),
    rule: Some(ReclaimRule::Drop),
    entry_metadata_bytes: MAP_ENTRY_OVERHEAD + size_of::<u64>() + size_of::<Held>(),
};

/// A glyph as a paint draws it: its master's data, the glyph, the font's
/// size in sixteenths of a point and resolution in dots an inch, the matrix
/// painting it, the quarter of a pixel it is placed at across and up,
/// whether in whole pixels, and what its levels are made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Drawing {
    pub(crate) master: u64,
    pub(crate) glyph: u32,
    pub(crate) points: (i32, i32),
    pub(crate) resolution: (i32, i32),
    pub(crate) matrix: [u32; 6],
    pub(crate) phase: (i32, i32),
    pub(crate) whole: bool,
    pub(crate) raster: Raster,
}

/// A glyph's levels as the cache holds them.
struct Drawn(Rc<Bitmap>);

impl CachedBytes for Drawn {
    fn payload_bytes(&self) -> usize {
        self.0.bytes()
    }

    /// The levels wipe themselves when the last share of them goes.
    fn wipe(&mut self) {}
}

/// The glyph cache's declaration: pixels drawn from a font's data, cheap to
/// draw again, the first to go under pressure, and let go with the data.
const GLYPHS: CacheCandidate = CacheCandidate {
    class: Some(ReclaimClass::DisposableUi),
    owner: Some(ReclaimOwner::UserlandProcess("riscos-font")),
    rebuild_cost: RebuildCost::Cheap,
    sensitivity: Some(Sensitivity::UserData),
    invalidation: Some(InvalidationSource::GenerationToken),
    rule: Some(ReclaimRule::Drop),
    entry_metadata_bytes: MAP_ENTRY_OVERHEAD + size_of::<Drawing>() + size_of::<Drawn>(),
};

/// A bitmap file as the cache holds it.
struct File(Rc<HeldFile>);

impl CachedBytes for File {
    fn payload_bytes(&self) -> usize {
        self.0.bytes()
    }

    /// The file's bytes wipe themselves when the last share of them goes.
    fn wipe(&mut self) {}
}

/// The bitmap files' declaration: read from the guest's font directories,
/// cheap to read again, and let go with the glyphs drawn from them.
const FILES: CacheCandidate = CacheCandidate {
    class: Some(ReclaimClass::TransformCache),
    owner: Some(ReclaimOwner::UserlandProcess("riscos-font")),
    rebuild_cost: RebuildCost::Moderate,
    sensitivity: Some(Sensitivity::UserData),
    invalidation: Some(InvalidationSource::GenerationToken),
    rule: Some(ReclaimRule::Drop),
    entry_metadata_bytes: MAP_ENTRY_OVERHEAD + size_of::<Wanted>() + size_of::<File>(),
};

/// The masters' data, by master serial, the bitmap files read for them, and
/// the glyphs drawn from either. The serials are the Font Manager's own, so
/// the data's keys need no keyed hash; the sizes and glyphs the guest
/// chooses, so theirs are hashed under the session's key.
pub(crate) struct Store {
    data: ReclaimCache<u64, Held, u64, BuildFastHash>,
    files: ReclaimCache<Wanted, File, u64, BuildSipHash13>,
    glyphs: ReclaimCache<Drawing, Drawn, u64, BuildSipHash13>,
    /// Bumped whenever every font's data is to be read afresh.
    generation: u64,
    /// Bumped whenever the bitmap files and the glyphs drawn are to be, as
    /// the data is.
    drawn: u64,
}

impl Store {
    /// An empty store, its budget a share of a session of `ceiling` bytes,
    /// its glyphs' keys hashed under `key`.
    pub(crate) fn new(
        ceiling: u64,
        key: HashSeed,
        pressure: &'static (dyn PressureGauge + 'static),
        log: &'static (dyn Sink + Sync),
    ) -> Self {
        let budget = || CacheBudget::from_backing(usize::try_from(ceiling).unwrap_or(usize::MAX));
        Self {
            data: ReclaimCache::new(
                "riscos.font",
                CANDIDATE,
                budget(),
                pressure,
                log,
                BuildFastHash::new(),
            ),
            files: ReclaimCache::new(
                "riscos.font.files",
                FILES,
                budget(),
                pressure,
                log,
                BuildSipHash13::with_seed(key),
            ),
            glyphs: ReclaimCache::new(
                "riscos.font.glyphs",
                GLYPHS,
                budget(),
                pressure,
                log,
                BuildSipHash13::with_seed(key),
            ),
            generation: 0,
            drawn: 0,
        }
    }

    /// The bitmap file `wanted`, where the cache holds it.
    pub(crate) fn file(&mut self, wanted: &Wanted) -> Option<Rc<HeldFile>> {
        self.files
            .find(&self.drawn, wanted)
            .map(|file| Rc::clone(&file.0))
    }

    /// `held` kept as the bitmap file `wanted`, where the budget has room.
    pub(crate) fn keep_file(&mut self, wanted: Wanted, held: &Rc<HeldFile>) {
        self.files
            .retain(&self.drawn, wanted, File(Rc::clone(held)));
    }

    /// The levels `drawing` paints, drawn by `draw` where the cache does not
    /// hold them; none where `draw` gives none.
    pub(crate) fn glyph<E>(
        &mut self,
        drawing: Drawing,
        draw: impl FnOnce() -> Result<Option<Bitmap>, E>,
    ) -> Result<Option<Rc<Bitmap>>, E> {
        if let Some(held) = self.glyphs.find(&self.drawn, &drawing) {
            return Ok(Some(Rc::clone(&held.0)));
        }
        let Some(bitmap) = draw()? else {
            return Ok(None);
        };
        let bitmap = Rc::new(bitmap);
        self.glyphs
            .retain(&self.drawn, drawing, Drawn(Rc::clone(&bitmap)));
        Ok(Some(bitmap))
    }

    /// The data of the master `serial`, where the cache still holds it.
    pub(crate) fn get(&mut self, serial: u64) -> Option<Rc<FontData>> {
        self.data
            .find(&self.generation, &serial)
            .map(|held| Rc::clone(&held.0))
    }

    /// The bytes the master `serial`'s data holds, while the store holds it.
    pub(crate) fn bytes_of(&self, serial: u64) -> Option<usize> {
        self.data
            .peek(&self.generation, &serial)
            .map(|held| held.0.bytes())
    }

    /// `data` kept as the master `serial`'s, where the budget has room.
    pub(crate) fn keep(&mut self, serial: u64, data: &Rc<FontData>) {
        self.data
            .retain(&self.generation, serial, Held(Rc::clone(data)));
    }

    /// The master `serial`'s data let go.
    pub(crate) fn forget(&mut self, serial: u64) {
        self.data.invalidate(&serial);
    }

    /// The bytes the data, files and glyphs held take.
    pub(crate) fn bytes(&self) -> usize {
        self.data
            .charged_bytes()
            .saturating_add(self.files.charged_bytes())
            .saturating_add(self.glyphs.charged_bytes())
    }

    /// Every master's data let go, to be read afresh when next wanted, and
    /// what was drawn from it.
    pub(crate) fn forget_all(&mut self) {
        self.generation += 1;
        self.forget_drawn();
    }

    /// Every bitmap file and glyph let go, to be read and drawn afresh.
    pub(crate) fn forget_drawn(&mut self) {
        self.drawn += 1;
    }
}

impl core::fmt::Debug for Store {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Store")
            .field("generation", &self.generation)
            .field("drawn", &self.drawn)
            .finish_non_exhaustive()
    }
}
