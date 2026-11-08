//! The commands a Draw window carries out, the chords that carry them out,
//! and the menus that offer them.
//!
//! A command's row — its picture, its label, its chord and when it may be
//! chosen — is stated once, by `Command::spec`: the menu draws its rows from
//! it and the keyboard finds its commands in it, so a row can never show a
//! chord the keyboard does not honour.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::window_ipc::{AppMenu, AppMenuSymbol as S};
use tairix_bezier::boolean::Operation;
use tairix_bezier::{Affine, Point};
use tairix_drawing::Node;
use tairix_geometry::Region;
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_window::docapp::{Outcome, Request};
use tairix_window::document::SavedDocument;
use tairix_window::menu::{MenuBuilder, Plate};

use crate::compute::{Compute, Reshape};
use crate::edit::{self, Arrange};
use crate::glyphs::{Arrived, Lettering};
use crate::input::ViewOutcome;
use crate::inspector::{LayerCommand, Tab};
use crate::layout::Layout;
use crate::tools::Tool;
use crate::view::{MenuKind, Own, View};
use crate::viewport::STEPS;

/// Where the menu's tool rows are numbered from.
const TOOL_IDS: u16 = 100;

/// Where the zoom menu's rows are numbered from.
const ZOOM_IDS: u16 = 200;

/// Where the paper sizes' rows are numbered from.
const PAPER_IDS: u16 = 300;

/// Where the units' rows are numbered from.
const UNIT_IDS: u16 = 340;

/// A command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum Command {
    /// Open another window.
    NewWindow = 1,
    /// Open a drawing.
    Open,
    /// Save.
    Save,
    /// Save under another name.
    SaveAs,
    /// Write a copy as PDF, EPS, PNG, JPEG, TIFF or SVG.
    SaveCopy,
    /// Close the window.
    Close,
    /// Take back the last step.
    Undo,
    /// Do the step taken back again.
    Redo,
    /// Copy the selection and remove it.
    Cut,
    /// Copy the selection.
    Copy,
    /// Paste the clipboard's drawing.
    Paste,
    /// Copy the selection beside itself.
    Duplicate,
    /// Remove the selection.
    Delete,
    /// Select everything in reach.
    SelectAll,
    /// Select nothing.
    SelectNone,
    /// Gather the selection into a group.
    Group,
    /// Release a group's members.
    Ungroup,
    /// Up past the next object.
    Raise,
    /// Down past the next object.
    Lower,
    /// To the top.
    ToFront,
    /// To the bottom.
    ToBack,
    /// Mirror across.
    FlipAcross,
    /// Mirror down.
    FlipDown,
    /// Turn a quarter anticlockwise.
    TurnLeft,
    /// Turn a quarter clockwise.
    TurnRight,
    /// Lock the selection.
    Lock,
    /// Unlock everything in the layer in hand.
    UnlockAll,
    /// Hide the selection.
    Hide,
    /// Show everything in the layer in hand.
    ShowAll,
    /// Union.
    Union,
    /// The lowest less the others.
    Difference,
    /// Intersection.
    Intersection,
    /// Exclusion.
    Exclusion,
    /// Turn shapes and text into paths.
    ToPaths,
    /// Turn strokes into outlines.
    OutlineStrokes,
    /// A new layer.
    NewLayer,
    /// Remove the layer in hand.
    RemoveLayer,
    /// Move the layer in hand up.
    RaiseLayer,
    /// Move the layer in hand down.
    LowerLayer,
    /// Zoom in a step.
    ZoomIn,
    /// Zoom out a step.
    ZoomOut,
    /// Zoom to 100%.
    ActualSize,
    /// Fit the page in the window.
    FitPage,
    /// Fit the selection in the window.
    FitSelection,
    /// Show outlines rather than paint.
    Outline,
    /// Show the grid.
    Grid,
    /// Snap to guides, the page and the grid.
    Snap,
    /// Turn the page between upright and landscape.
    Landscape,
    /// Give the page the ground the Page tab holds, or none.
    Ground,
    /// Show the selection only within its uppermost object.
    Clip,
    /// Show a clipped group whole again.
    ReleaseClip,
    /// Cut the selection into the pieces its overlaps make.
    Divide,
    /// Grow the selection outward by the offset distance.
    Outset,
    /// Shrink the selection inward by the offset distance.
    Inset,
    /// Trace the selection again with fewer nodes.
    Simplify,
    /// Run the selected paths the other way.
    Reverse,
    /// Show the page's settings in the inspector.
    PageSetup,
    /// Choose a picture to place in the layer in hand.
    PlacePicture,
    /// Set the selected text along the selected shape's outline.
    TextOnPath,
    /// Set texts along paths in lines again, each path given back.
    ReleaseFromPath,
}

impl Command {
    /// Every command, in the order of its id: the one numbered `n` is
    /// `ALL[n - 1]`.
    const ALL: [Self; 60] = [
        Self::NewWindow,
        Self::Open,
        Self::Save,
        Self::SaveAs,
        Self::SaveCopy,
        Self::Close,
        Self::Undo,
        Self::Redo,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::Duplicate,
        Self::Delete,
        Self::SelectAll,
        Self::SelectNone,
        Self::Group,
        Self::Ungroup,
        Self::Raise,
        Self::Lower,
        Self::ToFront,
        Self::ToBack,
        Self::FlipAcross,
        Self::FlipDown,
        Self::TurnLeft,
        Self::TurnRight,
        Self::Lock,
        Self::UnlockAll,
        Self::Hide,
        Self::ShowAll,
        Self::Union,
        Self::Difference,
        Self::Intersection,
        Self::Exclusion,
        Self::ToPaths,
        Self::OutlineStrokes,
        Self::NewLayer,
        Self::RemoveLayer,
        Self::RaiseLayer,
        Self::LowerLayer,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ActualSize,
        Self::FitPage,
        Self::FitSelection,
        Self::Outline,
        Self::Grid,
        Self::Snap,
        Self::Landscape,
        Self::Ground,
        Self::Clip,
        Self::ReleaseClip,
        Self::Divide,
        Self::Outset,
        Self::Inset,
        Self::Simplify,
        Self::Reverse,
        Self::PageSetup,
        Self::PlacePicture,
        Self::TextOnPath,
        Self::ReleaseFromPath,
    ];

