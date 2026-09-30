//! Painting the browser's current directory into a pixel [`Surface`].
//!
//! [`render_into`] turns a [`Browser`]'s path and entries into a premultiplied-alpha
//! [`Surface`] sized to the app's content viewport, using the active theme's
//! [`Palette`](tairix_theme::Palette) for the chrome and the shared
//! `lib/controls` collection controls for the items, every length converted
//! from logical pixels through the desktop's one [`Scale`]. The theme picks
//! the text face, never the caller. The surface is the window manager's to
//! place and round: the browser paints a *rectangular* buffer and the
//! compositor applies any corner radius through its single anti-aliased
//! rounded-corner path. There is no rounding here.
//!
//! The top row is the command toolbar; below it the current directory is drawn
//! in whichever [`ViewMode`] the browser holds — a column of full-width
//! [`TableRow`]s (list) or a wrapped grid of [`IconTile`]s (grid) — over the
//! one shared selection state, with a drawn [`ScrollBar`] in a reserved
//! right-edge gutter. Painting through the same collection controls the trusted
//! picker uses keeps the two views one coherent themed surface. The visible
//! window, each item's rectangle, the scroll offset, and the scrollbar geometry
//! all come from the one shared [`ViewLayout`], so the pointer hit-test
//! ([`entry_index_at`]) and the paint can never disagree.
//!
//! Every length saturates and every blit clips, so a degenerate viewport paints
//! nothing rather than panicking. Each scrolling surface — the item views, the
//! *Open With…* chooser's rows, a Properties window's sections — is laid out
//! unscrolled at its natural size and painted through its [`ScrollView`],
//! which confines it to the area it scrolls in: an item the viewport's edge crosses is drawn whole and cut
//! there, and nothing can mark the chrome above it or the scrollbar gutter
//! beside it.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::button::{Button, ButtonContent, ContentAlign};
use tairix_controls::decision::Dialog;
use tairix_controls::scroll::{ScrollModel, ScrollOrientation, ScrollRange, ScrollView};
use tairix_controls::state::{
    ActivityState, AuthorityState, ControlRole, ControlState, PointerState, SelectionState,
};
use tairix_controls::text::{Keystroke, TextField};
use tairix_controls::value::Progress;
use tairix_controls::{
    ground_fill, paint_icon_slot, stack, Checkbox, ChromeLayer, Fact, FactList, FieldAction,
    FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow, FlagSet, IconButton,
    IconTile, ListRow, Panel, ScrollBar, Tab, TableCell, TableRow, Tabs, Toolbar, FULL_COLOUR,
};
use tairix_font::{BitmapFont, ELLIPSIS};
use tairix_geometry::{GridFill, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::{TextRole, Theme};

use crate::browser::Browser;
use crate::chrome::{
    self, ManagerTool, ManagerToolModel, ToolbarBand, ToolbarCommand, ToolbarModel,
};
use crate::column::ScrollColumn;
use crate::delete::DeletePlan;
use crate::entry::{Entry, EntryKind};
use crate::format::{format_date, format_size};
use crate::layout::{GridFlow, GridMetrics, GridView, ListView, SidebarView, ViewLayout, ViewMode};
use crate::media::{entry_icon_request, icon_for_entry, media_for_name, MediaType};
use crate::open_with::OpenWithChooser;
use crate::places::{self, Place, Places};
use crate::progress::ProgressModel;
use crate::properties::{Attributes, Properties};
use crate::source::DirectorySource;
use crate::trash::DeleteDisposition;

/// Padding between a panel's edge and its label text, in logical pixels.
const LABEL_PADDING: u32 = 4;

/// Vertical padding above and below a row's glyphs, in logical pixels.
const ROW_PADDING: u32 = 2;

/// Relative widths of the list view's name, size, and modified columns.
///
/// [`TableRow::render`] scales these proportionally into the actual content
/// width, so they act as weights independent of the window size: the name
/// column dominates, with narrower size and date columns beside it. Defining
/// them once here keeps the column layout a single definition.
const COLUMNS: [u32; 3] = [240, 96, 128];

/// Paint `browser`'s current directory into `surface` at `viewport`'s size,
/// using `theme`'s palette and the shared collection controls.
///
/// The caller owns the surface, and holds it for the life of its window: a
/// repaint clipped to what a round changed
/// ([`Surface::with_clip`](tairix_raster::Surface::with_clip)) is sound only
/// because every pixel outside the clip is the one already on screen.
///
/// `tools` are the manager-only write tools ([`chrome::MANAGER_TOOLS`]) to draw
/// on the toolbar after the shared read-only commands: the file manager passes
/// them, the trusted read-only picker passes an empty slice so it never draws a
/// write tool. The read-only commands keep their positions regardless (the
/// toolbar left-packs fixed-width buttons), so a click on a read-only command
/// resolves identically for both consumers. `tool_model` supplies each write
/// tool's enable state (the file manager's [`ManagerToolModel`]; the picker's
/// [`ManagerToolModel::none`], since it draws none): a disabled tool renders
/// muted, never hidden.
///
/// `artwork` is the draw-site icon lookup the grid view resolves each tile's
/// real icon through: for every grid tile the renderer classifies the entry to
/// an [`IconKind`] — naming the bundle itself as well when the entry is one —
/// and asks `artwork` for a pre-rasterised surface at the tile's icon slot,
/// falling back to the built-in glyph when it returns `None`. The list view is
/// text-only and never consults it. A caller with no artwork cache passes
/// [`NoArtwork`](tairix_icon::NoArtwork), which always returns `None` (every
/// tile then draws its built-in glyph).
///
/// The window's own ground is `theme`'s surface on its ground: solid on an
/// opaque theme, and the translucent glass on a frosted one.
///
/// `scale` is the desktop's density factor: every chrome length here is
/// authored logically and converted through it, and the text face is the one
/// `theme`'s ladder names at that scale — the caller never chooses a typeface.
///
/// Only `viewport`'s dimensions are used; the window manager places the
/// surface at `viewport`'s origin.
pub fn render_into<S: DirectorySource>(
    surface: &mut Surface,
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    chrome: &ManagerChrome<'_>,
    artwork: &mut dyn IconArtwork,
) {
    let palette = theme.palette();

    surface.fill(ground_fill(theme, palette.surface, ChromeLayer::Ground).into());
    let area = content_area(viewport, scale, theme, chrome.sidebar, chrome.toolbar);
    if let (Some(places), Some(view)) = (
        chrome.sidebar,
        sidebar_view(viewport, scale, theme, chrome.sidebar, chrome.toolbar),
    ) {
        let selected = places.index_of(browser.components());
        draw_sidebar(surface, scale, theme, places, &view, selected, artwork);
    }
    // The toolbar is window chrome: its band spans the full window, above the
    // rail, so it aligns with the rest of the desktop's chrome. Everything
    // below it is laid out in `area`, the window less the rail.
    if chrome.toolbar.is_shown() {
        draw_toolbar(
            surface,
            scale,
            theme,
            browser,
            viewport,
            chrome.tools,
            chrome.tool_model,
            artwork,
        );
    }

    let content = content_viewport(area, scale, theme);
    if awaiting_listing(browser) {
        draw_listing_cue(surface, scale, theme, content);
        return;
    }
    match browser.view_mode() {
        ViewMode::List => draw_list(
            surface,
            scale,
            theme,
            browser,
            content,
            chrome.toolbar,
            artwork,
        ),
        ViewMode::Grid => draw_grid(
            surface,
            scale,
            theme,
            browser,
            content,
            chrome.toolbar,
            artwork,
        ),
    }
    draw_scrollbar(surface, scale, theme, browser, area, chrome.toolbar);
}

/// What the listing area says while a directory read is still in flight.
///
/// One definition, so the drawn cue and any observer of it (a test, a QEMU
/// vertical reading the scan-out) agree on the exact text.
pub const LISTING_MESSAGE: &str = "Listing…";

/// Whether the listing area should show [`LISTING_MESSAGE`] instead of items.
///
/// Two cases, one rule. A read of *somewhere else* is in flight, so the items on
/// screen belong to a directory the user has already asked to leave and showing
/// them as if current would be a lie. Or there are no items to show at all — a
/// window that has just opened — where a blank area says nothing. A re-read of
/// the directory already shown is neither: it keeps its items, so a periodic
/// re-list cannot make the view flicker.
fn awaiting_listing<S: DirectorySource>(browser: &Browser<S>) -> bool {
    match browser.listing_target() {
        None => false,
        Some(target) => target != browser.components() || browser.entries().is_empty(),
    }
}

/// Draw [`LISTING_MESSAGE`] centred in `content`, in the muted ink an inactive
/// label uses: it is a state, not an error.
fn draw_listing_cue(surface: &mut Surface, scale: Scale, theme: &Theme, content: Rect) {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let width = font.text_width(LISTING_MESSAGE);
    let x = content
        .left()
        .saturating_add_unsigned(content.width.saturating_sub(width) / 2);
    let y = content
        .top()
        .saturating_add_unsigned(content.height.saturating_sub(font.glyph_height()) / 2);
    font.draw_text(
        surface,
        x,
        y,
        LISTING_MESSAGE,
        theme.palette().on_surface_muted.into(),
    );
}

/// The manager-only chrome drawn around the shared browser view.
///
/// The file manager owns write tools and a places rail; the trusted file
/// picker owns neither, and passes [`ManagerChrome::none`]. Grouping them
/// keeps the pieces that appear and disappear together in one value, so a
/// caller cannot draw a rail's rows while hit-testing a window that has none.
///
/// The picker's emptiness is deliberate, not an omission to fill in later. It
/// is a read-only chooser: it has no write authority, so a write tool would be
/// a control it could never honour; and its whole purpose is bounded to the
/// directory tree the requesting application was authorised to be shown, so a
/// rail offering one-click jumps to arbitrary mounted volumes would widen the
/// pick beyond what was asked for. It draws the listing and nothing else.
pub struct ManagerChrome<'a> {
    /// The manager write tools drawn after the shared read-only commands.
    pub tools: &'a [ManagerTool],
    /// Each write tool's enable state; a disabled tool renders muted.
    pub tool_model: ManagerToolModel,
    /// The places rail drawn down the window's leading edge, or `None` for a
    /// view with no rail (the window is then laid out exactly as it is with no
    /// sidebar at all).
    pub sidebar: Option<&'a Places>,
    /// Whether the command toolbar strip is drawn across the top.
    pub toolbar: ToolbarBand,
}

impl ManagerChrome<'_> {
    /// The chrome of a view with no manager surface at all: no write tools, no
    /// enable model, and no places rail — but the shared read-only command
    /// toolbar, which is not a manager surface and which every consumer of the
    /// browser draws by default.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            tools: &[],
            tool_model: ManagerToolModel::none(),
            sidebar: None,
            toolbar: ToolbarBand::Shown,
        }
    }
}

/// The width of the places rail: a row's icon column, the widest fixed place
/// label, and the row padding around them, all measured from the drawn face
/// and the theme's metrics rather than a fixed pixel count, so the rail tracks
/// the interface's density. Clamped to a third of the window so a narrow
/// window keeps most of its width for the listing.
fn sidebar_width(scale: Scale, theme: &Theme, font: BitmapFont, viewport_width: u32) -> u32 {
    let pad = scale.scale_length(theme.metrics().control_inset).max(1);
    font.glyph_height()
        .saturating_add(font.text_width(places::WIDEST_FIXED_LABEL))
        .saturating_add(pad.saturating_mul(3))
        .min(viewport_width / 3)
}

/// The height of the band separating the user's own places from the mounted
/// volumes: the theme's control gap, so the separation reads at the same
/// rhythm as every other gap in the interface.
fn separator_height(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1)
}

/// The places rail's geometry for `sidebar` within `window` (the **whole**
/// window), or `None` when there is no rail to draw (no model, or a model with
/// no rows).
///
/// The rail is laid out *below* the command toolbar band — inset at the top by
/// [`chrome_height`], with the rest of the window's height — because the
/// toolbar is window chrome that spans the full width. A view with no toolbar
/// reserves no band, so the rail starts at the top of the window. Its row
/// pitch is [`row_height`], the pitch the list rows use, so the rail's
/// rows land on exactly the row grid of the listing beside them.
///
/// The one definition the painter, the pointer hit-test
/// ([`sidebar_index_at`]), the content inset ([`content_area`]) and the rail's
/// scrolling all read, so the drawn rail and every measurement of it agree by
/// construction. It is scrolled to where `sidebar`'s own column rests.
#[must_use]
pub fn sidebar_view(
    window: Rect,
    scale: Scale,
    theme: &Theme,
    sidebar: Option<&Places>,
    toolbar: ToolbarBand,
) -> Option<SidebarView> {
    let places = sidebar?;
    if places.is_empty() {
        return None;
    }
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let band = chrome_height(scale, theme, toolbar);
    Some(SidebarView::new(
        Rect::new(
            window.origin.x,
            window.origin.y.saturating_add_unsigned(band),
            window.width,
            window.height.saturating_sub(band),
        ),
        sidebar_width(scale, theme, font, window.width),
        (row_height(scale, theme), separator_height(scale, theme)),
        places.len(),
        places.volume_start(),
        (
            places.scroll().offset(),
            scale.scale_length(theme.metrics().scrollbar_breadth).max(1),
        ),
    ))
}

/// The window area the item view and the scrollbar occupy: the whole
/// `window`, less the places rail on the leading edge when one is drawn.
///
/// The command toolbar is **not** measured here. It is window chrome: its band
/// spans the full window width across the top, above the rail and over this
/// area's own top strip, so [`toolbar_command_at`], [`manager_tool_at`], and
/// [`manager_tool_rect`] take the *window* while every entry point below the
/// band — [`entry_index_at`], [`entry_rect`], [`scrollbar_bounds`] and the
/// overlays — takes this area. A caller drawing a rail resolves this once and
/// passes it wherever it would otherwise pass the window; the rows and the
/// scrollbar then sit exactly where a click looks for them.
///
/// A caller with no rail gets the window back unchanged, so a view without a
/// sidebar (the trusted file picker) is laid out precisely as it was before
/// there was one.
#[must_use]
pub fn content_area(
    window: Rect,
    scale: Scale,
    theme: &Theme,
    sidebar: Option<&Places>,
    toolbar: ToolbarBand,
) -> Rect {
    let Some(view) = sidebar_view(window, scale, theme, sidebar, toolbar) else {
        return window;
    };
    let rail = view.width();
    Rect::new(
        window.origin.x.saturating_add_unsigned(rail),
        window.origin.y,
        window.width.saturating_sub(rail),
        window.height,
    )
}

/// The places-rail row at window-local pixel `point`, or `None` when the point
/// is not on one — above the rail in the toolbar band, outside the rail, on its
/// bar, in the separation between the user's places and the volumes, or below
/// the last row.
///
/// Takes the **whole** window, the rectangle [`sidebar_view`] lays the rail out
/// in; it is the exact inverse of what [`render_into`] painted, through that one
/// shared geometry and the rail's own scroll.
#[must_use]
pub fn sidebar_index_at(
    window: Rect,
    scale: Scale,
    theme: &Theme,
    sidebar: Option<&Places>,
    toolbar: ToolbarBand,
    point: Point,
) -> Option<usize> {
    sidebar_view(window, scale, theme, sidebar, toolbar)?.index_at(point)
}

/// Route a pointer `event` at window-local `point` to the places rail's bar,
/// moving the rail's column: `None` when the rail shows no bar or the pointer
/// had nothing to do with it, otherwise whether it repainted anything.
///
/// The same routing the listing's bar takes, so the two behave alike. The bar
/// reports its own look, and a move reports the rows it slid.
pub fn sidebar_scroll_pointer(
    places: &mut Places,
    (window, scale, theme): (Rect, Scale, &Theme),
    toolbar: ToolbarBand,
    pointer: (Point, &InputEvent),
    damage: &mut Region,
) -> Option<bool> {
    let view = sidebar_view(window, scale, theme, Some(places), toolbar)?;
    let bar = view.bar_rect()?;
    places.scroll_mut().route(
        view.scroll_model(),
        (bar, view.rows_area()),
        scale,
        theme,
        pointer,
        damage,
    )
}

/// Scroll the places rail by a wheel turn of `(dx, dy)`, in the seat's scroll
/// units, answering whether it moved. A rail whose rows fit has nothing to
/// scroll. A move reports the bar and the rows it slid.
pub fn sidebar_scroll_wheel(
    places: &mut Places,
    (window, scale, theme): (Rect, Scale, &Theme),
    toolbar: ToolbarBand,
    delta: (i32, i32),
    damage: &mut Region,
) -> bool {
    let Some(view) = sidebar_view(window, scale, theme, Some(places), toolbar) else {
        return false;
    };
    let Some(bar) = view.bar_rect() else {
        return false;
    };
    places.scroll_mut().wheel(
        view.scroll_model(),
        delta,
        scale,
        (bar, view.rows_area()),
        damage,
    )
}

/// Scroll the places rail the least that shows its keyboard cursor's row
/// whole, answering whether it moved. A move reports the rows and the bar.
pub fn sidebar_reveal(
    places: &mut Places,
    (window, scale, theme): (Rect, Scale, &Theme),
    toolbar: ToolbarBand,
    damage: &mut Region,
) -> bool {
    let Some(view) = sidebar_view(window, scale, theme, Some(places), toolbar) else {
        return false;
    };
    let revealed = view.reveal(places.cursor());
    let moved = revealed != view.scroll_model().offset();
    places.scroll_mut().set_offset(revealed);
    if moved {
        damage.add(view.rows_area());
        if let Some(bar) = view.bar_rect() {
            damage.add(bar);
        }
    }
    moved
}

