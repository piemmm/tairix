//! The library's windows on the desktop (feature `rt`): the library window of
//! machine cards and the dialogs over it, each painted into a surface of its
//! own and presented once a turn, and the icon-bar presence that brings the
//! library back.

use alloc::format;
use alloc::string::String;

use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertSeverity, AlertSpec, AppMenu, AppMenuItemId, DialogModality,
    MenuOutcome, WindowEvent, WindowRegion, MENU_OPEN_REVISION,
};
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_window::app::{self, Fitted, WindowPane};
use tairix_window::{
    key_input_event, pointer_input_events, pointer_point, scroll_input_events, Desktop, Owed,
    Repaint, WindowClient, WindowSizing,
};

use crate::assistant::{Answer, Assistant};
use crate::menu::{bar_rows, BAR_CLICK, LIBRARY_ROW};
use crate::shelf::{Entry, LibraryLook, Pictures, Request, Shelf};

/// The window channel the library's windows are served over.
pub type Client = WindowClient<app::RtWindowTransport>;

/// Paint `damage` of a window through `render`, each rectangle under its own
/// clip, and owe it.
pub fn repaint(
    pane: &WindowPane,
    surface: &mut Surface,
    owed: &mut Owed,
    damage: &Region,
    mut render: impl FnMut(&mut Surface),
) {
    for rect in damage.rects() {
        if let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) {
            surface.with_clip(x, y, rect.width, rect.height, &mut render);
        }
    }
    owed.owe(pane.mode(), Repaint::Reported, damage);
}

/// The whole of a window, as damage.
#[must_use]
pub fn whole(pane: &WindowPane) -> Region {
    let mode = pane.mode();
    let mut damage = Region::new();
    damage.add(Rect::new(0, 0, mode.width_px, mode.height_px));
    damage
}

/// Present what a window owes.
///
/// # Errors
///
/// The desktop's refusal of the present.
pub fn present(
    pane: &mut WindowPane,
    surface: &Surface,
    owed: &mut Owed,
    client: &mut Client,
) -> Result<(), Errno> {
    let mode = *pane.mode();
    let Some(parts) = owed.take(&mode) else {
        return Ok(());
    };
    pane.present_list(client, surface, &parts)
}

/// Hand `each` the shared input events a delivered window event means.
pub fn inputs(event: &WindowEvent, mut each: impl FnMut(&InputEvent)) {
    match *event {
        WindowEvent::Pointer { x, y, action, .. } => {
            pointer_input_events(action, pointer_point(x, y)).for_each(|input| each(&input));
        }
        WindowEvent::Scrolled { x, y, dx, dy, .. } => {
            scroll_input_events(pointer_point(x, y), dx, dy).for_each(|input| each(&input));
        }
        WindowEvent::Key { key, .. } => each(&key_input_event(key)),
        _ => {}
    }
}

/// When a delivered event was made: a pointer's own time, else now.
#[must_use]
pub fn event_time(event: &WindowEvent) -> u64 {
    match *event {
        WindowEvent::Pointer { at_ns, .. } => at_ns,
        _ => tairix_rt::clock_get(),
    }
}

/// Show `rows` in the menu `open` up over window `window_id`, a refusal
/// reported under the program's `name`.
pub fn restate(client: &mut Client, (name, window_id): (&str, u64), open: u64, rows: &AppMenu) {
    if let Err(err) = client.restate_menu(window_id, open, MENU_OPEN_REVISION, rows) {
        app::report(name, format_args!("the menu could not be updated ({err})"));
    }
}

/// Declare the application's icon-bar presence: a press raises the window it
/// used last or opens the library, and the library's row sits between the
/// convention's ends.
///
/// # Errors
///
/// Why the presence was refused, as the user is told.
pub fn declare_bar(client: &mut Client, endpoint: u64) -> Result<(), String> {
    let declared =
        bar_rows().and_then(|rows| tairix_window::declaration(endpoint, BAR_CLICK, &rows));
    tairix_window::declare_app_bar(client, declared).map_err(|refused| format!("{refused}"))
}

/// What an application-scoped event asks of an application keeping a library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAsk {
    /// Bring the library back.
    Library,
    /// End the program.
    Quit,
}

