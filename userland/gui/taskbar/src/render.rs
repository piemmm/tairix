//! Painting the taskbar's regions into a pixel [`Surface`].
//!
//! [`TaskbarRenderer::paint`] lays the taskbar's computed [`BarLayout`] and
//! live state into the caller's premultiplied-alpha [`Surface`], over the
//! rectangles of it the caller names. The
//! two permanent leading launchers are the shared `lib/controls`
//! [`IconButton`](tairix_controls::IconButton)s the model owns, painted with
//! their live hover/pressed state; every running application is one shared
//! icon-only [`TaskbarItem`](tairix_controls::TaskbarItem) slot, so the bar's
//! application buttons have exactly one visual recipe and read as one strip
//! of equal icons. Per-application artwork (rasterised by the session) is
//! blitted through the control, drawn from the bundle the kernel attested
//! opened the window, so a running application is recognised by what it
//! actually is. The one mark the bar draws for itself is the [`BarLayout::separator`] rule dividing the Library launcher
//! from everything after it, filled in [`Palette::border`].
//! The bar's own background is the shared floating-surface plate
//! ([`paint_surface_plate`]) the popups it opens already wear: a rim one
//! [`plate_border`] thick in [`Palette::rim`] at the theme's chrome weight,
//! then the raised ground inside it, both rounded by
//! [`BarLayout::corner_radius`]. That is the same radius the window manager
//! cuts the bar window to through its single anti-aliased rounded-corner
//! path, exactly as it rounds windows, so the rim follows the bar's real
//! silhouette instead of squaring off across the cut. Placing the surface
//! stays the window manager's job.
//!
//! [`TaskbarRenderer::paint_library`] paints the open program-library popup:
//! the shared [`Panel`](tairix_controls::Panel) chrome anchored back at the
//! Library button, the search field, the sidebar's folders, the content's
//! tiles and captioned rows, the scrollbar when the content overflows, the
//! details strip, and the calm placeholder when nothing is listed. It lays
//! the popup out once per paint and visits, under each rectangle, only what
//! reaches it. The theme is the taskbar's own
//! ([`Taskbar::theme`]), so the geometry the input router hit-tests and the
//! pixels painted here can never come from two different themes.
//!
//! [`TaskbarRenderer::paint_notifications`] paints the notification popover:
//! the shared `lib/controls` [`Panel`](tairix_controls::Panel) chrome anchored
//! back at the notification region, and one shared [`Notification`] card per
//! raised notification (severity mapped to the card's composed state),
//! presented by the window manager as its own small window above the bar.
//!
//! [`TaskbarRenderer::paint_tray_readout`] paints the account capsule's
//! expanded readout: the shared [`TraySignal`](tairix_controls::TraySignal)
//! draws its own elevated plate at the geometry
//! [`Taskbar::tray_readout_layout`] computed, presented by the window
//! manager as its own small window beside the capsule (the capsule itself is
//! painted on the bar by [`paint`](TaskbarRenderer::paint)).
//!
//! Every painter takes the surface's own rectangles to paint and lays the
//! whole recipe under each as a clip, so a repaint scoped to one hovered slot
//! lands exactly the pixels a whole paint would have laid there and there is
//! no second "paint just this control" recipe to disagree with the first.
//!
//! Region rectangles are in screen space; each is translated into the
//! surface's local space by subtracting the surface's origin. The translation
//! saturates and [`Surface::fill_rect`] clips, so a degenerate layout paints
//! nothing rather than panicking. A label is truncated to
//! the characters that fit its region so text never spills into a neighbour.
//! Each status-signal slot draws a scalable, themeable [`tairix_icon`] vector
//! glyph resolved from the signal's kind, rasterised to the slot size and
//! composited through `lib/raster`'s single blit path.

use tairix_controls::damage;
use tairix_controls::shell::Notification;
use tairix_controls::state::{ActivityState, ValidationState};
use tairix_controls::{
    paint_framed_surface_plate, paint_surface_plate, plate_border, plate_content_inset,
    ChromeLayer, ControlRole, ControlState, IconTile, ListRow, PointerState, ScrollOrientation,
    ScrollView, SelectionState,
};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_hash::BuildFastHash;
use tairix_icon::{IconArtwork, IconKind, IconPicture, IconRequest, IconSet};
use tairix_log::Sink;
use tairix_raster::{Color, Surface};
use tairix_reclaim::{
    disposable_ui_budget, disposable_ui_cache, CacheAccounting, PressureGauge, ReclaimCache,
};
use tairix_theme::Palette;

use crate::clock::{ClockFaces, ClockLines};
use crate::layout::{local_rect, BarLayout};
use crate::library::{
    chrome_panel, folder_label, footer_font, ItemAt, LibraryItem, LibraryLayout, LibraryPopup,
    SectionKind,
};
use crate::notifications::{NotificationArea, NotifySeverity, TransientNotification};
use crate::taskbar::Taskbar;

/// Padding in pixels between a notification slot's edge and its icon glyph.
const ICON_PADDING: u32 = 4;

/// Worst-case per-entry bookkeeping the cache charges on top of a
/// notification glyph's own pixel bytes: the LRU/index tick and
/// charged-size fields (`u64` + `usize`) plus this cache's small share of
/// its two `BTreeMap`s' node overhead. `IconKind` itself is a bare enum
/// discriminant, so the key contributes negligible bytes beyond this.
const ENTRY_METADATA_BYTES: usize = 64;

