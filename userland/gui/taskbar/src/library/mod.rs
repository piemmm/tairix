//! The program library: the launcher the taskbar's Library button opens
//! (`plans/NEW-TASKBAR.md` T5).
//!
//! [`LibraryPopup`] is a pure model over what the session hands it — the
//! resolved [`Catalog`], each entry's manifest facts, the account's [`Shelf`]
//! of favourites and frequently used programs, and which bundles are running.
//! It never touches the VFS and holds no authority: everything it is asked to
//! do leaves as a typed outcome the session carries out.
//!
//! The panel is a search field over a sidebar and a content pane. The sidebar
//! is *Home* and one tab per folder; Home lists the favourites, one row of the
//! frequently used, then every program, and a folder tab lists that folder.
//! Typing anywhere searches (the `search` module): the content becomes the
//! ranked results as captioned rows, the first of them current, and the
//! sidebar narrows them to a folder. Everything is reachable from the
//! keyboard, and everything fails closed: a press on nothing changes nothing,
//! an outcome is only ever reported for an entry that exists, and degenerate
//! geometry draws chrome over an empty pane rather than panicking.

mod layout;
mod motion;
mod search;
mod view;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_controls::{
    ControlRole, ListRow, Panel, ScrollAction, ScrollBar, ScrollModel, ScrollOrientation,
    ScrollRange, SearchField, Tab, Tabs, TabsAction, TabsOrientation, TextAction,
};
use tairix_font::Cut;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_proglib::{Catalog, EntryId, LibraryCategory, Shelf};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::apps::AppIdentity;
use crate::edge::Edge;
use crate::layout::BarLayout;

pub(crate) use layout::{footer_font, Place};
pub use layout::{ItemAt, LibraryLayout, SectionLayout};
pub(crate) use motion::{ItemFrame, Stepped};
pub use view::{LibraryItem, LibraryPage, Section, SectionKind};

/// The popup's window title.
const POPUP_TITLE: &str = "Programs";

/// What the details strip says while nothing is highlighted.
const HINT: &str = "Type to search, or right-click a program for more";

/// The heading over the sidebar's folders.
const FOLDERS_HEADING: &str = "Folders";

/// The human-readable name of a library folder.
///
/// The [`LibraryCategory`] identifiers are locale-neutral store spellings
/// (`SystemTools`); this is the spelling a launcher shows.
#[must_use]
pub fn folder_label(category: LibraryCategory) -> &'static str {
    match category {
        LibraryCategory::Accessories => "Accessories",
        LibraryCategory::Graphics => "Graphics",
        LibraryCategory::Internet => "Internet",
        LibraryCategory::Multimedia => "Multimedia",
        LibraryCategory::Office => "Office",
        LibraryCategory::Programming => "Programming",
        LibraryCategory::Games => "Games",
        LibraryCategory::SystemTools => "System Tools",
        LibraryCategory::Utilities => "Utilities",
        LibraryCategory::Other => "Other",
    }
}

/// A shown program's request for its own icon: the entry, and the side it is
/// drawn at, so the session — which holds the decode capabilities the bar does
/// not — rasterises it at exactly that size.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryIconRequest {
    /// The catalog entry whose icon is wanted.
    pub entry: EntryId,
    /// The pixel side it is drawn at.
    pub side: u32,
}

/// Which part of the popup owns the keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LibraryFocus {
    /// The search field: printable keys edit the query.
    Search,
    /// The sidebar: arrows choose a page or narrow a search.
    Sidebar,
    /// The content: arrows move through the programs.
    Content,
}

/// The two clicks that reach an entry on a freshly opened launcher whatever
/// the account's history: its folder's tab, then its picture on that page.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LibraryRoute {
    /// The entry's folder tab in the sidebar.
    pub folder: Rect,
    /// The entry's picture once that folder is shown: a click anywhere on it
    /// opens the entry.
    pub picture: Rect,
}

/// What one routed event did to the open popup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PopupOutcome {
    /// Nothing changed.
    Ignored,
    /// Only what the popup draws changed, and the damage says where.
    Changed,
    /// Open this entry.
    Launch(EntryId),
    /// Open this entry as an administrator.
    LaunchElevated(EntryId),
    /// Open this entry's menu, hung from this rectangle.
    Menu(EntryId, Rect),
    /// The user dismissed the popup.
    Dismiss,
}

/// What routing one event reads beside the event itself: the geometry it
/// lands in, the look it is drawn with, and the instant it arrived.
#[derive(Copy, Clone)]
pub(crate) struct Routing<'a> {
    pub(crate) layout: &'a LibraryLayout,
    pub(crate) theme: &'a Theme,
    pub(crate) scale: Scale,
    pub(crate) now_ns: u64,
}

/// The program-library popup model. See the [module docs](self).
#[derive(Clone, Debug)]
pub struct LibraryPopup {
    open: bool,
    /// Whether this showing has been announced as on screen.
    shown: bool,
    catalog: Catalog,
    facts: BTreeMap<EntryId, AppIdentity>,
    shelf: Shelf,
    /// The bundle directories running, as last handed over.
    running_bundles: BTreeSet<String>,
    /// The catalogued entries that start one of them.
    running: BTreeSet<EntryId>,
    index: Vec<search::Indexed>,
    /// The non-empty folders, in taxonomy order: the sidebar's tabs after the
    /// first.
    folders: Vec<LibraryCategory>,
    search: SearchField,
    sidebar: Tabs,
    page: LibraryPage,
    scope: Option<LibraryCategory>,
    /// Whether what is shown is a search's results.
    searching: bool,
    sections: Vec<Section>,
    /// How many programs a search found, in all and per folder.
    found: (usize, BTreeMap<LibraryCategory, usize>),
    scroll: ScrollBar,
    focus: LibraryFocus,
    current: Option<ItemAt>,
    hover: Option<ItemAt>,
    pressed: Option<ItemAt>,
    artwork: BTreeMap<EntryId, Surface>,
    artwork_side: u32,
    motion: motion::Motion,
    /// Moves on whenever the content would draw differently, so a layer
    /// painted from it knows when it is stale.
    generation: u64,
}

impl Default for LibraryPopup {
    fn default() -> Self {
        Self::new()
    }
}

