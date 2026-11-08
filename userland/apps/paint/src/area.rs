//! The sprite file window (`plans/PAINT.md` PT32): a sprite area shown as its
//! sprites, each its thumbnail over its name, where sprites are made, renamed,
//! copied, deleted and reordered, and from which each opens in a window of its
//! own (PT33).
//!
//! Thumbnails are drawn on the queue's worker from the sprite as it stands and
//! kept until it changes; a paint draws what has landed and the picture glyph
//! meanwhile, so it reads nothing. Each sprite is drawn as the file manager
//! draws a file, through the shared icon tile.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::fs::Filetype;
use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{
    AppMenu, AppMenuItemId, AppMenuSymbol as S, CursorShape, SaveFormats,
};
use tairix_controls::{
    fill_area, withheld, ControlState, IconTile, Keystroke, ScrollAction, ScrollBar, ScrollModel,
    ScrollOrientation, ScrollRange, SelectionState, StatusBand, StatusTone, TileLayout,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconKind, IconPicture};
use tairix_input::{
    ClickKind, DoubleClickTracker, InputEvent, Key, Modifiers, NamedKey, PointerButton,
};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_window::docapp::{DialogWindow, Relayout, SaveTarget};
use tairix_window::document::{Access, SavedDocument};
use tairix_window::menu::{MenuBuilder, Plate};

use crate::dialog::{Answer, Form, NewSprite, Purpose};
use crate::document::{
    free_name, sprite_picture, Document, Entry, EntryId, ListRefusal, Snapshot, NAME_REFUSAL,
    SPRITE_STEM,
};
use crate::save::{format_for, natural, save_formats};
use crate::view::{
    Action, Compute, Computed, MenuKind, Outcome, Own, Request, APP_TITLE, RENAME_ENTRY,
    SPRITE_SIZE,
};
use tairix_image::SpriteName;

/// A thumbnail's side, in logical pixels.
const THUMB: u32 = 64;

/// Room between cells, in logical pixels.
const GAP: u32 = 6;

/// How a sprite's cell divides: its thumbnail above two lines of its name, as
/// a file's large tile does in the file manager.
const TILE: TileLayout = TileLayout::new(2);

/// Most thumbnails one job draws, so the sprites in view land first and a
/// large file never holds the queue for long.
const THUMBS_PER_JOB: usize = 32;

/// The client width a sprite window opens at, in logical pixels.
pub const OPEN_WIDTH: u32 = 250;

/// The tallest client a sprite window opens at, in logical pixels; a longer
/// list scrolls.
pub const OPEN_HEIGHT_MAX: u32 = 250;

/// The most sprite windows one choice opens or brings forward: the desktop
/// is asked for no more at once, however many sprites are chosen.
pub const SPRITES_AT_ONCE: usize = 16;

/// How opening the windows of a choice of sprites goes: no more than
/// [`SPRITES_AT_ONCE`], and none after the desktop refuses one, which says
/// why itself.
#[derive(Debug)]
pub struct SpriteOpening {
    asked: usize,
    opened: usize,
    short: usize,
    refused: bool,
}

impl SpriteOpening {
    /// An opening of `asked` sprites' windows.
    #[must_use]
    pub const fn of(asked: usize) -> Self {
        Self {
            asked,
            opened: 0,
            short: 0,
            refused: false,
        }
    }

    /// Whether another window is to be opened.
    #[must_use]
    pub const fn goes_on(&self) -> bool {
        !self.refused && self.opened < SPRITES_AT_ONCE
    }

    /// A window opened, or brought forward.
    pub const fn opened(&mut self) {
        self.opened += 1;
    }

    /// Memory refused a sprite its window.
    pub const fn short(&mut self) {
        self.short += 1;
    }

    /// The desktop refused a window.
    pub const fn refused(&mut self) {
        self.refused = true;
    }

    /// What the sprite file window says of it, once it is over.
    #[must_use]
    pub fn said(&self) -> Option<String> {
        let left = self.asked.saturating_sub(self.opened + self.short);
        match (self.short, left) {
            (0, 0) => None,
            // The desktop's own alert said why.
            _ if self.refused && self.short == 0 => None,
            (0, left) => Some(alloc::format!(
                "{SPRITES_AT_ONCE} sprites open at once: {left} more chosen stay shut"
            )),
            (1, _) => Some(String::from("There is not enough memory to open a sprite")),
            (short, _) => Some(alloc::format!(
                "There is not enough memory to open {short} sprites"
            )),
        }
    }
}

/// Where the sprite file window draws, for one window size: by default
/// every part empty, as for a window not yet laid out.
#[derive(Clone, Debug, Default)]
pub struct AreaLayout {
    window: Rect,
    grid: Rect,
    bar: Rect,
    status: Rect,
    /// A cell's width and height.
    cell: (u32, u32),
    thumb: u32,
    gap: u32,
    columns: usize,
    rows: usize,
}

impl AreaLayout {
    /// The status band, where what the window is told is shown.
    #[must_use]
    pub const fn status(&self) -> Rect {
        self.status
    }

    /// The thumbnail side this layout draws at, in physical pixels.
    #[must_use]
    pub const fn thumb(&self) -> u32 {
        self.thumb
    }

    /// The scroll bar's rectangle; empty when every sprite shows.
    #[must_use]
    pub const fn bar(&self) -> Rect {
        self.bar
    }

    /// The grid's rectangle.
    #[must_use]
    pub const fn grid(&self) -> Rect {
        self.grid
    }

    /// Where a dialog of the window's is laid out: below its own area.
    #[must_use]
    pub fn dialog_origin(&self) -> Point {
        Point::new(self.window.left(), self.window.bottom())
    }

    /// The content's height: every row and the gaps about them.
    fn content_height(&self) -> u32 {
        u32::try_from(self.rows)
            .unwrap_or(u32::MAX)
            .saturating_mul(self.cell.1 + self.gap)
            .saturating_add(self.gap)
    }