/// Paint the places rail: its raised band, one shared [`ListRow`] per place,
/// and the hairline separating the user's own places from the mounted
/// volumes.
///
/// Each row asks `artwork` for its icon at exactly the slot the row will draw
/// it in, so a volume shows the artwork for the medium it really sits on and
/// falls back to the built-in glyph when the system has no asset for it. The
/// rows are painted through the rail's scrolled view, so a row its edge crosses
/// is drawn whole and cut there, and only the rows it shows ask for artwork.
fn draw_sidebar(
    surface: &mut Surface,
    scale: Scale,
    theme: &Theme,
    places: &Places,
    view: &SidebarView,
    selected: Option<usize>,
    artwork: &mut dyn IconArtwork,
) {
    let palette = theme.palette();
    let rail = view.rail_rect();
    let rail_x = u32::try_from(rail.origin.x).unwrap_or(0);
    let rail_y = u32::try_from(rail.origin.y).unwrap_or(0);
    surface.fill_rect(
        rail_x,
        rail_y,
        rail.width,
        rail.height,
        palette.surface_raised.into(),
    );
    let shown = view.view();
    shown.paint(surface, |surface| {
        if let Some(band) = view.separator_rect() {
            let pad = scale.scale_length(theme.metrics().control_inset).max(1);
            let x = u32::try_from(band.origin.x)
                .unwrap_or(0)
                .saturating_add(pad);
            let y = u32::try_from(band.origin.y)
                .unwrap_or(0)
                .saturating_add(band.height / 2);
            surface.fill_rect(
                x,
                y,
                band.width.saturating_sub(pad.saturating_mul(2)),
                1,
                palette.on_surface_muted.into(),
            );
        }
        for index in view.visible_range() {
            let (Some(place), Some(bounds)) = (places.rows().get(index), view.row_rect(index))
            else {
                continue;
            };
            let row = place_row(place, places, index, selected);
            let side = row.icon_side(bounds, scale, theme);
            let art = artwork.artwork(IconRequest::kind(place.icon()), side);
            row.render(surface, bounds, scale, theme, art);
        }
    });
    if let Some(bar) = view.bar_rect() {
        draw_bar(
            places.scroll().scrollbar(),
            view.scroll_model(),
            surface,
            bar,
            scale,
            theme,
        );
    }
}

/// Build the shared [`ListRow`] for one rail row, carrying every state the
/// rail can put it in: a place whose target was refused reads disabled (and
/// never also hovered — a control the user cannot use does not light up under
/// the pointer), the row under the pointer hovers, the keyboard cursor's row
/// is focused while the rail owns focus, and the row matching the browser's
/// current location reads selected through the control's own selection state
/// rather than a highlight painted here.
fn place_row(place: &Place, places: &Places, index: usize, selected: Option<usize>) -> ListRow {
    let mut state = if place.is_available() {
        let idle = ControlState::idle();
        if places.hovered() == Some(index) {
            idle.with_pointer(PointerState::Hover)
        } else {
            idle
        }
    } else {
        ControlState::disabled()
    };
    if selected == Some(index) {
        state = state.with_selection(SelectionState::Selected);
    }
    let mut row = ListRow::new(place.label())
        .with_icon(place.icon())
        .with_state(state);
    row.set_in_focus_field(places.is_focused());
    row.set_focused(places.is_focused() && places.cursor() == index);
    row
}

/// Draw the visible list rows below the toolbar as shared [`TableRow`]s,
/// giving the selected entry the row chrome's selection state.
///
/// Each row's identity icon resolves through `artwork` at the row's own icon
/// side, exactly as a grid tile's does — so a row draws the shipped class
/// artwork where there is any and the cached built-in glyph otherwise, and
/// **no** row rasterises vector coverage a previous frame already resolved.
/// Only the rows on screen are asked for.
///
/// A row asks by *class* rather than naming a bundle: a list is a dense text
/// view of a directory that may hold hundreds of bundles, and reading each
/// one's manifest to find its own icon is work a row-height picture cannot
/// show. The grid, whose tiles are large enough to tell two applications
/// apart, names the bundle.
fn draw_list<S: DirectorySource>(
    surface: &mut Surface,
    scale: Scale,
    theme: &Theme,
    browser: &Browser<S>,
    content: Rect,
    toolbar: ToolbarBand,
    artwork: &mut dyn IconArtwork,
) {
    let view = list_view(browser, scale, theme, content, toolbar);
    let offset = browser.scroll_offset();
    let selected = browser.selected_index();
    let parent = browser.components();
    let entries = browser.entries();
    view.view(offset).paint(surface, |surface| {
        for index in view.visible_range(offset) {
            let (Some(entry), Some(bounds)) = (entries.get(index), view.row_rect(index)) else {
                break;
            };
            let kind = icon_for_entry(entry, parent);
            let row = entry_row(entry, selected == Some(index), kind);
            let side = TableRow::icon_side(bounds, scale, theme);
            let art = artwork.artwork(IconRequest::kind(kind), side);
            row.render(surface, bounds, scale, theme, &COLUMNS, art);
        }
    });
}

/// Draw the visible icon-grid tiles below the toolbar as shared [`IconTile`]s,
/// giving the selected entry the tile's selection state.
///
/// Each tile's icon is the shared classification ([`icon_for_entry`]): the
/// entry's content type and folder occupancy
/// decide an [`IconKind`], and `artwork` is asked for a
/// pre-rasterised surface at the tile's [`IconTile::icon_side`] slot. When it
/// supplies one the tile draws that artwork; otherwise the tile falls back to
/// the built-in glyph for the kind. The classification is resolved once here so
/// the manager and picker draw the same icon for the same entry.
///
/// An application bundle additionally names *itself* in the request, so the
/// artwork layer can prefer the icon the bundle carries in its own
/// `Resources/` over the generic bundle artwork. Only the tiles actually on
/// screen are asked for, so browsing a store of a thousand applications reads
/// and decodes only the ones in view.
///
/// A row holds only whole tiles and spreads its leftover width between them
/// ([`GridFill::Spread`]), so a widened window shares the extra space out
/// evenly until one more tile fits. Painting goes through the grid's scrolled
/// view, confined to the item area, so no tile can encroach on the scrollbar
/// gutter beside it or the chrome above it, and a row the view's edge crosses
/// is drawn whole and cut there.
fn draw_grid<S: DirectorySource>(
    surface: &mut Surface,
    scale: Scale,
    theme: &Theme,
    browser: &Browser<S>,
    content: Rect,
    toolbar: ToolbarBand,
    artwork: &mut dyn IconArtwork,
) {
    let view = grid_view(browser, scale, theme, content, toolbar);
    let offset = browser.scroll_offset();
    let selected = browser.selected_index();
    let parent = browser.components();
    let entries = browser.entries();
    // Spelled once for the whole frame; each bundle tile appends its own leaf
    // into one reused buffer rather than allocating a path per tile.
    let dir = crate::vfs::spell_absolute_path(parent);
    let mut bundle = String::new();
    view.view(offset).paint(surface, |surface| {
        for index in view.visible_range(offset) {
            let (Some(entry), Some(bounds)) = (entries.get(index), view.cell_rect(index)) else {
                break;
            };
            let kind = icon_for_entry(entry, parent);
            let request = entry_icon_request(&dir, entry, kind, &mut bundle);
            let mut state = ControlState::idle();
            if selected == Some(index) {
                state.selection = SelectionState::Selected;
            }
            let tile = grid_tile(entry, state, kind);
            let side = IconTile::icon_side(bounds, scale, theme);
            let art = artwork.artwork(request, side);
            tile.render(surface, bounds, scale, theme, art);
        }
    });
}

/// Draw the vertical [`ScrollBar`] in the reserved right-edge gutter, spanning
/// the item area below the toolbar. A viewport with no room for the gutter
/// (or with no scrollable content) simply draws nothing there.
fn draw_scrollbar<S: DirectorySource>(
    surface: &mut Surface,
    scale: Scale,
    theme: &Theme,
    browser: &Browser<S>,
    viewport: Rect,
    toolbar: ToolbarBand,
) {
    let Some(bounds) = scrollbar_bounds(scale, theme, viewport, toolbar) else {
        return;
    };
    draw_bar(
        browser.scroll().scrollbar(),
        scroll_model(browser, scale, theme, viewport, toolbar),
        surface,
        bounds,
        scale,
        theme,
    );
}

/// Draw a column's `bar` — its live hover, drag and held state — at `bounds`
/// over `model`, the geometry the column is drawn at this frame.
///
/// The bar is `Copy`, so the drawn one carries the column's interaction state
/// without disturbing the column that owns it.
fn draw_bar(
    bar: &ScrollBar,
    model: ScrollModel,
    surface: &mut Surface,
    bounds: Rect,
    scale: Scale,
    theme: &Theme,
) {
    let mut bar = *bar;
    bar.set_model(model);
    bar.render(surface, bounds, scale, theme);
}

/// The screen rectangle (window-local) the vertical [`ScrollBar`] occupies: the
/// reserved right-edge gutter spanning the item area below the toolbar, or
/// `None` when the window is too narrow for a gutter or too short for any item
/// area. This is the exact geometry the drawn scrollbar paints into (and that
/// [`scroll_pointer`] hit-tests against), so a pointer hit-test and the drawn
/// bar can never disagree.
#[must_use]
pub fn scrollbar_bounds(
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) -> Option<Rect> {
    let gutter = gutter_width(scale, theme, viewport.width);
    let header = chrome_height(scale, theme, toolbar);
    if gutter == 0 || viewport.height <= header {
        return None;
    }
    let content = content_viewport(viewport, scale, theme);
    Some(Rect::new(
        content.origin.x.saturating_add_unsigned(content.width),
        content
            .origin
            .y
            .saturating_add_unsigned(header.min(i32::MAX.unsigned_abs())),
        gutter,
        viewport.height.saturating_sub(header),
    ))
}

/// Route a pointer `event` (a primary press, release, or a motion) to the
/// browser's interactive scrollbar, returning `Some(repainted)` when the bar
/// took it (so the caller does not also treat the press as a click in the
/// content) — whether that reported anything — and `None` when the pointer had
/// nothing to do with the bar (the caller handles it as content input).
///
/// The bar owns the interaction the press started: a press on an end button or
/// track region steps the offset once, a press on the thumb captures a drag,
/// and the subsequent motions and the release are routed here (the window
/// manager's client pointer grab delivers them) until the release ends it. A
/// hover over the bar is taken so the bar can brighten. The bar reports its
/// own look, and a move reports the item area it slid. `event` must carry the
/// window-local pointer position (a press/release is preceded here by a
/// synthetic move to that position, exactly as the window controls are fed).
#[allow(clippy::too_many_arguments)] // The bar's geometry, the sample, and the round's report.
pub fn scroll_pointer<S: DirectorySource>(
    browser: &mut Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    point: Point,
    event: &InputEvent,
    damage: &mut Region,
) -> Option<bool> {
    let bounds = scrollbar_bounds(scale, theme, viewport, toolbar)?;
    let view = view_layout_for(browser, scale, theme, viewport, toolbar);
    let model = view.scroll_model(browser.scroll_offset());
    let shown = view.view(model.offset()).viewport();
    browser
        .scroll_mut()
        .route(model, (bounds, shown), scale, theme, (point, event), damage)
}

/// Build the [`TableRow`] for one list entry: a leading name cell carrying the
/// entry's `icon`, a trailing numeric size cell (blank for a directory, a
/// bundle, or a link, none of which carries a meaningful byte size — a link's
/// own size is the length of the path it stores), and a modified-date cell.
///
/// `icon` is the shared classification ([`icon_for_entry`]) the grid tile
/// draws too, so a row and a tile can never picture the same entry
/// differently. The name cell paints the built-in glyph for that kind — the
/// row control takes an [`IconKind`], not cached artwork — which is what makes
/// the kind readable in a view whose rows are otherwise text.
fn entry_row(entry: &Entry, selected: bool, icon: IconKind) -> TableRow {
    let size = if matches!(entry.kind(), EntryKind::File) {
        format_size(entry.size())
    } else {
        String::new()
    };
    let cells = vec![
        TableCell::new(entry_label(entry)).with_icon(icon),
        TableCell::numeric(size),
        TableCell::new(format_date(entry.modified())),
    ];
    let mut row = TableRow::new(cells);
    row.set_selected(selected);
    row
}

/// Build the [`IconTile`] for one grid entry: the entry's file-type `icon`
/// above its label, carrying the shared selection state when selected. The
/// `icon` is the shared classification ([`icon_for_entry`]) resolved by the
/// caller — a display hint only, decided once so a tile and a list row draw
/// the same icon for the same entry.
#[must_use]
pub fn grid_tile(entry: &Entry, state: ControlState, icon: IconKind) -> IconTile {
    IconTile::new(entry_label(entry), icon).with_state(state)
}

/// The name shown for an entry: exactly the name the volume holds.
///
/// No kind suffix is appended — both views carry the entry's icon
/// ([`icon_for_entry`]), so the label is the name and nothing else, and what
/// the user reads on screen is what they type, copy, and rename.
#[must_use]
pub fn entry_label(entry: &Entry) -> String {
    String::from(entry.name())
}

/// Height in pixels of one rendered list row, measured on the theme's own body
/// face at `scale` exactly as [`render_into`] draws them, so hit-testing and
/// painting can never disagree.
#[must_use]
pub fn row_height(scale: Scale, theme: &Theme) -> u32 {
    BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
        .glyph_height()
        .saturating_add(scale.scale_length(ROW_PADDING).saturating_mul(2))
}

/// The width of the reserved scrollbar gutter for a `viewport_width`-pixel
/// window: the theme's scrollbar breadth, clamped so it never exceeds the
/// window (a window too narrow for the gutter simply has none).
fn gutter_width(scale: Scale, theme: &Theme, viewport_width: u32) -> u32 {
    scale
        .scale_length(theme.metrics().scrollbar_breadth)
        .max(1)
        .min(viewport_width)
}

/// The command toolbar's band: the full width of `window`, at its top.
///
/// The toolbar is window chrome, so it spans the whole window rather than the
/// rail-inset [`content_area`] — it reaches the window's leading edge and the
/// places rail begins below it. One definition, so the drawn toolbar and each
/// of the three hit-tests that invert it cannot place it differently.
/// A hidden band has no rectangle at all rather than a flat one: the shared
/// [`Toolbar`] lays its buttons out from the origin it is given and would
/// resolve a press on the window's top row against a strip nothing painted.
fn toolbar_bounds(scale: Scale, theme: &Theme, window: Rect, toolbar: ToolbarBand) -> Option<Rect> {
    toolbar.is_shown().then(|| {
        Rect::new(
            window.origin.x,
            window.origin.y,
            window.width,
            chrome_height(scale, theme, toolbar),
        )
    })
}

/// The content viewport (the window minus the reserved scrollbar gutter). The
/// item views lay out within this, so no item ever underlaps the scrollbar.
fn content_viewport(viewport: Rect, scale: Scale, theme: &Theme) -> Rect {
    let gutter = gutter_width(scale, theme, viewport.width);
    Rect::new(
        viewport.origin.x,
        viewport.origin.y,
        viewport.width.saturating_sub(gutter),
        viewport.height,
    )
}

/// The dimensions of one grid tile and the gap between tiles, measured on the
/// theme's own body face at `scale`, so a tile grows with the desktop's
/// density instead of staying a fixed pixel count.
///
/// Shared with the desktop's icon column, which lays the same tiles out under
/// a different [`GridFlow`], so the two views can never disagree about how big
/// an icon tile is.
#[must_use]
pub fn grid_metrics(scale: Scale, theme: &Theme) -> GridMetrics {
    let glyph = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
        .glyph_height()
        .max(1);
    GridMetrics {
        cell_width: glyph.saturating_mul(6).max(scale.scale_length(48)),
        cell_height: glyph.saturating_mul(5).max(scale.scale_length(48)),
        gap: (glyph / 2).max(scale.scale_length(2)),
    }
}

/// The height in pixels of the command toolbar strip at the top of the window:
/// the theme's control height plus a gap above and below, scaled to physical
/// pixels. One definition so the drawn toolbar, the item area, and the
/// hit-tests all agree on where the chrome band sits.
#[must_use]
pub fn toolbar_height(scale: Scale, theme: &Theme) -> u32 {
    let metrics = theme.metrics();
    let gap = scale.scale_length(metrics.control_gap);
    scale
        .scale_length(metrics.control_height)
        .saturating_add(gap.saturating_mul(2))
        .max(1)
}

/// The total height reserved for the window chrome above the item area: the
/// command toolbar strip when `toolbar` is drawn, and nothing else. This is
/// the header the item views lay out below and the top of the scrollbar
/// gutter, so paint and hit-test share one offset — and it is zero for a view
/// with no strip, whose listing therefore starts at the top of the window
/// rather than below an empty band.
#[must_use]
pub fn chrome_height(scale: Scale, theme: &Theme, toolbar: ToolbarBand) -> u32 {
    if toolbar.is_shown() {
        toolbar_height(scale, theme)
    } else {
        0
    }
}

/// The group each toolbar command belongs to, so related commands read as a
/// unit with a quiet divider between groups: navigation, refresh, and the
/// view/sort presentation controls.
const fn toolbar_group(command: ToolbarCommand) -> u16 {
    match command {
        ToolbarCommand::Back | ToolbarCommand::Forward | ToolbarCommand::Up => 0,
        ToolbarCommand::Refresh => 1,
        ToolbarCommand::ToggleView | ToolbarCommand::Sort => 2,
    }
}

/// The toolbar group the manager-only write tools sit in — after the read-only
/// navigation/refresh/view groups (0..=2), so a quiet divider sets them apart.
const MANAGER_TOOL_GROUP: u16 = 3;

