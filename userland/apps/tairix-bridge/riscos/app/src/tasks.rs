//! A session's window, the Task Manager's view of it
//! (`plans/RISCOS-EMULATOR.md` §15.3): each context with its command line,
//! its slot and screen and whether it is paused or busy; what the shared areas hold
//! and the free pool; what the session was refused or its boot failed with;
//! and the commands — run, run in the shared session, pause or resume and
//! quit the task chosen, the settings.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    fill_area, ground_fill, Button, ButtonAction, ButtonContent, CellAlign, ChromeLayer,
    ControlState, HeaderColumn, TableCell, TableHeader, TableRow,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_procinfo::display::format_bytes as size;
use tairix_raster::{Color, Surface};
use tairix_riscos_kernel::memory::Shared;
use tairix_riscos_session::protocol::Context;
use tairix_theme::Theme;
use tairix_visible::shown_char;

/// The window's width in logical pixels.
const WIDTH: u32 = 560;

/// The width of each figure's column in logical pixels.
const FIGURE: u32 = 96;

/// The most tasks shown at once; the rest are scrolled to.
pub const SHOWN_MOST: usize = 12;

/// The most notes the window keeps, the oldest let go first, so a long
/// session's window never outgrows the screen.
pub const NOTES_MOST: usize = 8;

/// The most of the boot's failures the window says; the rest are counted.
pub const BOOT_SAID_MOST: usize = 4;

// The boot's failures and the count of the rest fit among the notes kept.
const _: () = assert!(BOOT_SAID_MOST < NOTES_MOST);

/// A session's kind, which its window's title says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The shared session.
    Shared,
    /// An isolated session.
    Isolated,
}

/// One task.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Task {
    /// Its context.
    pub context: Context,
    /// Its command line as shown.
    pub line: String,
    /// Its slot's bytes.
    pub slot: u32,
    /// Its screen memory's bytes.
    pub screen: u32,
    /// The front end holds it.
    pub paused: bool,
    /// Its program shows the hourglass.
    pub busy: bool,
}

/// What a command in the window asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Run a file as its provenance says.
    Run,
    /// Run a file in the shared session.
    RunShared,
    /// Hold `context` where it stands, or let it go on.
    Pause(Context, bool),
    /// Quit `context`.
    Quit(Context),
    /// Open the settings.
    Settings,
}

/// The buttons, in order across.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Run,
    RunShared,
    Pause,
    Quit,
    Settings,
}

const ACTS: [(Act, &str); 5] = [
    (Act::Run, "Run…"),
    (Act::RunShared, "Run in Shared Session…"),
    (Act::Pause, "Pause"),
    (Act::Quit, "Quit"),
    (Act::Settings, "Settings…"),
];

/// A session's window's content.
pub struct Tasks {
    kind: Kind,
    guest_bytes: u64,
    /// Its tasks, in the order they were launched.
    listed: Vec<Task>,
    shared: Option<Shared>,
    notes: VecDeque<String>,
    /// The boot's failures said, and those not.
    boot_said: usize,
    boot_unsaid: u64,
    /// The task chosen, by its context.
    chosen: Option<Context>,
    /// The first task shown.
    top: usize,
    /// Whether a file may be run in the shared session from here.
    offers_shared: bool,
    buttons: Vec<(Act, Button)>,
    pointer: Point,
}

/// The bytes of a RISC OS string as the window shows them: each as its
/// Latin-1 character, held to what may be drawn.
#[must_use]
pub fn shown(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| shown_char(char::from(byte)))
        .collect()
}

impl Tasks {
    /// The window of a session of `kind` whose guest has `guest_bytes` of
    /// RAM, offering a run in the shared session where `offers_shared`.
    #[must_use]
    pub fn new(kind: Kind, guest_bytes: u64, offers_shared: bool) -> Self {
        let mut tasks = Self {
            kind,
            guest_bytes,
            listed: Vec::new(),
            shared: None,
            notes: VecDeque::new(),
            boot_said: 0,
            boot_unsaid: 0,
            chosen: None,
            top: 0,
            offers_shared,
            buttons: Vec::new(),
            pointer: Point::new(-1, -1),
        };
        tasks.restate_buttons();
        tasks
    }