    /// Whether whatever is under way lands before it is carried out: every
    /// command that works on the drawing or writes it does, those that only
    /// look at it, or wait on the host, do not. Undo and Redo land it
    /// themselves, to tell whether it was what they take back.
    const fn lands_pending_first(self) -> bool {
        !matches!(
            self,
            Self::NewWindow
                | Self::Open
                | Self::Undo
                | Self::Redo
                | Self::Paste
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::ActualSize
                | Self::FitPage
                | Self::FitSelection
                | Self::Outline
                | Self::Grid
                | Self::Snap
        )
    }

    /// The command a menu row's id stands for.
    #[must_use]
    pub fn from_id(id: u16) -> Option<Self> {
        Self::ALL.get(usize::from(id.checked_sub(1)?)).copied()
    }

    /// The command `key`, pressed with `modifiers`, carries out.
    #[must_use]
    pub fn for_chord(key: Key, modifiers: Modifiers) -> Option<Self> {
        let pressed = |chord: Chord| chord.matches(key, modifiers);
        Self::ALL
            .into_iter()
            .find(|command| command.spec().chord.is_some_and(pressed))
            .or_else(|| {
                ALTERNATES
                    .iter()
                    .find(|(chord, _)| pressed(*chord))
                    .map(|&(_, command)| command)
            })
    }

    /// The row this command is offered on.
    fn spec(self) -> Spec {
        use When::{Always, Layers, Redoable, Selected, Several, Undoable};
        let row = Spec::new;
        match self {
            Self::NewWindow => row(S::NewWindow, "New window", Always).on(Chord::ctrl('n')),
            Self::Open => row(S::Open, "Open\u{2026}", Always).on(Chord::ctrl('o')),
            Self::Save => row(S::Save, "Save", Always).on(Chord::ctrl('s')),
            Self::SaveAs => row(S::SaveAs, "Save as\u{2026}", Always).on(Chord::ctrl_shift('s')),
            Self::SaveCopy => {
                row(S::SaveAs, "Save a copy\u{2026}", Always).on(Chord::ctrl_shift('e'))
            }
            Self::Close => row(S::Close, "Close", Always).on(Chord::ctrl('w')),
            Self::Undo => row(S::Undo, "Undo", Undoable).on(Chord::ctrl('z')),
            Self::Redo => row(S::Redo, "Redo", Redoable).on(Chord::ctrl_shift('z')),
            Self::Cut => row(S::Cut, "Cut", Selected).on(Chord::ctrl('x')),
            Self::Copy => row(S::Copy, "Copy", Selected).on(Chord::ctrl('c')),
            Self::Paste => row(S::Paste, "Paste", Always).on(Chord::ctrl('v')),
            Self::Duplicate => row(S::Duplicate, "Duplicate", Selected).on(Chord::ctrl('d')),
            Self::Delete => row(S::Delete, "Delete", Selected)
                .on(Chord::key(NamedKey::Delete))
                .destructive(),
            Self::SelectAll => row(S::SelectAll, "Select all", Always).on(Chord::ctrl('a')),
            Self::SelectNone => {
                row(S::SelectNone, "Select none", Selected).on(Chord::ctrl_shift('a'))
            }
            Self::Group => row(S::Group, "Group", Several).on(Chord::ctrl('g')),
            Self::Ungroup => row(S::Ungroup, "Ungroup", Selected).on(Chord::ctrl_shift('g')),
            Self::Raise => row(S::MoveUp, "Raise", Selected).on(Chord::key(NamedKey::PageUp)),
            Self::Lower => row(S::MoveDown, "Lower", Selected).on(Chord::key(NamedKey::PageDown)),
            Self::ToFront => {
                row(S::BringToFront, "Bring to front", Selected).on(Chord::key(NamedKey::Home))
            }
            Self::ToBack => {
                row(S::SendToBack, "Send to back", Selected).on(Chord::key(NamedKey::End))
            }
            Self::FlipAcross => row(S::FlipHorizontal, "Flip across", Selected),
            Self::FlipDown => row(S::FlipVertical, "Flip down", Selected),
            Self::TurnLeft => row(S::RotateLeft, "Turn left", Selected).on(Chord::ctrl('[')),
            Self::TurnRight => row(S::RotateRight, "Turn right", Selected).on(Chord::ctrl(']')),
            Self::Lock => row(S::Lock, "Lock", Selected).on(Chord::ctrl('l')),
            Self::UnlockAll => {
                row(S::Unlock, "Unlock all in layer", Always).on(Chord::ctrl_shift('l'))
            }
            Self::Hide => row(S::Hidden, "Hide", Selected).on(Chord::ctrl('h')),
            Self::ShowAll => {
                row(S::Visible, "Show all in layer", Always).on(Chord::ctrl_shift('h'))
            }
            Self::Union => row(S::Union, "Union", Several),
            Self::Difference => row(S::Difference, "Difference", Several),
            Self::Intersection => row(S::Intersection, "Intersection", Several),
            Self::Exclusion => row(S::Exclusion, "Exclusion", Several),
            Self::ToPaths => {
                row(S::ObjectToPath, "Object to path", Selected).on(Chord::ctrl_shift('c'))
            }
            Self::OutlineStrokes => row(S::StrokeToPath, "Stroke to path", Selected),
            Self::TextOnPath => row(S::ToolText, "Set text on path", Several),
            Self::ReleaseFromPath => row(S::ReleaseClip, "Release from path", Selected),
            Self::NewLayer => row(S::LayerNew, "New layer", Always).on(Chord::ctrl_shift('n')),
            Self::RemoveLayer => row(S::Remove, "Remove layer", Layers).destructive(),
            Self::RaiseLayer => row(S::LayerRaise, "Raise layer", Always)
                .on(Chord::ctrl_shift_key(NamedKey::PageUp)),
            Self::LowerLayer => row(S::LayerLower, "Lower layer", Always)
                .on(Chord::ctrl_shift_key(NamedKey::PageDown)),
            Self::ZoomIn => row(S::ZoomIn, "Zoom in", Always).on(Chord::ctrl('=')),
            Self::ZoomOut => row(S::ZoomOut, "Zoom out", Always).on(Chord::ctrl('-')),
            Self::ActualSize => row(S::ZoomActual, "Actual size", Always).on(Chord::ctrl('1')),
            Self::FitPage => row(S::ZoomFit, "Fit page", Always).on(Chord::ctrl('0')),
            Self::FitSelection => row(S::ZoomSelection, "Fit selection", Selected),
            Self::Outline => row(S::ViewOutline, "Outlines", Always).mark(|view| view.show.outline),
            Self::Grid => row(S::Grid, "Grid", Always)
                .on(Chord::ctrl('\''))
                .mark(|view| view.show.grid),
            Self::Snap => row(S::Snap, "Snap", Always)
                .on(Chord::bare('%'))
                .mark(|view| view.show.snapping),
            Self::Landscape => row(S::RotateRight, "Landscape", Always).mark(|view| {
                let page = view.document().drawing().page();
                page.width > page.height
            }),
            Self::Ground => row(S::PageGround, "Ground", Always)
                .mark(|view| view.document().drawing().page().background.is_some()),
            Self::Clip => row(S::Clip, "Clip to the top object", Several).on(Chord::ctrl('7')),
            Self::ReleaseClip => row(S::ReleaseClip, "Release clip", Selected),
            Self::Divide => row(S::Divide, "Divide", Several),
            Self::Outset => row(S::Outset, "Outset", Selected),
            Self::Inset => row(S::Inset, "Inset", Selected),
            Self::Simplify => row(S::Simplify, "Simplify", Selected),
            Self::Reverse => row(S::ReverseDirection, "Reverse direction", Selected),
            Self::PageSetup => row(S::Properties, "Page setup", Always),
            Self::PlacePicture => {
                row(S::PlacePicture, "Place picture\u{2026}", Always).on(Chord::ctrl_shift('p'))
            }
        }
    }
}