    /// Cell `index`'s rectangle in the window, scrolled `offset` down.
    #[must_use]
    pub fn cell_rect(&self, index: usize, offset: u32) -> Rect {
        let columns = self.columns.max(1);
        let (column, row) = (index % columns, index / columns);
        let (column, row) = (
            u32::try_from(column).unwrap_or(u32::MAX),
            u32::try_from(row).unwrap_or(u32::MAX),
        );
        let x = self
            .grid
            .left()
            .saturating_add_unsigned(self.gap + column * (self.cell.0 + self.gap));
        let y = self
            .grid
            .top()
            .saturating_add_unsigned(self.gap + row.saturating_mul(self.cell.1 + self.gap))
            .saturating_sub_unsigned(offset);
        Rect::new(x, y, self.cell.0, self.cell.1)
    }

    /// The cell of the `count` sprites under `point`, scrolled `offset` down.
    #[must_use]
    pub fn index_at(&self, point: Point, offset: u32, count: usize) -> Option<usize> {
        if !self.grid.contains(point) {
            return None;
        }
        let columns = self.columns.max(1);
        let x = u32::try_from(point.x - self.grid.left()).ok()?;
        let y = u32::try_from(point.y - self.grid.top()).ok()? + offset;
        let column = usize::try_from(x.checked_sub(self.gap)? / (self.cell.0 + self.gap)).ok()?;
        let row = usize::try_from(y.checked_sub(self.gap)? / (self.cell.1 + self.gap)).ok()?;
        let index = row.checked_mul(columns)?.checked_add(column)?;
        (column < columns && index < count && self.cell_rect(index, offset).contains(point))
            .then_some(index)
    }

    /// The sprites, of `count`, whose cells show scrolled `offset` down.
    #[must_use]
    pub fn shown(&self, offset: u32, count: usize) -> Range<usize> {
        let pitch = self.cell.1 + self.gap;
        let columns = self.columns.max(1);
        let first = (offset / pitch.max(1)) as usize * columns;
        let last = ((offset + self.grid.height) / pitch.max(1) + 1) as usize * columns;
        first.min(count)..last.min(count)
    }
}

/// A thumbnail as it landed: the sprite's content revision and the side it
/// was drawn for, and its picture where one could be drawn.
#[derive(Debug)]
struct Thumb {
    revision: u64,
    side: u32,
    surface: Option<Surface>,
}

/// The sprite file window's engine.
#[derive(Debug)]
pub struct AreaView {
    document: Document,
    name: String,
    /// The filetype the volume records for the file, for as long as that is
    /// the file it is saved to.
    recorded: Option<Filetype>,
    access: Access,
    double_click: Duration64,
    /// The sprites chosen, by identity.
    chosen: Vec<EntryId>,
    /// The sprite a Shift press or arrow runs from.
    anchor: Option<EntryId>,
    /// The sprite the keyboard is on.
    cursor: Option<EntryId>,
    offset: u32,
    scroll: ScrollBar,
    clicks: DoubleClickTracker,
    pointer: Point,
    modifiers: Modifiers,
    focused: bool,
    thumbs: BTreeMap<EntryId, Thumb>,
    /// Each sprite's content revision, raised as a window of its own hands
    /// back a change, so its thumbnail is drawn again.
    revisions: BTreeMap<EntryId, u64>,
    /// The thumbnail job in flight.
    asked: Option<u64>,
    next_job: u64,
    /// The side thumbnails are drawn at, as last laid out.
    side: u32,
    /// The picture glyph a sprite shows until its thumbnail lands, drawn
    /// once for that side rather than on every paint.
    placeholder: Option<Surface>,
    /// The sprites in view, as last laid out, drawn first.
    in_view: Range<usize>,
    /// Every sprite near the view has its thumbnail at the side drawn, so
    /// nothing need be looked for until the sprites, the view or the side
    /// change.
    fresh: bool,
    /// The file's generation when its thumbnails were last looked for.
    seen: u64,
    modal: Option<Form>,
    message: Option<String>,
    /// The sprites to be opened in windows of their own as soon as it shows:
    /// a new file's.
    pending: Vec<EntryId>,
    /// How many times the file has been renamed.
    renamed: u64,
    /// What its sprite windows were last brought into line with: the
    /// file's generation, whether it held changes, and its renames.
    synced: Option<(u64, bool, u64)>,
    /// Where everything it draws goes, for its window's size.
    layout: AreaLayout,
}

