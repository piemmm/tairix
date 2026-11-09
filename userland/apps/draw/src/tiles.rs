//! The tiles a window's drawing is painted from, rendered off the event loop.
//!
//! The drawing is seen on a grid of square tiles of device pixels at the
//! present zoom. A tile is rendered by a worker from the drawing as it stood
//! — a persistent tree, so handing it over costs a pointer — and kept here
//! with the scene it shows. An edit marks stale only the tiles its changed
//! area meets, found by walking the old and new trees together and passing
//! over every node they share; a stale tile is painted on until its
//! replacement lands, so nothing blinks. A window holds the tiles in view
//! and as many again twice over to scroll back into, once over under
//! moderate memory pressure and none under severe; tiles of other zooms and
//! those furthest from view are let go first.
//!
//! When the zoom changes, the tiles of the view just left stand in, scaled,
//! for the new zoom's until those land: the backdrop, kept from the last view
//! shown whole and let go once the view in front of it is.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_collections::{try_arc, HashMap};
use tairix_drawing::render::Mode;
use tairix_drawing::{Drawing, Node};
use tairix_hash::BuildFastHash;
use tairix_raster::{Mosaic, Surface};
use tairix_reclaim::PressureBand;
use tairix_util::fallible;
use tairix_util::mathf::{ceil, floor};

/// A tile's side, in device pixels.
pub const TILE: u32 = 256;

/// The most tiles one render job carries, so a job's answer lands soon and
/// a scroll is answered tile row by tile row.
pub const TILES_PER_JOB: usize = 8;

/// How many recent changes are remembered, so a tile rendered a few scenes
/// ago that no change since has touched is still taken.
const REMEMBERED_CHANGES: usize = 32;

/// Where a tile stands: the zoom it is rendered at and its column and row.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TileKey {
    /// The bits of the device pixels per drawing pixel it is rendered at.
    pub pixels: u64,
    /// Its column, counting tiles from the drawing's origin.
    pub column: i64,
    /// Its row.
    pub row: i64,
    /// Whether it is rendered as outlines.
    pub outline: bool,
}

impl TileKey {
    /// The device pixels per drawing pixel it is rendered at.
    #[must_use]
    pub fn pixels(&self) -> f64 {
        f64::from_bits(self.pixels)
    }

    /// Its top-left corner on the device grid.
    #[must_use]
    pub fn origin(&self) -> Point {
        let side = f64::from(TILE);
        Point::new(count(self.column) * side, count(self.row) * side)
    }

    /// The area it shows, in the drawing's space.
    #[must_use]
    pub fn area(&self) -> Bounds {
        let side = f64::from(TILE) / self.pixels();
        let corner = self.origin() / self.pixels();
        Bounds::new(corner, corner + Point::new(side, side))
    }
}

/// A tile index as a coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "tile indices are far below 2^52"
)]
fn count(index: i64) -> f64 {
    index as f64
}

/// A tile held, and the scene it shows.
struct Held {
    surface: Arc<Surface>,
    /// The scene it was rendered from.
    scene: u64,
    /// Whether that scene is the present one in all it shows.
    fresh: bool,
}

/// What a render job is asked to draw.
#[derive(Clone, Debug)]
pub struct RenderJob {
    /// The scene the tiles are of.
    pub scene: u64,
    /// The drawing as it stood in that scene.
    pub drawing: Drawing,
    /// The tiles.
    pub keys: Vec<TileKey>,
    /// How objects are drawn.
    pub mode: Mode,
}

/// The rectangle of tiles a view showed: the keys `covering` gave it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Span {
    pixels: u64,
    outline: bool,
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Span {
    /// The rectangle `visible` fills, or `None` for no tiles.
    fn of(visible: &[TileKey]) -> Option<Self> {
        let first = visible.first()?;
        Some(visible.iter().fold(
            Self {
                pixels: first.pixels,
                outline: first.outline,
                left: first.column,
                top: first.row,
                right: first.column,
                bottom: first.row,
            },
            |span, key| Self {
                left: span.left.min(key.column),
                top: span.top.min(key.row),
                right: span.right.max(key.column),
                bottom: span.bottom.max(key.row),
                ..span
            },
        ))
    }

    /// Its tiles' keys, row by row.
    fn keys(self) -> impl Iterator<Item = TileKey> {
        (self.top..=self.bottom).flat_map(move |row| {
            (self.left..=self.right).map(move |column| TileKey {
                pixels: self.pixels,
                column,
                row,
                outline: self.outline,
            })
        })
    }
}

