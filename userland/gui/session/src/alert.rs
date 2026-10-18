//! The desktop's alert windows (`plans/APPWIN.md` AW8): every error,
//! warning, notice and question, whoever raised it, in a window of its own the
//! session draws, sized to what it says.
//!
//! [`AlertWindow`] is one alert on screen: its compositor window, the shared
//! [`Alert`] control drawn into it, and the moment its buttons arm. [`Alerts`]
//! holds the alerts applications raised, by the id the window server minted,
//! each holding the window it names through the shared [`Holds`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::window_ipc::{AlertSeverity, AlertSpec};
use tairix_controls::{damage, Alert, AlertAction};
use tairix_icon::IconKind;
use tairix_theme::Theme;
use tairix_wm::{
    Compositor, InputEvent, Key, Modifiers, NamedKey, Point, PointerButton, Rect, Region, Scale,
    Surface, WindowId,
};

use crate::about::{SystemFacts, ABOUT_TITLE};
use crate::hold::Holds;
use crate::shell::DesktopShell;

/// How long an alert's buttons — or a dialog window's content — ignore input
/// after it first appears: a click or a key meant for what was there a moment
/// ago must not answer it.
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

/// A dialog window's title: who it speaks for, then what its application
/// titled it — the attested name first, so a title too long for its band
/// elides itself rather than the name.
#[must_use]
pub fn dialog_title(speaker: Speaker<'_>, title: &str) -> String {
    let who = match speaker {
        Speaker::App(name) => name,
        Speaker::Desktop => DESKTOP,
    };
    let title = title.trim();
    if title.is_empty() {
        String::from(who)
    } else {
        format!("{who} \u{2014} {title}")
    }
}

