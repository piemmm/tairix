//! The desktop session's **trial of a new desktop resolution**.
//!
//! A new resolution is drawn at once and then put to the user in a prompt the
//! session owns, counting down. It stands only if they choose *Keep Changes*;
//! *Revert*, `Escape`, the countdown running out, and a trial the session had
//! to abandon all put the previous resolution back. The settings call that
//! asked for it is held throughout and answered with the outcome, so the
//! application that asked learns whether the change stood.

use alloc::format;
use alloc::string::String;

use tairix_abi::input::KeyInput;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::window_ipc::{AlertButtonRole, AlertSpec};
use tairix_log::{EventId, Field, FieldValue};
use tairix_theme::Theme;
use tairix_wallpaper::Resolution;
use tairix_wm::{Compositor, Point, Rect, Scale, WindowId};

use crate::choice::{question, Choice, ChoicePrompt};
use crate::shell::DesktopShell;
use crate::switchuser::park_within;

/// How long a new resolution stands unanswered before it is reverted.
pub const TRIAL_SECONDS: u64 = 15;

/// What a settings apply asks of the desktop's resolution.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ResolutionAsk {
    /// The size the stored setting draws is unchanged: the apply is no trial,
    /// whatever size happens to be drawn.
    Same,
    /// A new size, drawn and put to the user before it is kept.
    Trial((u32, u32)),
    /// A new size the user could not see to keep, the screen being covered by
    /// a lock or a screensaver.
    Covered,
}

/// What an apply naming `asked` asks of a desktop whose setting in force is
/// `in_force`, on an output of `output` pixels, while the screen is `covered`
/// or not.
#[must_use]
pub fn resolution_ask(
    asked: Resolution,
    in_force: Resolution,
    output: (u32, u32),
    covered: bool,
) -> ResolutionAsk {
    let size = asked.on(output);
    if size == in_force.on(output) {
        ResolutionAsk::Same
    } else if covered {
        ResolutionAsk::Covered
    } else {
        ResolutionAsk::Trial(size)
    }
}

/// One-shot per trial: the prompt is open and holding the keyboard, so a host
/// answering it waits on the prompt rather than racing the change that asked.
pub const TRIAL_SHOWN: EventId = EventId(20_023);

/// The exact message [`TRIAL_SHOWN`] is logged with.
pub const TRIAL_SHOWN_MESSAGE: &str = "resolution trial shown";

/// One-shot per resize: the frame on the display is the first to show the
/// desktop whole at its new size, the wallpaper fitted to it settled. Logged
/// with a [`ResizedRecord`].
pub const DESKTOP_RESIZED: EventId = EventId(20_024);

/// The exact message [`DESKTOP_RESIZED`] is logged with.
pub const DESKTOP_RESIZED_MESSAGE: &str = "desktop on screen at its new size";

/// A trial's call is answered: reverted at once, or kept once the store has
/// spoken. Logged with the [`TrialOutcome`].
pub const TRIAL_CONCLUDED: EventId = EventId(20_025);

/// The exact message [`TRIAL_CONCLUDED`] is logged with.
pub const TRIAL_CONCLUDED_MESSAGE: &str = "resolution trial concluded";

/// What [`DESKTOP_RESIZED`] carries: the screen as applications now see it.
/// The one spelling of its fields, so the session writing them and a host
/// reading them cannot disagree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ResizedRecord {
    /// The desktop's width in pixels.
    pub width: u32,
    /// The desktop's height in pixels.
    pub height: u32,
}

impl ResizedRecord {
    const WIDTH: &'static str = "width";
    const HEIGHT: &'static str = "height";

    /// The record as the fields it is logged with.
    #[must_use]
    pub fn fields(&self) -> [Field<'static>; 2] {
        [
            Field {
                key: Self::WIDTH,
                value: FieldValue::UnsignedInt(u64::from(self.width)),
            },
            Field {
                key: Self::HEIGHT,
                value: FieldValue::UnsignedInt(u64::from(self.height)),
            },
        ]
    }

    /// Read a record back through `field`, which answers the rendered value a
    /// line carries under a key; `None` if either is absent or malformed.
    pub fn read<'a>(field: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        Some(Self {
            width: field(Self::WIDTH)?.parse().ok()?,
            height: field(Self::HEIGHT)?.parse().ok()?,
        })
    }
}

