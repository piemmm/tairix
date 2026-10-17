//! The desktop's alert windows (`plans/APPWIN.md` AW8): every error,
//! warning, notice and question, whoever raised it, in a 4:3 window of its own
//! the session draws.
//!
//! [`AlertWindow`] is one alert on screen: its compositor window, the shared
//! [`Alert`] control drawn into it, and the moment its buttons arm. [`Alerts`]
//! holds the alerts applications raised, by the id the window server minted,
//! and which window each one holds modal.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::window_ipc::{AlertSeverity, AlertSpec, PointerAction, WindowEvent};
use tairix_controls::{damage, Alert, AlertAction};
use tairix_theme::Theme;
use tairix_wm::{
    Compositor, InputEvent, Key, Modifiers, NamedKey, Point, PointerButton, Rect, Region, Scale,
    Surface, WindowId,
};

use crate::shell::DesktopShell;

/// How long an alert's buttons ignore input after it first appears: a click
/// or a key meant for what was there a moment ago must not answer it.
pub const ALERT_ARM_NS: u64 = 350 * tairix_abi::time::NANOS_PER_MILLI;

/// The most notices of its own the desktop keeps up at once: enough that a
/// second refusal never hides the first, few enough to stay readable.
pub const DESKTOP_NOTICES_MAX: usize = 4;

/// How far each further alert is set down and across from the last, in
/// logical pixels, so one never hides another exactly.
const CASCADE_STEP: u32 = 24;

/// The word an alert's title names the desktop by, which no application
/// speaks by.
const DESKTOP: &str = "Desktop";

/// Who an alert speaks for, which its title names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Speaker<'a> {
    /// An application, by its attested name.
    App(&'a str),
    /// The desktop itself.
    Desktop,
}

/// An alert window's title: what kind of thing it says and who says it, as
/// RISC OS titles its error boxes.
#[must_use]
pub fn alert_title(severity: AlertSeverity, speaker: Speaker<'_>) -> String {
    let kind = match severity {
        AlertSeverity::Error => "Error",
        AlertSeverity::Warning => "Warning",
        AlertSeverity::Information | AlertSeverity::Question => "Message",
    };
    let who = match speaker {
        Speaker::App(name) => name,
        Speaker::Desktop => DESKTOP,
    };
    format!("{kind} from {who}")
}

/// Who an application's alert speaks for: its signed title `name`, unless that
/// is blank or the desktop's own word — then `bundle`, the path the kernel
/// attests it was launched from, which no other program shares.
#[must_use]
pub fn app_speaker<'a>(name: &'a str, bundle: &'a str) -> Speaker<'a> {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(DESKTOP) {
        Speaker::App(bundle)
    } else {
        Speaker::App(name)
    }
}

/// One alert on screen.
pub struct AlertWindow {
    wm: WindowId,
    owner: Option<WindowId>,
    alert: Alert,
    /// The severity mark at the scale it was last drawn at.
    mark: Option<(Scale, Surface)>,
    /// The client extent it was last drawn at, which input is read against.
    extent: (u32, u32),
    /// The density and screen that extent was fitted to.
    fitted: (Scale, Rect),
    /// How many alerts down from the centre it was set.
    cascade: u32,
    /// When its buttons arm; `None` until the serve loop first sees it.
    armed_at: Option<u64>,
    /// The window that held the keyboard when the alert took it.
    returns_to: Option<WindowId>,
    /// What input changed since the alert was last painted.
    owed: Region,
}

impl AlertWindow {
    /// Put `spec` up in a window titled `title`, holding `owner` when it is
    /// some, set `cascade` alerts down from the centre, taking the keyboard
    /// when `focus`, and beneath the window holding it when `behind`. `None`
    /// when its surface or its window cannot be had.
    pub fn open(
        spec: &AlertSpec,
        title: &str,
        owner: Option<WindowId>,
        (cascade, focus, behind): (u32, bool, bool),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
        let alert = Alert::from_spec(spec);
        let extent = extent(
            &alert,
            compositor.screen_rect(),
            compositor.scale(),
            compositor.theme(),
        );
        let scale = compositor.scale();
        let mark = alert.mark(scale).map(|mark| (scale, mark));
        let surface = draw(&alert, extent, mark.as_ref(), shell, compositor)?;
        let step = cascade_step(scale, cascade);
        let returns_to = shell.router().focused();
        let wm =
            shell.open_alert_window(compositor, owner, surface, title, (step, focus, behind))?;
        let took = shell.router().focused() == Some(wm);
        Some(Self {
            wm,
            owner,
            alert,
            mark,
            extent,
            fitted: (scale, compositor.screen_rect()),
            cascade,
            armed_at: None,
            returns_to: returns_to.filter(|_| took),
            owed: Region::new(),
        })
    }

