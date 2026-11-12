//! The dialog a breakpoint is added through: what stops the machine — an
//! instruction executed, memory written or read, a port, an interrupt — and
//! where, each field checked as the user typed it before anything is held.

use alloc::format;
use alloc::string::String;
use alloc::vec;

use tairix_controls::{
    Button, ButtonContent, ControlRole, ControlState, Dialog, DialogAction, FieldAction,
    FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow, Keystroke, TextField,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::Entry;
use crate::app::address::{self, MOST_TYPED};
use crate::cpu::{linear, Regs, Seg};
use crate::debug::probe::{Access, Breakpoint, Direction, SPACE};

/// The dialog's title.
pub const TITLE: &str = "Add Breakpoint";

const WIDTH: u32 = 460;

/// What can stop the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Execute,
    Write,
    Access,
    Port(Direction),
    Interrupt,
}

const KINDS: [(Kind, &str); 7] = [
    (Kind::Execute, "Executing an instruction"),
    (Kind::Write, "Writing memory"),
    (Kind::Access, "Reading or writing memory"),
    (Kind::Port(Direction::In), "Reading a port"),
    (Kind::Port(Direction::Out), "Writing a port"),
    (Kind::Port(Direction::Both), "Reading or writing a port"),
    (Kind::Interrupt, "Entering an interrupt"),
];

/// The rows: the kind, then what it is at and what narrows it.
mod row {
    pub const KIND: usize = 0;
    pub const AT: usize = 1;
    pub const DETAIL: usize = 2;
}

impl Kind {
    /// The two fields' labels; none where a kind has no second.
    const fn labels(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Execute => ("Address", None),
            Self::Write | Self::Access => ("Address", Some("Bytes")),
            Self::Port(_) => ("Port", Some("Through port")),
            Self::Interrupt => ("Vector", Some("Function (AH)")),
        }
    }
}

/// What the dialog answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Nothing is added.
    Cancel,
    /// This is.
    Add(Entry),
}

/// The dialog's state.
#[derive(Debug)]
pub struct Adding {
    dialog: Dialog,
    group: FieldGroup,
    kind: Kind,
    /// The registers an address naming one reads.
    regs: Regs,
    pointer: Point,
    /// The press under way began on an open choice list, so none of it is
    /// the actions'.
    list_press: bool,
}

fn field() -> TextField {
    TextField::new().with_max_len(MOST_TYPED)
}

fn text_row(label: &str) -> FieldRow {
    FieldRow::new(label, FieldControl::Text(field()))
}

/// The detail row of `kind`: a field, or one standing disabled.
fn detail_row(kind: Kind) -> FieldRow {
    let (_, detail) = kind.labels();
    let mut row = text_row(detail.unwrap_or("\u{2014}"));
    if detail.is_none() {
        row.set_state(ControlState::disabled());
    }
    row
}

/// A hexadecimal number no greater than `most`, an `h` after it allowed.
fn hexadecimal(text: &str, most: u32) -> Option<u32> {
    let digits = text.trim();
    let digits = digits.strip_suffix(['h', 'H']).unwrap_or(digits);
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16)
        .ok()
        .filter(|&value| value <= most)
}

