//! A machine's settings: a dialog window's client of the hardware a machine
//! is given without its disks changing under it, every choice one the board
//! takes, and read-only while the machine runs.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ColumnAction, ControlRole, ControlState, Dialog, FieldAction,
    FieldColumn, FieldControl, FieldGroup, FieldRow, FormAction, FormDialog, TextField,
};
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_machines::record::MAX_NAME;
use tairix_machines::Standing;
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::library::Machine;
use crate::board::{BoardConfig, FloppyAdapter, SoundCard};
use crate::cpu::CpuModel;
use crate::machine::{expanded_board, SOUND_BLASTER};
use crate::storage::fdc::{DriveKind, Interface};
use crate::time::CpuClock;
use crate::video::monitor::{Monitor, Phosphor};
use crate::video::Adapter;

/// The dialog's title, which the desktop's frame carries.
pub const TITLE: &str = "Machine Settings";

const WIDTH: u32 = 600;

const PROCESSORS: [(CpuModel, &str); 6] = [
    (CpuModel::I8088, "Intel 8088"),
    (CpuModel::I8086, "Intel 8086"),
    (CpuModel::V20, "NEC V20"),
    (CpuModel::V30, "NEC V30"),
    (CpuModel::I80188, "Intel 80188"),
    (CpuModel::I80186, "Intel 80186"),
];

/// The processor speeds a machine is offered, and what each is called.
pub const SPEEDS: [(CpuClock, &str); 4] = [
    (CpuClock::PC, "4.77 MHz"),
    (CpuClock::TURBO_XT_SLOW, "7.16 MHz"),
    (CpuClock::I80186_8, "8 MHz"),
    (CpuClock::TURBO_XT, "10 MHz"),
];

const MEMORY: [(u16, &str); 3] = [(256, "256 KB"), (512, "512 KB"), (640, "640 KB")];

/// Expanded memory in its 16 KiB pages.
const EXPANDED: [(u16, &str); 5] = [
    (0, "None"),
    (64, "1 MB"),
    (128, "2 MB"),
    (256, "4 MB"),
    (512, "8 MB"),
];

const CARDS: [&str; 5] = ["MDA", "Hercules", "CGA", "EGA", "VGA"];

const MONO: [(Monitor, &str); 3] = [
    (Monitor::Mono(Phosphor::Green), "Monochrome, green"),
    (Monitor::Mono(Phosphor::Amber), "Monochrome, amber"),
    (Monitor::Mono(Phosphor::White), "Monochrome, white"),
];

const CGA_MONITORS: [(Monitor, &str); 5] = [
    (Monitor::Colour, "Colour"),
    (Monitor::Composite, "Composite colour"),
    MONO[0],
    MONO[1],
    MONO[2],
];

const COLOUR: [(Monitor, &str); 1] = [(Monitor::Colour, "Colour")];

const DRIVES: [(Option<DriveKind>, &str); 5] = [
    (None, "None"),
    (Some(DriveKind::Dd525), "5\u{bc}-inch 360 KB"),
    (Some(DriveKind::Hd525), "5\u{bc}-inch 1.2 MB"),
    (Some(DriveKind::Dd35), "3\u{bd}-inch 720 KB"),
    (Some(DriveKind::Hd35), "3\u{bd}-inch 1.44 MB"),
];

const SOUNDS: [&str; 3] = ["None", "AdLib", "Sound Blaster 2.0"];

/// The monitors card `card` (an index of [`CARDS`]) drives.
const fn monitors(card: usize) -> &'static [(Monitor, &'static str)] {
    match card {
        0 | 1 => &MONO,
        2 => &CGA_MONITORS,
        _ => &COLOUR,
    }
}

/// Where each setting is among the groups: a group and its row.
mod at {
    pub const MACHINE: usize = 0;
    pub const NAME: usize = 0;
    pub const PROCESSOR: usize = 1;
    pub const SPEED: usize = 2;
    pub const MEMORY: usize = 3;
    pub const EXPANDED: usize = 4;
    pub const COPROCESSOR: usize = 5;
    pub const PARTS: usize = 1;
    pub const CARD: usize = 0;
    pub const MONITOR: usize = 1;
    pub const SOUND: usize = 2;
    pub const DRIVE_A: usize = 3;
    pub const DRIVE_B: usize = 4;
    pub const GAME: usize = 5;
    pub const CLOCK: usize = 6;
}

/// Every choice the dialog holds, as indices of its lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    /// The machine's name.
    pub name: String,
    processor: usize,
    speed: usize,
    memory: usize,
    expanded: usize,
    coprocessor: bool,
    card: usize,
    monitor: usize,
    sound: usize,
    drives: [usize; 2],
    game: bool,
    clock: bool,
}

/// The index of the entry of `list` whose `keyed` is `key`, or the first.
fn index_of<T, K: PartialEq>(list: &[(T, &str)], key: &K, keyed: fn(&T) -> K) -> usize {
    list.iter()
        .position(|(item, _)| keyed(item) == *key)
        .unwrap_or(0)
}

