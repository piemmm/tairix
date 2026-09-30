//! The composed editor window: the editor, where it is scrolled to, the
//! chrome around it, and the one input entry point.
//!
//! Nothing here reads a file or asks a service. Input runs a command on the
//! editor and records the damage it did; what needs a worker or the desktop is
//! answered as a [`Request`] for `Run` to carry out.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuRow, AppMenuShortcut,
    WINDOW_TITLE_MAX,
};
use tairix_controls::{
    Button, ButtonContent, ControlRole, Dialog, DialogAction, ScrollAction, ScrollBar, ScrollModel,
    ScrollOrientation, ScrollRange, SearchField, SelectionState, TextAction, TextField,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{ClickRun, InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_sandbox::textsyntax::LexedBatch;
use tairix_syntax::{Diagnostic, Format};
use tairix_theme::Theme;

use crate::detect::{Indent, LineEnding};
use crate::editor::{Command, Editor, Effect, Mode, Motion};
use crate::find::{Options, Pattern, PatternError, Search, Step};
use crate::hex::{self, HexLayout};
use crate::highlight::LexJob;
use crate::layout::{Faces, Layout, FIND_BUTTONS};
use crate::text::{self, Row};

/// The name a window's document goes by until it is saved.
pub const UNTITLED: &str = "Untitled";

/// The application's name, as window titles end.
pub const APP_TITLE: &str = "TextEdit";

/// What stands between a document's name and [`APP_TITLE`] in its title.
const TITLE_SEPARATOR: &str = " \u{2014} ";

/// How a window's title marks a document changed since its file, and one
/// that may not be saved over.
const MODIFIED_MARK: &str = "*";
const READ_ONLY_MARK: &str = " (read-only)";

/// Longest selection the find field is filled from.
const FIND_SEED_BYTES: usize = 256;

/// How long a settings store goes unedited before its parser is asked about
/// it: long enough that typing does not ask once a keystroke.
pub const CHECK_SETTLE_NS: u64 = 400_000_000;

/// How the window's document may be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Access {
    /// A new document with no file yet.
    Untitled,
    /// A file this window was handed read-only.
    ReadOnly,
    /// A file this window may save over.
    Writable,
}

/// A menu the window asks the desktop to open.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuKind {
    /// The window's menu, opened by a secondary press anywhere in it: the
    /// clipboard rows, then File, Edit, Find and View as submenus. The window
    /// has no menu bar.
    Window,
    /// The formats, from the status band.
    Format,
    /// Text or hex, from the status band.
    Mode,
    /// The line-ending conventions, from the status band.
    LineEnding,
    /// The indentation units, from the status band.
    Indent,
}

/// The status band's clickable fields, right to left.
const STATUS_MENUS: [Option<MenuKind>; crate::layout::STATUS_FIELDS] = [
    None,
    Some(MenuKind::Indent),
    Some(MenuKind::LineEnding),
    Some(MenuKind::Mode),
    Some(MenuKind::Format),
];

/// What the window asks of `Run`.
#[derive(Debug)]
pub enum Request {
    /// Save the document where it came from, or ask where when it came from
    /// nowhere writable.
    Save,
    /// Ask where to save the document.
    SaveAs,
    /// Ask the desktop's picker for a document to open.
    Open,
    /// Open a new, empty window.
    NewWindow,
    /// Close this window: its document is saved or its changes were given up.
    Close,
    /// Save, then close once the save has landed.
    SaveThenClose,
    /// Put these bytes on the clipboard.
    Copy(Vec<u8>),
    /// Paste what the clipboard holds.
    Paste,
    /// Open a menu, anchored on `anchor` in the window.
    Menu {
        /// Which.
        kind: MenuKind,
        /// Where, in window pixels.
        anchor: Rect,
    },
    /// Run a search over a snapshot of the document.
    Search {
        /// Which search this is, echoed with its answer.
        id: u64,
        /// The search.
        search: Search,
        /// What to put in each match's place, for a Replace All.
        replacement: Option<Vec<u8>>,
    },
    /// Convert every line break to a convention, over a snapshot.
    Convert(LineEnding),
}

/// What an input event led to beyond the damage it recorded.
#[derive(Debug, Default)]
pub struct Outcome {
    /// What `Run` is asked to do.
    pub request: Option<Request>,
    /// The bands moved: lay the window out again and repaint it whole.
    pub relayout: bool,
}

impl Outcome {
    const fn none() -> Self {
        Self {
            request: None,
            relayout: false,
        }
    }

    const fn asking(request: Request) -> Self {
        Self {
            request: Some(request),
            relayout: false,
        }
    }

    const fn relaid() -> Self {
        Self {
            request: None,
            relayout: true,
        }
    }
}

/// What a menu row or a key asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Open a new window.
    NewWindow,
    /// Open a document.
    Open,
    /// Save.
    Save,
    /// Save somewhere new.
    SaveAs,
    /// Close the window.
    Close,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Cut the selection.
    Cut,
    /// Copy the selection.
    Copy,
    /// Paste.
    Paste,
    /// Select everything.
    SelectAll,
    /// Indent the selected lines.
    Indent,
    /// Outdent the selected lines.
    Outdent,
    /// Comment the selected lines out or in.
    ToggleComment,
    /// Switch between inserting and overwriting.
    ToggleOverwrite,
    /// Show the find bar.
    Find,
    /// Show the find bar at its replace field.
    Replace,
    /// Find the next match.
    FindNext,
    /// Find the match before.
    FindPrevious,
    /// Ask for a line to go to.
    GoToLine,
    /// Go to the next problem the parser reported.
    NextProblem,
    /// Show the document as text or hex.
    Mode(Mode),
    /// Colour and check the document as a format.
    Format(Format),
    /// Put tab stops this far apart.
    TabWidth(u8),
    /// Indent with this.
    Indentation(Indent),
    /// Convert the line breaks.
    LineEnding(LineEnding),
}

/// The actions with no argument, by id: an action's position here is its
/// menu id, less one.
const PLAIN_ACTIONS: [Action; 21] = [
    Action::NewWindow,
    Action::Open,
    Action::Save,
    Action::SaveAs,
    Action::Close,
    Action::Undo,
    Action::Redo,
    Action::Cut,
    Action::Copy,
    Action::Paste,
    Action::SelectAll,
    Action::Indent,
    Action::Outdent,
    Action::ToggleComment,
    Action::ToggleOverwrite,
    Action::Find,
    Action::Replace,
    Action::FindNext,
    Action::FindPrevious,
    Action::GoToLine,
    Action::NextProblem,
];

const MODES: [Mode; 2] = [Mode::Text, Mode::Hex];
const TAB_WIDTHS: [u8; 3] = [2, 4, 8];
const INDENTS: [Indent; 4] = [
    Indent::Tab,
    Indent::Spaces(2),
    Indent::Spaces(4),
    Indent::Spaces(8),
];
const ENDINGS: [LineEnding; 2] = [LineEnding::Lf, LineEnding::CrLf];

/// Where each argument-carrying family's ids start.
const FORMAT_IDS: u16 = 100;
const TAB_IDS: u16 = 200;
const INDENT_IDS: u16 = 300;
const ENDING_IDS: u16 = 400;
const MODE_IDS: u16 = 500;

impl Action {
    /// The menu id this action is chosen by.
    #[must_use]
    pub fn id(self) -> u16 {
        let at =
            |base: u16, index: Option<usize>| base + u16::try_from(index.unwrap_or(0)).unwrap_or(0);
        match self {
            Self::Mode(mode) => at(MODE_IDS, MODES.iter().position(|&m| m == mode)),
            Self::Format(format) => FORMAT_IDS + u16::from(format.index()),
            Self::TabWidth(width) => TAB_IDS + u16::from(width),
            Self::Indentation(indent) => at(INDENT_IDS, INDENTS.iter().position(|&i| i == indent)),
            Self::LineEnding(ending) => at(ENDING_IDS, ENDINGS.iter().position(|&e| e == ending)),
            plain => at(1, PLAIN_ACTIONS.iter().position(|&a| a == plain)),
        }
    }

    /// The action a menu id names, if any.
    #[must_use]
    pub fn from_id(id: u16) -> Option<Self> {
        let offset = |base: u16| usize::from(id - base);
        match id {
            0 => None,
            1..FORMAT_IDS => PLAIN_ACTIONS.get(usize::from(id - 1)).copied(),
            FORMAT_IDS..TAB_IDS => u8::try_from(id - FORMAT_IDS)
                .ok()
                .and_then(Format::from_index)
                .map(Self::Format),
            TAB_IDS..INDENT_IDS => u8::try_from(id - TAB_IDS)
                .ok()
                .filter(|width| TAB_WIDTHS.contains(width))
                .map(Self::TabWidth),
            INDENT_IDS..ENDING_IDS => INDENTS
                .get(offset(INDENT_IDS))
                .copied()
                .map(Self::Indentation),
            ENDING_IDS..MODE_IDS => ENDINGS
                .get(offset(ENDING_IDS))
                .copied()
                .map(Self::LineEnding),
            _ => MODES.get(offset(MODE_IDS)).copied().map(Self::Mode),
        }
    }
}

