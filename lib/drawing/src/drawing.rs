//! A drawing: its page, its layers, and its guides and grid.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Bounds, Point};
use tairix_colour::Rgba;
use tairix_util::fallible;

use crate::children::Children;
use crate::id::ObjectId;
use crate::limits::{self, MAX_DEPTH, MAX_GUIDES, MAX_OBJECTS, MAX_TOTAL_PATH_NODES};
use crate::node::{Kind, Node};
use crate::DrawingError;

/// A unit lengths are shown and typed in. The drawing's own unit is the
/// pixel at 96 to the inch, CSS's and SVG's user unit.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Unit {
    /// The pixel.
    #[default]
    Px,
    /// The point, 1/72 inch.
    Pt,
    /// The pica, twelve points.
    Pc,
    /// The millimetre.
    Mm,
    /// The centimetre.
    Cm,
    /// The inch.
    In,
}

impl Unit {
    /// Every unit, in the order a chooser offers them.
    pub const ALL: [Self; 6] = [Self::Px, Self::Pt, Self::Pc, Self::Mm, Self::Cm, Self::In];

    /// How many pixels one of this unit is.
    #[must_use]
    pub const fn pixels(self) -> f64 {
        match self {
            Self::Px => 1.0,
            Self::Pt => 96.0 / 72.0,
            Self::Pc => 16.0,
            Self::Mm => 96.0 / 25.4,
            Self::Cm => 96.0 / 2.54,
            Self::In => 96.0,
        }
    }

    /// Its CSS spelling.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Px => "px",
            Self::Pt => "pt",
            Self::Pc => "pc",
            Self::Mm => "mm",
            Self::Cm => "cm",
            Self::In => "in",
        }
    }

    /// The unit `symbol` spells.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.symbol() == symbol)
    }
}

/// The page a drawing is laid out on. Its size is held in its own unit, as
/// it was stated, so a page of 210 by 297 millimetres is that exactly.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Page {
    /// Its width, in its unit.
    pub width: f64,
    /// Its height, in its unit.
    pub height: f64,
    /// The unit its size is stated in and lengths are shown in.
    pub unit: Unit,
    /// What it is filled with beneath everything, if anything.
    pub background: Option<Rgba>,
}

impl Page {
    /// A page of `paper`'s size, with no background.
    #[must_use]
    pub const fn of(paper: &Paper) -> Self {
        Self {
            width: paper.width,
            height: paper.height,
            unit: paper.unit,
            background: None,
        }
    }

    /// Its width and height in pixels.
    #[must_use]
    pub const fn size(&self) -> Point {
        let pixels = self.unit.pixels();
        Point::new(self.width * pixels, self.height * pixels)
    }

    /// Its bounds, in pixels.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        Bounds::new(Point::ZERO, self.size())
    }
}

/// A named paper or screen size a page is laid out to.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Paper {
    /// Its usual name.
    pub name: &'static str,
    /// Its width, as it is usually held, in its unit.
    pub width: f64,
    /// Its height, as it is usually held, in its unit.
    pub height: f64,
    /// The unit it is defined in.
    pub unit: Unit,
}

impl Paper {
    const fn new(name: &'static str, width: f64, height: f64, unit: Unit) -> Self {
        Self {
            name,
            width,
            height,
            unit,
        }
    }
}

/// The sizes a new page is offered in; the first is the default.
pub const PAPER: [Paper; 16] = [
    Paper::new("A4", 210.0, 297.0, Unit::Mm),
    Paper::new("A3", 297.0, 420.0, Unit::Mm),
    Paper::new("A5", 148.0, 210.0, Unit::Mm),
    Paper::new("A6", 105.0, 148.0, Unit::Mm),
    Paper::new("A2", 420.0, 594.0, Unit::Mm),
    Paper::new("A1", 594.0, 841.0, Unit::Mm),
    Paper::new("A0", 841.0, 1189.0, Unit::Mm),
    Paper::new("B5", 176.0, 250.0, Unit::Mm),
    Paper::new("B4", 250.0, 353.0, Unit::Mm),
    Paper::new("US Letter", 8.5, 11.0, Unit::In),
    Paper::new("US Legal", 8.5, 14.0, Unit::In),
    Paper::new("Tabloid", 11.0, 17.0, Unit::In),
    Paper::new("Executive", 7.25, 10.5, Unit::In),
    Paper::new("Screen 1920 × 1080", 1920.0, 1080.0, Unit::Px),
    Paper::new("Screen 1280 × 720", 1280.0, 720.0, Unit::Px),
    Paper::new("Icon 256 × 256", 256.0, 256.0, Unit::Px),
];

