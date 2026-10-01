//! The Tasks section: the live task/application table
//! (`plans/NEW-SWITCHBOARD.md` S3, S4).
//!
//! Owns the caller's task view model ([`TaskSummary`]), the sortable
//! [`TableHeader`] and its [`TableRow`]s, and the section's layout, painting
//! and input.
//!
//! The table states what each task *is*. What may be done to one is its row's
//! menu, which the desktop draws from the rows [`crate::task_menu`] declares:
//! the section only asks for it, naming the task by identity, so a sample that
//! re-sorts the rows while the menu is up cannot re-point it.
//!
//! Sorting is an arrangement of the rows the sample produced, decided in
//! `arrange` alone; nothing here reads a figure the service did not measure.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::mem;

use tairix_abi::origin::ProcId;
use tairix_abi::sysinfo::ProcessState;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::IconArtwork;
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use tairix_controls::damage;
use tairix_controls::{
    ActivityState, CellAlign, Chart, ControlState, HeaderAction, HeaderColumn, PressureKind,
    PressureState, RecoveryState, RowAction, SelectionState, SortOrder, TableCell, TableHeader,
    TableRow,
};
use tairix_procinfo::display::{format_bytes, format_rate, percent};

use super::frame::{SectionAnatomy, SectionFrame};
use super::refresh::carry_hover;
use super::resources::TaskCostColumn;
use super::task_icon;
use super::{
    resolve_selection, ListInfo, SectionCtx, SectionOutcome, SectionView, Sweep, Switchboard,
    SwitchboardAction, SwitchboardModel, UNMEASURED_READING,
};

/// Which principal owns a task, as the row's Owner column.
///
/// Carried as the uid the process record reports plus the one classification
/// the surface makes of it, so the column states a reading rather than a
/// guess.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TaskOwner {
    /// The owning uid, exactly as the process record reports it.
    pub uid: u32,
    /// Whether that uid is the system principal.
    pub is_system: bool,
}

impl TaskOwner {
    /// The uid the system principal runs as.
    ///
    /// `uid = 0` is merely the system user in this system — its powers come
    /// from capabilities, never from the number — so this is a *display*
    /// classification for the Owner column, never an authority decision.
    pub const SYSTEM_UID: u32 = 0;

    /// The owner of a task running as `uid`.
    #[must_use]
    pub const fn new(uid: u32) -> Self {
        Self {
            uid,
            is_system: uid == Self::SYSTEM_UID,
        }
    }

    /// The Owner column's text.
    ///
    /// No interface maps a uid to a user name from this service, so a
    /// non-system owner reads as its number rather than a name this service
    /// would have to invent.
    #[must_use]
    pub fn label(self) -> String {
        if self.is_system {
            return String::from("system");
        }
        alloc::format!("uid {}", self.uid)
    }
}

/// A command the Tasks section can invoke on one task.
///
/// Each variant names an operation the service can genuinely carry out
/// ([`crate::model::apply_action`]), or one whose absence it states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TaskControl {
    /// Raise the task's own window and give it the focus.
    Switch,
    /// Show where the task's window is.
    Reveal,
    /// Suspend the task.
    Pause,
    /// Continue a suspended task.
    Resume,
    /// Lower the task's scheduling priority.
    LowerPriority,
    /// Show the task's own log entries.
    OpenLogs,
    /// End the task outright.
    ForceQuit,
}

/// Why a command cannot be carried out on one task.
///
/// A menu row can only be greyed, so the reason is what tells a reader
/// whether more authority would help: the Authority Mark is the desktop's
/// alone to draw.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TaskRefusal {
    /// The caller lacks process-control authority.
    NotPermitted,
    /// The task has exited.
    Exited,
    /// The task is paused.
    Paused,
    /// The task is not paused.
    NotPaused,
    /// The task already runs at the lowest level.
    AtLowest,
    /// No capability-gated query reads a task's own log entries.
    NoLogReader,
}

impl TaskRefusal {
    /// The reason a refused command states.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NotPermitted => "needs process-control authority",
            Self::Exited => "the task has exited",
            Self::Paused => "the task is paused",
            Self::NotPaused => "the task is not paused",
            Self::AtLowest => "already at the lowest priority",
            Self::NoLogReader => "no interface reads a task's log",
        }
    }
}

/// Whether one command may be carried out on one task, and if not, why.
pub type TaskVerdict = Result<(), TaskRefusal>;

