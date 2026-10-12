//! The Sound Effects pane (`plans/SOUND.md`): whether the desktop makes
//! sounds, the theme they come from, and each event's switch, level and
//! sound — the user's own choices, which their desktop session keeps and
//! hands the cue authority while it holds the seat.
//!
//! This application holds no authority over them and keeps no store of its
//! own: it asks the session, which decides, stores and answers. A file is
//! chosen in the session's own picker, so no path ever reaches this pane,
//! which is told a chosen file's name and nothing more.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::pinboard_ipc::{SoundFile, SoundReading};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::Errno;
use tairix_appconf::{bool_text, Registry};
use tairix_audio::volume::{level_at_permille, permille_of_level};
use tairix_controls::{
    Button, ButtonContent, ComboBox, ControlRole, ControlState, FieldAction, FieldControl,
    FieldGroup, FieldRow, Slider, Toggle,
};
use tairix_soundtheme::{layered, merge_pushed, SoundKey, SoundSettings};

/// What a reader may search for to reach the pane.
pub(crate) const EFFECTS_FACTS: [&str; 6] = [
    "Sound effects",
    "Desktop sounds",
    "Sound theme",
    "Alert sound",
    "Bell",
    "Notification sound",
];

/// What plays for an event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectFile {
    /// The theme's sound.
    Theme,
    /// The chosen file, by its name.
    Chosen(String),
    /// The chosen file, by its name, which the session could not hand over.
    Unavailable(String),
}

/// What the pane is built from: the user's choices as the session said they
/// are in effect, and the themes to choose among.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectsReading {
    /// The settings in effect.
    pub settings: SoundSettings,
    /// What plays for each event, in [`SoundEvent::ALL`] order.
    pub files: [EffectFile; SoundEvent::ALL.len()],
    /// The themes to choose among, in name order.
    pub themes: Vec<String>,
}

impl EffectsReading {
    /// The reading the session's `reading` and the `themes` it named make.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a document the sound registry refuses: a
    /// pane is never built from settings it would have to guess at.
    pub fn of(reading: &SoundReading<'_>, themes: Vec<String>) -> Result<Self, Errno> {
        let settings = merge_pushed(&SoundSettings::default(), reading.document)
            .map_err(|_| Errno::OutOfRange)?;
        let files = core::array::from_fn(|at| match reading.files[at] {
            SoundFile::Theme => EffectFile::Theme,
            SoundFile::Chosen(name) => EffectFile::Chosen(String::from(name)),
            SoundFile::Unavailable(name) => EffectFile::Unavailable(String::from(name)),
        });
        Ok(Self {
            settings,
            files,
            themes,
        })
    }

    /// The themes the theme row offers: those installed, and the one in
    /// effect though it is not, since it is what the settings say.
    fn offered_themes(&self) -> Vec<String> {
        let mut themes = self.themes.clone();
        let theme = self.settings.theme();
        if !themes.iter().any(|offered| offered == theme) {
            themes.push(String::from(theme));
        }
        themes
    }
}

/// Which setting of which event a row changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum EffectsRow {
    /// Whether the desktop makes any sound.
    Enabled,
    /// The theme sounds come from.
    Theme,
    /// Whether the event makes its sound.
    EventEnabled(SoundEvent),
    /// The event's level.
    EventGain(SoundEvent),
    /// Choose the file the event plays.
    Choose(SoundEvent),
    /// Have the event play its theme's sound again.
    UseTheme(SoundEvent),
}

/// What a row asks of the desktop session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectsAsk {
    /// Set `key` to what `line`, its settings-document line, spells.
    Apply {
        /// The setting.
        key: SoundKey,
        /// The line, as the sound registry renders it.
        line: String,
    },
    /// Have the user choose, in the session's picker, the file an event plays.
    Pick(SoundEvent),
    /// Have an event play its theme's sound again.
    UseTheme(SoundEvent),
}

/// One round trip to the desktop session about the user's sound choices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectsTrip {
    /// Lay this settings document over the user's choices.
    Apply(String),
    /// Have the user choose, in the session's picker, the file an event plays.
    Pick(SoundEvent),
    /// Have an event play its theme's sound again.
    UseTheme(SoundEvent),
}

