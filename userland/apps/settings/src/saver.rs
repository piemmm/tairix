//! The screensavers the desktop offers, as Settings lists them, and the rows
//! the chosen one's own group draws: every option its signed manifest
//! declares, then the desktop's settings for the feeds it is given.
//!
//! Settings reads no store: which savers exist, and what each declares, is
//! what the desktop's catalogue answers. A saver's values are held under keys
//! its own identifier names, so its rows write nothing of another's.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::saver::{SaverFeeds, SaverOption, SaverOptionKind, SaverValue};
use tairix_abi::window_ipc::{SaverOrigin, SaverStanding, ScreensaverEntry};
use tairix_abi::BundleId;
use tairix_controls::{ComboBox, FieldControl, FieldRow, Slider};
use tairix_wallpaper::{
    standing_reason, CatalogItem, DesktopSettings, ScreensaverChoice, SlideSource, TaskNames,
    WallpaperCategory,
};

use crate::form::{pick, switch_label, SWITCH};

// Every value a range holds has a stop on its slider, so each can be set.
const _: () =
    assert!(tairix_abi::saver::SAVER_RANGE_VALUES_MAX <= tairix_controls::SLIDER_STOPS_MAX);

/// One screensaver the desktop's catalogue lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaverEntry {
    /// Its place in the catalogue, which its preview is asked for by.
    pub index: u16,
    /// What a settings document names it by.
    pub choice: ScreensaverChoice,
    /// What the chooser shows.
    pub title: String,
    /// Its one-line purpose; empty when it states none.
    pub purpose: String,
    /// Where it comes from.
    pub origin: SaverOrigin,
    /// Whether it can be chosen.
    pub standing: SaverStanding,
    /// The privileged feeds it declares.
    pub feeds: SaverFeeds,
    /// The options it declares, in its manifest's order.
    pub options: Vec<SaverOption>,
}

impl SaverEntry {
    /// Catalogue entry `index` as the desktop answered it, or `None` for one
    /// this build cannot name: no document could hold its choice.
    #[must_use]
    pub fn of(index: u16, entry: &ScreensaverEntry<'_>) -> Option<Self> {
        Some(Self {
            index,
            choice: ScreensaverChoice::from_value(entry.choice)?,
            title: String::from(entry.title),
            purpose: String::from(entry.purpose),
            origin: entry.origin,
            standing: entry.standing,
            feeds: entry.feeds,
            options: entry.options().collect(),
        })
    }

    /// Add catalogue entry `index`, as the desktop answered it, to `savers`,
    /// unless no document could hold it. A second claim to a saver's
    /// identifier is kept: the desktop marks it a duplicate, which the pane
    /// says, and the chooser offers only what can be chosen.
    pub fn list(savers: &mut Vec<Self>, index: u16, entry: &ScreensaverEntry<'_>) {
        if let Some(saver) = Self::of(index, entry) {
            savers.push(saver);
        }
    }

    /// Whether it can be chosen.
    #[must_use]
    pub fn offered(&self) -> bool {
        self.standing == SaverStanding::Offered
    }

    /// The bundle it is, for one that is.
    const fn bundle(&self) -> Option<&BundleId> {
        match &self.choice {
            ScreensaverChoice::Saver(id) => Some(id),
            ScreensaverChoice::Blank | ScreensaverChoice::Dim => None,
        }
    }
}

/// What a reader searches for to reach a saver's own rows, which the pane
/// shows only for the saver chosen.
pub const SAVER_FACTS: [&str; 1] = ["Screensaver options"];

/// The label of the row choosing which pictures the slides feed offers.
pub const SLIDES_LABEL: &str = "Pictures shown";

/// The label of the row choosing whose task names the machine feed carries.
pub const TASKS_LABEL: &str = "Task names";

/// One row of the chosen saver's own group.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaverRow {
    /// The option at this place among those the saver declares.
    Option(usize),
    /// Which of the shipped pictures the slides feed offers.
    Slides,
    /// Whose task names the machine feed carries.
    Tasks,
}

impl SaverRow {
    /// The rows `saver` draws: its options, then one for each feed it is
    /// given whose setting the desktop holds.
    #[must_use]
    pub fn of(saver: &SaverEntry) -> Vec<Self> {
        let mut rows: Vec<Self> = (0..saver.options.len()).map(Self::Option).collect();
        if saver.feeds.slides() {
            rows.push(Self::Slides);
        }
        if saver.feeds.machine() {
            rows.push(Self::Tasks);
        }
        rows
    }