/// What the caller may do to one task: one verdict per command, decided
/// where the caller's authority and the task's own state are both known
/// (`crate::model`) rather than guessed at render time.
///
/// [`Default`] refuses everything, so a task built without an explicit
/// verdict offers no command at all rather than a permitted one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TaskAuthority {
    /// Whether the session may be asked to raise the task's window. Shared
    /// by [`TaskControl::Switch`] and [`TaskControl::Reveal`], which are the
    /// same request of the session.
    pub switch: TaskVerdict,
    /// Whether the task may be suspended.
    pub pause: TaskVerdict,
    /// Whether the task may be continued.
    pub resume: TaskVerdict,
    /// Whether the task's priority may be lowered.
    pub lower_priority: TaskVerdict,
    /// Whether the task may be ended outright.
    pub force_quit: TaskVerdict,
}

impl Default for TaskAuthority {
    /// Every command refused: an unstated authority never grants one.
    fn default() -> Self {
        let refused = Err(TaskRefusal::NotPermitted);
        Self {
            switch: refused,
            pause: refused,
            resume: refused,
            lower_priority: refused,
            force_quit: refused,
        }
    }
}

impl TaskAuthority {
    /// The verdict for one command — the single mapping the menu is built
    /// from and [`crate::model::apply_action`] re-checks against, so what is
    /// offered and what is permitted can never disagree.
    ///
    /// # Errors
    ///
    /// The [`TaskRefusal`] the command is refused for.
    /// [`TaskControl::OpenLogs`] is always [`TaskRefusal::NoLogReader`].
    pub const fn check(&self, control: TaskControl) -> TaskVerdict {
        match control {
            TaskControl::Switch | TaskControl::Reveal => self.switch,
            TaskControl::Pause => self.pause,
            TaskControl::Resume => self.resume,
            TaskControl::LowerPriority => self.lower_priority,
            TaskControl::OpenLogs => Err(TaskRefusal::NoLogReader),
            TaskControl::ForceQuit => self.force_quit,
        }
    }
}

/// One live task/application, as the caller's typed view model
/// (`plans/NEW-SWITCHBOARD.md`).
///
/// Switchboard renders it as a [`TableRow`] carrying the task's resource
/// pressure as a Pressure Rail and its recovery posture as a Signal Bead;
/// its activity is the Activity column's own sparkline, drawn where the
/// heading names it rather than as a seam under the whole row.
///
/// Every measured figure is an [`Option`]: `None` means the service did not
/// measure it, and the cell renders the explicit unmeasured mark. A zero
/// would read as a genuine idle reading, so an absent figure is never
/// flattened into one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskSummary {
    /// The task's stable, never-reused instance identity.
    ///
    /// What the selection and a menu's subject are keyed by, so neither
    /// silently re-points at a different task when a refresh or a re-sort
    /// moves the rows around it. A numeric pid would be no better — the
    /// kernel reuses it.
    pub proc_id: ProcId,
    /// The task's display name.
    pub name: String,
    /// The application-bundle directory the desktop launched the task from,
    /// when it launched it — what its row draws its icon from. [`None`] for a
    /// process nothing attests a bundle for, whose row then resolves its
    /// picture from its name.
    pub bundle: Option<String>,
    /// Which principal owns the task, for the Owner column.
    pub owner: TaskOwner,
    /// The CPU the scheduler last dispatched the task on, for the Core
    /// column. `None` for a task that is not currently on one.
    pub core: Option<u8>,
    /// The task's lifecycle state, for the State column. `None` for a row
    /// whose source reports no lifecycle.
    pub lifecycle: Option<ProcessState>,
    /// The task's CPU share over the last sample interval, in permille.
    /// `None` on the first sample of a task (no interval to divide by).
    pub cpu_permille: Option<u16>,
    /// Bytes of memory mapped in the task's address space.
    pub memory_bytes: Option<u64>,
    /// The task's storage throughput over the last sample interval, in
    /// bytes per second, derived from the delta of its own I/O counters.
    /// `None` when there is no previous reading to delta against.
    pub disk_bytes_per_sec: Option<u64>,
    /// The task's own recent CPU readings, oldest first, for the Activity
    /// column's sparkline. Empty until the task has been measured once.
    pub cpu_history: Vec<u16>,
    /// The resource pressure the task is under, if any.
    pub pressure: PressureState,
    /// What work the task is doing.
    pub activity: ActivityState,
    /// The task's recovery posture (hung, restart recommended, …).
    pub recovery: RecoveryState,
    /// What the caller may do to this task, one verdict per command.
    pub authority: TaskAuthority,
}