/// An ask waiting its turn.
#[derive(Debug)]
enum Waiting {
    /// Each setting a change is waiting for, with its newest line.
    Settings(Vec<(SoundKey, String)>),
    /// A pick or a return to the theme's sound.
    Action(EffectsTrip),
}

/// The asks a window has waiting for the session: one round trip at a time,
/// every waiting setting's newest value carried together, so a level stepped
/// as fast as the reader likes costs one store a round trip, never one a
/// step, and the value asked last is the one that lands. Picks and returns to
/// a theme's sound keep the order they were asked in. It performs no I/O.
#[derive(Debug, Default)]
pub struct EffectsAsks {
    waiting: VecDeque<Waiting>,
    in_flight: bool,
}

impl EffectsAsks {
    /// Nothing waiting, nothing in flight.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            waiting: VecDeque::new(),
            in_flight: false,
        }
    }

    /// Queue `ask`: a setting's new value replaces any it waits with, and an
    /// action already waiting is not asked twice.
    pub fn ask(&mut self, ask: EffectsAsk) {
        let action = match ask {
            EffectsAsk::Apply { key, line } => {
                let settings = self.waiting.iter_mut().find_map(|waiting| match waiting {
                    Waiting::Settings(settings) => Some(settings),
                    Waiting::Action(_) => None,
                });
                match settings {
                    Some(settings) => match settings.iter_mut().find(|(held, _)| *held == key) {
                        Some((_, held)) => *held = line,
                        None => settings.push((key, line)),
                    },
                    None => self
                        .waiting
                        .push_back(Waiting::Settings(alloc::vec![(key, line)])),
                }
                return;
            }
            EffectsAsk::Pick(event) => EffectsTrip::Pick(event),
            EffectsAsk::UseTheme(event) => EffectsTrip::UseTheme(event),
        };
        let asked = self
            .waiting
            .iter()
            .any(|waiting| matches!(waiting, Waiting::Action(held) if *held == action));
        if !asked {
            self.waiting.push_back(Waiting::Action(action));
        }
    }

    /// The round trip to start, if one is waiting and none is in flight.
    pub fn next_trip(&mut self) -> Option<EffectsTrip> {
        if self.in_flight {
            return None;
        }
        let trip = match self.waiting.pop_front()? {
            Waiting::Settings(settings) => {
                EffectsTrip::Apply(settings.into_iter().map(|(_, line)| line).collect())
            }
            Waiting::Action(action) => action,
        };
        self.in_flight = true;
        Some(trip)
    }

    /// The round trip in flight has come back, or could not be made.
    pub fn landed(&mut self) {
        self.in_flight = false;
    }
}

/// What an event is called on the pane, for an event whose sound a user's
/// choices reach.
const fn label(event: SoundEvent) -> &'static str {
    match event {
        SoundEvent::Shutdown => "Shut down",
        SoundEvent::DeviceAttached => "Device connected",
        SoundEvent::DeviceDetached => "Device disconnected",
        SoundEvent::Notification => "Notification",
        SoundEvent::NotificationMessage => "Message",
        SoundEvent::NotificationReminder => "Reminder",
        SoundEvent::Error => "Error",
        SoundEvent::Warning => "Warning",
        SoundEvent::Info => "Information",
        SoundEvent::Bell => "Terminal bell",
        SoundEvent::Startup => "Start-up",
        SoundEvent::Login => "Sign in",
        SoundEvent::Logout => "Sign out",
    }
}