    /// The alert's compositor window.
    #[must_use]
    pub const fn wm(&self) -> WindowId {
        self.wm
    }

    /// Where its buttons sit in its client, in spec order.
    #[must_use]
    pub fn button_rects(&self, shell: &DesktopShell, compositor: &Compositor) -> Vec<Rect> {
        self.alert.button_rects(
            self.bounds(),
            compositor.scale(),
            shell.session().active_theme(),
        )
    }

    /// What the alert says beneath its headline.
    #[must_use]
    pub fn detail(&self) -> &str {
        self.alert.detail()
    }

    /// Say `detail` instead, redrawn at once.
    pub fn set_detail(&mut self, detail: &str, shell: &DesktopShell, compositor: &mut Compositor) {
        self.alert.set_detail(detail);
        self.repaint(shell, compositor);
    }

    /// Start the arm delay at `now_ns` if it has not started.
    pub fn stamp(&mut self, now_ns: u64) {
        if self.armed_at.is_none() {
            self.armed_at = Some(now_ns.saturating_add(ALERT_ARM_NS));
        }
    }

    /// Whether the buttons answer input at `now_ns`.
    fn armed(&self, now_ns: u64) -> bool {
        self.armed_at.is_some_and(|at| now_ns >= at)
    }

    /// A primary click at the window-local `local`: the index of the button
    /// it chose, if it landed on one after the buttons armed.
    pub fn click(
        &mut self,
        local: Point,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<usize> {
        if !self.armed(now_ns) {
            return None;
        }
        let bounds = self.bounds();
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let mut damage = damage::sink();
        let mut chosen = None;
        for event in [
            InputEvent::PointerMoved { to: local },
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
        ] {
            if let Some(AlertAction::Chosen { index }) =
                self.alert
                    .on_pointer(&event, bounds, scale, theme, &mut damage)
            {
                chosen = Some(index);
            }
        }
        if chosen.is_none() {
            self.owe(&damage);
        }
        chosen
    }

    /// A wheel turn over the alert, which scrolls a detail that overflows.
    pub fn scroll(&mut self, dy: i32, shell: &DesktopShell, compositor: &Compositor) {
        let bounds = self.bounds();
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let mut damage = damage::sink();
        let _ = self.alert.on_pointer(
            &InputEvent::PointerScrolled { dx: 0, dy },
            bounds,
            scale,
            theme,
            &mut damage,
        );
        self.owe(&damage);
    }

    /// One key press: the index of the button it chose, if any. A key that
    /// would answer before the buttons arm answers nothing; one that only
    /// moves the focus or scrolls always acts.
    pub fn key(
        &mut self,
        key: &KeyInput,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<usize> {
        let KeyInput::Pressed { key, modifiers, .. } = key else {
            return None;
        };
        let key = match key {
            KeyValue::Named(named) => Key::Named(match named {
                NamedKeyCode::Enter => NamedKey::Enter,
                NamedKeyCode::Escape => NamedKey::Escape,
                NamedKeyCode::Tab => NamedKey::Tab,
                NamedKeyCode::Left => NamedKey::Left,
                NamedKeyCode::Right => NamedKey::Right,
                NamedKeyCode::Up => NamedKey::Up,
                NamedKeyCode::Down => NamedKey::Down,
                NamedKeyCode::PageUp => NamedKey::PageUp,
                NamedKeyCode::PageDown => NamedKey::PageDown,
                NamedKeyCode::Home => NamedKey::Home,
                NamedKeyCode::End => NamedKey::End,
                _ => return None,
            }),
            KeyValue::Char(' ') => Key::Char(' '),
            KeyValue::Char(_) => return None,
        };
        let modifiers = Modifiers {
            shift: modifiers.shift,
            ..Modifiers::default()
        };
        let bounds = self.bounds();
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let mut damage = damage::sink();
        let action = self
            .alert
            .on_key(key, modifiers, bounds, scale, theme, &mut damage);
        self.owe(&damage);
        match action {
            Some(AlertAction::Chosen { index }) if self.armed(now_ns) => Some(index),
            _ => None,
        }
    }

    /// Redraw the alert in the look and at the scale now in use.
    ///
    /// A change of look keeps the extent and the place it has, so the buttons
    /// stay under a reader about to press one. A change of density or of
    /// screen fits it again and centres it on the screen now there, as it
    /// opened: everything it drew has moved anyway.
    pub fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        let mode = (compositor.scale(), compositor.screen_rect());
        let refit = mode != self.fitted;
        if refit {
            self.extent = extent(
                &self.alert,
                compositor.screen_rect(),
                compositor.scale(),
                compositor.theme(),
            );
            self.fitted = mode;
        }
        if let Some(surface) = self.render(shell, compositor) {
            if compositor.set_surface(self.wm, surface) {
                self.owed.clear();
            }
        }
        if refit {
            if let Some(bounds) = compositor.window(self.wm).map(tairix_wm::Window::bounds) {
                let step = tairix_geometry::to_i32(cascade_step(mode.0, self.cascade));
                let centred = crate::shell::prompt_origin(mode.1, (bounds.width, bounds.height));
                let at = Rect::new(
                    centred.x.saturating_add(step),
                    centred.y.saturating_add(step),
                    bounds.width,
                    bounds.height,
                )
                .clamped_onto(mode.1)
                .origin;
                let _ = compositor.move_window(self.wm, at);
            }
        }
    }

    /// Paint what input changed since the last paint, into the window's kept
    /// frame and only there.
    pub fn paint_owed(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        if self.owed.is_empty() {
            return;
        }
        let (bounds, scale) = (self.bounds(), compositor.scale());
        let theme = shell.session().active_theme();
        let (alert, mark) = (&self.alert, self.mark.as_ref().map(|(_, mark)| mark));
        if compositor.repaint_window(self.wm, self.extent, &self.owed, |frame, rects| {
            damage::paint_parts(frame, rects, |frame| {
                alert.render(frame, bounds, scale, theme, mark);
            });
        }) {
            self.owed.clear();
        }
    }

    /// Where the keyboard goes when the alert comes down by itself: the window
    /// it holds, else the one it took the keyboard from.
    #[must_use]
    pub const fn hand_back(&self) -> Option<WindowId> {
        match self.owner {
            Some(owner) => Some(owner),
            None => self.returns_to,
        }
    }

    /// Take the alert down, handing the keyboard to `next` if it held it.
    pub fn close(
        self,
        next: Option<WindowId>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let _ = shell.close_alert_window(compositor, self.wm, next);
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.extent.0, self.extent.1)
    }