impl AreaView {
    /// The window of `document`, a sprite area, read from the file `name`
    /// whose volume records `recorded`, written as `access` allows, its
    /// presses paired under `double_click`.
    #[must_use]
    pub fn new(
        document: Document,
        name: String,
        recorded: Option<Filetype>,
        access: Access,
        double_click: Duration64,
    ) -> Self {
        let cursor = document.ids().first().copied();
        Self {
            document,
            name,
            recorded,
            access,
            double_click,
            chosen: cursor.into_iter().collect(),
            anchor: cursor,
            cursor,
            offset: 0,
            scroll: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::new(0, 0, 0), 1),
            ),
            clicks: DoubleClickTracker::new(),
            pointer: Point::new(0, 0),
            modifiers: Modifiers::default(),
            focused: false,
            thumbs: BTreeMap::new(),
            revisions: BTreeMap::new(),
            asked: None,
            next_job: 0,
            side: 0,
            placeholder: None,
            in_view: 0..0,
            fresh: false,
            seen: 0,
            modal: None,
            message: None,
            pending: Vec::new(),
            renamed: 0,
            synced: None,
            layout: AreaLayout::default(),
        }
    }

    /// The window, its every sprite opened in a window of its own as soon as
    /// it shows: what a new file is.
    #[must_use]
    pub fn opening_its_sprites(mut self) -> Self {
        self.pending = self.document.ids().to_vec();
        self
    }

    /// The sprites to be opened now, taken.
    pub fn take_pending(&mut self) -> Vec<EntryId> {
        core::mem::take(&mut self.pending)
    }

    /// Whether what its sprite windows show of the file — its sprites'
    /// names, whether it holds changes, its name — moved since this was last
    /// asked.
    pub fn members_moved(&mut self) -> bool {
        let now = (
            self.document.generation(),
            self.document.is_modified(),
            self.renamed,
        );
        let moved = self.synced != Some(now);
        self.synced = Some(now);
        moved
    }

    /// The sprite area.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    /// The file's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The sprites chosen.
    #[must_use]
    pub fn chosen(&self) -> &[EntryId] {
        &self.chosen
    }

    /// What the window was told last.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The form the window asks in, while one is up.
    #[must_use]
    pub const fn modal(&self) -> Option<&Form> {
        self.modal.as_ref()
    }

    /// How far down the grid is scrolled, in pixels.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Whether the window has the keyboard.
    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    /// The scroll bar.
    #[must_use]
    pub const fn scroll(&self) -> &ScrollBar {
        &self.scroll
    }

    /// Whether `id` is chosen.
    #[must_use]
    pub fn is_chosen(&self, id: EntryId) -> bool {
        self.chosen.contains(&id)
    }

    /// The sprite the keyboard is on.
    #[must_use]
    pub const fn cursor(&self) -> Option<EntryId> {
        self.cursor
    }

    /// The thumbnail drawn of `id` at `side`, where one has landed: one of
    /// another side waits for its own to be drawn.
    #[must_use]
    pub fn thumbnail(&self, id: EntryId, side: u32) -> Option<&Surface> {
        self.thumbs
            .get(&id)
            .filter(|thumb| thumb.side == side)?
            .surface
            .as_ref()
    }

    /// The name sprite `id` is called by, where it is here.
    #[must_use]
    pub fn sprite_name(&self, id: EntryId) -> Option<SpriteName> {
        let index = self.document.index_of(id)?;
        self.document.entries()[index].name().copied()
    }

    /// Where everything it draws goes, for its window's size.
    #[must_use]
    pub const fn layout(&self) -> &AreaLayout {
        &self.layout
    }

    /// Lay the window out for a `width`×`height` window, reporting what
    /// moved.
    pub fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        damage: &mut Region,
    ) {
        self.layout = self.layout_for(width, height, theme, scale);
        self.settle(damage);
    }

    /// The layout of a `width`×`height` window.
    fn layout_for(&self, width: u32, height: u32, theme: &Theme, scale: Scale) -> AreaLayout {
        let gap = scale.scale_length(GAP).max(1);
        let thumb = scale.scale_length(THUMB).max(1);
        let status_height = StatusBand::new(theme, scale).height();
        let cell = cell_size(thumb, scale, theme);
        let window = Rect::new(0, 0, width, height);
        let status = Rect::new(
            0,
            i32::try_from(height.saturating_sub(status_height)).unwrap_or(0),
            width,
            status_height.min(height),
        );
        let area_height = height.saturating_sub(status.height);
        let count = self.document.entries().len();
        let GridFit {
            columns,
            rows,
            bar: bar_width,
        } = grid_fit(
            width,
            area_height,
            count,
            cell,
            gap,
            bar_breadth(scale, theme),
        );
        AreaLayout {
            window,
            grid: Rect::new(0, 0, width.saturating_sub(bar_width), area_height),
            bar: Rect::new(
                i32::try_from(width.saturating_sub(bar_width)).unwrap_or(0),
                0,
                bar_width,
                area_height,
            ),
            status,
            cell,
            thumb,
            gap,
            columns,
            rows,
        }
    }

    /// The smallest window a sprite file is shown in: one cell and its band.
    #[must_use]
    pub fn min_size(theme: &Theme, scale: Scale) -> (u32, u32) {
        let gap = scale.scale_length(GAP).max(1);
        let cell = cell_size(scale.scale_length(THUMB).max(1), scale, theme);
        let bar = bar_breadth(scale, theme);
        (
            cell.0 + gap * 2 + bar,
            cell.1 + gap * 2 + StatusBand::new(theme, scale).height(),
        )
    }

    /// The window's size in logical pixels to open at, in `theme` at the
    /// reference scale: [`OPEN_WIDTH`] across and as tall as
    /// its sprites need at that width, at most [`OPEN_HEIGHT_MAX`], and never
    /// below [`Self::min_size`].
    #[must_use]
    pub fn open_size(count: usize, theme: &Theme) -> (u32, u32) {
        let cell = cell_size(THUMB, Scale::ONE, theme);
        let band = StatusBand::new(theme, Scale::ONE).height();
        let room = OPEN_HEIGHT_MAX.saturating_sub(band);
        let fit = grid_fit(
            OPEN_WIDTH,
            room,
            count.max(1),
            cell,
            GAP,
            bar_breadth(Scale::ONE, theme),
        );
        let rows = u32::try_from(fit.rows).unwrap_or(u32::MAX);
        let needed = rows
            .saturating_mul(cell.1 + GAP)
            .saturating_add(GAP)
            .saturating_add(band);
        let least = Self::min_size(theme, Scale::ONE);
        (
            OPEN_WIDTH.max(least.0),
            needed.min(OPEN_HEIGHT_MAX).max(least.1),
        )
    }

    /// Bring the scroll and what is in view into line with its layout.
    fn settle(&mut self, damage: &mut Region) {
        let content = u64::from(self.layout.content_height());
        let viewport = u64::from(self.layout.grid.height);
        let most = content.saturating_sub(viewport);
        let offset = u64::from(self.offset).min(most);
        self.offset = u32::try_from(offset).unwrap_or(0);
        self.scroll.set_model(ScrollModel::in_pixels(
            ScrollRange::new(content, viewport, offset),
            u64::from(self.layout.cell.1 + self.layout.gap),
        ));
        let count = self.document.entries().len();
        let in_view = self.layout.shown(self.offset, count);
        let generation = self.document.generation();
        if self.layout.thumb != self.side || in_view != self.in_view || generation != self.seen {
            self.fresh = false;
            self.seen = generation;
        }
        if self.layout.thumb != self.side {
            self.placeholder = tairix_icon::builtin_picture(IconKind::Image, self.layout.thumb);
        }
        self.side = self.layout.thumb;
        self.in_view = in_view;
        self.forget_far_thumbnails();
        damage.add(self.layout.window);
    }

    /// The sprites a thumbnail is drawn for: those in view and a view's
    /// worth either side, so a large file costs what its neighbourhood does.
    fn near(&self) -> Range<usize> {
        let span = self.in_view.len().max(1);
        let count = self.document.entries().len();
        self.in_view.start.saturating_sub(span)..(self.in_view.end + span).min(count)
    }

    /// Let go of the thumbnails of sprites twice as far off as [`near`]
    /// reaches, once more are held than it covers.
    ///
    /// [`near`]: Self::near
    fn forget_far_thumbnails(&mut self) {
        let span = self.in_view.len().max(1) * 2;
        if self.thumbs.len() <= span * 3 {
            return;
        }
        let ids = self.document.ids();
        let far = self.in_view.start.saturating_sub(span)..(self.in_view.end + span).min(ids.len());
        let kept = &ids[far];
        self.thumbs.retain(|id, _| kept.contains(id));
    }

    /// The pointer's shape over `at`.
    #[must_use]
    pub const fn cursor_shape(&self, _at: Point) -> CursorShape {
        CursorShape::Arrow
    }

    /// The window's title over `title`: the file's name, marked as any
    /// document window's is.
    pub fn write_title(&self, title: &mut String) {
        tairix_browse::vfs::write_document_title(
            title,
            &self.name,
            self.document.is_modified(),
            self.access == Access::ReadOnly,
            APP_TITLE,
        );
    }

    /// Tell the user `message`.
    pub fn say(&mut self, message: String) {
        self.message = Some(message);
    }

    fn state(&mut self, message: &str, damage: &mut Region) {
        self.message = Some(String::from(message));
        damage.add(self.layout.status);
    }

    /// What was said before an act is done with; what the act says itself
    /// stays.
    fn unsay(&mut self, damage: &mut Region) {
        if self.message.take().is_some() {
            damage.add(self.layout.status);
        }
    }

    /// The window gained or lost the keyboard.
    pub fn focus_changed(&mut self, focused: bool, damage: &mut Region) {
        self.focused = focused;
        self.damage_cursor(damage);
    }

    /// The rows the window's menu opens with.
    #[must_use]
    pub fn menu(&self, kind: MenuKind) -> AppMenu {
        let mut menu = MenuBuilder::titled(APP_TITLE);
        if kind == MenuKind::Window {
            self.window_rows(&mut menu);
        }
        menu.finish()
    }

    fn window_rows(&self, menu: &mut MenuBuilder) {
        let one = self.single();
        let chosen = !self.chosen.is_empty();
        let pictures = self.chosen_pictures().next().is_some();
        let count = self.document.entries().len();
        let root = Plate::Root;
        menu.item(Action::EditEntry, S::Open, "Edit", "Enter", pictures, root);
        menu.separator(root);
        menu.item(
            Action::NewEntry,
            S::NewDocument,
            "New sprite\u{2026}",
            "",
            true,
            root,
        );
        let name = one
            .and_then(|index| self.document.entries()[index].name())
            .map_or_else(String::new, ToString::to_string);
        menu.entry(
            Action::Rename,
            RENAME_ENTRY,
            S::Rename,
            "Rename\u{2026}",
            &name,
            root,
        );
        menu.item(
            Action::DuplicateEntry,
            S::Duplicate,
            "Duplicate",
            "",
            pictures,
            root,
        );
        menu.destructive(
            Action::DeleteEntry,
            S::Delete,
            "Delete",
            "Delete",
            chosen && self.chosen.len() < count,
            root,
        );
        let at = one.unwrap_or(0);
        menu.item(
            Action::EntryUp,
            S::MoveUp,
            "Move up",
            "",
            one.is_some() && at > 0,
            root,
        );
        menu.item(
            Action::EntryDown,
            S::MoveDown,
            "Move down",
            "",
            one.is_some() && at + 1 < count,
            root,
        );
        menu.separator(root);
        let (undo, redo) = self.document.can_undo_redo();
        menu.item(Action::Undo, S::Undo, "Undo", "Ctrl+Z", undo, root);
        menu.item(Action::Redo, S::Redo, "Redo", "Ctrl+Shift+Z", redo, root);
        menu.item(
            Action::SelectAll,
            S::SelectAll,
            "Select all",
            "Ctrl+A",
            count > 0,
            root,
        );
        menu.item(
            Action::Deselect,
            S::SelectNone,
            "Clear selection",
            "",
            chosen,
            root,
        );
        menu.separator(root);
        if let Some(file) = menu.submenu("File", S::File, root) {
            menu.item(Action::Open, S::Open, "Open\u{2026}", "Ctrl+O", true, file);
            menu.separator(file);
            menu.item(Action::Save, S::Save, "Save", "Ctrl+S", true, file);
            menu.item(
                Action::SaveAs,
                S::SaveAs,
                "Save as\u{2026}",
                "Ctrl+Shift+S",
                true,
                file,
            );
            menu.separator(file);
            menu.item(Action::Close, S::Close, "Close", "Ctrl+W", true, file);
        }
    }

    /// The menu row `item` was chosen.
    pub fn chosen_row(&mut self, item: AppMenuItemId, damage: &mut Region) -> Outcome {
        match Action::from_id(item.get()) {
            Some(action) => self.act(action, damage),
            None => Outcome::none(),
        }
    }

    /// The menu's entry field `item` was committed holding `text`.
    pub fn entered(&mut self, item: AppMenuItemId, text: &str, damage: &mut Region) -> Outcome {
        if item.get() != RENAME_ENTRY {
            return Outcome::none();
        }
        self.unsay(damage);
        let Some(index) = self.single() else {
            self.state("Choose one sprite to rename", damage);
            return Outcome::none();
        };
        let Some(name) = SpriteName::new(text.trim()) else {
            self.state(NAME_REFUSAL, damage);
            return Outcome::none();
        };
        if self.document.entries()[index].name() == Some(&name) {
            return Outcome::none();
        }
        if matches!(self.document.entries()[index], Entry::Kept(_)) {
            self.state("A kept sprite keeps its own name", damage);
            return Outcome::none();
        }
        let renamed = self.document.rename(index, name);
        self.list_changed(renamed, damage)
    }

    /// Carry out `action`.
    fn act(&mut self, action: Action, damage: &mut Region) -> Outcome {
        self.unsay(damage);
        match action {
            Action::EditEntry => self.open_chosen(damage),
            Action::NewEntry => {
                let name = SpriteName::new(SPRITE_STEM)
                    .and_then(|stem| free_name(&stem, self.document.names()))
                    .map_or_else(String::new, |name| name.to_string());
                self.modal = Some(Form::new_sprite(&name, SPRITE_SIZE));
                Outcome::none()
            }
            Action::DuplicateEntry => self.duplicate(damage),
            Action::DeleteEntry => self.delete(damage),
            Action::EntryUp | Action::EntryDown => self.reorder(action == Action::EntryUp, damage),
            Action::Undo => {
                let undone = self.document.undo();
                self.history_moved(undone.is_ok(), "There is nothing to undo", damage)
            }
            Action::Redo => {
                let redone = self.document.redo();
                self.history_moved(redone.is_ok(), "There is nothing to redo", damage)
            }
            Action::SelectAll => {
                self.chosen = self.document.ids().to_vec();
                damage.add(self.layout.grid);
                Outcome::none()
            }
            Action::Deselect => {
                self.chosen.clear();
                damage.add(self.layout.grid);
                Outcome::none()
            }
            Action::Open => Outcome::asking(Request::Open),
            Action::Save => Outcome::asking(Request::Save),
            Action::SaveAs => Outcome::asking(Request::SaveAs),
            Action::Close => self.close_requested(damage),
            _ => Outcome::none(),
        }
    }

    /// The user asked to close the window: asked first where the file holds
    /// changes, which every sprite window's are.
    pub fn close_requested(&mut self, _damage: &mut Region) -> Outcome {
        if self.document.is_modified() {
            Outcome::asking(Request::AskToClose)
        } else {
            Outcome::asking(Request::Close)
        }
    }

    /// The chosen sprites that are pictures, each one a window can show.
    fn chosen_pictures(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.chosen.iter().copied().filter(|&id| {
            self.document
                .index_of(id)
                .is_some_and(|index| matches!(self.document.entries()[index], Entry::Picture(_)))
        })
    }

    /// The one sprite chosen, where exactly one is.
    fn single(&self) -> Option<usize> {
        match self.chosen.as_slice() {
            [only] => self.document.index_of(*only),
            _ => None,
        }
    }

    fn open_chosen(&mut self, damage: &mut Region) -> Outcome {
        let ids: Vec<EntryId> = self.chosen_pictures().collect();
        if ids.is_empty() {
            let why = if self.chosen.is_empty() {
                "Choose a sprite to edit"
            } else {
                "A kept sprite cannot be edited"
            };
            self.state(why, damage);
            return Outcome::none();
        }
        Outcome::asking(Request::Own(Own::OpenSprites(ids)))
    }

    fn duplicate(&mut self, damage: &mut Region) -> Outcome {
        let chosen: Vec<EntryId> = self.chosen_pictures().collect();
        let mut made = Vec::new();
        for id in chosen {
            let Some(index) = self.document.index_of(id) else {
                continue;
            };
            let Entry::Picture(picture) = &self.document.entries()[index] else {
                continue;
            };
            let name = picture
                .sprite
                .as_ref()
                .and_then(|sprite| free_name(&sprite.name, self.document.names()));
            let Ok(mut copy) = picture.try_clone() else {
                self.state("There is not enough memory to copy the sprite", damage);
                break;
            };
            match (copy.sprite.as_mut(), name) {
                (Some(sprite), Some(name)) => sprite.name = name,
                (Some(_), None) => {
                    self.state("No name is free for a copy", damage);
                    break;
                }
                (None, _) => {}
            }
            let at = self.document.index_of(id).map_or(0, |index| index + 1);
            match self.document.insert(at, Entry::Picture(copy)) {
                Ok(()) => made.extend(self.document.ids().get(at).copied()),
                Err(refusal) => {
                    self.state(&alloc::format!("{refusal}"), damage);
                    break;
                }
            }
        }
        if made.is_empty() {
            return Outcome::none();
        }
        self.choose_only(&made);
        Outcome::relaid()
    }

    fn delete(&mut self, damage: &mut Region) -> Outcome {
        if self.chosen.len() >= self.document.entries().len() {
            self.state(&alloc::format!("{}", ListRefusal::LastEntry), damage);
            return Outcome::none();
        }
        let doomed = core::mem::take(&mut self.chosen);
        for id in &doomed {
            if let Some(index) = self.document.index_of(*id) {
                if let Err(refusal) = self.document.remove(index) {
                    self.state(&alloc::format!("{refusal}"), damage);
                    break;
                }
            }
        }
        self.cursor = self
            .cursor
            .filter(|id| self.document.index_of(*id).is_some());
        self.anchor = self
            .anchor
            .filter(|id| self.document.index_of(*id).is_some());
        self.chosen.extend(self.cursor);
        Outcome::relaid()
    }

    fn reorder(&mut self, up: bool, damage: &mut Region) -> Outcome {
        let Some(index) = self.single() else {
            self.state("Choose one sprite to move", damage);
            return Outcome::none();
        };
        let to = if up {
            index.checked_sub(1)
        } else {
            Some(index + 1).filter(|&to| to < self.document.entries().len())
        };
        let Some(to) = to else {
            return Outcome::none();
        };
        let moved = self.document.move_entry(index, to);
        self.list_changed(moved, damage)
    }

    fn list_changed(&mut self, changed: Result<(), ListRefusal>, damage: &mut Region) -> Outcome {
        match changed {
            Ok(()) => Outcome::relaid(),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), damage);
                Outcome::none()
            }
        }
    }

    fn history_moved(&mut self, moved: bool, nothing: &str, damage: &mut Region) -> Outcome {
        if !moved {
            self.state(nothing, damage);
            return Outcome::none();
        }
        let ids = self.document.ids();
        self.chosen.retain(|id| ids.contains(id));
        self.cursor = self.cursor.filter(|id| ids.contains(id));
        self.anchor = self.anchor.filter(|id| ids.contains(id));
        Outcome::relaid()
    }

    /// Choose `ids` alone, the last of them the keyboard's.
    fn choose_only(&mut self, ids: &[EntryId]) {
        self.chosen.clear();
        self.chosen.extend_from_slice(ids);
        self.cursor = ids.last().copied();
        self.anchor = self.cursor;
    }

    /// Feed `input` at `now_ns`.
    pub fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match *input {
            InputEvent::PointerMoved { to } => {
                self.pointer = to;
                self.on_bar(input, scale, theme, damage)
            }
            InputEvent::ModifiersChanged { modifiers } => {
                self.modifiers = modifiers;
                Outcome::none()
            }
            InputEvent::KeyPressed { key, modifiers } => {
                self.modifiers = modifiers;
                self.unsay(damage);
                self.on_key(key, modifiers, damage)
            }
            InputEvent::PointerScrolled { dx, dy } => {
                match self.scroll.wheel(dx, dy, scale, self.layout.bar, damage) {
                    Some(ScrollAction::ScrollTo { offset }) => self.scroll_to(offset, damage),
                    None => Outcome::none(),
                }
            }
            InputEvent::PointerPressed { button } if !self.layout.bar.contains(self.pointer) => {
                self.unsay(damage);
                self.on_press(button, now_ns, damage)
            }
            _ => self.on_bar(input, scale, theme, damage),
        }
    }

    fn on_bar(
        &mut self,
        input: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match self
            .scroll
            .on_pointer(input, self.layout.bar, scale, theme, damage)
        {
            Some(ScrollAction::ScrollTo { offset }) => self.scroll_to(offset, damage),
            None => Outcome::none(),
        }
    }

    fn scroll_to(&mut self, offset: u64, damage: &mut Region) -> Outcome {
        self.offset = u32::try_from(offset).unwrap_or(u32::MAX);
        self.settle(damage);
        Outcome::none()
    }

    fn on_press(&mut self, button: PointerButton, now_ns: u64, damage: &mut Region) -> Outcome {
        let count = self.document.entries().len();
        let hit = self
            .layout
            .index_at(self.pointer, self.offset, count)
            .and_then(|index| self.document.ids().get(index).copied());
        match (button, hit) {
            (PointerButton::Primary, Some(id)) => {
                let subject = u64::from(id_bits(id));
                let kind = self.clicks.register(
                    now_ns,
                    subject,
                    PointerButton::Primary,
                    self.double_click,
                );
                if kind == ClickKind::Double {
                    self.choose_only(&[id]);
                    damage.add(self.layout.grid);
                    return self.open_chosen(damage);
                }
                self.press_on(id, damage);
                Outcome::none()
            }
            (PointerButton::Primary, None) => {
                self.clicks.reset();
                if !self.modifiers.ctrl && !self.modifiers.shift {
                    self.chosen.clear();
                    damage.add(self.layout.grid);
                }
                Outcome::none()
            }
            (PointerButton::Secondary, hit) => {
                if let Some(id) = hit.filter(|id| !self.chosen.contains(id)) {
                    self.choose_only(&[id]);
                    damage.add(self.layout.grid);
                }
                Outcome::asking(Request::Menu {
                    kind: MenuKind::Window,
                    anchor: Rect::new(self.pointer.x, self.pointer.y, 1, 1),
                })
            }
            _ => Outcome::none(),
        }
    }

    /// A primary press on sprite `id`: Ctrl adds or takes it, Shift runs to
    /// it from the anchor, anything else chooses it alone.
    fn press_on(&mut self, id: EntryId, damage: &mut Region) {
        if self.modifiers.ctrl {
            match self.chosen.iter().position(|&held| held == id) {
                Some(at) => {
                    self.chosen.remove(at);
                }
                None => self.chosen.push(id),
            }
            self.anchor = Some(id);
        } else if self.modifiers.shift {
            self.run_to(id);
        } else {
            self.choose_only(&[id]);
        }
        self.cursor = Some(id);
        damage.add(self.layout.grid);
    }

    /// Choose the run of sprites from the anchor to `id`, inclusive.
    fn run_to(&mut self, id: EntryId) {
        let to = self.document.index_of(id);
        let from = self
            .anchor
            .and_then(|anchor| self.document.index_of(anchor))
            .or(to);
        let (Some(from), Some(to)) = (from, to) else {
            return;
        };
        let (low, high) = (from.min(to), from.max(to));
        self.chosen.clear();
        self.chosen
            .extend_from_slice(&self.document.ids()[low..=high]);
    }

    fn on_key(&mut self, key: Key, modifiers: Modifiers, damage: &mut Region) -> Outcome {
        let count = self.document.entries().len();
        if count == 0 {
            return Outcome::none();
        }
        let shortcut = match key {
            Key::Char(c) if modifiers.ctrl => match c.to_ascii_lowercase() {
                'a' => Some(Action::SelectAll),
                'z' if modifiers.shift => Some(Action::Redo),
                'z' => Some(Action::Undo),
                'y' => Some(Action::Redo),
                's' if modifiers.shift => Some(Action::SaveAs),
                's' => Some(Action::Save),
                'o' => Some(Action::Open),
                'w' => Some(Action::Close),
                _ => None,
            },
            Key::Named(NamedKey::Enter) => Some(Action::EditEntry),
            Key::Named(NamedKey::Delete) => Some(Action::DeleteEntry),
            _ => None,
        };
        if let Some(action) = shortcut {
            return self.act(action, damage);
        }
        let at = self
            .cursor
            .and_then(|id| self.document.index_of(id))
            .unwrap_or(0);
        let columns = self.layout.columns.max(1);
        let page = columns
            * (self.layout.grid.height / (self.layout.cell.1 + self.layout.gap).max(1)).max(1)
                as usize;
        let to = match key {
            Key::Named(NamedKey::Left) => at.saturating_sub(1),
            Key::Named(NamedKey::Right) => (at + 1).min(count - 1),
            Key::Named(NamedKey::Up) => at.saturating_sub(columns),
            Key::Named(NamedKey::Down) => (at + columns).min(count - 1),
            Key::Named(NamedKey::PageUp) => at.saturating_sub(page),
            Key::Named(NamedKey::PageDown) => (at + page).min(count - 1),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => count - 1,
            _ => return Outcome::none(),
        };
        let id = self.document.ids()[to];
        if modifiers.shift {
            self.run_to(id);
        } else {
            self.choose_only(&[id]);
        }
        self.cursor = Some(id);
        self.reveal(to, damage);
        damage.add(self.layout.grid);
        Outcome::none()
    }

    /// Scroll cell `index` into view.
    fn reveal(&mut self, index: usize, damage: &mut Region) {
        let row = u32::try_from(index / self.layout.columns.max(1)).unwrap_or(u32::MAX);
        let top = row.saturating_mul(self.layout.cell.1 + self.layout.gap);
        let bottom = top.saturating_add(self.layout.cell.1 + self.layout.gap * 2);
        if top < self.offset {
            self.offset = top;
        } else if bottom > self.offset + self.layout.grid.height {
            self.offset = bottom.saturating_sub(self.layout.grid.height);
        }
        self.settle(damage);
    }

    fn damage_cursor(&self, damage: &mut Region) {
        if let Some(index) = self.cursor.and_then(|id| self.document.index_of(id)) {
            damage.add(self.layout.cell_rect(index, self.offset));
        }
    }

    /// Take `picture` as sprite `id`'s, as its own window edited it, and draw
    /// its thumbnail again.
    pub fn adopt_edited(
        &mut self,
        id: EntryId,
        picture: crate::document::Picture,
        damage: &mut Region,
    ) -> Outcome {
        let Some(index) = self.document.adopt_edited(id, picture) else {
            return Outcome::none();
        };
        *self.revisions.entry(id).or_insert(0) += 1;
        self.fresh = false;
        self.seen = self.document.generation();
        damage.add(self.layout.cell_rect(index, self.offset));
        self.wanted().map_or_else(Outcome::none, Outcome::asking)
    }

    /// The thumbnails to draw next, as a job for the queue's worker: the
    /// sprites whose thumbnail is missing or stale, those in view first. One
    /// job is out at a time.
    pub fn wanted(&mut self) -> Option<Request> {
        if self.fresh || self.asked.is_some() || self.side == 0 {
            return None;
        }
        let entries = self.document.entries();
        let ids = self.document.ids();
        let stale = |index: usize| {
            let id = ids[index];
            let revision = self.revisions.get(&id).copied().unwrap_or(0);
            matches!(entries[index], Entry::Picture(_))
                && self
                    .thumbs
                    .get(&id)
                    .is_none_or(|thumb| thumb.revision != revision || thumb.side != self.side)
        };
        let near = self.near();
        let order = self
            .in_view
            .clone()
            .chain(near.start..self.in_view.start)
            .chain(self.in_view.end..near.end);
        let mut pictures = Vec::new();
        for index in order.filter(|&index| stale(index)).take(THUMBS_PER_JOB) {
            let Entry::Picture(picture) = &entries[index] else {
                continue;
            };
            let Ok(copy) = picture.try_clone() else {
                break;
            };
            let id = ids[index];
            let revision = self.revisions.get(&id).copied().unwrap_or(0);
            pictures.push((id, revision, copy));
        }
        if pictures.is_empty() {
            self.fresh = true;
            return None;
        }
        let job = self.next_job;
        self.next_job += 1;
        self.asked = Some(job);
        Some(Request::Own(Own::Compute {
            job,
            work: Compute::Thumbnails {
                side: self.side,
                pictures,
            },
        }))
    }

    /// Job `job` was refused a place on the queue: none is out, and the next
    /// change asks again.
    pub fn refused(&mut self, job: u64) {
        if self.asked == Some(job) {
            self.asked = None;
        }
    }

    /// What the worker answered job `job` with.
    pub fn computed(&mut self, job: u64, computed: Computed, damage: &mut Region) -> Outcome {
        if self.asked != Some(job) {
            return Outcome::none();
        }
        self.asked = None;
        if let Computed::Thumbnails { side, drawn } = computed {
            for (id, revision, pixels) in drawn {
                let surface = pixels.and_then(|pixels| Surface::from_rgba8(side, side, &pixels));
                self.thumbs.insert(
                    id,
                    Thumb {
                        revision,
                        side,
                        surface,
                    },
                );
                if let Some(index) = self.document.index_of(id) {
                    damage.add(self.layout.cell_rect(index, self.offset));
                }
            }
        }
        self.wanted().map_or_else(Outcome::none, Outcome::asking)
    }

    /// The form the window asks in, beside its area, holding the window.
    #[must_use]
    pub fn dialog_window(&self, scale: Scale, theme: &Theme) -> Option<DialogWindow<'_>> {
        let form = self.modal.as_ref()?;
        Some(DialogWindow {
            title: form.title(),
            rect: form.rect(self.layout.dialog_origin(), scale, theme),
            modality: tairix_abi::window_ipc::DialogModality::OwnerModal,
        })
    }

    /// Feed `input` that reached the window's form.
    pub fn dialog_input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Outcome {
        let origin = self.layout.dialog_origin();
        let Some(form) = self.modal.as_mut() else {
            return Outcome::none();
        };
        let answer = match *input {
            InputEvent::KeyPressed { key, modifiers } => form.on_key(
                Keystroke {
                    key,
                    modifiers,
                    at_ns: now_ns,
                },
                origin,
                scale,
                theme,
                damage,
            ),
            InputEvent::KeyReleased { .. } | InputEvent::ModifiersChanged { .. } => None,
            _ => form.on_pointer(input, origin, scale, theme, damage),
        };
        match (answer, self.modal.take()) {
            (Some(Answer::Confirmed), Some(form)) => self.form_answered(form, damage),
            (None, form) => {
                self.modal = form;
                Outcome::none()
            }
            (Some(Answer::Cancelled | Answer::Confirmed), _) => Outcome::none(),
        }
    }

    /// The desktop would not open the form, for `why`.
    pub fn dialog_refused(&mut self, why: &str, damage: &mut Region) -> Outcome {
        self.modal = None;
        self.state(
            &alloc::format!("The form could not be shown: {why}"),
            damage,
        );
        Outcome::none()
    }

    fn form_answered(&mut self, mut form: Form, damage: &mut Region) -> Outcome {
        if form.purpose() != Purpose::NewSprite {
            return Outcome::none();
        }
        let new: NewSprite = match form.new_sprite_answer() {
            Ok(new) if self.document.names_taken(&new.name, None) => {
                form.refuse("A sprite of that name is already here");
                self.modal = Some(form);
                return Outcome::none();
            }
            Ok(new) => new,
            Err(reason) => {
                form.refuse(&reason);
                self.modal = Some(form);
                return Outcome::none();
            }
        };
        let Some(picture) = sprite_picture(&new) else {
            self.state("There is not enough memory for the sprite", damage);
            return Outcome::none();
        };
        let at = self
            .cursor
            .and_then(|id| self.document.index_of(id))
            .map_or(self.document.entries().len(), |index| index + 1);
        match self.document.insert(at, Entry::Picture(picture)) {
            Ok(()) => {
                let id = self.document.ids()[at];
                self.choose_only(&[id]);
                Outcome {
                    request: Some(Request::Own(Own::OpenSprites(alloc::vec![id]))),
                    relayout: Relayout::Whole,
                }
            }
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), damage);
                Outcome::none()
            }
        }
    }

    /// Why the file cannot be saved as `name` into `target`, when it cannot:
    /// only its own file's recorded filetype speaks for a name that says none.
    #[must_use]
    pub fn refuse_save(&self, name: &str, target: SaveTarget) -> Option<String> {
        let recorded = self.recorded.filter(|_| target == SaveTarget::Own);
        format_for(
            name,
            recorded,
            self.document.entries(),
            self.document.origin(),
        )
        .err()
        .map(|refusal| alloc::format!("{refusal}"))
    }

    /// The extension a Save As offers.
    #[must_use]
    pub fn offered_extension(&self) -> &'static str {
        natural(self.document.entries(), self.document.origin()).extension()
    }

    /// The one format a Save As writes the file in: its own.
    ///
    /// # Errors
    ///
    /// Why no name can hold the file.
    pub fn save_formats(&self) -> Result<SaveFormats, String> {
        let entries = self.document.entries();
        let origin = self.document.origin();
        save_formats(entries, origin, natural(entries, origin))
            .map_err(|refusal| alloc::format!("{refusal}"))
    }

    /// Release the history to memory pressure `band`.
    pub fn adopt_pressure(&mut self, band: tairix_reclaim::PressureBand) {
        self.document.adopt_pressure(band);
    }
}

