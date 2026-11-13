//! The settings window's view of the emulator's settings: four categories of
//! rows, each row's action carried into the settings, and what a row asks of
//! the application itself — a ROM image to install.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{ColumnAction, FieldAction, FieldControl, FieldGroup, FieldRow, Slider, Tab};
use tairix_crt::screen::Scaling;
use tairix_icon::IconKind;
use tairix_window::docapp::settings;

use super::keys::Mapping;
use super::settings::{Image, Preferences, Shipped, INSTALLED};
use crate::worker::protocol::SLOTS;

/// The settings window over the emulator's settings.
pub type Sheet = settings::SettingsWindow<Preferences, ()>;

/// Where a ROM image the user picks is to be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Socket {
    /// The OS socket.
    Os,
    /// A sideways slot, 0 to 15.
    Slot(u8),
}

const CATEGORIES: [(&str, IconKind); 4] = [
    ("ROMs", IconKind::Computer),
    ("Startup", IconKind::Startup),
    ("Display", IconKind::Display),
    ("Keyboard and sound", IconKind::Keyboard),
];

mod category {
    pub const ROMS: usize = 0;
    pub const STARTUP: usize = 1;
    pub const DISPLAY: usize = 2;
    pub const INPUT: usize = 3;
}

const INSTALL: &str = "Install an image…";
const NEXT_RESET: &str = "Takes effect at the next reset.";
/// What a slot may hold besides an installed image, in its list's order.
const FITTINGS: [Image; 4] = [
    Image::Empty,
    Image::Ram,
    Image::Shipped(Shipped::Basic),
    Image::Shipped(Shipped::Dfs),
];
const MAPPINGS: [Mapping; 2] = [Mapping::Logical, Mapping::Physical];
const SCALINGS: [Scaling; 2] = [Scaling::Sharp, Scaling::Smooth];

/// What an image is called in a list.
fn called(image: &Image) -> &str {
    match image {
        Image::Empty => "Empty",
        Image::Ram => "Sideways RAM",
        Image::Shipped(Shipped::Mos) => "TAIRiX MOS",
        Image::Shipped(Shipped::Basic) => "TAIRiX BASIC",
        Image::Shipped(Shipped::Dfs) => "TAIRiX DFS",
        Image::Installed(blob) => blob.strip_prefix(INSTALLED).unwrap_or(blob),
    }
}

/// A socket's list: what it may hold, the image installed in it if one is,
/// and the way to install another, last.
fn socket_row(label: &str, offered: &[Image], fitted: &Image) -> FieldRow {
    let mut choices: Vec<&str> = offered.iter().map(called).collect();
    let installed = matches!(fitted, Image::Installed(_));
    if installed {
        choices.push(called(fitted));
    }
    choices.push(INSTALL);
    let at = offered
        .iter()
        .position(|offer| offer == fitted)
        .unwrap_or(offered.len());
    FieldRow::choice(label, &choices, at)
}

/// What choosing an entry of a socket's list asks.
enum Choice {
    /// Fit this.
    Fit(Image),
    /// Keep the installed image it holds.
    Keep,
    /// Install another.
    Install,
}

/// What choosing `index` of a socket's list asks.
fn chosen(offered: &[Image], fitted: &Image, index: usize) -> Choice {
    if let Some(offer) = offered.get(index) {
        return Choice::Fit(offer.clone());
    }
    let installed = usize::from(matches!(fitted, Image::Installed(_)));
    if index < offered.len() + installed {
        Choice::Keep
    } else {
        Choice::Install
    }
}

fn roms(record: &Preferences) -> Vec<FieldGroup> {
    let os = socket_row("OS", &[Image::Shipped(Shipped::Mos)], &record.os);
    let slots = (0..SLOTS)
        .rev()
        .map(|slot| socket_row(&format!("Slot {slot}"), &FITTINGS, &record.slots[slot]))
        .collect();
    vec![
        FieldGroup::new("OPERATING SYSTEM", vec![os]),
        FieldGroup::new("SIDEWAYS SLOTS", slots).with_footnote(NEXT_RESET),
    ]
}

fn startup(record: &Preferences) -> Vec<FieldGroup> {
    let modes = ["0", "1", "2", "3", "4", "5", "6", "7"];
    let boots = ["SHIFT-BREAK boots a disc", "BREAK boots a disc"];
    let timings = ["0, fastest", "1", "2", "3, slowest"];
    vec![FieldGroup::new(
        "KEYBOARD LINKS",
        vec![
            FieldRow::choice("Screen mode", &modes, usize::from(record.mode)),
            FieldRow::choice("Boot", &boots, usize::from(!record.shift_boots)),
            FieldRow::choice("Disc timings", &timings, usize::from(record.disc_timing))
                .with_description("The step rate the DFS drives the heads at."),
        ],
    )
    .with_footnote(NEXT_RESET)]
}