/// The epoch a cached notification glyph is valid for: the tint it is drawn
/// in, the pixel side it is rasterised to, and the generation of the active
/// [`IconSet`]. A theme change moves the tint on, a scale change moves the
/// side on, and installing a different icon set moves the generation on — any
/// of the three invalidates every cached glyph.
pub type IconEpoch = (Color, u32, u64);

/// Build the one [`ReclaimCache`] a [`TaskbarRenderer`] retains rasterised
/// notification glyphs in, classified through the shared desktop cache
/// policy (`tairix_reclaim::disposable_ui_cache`).
///
/// `seat` is the seat the renderer belongs to and `fb_bytes` is the real
/// output's backing byte size, so the cache's budget scales with the actual
/// display rather than a guessed constant; `pressure` and `sink` are the
/// process's live pressure gauge and audit sink. The embedder — the only
/// party that knows all four — calls this once and hands the result to
/// [`TaskbarRenderer::new`].
#[must_use]
pub fn icon_cache(
    seat: u64,
    fb_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
) -> ReclaimCache<IconKind, Surface, IconEpoch, BuildFastHash> {
    // Keyed by an icon kind this bar itself chose, so the fast unkeyed hash
    // is correct and is named here.
    disposable_ui_cache(
        "taskbar.icon",
        seat,
        fb_bytes,
        ENTRY_METADATA_BYTES,
        pressure,
        sink,
        BuildFastHash::new(),
    )
}

/// Everything [`draw_icon`] needs to resolve and cache a notification glyph:
/// the across-frame glyph cache, the active icon set, and the set's
/// generation (part of the cache epoch, so installing a new set invalidates
/// the cached glyphs). Bundled so the painters take one parameter rather than
/// three.
struct IconContext<'a> {
    cache: &'a mut ReclaimCache<IconKind, Surface, IconEpoch, BuildFastHash>,
    set: &'a IconSet,
    generation: u64,
}

/// Paints a taskbar into a [`Surface`], caching the rasterised notification
/// glyphs so each is converted only once per tint and size.
///
/// The renderer holds the icon cache across frames: the bar's regions, clock,
/// and task titles are cheap to repaint every frame, but the vector
/// notification glyphs are rasterised through `lib/raster` once per
/// theme/scale and reused until one changes — the SVG-first "convert once,
/// re-render only on a scale or theme change" rule, through the same bounded,
/// pressure-governed [`ReclaimCache`] the window manager uses for cursors.
/// The cache is a required constructor argument, not something this crate
/// builds itself: a cache built without a live pressure gauge would classify
/// and serve every lookup correctly while retaining nothing, which is a
/// defect that looks exactly like working software.
///
/// Neither `Clone` nor `PartialEq`/`Eq`/`Default` are derived: the cache
/// holds a pressure gauge and a diagnostics sink behind trait objects, which
/// are neither cloneable nor comparable, and cloning a live cache's charged
/// ledger would double-count its bytes.
#[derive(Debug)]
pub struct TaskbarRenderer {
    icons: ReclaimCache<IconKind, Surface, IconEpoch, BuildFastHash>,
    icon_set: IconSet,
    icon_generation: u64,
    /// The launcher's content painted once while it makes an entrance, so
    /// each frame of the entrance is a blend of this rather than a repaint.
    /// Held only while one runs, and repainted only when the content would
    /// draw differently.
    library_layer: Option<ContentLayer>,
}

/// The launcher's content as one picture, and the generation of the content
/// it was painted from.
#[derive(Debug)]
struct ContentLayer {
    generation: u64,
    surface: Surface,
}

impl TaskbarRenderer {
    /// Hold the glyph cache to what an output of `fb_bytes` allows, the
    /// output having changed size: the budget [`icon_cache`] builds it
    /// under.
    pub fn rescale_cache(&mut self, fb_bytes: usize) {
        let _ = self.icons.rebudget(disposable_ui_budget(fb_bytes));
    }

    /// A renderer drawing the built-in icon set, caching rasterised glyphs
    /// in `cache`.
    ///
    /// The desktop has a complete icon set before any on-disk SVG asset
    /// loads; [`set_icons`](Self::set_icons) swaps a loaded set in at
    /// runtime. `cache` is built by the embedder from the shared desktop
    /// cache policy ([`icon_cache`]), wired to the real display backing
    /// size, the owning seat, and the process's live pressure gauge — this
    /// renderer never invents that policy itself.
    #[must_use]
    pub const fn new(cache: ReclaimCache<IconKind, Surface, IconEpoch, BuildFastHash>) -> Self {
        Self {
            icons: cache,
            icon_set: IconSet::builtin(),
            icon_generation: 0,
            library_layer: None,
        }
    }

    /// Install a loaded notification-icon set, replacing the one in use.
    ///
    /// The set is the decoded on-disk SVG assets (`IconSet::from_assets`),
    /// each kind keeping its authored colours, with a built-in fallback for
    /// any kind the assets omit. Installing a set bumps
    /// an internal generation that is part of the glyph-cache epoch, so the
    /// next render discards the previously rasterised glyphs and re-rasterises
    /// from the new set. The generation saturates rather
    /// than wrapping.
    pub fn set_icons(&mut self, set: IconSet) {
        self.icon_set = set;
        self.icon_generation = self.icon_generation.saturating_add(1);
    }

    /// The notification-icon set currently in use (built-in until
    /// [`set_icons`](Self::set_icons) installs a loaded one).
    #[must_use]
    pub const fn icons(&self) -> &IconSet {
        &self.icon_set
    }

