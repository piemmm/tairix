//! A context's screen window: its picture scaled by the desktop into the
//! largest rectangle of the mode's pixel aspect the window holds, the
//! border's colour round it and the bar above it, and the raw keyboard and
//! the pointer given to the guest.

use alloc::format;
use alloc::string::String;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode};
use tairix_abi::input::PointerButtonCode;
use tairix_abi::window_ipc::{
    CursorShape, KeyboardMode, PointerAction, ViewFilter, WindowEvent, WindowRegion,
    WindowSizeState, WindowSizing, WindowView,
};
use tairix_geometry::{Region, Scale};
use tairix_machines::menu::Followed;
use tairix_raster::{Color, Surface};
use tairix_riscos_app::bar::{self, Bar, Command, Shown};
use tairix_riscos_app::input::{os_units, ButtonOrder, Held, Pointing};
use tairix_riscos_app::screen::Picture;
use tairix_riscos_app::settings::Scaling;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_session::protocol::{Cursor, Screen, Shape};
use tairix_theme::Theme;
use tairix_window::app::{self, Fitted, WindowPane};
use tairix_window::{pointer_input_events, pointer_point, Desktop};

use crate::front::{Client, APP_NAME};

/// Nanoseconds in a centisecond, the unit a cursor flashes in.
const CENTISECOND_NS: u64 = 10_000_000;

/// The least a screen window shrinks to, in logical pixels.
const LEAST: (u32, u32) = (160, 120);

/// What a screen window's event asks of its session.
pub enum Asked {
    /// A command from its menu.
    Command(Command),
    /// The window is to close.
    Close,
}

/// One context's screen window.
pub struct ScreenWindow {
    pane: WindowPane,
    /// The window's own picture: the bar, and the border round the screen.
    frame: Surface,
    picture: Picture,
    bar: Bar,
    held: Held,
    pointing: Pointing,
    /// Where the pointer last was over the picture, in OS units.
    place: (i32, i32),
    /// How it is shown, as its menu says.
    shown: Shown,
    /// The frame is to be painted whole.
    frame_owed: bool,
    /// The desktop's own pointer is hidden over the picture.
    hid_pointer: bool,
    /// The window size the desktop refused to show the picture at.
    refused_at: Option<(u32, u32)>,
    /// When a flashing cursor next turns over.
    flash_due: Option<u64>,
    menu: Option<Followed<()>>,
    title: String,
}

/// The window's size in logical pixels for `screen`: its OS units, the
/// finer axis a pixel to a desktop pixel.
fn logical_size(screen: &Screen) -> (u32, u32) {
    let finest = screen.x_eig.min(screen.y_eig);
    (
        (screen.width << screen.x_eig) >> finest,
        (screen.height << screen.y_eig) >> finest,
    )
}

impl ScreenWindow {
    /// The window of a context showing `screen`, titled `title`, its picture
    /// scaled `scaling`.
    ///
    /// # Errors
    ///
    /// Why the window or its picture could not be made.
    pub fn open(
        client: &mut Client,
        (endpoint, desktop, theme): (u64, &Desktop, &Theme),
        screen: Screen,
        title: &str,
        scaling: Scaling,
    ) -> Result<Self, String> {
        let picture = Picture::new(screen).ok_or("no memory for its screen")?;
        let scale = desktop.scale();
        let (width, height) = logical_size(&screen);
        let (width, height) = desktop.window_size(width, height);
        let height = height.saturating_add(Bar::height(scale, theme));
        let (least_width, least_height) = desktop.window_size(LEAST.0, LEAST.1);
        let sizing = WindowSizing::Resizable {
            min_width_px: least_width,
            min_height_px: least_height.saturating_add(Bar::height(scale, theme)),
            max_width_px: 0,
            max_height_px: 0,
        };
        let frame = Surface::new(width, height).ok_or("no memory for its window")?;
        let pane = WindowPane::open(
            client,
            endpoint,
            &app::mode_for(width, height),
            title,
            sizing,
        )
        .map_err(|err| format!("its window could not be opened ({err})"))?;
        if let Err(err) = client.set_keyboard_mode(pane.id(), KeyboardMode::Raw) {
            app::report(
                APP_NAME,
                format_args!("the screen takes no raw keys ({err})"),
            );
        }
        Ok(Self {
            pane,
            frame,
            picture,
            bar: Bar::default(),
            held: Held::default(),
            pointing: Pointing::default(),
            place: (0, 0),
            shown: Shown {
                fullscreen: false,
                scaling,
                paused: false,
            },
            frame_owed: true,
            hid_pointer: false,
            refused_at: None,
            flash_due: None,
            menu: None,
            title: String::from(title),
        })
    }

