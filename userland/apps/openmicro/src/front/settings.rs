//! A machine's settings (`plans/BEEB.md` §5, `plans/C64.md` §6): a dialog
//! window's client of its name, its ROMs — the Electron's Plus 1 with them,
//! and a C64's standard, SID and drives — the Model B's start-up links, a
//! C64's keypad joystick, and its sound and tape loading. What is fitted is
//! read-only unless the machine is off; the rest may change while it runs.
//!
//! A ROM socket's list ends with *Install an image…*, which the dialog does
//! not answer: it says which socket is waiting, and the image installed is
//! offered and fitted there once it is kept.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ColumnAction, ControlRole, ControlState, Dialog, FieldAction,
    FieldColumn, FieldControl, FieldGroup, FieldRow, FormAction, FormDialog, Slider, TextField,
};
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_machines::record::MAX_NAME;
use tairix_machines::Standing;
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::library::{
    Fitting, Image, Joystick, Machine, Shipped, Socket, FULL_VOLUME, JOYSTICKS, SIDS, STANDARDS,
};
use crate::machine::c64::{sid, Standard};

/// The dialog's title, which the desktop's frame carries.
pub const TITLE: &str = "Machine Settings";

const WIDTH: u32 = 560;
const INSTALL: &str = "Install an image\u{2026}";
const MODES: [&str; 8] = ["0", "1", "2", "3", "4", "5", "6", "7"];
const BOOTS: [&str; 2] = ["SHIFT-BREAK boots a disc", "BREAK boots a disc"];
const TIMINGS: [&str; 4] = ["0, fastest", "1", "2", "3, slowest"];

/// Where each group lies: the machine's first, its ROMs next, the Model B's
/// links after them, and the sound last.
mod at {
    pub const MACHINE: usize = 0;
    pub const NAME: usize = 0;
    pub const PLUS1: usize = 1;
    pub const STANDARD: usize = 1;
    pub const SID: usize = 2;
    /// Device 8's drive, then device 9's.
    pub const DRIVES: usize = 3;
    pub const JOYSTICK: usize = 5;
    pub const ROMS: usize = 1;
    pub const LINKS: usize = 2;
    pub const MODE: usize = 0;
    pub const BOOT: usize = 1;
    pub const TIMING: usize = 2;
    pub const VOLUME: usize = 0;
    pub const FAST_TAPE: usize = 1;
}

/// Every choice the dialog holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    /// The machine's name.
    pub name: String,
    /// What it is fitted with.
    pub fitting: Fitting,
    /// Its sound's level, in thousandths.
    pub volume: u16,
    /// Whether a tape loads as fast as the machine can run.
    pub fast_loading: bool,
    /// Where a C64's keypad plays a joystick.
    pub joystick: Joystick,
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
    /// The ROM images installed in the store, which every socket offers.
    installed: Vec<String>,
    /// Why what is fitted cannot change where the machine stands.
    locked: Option<&'static str>,
    /// A socket whose image is to be installed, until it is taken.
    install: Option<Socket>,
}

/// What `socket`, holding `fitted`, offers: what it may hold, then every
/// installed image, the one it holds among them even if the store has lost
/// it.
fn listed(socket: Socket, fitted: &Image, installed: &[String]) -> Vec<Image> {
    let mut list: Vec<Image> = socket.offered().to_vec();
    list.extend(installed.iter().cloned().map(Image::Installed));
    if !list.contains(fitted) {
        list.push(fitted.clone());
    }
    list
}

/// `socket`'s row: its list, what it holds chosen, *Install…* last.
fn socket_row(socket: Socket, fitted: &Image, installed: &[String]) -> FieldRow {
    let list = listed(socket, fitted, installed);
    let mut names: Vec<&str> = list.iter().map(Image::called).collect();
    names.push(INSTALL);
    let at = list.iter().position(|image| image == fitted).unwrap_or(0);
    FieldRow::choice(socket.label(), &names, at)
}