impl From<Command> for u16 {
    fn from(command: Command) -> Self {
        command as Self
    }
}

/// Chords that carry out a command beside the one its row shows.
const ALTERNATES: [(Chord, Command); 3] = [
    (Chord::ctrl('y'), Command::Redo),
    (Chord::ctrl('+'), Command::ZoomIn),
    (Chord::key(NamedKey::Backspace), Command::Delete),
];

/// A key and the modifiers held with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Chord {
    ctrl: bool,
    shift: bool,
    key: Key,
}

impl Chord {
    const fn bare(key: char) -> Self {
        Self {
            ctrl: false,
            shift: false,
            key: Key::Char(key),
        }
    }

    const fn ctrl(key: char) -> Self {
        Self {
            ctrl: true,
            shift: false,
            key: Key::Char(key),
        }
    }

    const fn ctrl_shift(key: char) -> Self {
        Self {
            ctrl: true,
            shift: true,
            key: Key::Char(key),
        }
    }

    const fn key(key: NamedKey) -> Self {
        Self {
            ctrl: false,
            shift: false,
            key: Key::Named(key),
        }
    }

    const fn ctrl_shift_key(key: NamedKey) -> Self {
        Self {
            ctrl: true,
            shift: true,
            key: Key::Named(key),
        }
    }

    /// Whether `key` pressed with `modifiers` is this chord. Shift is part
    /// of the character typed for anything but a letter, so only a letter's
    /// chord, or a named key's, minds it.
    fn matches(self, key: Key, modifiers: Modifiers) -> bool {
        if modifiers.alt || modifiers.meta || modifiers.ctrl != self.ctrl {
            return false;
        }
        match (self.key, key) {
            (Key::Char(wanted), Key::Char(typed)) => {
                typed.to_ascii_lowercase() == wanted
                    && (!wanted.is_ascii_alphabetic() || modifiers.shift == self.shift)
            }
            (Key::Named(wanted), Key::Named(pressed)) => {
                pressed == wanted && modifiers.shift == self.shift
            }
            _ => false,
        }
    }

    /// The chord as a menu row shows it, written to `out`.
    fn caption(self, out: &mut String) {
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        let name = match self.key {
            Key::Char(character) => {
                out.push(character.to_ascii_uppercase());
                return;
            }
            Key::Named(NamedKey::Function { number }) => {
                // Writing to a string cannot fail.
                let _ = write!(out, "F{number}");
                return;
            }
            Key::Named(NamedKey::Enter) => "Enter",
            Key::Named(NamedKey::Escape) => "Esc",
            Key::Named(NamedKey::Backspace) => "Backspace",
            Key::Named(NamedKey::Tab) => "Tab",
            Key::Named(NamedKey::Delete) => "Delete",
            Key::Named(NamedKey::Insert) => "Insert",
            Key::Named(NamedKey::Home) => "Home",
            Key::Named(NamedKey::End) => "End",
            Key::Named(NamedKey::PageUp) => "Page Up",
            Key::Named(NamedKey::PageDown) => "Page Down",
            Key::Named(NamedKey::Left) => "Left",
            Key::Named(NamedKey::Right) => "Right",
            Key::Named(NamedKey::Up) => "Up",
            Key::Named(NamedKey::Down) => "Down",
        };
        out.push_str(name);
    }
}

/// When a command's row may be chosen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum When {
    /// Whatever the window holds.
    Always,
    /// While something is selected.
    Selected,
    /// While more than one thing is selected.
    Several,
    /// While there is a step to take back.
    Undoable,
    /// While there is a step to do again.
    Redoable,
    /// While the drawing has a layer to spare.
    Layers,
}

/// How a command's row is drawn.
#[derive(Copy, Clone)]
enum Look {
    /// Plainly.
    Plain,
    /// With the destructive emphasis.
    Destructive,
    /// As a setting, ticked while the window answers that it is on.
    Mark(fn(&View) -> bool),
}

/// A command's row: its picture, label, chord, and when it may be chosen.
#[derive(Copy, Clone)]
struct Spec {
    symbol: S,
    label: &'static str,
    chord: Option<Chord>,
    when: When,
    look: Look,
}

impl Spec {
    const fn new(symbol: S, label: &'static str, when: When) -> Self {
        Self {
            symbol,
            label,
            chord: None,
            when,
            look: Look::Plain,
        }
    }

    const fn on(self, chord: Chord) -> Self {
        Self {
            chord: Some(chord),
            ..self
        }
    }

    const fn destructive(self) -> Self {
        Self {
            look: Look::Destructive,
            ..self
        }
    }

