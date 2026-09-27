//! The Wallpaper pane's picture gallery.
//!
//! Settings holds no filesystem capability and no sandbox, so it neither
//! lists the shipped store nor decodes a picture: the desktop session
//! answers both, and this is the model over those answers. It performs no
//! I/O, and every tile's pixels arrive already rendered
//! ([`Gallery::set_picture`]) or refused ([`Gallery::mark_refused`]).
//!
//! A paint draws what has come back and a placeholder for what has not, so
//! the gallery is usable from its first frame and fills in as the answers
//! land.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_browse::layout::{GridFill, GridFlow, GridView};
use tairix_browse::render::grid_metrics;
use tairix_controls::state::{ControlState, FocusState, PointerState, SelectionState};
use tairix_controls::{IconTile, ScrollModel};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconKind, IconPicture};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_wallpaper::{Backdrop, CatalogItem, DesktopSettings, WallpaperChoice, WallpaperPath};

/// The label the "no picture" candidate is drawn under.
pub const NONE_LABEL: &str = "No picture";

/// A candidate picture's lifecycle.
///
/// A candidate whose picture will not arrive shows its built-in glyph and
/// its name — never a blank tile — and a refusal is remembered so a file
/// the desktop cannot render costs one attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Picture {
    /// The "no picture" entry: painted from the backdrop colour in effect,
    /// resolved at paint time because the theme decides it.
    Backdrop,
    /// Not yet asked for, or asked for and not yet answered.
    Pending,
    /// Rendered by the desktop, at the square side the tile asked for.
    Ready(Surface),
    /// The desktop refused this picture once; it is not asked for again.
    Refused,
}

/// One picture the gallery offers.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    /// What choosing this tile sets.
    choice: WallpaperChoice,
    /// The tile's label.
    label: String,
    /// This candidate's position in the desktop's catalog, or `None` for
    /// one the catalog does not hold — the "no picture" entry, and a
    /// picture in effect from outside the shipped store. Neither can be
    /// rendered, because a render names a catalog position and nothing
    /// else.
    catalog: Option<u16>,
    /// The tile's picture.
    picture: Picture,
}

/// What routing one event to the gallery concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GalleryOutcome {
    /// Nothing on screen changed.
    Idle,
    /// The gallery changed and must be re-presented.
    Changed,
    /// The reader chose a picture: the settings the choice implies, for
    /// the caller to render and post.
    Chose(DesktopSettings),
}

/// What one key did to a gallery holding the keyboard cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GalleryKey {
    /// The key means nothing here.
    Idle,
    /// The cursor moved to another tile, which the owner scrolls into view.
    Moved,
    /// The reader chose the tile under the cursor: the settings the choice
    /// implies, for the caller to render and post.
    Chose(DesktopSettings),
    /// The cursor stepped up out of the first line, back to whatever sits
    /// above the gallery.
    Left,
}

/// One picture the caller must ask the desktop to render.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PictureWanted {
    /// The catalog position to render.
    pub index: u16,
    /// The square side to render it at.
    pub side: u16,
}

/// One tile as it is about to be painted: which candidate, where, and the
/// swatch the "no picture" entry draws.
#[derive(Copy, Clone)]
struct Tile<'a> {
    position: usize,
    candidate: &'a Candidate,
    bounds: Rect,
    swatch: Option<&'a Surface>,
}

/// The Wallpaper pane's gallery: the candidates it offers, which one is
/// chosen, the keyboard cursor, and the pointer state over them.
pub struct Gallery {
    candidates: Vec<Candidate>,
    selected: usize,
    /// The tile the keyboard cursor is on, drawn with its ring while the
    /// gallery holds the cursor.
    cursor: usize,
    /// Whether the gallery holds the window's keyboard cursor.
    focused: bool,
    hovered: Option<usize>,
    armed: Option<usize>,
    /// Where the pointer last moved to: a press and a release carry no
    /// position of their own, so the gallery tracks it as every other
    /// surface does.
    pointer: Point,
    /// The settings the chosen tile is written into, so a choice posts the
    /// pinboard document the rest of the pane also edits.
    settings: DesktopSettings,
}