    /// Wipe every cached notification glyph, so no rasterised pixel data
    /// from this seat's session survives it.
    ///
    /// Called when the seat this renderer belongs to is lost or its session
    /// ends. The cache stays usable afterwards — a later render simply
    /// rebuilds what it needs — this only discards what was already
    /// rendered.
    pub fn teardown(&mut self) {
        self.icons.teardown();
    }

    /// Apply the current memory-pressure band's forced shrink to the glyph
    /// cache, returning the bytes released.
    ///
    /// The session calls this when the kernel wakes it with a deepened
    /// band, so the bar gives its rasterised pixels back at the moment
    /// pressure rises rather than at whatever later frame happens to
    /// repaint a notification. A band that demands nothing releases
    /// nothing.
    pub fn trim(&mut self) -> usize {
        self.icons.enforce_pressure()
    }

    /// Rasterised notification glyphs currently retained.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.icons.len()
    }

    /// Bytes the glyph cache currently has charged: retained pixel data
    /// plus its own per-entry bookkeeping.
    #[must_use]
    pub fn cache_bytes(&self) -> usize {
        self.icons.charged_bytes()
    }

    /// The glyph cache's byte ledger and event counters, for diagnostics.
    #[must_use]
    pub fn cache_stats(&self) -> &CacheAccounting {
        self.icons.accounting()
    }

    /// Paint `taskbar` into a [`Surface`] using its own theme's palette.
    ///
    /// `artwork` is the desktop's shipped icon lookup: the two leading
    /// launchers draw the [`Library`](IconKind::Library) and
    /// [`Folder`](IconKind::Folder) artwork, and a running application's slot
    /// draws its own artwork when it has one, its kind's shipped artwork
    /// otherwise. The trailing capsule asks the lookup for nothing while it
    /// can draw the account's own identity disc, which is what an account
    /// looks like — no shipped class picture stands in for a person. Every
    /// one of those falls back to its
    /// built-in glyph when the lookup returns `None` — the shared icon slot
    /// does that itself — so a system with no `/System/Graphics` is fully
    /// usable ([`NoArtwork`](tairix_icon::NoArtwork) resolves entirely to
    /// glyphs).
    ///
    /// The bar's background is the shared floating-surface plate
    /// ([`paint_surface_plate`]): a rim one [`plate_border`] thick in the
    /// palette's `rim`, then the raised ground inside it, both at the
    /// palette's `chrome_alpha` and both rounded by
    /// [`BarLayout::corner_radius`]. The rim is the bar's own edge rather than
    /// a mark on it, so it takes the same weight as the ground and reads a
    /// step lighter than it on a dark theme, a step darker on a light one.
    /// Those two are the bar's translucent layers — the wallpaper and the
    /// windows behind read through them over the backdrop the compositor
    /// blurs. A slot's hover or press wash is a plate laid on them, a step
    /// more solid, and the ink on top (the glyphs, the clock, the separator
    /// rule, every seam and bead) is drawn solid so it reads against whatever
    /// wallpaper is behind.
    ///
    /// `rects` are the surface's own rectangles to paint, each drawn under its
    /// own clip through the one shared recipe, so a bar repainted for one
    /// hovered slot lands exactly the pixels a whole paint would have there.
    /// A whole paint is the surface's own rectangle.
    pub fn paint(
        &mut self,
        taskbar: &Taskbar,
        scale: Scale,
        artwork: &mut dyn IconArtwork,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        damage::paint_parts(surface, rects, |surface| {
            self.paint_bar(taskbar, scale, artwork, surface);
        });
    }

    /// The bar's whole recipe, laid across the surface in its own pixels.
    ///
    /// The regions are ordinary seated controls that know nothing of the bar's
    /// rounded ends, so the bar is laid as a framed plate: its rim goes down
    /// last and confines whatever a slot hard against an end drew to the pill.
    fn paint_bar(
        &mut self,
        taskbar: &Taskbar,
        scale: Scale,
        artwork: &mut dyn IconArtwork,
        surface: &mut Surface,
    ) {
        let theme = taskbar.theme();
        let layout = taskbar.layout(scale);
        let mut icons = IconContext {
            cache: &mut self.icons,
            set: &self.icon_set,
            generation: self.icon_generation,
        };
        let origin = layout.bar.origin;
        let clock_faces = ClockFaces::of(theme, scale);

        // The bar wears the same plate as the popups it opens; its regions are
        // laid out in screen space, so the interior the plate reports is moot.
        paint_framed_surface_plate(
            surface,
            (0, 0, layout.bar.width, layout.bar.height),
            (layout.corner_radius, plate_border(theme, scale)),
            theme,
            (theme.palette().surface_raised, ChromeLayer::Ground),
            |surface, _| {
                if !layout.library.is_empty() {
                    let button = taskbar.library_button();
                    let bounds = local_rect(layout.library, origin);
                    let side = button.icon_side(bounds, scale, theme);
                    let art = artwork.artwork(IconRequest::kind(IconKind::Library), side);
                    button.render(surface, bounds, scale, theme, art);
                }

                surface.fill_rect(
                    local(layout.separator.left(), origin.x),
                    local(layout.separator.top(), origin.y),
                    layout.separator.width,
                    layout.separator.height,
                    theme.palette().border.into(),
                );

                let strip = taskbar.apps();
                for (index, slot) in layout.apps.iter().enumerate() {
                    if slot.is_empty() {
                        continue;
                    }
                    let (Some(item), Some(app)) = (strip.item(index), strip.get(index)) else {
                        continue;
                    };
                    // The slot wears its own application's icon, resolved by
                    // the session from the bundle the kernel attested owns the
                    // process, so an application is recognised by its own
                    // picture.
                    let bounds = local_rect(*slot, origin);
                    let side = item.icon_side(bounds, scale, theme);
                    let art = slot_artwork(app.artwork(), app.icon(), side, artwork);
                    item.render(surface, bounds, scale, theme, art);
                }

                paint_trailing(
                    surface,
                    &layout,
                    taskbar.notifications(),
                    (taskbar.clock().lines(), clock_faces),
                    theme.palette(),
                    &mut icons,
                );

                if !layout.switchboard.is_empty() {
                    let tray = taskbar.tray();
                    let signal = tray.signal();
                    let bounds = local_rect(layout.switchboard, origin);
                    let side = signal.icon_side(bounds, scale, theme);
                    // The capsule wears the account, not a class glyph, so its
                    // own disc outranks anything the shipped-artwork lookup
                    // holds.
                    let disc = tray.identity_disc(side, scale, theme);
                    let art = slot_artwork(disc.as_ref(), signal.icon(), side, artwork);
                    signal.render(surface, bounds, scale, theme, art);
                }
            },
        );
    }

    /// Paint the open window picker into a [`Surface`] using the taskbar's
    /// own theme.
    ///
    /// Each cell draws the thumbnail the embedder supplied for that window,
    /// falling back to the application's own glyph when it has none, so a
    /// cell always states something; a grid with more rows than the panel
    /// shows draws the scrollbar that reaches the rest. `rects` are the
    /// surface's own rectangles to paint; a closed picker paints nothing.
    pub fn paint_picker(
        &self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        damage::paint_parts(surface, rects, |surface| {
            paint_picker_panel(taskbar, scale, surface);
        });
    }

    /// Paint the open program-library popup into a [`Surface`] using the
    /// taskbar's own theme.
    ///
    /// `rects` are the surface's own rectangles to paint; a closed popup
    /// touches none of them, so a window still leaving the screen keeps what
    /// it showed. The window manager places the surface above the bar and
    /// rounds it with
    /// [`LibraryLayout::corner_radius`](crate::library::LibraryLayout::corner_radius),
    /// exactly as it rounds the bar.
    pub fn paint_library(
        &mut self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        let popup = taskbar.library();
        if popup.entrance_frame(0).is_none() {
            self.library_layer = None;
        }
        // A window still leaving the screen keeps the pixels it had.
        if !popup.is_open() {
            return;
        }
        let layout = taskbar.library_layout(scale);
        let paint = LibraryPaint {
            popup,
            chrome: chrome_panel(local_point(layout.anchor, layout.panel.origin)),
            layout,
            scale,
            theme: taskbar.theme(),
        };
        let layer = &mut self.library_layer;
        damage::paint_parts_within(surface, rects, |surface, part| {
            paint_library_panel(&paint, surface, part, layer);
        });
    }

    /// Paint the notification popover into a [`Surface`] using the taskbar's
    /// own theme.
    ///
    /// `rects` are the surface's own rectangles to paint; a popover with no
    /// notification raised paints nothing. The window manager places the
    /// surface above the bar and rounds it with
    /// [`NotificationsLayout::corner_radius`](crate::NotificationsLayout::corner_radius),
    /// exactly as it rounds the bar and the library popup. Each card is the
    /// shared [`Notification`] control, so the popover restates no chrome and
    /// no colour algebra of its own.
    pub fn paint_notifications(
        &self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        damage::paint_parts(surface, rects, |surface| {
            paint_notifications_panel(taskbar, scale, surface);
        });
    }

    /// Paint the Switchboard capsule's expanded readout into a [`Surface`]
    /// using the taskbar's own theme.
    ///
    /// `rects` are the surface's own rectangles to paint; a collapsed readout
    /// paints nothing. The window manager places the surface beside the
    /// capsule and rounds it with
    /// [`ReadoutLayout::corner_radius`](crate::ReadoutLayout::corner_radius)
    /// — the same popup radius the readout plate draws for itself, kept in
    /// step by construction.
    pub fn paint_tray_readout(
        &self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        damage::paint_parts(surface, rects, |surface| {
            paint_readout_panel(taskbar, scale, surface);
        });
    }

    /// Paint the open network readout into a [`Surface`] using the taskbar's
    /// own theme. The window manager places it outward from the network signal
    /// and rounds it with
    /// [`ReadoutLayout::corner_radius`](crate::ReadoutLayout::corner_radius).
    pub fn paint_network_readout(
        &self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        damage::paint_parts(surface, rects, |surface| {
            paint_network_panel(taskbar, scale, surface);
        });
    }

    /// Paint the open volume panel into a [`Surface`] using the taskbar's own
    /// theme. The window manager places it beside the volume signal and
    /// rounds it with
    /// [`SoundPanelLayout::corner_radius`](crate::SoundPanelLayout::corner_radius).
    pub fn paint_sound(
        &self,
        taskbar: &Taskbar,
        scale: Scale,
        surface: &mut Surface,
        rects: &[Rect],
    ) {
        let Some(layout) = taskbar.sound_layout(scale) else {
            return;
        };
        damage::paint_parts(surface, rects, |surface| {
            taskbar
                .sound()
                .paint(&layout, scale, taskbar.theme(), surface);
        });
    }
}