    const fn mark(self, on: fn(&View) -> bool) -> Self {
        Self {
            look: Look::Mark(on),
            ..self
        }
    }
}

/// A row of the window menu.
#[derive(Copy, Clone, Debug)]
enum Row {
    /// A command's row.
    Command(Command),
    /// A rule between rows.
    Rule,
    /// The paper sizes, the page's ticked.
    Papers,
    /// The units, the page's ticked, in a submenu of their own.
    Units,
    /// The tools, the one in hand ticked.
    Tools,
}

const FILE: &[Row] = &[
    Row::Command(Command::NewWindow),
    Row::Command(Command::Open),
    Row::Command(Command::PlacePicture),
    Row::Rule,
    Row::Command(Command::Save),
    Row::Command(Command::SaveAs),
    Row::Command(Command::SaveCopy),
    Row::Rule,
    Row::Command(Command::Close),
];

const PAGE: &[Row] = &[
    Row::Command(Command::PageSetup),
    Row::Rule,
    Row::Papers,
    Row::Rule,
    Row::Command(Command::Landscape),
    Row::Command(Command::Ground),
    Row::Units,
];

const EDIT: &[Row] = &[
    Row::Command(Command::Undo),
    Row::Command(Command::Redo),
    Row::Rule,
    Row::Command(Command::Cut),
    Row::Command(Command::Copy),
    Row::Command(Command::Paste),
    Row::Command(Command::Duplicate),
    Row::Command(Command::Delete),
    Row::Rule,
    Row::Command(Command::SelectAll),
    Row::Command(Command::SelectNone),
];

const OBJECT: &[Row] = &[
    Row::Command(Command::Group),
    Row::Command(Command::Ungroup),
    Row::Command(Command::Clip),
    Row::Command(Command::ReleaseClip),
    Row::Rule,
    Row::Command(Command::ToFront),
    Row::Command(Command::Raise),
    Row::Command(Command::Lower),
    Row::Command(Command::ToBack),
    Row::Rule,
    Row::Command(Command::FlipAcross),
    Row::Command(Command::FlipDown),
    Row::Command(Command::TurnLeft),
    Row::Command(Command::TurnRight),
    Row::Rule,
    Row::Command(Command::Lock),
    Row::Command(Command::UnlockAll),
    Row::Command(Command::Hide),
    Row::Command(Command::ShowAll),
];

const PATH: &[Row] = &[
    Row::Command(Command::Union),
    Row::Command(Command::Difference),
    Row::Command(Command::Intersection),
    Row::Command(Command::Exclusion),
    Row::Command(Command::Divide),
    Row::Rule,
    Row::Command(Command::ToPaths),
    Row::Command(Command::OutlineStrokes),
    Row::Rule,
    Row::Command(Command::Outset),
    Row::Command(Command::Inset),
    Row::Command(Command::Simplify),
    Row::Command(Command::Reverse),
    Row::Rule,
    Row::Command(Command::TextOnPath),
    Row::Command(Command::ReleaseFromPath),
];

const LAYER: &[Row] = &[
    Row::Command(Command::NewLayer),
    Row::Command(Command::RemoveLayer),
    Row::Command(Command::RaiseLayer),
    Row::Command(Command::LowerLayer),
];

const VIEW: &[Row] = &[
    Row::Command(Command::ZoomIn),
    Row::Command(Command::ZoomOut),
    Row::Command(Command::ActualSize),
    Row::Command(Command::FitPage),
    Row::Command(Command::FitSelection),
    Row::Rule,
    Row::Command(Command::Outline),
    Row::Command(Command::Grid),
    Row::Command(Command::Snap),
];

/// The window menu: each submenu's title, its picture, and its rows.
const MENU: [(&str, S, &[Row]); 8] = [
    ("File", S::File, FILE),
    ("Page", S::Pages, PAGE),
    ("Edit", S::Edit, EDIT),
    ("Object", S::Properties, OBJECT),
    ("Path", S::ObjectToPath, PATH),
    ("Layer", S::Layers, LAYER),
    ("View", S::View, VIEW),
    ("Tools", S::Tools, &[Row::Tools]),
];

/// What a chosen menu row asks for.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Chosen {
    /// A command.
    Command(Command),
    /// A tool.
    Tool(Tool),
    /// A zoom.
    Zoom(f64),
    /// The paper size at this index of the table.
    Paper(usize),
    /// A unit for the page and the rulers.
    Unit(tairix_drawing::Unit),
}

/// What the row numbered `id` stands for.
#[must_use]
pub fn chosen(id: u16) -> Option<Chosen> {
    let from = |base: u16| id.checked_sub(base).map(usize::from);
    if let Some(tool) = from(TOOL_IDS).and_then(|at| Tool::ALL.get(at)) {
        return Some(Chosen::Tool(*tool));
    }
    if let Some(zoom) = from(ZOOM_IDS).and_then(|at| STEPS.get(at)) {
        return Some(Chosen::Zoom(*zoom));
    }
    if let Some(index) = from(PAPER_IDS).filter(|at| *at < tairix_drawing::PAPER.len()) {
        return Some(Chosen::Paper(index));
    }
    if let Some(unit) = from(UNIT_IDS).and_then(|at| tairix_drawing::Unit::ALL.get(at)) {
        return Some(Chosen::Unit(*unit));
    }
    Command::from_id(id).map(Chosen::Command)
}

/// The id of the `index`th row numbered from `base`.
fn numbered(base: u16, index: usize) -> u16 {
    base.saturating_add(u16::try_from(index).unwrap_or(u16::MAX))
}

/// What holds an object out of reach: a lock, or being hidden.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Hold {
    Lock,
    Hide,
}

impl Hold {
    fn holds(self, node: &Node) -> bool {
        match self {
            Self::Lock => node.locked(),
            Self::Hide => !node.visible(),
        }
    }

    fn set(self, node: &Node, held: bool) -> Node {
        match self {
            Self::Lock => node.with_locked(held),
            Self::Hide => node.with_visible(!held),
        }
    }

    const fn label(self, held: bool) -> &'static str {
        match (self, held) {
            (Self::Lock, true) => "Lock",
            (Self::Lock, false) => "Unlock all",
            (Self::Hide, true) => "Hide",
            (Self::Hide, false) => "Show all",
        }
    }
}