impl Gallery {
    /// The gallery offering `catalog` to a desktop showing `settings`.
    ///
    /// The "no picture" entry always leads, so a plain backdrop is always
    /// one press away. A picture in effect that the catalog does not hold
    /// — one set before it was removed from the store — is appended under
    /// its own leaf name rather than silently dropped, so the pane never
    /// hides the choice that is actually in force.
    #[must_use]
    pub fn new(catalog: &[CatalogItem], settings: &DesktopSettings) -> Self {
        let mut candidates = Vec::with_capacity(catalog.len().saturating_add(2));
        candidates.push(Candidate {
            choice: WallpaperChoice::None,
            label: String::from(NONE_LABEL),
            catalog: None,
            picture: Picture::Backdrop,
        });
        for (at, item) in catalog.iter().enumerate() {
            let (Ok(path), Ok(index)) = (WallpaperPath::new(&item.path()), u16::try_from(at))
            else {
                continue;
            };
            candidates.push(Candidate {
                choice: WallpaperChoice::Image(path),
                label: item.file.clone(),
                catalog: Some(index),
                picture: Picture::Pending,
            });
        }
        let mut gallery = Self {
            candidates,
            selected: 0,
            cursor: 0,
            focused: false,
            hovered: None,
            armed: None,
            pointer: Point::ORIGIN,
            settings: settings.clone(),
        };
        gallery.select_in_effect();
        gallery
    }

    /// Adopt the settings the desktop now holds: the selection follows the
    /// store, so an apply the session refused visibly reverts.
    pub fn adopt(&mut self, settings: &DesktopSettings) {
        self.settings = settings.clone();
        self.candidates
            .retain(|held| held.catalog.is_some() || held.choice == WallpaperChoice::None);
        self.select_in_effect();
    }

    /// Put the selection on the picture in effect, appending it as a
    /// candidate of its own when the catalog does not hold it.
    fn select_in_effect(&mut self) {
        if let Some(at) = self
            .candidates
            .iter()
            .position(|candidate| candidate.choice == self.settings.wallpaper)
        {
            self.selected = at;
            return;
        }
        let WallpaperChoice::Image(ref path) = self.settings.wallpaper else {
            self.selected = 0;
            return;
        };
        self.candidates.push(Candidate {
            choice: self.settings.wallpaper.clone(),
            label: leaf_name(path),
            catalog: None,
            picture: Picture::Refused,
        });
        self.selected = self.candidates.len().saturating_sub(1);
    }

    /// The next picture the caller should ask the desktop to render for
    /// `bounds`, or `None` when every tile has its answer.
    ///
    /// One at a time, because the desktop renders one at a time: the
    /// caller asks again when the answer to this one arrives.
    #[must_use]
    pub fn next_wanted(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<PictureWanted> {
        let side = u16::try_from(self.tile_side(bounds, scale, theme)).ok()?;
        if side == 0 {
            return None;
        }
        let index = self.candidates.iter().find_map(|candidate| {
            matches!(candidate.picture, Picture::Pending)
                .then_some(candidate.catalog)
                .flatten()
        })?;
        Some(PictureWanted { index, side })
    }

    /// Adopt the pixels the desktop rendered for catalog position `index`.
    ///
    /// An answer for a position the gallery does not hold, or whose pixels
    /// are not the square they claim, is dropped: a tile keeps waiting
    /// rather than drawing something of the wrong shape.
    pub fn set_picture(&mut self, index: u16, side: u16, pixels: &[u8]) -> bool {
        let Some(candidate) = self.at_catalog(index) else {
            return false;
        };
        let Some(surface) = Surface::from_rgba8(u32::from(side), u32::from(side), pixels) else {
            candidate.picture = Picture::Refused;
            return true;
        };
        candidate.picture = Picture::Ready(surface);
        true
    }

    /// Record that the desktop refused catalog position `index`, so it is
    /// never asked for again and its tile stops waiting.
    pub fn mark_refused(&mut self, index: u16) -> bool {
        match self.at_catalog(index) {
            Some(candidate) => {
                candidate.picture = Picture::Refused;
                true
            }
            None => false,
        }
    }

    /// Ask every catalog tile for its picture again — what a scale or
    /// theme change costs, since a rendered picture is square at one side
    /// only.
    ///
    /// A refusal is *not* retried: a picture the desktop could not render
    /// at one side it cannot render at another either, and asking again
    /// every time the scale moves would be the retry loop the charter
    /// forbids.
    pub fn invalidate_pictures(&mut self) {
        for candidate in &mut self.candidates {
            if matches!(candidate.picture, Picture::Ready(_)) {
                candidate.picture = Picture::Pending;
            }
        }
    }

    fn at_catalog(&mut self, index: u16) -> Option<&mut Candidate> {
        self.candidates
            .iter_mut()
            .find(|candidate| candidate.catalog == Some(index))
    }

    /// The square side a tile in `bounds` draws its picture at.
    #[must_use]
    pub fn tile_side(&self, bounds: Rect, scale: Scale, theme: &Theme) -> u32 {
        let metrics = grid_metrics(scale, theme);
        IconTile::icon_side(
            Rect::new(0, 0, metrics.cell_width, metrics.cell_height),
            scale,
            theme,
        )
        .min(u32::from(
            tairix_abi::window_ipc::WINDOW_WALLPAPER_PREVIEW_MAX_SIDE,
        ))
        .min(bounds.width.max(1))
    }

    /// The grid the tiles are laid out in within `bounds`.
    fn grid(&self, bounds: Rect, scale: Scale, theme: &Theme) -> GridView {
        GridView::new(
            bounds,
            grid_metrics(scale, theme),
            0,
            self.candidates.len(),
            GridFlow::RowsFromLeading,
            GridFill::Spread,
        )
    }

    /// The scroll model the gallery moves through in `bounds`, in pixels,
    /// stepping a line of tiles and the gap after it a line.
    #[must_use]
    pub fn scroll_model(
        &self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        offset: u64,
    ) -> ScrollModel {
        self.grid(bounds, scale, theme).scroll_model(offset)
    }

    /// Draw the gallery into `surface` at `bounds`, scrolled `offset`
    /// pixels.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        offset: u64,
        scale: Scale,
        theme: &Theme,
    ) {
        let grid = self.grid(bounds, scale, theme);
        let swatch = self.backdrop_swatch(bounds, scale, theme);
        grid.view(offset).paint(surface, |surface| {
            for position in grid.visible_range(offset) {
                let (Some(rect), Some(candidate)) =
                    (grid.cell_rect(position), self.candidates.get(position))
                else {
                    continue;
                };
                self.render_tile(
                    surface,
                    Tile {
                        position,
                        candidate,
                        bounds: rect,
                        swatch: swatch.as_ref(),
                    },
                    scale,
                    theme,
                );
            }
        });
    }