/// What `event`, an application-scoped one, asks: nothing for one that is
/// not the icon bar's.
#[must_use]
pub fn bar_ask(event: &WindowEvent) -> Option<BarAsk> {
    match *event {
        WindowEvent::AppBarMenu { item, .. } if tairix_window::is_quit(item) => Some(BarAsk::Quit),
        WindowEvent::AppBarDefault => Some(BarAsk::Library),
        WindowEvent::AppBarMenu { item, .. } if item.get() == LIBRARY_ROW => Some(BarAsk::Library),
        _ => None,
    }
}

/// The question a machine's deletion asks first, saying what `goes` with it.
#[must_use]
pub fn delete_question(name: &str, goes: &str) -> Option<AlertSpec> {
    let buttons = [
        AlertButton::new("Delete", AlertButtonRole::Destructive).ok()?,
        AlertButton::new("Keep", AlertButtonRole::Cancel).ok()?,
    ];
    AlertSpec::fitted(
        AlertSeverity::Question,
        &format!("Delete \u{201c}{name}\u{201d}?"),
        goes,
        &buttons,
    )
    .ok()
}

/// What a delivered event asks of the library window's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryEvent {
    /// Nothing further.
    Nothing,
    /// The window was closed.
    Closed,
    /// Row `item` of the menu `open_id` was chosen, or none, the menu `kept`
    /// up or ending with it.
    Menu {
        /// The menu.
        open_id: u64,
        /// The row, if one was chosen.
        item: Option<AppMenuItemId>,
        /// Whether the menu stays up.
        kept: bool,
    },
    /// What the window asks for.
    Request(Request),
}

/// The library window: its pane, the view it shows and what it owes.
pub struct LibraryWindow {
    pane: WindowPane,
    surface: Surface,
    owed: Owed,
    shelf: Shelf,
}