    fn owe(&mut self, damage: &Region) {
        for &rect in damage.rects() {
            self.owed.add(rect);
        }
    }

    /// The client surface, drawn whole. The mark is drawn once per scale.
    fn render(&mut self, shell: &DesktopShell, compositor: &Compositor) -> Option<Surface> {
        let scale = compositor.scale();
        if self.mark.as_ref().is_none_or(|(at, _)| *at != scale) {
            self.mark = self.alert.mark(scale).map(|mark| (scale, mark));
        }
        draw(
            &self.alert,
            self.extent,
            self.mark.as_ref(),
            shell,
            compositor,
        )
    }
}

/// `alert` drawn whole at the client `extent`, with `mark` its severity mark.
fn draw(
    alert: &Alert,
    (width, height): (u32, u32),
    mark: Option<&(Scale, Surface)>,
    shell: &DesktopShell,
    compositor: &Compositor,
) -> Option<Surface> {
    let mut surface = Surface::new(width, height)?;
    alert.render(
        &mut surface,
        Rect::new(0, 0, width, height),
        compositor.scale(),
        shell.session().active_theme(),
        mark.map(|(_, mark)| mark),
    );
    Some(surface)
}

/// How far down and across from the centre the `cascade`th alert is set.
fn cascade_step(scale: Scale, cascade: u32) -> u32 {
    scale.scale_length(CASCADE_STEP).saturating_mul(cascade)
}

