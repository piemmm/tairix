//! A document application's windows: a window per document, each showing an
//! engine the application supplies, all in one process on the icon bar.
//!
//! This half is the contract the host drives a window's engine through —
//! [`DocumentView`], the [`Outcome`] an input comes to, the [`Request`]s
//! every document window makes alike, the [`ToolWindow`]s it tears out and
//! the [`DialogWindow`] it asks a question in — so an engine is host-tested
//! against the very interface it runs under. On
//! the bare-metal targets the
//! module is also the host itself (`Host`, `run`): the windows and the events
//! that reach them, each window's file and the saves and choosers it waits on,
//! the queue saves are written on, the icon-bar presence, and the exit that
//! sees every save out. It is named here in prose rather than linked, because
//! a host documentation build has no such items.

use alloc::string::String;

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItemId, CursorShape, DialogModality, SaveFormats, WindowSizing,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;

use crate::document::SavedDocument;

#[cfg(any(test, all(freestanding, feature = "rt")))]
mod closing;
#[cfg(all(freestanding, feature = "rt"))]
mod host;
#[cfg(all(freestanding, feature = "rt"))]
pub use host::*;
#[cfg(any(test, all(freestanding, feature = "rt")))]
mod members;
#[cfg(any(test, all(freestanding, feature = "rt")))]
mod refit;
#[cfg(any(test, all(freestanding, feature = "rt")))]
mod scroll;
#[cfg(feature = "settings")]
pub mod settings;
#[cfg(any(test, all(freestanding, feature = "rt")))]
mod title;

/// How far an input reshaped the window.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub enum Relayout {
    /// Nothing moved.
    #[default]
    None,
    /// Lay the window out again: what moved lies within what the engine
    /// reported.
    Reported,
    /// Lay the window out again and repaint it whole.
    Whole,
}

/// What an input came to beyond the damage it recorded.
#[derive(Debug)]
pub struct Outcome<R> {
    /// What the host is asked to carry out.
    pub request: Option<R>,
    /// Whether the window is laid out again, and how much of it repainted.
    pub relayout: Relayout,
}

impl<R> Outcome<R> {
    /// Nothing to carry out.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            request: None,
            relayout: Relayout::None,
        }
    }

    /// `request`, to carry out.
    #[must_use]
    pub const fn asking(request: R) -> Self {
        Self {
            request: Some(request),
            relayout: Relayout::None,
        }
    }

    /// Nothing to carry out, but the window laid out again and repainted
    /// whole.
    #[must_use]
    pub const fn relaid() -> Self {
        Self {
            request: None,
            relayout: Relayout::Whole,
        }
    }

    /// Nothing to carry out, but the window laid out again within what the
    /// engine reported.
    #[must_use]
    pub const fn reshaped() -> Self {
        Self {
            request: None,
            relayout: Relayout::Reported,
        }
    }
}

/// What a document window asks the host for: what every document window asks
/// alike, which the host carries out, or `Own`, which only its application
/// can.
#[derive(Debug)]
pub enum Request<K, O> {
    /// Save where the document came from, else ask where.
    Save,
    /// Save, and close the window once it has.
    SaveThenClose,
    /// Ask how and where to save the document.
    SaveAs,
    /// Ask where to save the document, how having been answered
    /// ([`DocumentView::ask_how`]); close the window once it is saved when
    /// `then_close`.
    SaveWhere {
        /// Close the window once saved.
        then_close: bool,
    },
    /// Ask where to write a copy of the document, and in which of the
    /// formats [`DocumentView::copy_formats`] offers: the document's own
    /// file, name and changes are left as they are.
    SaveCopy,
    /// Ask for a document to open.
    Open,
    /// Ask for a file to read into the document, which the application
    /// reads: handed over read-only, whatever the application may edit.
    Import,
    /// Close the window: its document is saved or its changes given up.
    Close,
    /// Ask whether to save the document's changes before the window closes:
    /// the host puts the question in an alert of its own.
    AskToClose,
    /// Open the window's `kind` menu at `anchor`, in window pixels.
    Menu {
        /// Which of the window's menus.
        kind: K,
        /// Where it opens from.
        anchor: Rect,
    },
    /// Something only the application carries out.
    Own(O),
}

