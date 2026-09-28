//! The tab strip: [`Tab`] and [`Tabs`] (spec §11.12).
//!
//! A tab strip selects one of several views. In its default
//! [`TabsOrientation::Horizontal`] it is a row of equal-width items sharing
//! the strip's width, each carrying a strong lower seam when selected and
//! reading on the content surface. Laid out [`TabsOrientation::Vertical`] it
//! is a *sidebar list*: items stacked top-down, each at its own content
//! height, carrying a strong leading seam and a quiet selected plate.
//!
//! A vertical item is a list entry rather than a tab shape, so it takes the
//! anatomy a sidebar needs: an optional leading [`icon`](Tab::with_icon), its
//! label, an optional live [`reading`](Tab::with_reading) trailing on that
//! same line, an optional [`disclosure`](Tab::with_disclosure) chevron
//! trailing it, and an optional bounded [`trend`](Tab::with_trend) beneath —
//! which makes the strip a live summary of everything it selects between.
//! Items may be grouped: [`with_group`](Tab::with_group) puts a quiet heading
//! above the item that starts a group,
//! [`with_group_break`](Tab::with_group_break) sets it apart by a blank band
//! instead, and [`nested`](Tab::nested) indents an entry that is a page of the
//! disclosing entry above it, so one cursor walks a two-level list as a single
//! column. A vertical strip stacks rather than splits, every entry at its
//! natural height, and states the height it wants
//! ([`Tabs::measured_height`]); a list longer than its box is its owner's to
//! scroll through a [`ScrollView`](crate::ScrollView), never one the strip
//! squeezes or truncates.
//!
//! A horizontal strip has one row and no room for either, so it draws neither;
//! a reading belongs in its label there (see [`Tab::set_label`]).
//!
//! A loading tab shows a Heat Seam on the same edge as the selection seam; a
//! modified tab shows a small Signal Bead; and an error tab shows a warning or
//! recovery bead so its state is legible without colour (spec §11.12, §15).
//! One definition drives both orientations: the strip owns keyboard navigation
//! (Left/Right move the current tab in a horizontal strip, Up/Down in a
//! vertical one, Home/End jump to the ends in either, Enter/Space select it)
//! and pointer hover/click, emitting a typed [`TabsAction`]; it enforces no
//! authority. A vertical strip also answers the tree keys a disclosing entry
//! needs: Right shows its pages or steps onto the first, Left hides them or
//! climbs from a page back to its entry. Every colour, metric, and radius
//! resolves from the active [`Theme`] and [`Scale`].

use alloc::string::String;
use alloc::vec::Vec;

use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{Rgba, TextRole, Theme};

use crate::chart::Chart;
use crate::damage;
use crate::disclosure::{tree_step, TreeKey, TreeRow, TreeStep};
use crate::paint::{
    draw_outline, ground_fill, heavy_contrast, icon_slot_side, line_budget, paint_bead,
    paint_chevron, paint_icon_slot, paint_run, plate_border, rail_thickness, role_font, run_width,
    seam_thickness, seam_width, surface_rect, text_plate_height, to_i32, withheld, BeadShape,
    ChevronDir, ChromeLayer, TextBlock, FULL_COLOUR,
};
use crate::state::{
    ActivityState, ControlDisposition, ControlState, RenderInvariant, SelectionState,
    ValidationState,
};

/// The outcome of feeding input to a [`Tabs`] strip.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TabsAction {
    /// The tab at `index` was chosen and its view should become active.
    Selected {
        /// The zero-based index of the chosen tab.
        index: usize,
    },
    /// The reader asked for the pages of the disclosing entry at `index` to
    /// be shown or hidden.
    ///
    /// The strip states a posture and holds none, so the owner applies this
    /// to its own model and restates the strip.
    Disclose {
        /// The zero-based index of the disclosing entry.
        index: usize,
        /// Whether its pages should be shown.
        open: bool,
    },
}

/// Which axis a [`Tabs`] strip stacks its items along (spec §11.12).
///
/// [`Tabs::new`] always produces [`TabsOrientation::Horizontal`]; a sidebar
/// selects [`TabsOrientation::Vertical`] with [`Tabs::with_orientation`]. Both
/// share the one selection, hit-testing, keyboard, and action model — the axis
/// items stack along, which edge carries the selection seam, and whether an
/// item is a tab shape or a sidebar list entry differ.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TabsOrientation {
    /// A strip across the top: equal-width tabs, each carrying a strong lower
    /// seam when selected.
    Horizontal,
    /// A column down the side: a sidebar list, each entry at its own content
    /// height, carrying a strong leading seam and a quiet selected plate.
    Vertical,
}

/// A group of a vertical [`Tabs`] strip that has no entries, and why.
///
/// A group heading is drawn by the item that *starts* its group, so a group
/// with nothing in it has nothing to hang a heading on and simply vanishes —
/// leaving a reader unable to tell "this machine has no such device" from
/// "this session was refused the inventory". This states the difference: the
/// heading, and one line under it saying why the group is empty. It selects
/// nothing, takes no keyboard cursor, and does not shift any item's index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabGroupAbsence {
    /// The heading the empty group would have carried.
    heading: String,
    /// One line saying why it is empty.
    statement: String,
    /// The item index this group would have started at, so the strip draws
    /// the empty group in its own rail position rather than at the end.
    before: usize,
}

impl TabGroupAbsence {
    /// A group `heading` with `statement` under it, positioned where the
    /// group's first item *would* have been — the index of the first item
    /// that follows it, or the item count to place it last.
    #[must_use]
    pub fn new(heading: impl Into<String>, statement: impl Into<String>, before: usize) -> Self {
        Self {
            heading: heading.into(),
            statement: statement.into(),
            before,
        }
    }

    /// The heading.
    #[must_use]
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// The line stating why the group is empty.
    #[must_use]
    pub fn statement(&self) -> &str {
        &self.statement
    }

    /// The item index this empty group is drawn before.
    #[must_use]
    pub const fn before(&self) -> usize {
        self.before
    }
}

/// One tab in a [`Tabs`] strip (spec §11.12).
///
/// A tab's selected/loading/error state is read from its composed
/// [`ControlState`] (selection, activity, validation); the modified flag is a
/// small explicit marker for unsaved work. The tab renders state and never
/// dispatches — selection commits through the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    label: String,
    icon: Option<IconKind>,
    reading: Option<String>,
    trend: Option<Chart>,
    group: Option<String>,
    /// Set when this entry starts a group set apart by a blank band.
    group_break: bool,
    /// Set when this entry discloses pages of its own, and whether they are
    /// currently shown.
    disclosure: Option<bool>,
    /// Set when this entry is a page of the disclosing entry above it.
    nested: bool,
    modified: bool,
    state: ControlState,
}