/// Build the drawn command toolbar for `model`: one [`IconButton`] per
/// [`chrome::TOOLBAR_COMMANDS`] entry, in order, each carrying the command's
/// glyph and rendered disabled (not hidden) when the model reports the command
/// is not currently actionable, so the toolbar's shape is stable. The
/// manager-only write `tools` follow the read-only commands (a picker passes an
/// empty slice), so their [`Toolbar`] indices are
/// `chrome::TOOLBAR_COMMANDS.len() + i`.
fn build_toolbar(
    model: ToolbarModel,
    tools: &[ManagerTool],
    tool_model: ManagerToolModel,
) -> Toolbar {
    let mut toolbar = Toolbar::new();
    for &command in chrome::TOOLBAR_COMMANDS {
        let mut button = IconButton::new(command.icon(), ControlRole::Navigation);
        if !model.is_enabled(command) {
            button.set_state(ControlState::disabled());
        }
        toolbar = toolbar.with_icon(button, toolbar_group(command));
    }
    for &tool in tools {
        let mut button = IconButton::new(tool.icon(), ControlRole::Neutral);
        if !tool_model.is_enabled(tool) {
            button.set_state(ControlState::disabled());
        }
        toolbar = toolbar.with_icon(button, MANAGER_TOOL_GROUP);
    }
    toolbar
}

/// The width the widest command toolbar — every read-only command plus every
/// manager write tool — needs to seat every tool at `scale`.
///
/// A browser window's declared floor is derived from this rather than
/// hand-picked, so the strip can never be handed a band too narrow for its
/// own tools: the shared [`Toolbar`] would then scroll, and a window whose
/// toolbar is rebuilt per frame holds no offset to scroll with.
#[must_use]
pub fn toolbar_natural_width(scale: Scale, theme: &Theme) -> u32 {
    build_toolbar(
        ToolbarModel::all_enabled(),
        chrome::MANAGER_TOOLS,
        ManagerToolModel::new(true),
    )
    .natural_width(scale, theme)
}

/// Draw the command toolbar in the top strip: [`chrome::TOOLBAR_COMMANDS`] then
/// the manager-only write `tools`, as themed [`IconButton`]s over the
/// [`ToolbarModel`], spanning the full window width above the item view. A
/// disabled command reads muted rather than vanishing (the model decides
/// which).
#[allow(clippy::too_many_arguments)] // The band, its model, its tools, and the icon lookup.
fn draw_toolbar<S: DirectorySource>(
    surface: &mut Surface,
    scale: Scale,
    theme: &Theme,
    browser: &Browser<S>,
    window: Rect,
    tools: &[ManagerTool],
    tool_model: ManagerToolModel,
    artwork: &mut dyn IconArtwork,
) {
    let toolbar = build_toolbar(ToolbarModel::for_browser(browser), tools, tool_model);
    let Some(bounds) = toolbar_bounds(scale, theme, window, ToolbarBand::Shown) else {
        return;
    };
    toolbar.render(surface, bounds, scale, theme, artwork);
}

/// The actionable toolbar command at window-local pixel `point`, or `None`
/// when the click is not on one — outside the toolbar band, on a group
/// gutter, on a manager write tool, or on a command the [`ToolbarModel`] has
/// disabled (fail closed: a disabled tool does not act). It mirrors the
/// drawn toolbar's own layout so a click resolves to exactly the tool
/// [`render_into`] painted. The read-only commands keep the same positions
/// whether or not write tools follow them, so this needs no `tools` argument.
///
/// `window` is the **whole** window, the rectangle the toolbar band spans —
/// not the rail-inset [`content_area`] the listing takes.
#[must_use]
pub fn toolbar_command_at<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    band: ToolbarBand,
    point: Point,
) -> Option<ToolbarCommand> {
    let model = ToolbarModel::for_browser(browser);
    let toolbar = build_toolbar(model, &[], ManagerToolModel::none());
    let bounds = toolbar_bounds(scale, theme, window, band)?;
    let index = toolbar.tool_at(bounds, scale, theme, point)?;
    let command = *chrome::TOOLBAR_COMMANDS.get(index)?;
    model.is_enabled(command).then_some(command)
}

/// The manager-only write [`ManagerTool`] at window-local pixel `point`, or
/// `None` when the click is not on one. `tools` is the same set handed to
/// [`render_into`] (a read-only picker passes an empty slice and so never resolves a
/// write tool). The full toolbar — read-only commands then the write tools — is
/// rebuilt so the write tools sit at exactly the positions [`render_into`] painted
/// them; a hit resolves only in the write-tool index range, so a click
/// on a read-only command returns `None` here (it is handled by
/// [`toolbar_command_at`]). `tool_model` is the same enable state handed to
/// [`render_into`]: a click on a tool the model has disabled resolves to `None`
/// (fail closed — a disabled tool does not act).
///
/// `window` is the **whole** window, the rectangle the toolbar band spans —
/// not the rail-inset [`content_area`] the listing takes.
#[must_use]
#[allow(clippy::too_many_arguments)] // The band, the sample, and the tools with their enable state.
pub fn manager_tool_at<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    band: ToolbarBand,
    point: Point,
    tools: &[ManagerTool],
    tool_model: ManagerToolModel,
) -> Option<ManagerTool> {
    let toolbar = build_toolbar(ToolbarModel::for_browser(browser), tools, tool_model);
    let bounds = toolbar_bounds(scale, theme, window, band)?;
    let index = toolbar.tool_at(bounds, scale, theme, point)?;
    let tool_index = index.checked_sub(chrome::TOOLBAR_COMMANDS.len())?;
    let tool = tools.get(tool_index).copied()?;
    tool_model.is_enabled(tool).then_some(tool)
}

/// The window-local [`Rect`] the manager write `tool` occupies, or `None`
/// when `tool` is not among `tools`. The forward mirror of
/// [`manager_tool_at`] over the same rebuilt toolbar (read-only commands then
/// the write tools), so a caller that must aim *at* a write tool — the desktop
/// integration harness that clicks New Folder — reads the exact geometry
/// [`render_into`] paints and [`manager_tool_at`] hit-tests, never a hand-copied
/// position. Fails closed: an out-of-range or unlisted tool is `None`.
///
/// The toolbar left-packs fixed-width buttons and a disabled tool renders in
/// place (muted, never hidden), so a tool's rectangle is independent of its
/// enable state; the geometry is built with every tool enabled
/// ([`ManagerToolModel::new(true)`](ManagerToolModel::new)) and a caller that
/// must only *act* on an enabled tool gates that through [`manager_tool_at`].
///
/// `window` is the **whole** window, the rectangle the toolbar band spans —
/// not the rail-inset [`content_area`] the listing takes.
#[must_use]
pub fn manager_tool_rect<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    band: ToolbarBand,
    tools: &[ManagerTool],
    tool: ManagerTool,
) -> Option<Rect> {
    let position = tools.iter().position(|&t| t == tool)?;
    let toolbar = build_toolbar(
        ToolbarModel::for_browser(browser),
        tools,
        ManagerToolModel::new(true),
    );
    let bounds = toolbar_bounds(scale, theme, window, band)?;
    let index = chrome::TOOLBAR_COMMANDS.len().checked_add(position)?;
    toolbar.tool_rect(index, bounds, scale, theme)
}

/// The [`ListView`] for `browser` at the given content viewport.
fn list_view<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    content: Rect,
    toolbar: ToolbarBand,
) -> ListView {
    ListView::new(
        content,
        row_height(scale, theme),
        chrome_height(scale, theme, toolbar),
        browser.entries().len(),
    )
}

/// The [`GridView`] for `browser` at the given content viewport.
///
/// The window is resizable, so the grid spreads ([`GridFill::Spread`]): a row's
/// leftover width is shared out evenly between its tiles rather than parked as
/// a blank margin at the trailing edge, and widening the window past one more
/// tile re-flows the listing into the extra column.
fn grid_view<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    content: Rect,
    toolbar: ToolbarBand,
) -> GridView {
    GridView::new(
        content,
        grid_metrics(scale, theme),
        chrome_height(scale, theme, toolbar),
        browser.entries().len(),
        GridFlow::RowsFromLeading,
        GridFill::Spread,
    )
}

/// The scroll model the drawn [`ScrollBar`] and the wheel share: the active
/// view's clamped [`ScrollRange`] in pixels, stepping a row (or a line of
/// tiles) a line. `theme` supplies the scrollbar gutter width so the model
/// measures the same content viewport the renderer draws.
#[must_use]
pub fn scroll_model<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) -> ScrollModel {
    view_layout_for(browser, scale, theme, viewport, toolbar).scroll_model(browser.scroll_offset())
}

/// The client height a browser window `width` pixels wide needs to show its
/// whole listing, and the places rail beside it, with nothing below them —
/// the ceiling that keeps a blank band from opening beneath the items. `None`
/// while the listing is still being read, when what it holds is not known.
///
/// Height plays no part in it: at one width the items wrap, and the rail lays
/// out, identically however tall the window is.
#[must_use]
pub fn fitted_height<S: DirectorySource>(
    browser: &Browser<S>,
    width: u32,
    scale: Scale,
    theme: &Theme,
    sidebar: Option<&Places>,
    toolbar: ToolbarBand,
) -> Option<u32> {
    if awaiting_listing(browser) {
        return None;
    }
    let window = Rect::new(0, 0, width, 0);
    let area = content_area(window, scale, theme, sidebar, toolbar);
    let items = view_layout_for(browser, scale, theme, area, toolbar)
        .scroll_model(0)
        .range()
        .content_extent();
    let rail = sidebar_view(window, scale, theme, sidebar, toolbar)
        .map_or(0, |rail| rail.content_height());
    let content = u32::try_from(items.max(rail)).unwrap_or(u32::MAX);
    Some(chrome_height(scale, theme, toolbar).saturating_add(content))
}

/// Scroll by the wheel's `(dx, dy)`, in the seat's scroll units, through the
/// browser's own bar, answering whether the listing moved.
///
/// The bar carries what is short of a whole pixel into the next turn. A move
/// reports the bar and the items it slid.
pub fn scroll_wheel<S: DirectorySource>(
    browser: &mut Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    delta: (i32, i32),
    damage: &mut Region,
) -> bool {
    let Some(bounds) = scrollbar_bounds(scale, theme, viewport, toolbar) else {
        return false;
    };
    let view = view_layout_for(browser, scale, theme, viewport, toolbar);
    let model = view.scroll_model(browser.scroll_offset());
    let shown = view.view(model.offset()).viewport();
    browser
        .scroll_mut()
        .wheel(model, delta, scale, (bounds, shown), damage)
}

/// Adjust the scroll offset so the current selection is visible, moving the
/// least (a no-op when it already is). A caller runs this after a
/// selection-changing key or a directory change, before it repaints.
pub fn reveal_selection<S: DirectorySource>(
    browser: &mut Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) {
    let view = view_layout_for(browser, scale, theme, viewport, toolbar);
    let revealed = view.reveal(browser.scroll_offset(), browser.selected_index());
    browser.set_scroll_offset(revealed);
}

/// The index of the entry at window-local pixel `point` for the browser's
/// current view and scroll offset, or `None` for the toolbar band, an empty
/// gap, the scrollbar gutter, and any coordinate outside the item area.
///
/// This mirrors [`render_into`]'s own layout through the shared [`ViewLayout`], so a
/// pointer-driven view resolves a click to exactly the item the user saw —
/// never a re-derived guess. `theme` supplies the same scrollbar gutter width
/// the renderer reserved.
#[must_use]
pub fn entry_index_at<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    point: Point,
) -> Option<usize> {
    view_layout_for(browser, scale, theme, viewport, toolbar)
        .index_at(browser.scroll_offset(), point)
}

/// The window-local pixel rectangle of entry `index` that shows, or `None`
/// when it is scrolled out of view (or the view seats nothing there) — the
/// whole item, or the part of it the viewport's edge leaves.
///
/// This is [`render_into`]'s own layout for that entry, through the shared
/// [`ViewLayout`], so a caller reporting damage for a mark that moved between
/// two entries names exactly the pixels the renderer painted. A caller
/// reveals the selection first (via [`reveal_selection`]) if it needs the
/// whole entry on screen.
#[must_use]
pub fn entry_rect<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    index: usize,
) -> Option<Rect> {
    let view = view_layout_for(browser, scale, theme, viewport, toolbar);
    view.item_rect(browser.scroll_offset(), index)
}

/// The window-local pixel rectangle of the browser's currently selected item's
/// **name** that shows, or `None` when nothing is selected or the selection is
/// scrolled out of view.
///
/// The in-place rename editor sits over the name, not the item — the whole
/// item rectangle is the row (icon, name, size and date columns) or the whole
/// tile (picture above the label), and a field laid over either covers what the
/// user is not editing — so this is what a key typed into it repaints. The
/// editor itself is drawn by [`draw_rename_field`]; [`entry_rect`] stays the
/// *item's* rectangle.
#[must_use]
pub fn selection_name_rect<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) -> Option<Rect> {
    entry_name_rect(
        browser,
        scale,
        theme,
        viewport,
        toolbar,
        browser.selected_index()?,
    )
}

/// The window-local pixel rectangle of entry `index`'s **name** that shows, or
/// `None` when it is scrolled out of view (or the view seats no name there).
///
/// Read from the drawn controls themselves — the list row's own name-cell text
/// span, the grid tile's own label band — so an overlay cannot land where the
/// name is not. The band is grown to a field's own height where it is shorter
/// (a tile's label band is one line of glyphs, and a field wants its plate)
/// and clamped back into the item's rectangle, so the editor never spills onto
/// a neighbour. An item the viewport's edge cuts answers the part of its name
/// that shows.
#[must_use]
pub fn entry_name_rect<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    index: usize,
) -> Option<Rect> {
    let (field, view) = name_field(browser, scale, theme, viewport, toolbar, index)?;
    view.to_window(field)
}

/// Draw the in-place rename `field` over the selected item's name, laid out
/// where the name is and cut by the viewport's edge with the item it names, so
/// a scroll carries the editor with its item rather than squeezing it into
/// what is left in view.
#[allow(clippy::too_many_arguments)] // The field, the browser, and the listing's geometry.
pub fn draw_rename_field<S: DirectorySource>(
    surface: &mut Surface,
    field: &TextField,
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) {
    let Some(selected) = browser.selected_index() else {
        return;
    };
    if let Some((bounds, view)) = name_field(browser, scale, theme, viewport, toolbar, selected) {
        view.paint(surface, |surface| {
            field.render(surface, bounds, scale, theme);
        });
    }
}

/// Where entry `index`'s name field is laid out, unscrolled, and the view it
/// shows through.
fn name_field<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
    index: usize,
) -> Option<(Rect, ScrollView)> {
    let view = view_layout_for(browser, scale, theme, viewport, toolbar);
    let item = view.layout_rect(index)?;
    let name = match view {
        // The name is the first cell, and the row control reports the span
        // its glyphs occupy inside that column.
        ViewLayout::List(_) => {
            let entry = browser.entries().get(index)?;
            let kind = icon_for_entry(entry, browser.components());
            entry_row(entry, false, kind).cell_text_rect(item, scale, theme, &COLUMNS, 0)?
        }
        ViewLayout::Grid(_) => IconTile::label_rect(item, scale, theme)?,
    };
    let height = name.height.max(TextField::height(scale, theme));
    let field = Rect::new(name.left(), name.top(), name.width, height).intersection(&item);
    (!field.is_empty()).then(|| (field, view.view(browser.scroll_offset())))
}

/// The window-local pixel rectangle the item area occupies — every entry the
/// view draws, and nothing else.
///
/// A scroll moves every row at once, and a listing change replaces them all,
/// so this is what such a round repaints. It is the renderer's own content
/// viewport, so the reported rectangle and the painted one are the same fact.
#[must_use]
pub fn item_area(scale: Scale, theme: &Theme, viewport: Rect) -> Rect {
    content_viewport(viewport, scale, theme)
}

/// The half-open range of entry indices `browser` currently draws at
/// `viewport` — the one definition of "what is on screen", whichever view is
/// active.
///
/// [`render_into`] iterates exactly this range, so a caller that resolves per-entry
/// state through it (the file manager's folder-occupancy probe,
/// [`Browser::resolve_occupancy`]) pays for precisely the entries the next
/// frame paints, and the two can never disagree about which those are.
///
/// [`Browser::resolve_occupancy`]: crate::Browser::resolve_occupancy
#[must_use]
pub fn visible_range<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) -> core::ops::Range<usize> {
    view_layout_for(browser, scale, theme, viewport, toolbar).visible_range(browser.scroll_offset())
}

/// The resolved view layout for `browser` at `viewport` — the one dispatch the
/// scroll helpers and the pointer hit-test share, laid out within the same
/// content viewport (window minus the scrollbar gutter) the renderer uses.
fn view_layout_for<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    toolbar: ToolbarBand,
) -> ViewLayout {
    let content = content_viewport(viewport, scale, theme);
    match browser.view_mode() {
        ViewMode::List => ViewLayout::List(list_view(browser, scale, theme, content, toolbar)),
        ViewMode::Grid => ViewLayout::Grid(grid_view(browser, scale, theme, content, toolbar)),
    }
}

/// The most facts the General section states, which a Properties window's
/// opening height reserves room for.
const PROPERTY_ROW_COUNT: usize = Field::ALL.len();

/// One fact a Properties window's General section states.
///
/// A closed vocabulary rather than a label/value list, so the display order,
/// each fact's label, each fact's value, and which facts a given node shows
/// at all are one definition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Field {
    /// The human kind label.
    Kind,
    /// The spelling a symbolic link stores.
    Alias,
    /// Apparent size, with the on-disk allocation beside it.
    Size,
    /// The four timestamps.
    Created,
    /// See [`Field::Created`].
    Modified,
    /// See [`Field::Created`].
    Accessed,
    /// See [`Field::Created`].
    Changed,
}

