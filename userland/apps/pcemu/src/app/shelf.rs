//! The library window: each machine a card of its last screen and its name,
//! the commands above them, and what the chosen machine is in the band
//! beneath. The cards scroll between the two when there are more than the
//! window shows.
//!
//! One layout, made when the window, the machines or the scroll change,
//! serves the paint and the hit test.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::time::Duration64;
use tairix_controls::{
    Button, ButtonAction, ScrollAction, ScrollBar, ScrollModel, ScrollOrientation, ScrollRange,
    SelectionState, StatusBand, StatusTone, WindowPreview, WindowPreviewAction,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{ClickKind, DoubleClickTracker, InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::library::Machine;
use crate::board::{BoardConfig, SoundCard};
use crate::cpu::CpuModel;
use crate::video::Adapter;

/// What the library window asks of its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Make a machine: the new-machine assistant.
    New,
    /// Start machine `id`, or raise its window.
    Start(u32),
    /// Machine `id`'s settings.
    Settings(u32),
    /// A copy of machine `id`, its disks with it.
    Duplicate(u32),
    /// Delete machine `id`, once asked.
    Delete(u32),
}

/// The commands along the top, in order.
const COMMANDS: [&str; 5] = [
    "New\u{2026}",
    "Start",
    "Settings\u{2026}",
    "Duplicate",
    "Delete\u{2026}",
];

/// A card's extent at the reference density: a 4:3 screen and its caption.
const CARD: (u32, u32) = (224, 204);

/// Where a machine stands, as the library shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Turned off.
    Off,
    /// Running in a window of its own.
    Running,
    /// Being kept as it suspends.
    Saving,
    /// Suspended: started, it resumes where it stopped.
    Suspended,
}

impl Standing {
    /// How the library's status band names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Running => "running",
            Self::Saving => "saving",
            Self::Suspended => "suspended",
        }
    }

    /// Why its hardware cannot be changed, where it cannot: only a machine
    /// turned off can be.
    #[must_use]
    pub const fn locked(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Running => Some("The machine is running: shut it down to change its hardware."),
            Self::Saving => Some("The machine is being kept as it suspends."),
            Self::Suspended => {
                Some("The machine is suspended: start it and shut it down to change its hardware.")
            }
        }
    }

    /// Whether it may be copied or deleted: not under its window, nor while
    /// it is being kept.
    #[must_use]
    pub const fn at_rest(self) -> bool {
        matches!(self, Self::Off | Self::Suspended)
    }

    /// Whether it may be started or its window raised: not while it is being
    /// kept, which a start would race.
    #[must_use]
    pub const fn startable(self) -> bool {
        !matches!(self, Self::Saving)
    }
}

/// One machine's card.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Card {
    id: u32,
    preview: WindowPreview,
    standing: Standing,
    about: String,
}

/// Where everything is drawn at one size and scroll.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    commands: Vec<Rect>,
    /// Where the cards are seen: between the commands and the band, beside
    /// the scrollbar.
    area: Rect,
    /// Every card, where the scroll puts it.
    cards: Vec<Rect>,
    /// The scrollbar, while the cards overflow the area.
    bar: Option<Rect>,
    status: Rect,
}

/// The library window's state.
#[derive(Debug)]
pub struct Shelf {
    commands: Vec<Button>,
    cards: Vec<Card>,
    selected: Option<usize>,
    clicks: DoubleClickTracker,
    pointer: Point,
    size: (u32, u32),
    /// How far the cards are scrolled, in pixels.
    offset: u32,
    bar: ScrollBar,
    layout: Layout,
}

impl Default for Shelf {
    fn default() -> Self {
        Self::new()
    }
}

/// What a processor is called.
const fn cpu_name(cpu: CpuModel) -> &'static str {
    match cpu {
        CpuModel::I8088 => "8088",
        CpuModel::I8086 => "8086",
        CpuModel::V20 => "NEC V20",
        CpuModel::V30 => "NEC V30",
        CpuModel::I80188 => "80188",
        CpuModel::I80186 => "80186",
    }
}

/// What a display adapter is called.
const fn adapter_name(adapter: Adapter) -> &'static str {
    match adapter {
        Adapter::Mda(_) => "MDA",
        Adapter::Hercules(_) => "Hercules",
        Adapter::Cga(_) => "CGA",
        Adapter::Ega(_) => "EGA",
        Adapter::Vga(_) => "VGA",
    }
}

