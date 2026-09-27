//! The [`Gallery`]: the client-content composition the `widgets.app` bundle's
//! `Run` binary presents.
//!
//! The window's furniture (frame, title bar, command buttons) is drawn
//! server-side by the compositor, so the gallery renders only client content:
//! a [`Tabs`] strip selecting one control family and a panel of captioned demo
//! widgets for the selected family. Each family is one [`GalleryTab`]; each
//! panel is a column of [`DemoItem`]s laid out top-to-bottom, a caption on the
//! left and the live [`DemoWidget`] on the right. Pointer and key events are
//! routed to the tab strip or to the demo widget under focus; nothing here
//! performs privileged work.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{damage, Tab, Tabs, TabsAction};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, Modifiers, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::panels;
use crate::widget::{DemoContext, DemoWidget};

/// One control family, shown on its own tab.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GalleryTab {
    /// Push buttons: [`Button`](tairix_controls::Button),
    /// [`IconButton`](tairix_controls::IconButton),
    /// [`SplitButton`](tairix_controls::SplitButton).
    Buttons,
    /// Boolean selectors: toggle, checkbox, radio.
    Selectors,
    /// Value controls: slider, progress.
    Values,
    /// Text entries: text field, search field.
    Text,
    /// Choice controls: combo box, menu.
    Choice,
    /// Collection surfaces: list row, table row, card, panel.
    Collections,
    /// Form fields: the settings row and the captioned group it sits in.
    Forms,
    /// Bars: toolbar and scroll bars.
    Bars,
    /// Feedback surfaces: dialog, tooltip, help tip.
    Feedback,
    /// Window-manager furniture: the command buttons.
    Window,
}

impl GalleryTab {
    /// Every tab, in strip order.
    pub const ALL: [GalleryTab; 10] = [
        GalleryTab::Buttons,
        GalleryTab::Selectors,
        GalleryTab::Values,
        GalleryTab::Text,
        GalleryTab::Choice,
        GalleryTab::Collections,
        GalleryTab::Forms,
        GalleryTab::Bars,
        GalleryTab::Feedback,
        GalleryTab::Window,
    ];

    /// This tab's zero-based index in [`Self::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// The tab at `index`, if in range.
    #[must_use]
    pub fn from_index(index: usize) -> Option<GalleryTab> {
        Self::ALL.get(index).copied()
    }

    /// The tab's strip label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            GalleryTab::Buttons => "Buttons",
            GalleryTab::Selectors => "Selectors",
            GalleryTab::Values => "Values",
            GalleryTab::Text => "Text",
            GalleryTab::Choice => "Choice",
            GalleryTab::Collections => "Collections",
            GalleryTab::Forms => "Forms",
            GalleryTab::Bars => "Bars",
            GalleryTab::Feedback => "Feedback",
            GalleryTab::Window => "Window",
        }
    }
}

/// One demonstrated control in a panel: a caption, the live widget, and its
/// row height and optional fixed widget width in *logical* pixels (scaled at
/// layout time).
#[derive(Clone, Debug)]
pub struct DemoItem {
    /// The left-column caption naming the variation.
    pub caption: String,
    /// The live shared control.
    pub widget: DemoWidget,
    /// The row height in logical pixels.
    pub height: u32,
    /// A fixed widget width in logical pixels, or `None` to fill the row.
    pub width: Option<u32>,
}

impl DemoItem {
    /// A demo item filling the row width at the given logical height.
    #[must_use]
    pub fn new(caption: impl Into<String>, widget: DemoWidget, height: u32) -> Self {
        Self {
            caption: caption.into(),
            widget,
            height,
            width: None,
        }
    }

    /// This item with a fixed logical widget width instead of filling the row.
    #[must_use]
    pub fn with_width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }
}

/// The logical width of a panel's left caption column, in reference pixels.
const CAPTION_WIDTH: u32 = 168;

/// Where and how one demo widget is drawn, assembled from what the gallery
/// already holds.
///
/// A focus mark draws wholly inside its widget's own rectangle, so the
/// focus path passes an empty viewport: it opens no list that would need one.
fn ctx(rect: Rect, viewport: Rect, scale: Scale, theme: &Theme) -> DemoContext<'_> {
    DemoContext {
        rect,
        viewport,
        scale,
        theme,
    }
}

/// A part of the gallery that takes input: the tab strip, or one demo item of
/// the current panel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    /// The tab strip.
    Tabs,
    /// The demo item at this index in the current panel.
    Item(usize),
}