/// Each ROM row's socket, what it shows and whether the machine has it: every
/// socket it has, an Electron's Plus 1 ones while it has none and a C64's
/// drives' while unfitted, as fitted ones would hold them, so the dialog
/// keeps its height.
fn rom_sockets(fitting: &Fitting) -> Vec<(Socket, Image, bool)> {
    let present = |(socket, image): (Socket, &Image)| (socket, image.clone(), true);
    if let Fitting::C64 { drives, .. } = fitting {
        let roms = fitting
            .sockets()
            .filter(|(socket, _)| !matches!(socket, Socket::Dos(_)))
            .map(present);
        let dos = drives.iter().zip(0u8..).map(|(dos, n)| {
            let shown = dos.clone().unwrap_or(Image::Shipped(Shipped::C1541Dos));
            (Socket::Dos(n), shown, dos.is_some())
        });
        return roms.chain(dos).collect();
    }
    let mut rows: Vec<(Socket, Image, bool)> = fitting.sockets().map(present).collect();
    if let Fitting::Electron { plus1: None, .. } = fitting {
        let absent = Fitting::electron(true);
        let plus1 = absent
            .sockets()
            .filter(|(socket, _)| matches!(socket, Socket::Plus1 | Socket::Cartridge(_)))
            .map(|(socket, image)| (socket, image.clone(), false));
        rows.extend(plus1);
    }
    rows
}

/// The ROM rows, each offering every installed image: those of sockets the
/// machine has not got, and all of them while what is fitted is `locked`,
/// disabled.
fn rom_rows(fitting: &Fitting, installed: &[String], locked: bool) -> Vec<FieldRow> {
    rom_sockets(fitting)
        .into_iter()
        .map(|(socket, image, present)| {
            let mut row = socket_row(socket, &image, installed);
            if locked || !present {
                row.set_state(ControlState::disabled());
            }
            row
        })
        .collect()
}

/// The MACHINE group's rows: the name, what the model is made with — an
/// Electron's Plus 1, a C64's standard, SID and drives — and a C64's keypad
/// joystick.
fn machine_rows(choices: &Choices) -> Vec<FieldRow> {
    let name = TextField::new()
        .with_text(&choices.name)
        .with_max_len(MAX_NAME);
    let mut rows = vec![FieldRow::new("Name", FieldControl::Text(name))];
    match &choices.fitting {
        Fitting::ModelB { .. } => {}
        Fitting::Electron { plus1, .. } => {
            rows.push(FieldRow::toggle("Plus 1", plus1.is_some()));
        }
        Fitting::C64 {
            standard,
            sid,
            drives,
            ..
        } => {
            rows.push(FieldRow::choice(
                "Standard",
                &STANDARDS,
                index_of(&Standard::ALL, standard),
            ));
            rows.push(FieldRow::choice(
                "SID",
                &SIDS,
                index_of(&sid::Model::ALL, sid),
            ));
            rows.push(FieldRow::toggle("Drive 8", drives[0].is_some()));
            rows.push(FieldRow::toggle("Drive 9", drives[1].is_some()));
            rows.push(
                FieldRow::choice(
                    "Keypad joystick",
                    &JOYSTICKS,
                    index_of(&Joystick::ALL, &choices.joystick),
                )
                .with_description("The numeric keypad's arrows move it, 0 and 5 fire."),
            );
        }
    }
    rows
}

/// `column` with what is fitted disabled: the rows the model is made with,
/// and a Model B's links.
fn lock_fitting(column: &mut FieldColumn, fitting: &Fitting) {
    let (fitted, links) = match fitting {
        Fitting::ModelB { .. } => (0..0, true),
        Fitting::Electron { .. } => (at::PLUS1..at::PLUS1 + 1, false),
        Fitting::C64 { .. } => (at::STANDARD..at::JOYSTICK, false),
    };
    let groups = column.groups_mut();
    if let Some(machine) = groups.get_mut(at::MACHINE) {
        for row in machine
            .rows_mut()
            .iter_mut()
            .take(fitted.end)
            .skip(fitted.start)
        {
            row.set_state(ControlState::disabled());
        }
    }
    if let Some(group) = groups.get_mut(at::LINKS).filter(|_| links) {
        for row in group.rows_mut() {
            row.set_state(ControlState::disabled());
        }
    }
}