impl LibraryPopup {
    /// A closed popup over an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        let mut popup = Self {
            open: false,
            shown: false,
            catalog: Catalog::default(),
            facts: BTreeMap::new(),
            shelf: Shelf::default(),
            running_bundles: BTreeSet::new(),
            running: BTreeSet::new(),
            index: Vec::new(),
            folders: Vec::new(),
            search: SearchField::new().with_placeholder("Search programs"),
            sidebar: Tabs::new(Vec::new()).with_orientation(TabsOrientation::Vertical),
            page: LibraryPage::Home,
            scope: None,
            searching: false,
            sections: Vec::new(),
            found: (0, BTreeMap::new()),
            scroll: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::EMPTY, 1),
            ),
            focus: LibraryFocus::Search,
            current: None,
            hover: None,
            pressed: None,
            artwork: BTreeMap::new(),
            artwork_side: 0,
            motion: motion::Motion::default(),
            generation: 0,
        };
        popup.rebuild();
        popup
    }

    /// Whether the popup is open. While it is, the desktop routes every
    /// pointer, scroll, and key event here.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the closed popup's window is still leaving the screen.
    #[must_use]
    pub const fn is_departing(&self) -> bool {
        !self.open && self.motion.departing()
    }

    /// Whether the window is still rising, sinking or fading: while it is
    /// not, it stands where it was last placed.
    #[must_use]
    pub const fn is_placing(&self) -> bool {
        self.motion.placing()
    }

    /// Whether `bundles` is the running set the popup already holds.
    pub(crate) fn runs_exactly(&self, bundles: &BTreeSet<String>) -> bool {
        *bundles == self.running_bundles
    }

    /// Announce this showing once a presented frame shows it whole: open, and
    /// risen into place. Once per open, and never for a closed popup.
    pub fn report_newly_shown(&mut self, report: impl FnOnce()) {
        if !self.open || self.shown || self.motion.placing() {
            return;
        }
        self.shown = true;
        report();
    }

    /// The resolved catalog the popup lists.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Adopt the resolved `catalog`, rebuilding what is shown, and answer
    /// whether it differed. The same catalog handed again — the launcher
    /// re-reads it on every open — keeps everything, its icons included.
    pub(crate) fn set_catalog(&mut self, catalog: Catalog) -> bool {
        if catalog == self.catalog {
            return false;
        }
        self.catalog = catalog;
        self.running = running_in(&self.catalog, &self.running_bundles);
        self.artwork.clear();
        self.reindex();
        self.rebuild();
        true
    }

    /// Adopt each entry's manifest facts — what it is for, its version, and
    /// who made it — which the search reads and the details strip and *Info*
    /// state, answering whether they differed.
    pub(crate) fn set_facts(&mut self, facts: BTreeMap<EntryId, AppIdentity>) -> bool {
        if facts == self.facts {
            return false;
        }
        self.facts = facts;
        self.reindex();
        self.rebuild();
        true
    }

    /// The manifest facts of `entry`, where they are known.
    #[must_use]
    pub fn facts_of(&self, entry: &EntryId) -> Option<&AppIdentity> {
        self.facts.get(entry)
    }

    /// Adopt the account's shelf — its favourites and frequently used —
    /// answering whether it differed.
    pub(crate) fn set_shelf(&mut self, fresh: Shelf) -> bool {
        if fresh == self.shelf {
            return false;
        }
        self.shelf = fresh;
        self.rebuild();
        true
    }

    /// The account's shelf as last handed over.
    #[must_use]
    pub const fn shelf(&self) -> &Shelf {
        &self.shelf
    }

    /// Adopt which bundle directories are running, reporting into `damage` the
    /// shown programs whose mark that moves and the details strip where what
    /// it says changed, and answering whether any entry's mark moved.
    pub(crate) fn set_running(
        &mut self,
        bundles: BTreeSet<String>,
        layout: &LibraryLayout,
        damage: &mut Region,
    ) -> bool {
        if bundles == self.running_bundles {
            return false;
        }
        let running = running_in(&self.catalog, &bundles);
        self.running_bundles = bundles;
        if running == self.running {
            return false;
        }
        let said = self.details(layout);
        for (at, _) in layout.shown() {
            let moved = self
                .item(at)
                .is_some_and(|item| self.running.contains(&item.id) != running.contains(&item.id));
            if moved {
                owe_items(layout, [Some(at)], damage);
            }
        }
        self.running = running;
        if self.details(layout) != said {
            damage.add(layout.footer);
        }
        self.touch();
        true
    }

    /// Whether `entry` is running.
    #[must_use]
    pub fn is_running(&self, entry: &EntryId) -> bool {
        self.running.contains(entry)
    }

    /// Whether `entry` is one of the account's favourites.
    #[must_use]
    pub fn is_favourite(&self, entry: &EntryId) -> bool {
        self.shelf.favourites.contains(entry)
    }

    /// Whether the account has a record of using `entry`.
    #[must_use]
    pub fn has_use(&self, entry: &EntryId) -> bool {
        self.shelf.weights.contains_key(entry)
    }

    /// The shown sections, in order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The program at `at`.
    #[must_use]
    pub fn item(&self, at: ItemAt) -> Option<&LibraryItem> {
        self.sections.get(at.section)?.items.get(at.item)
    }

    /// The page being browsed.
    #[must_use]
    pub const fn page(&self) -> LibraryPage {
        self.page
    }

    /// The folder a search is narrowed to, if any.
    #[must_use]
    pub const fn scope(&self) -> Option<LibraryCategory> {
        self.scope
    }

    /// Which part of the popup owns the keyboard.
    #[must_use]
    pub const fn focus(&self) -> LibraryFocus {
        self.focus
    }

    /// The program the keyboard is on.
    #[must_use]
    pub const fn current(&self) -> Option<ItemAt> {
        self.current
    }

    /// The program drawn as the keyboard's: a search's current result
    /// wherever the keyboard is, or the cursor while the keyboard is in the
    /// content.
    #[must_use]
    pub fn selection(&self) -> Option<ItemAt> {
        if self.searching || self.focus == LibraryFocus::Content {
            self.current
        } else {
            None
        }
    }

    /// The [`selection`](Self::selection) while any of it shows in `layout`:
    /// what `Enter`, the menu key and the details strip act on, so none of
    /// them acts on a program scrolled out of sight.
    #[must_use]
    pub fn shown_selection(&self, layout: &LibraryLayout) -> Option<ItemAt> {
        self.selection()
            .filter(|&at| layout.item_rect(at).is_some())
    }

    /// The program under the pointer.
    #[must_use]
    pub const fn hover(&self) -> Option<ItemAt> {
        self.hover
    }

    /// The program a primary press is held on.
    #[must_use]
    pub const fn pressed(&self) -> Option<ItemAt> {
        self.pressed
    }

    /// The query.
    #[must_use]
    pub fn search_text(&self) -> &str {
        self.search.text()
    }

    /// The search field, for painting.
    #[must_use]
    pub const fn search_field(&self) -> &SearchField {
        &self.search
    }

    /// The sidebar, for painting.
    #[must_use]
    pub const fn sidebar(&self) -> &Tabs {
        &self.sidebar
    }

    /// Moves on whenever the content would draw differently.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The calm sentence shown where nothing is listed, or `None` while
    /// something is: an empty library and a query matching nothing are worded
    /// apart, so the user knows which they are looking at.
    #[must_use]
    pub fn placeholder(&self) -> Option<String> {
        if !self.sections.is_empty() {
            return None;
        }
        if !self.search.has_query() {
            return Some(String::from("No programs are catalogued"));
        }
        Some(match self.scope {
            Some(category) => alloc::format!("No matching programs in {}", folder_label(category)),
            None => String::from("No matching programs"),
        })
    }

    /// What the details strip says over `layout`: what the program under the
    /// pointer — or else the [`shown_selection`](Self::shown_selection) — is
    /// for, its folder, and whether it is running; or the hint, when neither
    /// rests on one.
    #[must_use]
    pub fn details(&self, layout: &LibraryLayout) -> String {
        let Some(item) = self
            .hover
            .or(self.shown_selection(layout))
            .and_then(|at| self.item(at))
        else {
            return String::from(HINT);
        };
        let mut said = String::new();
        if let Some(purpose) = self
            .facts
            .get(&item.id)
            .and_then(|facts| facts.purpose.as_ref())
        {
            said.push_str(purpose);
        }
        if let Some(entry) = self.catalog.entry(&item.id) {
            if !said.is_empty() {
                said.push_str("  \u{b7}  ");
            }
            said.push_str(folder_label(entry.category()));
        }
        if self.running.contains(&item.id) {
            said.push_str("  \u{b7}  Running");
        }
        said
    }

    /// The owner-resolved icon of `entry`, if it has arrived.
    #[must_use]
    pub fn artwork(&self, entry: &EntryId) -> Option<&Surface> {
        self.artwork.get(entry)
    }

    /// The entrance the content is making, while it makes one, and the frame
    /// of it to draw.
    pub(crate) fn entrance_frame(&self, order: usize) -> Option<ItemFrame> {
        let at = self.motion.at_ns();
        self.motion
            .entrance()
            .map(|entrance| entrance.frame(order, at))
    }

    /// The strength the keyboard's mark is drawn at on `(kind, entry)` in the
    /// current frame, `None` where it follows the selection.
    pub(crate) fn mark_strength(&self, kind: SectionKind, entry: &EntryId) -> Option<u8> {
        self.motion.mark_strength(kind, entry)
    }

    /// How far toward the bar the window stands, and how strongly it is
    /// drawn, in the current frame.
    #[must_use]
    pub fn placement(&self) -> (i32, u8) {
        self.motion.placement()
    }

    /// Lay the popup out, opened from `bar`'s Library button on a bar pinned to
    /// `edge` of a `screen` at `scale` under `theme`.
    #[must_use]
    pub fn layout(
        &self,
        edge: Edge,
        bar: &BarLayout,
        screen: (u32, u32),
        scale: Scale,
        theme: &Theme,
    ) -> LibraryLayout {
        let place = Place {
            edge,
            bar,
            screen,
            scale,
            theme,
        };
        LibraryLayout::compute(
            &place,
            &self.sections,
            &self.sidebar,
            self.scroll.model().offset(),
        )
    }

    /// The icons the shown programs want, one per entry, at the side each is
    /// drawn at — only what `layout` shows, so a large library never asks for
    /// an icon nobody sees.
    #[must_use]
    pub fn visible_icon_requests(
        &self,
        layout: &LibraryLayout,
        scale: Scale,
        theme: &Theme,
    ) -> Vec<LibraryIconRequest> {
        let mut seen = BTreeSet::new();
        layout
            .shown()
            .filter_map(|(at, rect)| {
                let item = self.item(at)?;
                if !seen.insert(&item.id) {
                    return None;
                }
                Some(LibraryIconRequest {
                    entry: item.id.clone(),
                    side: icon_side(layout, at, rect, scale, theme),
                })
            })
            .collect()
    }

    /// File the owner-resolved `artwork` for `entry` at `side`, or clear it
    /// with `None`, reporting each shown rectangle whose picture changed. A
    /// side other than the one held clears every picture, since each was
    /// rasterised for the old one.
    pub(crate) fn set_artwork(
        &mut self,
        entry: &EntryId,
        side: u32,
        layout: &LibraryLayout,
        artwork: Option<Surface>,
        damage: &mut Region,
    ) {
        if side != self.artwork_side {
            self.artwork.clear();
            self.artwork_side = side;
        }
        if self.artwork.get(entry) == artwork.as_ref() {
            return;
        }
        match artwork {
            Some(picture) => {
                self.artwork.insert(entry.clone(), picture);
            }
            None => {
                self.artwork.remove(entry);
            }
        }
        self.touch();
        for (at, _) in layout.shown() {
            if self.item(at).is_some_and(|item| item.id == *entry) {
                if let Some(rect) = layout.item_rect(at) {
                    damage.add(rect);
                }
            }
        }
    }

    /// The two clicks that reach `entry` on a freshly opened launcher laid out
    /// from `place`, whatever the account's history, or `None` where the
    /// sidebar is not shown, the entry is not catalogued, or its picture is
    /// not wholly on its folder's first screen.
    pub(crate) fn route_to(&self, entry: &EntryId, place: &Place<'_>) -> Option<LibraryRoute> {
        let category = self.catalog.entry(entry)?.category();
        let mut fresh = Self::new();
        fresh.catalog = self.catalog.clone();
        fresh.facts = self.facts.clone();
        fresh.shelf = self.shelf.clone();
        fresh.reindex();
        fresh.reset();
        let home = LibraryLayout::compute(place, &fresh.sections, &fresh.sidebar, 0);
        let tab = 1 + fresh
            .folders
            .iter()
            .position(|&folder| folder == category)?;
        let folder = fresh
            .sidebar
            .tab_rect(tab, home.sidebar?, place.scale, place.theme)?;
        fresh.page = LibraryPage::Folder(category);
        fresh.rebuild();
        let shown = LibraryLayout::compute(place, &fresh.sections, &fresh.sidebar, 0);
        let item = fresh
            .sections
            .first()?
            .items
            .iter()
            .position(|item| item.id == *entry)?;
        let cell = shown.laid(ItemAt { section: 0, item })?;
        let picture = shown
            .tile
            .core_rect(cell, place.scale, place.theme)
            .filter(|picture| picture.intersection(&shown.viewport) == *picture)?;
        Some(LibraryRoute { folder, picture })
    }

    /// Open afresh: query cleared, Home shown from its top, the keyboard on
    /// the search field. The same catalog and shelf always open the same way.
    pub(crate) fn open(&mut self, now_ns: u64, scale: Scale, theme: &Theme) {
        self.reset();
        self.open = true;
        self.shown = false;
        self.motion.open(now_ns, theme.motion(), scale);
    }

    /// Drop what the pointer lit, now that it rests on none of the bar's
    /// surfaces, reporting what redraws.
    pub(crate) fn pointer_left(&mut self, layout: &LibraryLayout, damage: &mut Region) {
        let Some(hover) = self.hover.take() else {
            return;
        };
        owe_items(layout, [Some(hover)], damage);
        damage.add(layout.footer);
        self.touch();
    }

    /// End a departure at once, answering whether the window was still
    /// leaving: what a window taking the screen asks, so the two never draw
    /// over each other.
    pub(crate) fn finish_departure(&mut self) -> bool {
        !self.open && self.motion.finish_departure()
    }

    /// Close, keeping the catalog. The window sinks away; nothing it shows
    /// reacts any more.
    pub(crate) fn close(&mut self, now_ns: u64, scale: Scale, theme: &Theme) {
        if !self.open {
            return;
        }
        self.open = false;
        self.hover = None;
        self.pressed = None;
        self.motion.close(now_ns, theme.motion(), scale);
    }

    /// Step the motion to `now_ns`, reporting into `damage` what the frame
    /// redraws, and answering what changed.
    pub(crate) fn advance(
        &mut self,
        now_ns: u64,
        layout: &LibraryLayout,
        damage: &mut Region,
    ) -> Stepped {
        // Read before the step: a mark that ends in this frame still redraws.
        self.owe_marked(layout, damage);
        let stepped = self.motion.advance(now_ns);
        if stepped.content {
            damage.add(layout.viewport);
        }
        stepped
    }

    /// Nanoseconds until the next frame of the popup's motion is due on a
    /// display refreshing at `refresh`, or `None` while nothing moves.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64, refresh: RefreshRate) -> Option<u64> {
        self.motion.next_frame_in(now_ns, refresh)
    }

    /// Report every shown program the keyboard's mark is moving onto or off.
    fn owe_marked(&self, layout: &LibraryLayout, damage: &mut Region) {
        if !self.motion.marking() {
            return;
        }
        for (at, _) in layout.shown() {
            if self.marked(at) {
                owe_items(layout, [Some(at)], damage);
            }
        }
    }

    /// End any entrance at once, owing the viewport it was drawing through.
    fn settle_entrance(&mut self, layout: &LibraryLayout, damage: &mut Region) {
        if self.motion.settle_content() {
            damage.add(layout.viewport);
        }
    }

    /// Whether the keyboard's mark is moving onto or off the program at `at`.
    fn marked(&self, at: ItemAt) -> bool {
        self.sections.get(at.section).is_some_and(|section| {
            section
                .items
                .get(at.item)
                .is_some_and(|item| self.motion.mark_strength(section.kind, &item.id).is_some())
        })
    }

    /// Route one pointer `event`, tracked at `point`.
    pub(crate) fn route_pointer(
        &mut self,
        event: &InputEvent,
        point: Point,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let Routing {
            layout,
            theme,
            scale,
            ..
        } = routing;
        self.sync_scroll(layout);
        let mut changed = self
            .search
            .on_pointer(event, layout.search, scale, theme, damage)
            .is_some();
        if let Some(bounds) = layout.scrollbar {
            if let Some(ScrollAction::ScrollTo { offset }) =
                self.scroll.on_pointer(event, bounds, scale, theme, damage)
            {
                self.scroll_to(offset, layout, damage);
                changed = true;
            }
        }
        if let Some(bounds) = layout.sidebar {
            if let Some(TabsAction::Selected { index }) =
                self.sidebar.on_pointer(event, bounds, scale, theme, damage)
            {
                self.choose_tab(index, routing, damage);
                return PopupOutcome::Changed;
            }
        }
        let outcome = match *event {
            InputEvent::PointerMoved { .. } => {
                let hover = self.item_at(point, routing);
                if hover != self.hover {
                    owe_items(layout, [self.hover, hover], damage);
                    damage.add(layout.footer);
                    self.hover = hover;
                    self.touch();
                    changed = true;
                }
                PopupOutcome::Ignored
            }
            InputEvent::PointerPressed { button } => self.press(button, point, routing, damage),
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.release(point, routing, damage),
            InputEvent::PointerCancelled => {
                if let Some(pressed) = self.pressed.take() {
                    owe_items(layout, [Some(pressed)], damage);
                    self.touch();
                    changed = true;
                }
                PopupOutcome::Ignored
            }
            InputEvent::PointerScrolled { dx, dy } => {
                let wheeled = layout
                    .scrollbar
                    .and_then(|bounds| self.scroll.wheel(dx, dy, scale, bounds, damage))
                    .map(|ScrollAction::ScrollTo { offset }| offset);
                if let Some(offset) = wheeled {
                    self.scroll_to(offset, layout, damage);
                    changed = true;
                }
                PopupOutcome::Ignored
            }
            InputEvent::PointerReleased { .. }
            | InputEvent::Pinch { .. }
            | InputEvent::KeyPressed { .. }
            | InputEvent::KeyReleased { .. }
            | InputEvent::ModifiersChanged { .. } => PopupOutcome::Ignored,
        };
        match outcome {
            PopupOutcome::Ignored => changed_outcome(changed),
            decided => decided,
        }
    }

    /// A press of `button` at `point`: the search takes the keyboard, a
    /// program arms (primary) or opens its menu (secondary), the panel's own
    /// ground is claimed, and anywhere else dismisses.
    fn press(
        &mut self,
        button: PointerButton,
        point: Point,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let layout = routing.layout;
        let under = self.item_at(point, routing);
        match button {
            PointerButton::Primary => {
                self.pressed = None;
                if layout.search.contains(point) {
                    self.give_focus(LibraryFocus::Search, layout, damage);
                    return PopupOutcome::Changed;
                }
                if let Some(at) = under {
                    // A program opens on the release that ends the press, so a
                    // press slid away never opens the wrong one.
                    self.pressed = Some(at);
                    owe_items(layout, [Some(at)], damage);
                    self.touch();
                    return PopupOutcome::Changed;
                }
            }
            PointerButton::Secondary => {
                if let Some((item, anchor)) =
                    under.and_then(|at| Some((self.item(at)?, layout.item_rect(at)?)))
                {
                    return PopupOutcome::Menu(item.id.clone(), anchor);
                }
            }
            PointerButton::Middle => {}
        }
        if layout.panel.contains(point) {
            PopupOutcome::Ignored
        } else {
            PopupOutcome::Dismiss
        }
    }

    /// The primary release at `point`: a press released on the program it
    /// armed opens it.
    fn release(&mut self, point: Point, routing: Routing<'_>, damage: &mut Region) -> PopupOutcome {
        let Some(pressed) = self.pressed.take() else {
            return PopupOutcome::Ignored;
        };
        owe_items(routing.layout, [Some(pressed)], damage);
        self.touch();
        if self.item_at(point, routing) == Some(pressed) {
            if let Some(item) = self.item(pressed) {
                return PopupOutcome::Launch(item.id.clone());
            }
        }
        PopupOutcome::Changed
    }

    /// Route one key press.
    pub(crate) fn route_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        self.sync_scroll(routing.layout);
        match key {
            Key::Named(NamedKey::Tab) => {
                return self.cycle_focus(!modifiers.shift, routing, damage);
            }
            Key::Named(NamedKey::Function { number: 10 }) if modifiers.shift => {
                return self.act_on_selection(routing, damage, Self::menu_on);
            }
            Key::Named(NamedKey::Enter) if modifiers.ctrl && modifiers.shift => {
                return self.act_on_selection(routing, damage, |popup, layout| {
                    popup
                        .chosen(layout)
                        .map_or(PopupOutcome::Ignored, PopupOutcome::LaunchElevated)
                });
            }
            _ => {}
        }
        match self.focus {
            LibraryFocus::Search => self.key_in_search(key, modifiers, routing, damage),
            LibraryFocus::Sidebar => self.key_in_sidebar(key, modifiers, routing, damage),
            LibraryFocus::Content => self.key_in_content(key, modifiers, routing, damage),
        }
    }

    fn key_in_search(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let layout = routing.layout;
        match key {
            Key::Named(NamedKey::Down) => {
                let target = self
                    .current
                    .filter(|&at| layout.holds(at))
                    .or_else(|| layout.first());
                match target {
                    Some(at) => {
                        self.give_focus(LibraryFocus::Content, layout, damage);
                        self.place_cursor(at, routing, damage);
                        PopupOutcome::Changed
                    }
                    None => PopupOutcome::Ignored,
                }
            }
            Key::Named(NamedKey::Enter) => self.launch_selection(routing, damage),
            Key::Named(NamedKey::Escape) if !self.search.has_query() => PopupOutcome::Dismiss,
            _ => match self.search.on_key(key, modifiers, layout.search, damage) {
                Some(TextAction::Edited) => {
                    self.query_changed(routing, damage);
                    PopupOutcome::Changed
                }
                Some(_) => PopupOutcome::Changed,
                None => PopupOutcome::Ignored,
            },
        }
    }

    fn key_in_sidebar(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let Routing {
            layout,
            theme,
            scale,
            ..
        } = routing;
        let Some(bounds) = layout.sidebar else {
            self.give_focus(LibraryFocus::Search, layout, damage);
            return PopupOutcome::Changed;
        };
        match key {
            Key::Named(NamedKey::Up | NamedKey::Down | NamedKey::Home | NamedKey::End) => {
                let _ = self.sidebar.on_key(key, bounds, scale, theme, damage);
                if let Some(index) = self.sidebar.current() {
                    self.choose_tab(index, routing, damage);
                }
                PopupOutcome::Changed
            }
            Key::Named(NamedKey::Right | NamedKey::Enter) => {
                let target = self
                    .current
                    .filter(|&at| layout.holds(at))
                    .or_else(|| layout.first());
                match target {
                    Some(at) => {
                        self.give_focus(LibraryFocus::Content, layout, damage);
                        self.place_cursor(at, routing, damage);
                        PopupOutcome::Changed
                    }
                    None => PopupOutcome::Ignored,
                }
            }
            Key::Named(NamedKey::Escape) => self.escape(routing, damage),
            Key::Char(_) | Key::Named(NamedKey::Backspace) => {
                self.give_focus(LibraryFocus::Search, layout, damage);
                self.key_in_search(key, modifiers, routing, damage)
            }
            Key::Named(_) => PopupOutcome::Ignored,
        }
    }

    fn key_in_content(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let layout = routing.layout;
        let moved = match key {
            Key::Named(NamedKey::Left) => self.step_across(layout, false),
            Key::Named(NamedKey::Right) => self.step_across(layout, true),
            Key::Named(NamedKey::Up) => {
                let above = self.current.and_then(|at| row_above(layout, at));
                if above.is_none() {
                    self.give_focus(LibraryFocus::Search, layout, damage);
                    return PopupOutcome::Changed;
                }
                above
            }
            Key::Named(NamedKey::Down) => self.current.and_then(|at| row_below(layout, at)),
            Key::Named(NamedKey::Home) => layout.first(),
            Key::Named(NamedKey::End) => layout.last(),
            Key::Named(NamedKey::PageUp) => self.page_step(layout, false),
            Key::Named(NamedKey::PageDown) => self.page_step(layout, true),
            // A space carries on a query being typed; on a page it opens.
            Key::Char(' ') if self.search.has_query() => {
                self.give_focus(LibraryFocus::Search, layout, damage);
                return self.key_in_search(key, modifiers, routing, damage);
            }
            Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                return self.launch_selection(routing, damage);
            }
            Key::Named(NamedKey::Escape) => return self.escape(routing, damage),
            Key::Char(_) | Key::Named(NamedKey::Backspace) => {
                self.give_focus(LibraryFocus::Search, layout, damage);
                return self.key_in_search(key, modifiers, routing, damage);
            }
            Key::Named(_) => return PopupOutcome::Ignored,
        };
        match moved {
            Some(at) => {
                self.place_cursor(at, routing, damage);
                PopupOutcome::Changed
            }
            None => PopupOutcome::Ignored,
        }
    }

    /// `Escape`: clear a query and hand the keyboard back to the search, else
    /// dismiss.
    fn escape(&mut self, routing: Routing<'_>, damage: &mut Region) -> PopupOutcome {
        if !self.search.has_query() {
            return PopupOutcome::Dismiss;
        }
        self.search.set_text("");
        damage.add(routing.layout.search);
        self.give_focus(LibraryFocus::Search, routing.layout, damage);
        self.query_changed(routing, damage);
        PopupOutcome::Changed
    }

    /// The program `Enter` opens: the
    /// [`shown_selection`](Self::shown_selection), so an empty query opens
    /// nothing the user has not moved onto.
    fn chosen(&self, layout: &LibraryLayout) -> Option<EntryId> {
        let at = self.shown_selection(layout)?;
        self.item(at).map(|item| item.id.clone())
    }

    /// `Enter`: open the selection.
    fn launch_selection(&mut self, routing: Routing<'_>, damage: &mut Region) -> PopupOutcome {
        self.act_on_selection(routing, damage, |popup, layout| {
            popup
                .chosen(layout)
                .map_or(PopupOutcome::Ignored, PopupOutcome::Launch)
        })
    }

    /// `Shift+F10`: the menu of the selection, hung from it.
    fn menu_on(&self, layout: &LibraryLayout) -> PopupOutcome {
        let Some(at) = self.shown_selection(layout) else {
            return PopupOutcome::Ignored;
        };
        match (self.item(at), layout.item_rect(at)) {
            (Some(item), Some(anchor)) => PopupOutcome::Menu(item.id.clone(), anchor),
            _ => PopupOutcome::Ignored,
        }
    }

    /// Act on the selection with `act`, or — where it has been scrolled out of
    /// sight — bring it back into view instead, so a key never acts on a
    /// program the user cannot see.
    fn act_on_selection(
        &mut self,
        routing: Routing<'_>,
        damage: &mut Region,
        act: impl FnOnce(&Self, &LibraryLayout) -> PopupOutcome,
    ) -> PopupOutcome {
        let layout = routing.layout;
        match self.selection().filter(|&at| layout.holds(at)) {
            Some(at) if layout.item_rect(at).is_none() => {
                self.place_cursor(at, routing, damage);
                PopupOutcome::Changed
            }
            _ => act(self, layout),
        }
    }

    /// The query changed: search, or browse again once it is empty, the
    /// content arriving afresh whenever the popup changes between the two.
    fn query_changed(&mut self, routing: Routing<'_>, damage: &mut Region) {
        let was_searching = self.searching;
        if !self.search.has_query() {
            self.scope = None;
        }
        self.rebuild();
        self.current = self.first_result();
        self.hover = None;
        self.reset_scroll();
        if was_searching == self.searching {
            self.settle_entrance(routing.layout, damage);
        } else {
            self.motion
                .arrive(routing.now_ns, routing.theme.motion(), routing.scale, true);
        }
        owe_content(routing.layout, damage);
    }

    /// Choose sidebar tab `index`: a page while browsing, a folder to narrow
    /// a search to while searching.
    fn choose_tab(&mut self, index: usize, routing: Routing<'_>, damage: &mut Region) {
        let before = self.selected_tab();
        let folder = index
            .checked_sub(1)
            .and_then(|at| self.folders.get(at).copied());
        if index != 0 && folder.is_none() {
            return;
        }
        if self.search.has_query() {
            self.scope = folder;
        } else {
            self.page = folder.map_or(LibraryPage::Home, LibraryPage::Folder);
        }
        if self.selected_tab() == before {
            return;
        }
        let layout = routing.layout;
        let said = self.details(layout);
        self.rebuild();
        self.current = self.first_result();
        self.hover = None;
        self.reset_scroll();
        self.motion.arrive(
            routing.now_ns,
            routing.theme.motion(),
            routing.scale,
            index > before,
        );
        // A page changes the content and moves the sidebar's selection; the
        // counts it shows stand.
        damage.add(layout.content_column());
        if let Some(strip) = layout.sidebar {
            for tab in [before, index] {
                if let Some(rect) = self
                    .sidebar
                    .tab_rect(tab, strip, routing.scale, routing.theme)
                {
                    damage.add(rect);
                }
            }
        }
        if self.details(layout) != said {
            damage.add(layout.footer);
        }
    }

    /// Where the keyboard starts on what was just shown: a search's best
    /// match, and nothing on a page until the user moves onto one.
    fn first_result(&self) -> Option<ItemAt> {
        let first = ItemAt {
            section: 0,
            item: 0,
        };
        (self.searching && self.item(first).is_some()).then_some(first)
    }

    /// Move the keyboard to `focus`, reporting what redraws.
    fn give_focus(&mut self, focus: LibraryFocus, layout: &LibraryLayout, damage: &mut Region) {
        if focus == self.focus {
            return;
        }
        let content = focus == LibraryFocus::Content;
        if content != (self.focus == LibraryFocus::Content) {
            if let Some(bar) = layout.scrollbar {
                damage.add(bar);
            }
        }
        self.focus = focus;
        self.search.set_focused(focus == LibraryFocus::Search);
        self.scroll.set_focused(content);
        damage.add(layout.search);
        if let Some(sidebar) = layout.sidebar {
            damage.add(sidebar);
        }
        // Whether the cursor is drawn, and what the strip says of it, follows
        // the keyboard into and out of the content.
        owe_items(layout, [self.current], damage);
        damage.add(layout.footer);
        if focus == LibraryFocus::Sidebar {
            self.sidebar.adopt_current(Some(self.selected_tab()));
        } else {
            self.sidebar.adopt_current(None);
        }
        self.touch();
    }

    /// `Tab` and `Shift+Tab`: the next part of the popup there is something in.
    fn cycle_focus(
        &mut self,
        forward: bool,
        routing: Routing<'_>,
        damage: &mut Region,
    ) -> PopupOutcome {
        let layout = routing.layout;
        let offered = [
            Some(LibraryFocus::Search),
            layout.sidebar.map(|_| LibraryFocus::Sidebar),
            layout.first().map(|_| LibraryFocus::Content),
        ];
        let parts = || offered.iter().flatten().copied();
        let focus = self.focus;
        let next = if forward {
            parts()
                .skip_while(|&part| part != focus)
                .nth(1)
                .or_else(|| parts().next())
        } else {
            parts()
                .take_while(|&part| part != focus)
                .last()
                .or_else(|| parts().last())
        };
        let Some(next) = next.filter(|&next| next != focus) else {
            return PopupOutcome::Ignored;
        };
        self.give_focus(next, layout, damage);
        if next == LibraryFocus::Content {
            if let Some(at) = self
                .current
                .filter(|&at| layout.holds(at))
                .or_else(|| layout.first())
            {
                self.place_cursor(at, routing, damage);
            }
        }
        PopupOutcome::Changed
    }

    /// Put the keyboard on `at`, scrolling it into view and moving the mark.
    fn place_cursor(&mut self, at: ItemAt, routing: Routing<'_>, damage: &mut Region) {
        let layout = routing.layout;
        if !layout.holds(at) {
            return;
        }
        if self.current != Some(at) {
            let leaving = self.current.and_then(|was| self.key_of(was));
            let arriving = self.key_of(at);
            if leaving != arriving {
                // A mark still moving is replaced, and what it was drawing on
                // redraws at rest.
                self.owe_marked(layout, damage);
                self.motion
                    .mark(routing.now_ns, routing.theme.motion(), leaving, arriving);
            }
        }
        owe_items(layout, [self.current, Some(at)], damage);
        damage.add(layout.footer);
        self.current = Some(at);
        self.settle_entrance(layout, damage);
        if let Some(offset) = layout.revealing(at) {
            if offset != u64::from(layout.offset) {
                self.scroll_to(offset, layout, damage);
            }
        }
        self.touch();
    }

    /// What a mark and the cursor are kept against across rebuilds: the
    /// section and the entry.
    fn key_of(&self, at: ItemAt) -> Option<(SectionKind, EntryId)> {
        let section = self.sections.get(at.section)?;
        Some((section.kind, section.items.get(at.item)?.id.clone()))
    }

    /// Where the program `key` names now stands.
    fn find(&self, (kind, id): &(SectionKind, EntryId)) -> Option<ItemAt> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section, shown)| {
                let item = (shown.kind == *kind)
                    .then(|| shown.items.iter().position(|item| item.id == *id))??;
                Some(ItemAt { section, item })
            })
    }

    /// Left or right: the neighbouring program, continuing into the
    /// neighbouring section at either end.
    fn step_across(&self, layout: &LibraryLayout, forward: bool) -> Option<ItemAt> {
        let Some(at) = self.current.filter(|&at| layout.holds(at)) else {
            return layout.first();
        };
        let places = || all_laid(layout).map(|(each, _)| each);
        if forward {
            places().skip_while(|&each| each != at).nth(1)
        } else {
            places().take_while(|&each| each != at).last()
        }
    }

    /// `PageUp`/`PageDown`: the program nearest the same column a viewport away.
    fn page_step(&self, layout: &LibraryLayout, down: bool) -> Option<ItemAt> {
        let Some(at) = self.current.filter(|&at| layout.holds(at)) else {
            return layout.first();
        };
        let rect = layout.laid(at)?;
        let reach = to_i32(layout.viewport.height);
        let target = if down {
            rect.top().saturating_add(reach)
        } else {
            rect.top().saturating_sub(reach)
        };
        let centre = centre_x(rect);
        all_laid(layout)
            .min_by_key(|(_, candidate)| {
                (
                    candidate.top().saturating_sub(target).unsigned_abs(),
                    centre_x(*candidate).saturating_sub(centre).unsigned_abs(),
                )
            })
            .map(|(each, _)| each)
            .filter(|&each| each != at)
            .or(if down { layout.last() } else { layout.first() })
    }

    /// The program under screen `point`: a tile's picture or its drawn name,
    /// never the ground beside them, or a row anywhere on it.
    fn item_at(&self, point: Point, routing: Routing<'_>) -> Option<ItemAt> {
        let layout = routing.layout;
        let content = layout.view().to_content(point)?;
        // Cells never overlap and a tile's parts lie inside its cell, so only
        // the one cell holding the point needs its name measured.
        let (at, rect) = layout.cell_at(content)?;
        if layout.sections.get(at.section)?.kind.is_list() {
            return Some(at);
        }
        let item = self.item(at)?;
        layout
            .tile
            .parts(rect, routing.scale, routing.theme, &item.name, Cut::End)
            .is_some_and(|parts| parts.contains(content))
            .then_some(at)
    }

    /// The sidebar tab standing for what is shown.
    fn selected_tab(&self) -> usize {
        let folder = if self.search.has_query() {
            self.scope
        } else {
            match self.page {
                LibraryPage::Home => None,
                LibraryPage::Folder(category) => Some(category),
            }
        };
        folder
            .and_then(|folder| self.folders.iter().position(|&each| each == folder))
            .map_or(0, |at| at + 1)
    }

    /// Fold the catalog for the search.
    fn reindex(&mut self) {
        self.index = self
            .catalog
            .entries()
            .map(|entry| {
                let purpose = self
                    .facts
                    .get(entry.id())
                    .and_then(|facts| facts.purpose.as_deref());
                search::Indexed::new(entry, purpose, folder_label(entry.category()))
            })
            .collect();
        self.folders = self.catalog.folders();
    }

    /// The opening state: no query, Home, nothing marked, the keyboard on the
    /// search, scrolled to the top.
    fn reset(&mut self) {
        self.search.set_text("");
        self.search.set_focused(true);
        self.page = LibraryPage::Home;
        self.scope = None;
        self.focus = LibraryFocus::Search;
        self.current = None;
        self.hover = None;
        self.pressed = None;
        self.scroll.set_focused(false);
        self.rebuild();
        self.reset_scroll();
    }

    /// Rebuild what is shown from the catalog, the shelf, and the query,
    /// keeping the keyboard on the program it was on.
    fn rebuild(&mut self) {
        let kept = self.current.and_then(|at| self.key_of(at));
        self.searching = self.search.has_query();
        if self.searching {
            let hits = search::search(&self.index, self.search.text(), &self.shelf.weights);
            let mut per_folder = BTreeMap::new();
            for hit in &hits {
                if let Some(program) = self.index.get(hit.index) {
                    *per_folder.entry(program.category()).or_insert(0usize) += 1;
                }
            }
            self.found = (hits.len(), per_folder);
            if self
                .scope
                .is_some_and(|scope| !self.folders.contains(&scope))
            {
                self.scope = None;
            }
            self.sections = view::results(&self.catalog, &self.index, &hits, self.scope);
        } else {
            if let LibraryPage::Folder(category) = self.page {
                if !self.folders.contains(&category) {
                    self.page = LibraryPage::Home;
                }
            }
            self.found = (0, BTreeMap::new());
            self.sections = view::page(&self.catalog, &self.shelf, self.page);
        }
        self.restate_sidebar();
        self.current = kept.and_then(|key| self.find(&key));
        if self.hover.is_some_and(|at| self.item(at).is_none()) {
            self.hover = None;
        }
        // A press is keyed to a place, and what sits there may have changed.
        self.pressed = None;
        self.touch();
    }

    /// Restate the sidebar's tabs for the folders and the mode, keeping where
    /// the pointer rests on it.
    fn restate_sidebar(&mut self) {
        let searching = self.search.has_query();
        let mut tabs = Vec::with_capacity(self.folders.len() + 1);
        tabs.push(if searching {
            Tab::new("All").with_reading(alloc::format!("{}", self.found.0))
        } else {
            Tab::new("Home")
        });
        for (at, &folder) in self.folders.iter().enumerate() {
            let count = if searching {
                self.found.1.get(&folder).copied().unwrap_or(0)
            } else {
                self.catalog.folder(folder).len()
            };
            let mut tab = Tab::new(folder_label(folder)).with_reading(alloc::format!("{count}"));
            if at == 0 {
                tab = tab.with_group(FOLDERS_HEADING);
            }
            tabs.push(tab);
        }
        let mut fresh = Tabs::new(tabs).with_orientation(TabsOrientation::Vertical);
        fresh.adopt_selected(self.selected_tab());
        fresh.adopt_current((self.focus == LibraryFocus::Sidebar).then(|| self.selected_tab()));
        self.sidebar.restate(fresh);
    }

    /// The content would draw differently.
    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bring the scrollbar's model in step with the content and the viewport.
    fn sync_scroll(&mut self, layout: &LibraryLayout) {
        self.scroll
            .set_model(scroll_model(layout, self.scroll.model().offset()));
    }

    /// The scrollbar as it stands over `layout`'s content, for painting: its
    /// range is the layout's own, whatever has changed since an event last
    /// synced it.
    #[must_use]
    pub fn scrollbar_for(&self, layout: &LibraryLayout) -> ScrollBar {
        let mut bar = self.scroll;
        bar.set_model(scroll_model(layout, self.scroll.model().offset()));
        bar
    }

    /// Scroll to `offset`, settling any entrance and reporting what redraws.
    fn scroll_to(&mut self, offset: u64, layout: &LibraryLayout, damage: &mut Region) {
        self.scroll.set_model(self.scroll.model().scroll_to(offset));
        self.settle_entrance(layout, damage);
        damage.add(layout.viewport);
        if let Some(bar) = layout.scrollbar {
            damage.add(bar);
        }
        self.touch();
    }

    fn reset_scroll(&mut self) {
        self.scroll.set_model(self.scroll.model().scroll_to(0));
    }

    /// When the press the popup's scrollbar holds steps next, or `None` when
    /// nothing held would move.
    pub(crate) fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.scroll.repeat_deadline(now_ns)
    }

    /// Step the press the scrollbar holds if its repeat is due, answering
    /// whether the content moved.
    pub(crate) fn repeat(&mut self, routing: Routing<'_>, damage: &mut Region) -> bool {
        let Routing {
            layout,
            theme,
            scale,
            now_ns,
        } = routing;
        self.sync_scroll(layout);
        let Some(bounds) = layout.scrollbar else {
            return false;
        };
        match self.scroll.repeat(now_ns, bounds, scale, theme, damage) {
            Some(ScrollAction::ScrollTo { offset }) => {
                self.scroll_to(offset, layout, damage);
                true
            }
            None => false,
        }
    }
}