/// Where keyboard input goes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    Grid,
    Find,
    Replace,
}

/// What a drag in the grid extends by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Grain {
    Character,
    Word,
    Line,
}

/// A selection being dragged out: what it extends by and the unit it began
/// on, which stays selected whichever way it is dragged.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Drag {
    grain: Grain,
    anchor: (usize, usize),
}

/// A modal question over the window.
#[derive(Debug)]
enum Modal {
    /// Save the changes before closing?
    Close(Dialog),
    /// Which line to go to?
    GoTo(Dialog, TextField),
}

/// Pixels a bar stands past the first row or column shown, short of a whole
/// cell, and the position they are past: what a slow wheel has moved but not
/// yet a cell's worth. Left at one position, they count for nothing at
/// another.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Carry<P> {
    at: P,
    pixels: u64,
}

impl<P: PartialEq> Carry<P> {
    fn at(&self, position: &P) -> u64 {
        if self.at == *position {
            self.pixels
        } else {
            0
        }
    }
}

/// One editor window's state.
#[derive(Debug)]
pub struct View {
    editor: Editor,
    name: String,
    access: Access,
    /// The first row shown in the text view.
    top: Row,
    /// The first row shown in the hex view.
    hex_top: usize,
    /// The first column shown.
    left: usize,
    /// The vertical bar's pixels past `(top, hex_top)`.
    down: Carry<(Row, usize)>,
    /// The horizontal bar's pixels past `left`.
    across: Carry<usize>,
    find_open: bool,
    find: SearchField,
    replace: TextField,
    find_buttons: [Button; FIND_BUTTONS.len()],
    options: Options,
    hex_pattern: bool,
    focus: Focus,
    vertical: ScrollBar,
    horizontal: ScrollBar,
    clicks: ClickRun,
    drag: Option<Drag>,
    pointer: Point,
    modifiers: Modifiers,
    message: Option<String>,
    search: Option<u64>,
    next_search: u64,
    modal: Option<Modal>,
    double_click: Duration64,
    /// The widest row measured in this view, in columns: what the
    /// horizontal bar spans. It grows as wider rows come into view; `None`
    /// until the view has been measured at all.
    widest: Option<usize>,
    /// The user chose the format, so no later detection replaces it.
    format_chosen: bool,
    /// The document generation [`View::check_due`] last saw, and when it
    /// first saw it.
    seen: (u64, u64),
}

impl View {
    /// A window editing `editor`'s document, called `name`, which it may
    /// write as `access` says; presses pair under `double_click`.
    #[must_use]
    pub fn new(editor: Editor, name: String, access: Access, double_click: Duration64) -> Self {
        let flat = || ScrollModel::new(ScrollRange::new(0, 0, 0), 1, 1);
        Self {
            editor,
            name,
            access,
            top: Row::default(),
            hex_top: 0,
            left: 0,
            down: Carry::default(),
            across: Carry::default(),
            find_open: false,
            find: SearchField::new().with_placeholder("Find"),
            replace: TextField::new().with_placeholder("Replace with"),
            find_buttons: FIND_BUTTONS.map(Button::labelled),
            options: Options::default(),
            hex_pattern: false,
            focus: Focus::Grid,
            vertical: ScrollBar::new(ScrollOrientation::Vertical, flat()),
            horizontal: ScrollBar::new(ScrollOrientation::Horizontal, flat()),
            clicks: ClickRun::new(),
            drag: None,
            pointer: Point::new(-1, -1),
            modifiers: Modifiers::default(),
            message: None,
            search: None,
            next_search: 1,
            modal: None,
            double_click,
            widest: None,
            format_chosen: false,
            seen: (0, 0),
        }
    }

