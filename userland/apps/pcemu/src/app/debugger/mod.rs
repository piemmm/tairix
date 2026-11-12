//! A machine's debugger window (`plans/PCEMU.md` §3.1): its processor, its
//! 8087, its code, memory and stack as they stood at the last stop, its
//! breakpoints, and the controls that stop it and send it on. Everything
//! shown was read and written out by the worker; the window lays it out,
//! asks for what it shows and hands its commands on, waiting on neither —
//! a look asked while another is out is asked again once that is answered,
//! the latest view winning.

mod adding;
pub mod shown;

pub use adding::{Adding, Answer as Added, TITLE as ADDING_TITLE};

use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{Button, ButtonAction, ControlState, Keystroke, TextAction, TextField};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use super::address;
use crate::cpu::{linear, Regs, Seg};
use crate::debug::probe::{Breakpoint, Go, Reason};
use crate::debug::Registers;
use crate::link::debug::{Look, Seen, MOST_AFTER, MOST_BEFORE, MOST_MEMORY, MOST_STACK};

/// The window's title, after the machine's name.
pub const TITLE: &str = "Debugger";

/// What the window asks of the machine it debugs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Stop it.
    Break,
    /// Send it on.
    Go(Go),
    /// Hold a breakpoint.
    Add(Breakpoint),
    /// Hold one no more.
    Remove(Breakpoint),
    /// Read what the window shows.
    Look(Look),
    /// Open the dialog that adds a breakpoint.
    AddBreakpoint,
}

/// A breakpoint, and the address it was named by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The breakpoint.
    pub breakpoint: Breakpoint,
    /// Its address as `segment:offset`, or empty for one with none.
    pub at: String,
}

/// A listing line as the worker wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    ip: u16,
    bytes: Vec<u8>,
    text: String,
}

/// The controls along the window's top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Continue,
    Break,
    Step,
    StepOver,
    StepOut,
    Add,
}

const CONTROLS: [(Control, &str); 6] = [
    (Control::Continue, "Continue"),
    (Control::Break, "Break"),
    (Control::Step, "Step"),
    (Control::StepOver, "Step Over"),
    (Control::StepOut, "Step Out"),
    (Control::Add, "Add Breakpoint\u{2026}"),
];

/// The address fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Code,
    Memory,
}

/// Bytes a memory row shows.
const ROW_BYTES: u16 = 16;
/// Lines a wheel's detent scrolls.
const DETENT_LINES: i32 = 3;
/// Cells the listing's gutter takes: the breakpoint mark and a space.
const GUTTER_CELLS: u32 = 2;

/// Where everything lies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Layout {
    buttons: [Rect; 6],
    status: Rect,
    fields: [Rect; 2],
    code: Rect,
    memory: Rect,
    registers: Rect,
    stack: Rect,
    breakpoints: Rect,
    /// A text line's height and a cell's width.
    line: u32,
    cell: u32,
}

impl Layout {
    /// The lines `pane` holds below its heading.
    const fn rows(&self, pane: Rect) -> u32 {
        pane.height.saturating_sub(self.line) / if self.line == 0 { 1 } else { self.line }
    }

    /// The rect of line `n` of `pane`'s body.
    fn row(&self, pane: Rect, n: u32) -> Rect {
        let top = pane
            .top()
            .saturating_add_unsigned(self.line.saturating_mul(n.saturating_add(1)));
        Rect::new(pane.left(), top, pane.width, self.line)
    }

    /// The body line of `pane` `point` lies on.
    fn row_at(&self, pane: Rect, point: Point) -> Option<u32> {
        if !pane.contains(point) || self.line == 0 {
            return None;
        }
        let down = u32::try_from(point.y - pane.top()).ok()?;
        (down / self.line).checked_sub(1)
    }
}

/// Whether a look is out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Looking {
    /// None is.
    Idle,
    /// One is.
    Out,
    /// One is, and another is wanted once it is answered.
    Stale,
}