/// How a trial's call was answered, as [`TRIAL_CONCLUDED`] records it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrialOutcome {
    /// Kept, and the store holds it.
    Kept,
    /// Kept, but the store refused it, so the resolution in force comes back.
    Refused,
    /// Reverted, by the user or the countdown, or abandoned.
    Reverted,
}

impl TrialOutcome {
    const FIELD: &'static str = "outcome";
    const ALL: [Self; 3] = [Self::Kept, Self::Refused, Self::Reverted];

    /// The outcome's logged spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Refused => "refused",
            Self::Reverted => "reverted",
        }
    }

    /// The outcome as the field it is logged with.
    #[must_use]
    pub const fn fields(self) -> [Field<'static>; 1] {
        [Field {
            key: Self::FIELD,
            value: FieldValue::Str(self.as_str()),
        }]
    }

    /// Read an outcome back through `field`, as [`ResizedRecord::read`] does;
    /// `None` if it is absent or not one of the spellings.
    pub fn read<'a>(field: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        let spelled = field(Self::FIELD)?;
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == spelled)
    }
}

/// The question the prompt asks.
const HEADLINE: &str = "Keep this resolution?";

/// A resolution on trial: the request that asked for it and the call to
/// answer. What it reverts to is the resolution the settings in force name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trial {
    /// The settings document the call carried. It is merged over the
    /// settings in force when the trial is kept, never over the ones in force
    /// when it began, so a change made meanwhile is not undone.
    pub request: String,
    /// The settings call held until the trial ends.
    pub ticket: u64,
}

/// How a trial ended. The prompt is already closed by the time one is taken
/// ([`ResolutionPrompt::take_ended`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrialEnd {
    /// Kept: the request is persisted, and the call answered once it is.
    Kept(Trial),
    /// Reverted, by the user or the countdown, or abandoned: the resolution
    /// in force is drawn again and the call is answered
    /// [`Declined`](tairix_abi::Errno::Declined).
    Reverted(Trial),
}

/// The showing prompt.
struct Active {
    trial: Trial,
    prompt: ChoicePrompt,
    due_ns: u64,
}

/// The session's resolution-trial slot: idle, or holding the one trial whose
/// prompt is up.
///
/// The input routers only answer the prompt; how a trial ended is held here
/// until the serve loop, which owns the settings and the screen, takes it.
#[derive(Default)]
pub struct ResolutionPrompt {
    active: Option<Active>,
    ended: Option<TrialEnd>,
}