impl Tab {
    /// A neutral, enabled tab with the given label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            reading: None,
            trend: None,
            group: None,
            group_break: false,
            disclosure: None,
            nested: false,
            modified: false,
            state: ControlState::idle(),
        }
    }

    /// This tab flagged as having unsaved modifications (draws a Signal Bead).
    #[must_use]
    pub fn with_modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    /// This tab with the given composed state (selection/activity/validation).
    #[must_use]
    pub fn with_state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    /// This entry with a leading glyph, drawn before its label — sidebar
    /// anatomy, so a horizontal strip draws it nowhere.
    ///
    /// The picture comes from the owner's artwork lookup at
    /// [`Tabs::render`]'s own slot side, so a strip of glyphs costs a cache
    /// lookup per entry rather than re-resolving vector coverage each frame.
    #[must_use]
    pub fn with_icon(mut self, icon: IconKind) -> Self {
        self.icon = Some(icon);
        self
    }

    /// This entry as one that discloses pages of its own, `expanded` saying
    /// whether they are shown — sidebar anatomy, so a horizontal strip draws
    /// it nowhere.
    ///
    /// The chevron states the entry's own posture and nothing more: what
    /// choosing it does is the owner's, which is what lets the same strip
    /// hold a list whose sections both select a view and open their pages.
    #[must_use]
    pub fn with_disclosure(mut self, expanded: bool) -> Self {
        self.disclosure = Some(expanded);
        self
    }

    /// This entry as a page of the disclosing entry above it, drawn indented —
    /// sidebar anatomy, so a horizontal strip draws it nowhere.
    #[must_use]
    pub fn nested(mut self) -> Self {
        self.nested = true;
        self
    }

    /// This entry with a current reading, drawn trailing its label on the same
    /// line — sidebar anatomy, so a horizontal strip draws it nowhere.
    #[must_use]
    pub fn with_reading(mut self, reading: impl Into<String>) -> Self {
        self.reading = Some(reading.into());
        self
    }

    /// This entry with a bounded trend drawn beneath its label — sidebar
    /// anatomy, so a horizontal strip draws it nowhere.
    ///
    /// An entry that carries no trend claims no room for one, so the absence of
    /// an instrument is what says a reading is a fact rather than a rate.
    #[must_use]
    pub fn with_trend(mut self, trend: Chart) -> Self {
        self.trend = Some(trend);
        self
    }

    /// This entry as the start of a group, introduced by the quiet heading
    /// `heading` above it — sidebar anatomy, so a horizontal strip draws it
    /// nowhere.
    #[must_use]
    pub fn with_group(mut self, heading: impl Into<String>) -> Self {
        self.group = Some(heading.into());
        self
    }

    /// This entry as the first of a new, unnamed group, set apart from what is
    /// above it by a blank band half an entry's line tall — sidebar anatomy,
    /// so a horizontal strip draws it nowhere.
    ///
    /// A break with nothing above it draws nothing: there is no group before
    /// it to divide it from.
    #[must_use]
    pub fn with_group_break(mut self, group_break: bool) -> Self {
        self.group_break = group_break;
        self
    }

    /// The tab's label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The entry's leading glyph, if it has one.
    #[must_use]
    pub fn icon(&self) -> Option<IconKind> {
        self.icon
    }

    /// Whether this entry's own pages are shown, or `None` when it discloses
    /// none.
    #[must_use]
    pub fn disclosure(&self) -> Option<bool> {
        self.disclosure
    }

    /// Whether this entry is a page of the disclosing entry above it.
    #[must_use]
    pub fn is_nested(&self) -> bool {
        self.nested
    }

    /// Replace the tab's label, leaving the rest of the tab alone.
    ///
    /// For a strip whose labels carry a live reading — a count beside the
    /// name — so the owner can re-label in place and keep the strip that
    /// holds where the pointer and the keyboard cursor are.
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }

    /// The entry's current reading, if any.
    #[must_use]
    pub fn reading(&self) -> Option<&str> {
        self.reading.as_deref()
    }

    /// Replace the entry's reading, leaving the rest of the entry alone — the
    /// live-sample counterpart of [`set_label`](Self::set_label).
    pub fn set_reading(&mut self, reading: Option<String>) {
        self.reading = reading;
    }

    /// The entry's trend, if any.
    #[must_use]
    pub fn trend(&self) -> Option<&Chart> {
        self.trend.as_ref()
    }

    /// Replace the entry's trend, leaving the rest of the entry alone — the
    /// live-sample counterpart of [`set_label`](Self::set_label).
    pub fn set_trend(&mut self, trend: Option<Chart>) {
        self.trend = trend;
    }

    /// The heading introducing this entry's group, if it starts one.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// Whether this entry starts a group set apart by a blank band.
    #[must_use]
    pub fn is_group_break(&self) -> bool {
        self.group_break
    }

    /// The tab's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the tab's composed state (e.g. from a model update).
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Whether the tab is the selected one.
    #[must_use]
    pub fn is_selected(&self) -> bool {
        self.state.selection == SelectionState::Selected
    }

    /// Whether the tab's view is loading (any in-progress activity).
    fn is_loading(&self) -> bool {
        matches!(
            self.state.activity,
            ActivityState::Working | ActivityState::Indeterminate | ActivityState::Progress(_)
        )
    }
}

/// The `(offset, extent)` span of item `index` when `count` equal items share
/// `total` pixels along one axis, the last absorbing the rounding remainder.
///
/// The horizontal strip's whole geometry: equal tabs sharing the strip's
/// width. A vertical strip stacks at content height instead, so it does not
/// come through here.
fn axis_span(index: usize, count: usize, total: u32) -> Option<(u32, u32)> {
    let count_u32 = u32::try_from(count).ok()?;
    if count_u32 == 0 || total == 0 || index >= count {
        return None;
    }
    let idx = u32::try_from(index).ok()?;
    let each = total / count_u32;
    if each == 0 {
        return None;
    }
    let offset = idx * each;
    // The last item absorbs the rounding remainder so the strip fills the
    // whole axis.
    let extent = if index + 1 == count {
        total - idx * each
    } else {
        each
    };
    Some((offset, extent))
}

/// The `(x, y, w, h)` a seam `thickness` deep and `extent` long occupies on a
/// tab's own `rect`: the lower edge of a horizontal tab, the leading (left)
/// edge of a vertical one.
///
/// This is the one definition of which edge carries a seam, so a tab's
/// selection seam and its Heat Seam can never end up on different edges of the
/// same tab. The extent is capped by the tab's own span along that edge, so a
/// seam never runs past the tab it belongs to.
#[must_use]
fn seam_rect(
    orientation: TabsOrientation,
    rect: (u32, u32, u32, u32),
    thickness: u32,
    extent: u32,
) -> (u32, u32, u32, u32) {
    let (x, y, w, h) = rect;
    match orientation {
        TabsOrientation::Horizontal => (
            x,
            y.saturating_add(h).saturating_sub(thickness),
            extent.min(w),
            thickness,
        ),
        TabsOrientation::Vertical => (x, y, thickness, extent.min(h)),
    }
}

/// What one band of a strip's stack is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BandKind {
    /// The quiet heading introducing the group the item at this index starts.
    Heading(usize),
    /// The item at this index.
    Item(usize),
    /// The stated absence at this index into the strip's absences: its own
    /// heading and the line under it. Selects nothing and is never hit-tested.
    Absence(usize),
    /// The blank band setting apart the group the item at this index starts.
    /// Draws nothing and selects nothing.
    Break(usize),
}

/// One band of a strip's stack and the rectangle it occupies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Band {
    kind: BandKind,
    rect: Rect,
}