/// The tiles of the last view shown whole, standing in for a new zoom's.
struct Backdrop {
    span: Span,
    /// How many columns across it is.
    columns: u32,
    /// Row by row across `span`; a place no tile was held for holds none.
    tiles: Vec<Option<Arc<Surface>>>,
}

/// What the backdrop stands in with: its tiles as one picture, the zoom they
/// were rendered at, and where on that zoom's device grid the picture's
/// first pixel lies.
pub struct StandIn<'t> {
    /// The tiles, with absent places transparent.
    pub picture: Mosaic<'t, Arc<Surface>>,
    /// The device pixels per drawing pixel they were rendered at.
    pub pixels: f64,
    /// The picture's top-left corner on that zoom's device grid.
    pub origin: Point,
}

impl StandIn<'_> {
    /// The map from the picture's pixels onto the device grid of a view at
    /// `pixels` device pixels per drawing pixel.
    #[must_use]
    pub fn onto(&self, pixels: f64) -> Affine {
        let ratio = pixels / self.pixels;
        Affine::translate(self.origin.x, self.origin.y).then(Affine::scale(ratio, ratio))
    }
}

/// A window's tiles.
pub struct Tiles {
    held: HashMap<TileKey, Held, BuildFastHash>,
    /// How many times the tiles in view are held again beyond them.
    spare: usize,
    /// The scene the tiles should show.
    scene: u64,
    /// The tiles asked for and not yet landed: one job's at most.
    asked: Vec<TileKey>,
    /// Tiles a render could not make, not asked again until the drawing, the
    /// view's mode or the memory pressure changes.
    held_off: Vec<TileKey>,
    /// The latest changes: the scene each made, and the area it touched.
    changes: VecDeque<(u64, Bounds)>,
    /// The view the window last showed.
    shown: Option<Span>,
    backdrop: Option<Backdrop>,
}

impl Default for Tiles {
    fn default() -> Self {
        Self {
            held: HashMap::with_hasher(BuildFastHash::new()),
            spare: spare_at(PressureBand::Normal),
            scene: 0,
            asked: Vec::new(),
            held_off: Vec::new(),
            changes: VecDeque::new(),
            shown: None,
            backdrop: None,
        }
    }
}

/// How many times the tiles in view a window holds again beyond them at
/// memory pressure `band`.
const fn spare_at(band: PressureBand) -> usize {
    match band {
        PressureBand::Normal | PressureBand::Mild => 2,
        PressureBand::Moderate => 1,
        PressureBand::Severe | PressureBand::Critical => 0,
    }
}

impl Tiles {
    /// Hold what memory pressure `band` allows beyond `visible`, the tiles in
    /// view, letting go of the rest now: under severe pressure that is the
    /// backdrop too.
    pub fn adopt_pressure(&mut self, band: PressureBand, visible: &[TileKey]) {
        self.spare = spare_at(band);
        self.held_off.clear();
        if self.spare == 0 {
            self.backdrop = None;
        }
        self.evict(visible);
    }

    /// The tile at `key`, if one is held: fresh, or stale and shown until it
    /// is replaced.
    #[must_use]
    pub fn tile(&self, key: &TileKey) -> Option<&Surface> {
        self.held.get(key).map(|held| &*held.surface)
    }