impl Field {
    /// Every fact, in display order.
    const ALL: [Self; 7] = [
        Self::Kind,
        Self::Alias,
        Self::Size,
        Self::Created,
        Self::Modified,
        Self::Accessed,
        Self::Changed,
    ];

    /// The fact's label.
    const fn label(self) -> &'static str {
        match self {
            Self::Kind => "Kind",
            Self::Alias => "Alias to",
            Self::Size => "Size",
            Self::Created => "Created",
            Self::Modified => "Modified",
            Self::Accessed => "Accessed",
            Self::Changed => "Changed",
        }
    }

    /// Whether this fact has anything to say about `props`.
    ///
    /// Only the alias row is conditional: a node that stores no target has
    /// nothing to put there, and an empty row would read as a broken link.
    fn shown(self, props: &Properties) -> bool {
        !matches!(self, Self::Alias) || props.target().is_some()
    }

    /// The fact's value, straight from the model.
    fn value(self, props: &Properties) -> String {
        match self {
            Self::Kind => String::from(props.kind_label()),
            Self::Alias => String::from(props.target().unwrap_or_default()),
            Self::Size => alloc::format!(
                "{} ({} on disk)",
                props.size_display(),
                props.allocated_display()
            ),
            Self::Created => props.created_display(),
            Self::Modified => props.modified_display(),
            Self::Accessed => props.accessed_display(),
            Self::Changed => props.changed_display(),
        }
    }

    /// The facts `props` states, in display order.
    fn stated(props: &Properties) -> impl Iterator<Item = Self> + '_ {
        Self::ALL.into_iter().filter(|field| field.shown(props))
    }
}

/// The Permissions section's reading of `props`'s mode: the symbolic
/// spelling, with the octal one beside it.
fn mode_reading(props: &Properties) -> String {
    alloc::format!("{} ({})", props.permissions(), props.mode_octal())
}

/// The facts the General section states for `props`, as its rows draw them.
#[cfg(test)]
pub(crate) fn general_facts(props: &Properties) -> Vec<(&'static str, String)> {
    Field::stated(props)
        .map(|field| (field.label(), field.value(props)))
        .collect()
}

/// The mode reading the Permissions section draws for `props`.
#[cfg(test)]
pub(crate) fn mode_reading_for_test(props: &Properties) -> String {
    mode_reading(props)
}

/// Draw `fields` as a [`FactList`] inset within `content`.
fn draw_fact_rows(
    surface: &mut Surface,
    fields: impl Iterator<Item = Field>,
    props: &Properties,
    content: Rect,
    scale: Scale,
    theme: &Theme,
) {
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let bounds = Rect::new(
        content.left().saturating_add(to_i32(pad)),
        content.top().saturating_add(to_i32(pad)),
        content.width.saturating_sub(pad.saturating_mul(2)),
        content.height.saturating_sub(pad.min(content.height)),
    );
    FactList::new(
        fields
            .map(|field| Fact::new(field.label(), field.value(props)))
            .collect(),
    )
    .with_separators(true)
    .render(surface, bounds, scale, theme);
}

/// The nine settable owner/group/other × read/write/execute permission bits, in
/// the left-to-right order the permission control lays them out (the owner
/// triad, then group, then other) — the same order as the symbolic `rwxrwxrwx`
/// spelling they sit over, so the drawn toggles and their hit-test share one
/// definition of which cell carries which bit.
///
/// Only these nine `rwx` bits are offered as toggles — the familiar, legible
/// permission set. The setuid/setgid/sticky bits stay visible in the
/// Properties fields' octal and symbolic spelling and are edited through the
/// `chmod` command: a deliberate scope boundary for a best-in-class,
/// bloat-free surface, not an omission. Toggling a cell flips only its own
/// `rwx` bit and preserves whatever the higher bits currently are.
pub const PERMISSION_BITS: [u32; 9] = [
    0o400, 0o200, 0o100, // owner: read, write, execute
    0o040, 0o020, 0o010, // group: read, write, execute
    0o004, 0o002, 0o001, // other: read, write, execute
];

/// Which of the nine [`PERMISSION_BITS`] `mode` currently sets, in the same
/// left-to-right order — the one definition the drawn toggles' states and their
/// tests read, so a toggle can never disagree with the mode it depicts.
#[must_use]
pub const fn permission_cells(mode: u32) -> [bool; 9] {
    let mut cells = [false; 9];
    let mut i = 0;
    while i < PERMISSION_BITS.len() {
        cells[i] = mode & PERMISSION_BITS[i] != 0;
        i += 1;
    }
    cells
}

/// The permission classes, one access row each, in the triad order of
/// [`PERMISSION_BITS`].
const PERMISSION_ROW_LABELS: [&str; 3] = ["Owner", "Group", "Other"];

/// A class's three flags, in the order [`PERMISSION_BITS`] lays a triad out.
const PERMISSION_FLAG_LABELS: [&str; 3] = ["Read", "Write", "Execute"];

/// What the attributes section says in place of a list it has no rows for.
const ATTR_UNSUPPORTED: &str = "not stored by this volume";

/// What the attributes section says for a node that carries none.
const ATTR_NONE: &str = "none";

/// What the attributes section says when the listing itself was refused.
const ATTR_REFUSED: &str = "could not be read";

/// The pixel height of a control plate at `scale`: the theme's own control
/// height, never a text row pitch.
///
/// A plate laid out on the row pitch a line of *type* occupies is too short
/// for the control it draws, and the label ends up crowding the frame it is
/// meant to stay clear of. Every action band in this module reserves its
/// buttons this height, so a button in a dialog is the same object as a button
/// on a toolbar.
fn control_height(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_height).max(1)
}

/// The logical side of the artwork an identity band draws, at the reference
/// density.
///
/// Large enough that a file-class picture reads as a picture rather than as a
/// list glyph, which is the point of naming a window's subject once at the top
/// instead of as one row among its fields.
const IDENTITY_ART: u32 = 48;

/// What a window's identity band says about the node it is about.
///
/// Both of the file manager's own windows open with one of these, so the
/// subject of a Properties window and the subject of an "Open With…" chooser
/// are named, pictured and laid out by one definition rather than two.
#[derive(Copy, Clone)]
pub struct Identity<'a> {
    /// The leaf name, drawn large.
    pub name: &'a str,
    /// The muted line beneath it — what the thing is, and how big.
    pub detail: &'a str,
    /// The request the band's artwork resolves through.
    pub art: IconRequest<'a>,
}

/// The height an identity band occupies at `scale`: its artwork, or its two
/// lines of text, whichever is taller, plus the padding around them.
#[must_use]
pub fn identity_height(scale: Scale, theme: &Theme) -> u32 {
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let title = BitmapFont::for_role(theme.fonts(), TextRole::ItemTitle, scale).glyph_height();
    let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale).glyph_height();
    let text = title
        .saturating_add(body)
        .saturating_add(scale.scale_length(ROW_PADDING));
    scale
        .scale_length(IDENTITY_ART)
        .max(text)
        .saturating_add(pad.saturating_mul(2))
        .max(1)
}

/// Draw the identity band for `identity` across `bounds`: the node's own
/// artwork, its name, and the muted detail line, closed by a hairline that
/// separates it from whatever the window puts below.
///
/// The artwork resolves through the caller's cache like every other picture in
/// this module, so the band costs one cached lookup and falls back to the
/// built-in glyph when nothing of the thing's own will serve.
fn draw_identity(
    surface: &mut Surface,
    identity: Identity<'_>,
    scale: Scale,
    theme: &Theme,
    bounds: Rect,
    artwork: &mut dyn IconArtwork,
) {
    let palette = theme.palette();
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let side = scale
        .scale_length(IDENTITY_ART)
        .min(bounds.height.saturating_sub(pad.min(bounds.height)))
        .max(1);
    let art_x = bounds.left().saturating_add(to_i32(pad));
    let art_y = bounds
        .top()
        .saturating_add(to_i32(bounds.height.saturating_sub(side) / 2));
    let picture = artwork.artwork(identity.art, side);
    paint_icon_slot(
        surface,
        (
            u32::try_from(art_x).unwrap_or(0),
            u32::try_from(art_y).unwrap_or(0),
            side,
        ),
        identity.art.icon_kind(),
        palette.on_surface.into(),
        picture,
        FULL_COLOUR,
    );

    let title = BitmapFont::for_role(theme.fonts(), TextRole::ItemTitle, scale);
    let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let text_x = art_x
        .saturating_add(to_i32(side))
        .saturating_add(to_i32(scale.scale_length(theme.metrics().control_gap)));
    let budget = u32::try_from(
        bounds
            .left()
            .saturating_add(to_i32(bounds.width))
            .saturating_sub(text_x)
            .saturating_sub(to_i32(pad)),
    )
    .unwrap_or(0);
    let gap = scale.scale_length(ROW_PADDING);
    let block = title
        .glyph_height()
        .saturating_add(gap)
        .saturating_add(body.glyph_height());
    let name_y = bounds
        .top()
        .saturating_add(to_i32(bounds.height.saturating_sub(block) / 2));
    let (name, elided) = title.elide_to_width(identity.name, budget);
    let pen = title.draw_text(surface, text_x, name_y, name, palette.on_surface.into());
    if elided {
        title.draw_text(surface, pen, name_y, ELLIPSIS, palette.on_surface.into());
    }
    body.draw_text(
        surface,
        text_x,
        name_y.saturating_add(to_i32(title.glyph_height().saturating_add(gap))),
        body.truncate_to_width(identity.detail, budget),
        palette.on_surface_muted.into(),
    );

    let rule = scale.scale_length(theme.metrics().border_thickness).max(1);
    surface.fill_rect(
        u32::try_from(bounds.left()).unwrap_or(0),
        u32::try_from(
            bounds
                .top()
                .saturating_add(to_i32(bounds.height.saturating_sub(rule))),
        )
        .unwrap_or(0),
        bounds.width,
        rule,
        palette.border.into(),
    );
}

/// Which of the two owning ids the inline ownership control edits.
///
/// The owning user (`uid`) and group (`gid`) are the two independently
/// editable values on a Properties surface's ownership rows; a click resolves
/// to exactly one of them and the caller commits that one field.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum OwnerField {
    /// The owning user id (`chown`).
    Uid,
    /// The owning group id (`chgrp`).
    Gid,
}

impl OwnerField {
    /// Both fields, in the order their rows are drawn.
    const BOTH: [Self; 2] = [Self::Uid, Self::Gid];

    /// The row's label.
    const fn label(self) -> &'static str {
        match self {
            Self::Uid => "Owner",
            Self::Gid => "Group",
        }
    }

    /// The id this field currently names on `props`.
    const fn id(self, props: &Properties) -> u32 {
        match self {
            Self::Uid => props.uid(),
            Self::Gid => props.gid(),
        }
    }

    /// The ownership row this field's cell is drawn on.
    fn row(self) -> Option<usize> {
        Self::BOTH.iter().position(|field| *field == self)
    }
}

/// Which section of a Properties window is on show.
///
/// A closed vocabulary: the window's tab strip, the body it draws, and the
/// hit-test that routes a press into that body all read this one enumeration,
/// so a press can never be resolved against a section the user is not looking
/// at.
#[derive(Copy, Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum PropertiesTab {
    /// The node's metadata: what it is, how big, and when it changed.
    #[default]
    General,
    /// The mode bits and the owning ids.
    Permissions,
    /// The extended-attribute store.
    Attributes,
}

impl PropertiesTab {
    /// Every section, in the order the tab strip draws them.
    pub const ALL: [Self; 3] = [Self::General, Self::Permissions, Self::Attributes];

    /// The section's tab label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Permissions => "Permissions",
            Self::Attributes => "Attributes",
        }
    }

    /// The section at `index` in [`Self::ALL`], or `None` past the end.
    #[must_use]
    pub fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// This section's own index in the tab strip.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// The section `steps` away from this one, clamped at either end so a
    /// keyboard walk never wraps past the strip.
    #[must_use]
    pub fn stepped(self, steps: i32) -> Self {
        let last = Self::ALL.len().saturating_sub(1);
        let target = i64::from(i32::try_from(self.index()).unwrap_or(0)) + i64::from(steps);
        let clamped = target.clamp(0, i64::try_from(last).unwrap_or(0));
        Self::at(usize::try_from(clamped).unwrap_or(0)).unwrap_or(Self::General)
    }
}

/// What a press on a Properties window resolves to.
///
/// One hit-test rather than one per control, so the precedence between them is
/// stated once: the tab strip is resolved before the body it selects, the
/// capability-free permission toggles before the privileged ownership control,
/// and a press on nothing resolves to nothing — never to whichever control
/// happens to be nearest (fail closed).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PropertiesTarget {
    /// A tab in the strip, by the section it selects.
    Tab(PropertiesTab),
    /// One of the nine permission toggles, by the `rwx` bit it flips.
    Permission(u32),
    /// An owning id's value cell, which only a holder of `CAP_FS_CHOWN` may
    /// edit.
    Owner(OwnerField),
    /// An attribute row, by its index in the visible set.
    Attribute(usize),
    /// The `key = value` editor's field.
    Editor,
    /// One of the attribute actions beneath the list.
    Action(AttrAction),
}

/// What a press on the attributes action band asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum AttrAction {
    /// Apply the editor's `key = value` line to the node.
    Set,
    /// Remove the attribute the cursor row names.
    Remove,
}

/// The two attribute actions, in the order they are drawn.
const ATTR_ACTIONS: [(AttrAction, &str); 2] =
    [(AttrAction::Remove, "Remove"), (AttrAction::Set, "Set")];

/// Where a Properties window's three bands sit within its client area: the
/// identity band naming the node, the tab strip selecting a section, and the
/// body that section draws into.
///
/// Resolved once from the client alone — not from the node — so switching
/// section or adopting a fresh read never moves the frame under the pointer.
/// The body is [`None`] when the client leaves none, so a window dragged tiny
/// draws and resolves nothing there rather than placing controls off its own
/// surface.
struct PropertiesLayout {
    /// The identity band across the top.
    identity: Rect,
    /// The tab strip below it.
    tabs: Rect,
    /// What the selected section draws into.
    body: Option<Rect>,
}

impl PropertiesLayout {
    /// Resolve the window's bands from its `content` (its whole client area).
    fn resolve(content: Rect, scale: Scale, theme: &Theme) -> Self {
        let head = identity_height(scale, theme).min(content.height);
        let strip = tab_strip_height(scale, theme).min(content.height.saturating_sub(head));
        let identity = Rect::new(content.left(), content.top(), content.width, head);
        let tabs = Rect::new(
            content.left(),
            content.top().saturating_add(to_i32(head)),
            content.width,
            strip,
        );
        let used = head.saturating_add(strip);
        let height = content.height.saturating_sub(used);
        let body = (height > 0).then(|| {
            Rect::new(
                content.left(),
                content.top().saturating_add(to_i32(used)),
                content.width,
                height,
            )
        });
        Self {
            identity,
            tabs,
            body,
        }
    }
}

/// The Permissions section's groups, in the order they stack down the body.
const ACCESS: usize = 0;
/// See [`ACCESS`].
const OWNERSHIP: usize = 1;

/// The access group's first class row: the mode reading sits above the three.
const FIRST_CLASS_ROW: usize = 1;

/// The access group's caption.
const ACCESS_CAPTION: &str = "ACCESS";

/// The ownership group's caption.
const OWNERSHIP_CAPTION: &str = "OWNERSHIP";

/// The label of the symbolic and octal mode reading.
const MODE_LABEL: &str = "Mode";

/// What the ownership group says to a session that may not reassign an owner,
/// so the Authority Mark on its rows is explained on the surface.
const OWNERSHIP_REFUSED: &str = "Changing ownership here needs the capability to reassign it.";

/// The Permissions section, composed of the shared form family: an access
/// group — the mode reading over one row of `rwx` flags per class — and an
/// ownership group holding the two owning ids.
///
/// Built from what the window shows and placed down the body through the one
/// shared plate column; the paint, the hit-test and the keyboard all read that
/// one placement, so a press or a key only ever reaches a control that was
/// drawn.
struct PermsSection {
    groups: [FieldGroup; 2],
}

/// Whether the session may reassign an owner, and the id editor it has open.
type OwnerGate<'a> = (bool, Option<(OwnerField, &'a TextField)>);

impl PermsSection {
    /// The section showing `props` under `gate`, the keyboard resting where
    /// `cursor` says.
    fn new(props: &Properties, gate: OwnerGate<'_>, cursor: PermsCursor) -> Self {
        let mut section = Self::compose(
            mode_reading(props),
            permission_cells(props.mode()),
            OwnerField::BOTH.map(|field| field.id(props)),
            gate,
            cursor.flag,
        );
        // An open id editor holds the keyboard, so its row is the focused one.
        let focus = gate
            .1
            .and_then(|(field, _)| field.row().map(|row| (OWNERSHIP, row)))
            .or(cursor.row);
        for (index, group) in section.groups.iter_mut().enumerate() {
            group.adopt_focus(focus.and_then(|(at, row)| (at == index).then_some(row)));
        }
        section
    }

    /// The section's shape, measured before any node has been read: its
    /// height depends on its rows, never on the values they show, and a
    /// session refused ownership is the taller of the two.
    fn shape() -> Self {
        Self::compose(String::new(), [false; 9], [0; 2], (false, None), 0)
    }

