//! A document application's windows: a window per document, each showing an
//! engine the application supplies, all in one process on the icon bar.
//!
//! This half is the contract the host drives a window's engine through —
//! [`DocumentView`], the [`Outcome`] an input comes to, the [`Request`]s
//! every document window makes alike, and the [`ToolWindow`]s it tears out —
//! so an engine is host-tested against the very interface it runs under. On
//! the bare-metal targets the
//! module is also the host itself (`Host`, `run`): the windows and the events
//! that reach them, each window's file and the saves and choosers it waits on,
//! the queue saves are written on, the icon-bar presence, and the exit that
//! sees every save out. It is named here in prose rather than linked, because
//! a host documentation build has no such items.

use alloc::string::String;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveEndings};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;

use crate::document::SavedDocument;

#[cfg(all(freestanding, feature = "rt"))]
mod host;
#[cfg(all(freestanding, feature = "rt"))]
pub use host::*;

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
    /// Ask for a document to open.
    Open,
    /// Close the window: its document is saved or its changes given up.
    Close,
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
/// manager titles and the user moves by its band, showing `rect` of the
/// window's own drawing and taking the input that lands on it as the
/// window's own ([`DocumentView::tool_window`]).
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
}

impl ToolWindow<'_> {
    /// Whether it may show its rectangle of a window whose own area is
    /// `area`: one with pixels in it, neither left of nor above the drawing's
    /// origin, and overlapping nothing of the window's, so every pixel and
    /// every input belongs to one of them alone. The host opens no tool
    /// window that does not fit.
    #[must_use]
    pub fn fits_beside(&self, area: Rect) -> bool {
        self.rect.surface_origin().is_some()
            && !self.rect.is_empty()
            && self.rect.intersection(&area).is_empty()
    }
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

/// What the host needs of the engine a document window shows.
pub trait DocumentView: SavedDocument {
    /// Where everything the window draws goes, for a window of one size.
    type Layout;
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

    /// The layout of a `width`×`height` window.
    fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Self::Faces,
    ) -> Self::Layout;

    /// The smallest window it can be laid out in, as `(width, height)`.
    fn min_size(&self, theme: &Theme, scale: Scale, faces: Self::Faces) -> (u32, u32);

    /// Bring the view into line with `layout`, reporting what moved.
    fn settle(&mut self, layout: &Self::Layout, damage: &mut Region);

    /// Where what the window is told ([`SavedDocument::say`]) is shown.
    fn message_area(layout: &Self::Layout) -> Rect;

    /// Whether the question showing is whether to save before closing,
    /// which a quit waits on; a quit puts that question in place of any
    /// other.
    fn asking_to_close(&self) -> bool;

    /// The pointer's shape over `at`.
    fn cursor(&self, layout: &Self::Layout, at: Point) -> CursorShape;

    /// The tip for what the pointer is over, and the region it covers.
    fn tool_tip(
        &self,
        _layout: &Self::Layout,
        _scale: Scale,
        _theme: &Theme,
    ) -> Option<(Rect, &str)> {
        None
    }

    /// The rows of the window's `kind` menu.
    fn menu(&self, kind: Self::MenuKind) -> AppMenu;

    /// The user asked to close the window.
    fn close_requested(&mut self, layout: &Self::Layout, damage: &mut Region) -> ViewOutcome<Self>;

    /// The menu row `item` was chosen.
    fn chosen(
        &mut self,
        item: AppMenuItemId,
        layout: &Self::Layout,
        damage: &mut Region,
    ) -> ViewOutcome<Self>;

    /// A menu's entry field `item` was committed holding `text`. A window
    /// whose menus hold no entry field is never answered with one.
    fn entered(
        &mut self,
        _item: AppMenuItemId,
        _text: &str,
        _layout: &Self::Layout,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// The window gained or lost the keyboard.
    fn focus_changed(&mut self, focused: bool, layout: &Self::Layout, damage: &mut Region);

    /// Feed `input` — a key, the modifiers held, or the pointer — at
    /// `now_ns`.
    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &Self::Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome<Self>;

    /// Why the document cannot be saved as `name`, when it cannot: a save
    /// refused so never touches the file.
    fn refuse_save(&self, _name: &str) -> Option<String> {
        None
    }

    /// Ask how the document is to be saved before the picker asks where,
    /// closing the window once it is saved when `then_close`: answering
    /// `Some` puts the question up, or begins what putting it up needs —
    /// which the host carries out as any outcome — and its answer asks for
    /// [`Request::SaveWhere`]. By default nothing is asked.
    fn ask_how(
        &mut self,
        _then_close: bool,
        _layout: &Self::Layout,
        _damage: &mut Region,
    ) -> Option<ViewOutcome<Self>> {
        None
    }

    /// The extension a Save As offers the document under a name of its own:
    /// one it has never had, or one its own name cannot be written as.
    fn offered_extension(&self) -> &'static str;

    /// The endings a Save As holds the chosen name to, so the picker refuses
    /// a name the save would before it makes the file: by default none, as
    /// any name holds the document.
    ///
    /// # Errors
    ///
    /// Why no name can hold it: no picker is opened.
    fn save_endings(&self) -> Result<SaveEndings, String> {
        Ok(SaveEndings::ANY)
    }

    /// The `index`th tool window the window wants open as laid out by
    /// `layout`, or `None` past the last. Asked after every round, so the
    /// view says what it wants and the host opens, resizes, retitles and
    /// closes tool windows to match. By default none.
    fn tool_window(&self, _layout: &Self::Layout, _index: usize) -> Option<ToolWindow<'_>> {
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
        _layout: &Self::Layout,
        _scale: Scale,
        _theme: &Theme,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }

    /// Tool window `id` went, for `why`. A view answering
    /// [`ToolGone::Refused`] stops wanting it, or it is asked for again.
    fn tool_gone(
        &mut self,
        _id: u32,
        _why: ToolGone,
        _layout: &Self::Layout,
        _damage: &mut Region,
    ) -> ViewOutcome<Self> {
        Outcome::none()
    }
}