/// The window's state.
pub struct Debugger {
    fitted: bool,
    running: bool,
    reason: Option<Reason>,
    registers: Option<Registers>,
    /// The registers at the stop before, which what changed is shown against.
    before: Option<Registers>,
    /// Where the listing is read from, and what it read.
    code: (u16, u16),
    listing: Vec<Shown>,
    /// The listing's lines before the one at `code`.
    leading: usize,
    /// Where memory is read from, and what it held.
    memory: Option<(u16, u16)>,
    bytes: Vec<u8>,
    /// SS:SP at the last look, and the words there.
    stack_at: (u16, u16),
    stack: Vec<u8>,
    entries: Vec<Entry>,
    looking: Looking,
    buttons: [Button; 6],
    fields: [TextField; 2],
    focus: Option<Field>,
    pointer: Point,
    size: (u32, u32),
}

impl Debugger {
    /// The window for a machine with an 8087 where `fitted`, `size` pixels
    /// large, its machine running.
    #[must_use]
    pub fn new(fitted: bool, size: (u32, u32)) -> Self {
        let mut debugger = Self {
            fitted,
            running: true,
            reason: None,
            registers: None,
            before: None,
            code: (0, 0),
            listing: Vec::new(),
            leading: 0,
            memory: None,
            bytes: Vec::new(),
            stack_at: (0, 0),
            stack: Vec::new(),
            entries: Vec::new(),
            looking: Looking::Idle,
            buttons: CONTROLS.map(|(_, label)| Button::labelled(label)),
            fields: [
                TextField::new().with_placeholder("CS:IP"),
                TextField::new().with_placeholder("DS:0000"),
            ],
            focus: None,
            pointer: Point::ORIGIN,
            size,
        };
        debugger.enable();
        debugger
    }

    /// The breakpoints held, in the order they were added.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The registers at the last stop, which an address naming one reads.
    #[must_use]
    pub fn registers(&self) -> Option<Regs> {
        self.registers.as_ref().map(|registers| registers.regs)
    }