/// The client extent `alert` opens at on `screen` at `scale` in `theme`: 4:3
/// with the dialog frame around it, and no larger than the screen allows.
pub(crate) fn extent(alert: &Alert, screen: Rect, scale: Scale, theme: &Theme) -> (u32, u32) {
    let frame = tairix_controls::WindowFrame::dialog();
    let probe = frame.outer_for_client(Rect::new(0, 0, 0, 0), scale, theme);
    alert.client_extent(
        (probe.width, probe.height),
        (screen.width, screen.height),
        scale,
        theme,
    )
}

/// Whether `event` still reaches a window an alert holds: nothing that
/// starts anything does — a press, a motion, a key, a scroll, a pinch — but a
/// release does, so a press the window saw before the alert held it is not
/// left down when the alert is answered.
#[must_use]
pub const fn reaches_held_window(event: &WindowEvent) -> bool {
    match event {
        WindowEvent::Pointer {
            action: PointerAction::Released(_),
            ..
        }
        | WindowEvent::Key {
            key: KeyInput::Released { .. },
            ..
        } => true,
        WindowEvent::Pointer { .. }
        | WindowEvent::Key { .. }
        | WindowEvent::Scrolled { .. }
        | WindowEvent::Pinch { .. } => false,
        _ => true,
    }
}

/// One alert on screen, and who raised it.
struct AppAlert {
    origin: Origin,
    window: AlertWindow,
}

/// Who raised an alert, which decides where its answer goes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Origin {
    /// An application, by the id the window server minted, holding the
    /// window-channel window it names, if any. Its answer is relayed.
    Client { id: u64, holds: Option<u64> },
    /// The desktop itself, stating a refusal: answered by its one button and
    /// relayed to no one.
    Desktop,
}

/// The alerts up — each application's, by the id the window server minted,
/// and the desktop's own notices — in the order they went up: the newest
/// holding a window is the one in front.
///
/// Bounded: the server admits at most
/// [`ALERT_MAX_OPEN`](tairix_abi::window_ipc::ALERT_MAX_OPEN) per
/// application, and the desktop at most [`DESKTOP_NOTICES_MAX`] notices, so a
/// scan is the right structure for so few.
#[derive(Default)]
pub struct Alerts {
    up: Vec<AppAlert>,
}

/// An answer the user gave to one application's alert, for the serve loop to
/// relay.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    /// The alert answered.
    pub alert_id: u64,
    /// The button chosen.
    pub index: u8,
}

impl Alerts {
    /// Put up alert `id`, holding window-channel window `holds` — shown as
    /// `owner` — titled `title`. Returns whether it is up.
    pub fn open(
        &mut self,
        (id, holds, owner): (u64, Option<u64>, Option<WindowId>),
        spec: &AlertSpec,
        title: &str,
        (focus, behind): (bool, bool),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let cascade = u32::try_from(self.up.len()).unwrap_or(u32::MAX);
        match AlertWindow::open(
            spec,
            title,
            owner,
            (cascade, focus, behind),
            shell,
            compositor,
        ) {
            Some(window) => {
                self.up.push(AppAlert {
                    origin: Origin::Client { id, holds },
                    window,
                });
                true
            }
            None => false,
        }
    }

    /// Put up a notice the desktop states itself, titled `title` and holding
    /// no window. It answers the user's own gesture, so it takes the keyboard;
    /// one answered is taken down and relayed to no application. Returns
    /// whether it is up: past [`DESKTOP_NOTICES_MAX`] the oldest notice gives
    /// way, so a run of refused gestures cannot pile alerts up without end.
    pub fn notify(
        &mut self,
        spec: &AlertSpec,
        title: &str,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let notices = self
            .up
            .iter()
            .filter(|alert| alert.origin == Origin::Desktop)
            .count();
        if notices >= DESKTOP_NOTICES_MAX {
            if let Some(oldest) = self
                .up
                .iter()
                .position(|alert| alert.origin == Origin::Desktop)
            {
                self.take_down(oldest, shell, compositor);
            }
        }
        let cascade = u32::try_from(self.up.len()).unwrap_or(u32::MAX);
        match AlertWindow::open(spec, title, None, (cascade, true, false), shell, compositor) {
            Some(window) => {
                self.up.push(AppAlert {
                    origin: Origin::Desktop,
                    window,
                });
                true
            }
            None => false,
        }
    }