/// The scroll model of `layout`'s content scrolled `offset` pixels, a step
/// being one of its programs.
fn scroll_model(layout: &LibraryLayout, offset: u64) -> ScrollModel {
    let line = layout
        .first()
        .and_then(|first| layout.laid(first))
        .map_or(1, |rect| u64::from(rect.height));
    ScrollModel::in_pixels(
        ScrollRange::new(
            u64::from(layout.content_height),
            u64::from(layout.viewport.height),
            offset,
        ),
        line,
    )
}

/// The entries of `catalog` that start a bundle `bundles` names.
fn running_in(catalog: &Catalog, bundles: &BTreeSet<String>) -> BTreeSet<EntryId> {
    catalog
        .entries()
        .filter(|entry| bundles.contains(entry.bundle().as_str()))
        .map(|entry| entry.id().clone())
        .collect()
}

/// Report the shown rectangles of `items` into `damage`.
fn owe_items<const N: usize>(
    layout: &LibraryLayout,
    items: [Option<ItemAt>; N],
    damage: &mut Region,
) {
    for rect in items
        .into_iter()
        .flatten()
        .filter_map(|at| layout.item_rect(at))
    {
        damage.add(rect);
    }
}

/// The program in the row above `at` nearest its column, crossing into the
/// section above from a first row.
fn row_above(layout: &LibraryLayout, at: ItemAt) -> Option<ItemAt> {
    let rect = layout.laid(at)?;
    nearest_in_row(
        layout,
        rect,
        |candidate| candidate.bottom() <= rect.top(),
        true,
    )
}