    /// Whether the machine runs, as far as the window knows.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    fn enable(&mut self) {
        for ((control, _), button) in CONTROLS.iter().zip(&mut self.buttons) {
            let usable = match control {
                Control::Break => self.running,
                Control::Add => true,
                _ => !self.running,
            };
            button.set_state(if usable {
                ControlState::default()
            } else {
                ControlState::disabled()
            });
        }
    }

    fn font(scale: Scale, theme: &Theme) -> BitmapFont {
        BitmapFont::for_role(theme.fonts(), TextRole::Monospace, scale)
    }

    /// Where everything lies at `size`.
    fn layout(&self, (scale, theme): (Scale, &Theme)) -> Layout {
        let font = Self::font(scale, theme);
        let (line, cell) = (font.line_height().max(1), font.cell_width().max(1));
        let gap = scale.scale_length(theme.metrics().control_gap);
        let height = Button::height(scale, theme);
        let (width, tall) = self.size;
        let mut x = gap;
        let buttons = core::array::from_fn(|n| {
            let width = self.buttons[n].measured_width(scale, theme);
            let rect = Rect::new(
                i32::try_from(x).unwrap_or(0),
                i32::try_from(gap).unwrap_or(0),
                width,
                height,
            );
            x = x.saturating_add(width).saturating_add(gap);
            rect
        });
        let top = gap.saturating_mul(2).saturating_add(height);
        let status = Rect::new(
            i32::try_from(x).unwrap_or(0),
            i32::try_from(gap).unwrap_or(0),
            width.saturating_sub(x).saturating_sub(gap),
            height,
        );
        let left_width = width.saturating_mul(3) / 5;
        let below = tall.saturating_sub(top).saturating_sub(gap);
        let column = |left: u32, width: u32, share: u32, at: u32| {
            Rect::new(
                i32::try_from(left).unwrap_or(0),
                i32::try_from(top.saturating_add(at)).unwrap_or(0),
                width,
                below.saturating_mul(share) / 100,
            )
        };
        let left = gap;
        let lw = left_width.saturating_sub(gap.saturating_mul(2));
        let right = left_width;
        let rw = width.saturating_sub(left_width).saturating_sub(gap);
        let code_share = 62;
        let code = column(left, lw, code_share, 0);
        let memory_top = code.height.saturating_add(gap);
        let memory = Rect::new(
            code.left(),
            code.top().saturating_add_unsigned(memory_top),
            lw,
            below.saturating_sub(memory_top),
        );
        let fpu_lines = if self.fitted { 10 } else { 0 };
        let registers = Rect::new(
            i32::try_from(right).unwrap_or(0),
            code.top(),
            rw,
            line.saturating_mul(6 + fpu_lines),
        );
        let rest_top = registers.height.saturating_add(gap);
        let rest = below.saturating_sub(rest_top);
        let stack = Rect::new(
            registers.left(),
            registers.top().saturating_add_unsigned(rest_top),
            rw,
            rest / 2,
        );
        let breakpoints = Rect::new(
            registers.left(),
            stack
                .top()
                .saturating_add_unsigned(stack.height.saturating_add(gap)),
            rw,
            rest.saturating_sub(stack.height).saturating_sub(gap),
        );
        let field = |pane: Rect| {
            let at = cell.saturating_mul(9);
            Rect::new(
                pane.left().saturating_add_unsigned(at),
                pane.top(),
                pane.width.saturating_sub(at).min(cell.saturating_mul(18)),
                line,
            )
        };
        Layout {
            buttons,
            status,
            fields: [field(code), field(memory)],
            code,
            memory,
            registers,
            stack,
            breakpoints,
            line,
            cell,
        }
    }

    /// What a look at the window as it lies asks for.
    fn look(&self, layout: &Layout) -> Look {
        let rows = layout.rows(layout.code);
        let before = u8::try_from(rows / 4)
            .unwrap_or(MOST_BEFORE)
            .min(MOST_BEFORE);
        let after = u8::try_from(rows.saturating_sub(u32::from(before)))
            .unwrap_or(MOST_AFTER)
            .clamp(1, MOST_AFTER);
        let memory_rows = u16::try_from(layout.rows(layout.memory)).unwrap_or(u16::MAX);
        let bytes = memory_rows.saturating_mul(ROW_BYTES).min(MOST_MEMORY);
        let stack = u8::try_from(layout.rows(layout.stack))
            .unwrap_or(MOST_STACK)
            .min(MOST_STACK);
        Look {
            number: 0,
            code: self.code,
            lines: (before, after),
            memory: self.memory.unwrap_or_default(),
            bytes,
            stack,
        }
    }

    /// A look, where the processor is stopped and none is out; one wanted
    /// once the one out is answered otherwise.
    fn want(&mut self, looks: (Scale, &Theme)) -> Option<Ask> {
        if self.running {
            return None;
        }
        if self.looking != Looking::Idle {
            self.looking = Looking::Stale;
            return None;
        }
        self.looking = Looking::Out;
        Some(Ask::Look(self.look(&self.layout(looks))))
    }

    /// The window is `size` pixels large now: a look, if it shows more or
    /// less than it did.
    pub fn resize(&mut self, size: (u32, u32), looks: (Scale, &Theme)) -> Option<Ask> {
        let shown = self.look(&self.layout(looks));
        self.size = size;
        (self.look(&self.layout(looks)) != shown)
            .then(|| self.want(looks))
            .flatten()
    }

    /// The whole window, as damage.
    fn whole(&self, damage: &mut Region) {
        damage.add(Rect::new(0, 0, self.size.0, self.size.1));
    }

    /// The processor stopped, `reason` why: the window shows where, asking
    /// for its code from there.
    pub fn stopped(
        &mut self,
        reason: Reason,
        registers: Registers,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        self.whole(damage);
        let regs = registers.regs;
        self.code = (regs.sreg(Seg::Cs), regs.ip);
        if self.memory.is_none() {
            self.memory = Some((regs.sreg(Seg::Ds), 0));
        }
        self.before = self.registers.replace(registers);
        self.reason = Some(reason);
        self.running = false;
        self.enable();
        self.want(looks)
    }

    /// The machine was reset: it runs, and no look at the stop it left is
    /// answered.
    pub fn runs(&mut self, damage: &mut Region) {
        self.running = true;
        self.looking = Looking::Idle;
        self.enable();
        self.whole(damage);
    }

    /// The look asked could not be sent: none is out.
    pub fn unanswered(&mut self) {
        self.looking = Looking::Idle;
    }

    /// A look's answer: shown, and the look wanted meanwhile asked.
    pub fn seen(
        &mut self,
        seen: &Seen<'_>,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        let layout = self.layout(looks);
        for pane in [layout.code, layout.memory, layout.stack] {
            damage.add(pane);
        }
        let stale = core::mem::replace(&mut self.looking, Looking::Idle) == Looking::Stale;
        let listing = seen.listing;
        self.listing = listing
            .lines()
            .map(|line| Shown {
                ip: line.ip,
                bytes: line.bytes.to_vec(),
                text: String::from(line.text),
            })
            .collect();
        self.leading = usize::from(listing.before);
        self.bytes = seen.memory.to_vec();
        if let Some(regs) = self.registers.as_ref().map(|registers| registers.regs) {
            self.stack_at = (regs.sreg(Seg::Ss), regs.sp());
        }
        self.stack = seen.stack.to_vec();
        if stale {
            return self.want(looks);
        }
        None
    }

    /// Holds `entry`, the dialog having made it: the breakpoint to add,
    /// unless it is held already.
    pub fn add(
        &mut self,
        entry: Entry,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        let layout = self.layout(looks);
        damage.add(layout.breakpoints);
        damage.add(layout.code);
        self.hold(entry)
    }

    fn hold(&mut self, entry: Entry) -> Option<Ask> {
        if self
            .entries
            .iter()
            .any(|held| held.breakpoint == entry.breakpoint)
        {
            return None;
        }
        let ask = Ask::Add(entry.breakpoint);
        self.entries.push(entry);
        Some(ask)
    }

    fn remove(&mut self, at: usize) -> Option<Ask> {
        (at < self.entries.len()).then(|| Ask::Remove(self.entries.remove(at).breakpoint))
    }

    /// Adds a code breakpoint at listing line `n`, or removes the one there.
    fn toggle_line(&mut self, n: usize) -> Option<Ask> {
        let line = self.listing.get(n)?;
        let (cs, ip) = (self.code.0, line.ip);
        let breakpoint = Breakpoint::Code(linear(cs, ip));
        match self
            .entries
            .iter()
            .position(|entry| entry.breakpoint == breakpoint)
        {
            Some(at) => self.remove(at),
            None => self.hold(Entry {
                breakpoint,
                at: alloc::format!("{cs:04X}:{ip:04X}"),
            }),
        }
    }

    /// The physical address `addr` as the window writes it: in the segment
    /// a watch over it was named in, where one was, else as a physical
    /// address.
    fn place(&self, addr: u32) -> String {
        self.entries
            .iter()
            .find_map(|entry| {
                let (segment, _) = entry.at.split_once(':')?;
                let segment = u16::from_str_radix(segment, 16).ok()?;
                let offset = addr.checked_sub(u32::from(segment) << 4)?;
                let offset = u16::try_from(offset).ok()?;
                Some(alloc::format!("{segment:04X}:{offset:04X}"))
            })
            .unwrap_or_else(|| alloc::format!("{addr:05X}h"))
    }

    /// The status line: running, or why it stopped.
    fn status(&self) -> String {
        match (&self.reason, &self.registers, self.running) {
            (Some(reason), Some(registers), false) => {
                shown::reason(reason, registers, |addr| self.place(addr))
            }
            _ => String::from("Running"),
        }
    }

    /// Sends the machine on as `go` says.
    fn go(&mut self, go: Go) -> Option<Ask> {
        if self.running {
            return None;
        }
        self.running = true;
        self.enable();
        Some(Ask::Go(go))
    }

    fn control(&mut self, control: Control) -> Option<Ask> {
        match control {
            Control::Continue => self.go(Go::Continue),
            Control::Step => self.go(Go::Step),
            Control::StepOver => self.go(Go::StepOver),
            Control::StepOut => self.go(Go::StepOut),
            Control::Break => self.running.then_some(Ask::Break),
            Control::Add => Some(Ask::AddBreakpoint),
        }
    }

    /// Takes `event` at monotonic `at_ns`: what it asks of the machine, if
    /// anything. What it changed is added to `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        let layout = self.layout(looks);
        if let Some(stroke) = Keystroke::pressed(*event, at_ns) {
            return self.on_key(stroke.key, stroke.modifiers, &layout, looks, damage);
        }
        match *event {
            InputEvent::PointerMoved { to } => self.pointer = to,
            InputEvent::PointerScrolled { dy, .. } => {
                let ask = self.scroll(dy, &layout, looks);
                return ask;
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if let Some(ask) = self.press(&layout, damage) {
                    return Some(ask);
                }
            }
            _ => {}
        }
        for (n, field) in self.fields.iter_mut().enumerate() {
            field.on_pointer(event, layout.fields[n], looks.0, looks.1, damage);
        }
        let mut ask = None;
        for (n, button) in self.buttons.iter_mut().enumerate() {
            if button.on_pointer(event, layout.buttons[n], damage) == Some(ButtonAction::Activated)
            {
                ask = Some(CONTROLS[n].0);
            }
        }
        let ask = ask.and_then(|control| self.control(control));
        if ask.is_some() {
            damage.add(Rect::new(0, 0, self.size.0, self.size.1));
        }
        ask
    }

    /// A press: focuses the field under the pointer, or acts on the pane
    /// line it lies on.
    fn press(&mut self, layout: &Layout, damage: &mut Region) -> Option<Ask> {
        let focus = [Field::Code, Field::Memory]
            .into_iter()
            .zip(layout.fields)
            .find(|(_, rect)| rect.contains(self.pointer))
            .map(|(field, _)| field);
        self.set_focus(focus, layout, damage);
        if focus.is_some() {
            return None;
        }
        let gutter = layout.cell.saturating_mul(GUTTER_CELLS);
        if let Some(n) = layout.row_at(layout.code, self.pointer) {
            let x = u32::try_from(self.pointer.x - layout.code.left()).unwrap_or(u32::MAX);
            if x < gutter {
                damage.add(layout.code);
                damage.add(layout.breakpoints);
                return self.toggle_line(usize::try_from(n).ok()?);
            }
        }
        if let Some(n) = layout.row_at(layout.breakpoints, self.pointer) {
            let mark = layout.breakpoints.right().saturating_sub_unsigned(gutter);
            if self.pointer.x >= mark {
                damage.add(layout.code);
                damage.add(layout.breakpoints);
                return self.remove(usize::try_from(n).ok()?);
            }
        }
        None
    }

    fn set_focus(&mut self, focus: Option<Field>, layout: &Layout, damage: &mut Region) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        for (n, field) in self.fields.iter_mut().enumerate() {
            field.set_focused(focus == Some([Field::Code, Field::Memory][n]));
            damage.add(layout.fields[n]);
        }
    }

    /// The wheel turned `dy` scroll units over the pointer: the listing or
    /// memory moves by lines.
    fn scroll(&mut self, dy: i32, layout: &Layout, looks: (Scale, &Theme)) -> Option<Ask> {
        if self.running || dy == 0 {
            return None;
        }
        let detent = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT.max(1);
        let lines = (dy.saturating_mul(DETENT_LINES) / detent).clamp(-64, 64);
        let lines = if lines == 0 { dy.signum() } else { lines };
        if layout.code.contains(self.pointer) {
            let target = self
                .leading
                .saturating_add_signed(isize::try_from(lines).unwrap_or(0));
            let line = self.listing.get(target).or_else(|| {
                if lines < 0 {
                    self.listing.first()
                } else {
                    self.listing.last()
                }
            })?;
            self.code.1 = line.ip;
        } else if layout.memory.contains(self.pointer) {
            let (segment, offset) = self.memory?;
            let step = i32::from(ROW_BYTES).saturating_mul(lines);
            let offset = offset.wrapping_add_signed(i16::try_from(step).unwrap_or(0));
            self.memory = Some((segment, offset));
        } else {
            return None;
        }
        self.want(looks)
    }

    fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        if let Some(focus) = self.focus {
            let n = usize::from(focus == Field::Memory);
            let action = self.fields[n].on_key(key, modifiers, layout.fields[n], damage);
            return match action {
                Some(TextAction::Submitted) => self.navigate(focus, layout, looks, damage),
                Some(TextAction::Cancelled) => {
                    self.set_focus(None, layout, damage);
                    None
                }
                _ => None,
            };
        }
        let shift = modifiers.shift;
        let ask = match key {
            Key::Named(NamedKey::Function { number: 5 }) if self.running => Some(Ask::Break),
            Key::Named(NamedKey::Function { number: 5 }) => self.go(Go::Continue),
            Key::Named(NamedKey::Function { number: 10 }) => self.go(Go::StepOver),
            Key::Named(NamedKey::Function { number: 11 }) if shift => self.go(Go::StepOut),
            Key::Named(NamedKey::Function { number: 11 }) => self.go(Go::Step),
            Key::Named(NamedKey::Function { number: 9 }) => self.toggle_line(self.leading),
            _ => None,
        };
        if ask.is_some() {
            damage.add(Rect::new(0, 0, self.size.0, self.size.1));
        }
        ask
    }

    /// Goes where field `field` names: its pane reads from there.
    fn navigate(
        &mut self,
        field: Field,
        layout: &Layout,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Ask> {
        let regs = self.registers.as_ref()?.regs;
        let n = usize::from(field == Field::Memory);
        let typed = self.fields[n].text();
        let Some(place) = address::parse(typed, &regs) else {
            self.fields[n].set_message(Some(String::from("Not an address")));
            damage.add(layout.fields[n]);
            return None;
        };
        self.fields[n].set_message(None);
        let at = match field {
            Field::Code => {
                self.code = place.resolve(self.code.0);
                self.code
            }
            Field::Memory => {
                let segment = self
                    .memory
                    .map_or(regs.sreg(Seg::Ds), |(segment, _)| segment);
                let at = place.resolve(segment);
                self.memory = Some(at);
                at
            }
        };
        self.fields[n].set_text(alloc::format!("{:04X}:{:04X}", at.0, at.1));
        self.set_focus(None, layout, damage);
        self.want(looks)
    }

    /// Paints what `damage` covers.
    pub fn render(&self, surface: &mut Surface, damage: &Region, (scale, theme): (Scale, &Theme)) {
        let layout = self.layout((scale, theme));
        let palette = theme.palette();
        let font = Self::font(scale, theme);
        let paints = |rect: Rect| damage.intersects(rect);
        let whole = Rect::new(0, 0, self.size.0, self.size.1);
        if paints(whole) {
            fill(
                surface,
                whole.intersection(&damage.bounds()),
                Color::from(palette.surface),
            );
        }
        for (n, button) in self.buttons.iter().enumerate() {
            if paints(layout.buttons[n]) {
                button.render(surface, layout.buttons[n], scale, theme);
            }
        }
        let ink = Color::from(if self.running {
            palette.on_surface_muted
        } else {
            palette.on_surface
        });
        if paints(layout.status) {
            let text = self.status();
            let shown = font.elide_to_width(&text, layout.status.width).0;
            let y = font.centred_top(layout.status.top(), layout.status.height);
            font.draw_text(
                surface,
                layout.status.left(),
                y,
                shown,
                Color::from(palette.on_surface),
            );
        }
        let pane = Pane {
            layout: &layout,
            font,
            ink,
            muted: Color::from(palette.on_surface_muted),
            changed: Color::from(palette.accent),
            plate: Color::from(palette.surface_raised),
            mark: Color::from(palette.danger),
            current: Color::from(palette.selection_fill),
        };
        if paints(layout.code) {
            self.render_code(surface, &pane);
        }
        if paints(layout.memory) {
            self.render_memory(surface, &pane);
        }
        if paints(layout.registers) {
            self.render_registers(surface, &pane);
        }
        if paints(layout.stack) {
            self.render_stack(surface, &pane);
        }
        if paints(layout.breakpoints) {
            self.render_breakpoints(surface, &pane);
        }
        for (n, field) in self.fields.iter().enumerate() {
            if paints(layout.fields[n]) {
                field.render(surface, layout.fields[n], scale, theme);
            }
        }
    }

    fn render_code(&self, surface: &mut Surface, pane: &Pane<'_>) {
        let rect = pane.layout.code;
        pane.plate(surface, rect, "Code");
        let rows = pane.layout.rows(rect);
        let (cs, current) = (self.code.0, self.current_ip());
        for (n, line) in (0..rows).zip(&self.listing) {
            let row = pane.layout.row(rect, n);
            if Some(line.ip) == current {
                fill(surface, row, pane.current);
            }
            if self
                .entries
                .iter()
                .any(|entry| entry.breakpoint == Breakpoint::Code(linear(cs, line.ip)))
            {
                pane.text(surface, row, 0, "\u{25cf}", pane.mark);
            }
            let text = shown::code_row(cs, line.ip, &line.bytes, &line.text);
            pane.text(surface, row, GUTTER_CELLS, &text, pane.ink);
        }
    }

    /// The offset the processor stopped at, where its code segment is the
    /// one listed.
    fn current_ip(&self) -> Option<u16> {
        self.registers
            .as_ref()
            .filter(|registers| !self.running && registers.regs.sreg(Seg::Cs) == self.code.0)
            .map(|registers| registers.regs.ip)
    }

    fn render_memory(&self, surface: &mut Surface, pane: &Pane<'_>) {
        let rect = pane.layout.memory;
        pane.plate(surface, rect, "Memory");
        let (segment, offset) = self.memory.unwrap_or_default();
        for (n, chunk) in (0..pane.layout.rows(rect)).zip(self.bytes.chunks(usize::from(ROW_BYTES)))
        {
            let at = offset.wrapping_add(u16::try_from(n).unwrap_or(0).wrapping_mul(ROW_BYTES));
            let text = shown::memory_row(segment, at, chunk);
            pane.text(surface, pane.layout.row(rect, n), 0, &text, pane.ink);
        }
    }

    fn render_registers(&self, surface: &mut Surface, pane: &Pane<'_>) {
        let rect = pane.layout.registers;
        pane.plate(surface, rect, "Registers");
        let Some(registers) = &self.registers else {
            return;
        };
        let mut rows = shown::registers(registers, self.before.as_ref());
        if let Some(fpu) = &registers.coprocessor {
            let before = self
                .before
                .as_ref()
                .and_then(|before| before.coprocessor.as_ref());
            rows.extend(shown::coprocessor(fpu, before));
        }
        for (n, row) in (0..pane.layout.rows(rect)).zip(&rows) {
            pane.spans(surface, pane.layout.row(rect, n), row);
        }
    }

    fn render_stack(&self, surface: &mut Surface, pane: &Pane<'_>) {
        let rect = pane.layout.stack;
        pane.plate(surface, rect, "Stack");
        let (segment, sp) = self.stack_at;
        for (n, pair) in (0..pane.layout.rows(rect)).zip(self.stack.as_chunks::<2>().0) {
            let word = u16::from_le_bytes(*pair);
            let offset = sp.wrapping_add(u16::try_from(n * 2).unwrap_or(0));
            let text = shown::stack_row(segment, offset, word);
            pane.text(surface, pane.layout.row(rect, n), 0, &text, pane.ink);
        }
    }

    fn render_breakpoints(&self, surface: &mut Surface, pane: &Pane<'_>) {
        let rect = pane.layout.breakpoints;
        pane.plate(surface, rect, "Breakpoints");
        let mark = rect.width / pane.layout.cell.max(1);
        for (n, entry) in (0..pane.layout.rows(rect)).zip(&self.entries) {
            let row = pane.layout.row(rect, n);
            let text = shown::breakpoint(&entry.breakpoint, &entry.at);
            pane.text(surface, row, 0, &text, pane.ink);
            pane.text(
                surface,
                row,
                mark.saturating_sub(GUTTER_CELLS),
                "\u{2715}",
                pane.muted,
            );
        }
    }
}