/// The network readout's whole recipe: the floating plate, then each
/// interface's facts inside the control inset.
fn paint_network_panel(taskbar: &Taskbar, scale: Scale, surface: &mut Surface) {
    let Some(layout) = taskbar.network_readout_layout(scale) else {
        return;
    };
    let theme = taskbar.theme();
    let (width, height) = (layout.panel.width, layout.panel.height);
    if paint_surface_plate(
        surface,
        (0, 0, width, height),
        (layout.corner_radius, plate_border(theme, scale)),
        theme,
        (theme.palette().surface_raised, ChromeLayer::Ground),
    )
    .is_none()
    {
        return;
    }
    let content = Rect::new(0, 0, width, height).inset(plate_content_inset(theme, scale));
    taskbar
        .network_facts()
        .render(surface, content, scale, theme);
}

/// The picker's whole recipe, laid across the surface in its own pixels.
fn paint_picker_panel(taskbar: &Taskbar, scale: Scale, surface: &mut Surface) {
    let Some(layout) = taskbar.picker_layout(scale) else {
        return;
    };
    let theme = taskbar.theme();
    let origin = layout.panel.origin;
    let _ = paint_surface_plate(
        surface,
        (0, 0, layout.panel.width, layout.panel.height),
        (layout.corner_radius, plate_border(theme, scale)),
        theme,
        (theme.palette().surface_raised, ChromeLayer::Ground),
    );
    let picker = taskbar.picker();
    let grid = ScrollView::new(
        ScrollOrientation::Vertical,
        local_rect(layout.viewport, origin),
        u64::from(layout.offset),
    );
    grid.paint(surface, |surface| {
        for (index, cell) in layout.cells.iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            let (Some(preview), Some(entry)) = (picker.preview(index), picker.entries().get(index))
            else {
                continue;
            };
            // The picker holds no icon cache of its own, and this fallback is
            // the transient case of a window whose first frame has not
            // arrived: a popup that opens on a gesture, so the glyph is drawn
            // inline here.
            preview.render(
                surface,
                local_rect(*cell, origin),
                scale,
                theme,
                entry.thumbnail(),
                None,
            );
        }
    });
    if let Some(scrollbar) = layout.scrollbar {
        picker
            .scrollbar()
            .render(surface, local_rect(scrollbar, origin), scale, theme);
    }
}

