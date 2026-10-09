//! Paint's settings window: its categories down a sidebar, each category's
//! settings in a panel beside it, and *Restore defaults* beneath.
//!
//! It edits a copy of the settings in force and asks the application for
//! every change, which applies it to every window as it settles and writes it
//! off the loop; what the store then says comes back through
//! [`SettingsWindow::adopt`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::window_ipc::CursorShape;
use tairix_colour::Rgb;
use tairix_controls::{Button, Keystroke, NumberField, Tab, Tabs, TabsAction, TabsOrientation};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_window::docapp::{AppRequest, AppView, Outcome};

use crate::canvas::MAX_SIDE;
use crate::document::colours_for;
use crate::filter::Parameter;
use crate::layout::Faces;
use crate::pane::{Arrangement, PaneKind, Side};
use crate::panel::{switch, Cell, Panel, PanelEvent, PanelOutcome, Part, Plain};
use crate::preferences::{
    GridStyle, OpenAt, Preferences, Shades, Surround, CHECKER_SIDES, LEAST_GRID_OPACITY,
    MOST_GRID_SPACING, PIXEL_GRID_ZOOMS,
};
use crate::save::SaveFormat;
use crate::tool::Tool;

/// The window's size as it opens, in logical pixels.
const SIZE: (u32, u32) = (600, 440);

/// The least it may be made, in logical pixels.
const LEAST: (u32, u32) = (440, 320);

/// The sidebar's width, in logical pixels.
const SIDEBAR: u32 = 150;

/// The gap around and between the window's regions, in logical pixels.
const GAP: u32 = 12;

/// The zooms, in percent, the pixel grid may be asked to show from, after
/// *Never*.
const PIXEL_GRID_FROM: [u32; 11] = [200, 300, 400, 600, 800, 1200, 1600, 2400, 3200, 4800, 6400];

/// A group of settings, as the sidebar lists them.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Category {
    /// The tool a window starts with, and what a picture opens at.
    #[default]
    General,
    /// What *New picture* offers to start.
    NewPicture,
    /// The grid and the pixel grid.
    Grid,
    /// The checkerboard and what surrounds the picture.
    Canvas,
    /// The panes a new window opens with.
    Panes,
}

impl Category {
    /// Every category, in the sidebar's order.
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::NewPicture,
        Self::Grid,
        Self::Canvas,
        Self::Panes,
    ];

    /// What the sidebar calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::NewPicture => "New picture",
            Self::Grid => "Grid",
            Self::Canvas => "Canvas",
            Self::Panes => "Panes",
        }
    }

    const fn icon(self) -> IconKind {
        match self {
            Self::General => IconKind::Settings,
            Self::NewPicture => IconKind::Image,
            Self::Grid => IconKind::PixelGrid,
            Self::Canvas => IconKind::Wallpaper,
            Self::Panes => IconKind::Display,
        }
    }
}

/// What the settings window asks the application for.
// Made and carried out once an input event, never held in numbers, so the
// edit's two records travel by value rather than through an allocation each.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsRequest {
    /// The settings became `now` from `was`; `settled` once the interaction
    /// that changed them is over.
    Edit {
        /// Before.
        was: Preferences,
        /// After.
        now: Preferences,
        /// Whether to write them.
        settled: bool,
    },
    /// Put every setting back to what the layers beneath the user's own say.
    Restore,
    /// Take the panes of the window last worked in as the ones a new window
    /// opens with.
    TakePanes,
}

/// Where the window's regions stand.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SettingsLayout {
    window: Rect,
    sidebar: Rect,
    panel: Rect,
    footer: Rect,
}

/// Which region has the keyboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Keyboard {
    Sidebar,
    Panel,
    Footer,
}

impl Keyboard {
    /// The region Tab walks to from this one: the next when `forward`.
    const fn next(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Sidebar, true) | (Self::Footer, false) => Self::Panel,
            (Self::Panel, true) | (Self::Sidebar, false) => Self::Footer,
            (Self::Footer, true) | (Self::Panel, false) => Self::Sidebar,
        }
    }
}