/// What a pane is painted with.
struct Pane<'a> {
    layout: &'a Layout,
    font: BitmapFont,
    ink: Color,
    muted: Color,
    changed: Color,
    plate: Color,
    mark: Color,
    current: Color,
}

impl Pane<'_> {
    /// The pane's plate and its heading.
    fn plate(&self, surface: &mut Surface, rect: Rect, heading: &str) {
        fill(surface, rect, self.plate);
        let top = Rect::new(rect.left(), rect.top(), rect.width, self.layout.line);
        self.text(surface, top, 0, heading, self.muted);
    }

    /// `text` on `row` from cell `cell`, cut to the row.
    fn text(&self, surface: &mut Surface, row: Rect, cell: u32, text: &str, color: Color) {
        let x = row
            .left()
            .saturating_add_unsigned(self.layout.cell.saturating_mul(cell));
        let room = row
            .width
            .saturating_sub(self.layout.cell.saturating_mul(cell));
        let shown = self.font.truncate_to_width(text, room);
        self.font.draw_text(surface, x, row.top(), shown, color);
    }

    /// `spans` along `row`, a cell between each, those changed in the
    /// accent.
    fn spans(&self, surface: &mut Surface, row: Rect, spans: &[shown::Span]) {
        let mut cell = 0u32;
        for span in spans {
            let color = if span.changed { self.changed } else { self.ink };
            self.text(surface, row, cell, &span.text, color);
            let width = u32::try_from(span.text.chars().count()).unwrap_or(u32::MAX);
            cell = cell.saturating_add(width).saturating_add(2);
        }
    }
}

/// Fills `rect` of `surface`, the part within it.
fn fill(surface: &mut Surface, rect: Rect, color: Color) {
    if let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) {
        surface.fill_rect(x, y, rect.width, rect.height, color);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