    /// What stands in for the tiles of a view drawn as `outline` says while
    /// they are not yet held: the backdrop, if one is kept for that mode.
    #[must_use]
    pub fn stand_in(&self, outline: bool) -> Option<StandIn<'_>> {
        let backdrop = self
            .backdrop
            .as_ref()
            .filter(|b| b.span.outline == outline)?;
        let span = backdrop.span;
        let at = TileKey {
            pixels: span.pixels,
            column: span.left,
            row: span.top,
            outline,
        };
        Some(StandIn {
            picture: Mosaic::new(TILE, backdrop.columns, &backdrop.tiles)?,
            pixels: at.pixels(),
            origin: at.origin(),
        })
    }

    /// The scene tiles are rendered for now.
    #[must_use]
    pub const fn scene(&self) -> u64 {
        self.scene
    }

    /// The drawing became `new`, scene `scene`, from `old`: the tiles the
    /// change meets are stale, and the rest show the new scene too.
    pub fn changed(&mut self, old: &Drawing, new: &Drawing, scene: u64) {
        let area = changed_area(old, new);
        self.scene = scene;
        if self.changes.len() == REMEMBERED_CHANGES {
            self.changes.pop_front();
        }
        // Forgetting a change only stops an older tile being taken.
        if self.changes.try_reserve(1).is_ok() {
            self.changes.push_back((scene, area));
        } else {
            self.changes.clear();
        }
        // A job out stays asked for: it lands, its stale tiles are dropped,
        // and only then is the next asked, so a window has one at a time.
        self.held_off.clear();
        for (key, held) in &mut self.held {
            if held.fresh && !key.area().intersects(area) {
                held.scene = scene;
            } else {
                held.fresh = false;
            }
        }
    }

    /// Everything is stale: the mode, or what every tile depends on, changed.
    pub fn invalidate(&mut self, scene: u64) {
        self.scene = scene;
        self.held_off.clear();
        self.changes.clear();
        for held in self.held.values_mut() {
            held.fresh = false;
        }
    }

    /// The tiles of `visible`, in the order given, that want rendering: at
    /// most a job's worth, noted as asked, and none while a job is out, so a
    /// job is never left waiting behind another of the same window's. Asked
    /// each turn, it is also how the cache learns the view moved.
    pub fn wanted(&mut self, visible: &[TileKey]) -> Vec<TileKey> {
        self.follow(visible);
        let mut wanted = Vec::new();
        if !self.asked.is_empty()
            || wanted.try_reserve_exact(TILES_PER_JOB).is_err()
            || self.asked.try_reserve(TILES_PER_JOB).is_err()
        {
            return Vec::new();
        }
        wanted.extend(
            visible
                .iter()
                .copied()
                .filter(|key| {
                    !self.held.get(key).is_some_and(|held| held.fresh)
                        && !self.held_off.contains(key)
                })
                .take(TILES_PER_JOB),
        );
        wanted.sort_unstable();
        self.asked.extend_from_slice(&wanted);
        wanted
    }

    /// The view is now `visible`. A change of zoom keeps the view just left
    /// as the backdrop if it was shown whole, or if no backdrop is kept at
    /// all; a change of mode lets the backdrop go, as does a view whose own
    /// tiles are all held.
    fn follow(&mut self, visible: &[TileKey]) {
        let Some(now) = Span::of(visible) else {
            return;
        };
        let Some(was) = self.shown.replace(now) else {
            return;
        };
        if was.outline != now.outline {
            self.backdrop = None;
        } else if was.pixels != now.pixels && self.spare > 0 {
            let whole = was.keys().all(|key| self.held.contains_key(&key));
            if whole || self.backdrop.is_none() {
                if let Some(backdrop) = self.gathered(was) {
                    self.backdrop = Some(backdrop);
                }
            }
        }
        if self.backdrop.is_some() && visible.iter().all(|key| self.held.contains_key(key)) {
            self.backdrop = None;
        }
    }

    /// The tiles held of `span`, or `None` where none is or the list of
    /// them cannot be had.
    fn gathered(&self, span: Span) -> Option<Backdrop> {
        let columns = u32::try_from(span.right - span.left + 1).ok()?;
        let rows = usize::try_from(span.bottom - span.top + 1).ok()?;
        let places = rows.checked_mul(usize::try_from(columns).ok()?)?;
        let mut tiles = Vec::new();
        tiles.try_reserve_exact(places).ok()?;
        tiles.extend(
            span.keys()
                .map(|key| self.held.get(&key).map(|held| Arc::clone(&held.surface))),
        );
        tiles.iter().any(Option::is_some).then_some(Backdrop {
            span,
            columns,
            tiles,
        })
    }

    /// Those of `keys` still asked for were not rendered: no longer asked,
    /// and not asked again until something they show, or the memory they
    /// need, changes.
    pub fn refused(&mut self, keys: &[TileKey]) {
        for key in keys {
            if self.unask(key) {
                // A refusal that cannot be noted is asked again: never lost.
                let _ = fallible::try_push(&mut self.held_off, *key);
            }
        }
    }

    /// The job asked for `keys` was never taken up: they are asked for again
    /// with the next job, once one can be taken.
    pub fn not_taken(&mut self, keys: &[TileKey]) {
        for key in keys {
            self.unask(key);
        }
    }

    /// `key` no longer asked for, answering whether it was.
    fn unask(&mut self, key: &TileKey) -> bool {
        let Some(at) = self.asked.iter().position(|asked| asked == key) else {
            return false;
        };
        self.asked.swap_remove(at);
        true
    }

    /// The tile at `key`, rendered from `scene`, landed: kept if nothing
    /// since that scene has touched what it shows, `visible` the tiles in
    /// view so the cache lets go of what is furthest from them first.
    /// Answers whether it was kept, and so shows from now on where it lies.
    pub fn landed(
        &mut self,
        scene: u64,
        key: TileKey,
        surface: Surface,
        visible: &[TileKey],
    ) -> bool {
        self.asked.retain(|asked| *asked != key);
        if !self.still_shows(scene, &key) {
            return false;
        }
        // A tile the heap cannot hold is held off as one a render could not
        // make, until the memory pressure changes.
        let Ok(surface) = try_arc(surface) else {
            self.refused_landing(key);
            return false;
        };
        let held = Held {
            surface,
            scene: self.scene,
            fresh: true,
        };
        if self.held.try_insert(key, held).is_err() {
            self.refused_landing(key);
            return false;
        }
        self.evict(visible);
        true
    }

    /// `key` landed and could not be held.
    fn refused_landing(&mut self, key: TileKey) {
        let _ = fallible::try_push(&mut self.held_off, key);
    }

    /// Whether a tile rendered from `scene` shows the present scene: no
    /// change since has touched it, as far back as changes are remembered.
    fn still_shows(&self, scene: u64, key: &TileKey) -> bool {
        if scene == self.scene {
            return true;
        }
        let remembered = self
            .changes
            .front()
            .is_some_and(|&(oldest, _)| oldest <= scene + 1);
        let area = key.area();
        remembered
            && self
                .changes
                .iter()
                .filter(|&&(made, _)| made > scene)
                .all(|(_, touched)| !touched.intersects(area))
    }

    /// Hold no more than the tiles of `visible` and the spare allowed beyond
    /// them: let go of tiles of other zooms and modes first, then of those
    /// furthest from view, one at a time so the choice needs no room of its
    /// own.
    fn evict(&mut self, visible: &[TileKey]) {
        let allowed = visible.len().saturating_mul(1 + self.spare);
        let Some(view) = Span::of(visible) else {
            return;
        };
        // How far a tile stands outside the view, in tiles.
        let distance = |key: &TileKey| -> u64 {
            if key.pixels != view.pixels || key.outline != view.outline {
                return u64::MAX;
            }
            let outside = |at: i64, low: i64, high: i64| {
                let beyond = low.saturating_sub(at).max(at.saturating_sub(high));
                u64::try_from(beyond.max(0)).unwrap_or(u64::MAX)
            };
            outside(key.column, view.left, view.right).saturating_add(outside(
                key.row,
                view.top,
                view.bottom,
            ))
        };
        while self.held.len() > allowed {
            let furthest = self
                .held
                .keys()
                .copied()
                .max_by_key(|key| (distance(key), *key));
            let Some(furthest) = furthest else {
                return;
            };
            self.held.remove(&furthest);
        }
    }
}