/// Where the gallery's parts are for one event: the tab strip and every item
/// of the current panel.
struct Placement {
    viewport: Rect,
    tabs: Rect,
    items: Vec<Rect>,
}

impl Placement {
    /// The part at `at`, if it is over one.
    fn part_at(&self, at: Point) -> Option<Part> {
        if self.tabs.contains(at) {
            return Some(Part::Tabs);
        }
        self.items
            .iter()
            .position(|rect| rect.contains(at))
            .map(Part::Item)
    }
}

/// The widget-gallery client content: a tab strip plus the current family's
/// panel of demo widgets.
///
/// The gallery owns one panel of [`DemoItem`]s per [`GalleryTab`], built once
/// at construction. Rendering draws the tab strip and the selected panel; a
/// demo widget's value change is reflected straight back into it (the gallery
/// is the control's owner). A pointer event reaches the part under the
/// pointer, and a move away tells the part it left; a press is held by the
/// part it began on until its release, and an open choice list holds every
/// event until it closes. Keys go to the part holding keyboard focus, which a
/// press moves and a hover never does.
#[derive(Clone, Debug)]
pub struct Gallery {
    tabs: Tabs,
    panels: Vec<Vec<DemoItem>>,
    current: GalleryTab,
    focus: Part,
    /// Where the pointer is, once it has been anywhere.
    pointer: Option<Point>,
    /// The part the pointer last moved over, which it tells on leaving.
    hovered: Option<Part>,
    /// The part a primary press is held on, which every pointer event reaches
    /// until the release.
    pressed: Option<Part>,
}

impl Default for Gallery {
    fn default() -> Self {
        Self::new()
    }
}

impl Gallery {
    /// Build the gallery with every family's panel populated and the first tab
    /// selected.
    #[must_use]
    pub fn new() -> Self {
        let mut tabs = Tabs::new(
            GalleryTab::ALL
                .iter()
                .map(|t| Tab::new(t.title()))
                .collect::<Vec<_>>(),
        );
        tabs.adopt_selected(0);
        let panels = GalleryTab::ALL.iter().map(|t| panels::build(*t)).collect();
        Self {
            tabs,
            panels,
            current: GalleryTab::Buttons,
            focus: Part::Tabs,
            pointer: None,
            hovered: None,
            pressed: None,
        }
    }

    /// The currently selected tab.
    #[must_use]
    pub fn current_tab(&self) -> GalleryTab {
        self.current
    }

    /// The demo items of the currently selected panel.
    #[must_use]
    pub fn current_panel(&self) -> &[DemoItem] {
        &self.panels[self.current.index()]
    }

    /// The tab strip and content rectangles within `viewport`.
    fn layout(viewport: Rect, scale: Scale, theme: &Theme) -> (Rect, Rect) {
        let tab_h = scale
            .scale_length(theme.metrics().control_height)
            .max(1)
            .min(viewport.height);
        let tabs = Rect::new(viewport.left(), viewport.top(), viewport.width, tab_h);
        let content = Rect::new(
            viewport.left(),
            viewport.top() + i32::try_from(tab_h).unwrap_or(0),
            viewport.width,
            viewport.height.saturating_sub(tab_h),
        );
        (tabs, content)
    }

    /// The widget rectangle of each demo item in the current panel, laid out
    /// as a column within `content`. The caption occupies a fixed left column
    /// and the widget fills (or takes its fixed width in) the remainder.
    fn item_rects(&self, content: Rect, scale: Scale, theme: &Theme) -> Vec<Rect> {
        let pad = scale.scale_length(theme.metrics().control_inset).max(2);
        let gap = scale.scale_length(theme.metrics().control_gap).max(2);
        let caption_w = scale
            .scale_length(CAPTION_WIDTH)
            .min(content.width.saturating_sub(pad.saturating_mul(2)) / 2);
        let x0 = content.left() + i32::try_from(pad).unwrap_or(0);
        let wx = x0 + i32::try_from(caption_w + gap).unwrap_or(0);
        let right = content.right() - i32::try_from(pad).unwrap_or(0);
        let fill_w = u32::try_from((right - wx).max(0)).unwrap_or(0);
        let mut y = content.top() + i32::try_from(pad).unwrap_or(0);
        let mut rects = Vec::new();
        for item in &self.panels[self.current.index()] {
            let ih = scale.scale_length(item.height).max(1);
            let ww = item
                .width
                .map_or(fill_w, |w| scale.scale_length(w).min(fill_w));
            rects.push(Rect::new(wx, y, ww, ih));
            y += i32::try_from(ih + gap).unwrap_or(0);
        }
        rects
    }