/// Which way a guide runs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GuideAxis {
    /// Across the page, at a height.
    Horizontal,
    /// Down the page, at a distance from its left.
    Vertical,
}

/// A guide line objects snap to.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Guide {
    /// Which way it runs.
    pub axis: GuideAxis,
    /// Where it stands, in pixels from the page's top or left.
    pub position: f64,
}

/// The drawing's grid.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Grid {
    /// The spacing of its major lines, in pixels.
    pub spacing: f64,
    /// How many cells each major cell is cut into.
    pub subdivisions: u32,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            spacing: 96.0 / 2.54,
            subdivisions: 4,
        }
    }
}

/// What a drawing says of itself. Each is trimmed and not empty, the title one
/// line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Info {
    /// Its title.
    pub title: Option<Arc<str>>,
    /// What it shows, in words.
    pub description: Option<Arc<str>>,
}

impl Info {
    fn check(&self) -> Result<(), DrawingError> {
        let fits = |text: &str, line_breaks: bool| {
            !text.is_empty()
                && text.len() <= limits::MAX_TEXT_BYTES
                && text.trim() == text
                && text
                    .chars()
                    .all(|c| !c.is_control() || (line_breaks && matches!(c, '\n' | '\t')))
        };
        let title = self.title.as_deref().is_none_or(|title| fits(title, false));
        let description = self
            .description
            .as_deref()
            .is_none_or(|description| fits(description, true));
        if title && description {
            Ok(())
        } else {
            Err(DrawingError::Invalid)
        }
    }
}

/// A drawing: its page, its layers bottom first, its guides, its grid and
/// what it says of itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawing {
    page: Page,
    layers: Children,
    guides: Arc<[Guide]>,
    grid: Grid,
    info: Info,
}

/// Where a node stands: the index of its layer, then of each group down to
/// it.
pub type NodePath = Vec<usize>;

impl Drawing {
    /// The drawing of `page`, `layers`, `guides` and `grid`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for a page or grid out of range or a layer
    /// that is not a group, and [`DrawingError::TooComplex`] past the
    /// drawing's bounds.
    pub fn new(
        page: Page,
        layers: Children,
        guides: Arc<[Guide]>,
        grid: Grid,
    ) -> Result<Self, DrawingError> {
        Self::checked(page, layers, guides, grid, Info::default())
    }

    fn checked(
        page: Page,
        layers: Children,
        guides: Arc<[Guide]>,
        grid: Grid,
        info: Info,
    ) -> Result<Self, DrawingError> {
        let size = page.size();
        let page_valid =
            limits::length(size.x) && limits::length(size.y) && size.x > 0.0 && size.y > 0.0;
        let grid_valid = limits::length(grid.spacing)
            && grid.spacing > 0.0
            && (1..=64).contains(&grid.subdivisions);
        if !page_valid || !grid_valid {
            return Err(DrawingError::Invalid);
        }
        if guides.len() > MAX_GUIDES {
            return Err(DrawingError::TooComplex);
        }
        if !guides
            .iter()
            .all(|guide| limits::coordinate(guide.position))
        {
            return Err(DrawingError::Invalid);
        }
        if !layers
            .iter()
            .all(|layer| matches!(layer.kind(), Kind::Group { .. }))
        {
            return Err(DrawingError::Invalid);
        }
        let summary = layers.summary();
        if summary.objects > MAX_OBJECTS
            || summary.path_nodes > MAX_TOTAL_PATH_NODES
            || summary.depth > MAX_DEPTH
        {
            return Err(DrawingError::TooComplex);
        }
        info.check()?;
        Ok(Self {
            page,
            layers,
            guides,
            grid,
            info,
        })
    }