/// What one paint of the launcher draws from, derived once for every
/// rectangle it repaints.
struct LibraryPaint<'a> {
    popup: &'a LibraryPopup,
    layout: LibraryLayout,
    chrome: tairix_controls::Panel,
    scale: Scale,
    theme: &'a tairix_theme::Theme,
}

/// The open launcher's whole recipe under the surface-local rectangle `part`,
/// each piece drawn only where it reaches it.
fn paint_library_panel(
    paint: &LibraryPaint<'_>,
    surface: &mut Surface,
    part: Rect,
    layer: &mut Option<ContentLayer>,
) {
    let LibraryPaint {
        popup,
        ref layout,
        ref chrome,
        scale,
        theme,
    } = *paint;
    let origin = layout.panel.origin;
    let reaches = |rect: Rect| !local_rect(rect, origin).intersection(&part).is_empty();
    let hairline = Color::from(theme.palette().border);

    let local_bounds = Rect::new(0, 0, layout.panel.width, layout.panel.height);
    chrome.render(surface, local_bounds, scale, theme);
    if reaches(layout.search) {
        popup
            .search_field()
            .render(surface, local_rect(layout.search, origin), scale, theme);
    }
    if let Some(sidebar) = layout.sidebar.filter(|&sidebar| reaches(sidebar)) {
        popup.sidebar().render(
            surface,
            local_rect(sidebar, origin),
            scale,
            theme,
            &mut tairix_icon::NoArtwork,
        );
    }
    if reaches(layout.divider) {
        fill_local(surface, local_rect(layout.divider, origin), hairline);
    }

    if reaches(layout.viewport) {
        let within = unscrolled(part, origin, layout);
        if popup.entrance_frame(0).is_some() {
            paint_entering(
                surface,
                origin,
                popup,
                layout,
                (scale, theme),
                layer,
                within,
            );
        } else {
            paint_content(surface, origin, popup, layout, (scale, theme), within);
        }
        if let Some(placeholder) = popup.placeholder() {
            draw_label(
                surface,
                origin,
                layout.viewport,
                &placeholder,
                theme.palette().on_surface_muted.into(),
                BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, scale),
            );
        }
    }
    if let Some(scrollbar) = layout.scrollbar.filter(|&scrollbar| reaches(scrollbar)) {
        popup
            .scrollbar_for(layout)
            .render(surface, local_rect(scrollbar, origin), scale, theme);
    }

    if reaches(layout.footer_rule) {
        fill_local(surface, local_rect(layout.footer_rule, origin), hairline);
    }
    if reaches(layout.footer) {
        let font = footer_font(theme, scale);
        let footer = local_rect(layout.footer, origin);
        let details = popup.details(layout);
        let text = font.elide_to_width(&details, footer.width);
        let y = font.centred_top(
            footer
                .top()
                .saturating_add(to_i32(layout.footer_rule.height)),
            footer.height.saturating_sub(layout.footer_rule.height),
        );
        tairix_controls::paint_run(
            surface,
            font,
            text,
            (footer.left(), y),
            Color::from(theme.palette().on_surface_muted),
            None,
        );
    }
}

/// The surface-local rectangle `part` of a surface standing at screen
/// `origin`, in `layout`'s unscrolled content space.
fn unscrolled(part: Rect, origin: Point, layout: &LibraryLayout) -> Rect {
    Rect::new(
        part.left().saturating_add(origin.x),
        part.top()
            .saturating_add(origin.y)
            .saturating_add(to_i32(layout.offset)),
        part.width,
        part.height,
    )
}