impl View {
    /// The menu of `kind`.
    #[must_use]
    pub fn build_menu(&self, kind: MenuKind) -> AppMenu {
        match kind {
            MenuKind::Window => self.window_menu(),
            MenuKind::Zoom => {
                let mut menu = MenuBuilder::new();
                for (index, zoom) in STEPS.iter().enumerate() {
                    let label = crate::viewport::readout(*zoom);
                    let on = (*zoom - self.viewport.zoom()).abs() < 1e-9;
                    menu.radio(
                        numbered(ZOOM_IDS, index),
                        S::ZoomLevel,
                        &label,
                        "",
                        on,
                        Plate::Root,
                    );
                }
                menu.finish()
            }
        }
    }

    fn window_menu(&self) -> AppMenu {
        let mut menu = MenuBuilder::titled("Draw");
        let mut caption = String::new();
        for (title, symbol, rows) in MENU {
            let Some(plate) = menu.submenu(title, symbol, Plate::Root) else {
                continue;
            };
            for &row in rows {
                match row {
                    Row::Command(command) => {
                        self.command_row(&mut menu, command, plate, &mut caption);
                    }
                    Row::Rule => menu.separator(plate),
                    Row::Papers => self.paper_rows(&mut menu, plate),
                    Row::Units => self.unit_rows(&mut menu, plate),
                    Row::Tools => self.tool_rows(&mut menu, plate),
                }
            }
        }
        menu.finish()
    }

    fn command_row(
        &self,
        menu: &mut MenuBuilder,
        command: Command,
        plate: Plate,
        caption: &mut String,
    ) {
        let spec = command.spec();
        caption.clear();
        if let Some(chord) = spec.chord {
            chord.caption(caption);
        }
        let label = self.label_of(command, spec.label);
        let enabled = self.allows(spec.when);
        match spec.look {
            Look::Plain => menu.item(command, spec.symbol, &label, caption, enabled, plate),
            Look::Destructive => {
                menu.destructive(command, spec.symbol, &label, caption, enabled, plate);
            }
            Look::Mark(on) => menu.mark(command, spec.symbol, &label, caption, on(self), plate),
        }
    }