impl LibraryWindow {
    /// Open the library window as `look` says, on `desktop`.
    ///
    /// # Errors
    ///
    /// Why it could not be opened, as the user is told.
    pub fn open(
        client: &mut Client,
        endpoint: u64,
        (desktop, theme): (&Desktop, &Theme),
        look: LibraryLook,
    ) -> Result<Self, String> {
        let (width, height) = desktop.window_size(look.size.0, look.size.1);
        let least = desktop.window_size(look.least.0, look.least.1);
        let mode = app::mode_for(width, height);
        let surface = Surface::new(mode.width_px, mode.height_px)
            .ok_or_else(|| String::from("there is not enough memory to draw the library"))?;
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1,
            max_width_px: 0,
            max_height_px: 0,
        };
        let pane = WindowPane::open(client, endpoint, &mode, look.title, sizing)
            .map_err(|err| format!("the desktop would not open the library ({err})"))?;
        let mut shelf = Shelf::new();
        shelf.set_size((mode.width_px, mode.height_px), (desktop.scale(), theme));
        Ok(Self {
            pane,
            surface,
            owed: Owed::new(),
            shelf,
        })
    }

    /// The window's id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.pane.id()
    }

    /// Show `entries`, repainting what that changed.
    pub fn show(
        &mut self,
        entries: impl IntoIterator<Item = Entry>,
        looks: (Scale, &Theme),
        pictures: &Pictures,
    ) {
        let mut damage = Region::new();
        self.shelf.show(entries, looks, &mut damage);
        self.paint(&damage, looks, pictures);
    }

    /// Repaint the whole window: its size or its look changed.
    pub fn paint_whole(&mut self, looks: (Scale, &Theme), pictures: &Pictures) {
        let damage = whole(&self.pane);
        self.paint(&damage, looks, pictures);
    }

    /// Machine `id`'s picture was drawn anew: repaint its card.
    pub fn picture_drawn(&mut self, id: u32, looks: (Scale, &Theme), pictures: &Pictures) {
        if let Some(card) = self.shelf.card_bounds(id) {
            let mut damage = Region::new();
            damage.add(card);
            self.paint(&damage, looks, pictures);
        }
    }

    /// The desktop's look or scale moved: lay the window out again.
    pub fn relook(&mut self, looks: (Scale, &Theme), pictures: &Pictures) {
        let mode = *self.pane.mode();
        self.shelf.set_size((mode.width_px, mode.height_px), looks);
        self.paint_whole(looks, pictures);
    }

    /// Take a delivered `event`, the desktop's double-click `interval`
    /// pairing presses: what it asks.
    pub fn event(
        &mut self,
        client: &mut Client,
        event: &WindowEvent,
        (looks, interval): ((Scale, &Theme), tairix_abi::time::Duration64),
        pictures: &Pictures,
    ) -> LibraryEvent {
        match *event {
            WindowEvent::CloseRequested { .. } => return LibraryEvent::Closed,
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } => {
                let item = match outcome {
                    MenuOutcome::Chosen(item) => Some(item),
                    _ => None,
                };
                return LibraryEvent::Menu {
                    open_id,
                    item,
                    kept: false,
                };
            }
            WindowEvent::MenuChosen { open_id, item, .. } => {
                return LibraryEvent::Menu {
                    open_id,
                    item: Some(item),
                    kept: true,
                };
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                let mode = app::mode_for(width_px, height_px);
                if self.pane.resize_with(client, &mode, &mut self.surface) == Fitted::Adopted {
                    self.relook(looks, pictures);
                }
                return LibraryEvent::Nothing;
            }
            WindowEvent::ContentReleased { .. } => {
                self.pane.release_frames();
                self.owed.owe_whole();
                return LibraryEvent::Nothing;
            }
            WindowEvent::RedrawRequested { .. } => {
                self.owed.owe_whole();
                return LibraryEvent::Nothing;
            }
            _ => {}
        }
        let at_ns = event_time(event);
        let mut damage = Region::new();
        let mut asked = None;
        let view = &mut self.shelf;
        inputs(event, |input| {
            asked = asked.or(view.on_input(input, (at_ns, interval), looks, &mut damage));
        });
        if !damage.is_empty() {
            self.paint(&damage, looks, pictures);
        }
        asked.map_or(LibraryEvent::Nothing, LibraryEvent::Request)
    }

    /// When the held scrollbar press next steps.
    pub fn deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.shelf.deadline(now_ns)
    }

    /// Step the held scrollbar press, once it is due.
    pub fn step(&mut self, now_ns: u64, looks: (Scale, &Theme), pictures: &Pictures) {
        let mut damage = Region::new();
        self.shelf.step(now_ns, looks, &mut damage);
        if !damage.is_empty() {
            self.paint(&damage, looks, pictures);
        }
    }

    /// Open `rows` as a menu at `at` in the window: the menu's id.
    ///
    /// # Errors
    ///
    /// The desktop's refusal.
    pub fn open_menu(&self, client: &mut Client, at: Point, rows: &AppMenu) -> Result<u64, Errno> {
        let anchor = WindowRegion::new(at.x, at.y, 0, 0)?;
        client.open_menu(self.pane.id(), anchor, rows)
    }

    /// Present what the window owes.
    ///
    /// # Errors
    ///
    /// The desktop's refusal of the present.
    pub fn present(&mut self, client: &mut Client) -> Result<(), Errno> {
        present(&mut self.pane, &self.surface, &mut self.owed, client)
    }

    /// Present what the window drew while both its frames were the desktop's.
    ///
    /// # Errors
    ///
    /// The desktop's refusal of the present.
    pub fn present_deferred(&mut self, client: &mut Client) -> Result<(), Errno> {
        self.pane.present_deferred(client, &self.surface)
    }

    /// Close the window.
    pub fn close(self, client: &mut Client) {
        let _ = self.pane.close(client);
    }

    fn paint(&mut self, damage: &Region, looks: (Scale, &Theme), pictures: &Pictures) {
        let view = &self.shelf;
        repaint(
            &self.pane,
            &mut self.surface,
            &mut self.owed,
            damage,
            |surface| {
                view.render(surface, looks, |id| pictures.get(id));
            },
        );
    }
}

/// What a dialog over a window holds: a form that answers once.
pub trait Sheet {
    /// What it answers.
    type Answer;

    /// The size it is drawn at, in physical pixels.
    fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32);

    /// Draw it.
    fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme);

    /// Take `event` at `at_ns`: its answer, once it has one, and what it
    /// redrew in `damage`.
    fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Self::Answer>;
}

impl Sheet for Assistant {
    type Answer = Answer;

    fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        Assistant::extent(self, scale, theme)
    }

    fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        Assistant::render(self, surface, scale, theme);
    }

    fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        Assistant::on_input(self, event, at_ns, looks, damage)
    }
}

/// What a delivered event did to a dialog.
pub enum SheetEvent<A> {
    /// Nothing further.
    Nothing,
    /// It was dismissed.
    Closed,
    /// It answered.
    Answered(A),
}