fn display(record: &Preferences) -> Vec<FieldGroup> {
    let at = SCALINGS
        .iter()
        .position(|&s| s == record.scaling)
        .unwrap_or(0);
    vec![FieldGroup::new(
        "PICTURE",
        vec![
            FieldRow::choice("Scaling", &["Sharp", "Smooth"], at)
                .with_description("Sharp keeps each dot square-edged."),
            FieldRow::toggle("Scan lines", record.scan_lines),
        ],
    )]
}

fn input(record: &Preferences) -> Vec<FieldGroup> {
    let at = MAPPINGS
        .iter()
        .position(|&m| m == record.keyboard)
        .unwrap_or(0);
    let volume = u16::try_from(record.volume).unwrap_or(u16::MAX);
    vec![
        FieldGroup::new(
            "KEYBOARD",
            vec![
                FieldRow::choice("Keys", &["By character", "By position"], at).with_description(
                    "By position places the BBC's keys where a game expects them.",
                ),
            ],
        ),
        FieldGroup::new(
            "SOUND",
            vec![FieldRow::new(
                "Volume",
                FieldControl::Slider(Slider::new(volume)),
            )],
        ),
        FieldGroup::new(
            "CASSETTE",
            vec![FieldRow::toggle("Fast loading", record.fast_tape)
                .with_description("Runs the machine as fast as it can while the motor runs.")],
        ),
    ]
}

const fn index_of(action: &FieldAction) -> Option<usize> {
    match action {
        FieldAction::Selected { index } => Some(*index),
        _ => None,
    }
}

const fn on_of(action: &FieldAction) -> Option<bool> {
    match action {
        FieldAction::Set { on } => Some(*on),
        _ => None,
    }
}

fn edit_roms(
    record: &mut Preferences,
    (group, row): (usize, usize),
    index: usize,
) -> Option<Socket> {
    if group == 0 {
        return match chosen(&[Image::Shipped(Shipped::Mos)], &record.os, index) {
            Choice::Fit(image) => {
                record.os = image;
                None
            }
            Choice::Keep => None,
            Choice::Install => Some(Socket::Os),
        };
    }
    let slot = SLOTS.checked_sub(row + 1)?;
    let fitted = record.slots.get_mut(slot)?;
    match chosen(&FITTINGS, fitted, index) {
        Choice::Fit(image) => {
            *fitted = image;
            None
        }
        Choice::Keep => None,
        Choice::Install => u8::try_from(slot).ok().map(Socket::Slot),
    }
}

impl settings::Preferences for Preferences {
    const TITLE: &'static str = "BBC Micro settings";
    type Ask = Socket;

    fn categories() -> Vec<Tab> {
        CATEGORIES
            .iter()
            .map(|&(label, icon)| Tab::new(label).with_icon(icon))
            .collect()
    }

    fn groups(&self, category: usize) -> Vec<FieldGroup> {
        match category {
            category::ROMS => roms(self),
            category::STARTUP => startup(self),
            category::DISPLAY => display(self),
            _ => input(self),
        }
    }

    fn act(&mut self, category: usize, acted: &ColumnAction) -> Option<Socket> {
        let (group, row, action) = (acted.group, acted.action.row, &acted.action.action);
        match category {
            category::ROMS => return edit_roms(self, (group, row), index_of(action)?),
            category::STARTUP => {
                let index = u8::try_from(index_of(action)?).ok()?;
                match row {
                    0 if index <= 7 => self.mode = index,
                    1 => self.shift_boots = index == 0,
                    2 if index <= 3 => self.disc_timing = index,
                    _ => {}
                }
            }
            category::DISPLAY => match (row, action) {
                (0, _) => {
                    if let Some(&scaling) = index_of(action).and_then(|at| SCALINGS.get(at)) {
                        self.scaling = scaling;
                    }
                }
                (1, _) => self.scan_lines = on_of(action)?,
                _ => {}
            },
            category::INPUT => match (group, action) {
                (0, _) => {
                    if let Some(&mapping) = index_of(action).and_then(|at| MAPPINGS.get(at)) {
                        self.keyboard = mapping;
                    }
                }
                (1, FieldAction::SetValue { permille } | FieldAction::Settled { permille }) => {
                    self.volume = u32::from(*permille).min(tairix_appconf::PERMILLE_FULL);
                }
                (2, _) => self.fast_tape = on_of(action)?,
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn reshapes(&self, was: &Self, category: usize) -> bool {
        category == category::ROMS && (was.os != self.os || was.slots != self.slots)
    }
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
mod tests;