    /// The window's id.
    pub const fn id(&self) -> u64 {
        self.pane.id()
    }

    /// The window closed.
    pub fn close(self, client: &mut Client) {
        let _ = self.pane.close(client);
    }

    /// The screen's geometry is now `screen`.
    pub fn screen(&mut self, screen: Screen) {
        let border = screen.border != self.picture.screen().border;
        if !self.picture.reshape(screen) {
            app::report(APP_NAME, "no memory for a screen's new mode");
        }
        self.frame_owed |= border;
    }

    /// Lines from `first`, as a damage record holds them.
    pub fn damage(&mut self, first: u32, pixels: &[u8]) {
        self.picture.damage(first, pixels);
    }

    /// The text cursor is now `cursor`, lit afresh at `now`.
    pub fn cursor(&mut self, cursor: Option<Cursor>, now: u64) {
        self.picture.set_cursor(cursor);
        self.flash_due = self.flash_period().map(|period| now.saturating_add(period));
    }

    /// The pointer's shape is now `shape`.
    pub fn pointer(&mut self, client: &mut Client, shape: Option<&Shape<'_>>) {
        if !self.picture.set_pointer(shape) {
            app::report(APP_NAME, "no memory for a screen's pointer");
        }
        if !self.picture.shows_pointer() {
            self.show_desktop_pointer(client, true);
        }
    }

    /// The pointer's active point is at screen pixel `(x, y)`.
    pub fn pointer_at(&mut self, x: i32, y: i32) {
        self.picture.set_pointer_at(x, y);
    }

    /// The guest set the pointer or the mouse itself.
    pub fn pointer_set(&mut self) {
        self.pointing.guest_set();
    }

    /// Its context is paused, or let go on.
    pub const fn paused(&mut self, paused: bool) {
        self.shown.paused = paused;
    }

    /// The picture is scaled `scaling` from now on.
    pub const fn scale(&mut self, scaling: Scaling) {
        self.shown.scaling = scaling;
    }

    /// Full screen, or the window again.
    pub fn toggle_fullscreen(&mut self, client: &mut Client) {
        let state = if self.shown.fullscreen {
            WindowSizeState::Restored
        } else {
            WindowSizeState::Fullscreen
        };
        if let Err(err) = client.set_size_state(self.id(), state) {
            app::report(
                APP_NAME,
                format_args!("the screen could not change size ({err})"),
            );
        }
    }

    /// When the cursor next turns over.
    pub const fn due(&self) -> Option<u64> {
        self.flash_due
    }

    /// The clock is at `now`: a flashing cursor turned over if it is due.
    pub fn tick(&mut self, now: u64) {
        let (Some(due), Some(period)) = (self.flash_due, self.flash_period()) else {
            self.flash_due = None;
            return;
        };
        if now >= due {
            self.picture.flash();
            self.flash_due = Some(now.saturating_add(period));
        }
    }

    fn flash_period(&self) -> Option<u64> {
        let half = self.picture.cursor_flash()?.half_period_cs()?;
        Some(u64::from(half) * CENTISECOND_NS)
    }

    /// The rectangle the picture is shown in, below the bar.
    fn placement(&self, mode: &DisplayMode, bar: u32) -> Option<DamageRect> {
        let below = mode
            .height_px
            .checked_sub(bar)
            .filter(|&height| height > 0)?;
        let target = self.picture.placement((mode.width_px, below))?;
        Some(DamageRect {
            x: target.x,
            y: target.y + bar,
            width_px: target.width,
            height_px: target.height,
        })
    }

    /// The desktop refused the last resize: the picture is not asked for
    /// again at that size, and the user is told once.
    pub fn view_refused(&mut self) {
        let mode = self.pane.mode();
        let size = (mode.width_px, mode.height_px);
        if self.refused_at != Some(size) {
            self.refused_at = Some(size);
            app::report(
                APP_NAME,
                "the desktop would not show a screen at its window's size",
            );
        }
    }