/// Fill a surface-local rectangle, skipping one that starts off the surface.
fn fill_local(surface: &mut Surface, rect: Rect, color: Color) {
    if let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) {
        surface.fill_rect(x, y, rect.width, rect.height, color);
    }
}

/// Paint the launcher's content — headings, tiles, rows — that reaches
/// `within`, in unscrolled content space, through its scrolled viewport, the
/// surface standing at screen `origin`.
fn paint_content(
    surface: &mut Surface,
    origin: Point,
    popup: &LibraryPopup,
    layout: &LibraryLayout,
    (scale, theme): (Scale, &tairix_theme::Theme),
    within: Rect,
) {
    let view = ScrollView::new(
        ScrollOrientation::Vertical,
        local_rect(layout.viewport, origin),
        u64::from(layout.offset),
    );
    let reaches = |rect: Rect| !rect.intersection(&within).is_empty();
    view.paint(surface, |surface| {
        for section in layout
            .sections
            .iter()
            .filter(|section| reaches(section.heading))
        {
            tairix_controls::block::title(
                surface,
                local_rect(section.heading, origin),
                scale,
                theme,
                &section.kind.heading(),
            );
        }
        for (at, rect) in layout.shown().filter(|&(_, rect)| reaches(rect)) {
            let (Some(section), Some(program)) = (layout.sections.get(at.section), popup.item(at))
            else {
                continue;
            };
            paint_program(
                surface,
                local_rect(rect, origin),
                popup,
                layout,
                (at, section.kind, program),
                (scale, theme),
            );
        }
    });
}

/// Paint one program as its section shows it: a tile, or a captioned row.
fn paint_program(
    surface: &mut Surface,
    bounds: Rect,
    popup: &LibraryPopup,
    layout: &LibraryLayout,
    (at, kind, program): (ItemAt, SectionKind, &LibraryItem),
    (scale, theme): (Scale, &tairix_theme::Theme),
) {
    let mut state = ControlState::idle();
    if popup.pressed() == Some(at) {
        state = state.with_pointer(PointerState::Pressed);
    } else if popup.hover() == Some(at) {
        state = state.with_pointer(PointerState::Hover);
    }
    let artwork = popup.artwork(&program.id).map(IconPicture::coloured);
    let running = popup.is_running(&program.id);
    if kind.is_list() {
        let caption = popup
            .facts_of(&program.id)
            .and_then(|facts| facts.purpose.clone())
            .or_else(|| {
                popup
                    .catalog()
                    .entry(&program.id)
                    .map(|entry| alloc::string::String::from(folder_label(entry.category())))
            })
            .unwrap_or_default();
        let mut row = ListRow::new(program.name.clone())
            .with_icon(IconKind::AppBundle)
            .with_caption(caption)
            .with_emphasis(program.emphasis.clone())
            .with_state(state);
        if running {
            row = row.with_trailing("Running");
        }
        row.set_selected(popup.selection() == Some(at));
        row.render(surface, bounds, scale, theme, artwork);
        return;
    }
    if popup.selection() == Some(at) {
        state = state.with_selection(SelectionState::Selected);
    }
    let mut tile = IconTile::new(program.name.clone(), IconKind::AppBundle)
        .with_layout(layout.tile)
        .with_state(state)
        .with_live_mark(running);
    if let Some(strength) = popup.mark_strength(kind, &program.id) {
        tile = tile.with_selection_fade(strength);
    }
    tile.render(surface, bounds, scale, theme, artwork);
}

/// Paint a frame of the content's entrance under `within`, in unscrolled
/// content space: the content once into `layer` when it would draw
/// differently from what the layer holds, then each program and heading
/// reaching `within` blended from it at its own strength and rise.
///
/// A layer the heap refuses is no reason to show nothing: the content is
/// painted in place, at rest.
fn paint_entering(
    surface: &mut Surface,
    origin: Point,
    popup: &LibraryPopup,
    layout: &LibraryLayout,
    looks: (Scale, &tairix_theme::Theme),
    layer: &mut Option<ContentLayer>,
    within: Rect,
) {
    let viewport = layout.viewport;
    let size = (viewport.width, viewport.height);
    let fresh = layer
        .as_ref()
        .is_some_and(|held| held.generation == popup.generation());
    if !fresh {
        let whole = unscrolled(
            Rect::new(0, 0, viewport.width, viewport.height),
            viewport.origin,
            layout,
        );
        // The held surface is drawn over again while it fits, so a cascade the
        // pointer moves through allocates nothing frame to frame.
        let reused = layer
            .take()
            .filter(|held| (held.surface.width(), held.surface.height()) == size)
            .map(|held| held.surface)
            .map(|mut surface| {
                surface.fill(Color::TRANSPARENT);
                surface
            });
        *layer = reused
            .or_else(|| Surface::new(size.0, size.1))
            .map(|mut drawn| {
                paint_content(&mut drawn, viewport.origin, popup, layout, looks, whole);
                ContentLayer {
                    generation: popup.generation(),
                    surface: drawn,
                }
            });
    }
    let Some(held) = layer.as_ref() else {
        paint_content(surface, origin, popup, layout, looks, within);
        return;
    };
    let shown = local_rect(viewport, origin);
    let view = layout.view();
    let blend = |surface: &mut Surface, rect: Rect, frame: crate::library::ItemFrame| {
        // Each piece is drawn where its frame stands, which a rise can carry
        // into a part its resting place does not reach.
        let drawn = Rect::new(
            rect.left(),
            rect.top().saturating_add(frame.rise),
            rect.width,
            rect.height,
        );
        if drawn.intersection(&within).is_empty() {
            return;
        }
        let Some(on_screen) = view.to_window(rect) else {
            return;
        };
        let at = local_rect(on_screen, origin);
        let moved = Rect::new(
            at.left(),
            at.top().saturating_add(frame.rise),
            at.width,
            at.height,
        )
        .intersection(&shown);
        let (Ok(x), Ok(y)) = (u32::try_from(moved.left()), u32::try_from(moved.top())) else {
            return;
        };
        surface.with_clip(x, y, moved.width, moved.height, |clipped| {
            clipped.blit_faded(
                shown.left(),
                shown.top().saturating_add(frame.rise),
                &held.surface,
                frame.strength,
            );
        });
    };
    let frame_of = |order: usize| {
        popup
            .entrance_frame(order)
            .unwrap_or(crate::library::ItemFrame::ARRIVED)
    };
    // Every program takes the next step of the cascade in reading order, and a
    // heading arrives with the first program under it.
    let lead = |section: usize| -> usize {
        layout
            .sections
            .iter()
            .take(section)
            .map(|laid| laid.count)
            .sum()
    };
    for (index, section) in layout.sections.iter().enumerate() {
        blend(surface, section.heading, frame_of(lead(index)));
    }
    for (at, rect) in layout.shown() {
        blend(
            surface,
            rect,
            frame_of(lead(at.section).saturating_add(at.item)),
        );
    }
}

