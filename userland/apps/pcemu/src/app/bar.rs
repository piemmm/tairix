//! A machine window's bar: a lamp for each drive the machine has, lit while
//! the drive works, each diskette drive naming the image it holds, each
//! shared folder and the drive it is, and the machine's own menu at its
//! trailing end, a pull-down button. A click with either button on a diskette
//! drive, a share or the machine asks for its menu.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonAction, ButtonContent, ControlRole, ControlState, StatusBand, StatusTone,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::board::{BoardConfig, Lights};

/// A diskette drive's slot at the reference density: its lamp, its letter
/// and as much of an image's name as is worth showing.
const DISKETTE_SLOT: u32 = 220;

/// What a drive is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A diskette drive, which takes images.
    Diskette,
    /// A fixed disk, which is the machine's own.
    Fixed,
    /// A shared folder, which has no lamp: the drive DOS gave it, if any.
    Share(Option<u8>),
}

/// One drive's place on the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Drive {
    kind: Kind,
    /// Which of its kind it is: the board's unit number, or the share's slot.
    unit: u8,
    /// The image a diskette drive holds, or the folder a share is, and
    /// whether it refuses writes.
    holds: Option<(String, bool)>,
    slot: Rect,
}

/// A shared folder as the bar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shared {
    /// The folder's name.
    pub name: String,
    /// The drive DOS gave it, `0` for `A:`, if it has one.
    pub drive: Option<u8>,
    /// Whether it refuses writes.
    pub read_only: bool,
}

/// What the machine's own button shows, running, paused, and being kept as it
/// suspends.
const MACHINE: (IconKind, &str) = (IconKind::Display, "Machine");
const PAUSED: (IconKind, &str) = (IconKind::Pause, "Paused");
const SAVING: (IconKind, &str) = (IconKind::Display, "Saving\u{2026}");

/// The room the machine's button keeps above and below it in the bar, in
/// logical pixels.
const BUTTON_INSET: u32 = 2;

fn machine_content((icon, label): (IconKind, &str)) -> ButtonContent {
    ButtonContent::IconLabel {
        icon,
        label: String::from(label),
    }
}

/// Which menu a press on the bar asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    /// Diskette drive `unit`'s, `holds` saying whether it has an image to
    /// eject.
    Drive {
        /// The drive.
        unit: u8,
        /// Whether it holds an image.
        holds: bool,
    },
    /// Share `slot`'s.
    Share {
        /// The share.
        slot: u8,
    },
    /// The machine's own.
    Machine,
}

/// What a press on the bar asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuRequest {
    /// The menu.
    pub menu: Menu,
    /// The slot it is anchored to.
    pub anchor: Rect,
}

/// What a held press began on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pressed {
    Drive(usize),
    Machine,
}

/// The bar's state.
#[derive(Clone, Debug)]
pub struct MachineBar {
    drives: Vec<Drive>,
    lights: Lights,
    bounds: Rect,
    /// The machine's own menu, at the trailing end.
    machine: Button,
    machine_at: Rect,
    paused: bool,
    /// The machine is being kept as it suspends: nothing more is asked of it.
    saving: bool,
    pointer: Point,
    /// What a held press began on, and the button holding it.
    pressed: Option<(Pressed, PointerButton)>,
}

/// What a drive is called: the letter DOS gives the first two diskette
/// drives and the fixed disks after them, a third or fourth diskette drive
/// its number, its letter falling after the fixed disks' volumes, and a
/// share the letter DOS gave it.
fn label(kind: Kind, unit: u8) -> String {
    let letter = |drive: u8| format!("{}:", char::from(b'A'.saturating_add(drive)));
    match (kind, unit) {
        (Kind::Diskette, 0 | 1) => letter(unit),
        (Kind::Diskette, _) => format!("Diskette {}", u32::from(unit) + 1),
        (Kind::Fixed, _) => letter(unit.saturating_add(2)),
        (Kind::Share(drive), _) => drive.map(letter).unwrap_or_default(),
    }
}

/// Whether a drive of `kind` has a slot of its own on the bar: what it holds
/// is named, at a width the rest share.
const fn names_what_it_holds(kind: Kind) -> bool {
    !matches!(kind, Kind::Fixed)
}