    /// The groups for a mode `reading`, its `cells`, the owning `ids`, and
    /// the ownership gate with any id editor open, the keyboard resting on
    /// `flag` of whichever access row it reaches.
    fn compose(
        reading: String,
        cells: [bool; 9],
        ids: [u32; 2],
        (can_chown, editor): OwnerGate<'_>,
        flag: usize,
    ) -> Self {
        let mut access = Vec::with_capacity(PERMISSION_ROW_LABELS.len() + FIRST_CLASS_ROW);
        access.push(FieldRow::new(MODE_LABEL, FieldControl::Reading(reading)));
        for (triad, label) in PERMISSION_ROW_LABELS.iter().enumerate() {
            let flags = PERMISSION_FLAG_LABELS
                .iter()
                .enumerate()
                .map(|(bit, name)| {
                    let on = cells.get(triad * PERMISSION_FLAG_LABELS.len() + bit);
                    let selection = if on.copied().unwrap_or(false) {
                        SelectionState::Selected
                    } else {
                        SelectionState::Unselected
                    };
                    Checkbox::new(*name, selection)
                })
                .collect();
            access.push(FieldRow::new(
                *label,
                FieldControl::Flags(FlagSet::new(flags).with_focus(flag)),
            ));
        }

        // Reassigning an owner is privileged, unlike a mode change: without
        // `CAP_FS_CHOWN` the same cells are shown refused.
        let refused = ControlState::idle().with_authority(AuthorityState::NeedsCapability);
        let owners = OwnerField::BOTH
            .into_iter()
            .zip(ids)
            .map(|(field, id)| {
                let control = match editor {
                    Some((open, editor)) if open == field => FieldControl::Text(editor.clone()),
                    // A plate, not an idle text field, which would draw like
                    // the live one and hide whether keys are landing.
                    _ => FieldControl::Button(
                        Button::new(
                            ButtonContent::Label(alloc::format!("{id}")),
                            ControlRole::Neutral,
                        )
                        .aligned(ContentAlign::Leading),
                    ),
                };
                let row = FieldRow::new(field.label(), control);
                if can_chown {
                    row
                } else {
                    row.with_state(refused)
                }
            })
            .collect();
        let ownership = FieldGroup::new(OWNERSHIP_CAPTION, owners);
        let ownership = if can_chown {
            ownership
        } else {
            ownership.with_footnote(OWNERSHIP_REFUSED)
        };
        Self {
            groups: [FieldGroup::new(ACCESS_CAPTION, access), ownership],
        }
    }

    /// The one column the whole section lines up in: the width a class's
    /// flags need. The mode reading, the ids and an open id editor all begin
    /// where the flags do, and neither a node's reading nor an editor — which
    /// takes whatever column it is given — can move it.
    fn column(&self, scale: Scale, theme: &Theme) -> u32 {
        self.groups[ACCESS]
            .rows()
            .get(FIRST_CLASS_ROW)
            .and_then(|row| row.slot_width(scale, theme))
            .unwrap_or(0)
    }

    /// The section laid out at its natural height down `body`, scrolled
    /// `offset` pixels: how it scrolls, and where each group sits in its
    /// unscrolled layout.
    fn laid_out(
        &self,
        body: Rect,
        offset: u64,
        scale: Scale,
        theme: &Theme,
    ) -> (Scrolled, Vec<(usize, FieldLayout)>) {
        let (scrolled, frame) = Scrolled::measured(
            body,
            gutter_width(scale, theme, body.width),
            (offset, control_height(scale, theme)),
            |width| self.measured_height(width, scale, theme),
        );
        (scrolled, self.placed(frame, scale, theme))
    }

    /// Where each group sits down `body`, in stacking order, with the
    /// section's one column.
    fn placed(&self, body: Rect, scale: Scale, theme: &Theme) -> Vec<(usize, FieldLayout)> {
        let across = stack::plate_width(body.width, scale, theme);
        let column = self.column(scale, theme);
        stack::place(body, self.groups.len(), scale, theme, |index| {
            self.groups.get(index).map_or(0, |group| {
                group.measured_height(across, column, scale, theme)
            })
        })
        .into_iter()
        .map(|(index, rect)| (index, FieldLayout::new(rect, column)))
        .collect()
    }

    /// The height the section needs stacked in a body `width` pixels wide.
    fn measured_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let across = stack::plate_width(width, scale, theme);
        let column = self.column(scale, theme);
        stack::height(
            self.groups
                .iter()
                .map(|group| group.measured_height(across, column, scale, theme)),
            scale,
            theme,
        )
    }

    /// The narrowest body that seats a class's flags whole.
    fn natural_width(&self, scale: Scale, theme: &Theme) -> u32 {
        stack::column_width(
            self.groups[ACCESS].natural_width(scale, theme),
            scale,
            theme,
        )
    }

    /// Paint both groups down `body` scrolled `offset` pixels, with `bar`
    /// beside them when they outgrow it.
    fn render(
        &self,
        surface: &mut Surface,
        (body, offset): (Rect, u64),
        bar: &ScrollBar,
        scale: Scale,
        theme: &Theme,
    ) {
        let (scrolled, placed) = self.laid_out(body, offset, scale, theme);
        scrolled.view.paint(surface, |surface| {
            for (index, layout) in &placed {
                if let Some(group) = self.groups.get(*index) {
                    group.render(surface, *layout, scale, theme);
                }
            }
        });
        scrolled.draw_bar(surface, bar, scale, theme);
    }

    /// Where `(group, row)` is laid out among `placed`, or [`None`] for a row
    /// the section does not have.
    fn row_rect(
        &self,
        placed: &[(usize, FieldLayout)],
        (group, row): (usize, usize),
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let layout = placed.iter().find(|(index, _)| *index == group)?.1;
        self.groups.get(group)?.row_rect(row, layout, scale, theme)
    }

    /// What a press at `point`, in the layout `placed` lays the groups out
    /// in, lands on.
    ///
    /// The access group is resolved first, so the capability-free toggles
    /// come before the privileged ownership control. A refused ownership cell
    /// resolves to nothing rather than opening an editor whose commit the
    /// kernel could only refuse, and so does the editor already open: pressing
    /// the field being typed into must not reopen it and discard the typing.
    fn target_at(
        &self,
        placed: &[(usize, FieldLayout)],
        scale: Scale,
        theme: &Theme,
        point: Point,
    ) -> Option<PropertiesTarget> {
        placed.iter().copied().find_map(|(index, layout)| {
            let group = self.groups.get(index)?;
            let row = group.row_at(layout, scale, theme, point)?;
            let bounds = group.row_rect(row, layout, scale, theme)?;
            let field_row = group.rows().get(row)?;
            let control =
                field_row.control_rect(FieldLayout::new(bounds, layout.column), scale, theme)?;
            match field_row.control() {
                FieldControl::Flags(flags) => {
                    permission_target(row, flags.flag_at(control, scale, theme, point)?)
                }
                FieldControl::Button(_)
                    if index == OWNERSHIP
                        && control.contains(point)
                        && field_row.state().is_actionable() =>
                {
                    OwnerField::BOTH
                        .get(row)
                        .copied()
                        .map(PropertiesTarget::Owner)
                }
                _ => None,
            }
        })
    }

    /// The target a row's reported action names, exactly as a press on the
    /// same control would.
    fn target_of(&self, group: usize, action: FieldGroupAction) -> Option<PropertiesTarget> {
        match (group, action.action) {
            (ACCESS, FieldAction::SetFlag { index, .. }) => permission_target(action.row, index),
            (OWNERSHIP, FieldAction::Activated) => self.groups[OWNERSHIP]
                .rows()
                .get(action.row)
                .filter(|row| row.state().is_actionable())
                .and_then(|_| OwnerField::BOTH.get(action.row).copied())
                .map(PropertiesTarget::Owner),
            _ => None,
        }
    }

    /// The flag the focused row of `group` rests on, when it is an access row.
    fn focused_flag(&self, group: usize) -> Option<usize> {
        let group = self.groups.get(group)?;
        match group.rows().get(group.focus()?)?.control() {
            FieldControl::Flags(flags) => Some(flags.focus()),
            _ => None,
        }
    }

    /// Feed `key` to the section laid out as `placed`, the keyboard resting
    /// where `cursor` says, reporting the rows it repainted in that layout.
    fn key(
        &mut self,
        placed: &[(usize, FieldLayout)],
        cursor: PermsCursor,
        stroke: Keystroke,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> PermsKeyed {
        let key = stroke.key;
        let unchanged = PermsKeyed {
            cursor,
            target: None,
        };
        let exists = |section: &Self, at| section.row_rect(placed, at, scale, theme).is_some();
        let moved = |section: &Self, row: Option<(usize, usize)>, flag, damage: &mut Region| {
            for at in [cursor.row, row].into_iter().flatten() {
                if let Some(rect) = section.row_rect(placed, at, scale, theme) {
                    damage.add(rect);
                }
            }
            PermsKeyed {
                cursor: PermsCursor { row, flag },
                target: None,
            }
        };

        let Some((group, row)) = cursor.row else {
            let first = (ACCESS, 0);
            let enters =
                matches!(key, Key::Named(NamedKey::Down | NamedKey::Tab)) && exists(self, first);
            return if enters {
                moved(self, Some(first), cursor.flag, damage)
            } else {
                unchanged
            };
        };
        if matches!(key, Key::Named(NamedKey::Tab | NamedKey::Escape)) {
            return moved(self, None, cursor.flag, damage);
        }
        let Some(layout) = placed
            .iter()
            .find(|(index, _)| *index == group)
            .map(|(_, layout)| *layout)
        else {
            return unchanged;
        };
        let Some(focused) = self.groups.get_mut(group) else {
            return unchanged;
        };
        let acted = focused.on_key(stroke, layout, scale, theme, damage);
        let now = focused.focus();
        let flag = self.focused_flag(group).unwrap_or(cursor.flag);
        let Some(action) = acted else {
            // The group clamps at its own ends; carrying the cursor into the
            // next group is the section's.
            let carried = match key {
                Key::Named(NamedKey::Down) if now == Some(row) => {
                    Some((group.saturating_add(1), 0))
                }
                Key::Named(NamedKey::Up) if now == Some(row) && group > 0 => self
                    .groups
                    .get(group - 1)
                    .map(|above| (group - 1, above.len().saturating_sub(1))),
                _ => None,
            };
            if let Some(next) = carried.filter(|next| exists(self, *next)) {
                return moved(self, Some(next), flag, damage);
            }
            return PermsKeyed {
                cursor: PermsCursor {
                    row: now.map(|row| (group, row)),
                    flag,
                },
                target: None,
            };
        };
        PermsKeyed {
            cursor: PermsCursor {
                row: now.map(|row| (group, row)),
                flag,
            },
            target: self.target_of(group, action),
        }
    }
}

/// The toggle for flag `flag` of the access row `row`.
fn permission_target(row: usize, flag: usize) -> Option<PropertiesTarget> {
    let triad = row.checked_sub(FIRST_CLASS_ROW)?;
    if flag >= PERMISSION_FLAG_LABELS.len() {
        return None;
    }
    PERMISSION_BITS
        .get(triad * PERMISSION_FLAG_LABELS.len() + flag)
        .copied()
        .map(PropertiesTarget::Permission)
}

/// The attributes section's geometry within its body: the list band, the
/// `key = value` editor at the foot, and the action buttons beside it.
///
/// The editor stays put at the foot; the attribute rows scroll in the band
/// above it.
struct AttrsLayout {
    /// The band the attribute rows scroll in, gutter included.
    band: Option<Rect>,
    /// The editor's text field.
    editor: Rect,
    /// The action buttons, in [`ATTR_ACTIONS`] order.
    actions: [Rect; ATTR_ACTIONS.len()],
}

impl AttrsLayout {
    /// Resolve the section from `body`, or `None` when it leaves no room for
    /// the editor the section is edited through.
    fn resolve(body: Rect, scale: Scale, theme: &Theme, font: BitmapFont) -> Option<Self> {
        let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
        let plate = control_height(scale, theme);
        let left = body.left().saturating_add(to_i32(pad));
        let width = body.width.saturating_sub(pad.saturating_mul(2));
        if width == 0 || body.height <= plate {
            return None;
        }
        let bottom = body.top().saturating_add(to_i32(body.height));
        let editor_top = bottom
            .saturating_sub(to_i32(plate))
            .saturating_sub(to_i32(pad));
        let gap = scale.scale_length(theme.metrics().control_gap).max(1);
        let mut right = left.saturating_add(to_i32(width));
        let mut actions = [Rect::EMPTY; ATTR_ACTIONS.len()];
        for slot in (0..ATTR_ACTIONS.len()).rev() {
            let button = action_button_width(ATTR_ACTIONS[slot].1, scale, theme, font).min(width);
            let x = right.saturating_sub(to_i32(button));
            actions[slot] = Rect::new(x, editor_top, button, plate);
            right = x.saturating_sub(to_i32(gap));
        }
        let editor_w = u32::try_from(right.saturating_sub(left)).unwrap_or(0);
        if editor_w == 0 {
            return None;
        }
        let editor = Rect::new(left, editor_top, editor_w, plate);
        let rows_top = body.top().saturating_add(to_i32(pad));
        let list_h = u32::try_from(
            editor_top
                .saturating_sub(to_i32(pad))
                .saturating_sub(rows_top),
        )
        .unwrap_or(0);
        Some(Self {
            band: (list_h > 0).then(|| Rect::new(left, rows_top, width, list_h)),
            editor,
            actions,
        })
    }

    /// `count` attribute rows laid out unscrolled down the band, scrolled
    /// `offset` pixels, beside the gutter they need when they outgrow it — or
    /// `None` when the section left no band.
    fn rows(
        &self,
        count: usize,
        offset: u64,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(ListView, Scrolled)> {
        let band = self.band?;
        let line = row_height(scale, theme).max(1);
        let content = u32::try_from(count)
            .unwrap_or(u32::MAX)
            .saturating_mul(line);
        let (scrolled, frame) = Scrolled::measured(
            band,
            gutter_width(scale, theme, band.width),
            (offset, line),
            |_| content,
        );
        let area = Rect::new(frame.left(), frame.top(), frame.width, band.height);
        Some((ListView::new(area, line, 0, count), scrolled))
    }
}

/// A section's scrolling column: the view it shows through, the gutter its
/// bar sits in once its content outgrows the space it has, and the model the
/// bar and the wheel move it through.
#[derive(Copy, Clone, Debug)]
struct Scrolled {
    /// The column's on-screen area, at its current scroll.
    view: ScrollView,
    /// The gutter beside it, while the content outgrows it.
    gutter: Option<Rect>,
    /// The column's scroll, clamped to its content.
    model: ScrollModel,
}

impl Scrolled {
    /// Lay content whose height at a width `extent_at` answers down `area`,
    /// scrolled `offset` pixels and stepping `line` pixels a line: the column,
    /// and the frame its content is laid out in, unscrolled — the area's top,
    /// the width a `gutter`-wide bar leaves, the content's own height.
    ///
    /// A bar narrows the column and a narrower column can wrap taller, so
    /// content that overflows is measured again at the width it is laid out in.
    fn measured(
        area: Rect,
        gutter: u32,
        (offset, line): (u64, u32),
        extent_at: impl Fn(u32) -> u32,
    ) -> (Self, Rect) {
        let full = extent_at(area.width);
        let bar = (full > area.height && gutter > 0 && gutter < area.width).then_some(gutter);
        let width = area.width.saturating_sub(bar.unwrap_or(0));
        let extent = if bar.is_some() {
            extent_at(width)
        } else {
            full
        };
        let model = ScrollModel::in_pixels(
            ScrollRange::new(u64::from(extent), u64::from(area.height), offset),
            u64::from(line),
        );
        let shown = Rect::new(area.left(), area.top(), width, area.height);
        let scrolled = Self {
            view: ScrollView::new(ScrollOrientation::Vertical, shown, model.offset()),
            gutter: bar.map(|gutter| {
                Rect::new(
                    area.left().saturating_add(to_i32(width)),
                    area.top(),
                    gutter,
                    area.height,
                )
            }),
            model,
        };
        (
            scrolled,
            Rect::new(area.left(), area.top(), width, extent.max(area.height)),
        )
    }

    /// Draw `bar` in the gutter, when the column has one.
    fn draw_bar(&self, surface: &mut Surface, bar: &ScrollBar, scale: Scale, theme: &Theme) {
        if let Some(gutter) = self.gutter {
            draw_bar(bar, self.model, surface, gutter, scale, theme);
        }
    }

    /// The scroll that shows `rect` of the column's layout while moving the
    /// least.
    fn revealing(&self, rect: Rect) -> u64 {
        let top = self.view.viewport().top();
        let start = u64::try_from(rect.top().saturating_sub(top)).unwrap_or(0);
        self.model.revealing(start, u64::from(rect.height)).offset()
    }
}

/// The intrinsic width of an action button carrying `label`: the label plus
/// the theme's own text inset either side, floored so a short word still gets
/// a pressable plate.
fn action_button_width(label: &str, scale: Scale, theme: &Theme, font: BitmapFont) -> u32 {
    let inset = scale.scale_length(theme.metrics().control_inset);
    font.text_width(label)
        .saturating_add(inset.saturating_mul(2))
        .max(control_height(scale, theme).saturating_mul(2))
}

/// The height a tab strip occupies.
fn tab_strip_height(scale: Scale, theme: &Theme) -> u32 {
    Tabs::new(
        PropertiesTab::ALL
            .iter()
            .map(|tab| Tab::new(tab.label()))
            .collect(),
    )
    .measured_height(scale, theme)
}

/// The tab strip a Properties window draws, with `selected` current.
fn properties_tabs(selected: PropertiesTab) -> Tabs {
    let mut tabs = Tabs::new(
        PropertiesTab::ALL
            .iter()
            .map(|tab| Tab::new(tab.label()))
            .collect(),
    );
    tabs.adopt_selected(selected.index());
    tabs
}

/// What a Properties window is showing right now.
///
/// The read a window is opened by leaves the loop (a node's metadata is one
/// `fs_stat` and its attributes one call per key), so a window states that it
/// is reading, or why it could not, rather than showing an empty or invented
/// summary until the answer lands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PropertiesFrame<'a> {
    /// The read is in flight.
    Reading,
    /// The read was refused; the reason is stated on the surface.
    Refused(&'a str),
    /// The node's metadata and attributes, as the read found them.
    Ready(&'a Properties),
}

/// Where the keyboard cursor stands in the Permissions section.
///
/// The section holds the keyboard only once the reader takes it there from
/// the section strip, so until then the strip's own Left and Right keep
/// walking sections.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PermsCursor {
    /// The group and row the cursor is on, or `None` while the strip holds
    /// the keyboard.
    pub row: Option<(usize, usize)>,
    /// Which flag of an access row the cursor is on, kept as it moves between
    /// rows so walking down a column stays in it.
    pub flag: usize,
}

impl PermsCursor {
    /// Whether the section holds the keyboard.
    #[must_use]
    pub const fn holds(self) -> bool {
        self.row.is_some()
    }
}

/// Everything a Properties window is currently *showing*, as opposed to what
/// it is showing it *about*: which section is selected, how far that section
/// is scrolled, and where each section's keyboard cursor rests.
///
/// One value threaded through the draw and every hit-test, so the section a
/// press is resolved against is always the section that was painted, at the
/// scroll it was painted at.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PropertiesView {
    /// The selected section.
    pub tab: PropertiesTab,
    /// How far the selected section's scrolling column is scrolled, in
    /// pixels: the General or Permissions body, or the attribute list above
    /// the editor, which stays put.
    pub scroll: u64,
    /// The attribute row the keyboard and the Remove action act on.
    pub cursor: usize,
    /// The Permissions section's keyboard cursor.
    pub perms: PermsCursor,
}