    /// Its page.
    #[must_use]
    pub const fn page(&self) -> &Page {
        &self.page
    }

    /// Its layers, bottom first.
    #[must_use]
    pub const fn layers(&self) -> &Children {
        &self.layers
    }

    /// Its guides.
    #[must_use]
    pub fn guides(&self) -> &[Guide] {
        &self.guides
    }

    /// Its grid.
    #[must_use]
    pub const fn grid(&self) -> Grid {
        self.grid
    }

    /// Whether `other` is this very drawing — the same layers, shared, on the
    /// same page with the same guides, grid and title — without comparing
    /// what the layers hold: an edit replaces the layers it touches.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        self.page == other.page
            && self.grid == other.grid
            && self.info == other.info
            && (Arc::ptr_eq(&self.guides, &other.guides) || self.guides == other.guides)
            && self.layers.len() == other.layers.len()
            && self
                .layers
                .iter()
                .zip(other.layers.iter())
                .all(|(mine, theirs)| Arc::ptr_eq(mine, theirs))
    }

    /// What it says of itself.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The drawing saying `info` of itself.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for a title or description that is empty,
    /// untrimmed, holds a control character (a description may break lines)
    /// or is longer than [`limits::MAX_TEXT_BYTES`].
    pub fn with_info(&self, info: Info) -> Result<Self, DrawingError> {
        info.check()?;
        Ok(Self {
            info,
            ..self.clone()
        })
    }

    /// How many objects it holds, layers and groups counted.
    #[must_use]
    pub const fn objects(&self) -> usize {
        self.layers.summary().objects
    }

    /// Bounds that hold everything it paints.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.layers.summary().bounds
    }

    /// The drawing on `page`.
    ///
    /// # Errors
    ///
    /// As [`Drawing::new`].
    pub fn with_page(&self, page: Page) -> Result<Self, DrawingError> {
        Self::checked(
            page,
            self.layers.clone(),
            self.guides.clone(),
            self.grid,
            self.info.clone(),
        )
    }

    /// The drawing with `layers`.
    ///
    /// # Errors
    ///
    /// As [`Drawing::new`].
    pub fn with_layers(&self, layers: Children) -> Result<Self, DrawingError> {
        Self::checked(
            self.page,
            layers,
            self.guides.clone(),
            self.grid,
            self.info.clone(),
        )
    }

    /// The drawing with `guides`.
    ///
    /// # Errors
    ///
    /// As [`Drawing::new`].
    pub fn with_guides(&self, guides: Arc<[Guide]>) -> Result<Self, DrawingError> {
        Self::checked(
            self.page,
            self.layers.clone(),
            guides,
            self.grid,
            self.info.clone(),
        )
    }

    /// The drawing with `grid`.
    ///
    /// # Errors
    ///
    /// As [`Drawing::new`].
    pub fn with_grid(&self, grid: Grid) -> Result<Self, DrawingError> {
        Self::checked(
            self.page,
            self.layers.clone(),
            self.guides.clone(),
            grid,
            self.info.clone(),
        )
    }

    /// The node at `path`, if one stands there.
    #[must_use]
    pub fn node_at(&self, path: &[usize]) -> Option<&Arc<Node>> {
        let (&first, rest) = path.split_first()?;
        let mut node = self.layers.get(first)?;
        for &index in rest {
            node = node.children()?.get(index)?;
        }
        Some(node)
    }

    /// Where the node `id` names stands, searching the layers bottom first
    /// and each group's children in order; a group's clip is not searched.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the search cannot hold its path.
    pub fn find(&self, id: ObjectId) -> Result<Option<NodePath>, DrawingError> {
        let mut path: NodePath = Vec::new();
        if find_in(&self.layers, id, &mut path)? {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }

    /// Where each of `ids` names a node, as [`find`](Self::find) answers for
    /// one, in a single walk that ends once every one is found.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the answer or the walk cannot be
    /// held.
    pub fn find_each(&self, ids: &[ObjectId]) -> Result<Vec<Option<NodePath>>, DrawingError> {
        let mut wanted = Vec::new();
        if !fallible::reserve(&mut wanted, ids.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        wanted.extend(ids.iter().copied().zip(0..));
        wanted.sort_unstable();
        let mut walk = Walk {
            wanted: &wanted,
            found: fallible::filled(ids.len(), None).ok_or(DrawingError::OutOfMemory)?,
            left: ids.len(),
            path: Vec::new(),
        };
        walk.through(&self.layers)?;
        Ok(walk.found)
    }

    /// The drawing with the node at `path` replaced by `node`, every group
    /// above it made anew around it and everything else shared.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] where nothing stands at `path` or a layer
    /// would stop being a group, and as [`Drawing::new`] otherwise.
    pub fn with_node_at(&self, path: &[usize], node: Arc<Node>) -> Result<Self, DrawingError> {
        let layers = replaced_in(&self.layers, path, node)?;
        self.with_layers(layers)
    }
}

/// Whether `id` names a node within `children`, its path pushed onto `path`
/// when it does.
fn find_in(children: &Children, id: ObjectId, path: &mut NodePath) -> Result<bool, DrawingError> {
    for (index, node) in children.iter().enumerate() {
        if !fallible::push(path, index) {
            return Err(DrawingError::OutOfMemory);
        }
        if node.id() == id {
            return Ok(true);
        }
        if let Some(inner) = node.children() {
            if find_in(inner, id, path)? {
                return Ok(true);
            }
        }
        path.pop();
    }
    Ok(false)
}

/// A search for many nodes at once: the identities wanted, sorted, each
/// with its place in the answer.
struct Walk<'w> {
    wanted: &'w [(ObjectId, usize)],
    found: Vec<Option<NodePath>>,
    left: usize,
    path: NodePath,
}