    /// The caption rectangle (left column) aligned with widget `rect`.
    fn caption_rect(rect: Rect, content: Rect, scale: Scale, theme: &Theme) -> Rect {
        let pad = scale.scale_length(theme.metrics().control_inset).max(2);
        let caption_w = scale
            .scale_length(CAPTION_WIDTH)
            .min(content.width.saturating_sub(pad.saturating_mul(2)) / 2);
        Rect::new(
            content.left() + i32::try_from(pad).unwrap_or(0),
            rect.top(),
            caption_w,
            rect.height,
        )
    }

    /// Draw the gallery client content into `surface` filling `viewport`.
    pub fn render(
        &self,
        surface: &mut Surface,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) {
        let palette = theme.palette();
        surface.fill_rect(
            0,
            0,
            viewport.width,
            viewport.height,
            Color::from(palette.surface),
        );
        let (tabs_rect, content) = Self::layout(viewport, scale, theme);
        // The gallery shows the built-in glyph: it is a control catalogue, not
        // an application with icon artwork of its own to supply.
        self.tabs
            .render(surface, tabs_rect, scale, theme, &mut NoArtwork);

        let rects = self.item_rects(content, scale, theme);
        let glyph_h = font.glyph_height();
        for (item, rect) in self.panels[self.current.index()].iter().zip(&rects) {
            let caption = Self::caption_rect(*rect, content, scale, theme);
            let text = font.truncate_to_width(&item.caption, caption.width);
            let ty = caption.top()
                + (i32::try_from(caption.height).unwrap_or(0)
                    - i32::try_from(glyph_h).unwrap_or(0))
                .max(0)
                    / 2;
            font.draw_text(
                surface,
                caption.left(),
                ty,
                text,
                Color::from(palette.on_surface),
            );
            item.widget
                .render(surface, ctx(*rect, viewport, scale, theme));
        }
    }