/// The tiles covering `area`, a rectangle of the canvas in device pixels,
/// for a view drawing the drawing at `pixels` device pixels per drawing
/// pixel with its origin at `origin` in the canvas: centre out, row by row.
#[must_use]
pub fn covering(area: Bounds, origin: Point, pixels: f64, outline: bool) -> Vec<TileKey> {
    let side = f64::from(TILE);
    let span = |low: f64, high: f64| (index(floor(low / side)), index(ceil(high / side) - 1.0));
    let (left, right) = span(area.min.x - origin.x, area.max.x - origin.x);
    let (top, bottom) = span(area.min.y - origin.y, area.max.y - origin.y);
    let between = |low: i64, high: i64| {
        usize::try_from(high.saturating_sub(low).saturating_add(1)).unwrap_or(0)
    };
    let mut keys = Vec::new();
    let wanted = between(left, right).saturating_mul(between(top, bottom));
    if wanted == 0 || keys.try_reserve(wanted).is_err() {
        return keys;
    }
    for row in top..=bottom {
        for column in left..=right {
            keys.push(TileKey {
                pixels: pixels.to_bits(),
                column,
                row,
                outline,
            });
        }
    }
    let centre = Point::new(
        f64::midpoint(count(left), count(right)),
        f64::midpoint(count(top), count(bottom)),
    );
    // Ties fall row by row, as the keys were laid; an unstable sort keyed to
    // the place as well needs no room beside the list.
    keys.sort_unstable_by(|a, b| {
        let from = |key: &TileKey| {
            (count(key.column) - centre.x).abs() + (count(key.row) - centre.y).abs()
        };
        from(a)
            .total_cmp(&from(b))
            .then((a.row, a.column).cmp(&(b.row, b.column)))
    });
    keys
}