/// The popover's whole recipe, laid across the surface in its own pixels.
fn paint_notifications_panel(taskbar: &Taskbar, scale: Scale, surface: &mut Surface) {
    let Some(layout) = taskbar.notifications_layout(scale) else {
        return;
    };
    let theme = taskbar.theme();
    let origin = layout.panel.origin;
    let local_bounds = Rect::new(0, 0, layout.panel.width, layout.panel.height);

    crate::layout::notif_panel(local_point(layout.anchor, origin)).render(
        surface,
        local_bounds,
        scale,
        theme,
    );

    let notifications = taskbar.notifications();
    for placed in &layout.cards {
        let Some(note) = notifications.notification(placed.index) else {
            continue;
        };
        notification_card(note).render(surface, local_rect(placed.card, origin), scale, theme);
    }
}

/// The readout's whole recipe, laid across the surface in its own pixels.
fn paint_readout_panel(taskbar: &Taskbar, scale: Scale, surface: &mut Surface) {
    let Some(layout) = taskbar.tray_readout_layout(scale) else {
        return;
    };
    let theme = taskbar.theme();
    let local = Rect::new(0, 0, layout.panel.width, layout.panel.height);
    taskbar
        .tray()
        .signal()
        .render_readout(surface, local, scale, theme);
}

/// The shared notification card for `note`, its role and composed state mapping
/// the notification's severity to the Reactive Alloy semantics: an
/// informational notice stays calm, a success shows the completion accent, a
/// warning shows the caution rail, and a critical notification reads as a
/// destructive, invalid state. Built once here so the popover render (and any
/// later hit-test) compose the identical control.
pub(crate) fn notification_card(note: &TransientNotification) -> Notification {
    let (role, state) = match note.severity {
        NotifySeverity::Info => (ControlRole::Neutral, ControlState::idle()),
        NotifySeverity::Success => (
            ControlRole::Recommended,
            ControlState::idle().with_activity(ActivityState::Complete),
        ),
        NotifySeverity::Warning => (
            ControlRole::Neutral,
            ControlState::idle().with_validation(ValidationState::Warning),
        ),
        NotifySeverity::Critical => (
            ControlRole::Destructive,
            ControlState::idle().with_validation(ValidationState::Invalid),
        ),
    };
    let card = Notification::new(note.title.clone())
        .with_role(role)
        .with_state(state);
    if note.body.is_empty() {
        card
    } else {
        card.with_message(note.body.clone())
    }
}

/// Fill the notification and clock regions of an already-created bar
/// surface.
fn paint_trailing(
    surface: &mut Surface,
    layout: &BarLayout,
    notifications: &NotificationArea,
    (clock, faces): (&ClockLines, ClockFaces),
    palette: &Palette,
    icons: &mut IconContext<'_>,
) {
    let origin = layout.bar.origin;
    for (slot, signal) in layout.notifications.iter().zip(notifications.signals()) {
        draw_icon(
            surface,
            origin,
            *slot,
            signal.kind.icon(),
            palette.on_surface_muted.into(),
            icons,
        );
    }

    paint_clock(
        surface,
        origin,
        layout.clock,
        clock,
        faces,
        palette.on_surface.into(),
    );
}