impl Default for TaskSummary {
    /// An unnamed, unmeasured task offering no command — the shape a caller
    /// fills in field by field, never a row that reads as a real one.
    fn default() -> Self {
        Self {
            proc_id: ProcId::KERNEL,
            name: String::new(),
            bundle: None,
            owner: TaskOwner::default(),
            core: None,
            lifecycle: None,
            cpu_permille: None,
            memory_bytes: None,
            disk_bytes_per_sec: None,
            cpu_history: Vec::new(),
            pressure: PressureState::None,
            activity: ActivityState::Idle,
            recovery: RecoveryState::None,
            authority: TaskAuthority::default(),
        }
    }
}

impl TaskSummary {
    /// The lifecycle's State-column text, or the unmeasured mark for a row
    /// whose source reports none.
    fn state_text(&self) -> &'static str {
        match self.lifecycle {
            Some(ProcessState::Runnable) => "Runnable",
            Some(ProcessState::Running) => "Running",
            Some(ProcessState::Blocked) => "Blocked",
            Some(ProcessState::Zombie) => "Zombie",
            Some(ProcessState::Stopped) => "Stopped",
            None => UNMEASURED_READING,
        }
    }
}

/// One column of the Tasks table: its heading, its share of the row's
/// width, how its cells align, and whether it can be sorted by.
///
/// One declaration per column, read by the heading, the cells and the
/// per-cell geometry alike, so a column can never be described one way in
/// the header and another in the rows.
struct ColumnSpec {
    /// The column heading's text.
    title: &'static str,
    /// The column's relative share of the row's content width.
    weight: u32,
    /// How this column's cells and heading align their text.
    align: CellAlign,
    /// Whether the heading offers to sort by this column.
    sortable: bool,
}

/// The Tasks table's columns, in draw order (`plans/switchboard/01-tasks.png`).
///
/// Every column is a *reading* about the task. The Activity column carries a
/// sparkline rather than text and so is not sortable: there is no single
/// value to order by.
const COLUMNS: [ColumnSpec; 9] = [
    ColumnSpec {
        title: "Task",
        weight: 28,
        align: CellAlign::Leading,
        sortable: true,
    },
    ColumnSpec {
        title: "Owner",
        weight: 10,
        align: CellAlign::Leading,
        sortable: true,
    },
    ColumnSpec {
        title: "State",
        weight: 10,
        align: CellAlign::Leading,
        sortable: true,
    },
    ColumnSpec {
        title: "Activity",
        weight: 12,
        align: CellAlign::Center,
        sortable: false,
    },
    ColumnSpec {
        title: "CPU",
        weight: 8,
        align: CellAlign::Trailing,
        sortable: true,
    },
    ColumnSpec {
        title: "Memory",
        weight: 10,
        align: CellAlign::Trailing,
        sortable: true,
    },
    ColumnSpec {
        title: "Disk",
        weight: 10,
        align: CellAlign::Trailing,
        sortable: true,
    },
    ColumnSpec {
        title: "Network",
        weight: 10,
        align: CellAlign::Trailing,
        sortable: false,
    },
    ColumnSpec {
        title: "Core",
        weight: 8,
        align: CellAlign::Trailing,
        sortable: true,
    },
];

/// The Task column: the row's icon and name.
const COL_TASK: usize = 0;
/// The Owner column: which principal the task runs as.
const COL_OWNER: usize = 1;
/// The State column.
const COL_STATE: usize = 2;
/// The Activity column, whose rect the CPU sparkline is drawn into.
const COL_ACTIVITY: usize = 3;
/// The CPU column.
const COL_CPU: usize = 4;
/// The Memory column.
const COL_MEMORY: usize = 5;
/// The Disk column.
const COL_DISK: usize = 6;
/// The Network column, which has no interface to read and is always
/// unmeasured.
const COL_NETWORK: usize = 7;
/// The Core column: the CPU the scheduler last dispatched the task on.
const COL_CORE: usize = 8;

/// The column weights alone, in draw order — the one geometry every column
/// query is resolved through, so the heading, the cells and the sparkline can
/// never land in different places.
///
/// Held as a constant rather than collected per call: every row asks for it
/// twice on every redraw, and the table redraws on every sample, so
/// building a fresh vector each time would be pure per-row waste for values
/// that cannot change.
const COLUMN_WEIGHTS: [u32; COLUMNS.len()] = column_weights();

/// [`COLUMN_WEIGHTS`], derived from the one column declaration so the two
/// can never disagree.
const fn column_weights() -> [u32; COLUMNS.len()] {
    let mut weights = [0; COLUMNS.len()];
    let mut i = 0;
    while i < COLUMNS.len() {
        weights[i] = COLUMNS[i].weight;
        i += 1;
    }
    weights
}

/// Which of a Tasks section's two cursor bands the keyboard is in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum FocusBand {
    /// A header control, by its stop.
    Header(usize),
    /// A shown row, by its position in the arrangement.
    Row(usize),
}