    fn render_tile(&self, surface: &mut Surface, tile: Tile<'_>, scale: Scale, theme: &Theme) {
        let Tile {
            position,
            candidate,
            bounds,
            swatch,
        } = tile;
        let selected = position == self.selected;
        let focused = self.focused && position == self.cursor;
        let pointer = if self.armed == Some(position) {
            PointerState::Pressed
        } else if self.hovered == Some(position) {
            PointerState::Hover
        } else {
            PointerState::None
        };
        let state = ControlState::idle()
            .with_pointer(pointer)
            .with_selection(if selected {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            })
            .with_focus(FocusState {
                focused,
                in_focus_field: self.focused,
            });
        let artwork = match &candidate.picture {
            Picture::Ready(pixels) => Some(pixels),
            Picture::Backdrop => swatch,
            Picture::Pending | Picture::Refused => None,
        };
        IconTile::new(candidate.label.clone(), IconKind::Image)
            .with_state(state)
            .render(
                surface,
                bounds,
                scale,
                theme,
                artwork.map(IconPicture::Artwork),
            );
    }

    /// The flat swatch the "no picture" tile draws, in the colour the
    /// backdrop is actually set to.
    fn backdrop_swatch(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<Surface> {
        let side = self.tile_side(bounds, scale, theme);
        if side == 0 {
            return None;
        }
        let colour = match self.settings.backdrop {
            Backdrop::Theme => Color::from(theme.palette().desktop),
            Backdrop::Colour(rgb) => Color::rgb(rgb.r, rgb.g, rgb.b),
        };
        Surface::filled(side, side, colour.premultiply())
    }

    /// Route one pointer event over the gallery at `bounds`, scrolled
    /// `offset` pixels, reporting the tiles whose look it changed.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        offset: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> GalleryOutcome {
        if let InputEvent::PointerMoved { to } = *event {
            self.pointer = to;
        }
        let grid = self.grid(bounds, scale, theme);
        // The pointer and the grid are both in the window's space: the grid
        // hit-tests through its own scrolled view.
        let over = grid.index_at(offset, self.pointer);
        let mut mark = |tile: Option<usize>| {
            if let Some(shown) = tile.and_then(|tile| grid.shown_rect(offset, tile)) {
                damage.add(shown);
            }
        };
        match *event {
            InputEvent::PointerMoved { .. } => {
                if over == self.hovered {
                    return GalleryOutcome::Idle;
                }
                mark(self.hovered);
                mark(over);
                self.hovered = over;
                GalleryOutcome::Changed
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if over.is_none() {
                    return GalleryOutcome::Idle;
                }
                self.armed = over;
                mark(over);
                GalleryOutcome::Changed
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let Some(armed) = self.armed.take() else {
                    return GalleryOutcome::Idle;
                };
                mark(Some(armed));
                // A press released away from the tile it started on does
                // nothing, exactly as every shared control behaves.
                if over != Some(armed) {
                    return GalleryOutcome::Changed;
                }
                let Some(candidate) = self.candidates.get(armed) else {
                    return GalleryOutcome::Changed;
                };
                mark(Some(self.selected));
                mark(Some(self.cursor));
                self.selected = armed;
                self.cursor = armed;
                self.settings.wallpaper = candidate.choice.clone();
                GalleryOutcome::Chose(self.settings.clone())
            }
            _ => GalleryOutcome::Idle,
        }
    }