/// What a window of the application's own that holds no document — its
/// settings, say — shows. The host drives it as it drives a document's: it is
/// opened by the application, laid out, fed its input, painted from what it
/// reports, and closed by its close mark or by Quit, which it never holds
/// back.
pub trait AppView {
    /// Where everything the window draws goes, for a window of one size.
    type Layout;
    /// The faces it sets its text in.
    type Faces: Copy;
    /// What an input asks only the application for.
    type Own;

    /// What its title reads.
    fn title(&self) -> &str;

    /// The size it opens at, in logical pixels.
    fn size(&self) -> (u32, u32);

    /// The layout of a `width`×`height` window.
    fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Self::Faces,
    ) -> Self::Layout;

    /// The smallest window it can be laid out in, as `(width, height)`.
    fn min_size(&self, theme: &Theme, scale: Scale, faces: Self::Faces) -> (u32, u32);

    /// Bring the view into line with `layout`, reporting what moved.
    fn settle(&mut self, layout: &Self::Layout, damage: &mut Region);

    /// The pointer's shape over `at`.
    fn cursor(&self, layout: &Self::Layout, at: Point) -> CursorShape;

    /// The window gained or lost the keyboard.
    fn focus_changed(&mut self, focused: bool, layout: &Self::Layout, damage: &mut Region);

    /// Feed `input` — a key, the modifiers held, or the pointer — at
    /// `now_ns`.
    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &Self::Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Self::Own>>;
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
    type Layout = ();
    type Faces = F;
    type Own = core::convert::Infallible;

    fn title(&self) -> &str {
        self.never()
    }

    fn size(&self) -> (u32, u32) {
        self.never()
    }

    fn layout(&self, _: u32, _: u32, _: &Theme, _: Scale, _: F) {
        let () = self.never();
    }

    fn min_size(&self, _: &Theme, _: Scale, _: F) -> (u32, u32) {
        self.never()
    }

    fn settle(&mut self, (): &(), _: &mut Region) {
        let () = self.never();
    }

    fn cursor(&self, (): &(), _: Point) -> CursorShape {
        self.never()
    }

    fn focus_changed(&mut self, _: bool, (): &(), _: &mut Region) {
        let () = self.never();
    }

    fn input(
        &mut self,
        _: &InputEvent,
        _: u64,
        (): &(),
        _: Scale,
        _: &Theme,
        _: &mut Region,
    ) -> Outcome<AppRequest<Self::Own>> {
        self.never()
    }
}

#[cfg(test)]
mod tests {
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
            }
            .fits_beside(area)
        };
        assert!(fits(Rect::new(400, 0, 120, 200)), "right of it");
        assert!(fits(Rect::new(0, 300, 120, 200)), "below it");
        assert!(!fits(Rect::new(399, 0, 120, 200)), "over its last column");
        assert!(!fits(Rect::new(-1, 400, 120, 200)), "left of the drawing");
        assert!(!fits(Rect::new(400, 0, 0, 200)), "with no pixels");
    }
}