    /// The editor.
    #[must_use]
    pub const fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The editor, for `Run` to feed answers to.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// What the document is called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How the document may be written.
    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }

    /// The document became `name`, writable, saved as `generation`.
    pub fn saved(&mut self, generation: u64, name: Option<String>) {
        self.editor.saved(generation);
        if let Some(name) = name {
            self.name = name;
        }
        self.access = Access::Writable;
        self.message = Some(String::from("Saved"));
    }

    /// Say `message` in the status band.
    pub fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Write what the window's title reads over `title`, reusing its room:
    /// the name shortened to what the title field holds beside the marks.
    pub fn write_title(&self, title: &mut String) {
        title.clear();
        let modified = if self.editor.is_modified() {
            MODIFIED_MARK
        } else {
            ""
        };
        let read_only = if self.access == Access::ReadOnly {
            READ_ONLY_MARK
        } else {
            ""
        };
        let budget = WINDOW_TITLE_MAX
            .saturating_sub(modified.len() + read_only.len())
            .saturating_sub(TITLE_SEPARATOR.len() + APP_TITLE.len());
        title.push_str(modified);
        tairix_browse::vfs::push_title_name(title, &self.name, budget);
        title.push_str(read_only);
        title.push_str(TITLE_SEPARATOR);
        title.push_str(APP_TITLE);
    }

    /// The status band's message: the last thing said, else what the
    /// document's parser reported.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Whether the find bar is open.
    #[must_use]
    pub const fn find_open(&self) -> bool {
        self.find_open
    }

    /// The find field, the replace field and the find bar's buttons.
    #[must_use]
    pub fn find_controls(&self) -> (&SearchField, &TextField, &[Button; FIND_BUTTONS.len()]) {
        (&self.find, &self.replace, &self.find_buttons)
    }

    /// The two scrollbars.
    #[must_use]
    pub const fn scrollbars(&self) -> (&ScrollBar, &ScrollBar) {
        (&self.vertical, &self.horizontal)
    }

    /// The modal question showing, if any: its dialog and, for a question
    /// with an answer to type, its field.
    #[must_use]
    pub fn modal(&self) -> Option<(&Dialog, Option<&TextField>)> {
        match &self.modal {
            Some(Modal::Close(dialog)) => Some((dialog, None)),
            Some(Modal::GoTo(dialog, field)) => Some((dialog, Some(field))),
            None => None,
        }
    }

    /// Where the scroll stands: the first text row, the first hex row, the
    /// first column.
    #[must_use]
    pub const fn scroll(&self) -> (Row, usize, usize) {
        (self.top, self.hex_top, self.left)
    }

    /// The layout of a `width`×`height` window.
    #[must_use]
    pub fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
    ) -> Layout {
        Layout::for_window(
            width,
            height,
            theme,
            scale,
            faces,
            self.find_open,
            self.gutter_digits(),
        )
    }

    fn gutter_digits(&self) -> u32 {
        match self.editor.mode() {
            Mode::Hex => 0,
            Mode::Text => digits(self.editor.document().line_count()).max(3),
        }
    }

    /// The rows the text view shows, top first, at most `count`.
    pub fn visible_rows(&self, count: usize) -> impl Iterator<Item = Row> + '_ {
        let document = self.editor.document();
        core::iter::successors(Some(self.top), move |&row| text::next_row(document, row))
            .take(count)
    }

    /// The lines the text view shows.
    fn visible_lines(&self, layout: &Layout) -> core::ops::Range<usize> {
        let last = self
            .visible_rows(layout.rows().max(1))
            .last()
            .map_or(self.top.line, |row| row.line);
        self.top.line..last + 1
    }

    /// The next batch worth lexing to colour what the window shows.
    pub fn lex_job(&mut self, layout: &Layout) -> Option<LexJob> {
        if self.editor.mode() == Mode::Hex {
            return None;
        }
        let lines = self.visible_lines(layout);
        let (highlight, document) = self.editor.highlight_mut();
        highlight.next_job(document, lines).unwrap_or(None)
    }

    /// Take in the lexer's answer to batch `id`, repainting the rows it
    /// coloured.
    pub fn lexed(&mut self, id: u64, batch: &LexedBatch, layout: &Layout, damage: &mut Region) {
        let lines = self.editor.highlight_mut().0.adopt(id, batch);
        if lines.is_empty() {
            return;
        }
        for (index, row) in self.visible_rows(layout.rows().max(1)).enumerate() {
            if lines.contains(&row.line) {
                damage.add(layout.row_rect(index));
            }
        }
    }

    /// Batch `id` could not be lexed. A window that then gives up colouring
    /// is drawn plain and says why.
    pub fn lex_failed(&mut self, id: u64, layout: &Layout, damage: &mut Region) {
        let highlight = self.editor.highlight_mut().0;
        highlight.failed(id);
        if highlight.stopped().is_some() {
            damage.add(layout.grid());
            damage.add(layout.status());
        }
    }

    /// Adopt the document converted to `to` as of `generation`.
    pub fn converted(
        &mut self,
        generation: u64,
        chunks: Option<Vec<Vec<u8>>>,
        to: LineEnding,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let before = self.editor.selection();
        let effect = self.editor.converted(generation, chunks, to);
        self.message = None;
        damage.add(layout.status());
        self.report(effect, before, layout, damage)
    }

    /// The document's opening bytes say it is written in `format`. A format
    /// the user chose stands.
    pub fn detected(&mut self, format: Format, layout: &Layout, damage: &mut Region) -> Outcome {
        if self.format_chosen || format == self.editor.format() {
            return Outcome::none();
        }
        self.command(&Command::SetFormat(format), layout, damage)
    }

    /// Take in what the document's store parser said about `generation`.
    pub fn checked(
        &mut self,
        generation: u64,
        diagnostics: Vec<Diagnostic>,
        layout: &Layout,
        damage: &mut Region,
    ) {
        self.editor.checked(generation, diagnostics);
        damage.add(layout.gutter());
        damage.add(layout.status());
    }

    /// When the document's store parser is due to be asked about it, seen at
    /// `now_ns`: once edits have paused for [`CHECK_SETTLE_NS`], or `None`
    /// when it needs no check. The document as opened is due at once.
    pub fn check_due(&mut self, now_ns: u64) -> Option<u64> {
        if !self.editor.wants_check() {
            return None;
        }
        let generation = self.editor.generation();
        if self.seen.0 != generation {
            self.seen = (generation, now_ns);
            return Some(now_ns.saturating_add(CHECK_SETTLE_NS));
        }
        Some(if generation == 0 {
            0
        } else {
            self.seen.1.saturating_add(CHECK_SETTLE_NS)
        })
    }

    /// Whether search `id` is still the one this window wants answered.
    #[must_use]
    pub fn wants_search(&self, id: u64) -> bool {
        self.search == Some(id)
    }

    /// Bring the scroll and the bars into line with the caret and report
    /// what moved.
    pub fn settle(&mut self, layout: &Layout, damage: &mut Region) {
        // Only the text view measures its widest row; the hex view's width is
        // its layout's.
        let unmeasured = self.editor.mode() == Mode::Text && self.widest.is_none();
        if self.reveal_caret(layout) || unmeasured {
            damage.add(layout.gutter());
            damage.add(layout.grid());
            self.measure(self.top.line..usize::MAX, layout);
        }
        self.sync_bars(layout, damage);
        damage.add(layout.status());
    }

    /// Widen the horizontal span to the widest visible row among `lines`.
    fn measure(&mut self, lines: core::ops::Range<usize>, layout: &Layout) {
        if self.editor.mode() == Mode::Hex {
            return;
        }
        let document = self.editor.document();
        let tab = self.editor.tab_width();
        let widest = self
            .visible_rows(layout.rows().max(1))
            .filter(|row| lines.contains(&row.line))
            .map(|row| {
                let bounds = text::row_bounds(document, row);
                text::column_of(document, bounds, bounds.end, tab)
            })
            .max()
            .unwrap_or(0);
        self.widest = Some(self.widest.map_or(widest, |seen| seen.max(widest)));
    }

    /// Scroll so the caret is in view; whether anything moved.
    fn reveal_caret(&mut self, layout: &Layout) -> bool {
        let rows = layout.rows().max(1);
        let columns = layout.columns().max(1);
        let before = (self.top, self.hex_top, self.left);
        let document = self.editor.document();
        let caret = self.editor.selection().head;
        let column = match self.editor.mode() {
            Mode::Text => {
                let row = text::row_of(document, caret);
                if row < self.top {
                    self.top = row;
                } else if !self.row_within(row, rows) {
                    let index = text::row_index(document, row);
                    self.top = text::row_at(document, index.saturating_sub(rows - 1));
                }
                text::column_of(
                    document,
                    text::row_bounds(document, row),
                    caret,
                    self.editor.tab_width(),
                )
            }
            Mode::Hex => {
                let row = caret / hex::BYTES_PER_ROW;
                if row < self.hex_top {
                    self.hex_top = row;
                } else if row >= self.hex_top + rows {
                    self.hex_top = row + 1 - rows;
                }
                HexLayout::for_len(document.len()).caret_column(self.editor.hex_caret())
            }
        };
        if column < self.left {
            self.left = column;
        } else if column >= self.left + columns {
            self.left = column + 1 - columns;
        }
        before != (self.top, self.hex_top, self.left)
    }

    /// Whether `row` is one of the `rows` rows from the top.
    fn row_within(&self, row: Row, rows: usize) -> bool {
        let document = self.editor.document();
        let top = text::row_index(document, self.top);
        (top..top.saturating_add(rows)).contains(&text::row_index(document, row))
    }

    /// Bring both bars into line with the scroll: pixel ranges over the rows
    /// and the columns there are, so a wheel, a step and a page all move by
    /// the cells they span.
    fn sync_bars(&mut self, layout: &Layout, damage: &mut Region) {
        let (cell_w, cell_h) = layout.cell();
        let (cell_w, cell_h) = (u64::from(cell_w.max(1)), u64::from(cell_h.max(1)));
        let rows = layout.rows() as u64;
        let columns = layout.columns();
        let document = self.editor.document();
        let (content, first, wide) = match self.editor.mode() {
            Mode::Text => (
                document.row_count(),
                text::row_index(document, self.top),
                self.widest.unwrap_or(0),
            ),
            Mode::Hex => (
                hex::rows(document.len()),
                self.hex_top,
                HexLayout::for_len(document.len()).width(),
            ),
        };
        let down = (first as u64)
            .saturating_mul(cell_h)
            .saturating_add(self.down.at(&(self.top, self.hex_top)));
        let vertical = ScrollModel::in_pixels(
            ScrollRange::new(
                (content as u64).max(rows).saturating_mul(cell_h),
                rows.saturating_mul(cell_h),
                down,
            ),
            cell_h,
        );
        let across = (self.left as u64)
            .saturating_mul(cell_w)
            .saturating_add(self.across.at(&self.left));
        let horizontal = ScrollModel::in_pixels(
            ScrollRange::new(
                (wide.max(self.left + columns) as u64).saturating_mul(cell_w),
                (columns as u64).saturating_mul(cell_w),
                across,
            ),
            cell_w,
        );
        if vertical != self.vertical.model() {
            self.vertical.set_model(vertical);
            damage.add(layout.vertical_bar());
        }
        if horizontal != self.horizontal.model() {
            self.horizontal.set_model(horizontal);
            damage.add(layout.horizontal_bar());
        }
    }

    /// Report the rows an editor effect touched, and whether the bands must
    /// move.
    fn report(
        &mut self,
        effect: Effect,
        before: crate::selection::Selection,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if let Some(refused) = effect.refused {
            self.message = Some(refused.to_string());
        } else if effect.text.is_some() {
            self.message = None;
        }
        if effect.view {
            self.widest = None;
        }
        if effect.view || self.gutter_digits() != layout.gutter_digits() {
            return Outcome::relaid();
        }
        if self.editor.mode() == Mode::Hex {
            self.damage_hex_rows(&effect, before, layout, damage);
            self.settle(layout, damage);
            return Outcome::none();
        }
        if let Some(lines) = effect.text {
            self.measure(
                lines.first..lines.last.map_or(usize::MAX, |last| last + 1),
                layout,
            );
        }
        let document = self.editor.document();
        let lines_of = |selection: crate::selection::Selection| {
            let range = selection.range();
            document.line_of(range.start)..=document.line_of(range.end)
        };
        let (was, now) = (lines_of(before), lines_of(self.editor.selection()));
        for (index, row) in self.visible_rows(layout.rows().max(1)).enumerate() {
            let text_hit = effect.text.is_some_and(|lines| {
                row.line >= lines.first && lines.last.is_none_or(|last| row.line <= last)
            });
            let selection_hit =
                effect.selection && (was.contains(&row.line) || now.contains(&row.line));
            if text_hit || selection_hit {
                damage.add(layout.row_rect(index));
            }
        }
        self.settle(layout, damage);
        Outcome::none()
    }

    /// Report the hex rows on screen that `effect` changed: those showing the
    /// selection before and after it, and those from the first changed line
    /// to the last, or to the end where every byte after it moved.
    fn damage_hex_rows(
        &self,
        effect: &Effect,
        before: crate::selection::Selection,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let shown = self.hex_top..self.hex_top + layout.rows();
        let mut rows = |bytes: core::ops::Range<usize>| {
            let first = (bytes.start / hex::BYTES_PER_ROW).max(shown.start);
            let last = bytes.end.saturating_sub(1).max(bytes.start) / hex::BYTES_PER_ROW;
            for row in first..=last.min(shown.end.saturating_sub(1)) {
                damage.add(layout.row_rect(row - shown.start));
            }
        };
        if effect.selection {
            rows(before.range());
            rows(self.editor.selection().range());
        }
        if let Some(lines) = effect.text {
            let document = self.editor.document();
            let end = lines
                .last
                .map_or(usize::MAX, |last| document.line_bounds(last).next);
            rows(document.line_start(lines.first)..end);
        }
    }

    /// Run `command` on the editor and report what it did.
    fn command(&mut self, command: &Command, layout: &Layout, damage: &mut Region) -> Outcome {
        let before = self.editor.selection();
        let effect = self
            .editor
            .run(command, layout.rows().saturating_sub(1).max(1));
        self.report(effect, before, layout, damage)
    }

    /// Carry out `action`.
    pub fn act(&mut self, action: Action, layout: &Layout, damage: &mut Region) -> Outcome {
        let command = match action {
            Action::NewWindow => return Outcome::asking(Request::NewWindow),
            Action::Open => return Outcome::asking(Request::Open),
            Action::Save => return Outcome::asking(Request::Save),
            Action::SaveAs => return Outcome::asking(Request::SaveAs),
            Action::Close => return self.close_requested(layout, damage),
            Action::Copy => return self.copy(),
            Action::Cut => return self.cut(layout, damage),
            Action::Paste => return Outcome::asking(Request::Paste),
            Action::Find => return self.open_find(Focus::Find, layout, damage),
            Action::Replace => return self.open_find(Focus::Replace, layout, damage),
            Action::FindNext => return self.search(false, None, layout, damage),
            Action::FindPrevious => return self.search(true, None, layout, damage),
            Action::GoToLine => return self.ask_go_to(layout, damage),
            Action::LineEnding(ending) => {
                self.message = Some(String::from("Converting line endings\u{2026}"));
                damage.add(layout.status());
                return Outcome::asking(Request::Convert(ending));
            }
            Action::Undo => Command::Undo,
            Action::Redo => Command::Redo,
            Action::SelectAll => Command::SelectAll,
            Action::Indent => Command::Indent,
            Action::Outdent => Command::Outdent,
            Action::ToggleComment => Command::ToggleComment,
            Action::ToggleOverwrite => Command::ToggleOverwrite,
            Action::NextProblem => Command::NextProblem,
            Action::Mode(mode) => Command::SetMode(mode),
            Action::Format(format) => {
                self.format_chosen = true;
                Command::SetFormat(format)
            }
            Action::TabWidth(width) => Command::SetTabWidth(width),
            Action::Indentation(indent) => Command::SetIndent(indent),
        };
        self.command(&command, layout, damage)
    }

    fn copy(&mut self) -> Outcome {
        match self.editor.copy() {
            Ok(bytes) if !bytes.is_empty() => Outcome::asking(Request::Copy(bytes)),
            Ok(_) => Outcome::none(),
            Err(why) => {
                self.message = Some(why.to_string());
                Outcome::none()
            }
        }
    }

    fn cut(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let before = self.editor.selection();
        let (effect, bytes) = self.editor.cut();
        let mut outcome = self.report(effect, before, layout, damage);
        if let Some(bytes) = bytes {
            outcome.request = Some(Request::Copy(bytes));
        }
        outcome
    }

    /// Paste `bytes`, which the clipboard held: into the find field that has
    /// the keyboard, else into the document.
    pub fn paste(&mut self, bytes: &[u8], layout: &Layout, damage: &mut Region) -> Outcome {
        if self.focus != Focus::Grid {
            let Ok(text) = core::str::from_utf8(bytes) else {
                self.message = Some(String::from("The clipboard does not hold text"));
                damage.add(layout.status());
                return Outcome::none();
            };
            if self.focus == Focus::Find {
                self.find.insert_text(text, layout.find_field(), damage);
            } else {
                self.replace
                    .insert_text(text, layout.replace_field(), damage);
            }
            return Outcome::none();
        }
        let before = self.editor.selection();
        let effect = self.editor.replace_selection(bytes);
        self.report(effect, before, layout, damage)
    }

    /// The user asked to close the window.
    pub fn close_requested(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editor.is_modified() {
            return Outcome::asking(Request::Close);
        }
        let title = alloc::format!("Save the changes to \u{201c}{}\u{201d}?", self.name);
        let dialog = Dialog::new(title)
            .with_message("Your changes will be lost if you do not save them.")
            .with_actions(alloc::vec![
                Button::labelled("Cancel"),
                Button::new(
                    ButtonContent::Label(String::from("Don\u{2019}t Save")),
                    ControlRole::Destructive
                ),
                Button::new(
                    ButtonContent::Label(String::from("Save")),
                    ControlRole::Recommended
                ),
            ]);
        self.modal = Some(Modal::Close(dialog));
        damage.add(layout.window());
        Outcome::none()
    }

    fn ask_go_to(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let dialog = Dialog::new("Go to line").with_actions(alloc::vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Go")),
                ControlRole::Recommended
            ),
        ]);
        let mut field = TextField::new()
            .with_placeholder("Line number")
            .with_max_len(20);
        field.set_focused(true);
        self.modal = Some(Modal::GoTo(dialog, field));
        damage.add(layout.window());
        Outcome::none()
    }

    /// Where a modal question is drawn, for a dialog in a `window`.
    #[must_use]
    pub fn modal_rect(
        dialog: &Dialog,
        window: Rect,
        field: bool,
        scale: Scale,
        theme: &Theme,
    ) -> Rect {
        let width = scale.scale_length(420).min(window.width);
        let content = if field {
            TextField::height(scale, theme)
        } else {
            0
        };
        let height = dialog
            .height_for_content(content, width, scale, theme)
            .min(window.height);
        let x = window
            .left()
            .saturating_add_unsigned((window.width - width) / 2);
        let y = window
            .top()
            .saturating_add_unsigned((window.height - height) / 3);
        Rect::new(x, y, width, height)
    }

    fn modal_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let window = layout.window();
        let answer = match &mut self.modal {
            Some(Modal::Close(dialog)) => {
                let bounds = Self::modal_rect(dialog, window, false, scale, theme);
                dialog.on_pointer(event, bounds, scale, theme, damage)
            }
            Some(Modal::GoTo(dialog, field)) => {
                let bounds = Self::modal_rect(dialog, window, true, scale, theme);
                if let Some(content) = dialog.content_rect(bounds, scale, theme) {
                    field.on_pointer(event, content, scale, theme, damage);
                }
                dialog.on_pointer(event, bounds, scale, theme, damage)
            }
            None => None,
        };
        match answer {
            Some(DialogAction::ActionActivated { index }) => {
                self.answer_modal(index, layout, damage)
            }
            None => Outcome::none(),
        }
    }

    /// A key while a question is showing: a typed answer repaints the field,
    /// a moved focus the dialog, and only an answer the window beneath.
    fn modal_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let window = layout.window();
        let answer = match (&mut self.modal, key) {
            (Some(_), Key::Named(NamedKey::Escape)) => Some(0),
            (Some(Modal::GoTo(..)), Key::Named(NamedKey::Enter)) => Some(1),
            (Some(Modal::GoTo(dialog, field)), _) => {
                let bounds = Self::modal_rect(dialog, window, true, scale, theme);
                if let Some(content) = dialog.content_rect(bounds, scale, theme) {
                    field.on_key(key, modifiers, content, damage);
                }
                None
            }
            (Some(Modal::Close(dialog)), _) => {
                damage.add(Self::modal_rect(dialog, window, false, scale, theme));
                dialog
                    .on_key(key)
                    .map(|DialogAction::ActionActivated { index }| index)
            }
            (None, _) => None,
        };
        match answer {
            Some(index) => self.answer_modal(index, layout, damage),
            None => Outcome::none(),
        }
    }

    fn answer_modal(&mut self, index: usize, layout: &Layout, damage: &mut Region) -> Outcome {
        damage.add(layout.window());
        match self.modal.take() {
            Some(Modal::Close(_)) => match index {
                1 => Outcome::asking(Request::Close),
                2 => Outcome::asking(Request::SaveThenClose),
                _ => Outcome::none(),
            },
            Some(Modal::GoTo(_, field)) if index == 1 => {
                if let Ok(line) = field.text().trim().parse::<usize>() {
                    self.command(&Command::GoToLine(line), layout, damage)
                } else {
                    self.message = Some(String::from("A line number is a whole number"));
                    Outcome::none()
                }
            }
            _ => Outcome::none(),
        }
    }

    fn open_find(&mut self, focus: Focus, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.find_open && self.find.text().is_empty() {
            let selection = self.editor.selection().range();
            if !selection.is_empty() && selection.len() <= FIND_SEED_BYTES {
                if let Ok(bytes) = self.editor.copy() {
                    if let Ok(text) = core::str::from_utf8(&bytes) {
                        if !text.contains('\n') {
                            self.find.set_text(text);
                        }
                    }
                }
            }
        }
        self.focus_find(focus);
        if self.find_open {
            damage.add(layout.find());
            return Outcome::none();
        }
        self.find_open = true;
        Outcome::relaid()
    }

    fn focus_find(&mut self, focus: Focus) {
        self.focus = focus;
        self.find.set_focused(focus == Focus::Find);
        self.replace.set_focused(focus == Focus::Replace);
    }

    /// Give the keyboard focus to the find field, replace field or document
    /// under the pointer, as a primary press there would, so the window
    /// menu's rows act on what was pressed.
    fn focus_pressed(&mut self, layout: &Layout, damage: &mut Region) {
        let under = if self.find_open && layout.find_field().contains(self.pointer) {
            Focus::Find
        } else if self.find_open && layout.replace_field().contains(self.pointer) {
            Focus::Replace
        } else if layout.cell_at(self.pointer).is_some() {
            Focus::Grid
        } else {
            return;
        };
        if under != self.focus {
            self.focus_find(under);
            damage.add(layout.find());
        }
    }

    fn close_find(&mut self) -> Outcome {
        self.find_open = false;
        self.focus_find(Focus::Grid);
        self.search = None;
        Outcome::relaid()
    }

    /// The pattern the find field spells, or why it spells none.
    fn pattern(&self) -> Result<Pattern, PatternError> {
        if self.hex_pattern {
            Pattern::hex(self.find.text())
        } else {
            Pattern::text(self.find.text(), self.options)
        }
    }

    /// The bytes the replace field spells.
    fn replacement(&self) -> Result<Vec<u8>, PatternError> {
        if self.hex_pattern {
            crate::find::parse_hex(self.replace.text())
        } else {
            Ok(self.replace.text().as_bytes().to_vec())
        }
    }

    /// Ask for the next (or, `backward`, the previous) match, or every match
    /// when `replacement` is given.
    fn search(
        &mut self,
        backward: bool,
        replacement: Option<Vec<u8>>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        damage.add(layout.status());
        let pattern = match self.pattern() {
            Ok(pattern) => pattern,
            Err(PatternError::Empty) => {
                self.message = Some(String::from("Type something to find"));
                return Outcome::none();
            }
            Err(PatternError::TooLong) => {
                self.message = Some(String::from("That is too long to search for"));
                return Outcome::none();
            }
            Err(PatternError::NotHex) => {
                self.message = Some(String::from("Hex search takes pairs of hex digits"));
                return Outcome::none();
            }
        };
        let range = self.editor.selection().range();
        let search = match (&replacement, backward) {
            (Some(_), _) => Search::all(pattern),
            (None, true) => Search::previous(pattern, range.start),
            (None, false) => Search::next(pattern, range.end),
        };
        let id = self.next_search;
        self.next_search += 1;
        self.search = Some(id);
        self.message = Some(String::from("Searching\u{2026}"));
        Outcome::asking(Request::Search {
            id,
            search,
            replacement,
        })
    }

    fn replace_one(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let (Ok(pattern), Ok(bytes)) = (self.pattern(), self.replacement()) else {
            self.message = Some(String::from("Nothing to replace with that"));
            return Outcome::none();
        };
        let selection = self.editor.selection().range();
        let mut replaced = Outcome::none();
        if !selection.is_empty() && pattern.is_match(self.editor.document(), selection) {
            let before = self.editor.selection();
            let effect = self.editor.replace_selection(&bytes);
            replaced = self.report(effect, before, layout, damage);
        }
        let mut outcome = self.search(false, None, layout, damage);
        outcome.relayout |= replaced.relayout;
        outcome
    }

    fn replace_all(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if let Ok(bytes) = self.replacement() {
            self.search(false, Some(bytes), layout, damage)
        } else {
            self.message = Some(String::from("Hex replacement takes pairs of hex digits"));
            Outcome::none()
        }
    }

    /// Take in the answer to search `id`, which ran over the document as it
    /// was at `generation`.
    pub fn found(
        &mut self,
        id: u64,
        generation: u64,
        step: Step,
        replacement: Option<&[u8]>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if self.search != Some(id) {
            return Outcome::none();
        }
        self.search = None;
        damage.add(layout.status());
        if generation != self.editor.generation() {
            self.message = Some(String::from(
                "The document changed while searching; search again",
            ));
            return Outcome::none();
        }
        match step {
            Step::Found(range) => {
                self.message = None;
                let before = self.editor.selection();
                let effect = self.editor.select_match(range);
                self.report(effect, before, layout, damage)
            }
            Step::Missing => {
                self.message = Some(String::from("Not found"));
                Outcome::none()
            }
            Step::All { matches, more } => {
                let count = matches.len();
                let before = self.editor.selection();
                let effect = self
                    .editor
                    .replace_all(&matches, replacement.unwrap_or_default());
                let outcome = self.report(effect, before, layout, damage);
                if effect.refused.is_none() {
                    self.message = Some(match (count, more) {
                        (0, _) => String::from("Not found"),
                        (count, true) => {
                            alloc::format!("Replaced {count}; more remain: Replace All again")
                        }
                        (1, false) => String::from("Replaced 1"),
                        (count, false) => alloc::format!("Replaced {count}"),
                    });
                }
                outcome
            }
            Step::Partial => Outcome::none(),
        }
    }

    /// The window gained or lost the keyboard. Only the caret shows which,
    /// so only its row is repainted; losing it ends a drag in progress.
    pub fn focus_changed(&mut self, focused: bool, layout: &Layout, damage: &mut Region) {
        if !focused {
            self.drag = None;
        }
        if let Some(index) = self.caret_row_index(layout) {
            damage.add(layout.row_rect(index));
        }
    }

    /// Which of the shown rows holds the caret, when one does.
    fn caret_row_index(&self, layout: &Layout) -> Option<usize> {
        let rows = layout.rows().max(1);
        let caret = self.editor.selection().head;
        match self.editor.mode() {
            Mode::Text => {
                let row = text::row_of(self.editor.document(), caret);
                self.visible_rows(rows).position(|shown| shown == row)
            }
            Mode::Hex => (caret / hex::BYTES_PER_ROW)
                .checked_sub(self.hex_top)
                .filter(|&index| index < rows),
        }
    }

    /// Feed one pointer event, at monotonic time `now_ns`.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if let InputEvent::ModifiersChanged { modifiers } = event {
            self.modifiers = *modifiers;
            return Outcome::none();
        }
        if self.modal.is_some() {
            return self.modal_pointer(event, layout, scale, theme, damage);
        }
        if matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Secondary
            }
        ) && layout.window().contains(self.pointer)
        {
            self.focus_pressed(layout, damage);
            return Outcome::asking(Request::Menu {
                kind: MenuKind::Window,
                anchor: Rect::new(self.pointer.x, self.pointer.y, 0, 0),
            });
        }
        if self.find_open {
            if let Some(outcome) = self.find_pointer(event, layout, scale, theme, damage) {
                return outcome;
            }
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.vertical
                .on_pointer(event, layout.vertical_bar(), scale, theme, damage)
        {
            return self.scroll_to(Some(offset), None, layout, damage);
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.horizontal
                .on_pointer(event, layout.horizontal_bar(), scale, theme, damage)
        {
            return self.scroll_to(None, Some(offset), layout, damage);
        }
        if let InputEvent::PointerPressed {
            button: PointerButton::Primary,
        } = event
        {
            if let Some(index) = layout
                .status_fields()
                .iter()
                .position(|field| field.contains(self.pointer))
            {
                return match STATUS_MENUS[index] {
                    Some(kind) => Outcome::asking(Request::Menu {
                        kind,
                        anchor: layout.status_fields()[index],
                    }),
                    None => self.act(Action::ToggleOverwrite, layout, damage),
                };
            }
        }
        self.grid_pointer(event, now_ns, layout, scale, damage)
    }

    fn find_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        if pressed && layout.find_field().contains(self.pointer) {
            self.focus_find(Focus::Find);
        } else if pressed && layout.replace_field().contains(self.pointer) {
            self.focus_find(Focus::Replace);
        }
        if self
            .find
            .on_pointer(event, layout.find_field(), scale, theme, damage)
            .is_some()
            | self
                .replace
                .on_pointer(event, layout.replace_field(), scale, theme, damage)
                .is_some()
        {
            return Some(Outcome::none());
        }
        let bounds = *layout.find_buttons();
        let hit = self
            .find_buttons
            .iter_mut()
            .zip(bounds)
            .position(|(button, rect)| button.on_pointer(event, rect, damage).is_some())?;
        Some(self.find_button(hit, layout, damage))
    }

    fn find_button(&mut self, index: usize, layout: &Layout, damage: &mut Region) -> Outcome {
        match index {
            0 => self.options.match_case = !self.options.match_case,
            1 => self.options.whole_word = !self.options.whole_word,
            2 => self.hex_pattern = !self.hex_pattern,
            3 => return self.search(true, None, layout, damage),
            4 => return self.search(false, None, layout, damage),
            5 => return self.replace_one(layout, damage),
            6 => return self.replace_all(layout, damage),
            _ => return self.close_find(),
        }
        let marks = [
            self.options.match_case,
            self.options.whole_word,
            self.hex_pattern,
        ];
        for (button, on) in self.find_buttons.iter_mut().zip(marks) {
            let mut state = button.state();
            state.selection = if on {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            };
            button.set_state(state);
        }
        damage.add(layout.find());
        Outcome::none()
    }

    fn grid_pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                let Some((row, half)) = layout.cell_at(self.pointer) else {
                    self.clicks.reset();
                    return Outcome::none();
                };
                if self.focus != Focus::Grid {
                    self.focus_find(Focus::Grid);
                    damage.add(layout.find());
                }
                let in_gutter = layout.gutter().contains(self.pointer);
                let subject = ((row as u64) << 32) | (half as u64 / 2);
                let run = self.clicks.register(
                    now_ns,
                    subject,
                    PointerButton::Primary,
                    self.double_click,
                    3,
                );
                self.press(row, half, if in_gutter { 3 } else { run }, layout, damage)
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                self.drag = None;
                Outcome::none()
            }
            InputEvent::PointerMoved { .. } if self.drag.is_some() => self.drag_to(layout, damage),
            InputEvent::PointerScrolled { dx, dy }
                if layout.grid().contains(self.pointer)
                    || layout.gutter().contains(self.pointer) =>
            {
                let y = self
                    .vertical
                    .wheel(*dx, *dy, scale, layout.vertical_bar(), damage);
                let x = self
                    .horizontal
                    .wheel(*dx, *dy, scale, layout.horizontal_bar(), damage);
                let offset = |action: Option<ScrollAction>| {
                    action.map(|ScrollAction::ScrollTo { offset }| offset)
                };
                self.scroll_to(offset(y), offset(x), layout, damage)
            }
            _ => Outcome::none(),
        }
    }

    /// The text row `row` rows down the grid, if the document reaches it.
    fn text_row(&self, row: usize) -> Option<text::Row> {
        let document = self.editor.document();
        let index = text::row_index(document, self.top).checked_add(row)?;
        (index < document.row_count()).then(|| text::row_at(document, index))
    }

    /// The unit under a grid row and half-cell of the text view: what a word
    /// or line selection is made from.
    fn unit_under(&self, row: usize, half: usize) -> usize {
        let document = self.editor.document();
        self.text_row(row).map_or(document.len(), |at| {
            text::unit_under(
                document,
                text::row_bounds(document, at),
                half + self.left * 2,
                self.editor.tab_width(),
            )
        })
    }

    /// The document offset a grid row and half-cell stand for.
    fn offset_at(&self, row: usize, half: usize) -> usize {
        let document = self.editor.document();
        match self.editor.mode() {
            Mode::Text => self.text_row(row).map_or(document.len(), |at| {
                text::offset_at(
                    document,
                    text::row_bounds(document, at),
                    half + self.left * 2,
                    self.editor.tab_width(),
                )
            }),
            Mode::Hex => {
                let layout = HexLayout::for_len(document.len());
                layout
                    .hit(self.hex_top + row, half / 2 + self.left, document.len())
                    .map_or(document.len(), |caret| caret.offset)
            }
        }
    }

    fn press(
        &mut self,
        row: usize,
        half: usize,
        run: u8,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let before = self.editor.selection();
        let effect = if self.editor.mode() == Mode::Hex {
            let len = self.editor.document().len();
            let hex_layout = HexLayout::for_len(len);
            let Some(caret) = hex_layout.hit(self.hex_top + row, half / 2 + self.left, len) else {
                return Outcome::none();
            };
            self.drag = Some(Drag {
                grain: Grain::Character,
                anchor: (caret.offset, caret.offset),
            });
            self.editor.click_hex(caret, self.modifiers.shift)
        } else {
            let at = self.offset_at(row, half);
            let under = self.unit_under(row, half);
            let (grain, unit) = match run {
                2 => (
                    Grain::Word,
                    text::word_around(self.editor.document(), under),
                ),
                3 => {
                    let line = self.editor.document().line_of(under);
                    (
                        Grain::Line,
                        (
                            self.editor.document().line_start(line),
                            self.editor.document().line_bounds(line).next,
                        ),
                    )
                }
                _ => (Grain::Character, (at, at)),
            };
            let effect = if grain == Grain::Character {
                self.editor.click(at, self.modifiers.shift)
            } else {
                self.editor.click(unit.0, false);
                self.editor.click(unit.1, true)
            };
            // A shift-click keeps the anchor it extended from, and a drag
            // from it does too.
            let anchor = if grain == Grain::Character {
                (
                    self.editor.selection().anchor,
                    self.editor.selection().anchor,
                )
            } else {
                unit
            };
            self.drag = Some(Drag { grain, anchor });
            effect
        };
        self.report(effect, before, layout, damage)
    }

    fn drag_to(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(drag) = self.drag else {
            return Outcome::none();
        };
        let grid = layout.grid();
        // Past the grid's top or bottom the view scrolls a row toward the
        // pointer each time it moves.
        if self.pointer.y < grid.top() {
            self.nudge(false);
        } else if self.pointer.y >= grid.bottom() {
            self.nudge(true);
        }
        let (row, half) = layout.cell_near(self.pointer);
        let row = row.min(layout.rows().saturating_sub(1));
        let at = self.offset_at(row, half);
        let under = self.unit_under(row, half);
        let document = self.editor.document();
        let unit = match drag.grain {
            Grain::Character => (at, at),
            Grain::Word => text::word_around(document, under),
            Grain::Line => {
                let line = document.line_of(under);
                (document.line_start(line), document.line_bounds(line).next)
            }
        };
        let (anchor, head) = if unit.0 < drag.anchor.0 {
            (drag.anchor.1, unit.0)
        } else {
            (drag.anchor.0, unit.1.max(drag.anchor.1))
        };
        let before = self.editor.selection();
        self.editor.click(anchor, false);
        let effect = self.editor.click(head, true);
        self.report(
            Effect {
                selection: true,
                ..effect
            },
            before,
            layout,
            damage,
        )
    }

    fn nudge(&mut self, down: bool) {
        let document = self.editor.document();
        match self.editor.mode() {
            Mode::Text => {
                let moved = if down {
                    text::next_row(document, self.top)
                } else {
                    text::prev_row(document, self.top)
                };
                if let Some(row) = moved {
                    self.top = row;
                }
            }
            Mode::Hex if down => {
                self.hex_top = (self.hex_top + 1).min(hex::rows(document.len()).saturating_sub(1));
            }
            Mode::Hex => self.hex_top = self.hex_top.saturating_sub(1),
        }
    }

    /// Scroll to where a bar asked, `down` and `across` pixels in: the cells
    /// they reach, and the pixels short of the next kept for the next ask.
    fn scroll_to(
        &mut self,
        down: Option<u64>,
        across: Option<u64>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let (cell_w, cell_h) = layout.cell();
        let (cell_w, cell_h) = (u64::from(cell_w.max(1)), u64::from(cell_h.max(1)));
        let before = (self.top, self.hex_top, self.left);
        let cells = |pixels: u64, cell: u64| usize::try_from(pixels / cell).unwrap_or(usize::MAX);
        if let Some(pixels) = down {
            let document = self.editor.document();
            let index = cells(pixels, cell_h);
            match self.editor.mode() {
                Mode::Text => self.top = text::row_at(document, index),
                Mode::Hex => {
                    self.hex_top = index.min(hex::rows(document.len()).saturating_sub(1));
                }
            }
            self.down = Carry {
                at: (self.top, self.hex_top),
                pixels: pixels % cell_h,
            };
        }
        if let Some(pixels) = across {
            self.left = cells(pixels, cell_w);
            self.across = Carry {
                at: self.left,
                pixels: pixels % cell_w,
            };
        }
        if before != (self.top, self.hex_top, self.left) {
            damage.add(layout.gutter());
            damage.add(layout.grid());
        }
        self.sync_bars(layout, damage);
        Outcome::none()
    }

    /// Feed one key press.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        self.modifiers = modifiers;
        if self.modal.is_some() {
            return self.modal_key(key, modifiers, layout, scale, theme, damage);
        }
        if let Some(action) = shortcut(key, modifiers, self.editor.mode()) {
            return self.run(action, layout, damage);
        }
        match self.focus {
            Focus::Grid => self.grid_key(key, modifiers, layout, damage),
            Focus::Find | Focus::Replace => self.find_key(key, modifiers, layout, damage),
        }
    }

    /// Carry out `action` on what holds the keyboard focus, as its shortcut
    /// would, whether pressed or chosen from a menu.
    fn run(&mut self, action: Action, layout: &Layout, damage: &mut Region) -> Outcome {
        match self.field_action(action, layout, damage) {
            Some(outcome) => outcome,
            None => self.act(action, layout, damage),
        }
    }

    /// `action` on a focused find field: the clipboard and select-all act on
    /// the field, and undo and redo do nothing, since the field keeps no
    /// history; `None` for one that means the same anywhere.
    fn field_action(
        &mut self,
        action: Action,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        if self.focus == Focus::Grid {
            return None;
        }
        let (find, replace) = (layout.find_field(), layout.replace_field());
        Some(match action {
            Action::Copy => self
                .field_copy()
                .map_or_else(Outcome::none, Outcome::asking),
            Action::Cut => {
                let copied = self.field_copy();
                if self.focus == Focus::Find {
                    self.find.delete_selection(find, damage);
                } else {
                    self.replace.delete_selection(replace, damage);
                }
                copied.map_or_else(Outcome::none, Outcome::asking)
            }
            Action::SelectAll => {
                if self.focus == Focus::Find {
                    self.find.select_all(find, damage);
                } else {
                    self.replace.select_all(replace, damage);
                }
                Outcome::none()
            }
            Action::Undo | Action::Redo => Outcome::none(),
            _ => return None,
        })
    }

    /// The focused find field's selected text.
    fn field_selection(&self) -> Option<&str> {
        match self.focus {
            Focus::Find => self.find.selected_text(),
            Focus::Replace => self.replace.selected_text(),
            Focus::Grid => None,
        }
    }

    /// A copy of the focused find field's selection.
    fn field_copy(&self) -> Option<Request> {
        self.field_selection()
            .map(|text| Request::Copy(text.as_bytes().to_vec()))
    }

    fn find_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        match key {
            Key::Named(NamedKey::Enter) if self.focus == Focus::Replace => {
                self.replace_one(layout, damage)
            }
            Key::Named(NamedKey::Enter) => self.search(modifiers.shift, None, layout, damage),
            Key::Named(NamedKey::Escape) => self.close_find(),
            Key::Named(NamedKey::Tab) => {
                self.focus_find(if self.focus == Focus::Find {
                    Focus::Replace
                } else {
                    Focus::Find
                });
                damage.add(layout.find());
                Outcome::none()
            }
            _ => {
                let acted = if self.focus == Focus::Find {
                    self.find
                        .on_key(key, modifiers, layout.find_field(), damage)
                } else {
                    self.replace
                        .on_key(key, modifiers, layout.replace_field(), damage)
                };
                if acted == Some(TextAction::Cancelled) {
                    return self.close_find();
                }
                Outcome::none()
            }
        }
    }

    fn grid_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let extend = modifiers.shift;
        let word = modifiers.ctrl;
        let motion = |motion| Command::Move { motion, extend };
        let command = match key {
            Key::Named(NamedKey::Left) => {
                motion(if word { Motion::WordLeft } else { Motion::Left })
            }
            Key::Named(NamedKey::Right) => motion(if word {
                Motion::WordRight
            } else {
                Motion::Right
            }),
            Key::Named(NamedKey::Up) => motion(Motion::Up),
            Key::Named(NamedKey::Down) => motion(Motion::Down),
            Key::Named(NamedKey::Home) => motion(if word {
                Motion::DocumentStart
            } else {
                Motion::LineStart
            }),
            Key::Named(NamedKey::End) => motion(if word {
                Motion::DocumentEnd
            } else {
                Motion::LineEnd
            }),
            Key::Named(NamedKey::PageUp) => motion(Motion::PageUp),
            Key::Named(NamedKey::PageDown) => motion(Motion::PageDown),
            Key::Named(NamedKey::Backspace) if word => Command::DeleteWordLeft,
            Key::Named(NamedKey::Backspace) => Command::Backspace,
            Key::Named(NamedKey::Delete) if word => Command::DeleteWordRight,
            Key::Named(NamedKey::Delete) => Command::Delete,
            Key::Named(NamedKey::Enter) => Command::Newline,
            Key::Named(NamedKey::Tab) if self.editor.mode() == Mode::Hex => {
                let pane = match self.editor.hex_caret().pane {
                    hex::Pane::Hex => hex::Pane::Ascii,
                    hex::Pane::Ascii => hex::Pane::Hex,
                };
                let before = self.editor.selection();
                let effect = self.editor.set_pane(pane);
                return self.report(effect, before, layout, damage);
            }
            Key::Named(NamedKey::Tab) if extend => Command::Backtab,
            Key::Named(NamedKey::Tab) => Command::Tab,
            Key::Named(NamedKey::Insert) => Command::ToggleOverwrite,
            Key::Named(NamedKey::Escape) if self.find_open => return self.close_find(),
            Key::Char(ch) if !modifiers.ctrl && !modifiers.alt && !modifiers.meta => {
                Command::Type(ch)
            }
            _ => return Outcome::none(),
        };
        self.command(&command, layout, damage)
    }

    /// A menu row was chosen.
    pub fn chosen(&mut self, id: AppMenuItemId, layout: &Layout, damage: &mut Region) -> Outcome {
        match Action::from_id(id.get()) {
            Some(action) => self.run(action, layout, damage),
            None => Outcome::none(),
        }
    }

    /// The rows `kind` opens as.
    #[must_use]
    pub fn menu(&self, kind: MenuKind) -> AppMenu {
        let mut menu = match kind {
            MenuKind::Window => Menu::titled(APP_TITLE),
            _ => Menu::default(),
        };
        match kind {
            MenuKind::Window => self.window_rows(&mut menu),
            MenuKind::Format => self.formats(&mut menu, Plate::Root),
            MenuKind::Mode => self.modes(&mut menu, Plate::Root),
            MenuKind::LineEnding => self.endings(&mut menu, Plate::Root),
            MenuKind::Indent => {
                self.indents(&mut menu, Plate::Root);
                menu.separator(Plate::Root);
                self.tab_widths(&mut menu, Plate::Root);
            }
        }
        menu.menu
    }

    /// The window's menu: the clipboard, which a secondary press most often
    /// wants, on the plate it opens with, then each of the window's menus as
    /// a submenu.
    fn window_rows(&self, menu: &mut Menu) {
        self.clipboard_rows(menu, Plate::Root);
        menu.separator(Plate::Root);
        if let Some(file) = menu.submenu("File", Plate::Root) {
            file_rows(menu, file);
        }
        if let Some(edit) = menu.submenu("Edit", Plate::Root) {
            self.edit_rows(menu, edit);
        }
        if let Some(find) = menu.submenu("Find", Plate::Root) {
            self.find_rows(menu, find);
        }
        if let Some(view) = menu.submenu("View", Plate::Root) {
            self.view_rows(menu, view);
        }
    }

    /// Whether the selected lines can be commented out: text, in a format
    /// with a line comment.
    fn can_comment(&self) -> bool {
        self.editor.mode() == Mode::Text && self.editor.format().line_comment().is_some()
    }

    fn clipboard_rows(&self, menu: &mut Menu, plate: Plate) {
        let selected = match self.focus {
            Focus::Grid => !self.editor.selection().is_empty(),
            Focus::Find | Focus::Replace => self.field_selection().is_some(),
        };
        menu.item(Action::Cut, "Cut", "Ctrl+X", selected, plate);
        menu.item(Action::Copy, "Copy", "Ctrl+C", selected, plate);
        menu.item(Action::Paste, "Paste", "Ctrl+V", true, plate);
        menu.item(Action::SelectAll, "Select all", "Ctrl+A", true, plate);
    }

    fn edit_rows(&self, menu: &mut Menu, plate: Plate) {
        let text = self.editor.mode() == Mode::Text;
        let history = self.focus == Focus::Grid;
        let (undo, redo) = self.editor.can_undo_redo();
        menu.item(Action::Undo, "Undo", "Ctrl+Z", history && undo, plate);
        menu.item(Action::Redo, "Redo", "Ctrl+Shift+Z", history && redo, plate);
        menu.separator(plate);
        menu.item(Action::Indent, "Indent", "Ctrl+]", text, plate);
        menu.item(Action::Outdent, "Outdent", "Ctrl+[", text, plate);
        let comment = self.can_comment();
        menu.item(
            Action::ToggleComment,
            "Toggle comment",
            "Ctrl+/",
            comment,
            plate,
        );
        menu.separator(plate);
        let overwrite = self.editor.overwrite();
        menu.mark(
            Action::ToggleOverwrite,
            "Overwrite",
            "Insert",
            overwrite,
            plate,
        );
    }

    fn find_rows(&self, menu: &mut Menu, plate: Plate) {
        let text = self.editor.mode() == Mode::Text;
        let problems = !self.editor.diagnostics().0.is_empty();
        menu.item(Action::Find, "Find\u{2026}", "Ctrl+F", true, plate);
        menu.item(Action::Replace, "Replace\u{2026}", "Ctrl+H", true, plate);
        menu.item(Action::FindNext, "Find next", "F3", true, plate);
        menu.item(
            Action::FindPrevious,
            "Find previous",
            "Shift+F3",
            true,
            plate,
        );
        menu.separator(plate);
        menu.item(
            Action::GoToLine,
            "Go to line\u{2026}",
            "Ctrl+L",
            text,
            plate,
        );
        menu.item(Action::NextProblem, "Next problem", "F8", problems, plate);
    }

    fn view_rows(&self, menu: &mut Menu, plate: Plate) {
        self.modes(menu, plate);
        menu.separator(plate);
        if let Some(formats) = menu.submenu("Format", plate) {
            self.formats(menu, formats);
        }
        if let Some(tabs) = menu.submenu("Tab width", plate) {
            self.tab_widths(menu, tabs);
        }
        if let Some(indents) = menu.submenu("Indentation", plate) {
            self.indents(menu, indents);
        }
        if let Some(endings) = menu.submenu("Line endings", plate) {
            self.endings(menu, endings);
        }
    }

    fn modes(&self, menu: &mut Menu, plate: Plate) {
        menu.radio(
            Action::Mode(Mode::Text),
            "Text",
            "",
            self.editor.mode() == Mode::Text,
            plate,
        );
        menu.radio(
            Action::Mode(Mode::Hex),
            "Hex",
            "Ctrl+Shift+H",
            self.editor.mode() == Mode::Hex,
            plate,
        );
    }

    fn formats(&self, menu: &mut Menu, plate: Plate) {
        for format in Format::ALL {
            menu.radio(
                Action::Format(format),
                format.label(),
                "",
                self.editor.format() == format,
                plate,
            );
        }
    }

    fn tab_widths(&self, menu: &mut Menu, plate: Plate) {
        for (width, label) in TAB_WIDTHS.into_iter().zip(TAB_LABELS) {
            menu.radio(
                Action::TabWidth(width),
                label,
                "",
                self.editor.tab_width() == usize::from(width),
                plate,
            );
        }
    }

    fn indents(&self, menu: &mut Menu, plate: Plate) {
        for (indent, label) in INDENTS.into_iter().zip(INDENT_LABELS) {
            menu.radio(
                Action::Indentation(indent),
                label,
                "",
                self.editor.indent() == indent,
                plate,
            );
        }
    }

    fn endings(&self, menu: &mut Menu, plate: Plate) {
        for ending in ENDINGS {
            menu.radio(
                Action::LineEnding(ending),
                ending.label(),
                "",
                self.editor.line_ending() == ending,
                plate,
            );
        }
    }
}