    /// Route one pointer event, returning whether the view should repaint:
    /// whenever anything was reported or a value changed.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let mut drew = damage::sink();
        let changed = self.route_pointer(event, viewport, scale, theme, &mut drew);
        reported(&drew, damage) || changed
    }

    /// Where a pointer event goes: to the part holding the pointer if one
    /// is, else to the part under it and, for a move, to the part it left.
    fn route_pointer(
        &mut self,
        event: &InputEvent,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = Some(*to);
        }
        let placed = self.placement(viewport, scale, theme);
        if let Some(holder) = self.holder() {
            let mut changed = self.deliver(holder, event, &placed, scale, theme, damage);
            if is_primary_release(event) && self.pressed.take().is_some() {
                // The press kept every other part from seeing the pointer
                // arrive, so the part it was released over is told now.
                changed |= self.follow_pointer(viewport, scale, theme, damage);
            }
            return changed;
        }
        let under = self.pointer.and_then(|at| placed.part_at(at));
        let mut changed = false;
        if matches!(event, InputEvent::PointerMoved { .. }) {
            if let Some(left) = self.hovered.filter(|part| Some(*part) != under) {
                changed |= self.deliver(left, event, &placed, scale, theme, damage);
            }
            self.hovered = under;
        }
        let Some(part) = under else {
            return changed;
        };
        if is_primary_press(event) {
            self.pressed = Some(part);
            if matches!(part, Part::Item(_)) {
                self.set_focus(part, viewport, scale, theme, damage);
            }
        }
        self.deliver(part, event, &placed, scale, theme, damage) | changed
    }

    /// Deliver the pointer again where it rests, once what lies under it has
    /// changed without it moving.
    fn follow_pointer(
        &mut self,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(at) = self.pointer else {
            return false;
        };
        let resting = InputEvent::PointerMoved { to: at };
        self.route_pointer(&resting, viewport, scale, theme, damage)
    }

    /// The part holding the pointer: the one a primary press is held on, or a
    /// widget showing a choice list, which holds it until the list closes.
    fn holder(&self) -> Option<Part> {
        self.pressed.or_else(|| self.listing().map(Part::Item))
    }

    /// The item showing a choice list, if one is.
    fn listing(&self) -> Option<usize> {
        self.current_panel()
            .iter()
            .position(|item| item.widget.holds_pointer())
    }

    /// Where every part of the gallery is for `viewport`, resolved once for
    /// every delivery one event makes.
    fn placement(&self, viewport: Rect, scale: Scale, theme: &Theme) -> Placement {
        let (tabs, content) = Self::layout(viewport, scale, theme);
        Placement {
            viewport,
            tabs,
            items: self.item_rects(content, scale, theme),
        }
    }

    /// Hand `event` to `part`, answering whether a value changed.
    fn deliver(
        &mut self,
        part: Part,
        event: &InputEvent,
        placed: &Placement,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        match part {
            Part::Tabs => match self
                .tabs
                .on_pointer(event, placed.tabs, scale, theme, damage)
            {
                Some(TabsAction::Selected { index }) => {
                    self.select_index(index, placed.viewport, scale, theme, damage)
                }
                None => false,
            },
            Part::Item(idx) => {
                let (Some(rect), Some(item)) = (
                    placed.items.get(idx).copied(),
                    self.panels[self.current.index()].get_mut(idx),
                ) else {
                    return false;
                };
                let changed =
                    item.widget
                        .on_pointer(event, ctx(rect, placed.viewport, scale, theme), damage);
                if changed {
                    self.enforce_radio_group(idx, &placed.items, damage);
                }
                changed
            }
        }
    }

    /// Route one key press, returning whether the view should repaint:
    /// whenever anything was reported or a value changed. `Tab` and
    /// `Shift+Tab` move focus between the tab strip and the interactive demo
    /// widgets; every other key goes to the focused part.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let mut drew = damage::sink();
        let shown = self.current;
        let mut changed = self.route_key(key, modifiers, viewport, scale, theme, &mut drew);
        // A panel switched in beneath a resting pointer shows what it hovers.
        if self.current != shown {
            changed |= self.follow_pointer(viewport, scale, theme, &mut drew);
        }
        reported(&drew, damage) || changed
    }

    /// Hand one key press to the focused part, answering whether a value
    /// changed.
    ///
    /// An open choice list holds the keyboard as it holds the pointer, so
    /// `Tab` reaches it rather than walking focus off it and leaving it open.
    fn route_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        if key == Key::Named(tairix_input::NamedKey::Tab) && self.listing().is_none() {
            self.focus_step(!modifiers.shift, viewport, scale, theme, damage);
            return true;
        }
        // The focused widget's own rectangle comes from the same layout the
        // render and pointer paths use, so a key reports the pixels it changed.
        let (tabs, content) = Self::layout(viewport, scale, theme);
        match self.focus {
            Part::Tabs => match self.tabs.on_key(key, tabs, scale, theme, damage) {
                Some(TabsAction::Selected { index }) => {
                    self.select_index(index, viewport, scale, theme, damage)
                }
                None => false,
            },
            Part::Item(idx) => {
                let rects = self.item_rects(content, scale, theme);
                let rect = rects.get(idx).copied().unwrap_or(Rect::EMPTY);
                let Some(item) = self.panels[self.current.index()].get_mut(idx) else {
                    return false;
                };
                let changed =
                    item.widget
                        .on_key(key, modifiers, ctx(rect, viewport, scale, theme), damage);
                if changed {
                    self.enforce_radio_group(idx, &rects, damage);
                }
                changed
            }
        }
    }

    /// Select the tab at `index`, returning whether it changed.
    ///
    /// A different panel is drawn, so the whole content band is reported; the
    /// strip reports the two tab plates itself. The widget the pointer was
    /// over in the panel put away is told the pointer left, so it does not
    /// show a hover on return that nothing is causing.
    fn select_index(
        &mut self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(tab) = GalleryTab::from_index(index) else {
            return false;
        };
        if tab == self.current {
            return false;
        }
        // A widget still held by a press is left alone: telling it the
        // pointer moved would drag its value there.
        if let Some(Part::Item(idx)) = self.hovered.filter(|_| self.pressed != self.hovered) {
            self.leave(idx, viewport, scale, theme, damage);
        }
        self.hovered = self.hovered.filter(|part| *part == Part::Tabs);
        self.pressed = self.pressed.filter(|part| *part == Part::Tabs);
        self.current = tab;
        let (tabs, content) = Self::layout(viewport, scale, theme);
        self.tabs.set_selected(index, tabs, scale, theme, damage);
        damage.add(content);
        self.set_focus(Part::Tabs, viewport, scale, theme, damage);
        true
    }

    /// Tell item `idx` of the current panel the pointer is not over it, so it
    /// drops the hover look it would otherwise keep while hidden.
    fn leave(
        &mut self,
        idx: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let (_, content) = Self::layout(viewport, scale, theme);
        let Some(rect) = self.item_rects(content, scale, theme).get(idx).copied() else {
            return;
        };
        let away = InputEvent::PointerMoved {
            to: Point::new(rect.left(), rect.top().saturating_sub(1)),
        };
        if let Some(item) = self.panels[self.current.index()].get_mut(idx) {
            item.widget
                .on_pointer(&away, ctx(rect, viewport, scale, theme), damage);
        }
    }

    /// Move focus to `focus`, updating the widgets' and tab strip's focus
    /// marks so exactly one part reads as focused.
    ///
    /// Every widget's mark is a function of [`Self::focus`], so the ring can only
    /// move between the item it left and the item it arrives on: those are what
    /// the ring costs, and the strip reports its own cell when focus lands there.
    fn set_focus(
        &mut self,
        focus: Part,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let (tabs, content) = Self::layout(viewport, scale, theme);
        let rects = self.item_rects(content, scale, theme);
        damage::move_mark(
            Self::focused_item(self.focus),
            Self::focused_item(focus),
            |idx| rects.get(idx).copied(),
            damage,
        );
        for (idx, item) in self.panels[self.current.index()].iter_mut().enumerate() {
            let rect = rects.get(idx).copied().unwrap_or(Rect::EMPTY);
            item.widget
                .set_focused(false, ctx(rect, Rect::EMPTY, scale, theme), damage);
        }
        self.tabs.set_current(None, tabs, scale, theme, damage);
        self.focus = focus;
        match focus {
            Part::Tabs => {
                self.tabs
                    .set_current(Some(self.current.index()), tabs, scale, theme, damage);
            }
            Part::Item(idx) => {
                let rect = rects.get(idx).copied().unwrap_or(Rect::EMPTY);
                if let Some(item) = self.panels[self.current.index()].get_mut(idx) {
                    item.widget
                        .set_focused(true, ctx(rect, Rect::EMPTY, scale, theme), damage);
                }
            }
        }
    }

    /// The panel item `focus` sits on, or `None` when it sits on the tab strip.
    fn focused_item(focus: Part) -> Option<usize> {
        match focus {
            Part::Tabs => None,
            Part::Item(idx) => Some(idx),
        }
    }

    /// Advance keyboard focus forward (`true`) or backward (`false`) through
    /// the tab strip and the panel's interactive widgets, wrapping around.
    fn focus_step(
        &mut self,
        forward: bool,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let interactive: Vec<usize> = self.panels[self.current.index()]
            .iter()
            .enumerate()
            .filter(|(_, item)| item.widget.is_interactive())
            .map(|(i, _)| i)
            .collect();
        // The focus ring: the tab strip, then each interactive item in order.
        let current_pos = match self.focus {
            Part::Tabs => 0,
            Part::Item(idx) => interactive
                .iter()
                .position(|&i| i == idx)
                .map_or(0, |p| p + 1),
        };
        let ring_len = interactive.len() + 1;
        let next_pos = if forward {
            (current_pos + 1) % ring_len
        } else {
            (current_pos + ring_len - 1) % ring_len
        };
        let next = if next_pos == 0 {
            Part::Tabs
        } else {
            Part::Item(interactive[next_pos - 1])
        };
        self.set_focus(next, viewport, scale, theme, damage);
    }

    /// The on-screen widget rectangle of demo item `index` in the current
    /// panel, for pointer-routing tests.
    #[cfg(test)]
    pub(crate) fn widget_rect_for_test(
        &self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let (_, content) = Self::layout(viewport, scale, theme);
        self.item_rects(content, scale, theme).get(index).copied()
    }

    /// Keep a radio group single-selection: if the item just actuated is a now
    /// selected radio, clear every other radio in the panel.
    ///
    /// A cleared radio's plate changes, and only this owner knows where it drew
    /// it, so each one it actually clears is reported.
    fn enforce_radio_group(&mut self, idx: usize, rects: &[Rect], damage: &mut Region) {
        let panel = &mut self.panels[self.current.index()];
        if panel
            .get(idx)
            .is_some_and(|it| it.widget.is_selected_radio())
        {
            for (i, item) in panel.iter_mut().enumerate() {
                if i != idx && item.widget.clear_radio() {
                    damage.add(rects.get(i).copied().unwrap_or(Rect::EMPTY));
                }
            }
        }
    }
}

/// Fold what a round drew into the caller's `damage`, answering whether it
/// drew anything.
fn reported(drew: &Region, damage: &mut Region) -> bool {
    for rect in drew.rects() {
        damage.add(*rect);
    }
    !drew.is_empty()
}

/// Whether `event` is a primary press, which is what moves keyboard focus.
const fn is_primary_press(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::PointerPressed {
            button: PointerButton::Primary
        }
    )
}

/// Whether `event` is a primary release, which ends a held press.
const fn is_primary_release(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::PointerReleased {
            button: PointerButton::Primary
        }
    )
}