/// The Properties window's default extent in physical pixels at `scale`: wide
/// enough for the label and value columns and for every access flag whole,
/// and tall enough to show the identity band, the tab strip, and the tallest
/// section's whole content.
///
/// A window, so the user may resize it; this is only what it opens at. The
/// Permissions section's share is measured from its own composition, so a
/// wider type ladder is seated rather than cut, and no section opens already
/// clipped.
#[must_use]
pub fn properties_window_extent(scale: Scale, theme: &Theme) -> (u32, u32) {
    let line = control_height(scale, theme).max(1);
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let perms = PermsSection::shape();
    let width = scale
        .scale_length(PROPERTIES_WINDOW_WIDTH)
        .max(perms.natural_width(scale, theme))
        .max(1);
    // General: every metadata field the section can show, as fact rows.
    let general = fact_rows_height(PROPERTY_ROW_COUNT, scale, theme);
    // Attributes: a few rows of list plus the editor band.
    let attributes = row_height(scale, theme)
        .saturating_mul(PROPERTIES_OPEN_ATTR_ROWS)
        .saturating_add(line)
        .saturating_add(pad.saturating_mul(4));
    let body = general
        .max(perms.measured_height(width, scale, theme))
        .max(attributes);
    (
        width,
        identity_height(scale, theme)
            .saturating_add(tab_strip_height(scale, theme))
            .saturating_add(body)
            .max(1),
    )
}

/// The Properties window's narrowest opening width, in logical pixels at the
/// reference density: room for the label column, a value as long as a
/// timestamp or a path, and the identity band's name beside its artwork.
const PROPERTIES_WINDOW_WIDTH: u32 = 460;

/// How many attribute rows the window opens tall enough to show. The list
/// scrolls, so this is a starting size and not a bound on what a node may
/// carry.
const PROPERTIES_OPEN_ATTR_ROWS: u32 = 5;

/// Draw a Properties window's whole client area for `frame`.
///
/// The window is an identity band naming the node, a tab strip, and the
/// selected section's body — no second panel header inside a window that
/// already has a title bar. `view` says which section is current and how far
/// it is scrolled; `controls` carries the live editors the sections draw over
/// their rows and the bar they scroll through. A section taller than the body
/// is laid out at its natural height and shown through it, cut at its edges,
/// with the bar beside it.
///
/// It reads only the already-authorised [`Properties`] and draws: no I/O, no
/// authority, and every blit clips, so a window dragged small shows what fits
/// rather than panicking. The ownership control is editable only when
/// `controls.can_chown`, which the caller sets only where the launching user
/// holds `CAP_FS_CHOWN`.
#[allow(clippy::too_many_arguments)] // The node, what it shows, its live controls, and the frame.
pub fn draw_properties_window(
    surface: &mut Surface,
    frame: PropertiesFrame<'_>,
    view: PropertiesView,
    controls: PropertiesControls<'_>,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    artwork: &mut dyn IconArtwork,
) {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let palette = theme.palette();
    surface.fill_rect(
        u32::try_from(window.left()).unwrap_or(0),
        u32::try_from(window.top()).unwrap_or(0),
        window.width,
        window.height,
        palette.surface.into(),
    );
    let layout = PropertiesLayout::resolve(window, scale, theme);
    draw_identity(
        surface,
        controls.identity,
        scale,
        theme,
        layout.identity,
        artwork,
    );

    // A window that has nothing to describe yet says so where its body would
    // be, and draws no tab strip: there is nothing to choose between.
    let props = match frame {
        PropertiesFrame::Reading => {
            draw_body_note(surface, &layout, PROPERTIES_READING, scale, theme, font);
            return;
        }
        PropertiesFrame::Refused(reason) => {
            draw_body_note(surface, &layout, reason, scale, theme, font);
            return;
        }
        PropertiesFrame::Ready(props) => props,
    };
    properties_tabs(view.tab).render(surface, layout.tabs, scale, theme, artwork);
    let Some(body) = layout.body else {
        return;
    };
    match view.tab {
        PropertiesTab::General => {
            let (scrolled, frame) = general_column(props, body, view.scroll, scale, theme);
            scrolled.view.paint(surface, |surface| {
                draw_fact_rows(surface, Field::stated(props), props, frame, scale, theme);
            });
            scrolled.draw_bar(surface, controls.scrollbar, scale, theme);
        }
        PropertiesTab::Permissions => {
            PermsSection::new(props, controls.owner_gate(), view.perms).render(
                surface,
                (body, view.scroll),
                controls.scrollbar,
                scale,
                theme,
            );
        }
        PropertiesTab::Attributes => {
            draw_attributes_section(surface, props, body, view, controls, scale, theme, font);
        }
    }
}

/// State a window's body as one muted line, for a window with nothing to
/// section yet.
fn draw_body_note(
    surface: &mut Surface,
    layout: &PropertiesLayout,
    note: &str,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) {
    let Some(body) = layout.body else {
        return;
    };
    let pad = to_i32(scale.scale_length(LABEL_PADDING).saturating_mul(2));
    font.draw_text(
        surface,
        body.left().saturating_add(pad),
        body.top().saturating_add(pad),
        font.truncate_to_width(note, body.width),
        theme.palette().on_surface_muted.into(),
    );
}

/// What the window says while its read is in flight.
const PROPERTIES_READING: &str = "Reading…";

/// The live editors and identity a Properties window draws over its rows.
#[derive(Copy, Clone)]
pub struct PropertiesControls<'a> {
    /// What the identity band names and pictures.
    pub identity: Identity<'a>,
    /// Whether the launching user holds `CAP_FS_CHOWN`, the one gate on
    /// offering an editable ownership control at all.
    pub can_chown: bool,
    /// The open owning-id editor, when one is being typed into.
    pub owner: Option<(OwnerField, &'a TextField)>,
    /// The `key = value` attribute editor, which the window always has: it is
    /// the one text surface the section is edited through.
    pub attribute: &'a TextField,
    /// The bar of the selected section's scrolling column, carrying its live
    /// hover and drag state.
    pub scrollbar: &'a ScrollBar,
}

impl<'a> PropertiesControls<'a> {
    /// The two of these the Permissions section is composed from.
    const fn owner_gate(self) -> OwnerGate<'a> {
        (self.can_chown, self.owner)
    }
}

/// The General section's column: the node's metadata facts, laid out at
/// their natural height down `body` and scrolled `offset` pixels, and the
/// frame they are laid out in.
///
/// Every value comes straight from the [`Properties`] model, so a timestamp
/// the backing does not keep renders blank rather than a fabricated wall time.
fn general_column(
    props: &Properties,
    body: Rect,
    offset: u64,
    scale: Scale,
    theme: &Theme,
) -> (Scrolled, Rect) {
    let facts = Field::stated(props).count();
    let line = FactList::row_height(scale, theme);
    let extent = fact_rows_height(facts, scale, theme);
    Scrolled::measured(
        body,
        gutter_width(scale, theme, body.width),
        (offset, line),
        |_| extent,
    )
}

/// The height `facts` fact rows take drawn by [`draw_fact_rows`]: the rows
/// and the padding either side of them.
fn fact_rows_height(facts: usize, scale: Scale, theme: &Theme) -> u32 {
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    FactList::row_height(scale, theme)
        .saturating_mul(u32::try_from(facts).unwrap_or(u32::MAX))
        .saturating_add(pad.saturating_mul(2))
}

/// Draw the extended-attribute section: the node's attributes as selectable
/// rows with the cursor row marked, scrolled in the band above the
/// `key = value` editor with the bar beside them once they outgrow it, and the
/// editor with its Set and Remove actions at the foot.
///
/// A volume that stores no attributes says so, and a node that carries none
/// says that instead — an empty list would be a claim the reader cannot tell
/// apart from either.
#[allow(clippy::too_many_arguments)] // The node, its body, its live state, and the frame.
fn draw_attributes_section(
    surface: &mut Surface,
    props: &Properties,
    body: Rect,
    view: PropertiesView,
    controls: PropertiesControls<'_>,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) {
    let Some(layout) = AttrsLayout::resolve(body, scale, theme, font) else {
        return;
    };
    let palette = theme.palette();
    let attrs = props.attributes();
    let note = match attrs {
        Attributes::Unread | Attributes::Unsupported => Some(String::from(ATTR_UNSUPPORTED)),
        Attributes::Refused(errno) => Some(alloc::format!("{ATTR_REFUSED} ({errno})")),
        Attributes::Visible(list) if list.is_empty() => Some(String::from(ATTR_NONE)),
        Attributes::Visible(_) => None,
    };
    if let Some(note) = note {
        if let Some(band) = layout.band {
            font.draw_text(
                surface,
                band.left(),
                band.top(),
                font.truncate_to_width(&note, band.width),
                palette.on_surface_muted.into(),
            );
        }
    } else if let Some((rows, scrolled)) =
        layout.rows(attrs.visible().len(), view.scroll, scale, theme)
    {
        let list = attrs.visible();
        scrolled.view.paint(surface, |surface| {
            for index in rows.visible_range(view.scroll) {
                let (Some(attr), Some(bounds)) = (list.get(index), rows.row_rect(index)) else {
                    break;
                };
                let mut row = ListRow::new(attr.key_display()).with_trailing(attr.display());
                row.set_selected(index == view.cursor);
                row.render(surface, bounds, scale, theme, None);
            }
        });
        scrolled.draw_bar(surface, controls.scrollbar, scale, theme);
    }
    controls
        .attribute
        .render(surface, layout.editor, scale, theme);
    for ((action, label), rect) in ATTR_ACTIONS.iter().zip(layout.actions.iter()) {
        let role = match action {
            AttrAction::Set => ControlRole::Primary,
            AttrAction::Remove => ControlRole::Destructive,
        };
        Button::new(ButtonContent::Label(String::from(*label)), role)
            .render(surface, *rect, scale, theme);
    }
}

/// What a press at window-local `point` on a Properties window showing `props`
/// resolves to, or `None` when it is on nothing.
///
/// Mirrors [`draw_properties_window`]'s placement through the one shared
/// layout, so a press acts on exactly the control the user saw: the tab strip
/// first, then only the controls the selected section actually drew, where its
/// scroll shows them. The capability-free permission toggles resolve before
/// the privileged ownership control, so a session that may not reassign an
/// owner can still toggle a mode bit on the same surface; a press on nothing —
/// the scroll gutter included — changes nothing.
///
/// `controls` are the ones the draw took — the ownership gate and any open id
/// editor among them — so the hit-test resolves exactly what was painted.
#[must_use]
pub fn properties_hit(
    props: &Properties,
    view: PropertiesView,
    controls: PropertiesControls<'_>,
    window: Rect,
    scale: Scale,
    theme: &Theme,
    point: Point,
) -> Option<PropertiesTarget> {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let layout = PropertiesLayout::resolve(window, scale, theme);
    if let Some(index) = properties_tabs(view.tab).tab_at(layout.tabs, scale, theme, point) {
        return PropertiesTab::at(index).map(PropertiesTarget::Tab);
    }
    let body = layout.body.filter(|body| body.contains(point))?;
    match view.tab {
        PropertiesTab::General => None,
        PropertiesTab::Permissions => {
            let section = PermsSection::new(props, controls.owner_gate(), view.perms);
            let (scrolled, placed) = section.laid_out(body, view.scroll, scale, theme);
            let at = scrolled.view.to_content(point)?;
            section.target_at(&placed, scale, theme, at)
        }
        PropertiesTab::Attributes => {
            let attrs = AttrsLayout::resolve(body, scale, theme, font)?;
            for ((action, _), rect) in ATTR_ACTIONS.iter().zip(attrs.actions.iter()) {
                if contains(*rect, point) {
                    return Some(PropertiesTarget::Action(*action));
                }
            }
            if contains(attrs.editor, point) {
                return Some(PropertiesTarget::Editor);
            }
            let count = props.attributes().visible().len();
            let (rows, _) = attrs.rows(count, view.scroll, scale, theme)?;
            rows.index_at(view.scroll, point)
                .map(PropertiesTarget::Attribute)
        }
    }
}

/// Whether `point` lies inside `rect`, on the same half-open convention every
/// other hit-test in this module uses.
fn contains(rect: Rect, point: Point) -> bool {
    let right = rect.left().saturating_add(to_i32(rect.width));
    let bottom = rect.top().saturating_add(to_i32(rect.height));
    point.x >= rect.left() && point.x < right && point.y >= rect.top() && point.y < bottom
}

/// The scrolling column the section `view` shows, and where in its layout
/// the section's keyboard cursor rests — or `None` when the window lays out no
/// column: a body too short, or an attribute list with nothing in it.
///
/// `can_chown` is the gate the draw took: a session refused ownership is shown
/// a footnote saying why, which lengthens the Permissions column.
fn section_column(
    props: &Properties,
    view: PropertiesView,
    can_chown: bool,
    (window, scale, theme): (Rect, Scale, &Theme),
) -> Option<(Scrolled, Option<Rect>)> {
    let body = PropertiesLayout::resolve(window, scale, theme).body?;
    match view.tab {
        PropertiesTab::General => Some((
            general_column(props, body, view.scroll, scale, theme).0,
            None,
        )),
        PropertiesTab::Permissions => {
            let section = PermsSection::new(props, (can_chown, None), view.perms);
            let (scrolled, placed) = section.laid_out(body, view.scroll, scale, theme);
            let cursor = view
                .perms
                .row
                .and_then(|at| section.row_rect(&placed, at, scale, theme));
            Some((scrolled, cursor))
        }
        PropertiesTab::Attributes => {
            let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
            let attrs = props.attributes().visible();
            if attrs.is_empty() {
                return None;
            }
            let (rows, scrolled) = AttrsLayout::resolve(body, scale, theme, font)?.rows(
                attrs.len(),
                view.scroll,
                scale,
                theme,
            )?;
            Some((scrolled, rows.row_rect(view.cursor)))
        }
    }
}

/// Scroll the selected section by a wheel turn of `(dx, dy)`, in the seat's
/// scroll units, through `scroll`, answering whether it moved.
///
/// `scroll` is the window's own column, the one `view.scroll` was read from;
/// `can_chown` is the gate the draw took. A section that fits its body has
/// nothing to scroll. A move reports the column and its bar.
pub fn properties_scroll_wheel(
    scroll: &mut ScrollColumn,
    props: &Properties,
    view: PropertiesView,
    can_chown: bool,
    frame: (Rect, Scale, &Theme),
    delta: (i32, i32),
    damage: &mut Region,
) -> bool {
    let Some((scrolled, _)) = section_column(props, view, can_chown, frame) else {
        return false;
    };
    let Some(gutter) = scrolled.gutter else {
        return false;
    };
    scroll.wheel(
        scrolled.model,
        delta,
        frame.1,
        (gutter, scrolled.view.viewport()),
        damage,
    )
}

/// Route a pointer `event` at window-local `point` to the selected section's
/// scroll bar, moving `scroll`: `None` when the pointer had nothing to do with
/// the bar, otherwise whether it repainted anything.
///
/// The same routing the listing and the *Open With…* chooser use, so a drag on
/// this bar behaves exactly as a drag on either of those. The bar reports its
/// own look, and a move reports the column it slid.
pub fn properties_scroll_pointer(
    scroll: &mut ScrollColumn,
    props: &Properties,
    view: PropertiesView,
    can_chown: bool,
    frame: (Rect, Scale, &Theme),
    pointer: (Point, &InputEvent),
    damage: &mut Region,
) -> Option<bool> {
    let (scrolled, _) = section_column(props, view, can_chown, frame)?;
    let gutter = scrolled.gutter?;
    let (_, scale, theme) = frame;
    scroll.route(
        scrolled.model,
        (gutter, scrolled.view.viewport()),
        scale,
        theme,
        pointer,
        damage,
    )
}