/// The category at `index` of the sidebar.
fn category_at(index: usize) -> Category {
    Category::ALL.get(index).copied().unwrap_or_default()
}

/// The footer: *Restore defaults*, and what the store last refused.
const RESTORE: usize = 0;

/// The settings window.
#[derive(Clone, Debug)]
pub struct SettingsWindow {
    sidebar: Tabs,
    shown: Category,
    panel: Panel,
    footer: Panel,
    record: Preferences,
    said: Option<String>,
    keyboard: Option<Keyboard>,
    pointer: Point,
}

impl SettingsWindow {
    /// The window showing `record`, at `category`.
    #[must_use]
    pub fn new(record: Preferences, category: Category) -> Self {
        let tabs = Category::ALL
            .iter()
            .map(|category| Tab::new(category.label()).with_icon(category.icon()))
            .collect();
        let mut sidebar = Tabs::new(tabs).with_orientation(TabsOrientation::Vertical);
        let index = Category::ALL
            .iter()
            .position(|&at| at == category)
            .unwrap_or(0);
        sidebar.adopt_selected(index);
        let panel = panel_for(category, &record);
        Self {
            sidebar,
            shown: category,
            panel,
            footer: footer(None),
            record,
            said: None,
            keyboard: None,
            pointer: Point::new(-1, -1),
        }
    }

    /// The category on show.
    #[must_use]
    pub const fn category(&self) -> Category {
        self.shown
    }

    /// The settings the controls show.
    #[must_use]
    pub const fn record(&self) -> &Preferences {
        &self.record
    }

    /// Show `record`, the settings now in force, leaving a control in use as
    /// the user has it: its own settle writes it.
    pub fn adopt(&mut self, record: &Preferences, layout: &SettingsLayout, damage: &mut Region) {
        if *record == self.record || self.panel.holding() || self.panel.listing() {
            return;
        }
        self.record.clone_from(record);
        self.rebuild(layout, damage);
    }

    /// Say `message` beneath the settings, or nothing.
    pub fn say(&mut self, message: Option<String>, layout: &SettingsLayout, damage: &mut Region) {
        if message == self.said {
            return;
        }
        self.said = message;
        self.footer = footer(self.said.as_deref());
        damage.add(layout.footer);
    }

    /// Build the panel again for the record, the keyboard keeping its stop.
    fn rebuild(&mut self, layout: &SettingsLayout, damage: &mut Region) {
        let stop = self.panel.focus();
        self.panel = panel_for(self.shown, &self.record);
        if let Some(stop) = stop {
            if !self.panel.focus_at(stop) && self.keyboard == Some(Keyboard::Panel) {
                self.keyboard = None;
            }
        }
        damage.add(layout.panel);
    }

    fn show(&mut self, category: Category, layout: &SettingsLayout, damage: &mut Region) {
        if category != self.shown {
            self.shown = category;
            self.panel = panel_for(category, &self.record);
            damage.add(layout.panel);
        }
    }