    /// The window's title.
    #[must_use]
    pub fn title(&self) -> String {
        match self.kind {
            Kind::Shared => String::from("TAIRiX Bridge"),
            Kind::Isolated => String::from("TAIRiX Bridge — isolated session"),
        }
    }

    /// The tasks, in the order they were launched.
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.listed
    }

    /// The guest has `guest_bytes` of RAM.
    pub fn ram(&mut self, guest_bytes: u64) {
        self.guest_bytes = guest_bytes;
    }

    /// `task` was launched.
    pub fn launched(&mut self, task: Task) {
        self.listed.push(task);
        self.restate_buttons();
    }

    /// `context` ended.
    pub fn ended(&mut self, context: Context) {
        self.listed.retain(|task| task.context != context);
        if self.chosen == Some(context) {
            self.chosen = None;
        }
        self.top = self.top.min(self.listed.len().saturating_sub(SHOWN_MOST));
        self.restate_buttons();
    }

    /// `context` holds `slot` bytes of its own and `screen` of screen.
    pub fn own(&mut self, context: Context, slot: u32, screen: u32) {
        if let Some(task) = self.task_mut(context) {
            task.slot = slot;
            task.screen = screen;
        }
    }

    /// `context` is paused, or let go on.
    pub fn paused(&mut self, context: Context, paused: bool) {
        if let Some(task) = self.task_mut(context) {
            task.paused = paused;
        }
        self.restate_buttons();
    }

    /// `context`'s program shows its hourglass, or no longer does.
    pub fn busy(&mut self, context: Context, busy: bool) {
        if let Some(task) = self.task_mut(context) {
            task.busy = busy;
        }
    }

    /// Whether any task's program shows its hourglass and goes on: one the
    /// front end holds is paused, not busy.
    #[must_use]
    pub fn any_busy(&self) -> bool {
        self.listed.iter().any(|task| task.busy && !task.paused)
    }

    /// No task busy, as none is once its session's worker has gone.
    pub fn clear_busy(&mut self) {
        for task in &mut self.listed {
            task.busy = false;
        }
    }

    /// The shared areas now hold `shared`.
    pub fn shared(&mut self, shared: Shared) {
        self.shared = Some(shared);
    }

    /// Whether `context`'s task is paused.
    #[must_use]
    pub fn is_paused(&self, context: Context) -> bool {
        self.listed
            .iter()
            .any(|task| task.context == context && task.paused)
    }

    /// `note` said, below what was said before.
    pub fn note(&mut self, note: String) {
        if self.notes.len() == NOTES_MOST {
            self.notes.pop_front();
        }
        self.notes.push_back(note);
    }

    /// The boot failed running `line` with `error`: said while fewer than
    /// [`BOOT_SAID_MOST`] have been, counted after.
    pub fn boot_failed(&mut self, line: &str, error: &str) {
        if self.boot_said < BOOT_SAID_MOST {
            self.boot_said += 1;
            self.note(format!("{line}: {error}"));
        } else {
            self.boot_unsaid = self.boot_unsaid.saturating_add(1);
        }
    }

    /// The boot failed `count` more times than its worker told.
    pub fn boot_omitted(&mut self, count: u32) {
        self.boot_unsaid = self.boot_unsaid.saturating_add(u64::from(count));
    }

    /// The boot ended: how many of its failures went unsaid, where any did.
    pub fn boot_ended(&mut self) {
        let unsaid = core::mem::take(&mut self.boot_unsaid);
        if unsaid > 0 {
            self.note(format!("and {unsaid} more failures"));
        }
    }

    /// Whether a file may be run in the shared session from here.
    pub fn offer_shared(&mut self, offers: bool) {
        self.offers_shared = offers;
        self.restate_buttons();
    }

    /// The window's extent: its width, and as tall as what it shows.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let layout = self.layout(scale, theme);
        (layout.width, layout.height)
    }

    /// The whole window painted.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        let layout = self.layout(scale, theme);
        let ground = ground_fill(theme, theme.palette().surface, ChromeLayer::Ground);
        let whole = Rect::new(0, 0, surface.width(), surface.height());
        fill_area(surface, whole, Color::from(ground));
        let columns = layout.columns();
        TableHeader::new(vec![
            HeaderColumn::new(self.title()),
            HeaderColumn::new("Slot").with_align(CellAlign::Trailing),
            HeaderColumn::new("Screen").with_align(CellAlign::Trailing),
            HeaderColumn::new("State"),
        ])
        .render(surface, layout.header, scale, theme, &columns);
        for (row, task) in self.shown_tasks().enumerate() {
            self.task_row(task)
                .render(surface, layout.row(row), scale, theme, &columns, None);
        }
        for (row, (name, value)) in self.facts().into_iter().enumerate() {
            TableRow::new(vec![TableCell::new(name), TableCell::numeric(value)]).render(
                surface,
                layout.fact(row),
                scale,
                theme,
                &layout.fact_columns(),
                None,
            );
        }
        for (row, note) in self.notes.iter().enumerate() {
            TableRow::new(vec![TableCell::new(note.as_str())]).render(
                surface,
                layout.note(row),
                scale,
                theme,
                &[layout.width],
                None,
            );
        }
        for (index, (_, button)) in self.buttons.iter().enumerate() {
            button.render(
                surface,
                layout.button(index, &self.buttons, scale, theme),
                scale,
                theme,
            );
        }
    }

    /// `event` taken: the command it gave, where one; what changed in
    /// `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Command> {
        let layout = self.layout(scale, theme);
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if let InputEvent::PointerPressed {
            button: PointerButton::Primary,
        } = event
        {
            let picked = (0..self.shown_tasks().count())
                .find(|&row| layout.row(row).contains(self.pointer))
                .and_then(|row| self.listed.get(self.top + row))
                .map(|task| task.context);
            if picked.is_some() && picked != self.chosen {
                self.chosen = picked;
                self.restate_buttons();
                damage.add(Rect::new(0, 0, layout.width, layout.height));
            }
        }
        if let InputEvent::PointerScrolled { dy, .. } = event {
            let most = self.listed.len().saturating_sub(SHOWN_MOST);
            let top = match dy.signum() {
                1 => (self.top + 1).min(most),
                -1 => self.top.saturating_sub(1),
                _ => self.top,
            };
            if top != self.top {
                self.top = top;
                damage.add(Rect::new(0, 0, layout.width, layout.height));
            }
        }
        let mut acted = None;
        let placed: Vec<Rect> = (0..self.buttons.len())
            .map(|index| layout.button(index, &self.buttons, scale, theme))
            .collect();
        for ((act, button), bounds) in self.buttons.iter_mut().zip(placed) {
            if button.on_pointer(event, bounds, damage) == Some(ButtonAction::Activated) {
                acted = Some(*act);
            }
        }
        let chosen = self.chosen.and_then(|context| self.task(context));
        match acted? {
            Act::Run => Some(Command::Run),
            Act::RunShared => Some(Command::RunShared),
            Act::Settings => Some(Command::Settings),
            Act::Pause => chosen.map(|task| Command::Pause(task.context, !task.paused)),
            Act::Quit => chosen.map(|task| Command::Quit(task.context)),
        }
    }

    fn task(&self, context: Context) -> Option<&Task> {
        self.listed.iter().find(|task| task.context == context)
    }

    fn task_mut(&mut self, context: Context) -> Option<&mut Task> {
        self.listed.iter_mut().find(|task| task.context == context)
    }

    fn shown_tasks(&self) -> impl Iterator<Item = &Task> {
        self.listed.iter().skip(self.top).take(SHOWN_MOST)
    }

    fn task_row(&self, task: &Task) -> TableRow {
        let mut row = TableRow::new(vec![
            TableCell::new(task.line.as_str()),
            TableCell::numeric(size(u64::from(task.slot))),
            TableCell::numeric(size(u64::from(task.screen))),
            TableCell::new(state(task)),
        ]);
        if self.chosen == Some(task.context) {
            row.set_selected(true);
        }
        row
    }

    /// The shared areas, each named.
    fn areas(shared: &Shared) -> [(&'static str, u32); 7] {
        [
            ("System heap", shared.system_heap),
            ("Module area", shared.rma),
            ("System sprites", shared.sprites),
            ("Font cache", shared.font_cache),
            ("RAM disc", shared.ram_disc),
            ("Other areas", shared.made),
            ("Free", shared.free),
        ]
    }

    /// How many facts the window states: the RAM, and the shared areas once
    /// told.
    fn fact_count(&self) -> usize {
        1 + self.shared.map_or(0, |shared| Self::areas(&shared).len())
    }

    /// What the session's memory holds, each named.
    fn facts(&self) -> Vec<(&'static str, String)> {
        let mut facts = vec![("RAM", size(self.guest_bytes))];
        if let Some(shared) = self.shared {
            facts.extend(
                Self::areas(&shared)
                    .into_iter()
                    .map(|(name, bytes)| (name, size(u64::from(bytes)))),
            );
        }
        facts
    }

    /// The buttons as the session and the task chosen allow.
    fn restate_buttons(&mut self) {
        let chosen = self.chosen.and_then(|context| self.task(context)).cloned();
        let kept = core::mem::take(&mut self.buttons);
        self.buttons = ACTS
            .iter()
            .filter(|(act, _)| *act != Act::RunShared || self.offers_shared)
            .map(|&(act, label)| {
                let label = match (act, &chosen) {
                    (Act::Pause, Some(task)) if task.paused => "Resume",
                    _ => label,
                };
                let same = |held: &Button| {
                    matches!(held.content(), ButtonContent::Label(text) if text == label)
                };
                let mut button = kept
                    .iter()
                    .find(|(was, held)| *was == act && same(held))
                    .map_or_else(|| Button::labelled(label), |(_, held)| held.clone());
                let needs_task = matches!(act, Act::Pause | Act::Quit);
                button.set_state(if needs_task && chosen.is_none() {
                    ControlState::disabled()
                } else {
                    ControlState::default()
                });
                (act, button)
            })
            .collect();
    }

    fn layout(&self, scale: Scale, theme: &Theme) -> Layout {
        let metrics = theme.metrics();
        let row = scale.scale_length(metrics.control_height).max(1);
        let gap = scale.scale_length(metrics.control_gap);
        let width = scale.scale_length(WIDTH);
        let rows = self.shown_tasks().count().max(1);
        let facts = self.fact_count();
        let notes = self.notes.len();
        let header = Rect::new(0, 0, width, row);
        let tasks_top = row;
        let facts_top = tasks_top + to_u32(rows) * row + gap;
        let notes_top = facts_top + to_u32(facts) * row + gap;
        let buttons_top = notes_top + to_u32(notes) * row + gap;
        Layout {
            width,
            height: buttons_top + row + gap,
            row,
            gap,
            figure: scale.scale_length(FIGURE),
            header,
            tasks_top,
            facts_top,
            notes_top,
            buttons_top,
        }
    }
}