impl MachineBar {
    /// The bar of a machine of `config` with fixed disks on the units
    /// `fixed` says.
    #[must_use]
    pub fn new(config: &BoardConfig, fixed: [bool; 2]) -> Self {
        let diskettes = config.floppy.map_or([None; 4], |floppy| floppy.drives);
        let units = |kinds: &mut dyn Iterator<Item = (u8, bool)>, kind| {
            kinds
                .filter(|(_, fitted)| *fitted)
                .map(|(unit, _)| Drive {
                    kind,
                    unit,
                    holds: None,
                    slot: Rect::EMPTY,
                })
                .collect::<Vec<_>>()
        };
        let mut drives = units(
            &mut (0u8..).zip(diskettes.iter().map(Option::is_some)),
            Kind::Diskette,
        );
        drives.extend(units(&mut (0u8..).zip(fixed), Kind::Fixed));
        Self {
            drives,
            lights: Lights::default(),
            bounds: Rect::EMPTY,
            machine: Button::new(machine_content(MACHINE), ControlRole::Neutral).pulls_down(),
            machine_at: Rect::EMPTY,
            paused: false,
            saving: false,
            pointer: Point::ORIGIN,
            pressed: None,
        }
    }

    /// The bar's height: a status band's, or more where the machine's button
    /// needs it.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        let button = Button::height(scale, theme)
            .saturating_add(scale.scale_length(BUTTON_INSET).saturating_mul(2));
        StatusBand::new(theme, scale).height().max(button)
    }

    /// Lay the bar out along `bounds`.
    pub fn place(&mut self, bounds: Rect, (scale, theme): (Scale, &Theme)) {
        self.bounds = bounds;
        let band = StatusBand::new(theme, scale);
        let lamp = band.face().line_height();
        let gap = scale.scale_length(theme.metrics().control_gap);
        // As wide as the widest it ever shows, so a pause never moves it.
        let machine_width = [MACHINE, PAUSED, SAVING]
            .map(|shown| {
                Button::new(machine_content(shown), ControlRole::Neutral)
                    .pulls_down()
                    .measured_width(scale, theme)
            })
            .into_iter()
            .max()
            .unwrap_or(0)
            .min(bounds.width.saturating_sub(gap));
        let machine_height = Button::height(scale, theme).min(bounds.height);
        self.machine_at = Rect::new(
            bounds
                .right()
                .saturating_sub_unsigned(machine_width.saturating_add(gap)),
            bounds
                .top()
                .saturating_add_unsigned(bounds.height.saturating_sub(machine_height) / 2),
            machine_width,
            machine_height,
        );
        let fixed_width = band.slot_width("C:") + lamp;
        let fixed = self
            .drives
            .iter()
            .filter(|drive| !names_what_it_holds(drive.kind))
            .count();
        let named = self.drives.len() - fixed;
        let room = bounds
            .width
            .saturating_sub(machine_width.saturating_add(gap * 2))
            .saturating_sub(u32::try_from(fixed).unwrap_or(0) * fixed_width);
        let each = u32::try_from(named)
            .ok()
            .and_then(|n| room.checked_div(n))
            .unwrap_or(0);
        let named_width = scale.scale_length(DISKETTE_SLOT).min(each);
        let mut x = bounds.left();
        for drive in &mut self.drives {
            let width = if names_what_it_holds(drive.kind) {
                named_width
            } else {
                fixed_width
            };
            drive.slot = Rect::new(x, bounds.top(), width, bounds.height);
            x = x.saturating_add_unsigned(width);
        }
    }

    /// Show the drives' lamps as `lights` says, adding the lamps that changed
    /// to `damage`.
    pub fn set_lights(&mut self, lights: Lights, damage: &mut Region) {
        let was = core::mem::replace(&mut self.lights, lights);
        for drive in &self.drives {
            if lit(was, drive) != lit(lights, drive) {
                damage.add(drive.slot);
            }
        }
    }

    /// Show diskette drive `unit` holding the image called `name` — read-only
    /// where `read_only` — or, with none, empty.
    pub fn set_held(&mut self, unit: u8, held: Option<(String, bool)>, damage: &mut Region) {
        if let Some(drive) = self
            .drives
            .iter_mut()
            .find(|drive| drive.kind == Kind::Diskette && drive.unit == unit)
        {
            drive.holds = held;
            damage.add(drive.slot);
        }
    }

    /// Show share `slot` as `shown`, or with `None` no longer shared: the
    /// bar laid out again within its bounds and added whole to `damage`.
    pub fn set_share(
        &mut self,
        slot: u8,
        shown: Option<Shared>,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) {
        let at = self
            .drives
            .iter()
            .position(|drive| matches!(drive.kind, Kind::Share(_)) && drive.unit == slot);
        match (at, shown) {
            (Some(at), Some(shown)) => {
                let drive = &mut self.drives[at];
                drive.kind = Kind::Share(shown.drive);
                drive.holds = Some((shown.name, shown.read_only));
            }
            (Some(at), None) => {
                self.drives.remove(at);
                self.pressed = None;
            }
            (None, Some(shown)) => {
                let after = self
                    .drives
                    .iter()
                    .position(|drive| matches!(drive.kind, Kind::Share(_)) && drive.unit > slot);
                let drive = Drive {
                    kind: Kind::Share(shown.drive),
                    unit: slot,
                    holds: Some((shown.name, shown.read_only)),
                    slot: Rect::EMPTY,
                };
                self.drives
                    .insert(after.unwrap_or(self.drives.len()), drive);
                self.pressed = None;
            }
            (None, None) => return,
        }
        self.place(self.bounds, looks);
        damage.add(self.bounds);
    }

    /// Show the machine paused, or running, adding its button to `damage`.
    pub fn set_paused(&mut self, paused: bool, damage: &mut Region) {
        if self.paused != paused {
            self.paused = paused;
            if !self.saving {
                let shown = if paused { PAUSED } else { MACHINE };
                self.machine.set_content(machine_content(shown));
                damage.add(self.machine_at);
            }
        }
    }

    /// Show the machine being kept as it suspends, adding its button to
    /// `damage`: from now on no press asks for a menu.
    pub fn set_saving(&mut self, damage: &mut Region) {
        if !self.saving {
            self.saving = true;
            self.pressed = None;
            self.machine.set_content(machine_content(SAVING));
            self.machine.set_state(ControlState::disabled());
            damage.add(self.machine_at);
        }
    }

    /// Whether the machine has a diskette drive to take an image.
    #[must_use]
    pub fn takes_diskettes(&self) -> bool {
        self.drives.iter().any(|drive| drive.kind == Kind::Diskette)
    }

    /// The diskette drive whose slot holds `point`, or else the first one:
    /// where an image dropped there goes. None for a machine with none.
    #[must_use]
    pub fn diskette_at(&self, point: Point) -> Option<u8> {
        let mut diskettes = self
            .drives
            .iter()
            .filter(|drive| drive.kind == Kind::Diskette);
        let first = diskettes.clone().next().map(|drive| drive.unit);
        diskettes
            .find(|drive| drive.slot.contains(point))
            .map(|drive| drive.unit)
            .or(first)
    }

    /// Paint the bar: its band, each drive's lamp, letter and image.
    pub fn render(&self, surface: &mut Surface, (scale, theme): (Scale, &Theme)) {
        let band = StatusBand::new(theme, scale);
        let lamp = band.face().line_height();
        let palette = theme.palette();
        let texts: Vec<(Rect, String, StatusTone)> = self
            .drives
            .iter()
            .map(|drive| {
                let (text, tone) = shown(drive);
                let slot = drive.slot;
                let after = lamp.saturating_add(lamp / 2);
                let text_slot = Rect::new(
                    slot.left().saturating_add_unsigned(after),
                    slot.top(),
                    slot.width.saturating_sub(after),
                    slot.height,
                );
                (text_slot, text, tone)
            })
            .collect();
        let parts: Vec<(Rect, &str, StatusTone)> = texts
            .iter()
            .map(|(slot, text, tone)| (*slot, text.as_str(), *tone))
            .collect();
        band.paint(surface, self.bounds, theme, &parts);
        self.machine.render(surface, self.machine_at, scale, theme);
        let side = (lamp * 2 / 3).max(2);
        for drive in &self.drives {
            let colour = match (lit(self.lights, drive), drive.kind) {
                (_, Kind::Share(_)) => continue,
                (true, Kind::Diskette) => palette.success,
                (true, Kind::Fixed) => palette.warning,
                (false, _) => palette.on_surface_muted,
            };
            let x = drive.slot.left().saturating_add_unsigned(lamp / 3);
            let y = drive
                .slot
                .top()
                .saturating_add_unsigned(drive.slot.height.saturating_sub(side) / 2);
            if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
                surface.fill_round_rect(x, y, side, side, side / 2, Color::from(colour));
            }
        }
    }

    /// Where the bar lies.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Whether a press on a diskette drive or the machine is held, so its
    /// release is the bar's.
    #[must_use]
    pub const fn is_pressing(&self) -> bool {
        self.pressed.is_some()
    }

    /// What a press at the pointer would begin on.
    fn under_pointer(&self) -> Option<Pressed> {
        if self.saving {
            return None;
        }
        if self.machine_at.contains(self.pointer) {
            return Some(Pressed::Machine);
        }
        self.drives
            .iter()
            .position(|drive| drive.kind != Kind::Fixed && drive.slot.contains(self.pointer))
            .map(Pressed::Drive)
    }

    /// Take `event`, adding what it redrew to `damage`: the menu a diskette
    /// drive's slot, a share's or the machine's button asks for, once pressed
    /// and let go over it with the same button, primary or secondary.
    pub fn on_input(&mut self, event: &InputEvent, damage: &mut Region) -> Option<MenuRequest> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let clicked = self.machine.on_pointer(event, self.machine_at, damage)
            == Some(ButtonAction::Activated);
        match event {
            InputEvent::PointerPressed { button } => {
                self.pressed = self.under_pointer().map(|on| (on, *button));
                None
            }
            InputEvent::PointerReleased { button } => {
                let (pressed, with) = self.pressed.take()?;
                if with != *button || self.under_pointer() != Some(pressed) {
                    return None;
                }
                let opens = match with {
                    PointerButton::Primary => clicked || pressed != Pressed::Machine,
                    PointerButton::Secondary => true,
                    PointerButton::Middle => false,
                };
                if !opens {
                    return None;
                }
                Some(match pressed {
                    Pressed::Machine => MenuRequest {
                        menu: Menu::Machine,
                        anchor: self.machine_at,
                    },
                    Pressed::Drive(n) => {
                        let drive = self.drives.get(n)?;
                        let menu = match drive.kind {
                            Kind::Share(_) => Menu::Share { slot: drive.unit },
                            Kind::Diskette | Kind::Fixed => Menu::Drive {
                                unit: drive.unit,
                                holds: drive.holds.is_some(),
                            },
                        };
                        MenuRequest {
                            menu,
                            anchor: drive.slot,
                        }
                    }
                })
            }
            InputEvent::PointerCancelled => {
                self.pressed = None;
                None
            }
            _ => None,
        }
    }
}