impl Settings {
    /// The settings of `machine`, which stands as `standing` says, offering
    /// the `installed` images: what is fitted read-only unless it is off.
    #[must_use]
    pub fn new(machine: &Machine, standing: Standing, installed: Vec<String>) -> Self {
        let locked = standing.locked();
        let choices = Choices {
            name: machine.name.clone(),
            fitting: machine.fitting.clone(),
            volume: machine.volume,
            fast_loading: machine.fast_loading,
            joystick: machine.joystick,
        };
        let roms = rom_rows(&choices.fitting, &installed, locked.is_some());
        let mut groups = vec![
            FieldGroup::new("MACHINE", machine_rows(&choices)),
            FieldGroup::new("ROMS", roms),
        ];
        if let Fitting::ModelB { links, .. } = &choices.fitting {
            groups.push(FieldGroup::new(
                "KEYBOARD LINKS",
                vec![
                    FieldRow::choice("Screen mode", &MODES, usize::from(links.mode)),
                    FieldRow::choice("Boot", &BOOTS, usize::from(!links.shift_boots)),
                    FieldRow::choice("Disc timings", &TIMINGS, usize::from(links.disc_timing)),
                ],
            ));
        }
        groups.push(FieldGroup::new(
            "SOUND AND TAPE",
            vec![
                FieldRow::new("Volume", FieldControl::Slider(Slider::new(choices.volume))),
                FieldRow::toggle("Fast loading", choices.fast_loading)
                    .with_description("Runs the machine as fast as it can while the motor runs."),
            ],
        ));
        let mut column = FieldColumn::new(groups);
        if locked.is_some() {
            lock_fitting(&mut column, &choices.fitting);
        }
        let actions = vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Apply")),
                ControlRole::Recommended,
            ),
        ];
        let mut dialog = Dialog::new(TITLE).in_window().with_actions(actions);
        if let Some(why) = locked {
            dialog = dialog.with_message(why);
        }
        Self {
            form: FormDialog::new(dialog, column, WIDTH),
            choices,
            installed,
            locked,
            install: None,
        }
    }

    /// What the dialog holds now.
    #[must_use]
    pub const fn choices(&self) -> &Choices {
        &self.choices
    }

    /// The socket waiting for an image to be installed, taken once.
    pub fn take_install(&mut self) -> Option<Socket> {
        self.install.take()
    }

    /// Image `blob` was installed for `socket`: offered by every socket, and
    /// fitted in that one.
    pub fn installed(
        &mut self,
        socket: Socket,
        blob: String,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) {
        if !self.installed.contains(&blob) {
            self.installed.push(blob.clone());
        }
        if self.locked.is_none() {
            if let Some(fitted) = self.choices.fitting.socket_mut(socket) {
                *fitted = Image::Installed(blob);
            }
        }
        self.relist(looks, damage);
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
            FormAction::Action(index) => Some(if index == 0 {
                Answer::Cancel
            } else {
                Answer::Apply(self.choices.clone())
            }),
            FormAction::Dismissed => Some(Answer::Cancel),
        }
    }

    /// The group the sound's rows are in.
    fn sound_group(&self) -> usize {
        self.form.column().groups().len() - 1
    }

    fn take(&mut self, acted: &ColumnAction, looks: (Scale, &Theme), damage: &mut Region) {
        let (group, row) = (acted.group, acted.action.row);
        let sound = self.sound_group();
        match (group, row, &acted.action.action) {
            (at::MACHINE, at::NAME, FieldAction::Text(_)) => {
                if let Some(FieldControl::Text(field)) = self.form.column().groups()[at::MACHINE]
                    .rows()
                    .get(at::NAME)
                    .map(FieldRow::control)
                {
                    let mut name = String::from(field.text());
                    name.truncate(name.floor_char_boundary(MAX_NAME));
                    self.choices.name = name;
                }
            }
            (
                group,
                row,
                FieldAction::SetValue { permille } | FieldAction::Settled { permille },
            ) if group == sound && row == at::VOLUME => {
                self.choices.volume = (*permille).min(FULL_VOLUME);
            }
            (group, row, FieldAction::Set { on }) if group == sound && row == at::FAST_TAPE => {
                self.choices.fast_loading = *on;
                self.form.show_toggle((group, row), *on, looks, damage);
            }
            (at::MACHINE, at::JOYSTICK, FieldAction::Selected { index })
                if matches!(self.choices.fitting, Fitting::C64 { .. }) =>
            {
                if let Some(&joystick) = Joystick::ALL.get(*index) {
                    self.choices.joystick = joystick;
                }
            }
            _ if self.locked.is_some() => {}
            (at::MACHINE, row, action) if matches!(self.choices.fitting, Fitting::C64 { .. }) => {
                self.refit_c64((row, action), looks, damage);
            }
            (at::MACHINE, at::PLUS1, FieldAction::Set { on }) => {
                if let Fitting::Electron { os, basic, .. } = &self.choices.fitting {
                    let mut fitting = Fitting::electron(*on);
                    if let Fitting::Electron {
                        os: new_os,
                        basic: new_basic,
                        ..
                    } = &mut fitting
                    {
                        *new_os = os.clone();
                        *new_basic = basic.clone();
                    }
                    self.choices.fitting = fitting;
                }
                self.form.show_toggle((group, row), *on, looks, damage);
                self.relist(looks, damage);
            }
            (at::ROMS, row, FieldAction::Selected { index }) => {
                self.pick(row, *index, looks, damage);
            }
            (at::LINKS, row, FieldAction::Selected { index }) => {
                if let Fitting::ModelB { links, .. } = &mut self.choices.fitting {
                    let index = u8::try_from(*index).unwrap_or(0);
                    match row {
                        at::MODE => links.mode = index.min(7),
                        at::BOOT => links.shift_boots = index == 0,
                        at::TIMING => links.disc_timing = index.min(3),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// A C64's standard, SID or drive changed on row `row` of the machine's
    /// rows.
    fn refit_c64(
        &mut self,
        (row, action): (usize, &FieldAction),
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) {
        let Fitting::C64 {
            standard,
            sid,
            drives,
            ..
        } = &mut self.choices.fitting
        else {
            return;
        };
        match (row, action) {
            (at::STANDARD, FieldAction::Selected { index }) => {
                if let Some(&chosen) = Standard::ALL.get(*index) {
                    *standard = chosen;
                }
            }
            (at::SID, FieldAction::Selected { index }) => {
                if let Some(&chosen) = sid::Model::ALL.get(*index) {
                    *sid = chosen;
                }
            }
            (row, FieldAction::Set { on }) => {
                let Some(dos) = row.checked_sub(at::DRIVES).and_then(|n| drives.get_mut(n)) else {
                    return;
                };
                *dos = on.then(|| dos.clone().unwrap_or(Image::Shipped(Shipped::C1541Dos)));
                self.form
                    .show_toggle((at::MACHINE, row), *on, looks, damage);
                self.relist(looks, damage);
            }
            _ => {}
        }
    }

    /// Choice `index` of ROM row `row`: an image fitted, or the socket marked
    /// to have one installed and its row shown as it was.
    fn pick(&mut self, row: usize, index: usize, looks: (Scale, &Theme), damage: &mut Region) {
        let Some(&(socket, _, true)) = rom_sockets(&self.choices.fitting).get(row) else {
            return;
        };
        let Some(fitted) = self.choices.fitting.socket_mut(socket) else {
            return;
        };
        if let Some(image) = listed(socket, fitted, &self.installed).get(index) {
            *fitted = image.clone();
        } else {
            self.install = Some(socket);
            self.relist(looks, damage);
        }
    }

    /// The ROM rows built again from the choices, every socket offering every
    /// installed image.
    fn relist(&mut self, looks: (Scale, &Theme), damage: &mut Region) {
        let rebuilt = rom_rows(
            &self.choices.fitting,
            &self.installed,
            self.locked.is_some(),
        );
        let (_, place) = self.form.place(looks.0, looks.1);
        let roms = &mut self.form.column_mut().groups_mut()[at::ROMS];
        for (held, row) in roms.rows_mut().iter_mut().zip(rebuilt) {
            *held = row;
        }
        roms.adopt_focus(roms.focus());
        damage.add(place.bounds);
    }
}

/// `value`'s place among `all`, the first where it has none.
fn index_of<T: PartialEq>(all: &[T], value: &T) -> usize {
    all.iter()
        .position(|candidate| candidate == value)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