/// A whole tile coordinate as an index: the viewport's reach keeps it far
/// inside the range, and the cast saturates beyond it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a whole number far inside i64, saturating were it not"
)]
fn index(at: f64) -> i64 {
    at as i64
}

/// Bounds, in the drawing's space, holding everything that differs between
/// `old` and `new` as a tile draws them. The page is the canvas's to draw,
/// so a change to it alone stales no tile.
#[must_use]
pub fn changed_area(old: &Drawing, new: &Drawing) -> Bounds {
    if old.layers().len() != new.layers().len() {
        return old.bounds().union(new.bounds());
    }
    old.layers()
        .iter()
        .zip(new.layers().iter())
        .fold(Bounds::EMPTY, |area, (a, b)| {
            area.union(node_change(a, b, Affine::IDENTITY))
        })
}

/// Bounds, in the space `to_drawing` maps onto the drawing's, holding what
/// differs between two nodes placed in it.
fn node_change(a: &Arc<Node>, b: &Arc<Node>, to_drawing: Affine) -> Bounds {
    if Arc::ptr_eq(a, b) {
        return Bounds::EMPTY;
    }
    if let (Some(ours), Some(theirs)) = (a.children(), b.children()) {
        let same_frame = a.transform() == b.transform()
            && a.opacity().to_bits() == b.opacity().to_bits()
            && a.visible() == b.visible()
            && ours.len() == theirs.len()
            && same_clip(a, b);
        if same_frame {
            let inner = a.transform().then(to_drawing);
            return ours
                .iter()
                .zip(theirs.iter())
                .fold(Bounds::EMPTY, |area, (x, y)| {
                    area.union(node_change(x, y, inner))
                });
        }
    }
    a.bounds().union(b.bounds()).transformed(to_drawing)
}

fn same_clip(a: &Node, b: &Node) -> bool {
    match (a.kind(), b.kind()) {
        (
            tairix_drawing::Kind::Group { clip: Some(x), .. },
            tairix_drawing::Kind::Group { clip: Some(y), .. },
        ) => Arc::ptr_eq(x, y),
        (
            tairix_drawing::Kind::Group { clip: None, .. },
            tairix_drawing::Kind::Group { clip: None, .. },
        ) => true,
        _ => false,
    }
}

#[cfg(test)]
#[path = "tiles_tests.rs"]
mod tests;