impl ResolutionPrompt {
    /// An idle slot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active: None,
            ended: None,
        }
    }

    /// How the last trial ended, if it has and the end has not been taken.
    pub fn take_ended(&mut self) -> Option<TrialEnd> {
        self.ended.take()
    }

    /// The centre of the showing prompt's button at `index`, in its client.
    #[cfg(test)]
    pub(crate) fn button_centre(
        &self,
        index: usize,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<Point> {
        Some(
            self.active
                .as_ref()?
                .prompt
                .button_centre(index, shell, compositor),
        )
    }

    /// The compositor window of the showing prompt, if one is up; the
    /// embedder routes its key and click input here.
    #[must_use]
    pub fn wm_id(&self) -> Option<WindowId> {
        self.active.as_ref().map(|active| active.prompt.wm())
    }

    /// Whether a trial is in progress.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.active.is_some()
    }

    /// Put `trial` to the user at `now_ns`, the new resolution already drawn.
    ///
    /// # Errors
    ///
    /// The trial back, unasked, when a trial is already up or the prompt
    /// cannot be shown: the caller reverts it at once, because a resolution
    /// nobody is shown a way to keep or revert must not stand.
    pub fn ask(
        &mut self,
        trial: Trial,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Trial> {
        if self.active.is_some() {
            return Err(trial);
        }
        let due_ns = now_ns.saturating_add(TRIAL_SECONDS * u64::from(NANOS_PER_SEC));
        let Some(mut prompt) = build_spec(seconds_left(due_ns, now_ns))
            .and_then(|spec| ChoicePrompt::open(&spec, None, shell, compositor))
        else {
            return Err(trial);
        };
        prompt.stamp(now_ns);
        self.active = Some(Active {
            trial,
            prompt,
            due_ns,
        });
        Ok(())
    }

    /// `park_ns` shortened to when the countdown next moves.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        let Some(active) = &self.active else {
            return park_ns;
        };
        park_within(
            park_ns,
            Some(next_step(active.due_ns, now_ns).saturating_sub(now_ns)),
        )
    }

    /// Bring the countdown up to `now_ns`: the seconds left redrawn, or the
    /// trial reverted once none are. Answers whether it ended.
    pub fn tick(
        &mut self,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if now_ns >= active.due_ns {
            return self.conclude(shell, compositor, Choice::Declined);
        }
        let message = countdown(seconds_left(active.due_ns, now_ns));
        if active.prompt.detail() != message {
            active.prompt.set_detail(&message, shell, compositor);
        }
        false
    }

    /// Apply one key press at `now_ns`: `Escape` reverts, `Enter` keeps,
    /// `Space` presses the focused button, and `Tab` or an arrow moves the
    /// focus. Answers whether the trial ended.
    pub fn handle_key(
        &mut self,
        key: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let Some(choice) = self
            .active
            .as_mut()
            .and_then(|active| active.prompt.key(key, now_ns, shell, compositor))
        else {
            return false;
        };
        self.conclude(shell, compositor, choice)
    }

    /// Apply one primary-button press-and-release at the prompt-window-local
    /// position `local`; a press anywhere but a button leaves the question up.
    /// Answers whether the trial ended.
    pub fn handle_click(
        &mut self,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let Some(choice) = self
            .active
            .as_mut()
            .and_then(|active| active.prompt.click(local, now_ns, shell, compositor))
        else {
            return false;
        };
        self.conclude(shell, compositor, choice)
    }

    /// End the trial unanswered, reverting it: the session is stepping aside,
    /// locking, or going away, and a resolution nobody kept does not stand.
    /// Answers whether a trial was up.
    pub fn abandon(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) -> bool {
        self.conclude(shell, compositor, Choice::Declined)
    }

    /// Redraw the showing prompt, so a theme or scale change behind it draws
    /// it in what is now in use. A surface that cannot be built leaves the
    /// previous frame up.
    pub fn repaint(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            active.prompt.repaint(shell, compositor);
        }
    }

    /// Paint what input changed in the prompt since the last paint.
    pub fn paint_owed(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            active.prompt.paint_owed(shell, compositor);
        }
    }

    fn conclude(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        choice: Choice,
    ) -> bool {
        let Some(active) = self.active.take() else {
            return false;
        };
        active.prompt.close(shell, compositor);
        self.ended = Some(match choice {
            Choice::Accepted => TrialEnd::Kept(active.trial),
            Choice::Declined => TrialEnd::Reverted(active.trial),
        });
        true
    }
}

/// Whole seconds left before `due_ns`, counting a part second as one.
fn seconds_left(due_ns: u64, now_ns: u64) -> u64 {
    due_ns
        .saturating_sub(now_ns)
        .div_ceil(u64::from(NANOS_PER_SEC))
}

/// When the countdown shown at `now_ns` next drops a second.
fn next_step(due_ns: u64, now_ns: u64) -> u64 {
    let left = seconds_left(due_ns, now_ns);
    due_ns.saturating_sub(left.saturating_sub(1) * u64::from(NANOS_PER_SEC))
}

/// The countdown sentence for `seconds` left.
fn countdown(seconds: u64) -> String {
    let unit = if seconds == 1 { "second" } else { "seconds" };
    format!("The previous resolution comes back in {seconds} {unit}.")
}

/// Where the trial's prompt opens on `screen` at `scale` in `theme`, frame
/// included, at the countdown it opens with: what a check of the desktop's own
/// pixels must allow it to cover. `None` only for compiled-in text no alert
/// can carry.
#[must_use]
pub fn prompt_rect(screen: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let alert = tairix_controls::Alert::from_spec(&build_spec(TRIAL_SECONDS)?);
    let client = crate::alert::extent(&alert, screen, scale, theme);
    Some(crate::shell::alert_rect(screen, client, 0, (scale, theme)))
}

/// The prompt's alert at `seconds` left: *Revert* leading, *Keep Changes*
/// trailing and the default.
fn build_spec(seconds: u64) -> Option<AlertSpec> {
    question(
        HEADLINE,
        &countdown(seconds),
        "Revert",
        ("Keep Changes", AlertButtonRole::Default),
    )
}

#[cfg(test)]
#[path = "resolution_tests.rs"]
mod tests;