    /// Give the gallery the keyboard cursor, on the chosen tile, or take it
    /// away, reporting the tile its ring leaves or lands on.
    pub fn set_focused(
        &mut self,
        focused: bool,
        (bounds, offset): (Rect, u64),
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if focused && !self.focused {
            self.cursor = self.selected;
        }
        if focused != self.focused {
            self.focused = focused;
            let grid = self.grid(bounds, scale, theme);
            if let Some(shown) = grid.shown_rect(offset, self.cursor) {
                damage.add(shown);
            }
        }
    }

    /// Whether the gallery holds the window's keyboard cursor.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Route one key to a gallery holding the keyboard cursor in `bounds`,
    /// scrolled `offset` pixels: the arrows walk the tiles a tile or a line
    /// at a time, Page Up and Page Down a band of lines, Home and End go to
    /// the ends, and Enter or Space chooses the tile the cursor is on.
    pub fn on_key(
        &mut self,
        key: Key,
        (bounds, offset): (Rect, u64),
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> GalleryKey {
        if !self.focused || self.candidates.is_empty() {
            return GalleryKey::Idle;
        }
        let grid = self.grid(bounds, scale, theme);
        let across = grid.cells_per_line().max(1);
        let page = across.saturating_mul(whole_lines(&grid).max(1));
        let last = self.candidates.len() - 1;
        let at = self.cursor;
        let to = match key {
            Key::Named(NamedKey::Left) => at.saturating_sub(1),
            Key::Named(NamedKey::Right) => at.saturating_add(1).min(last),
            Key::Named(NamedKey::Up) if at < across => return GalleryKey::Left,
            Key::Named(NamedKey::Up) => at - across,
            Key::Named(NamedKey::Down) => at.saturating_add(across).min(last),
            Key::Named(NamedKey::PageUp) => at.saturating_sub(page),
            Key::Named(NamedKey::PageDown) => at.saturating_add(page).min(last),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => last,
            Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                return self.choose_cursor(&grid, offset, damage)
            }
            _ => return GalleryKey::Idle,
        };
        if to == at {
            return GalleryKey::Idle;
        }
        for tile in [at, to] {
            if let Some(shown) = grid.shown_rect(offset, tile) {
                damage.add(shown);
            }
        }
        self.cursor = to;
        GalleryKey::Moved
    }

    /// Choose the tile the cursor is on, as a press released on it would.
    fn choose_cursor(&mut self, grid: &GridView, offset: u64, damage: &mut Region) -> GalleryKey {
        let Some(candidate) = self.candidates.get(self.cursor) else {
            return GalleryKey::Idle;
        };
        for tile in [self.selected, self.cursor] {
            if let Some(shown) = grid.shown_rect(offset, tile) {
                damage.add(shown);
            }
        }
        self.selected = self.cursor;
        self.settings.wallpaper = candidate.choice.clone();
        GalleryKey::Chose(self.settings.clone())
    }

    /// The offset that shows the whole of the cursor's tile in `bounds` from
    /// `offset`, moving the least.
    #[must_use]
    pub fn reveal(&self, bounds: Rect, offset: u64, scale: Scale, theme: &Theme) -> u64 {
        self.grid(bounds, scale, theme)
            .reveal(offset, Some(self.cursor))
    }

    /// How many candidates the gallery offers, for a test or a caller
    /// sizing its column.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether the gallery offers nothing at all, which the always-present
    /// "no picture" entry rules out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Which candidate is chosen.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Which candidate the keyboard cursor is on.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Which tile the pointer lights.
    #[cfg(test)]
    pub(crate) const fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// What the window shows of tile `index` in `bounds` scrolled `offset`
    /// pixels.
    #[cfg(test)]
    pub(crate) fn shown_rect(
        &self,
        index: usize,
        (bounds, offset): (Rect, u64),
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        self.grid(bounds, scale, theme).shown_rect(offset, index)
    }
}

impl GalleryOutcome {
    /// Whether the gallery must be re-presented.
    #[must_use]
    pub const fn changed(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// How many whole lines of tiles `grid` shows at once: a page of the cursor's
/// walk.
fn whole_lines(grid: &GridView) -> usize {
    let line = grid.scroll_model(0).line_step();
    let seen = grid.scroll_range(0).viewport_extent();
    usize::try_from(seen.checked_div(line).unwrap_or(0)).unwrap_or(usize::MAX)
}

/// The last path segment of `path`, for a candidate the catalog does not
/// hold.
fn leaf_name(path: &WallpaperPath) -> String {
    path.as_str()
        .rsplit('/')
        .next()
        .unwrap_or(path.as_str())
        .to_string()
}

#[cfg(test)]
#[path = "gallery_tests.rs"]
mod tests;