/// What a task's row says of it: held by the front end, its program busy,
/// or running.
const fn state(task: &Task) -> &'static str {
    if task.paused {
        "Paused"
    } else if task.busy {
        "Busy"
    } else {
        "Running"
    }
}

fn to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn at(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Where everything in the window is, in physical pixels.
struct Layout {
    width: u32,
    height: u32,
    row: u32,
    gap: u32,
    figure: u32,
    header: Rect,
    tasks_top: u32,
    facts_top: u32,
    notes_top: u32,
    buttons_top: u32,
}

impl Layout {
    fn columns(&self) -> [u32; 4] {
        let line = self.width.saturating_sub(3 * self.figure);
        [line, self.figure, self.figure, self.figure]
    }

    fn fact_columns(&self) -> [u32; 2] {
        [self.width.saturating_sub(self.figure * 2), self.figure * 2]
    }

    fn row(&self, row: usize) -> Rect {
        Rect::new(
            0,
            at(self.tasks_top + to_u32(row) * self.row),
            self.width,
            self.row,
        )
    }

    fn fact(&self, row: usize) -> Rect {
        Rect::new(
            0,
            at(self.facts_top + to_u32(row) * self.row),
            self.width,
            self.row,
        )
    }

    fn note(&self, row: usize) -> Rect {
        Rect::new(
            0,
            at(self.notes_top + to_u32(row) * self.row),
            self.width,
            self.row,
        )
    }

    fn button(&self, index: usize, buttons: &[(Act, Button)], scale: Scale, theme: &Theme) -> Rect {
        let mut x = self.gap;
        for (_, button) in &buttons[..index.min(buttons.len())] {
            x += button.measured_width(scale, theme) + self.gap;
        }
        let width = buttons
            .get(index)
            .map_or(0, |(_, button)| button.measured_width(scale, theme));
        Rect::new(at(x), at(self.buttons_top), width, self.row)
    }
}

#[cfg(test)]
mod tests;