    fn wanted_view(&self, mode: &DisplayMode, bar: u32) -> Option<WindowView> {
        if self.refused_at == Some((mode.width_px, mode.height_px)) {
            return None;
        }
        let screen = self.picture.screen();
        Some(WindowView {
            width_px: screen.width,
            height_px: screen.height,
            placement: self.placement(mode, bar)?,
            filter: match self.shown.scaling {
                Scaling::Sharp => ViewFilter::Sharp,
                Scaling::Smooth => ViewFilter::Smooth,
            },
        })
    }

    /// The window's frames shaped as `mode` carrying `view`, its frame
    /// fresh where its size changed.
    fn reshape(
        &mut self,
        client: &mut Client,
        mode: &DisplayMode,
        view: Option<WindowView>,
    ) -> Fitted {
        let resized = (mode.width_px, mode.height_px) != (self.frame.width(), self.frame.height());
        let fresh = if resized {
            match Surface::new(mode.width_px, mode.height_px) {
                Some(fresh) => Some(fresh),
                None => return Fitted::Kept,
            }
        } else {
            None
        };
        let fitted = self.pane.reshape(client, mode, view);
        if fitted == Fitted::Adopted {
            if let Some(fresh) = fresh {
                self.frame = fresh;
            }
            self.frame_owed = true;
            self.picture.owe_whole();
        }
        fitted
    }

    /// The window resized to `size`, full screen or not.
    fn resized(
        &mut self,
        client: &mut Client,
        (width, height): (u32, u32),
        state: WindowSizeState,
        looks: (Scale, &Theme),
    ) {
        self.shown.fullscreen = state.is_fullscreen();
        let mode = app::mode_for(width, height);
        let view = self.wanted_view(&mode, Bar::height(looks.0, looks.1));
        if self.reshape(client, &mode, view) == Fitted::Kept {
            app::report(APP_NAME, app::SIZE_KEPT);
        }
    }

    /// What changed painted and presented.
    pub fn paint(&mut self, client: &mut Client, (scale, theme): (Scale, &Theme)) {
        let bar = Bar::height(scale, theme);
        let mode = *self.pane.mode();
        let wanted = self.wanted_view(&mode, bar);
        if wanted.as_ref() != self.pane.view() {
            match self.reshape(client, &mode, wanted) {
                Fitted::Deferred => return,
                Fitted::Kept | Fitted::Adopted => {}
            }
        }
        let whole = DamageRect {
            x: 0,
            y: 0,
            width_px: mode.width_px,
            height_px: mode.height_px,
        };
        let frame = if self.frame_owed {
            self.frame_owed = false;
            let [_, r, g, b] = self.picture.screen().border.to_be_bytes();
            self.frame
                .fill_rect(0, 0, mode.width_px, mode.height_px, Color::rgb(r, g, b));
            self.bar
                .render(&mut self.frame, mode.width_px, scale, theme);
            DamageList::new(&[whole]).ok()
        } else {
            None
        };
        let view = self
            .pane
            .view_picture_mut()
            .and_then(|picture| self.picture.paint(picture));
        let presented = match (frame, view) {
            (frame, Some(view)) => {
                self.pane
                    .present_view(client, &self.frame, frame.as_ref(), &view)
            }
            (Some(frame), None) => self.pane.present_list(client, &self.frame, &frame),
            (None, None) => Ok(()),
        };
        if let Err(err) = presented {
            app::report(
                APP_NAME,
                format_args!("a screen could not be presented ({err})"),
            );
        }
    }

    /// Present what the pane held back for a frame to come back.
    pub fn present_deferred(&mut self, client: &mut Client) {
        if let Err(err) = self.pane.present_deferred(client, &self.frame) {
            app::report(
                APP_NAME,
                format_args!("a screen could not be presented ({err})"),
            );
        }
    }

    fn show_desktop_pointer(&mut self, client: &mut Client, show: bool) {
        if self.hid_pointer == show {
            let shape = if show {
                CursorShape::Arrow
            } else {
                CursorShape::None
            };
            if client.set_cursor(self.id(), shape).is_ok() {
                self.hid_pointer = !show;
            }
        }
    }

    fn open_menu(&mut self, client: &mut Client, anchor: tairix_geometry::Rect) {
        let Ok(rows) = bar::menu(&self.shown) else {
            return;
        };
        let below = anchor.origin.y + i32::try_from(anchor.height).unwrap_or(0);
        let opened = WindowRegion::new(anchor.origin.x, below, 0, 0)
            .and_then(|at| client.open_menu(self.id(), at, &rows));
        match opened {
            Ok(open) => self.menu = Some(Followed::new(open, (), rows)),
            Err(err) => app::report(
                APP_NAME,
                format_args!("the screen's menu could not open ({err})"),
            ),
        }
    }