/// The running edges of an entry's label line as its anatomy claims room:
/// where the label may start, where the trailing marks end, and how much text
/// budget is left.
struct LabelLine {
    lead: u32,
    trail: u32,
    avail: u32,
}

/// What painting one entry needs beyond the entry itself: where it goes, at
/// what density, in which theme and face, and the owner's icon lookup.
///
/// One value rather than five loose parameters threaded through every helper
/// the entry's anatomy is drawn by.
struct EntryPaint<'a> {
    rect: (u32, u32, u32, u32),
    scale: Scale,
    theme: &'a Theme,
    font: BitmapFont,
    /// The height of the entry's label line ([`Tabs::entry_line`]).
    line: u32,
    artwork: &'a mut dyn IconArtwork,
}

/// Item `index`'s rectangle within `bands`, or `None` when there is no such
/// item — the one rule every damage report and hit test applies.
fn item_area(bands: &[Band], index: usize) -> Option<Rect> {
    bands
        .iter()
        .find(|band| band.kind == BandKind::Item(index))
        .map(|band| band.rect)
}

/// Whether two runs name the same entries in the same order — the identity a
/// strip's hover and press latch are about, since each names one entry. An
/// entry's live reading and trend are the sample's to say, so they are not
/// part of it.
fn same_entries(live: &[Tab], fresh: &[Tab]) -> bool {
    live.len() == fresh.len()
        && live.iter().zip(fresh).all(|(live, fresh)| {
            live.label() == fresh.label()
                && live.group() == fresh.group()
                && live.is_group_break() == fresh.is_group_break()
                && live.is_nested() == fresh.is_nested()
        })
}

/// A row of equal-width tabs, or — laid out [`TabsOrientation::Vertical`] — a
/// sidebar list of grouped entries, selecting one of several views
/// (spec §11.12).
///
/// The strip keeps where the pointer rests (the *hovered* tab) apart from
/// where the keyboard cursor is (the *current* tab): both lift their plate,
/// only the current tab is ringed. One record for both would blink a resting
/// pointer's highlight off every time a host re-stated its keyboard focus,
/// which hosts do whenever their model refreshes. Neither is the *selected*
/// tab, whose view is the one on show: selection commits through the owner via
/// [`TabsAction::Selected`], which then updates the items' [`SelectionState`]
/// (helper [`Tabs::set_selected`]).
///
/// Equal strips draw the same pixels, so a host may use `==` as its repaint
/// gate: the items, the orientation, and both records of attention compare.
/// The pointer coordinate and the pressed-tab latch do not — no render path
/// reads either, and a press's visible consequence is the lift the pointer's
/// own motion already stated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tabs {
    items: Vec<Tab>,
    orientation: TabsOrientation,
    /// The tab the pointer rests on, or the one holding a press while the
    /// pointer slides off it.
    hovered: Option<usize>,
    /// The tab the keyboard cursor is on.
    current: Option<usize>,
    /// The last pointer position, mapped to a tab on the next press or
    /// release — hit-testing input, never drawn.
    pointer: RenderInvariant<Point>,
    /// The tab a primary press landed on, held until release so a click that
    /// slides onto another tab does not select it; the pressed tab keeps its
    /// lift meanwhile.
    armed: RenderInvariant<Option<usize>>,
    /// The groups that have no entries, and why — drawn in rail position, in
    /// `before` order. Vertical strips only: a horizontal strip has one row
    /// and no group headings to state an absence under.
    absences: Vec<TabGroupAbsence>,
}

impl Tabs {
    /// A horizontal tab strip over the given items.
    #[must_use]
    pub fn new(tabs: Vec<Tab>) -> Self {
        Self {
            items: tabs,
            orientation: TabsOrientation::Horizontal,
            hovered: None,
            current: None,
            pointer: RenderInvariant::new(Point::ORIGIN),
            armed: RenderInvariant::new(None),
            absences: Vec::new(),
        }
    }

    /// This strip with `absences` stating the groups that have no entries.
    ///
    /// Drawn in rail position and in `before` order, so an empty group
    /// appears where it belongs rather than after everything. Absences shift
    /// no item's index: [`TabsAction::Selected`], [`Tabs::len`], and every
    /// selection entry point still count items alone.
    #[must_use]
    pub fn with_absences(mut self, mut absences: Vec<TabGroupAbsence>) -> Self {
        absences.sort_by_key(|absence| absence.before);
        self.absences = absences;
        self
    }

    /// The groups this strip states as empty, in rail order.
    #[must_use]
    pub fn absences(&self) -> &[TabGroupAbsence] {
        &self.absences
    }

    /// Take `fresh`'s entries, absences, selection and keyboard cursor, keeping
    /// the records only this strip holds: where the pointer last was, which
    /// entry it rests on, and which entry a press is waiting on.
    ///
    /// This is how a host whose entries come and go — one per device, per
    /// volume, per interface — adopts each fresh sample. Assigning a freshly
    /// built strip over a live one instead gives it amnesia: it no longer knows
    /// where the pointer is, so the next press hit-tests against the origin and
    /// selects nothing until the reader moves the pointer again, a press already
    /// waiting for its release is swallowed, and the entry under a resting
    /// pointer loses its lift on every sample.
    ///
    /// The pointer coordinate survives whatever the entries became: it is where
    /// the reader's pointer is, not a claim about the sample. The hover, the
    /// press latch and the keyboard cursor each name one *entry*, so they
    /// survive only while the run of entries is the same run — an entry's live
    /// reading and trend are the sample's to say and do not disturb them, but a
    /// strip that gained, lost or re-ordered an entry drops all three and waits
    /// for the reader's next input.
    ///
    /// The cursor is the reader's own, not the sample's: a reader who has moved
    /// it down the strip without committing keeps it there across every
    /// refresh, where taking `fresh`'s would snap it back to wherever the host
    /// last set the selection.
    ///
    /// Answers whether the strip's drawn state moved, which is what decides
    /// whether the column it sits in owes a repaint.
    pub fn restate(&mut self, mut fresh: Tabs) -> bool {
        fresh.pointer = self.pointer;
        if same_entries(&self.items, &fresh.items) {
            fresh.hovered = self.hovered;
            fresh.armed = self.armed;
            fresh.current = self.current;
        } else {
            fresh.hovered = None;
            fresh.armed = RenderInvariant::new(None);
        }
        let moved = *self != fresh;
        *self = fresh;
        moved
    }