    fn paper_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        let page = self.document.drawing().page();
        let size = (page.width, page.height);
        for (index, paper) in tairix_drawing::PAPER.iter().enumerate() {
            let fits = (paper.width, paper.height) == size || (paper.height, paper.width) == size;
            let on = fits && paper.unit == page.unit;
            menu.radio(
                numbered(PAPER_IDS, index),
                S::Pages,
                paper.name,
                "",
                on,
                plate,
            );
        }
    }

    fn unit_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        let Some(units) = menu.submenu("Units", S::Units, plate) else {
            return;
        };
        let unit = self.document.drawing().page().unit;
        for (index, each) in tairix_drawing::Unit::ALL.iter().enumerate() {
            let on = *each == unit;
            menu.radio(
                numbered(UNIT_IDS, index),
                S::Units,
                each.symbol(),
                "",
                on,
                units,
            );
        }
    }

    fn tool_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        for (index, tool) in Tool::ALL.iter().enumerate() {
            let mut key = [0; 4];
            let caption = tool.key().to_ascii_uppercase().encode_utf8(&mut key);
            let on = *tool == self.tool;
            menu.radio(
                numbered(TOOL_IDS, index),
                tool.symbol(),
                tool.name(),
                caption,
                on,
                plate,
            );
        }
    }

    /// What `command`'s row says: Undo and Redo name the step they take.
    fn label_of(&self, command: Command, label: &'static str) -> Cow<'static, str> {
        let step = match command {
            Command::Undo => self.undo_step(),
            Command::Redo => self.redo_step(),
            _ => None,
        };
        step.map_or(Cow::Borrowed(label), |step| {
            Cow::Owned(alloc::format!("{label} {step}"))
        })
    }

    /// What Undo would take back: what is under way, which lands first, or
    /// else the document's last step.
    fn undo_step(&self) -> Option<&'static str> {
        self.live
            .as_ref()
            .map(|live| live.label)
            .or_else(|| self.document.undo_label())
    }

    /// What Redo would do again: nothing while a change under way would
    /// land first and so end what could be done again.
    fn redo_step(&self) -> Option<&'static str> {
        self.live
            .is_none()
            .then(|| self.document.redo_label())
            .flatten()
    }

    /// Whether a row offered `when` may be chosen now.
    fn allows(&self, when: When) -> bool {
        match when {
            When::Always => true,
            When::Selected => !self.selection.is_empty(),
            When::Several => self.selection.len() > 1,
            When::Undoable => self.undo_step().is_some(),
            When::Redoable => self.redo_step().is_some(),
            When::Layers => self.document.drawing().layers().len() > 1,
        }
    }

    /// The command a chord pressed now carries out: one whose row may be
    /// chosen now.
    pub(crate) fn chord(&self, key: Key, modifiers: Modifiers) -> Option<Command> {
        Command::for_chord(key, modifiers).filter(|command| self.allows(command.spec().when))
    }

    /// Carry out a chosen menu row.
    pub fn choose(&mut self, chosen: Chosen, layout: &Layout, damage: &mut Region) -> ViewOutcome {
        damage.add(layout.window);
        match chosen {
            Chosen::Command(command) => return self.carry_out(command, layout, damage),
            Chosen::Tool(tool) => self.choose_tool(tool),
            Chosen::Zoom(zoom) => self.viewport.zoom_at(zoom, canvas_centre(layout)),
            Chosen::Paper(index) => {
                self.land_pending(damage);
                self.set_paper(index, layout);
            }
            Chosen::Unit(unit) => {
                self.land_pending(damage);
                self.set_unit(unit);
            }
        }
        Outcome::none()
    }

    /// Carry out `command`.
    pub fn carry_out(
        &mut self,
        command: Command,
        layout: &Layout,
        damage: &mut Region,
    ) -> ViewOutcome {
        damage.add(layout.window);
        if command.lands_pending_first() {
            self.land_pending(damage);
        }
        match command {
            Command::NewWindow => return self.asking(Request::Own(Own::NewWindow)),
            Command::Open => return self.asking(Request::Open),
            Command::PlacePicture => return self.asking(Request::Import),
            Command::Save => return self.asking(Request::Save),
            Command::SaveAs => return self.asking(Request::SaveAs),
            Command::SaveCopy => return self.asking(Request::SaveCopy),
            Command::Close => return self.closing(damage),
            Command::Cut | Command::Copy => return self.copy(command == Command::Cut),
            Command::Paste => return self.asking(Request::Own(Own::Paste)),
            Command::Union => return self.reshape(Reshape::Combine(Operation::Union)),
            Command::Difference => return self.reshape(Reshape::Combine(Operation::Difference)),
            Command::Intersection => {
                return self.reshape(Reshape::Combine(Operation::Intersection));
            }
            Command::Exclusion => return self.reshape(Reshape::Combine(Operation::Exclusion)),
            Command::Divide => return self.reshape(Reshape::Divide),
            Command::Outset => return self.reshape(Reshape::Offset(self.offset_distance)),
            Command::Inset => return self.reshape(Reshape::Offset(-self.offset_distance)),
            Command::Simplify => return self.reshape(Reshape::Simplify(self.simplify_tolerance)),
            Command::ToPaths => return self.reshape(Reshape::ToPaths),
            Command::OutlineStrokes => return self.reshape(Reshape::OutlineStrokes),
            Command::Undo => self.undo(false, damage),
            Command::Redo => self.undo(true, damage),
            Command::Duplicate => self.duplicate(),
            Command::Delete => self.delete_selection(),
            Command::SelectAll => self.select_all(),
            Command::SelectNone => self.selection.clear(),
            Command::Group => self.group(),
            Command::Ungroup => self.ungroup(),
            Command::Clip => self.clip(),
            Command::ReleaseClip => {
                let result = edit::release_clip(self.document.drawing(), &self.selection);
                self.apply(result, "Release clip");
            }
            Command::TextOnPath => {
                match edit::set_on_path(self.document.drawing(), &self.selection) {
                    Ok((drawing, path)) => {
                        self.commit(drawing, "Set text on path");
                        self.select(alloc::vec![path]);
                    }
                    Err(error) => self.say_error("Set text on path", error),
                }
            }
            Command::ReleaseFromPath => self.release_from_path(),
            Command::Reverse => {
                let result = edit::reverse(self.document.drawing(), &self.selection);
                self.apply(result, "Reverse direction");
            }
            Command::Raise => self.arrange(Arrange::Raise),
            Command::Lower => self.arrange(Arrange::Lower),
            Command::ToFront => self.arrange(Arrange::Top),
            Command::ToBack => self.arrange(Arrange::Bottom),
            Command::FlipAcross | Command::FlipDown => self.flip(command == Command::FlipDown),
            Command::TurnLeft => self.turn(-90.0),
            Command::TurnRight => self.turn(90.0),
            Command::Lock => self.hold(Hold::Lock),
            Command::Hide => self.hold(Hold::Hide),
            Command::UnlockAll => self.release_layer(Hold::Lock),
            Command::ShowAll => self.release_layer(Hold::Hide),
            Command::NewLayer => self.layer_command(LayerCommand::New),
            Command::RemoveLayer => self.layer_command(LayerCommand::Remove),
            Command::RaiseLayer => self.layer_command(LayerCommand::Raise),
            Command::LowerLayer => self.layer_command(LayerCommand::Lower),
            Command::ZoomIn | Command::ZoomOut => {
                let zoom = self.viewport.stepped(command == Command::ZoomOut);
                self.viewport.zoom_at(zoom, canvas_centre(layout));
            }
            Command::ActualSize => self.viewport.zoom_at(1.0, canvas_centre(layout)),
            Command::FitPage => self.fit(self.document.drawing().page().bounds(), layout),
            Command::FitSelection => self.fit(self.selection_bounds(), layout),
            Command::Outline => self.toggle_outlines(),
            Command::Grid => self.show.grid = !self.show.grid,
            Command::Snap => self.show.snapping = !self.show.snapping,
            Command::Landscape => {
                let page = self.document.drawing().page();
                self.lay_page(page.width <= page.height);
            }
            Command::Ground => self.toggle_ground(),
            Command::PageSetup => self.inspector.show_tab(Tab::Page),
        }
        self.refresh_inspector();
        Outcome::none()
    }

    /// Ask the host for `request`, the inspector brought up to date first.
    fn asking(&mut self, request: Request<MenuKind, Own>) -> ViewOutcome {
        self.refresh_inspector();
        Outcome::asking(request)
    }

    /// The window is to close: what is being typed or drawn with the pen
    /// lands first, and a drawing holding changes is never closed unasked.
    pub(crate) fn closing(&mut self, damage: &mut Region) -> ViewOutcome {
        self.land_pending(damage);
        self.finish_pen();
        let request = if self.is_modified() {
            Request::AskToClose
        } else {
            Request::Close
        };
        self.asking(request)
    }

    /// Put the selection on the clipboard, taking it out of the drawing when
    /// `cut`.
    fn copy(&mut self, cut: bool) -> ViewOutcome {
        let copied = match self.selection_drawing() {
            Ok(Some(copied)) => copied,
            Ok(None) => return Outcome::none(),
            Err(error) => {
                self.say_error("Copy", error.into());
                return Outcome::none();
            }
        };
        if cut {
            self.delete_selection();
        }
        self.asking(Request::Own(Own::Copy(copied)))
    }

    fn duplicate(&mut self) {
        let mut ids = self.document.ids().clone();
        let offset = Point::new(
            self.handling.duplicate_offset,
            self.handling.duplicate_offset,
        );
        match edit::duplicate(self.document.drawing(), &self.selection, offset, &mut ids) {
            Ok((drawing, placed)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Duplicate");
                self.select(placed);
            }
            Err(error) => self.say_error("Duplicate", error),
        }
    }

    fn group(&mut self) {
        let mut ids = self.document.ids().clone();
        match edit::group(self.document.drawing(), &self.selection, &mut ids) {
            Ok((drawing, path)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Group");
                self.select(alloc::vec![path]);
            }
            Err(error) => self.say_error("Group", error),
        }
    }

    /// Each selected text set along a path set in lines again, its path
    /// given back after it stroked as new lines are drawn.
    fn release_from_path(&mut self) {
        let mut ids = self.document.ids().clone();
        let mut line = self.style.clone();
        line.fill = tairix_drawing::Paint::None;
        match edit::release_from_path(self.document.drawing(), &self.selection, &line, &mut ids) {
            Ok((drawing, placed)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Release from path");
                self.select(placed);
            }
            Err(error) => self.say_error("Release from path", error),
        }
    }

    fn clip(&mut self) {
        let mut ids = self.document.ids().clone();
        match edit::clip(self.document.drawing(), &self.selection, &mut ids) {
            Ok((drawing, path)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Clip");
                self.select(alloc::vec![path]);
            }
            Err(error) => self.say_error("Clip", error),
        }
    }

    fn ungroup(&mut self) {
        let Some(path) = self.selection.first() else {
            return;
        };
        let mut ids = self.document.ids().clone();
        match edit::ungroup(self.document.drawing(), path, &mut ids) {
            Ok((drawing, placed)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Ungroup");
                self.select(placed);
            }
            Err(error) => self.say_error("Ungroup", error),
        }
    }

    fn flip(&mut self, down: bool) {
        let glyphs = Lettering::new(Arrived);
        let result = edit::flip(self.document.drawing(), &self.selection, down, &glyphs);
        self.apply(result, "Flip");
    }

    /// Turn the selection by `degrees` clockwise about its centre.
    fn turn(&mut self, degrees: f64) {
        let centre = self.selection_bounds().centre();
        let map = Affine::rotate_degrees_about(degrees, centre.x, centre.y);
        let result = edit::transform(self.document.drawing(), &self.selection, map);
        self.apply(result, "Turn");
    }

    /// Lock or hide the selection, which then can no longer be selected.
    fn hold(&mut self, hold: Hold) {
        let result = edit::each(self.document.drawing(), &self.selection, |_, _, node| {
            Ok(Some(hold.set(node, true)))
        });
        if self.apply(result, hold.label(true)) {
            self.selection.clear();
        }
    }

    /// Unlock, or show, what the layer in hand holds so.
    fn release_layer(&mut self, hold: Hold) {
        let drawing = self.document.drawing();
        let paths: Vec<_> = drawing
            .node_at(&[self.layer])
            .and_then(|layer| layer.children())
            .into_iter()
            .flat_map(|children| children.iter().enumerate())
            .filter(|(_, child)| hold.holds(child))
            .map(|(index, _)| alloc::vec![self.layer, index])
            .collect();
        if paths.is_empty() {
            return;
        }
        let result = edit::each(drawing, &paths, |_, _, node| {
            Ok(Some(hold.set(node, false)))
        });
        self.apply(result, hold.label(false));
    }

    fn toggle_outlines(&mut self) {
        self.show.outline = !self.show.outline;
        self.scene += 1;
        self.tiles.invalidate(self.scene);
    }

    /// The page laid out wider than it is tall, or taller than it is wide
    /// when not `landscape`.
    pub(crate) fn lay_page(&mut self, landscape: bool) {
        self.repage("Orientation", true, |page| *page = page.turned(landscape));
    }

    /// The page given the ground the Page tab's picker holds, or none where
    /// it has one.
    fn toggle_ground(&mut self) {
        let ground = self.inspector.ground_colour();
        self.repage("Page ground", true, |page| {
            page.background = match page.background {
                Some(_) => None,
                None => Some(ground),
            };
        });
    }

    /// The page made the paper at `index` of the table, turned as the page
    /// is turned now, and fitted in the window.
    fn set_paper(&mut self, index: usize, layout: &Layout) {
        let Some(paper) = tairix_drawing::PAPER.get(index) else {
            return;
        };
        let sized = self.repage("Page size", true, |page| {
            let landscape = page.width > page.height;
            let held = tairix_drawing::Page {
                background: page.background,
                ..tairix_drawing::Page::of(paper)
            };
            // A page on its side takes the paper on its side; an upright one
            // takes it as it is usually held, a screen wider than tall.
            *page = if landscape { held.turned(true) } else { held };
        });
        if sized {
            self.fit(self.shown().page().bounds(), layout);
        }
    }

    /// The page measured in `unit`, its size unchanged.
    pub(crate) fn set_unit(&mut self, unit: tairix_drawing::Unit) {
        self.repage("Units", true, |page| *page = page.measured_in(unit));
    }

    /// The page as `alter` leaves it: shown while `settled` is false, landed
    /// as one step called `label` once it is, and joining typing under way.
    /// Answers whether the drawing took it.
    pub(crate) fn repage(
        &mut self,
        label: &'static str,
        settled: bool,
        alter: impl FnOnce(&mut tairix_drawing::Page),
    ) -> bool {
        let base = self.edit_base();
        let mut page = *base.page();
        alter(&mut page);
        let result = base.with_page(page).map_err(edit::EditError::from);
        let taken = result.is_ok();
        self.take_change(result, label, settled);
        taken
    }

    /// The grid as `alter` leaves it, landed as one step called `label`.
    pub(crate) fn regrid(
        &mut self,
        label: &'static str,
        alter: impl FnOnce(&mut tairix_drawing::Grid),
    ) {
        let base = self.edit_base();
        let mut grid = base.grid();
        alter(&mut grid);
        let result = base.with_grid(grid).map_err(edit::EditError::from);
        self.take_change(result, label, true);
    }

    /// Fit `bounds` in the canvas.
    pub(crate) fn fit(&mut self, bounds: tairix_bezier::Bounds, layout: &Layout) {
        let size = Point::new(
            f64::from(layout.canvas.width),
            f64::from(layout.canvas.height),
        );
        let margin = f64::from(layout.length(24));
        self.viewport.fit(bounds, size, margin);
    }

    /// Select everything that may be: what is shown and unlocked, in layers
    /// shown and unlocked.
    fn select_all(&mut self) {
        match every_editable(self.shown()) {
            Some(all) => self.selection = all,
            None => {
                self.message = Some(String::from(
                    "There is not enough memory to select everything",
                ));
            }
        }
    }

    /// Ask the queue to reshape the selection.
    fn reshape(&mut self, operation: Reshape) -> ViewOutcome {
        if self.selection.is_empty() {
            return Outcome::none();
        }
        let Some(selection) = crate::view::copied_paths(&self.selection) else {
            self.message = Some(String::from("There is not enough memory to do that now"));
            return Outcome::none();
        };
        let job = self.next_job();
        self.pending = Some(job);
        self.message = Some(String::from("Working\u{2026}"));
        let compute = Compute {
            job,
            generation: self.document.generation(),
            drawing: self.document.drawing().clone(),
            selection,
            operation,
            ids: self.document.ids().clone(),
        };
        self.asking(Request::Own(Own::Compute(alloc::boxed::Box::new(compute))))
    }

    /// The queue's answer to a reshaping landed.
    pub fn computed(
        &mut self,
        computed: crate::compute::Computed,
        layout: &Layout,
        damage: &mut Region,
    ) {
        if self.pending != Some(computed.job) {
            return;
        }
        self.pending = None;
        damage.add(layout.window);
        if self.live.is_some() || computed.generation != self.document.generation() {
            self.message = Some(alloc::format!(
                "{} was not made: the drawing changed meanwhile",
                computed.label
            ));
            return;
        }
        match computed.result {
            Ok((drawing, selection, ids)) => {
                // Typing with nothing typed holds nothing to land, and ends.
                self.finish_typing();
                *self.document.ids() = ids;
                self.commit(drawing, computed.label);
                self.select(selection);
                self.message = None;
            }
            Err(error) => self.say_error(computed.label, error),
        }
        self.refresh_inspector();
    }
}