/// What a document window's input comes to, for a window whose menus are
/// `V::MenuKind` and whose own requests are `V::Own`.
pub type ViewOutcome<V> = Outcome<Request<<V as DocumentView>::MenuKind, <V as DocumentView>::Own>>;

/// A tool window a document window wants open: a floating palette the window
/// manager titles and the user moves by its band — and resizes by its edges
/// where `sizing` lets it — showing `rect` of the window's own drawing and
/// taking the input that lands on it as the window's own
/// ([`DocumentView::tool_window`]).
///
/// The rectangle is laid out beside the window's own area — right of it or
/// below it, never overlapping it or another tool window's — so one drawing
/// and one set of hit tests serve the window and every palette it has torn
/// out, and the host only translates.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ToolWindow<'a> {
    /// The view's name for it, unique among its window's tool windows.
    pub id: u32,
    /// What its band reads.
    pub title: &'a str,
    /// The part of the window's drawing it shows, in the window's pixels;
    /// never left of or above the drawing's origin.
    pub rect: Rect,
    /// Whether the user may resize it, and within what range of clients, in
    /// the window's pixels: the host restates the range as it moves and
    /// hands each size the user gives it to [`DocumentView::tool_resized`].
    /// Its kind holds while it is open; one wanted at another kind is opened
    /// afresh.
    pub sizing: WindowSizing,
}

impl ToolWindow<'_> {
    /// Whether it may show its rectangle of a window whose own area is
    /// `area` ([`fits_beside`]). The host opens no tool window that does not
    /// fit.
    #[must_use]
    pub fn fits_beside(&self, area: Rect) -> bool {
        fits_beside(self.rect, area)
    }
}

/// Whether `rect` of a window's drawing may be shown in a window of its own
/// beside the window's own `area`: one with pixels in it, neither left of nor
/// above the drawing's origin, and overlapping nothing of the window's, so
/// every pixel and every input belongs to one of them alone.
#[must_use]
pub fn fits_beside(rect: Rect, area: Rect) -> bool {
    rect.surface_origin().is_some() && !rect.is_empty() && rect.intersection(&area).is_empty()
}

/// Where a menu a view anchors at `anchor` in its drawing opens: over the
/// first of `tools` — each a tool window's name and the part of the drawing
/// it shows — that holds the anchor's corner, the anchor then in that tool
/// window's own pixels, or else over the window itself.
#[must_use]
pub fn menu_over<T>(anchor: Rect, tools: impl IntoIterator<Item = (T, Rect)>) -> (Option<T>, Rect) {
    let corner = Point::new(anchor.left(), anchor.top());
    tools
        .into_iter()
        .find(|(_, shown)| shown.contains(corner))
        .map_or((None, anchor), |(tool, shown)| {
            let local = Rect::new(
                corner.x.saturating_sub(shown.left()),
                corner.y.saturating_sub(shown.top()),
                anchor.width,
                anchor.height,
            );
            (Some(tool), local)
        })
}

/// The dialog a document window wants open: a form or a question about its
/// document, in a window of its own that the desktop frames, centres over the
/// document's, and lets hold it as `modality` says
/// ([`DocumentView::dialog_window`]).
///
/// It shows `rect` of the window's own drawing, laid out beside the window's
/// area as a tool window's is and overlapping no tool window's, and what
/// reaches it goes to [`DocumentView::dialog_input`], so the window's own
/// input is never filtered for it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DialogWindow<'a> {
    /// What its band reads after the application's name.
    pub title: &'a str,
    /// The part of the window's drawing it shows, in the window's pixels.
    pub rect: Rect,
    /// Whether it holds the window until it closes.
    pub modality: DialogModality,
}

/// Where a tool window opens ([`DocumentView::tool_opening`]).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolOpening {
    /// Its client's top-left from the window's client origin, in pixels.
    pub offset: (i32, i32),
    /// Where along its band, in pixels from its left edge, the press the
    /// window still holds carries it — the press that tore it out — if any.
    pub carry: Option<u32>,
}

/// One report of a tool window being moved by its band
/// ([`DocumentView::tool_moved`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ToolMove {
    /// Which tool window.
    pub id: u32,
    /// Where the pointer is over the window, in its pixels, when it is over
    /// the window with nothing else in the way.
    pub over: Option<Point>,
    /// The move is over: this is where it was let go.
    pub ended: bool,
}