impl Adding {
    /// The dialog, an address naming a register reading `regs`.
    #[must_use]
    pub fn new(regs: Regs) -> Self {
        let kind = Kind::Execute;
        let names: vec::Vec<&str> = KINDS.iter().map(|(_, name)| *name).collect();
        let group = FieldGroup::new(
            "STOP WHEN",
            vec![
                FieldRow::choice("Kind", &names, 0),
                text_row(kind.labels().0),
                detail_row(kind),
            ],
        );
        let actions = vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Add")),
                ControlRole::Recommended,
            ),
        ];
        Self {
            dialog: Dialog::new(TITLE).in_window().with_actions(actions),
            group,
            kind,
            regs,
            pointer: Point::ORIGIN,
            list_press: false,
        }
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let column =
            FieldGroup::shared_column(core::slice::from_ref(&self.group), band, scale, theme);
        let content = self.group.measured_height(band, column, scale, theme);
        (
            width,
            self.dialog.height_for_content(content, width, scale, theme),
        )
    }

    fn layout(&self, scale: Scale, theme: &Theme) -> (Rect, FieldLayout) {
        let (width, height) = self.extent(scale, theme);
        let bounds = Rect::new(0, 0, width, height);
        let content = self
            .dialog
            .content_rect(bounds, scale, theme)
            .unwrap_or(Rect::EMPTY);
        let column = FieldGroup::shared_column(
            core::slice::from_ref(&self.group),
            content.width,
            scale,
            theme,
        );
        let height = self
            .group
            .measured_height(content.width, column, scale, theme);
        let rect = Rect::new(content.left(), content.top(), content.width, height);
        // A choice list may hang anywhere in the client, over the actions
        // included, since it is modal.
        let layout = FieldLayout::new(rect, column).within(bounds);
        (bounds, self.group.place_popup(layout, scale, theme))
    }

    /// Paints the whole client.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        let (bounds, layout) = self.layout(scale, theme);
        self.dialog.render(surface, bounds, scale, theme);
        self.group.render(surface, layout, scale, theme);
        self.group.render_popup(surface, layout.popup, scale, theme);
    }

    fn listing(&self) -> bool {
        self.group.rows().iter().any(FieldRow::popup_open)
    }

    /// Takes `event` at monotonic `at_ns`: the answer, once there is one.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        let (bounds, layout) = self.layout(scale, theme);
        let listing = self.listing();
        if let Some(stroke) = Keystroke::pressed(*event, at_ns) {
            if stroke.key == Key::Named(NamedKey::Escape) && !listing {
                return Some(Answer::Cancel);
            }
            if let Some(action) = self.group.on_key(stroke, layout, scale, theme, damage) {
                self.take(&action, bounds, damage);
                return None;
            }
            if listing {
                return None;
            }
            return match self.dialog.on_key(stroke.key) {
                Some(DialogAction::ActionActivated { index }) => self.answer(index, damage, bounds),
                _ => None,
            };
        }
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if matches!(event, InputEvent::PointerPressed { .. }) {
            // A press that takes an open list away is not also a press on
            // whatever the list hung over.
            self.list_press = listing;
        }
        if let Some(action) = self.group.on_pointer(event, layout, scale, theme, damage) {
            self.take(&action, bounds, damage);
        }
        let to_actions = !listing && !self.list_press;
        if matches!(
            event,
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
        ) {
            self.list_press = false;
        }
        if !to_actions {
            return None;
        }
        match self.dialog.on_pointer(event, bounds, scale, theme, damage) {
            Some(DialogAction::ActionActivated { index }) => self.answer(index, damage, bounds),
            _ => None,
        }
    }

    fn take(&mut self, action: &FieldGroupAction, whole: Rect, damage: &mut Region) {
        if let (row::KIND, FieldAction::Selected { index }) = (action.row, &action.action) {
            let Some(&(kind, _)) = KINDS.get(*index) else {
                return;
            };
            self.kind = kind;
            let rows = self.group.rows_mut();
            if let Some(at) = rows.get_mut(row::AT) {
                *at = text_row(kind.labels().0);
            }
            if let Some(detail) = rows.get_mut(row::DETAIL) {
                *detail = detail_row(kind);
            }
            damage.add(whole);
        }
    }

    /// The text of row `n`.
    fn typed(&self, n: usize) -> &str {
        match self.group.rows().get(n).map(FieldRow::control) {
            Some(FieldControl::Text(field)) => field.text(),
            _ => "",
        }
    }

    fn answer(&mut self, index: usize, damage: &mut Region, whole: Rect) -> Option<Answer> {
        if index == 0 {
            return Some(Answer::Cancel);
        }
        match self.entry() {
            Ok(entry) => Some(Answer::Add(entry)),
            Err(why) => {
                self.dialog = self.dialog.clone().with_message(why);
                damage.add(whole);
                None
            }
        }
    }

    /// The breakpoint the fields name, or what is wrong with them.
    fn entry(&self) -> Result<Entry, &'static str> {
        let (at, detail) = (self.typed(row::AT), self.typed(row::DETAIL).trim());
        match self.kind {
            Kind::Execute | Kind::Write | Kind::Access => {
                let segment = if self.kind == Kind::Execute {
                    self.regs.sreg(Seg::Cs)
                } else {
                    self.regs.sreg(Seg::Ds)
                };
                let (segment, offset) = address::parse(at, &self.regs)
                    .ok_or("The address is not one")?
                    .resolve(segment);
                let first = linear(segment, offset);
                let label = format!("{segment:04X}:{offset:04X}");
                let breakpoint = match self.kind {
                    Kind::Execute => Breakpoint::Code(first),
                    kind => {
                        let len = if detail.is_empty() {
                            1
                        } else {
                            detail
                                .parse::<u32>()
                                .ok()
                                .filter(|len| (1..=0x1_0000).contains(len))
                                .ok_or("Bytes must be from 1 to 65536")?
                        };
                        if len > SPACE - first {
                            return Err("The bytes run past the end of memory");
                        }
                        let access = if kind == Kind::Write {
                            Access::Write
                        } else {
                            Access::Any
                        };
                        Breakpoint::Memory { first, len, access }
                    }
                };
                Ok(Entry {
                    breakpoint,
                    at: label,
                })
            }
            Kind::Port(direction) => {
                let first = hexadecimal(at, 0xFFFF).ok_or("The port is not a number to FFFFh")?;
                let last = if detail.is_empty() {
                    first
                } else {
                    hexadecimal(detail, 0xFFFF)
                        .filter(|&last| last >= first)
                        .ok_or("The last port must be a number from the first to FFFFh")?
                };
                Ok(Entry {
                    breakpoint: Breakpoint::Port {
                        first: u16::try_from(first).map_err(|_| "The port is not one")?,
                        last: u16::try_from(last).map_err(|_| "The port is not one")?,
                        direction,
                    },
                    at: String::new(),
                })
            }
            Kind::Interrupt => {
                let vector = hexadecimal(at, 0xFF).ok_or("The vector is not a number to FFh")?;
                let ah = if detail.is_empty() {
                    None
                } else {
                    Some(hexadecimal(detail, 0xFF).ok_or("The function is not a number to FFh")?)
                };
                let byte = |n: u32| u8::try_from(n).map_err(|_| "Not a byte");
                Ok(Entry {
                    breakpoint: Breakpoint::Interrupt {
                        vector: byte(vector)?,
                        ah: ah.map(byte).transpose()?,
                    },
                    at: String::new(),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "adding_tests.rs"]
mod tests;
