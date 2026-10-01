//! The [`Gallery`]: the client-content composition the `widgets.app` bundle's
//! `Run` binary presents.
//!
//! The window's furniture (frame, title bar, command buttons) is drawn
//! server-side by the compositor, so the gallery renders only client content:
//! a [`Tabs`] strip selecting one control family and a panel of captioned demo
//! widgets for the selected family. Each family is one [`GalleryTab`]; each
//! panel is a column of [`DemoItem`]s laid out top-to-bottom, a caption on the
//! left and the live [`DemoWidget`] on the right, scrolled beneath the strip
//! when it is taller than the window. Pointer and key events are routed to the
//! tab strip, the scroll bar, or the demo widget under focus; nothing here
//! performs privileged work.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{
    damage, Keystroke, ScrollBar, ScrollModel, ScrollOrientation, ScrollRange, ScrollView, Tab,
    Tabs, TabsAction,
};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, PointerButton};
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

/// A part of the gallery that takes input: the tab strip, the scroll bar, or
/// one demo item of the current panel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    /// The tab strip.
    Tabs,
    /// The bar the panel's column scrolls by.
    Bar,
    /// The demo item at this index in the current panel.
    Item(usize),
}

/// Where the gallery's parts are for one viewport.
struct Frame {
    viewport: Rect,
    tabs: Rect,
    /// Everything beneath the strip: the body and the bar beside it.
    content: Rect,
    /// Present while the column is taller than the body.
    bar: Option<Rect>,
    /// The panel's column scrolled into the body.
    view: ScrollView,
    /// The client a choice list must fit inside, in the column's layout.
    client: Rect,
    /// Every item's widget rectangle in the column's unscrolled layout.
    items: Vec<Rect>,
}

impl Frame {
    /// The part at `at`, if it is over one.
    fn part_at(&self, at: Point) -> Option<Part> {
        if self.tabs.contains(at) {
            return Some(Part::Tabs);
        }
        if self.bar.is_some_and(|bar| bar.contains(at)) {
            return Some(Part::Bar);
        }
        let at = self.view.to_content(at)?;
        self.items
            .iter()
            .position(|rect| rect.contains(at))
            .map(Part::Item)
    }