const TAB_LABELS: [&str; 3] = [
    "Tab stops every 2",
    "Tab stops every 4",
    "Tab stops every 8",
];
const INDENT_LABELS: [&str; 4] = [
    "Indent with tabs",
    "Indent with 2 spaces",
    "Indent with 4 spaces",
    "Indent with 8 spaces",
];

/// The File menu's rows, the same whatever the document.
fn file_rows(menu: &mut Menu, plate: Plate) {
    menu.item(Action::NewWindow, "New window", "Ctrl+N", true, plate);
    menu.item(Action::Open, "Open\u{2026}", "Ctrl+O", true, plate);
    menu.separator(plate);
    menu.item(Action::Save, "Save", "Ctrl+S", true, plate);
    menu.item(
        Action::SaveAs,
        "Save as\u{2026}",
        "Ctrl+Shift+S",
        true,
        plate,
    );
    menu.separator(plate);
    menu.item(Action::Close, "Close", "Ctrl+W", true, plate);
}

/// Which plate of a menu a row is laid on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Plate {
    /// The plate the menu opens with.
    Root,
    /// The submenu the row at this index opens.
    Under(usize),
}

/// A menu being built; a row the menu cannot hold is left out rather than
/// the whole menu, since a menu is incidental to editing.
struct Menu {
    menu: AppMenu,
}