/// A sprite's identity as a click's subject.
fn id_bits(id: EntryId) -> u32 {
    u32::try_from(id.get()).unwrap_or(u32::MAX)
}

/// The columns and rows a grid of cells is laid out in, and the breadth its
/// scroll bar takes from its width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GridFit {
    columns: usize,
    rows: usize,
    /// The breadth a scroll bar takes from the width: zero unless the rows
    /// overflow `room`.
    bar: u32,
}

/// The one fitting of `count` cells of `cell`, `gap` apart and around, into
/// `across` by `room`, with a `bar`-wide scroll bar taken from the width only
/// when the rows overflow — shared by the layout and the opening size so the
/// two can never disagree.
fn grid_fit(across: u32, room: u32, count: usize, cell: (u32, u32), gap: u32, bar: u32) -> GridFit {
    let fit = |width: u32| {
        let columns = (width.saturating_sub(gap) / (cell.0 + gap)).max(1) as usize;
        (columns, count.div_ceil(columns))
    };
    let (columns, rows) = fit(across);
    let pitch = u64::from(cell.1 + gap);
    if rows as u64 * pitch + u64::from(gap) <= u64::from(room) {
        return GridFit {
            columns,
            rows,
            bar: 0,
        };
    }
    let (columns, rows) = fit(across.saturating_sub(bar));
    GridFit { columns, rows, bar }
}