/// Why a tool window went ([`DocumentView::tool_gone`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ToolGone {
    /// The user pressed its close mark.
    Closed,
    /// The desktop would not open it.
    Refused,
}

/// Which file a save writes ([`DocumentView::refuse_save`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaveTarget {
    /// The document's own, whose recorded format may say what its name does
    /// not.
    Own,
    /// Another, named in a Save As: only its name says its format.
    Another,
}

/// What the host needs of the engine a document window shows.
pub trait DocumentView: SavedDocument {
    /// The faces the window sets its text in, chosen again on a desktop
    /// change.
    type Faces: Copy;
    /// Which of the window's menus a [`Request::Menu`] opens.
    type MenuKind: Copy;
    /// What an input asks only the application for.
    type Own;

    /// What the document is called.
    fn name(&self) -> &str;

    /// Write what the window's title reads over `title`, reusing its room.
    fn write_title(&self, title: &mut String);

    /// Lay the view out for a `width`×`height` window, reporting what moved.
    /// The host lays a view out before it feeds it input or draws it.
    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Self::Faces,
        damage: &mut Region,
    );

    /// The smallest window it can be laid out in, as `(width, height)`.
    fn min_size(&self, theme: &Theme, scale: Scale, faces: Self::Faces) -> (u32, u32);

    /// The size, in logical pixels, a window opened for this view takes, laid
    /// out in `theme` with `faces` at the reference scale, or `None` for the
    /// application's own.
    fn open_size(&self, _theme: &Theme, _faces: Self::Faces) -> Option<(u32, u32)> {
        None
    }

    /// Where what the window is told ([`SavedDocument::say`]) is shown.
    fn message_area(&self) -> Rect;

    /// Whether a question of the view's own showing is part of closing — how
    /// to save before the window closes — which a quit waits on. Whether to
    /// save at all is the host's question, not the view's.
    fn asking_to_close(&self) -> bool {
        false
    }

    /// The pointer's shape over `at`.
    fn cursor(&self, at: Point) -> CursorShape;

    /// The tip for what the pointer is over, and the region it covers.
    fn tool_tip(&self, _scale: Scale, _theme: &Theme) -> Option<(Rect, &str)> {
        None
    }

    /// The rows of the window's `kind` menu.
    fn menu(&self, kind: Self::MenuKind) -> AppMenu;

    /// The user asked to close the window: answer [`Request::Close`], or
    /// [`Request::AskToClose`] for a document holding changes.
    fn close_requested(&mut self, damage: &mut Region) -> ViewOutcome<Self>;

    /// The user chose to save before the window closes: answer
    /// [`Request::SaveThenClose`] once what the view holds apart from its
    /// document is in it, as any save of its own would.
    fn save_then_close(&mut self, _damage: &mut Region) -> ViewOutcome<Self> {
        Outcome::asking(Request::SaveThenClose)
    }

    /// The menu row `item` was chosen.
    fn chosen(&mut self, item: AppMenuItemId, damage: &mut Region) -> ViewOutcome<Self>;

    /// A menu's entry field `item` was committed holding `text`. A window
    /// whose menus hold no entry field is never answered with one.
    fn entered(
        &mut self,
        _item: AppMenuItemId,
        _text: &str,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// The window gained or lost the keyboard.
    fn focus_changed(&mut self, focused: bool, damage: &mut Region);

    /// Feed `input` — a key, the modifiers held, or the pointer — at
    /// `now_ns`: for the pointer, when its device reported it.
    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome<Self>;

    /// Why the document cannot be saved as `name`, into `target`, when it
    /// cannot: a save refused so never touches the file.
    fn refuse_save(&self, _name: &str, _target: SaveTarget) -> Option<String> {
        None
    }

    /// Whether a file of media type `media` dropped at window-local `at` is
    /// read into the document there, as an import is; a drop it is not is
    /// left to the application, which opens the file or refuses it.
    fn places(&self, _at: Point, _media: &str) -> bool {
        false
    }

    /// Ask how the document is to be saved before the picker asks where,
    /// closing the window once it is saved when `then_close`: answering
    /// `Some` puts the question up, or begins what putting it up needs —
    /// which the host carries out as any outcome — and its answer asks for
    /// [`Request::SaveWhere`]. By default nothing is asked.
    fn ask_how(&mut self, _then_close: bool, _damage: &mut Region) -> Option<ViewOutcome<Self>> {
        None
    }

    /// The extension a Save As offers the document under a name of its own —
    /// one it has never had, or one its own name cannot be written as — or
    /// `""` for a format named by its file type alone.
    fn offered_extension(&self) -> &'static str;

    /// The formats a copy may be written in, for the chooser a
    /// [`Request::SaveCopy`] opens, or why no copy can be written.
    ///
    /// # Errors
    ///
    /// Why the document cannot be copied, for an application that writes no
    /// copies: the default.
    fn copy_formats(&self) -> Result<SaveFormats, String> {
        Err(String::from("This application writes no copies"))
    }

    /// The extension a copy's name is offered with.
    fn copy_extension(&self) -> &'static str {
        self.offered_extension()
    }

    /// The formats a Save As can write and the one the document is in now,
    /// which the picker opens on and holds the chosen name to before it makes
    /// the file — typed as the format chosen, by its recorded file type or its
    /// name, so the save tells the format from the file: by default none, as
    /// any name holds the document.
    ///
    /// # Errors
    ///
    /// Why no name can hold it: no picker is opened.
    fn save_formats(&self) -> Result<SaveFormats, String> {
        Ok(SaveFormats::ANY)
    }

    /// The `index`th tool window the window wants open, or `None` past the
    /// last. Asked after every round, so the view says what it wants and the
    /// host opens, resizes, retitles and closes tool windows to match. By
    /// default none.
    fn tool_window(&self, _index: usize) -> Option<ToolWindow<'_>> {
        None
    }

    /// Where tool window `id` opens, asked once as the host opens it: a carry
    /// belongs to the open it was asked for and to no later one.
    fn tool_opening(&mut self, _id: u32) -> ToolOpening {
        ToolOpening::default()
    }

    /// A tool window is being moved by its band, as `moved` reports.
    fn tool_moved(
        &mut self,
        _moved: ToolMove,
        _scale: Scale,
        _theme: &Theme,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// The user resized tool window `id` to `size`, `(width, height)` in the
    /// window's pixels and within the range its [`ToolWindow::sizing`]
    /// stated: the host has re-mapped it at that size, and paints it whole
    /// once the view lays it out. One held at its own size is never told. By
    /// default nothing.
    fn tool_resized(
        &mut self,
        _id: u32,
        _size: (u32, u32),
        _scale: Scale,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// Tool window `id` went, for `why`. A view answering
    /// [`ToolGone::Refused`] stops wanting it, or it is asked for again.
    fn tool_gone(&mut self, _id: u32, _why: ToolGone, _damage: &mut Region) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// The dialog the window wants open at `scale` in `theme`, if any. Asked
    /// after every round, as tool windows are, so the host opens, resizes,
    /// retitles and closes it to match. By default none.
    fn dialog_window(&self, _scale: Scale, _theme: &Theme) -> Option<DialogWindow<'_>> {
        None
    }

    /// Feed `input` that reached the window's dialog — a key while it holds
    /// the keyboard, or the pointer over it in the drawing's pixels — at
    /// `now_ns`: for the pointer, when its device reported it.
    fn dialog_input(
        &mut self,
        _input: &InputEvent,
        _now_ns: u64,
        _scale: Scale,
        _theme: &Theme,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// The dialog gained or lost the keyboard; the host repaints it whole.
    fn dialog_focus_changed(&mut self, _focused: bool, _damage: &mut Region) {}

    /// The desktop would not open the dialog, for `why`. The view stops
    /// wanting it, or it is asked for again.
    fn dialog_refused(&mut self, _why: &str, _damage: &mut Region) -> ViewOutcome<Self> {
        Outcome::none()
    }
}

/// What a window of the application's own that holds no document — its
/// settings, say — shows. The host drives it as it drives a document's: it is
/// opened by the application, laid out, fed its input, painted from what it
/// reports, and closed by its close mark or by Quit, which it never holds
/// back.
pub trait AppView {
    /// The faces it sets its text in.
    type Faces: Copy;
    /// What an input asks only the application for.
    type Own;

    /// What its title reads.
    fn title(&self) -> &str;

    /// The size it opens at, in logical pixels.
    fn size(&self) -> (u32, u32);

    /// Lay the view out for a `width`×`height` window, reporting what moved.
    /// The host lays a view out before it feeds it input or draws it.
    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Self::Faces,
        damage: &mut Region,
    );

    /// The smallest window it can be laid out in, as `(width, height)`.
    fn min_size(&self, theme: &Theme, scale: Scale, faces: Self::Faces) -> (u32, u32);

    /// The pointer's shape over `at`.
    fn cursor(&self, at: Point) -> CursorShape;

    /// The window gained or lost the keyboard.
    fn focus_changed(&mut self, focused: bool, damage: &mut Region);

    /// Feed `input` — a key, the modifiers held, or the pointer — at
    /// `now_ns`: for the pointer, when its device reported it.
    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Self::Own>>;

    /// When a control of the window next changes on its own — a held one
    /// stepping — if one will. Asked after every round, so nothing polls.
    fn deadline_ns(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }

    /// Carry out what came due by `now_ns`, reporting what moved.
    fn advance(&mut self, _now_ns: u64, _scale: Scale, _theme: &Theme, _damage: &mut Region) {}
}

