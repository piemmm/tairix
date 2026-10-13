//! The Sound Effects pane (`plans/SOUND.md`): whether the desktop makes
//! sounds, the theme they come from, how loud they are, and what each event
//! plays — the user's own choices, which their desktop session keeps and
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
    ComboBox, ControlState, FieldAction, FieldControl, FieldGroup, FieldRow, Slider, Toggle,
};
use tairix_soundtheme::{layered, merge_pushed, SoundKey, SoundSettings};

/// What a reader may search for to reach the pane.
pub(crate) const EFFECTS_FACTS: [&str; 7] = [
    "Sound effects",
    "Desktop sounds",
    "Sound theme",
    "Alert sound",
    "Alert volume",
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
    /// How far every sound is turned down.
    Gain,
    /// What the event plays.
    Event(SoundEvent),
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

/// What an event's chooser offers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Offer {
    /// The theme's sound.
    Theme,
    /// The file chosen for it.
    Chosen,
    /// No sound at all.
    Silent,
    /// Choose a file in the session's picker: a command, so the chooser never
    /// rests on it.
    Pick,
}

/// What the chooser of an event playing `file` offers and what it calls each,
/// in its order: the chosen file, by its name, only while there is one.
fn offers(file: &EffectFile) -> impl Iterator<Item = (Offer, &str)> {
    let chosen = match file {
        EffectFile::Theme => None,
        EffectFile::Chosen(name) | EffectFile::Unavailable(name) => {
            Some((Offer::Chosen, name.as_str()))
        }
    };
    [(Offer::Theme, "Theme sound")]
        .into_iter()
        .chain(chosen)
        .chain([(Offer::Silent, "None"), (Offer::Pick, "Choose File…")])
}

/// What `event` plays as `reading` has it.
fn plays(reading: &EffectsReading, event: SoundEvent) -> Offer {
    if !reading.settings.event(event).enabled {
        Offer::Silent
    } else if matches!(reading.files[event.index()], EffectFile::Theme) {
        Offer::Theme
    } else {
        Offer::Chosen
    }
}

/// The pane's plates and the row each control changes.
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
    let plate = |caption: &str, owners: Vec<EffectsRow>| {
        let rows = owners.iter().map(|owner| row(*owner, reading)).collect();
        (FieldGroup::new(caption, rows), owners)
    };
    let (effects, general) = plate(
        "SOUND EFFECTS",
        alloc::vec![EffectsRow::Enabled, EffectsRow::Theme, EffectsRow::Gain],
    );
    let (events, each) = plate(
        "EVENT SOUNDS",
        SoundEvent::ALL
            .into_iter()
            .filter(|event| layered(*event))
            .map(EffectsRow::Event)
            .collect(),
    );
    (
        alloc::vec![
            effects.with_footnote(
                "Start-up, sign-in and sign-out sounds are set by this machine's administrator: \
                 they play while no one's choices are in force."
            ),
            events,
        ],
        alloc::vec![general, each],
    )
}

/// The row `owner` changes, as `reading` has it.
///
/// The one definition of each row, so a plate built afresh and a row brought
/// in line in place cannot disagree. While the desktop makes no sound every
/// row but its switch is disabled: none of them applies.
pub(crate) fn row(owner: EffectsRow, reading: &EffectsReading) -> FieldRow {
    let settings = &reading.settings;
    let applies = ControlState::idle().with_enabled(settings.enabled());
    match owner {
        EffectsRow::Enabled => FieldRow::new(
            "Play sound effects",
            FieldControl::Toggle(Toggle::new("", settings.enabled())),
        ),
        EffectsRow::Theme => {
            let themes = reading.offered_themes();
            let at = themes
                .iter()
                .position(|theme| theme == settings.theme())
                .unwrap_or(0);
            FieldRow::new(
                "Theme",
                FieldControl::Combo(ComboBox::new(themes).with_selected(at)),
            )
            .with_state(applies)
        }
        EffectsRow::Gain => FieldRow::new(
            "Volume",
            FieldControl::Slider(Slider::new(permille_of_level(settings.gain()))),
        )
        .with_description(format!("{}", settings.gain()))
        .with_state(applies),
        EffectsRow::Event(event) => {
            let file = &reading.files[event.index()];
            let current = plays(reading, event);
            let at = offers(file)
                .position(|(offer, _)| offer == current)
                .unwrap_or(0);
            let choices = offers(file).map(|(_, name)| String::from(name)).collect();
            let row = FieldRow::new(
                label(event),
                FieldControl::Combo(ComboBox::new(choices).with_selected(at)),
            )
            .with_state(applies);
            match file {
                EffectFile::Unavailable(name) if current == Offer::Chosen => row.with_description(
                    format!("{name} cannot be read, so the theme's sound plays."),
                ),
                _ => row,
            }
        }
    }
}

/// Whether `row` shows what a change to `changed`'s control moves: the
/// desktop's switch decides whether every other row applies.
pub(crate) fn touches(changed: EffectsRow, row: EffectsRow) -> bool {
    changed == EffectsRow::Enabled || changed == row
}

/// Show `ask` in `reading` as the session will hold it once made, answering
/// whether the reading moved: a pick moves nothing until the session has the
/// file.
pub(crate) fn apply(reading: &mut EffectsReading, ask: &EffectsAsk) -> bool {
    match ask {
        EffectsAsk::Apply { line, .. } => match merge_pushed(&reading.settings, line) {
            Ok(settings) => {
                reading.settings = settings;
                true
            }
            Err(_) => false,
        },
        EffectsAsk::UseTheme(event) => {
            reading.files[event.index()] = EffectFile::Theme;
            reading
                .settings
                .read(SoundKey::EventEnabled(*event), bool_text(true))
        }
        EffectsAsk::Pick(_) => false,
    }
}

/// Show the level a volume drag has reached at `permille`, before it settles
/// and is asked for, answering whether the reading moved.
pub(crate) fn show_level(reading: &mut EffectsReading, permille: u16) -> bool {
    let level = level_at_permille(permille);
    level != reading.settings.gain() && reading.settings.read(SoundKey::Gain, &format!("{level}"))
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
        (EffectsRow::Gain, FieldAction::Settled { permille }) => {
            changed(SoundKey::Gain, &format!("{}", level_at_permille(*permille)))
        }
        (EffectsRow::Event(event), FieldAction::Selected { index }) => {
            match offers(&reading.files[event.index()]).nth(*index)? {
                (Offer::Theme, _) => Some(EffectsAsk::UseTheme(event)),
                (Offer::Chosen, _) => changed(SoundKey::EventEnabled(event), bool_text(true)),
                (Offer::Silent, _) => changed(SoundKey::EventEnabled(event), bool_text(false)),
                (Offer::Pick, _) => Some(EffectsAsk::Pick(event)),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