    /// This strip laid out along `orientation`.
    #[must_use]
    pub fn with_orientation(mut self, orientation: TabsOrientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// The strip's orientation.
    #[must_use]
    pub fn orientation(&self) -> TabsOrientation {
        self.orientation
    }

    /// The extent the strip needs *across* its own axis — the height a
    /// horizontal strip occupies, or the width a vertical one does — from the
    /// font's line height and the theme's control padding, floored at the
    /// theme's standard control height so a strip never reads shorter than an
    /// ordinary control.
    ///
    /// A horizontal strip's height is the same for every tab regardless of
    /// how many share it, but a vertical strip's *width* is fixed regardless
    /// of how many entries stack down it; that fixed width has to comfortably
    /// hold the modified/error Signal Bead beside the label without the two
    /// competing for space, so the vertical extent additionally reserves the
    /// bead's own footprint.
    #[must_use]
    pub fn measured_extent(&self, scale: Scale, theme: &Theme) -> u32 {
        let base = text_plate_height(theme, scale, TextRole::Body);
        match self.orientation {
            TabsOrientation::Horizontal => base,
            TabsOrientation::Vertical => {
                let pad = scale.scale_length(theme.metrics().control_inset).max(1);
                let bead = scale.scale_length(theme.metrics().bead_size).max(3);
                base.saturating_add(bead).saturating_add(pad)
            }
        }
    }

    /// The height the strip needs to draw whole.
    ///
    /// A horizontal strip is one row, so this is its
    /// [`measured_extent`](Self::measured_extent). A vertical strip stacks, so
    /// this is every group heading and break plus every entry at its own
    /// content height, plus every stated absence — which is what an owner
    /// whose entry list is *discovered* rather than fixed reserves and scrolls,
    /// instead of squeezing entries into whatever column it happens to have.
    #[must_use]
    pub fn measured_height(&self, scale: Scale, theme: &Theme) -> u32 {
        match self.orientation {
            TabsOrientation::Horizontal => self.measured_extent(scale, theme),
            TabsOrientation::Vertical => {
                let mut total = 0u32;
                self.stack(scale, theme, |_, height| {
                    total = total.saturating_add(height);
                });
                total
            }
        }
    }

    /// Walk a vertical strip's bands top-down, handing `visit` each one and
    /// the height it claims.
    ///
    /// The one definition of the stack: [`layout`](Self::layout) places what
    /// this yields and [`measured_height`](Self::measured_height) sums it, so
    /// the height an owner reserves is always the height the strip lays out.
    fn stack(&self, scale: Scale, theme: &Theme, mut visit: impl FnMut(BandKind, u32)) {
        let heading = heading_height(scale, theme);
        let absence = heading.saturating_add(text_plate_height(theme, scale, TextRole::Body));
        let line = self.entry_line(scale, theme);
        let gap = line / 2;
        let mut above = false;
        let mut absences = self.absences.iter().enumerate().peekable();
        for (index, tab) in self.items.iter().enumerate() {
            // The empty groups that belong above this item, in their own rail
            // position.
            while let Some((slot, _)) = absences.next_if(|(_, stated)| stated.before <= index) {
                visit(BandKind::Absence(slot), absence);
                above = true;
            }
            if tab.group_break && above {
                visit(BandKind::Break(index), gap);
            }
            if tab.group.is_some() {
                visit(BandKind::Heading(index), heading);
            }
            visit(BandKind::Item(index), entry_height(tab, line, scale, theme));
            above = true;
        }
        // A trailing group with nothing in it, and the whole-strip case where
        // there are no items at all.
        for (slot, _) in absences {
            visit(BandKind::Absence(slot), absence);
        }
    }

    /// The strip's items.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.items
    }