/// Scroll the selected section the least that shows its keyboard cursor —
/// the Permissions row it rests on, or the attribute row — answering whether
/// it moved.
///
/// What a key that moved a cursor asks for next, so the row it lands on is
/// always one the reader can see. A move reports the column and its bar.
pub fn properties_reveal(
    scroll: &mut ScrollColumn,
    props: &Properties,
    view: PropertiesView,
    can_chown: bool,
    frame: (Rect, Scale, &Theme),
    damage: &mut Region,
) -> bool {
    let Some((scrolled, Some(cursor))) = section_column(props, view, can_chown, frame) else {
        return false;
    };
    let offset = scrolled.revealing(cursor);
    scroll.set_offset(offset);
    if offset == scrolled.model.offset() {
        return false;
    }
    damage.add(scrolled.view.viewport());
    if let Some(gutter) = scrolled.gutter {
        damage.add(gutter);
    }
    true
}

/// The window-local rectangle of the active owner editor for `field` that
/// shows on a window showing `props` at `view`'s scroll, or `None` when its
/// row is scrolled out of view or the body has no room to lay one out.
///
/// An editor takes the whole slot of its ownership row, so this is that
/// slot — the one placement [`draw_properties_window`] draws it at, published
/// so the host feeding that editor keys reports the rectangle it repaints
/// instead of re-deriving this layout.
#[must_use]
pub fn properties_owner_editor_rect(
    props: &Properties,
    view: PropertiesView,
    window: Rect,
    scale: Scale,
    theme: &Theme,
    field: OwnerField,
) -> Option<Rect> {
    let body = PropertiesLayout::resolve(window, scale, theme).body?;
    let section = PermsSection::new(props, (true, None), PermsCursor::default());
    let (scrolled, placed) = section.laid_out(body, view.scroll, scale, theme);
    let layout = placed
        .into_iter()
        .find_map(|(index, layout)| (index == OWNERSHIP).then_some(layout))?;
    let group = &section.groups[OWNERSHIP];
    let row = field.row()?;
    let bounds = group.row_rect(row, layout, scale, theme)?;
    let slot =
        group
            .rows()
            .get(row)?
            .slot_rect(FieldLayout::new(bounds, layout.column), scale, theme)?;
    scrolled.view.to_window(slot)
}

/// What a key did to the Permissions section.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PermsKeyed {
    /// Where the section's keyboard cursor now stands.
    pub cursor: PermsCursor,
    /// The control the key activated, named exactly as a press on it would
    /// be, so the host acts on it through the one path both take.
    pub target: Option<PropertiesTarget>,
}

/// Feed `key` to the Permissions section of a window showing `props`,
/// reporting the rows and flags it repainted into `damage`.
///
/// From the strip, Down or Tab takes the keyboard onto the section's first
/// row. There Up and Down walk the rows and carry on into the next group,
/// Home and End jump within one, Left and Right walk an access row's flags,
/// and Space or Enter toggles the flag or opens the ownership cell the cursor
/// rests on; Tab or Escape hands the keyboard back to the strip. A refused
/// ownership cell is reached and read but resolves to nothing, and while an
/// id editor is open the keyboard is the editor's and nothing here moves.
///
/// A cursor that moved onto a row the scroll hides is revealed by
/// [`properties_reveal`].
#[allow(clippy::too_many_arguments)] // The node, what it shows, the frame, and the key.
#[must_use]
pub fn properties_permissions_key(
    props: &Properties,
    view: PropertiesView,
    controls: PropertiesControls<'_>,
    window: Rect,
    scale: Scale,
    theme: &Theme,
    key: Keystroke,
    damage: &mut Region,
) -> PermsKeyed {
    let unchanged = PermsKeyed {
        cursor: view.perms,
        target: None,
    };
    // An open id editor holds the keyboard; the section answers no key while
    // it does.
    if controls.owner.is_some() {
        return unchanged;
    }
    let Some(body) = PropertiesLayout::resolve(window, scale, theme).body else {
        return unchanged;
    };
    let mut section = PermsSection::new(props, controls.owner_gate(), view.perms);
    let (scrolled, placed) = section.laid_out(body, view.scroll, scale, theme);
    let mut drew = tairix_controls::damage::sink();
    let keyed = section.key(&placed, view.perms, key, (scale, theme), &mut drew);
    scrolled.view.report(&drew, damage);
    keyed
}

/// Where the `key = value` attribute editor's field is drawn, or `None` when
/// the section does not fit.
#[must_use]
pub fn properties_attr_editor_rect(window: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let body = PropertiesLayout::resolve(window, scale, theme).body?;
    AttrsLayout::resolve(body, scale, theme, font).map(|attrs| attrs.editor)
}

/// Saturating `u32` → `i32`.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// The action-button index of the destructive **Delete** action in the
/// delete-confirmation [`Dialog`] [`build_delete_dialog`] produces.
pub const DELETE_CONFIRM_INDEX: usize = 0;

/// The action-button index of the safe **Cancel** action in the
/// delete-confirmation [`Dialog`] [`build_delete_dialog`] produces.
pub const DELETE_CANCEL_INDEX: usize = 1;

/// Build the modal delete-confirmation [`Dialog`] for `plan`, worded honestly
/// for the `disposition` the caller will actually carry out: a recoverable
/// **Move to Trash** or an irreversible **Delete Permanently**.
///
/// The [`DeleteTarget`](crate::DeleteTarget) count and
/// [`has_directories`](DeletePlan::has_directories) come straight from the
/// already-captured [`DeletePlan`], so the confirmation reports the true scope
/// of the removal rather than a fabricated figure. `disposition`
/// ([`DeleteDisposition`]) is the caller's own decision — computed from the
/// targets' and the user's Trash directory's volume ids — so the dialog never
/// promises a wording its execution will not honour: a
/// [`Trash`](DeleteDisposition::Trash) confirmation offers a safe, recoverable
/// **Move to Trash**, a [`Permanent`](DeleteDisposition::Permanent) one the
/// destructive **Delete Permanently** with the honest warmth on the safe
/// Cancel. The dialog performs nothing itself — the caller drives the removal
/// in its own capability-checked tail once the user confirms — so composing it
/// grants no authority. Both the file manager (which builds one) and, in
/// principle, any other write-capable consumer share this one definition; the
/// read-only picker never deletes, so it never builds one.
#[must_use]
pub fn build_delete_dialog(plan: &DeletePlan, disposition: DeleteDisposition) -> Dialog {
    match disposition {
        DeleteDisposition::Trash => build_trash_dialog(plan),
        DeleteDisposition::Permanent => build_permanent_delete_dialog(plan),
    }
}

/// The recoverable **Move to Trash** confirmation: nothing is destroyed, so the
/// confirm action is the recommended (safe) primary rather than a destructive
/// one, and the message states that trashed items can be restored.
fn build_trash_dialog(plan: &DeletePlan) -> Dialog {
    let title = if plan.len() == 1 {
        alloc::format!(
            "Move \u{201c}{}\u{201d} to Trash?",
            plan.targets()[0].name()
        )
    } else {
        alloc::format!("Move {} items to Trash?", plan.len())
    };
    // Recoverable: the honest warmth sits on the confirm action because the
    // move can be undone by restoring from Trash — it is not destructive.
    let confirm = Button::new(
        ButtonContent::Label(String::from("Move to Trash")),
        ControlRole::Recommended,
    );
    let cancel = Button::new(
        ButtonContent::Label(String::from("Cancel")),
        ControlRole::Neutral,
    );
    Dialog::new(title)
        .with_message("Items stay in the Trash until you empty it, so you can restore them.")
        .with_actions(vec![confirm, cancel])
}

/// The irreversible **Delete Permanently** confirmation: the destructive action
/// carries the Destructive role and the confirmation posture, and Cancel is the
/// recommended (safe, trailing) action so the honest warmth sits on the safe
/// choice, never on the delete.
fn build_permanent_delete_dialog(plan: &DeletePlan) -> Dialog {
    let title = if plan.len() == 1 {
        alloc::format!(
            "Delete \u{201c}{}\u{201d} permanently?",
            plan.targets()[0].name()
        )
    } else {
        alloc::format!("Delete {} items permanently?", plan.len())
    };
    let message = if plan.has_directories() {
        "Folders and everything inside them will be removed. This cannot be undone."
    } else {
        "This cannot be undone."
    };
    let mut delete = Button::new(
        ButtonContent::Label(String::from("Delete Permanently")),
        ControlRole::Destructive,
    );
    delete.set_state(ControlState::idle().with_authority(AuthorityState::NeedsConfirmation));
    let cancel = Button::new(
        ButtonContent::Label(String::from("Cancel")),
        ControlRole::Recommended,
    );
    Dialog::new(title)
        .with_message(message)
        .with_actions(vec![delete, cancel])
}

/// The centered, clamped bounds of the delete-confirmation dialog within
/// `viewport`.
///
/// Sized to comfortably show the title, the warning message, and the action
/// button band, and clamped to the window so a small window still yields a
/// drawable — if clipped — dialog rather than a panic. One definition so
/// [`draw_delete_dialog`] and [`delete_dialog_action_at`] place and hit-test
/// the same rectangle.
#[must_use]
pub fn delete_dialog_rect(viewport: Rect, scale: Scale, theme: &Theme) -> Rect {
    // Title bar, up to two message lines, and the action-button band, with
    // margins — generous so the buttons are not clipped at a normal size.
    centered_overlay_rect(viewport, scale, theme, 6)
}

/// A centered, clamped modal-overlay rectangle within `viewport`, sized to a
/// title bar plus `content_lines` text rows and four-fifths of the window
/// width, clamped so a small window still yields a drawable — if clipped —
/// rectangle rather than a panic.
///
/// The one sizing definition the delete-confirmation dialog and the progress
/// panel share, so their placement stays consistent and cannot drift.
fn centered_overlay_rect(viewport: Rect, scale: Scale, theme: &Theme, content_lines: u32) -> Rect {
    let line = row_height(scale, theme);
    let title = scale.scale_length(theme.metrics().title_bar_height).max(1);
    let content = line.saturating_mul(content_lines);
    let height = title.saturating_add(content).min(viewport.height.max(1));
    let width = viewport
        .width
        .saturating_mul(4)
        .checked_div(5)
        .unwrap_or(viewport.width)
        .clamp(1, viewport.width.max(1));
    let x = viewport
        .origin
        .x
        .saturating_add(to_i32(viewport.width.saturating_sub(width) / 2));
    let y = viewport
        .origin
        .y
        .saturating_add(to_i32(viewport.height.saturating_sub(height) / 2));
    Rect::new(x, y, width, height)
}

/// Draw the delete-confirmation `dialog` centered in `viewport`, on top of the
/// current view.
///
/// Every blit clips, so a window too small for the whole dialog simply shows
/// what fits rather than panicking. It reads only the passed-in dialog and
/// draws — it performs no I/O and holds no authority.
pub fn draw_delete_dialog(
    surface: &mut Surface,
    dialog: &Dialog,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
) {
    let bounds = delete_dialog_rect(viewport, scale, theme);
    dialog.render(surface, bounds, scale, theme);
}

/// The action-button index the delete-confirmation `dialog` draws at
/// window-local pixel `point`, or `None` when the click is not on a button.
///
/// This mirrors [`draw_delete_dialog`]'s placement through the shared
/// [`delete_dialog_rect`] and the dialog's own
/// [`action_rects`](Dialog::action_rects) geometry, so a click resolves to
/// exactly the button the user pressed — [`DELETE_CONFIRM_INDEX`] for Delete,
/// [`DELETE_CANCEL_INDEX`] for Cancel. Only the file manager calls it; a click
/// anywhere but a button returns `None`, changing nothing (fail closed).
#[must_use]
pub fn delete_dialog_action_at(
    dialog: &Dialog,
    viewport: Rect,
    scale: Scale,
    theme: &Theme,
    point: Point,
) -> Option<usize> {
    let bounds = delete_dialog_rect(viewport, scale, theme);
    let rects = dialog.action_rects(bounds, scale, theme);
    for (i, rect) in rects.iter().enumerate() {
        if rect.width == 0 {
            continue;
        }
        let right = rect.left().saturating_add(to_i32(rect.width));
        let bottom = rect.top().saturating_add(to_i32(rect.height));
        if point.x >= rect.left() && point.x < right && point.y >= rect.top() && point.y < bottom {
            return Some(i);
        }
    }
    None
}

/// The centered, clamped bounds of the long-operation progress panel within
/// `viewport`.
///
/// One definition so [`draw_progress_dialog`] and [`progress_cancel_at`] place
/// and hit-test the same rectangle, sized like the delete-confirmation dialog
/// so the two modal surfaces sit consistently.
#[must_use]
pub fn progress_dialog_rect(viewport: Rect, scale: Scale, theme: &Theme) -> Rect {
    centered_overlay_rect(viewport, scale, theme, 6)
}

/// The Cancel-button rectangle within the progress panel's `content` area —
/// bottom-right, sized to the "Cancel" label plus padding, clamped to the
/// content so a small window never places it off the panel. The one definition
/// [`draw_progress_dialog`] paints and [`progress_cancel_at`] hit-tests, so a
/// click resolves to exactly the drawn button.
fn progress_cancel_rect(content: Rect, scale: Scale, theme: &Theme, font: BitmapFont) -> Rect {
    let pad = font.text_width("  ").max(scale.scale_length(LABEL_PADDING));
    let width = font
        .text_width("Cancel")
        .saturating_add(pad.saturating_mul(2))
        .min(content.width);
    let height = row_height(scale, theme).min(content.height);
    let x = content
        .left()
        .saturating_add(to_i32(content.width.saturating_sub(width)));
    let y = content
        .top()
        .saturating_add(to_i32(content.height.saturating_sub(height)));
    Rect::new(x, y, width, height)
}

/// Build the progress panel's [`Progress`] trace for `model`: an indeterminate
/// "working" bar captioned with the model's honest running count.
///
/// The total is unknown until the driving walk's reads reveal it, so the trace
/// is [`ActivityState::Working`] (a bounded moving segment) rather than a
/// fabricated percentage. Its moving-segment phase is derived from the count,
/// so the bar advances on real job-progress events, never an idle animation
/// loop.
#[must_use]
pub fn build_progress(model: &ProgressModel) -> Progress {
    let mut progress = Progress::new().with_label(model.status_line());
    progress.set_state(ControlState::idle().with_activity(ActivityState::Working));
    // A permille phase that turns over as items are processed — motion is
    // driven by real progress, not a timer.
    let phase = u16::try_from(model.done() % 1000).unwrap_or(0);
    progress.set_phase(phase);
    progress
}

/// Build the progress panel's Cancel [`Button`] for `model`: enabled while the
/// run is in progress, disabled once a cancel has already been latched (so a
/// second press cannot re-request what is already stopping).
#[must_use]
pub fn build_progress_cancel(model: &ProgressModel) -> Button {
    let mut button = Button::new(
        ButtonContent::Label(String::from("Cancel")),
        ControlRole::Neutral,
    );
    if model.is_cancel_requested() {
        button.set_state(ControlState::disabled());
    }
    button
}

/// Draw the long-operation progress panel for `model` centered in `viewport`,
/// on top of the current view: a titled [`Panel`], an indeterminate progress
/// trace captioned with the honest running count, and a Cancel button.
///
/// Every blit clips, so a window too small for the whole panel simply shows
/// what fits rather than panicking. It reads only the passed-in model and draws
/// — it performs no I/O and holds no authority. Only the write-capable file
/// manager drives a long operation, so only it draws this; the read-only picker
/// never does.
pub fn draw_progress_dialog(
    surface: &mut Surface,
    model: &ProgressModel,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
) {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let bounds = progress_dialog_rect(viewport, scale, theme);
    let panel = Panel::new(model.title());
    panel.render(surface, bounds, scale, theme);
    let Some(content) = panel.content_rect(bounds, scale, theme) else {
        return;
    };
    let bar = Rect::new(
        content.left(),
        content.top(),
        content.width,
        row_height(scale, theme),
    );
    build_progress(model).render(surface, bar, scale, theme);
    let cancel_rect = progress_cancel_rect(content, scale, theme, font);
    build_progress_cancel(model).render(surface, cancel_rect, scale, theme);
}

/// Whether the progress panel's Cancel button is drawn at window-local pixel
/// `point`.
///
/// Mirrors [`draw_progress_dialog`]'s placement through the shared
/// [`progress_dialog_rect`] and the same private cancel-button rectangle it
/// paints, so a click resolves to exactly the drawn button. A click anywhere
/// but the button — or on a panel too small to place it — returns `false`,
/// changing nothing (fail closed).
#[must_use]
pub fn progress_cancel_at(viewport: Rect, scale: Scale, theme: &Theme, point: Point) -> bool {
    let bounds = progress_dialog_rect(viewport, scale, theme);
    // The content area is title-text-independent, so an empty-title panel
    // mirrors the titled panel [`draw_progress_dialog`] draws.
    let Some(content) = Panel::new(String::new()).content_rect(bounds, scale, theme) else {
        return false;
    };
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let rect = progress_cancel_rect(content, scale, theme, font);
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    let right = rect.left().saturating_add(to_i32(rect.width));
    let bottom = rect.top().saturating_add(to_i32(rect.height));
    point.x >= rect.left() && point.x < right && point.y >= rect.top() && point.y < bottom
}

/// Most candidate rows the "Open With…" chooser's popup is sized to show at
/// once.
///
/// A bound on the *popup*, not on the candidate set: the set grows with the
/// applications a user installs, and a longer list scrolls inside the panel.
/// Fewer candidates make a **shorter** popup — the surface is sized to its
/// content, so one candidate is one row of plate and not eight.
pub const OPEN_WITH_MAX_ROWS: usize = 8;

/// The rows the chooser's list wants for `candidates`: all of them, up to the
/// bound, and never none — a chooser is never built over an empty list.
fn open_with_wanted_rows(candidates: usize) -> u32 {
    u32::try_from(candidates.clamp(1, OPEN_WITH_MAX_ROWS)).unwrap_or(1)
}