impl Default for Menu {
    fn default() -> Self {
        Self {
            menu: AppMenu::EMPTY,
        }
    }
}

impl Menu {
    /// An empty menu whose plate is titled `title`, or an untitled one where
    /// the title cannot be carried.
    fn titled(title: &str) -> Self {
        AppMenuLabel::new(title).map_or_else(
            |_| Self::default(),
            |title| Self {
                menu: AppMenu::titled(title),
            },
        )
    }

    fn push(&mut self, row: &AppMenuRow, plate: Plate) -> Result<(), tairix_abi::Errno> {
        match plate {
            Plate::Root => self.menu.push(*row),
            Plate::Under(parent) => self.menu.push_under(*row, parent),
        }
    }

    fn build(
        &mut self,
        action: Action,
        label: &str,
        shortcut: &str,
        enabled: bool,
        mark: AppMenuMark,
        plate: Plate,
    ) {
        let (Ok(id), Ok(label)) = (AppMenuItemId::new(action.id()), AppMenuLabel::new(label))
        else {
            return;
        };
        let mut item = AppMenuItem::new(id, label).with_mark(mark);
        if !shortcut.is_empty() {
            if let Ok(caption) = AppMenuShortcut::new(shortcut) {
                item = item.with_shortcut(caption);
            }
        }
        if !enabled {
            item = item.disabled();
        }
        let _ = self.push(&AppMenuRow::Item(item), plate);
    }