impl Walk<'_> {
    fn through(&mut self, children: &Children) -> Result<(), DrawingError> {
        for (index, node) in children.iter().enumerate() {
            if self.left == 0 {
                return Ok(());
            }
            if !fallible::push(&mut self.path, index) {
                return Err(DrawingError::OutOfMemory);
            }
            let id = node.id();
            let first = self.wanted.partition_point(|&(wanted, _)| wanted < id);
            for &(_, slot) in self.wanted[first..]
                .iter()
                .take_while(|&&(wanted, _)| wanted == id)
            {
                let held = fallible::copied(&self.path).ok_or(DrawingError::OutOfMemory)?;
                if let Some(answer) = self.found.get_mut(slot).filter(|answer| answer.is_none()) {
                    *answer = Some(held);
                    self.left -= 1;
                }
            }
            if let Some(inner) = node.children() {
                self.through(inner)?;
            }
            self.path.pop();
        }
        Ok(())
    }
}

/// `children` with the node at `path` within them replaced by `node`.
fn replaced_in(
    children: &Children,
    path: &[usize],
    node: Arc<Node>,
) -> Result<Children, DrawingError> {
    let (&index, rest) = path.split_first().ok_or(DrawingError::Invalid)?;
    if rest.is_empty() {
        return children.replaced(index, node);
    }
    let parent = children.get(index).ok_or(DrawingError::Invalid)?;
    let Kind::Group {
        children: inner,
        clip,
    } = parent.kind()
    else {
        return Err(DrawingError::Invalid);
    };
    let rebuilt = replaced_in(inner, rest, node)?;
    let group = parent.with_kind(Kind::Group {
        children: rebuilt,
        clip: clip.clone(),
    })?;
    children.replaced(index, Arc::new(group))
}

#[cfg(test)]
#[path = "drawing_tests.rs"]
mod tests;