/// Every object of `drawing` that may be edited — in a shown, unlocked
/// layer, and shown and unlocked itself — or `None` when they cannot all be
/// held.
fn every_editable(drawing: &tairix_drawing::Drawing) -> Option<Vec<tairix_drawing::NodePath>> {
    let mut all = Vec::new();
    for (index, layer) in drawing.layers().iter().enumerate() {
        if !layer.visible() || layer.locked() {
            continue;
        }
        let Some(children) = layer.children() else {
            continue;
        };
        let mut chosen = Vec::new();
        chosen.try_reserve(children.len()).ok()?;
        chosen.extend(
            children
                .iter()
                .enumerate()
                .filter(|(_, child)| child.visible() && !child.locked())
                .map(|(at, _)| at),
        );
        let paths = edit::paths_in(&[index], chosen.into_iter()).ok()?;
        all.try_reserve(paths.len()).ok()?;
        all.extend(paths);
    }
    Some(all)
}

fn canvas_centre(layout: &Layout) -> Point {
    Point::new(
        f64::from(layout.canvas.width) / 2.0,
        f64::from(layout.canvas.height) / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec::Vec;

    use tairix_input::{Key, Modifiers, NamedKey};

    use super::{chosen, Chord, Chosen, Command, Row, ALTERNATES, MENU};

    fn ctrl(shift: bool) -> Modifiers {
        Modifiers {
            ctrl: true,
            shift,
            ..Modifiers::default()
        }
    }

    /// Whether some press of either chord's key would be both chords.
    fn overlap(a: Chord, b: Chord) -> bool {
        [a.key, b.key].into_iter().any(|key| {
            [false, true].into_iter().any(|shift| {
                let typed = match key {
                    Key::Char(character) if shift => Key::Char(character.to_ascii_uppercase()),
                    other => other,
                };
                let held = Modifiers {
                    ctrl: a.ctrl,
                    shift,
                    ..Modifiers::default()
                };
                a.matches(typed, held) && b.matches(typed, held)
            })
        })
    }

    #[test]
    fn every_command_is_numbered_by_its_place_and_offered_on_the_menu_once() {
        for (index, command) in Command::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(u16::from(command)), index + 1, "{command:?}");
            assert_eq!(chosen(u16::from(command)), Some(Chosen::Command(command)));
            let offered = MENU
                .iter()
                .flat_map(|(_, _, rows)| rows.iter())
                .filter(|row| matches!(row, Row::Command(each) if *each == command))
                .count();
            assert_eq!(offered, 1, "{command:?} is offered once");
        }
        assert_eq!(Command::from_id(0), None);
        let past = u16::try_from(Command::ALL.len() + 1).expect("small");
        assert_eq!(Command::from_id(past), None);
    }

    #[test]
    fn no_two_commands_answer_one_chord() {
        let mut chords: Vec<(Chord, Command)> = Command::ALL
            .into_iter()
            .filter_map(|command| command.spec().chord.map(|chord| (chord, command)))
            .collect();
        chords.extend(ALTERNATES);
        for (index, &(chord, command)) in chords.iter().enumerate() {
            for &(other, by) in &chords[index + 1..] {
                assert!(
                    !overlap(chord, other),
                    "{command:?} and {by:?} share {chord:?}"
                );
            }
        }
    }

    #[test]
    fn a_chord_is_found_by_its_key_and_shown_as_a_row_writes_it() {
        assert_eq!(
            Command::for_chord(Key::Char('S'), ctrl(true)),
            Some(Command::SaveAs)
        );
        assert_eq!(
            Command::for_chord(Key::Char('s'), ctrl(false)),
            Some(Command::Save)
        );
        assert_eq!(
            Command::for_chord(Key::Char('+'), ctrl(true)),
            Some(Command::ZoomIn)
        );
        assert_eq!(
            Command::for_chord(Key::Char('='), ctrl(false)),
            Some(Command::ZoomIn)
        );
        assert_eq!(
            Command::for_chord(Key::Char('y'), ctrl(false)),
            Some(Command::Redo)
        );
        let bare = Modifiers::default();
        let backspace = Key::Named(NamedKey::Backspace);
        assert_eq!(Command::for_chord(backspace, bare), Some(Command::Delete));
        assert_eq!(
            Command::for_chord(Key::Char('s'), bare),
            None,
            "a bare letter is a tool's"
        );
        let alt = Modifiers {
            alt: true,
            ..ctrl(false)
        };
        assert_eq!(Command::for_chord(Key::Char('s'), alt), None);
        let caption = |chord: Chord| {
            let mut out = String::new();
            chord.caption(&mut out);
            out
        };
        assert_eq!(caption(Chord::ctrl_shift('s')), "Ctrl+Shift+S");
        assert_eq!(
            caption(Chord::ctrl_shift_key(NamedKey::PageUp)),
            "Ctrl+Shift+Page Up"
        );
        assert_eq!(
            caption(Chord::key(NamedKey::Function { number: 11 })),
            "F11"
        );
    }
}