/// The program in the row below `at` nearest its column, crossing into the
/// section below from a last row.
fn row_below(layout: &LibraryLayout, at: ItemAt) -> Option<ItemAt> {
    let rect = layout.laid(at)?;
    nearest_in_row(
        layout,
        rect,
        |candidate| candidate.top() >= rect.bottom(),
        false,
    )
}

/// Every laid-out program and where it stands, in reading order.
fn all_laid(layout: &LibraryLayout) -> impl Iterator<Item = (ItemAt, Rect)> + '_ {
    (0..layout.sections.len()).flat_map(|section| layout.laid_in(section))
}

/// The program nearest `from`'s column in the nearest row `beyond` admits —
/// above when `upward`, below otherwise.
fn nearest_in_row(
    layout: &LibraryLayout,
    from: Rect,
    beyond: impl Fn(&Rect) -> bool,
    upward: bool,
) -> Option<ItemAt> {
    let centre = centre_x(from);
    let candidates = || all_laid(layout).filter(|(_, rect)| beyond(rect));
    let tops = candidates().map(|(_, rect)| rect.top());
    let row = if upward { tops.max()? } else { tops.min()? };
    candidates()
        .filter(|(_, rect)| rect.top() == row)
        .min_by_key(|(_, rect)| centre_x(*rect).saturating_sub(centre).unsigned_abs())
        .map(|(at, _)| at)
}