/// A dialog window: its pane, the form it shows and what it owes.
pub struct SheetWindow<S> {
    pane: WindowPane,
    surface: Surface,
    owed: Owed,
    sheet: S,
}

impl<S: Sheet> SheetWindow<S> {
    /// Open `sheet` titled `title` as an owner-modal dialog over window
    /// `owner`.
    ///
    /// # Errors
    ///
    /// Why it could not be opened, as the user is told.
    pub fn open(
        client: &mut Client,
        (owner, endpoint): (u64, u64),
        looks: (Scale, &Theme),
        sheet: S,
        title: &str,
    ) -> Result<Self, String> {
        let (scale, theme) = looks;
        let (width, height) = sheet.extent(scale, theme);
        let mode = app::mode_for(width, height);
        let surface = Surface::new(mode.width_px, mode.height_px)
            .ok_or_else(|| String::from("there is not enough memory to draw it"))?;
        let pane = WindowPane::open_dialog(
            client,
            owner,
            endpoint,
            &mode,
            DialogModality::OwnerModal,
            title,
        )
        .map_err(|err| format!("{err}"))?;
        let mut win = Self {
            pane,
            surface,
            owed: Owed::new(),
            sheet,
        };
        let damage = whole(&win.pane);
        win.paint(&damage, looks);
        Ok(win)
    }

    /// The window's id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.pane.id()
    }

    /// The form shown.
    pub const fn sheet(&self) -> &S {
        &self.sheet
    }

    /// Change the form shown through `change`, repainting what it redrew.
    pub fn change(&mut self, looks: (Scale, &Theme), change: impl FnOnce(&mut S, &mut Region)) {
        let mut damage = Region::new();
        change(&mut self.sheet, &mut damage);
        if !damage.is_empty() {
            self.paint(&damage, looks);
        }
    }

    /// Take a delivered `event`: what it did.
    pub fn event(&mut self, event: &WindowEvent, looks: (Scale, &Theme)) -> SheetEvent<S::Answer> {
        let mut answer = None;
        let mut damage = Region::new();
        match *event {
            WindowEvent::CloseRequested { .. } => return SheetEvent::Closed,
            WindowEvent::ContentReleased { .. } => {
                self.pane.release_frames();
                self.owed.owe_whole();
            }
            WindowEvent::RedrawRequested { .. } => self.owed.owe_whole(),
            _ => {
                let at_ns = event_time(event);
                let sheet = &mut self.sheet;
                inputs(event, |input| {
                    answer = answer
                        .take()
                        .or(sheet.on_input(input, at_ns, looks, &mut damage));
                });
            }
        }
        if !damage.is_empty() {
            self.paint(&damage, looks);
        }
        answer.map_or(SheetEvent::Nothing, SheetEvent::Answered)
    }

    /// Present what the dialog owes.
    ///
    /// # Errors
    ///
    /// The desktop's refusal of the present.
    pub fn present(&mut self, client: &mut Client) -> Result<(), Errno> {
        present(&mut self.pane, &self.surface, &mut self.owed, client)
    }

    /// Present what the dialog drew while both its frames were the desktop's.
    ///
    /// # Errors
    ///
    /// The desktop's refusal of the present.
    pub fn present_deferred(&mut self, client: &mut Client) -> Result<(), Errno> {
        self.pane.present_deferred(client, &self.surface)
    }

    /// The desktop's look or scale moved: the dialog sized again, and drawn
    /// whole where its size changed.
    pub fn relook(&mut self, client: &mut Client, looks: (Scale, &Theme)) {
        let (width, height) = self.sheet.extent(looks.0, looks.1);
        let mode = app::mode_for(width, height);
        if self.pane.resize_with(client, &mode, &mut self.surface) != Fitted::Kept {
            let damage = whole(&self.pane);
            self.paint(&damage, looks);
        }
    }

    /// Close the dialog.
    pub fn close(self, client: &mut Client) {
        let _ = self.pane.close(client);
    }

    fn paint(&mut self, damage: &Region, (scale, theme): (Scale, &Theme)) {
        let sheet = &self.sheet;
        repaint(
            &self.pane,
            &mut self.surface,
            &mut self.owed,
            damage,
            |surface| {
                sheet.render(surface, scale, theme);
            },
        );
    }
}