/// A scroll bar's breadth at `scale` in `theme`.
fn bar_breadth(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().scrollbar_breadth).max(1)
}

/// The cell that draws a `thumb`-pixel thumbnail exactly, and its name.
fn cell_size(thumb: u32, scale: Scale, theme: &Theme) -> (u32, u32) {
    (
        TILE.width_for(thumb, scale, theme),
        TILE.height_for(thumb, scale, theme),
    )
}

impl SavedDocument for AreaView {
    type Snapshot = Snapshot;

    fn access(&self) -> Access {
        self.access
    }

    fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    fn is_empty(&self) -> bool {
        !self.document.is_modified() && self.document.history_depth() == 0
    }

    fn snapshot(&mut self) -> Option<(u64, Arc<Snapshot>)> {
        let snapshot = self.document.snapshot().ok()?;
        Some((self.document.generation(), Arc::new(snapshot)))
    }

    fn saved(&mut self, generation: u64) {
        self.document.saved(generation);
    }

    fn rename(&mut self, name: String) {
        self.name = name;
        self.recorded = None;
        self.renamed += 1;
    }

    fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    fn say(&mut self, message: String) {
        AreaView::say(self, message);
    }
}

/// Paint the sprite file window into `surface`, whose clip narrows it to
/// what this round repaints.
pub fn render_area(surface: &mut Surface, view: &AreaView, theme: &Theme, scale: Scale) {
    let layout = view.layout();
    let palette = theme.palette();
    if !withheld(surface, layout.grid) {
        fill_area(surface, layout.grid, Color::from(palette.surface));
        let count = view.document.entries().len();
        for index in layout.shown(view.offset, count) {
            let cell = layout.cell_rect(index, view.offset);
            if withheld(surface, cell) {
                continue;
            }
            render_cell(surface, view, (index, cell), scale, theme);
        }
    }
    if !layout.bar.is_empty() && !withheld(surface, layout.bar) {
        view.scroll.render(surface, layout.bar, scale, theme);
    }
    if !withheld(surface, layout.status) {
        let band = StatusBand::new(theme, scale);
        let mut said = String::new();
        let (text, tone) = if let Some(message) = view.message.as_deref() {
            (message, StatusTone::Ink)
        } else {
            let count = view.document.entries().len();
            let _ = core::fmt::Write::write_fmt(
                &mut said,
                format_args!("{count} sprite{}", if count == 1 { "" } else { "s" }),
            );
            (said.as_str(), StatusTone::Muted)
        };
        let message = band.slots(layout.status, [], []).message;
        band.paint(surface, layout.status, theme, &[(message, text, tone)]);
    }
    if let Some(form) = view.modal.as_ref() {
        form.render(surface, layout.dialog_origin(), scale, theme);
    }
}

/// Sprite `index`'s cell at `cell`, drawn as the file manager draws a file:
/// its thumbnail, or the picture glyph until one lands, over its name.
fn render_cell(
    surface: &mut Surface,
    view: &AreaView,
    (index, cell): (usize, Rect),
    scale: Scale,
    theme: &Theme,
) {
    let layout = view.layout();
    let id = view.document.ids()[index];
    let entry = &view.document.entries()[index];
    let mut state = ControlState::idle();
    if view.is_chosen(id) {
        state.selection = SelectionState::Selected;
    }
    state.focus.focused = view.focused && view.cursor == Some(id);
    let picture = matches!(entry, Entry::Picture(_))
        .then(|| view.thumbnail(id, layout.thumb))
        .flatten()
        .map(IconPicture::coloured)
        .or_else(|| {
            view.placeholder
                .as_ref()
                .map(|glyph| IconPicture::builtin(IconKind::Image, glyph))
        });
    IconTile::new(
        entry.name().map(ToString::to_string).unwrap_or_default(),
        IconKind::Image,
    )
    .with_layout(TILE)
    .with_state(state)
    .render(surface, cell, scale, theme, picture);
}

#[cfg(test)]
#[path = "area_tests.rs"]
mod tests;