/// The narrowest the chooser is ever drawn, in logical pixels at the reference
/// density — a panel narrower than this reads as a sliver whatever it holds.
///
/// A floor, not the width: the rule below widens it to whatever the longest
/// candidate, the title, and the action buttons actually measure.
const OPEN_WITH_MIN_WIDTH: u32 = 200;

/// The extent of the chooser's own popup window for `chooser`, capped to the
/// `screen` it must fit on.
///
/// The popup **is** the chooser: the panel fills it, so the surface's own size
/// is what decides how many rows are shown and how much of each name reads. A
/// one-candidate chooser is a one-row popup rather than a plate with seven rows
/// of nothing, and a chooser of short names is a compact panel rather than a
/// letterbox four fifths of the display wide — the manager's *centred* modal
/// surfaces take that proportion because they are drawn over the listing; a
/// surface in its own window is sized to what it draws.
///
/// Both extents are content-derived through the same inverses the panel lays
/// its content out with, so nothing the chooser was widened or heightened for
/// is elided or clipped: the identity band naming the file, the widest
/// candidate row, and the whole action band each fit, floored at a stated
/// logical width and clamped to the screen.
#[must_use]
pub fn open_with_chooser_extent(
    chooser: &OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    screen: Rect,
) -> (u32, u32) {
    let rows =
        row_height(scale, theme).saturating_mul(open_with_wanted_rows(chooser.candidates().len()));
    // The identity band and the action band come from the same inverses the
    // panel lays them out with rather than a second reckoning of its rim: a
    // difference of one border there costs the list a whole row once it is
    // divided by a row height.
    let content = rows
        .saturating_add(identity_height(scale, theme))
        .saturating_add(open_with_action_band(scale, theme));
    let height = Panel::height_for_content(content, scale, theme);
    (
        open_with_chooser_width(chooser, scale, theme, screen),
        height.min(screen.height.max(1)).max(1),
    )
}

/// The chooser popup's width: the widest of what it must hold, floored at
/// [`OPEN_WITH_MIN_WIDTH`] and clamped to `screen`.
///
/// Each candidate is measured as the [`ListRow`] it is drawn as — icon column,
/// paddings and all — with the scroll gutter beside it, so a name the panel was
/// widened for is not then elided by the row's own reservations.
fn open_with_chooser_width(
    chooser: &OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    screen: Rect,
) -> u32 {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let row = row_height(scale, theme);
    let rows = chooser
        .candidates()
        .iter()
        .map(|candidate| {
            ListRow::new(candidate.name())
                .with_icon(IconKind::AppBundle)
                .with_trailing(OPEN_WITH_DEFAULT_MARK)
                .width_for_content(row, scale, theme)
        })
        .max()
        .unwrap_or(0);
    // The gutter is sized off the panel's own width, which is what this is
    // computing; asking for it at the floor keeps the reservation stable
    // instead of chasing its own answer.
    let gutter = gutter_width(scale, theme, scale.scale_length(OPEN_WITH_MIN_WIDTH));
    let title = BitmapFont::for_role(theme.fonts(), TextRole::ItemTitle, scale)
        .text_width(chooser.display_name())
        .saturating_add(scale.scale_length(IDENTITY_ART))
        .saturating_add(scale.scale_length(theme.metrics().control_gap))
        .saturating_add(scale.scale_length(LABEL_PADDING).saturating_mul(4));
    let content = rows
        .saturating_add(gutter)
        .max(title)
        .max(open_with_actions_width(scale, theme, font));
    Panel::width_for_content(content, scale, theme)
        .max(scale.scale_length(OPEN_WITH_MIN_WIDTH))
        .clamp(1, screen.width.max(1))
}

/// The trailing mark on the candidate a plain *Open* would have used.
///
/// The chooser is reached to override that choice, so which application would
/// have been picked anyway is the one thing the list must say — otherwise the
/// user is choosing between names with no idea which is the status quo.
const OPEN_WITH_DEFAULT_MARK: &str = "Default";

/// The chooser panel's bounds within its own popup `viewport`: the whole of
/// it.
///
/// One placement definition, shared by [`draw_open_with_chooser`],
/// [`open_with_row_at`], [`open_with_action_at`] and the chooser's scrolling
/// paths, so what is drawn and what a press resolves to can never disagree.
#[must_use]
pub const fn open_with_chooser_rect(viewport: Rect) -> Rect {
    viewport
}

/// The chooser's candidate rows, laid out unscrolled down its list band beside
/// the scroll gutter, and the gutter — or `None` when the popup leaves the list
/// no room at all.
///
/// Derived from the band the popup actually has, so a popup the screen clamped
/// shows what it can rather than what it asked for, cutting the row its edge
/// crosses.
fn open_with_rows(
    chooser: &OpenWithChooser,
    viewport: Rect,
    scale: Scale,
    theme: &Theme,
) -> Option<(ListView, Option<Rect>)> {
    let band = open_with_list_rect(viewport, scale, theme)?;
    let gutter = gutter_width(scale, theme, band.width);
    let rows = ListView::new(
        Rect::new(
            band.left(),
            band.top(),
            band.width.saturating_sub(gutter),
            band.height,
        ),
        row_height(scale, theme),
        0,
        chooser.candidates().len(),
    );
    let bar = (gutter > 0).then(|| {
        Rect::new(
            band.left()
                .saturating_add(to_i32(band.width.saturating_sub(gutter))),
            band.top(),
            gutter,
            band.height,
        )
    });
    Some((rows, bar))
}

/// The chooser panel's whole content area — the identity band, the rows, the
/// scroll gutter, and the action band beneath them.
fn open_with_content_rect(viewport: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let bounds = open_with_chooser_rect(viewport);
    Panel::new(String::new()).content_rect(bounds, scale, theme)
}

/// The identity band across the top of the chooser's content, naming the file
/// being opened.
fn open_with_identity_rect(viewport: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let content = open_with_content_rect(viewport, scale, theme)?;
    let height = identity_height(scale, theme).min(content.height);
    (height > 0).then(|| Rect::new(content.left(), content.top(), content.width, height))
}

/// The part of the content the candidate list occupies: between the identity
/// band and the action band.
fn open_with_list_rect(viewport: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let content = open_with_content_rect(viewport, scale, theme)?;
    let head = identity_height(scale, theme).min(content.height);
    let actions = open_with_action_band(scale, theme).min(content.height.saturating_sub(head));
    let height = content.height.saturating_sub(head).saturating_sub(actions);
    (height > 0).then(|| {
        Rect::new(
            content.left(),
            content.top().saturating_add(to_i32(head)),
            content.width,
            height,
        )
    })
}

/// The height the chooser's action band occupies: a control plate plus the
/// padding that keeps it off the list above and the panel's rim below.
fn open_with_action_band(scale: Scale, theme: &Theme) -> u32 {
    control_height(scale, theme).saturating_add(scale.scale_length(LABEL_PADDING).saturating_mul(4))
}

/// What a press on the chooser's action band asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OpenWithAction {
    /// Open the file with the current candidate.
    Open,
    /// Close the chooser without opening anything.
    Cancel,
}

/// The two action buttons the chooser offers, in the order they are drawn.
const OPEN_WITH_ACTIONS: [(OpenWithAction, &str); 2] = [
    (OpenWithAction::Cancel, "Cancel"),
    (OpenWithAction::Open, "Open"),
];

/// Where each action button is drawn in the band beneath the list, trailing
/// edge last — the one definition [`draw_open_with_chooser`] paints and
/// [`open_with_action_at`] hit-tests.
///
/// Each button is a full control plate, on the theme's own control height
/// rather than a text row pitch, so it is the same object as a button anywhere
/// else on the desktop.
///
/// `None` when the popup leaves no band, which draws and resolves nothing
/// (fail closed): a chooser with no visible Open button is closed with Escape
/// or by activating a row, never left with a hidden action.
fn open_with_action_rects(
    viewport: Rect,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) -> Option<[Rect; OPEN_WITH_ACTIONS.len()]> {
    let content = open_with_content_rect(viewport, scale, theme)?;
    let band = open_with_action_band(scale, theme);
    let plate = control_height(scale, theme);
    if plate == 0 || content.height < band {
        return None;
    }
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    let widths = open_with_action_widths(scale, theme, font);
    // Seated on the band's own baseline, with the padding the band reserved
    // kept clear beneath it.
    let top = content
        .top()
        .saturating_add(to_i32(content.height.saturating_sub(band)))
        .saturating_add(to_i32(pad));
    let mut right = content
        .left()
        .saturating_add(to_i32(content.width.saturating_sub(pad)));
    let mut rects = [Rect::EMPTY; OPEN_WITH_ACTIONS.len()];
    // Laid out from the trailing edge back, so the primary action sits
    // furthest right whatever the labels measure.
    for slot in (0..OPEN_WITH_ACTIONS.len()).rev() {
        let width = widths[slot].min(content.width);
        let left = right.saturating_sub(to_i32(width));
        rects[slot] = Rect::new(left, top, width, plate);
        right = left.saturating_sub(to_i32(gap));
    }
    Some(rects)
}

/// Each action button's intrinsic width, in the order they are drawn — the one
/// definition [`open_with_action_rects`] places and the chooser's own extent
/// reserves room for, so the band can never be sized narrower than the buttons
/// it must hold.
fn open_with_action_widths(
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) -> [u32; OPEN_WITH_ACTIONS.len()] {
    OPEN_WITH_ACTIONS.map(|(_, label)| action_button_width(label, scale, theme, font))
}

/// The whole action band's intrinsic width: every button, the gap between each
/// pair, and the padding the trailing and leading edges keep.
fn open_with_actions_width(scale: Scale, theme: &Theme, font: BitmapFont) -> u32 {
    let pad = scale.scale_length(LABEL_PADDING).saturating_mul(2);
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    let buttons = open_with_action_widths(scale, theme, font)
        .into_iter()
        .fold(0u32, u32::saturating_add);
    buttons
        .saturating_add(
            gap.saturating_mul(
                u32::try_from(OPEN_WITH_ACTIONS.len().saturating_sub(1)).unwrap_or(0),
            ),
        )
        .saturating_add(pad.saturating_mul(2))
}

/// Draw the "Open With…" `chooser` into its own popup `viewport`: a panel
/// opening with the identity band that names the file, one [`ListRow`] per
/// candidate the list shows any part of — the rows its edges cross drawn whole
/// and cut there — with the current one selected and the default one marked,
/// the scrollbar beside them, and the Open/Cancel actions beneath.
///
/// Each candidate's row draws its application's own icon where `artwork`
/// resolves one and the built-in bundle glyph otherwise, exactly as a grid
/// tile does — so the user picks between applications they recognise rather
/// than between nine identical glyphs. It reads only the passed-in chooser and
/// draws — no I/O, no authority — and every blit clips, so a popup too small
/// for the panel simply shows what fits.
pub fn draw_open_with_chooser(
    surface: &mut Surface,
    chooser: &OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    artwork: &mut dyn IconArtwork,
) {
    let bounds = open_with_chooser_rect(viewport);
    Panel::new(String::new()).render(surface, bounds, scale, theme);
    if let Some(band) = open_with_identity_rect(viewport, scale, theme) {
        draw_identity(
            surface,
            Identity {
                name: chooser.display_name(),
                detail: OPEN_WITH_PROMPT,
                // Fail closed to the generic type when the name carries no
                // recognised extension, exactly as a listed entry does.
                art: IconRequest::kind(
                    media_for_name(chooser.display_name())
                        .unwrap_or(MediaType::ApplicationOctetStream)
                        .icon(),
                ),
            },
            scale,
            theme,
            band,
            artwork,
        );
    }
    let Some((rows, gutter)) = open_with_rows(chooser, viewport, scale, theme) else {
        return;
    };
    let offset = chooser.offset();
    rows.view(offset).paint(surface, |surface| {
        for index in rows.visible_range(offset) {
            let (Some(candidate), Some(bounds)) =
                (chooser.candidates().get(index), rows.row_rect(index))
            else {
                break;
            };
            let mut row = ListRow::new(candidate.name()).with_icon(IconKind::AppBundle);
            if index == OPEN_WITH_DEFAULT_INDEX {
                row = row.with_trailing(OPEN_WITH_DEFAULT_MARK);
            }
            row.set_selected(index == chooser.selected());
            let side = row.icon_side(bounds, scale, theme);
            let art = artwork.artwork(
                IconRequest::bundle(IconKind::AppBundle, candidate.bundle_path()),
                side,
            );
            row.render(surface, bounds, scale, theme, art);
        }
    });
    if let Some(gutter) = gutter {
        draw_bar(
            chooser.scroll().scrollbar(),
            rows.scroll_model(offset),
            surface,
            gutter,
            scale,
            theme,
        );
    }
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    if let Some(rects) = open_with_action_rects(viewport, scale, theme, font) {
        for ((action, label), rect) in OPEN_WITH_ACTIONS.iter().zip(rects.iter()) {
            build_open_with_action(*action, label, chooser).render(surface, *rect, scale, theme);
        }
    }
}

/// What the chooser's identity band says beneath the file's name.
const OPEN_WITH_PROMPT: &str = "Choose an application to open this with";

/// The candidate a plain *Open* would have used: the first, because
/// `applications_for` ranks the most specific claim first.
const OPEN_WITH_DEFAULT_INDEX: usize = 0;

/// Build one of the chooser's action buttons: Open is the primary action and
/// is offered only while a candidate is current, Cancel is always available.
fn build_open_with_action(
    action: OpenWithAction,
    label: &str,
    chooser: &OpenWithChooser,
) -> Button {
    let role = match action {
        OpenWithAction::Open => ControlRole::Primary,
        OpenWithAction::Cancel => ControlRole::Neutral,
    };
    let mut button = Button::new(ButtonContent::Label(String::from(label)), role);
    if action == OpenWithAction::Open && chooser.chosen().is_none() {
        button.set_state(ControlState::disabled());
    }
    button
}

/// The action the drawn `chooser` resolves popup-local pixel `point` to, or
/// `None` when the press is not on one.
///
/// Mirrors [`draw_open_with_chooser`]'s own band through the one private
/// action-rectangle rule they share, so a click resolves to exactly the button
/// the user pressed. An Open with no current candidate resolves to nothing
/// rather than opening whatever happens to be first (fail closed).
#[must_use]
pub fn open_with_action_at(
    chooser: &OpenWithChooser,
    viewport: Rect,
    scale: Scale,
    theme: &Theme,
    point: Point,
) -> Option<OpenWithAction> {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let rects = open_with_action_rects(viewport, scale, theme, font)?;
    OPEN_WITH_ACTIONS
        .iter()
        .zip(rects.iter())
        .find(|(_, rect)| !rect.is_empty() && rect.contains(point))
        .map(|((action, _), _)| *action)
        .filter(|action| *action != OpenWithAction::Open || chooser.chosen().is_some())
}

/// The candidate index the drawn `chooser` resolves popup-local pixel `point`
/// to, or `None` when the press is not on a candidate row — off the panel, on
/// its identity band, in the scroll gutter, on the action band, or past the
/// last row (fail closed).
///
/// It mirrors [`draw_open_with_chooser`]'s geometry through the one private
/// row layout they share, so a press resolves to exactly the row the user saw,
/// on whatever part of it shows. The index is absolute (the chooser's scroll is
/// applied), so it names a candidate rather than a position on screen.
#[must_use]
pub fn open_with_row_at(
    chooser: &OpenWithChooser,
    viewport: Rect,
    scale: Scale,
    theme: &Theme,
    point: Point,
) -> Option<usize> {
    let (rows, _) = open_with_rows(chooser, viewport, scale, theme)?;
    rows.index_at(chooser.offset(), point)
}

/// Route a pointer `event` at window-local `point` to the chooser's own
/// scrollbar, reporting `Some(repainted)` when the bar took it (so the caller
/// does not also treat the press as a click on a row) — whether that reported
/// anything — and `None` when the pointer had nothing to do with the bar.
///
/// The bar owns the interaction its press started, exactly as the listing's
/// does ([`scroll_pointer`]) and through the same shared routing, so the two
/// cannot come to behave differently.
pub fn open_with_scroll_pointer(
    chooser: &mut OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    point: Point,
    event: &InputEvent,
    damage: &mut Region,
) -> Option<bool> {
    let (rows, gutter) = open_with_rows(chooser, viewport, scale, theme)?;
    let gutter = gutter?;
    let model = rows.scroll_model(chooser.offset());
    chooser.scroll_mut().route(
        model,
        (gutter, rows.list_area()),
        scale,
        theme,
        (point, event),
        damage,
    )
}

/// Scroll the chooser's list by a wheel turn of `(dx, dy)`, in the seat's
/// scroll units, through its own bar, answering whether it moved.
///
/// The bar carries what is short of a whole pixel into the next turn. A move
/// reports the bar and the rows it slid.
pub fn open_with_scroll_wheel(
    chooser: &mut OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
    delta: (i32, i32),
    damage: &mut Region,
) -> bool {
    let Some((rows, Some(gutter))) = open_with_rows(chooser, viewport, scale, theme) else {
        return false;
    };
    let model = rows.scroll_model(chooser.offset());
    chooser
        .scroll_mut()
        .wheel(model, delta, scale, (gutter, rows.list_area()), damage)
}

/// Scroll the chooser's list the least that shows the current candidate whole,
/// answering whether it moved.
///
/// The one rule keyboard traversal reveals through, so a selection can never
/// sit outside the drawn list.
pub fn open_with_reveal(
    chooser: &mut OpenWithChooser,
    scale: Scale,
    theme: &Theme,
    viewport: Rect,
) -> bool {
    let Some((rows, _)) = open_with_rows(chooser, viewport, scale, theme) else {
        return false;
    };
    let revealed = rows.reveal(chooser.offset(), Some(chooser.selected()));
    let at = rows.scroll_model(chooser.offset()).offset();
    chooser.scroll_mut().set_offset(revealed);
    revealed != at
}