    /// One event for the window: what it gives the guest through `send`,
    /// what it asks of its session.
    pub fn event(
        &mut self,
        client: &mut Client,
        event: &WindowEvent,
        (looks, order): ((Scale, &Theme), ButtonOrder),
        send: &mut impl FnMut(InputEvent),
    ) -> Option<Asked> {
        match *event {
            WindowEvent::CloseRequested { .. } => return Some(Asked::Close),
            WindowEvent::Resized {
                width_px,
                height_px,
                state,
                ..
            } => self.resized(client, (width_px, height_px), state, looks),
            WindowEvent::ContentReleased { .. } => self.pane.release_frames(),
            WindowEvent::Key { key, .. } => {
                if let Some(event) = self.held.key(key.physical(), key.is_press()) {
                    send(event);
                }
            }
            WindowEvent::Focus { focused: false, .. } => {
                let (keys, buttons) = self.held.release();
                keys.for_each(&mut *send);
                if buttons {
                    send(self.pointing.buttons(self.place, 0, false).0);
                }
            }
            WindowEvent::PointerLock { locked, .. } => {
                if let Some(release) = self.pointing.locked(locked, &mut self.held) {
                    send(release);
                }
                let title = if locked {
                    format!("{} \u{2014} tap Meta to release the mouse", self.title)
                } else {
                    self.title.clone()
                };
                let _ = client.set_title(self.id(), &title);
            }
            WindowEvent::PointerMotion { dx, dy, .. } => {
                if let Some(event) = self.pointing.moved((dx, dy), self.held.buttons()) {
                    send(event);
                }
            }
            WindowEvent::Pointer { x, y, action, .. } => {
                return self.pointer_event(client, (x, y), action, (looks, order), send);
            }
            WindowEvent::MenuChosen { open_id, item, .. } => {
                if self
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.open() == open_id)
                {
                    return bar::command(item).map(Asked::Command);
                }
            }
            WindowEvent::MenuClosed { open_id, .. }
                if self
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.open() == open_id) =>
            {
                self.menu = None;
            }
            _ => {}
        }
        None
    }

    fn pointer_event(
        &mut self,
        client: &mut Client,
        at: (u32, u32),
        action: PointerAction,
        (looks, order): ((Scale, &Theme), ButtonOrder),
        send: &mut impl FnMut(InputEvent),
    ) -> Option<Asked> {
        let (scale, theme) = looks;
        let mode = *self.pane.mode();
        let held = self.held.buttons() != 0;
        if Bar::covers(at, scale, theme) && !self.pointing.is_locked() && !held {
            self.show_desktop_pointer(client, true);
            let mut damage = Region::new();
            let mut anchor = None;
            for event in pointer_input_events(action, pointer_point(at.0, at.1)) {
                anchor = anchor.or(self.bar.on_input(&event, mode.width_px, looks, &mut damage));
            }
            if !damage.is_empty() {
                self.frame_owed = true;
            }
            if let Some(anchor) = anchor {
                self.open_menu(client, anchor);
            }
            return None;
        }
        if let Some(placed) = self.placement(&mode, Bar::height(scale, theme)) {
            let region = tairix_raster::resample::Region {
                x: placed.x,
                y: placed.y,
                width: placed.width_px,
                height: placed.height_px,
            };
            self.place = os_units(&self.picture.screen(), region, at);
        }
        self.show_desktop_pointer(client, !self.picture.shows_pointer());
        let edge = match action {
            PointerAction::Pressed(code) => Some((code, true)),
            PointerAction::Released(code) => Some((code, false)),
            PointerAction::Moved | PointerAction::Cancelled => None,
        };
        match edge {
            Some((code, down)) => {
                let buttons = self.held.button(order, code, down);
                let primary_up = code == PointerButtonCode::Primary && !down;
                let (event, ask) = self.pointing.buttons(self.place, buttons, primary_up);
                send(event);
                if ask {
                    if let Err(err) = client.lock_pointer(self.id(), true) {
                        app::report(
                            APP_NAME,
                            format_args!("the mouse could not be held ({err})"),
                        );
                    }
                }
            }
            None => {
                if let Some(event) = self.pointing.at(self.place, self.held.buttons()) {
                    send(event);
                }
            }
        }
        None
    }
}