    /// The view an item is reached through: the whole client while it shows a
    /// choice list, which hangs out of the body over the strip and the bar.
    fn reach(&self, listing: bool) -> ScrollView {
        if listing {
            self.view.confined_to(self.viewport)
        } else {
            self.view
        }
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
    /// Holds the column's one scroll position, re-clamped against the panel
    /// shown and the viewport at every layout.
    scroll: ScrollBar,
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
            scroll: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::EMPTY, 1),
            ),
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
    /// as a column from the top of `column`, unscrolled. The caption occupies a
    /// fixed left column and the widget fills (or takes its fixed width in) the
    /// remainder.
    fn item_rects(&self, column: Rect, scale: Scale, theme: &Theme) -> Vec<Rect> {
        let pad = scale.scale_length(theme.metrics().control_inset).max(2);
        let gap = scale.scale_length(theme.metrics().control_gap).max(2);
        let caption_w = scale
            .scale_length(CAPTION_WIDTH)
            .min(column.width.saturating_sub(pad.saturating_mul(2)) / 2);
        let x0 = column.left() + to_i32(pad);
        let wx = x0 + to_i32(caption_w + gap);
        let right = column.right() - to_i32(pad);
        let fill_w = u32::try_from((right - wx).max(0)).unwrap_or(0);
        let mut y = column.top() + to_i32(pad);
        let mut rects = Vec::new();
        for item in &self.panels[self.current.index()] {
            let ih = scale.scale_length(item.height).max(1);
            let ww = item
                .width
                .map_or(fill_w, |w| scale.scale_length(w).min(fill_w));
            rects.push(Rect::new(wx, y, ww, ih));
            y = y.saturating_add(to_i32(ih + gap));
        }
        rects
    }

    /// How tall the current panel's column is laid out: its items, their gaps,
    /// and the inset at either end. The width never changes it.
    fn column_height(&self, scale: Scale, theme: &Theme) -> u32 {
        let pad = scale.scale_length(theme.metrics().control_inset).max(2);
        let gap = scale.scale_length(theme.metrics().control_gap).max(2);
        let items = self.panels[self.current.index()]
            .iter()
            .map(|item| scale.scale_length(item.height).max(1))
            .fold(0u32, |sum, height| {
                sum.saturating_add(height).saturating_add(gap)
            });
        // The last item is followed by the inset, not a gap.
        pad.saturating_add(items.saturating_sub(gap))
            .saturating_add(pad)
    }

    /// Where every part of the gallery is for `viewport`, and the scroll model
    /// the column implies there: the held offset re-clamped against the
    /// current panel.
    fn resolve(&self, viewport: Rect, scale: Scale, theme: &Theme) -> (Frame, ScrollModel) {
        let (tabs, content) = Self::layout(viewport, scale, theme);
        let metrics = theme.metrics();
        let column = self.column_height(scale, theme);
        let breadth = scale
            .scale_length(metrics.scrollbar_breadth)
            .max(1)
            .min(content.width);
        let (body, bar) = if column > content.height {
            let body_w = content.width - breadth;
            let bar = Rect::new(
                content.left() + to_i32(body_w),
                content.top(),
                breadth,
                content.height,
            );
            (
                Rect {
                    width: body_w,
                    ..content
                },
                Some(bar),
            )
        } else {
            (content, None)
        };
        let line = scale.scale_length(metrics.control_height.saturating_add(metrics.control_gap));
        let model = ScrollModel::in_pixels(
            self.scroll
                .model()
                .range()
                .resize(u64::from(column), u64::from(body.height)),
            u64::from(line.max(1)),
        );
        let view = ScrollView::new(ScrollOrientation::Vertical, body, model.offset());
        let client = Rect::new(
            viewport.left(),
            viewport.top().saturating_add(to_i32(view.offset())),
            viewport.width,
            viewport.height,
        );
        let frame = Frame {
            viewport,
            tabs,
            content,
            bar,
            view,
            client,
            items: self.item_rects(body, scale, theme),
        };
        (frame, model)
    }

    /// [`resolve`](Self::resolve), adopting the scroll model it implies: the
    /// bar holds the column's only scroll position.
    fn frame(&mut self, viewport: Rect, scale: Scale, theme: &Theme) -> Frame {
        let (frame, model) = self.resolve(viewport, scale, theme);
        self.scroll.set_model(model);
        frame
    }

    /// The caption rectangle (left column) aligned with widget `rect`, both in
    /// the column's layout.
    fn caption_rect(rect: Rect, body: Rect, scale: Scale, theme: &Theme) -> Rect {
        let pad = scale.scale_length(theme.metrics().control_inset).max(2);
        let caption_w = scale
            .scale_length(CAPTION_WIDTH)
            .min(body.width.saturating_sub(pad.saturating_mul(2)) / 2);
        Rect::new(
            body.left() + to_i32(pad),
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
        let (frame, model) = self.resolve(viewport, scale, theme);
        // The gallery shows the built-in glyph: it is a control catalogue, not
        // an application with icon artwork of its own to supply.
        self.tabs
            .render(surface, frame.tabs, scale, theme, &mut NoArtwork);

        let body = frame.view.viewport();
        let panel = &self.panels[self.current.index()];
        frame.view.paint(surface, |column| {
            for (item, rect) in panel.iter().zip(&frame.items) {
                let row = Rect::new(body.left(), rect.top(), body.width, rect.height);
                if frame.view.to_window(row).is_none() {
                    continue;
                }
                let caption = Self::caption_rect(*rect, body, scale, theme);
                let text = font.truncate_to_width(&item.caption, caption.width);
                let ty = font.centred_top(caption.top(), caption.height);
                font.draw_text(
                    column,
                    caption.left(),
                    ty,
                    text,
                    Color::from(palette.on_surface),
                );
                item.widget
                    .render(column, ctx(*rect, frame.client, scale, theme));
            }
        });
        if let Some(bar) = frame.bar {
            let mut shown = self.scroll;
            shown.set_model(model);
            shown.render(surface, bar, scale, theme);
        }
        // An open choice list hangs over the strip and the bar as well as the
        // items beneath it, so it is drawn last and clipped to the client.
        let open = self
            .listing()
            .and_then(|index| Some((panel.get(index)?, *frame.items.get(index)?)));
        if let Some((item, rect)) = open {
            frame.reach(true).paint(surface, |client| {
                item.widget
                    .render_popup(client, ctx(rect, frame.client, scale, theme));
            });
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
    /// is, else to the part under it and, for a move, to the part it left. A
    /// wheel turn the part under the pointer does not use scrolls the column.
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
        let frame = self.frame(viewport, scale, theme);
        if let Some(holder) = self.holder() {
            let mut changed = self.deliver(holder, event, &frame, scale, theme, damage);
            if is_primary_release(event) && self.pressed.take().is_some() {
                // The press kept every other part from seeing the pointer
                // arrive, so the part it was released over is told now.
                changed |= self.follow_pointer(viewport, scale, theme, damage);
            }
            return changed;
        }
        let under = self.pointer.and_then(|at| frame.part_at(at));
        if let InputEvent::PointerScrolled { dy, .. } = *event {
            let used =
                under.is_some_and(|part| self.deliver(part, event, &frame, scale, theme, damage));
            return used || self.scroll_column(dy, &frame, (viewport, scale, theme), damage);
        }
        let mut changed = false;
        if matches!(event, InputEvent::PointerMoved { .. }) {
            if let Some(left) = self.hovered.filter(|part| Some(*part) != under) {
                changed |= self.deliver(left, event, &frame, scale, theme, damage);
            }
            self.hovered = under;
        }
        let Some(part) = under else {
            return changed;
        };
        if is_primary_press(event) {
            self.pressed = Some(part);
            if matches!(part, Part::Item(_) | Part::Bar) {
                self.set_focus(part, &frame, scale, theme, damage);
            }
        }
        self.deliver(part, event, &frame, scale, theme, damage) | changed
    }

    /// Scroll the column by a wheel turn the pointer's part did not use, when
    /// the pointer is over the body, answering whether it moved.
    ///
    /// The items move beneath a resting pointer, so it is delivered again for
    /// the hover to follow the item now under it.
    fn scroll_column(
        &mut self,
        dy: i32,
        frame: &Frame,
        (viewport, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let over_body = self
            .pointer
            .is_some_and(|at| frame.view.viewport().contains(at));
        let Some(bar) = frame.bar.filter(|_| over_body) else {
            return false;
        };
        if self.scroll.wheel(0, dy, scale, bar, damage).is_none() {
            return false;
        }
        damage.add(frame.view.viewport());
        self.follow_pointer(viewport, scale, theme, damage);
        true
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

    /// Hand `event` to `part`, answering whether a value changed.
    fn deliver(
        &mut self,
        part: Part,
        event: &InputEvent,
        frame: &Frame,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        match part {
            Part::Tabs => match self
                .tabs
                .on_pointer(event, frame.tabs, scale, theme, damage)
            {
                Some(TabsAction::Selected { index }) => {
                    self.select_index(index, frame, scale, theme, damage)
                }
                Some(TabsAction::Disclose { .. }) | None => false,
            },
            Part::Bar => {
                let Some(bar) = frame.bar else {
                    return false;
                };
                if self
                    .scroll
                    .on_pointer(event, bar, scale, theme, damage)
                    .is_none()
                {
                    return false;
                }
                damage.add(frame.view.viewport());
                true
            }
            Part::Item(idx) => {
                let (Some(rect), Some(item)) = (
                    frame.items.get(idx).copied(),
                    self.panels[self.current.index()].get_mut(idx),
                ) else {
                    return false;
                };
                let listed = item.widget.holds_pointer();
                let event = frame.reach(listed).event_in_layout(event);
                let mut drew = damage::sink();
                let changed = item.widget.on_pointer(
                    &event,
                    ctx(rect, frame.client, scale, theme),
                    &mut drew,
                );
                let listed = listed || item.widget.holds_pointer();
                if changed {
                    self.enforce_radio_group(idx, &frame.items, &mut drew);
                }
                frame.reach(listed).report(&drew, damage);
                changed
            }
        }
    }

    /// Route one key press, returning whether the view should repaint:
    /// whenever anything was reported or a value changed. `Tab` and
    /// `Shift+Tab` move focus between the tab strip, the interactive demo
    /// widgets and the scroll bar; every other key goes to the focused part.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let mut drew = damage::sink();
        let shown = (self.current, self.scroll.model().offset());
        let mut changed = self.route_key(stroke, viewport, scale, theme, &mut drew);
        // A panel switched in, or the column scrolled, beneath a resting
        // pointer shows what it now hovers.
        if (self.current, self.scroll.model().offset()) != shown {
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
        stroke: Keystroke,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let frame = self.frame(viewport, scale, theme);
        let key = stroke.key;
        if key == Key::Named(tairix_input::NamedKey::Tab) && self.listing().is_none() {
            self.focus_step(!stroke.modifiers.shift, &frame, scale, theme, damage);
            return true;
        }
        match self.focus {
            Part::Tabs => match self.tabs.on_key(key, frame.tabs, scale, theme, damage) {
                Some(TabsAction::Selected { index }) => {
                    self.select_index(index, &frame, scale, theme, damage)
                }
                Some(TabsAction::Disclose { .. }) | None => false,
            },
            Part::Bar => {
                let Some(bar) = frame.bar else {
                    return false;
                };
                if self.scroll.on_key(key, bar, damage).is_none() {
                    return false;
                }
                damage.add(frame.view.viewport());
                true
            }
            Part::Item(idx) => {
                let rect = frame.items.get(idx).copied().unwrap_or(Rect::EMPTY);
                let Some(item) = self.panels[self.current.index()].get_mut(idx) else {
                    return false;
                };
                let listed = item.widget.holds_pointer();
                let mut drew = damage::sink();
                let changed =
                    item.widget
                        .on_key(stroke, ctx(rect, frame.client, scale, theme), &mut drew);
                let listed = listed || item.widget.holds_pointer();
                if changed {
                    self.enforce_radio_group(idx, &frame.items, &mut drew);
                }
                frame.reach(listed).report(&drew, damage);
                changed
            }
        }
    }

    /// Select the tab at `index`, returning whether it changed.
    ///
    /// A different panel is drawn from its own top, so the whole content band
    /// is reported; the strip reports the two tab plates itself. The widget
    /// the pointer was over in the panel put away is told the pointer left, so
    /// it does not show a hover on return that nothing is causing.
    fn select_index(
        &mut self,
        index: usize,
        frame: &Frame,
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
            self.leave(idx, frame, scale, theme, damage);
        }
        self.hovered = self.hovered.filter(|part| *part == Part::Tabs);
        self.pressed = self.pressed.filter(|part| *part == Part::Tabs);
        self.current = tab;
        self.scroll.set_model(self.scroll.model().to_start());
        self.tabs
            .set_selected(index, frame.tabs, scale, theme, damage);
        damage.add(frame.content);
        self.set_focus(Part::Tabs, frame, scale, theme, damage);
        true
    }

    /// Tell item `idx` of the current panel the pointer is not over it, so it
    /// drops the hover look it would otherwise keep while hidden.
    fn leave(
        &mut self,
        idx: usize,
        frame: &Frame,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let Some(rect) = frame.items.get(idx).copied() else {
            return;
        };
        let away = InputEvent::PointerMoved {
            to: Point::new(rect.left(), rect.top().saturating_sub(1)),
        };
        let mut drew = damage::sink();
        if let Some(item) = self.panels[self.current.index()].get_mut(idx) {
            item.widget
                .on_pointer(&away, ctx(rect, frame.client, scale, theme), &mut drew);
        }
        frame.view.report(&drew, damage);
    }

    /// Move focus to `focus`, updating the widgets', the bar's and the tab
    /// strip's focus marks so exactly one part reads as focused.
    ///
    /// Every widget's mark is a function of [`Self::focus`], so the ring can only
    /// move between the part it left and the part it arrives on: those are what
    /// the ring costs, and the strip reports its own cell when focus lands there.
    fn set_focus(
        &mut self,
        focus: Part,
        frame: &Frame,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let mut drew = damage::sink();
        damage::move_mark(
            Self::focused_item(self.focus),
            Self::focused_item(focus),
            |idx| frame.items.get(idx).copied(),
            &mut drew,
        );
        for (idx, item) in self.panels[self.current.index()].iter_mut().enumerate() {
            let rect = frame.items.get(idx).copied().unwrap_or(Rect::EMPTY);
            item.widget
                .set_focused(false, ctx(rect, Rect::EMPTY, scale, theme), &mut drew);
        }
        if let Some(bar) = frame
            .bar
            .filter(|_| (self.focus == Part::Bar) != (focus == Part::Bar))
        {
            damage.add(bar);
        }
        self.scroll.set_focused(focus == Part::Bar);
        self.tabs
            .set_current(None, frame.tabs, scale, theme, damage);
        self.focus = focus;
        match focus {
            Part::Tabs => {
                self.tabs
                    .set_current(Some(self.current.index()), frame.tabs, scale, theme, damage);
            }
            Part::Bar => {}
            Part::Item(idx) => {
                let rect = frame.items.get(idx).copied().unwrap_or(Rect::EMPTY);
                if let Some(item) = self.panels[self.current.index()].get_mut(idx) {
                    item.widget
                        .set_focused(true, ctx(rect, Rect::EMPTY, scale, theme), &mut drew);
                }
            }
        }
        frame.view.report(&drew, damage);
    }

    /// The panel item `focus` sits on, or `None` when it sits on the tab strip
    /// or the bar.
    fn focused_item(focus: Part) -> Option<usize> {
        match focus {
            Part::Tabs | Part::Bar => None,
            Part::Item(idx) => Some(idx),
        }
    }

    /// Advance keyboard focus forward (`true`) or backward (`false`) through
    /// the tab strip, the panel's interactive widgets and, while the column
    /// scrolls, the bar, wrapping around; the column scrolls the least that
    /// shows a widget focus lands on.
    fn focus_step(
        &mut self,
        forward: bool,
        frame: &Frame,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let mut ring: Vec<Part> = core::iter::once(Part::Tabs)
            .chain(
                self.panels[self.current.index()]
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.widget.is_interactive())
                    .map(|(i, _)| Part::Item(i)),
            )
            .collect();
        if frame.bar.is_some() {
            ring.push(Part::Bar);
        }
        let current_pos = ring
            .iter()
            .position(|&part| part == self.focus)
            .unwrap_or(0);
        let next_pos = if forward {
            (current_pos + 1) % ring.len()
        } else {
            (current_pos + ring.len() - 1) % ring.len()
        };
        let next = ring[next_pos];
        self.set_focus(next, frame, scale, theme, damage);
        if let Part::Item(idx) = next {
            self.reveal(idx, frame, damage);
        }
    }

    /// Scroll the column the least that shows item `idx`, reporting the band
    /// beneath the strip when it moved.
    fn reveal(&mut self, idx: usize, frame: &Frame, damage: &mut Region) {
        let Some(rect) = frame.items.get(idx) else {
            return;
        };
        let body = frame.view.viewport();
        let model = self.scroll.model();
        let start = u64::try_from(rect.top().saturating_sub(body.top())).unwrap_or(0);
        let revealed = model.revealing(start, u64::from(rect.height));
        if revealed.offset() != model.offset() {
            self.scroll.set_model(revealed);
            damage.add(frame.content);
        }
    }

    /// Where the widget of demo item `index` of the current panel shows in the
    /// window, or `None` while it is scrolled wholly out of the body, for
    /// pointer-routing tests.
    #[cfg(test)]
    pub(crate) fn widget_rect_for_test(
        &self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let (frame, _) = self.resolve(viewport, scale, theme);
        frame.view.to_window(*frame.items.get(index)?)
    }

    /// Demo item `index`'s widget rectangle in the column's unscrolled layout,
    /// and how far the column is scrolled, for reachability tests.
    #[cfg(test)]
    pub(crate) fn column_place_for_test(
        &self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(Rect, u32)> {
        let (frame, _) = self.resolve(viewport, scale, theme);
        Some((*frame.items.get(index)?, frame.view.offset()))
    }

    /// Keep a radio group single-selection: if the item just actuated is a now
    /// selected radio, clear every other radio in the panel.
    ///
    /// A cleared radio's plate changes, and only this owner knows where it drew
    /// it, so each one it actually clears is reported, in the column's layout.
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