/// What a window of the application's own asks the host for.
#[derive(Debug, Eq, PartialEq)]
pub enum AppRequest<O> {
    /// Close it.
    Close,
    /// Something only the application carries out.
    Own(O),
}

/// The windows of an application that opens none of its own beside its
/// documents': there are none to show.
pub enum NoAppView<F> {
    /// Never made: an application with no windows of its own has none.
    Never(core::convert::Infallible, core::marker::PhantomData<F>),
}

impl<F> NoAppView<F> {
    /// Whatever is asked of one: none is ever made, so nothing ever is.
    #[must_use]
    pub fn never<T>(&self) -> T {
        match *self {
            Self::Never(never, _) => match never {},
        }
    }
}

impl<F: Copy> AppView for NoAppView<F> {
    type Faces = F;
    type Own = core::convert::Infallible;

    fn title(&self) -> &str {
        self.never()
    }

    fn size(&self) -> (u32, u32) {
        self.never()
    }

    fn lay_out(&mut self, _: u32, _: u32, _: &Theme, _: Scale, _: F, _: &mut Region) {
        let () = self.never();
    }

    fn min_size(&self, _: &Theme, _: Scale, _: F) -> (u32, u32) {
        self.never()
    }

    fn cursor(&self, _: Point) -> CursorShape {
        self.never()
    }