    fn item(&mut self, action: Action, label: &str, shortcut: &str, enabled: bool, plate: Plate) {
        self.build(action, label, shortcut, enabled, AppMenuMark::None, plate);
    }

    fn mark(&mut self, action: Action, label: &str, shortcut: &str, on: bool, plate: Plate) {
        let mark = if on {
            AppMenuMark::Check
        } else {
            AppMenuMark::None
        };
        self.build(action, label, shortcut, true, mark, plate);
    }

    fn radio(&mut self, action: Action, label: &str, shortcut: &str, on: bool, plate: Plate) {
        let mark = if on {
            AppMenuMark::Radio
        } else {
            AppMenuMark::None
        };
        self.build(action, label, shortcut, true, mark, plate);
    }

    fn separator(&mut self, plate: Plate) {
        let _ = self.push(&AppMenuRow::Separator, plate);
    }

    /// Open a submenu labelled `label` on `plate`, answering the plate its
    /// rows go on, or `None` when it could not be added — so its rows are
    /// left out with it rather than landing on another plate.
    fn submenu(&mut self, label: &str, plate: Plate) -> Option<Plate> {
        let index = self.menu.len();
        let label = AppMenuLabel::new(label).ok()?;
        self.push(
            &AppMenuRow::Submenu {
                label,
                enabled: true,
            },
            plate,
        )
        .ok()?;
        Some(Plate::Under(index))
    }
}

