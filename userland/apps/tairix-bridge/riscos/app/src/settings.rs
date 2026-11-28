//! The application's settings (`plans/RISCOS-EMULATOR.md` §15.3), kept in
//! its own store, and the form that edits them, each committed where its
//! control settles.

use alloc::string::String;
use alloc::vec;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, as_u32, bool_text, overwrite, Live, Registry};
use tairix_controls::{
    Button, ColumnAction, Dialog, FieldAction, FieldColumn, FieldGroup, FieldRow, FormAction,
    FormDialog,
};
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::input::ButtonOrder;

/// The form's title, which its window's frame carries.
pub const TITLE: &str = "TAIRiX Bridge Settings";

/// The form's width in logical pixels.
const WIDTH: u32 = 520;

/// The memory a session may take that the user may choose, in MiB, beside
/// the machine's own share.
pub const CEILINGS_MIB: [u32; 7] = [64, 128, 256, 512, 1024, 2048, 4096];

/// How a screen's picture is scaled to its window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scaling {
    /// Each pixel a block of the desktop's.
    #[default]
    Sharp,
    /// Filtered.
    Smooth,
}

/// What the user chose.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    /// Which desktop button is which of RISC OS's.
    pub buttons: ButtonOrder,
    /// The most a session may take, in MiB: one of [`CEILINGS_MIB`], or none
    /// for the machine's own share.
    pub ceiling_mib: Option<u32>,
    /// Every launch runs in an isolated session.
    pub isolate_all: bool,
    /// How the screens are scaled.
    pub scaling: Scaling,
}

impl Settings {
    /// The most a session may take, in bytes, where one was chosen.
    #[must_use]
    pub fn ceiling_bytes(&self) -> Option<u64> {
        self.ceiling_mib.map(|mib| u64::from(mib) << 20)
    }
}

/// One setting, by its key in the store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// `input.buttons`.
    Buttons,
    /// `sessions.memory`.
    Ceiling,
    /// `sessions.isolate-all`.
    IsolateAll,
    /// `screens.scaling`.
    Scaling,
}

impl Registry for Settings {
    type Key = Key;

    const KEYS: &'static [Key] = &[Key::Buttons, Key::Ceiling, Key::IsolateAll, Key::Scaling];

    fn name(key: Key) -> &'static str {
        match key {
            Key::Buttons => "input.buttons",
            Key::Ceiling => "sessions.memory",
            Key::IsolateAll => "sessions.isolate-all",
            Key::Scaling => "screens.scaling",
        }
    }

    fn read(&mut self, key: Key, text: &str) -> bool {
        match key {
            Key::Buttons => match text {
                "roles" => self.buttons = ButtonOrder::Roles,
                "mouse" => self.buttons = ButtonOrder::Mouse,
                _ => return false,
            },
            Key::Ceiling => match as_u32(text) {
                Ok(mib) if CEILINGS_MIB.contains(&mib) => self.ceiling_mib = Some(mib),
                _ => return false,
            },
            Key::IsolateAll => match as_bool(text) {
                Ok(on) => self.isolate_all = on,
                Err(_) => return false,
            },
            Key::Scaling => match text {
                "sharp" => self.scaling = Scaling::Sharp,
                "smooth" => self.scaling = Scaling::Smooth,
                _ => return false,
            },
        }
        true
    }

    fn spell(&self, key: Key, out: &mut String) -> bool {
        let text = match key {
            Key::Buttons => match self.buttons {
                ButtonOrder::Roles => "roles",
                ButtonOrder::Mouse => "mouse",
            },
            Key::Ceiling => {
                let Some(mib) = self.ceiling_mib else {
                    return false;
                };
                return write!(out, "{mib}").is_ok();
            }
            Key::IsolateAll => bool_text(self.isolate_all),
            Key::Scaling => match self.scaling {
                Scaling::Sharp => "sharp",
                Scaling::Smooth => "smooth",
            },
        };
        out.push_str(text);
        true
    }
}

impl Live for Settings {
    fn take(&mut self, other: &Self, key: Key) -> bool {
        match key {
            Key::Buttons => overwrite(&mut self.buttons, other.buttons),
            Key::Ceiling => overwrite(&mut self.ceiling_mib, other.ceiling_mib),
            Key::IsolateAll => overwrite(&mut self.isolate_all, other.isolate_all),
            Key::Scaling => overwrite(&mut self.scaling, other.scaling),
        }
    }
}