    /// Mutable access to the strip's items.
    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.items
    }

    /// The number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the strip has no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The index of the selected tab, if any.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.items.iter().position(Tab::is_selected)
    }

    /// Mark `index` as the selected tab and clear the selection from the
    /// others; an out-of-range index selects nothing (fail closed).
    ///
    /// Every tab whose selection actually changes reports its own rectangle, so
    /// moving the selection costs the tab it left and the tab it arrives on. The
    /// sweep is over all of them rather than those two, because the owner sets
    /// each tab's initial selection and nothing here may assume only one was
    /// ever lit.
    pub fn set_selected(
        &mut self,
        index: usize,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let bands = self.layout(bounds, scale, theme);
        let mut areas = bands
            .iter()
            .filter_map(|band| match band.kind {
                BandKind::Item(index) => Some((index, band.rect)),
                _ => None,
            })
            .peekable();
        for i in 0..self.items.len() {
            let rect = areas
                .next_if(|&(index, _)| index == i)
                .map_or(Rect::EMPTY, |(_, rect)| rect);
            let selection = if i == index {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            };
            if let Some(tab) = self.items.get_mut(i) {
                damage::set(&mut tab.state.selection, selection, rect, damage);
            }
        }
    }

    /// Adopt `index` as the selected tab without reporting, for a caller that is
    /// composing or rebuilding this strip and presents it whole.
    ///
    /// [`set_selected`](Self::set_selected) is the interactive move and reports
    /// the tabs whose plates change. A rebuild has no layout to resolve a tab
    /// against and nothing to report against either, so it says so here rather
    /// than passing a rectangle it does not have.
    pub fn adopt_selected(&mut self, index: usize) {
        for (i, tab) in self.items.iter_mut().enumerate() {
            tab.state.selection = if i == index {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            };
        }
    }

    /// The tab the keyboard cursor is on, if any.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Put the keyboard cursor on `index`, or take it off the strip with
    /// `None`; an out-of-range index takes it off (fail closed).
    ///
    /// Where the pointer rests is untouched, so a host may re-state its
    /// keyboard focus as often as its model refreshes; a re-state that lands the
    /// cursor where it already was reports nothing.
    pub fn set_current(
        &mut self,
        index: Option<usize>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.move_current(self.on_strip(index), bounds, scale, theme, damage);
    }

    /// Adopt `index` as the keyboard cursor without reporting, for a caller that
    /// is composing or rebuilding this strip and presents it whole.
    ///
    /// [`set_current`](Self::set_current) is the interactive move and reports the
    /// two tabs the cursor moves between. A rebuild has no layout to resolve a
    /// tab against and nothing to report against either, so it says so here
    /// rather than passing a rectangle it does not have.
    pub fn adopt_current(&mut self, index: Option<usize>) {
        self.current = self.on_strip(index);
    }

    /// `index` if it names a tab of this strip, else `None` — the one admission
    /// rule every cursor entry point applies.
    fn on_strip(&self, index: Option<usize>) -> Option<usize> {
        index.filter(|&i| i < self.items.len())
    }

    /// Hand `visit` every band of the strip, in order, with the rectangle it
    /// occupies; items come in index order.
    ///
    /// The one layout [`render`](Self::render), the hit test and every damage
    /// report read, so a press can never select a tab drawn at a different
    /// span. A horizontal strip is items alone, sharing the strip's width
    /// equally. A vertical strip stacks top-down ([`stack`](Self::stack)) — a
    /// group's break or heading, then its entries, each at its own content
    /// height — every band at its natural size: a list longer than its column
    /// is its owner's to show through a [`ScrollView`](crate::ScrollView),
    /// never one the strip cuts short.
    fn walk(&self, bounds: Rect, scale: Scale, theme: &Theme, mut visit: impl FnMut(Band)) {
        let Some((x, y, w, h)) = surface_rect(bounds) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        match self.orientation {
            TabsOrientation::Horizontal => {
                for index in 0..self.items.len() {
                    if let Some((offset, extent)) = axis_span(index, self.items.len(), w) {
                        visit(Band {
                            kind: BandKind::Item(index),
                            rect: Rect::new(to_i32(x + offset), to_i32(y), extent, h),
                        });
                    }
                }
            }
            TabsOrientation::Vertical => {
                let mut top = 0u32;
                self.stack(scale, theme, |kind, height| {
                    visit(Band {
                        kind,
                        rect: Rect::new(
                            to_i32(x),
                            to_i32(y).saturating_add(to_i32(top)),
                            w,
                            height,
                        ),
                    });
                    top = top.saturating_add(height);
                });
            }
        }
    }

    /// The bands [`walk`](Self::walk) visits, for a caller that needs them
    /// more than once.
    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Vec<Band> {
        let mut bands = Vec::with_capacity(self.items.len());
        self.walk(bounds, scale, theme, |band| bands.push(band));
        bands
    }

    /// The item under `point` and its rectangle, with the rectangle of item
    /// `also` — what a pointer sample needs, found without keeping the layout.
    fn item_under(
        &self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        point: Point,
        also: Option<usize>,
    ) -> (Option<(usize, Rect)>, Option<Rect>) {
        let mut under = None;
        let mut also_area = None;
        self.walk(bounds, scale, theme, |band| {
            let BandKind::Item(index) = band.kind else {
                return;
            };
            if under.is_none() && band.rect.contains(point) {
                under = Some((index, band.rect));
            }
            if also == Some(index) {
                also_area = Some(band.rect);
            }
        });
        (under, also_area)
    }

    /// Tab `index`'s area within `bounds`, or `None` when there is no such tab.
    #[must_use]
    pub fn tab_area(
        &self,
        index: usize,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        item_area(&self.layout(bounds, scale, theme), index)
    }

    /// The tab index under `point`, if any, for the given bounds.
    ///
    /// A point over a group heading answers `None`: a heading selects nothing
    /// (fail closed). A scrolled owner maps its pointer into the strip's own
    /// layout first, so a press lands on the entry the reader sees.
    #[must_use]
    pub fn tab_at(&self, bounds: Rect, scale: Scale, theme: &Theme, point: Point) -> Option<usize> {
        self.item_under(bounds, scale, theme, point, None)
            .0
            .map(|(index, _)| index)
    }

    /// Paint the strip into `surface` at `bounds` for the active theme.
    ///
    /// A sidebar entry's leading glyph is resolved through `artwork` at
    /// [`Self::icon_side`], so a strip of glyphs costs a cache lookup per
    /// entry rather than re-resolving vector coverage every frame; a caller
    /// holding no cache passes [`NoArtwork`](tairix_icon::NoArtwork) and each
    /// glyph is rasterised in place. A horizontal strip draws no glyph and
    /// never consults it.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
    ) {
        if withheld(surface, bounds) {
            return;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let line = self.entry_line(scale, theme);
        for band in self.layout(bounds, scale, theme) {
            // A band a scrolled owner shows none of costs nothing to skip.
            if withheld(surface, band.rect) {
                continue;
            }
            let Some(rect) = surface_rect(band.rect) else {
                continue;
            };
            match band.kind {
                BandKind::Heading(index) => {
                    self.paint_heading(surface, index, rect, scale, theme);
                }
                BandKind::Item(index) => {
                    self.paint_tab(
                        surface,
                        index,
                        &mut EntryPaint {
                            rect,
                            scale,
                            theme,
                            font,
                            line,
                            artwork,
                        },
                    );
                }
                BandKind::Absence(slot) => {
                    self.paint_absence(surface, slot, rect, scale, theme, font);
                }
                BandKind::Break(_) => {}
            }
        }
    }

    /// The side of the square icon slot a sidebar entry reserves, which is
    /// also the pixel side an owner's cache should rasterise its icons at.
    ///
    /// One definition, so what the strip paints at and what its owner
    /// resolves at cannot drift apart. Zero for a horizontal strip, which
    /// draws no icon.
    #[must_use]
    pub fn icon_side(&self, scale: Scale, theme: &Theme) -> u32 {
        match self.orientation {
            TabsOrientation::Horizontal => 0,
            TabsOrientation::Vertical => entry_icon_side(scale, theme),
        }
    }

    /// The height of every vertical entry's label line: the body text plate,
    /// or in a strip whose entries carry icons, whatever seats the icon with a
    /// control gap's clearance shared above and below it.
    ///
    /// One height for the whole strip, so a disclosed page's row keeps the
    /// rhythm of the rows around it whether or not it has an icon of its own.
    fn entry_line(&self, scale: Scale, theme: &Theme) -> u32 {
        let text = text_plate_height(theme, scale, TextRole::Body);
        if self.items.iter().all(|tab| tab.icon.is_none()) {
            return text;
        }
        let clearance = scale.scale_length(theme.metrics().control_gap).max(1);
        text.max(entry_icon_side(scale, theme).saturating_add(clearance))
    }

    /// Paint one empty group: its heading, then the line saying why it is
    /// empty, both quiet and on the surface behind them with no plate — the
    /// group is a break in the list carrying a statement, never an entry a
    /// reader could try to select.
    fn paint_absence(
        &self,
        surface: &mut Surface,
        slot: usize,
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) {
        let Some(absence) = self.absences.get(slot) else {
            return;
        };
        let (x, y, w, h) = rect;
        let pad = scale.scale_length(theme.metrics().control_inset).max(1);
        let gap = scale.scale_length(theme.metrics().control_gap).max(1);
        let Some(avail) = w.checked_sub(pad.saturating_mul(2)) else {
            return;
        };
        if avail == 0 || h < font.line_height() {
            return;
        }
        let muted = Color::from(theme.palette().on_surface_muted);
        paint_group_heading(surface, rect, scale, theme, absence.heading());
        let statement_top = y
            .saturating_add(heading_height(scale, theme))
            .saturating_add(gap);
        let room = y.saturating_add(h).saturating_sub(statement_top);
        // A stated absence is a sentence about why a group is empty, so it
        // wraps into the room the slot has rather than stopping mid-reason.
        TextBlock::prose(font, avail, line_budget(font, room), muted).paint(
            surface,
            absence.statement(),
            (x.saturating_add(pad), statement_top),
        );
    }

    /// Paint the group heading above the entry at `index`: its own text on the
    /// surface behind it, with no plate, so it reads as a break in the list
    /// rather than as another entry.
    fn paint_heading(
        &self,
        surface: &mut Surface,
        index: usize,
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
    ) {
        let Some(heading) = self.items.get(index).and_then(|tab| tab.group.as_deref()) else {
            return;
        };
        paint_group_heading(surface, rect, scale, theme, heading);
    }

    /// Paint the tab at `index` into the `rect` [`Self::layout`] gave it:
    /// its plate, the seam its orientation carries, the keyboard focus ring,
    /// then its own content.
    fn paint_tab(&self, surface: &mut Surface, index: usize, paint: &mut EntryPaint<'_>) {
        let Some(tab) = self.items.get(index) else {
            return;
        };
        let EntryPaint {
            rect,
            scale,
            theme,
            font,
            ..
        } = *paint;
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 {
            return;
        }
        let palette = theme.palette();
        let current = self.current == Some(index);
        let lifted = current || self.hovered == Some(index);

        match self.orientation {
            // A sidebar entry is a row: selection lifts it to the raised fill
            // and the pointer or keyboard cursor takes the shared wash, which
            // is deliberately not that fill — so the cursor can never imitate
            // selection and needs no ring of its own. A resting entry is an
            // inlay in the ground it sits on.
            TabsOrientation::Vertical => {
                let plate = if tab.is_selected() {
                    palette.surface_raised
                } else if lifted {
                    palette.surface_hover
                } else {
                    palette.surface
                };
                let plate = ground_fill(theme, plate, ChromeLayer::Inlay);
                surface.fill_rect(x, y, w, h, Color::from(plate));
                Self::paint_seam(surface, self.orientation, rect, scale, theme, tab);
                Self::paint_entry(surface, tab, paint);
            }
            // A horizontal tab is a page shape, not a row: the selected tab
            // reads as the content surface it opens onto, an unselected one is
            // quieter, and the keyboard cursor is ringed because a lift alone
            // would read as the pointer. It is a control raised on the ground,
            // so it takes a plate's weight on glass.
            TabsOrientation::Horizontal => {
                let plate = if tab.is_selected() {
                    palette.surface
                } else if lifted {
                    palette.surface_raised
                } else {
                    palette.surface_pressed
                };
                let plate = ground_fill(theme, plate, ChromeLayer::Plate);
                surface.fill_rect(x, y, w, h, Color::from(plate));
                Self::paint_seam(surface, self.orientation, rect, scale, theme, tab);
                if current {
                    draw_outline(
                        surface,
                        x,
                        y,
                        w,
                        h,
                        plate_border(theme, scale).max(1),
                        Color::from(palette.rim_active),
                    );
                }
                Self::paint_centred_label(surface, rect, scale, theme, font, tab);
            }
        }
    }

    /// Paint `tab`'s selection mark or Heat Seam onto the edge its
    /// `orientation` carries it on.
    ///
    /// A selected horizontal tab marks the lower edge of a page shape, so it
    /// takes the seam breadth; a selected sidebar entry marks its *leading*
    /// edge, which is the shared selection rail every row family draws and so
    /// takes the rail breadth. A loading tab draws a Heat Seam at the seam
    /// breadth in either orientation, proportional when the fraction is known.
    ///
    /// A selected tab shows selection rather than progress, so its mark wins
    /// over a Heat Seam it would otherwise draw on the very same edge.
    fn paint_seam(
        surface: &mut Surface,
        orientation: TabsOrientation,
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
        tab: &Tab,
    ) {
        let (_, _, w, h) = rect;
        let (along, cross) = match orientation {
            TabsOrientation::Horizontal => (w, h),
            TabsOrientation::Vertical => (h, w),
        };
        let base = seam_thickness(theme, scale);
        let (thickness, extent) = if tab.is_selected() {
            let mark = match orientation {
                TabsOrientation::Vertical => rail_thickness(theme, scale),
                // Heavier contrast doubles the selected seam, so selection
                // still carries where a hue shift alone would not.
                TabsOrientation::Horizontal => {
                    base.saturating_mul(if heavy_contrast(theme) { 2 } else { 1 })
                }
            };
            (mark.min(cross), along)
        } else if tab.is_loading() {
            (base.min(cross), seam_width(tab.state.activity, along))
        } else {
            return;
        };
        if thickness == 0 || extent == 0 {
            return;
        }
        let (sx, sy, sw, sh) = seam_rect(orientation, rect, thickness, extent);
        surface.fill_rect(sx, sy, sw, sh, Color::from(theme.palette().accent));
    }

    /// The colour `tab`'s label reads in: muted when its state rules it out,
    /// `selected` when it is the selected tab, the plain foreground otherwise.
    ///
    /// The selected colour is the caller's because the two orientations carry
    /// selection differently. A page shape has no lift or leading rail to carry
    /// it, so a horizontal tab's label takes the accent; a sidebar row already
    /// wears both, and tinting its label as well would make the entry's own
    /// name a third selection mark and leave the reading beside it the only
    /// plain text on the row.
    fn label_color(theme: &Theme, tab: &Tab, selected: Rgba) -> Color {
        let palette = theme.palette();
        Color::from(match tab.state.disposition() {
            ControlDisposition::DisabledByState => palette.on_surface_muted,
            _ if tab.is_selected() => selected,
            _ => palette.on_surface,
        })
    }

    /// Paint `tab`'s label centred in `rect`, and its Signal Bead at the
    /// top-trailing corner — the horizontal tab shape.
    ///
    /// The bead's footprint is carved out of the label's own budget before the
    /// label is laid out, so a long label is elided rather than running under
    /// the bead.
    fn paint_centred_label(
        surface: &mut Surface,
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
        tab: &Tab,
    ) {
        let (x, y, w, h) = rect;
        let border = plate_border(theme, scale);
        let pad = scale.scale_length(theme.metrics().control_inset).max(1);
        let bead_w = Self::bead_gutter(scale, theme, rect, tab);
        let avail = w
            .saturating_sub(border.saturating_add(pad).saturating_mul(2))
            .saturating_sub(bead_w);
        if avail > 0 {
            let run = font.elide_to_width(tab.label(), avail);
            let tw = run_width(font, run);
            let cx = to_i32(x) + to_i32(w.saturating_sub(bead_w)) / 2;
            let glyph_h = font.glyph_height();
            let text_y = to_i32(y) + (to_i32(h) - to_i32(glyph_h)).max(0) / 2;
            paint_run(
                surface,
                font,
                run,
                (cx - to_i32(tw) / 2, text_y),
                Self::label_color(theme, tab, theme.palette().accent),
                None,
            );
        }
        Self::paint_tab_bead(surface, rect, scale, theme, tab);
    }

    /// Paint `tab` as a sidebar list entry: its glyph and label leading, its
    /// reading and disclosure chevron trailing on that same line, and its
    /// trend beneath.
    ///
    /// Room is claimed in the order a reader needs it: the Signal Bead first
    /// (it is a state, not a reading), then the disclosure chevron and the
    /// reading, then the leading glyph, then the label, which is what gives
    /// way — elided with the shared mark, so a cut name never reads as a
    /// complete one. The reading is what the reader came for, and a row whose
    /// glyph gave way would leave a nameless indent. The trend draws only
    /// where a whole one still fits beneath the label line.
    fn paint_entry(surface: &mut Surface, tab: &Tab, paint: &mut EntryPaint<'_>) {
        let EntryPaint {
            rect,
            scale,
            theme,
            font,
            ..
        } = *paint;
        let (x, y, w, h) = rect;
        let pad = scale.scale_length(theme.metrics().control_inset).max(1);
        let gap = scale.scale_length(theme.metrics().control_gap).max(1);
        let indent = if tab.nested {
            Self::nest_indent(scale, theme)
        } else {
            0
        };
        let lead = seam_thickness(theme, scale)
            .saturating_add(pad)
            .saturating_add(indent);
        let Some(inner_w) = w.checked_sub(lead.saturating_add(pad)) else {
            Self::paint_tab_bead(surface, rect, scale, theme, tab);
            return;
        };
        let label_row = paint.line;
        let text_y = y.saturating_add(label_row.saturating_sub(font.glyph_height()) / 2);

        let bead_w = Self::bead_gutter(scale, theme, rect, tab);
        // The label line's running edges: what the glyph has claimed from the
        // leading side, and what the bead, chevron and reading have claimed
        // from the trailing one.
        let mut line = LabelLine {
            lead: x.saturating_add(lead),
            trail: x
                .saturating_add(lead)
                .saturating_add(inner_w)
                .saturating_sub(bead_w),
            avail: inner_w.saturating_sub(bead_w),
        };

        Self::paint_disclosure(surface, tab, paint, &mut line, label_row);
        Self::paint_entry_icon(surface, tab, paint, &mut line, label_row);

        if let Some(reading) = tab.reading() {
            let run = font.elide_to_width(reading, line.avail);
            let reading_w = run_width(font, run).min(line.avail);
            paint_run(
                surface,
                font,
                run,
                (to_i32(line.trail.saturating_sub(reading_w)), to_i32(text_y)),
                Color::from(theme.palette().on_surface_muted),
                None,
            );
            line.avail = line.avail.saturating_sub(reading_w.saturating_add(gap));
        }
        if line.avail > 0 {
            paint_run(
                surface,
                font,
                font.elide_to_width(tab.label(), line.avail),
                (to_i32(line.lead), to_i32(text_y)),
                Self::label_color(theme, tab, theme.palette().on_surface),
                None,
            );
        }

        if let Some(trend) = tab.trend() {
            let trend_h = chart_height(scale, theme);
            let trend_y = y.saturating_add(label_row);
            let trend_w = line.trail.saturating_sub(line.lead.min(line.trail));
            if trend_y.saturating_add(trend_h) <= y.saturating_add(h) && trend_w > 0 {
                trend.render(
                    surface,
                    Rect::new(to_i32(line.lead), to_i32(trend_y), trend_w, trend_h),
                    scale,
                    theme,
                );
            }
        }

        Self::paint_tab_bead(surface, rect, scale, theme, tab);
    }

    /// Draw `tab`'s disclosure chevron in the trailing gutter, claiming its
    /// room from `line` — or nothing, when the entry discloses nothing or the
    /// row cannot afford the mark.
    fn paint_disclosure(
        surface: &mut Surface,
        tab: &Tab,
        paint: &EntryPaint<'_>,
        line: &mut LabelLine,
        label_row: u32,
    ) {
        let Some(expanded) = tab.disclosure else {
            return;
        };
        let (_, y, _, h) = paint.rect;
        let gap = paint
            .scale
            .scale_length(paint.theme.metrics().control_gap)
            .max(1);
        let side = icon_slot_side(paint.font, label_row);
        let Some(remaining) = line.avail.checked_sub(side.saturating_add(gap)) else {
            return;
        };
        paint_chevron(
            surface,
            Rect::new(
                to_i32(line.trail.saturating_sub(side)),
                to_i32(y),
                side,
                label_row.min(h),
            ),
            if expanded {
                ChevronDir::Down
            } else {
                ChevronDir::Right
            },
            Self::label_color(paint.theme, tab, paint.theme.palette().on_surface),
        );
        line.trail = line.trail.saturating_sub(side.saturating_add(gap));
        line.avail = remaining;
    }

    /// Draw `tab`'s leading glyph, claiming its room from `line` — or
    /// nothing, when the entry names none or the row cannot afford the slot.
    fn paint_entry_icon(
        surface: &mut Surface,
        tab: &Tab,
        paint: &mut EntryPaint<'_>,
        line: &mut LabelLine,
        label_row: u32,
    ) {
        let Some(kind) = tab.icon else {
            return;
        };
        let (_, y, _, _) = paint.rect;
        let gap = paint
            .scale
            .scale_length(paint.theme.metrics().control_gap)
            .max(1);
        let side = entry_icon_side(paint.scale, paint.theme).min(label_row);
        let Some(remaining) = line.avail.checked_sub(side.saturating_add(gap)) else {
            return;
        };
        let tint = Self::label_color(paint.theme, tab, paint.theme.palette().on_surface);
        let picture = paint.artwork.artwork(IconRequest::kind(kind), side);
        paint_icon_slot(
            surface,
            (
                line.lead,
                y.saturating_add(label_row.saturating_sub(side) / 2),
                side,
            ),
            kind,
            tint,
            picture,
            FULL_COLOUR,
        );
        line.lead = line.lead.saturating_add(side.saturating_add(gap));
        line.avail = remaining;
    }

    /// The leading offset a nested entry is drawn at: one icon slot plus the
    /// gap after it, so a page lines up with the label of the entry that
    /// disclosed it rather than at an indent of its own.
    fn nest_indent(scale: Scale, theme: &Theme) -> u32 {
        entry_icon_side(scale, theme)
            .saturating_add(scale.scale_length(theme.metrics().control_gap).max(1))
    }

    /// The width `tab`'s Signal Bead claims at the trailing end of its label
    /// line, including the gap that keeps it off the text — zero when it shows
    /// none, or when the bead could not sit inside the tab at all.
    fn bead_gutter(scale: Scale, theme: &Theme, rect: (u32, u32, u32, u32), tab: &Tab) -> u32 {
        let (_, _, w, h) = rect;
        if Self::tab_bead(theme, tab).is_none() {
            return 0;
        }
        let pad = scale.scale_length(theme.metrics().control_inset).max(1);
        scale
            .scale_length(theme.metrics().bead_size)
            .max(3)
            .min(w)
            .min(h)
            .saturating_add(pad)
    }

    /// Paint `tab`'s Signal Bead at the top-trailing corner of `rect`, if it
    /// shows one.
    fn paint_tab_bead(
        surface: &mut Surface,
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
        tab: &Tab,
    ) {
        let Some((color, shape)) = Self::tab_bead(theme, tab) else {
            return;
        };
        let (x, y, w, h) = rect;
        let border = plate_border(theme, scale);
        let size = scale
            .scale_length(theme.metrics().bead_size)
            .max(3)
            .min(w)
            .min(h);
        // A bead that cannot sit inside its own tab past the plate border is
        // omitted: a cramped strip loses the marker rather than stamping it
        // over the neighbouring tab.
        let corner = border.saturating_add(size);
        if corner <= w.min(h) {
            paint_bead(
                surface,
                x.saturating_add(w - corner),
                y.saturating_add(border),
                size,
                color,
                shape,
            );
        }
    }

    /// The Signal Bead a tab shows, if any: an error bead (recovery diamond for
    /// invalid, warning diamond for a caution) takes priority over the modified
    /// dot, so an error is never hidden by an unsaved-work marker.
    fn tab_bead(theme: &Theme, tab: &Tab) -> Option<(Color, BeadShape)> {
        let palette = theme.palette();
        match tab.state.validation {
            ValidationState::Invalid => Some((Color::from(palette.recovery), BeadShape::Diamond)),
            ValidationState::Warning => Some((Color::from(palette.warning), BeadShape::Diamond)),
            _ if tab.modified => Some((Color::from(palette.accent), BeadShape::Check)),
            _ => None,
        }
    }

    /// Select the current tab if it is actionable, reporting the choice.
    fn choose(&self, index: usize) -> Option<TabsAction> {
        let tab = self.items.get(index)?;
        tab.state
            .is_actionable()
            .then_some(TabsAction::Selected { index })
    }

    /// Put the keyboard cursor on `next`, or take it off the strip with `None`,
    /// reporting the tab it left and the tab it arrives on rather than the whole
    /// strip.
    fn move_current(
        &mut self,
        next: Option<usize>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let bands = self.layout(bounds, scale, theme);
        if damage::move_mark(self.current, next, |index| item_area(&bands, index), damage) {
            self.current = next;
        }
    }

    /// Feed a pointer event; the tab under the pointer lifts and a completed
    /// primary click selects it.
    ///
    /// A press moves no pointer, so it states nothing new about where the
    /// pointer is; it only arms the tab it landed on.
    ///
    /// A lift that moves reports the two tabs it moved between; a sample that
    /// stays on one tab reports nothing.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TabsAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        let (under, hovered_area) =
            self.item_under(bounds, scale, theme, *self.pointer, self.hovered);
        let over = under.map(|(index, _)| index);
        match event {
            InputEvent::PointerMoved { .. } => {
                let area = |index| match under {
                    Some((at, rect)) if at == index => Some(rect),
                    _ => hovered_area.filter(|_| self.hovered == Some(index)),
                };
                if self.armed.is_none() && damage::move_mark(self.hovered, over, area, damage) {
                    self.hovered = over;
                }
                None
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                *self.armed = over;
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let armed = self.armed.take();
                match (armed, over) {
                    (Some(a), Some(o)) if a == o => self.choose(o),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Feed a key event: Left/Right move the current tab (wrapping) in a
    /// horizontal strip, Up/Down do the same in a vertical one, Home/End jump
    /// to the ends in either, and Enter/Space select the current tab. A moved
    /// cursor reports the two tabs it moved between; selecting reports nothing,
    /// because the strip draws the selection its owner sets.
    ///
    /// The two arrow pairs are deliberately exclusive to their own axis: a
    /// horizontal strip ignores Up/Down, and a vertical one never moves along
    /// its column for Left/Right, so a reader is never misled into thinking
    /// the wrong arrows move it. There Left/Right are the tree keys: Right
    /// reports [`TabsAction::Disclose`] opening a collapsed entry or steps onto
    /// its first page once it is open, and Left reports one closing an open
    /// entry or climbs from a page back to the entry that disclosed it.
    pub fn on_key(
        &mut self,
        key: Key,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TabsAction> {
        if self.items.is_empty() {
            return None;
        }
        let last = self.items.len() - 1;
        let (forward, backward) = match self.orientation {
            TabsOrientation::Horizontal => (NamedKey::Right, NamedKey::Left),
            TabsOrientation::Vertical => (NamedKey::Down, NamedKey::Up),
        };
        match key {
            Key::Named(named) if named == forward => {
                let next = match self.current {
                    Some(i) if i < last => i + 1,
                    _ => 0,
                };
                self.move_current(Some(next), bounds, scale, theme, damage);
                None
            }
            Key::Named(named) if named == backward => {
                let prev = match self.current {
                    Some(0) | None => last,
                    Some(i) => i - 1,
                };
                self.move_current(Some(prev), bounds, scale, theme, damage);
                None
            }
            Key::Named(NamedKey::Home) => {
                self.move_current(Some(0), bounds, scale, theme, damage);
                None
            }
            Key::Named(NamedKey::End) => {
                self.move_current(Some(last), bounds, scale, theme, damage);
                None
            }
            Key::Named(NamedKey::Enter) | Key::Char(' ') => self.choose(self.current?),
            Key::Named(NamedKey::Right) if self.orientation == TabsOrientation::Vertical => {
                self.tree_key(TreeKey::Inward, bounds, scale, theme, damage)
            }
            Key::Named(NamedKey::Left) if self.orientation == TabsOrientation::Vertical => {
                self.tree_key(TreeKey::Outward, bounds, scale, theme, damage)
            }
            _ => None,
        }
    }

    /// Answer a tree key on a vertical strip's current entry.
    ///
    /// Showing and hiding are the owner's to apply, so they are reported, and
    /// refused on an entry that refuses a press; a step moves only the cursor
    /// and reports the two entries it moved between.
    fn tree_key(
        &mut self,
        key: TreeKey,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TabsAction> {
        let step = tree_step(&self.items, self.current?, key, |tab| TreeRow {
            disclosure: tab.disclosure(),
            nested: tab.is_nested(),
        });
        match step? {
            TreeStep::Disclose { index, open } => self
                .items
                .get(index)?
                .state
                .is_actionable()
                .then_some(TabsAction::Disclose { index, open }),
            TreeStep::Move(to) => {
                self.move_current(Some(to), bounds, scale, theme, damage);
                None
            }
        }
    }
}

/// The height one vertical entry claims: the strip's `line`
/// ([`Tabs::entry_line`]), plus its own trend where it carries one.
///
/// The reading shares the label's line, so it costs no height; the trend is
/// what an entry pays for, which is why an entry with no rate behind it is
/// visibly shorter than one with a trace.
fn entry_height(tab: &Tab, line: u32, scale: Scale, theme: &Theme) -> u32 {
    match tab.trend {
        Some(_) => line.saturating_add(chart_height(scale, theme)),
        None => line,
    }
}

/// The side a sidebar entry's leading icon is drawn at.
fn entry_icon_side(scale: Scale, theme: &Theme) -> u32 {
    scale
        .scale_length(theme.metrics().sidebar_icon_extent)
        .max(1)
}

/// Paint a group heading into the top of `rect`: the accent, so a heading
/// names its group rather than reading as one more entry's label, at the
/// header role's own size.
///
/// Shared by a group's own heading and by the statement an empty group carries,
/// because both are the same heading and a reader must not be able to tell
/// which of the two they are looking at from its treatment.
fn paint_group_heading(
    surface: &mut Surface,
    rect: (u32, u32, u32, u32),
    scale: Scale,
    theme: &Theme,
    heading: &str,
) {
    let (x, y, w, h) = rect;
    let font = heading_font(theme, scale);
    let pad = scale.scale_length(theme.metrics().control_inset).max(1);
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    let Some(avail) = w.checked_sub(pad.saturating_mul(2)) else {
        return;
    };
    if avail == 0 || h < font.line_height() {
        return;
    }
    paint_run(
        surface,
        font,
        font.elide_to_width(heading, avail),
        (to_i32(x.saturating_add(pad)), to_i32(y.saturating_add(gap))),
        Color::from(theme.palette().accent),
        None,
    );
}

/// The face a group heading is set in: the role a header over a list takes,
/// which is below body size and bold.
///
/// The measurement and the paint read this one definition, so a heading's band
/// is always exactly as tall as the text put in it.
fn heading_font(theme: &Theme, scale: Scale) -> BitmapFont {
    role_font(theme, scale, TextRole::SectionHeader)
}

/// The height one group heading claims: its label with breathing room above
/// and below, so a heading separates its group rather than reading as another
/// entry.
fn heading_height(scale: Scale, theme: &Theme) -> u32 {
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    heading_font(theme, scale)
        .line_height()
        .saturating_add(gap.saturating_mul(2))
}

/// The height an entry's trend claims, from the theme's own chart metric.
fn chart_height(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().chart_height).max(1)
}