impl Choices {
    /// The choices `machine` stands at; a value no list holds reads as the
    /// list's first.
    #[must_use]
    pub fn of(machine: &Machine) -> Self {
        let config = &machine.config;
        let (card, monitor) = match config.video.iter().flatten().next() {
            Some(Adapter::Mda(phosphor)) => (0, Monitor::Mono(*phosphor)),
            Some(Adapter::Hercules(phosphor)) => (1, Monitor::Mono(*phosphor)),
            Some(Adapter::Cga(monitor)) => (2, *monitor),
            Some(Adapter::Ega(monitor)) => (3, *monitor),
            Some(Adapter::Vga(monitor)) => (4, *monitor),
            None => (4, Monitor::Colour),
        };
        let drive = |n: usize| {
            let kind = config.floppy.and_then(|floppy| floppy.drives[n]);
            index_of(&DRIVES, &kind, |k| *k)
        };
        Self {
            name: machine.name.clone(),
            processor: index_of(&PROCESSORS, &config.cpu, |cpu| *cpu),
            speed: index_of(&SPEEDS, &config.clock, |hz| *hz),
            memory: index_of(&MEMORY, &config.memory_kib, |kib| *kib),
            expanded: index_of(&EXPANDED, &config.ems.map_or(0, |ems| ems.pages), |p| *p),
            coprocessor: config.coprocessor,
            card,
            monitor: index_of(monitors(card), &monitor, |m| *m),
            sound: match config.sound.card {
                None => 0,
                Some(SoundCard::AdLib) => 1,
                Some(SoundCard::Blaster { .. }) => 2,
            },
            drives: [drive(0), drive(1)],
            game: config.ports.game,
            clock: config.ports.clock,
        }
    }

    /// `base` with these choices made.
    #[must_use]
    pub fn configured(&self, base: &BoardConfig) -> BoardConfig {
        let pick = |list: &'static [(Monitor, &str)], n: usize| {
            list.get(n)
                .or(list.first())
                .map_or(Monitor::Colour, |(m, _)| *m)
        };
        let monitor = pick(monitors(self.card), self.monitor);
        let phosphor = match monitor {
            Monitor::Mono(phosphor) => phosphor,
            Monitor::Colour | Monitor::Composite => Phosphor::Green,
        };
        let adapter = match self.card {
            0 => Adapter::Mda(phosphor),
            1 => Adapter::Hercules(phosphor),
            2 => Adapter::Cga(monitor),
            3 => Adapter::Ega(monitor),
            _ => Adapter::Vga(monitor),
        };
        let drives = self
            .drives
            .map(|n| DRIVES.get(n).and_then(|(kind, _)| *kind));
        let high = drives
            .iter()
            .flatten()
            .any(|kind| matches!(kind, DriveKind::Hd525 | DriveKind::Hd35));
        let floppy = drives.iter().any(Option::is_some).then_some(FloppyAdapter {
            interface: if high {
                Interface::HighDensity
            } else {
                Interface::Standard
            },
            drives: [drives[0], drives[1], None, None],
        });
        let pages = EXPANDED.get(self.expanded).map_or(0, |(pages, _)| *pages);
        let mut config = *base;
        config.cpu = PROCESSORS
            .get(self.processor)
            .map_or(base.cpu, |(cpu, _)| *cpu);
        config.clock = SPEEDS.get(self.speed).map_or(base.clock, |(hz, _)| *hz);
        config.memory_kib = MEMORY
            .get(self.memory)
            .map_or(base.memory_kib, |(kib, _)| *kib);
        config.ems = (pages > 0).then(|| expanded_board(pages));
        config.coprocessor = self.coprocessor;
        config.video = [Some(adapter), None];
        config.floppy = floppy;
        config.sound.card = match self.sound {
            1 => Some(SoundCard::AdLib),
            2 => Some(SOUND_BLASTER),
            _ => None,
        };
        config.ports.game = self.game;
        config.ports.clock = self.clock;
        config
    }
}