/// The header band's own keyboard stops, ahead of the rows: the sortable
/// column headings alone.
const HEADER_STOPS: usize = 1;
/// The column headings' stop.
const STOP_SORT: usize = 0;

/// One task rendered as a [`TableRow`] and its Activity sparkline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TaskEntry {
    pub(super) row: TableRow,
    /// The task's own CPU history, as the Activity column's sparkline.
    pub(super) spark: Chart,
    /// The bundle the task was launched from, where the session attested one,
    /// so the row's leading icon is that application's own picture.
    pub(super) bundle: Option<String>,
    /// The task's kernel-attested name, which resolves the leading icon of
    /// every process the desktop did not launch itself.
    pub(super) name: String,
}

/// The Tasks section: the adopted rows, the order shown over them, the
/// column headings, the selection, and the keyboard's place among them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TasksSection {
    /// Every adopted task, in model order — what the sort arranges.
    pub(super) tasks: Vec<TaskSummary>,
    /// The session's own account root, which resolves the icon of a task
    /// loaded from this user's own program store.
    pub(super) home: Option<String>,
    /// One row per *shown* task, in shown order.
    pub(super) entries: Vec<TaskEntry>,
    /// `order[i]` is the model index of shown row `i`, so a row the reader
    /// points at resolves back to the task rather than to a position.
    pub(super) order: Vec<usize>,
    /// The selected task's own identity, so the selection survives a refresh
    /// and a re-sort rather than following whichever row slid into its place.
    pub(super) selected: Option<ProcId>,
    /// The sortable column headings.
    pub(super) header: TableHeader,
    /// Where the content cursor is: the header's stops, then one per shown
    /// row.
    pub(super) focus: usize,
    /// Which of the focused thing's actions the cursor is on.
    pub(super) action: usize,
}

impl TasksSection {
    /// An empty Tasks section: no tasks, no selection, cursor on the column
    /// headings.
    pub(super) fn new() -> Self {
        Self {
            home: None,
            tasks: Vec::new(),
            entries: Vec::new(),
            order: Vec::new(),
            selected: None,
            header: TableHeader::new(
                COLUMNS
                    .iter()
                    .map(|column| {
                        let heading = if column.sortable {
                            HeaderColumn::new(column.title)
                        } else {
                            HeaderColumn::fixed(column.title)
                        };
                        heading.with_align(column.align)
                    })
                    .collect(),
            ),
            focus: 0,
            action: 0,
        }
    }

    /// Re-derive the shown rows from the adopted tasks: sort, then build one
    /// entry per row.
    ///
    /// The one place the shown order is decided, and so the one place the
    /// rows' pixels change: a fresh sample and a sort both report what they
    /// re-derived here. The sort is stable, so rows it cannot separate keep
    /// the order the sample reported them in.
    fn arrange(&mut self, sweep: &mut Sweep<'_, '_>) {
        let band = self.focus_band();
        let mut order: Vec<usize> = (0..self.tasks.len()).collect();
        if let Some((column, direction)) = self.header.sort() {
            order.sort_by(|a, b| {
                let (Some(left), Some(right)) = (self.tasks.get(*a), self.tasks.get(*b)) else {
                    return Ordering::Equal;
                };
                let compared = compare_column(left, right, column);
                if direction == SortOrder::Ascending {
                    compared
                } else {
                    compared.reverse()
                }
            });
        }
        self.order = order;
        self.selected = resolve_selection(
            self.selected,
            self.order
                .iter()
                .filter_map(|index| self.tasks.get(*index))
                .map(|task| task.proc_id),
        );
        let selected = self.selected;
        let retired = mem::take(&mut self.entries);
        self.entries = self
            .order
            .iter()
            .filter_map(|index| self.tasks.get(*index))
            .map(|task| Self::build(task, selected == Some(task.proc_id)))
            .collect();
        // The rows are re-derived per slot rather than matched by identity, so
        // the slot carries the hover the pointer is still over and never a
        // press begun on whichever task held it.
        carry_hover(
            retired.iter().map(|entry| &entry.row),
            self.entries.iter_mut().map(|entry| &mut entry.row),
        );
        self.restore_band(band);
        self.report_arrangement(&retired, sweep);
    }

    /// Report the visible slots whose row or trace differs from the one they
    /// held — or the whole list when its length changed, since that moved
    /// every row below the change.
    fn report_arrangement(&self, retired: &[TaskEntry], sweep: &mut Sweep<'_, '_>) {
        let Some(ctx) = sweep.ctx() else {
            return;
        };
        let info = self.list_info(&ctx.frame, ctx.scale, ctx.theme);
        if retired.len() != self.entries.len() {
            sweep.report(info.viewport);
            return;
        }
        for row in info.shown(ctx.offset) {
            let (Some(was), Some(now)) = (retired.get(row), self.entries.get(row)) else {
                continue;
            };
            if was != now {
                if let Some(rect) = info.window_rect(row, ctx.offset) {
                    sweep.report(rect);
                }
            }
        }
    }