    fn focus_changed(&mut self, _: bool, _: &mut Region) {
        let () = self.never();
    }

    fn input(
        &mut self,
        _: &InputEvent,
        _: u64,
        _: Scale,
        _: &Theme,
        _: &mut Region,
    ) -> Outcome<AppRequest<Self::Own>> {
        self.never()
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::window_ipc::WindowSizing;
    use tairix_geometry::Rect;

    use super::ToolWindow;

    #[test]
    fn a_tool_window_fits_only_beside_its_window() {
        let area = Rect::new(0, 0, 400, 300);
        let fits = |rect| {
            ToolWindow {
                id: 1,
                title: "Tools",
                rect,
                sizing: WindowSizing::Fixed,
            }
            .fits_beside(area)
        };
        assert!(fits(Rect::new(400, 0, 120, 200)), "right of it");
        assert!(fits(Rect::new(0, 300, 120, 200)), "below it");
        assert!(!fits(Rect::new(399, 0, 120, 200)), "over its last column");
        assert!(!fits(Rect::new(-1, 400, 120, 200)), "left of the drawing");
        assert!(!fits(Rect::new(400, 0, 0, 200)), "with no pixels");
    }

    #[test]
    fn a_menu_anchored_on_a_tool_window_opens_over_it() {
        let tools = [(7_u64, Rect::new(400, 20, 120, 200))];
        assert_eq!(
            super::menu_over(Rect::new(450, 60, 0, 0), tools),
            (Some(7), Rect::new(50, 40, 0, 0)),
            "in the tool window's own pixels"
        );
        assert_eq!(
            super::menu_over(Rect::new(30, 60, 0, 0), tools),
            (None, Rect::new(30, 60, 0, 0)),
            "over the window itself"
        );
    }
}