/// Who an application's alert or dialog speaks for: its signed title `name`,
/// unless that is blank, the desktop's own word, or holds a character that
/// draws nothing yet can hide or reorder the text beside it — then `bundle`,
/// the path the kernel attests it was launched from, which no other program
/// shares.
#[must_use]
pub fn app_speaker<'a>(name: &'a str, bundle: &'a str) -> Speaker<'a> {
    let name = name.trim();
    if name.is_empty()
        || name.eq_ignore_ascii_case(DESKTOP)
        || name.chars().any(tairix_util::utf8::is_hidden)
    {
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
    /// Whether the buttons had armed when the press in hand began: a click
    /// begun before the alert could be read never answers it.
    pressed_armed: bool,
    /// The window that held the keyboard when the alert took it.
    returns_to: Option<WindowId>,
    /// What input changed since the alert was last painted.
    owed: Region,
}

impl AlertWindow {
    /// Put `alert` up in a window titled `title`, holding `owner` when it is
    /// some, set `cascade` alerts down from the centre, taking the keyboard
    /// when `focus`, and beneath the window holding it when `behind`. `None`
    /// when its surface or its window cannot be had.
    pub fn open(
        alert: Alert,
        title: &str,
        owner: Option<WindowId>,
        (cascade, focus, behind): (u32, bool, bool),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
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
            pressed_armed: false,
            returns_to: returns_to.filter(|_| took),
            owed: Region::new(),
        })
    }

    /// The alert's compositor window.
    #[must_use]
    pub const fn wm(&self) -> WindowId {
        self.wm
    }

    /// Bring the alert to the front with the keyboard, which goes back on
    /// close to the window that held it until now. Returns `false` for a
    /// window the compositor no longer knows.
    pub fn bring_forward(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) -> bool {
        let before = shell.router().focused();
        if !shell.focus_holder(compositor, self.wm) {
            return false;
        }
        if before != Some(self.wm) {
            self.returns_to = before;
        }
        true
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

    /// One pointer event at the window-local `local`: the index of the button
    /// a click completed on, if the click began after the buttons armed. The
    /// bar beside text that overflows takes the presses that land on it.
    pub fn pointer(
        &mut self,
        local: Point,
        event: &InputEvent,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<usize> {
        let bounds = self.bounds();
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let mut damage = damage::sink();
        if matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        ) {
            self.pressed_armed = self.armed(now_ns);
        }
        // Everything lands where the event says, whatever the last motion
        // the alert saw.
        if !matches!(event, InputEvent::PointerMoved { .. }) {
            let _ = self.alert.on_pointer(
                &InputEvent::PointerMoved { to: local },
                bounds,
                scale,
                theme,
                &mut damage,
            );
        }
        let chosen = match self
            .alert
            .on_pointer(event, bounds, scale, theme, &mut damage)
        {
            Some(AlertAction::Chosen { index }) if self.pressed_armed => Some(index),
            _ => None,
        };
        if chosen.is_none() {
            self.owe(&damage);
        }
        chosen
    }

    /// When the press its bar holds steps next, or `None` when nothing held
    /// would move.
    pub fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.alert.repeat_deadline(now_ns)
    }

    /// Step the press its bar holds if its repeat is due at `now_ns`, owing
    /// what moved.
    pub fn repeat(&mut self, now_ns: u64, shell: &DesktopShell, compositor: &Compositor) {
        let bounds = self.bounds();
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let mut damage = damage::sink();
        self.alert.repeat(now_ns, bounds, scale, theme, &mut damage);
        self.owe(&damage);
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
        let _ = shell.close_transient_window(compositor, self.wm, next);
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

/// The client extent `alert` opens at on `screen` at `scale` in `theme`: what
/// its content needs inside the dialog frame, and no larger than the screen
/// allows.
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
    /// The desktop's one *About This System* window, answered like a notice.
    About,
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
    /// Put up alert `id` saying `spec`, titled `title`, holding
    /// window-channel window `holds` — shown as `owner` — its hold taken in
    /// `held`. Returns whether it is up.
    pub fn open(
        &mut self,
        (id, holds, owner): (u64, Option<u64>, Option<WindowId>),
        (spec, title): (&AlertSpec, &str),
        (focus, behind): (bool, bool),
        held: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let cascade = u32::try_from(self.up.len()).unwrap_or(u32::MAX);
        match AlertWindow::open(
            Alert::from_spec(spec),
            title,
            owner,
            (cascade, focus, behind),
            shell,
            compositor,
        ) {
            Some(window) => {
                if let Some(owner) = owner {
                    held.take(owner, window.wm());
                }
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
        holds: &mut Holds,
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
                self.take_down(oldest, holds, shell, compositor);
            }
        }
        self.open_own(
            Alert::from_spec(spec),
            title,
            Origin::Desktop,
            shell,
            compositor,
        )
    }

    /// Put up the *About This System* window stating `facts`, or bring the
    /// one already up to the front: asking again shows the same window, never
    /// a second. It answers the user's own gesture, so it takes the keyboard.
    /// Returns whether it is up.
    pub fn about(
        &mut self,
        facts: &SystemFacts,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if let Some(up) = self
            .up
            .iter_mut()
            .find(|alert| alert.origin == Origin::About)
        {
            return up.window.bring_forward(shell, compositor);
        }
        let Some(spec) = facts.spec() else {
            return false;
        };
        let alert = Alert::from_spec(&spec).with_picture(IconKind::About);
        self.open_own(alert, ABOUT_TITLE, Origin::About, shell, compositor)
    }

    /// Put up `alert`, one of the desktop's own speaking as `origin`, titled
    /// `title`, holding no window and taking the keyboard.
    fn open_own(
        &mut self,
        alert: Alert,
        title: &str,
        origin: Origin,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let cascade = u32::try_from(self.up.len()).unwrap_or(u32::MAX);
        match AlertWindow::open(
            alert,
            title,
            None,
            (cascade, true, false),
            shell,
            compositor,
        ) {
            Some(window) => {
                self.up.push(AppAlert { origin, window });
                true
            }
            None => false,
        }
    }

    /// Take alert `id` down without an answer: the window server has
    /// answered it already, or its application is gone.
    pub fn withdraw(
        &mut self,
        id: u64,
        holds: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let index = self.up.iter().position(
            |alert| matches!(alert.origin, Origin::Client { id: minted, .. } if minted == id),
        );
        if let Some(index) = index {
            self.take_down(index, holds, shell, compositor);
        }
    }

    /// Whether compositor window `wm` is one of these alerts.
    #[must_use]
    pub fn owns(&self, wm: WindowId) -> bool {
        self.up.iter().any(|alert| alert.window.wm() == wm)
    }

    /// Start the arm delay of every alert put up since the last wake.
    pub fn stamp(&mut self, now_ns: u64) {
        for alert in &mut self.up {
            alert.window.stamp(now_ns);
        }
    }

    /// One pointer event on alert window `wm` at its window-local `local`,
    /// and the answer a click completing on a button gave, the alert then
    /// taken down.
    #[allow(clippy::too_many_arguments)] // The event, its place and time, and the seat it lands in.
    pub fn pointer(
        &mut self,
        wm: WindowId,
        local: Point,
        event: &InputEvent,
        now_ns: u64,
        holds: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let index = self.position(wm)?;
        let chosen = self.up[index]
            .window
            .pointer(local, event, now_ns, shell, compositor)?;
        self.answered(index, chosen, holds, shell, compositor)
    }

    /// When a press any alert's bar holds steps next, or `None` when nothing
    /// held would move.
    pub fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.up
            .iter_mut()
            .filter_map(|alert| alert.window.repeat_deadline(now_ns))
            .min()
    }

    /// Step every alert bar's held press whose repeat is due at `now_ns`.
    pub fn repeat(&mut self, now_ns: u64, shell: &DesktopShell, compositor: &Compositor) {
        for alert in &mut self.up {
            alert.window.repeat(now_ns, shell, compositor);
        }
    }

    /// A key on focused alert window `wm`, and the answer it gave, the alert
    /// then taken down.
    pub fn key(
        &mut self,
        wm: WindowId,
        key: &KeyInput,
        now_ns: u64,
        holds: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let index = self.position(wm)?;
        let chosen = self.up[index].window.key(key, now_ns, shell, compositor)?;
        self.answered(index, chosen, holds, shell, compositor)
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

    /// Take alert `index` down, the keyboard going to the front of the
    /// window it held — the holder behind it, else that window — or, for an
    /// alert that held none, to the front of the window it took the keyboard
    /// from.
    fn take_down(
        &mut self,
        index: usize,
        holds: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let closing = self.up.remove(index).window;
        let next = match closing.owner {
            Some(_) => holds.release(closing.wm),
            None => closing.returns_to.map(|window| holds.front(window)),
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
        holds: &mut Holds,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let chosen = u8::try_from(chosen).ok()?;
        let origin = self.up[index].origin;
        self.take_down(index, holds, shell, compositor);
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