    /// Offer a pointer `event` to the rows shown at `ctx.offset` and at
    /// `from`, answering the row a press on it activated.
    fn offer_rows(
        &mut self,
        event: &InputEvent,
        from: u64,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<usize> {
        let info = self.list_info(&ctx.frame, ctx.scale, ctx.theme);
        info.offer(
            (from, ctx.offset),
            event,
            damage,
            |row, event, item, drew| {
                let entry = self.entries.get_mut(row)?;
                (entry.row.on_pointer(event, item, drew) == Some(RowAction::Activated))
                    .then_some(())
            },
        )
        .map(|(row, ())| row)
    }

    /// The identity of the task shown at row `row`.
    fn id_at_row(&self, row: usize) -> Option<ProcId> {
        self.tasks
            .get(*self.order.get(row)?)
            .map(|task| task.proc_id)
    }

    /// Select `id`, reporting the two rows its mark moved between.
    fn select(&mut self, id: ProcId, ctx: SectionCtx<'_>, damage: &mut Region) {
        let was = self.selected;
        if was == Some(id) {
            return;
        }
        let info = self.list_info(&ctx.frame, ctx.scale, ctx.theme);
        damage::move_mark(
            was,
            Some(id),
            |marked| {
                info.shown(ctx.offset)
                    .find(|row| self.id_at_row(*row) == Some(marked))
                    .and_then(|row| info.window_rect(row, ctx.offset))
            },
            damage,
        );
        self.selected = Some(id);
        for row in 0..self.entries.len() {
            let selected = self.id_at_row(row) == Some(id);
            if let Some(entry) = self.entries.get_mut(row) {
                entry.row.set_selected(selected);
            }
        }
    }

    /// Which band the content cursor is in, and where within it.
    ///
    /// A raw cursor index means different things either side of a change in
    /// how many rows are shown, so a re-arrangement resolves the cursor
    /// through its band rather than by keeping the number.
    fn focus_band(&self) -> FocusBand {
        match self.focus.checked_sub(HEADER_STOPS) {
            Some(row) => FocusBand::Row(row),
            None => FocusBand::Header(self.focus),
        }
    }

    /// Put the cursor back in `band` against the arrangement now on show.
    ///
    /// A row the arrangement no longer has falls back to the last row it
    /// does have, and a table with no rows at all puts the cursor on the
    /// column headings.
    fn restore_band(&mut self, band: FocusBand) {
        self.focus = match band {
            FocusBand::Header(stop) => stop.min(HEADER_STOPS.saturating_sub(1)),
            FocusBand::Row(row) => match self.entries.len().checked_sub(1) {
                Some(last) => HEADER_STOPS.saturating_add(row.min(last)),
                None => STOP_SORT,
            },
        };
        self.action = self
            .action
            .min(self.focused_action_count().saturating_sub(1));
    }

    /// Build a task's table row and its own CPU sparkline.
    ///
    /// The row's state carries its resource pressure (a Pressure Rail down
    /// its leading edge), its recovery posture (a Signal Bead) and whether it
    /// is the selected row — but deliberately *not* its activity: an activity
    /// in a control's state paints a Heat Seam along the whole lower edge,
    /// which under a table row reads as a rule beneath every working task
    /// rather than as a reading about one. The activity is shown in the
    /// Activity column instead, as the sparkline the heading promises.
    fn build(task: &TaskSummary, selected: bool) -> TaskEntry {
        let mut state = ControlState::idle()
            .with_pressure(task.pressure)
            .with_recovery(task.recovery);
        if selected {
            state = state.with_selection(SelectionState::Selected);
        }
        let mut cells = Vec::with_capacity(COLUMNS.len());
        // An application the desktop launched wears its own picture; every
        // other process resolves one from its kernel-attested name.
        cells.push(
            TaskEntry::cell(COL_TASK, &task.name)
                .with_icon(crate::view::task_icon_kind(task.bundle.as_deref())),
        );
        cells.push(TaskEntry::cell(COL_OWNER, &task.owner.label()));
        cells.push(TaskEntry::cell(COL_STATE, task.state_text()));
        // The Activity column's reading is the sparkline drawn over it, so
        // its cell carries no text of its own to draw underneath.
        cells.push(TaskEntry::cell(COL_ACTIVITY, ""));
        cells.push(TaskEntry::reading(COL_CPU, task.cpu_permille.map(percent)));
        cells.push(TaskEntry::reading(
            COL_MEMORY,
            task.memory_bytes.map(format_bytes),
        ));
        cells.push(TaskEntry::reading(
            COL_DISK,
            task.disk_bytes_per_sec.map(format_rate),
        ));
        // No interface reports a per-task network figure, so the column is
        // unmeasured for every row rather than a zero.
        cells.push(TaskEntry::reading(COL_NETWORK, None));
        // A task the scheduler has not placed reads unmeasured rather than
        // naming a core it is not on.
        cells.push(TaskEntry::reading(
            COL_CORE,
            task.core.map(|core| alloc::format!("{core}")),
        ));

        TaskEntry {
            row: TableRow::new(cells).with_state(state),
            spark: Chart::new(PressureKind::Cpu.signal_role())
                .with_samples(task.cpu_history.iter().copied()),
            bundle: task.bundle.clone(),
            name: task.name.clone(),
        }
    }

    /// The content-cursor stop that focuses shown row `row`, for a caller
    /// that knows a row and needs the cursor position naming it.
    pub(super) fn focus_index_for_row(&self, row: usize) -> usize {
        HEADER_STOPS.saturating_add(row.min(self.entries.len().saturating_sub(1)))
    }

    /// The pinned column-heading rectangle at the top of the primary
    /// region, above the rows that scroll beneath it.
    fn header_rect(frame: &SectionFrame, scale: Scale, theme: &Theme) -> Rect {
        let h = Switchboard::row_item_height(scale, theme).min(frame.primary.height);
        Rect::new(
            frame.primary.left(),
            frame.primary.top(),
            frame.primary.width,
            h,
        )
    }

    /// Mark the column headings' focused heading, against the pinned heading
    /// rectangle the paint and the hit test share.
    fn mark_header(&mut self, index: Option<usize>, sweep: &mut Sweep<'_, '_>) {
        match sweep.ctx {
            Some(ctx) => self.header.set_focus(
                index,
                Self::header_rect(&ctx.frame, ctx.scale, ctx.theme),
                ctx.scale,
                ctx.theme,
                &COLUMN_WEIGHTS,
                sweep.damage,
            ),
            None => self.header.adopt_focus(index),
        }
    }

    /// Which shown row the content cursor is on, or `None` when it is on
    /// the header.
    fn focused_row(&self) -> Option<usize> {
        let row = self.focus.checked_sub(HEADER_STOPS)?;
        (row < self.entries.len()).then_some(row)
    }

    /// Order the table by what a resource device costs, descending, as a
    /// resource pane's "sort tasks by" command asks.
    ///
    /// The sort is committed through the same path a column heading's own
    /// request takes, so what is drawn and what is ordered stay one fact.
    pub(super) fn sort_by_cost(
        &mut self,
        column: TaskCostColumn,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) {
        let index = match column {
            TaskCostColumn::Cpu => COL_CPU,
            TaskCostColumn::Memory => COL_MEMORY,
            TaskCostColumn::Disk => COL_DISK,
        };
        self.apply_sort(index, SortOrder::Descending, ctx, damage);
    }

    /// Apply a sort request from the column headings and re-arrange,
    /// reporting the headings whose caret changed.
    ///
    /// The header only *reports* the request; committing it here and
    /// re-reading it in [`Self::arrange`] keeps what is drawn and what is
    /// ordered the same one fact.
    fn apply_sort(
        &mut self,
        column: usize,
        order: SortOrder,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) {
        self.header.set_sort(
            Some((column, order)),
            Self::header_rect(&ctx.frame, ctx.scale, ctx.theme),
            ctx.scale,
            ctx.theme,
            &COLUMN_WEIGHTS,
            damage,
        );
        self.arrange(&mut Sweep::reporting(ctx, damage));
    }

    /// Feed a key to whichever header control the cursor is on.
    fn header_on_key(
        &mut self,
        key: Key,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<SectionOutcome> {
        match self.focus {
            STOP_SORT => {
                if let Some(HeaderAction::Sort { column, order }) = self.header.on_key(
                    key,
                    Self::header_rect(&ctx.frame, ctx.scale, ctx.theme),
                    ctx.scale,
                    ctx.theme,
                    &COLUMN_WEIGHTS,
                    damage,
                ) {
                    self.apply_sort(column, order, ctx, damage);
                }
                None
            }
            _ => None,
        }
    }

    /// Feed a key to the row the cursor is on: Enter or Space selects it and
    /// asks for its menu, which the screen anchors once the row is in view.
    fn row_on_key(
        &mut self,
        row: usize,
        key: Key,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<SectionOutcome> {
        if !matches!(key, Key::Named(NamedKey::Enter) | Key::Char(' ')) {
            return None;
        }
        let proc_id = self.id_at_row(row)?;
        self.select(proc_id, ctx, damage);
        Some(SectionOutcome::TaskMenu { proc_id, row })
    }
}

impl TaskEntry {
    /// A cell of plain label text for `column`, aligned as that column
    /// declares.
    fn cell(column: usize, text: &str) -> TableCell {
        let align = COLUMNS
            .get(column)
            .map_or(CellAlign::Leading, |spec| spec.align);
        TableCell::new(text).with_align(align)
    }

    /// A cell for a measured figure: the reading when the service measured
    /// it, or the explicit unmeasured mark — rendered disabled, so an
    /// absent figure cannot be mistaken for a small one — when it did not.
    fn reading(column: usize, text: Option<String>) -> TableCell {
        let align = COLUMNS
            .get(column)
            .map_or(CellAlign::Trailing, |spec| spec.align);
        match text {
            Some(text) => TableCell::numeric(text).with_align(align),
            None => TableCell::new(UNMEASURED_READING)
                .with_align(align)
                .with_state(ControlState::disabled()),
        }
    }

    /// The Activity column's own rectangle, which the sparkline is drawn
    /// into — taken from the row's cell spans rather than re-derived.
    fn spark_rect(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
        self.row
            .cell_rects(bounds, scale, theme, &COLUMN_WEIGHTS)
            .get(COL_ACTIVITY)
            .copied()
    }
}

/// Order two tasks by one sortable column.
///
/// The one comparison the sort uses, so every column orders by the value
/// its cell shows. An unmeasured figure sorts *after* every measured one in
/// ascending order rather than as a zero, so "sort by CPU" never buries a
/// real reading under rows nobody measured. A column with no single value
/// to order by compares equal, which — the sort being stable — leaves the
/// rows exactly as they were.
fn compare_column(left: &TaskSummary, right: &TaskSummary, column: usize) -> Ordering {
    match column {
        COL_TASK => left.name.cmp(&right.name),
        COL_OWNER => left.owner.uid.cmp(&right.owner.uid),
        COL_STATE => left.state_text().cmp(right.state_text()),
        COL_CPU => compare_reading(left.cpu_permille, right.cpu_permille),
        COL_MEMORY => compare_reading(left.memory_bytes, right.memory_bytes),
        COL_DISK => compare_reading(left.disk_bytes_per_sec, right.disk_bytes_per_sec),
        COL_CORE => compare_reading(left.core, right.core),
        _ => Ordering::Equal,
    }
}

/// Order two optional readings, an unmeasured one last.
fn compare_reading<T: Ord>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

impl SectionView for TasksSection {
    /// The rows and nothing else: the column headings are pinned inside the
    /// table, and a task's commands are its row's own menu.
    fn anatomy(&self) -> SectionAnatomy {
        SectionAnatomy {
            sidebar_width: 0,
            header_height: 0,
            detail_width: 0,
            impact_width: 0,
            rail_width: 0,
            footer_height: 0,
        }
    }

    fn adopt(&mut self, model: &SwitchboardModel, sweep: &mut Sweep<'_, '_>) {
        self.tasks.clone_from(&model.tasks);
        self.home.clone_from(&model.home);
        self.arrange(sweep);
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    /// The rows scroll beneath the pinned column headings, so the viewport
    /// starts where the headings end.
    fn list_info(&self, frame: &SectionFrame, scale: Scale, theme: &Theme) -> ListInfo {
        let header = Self::header_rect(frame, scale, theme);
        let rows = Rect::new(
            frame.primary.left(),
            frame.primary.top() + to_i32(header.height),
            frame.primary.width,
            frame.primary.height.saturating_sub(header.height),
        );
        ListInfo::rows(rows, self.entries.len(), scale, theme)
    }

    /// One per sortable heading where the cursor traverses the headings; one
    /// on a row, which carries no controls of its own.
    fn focused_action_count(&self) -> usize {
        match self.focus {
            STOP_SORT => self.header.columns().len().max(1),
            _ => 1,
        }
    }

    fn content_focus(&self) -> usize {
        self.focus
    }

    fn set_content_focus(&mut self, index: usize, _sweep: &mut Sweep<'_, '_>) {
        self.focus = index;
    }

    /// The header's stops, then one per shown row — so the headings stay
    /// reachable when a sample leaves no rows at all.
    fn focus_span(&self) -> usize {
        HEADER_STOPS.saturating_add(self.entries.len())
    }

    fn focus_row(&self, index: usize) -> Option<usize> {
        let row = index.checked_sub(HEADER_STOPS)?;
        (row < self.entries.len()).then_some(row)
    }

    fn row_action(&self) -> usize {
        self.action
    }

    fn set_row_action(&mut self, index: usize, sweep: &mut Sweep<'_, '_>) {
        self.action = index;
        // The column headings hold their own internal cursor, so the shared
        // action cursor is mirrored onto them rather than kept as a second,
        // separately-moving idea of the same thing.
        if self.focus == STOP_SORT {
            self.mark_header(Some(index), sweep);
        }
    }

    fn activate_focused(
        &mut self,
        key: Key,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<SectionOutcome> {
        if self.focus < HEADER_STOPS {
            return self.header_on_key(key, ctx, damage);
        }
        let row = self.focused_row()?;
        self.row_on_key(row, key, ctx, damage)
    }

    fn render(&self, surface: &mut Surface, ctx: SectionCtx<'_>, artwork: &mut dyn IconArtwork) {
        self.header.render(
            surface,
            Self::header_rect(&ctx.frame, ctx.scale, ctx.theme),
            ctx.scale,
            ctx.theme,
            &COLUMN_WEIGHTS,
        );

        let info = self.list_info(&ctx.frame, ctx.scale, ctx.theme);
        info.view(ctx.offset).paint(surface, |rows| {
            for index in info.shown(ctx.offset) {
                let Some(entry) = self.entries.get(index) else {
                    break;
                };
                let item = info.item_rect(index);
                // The row's leading icon is the application's own picture
                // where the desktop attests a bundle for the process, resolved
                // at the side the row will draw it at.
                let side = TableRow::icon_side(item, ctx.scale, ctx.theme);
                let request = task_icon(entry.bundle.as_deref(), &entry.name, self.home.as_deref());
                let picture = artwork.artwork(request, side);
                entry
                    .row
                    .render(rows, item, ctx.scale, ctx.theme, &COLUMN_WEIGHTS, picture);
                if let Some(rect) = entry.spark_rect(item, ctx.scale, ctx.theme) {
                    entry.spark.render(rows, rect, ctx.scale, ctx.theme);
                }
            }
        });
    }

    fn on_pointer(
        &mut self,
        event: &InputEvent,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<SectionOutcome> {
        if let Some(HeaderAction::Sort { column, order }) = self.header.on_pointer(
            event,
            Self::header_rect(&ctx.frame, ctx.scale, ctx.theme),
            ctx.scale,
            ctx.theme,
            &COLUMN_WEIGHTS,
        ) {
            self.apply_sort(column, order, ctx, damage);
            return None;
        }

        let pressed = self.offer_rows(event, ctx.offset, ctx, damage);
        if let Some(id) = pressed.and_then(|row| self.id_at_row(row)) {
            // Selection names the task, not the position it happens to
            // occupy: a re-sort must not move the highlight to whatever row
            // slid into that slot.
            self.select(id, ctx, damage);
        }
        None
    }

    /// A secondary press on a row selects it and asks for its menu at the
    /// press; anywhere else it asks for nothing.
    fn context_press(
        &mut self,
        at: Point,
        ctx: SectionCtx<'_>,
        damage: &mut Region,
    ) -> Option<SectionOutcome> {
        let row = self
            .list_info(&ctx.frame, ctx.scale, ctx.theme)
            .line_at(at, ctx.offset)?;
        let proc_id = self.id_at_row(row)?;
        self.select(proc_id, ctx, damage);
        Some(SectionOutcome::Action(SwitchboardAction::TaskMenu {
            proc_id,
            anchor: Rect::new(at.x, at.y, 0, 0),
        }))
    }

    fn rehover(&mut self, still: &InputEvent, from: u64, ctx: SectionCtx<'_>, damage: &mut Region) {
        self.offer_rows(still, from, ctx, damage);
    }

    fn apply_focus_marks(&mut self, focused: bool, sweep: &mut Sweep<'_, '_>) {
        let (stop, action) = (self.focus, self.action);
        let row_focus = self.focused_row();
        self.mark_header((focused && stop == STOP_SORT).then_some(action), sweep);

        // A row carries no controls of its own, so it takes the ring itself
        // rather than passing it to an action.
        let list = sweep
            .ctx()
            .map(|ctx| self.list_info(&ctx.frame, ctx.scale, ctx.theme));
        for (i, entry) in self.entries.iter_mut().enumerate() {
            let here = focused && row_focus == Some(i);
            let was = entry.row.state();
            entry.row.set_focused(here);
            entry.row.set_in_focus_field(here);
            sweep.restyled(was, entry.row.state(), |ctx| {
                list?.window_rect(i, ctx.offset)
            });
        }
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