    /// What a panel event asks for: the record edited, or a request.
    fn answered(
        &mut self,
        event: PanelEvent,
        layout: &SettingsLayout,
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest>> {
        if let (Category::Panes, PanelEvent::Pressed { button, .. }) = (self.shown, event) {
            return match button {
                0 => Outcome::asking(AppRequest::Own(SettingsRequest::TakePanes)),
                _ => self.edited(true, layout, damage, |record| {
                    record.panes = Arrangement::default();
                }),
            };
        }
        let settled = settled(event);
        let category = self.shown;
        self.edited(settled, layout, damage, |record| {
            edit(category, record, event);
        })
    }

    fn edited(
        &mut self,
        settled: bool,
        layout: &SettingsLayout,
        damage: &mut Region,
        change: impl FnOnce(&mut Preferences),
    ) -> Outcome<AppRequest<SettingsRequest>> {
        let was = self.record.clone();
        change(&mut self.record);
        tairix_appconf::Registry::normalise(&mut self.record);
        // A change another control shows the cause of — a format that holds
        // fewer colours, a choice that enables its fields — is shown at once.
        if reshapes(self.shown, &was, &self.record) {
            self.rebuild(layout, damage);
        }
        if was == self.record && !settled {
            return Outcome::none();
        }
        Outcome::asking(AppRequest::Own(SettingsRequest::Edit {
            was,
            now: self.record.clone(),
            settled,
        }))
    }

    fn panel_outcome(
        &mut self,
        outcome: PanelOutcome,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Outcome<AppRequest<SettingsRequest>>> {
        match outcome {
            PanelOutcome::Ignored => None,
            PanelOutcome::Event(event) => Some(self.answered(event, layout, damage)),
            PanelOutcome::Left { forward } => {
                let _ = self.enter(
                    Keyboard::Panel.next(forward),
                    forward,
                    layout,
                    context,
                    damage,
                );
                Some(Outcome::none())
            }
            PanelOutcome::Taken | PanelOutcome::Custom { .. } => Some(Outcome::none()),
        }
    }

    fn footer_outcome(
        &mut self,
        outcome: PanelOutcome,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Outcome<AppRequest<SettingsRequest>>> {
        match outcome {
            PanelOutcome::Ignored => None,
            PanelOutcome::Event(PanelEvent::Pressed { part: RESTORE, .. }) => {
                Some(Outcome::asking(AppRequest::Own(SettingsRequest::Restore)))
            }
            PanelOutcome::Left { forward } => {
                let _ = self.enter(
                    Keyboard::Footer.next(forward),
                    forward,
                    layout,
                    context,
                    damage,
                );
                Some(Outcome::none())
            }
            _ => Some(Outcome::none()),
        }
    }

    /// Give `region` the keyboard, entering it from its start when `forward`,
    /// and past it to the next when it has no stop; what the region left
    /// committed is answered.
    fn enter(
        &mut self,
        region: Keyboard,
        forward: bool,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<PanelEvent> {
        let committed = self.release(layout, context, damage);
        let mut region = region;
        for _ in 0..3 {
            let entered = match region {
                Keyboard::Sidebar => {
                    let (_, scale, theme) = context;
                    self.sidebar.set_current(
                        self.sidebar.selected(),
                        layout.sidebar,
                        scale,
                        theme,
                        damage,
                    );
                    true
                }
                Keyboard::Panel => self.panel.enter_focus(forward, layout.panel, damage),
                Keyboard::Footer => self.footer.enter_focus(forward, layout.footer, damage),
            };
            if entered {
                self.keyboard = Some(region);
                break;
            }
            region = region.next(forward);
        }
        committed
    }

    /// Take the keyboard from the region holding it, answering what a field
    /// it left committed.
    fn release(
        &mut self,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<PanelEvent> {
        match self.keyboard.take()? {
            Keyboard::Sidebar => {
                let (_, scale, theme) = context;
                self.sidebar
                    .set_current(None, layout.sidebar, scale, theme, damage);
                None
            }
            Keyboard::Panel => self.panel.blur(layout.panel, context, damage),
            Keyboard::Footer => {
                let _ = self.footer.blur(layout.footer, context, damage);
                None
            }
        }
    }

    fn key(
        &mut self,
        stroke: Keystroke,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest>> {
        let tab = stroke.key == Key::Named(NamedKey::Tab);
        let forward = !stroke.modifiers.shift;
        match self.keyboard {
            None if tab => {
                let start = if forward {
                    Keyboard::Sidebar
                } else {
                    Keyboard::Footer
                };
                let _ = self.enter(start, forward, layout, context, damage);
                Outcome::none()
            }
            None | Some(Keyboard::Sidebar) if stroke.key == Key::Named(NamedKey::Escape) => {
                Outcome::asking(AppRequest::Close)
            }
            None => Outcome::none(),
            Some(Keyboard::Sidebar) if tab => {
                let _ = self.enter(
                    Keyboard::Sidebar.next(forward),
                    forward,
                    layout,
                    context,
                    damage,
                );
                Outcome::none()
            }
            Some(Keyboard::Sidebar) => {
                let (_, scale, theme) = context;
                if let Some(TabsAction::Selected { index }) =
                    self.sidebar
                        .on_key(stroke.key, layout.sidebar, scale, theme, damage)
                {
                    self.show(category_at(index), layout, damage);
                }
                Outcome::none()
            }
            Some(Keyboard::Panel) => {
                let outcome =
                    self.panel
                        .on_key(stroke, (layout.panel, layout.window), context, damage);
                self.panel_outcome(outcome, layout, context, damage)
                    .unwrap_or_else(Outcome::none)
            }
            Some(Keyboard::Footer) => {
                let outcome =
                    self.footer
                        .on_key(stroke, (layout.footer, layout.window), context, damage);
                self.footer_outcome(outcome, layout, context, damage)
                    .unwrap_or_else(Outcome::none)
            }
        }
    }

    fn pointer_event(
        &mut self,
        input: &InputEvent,
        layout: &SettingsLayout,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest>> {
        if let InputEvent::PointerMoved { to } = input {
            self.pointer = *to;
        }
        let pressed = matches!(input, InputEvent::PointerPressed { .. });
        // A press elsewhere takes the keyboard from where it was, and a field
        // it held commits what was typed.
        let committed = if pressed {
            let region = region_at(self.pointer, layout);
            if region == self.keyboard {
                None
            } else {
                let committed = self.release(layout, context, damage);
                self.keyboard = region.filter(|&region| region != Keyboard::Sidebar);
                committed.map(|event| self.answered(event, layout, damage))
            }
        } else {
            None
        };
        // An open list or a held control sees every event first, wherever it
        // falls.
        let outcome = self
            .panel
            .on_pointer(input, (layout.panel, layout.window), context, damage);
        if let Some(answered) = self.panel_outcome(outcome, layout, context, damage) {
            return committed.unwrap_or(answered);
        }
        let outcome =
            self.footer
                .on_pointer(input, (layout.footer, layout.window), context, damage);
        if let Some(answered) = self.footer_outcome(outcome, layout, context, damage) {
            return answered;
        }
        let (_, scale, theme) = context;
        if let Some(TabsAction::Selected { index }) =
            self.sidebar
                .on_pointer(input, layout.sidebar, scale, theme, damage)
        {
            self.show(category_at(index), layout, damage);
        }
        committed.unwrap_or_else(Outcome::none)
    }

    /// Draw the window into `surface` as far as its clip admits.
    pub fn render(
        &self,
        surface: &mut Surface,
        layout: &SettingsLayout,
        (theme, scale, faces): (&Theme, Scale, Faces),
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        surface.fill(Color::from(palette.surface));
        let rule = Color::from(palette.border);
        let seam = layout.sidebar.right();
        if let (Ok(x), Ok(top)) = (u32::try_from(seam), u32::try_from(layout.window.top())) {
            surface.fill_rect(x, top, 1, layout.window.height, rule);
        }
        self.sidebar
            .render(surface, layout.sidebar, scale, theme, artwork);
        let context = (faces, scale, theme);
        self.panel.render(surface, layout.panel, context, &Plain);
        self.footer.render(surface, layout.footer, context, &Plain);
        self.panel
            .render_popup(surface, layout.panel, layout.window, context);
    }
}

impl AppView for SettingsWindow {
    type Layout = SettingsLayout;
    type Faces = Faces;
    type Own = SettingsRequest;

    fn title(&self) -> &'static str {
        "Paint settings"
    }

    fn size(&self) -> (u32, u32) {
        SIZE
    }

    fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
    ) -> SettingsLayout {
        let window = Rect::new(0, 0, width, height);
        let gap = scale.scale_length(GAP);
        let side = scale.scale_length(SIDEBAR).min(width / 3);
        let inner = window.inset(gap);
        let sidebar = Rect::new(inner.left(), inner.top(), side, inner.height);
        let left = sidebar
            .right()
            .saturating_add(i32::try_from(gap).unwrap_or(0));
        let content = Rect::new(
            left,
            inner.top(),
            u32::try_from(inner.right() - left).unwrap_or(0),
            inner.height,
        );
        let footer_height = self
            .footer
            .measured_height(content.width, faces, scale, theme)
            .min(content.height);
        let footer = Rect::new(
            content.left(),
            content.bottom() - i32::try_from(footer_height).unwrap_or(0),
            content.width,
            footer_height,
        );
        let panel = Rect::new(
            content.left(),
            content.top(),
            content.width,
            content.height.saturating_sub(footer_height + gap),
        );
        SettingsLayout {
            window,
            sidebar,
            panel,
            footer,
        }
    }

    fn min_size(&self, _: &Theme, scale: Scale, _: Faces) -> (u32, u32) {
        (scale.scale_length(LEAST.0), scale.scale_length(LEAST.1))
    }

    fn settle(&mut self, _: &SettingsLayout, _: &mut Region) {}

    fn cursor(&self, _: &SettingsLayout, _: Point) -> CursorShape {
        CursorShape::Arrow
    }

    /// The keyboard stays where it was, for the window's return.
    fn focus_changed(&mut self, _: bool, _: &SettingsLayout, _: &mut Region) {}

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest>> {
        let context = (Faces::of(theme, scale), scale, theme);
        match *input {
            InputEvent::KeyPressed { key, modifiers } => {
                let stroke = Keystroke {
                    key,
                    modifiers,
                    at_ns: now_ns,
                };
                self.key(stroke, layout, context, damage)
            }
            InputEvent::KeyReleased { .. } => Outcome::none(),
            _ => self.pointer_event(input, layout, context, damage),
        }
    }
}

/// The region `at` falls in, if any takes the keyboard.
fn region_at(at: Point, layout: &SettingsLayout) -> Option<Keyboard> {
    [
        (layout.sidebar, Keyboard::Sidebar),
        (layout.panel, Keyboard::Panel),
        (layout.footer, Keyboard::Footer),
    ]
    .into_iter()
    .find_map(|(rect, region)| rect.contains(at).then_some(region))
}

/// Whether `event` ends the interaction that made it.
const fn settled(event: PanelEvent) -> bool {
    match event {
        PanelEvent::Number { settled, .. } | PanelEvent::Moved { settled, .. } => settled,
        PanelEvent::Chosen { .. } | PanelEvent::Pressed { .. } | PanelEvent::Switched { .. } => {
            true
        }
    }
}

/// The footer, saying `said` beneath *Restore defaults* when there is
/// something to say.
fn footer(said: Option<&str>) -> Panel {
    let mut parts = alloc::vec![Part::Buttons(alloc::vec![Button::labelled(
        "Restore defaults"
    )])];
    if let Some(said) = said {
        parts.push(Part::Note(String::from(said)));
    }
    Panel::new(parts)
}

/// Three captioned fields for `colour`'s channels.
fn channels(colour: Rgb) -> Part {
    let field = |level: u8| NumberField::new(i32::from(level), 0, 255);
    Part::Fields(alloc::vec![
        Cell {
            label: "Red",
            field: field(colour.r)
        },
        Cell {
            label: "Green",
            field: field(colour.g)
        },
        Cell {
            label: "Blue",
            field: field(colour.b)
        },
    ])
}

/// Two captioned fields holding `values` between `least` and `most`.
fn pair(
    labels: (&'static str, &'static str),
    values: (u32, u32),
    (least, most): (u32, u32),
) -> Part {
    let to = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    Part::Fields(alloc::vec![
        Cell {
            label: labels.0,
            field: NumberField::new(to(values.0), to(least), to(most))
        },
        Cell {
            label: labels.1,
            field: NumberField::new(to(values.1), to(least), to(most))
        },
    ])
}

/// The checker shades a chosen pair starts from, and shows while the
/// theme's are used.
const STARTING_SHADES: (Rgb, Rgb) = (Rgb::new(0x9a, 0x9a, 0x9a), Rgb::new(0xc8, 0xc8, 0xc8));

/// The colour a chosen surround starts from, and shows while the theme's is
/// used.
const STARTING_SURROUND: Rgb = Rgb::new(0x80, 0x80, 0x80);

/// Where `value` stands in `all`, or the first entry when it is none of them.
fn index_in<T: PartialEq>(all: &[T], value: &T) -> usize {
    all.iter().position(|at| at == value).unwrap_or(0)
}

/// The panel for `category`, showing `record`.
fn panel_for(category: Category, record: &Preferences) -> Panel {
    Panel::new(match category {
        Category::General => general_parts(record),
        Category::NewPicture => new_picture_parts(record),
        Category::Grid => grid_parts(record),
        Category::Canvas => canvas_parts(record),
        Category::Panes => alloc::vec![
            Part::Note(described(&record.panes)),
            Part::Buttons(alloc::vec![
                Button::labelled("Use the front window's"),
                Button::labelled("Reset"),
            ]),
        ],
    })
}

/// The General category's parts: the starting tool and how pictures open.
fn general_parts(record: &Preferences) -> Vec<Part> {
    let tools: Vec<&str> = Tool::ALL.iter().map(|tool| tool.name()).collect();
    let opens: Vec<&str> = OpenAt::ALL.iter().map(|open| open.label()).collect();
    alloc::vec![
        Part::choice("Starting tool", &tools, index_in(&Tool::ALL, &record.tool)),
        Part::choice(
            "Open pictures",
            &opens,
            index_in(&OpenAt::ALL, &record.open_at)
        ),
    ]
}

/// The New picture category's parts: its size, format, colours and
/// background, the background held back where the format keeps none.
fn new_picture_parts(record: &Preferences) -> Vec<Part> {
    let formats: Vec<&str> = SaveFormat::ALL
        .iter()
        .map(|format| format.label())
        .collect();
    let colours: Vec<&str> = colours_for(record.format)
        .map(|colours| colours.label)
        .collect();
    let depth_at = colours_for(record.format)
        .position(|colours| colours.depth == record.new.depth)
        .unwrap_or(0);
    let mut transparent = switch("Transparent background", record.new.transparent);
    if !record.format.holds_transparency() {
        if let Part::Switch(check) = &mut transparent {
            let mut state = check.state();
            state.enabled = false;
            check.set_state(state);
        }
    }
    alloc::vec![
        pair(("Width", "Height"), record.new.size, (1, MAX_SIDE)),
        Part::choice(
            "Format",
            &formats,
            index_in(&SaveFormat::ALL, &record.format)
        ),
        Part::choice("Colours", &colours, depth_at),
        transparent,
    ]
}

/// The Grid category's parts: the spaced grid's spacing, offset, colour,
/// opacity, style and use, and the zoom the pixel grid starts at.
fn grid_parts(record: &Preferences) -> Vec<Part> {
    let styles: Vec<&str> = GridStyle::ALL.iter().map(|style| style.label()).collect();
    let spelt: Vec<String> = core::iter::once(String::from("Never"))
        .chain(PIXEL_GRID_FROM.iter().map(|zoom| format!("{zoom}%")))
        .collect();
    let zooms: Vec<&str> = spelt.iter().map(String::as_str).collect();
    let from = if record.pixel_grid_from == 0 {
        0
    } else {
        PIXEL_GRID_FROM
            .iter()
            .position(|&zoom| zoom >= record.pixel_grid_from)
            .map_or(PIXEL_GRID_FROM.len(), |at| at + 1)
    };
    let opacity = Parameter {
        label: "Opacity",
        least: percent(LEAST_GRID_OPACITY),
        most: 100,
    };
    alloc::vec![
        pair(
            ("Across", "Down"),
            record.grid.spacing,
            (1, MOST_GRID_SPACING)
        ),
        pair(
            ("Offset across", "Offset down"),
            record.grid.offset,
            (0, MOST_GRID_SPACING - 1)
        ),
        channels(record.grid.colour),
        Part::number(opacity, percent(record.grid.opacity)),
        Part::choice(
            "Style",
            &styles,
            index_in(&GridStyle::ALL, &record.grid.style)
        ),
        switch("Show in new windows", record.grid.shown),
        switch("Snap to the grid", record.grid.snap),
        Part::choice("Pixel grid from", &zooms, from),
    ]
}

/// The Canvas category's parts: the checker's squares and shades and what
/// lies around the picture, a chosen colour's fields held back while the
/// theme's is used.
fn canvas_parts(record: &Preferences) -> Vec<Part> {
    let (dark, light) = match record.shades {
        Shades::Theme => STARTING_SHADES,
        Shades::Chosen(dark, light) => (dark, light),
    };
    let around = match record.surround {
        Surround::Theme => STARTING_SURROUND,
        Surround::Chosen(colour) => colour,
    };
    let side = Parameter {
        label: "Checker squares",
        least: to_i32(CHECKER_SIDES.0),
        most: to_i32(CHECKER_SIDES.1),
    };
    let mut parts = alloc::vec![
        Part::number(side, to_i32(record.checker_side)),
        Part::choice(
            "Checker shades",
            &["The theme's", "Chosen"],
            usize::from(record.shades != Shades::Theme)
        ),
        channels(dark),
        channels(light),
        Part::choice(
            "Around the picture",
            &["The theme's", "Chosen"],
            usize::from(record.surround != Surround::Theme)
        ),
        channels(around),
    ];
    let shades = record.shades != Shades::Theme;
    let surround = record.surround != Surround::Theme;
    for (index, on) in [(2, shades), (3, shades), (5, surround)] {
        if let Some(Part::Fields(cells)) = parts.get_mut(index) {
            for cell in cells {
                let mut state = cell.field.state();
                state.enabled = on;
                cell.field.set_state(state);
            }
        }
    }
    parts
}

/// Whether moving from `was` to `now` changes what another control of
/// `category` offers, so its panel is built again.
fn reshapes(category: Category, was: &Preferences, now: &Preferences) -> bool {
    match category {
        Category::NewPicture => was.format != now.format || was.new != now.new,
        Category::Canvas => {
            (was.shades == Shades::Theme) != (now.shades == Shades::Theme)
                || (was.surround == Surround::Theme) != (now.surround == Surround::Theme)
        }
        Category::Panes => was.panes != now.panes,
        Category::General | Category::Grid => false,
    }
}

/// `value` as a count, none where it is negative.
fn unsigned(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// `value` as a colour channel's level.
fn level(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

/// Carry `event` of `category`'s panel into `record`.
fn edit(category: Category, record: &mut Preferences, event: PanelEvent) {
    match category {
        Category::General => edit_general(record, event),
        Category::NewPicture => edit_new_picture(record, event),
        Category::Grid => edit_grid(record, event),
        Category::Canvas => edit_canvas(record, event),
        // Its buttons ask the application; it edits nothing itself.
        Category::Panes => {}
    }
}

/// Carry `event` of the General panel into `record`.
fn edit_general(record: &mut Preferences, event: PanelEvent) {
    match event {
        PanelEvent::Chosen { part: 0, index } => {
            record.tool = Tool::ALL.get(index).copied().unwrap_or(record.tool);
        }
        PanelEvent::Chosen { part: 1, index } => {
            record.open_at = OpenAt::ALL.get(index).copied().unwrap_or(record.open_at);
        }
        _ => {}
    }
}

/// Carry `event` of the New picture panel into `record`.
fn edit_new_picture(record: &mut Preferences, event: PanelEvent) {
    match event {
        PanelEvent::Number {
            part: 0,
            cell,
            value,
            ..
        } => {
            let side = unsigned(value).clamp(1, MAX_SIDE);
            if cell == 0 {
                record.new.size.0 = side;
            } else {
                record.new.size.1 = side;
            }
        }
        PanelEvent::Chosen { part: 1, index } => {
            record.format = SaveFormat::ALL.get(index).copied().unwrap_or(record.format);
        }
        PanelEvent::Chosen { part: 2, index } => {
            if let Some(colours) = colours_for(record.format).nth(index) {
                record.new.depth = colours.depth;
            }
        }
        PanelEvent::Switched { part: 3, on } => record.new.transparent = on,
        _ => {}
    }
}

/// Carry `event` of the Grid panel into `record`.
fn edit_grid(record: &mut Preferences, event: PanelEvent) {
    match event {
        PanelEvent::Number {
            part: 0,
            cell,
            value,
            ..
        } => {
            let spacing = unsigned(value).clamp(1, MOST_GRID_SPACING);
            if cell == 0 {
                record.grid.spacing.0 = spacing;
            } else {
                record.grid.spacing.1 = spacing;
            }
        }
        PanelEvent::Number {
            part: 1,
            cell,
            value,
            ..
        } => {
            let offset = unsigned(value).min(MOST_GRID_SPACING - 1);
            if cell == 0 {
                record.grid.offset.0 = offset;
            } else {
                record.grid.offset.1 = offset;
            }
        }
        PanelEvent::Number {
            part: 2,
            cell,
            value,
            ..
        } => set_channel(&mut record.grid.colour, cell, level(value)),
        PanelEvent::Number { part: 3, value, .. } => {
            record.grid.opacity = (unsigned(value) * 10).clamp(LEAST_GRID_OPACITY, 1000);
        }
        PanelEvent::Chosen { part: 4, index } => {
            record.grid.style = GridStyle::ALL
                .get(index)
                .copied()
                .unwrap_or(record.grid.style);
        }
        PanelEvent::Switched { part: 5, on } => record.grid.shown = on,
        PanelEvent::Switched { part: 6, on } => record.grid.snap = on,
        PanelEvent::Chosen { part: 7, index } => {
            record.pixel_grid_from = index
                .checked_sub(1)
                .and_then(|at| PIXEL_GRID_FROM.get(at).copied())
                .unwrap_or(0)
                .clamp(0, PIXEL_GRID_ZOOMS.1);
        }
        _ => {}
    }
}

/// Carry `event` of the Canvas panel into `record`: choosing a colour of
/// one's own starts from the colour shown for the theme's.
fn edit_canvas(record: &mut Preferences, event: PanelEvent) {
    match event {
        PanelEvent::Number { part: 0, value, .. } => {
            record.checker_side = unsigned(value).clamp(CHECKER_SIDES.0, CHECKER_SIDES.1);
        }
        PanelEvent::Chosen { part: 1, index } => {
            record.shades = match (index, record.shades) {
                (0, _) => Shades::Theme,
                (_, Shades::Theme) => Shades::Chosen(STARTING_SHADES.0, STARTING_SHADES.1),
                (_, chosen) => chosen,
            };
        }
        PanelEvent::Number {
            part: part @ (2 | 3),
            cell,
            value,
            ..
        } => {
            if let Shades::Chosen(dark, light) = &mut record.shades {
                set_channel(if part == 2 { dark } else { light }, cell, level(value));
            }
        }
        PanelEvent::Chosen { part: 4, index } => {
            record.surround = match (index, record.surround) {
                (0, _) => Surround::Theme,
                (_, Surround::Theme) => Surround::Chosen(STARTING_SURROUND),
                (_, chosen) => chosen,
            };
        }
        PanelEvent::Number {
            part: 5,
            cell,
            value,
            ..
        } => {
            if let Surround::Chosen(colour) = &mut record.surround {
                set_channel(colour, cell, level(value));
            }
        }
        _ => {}
    }
}

fn set_channel(colour: &mut Rgb, cell: usize, level: u8) {
    match cell {
        0 => colour.r = level,
        1 => colour.g = level,
        _ => colour.b = level,
    }
}

fn percent(permille: u32) -> i32 {
    i32::try_from(permille / 10).unwrap_or(i32::MAX)
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// `panes` in words.
fn described(panes: &Arrangement) -> String {
    let mut words = String::new();
    for side in Side::BOTH {
        let names: Vec<&str> = panes
            .docked(side)
            .iter()
            .map(|docked| docked.kind.title())
            .collect();
        if names.is_empty() {
            continue;
        }
        if !words.is_empty() {
            words.push_str("; ");
        }
        let edge = match side {
            Side::Left => "on the left",
            Side::Right => "on the right",
        };
        let _ = write!(words, "{} {edge}", names.join(", "));
    }
    let floating: Vec<&str> = panes.floating().map(PaneKind::title).collect();
    let hidden: Vec<&str> = PaneKind::ALL
        .iter()
        .filter(|&&kind| !panes.shows(kind))
        .map(|kind| kind.title())
        .collect();
    for (names, state) in [(floating, "floating"), (hidden, "hidden")] {
        if names.is_empty() {
            continue;
        }
        if !words.is_empty() {
            words.push_str("; ");
        }
        let _ = write!(words, "{} {state}", names.join(", "));
    }
    words
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