/// The pane's plates and the row each control changes. A plate's reading
/// rows come after its controls, and change nothing.
pub(crate) fn groups(reading: Option<&EffectsReading>) -> (Vec<FieldGroup>, Vec<Vec<EffectsRow>>) {
    let Some(reading) = reading else {
        return (
            alloc::vec![FieldGroup::new(
                "SOUND EFFECTS",
                alloc::vec![FieldRow::new(
                    "Sounds",
                    FieldControl::Unmeasured(String::from("not read yet")),
                )],
            )],
            alloc::vec![Vec::new()],
        );
    };
    let settings = &reading.settings;
    let themes = reading.offered_themes();
    let at = themes
        .iter()
        .position(|theme| theme == settings.theme())
        .unwrap_or(0);
    let mut groups = alloc::vec![FieldGroup::new(
        "SOUND EFFECTS",
        alloc::vec![
            FieldRow::new(
                "Play sounds",
                FieldControl::Toggle(Toggle::new("", settings.enabled())),
            ),
            FieldRow::new(
                "Theme",
                FieldControl::Combo(ComboBox::new(themes).with_selected(at)),
            ),
            FieldRow::new(
                "Machine sounds",
                FieldControl::Reading(String::from(
                    "start-up, sign-in and sign-out are the machine's",
                )),
            )
            .with_description(
                "Set by this machine's administrator: they sound while no one's \
                               choices are in force."
            ),
        ],
    )];
    let mut owners = alloc::vec![alloc::vec![EffectsRow::Enabled, EffectsRow::Theme]];
    for event in SoundEvent::ALL.into_iter().filter(|event| layered(*event)) {
        let (group, rows) = event_group(reading, event);
        groups.push(group);
        owners.push(rows);
    }
    (groups, owners)
}

/// One event's plate: its switch, its level, and the sound it plays.
fn event_group(reading: &EffectsReading, event: SoundEvent) -> (FieldGroup, Vec<EffectsRow>) {
    let chosen = reading.settings.event(event);
    let file = &reading.files[event.index()];
    let plays = match file {
        EffectFile::Theme => String::from("Plays the theme's sound."),
        EffectFile::Chosen(name) => format!("Plays {name}."),
        EffectFile::Unavailable(name) => {
            format!("{name} cannot be read, so the theme's sound plays.")
        }
    };
    let button = |label: &str| {
        FieldControl::Button(Button::new(
            ButtonContent::Label(String::from(label)),
            ControlRole::Neutral,
        ))
    };
    let own = !matches!(file, EffectFile::Theme);
    let rows = alloc::vec![
        FieldRow::new(
            "Play",
            FieldControl::Toggle(Toggle::new("", chosen.enabled))
        ),
        FieldRow::new(
            "Level",
            FieldControl::Slider(Slider::new(permille_of_level(chosen.gain))),
        )
        .with_description(format!("{}", chosen.gain)),
        FieldRow::new("Sound", button("Choose…")).with_description(plays),
        FieldRow::new("", button("Use Theme Sound"))
            .with_state(ControlState::idle().with_enabled(own)),
    ];
    (
        FieldGroup::new(label(event).to_uppercase(), rows),
        alloc::vec![
            EffectsRow::EventEnabled(event),
            EffectsRow::EventGain(event),
            EffectsRow::Choose(event),
            EffectsRow::UseTheme(event),
        ],
    )
}

/// What `row`'s `action` asks of the session over `reading`: `None` for
/// nothing durable — a level on its way, a list opening. A level acts once,
/// where it settles.
pub(crate) fn effects_ask(
    row: EffectsRow,
    action: &FieldAction,
    reading: &EffectsReading,
) -> Option<EffectsAsk> {
    let changed = |key: SoundKey, text: &str| {
        let mut settings = reading.settings.clone();
        settings.read(key, text).then(|| EffectsAsk::Apply {
            key,
            line: settings.document_of(&[key]).render(),
        })
    };
    match (row, action) {
        (EffectsRow::Enabled, FieldAction::Set { on }) => {
            changed(SoundKey::Enabled, bool_text(*on))
        }
        (EffectsRow::Theme, FieldAction::Selected { index }) => {
            let theme = reading.offered_themes().into_iter().nth(*index)?;
            changed(SoundKey::Theme, &theme)
        }
        (EffectsRow::EventEnabled(event), FieldAction::Set { on }) => {
            changed(SoundKey::EventEnabled(event), bool_text(*on))
        }
        (EffectsRow::EventGain(event), FieldAction::Settled { permille }) => changed(
            SoundKey::EventGain(event),
            &format!("{}", level_at_permille(*permille)),
        ),
        (EffectsRow::Choose(event), FieldAction::Activated) => Some(EffectsAsk::Pick(event)),
        (EffectsRow::UseTheme(event), FieldAction::Activated) => Some(EffectsAsk::UseTheme(event)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