/// Where each row is.
mod at {
    pub(super) const INPUT: usize = 0;
    pub(super) const BUTTONS: usize = 0;
    pub(super) const SESSIONS: usize = 1;
    pub(super) const CEILING: usize = 0;
    pub(super) const ISOLATE: usize = 1;
    pub(super) const SCREENS: usize = 2;
    pub(super) const SCALING: usize = 0;
}

const BUTTON_ORDERS: [&str; 2] = ["Primary, secondary, middle", "Primary, middle, secondary"];
const SCALINGS: [&str; 2] = ["Sharp", "Smooth"];

/// What a form's input came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edited {
    /// The settings changed, to be committed where the control settled.
    Changed {
        /// The control settled.
        settled: bool,
    },
    /// The form was closed.
    Closed,
}

/// The form editing the settings.
pub struct Form {
    form: FormDialog,
    settings: Settings,
}

impl Form {
    /// The form over `settings`.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        let ceilings: vec::Vec<String> = core::iter::once(String::from("Automatic"))
            .chain(CEILINGS_MIB.iter().map(|&mib| size(mib)))
            .collect();
        let ceilings: vec::Vec<&str> = ceilings.iter().map(String::as_str).collect();
        let ceiling = settings
            .ceiling_mib
            .and_then(|mib| CEILINGS_MIB.iter().position(|&chosen| chosen == mib))
            .map_or(0, |at| at + 1);
        let groups = vec![
            FieldGroup::new(
                "INPUT",
                vec![FieldRow::choice(
                    "Select, Menu and Adjust",
                    &BUTTON_ORDERS,
                    match settings.buttons {
                        ButtonOrder::Roles => 0,
                        ButtonOrder::Mouse => 1,
                    },
                )],
            ),
            FieldGroup::new(
                "SESSIONS",
                vec![
                    FieldRow::choice("Memory for each session", &ceilings, ceiling)
                        .with_description("Sessions already running keep theirs."),
                    FieldRow::toggle("Isolate every program", settings.isolate_all)
                        .with_description("No program runs in the shared session."),
                ],
            ),
            FieldGroup::new(
                "SCREENS",
                vec![FieldRow::choice(
                    "Scaling",
                    &SCALINGS,
                    match settings.scaling {
                        Scaling::Sharp => 0,
                        Scaling::Smooth => 1,
                    },
                )],
            ),
        ];
        let dialog = Dialog::new(TITLE)
            .in_window()
            .with_actions(vec![Button::labelled("Close")]);
        Self {
            form: FormDialog::new(dialog, FieldColumn::new(groups), WIDTH),
            settings,
        }
    }

    /// What the form holds now.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        self.form.extent(scale, theme)
    }

    /// The whole form painted.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.form.render(surface, scale, theme);
    }

    /// `event` taken at monotonic `at_ns`: what it came to, where anything.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Edited> {
        match self.form.on_input(event, at_ns, looks, damage)? {
            FormAction::Field(acted) => self.take(&acted, looks, damage),
            FormAction::Action(_) | FormAction::Dismissed => Some(Edited::Closed),
        }
    }

    fn take(
        &mut self,
        acted: &ColumnAction,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Edited> {
        let (group, row) = (acted.group, acted.action.row);
        let action = &acted.action.action;
        let was = self.settings;
        let s = &mut self.settings;
        match (group, row, action) {
            (at::INPUT, at::BUTTONS, FieldAction::Selected { index }) => {
                s.buttons = if *index == 0 {
                    ButtonOrder::Roles
                } else {
                    ButtonOrder::Mouse
                };
            }
            (at::SESSIONS, at::CEILING, FieldAction::Selected { index }) => {
                s.ceiling_mib = index
                    .checked_sub(1)
                    .and_then(|at| CEILINGS_MIB.get(at).copied());
            }
            (at::SESSIONS, at::ISOLATE, FieldAction::Set { on }) => {
                s.isolate_all = *on;
                self.form.show_toggle((group, row), *on, looks, damage);
            }
            (at::SCREENS, at::SCALING, FieldAction::Selected { index }) => {
                s.scaling = if *index == 0 {
                    Scaling::Sharp
                } else {
                    Scaling::Smooth
                };
            }
            _ => return None,
        }
        (self.settings != was).then_some(Edited::Changed {
            settled: action.settles(),
        })
    }
}

/// `mib` as the form names it.
fn size(mib: u32) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        alloc::format!("{} GB", mib / 1024)
    } else {
        alloc::format!("{mib} MB")
    }
}

#[cfg(test)]
mod tests;