fn names<T>(list: &[(T, &'static str)]) -> Vec<&'static str> {
    list.iter().map(|(_, name)| *name).collect()
}

/// What the dialog answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Nothing changes.
    Cancel,
    /// The machine takes these choices.
    Apply(Choices),
}

/// The dialog's state.
#[derive(Debug)]
pub struct Settings {
    form: FormDialog,
    choices: Choices,
    /// The hardware cannot be changed where the machine stands.
    locked: bool,
}

impl Settings {
    /// The settings of `machine`, which stands as `standing` says: read-only
    /// unless it is turned off.
    #[must_use]
    pub fn new(machine: &Machine, standing: Standing) -> Self {
        let locked = standing.locked();
        let choices = Choices::of(machine);
        let name = TextField::new()
            .with_text(&choices.name)
            .with_max_len(MAX_NAME);
        let machine_group = FieldGroup::new(
            "MACHINE",
            vec![
                FieldRow::new("Name", FieldControl::Text(name)),
                FieldRow::choice("Processor", &names(&PROCESSORS), choices.processor),
                FieldRow::choice("Speed", &names(&SPEEDS), choices.speed),
                FieldRow::choice("Memory", &names(&MEMORY), choices.memory),
                FieldRow::choice("Expanded memory", &names(&EXPANDED), choices.expanded),
                FieldRow::toggle("8087 coprocessor", choices.coprocessor),
            ],
        );
        let parts = FieldGroup::new(
            "PARTS",
            vec![
                FieldRow::choice("Display card", &CARDS, choices.card),
                FieldRow::choice("Monitor", &names(monitors(choices.card)), choices.monitor),
                FieldRow::choice("Sound card", &SOUNDS, choices.sound),
                FieldRow::choice("Drive A", &names(&DRIVES), choices.drives[0]),
                FieldRow::choice("Drive B", &names(&DRIVES), choices.drives[1]),
                FieldRow::toggle("Game port", choices.game),
                FieldRow::toggle("Clock card", choices.clock),
            ],
        );
        let actions = if locked.is_some() {
            vec![Button::labelled("Close")]
        } else {
            vec![
                Button::labelled("Cancel"),
                Button::new(
                    ButtonContent::Label(String::from("Apply")),
                    ControlRole::Recommended,
                ),
            ]
        };
        let mut dialog = Dialog::new(TITLE).in_window().with_actions(actions);
        if let Some(why) = locked {
            dialog = dialog.with_message(why);
        }
        let mut column = FieldColumn::new(vec![machine_group, parts]);
        if locked.is_some() {
            for row in column
                .groups_mut()
                .iter_mut()
                .flat_map(FieldGroup::rows_mut)
            {
                row.set_state(ControlState::disabled());
            }
        }
        Self {
            form: FormDialog::new(dialog, column, WIDTH),
            choices,
            locked: locked.is_some(),
        }
    }

    /// What the dialog holds now.
    #[must_use]
    pub const fn choices(&self) -> &Choices {
        &self.choices
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        self.form.extent(scale, theme)
    }

    /// Paint the whole client.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.form.render(surface, scale, theme);
    }

    /// Take `event` at monotonic `at_ns`: the answer, once there is one.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        match self.form.on_input(event, at_ns, looks, damage)? {
            FormAction::Field(acted) => {
                self.take(&acted, looks, damage);
                None
            }
            FormAction::Action(index) => Some(self.answer(index)),
            FormAction::Dismissed => Some(Answer::Cancel),
        }
    }

    fn take(&mut self, acted: &ColumnAction, looks: (Scale, &Theme), damage: &mut Region) {
        if self.locked {
            return;
        }
        let (group, row) = (acted.group, acted.action.row);
        let c = &mut self.choices;
        match (group, row, &acted.action.action) {
            (at::MACHINE, at::NAME, FieldAction::Text(_)) => {
                if let Some(FieldControl::Text(field)) = self.form.column().groups()[at::MACHINE]
                    .rows()
                    .get(at::NAME)
                    .map(FieldRow::control)
                {
                    let mut name = String::from(field.text());
                    name.truncate(name.floor_char_boundary(MAX_NAME));
                    c.name = name;
                }
            }
            (at::MACHINE, row, FieldAction::Selected { index }) => match row {
                at::PROCESSOR => c.processor = *index,
                at::SPEED => c.speed = *index,
                at::MEMORY => c.memory = *index,
                at::EXPANDED => c.expanded = *index,
                _ => {}
            },
            (at::PARTS, at::CARD, FieldAction::Selected { index }) => {
                c.card = *index;
                c.monitor = 0;
                let monitor = FieldRow::choice("Monitor", &names(monitors(c.card)), 0);
                let (_, place) = self.form.place(looks.0, looks.1);
                let parts = &mut self.form.column_mut().groups_mut()[at::PARTS];
                if let Some(held) = parts.rows_mut().get_mut(at::MONITOR) {
                    *held = monitor;
                    // The new list's widest monitor may move the column every
                    // control lines up on.
                    damage.add(place.bounds);
                }
                parts.adopt_focus(parts.focus());
            }
            (at::PARTS, row, FieldAction::Selected { index }) => match row {
                at::MONITOR => c.monitor = *index,
                at::SOUND => c.sound = *index,
                at::DRIVE_A => c.drives[0] = *index,
                at::DRIVE_B => c.drives[1] = *index,
                _ => {}
            },
            (_, _, FieldAction::Set { on }) => {
                let on = *on;
                match (group, row) {
                    (at::MACHINE, at::COPROCESSOR) => c.coprocessor = on,
                    (at::PARTS, at::GAME) => c.game = on,
                    (at::PARTS, at::CLOCK) => c.clock = on,
                    _ => return,
                }
                self.form.show_toggle((group, row), on, looks, damage);
            }
            _ => {}
        }
    }

    fn answer(&self, index: usize) -> Answer {
        if self.locked || index == 0 {
            return Answer::Cancel;
        }
        Answer::Apply(self.choices.clone())
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