/// What `drive`'s slot reads, and in which tone: a drive's letter and what
/// it holds, an empty diskette drive's and a share without a drive's muted.
fn shown(drive: &Drive) -> (String, StatusTone) {
    let label = label(drive.kind, drive.unit);
    let text = match (&drive.holds, drive.kind) {
        (Some((name, read_only)), Kind::Share(None)) => {
            let kept = if *read_only { ", read-only" } else { "" };
            format!("{name} (no drive{kept})")
        }
        (Some((name, true)), _) => format!("{label} {name} (read-only)"),
        (Some((name, false)), _) => format!("{label} {name}"),
        (None, Kind::Diskette) => format!("{label} empty"),
        (None, Kind::Fixed | Kind::Share(_)) => label,
    };
    let tone = match (drive.kind, &drive.holds) {
        (Kind::Diskette, None) | (Kind::Share(None), _) => StatusTone::Muted,
        _ => StatusTone::Ink,
    };
    (text, tone)
}

/// Whether `drive`'s lamp is lit in `lights`.
fn lit(lights: Lights, drive: &Drive) -> bool {
    let bits = match drive.kind {
        Kind::Diskette => lights.floppy,
        Kind::Fixed => lights.fixed,
        Kind::Share(_) => return false,
    };
    bits & (1 << drive.unit) != 0
}

#[cfg(test)]
#[path = "bar_tests.rs"]
mod tests;