/// The line the status band states of `machine`.
#[must_use]
pub fn describe(machine: &Machine) -> String {
    describe_config(&machine.config, machine.disks.iter().flatten().count())
}

/// What a machine of `config` with `disks` hard disks is, in a line.
#[must_use]
pub fn describe_config(config: &BoardConfig, disks: usize) -> String {
    // Hundredths of a megahertz, rounded: ten million millihertz each.
    let hundredths = (config.clock.rate().millihertz() + 5_000_000) / 10_000_000;
    let mut line = format!(
        "{} at {}.{:02} MHz",
        cpu_name(config.cpu),
        hundredths / 100,
        hundredths % 100,
    );
    if config.coprocessor {
        line.push_str(" + 8087");
    }
    let _ = write!(line, " \u{b7} {} KB", config.memory_kib);
    if let Some(ems) = config.ems {
        let kib = u32::from(ems.pages) * 16;
        let _ = write!(line, " + {kib} KB expanded");
    }
    for adapter in config.video.iter().flatten() {
        line.push_str(" \u{b7} ");
        line.push_str(adapter_name(*adapter));
    }
    if disks > 0 {
        let plural = if disks == 1 { "" } else { "s" };
        let _ = write!(line, " \u{b7} {disks} hard disk{plural}");
    }
    match config.sound.card {
        Some(SoundCard::AdLib) => line.push_str(" \u{b7} AdLib"),
        Some(SoundCard::Blaster { .. }) => line.push_str(" \u{b7} Sound Blaster"),
        None => {}
    }
    line
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// `rect` as a clip window, none for one off the surface's top or left.
fn clip_of(rect: Rect) -> Option<(u32, u32, u32, u32)> {
    Some((
        u32::try_from(rect.left()).ok()?,
        u32::try_from(rect.top()).ok()?,
        rect.width,
        rect.height,
    ))
}

impl Shelf {
    /// A library window showing no machines yet.
    #[must_use]
    pub fn new() -> Self {
        let commands = COMMANDS
            .iter()
            .map(|label| Button::labelled(*label))
            .collect();
        let mut shelf = Self {
            commands,
            cards: Vec::new(),
            selected: None,
            clicks: DoubleClickTracker::new(),
            pointer: Point::ORIGIN,
            size: (0, 0),
            offset: 0,
            bar: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::EMPTY, 1),
            )
            .without_edges(),
            layout: Layout::default(),
        };
        shelf.enable_commands();
        shelf
    }

    /// The size every card's picture is drawn at, which the owner makes each
    /// machine's last screen at; none where a card holds no picture.
    #[must_use]
    pub fn picture_size(scale: Scale, theme: &Theme) -> Option<(u32, u32)> {
        let card = Rect::new(0, 0, scale.scale_length(CARD.0), scale.scale_length(CARD.1));
        let picture = WindowPreview::new(String::new(), IconKind::AppBundle)
            .thumbnail_bounds(card, scale, theme);
        (!picture.is_empty()).then_some((picture.width, picture.height))
    }

    /// Lay the window out at `size`.
    pub fn set_size(&mut self, size: (u32, u32), (scale, theme): (Scale, &Theme)) {
        self.size = size;
        self.relayout(scale, theme);
    }

    /// Show `machines`, `standing` saying where each stands, and add what
    /// that changed to `damage`; the chosen machine stays chosen while it is
    /// there.
    pub fn show(
        &mut self,
        machines: &[Machine],
        standing: impl Fn(u32) -> Standing,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        let chosen = self.selected().map(|(id, _)| id);
        let before = core::mem::take(&mut self.cards);
        let before_bar = self.layout.bar;
        self.cards = machines
            .iter()
            .map(|machine| Card {
                id: machine.id,
                preview: WindowPreview::new(machine.name.clone(), IconKind::AppBundle),
                standing: standing(machine.id),
                about: describe(machine),
            })
            .collect();
        self.selected = chosen
            .and_then(|id| self.cards.iter().position(|card| card.id == id))
            .or_else(|| (!self.cards.is_empty()).then_some(0));
        self.mark();
        self.enable_commands();
        self.relayout(scale, theme);
        let area = self.layout.area;
        let same = before.len() == self.cards.len()
            && before
                .iter()
                .zip(&self.cards)
                .all(|(was, now)| was.id == now.id)
            && before_bar == self.layout.bar;
        if same {
            for (rect, _) in self
                .layout
                .cards
                .iter()
                .zip(before.iter().zip(&self.cards))
                .filter(|(_, (was, now))| was != now)
            {
                damage.add(rect.intersection(&area));
            }
        } else {
            damage.add(area);
            for bar in [before_bar, self.layout.bar].into_iter().flatten() {
                damage.add(bar);
            }
        }
        damage.add(self.layout.status);
        for rect in &self.layout.commands {
            damage.add(*rect);
        }
    }

    /// Where machine `id`'s card is seen, for a picture of it drawn anew.
    #[must_use]
    pub fn card_bounds(&self, id: u32) -> Option<Rect> {
        let n = self.cards.iter().position(|card| card.id == id)?;
        let seen = self.layout.cards.get(n)?.intersection(&self.layout.area);
        (!seen.is_empty()).then_some(seen)
    }

    /// The chosen machine's number, and where it stands.
    #[must_use]
    pub fn selected(&self) -> Option<(u32, Standing)> {
        let card = self.cards.get(self.selected?)?;
        Some((card.id, card.standing))
    }

    /// Where everything is drawn now.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    fn mark(&mut self) {
        for (n, card) in self.cards.iter_mut().enumerate() {
            let selection = if Some(n) == self.selected {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            };
            let state = card.preview.state().with_selection(selection);
            card.preview.set_state(state);
        }
    }

    /// Only what can be done to the chosen machine is offered: a running one
    /// is neither copied nor deleted under its window, and one being kept is
    /// left to be kept.
    fn enable_commands(&mut self) {
        let chosen = self.selected().map(|(_, standing)| standing);
        let offered = [
            true,
            chosen.is_some_and(Standing::startable),
            chosen.is_some(),
            chosen.is_some_and(Standing::at_rest),
            chosen.is_some_and(Standing::at_rest),
        ];
        for (button, on) in self.commands.iter_mut().zip(offered) {
            let state = button.state().with_enabled(on);
            button.set_state(state);
        }
    }

    /// Lay everything out at the window's size and the scroll, the scroll held
    /// within what the cards need.
    fn relayout(&mut self, scale: Scale, theme: &Theme) {
        let (width, height) = self.size;
        let metrics = theme.metrics();
        let gap = scale.scale_length(metrics.control_gap).max(1);
        let inset = scale.scale_length(metrics.control_inset).max(1);
        let bar = scale.scale_length(metrics.control_height);
        let breadth = scale.scale_length(metrics.scrollbar_breadth).max(1);
        let mut x = gap;
        let commands = self
            .commands
            .iter()
            .map(|button| {
                let w = button.measured_width(scale, theme);
                let rect = Rect::new(to_i32(x), to_i32(gap), w, bar);
                x += w + gap;
                rect
            })
            .collect();
        let status_height = StatusBand::new(theme, scale).height();
        let status = Rect::new(
            0,
            to_i32(height.saturating_sub(status_height)),
            width,
            status_height,
        );
        let top = gap * 2 + bar;
        let seen = height.saturating_sub(status_height).saturating_sub(top);
        let (card_w, card_h) = (scale.scale_length(CARD.0), scale.scale_length(CARD.1));
        let count = u32::try_from(self.cards.len()).unwrap_or(u32::MAX);
        let across = |room: u32| (room.saturating_sub(inset) / (card_w + inset)).max(1);
        let content = |across: u32| count.div_ceil(across).saturating_mul(card_h + inset);
        let overflows = content(across(width)) > seen;
        let room = if overflows {
            width.saturating_sub(breadth)
        } else {
            width
        };
        let columns = across(room);
        let extent = content(columns);
        self.offset = self.offset.min(extent.saturating_sub(seen));
        let cards = (0..count)
            .map(|n| {
                let (column, row) = (n % columns, n / columns);
                let y = i64::from(top) + i64::from(row) * i64::from(card_h + inset)
                    - i64::from(self.offset);
                Rect::new(
                    to_i32(inset + column * (card_w + inset)),
                    i32::try_from(y).unwrap_or(i32::MIN),
                    card_w,
                    card_h,
                )
            })
            .collect();
        let range = ScrollRange::new(u64::from(extent), u64::from(seen), u64::from(self.offset));
        self.bar
            .set_model(ScrollModel::in_pixels(range, u64::from(card_h + inset)));
        self.layout = Layout {
            commands,
            area: Rect::new(0, to_i32(top), room, seen),
            cards,
            bar: overflows.then(|| Rect::new(to_i32(room), to_i32(top), breadth, seen)),
            status,
        };
    }

    /// Paint the window: its ground, the commands, the cards, the scrollbar
    /// and the band.
    pub fn render<'p>(
        &self,
        surface: &mut Surface,
        (scale, theme): (Scale, &Theme),
        picture: impl Fn(u32) -> Option<&'p Surface>,
    ) {
        let layout = &self.layout;
        surface.fill(tairix_raster::Color::from(theme.palette().surface));
        for (button, rect) in self.commands.iter().zip(&layout.commands) {
            button.render(surface, *rect, scale, theme);
        }
        if let Some((x, y, w, h)) = clip_of(layout.area) {
            surface.with_clip(x, y, w, h, |surface| {
                for (card, rect) in self.cards.iter().zip(&layout.cards) {
                    card.preview
                        .render(surface, *rect, scale, theme, picture(card.id), None);
                }
            });
        }
        if let Some(bar) = layout.bar {
            self.bar.render(surface, bar, scale, theme);
        }
        let band = StatusBand::new(theme, scale);
        let text = self.selected.and_then(|n| self.cards.get(n)).map_or(
            (
                String::from("No machines \u{2014} New\u{2026} makes one"),
                StatusTone::Muted,
            ),
            |card| {
                let state = card.standing.label();
                (format!("{} \u{b7} {state}", card.about), StatusTone::Ink)
            },
        );
        let inset = scale.scale_length(theme.metrics().control_inset).max(1);
        let slot = Rect::new(
            layout.status.left() + to_i32(inset),
            layout.status.top(),
            layout.status.width.saturating_sub(2 * inset),
            layout.status.height,
        );
        band.paint(surface, layout.status, theme, &[(slot, &text.0, text.1)]);
    }

    /// Take `event` at monotonic `now_ns`, the desktop's double-click
    /// `interval` pairing presses: what the window asks for, and what it
    /// redrew in `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        (now_ns, interval): (u64, Duration64),
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Request> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if let InputEvent::KeyPressed { key, modifiers } = event {
            return self.on_key(*key, modifiers.ctrl, (scale, theme), damage);
        }
        if let Some(bar) = self.layout.bar {
            let scrolled = match event {
                InputEvent::PointerScrolled { dx, dy } => {
                    self.bar.wheel(*dx, *dy, scale, bar, damage)
                }
                _ => self.bar.on_pointer(event, bar, scale, theme, damage),
            };
            if let Some(ScrollAction::ScrollTo { offset }) = scrolled {
                self.scroll_to(offset, (scale, theme), damage);
            }
            if self.bar.is_pressing() {
                return None;
            }
        }
        for (n, (button, rect)) in self
            .commands
            .iter_mut()
            .zip(&self.layout.commands)
            .enumerate()
        {
            if button.on_pointer(event, *rect, damage) == Some(ButtonAction::Activated) {
                return self.command(n);
            }
        }
        self.on_cards(event, (now_ns, interval), damage)
    }

    fn on_cards(
        &mut self,
        event: &InputEvent,
        (now_ns, interval): (u64, Duration64),
        damage: &mut Region,
    ) -> Option<Request> {
        let area = self.layout.area;
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        ) && area.contains(self.pointer);
        let mut chosen = None;
        for (n, (card, rect)) in self.cards.iter_mut().zip(&self.layout.cards).enumerate() {
            let seen = rect.intersection(&area);
            if card.preview.on_pointer(event, seen, damage) == Some(WindowPreviewAction::Activated)
            {
                chosen = Some(n);
            }
            if pressed && seen.contains(self.pointer) {
                let subject = u64::from(card.id);
                if self
                    .clicks
                    .register(now_ns, subject, PointerButton::Primary, interval)
                    == ClickKind::Double
                {
                    return card.standing.startable().then_some(Request::Start(card.id));
                }
            }
        }
        if let Some(n) = chosen {
            self.choose(n, damage);
        }
        None
    }

    /// When the held scrollbar press steps next, for the owner's wait.
    pub fn deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.bar.repeat_deadline(now_ns)
    }

    /// Step the held scrollbar press if it is due at `now_ns`.
    pub fn step(&mut self, now_ns: u64, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let Some(bar) = self.layout.bar else {
            return;
        };
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.bar.repeat(now_ns, bar, scale, theme, damage)
        {
            self.scroll_to(offset, (scale, theme), damage);
        }
    }

    fn scroll_to(&mut self, offset: u64, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        if offset == self.offset {
            return;
        }
        self.offset = offset;
        self.relayout(scale, theme);
        damage.add(self.layout.area);
        if let Some(bar) = self.layout.bar {
            damage.add(bar);
        }
    }

    fn command(&self, n: usize) -> Option<Request> {
        let id = self.selected().map(|(id, _)| id);
        match n {
            0 => Some(Request::New),
            1 => self
                .selected()
                .filter(|(_, standing)| standing.startable())
                .map(|(id, _)| Request::Start(id)),
            2 => id.map(Request::Settings),
            3 => id.map(Request::Duplicate),
            4 => id.map(Request::Delete),
            _ => None,
        }
    }

    fn on_key(
        &mut self,
        key: Key,
        ctrl: bool,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Request> {
        let count = self.cards.len();
        let across = layout_columns(&self.layout);
        let step = |n: usize, by: isize| {
            usize::try_from(isize::try_from(n).ok()? + by)
                .ok()
                .filter(|&m| m < count)
        };
        let to = match key {
            Key::Char('n' | 'N') if ctrl => return Some(Request::New),
            Key::Named(NamedKey::Enter) => {
                return self
                    .selected()
                    .filter(|(_, standing)| standing.startable())
                    .map(|(id, _)| Request::Start(id));
            }
            Key::Named(NamedKey::Delete) => {
                return self
                    .selected()
                    .filter(|(_, standing)| standing.at_rest())
                    .map(|(id, _)| Request::Delete(id));
            }
            Key::Named(NamedKey::Left) => self.selected.and_then(|n| step(n, -1)),
            Key::Named(NamedKey::Right) => self.selected.and_then(|n| step(n, 1)),
            Key::Named(NamedKey::Up) => self
                .selected
                .and_then(|n| step(n, -isize::try_from(across).ok()?)),
            Key::Named(NamedKey::Down) => self
                .selected
                .and_then(|n| step(n, isize::try_from(across).ok()?)),
            _ => None,
        };
        if let Some(n) = to {
            self.choose(n, damage);
            self.reveal(n, (scale, theme), damage);
        }
        None
    }

    /// Scroll card `n` wholly into the area.
    fn reveal(&mut self, n: usize, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let (Some(card), area) = (self.layout.cards.get(n), self.layout.area) else {
            return;
        };
        let offset = i64::from(self.offset);
        let above = i64::from(area.top()) - i64::from(card.top());
        let below = i64::from(card.bottom()) - i64::from(area.bottom());
        let wanted = if above > 0 {
            offset - above
        } else if below > 0 {
            offset + below
        } else {
            return;
        };
        self.scroll_to(u64::try_from(wanted).unwrap_or(0), (scale, theme), damage);
    }

    fn choose(&mut self, n: usize, damage: &mut Region) {
        if self.selected == Some(n) {
            return;
        }
        let area = self.layout.area;
        for rect in [self.selected, Some(n)]
            .into_iter()
            .flatten()
            .filter_map(|m| self.layout.cards.get(m))
        {
            damage.add(rect.intersection(&area));
        }
        damage.add(self.layout.status);
        for rect in &self.layout.commands {
            damage.add(*rect);
        }
        self.selected = Some(n);
        self.mark();
        self.enable_commands();
    }
}

/// Cards across a row, as `layout` laid them out.
fn layout_columns(layout: &Layout) -> usize {
    let Some(first) = layout.cards.first() else {
        return 1;
    };
    layout
        .cards
        .iter()
        .take_while(|rect| rect.top() == first.top())
        .count()
        .max(1)
}

#[cfg(test)]
#[path = "shelf_tests.rs"]
mod tests;