/// The horizontal middle of `rect`.
fn centre_x(rect: Rect) -> i32 {
    rect.left().saturating_add(to_i32(rect.width / 2))
}

/// Report everything a changed query redraws: the content column, the
/// sidebar's counts and selection, and the details strip.
fn owe_content(layout: &LibraryLayout, damage: &mut Region) {
    damage.add(layout.content_column());
    if let Some(sidebar) = layout.sidebar {
        damage.add(sidebar);
    }
    damage.add(layout.footer);
}

/// The side a shown program's icon is drawn at.
fn icon_side(layout: &LibraryLayout, at: ItemAt, rect: Rect, scale: Scale, theme: &Theme) -> u32 {
    let list = layout
        .sections
        .get(at.section)
        .is_some_and(|section| section.kind.is_list());
    if list {
        ListRow::new("")
            .with_icon(tairix_icon::IconKind::AppBundle)
            .with_caption("")
            .icon_side(rect, scale, theme)
    } else {
        layout.tile.icon_side(rect, scale, theme)
    }
}

/// Collapse a "did the visuals change" flag to an outcome.
fn changed_outcome(changed: bool) -> PopupOutcome {
    if changed {
        PopupOutcome::Changed
    } else {
        PopupOutcome::Ignored
    }
}

/// The popup's chrome: the shared panel, anchored back at the invoker, on the
/// floating ground every surface the bar opens shares.
pub(crate) fn chrome_panel(anchor: Point) -> Panel {
    Panel::new(POPUP_TITLE)
        .with_role(ControlRole::Navigation)
        .with_anchor(anchor)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