    /// The row as drawn for `saver` over `settings`, the shipped pictures
    /// being `catalog`; `None` for an option the saver does not declare.
    #[must_use]
    pub(crate) fn row(
        self,
        saver: &SaverEntry,
        settings: &DesktopSettings,
        catalog: &[CatalogItem],
    ) -> Option<FieldRow> {
        match self {
            Self::Option(at) => {
                let option = saver.options.get(at)?;
                let held = saver.bundle().map_or_else(
                    || option.default_value(),
                    |id| settings.saver_values.value(id, option),
                );
                Some(option_row(option, held))
            }
            Self::Slides => {
                let sources = slide_sources(catalog, &settings.slides);
                let labels = sources.iter().map(slides_label).collect();
                let at = sources
                    .iter()
                    .position(|source| *source == settings.slides)
                    .unwrap_or(0);
                Some(
                    FieldRow::new(SLIDES_LABEL, combo((labels, at)))
                        .with_description("Which of the shipped pictures the screensaver shows."),
                )
            }
            Self::Tasks => Some(
                FieldRow::new(
                    TASKS_LABEL,
                    combo(pick(&TaskNames::ALL, settings.task_names, tasks_label)),
                )
                .with_description(
                    "Whose busiest tasks the screensaver names. A task is named by its owner, \
                     and the screensaver draws over the locked screen, so another account's \
                     tasks are shown by what they cost unless every account's are chosen.",
                ),
            ),
        }
    }

    /// Write the choice at `index` onto `settings` for `saver`, one of the
    /// `listed` savers, the shipped pictures being `catalog`, answering whether
    /// it named one the row offers.
    ///
    /// Fails closed: an index outside the list the row was drawn with changes
    /// nothing, so a routing defect cannot post a value the reader did not
    /// choose. A document with no room for another saver value makes it by
    /// forgetting the values of savers no longer listed.
    pub(crate) fn adopt(
        self,
        (saver, listed): (&SaverEntry, &[SaverEntry]),
        index: usize,
        settings: &mut DesktopSettings,
        catalog: &[CatalogItem],
    ) -> bool {
        match self {
            Self::Option(at) => {
                let (Some(option), Some(id)) = (saver.options.get(at), saver.bundle()) else {
                    return false;
                };
                let Some(value) = option_value(option, index) else {
                    return false;
                };
                let values = &mut settings.saver_values;
                if values.set(id, option, value) {
                    return true;
                }
                values
                    .retain_savers(|held| listed.iter().any(|saver| saver.bundle() == Some(held)));
                values.set(id, option, value)
            }
            Self::Slides => match slide_sources(catalog, &settings.slides)
                .into_iter()
                .nth(index)
            {
                Some(source) => {
                    settings.slides = source;
                    true
                }
                None => false,
            },
            Self::Tasks => crate::form::set(&TaskNames::ALL, index, &mut settings.task_names),
        }
    }
}

/// The row `option` draws holding `held`; a value of another kind, which no
/// reading of the option gives, draws as its default.
fn option_row(option: &SaverOption, held: SaverValue) -> FieldRow {
    let (control, described) = match option.kind() {
        SaverOptionKind::Switch { default } => {
            let on = match held {
                SaverValue::Switch(on) => on,
                SaverValue::Choice(_) | SaverValue::Range(_) => *default,
            };
            (
                combo(pick(&SWITCH, on, switch_label)),
                String::from(option.note()),
            )
        }
        SaverOptionKind::Choice { choices, default } => {
            let at = match held {
                SaverValue::Choice(value) => choices.position(value.as_str()),
                SaverValue::Switch(_) | SaverValue::Range(_) => None,
            };
            let labels = choices
                .as_slice()
                .iter()
                .map(|choice| choice.label.as_str().to_string())
                .collect();
            (
                combo((labels, at.unwrap_or(usize::from(*default)))),
                String::from(option.note()),
            )
        }
        SaverOptionKind::Range {
            range,
            default,
            unit,
        } => {
            let value = match held {
                SaverValue::Range(value) if range.holds(value) => value,
                SaverValue::Switch(_) | SaverValue::Choice(_) | SaverValue::Range(_) => *default,
            };
            let spelled = |value: i32| spell_amount(value, unit.as_str());
            let mut slider = Slider::new(0)
                .with_stops(range.values())
                .with_ends(spelled(range.min()), spelled(range.max()));
            slider.set_value(
                slider
                    .stop_value(range.index_of(value).unwrap_or(0))
                    .unwrap_or(0),
            );
            let now = spelled(value);
            let described = if option.note().is_empty() {
                format!("Now {now}.")
            } else {
                format!("{} Now {now}.", option.note())
            };
            (FieldControl::Slider(slider), described)
        }
    };
    let row = FieldRow::new(option.label(), control);
    if described.is_empty() {
        row
    } else {
        row.with_description(described)
    }
}