    /// Take alert `id` down without an answer: the window server has
    /// answered it already, or its application is gone.
    pub fn withdraw(&mut self, id: u64, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let index = self.up.iter().position(
            |alert| matches!(alert.origin, Origin::Client { id: minted, .. } if minted == id),
        );
        if let Some(index) = index {
            self.take_down(index, shell, compositor);
        }
    }

    /// Whether compositor window `wm` is one of these alerts.
    #[must_use]
    pub fn owns(&self, wm: WindowId) -> bool {
        self.up.iter().any(|alert| alert.window.wm() == wm)
    }

    /// The alert holding window-channel window `window`, as its compositor
    /// window: the newest, which is the one in front.
    #[must_use]
    pub fn holding(&self, window: u64) -> Option<WindowId> {
        self.up
            .iter()
            .rev()
            .find(|alert| {
                matches!(alert.origin, Origin::Client { holds: Some(held), .. } if held == window)
            })
            .map(|alert| alert.window.wm())
    }

    /// Start the arm delay of every alert put up since the last wake.
    pub fn stamp(&mut self, now_ns: u64) {
        for alert in &mut self.up {
            alert.window.stamp(now_ns);
        }
    }

    /// A click on alert window `wm` at its window-local `local`, and the
    /// answer it gave, the alert then taken down.
    pub fn click(
        &mut self,
        wm: WindowId,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let index = self.position(wm)?;
        let chosen = self.up[index]
            .window
            .click(local, now_ns, shell, compositor)?;
        self.answered(index, chosen, shell, compositor)
    }

    /// A key on focused alert window `wm`, and the answer it gave, the alert
    /// then taken down.
    pub fn key(
        &mut self,
        wm: WindowId,
        key: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let index = self.position(wm)?;
        let chosen = self.up[index].window.key(key, now_ns, shell, compositor)?;
        self.answered(index, chosen, shell, compositor)
    }

    /// A wheel turn over alert window `wm`.
    pub fn scroll(&mut self, wm: WindowId, dy: i32, shell: &DesktopShell, compositor: &Compositor) {
        if let Some(index) = self.position(wm) {
            self.up[index].window.scroll(dy, shell, compositor);
        }
    }

    /// Redraw every alert in the look and at the scale now in use.
    pub fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        for alert in &mut self.up {
            alert.window.repaint(shell, compositor);
        }
    }

    /// Paint what input changed in each alert since the last paint: the
    /// serve loop's one paint after a drained run of input.
    pub fn paint_owed(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        for alert in &mut self.up {
            alert.window.paint_owed(shell, compositor);
        }
    }

    /// Take alert `index` down, the keyboard going to the alert behind it
    /// holding the same window, else that window, else — for an alert that
    /// held none — the window it took the keyboard from, or the front alert
    /// holding that one.
    fn take_down(&mut self, index: usize, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let closing = self.up.remove(index).window;
        let front_holding = |window: WindowId| {
            self.up
                .iter()
                .rev()
                .find(|alert| alert.window.owner == Some(window))
                .map(|alert| alert.window.wm)
        };
        let next = match closing.owner {
            Some(owner) => front_holding(owner).or(Some(owner)),
            None => closing
                .returns_to
                .map(|window| front_holding(window).unwrap_or(window)),
        };
        closing.close(next, shell, compositor);
    }

    fn position(&self, wm: WindowId) -> Option<usize> {
        self.up.iter().position(|alert| alert.window.wm() == wm)
    }

    /// Take alert `index` down, answered with button `chosen`: the answer an
    /// application is owed, or nothing for the desktop's own notice.
    fn answered(
        &mut self,
        index: usize,
        chosen: usize,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let chosen = u8::try_from(chosen).ok()?;
        let origin = self.up[index].origin;
        self.take_down(index, shell, compositor);
        let Origin::Client { id, .. } = origin else {
            return None;
        };
        Some(Answer {
            alert_id: id,
            index: chosen,
        })
    }
}

#[cfg(test)]
#[path = "alert_tests.rs"]
mod tests;
