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

use crate::standing::Standing;

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
    /// Machine `id`'s menu, at `at` in the window: a secondary press on its
    /// card.
    Menu {
        /// The machine.
        id: u32,
        /// Where the press was.
        at: Point,
    },
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

/// How the library window is opened: its title, and the logical size it
/// opens at and the least it may be made.
#[derive(Clone, Copy, Debug)]
pub struct LibraryLook {
    /// Its title.
    pub title: &'static str,
    /// The size it opens at.
    pub size: (u32, u32),
    /// The least it may be made.
    pub least: (u32, u32),
}

impl LibraryLook {
    /// The library window titled `title`, at the size every library opens
    /// at: room for two rows of three cards.
    #[must_use]
    pub const fn titled(title: &'static str) -> Self {
        Self {
            title,
            size: (960, 640),
            least: (480, 360),
        }
    }
}

/// A machine as the library window lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its number.
    pub id: u32,
    /// What it is called.
    pub name: String,
    /// What it is, in a line: the band beneath the cards states it.
    pub about: String,
    /// How it stands.
    pub standing: Standing,
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

    /// Show `entries`, and add what that changed to `damage`; the chosen
    /// machine stays chosen while it is there.
    pub fn show(
        &mut self,
        entries: impl IntoIterator<Item = Entry>,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        let chosen = self.selected().map(|(id, _)| id);
        let before = core::mem::take(&mut self.cards);
        let before_bar = self.layout.bar;
        self.cards = entries
            .into_iter()
            .map(|entry| Card {
                id: entry.id,
                preview: WindowPreview::new(entry.name, IconKind::AppBundle),
                standing: entry.standing,
                about: entry.about,
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

    /// Where the button along the top that asks `request` is; none for a
    /// card's menu, which no button asks.
    #[must_use]
    pub fn button_bounds(&self, request: Request) -> Option<Rect> {
        let n = match request {
            Request::New => 0,
            Request::Start(_) => 1,
            Request::Settings(_) => 2,
            Request::Duplicate(_) => 3,
            Request::Delete(_) => 4,
            Request::Menu { .. } => return None,
        };
        self.layout.commands.get(n).copied()
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
        if matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Secondary
            }
        ) {
            return self.card_menu(damage);
        }
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

    /// The menu of the card under the pointer, which a secondary press chooses
    /// as a primary one would; none off every card.
    fn card_menu(&mut self, damage: &mut Region) -> Option<Request> {
        let area = self.layout.area;
        if !area.contains(self.pointer) {
            return None;
        }
        let n = self
            .layout
            .cards
            .iter()
            .position(|rect| rect.intersection(&area).contains(self.pointer))?;
        self.choose(n, damage);
        Some(Request::Menu {
            id: self.cards.get(n)?.id,
            at: self.pointer,
        })
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

/// The machines' last screens the cards are drawn with, at the size the
/// cards draw them.
#[derive(Debug, Default)]
pub struct Pictures {
    size: Option<(u32, u32)>,
    held: Vec<(u32, Surface)>,
}

impl Pictures {
    /// The size pictures are drawn at, once the cards have one.
    #[must_use]
    pub const fn size(&self) -> Option<(u32, u32)> {
        self.size
    }

    /// Draw pictures at `size` from now on: whether that is a new size, every
    /// picture held let go with it.
    pub fn resize(&mut self, size: Option<(u32, u32)>) -> bool {
        if size == self.size {
            return false;
        }
        self.size = size;
        self.held.clear();
        true
    }

    /// Machine `id`'s picture.
    #[must_use]
    pub fn get(&self, id: u32) -> Option<&Surface> {
        self.held
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, picture)| picture)
    }

    /// Hold machine `id`'s picture, if it is drawn at the size the cards draw.
    pub fn put(&mut self, id: u32, rgba: &[u8]) {
        let Some((width, height)) = self.size else {
            return;
        };
        if let Some(picture) = Surface::from_rgba8(width, height, rgba) {
            self.hold(id, picture);
        }
    }

    /// Machine `to` shows what machine `from` shows: a copy's card.
    pub fn copy(&mut self, from: u32, to: u32) {
        if let Some(picture) = self.get(from).and_then(Surface::try_clone) {
            self.hold(to, picture);
        }
    }

    /// Let go of machine `id`'s picture.
    pub fn forget(&mut self, id: u32) {
        self.held.retain(|(held, _)| *held != id);
    }

    fn hold(&mut self, id: u32, picture: Surface) {
        self.forget(id);
        if self.held.try_reserve(1).is_ok() {
            self.held.push((id, picture));
        }
    }
}

#[cfg(test)]
#[path = "shelf_tests.rs"]
mod tests;