/// The action a key chord asks for, whatever has the keyboard.
fn shortcut(key: Key, modifiers: Modifiers, mode: Mode) -> Option<Action> {
    let ctrl = modifiers.ctrl && !modifiers.alt && !modifiers.meta;
    match key {
        Key::Named(NamedKey::Function { number: 3 }) if modifiers.shift => {
            Some(Action::FindPrevious)
        }
        Key::Named(NamedKey::Function { number: 3 }) => Some(Action::FindNext),
        Key::Named(NamedKey::Function { number: 8 }) => Some(Action::NextProblem),
        Key::Char(ch) if ctrl => match (ch.to_ascii_lowercase(), modifiers.shift) {
            ('n', _) => Some(Action::NewWindow),
            ('o', _) => Some(Action::Open),
            ('s', true) => Some(Action::SaveAs),
            ('s', false) => Some(Action::Save),
            ('w', _) => Some(Action::Close),
            ('z', true) | ('y', _) => Some(Action::Redo),
            ('z', false) => Some(Action::Undo),
            ('x', _) => Some(Action::Cut),
            ('c', _) => Some(Action::Copy),
            ('v', _) => Some(Action::Paste),
            ('a', _) => Some(Action::SelectAll),
            ('f', _) => Some(Action::Find),
            ('h', true) => Some(Action::Mode(if mode == Mode::Hex {
                Mode::Text
            } else {
                Mode::Hex
            })),
            ('h', false) => Some(Action::Replace),
            ('l', _) => Some(Action::GoToLine),
            (']', _) => Some(Action::Indent),
            ('[', _) => Some(Action::Outdent),
            ('/', _) => Some(Action::ToggleComment),
            _ => None,
        },
        _ => None,
    }
}

/// How many decimal digits `n` takes.
fn digits(n: usize) -> u32 {
    n.checked_ilog10().map_or(1, |log| log + 1)
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