/// The value the choice at `index` of `option`'s row names.
fn option_value(option: &SaverOption, index: usize) -> Option<SaverValue> {
    match option.kind() {
        SaverOptionKind::Switch { .. } => SWITCH.get(index).copied().map(SaverValue::Switch),
        SaverOptionKind::Choice { choices, .. } => choices
            .as_slice()
            .get(index)
            .map(|choice| SaverValue::Choice(choice.value)),
        SaverOptionKind::Range { range, .. } => u16::try_from(index)
            .ok()
            .and_then(|stop| range.value_at(stop))
            .map(SaverValue::Range),
    }
}

/// `value` in `unit`, or bare where it counts in none.
fn spell_amount(value: i32, unit: &str) -> String {
    if unit.is_empty() {
        value.to_string()
    } else {
        format!("{value} {unit}")
    }
}

/// A list of `labels` with the one at `at` held.
fn combo((labels, at): (Vec<String>, usize)) -> FieldControl {
    let mut combo = ComboBox::new(labels);
    combo.set_selected(at);
    FieldControl::Combo(combo)
}

/// What the slides row offers: every picture, then each category of the
/// shipped ones in the catalog's order, the one `held` among them even where
/// the catalog no longer has it.
fn slide_sources(catalog: &[CatalogItem], held: &SlideSource) -> Vec<SlideSource> {
    let mut sources = alloc::vec![SlideSource::Every];
    for item in catalog {
        let Some(category) = WallpaperCategory::new(&item.category) else {
            continue;
        };
        let source = SlideSource::Category(category);
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    if !sources.contains(held) {
        sources.push(held.clone());
    }
    sources
}

fn slides_label(source: &SlideSource) -> String {
    match source.category() {
        None => String::from("Every picture"),
        Some(category) => String::from(category.as_str()),
    }
}

const fn tasks_label(names: TaskNames) -> &'static str {
    match names {
        TaskNames::None => "None",
        TaskNames::Own => "Your own",
        TaskNames::All => "Every account's",
    }
}

/// The caption of the group the chosen saver's own rows sit in: its title,
/// or what says no saver of that name is listed.
#[must_use]
pub fn options_caption(chosen: Option<&SaverEntry>) -> String {
    chosen.map_or_else(
        || String::from("SCREENSAVER OPTIONS"),
        |saver| saver.title.to_uppercase(),
    )
}

/// What the pane says beneath the chosen saver's rows when the desktop lists
/// no saver of that name: once the catalogue has been answered, so a list
/// still to arrive is never mistaken for a saver gone.
#[must_use]
pub fn missing_statement(choice: &ScreensaverChoice, savers: &[SaverEntry]) -> Option<String> {
    let listed = savers.iter().any(|saver| saver.choice == *choice);
    (!savers.is_empty() && !listed).then(|| {
        format!(
            "The screensaver chosen, {}, is not installed: the screen is kept black until \
             another is chosen.",
            choice.as_str()
        )
    })
}

/// The entry `choice` names among `savers`: the one that can be chosen where a
/// second claim to its identifier is listed beside it.
#[must_use]
pub(crate) fn chosen<'a>(
    savers: &'a [SaverEntry],
    choice: &ScreensaverChoice,
) -> Option<&'a SaverEntry> {
    let mut named = savers.iter().filter(|saver| saver.choice == *choice);
    let first = named.clone().next();
    named.find(|saver| saver.offered()).or(first)
}

/// Why each saver the desktop lists cannot be chosen, one sentence apiece,
/// or `None` when every one can.
#[must_use]
pub fn unavailable_statement(savers: &[SaverEntry]) -> Option<String> {
    let lines: Vec<String> = savers
        .iter()
        .filter(|saver| !saver.offered())
        .map(|saver| {
            let whose = if saver.origin == SaverOrigin::User {
                " (yours)"
            } else {
                ""
            };
            format!(
                "{}{whose} cannot be chosen: {}.",
                saver.title,
                standing_reason(saver.standing)
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join(" "))
}

#[cfg(test)]
#[path = "saver_tests.rs"]
mod tests;