/// Draw the clock's lines in its screen-space `rect`: the date beneath the
/// time where both lines fit, beside it where they do not.
///
/// A time or date cut short reads as a different one, so a line the region
/// cannot hold whole is not drawn. The time comes first: the date is drawn
/// only with it, and the time alone where the date does not fit.
fn paint_clock(
    surface: &mut Surface,
    origin: Point,
    rect: Rect,
    lines: &ClockLines,
    faces: ClockFaces,
    ink: Color,
) {
    let whole =
        |font: BitmapFont, text: &str| !text.is_empty() && font.text_width(text) <= rect.width;
    if !whole(faces.time, &lines.time) {
        return;
    }
    let top = rect.top().saturating_sub(origin.y);
    let line_at = |font: BitmapFont, text: &str, y: i32, surface: &mut Surface| {
        let x_offset = rect.width.saturating_sub(font.text_width(text)) / 2;
        let x = rect
            .left()
            .saturating_sub(origin.x)
            .saturating_add(to_i32(x_offset));
        font.draw_text(surface, x, y, text, ink);
    };
    if whole(faces.date, &lines.date) {
        if faces.stacks(rect.height) {
            let time_height = faces.time.line_height();
            let block = time_height.saturating_add(faces.date.line_height());
            let block_top = top.saturating_add(to_i32(rect.height.saturating_sub(block) / 2));
            line_at(faces.time, &lines.time, block_top, surface);
            line_at(
                faces.date,
                &lines.date,
                block_top.saturating_add(to_i32(time_height)),
                surface,
            );
            return;
        }
        let time_width = faces.time.text_width(&lines.time);
        let pair = time_width
            .saturating_add(faces.gap)
            .saturating_add(faces.date.text_width(&lines.date));
        if pair <= rect.width {
            // Side by side on one baseline, the pair centred.
            let x = rect
                .left()
                .saturating_sub(origin.x)
                .saturating_add(to_i32(rect.width.saturating_sub(pair) / 2));
            let time_top = faces.time.centred_top(top, rect.height);
            let baseline = time_top.saturating_add(to_i32(faces.time.baseline()));
            faces.time.draw_text(surface, x, time_top, &lines.time, ink);
            let date_x = x.saturating_add(to_i32(time_width.saturating_add(faces.gap)));
            let date_top = baseline.saturating_sub(to_i32(faces.date.baseline()));
            faces
                .date
                .draw_text(surface, date_x, date_top, &lines.date, ink);
            return;
        }
    }
    draw_label(surface, origin, rect, &lines.time, ink, faces.time);
}

/// Draw `text` centred inside the screen-space `rect`, clipped to it. Text
/// wider than the region is truncated to the characters that fit.
fn draw_label(
    surface: &mut Surface,
    origin: Point,
    rect: Rect,
    text: &str,
    color: Color,
    font: BitmapFont,
) {
    if rect.is_empty() || text.is_empty() {
        return;
    }
    let fitted = font.truncate_to_width(text, rect.width);
    if fitted.is_empty() {
        return;
    }

    let text_width = font.text_width(fitted);
    let x_offset = rect.width.saturating_sub(text_width) / 2;
    let x = rect
        .left()
        .saturating_sub(origin.x)
        .saturating_add(to_i32(x_offset));
    let y = font.centred_top(rect.top().saturating_sub(origin.y), rect.height);
    font.draw_text(surface, x, y, fitted, color);
}

/// Draw a status signal's glyph centred in its screen-space `rect`, tinted
/// with `color`. The glyph is the scalable [`tairix_icon`] vector icon for
/// `kind`, rasterised to the slot size at this scale from the loaded
/// `/System/Graphics` icon set (with the built-in fallback), then composited
/// onto the bar-local surface through the shared blit path. The rasterised
/// glyph is taken from `icons`, which keeps it across frames so it is
/// converted only once per tint and size and re-rendered only on a theme or
/// scale change. An empty slot, a slot too small to hold a glyph, or an
/// unrenderable size paints nothing rather than panicking.
fn draw_icon(
    surface: &mut Surface,
    origin: Point,
    rect: Rect,
    kind: IconKind,
    color: Color,
    icons: &mut IconContext<'_>,
) {
    if rect.is_empty() {
        return;
    }
    let side = rect
        .width
        .min(rect.height)
        .saturating_sub(ICON_PADDING.saturating_mul(2));
    let set = icons.set;
    let epoch = (color, side, icons.generation);
    let Some(image) = icons
        .cache
        .get_or_build(&epoch, kind, || set.icon(kind, color).rasterise(side))
    else {
        return;
    };
    let x_offset = rect.width.saturating_sub(side) / 2;
    let y_offset = rect.height.saturating_sub(side) / 2;
    let x = to_i32(local(rect.left(), origin.x).saturating_add(x_offset));
    let y = to_i32(local(rect.top(), origin.y).saturating_add(y_offset));
    surface.blit(x, y, &image);
}

/// Resolve the artwork a slot draws: the picture the slot carries of its
/// *own* when it has one, else its kind's shipped artwork at `side`, else
/// `None` for the control's built-in glyph.
///
/// One order for both slots that own a picture — a running application's
/// bundle icon and the trailing capsule's account disc — so neither can be
/// resolved in a different order from the other. The final glyph fallback is
/// the shared icon slot's own, so `None` here is not a blank slot.
fn slot_artwork<'a>(
    own: Option<&'a Surface>,
    kind: IconKind,
    side: u32,
    artwork: &'a mut dyn IconArtwork,
) -> Option<IconPicture<'a>> {
    match own {
        Some(surface) => Some(IconPicture::coloured(surface)),
        None => artwork.artwork(IconRequest::kind(kind), side),
    }
}

/// Translate a screen-space point into surface-local space.
fn local_point(point: Point, origin: Point) -> Point {
    Point::new(
        point.x.saturating_sub(origin.x),
        point.y.saturating_sub(origin.y),
    )
}

/// Translate a screen-space coordinate into bar-local space, clamping a
/// coordinate that would fall before the bar's origin to zero.
fn local(coord: i32, origin: i32) -> u32 {
    u32::try_from(i64::from(coord) - i64::from(origin)).unwrap_or(0)
}
