//! The number field: typed, range-checked integer entry.
//!
//! A [`NumberField`] is a [`TextField`] that holds a number between two
//! bounds. Digits typed into it take effect as soon as they spell a number in
//! range, so whatever the value drives follows the typing; one that does not
//! — an empty field, a number past a bound — shows as invalid and moves
//! nothing until it is fixed or committed. Up and Down step it by a line,
//! Page Up and Page Down by a page, and the wheel steps it while it has the
//! keyboard. A step, Enter, and the focus leaving are each a whole
//! interaction and settle it.

use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::Surface;
use tairix_theme::{TextRole, Theme};
use tairix_util::fmt::format_i32;

use crate::paint::{plate_border, role_font};
use crate::scroll::wheel_steps;
use crate::state::{ControlState, RenderInvariant, ValidationState};
use crate::text::{TextAction, TextField};

/// What feeding input to a [`NumberField`] concluded.
///
/// As for a slider, the live value and the settled one are distinct, and
/// durable work belongs on [`Settled`](Self::Settled) alone: typing `255` is
/// three edits and one settle.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NumberAction {
    /// The value became `value` while the entry continues — a digit typed.
    /// Apply it live, and nothing more.
    Edited {
        /// The value now held.
        value: i32,
    },
    /// The entry finished on `value`: a step, Enter, Escape taking typing
    /// back, or the owner committing the field as the focus leaves it.
    Settled {
        /// The value it finished on.
        value: i32,
    },
}

/// A text field holding an integer within `min..=max`.
///
/// Equal fields draw the same pixels: the wheel's carried fraction is never
/// drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumberField {
    field: TextField,
    value: i32,
    min: i32,
    max: i32,
    line: i32,
    page: i32,
    /// The value the last interaction settled on, which Escape returns to.
    settled: i32,
    wheel: RenderInvariant<i64>,
}

impl NumberField {
    /// A field holding `value` held to `min..=max` (the bounds in either
    /// order), stepping by one a line and ten a page.
    #[must_use]
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        let (min, max) = (min.min(max), min.max(max));
        let value = value.clamp(min, max);
        let mut digits = [0; 12];
        Self {
            field: TextField::new()
                .with_max_len(longest(min, max))
                .with_text(format_i32(value, &mut digits)),
            value,
            min,
            max,
            line: 1,
            page: 10,
            settled: value,
            wheel: RenderInvariant::new(0),
        }
    }

    /// This field stepping `line` for Up, Down and a wheel detent and `page`
    /// for Page Up and Page Down; a step of zero moves nothing.
    #[must_use]
    pub fn with_steps(mut self, line: i32, page: i32) -> Self {
        self.line = line.max(0);
        self.page = page.max(0);
        self
    }

    /// The value held: the last number typed in range, or the last one
    /// stepped to or committed.
    #[must_use]
    pub const fn value(&self) -> i32 {
        self.value
    }

    /// The bounds, `(min, max)`.
    #[must_use]
    pub const fn range(&self) -> (i32, i32) {
        (self.min, self.max)
    }

    /// Hold `value`, clamped to the bounds, and show it, without reporting:
    /// the owner commits it and reports the repaint. It is settled, and any
    /// typing the field held is replaced.
    pub fn set_value(&mut self, value: i32) {
        self.value = value.clamp(self.min, self.max);
        self.settled = self.value;
        let _ = self.show_value();
    }

    /// The field's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.field.state()
    }

    /// Replace the field's composed state; the owner reports the repaint.
    pub fn set_state(&mut self, state: ControlState) {
        self.field.set_state(state);
    }

    /// Set the field's keyboard focus. An owner moving the focus away first
    /// [`commit`](Self::commit)s it, so typing left in it is not lost.
    pub fn set_focused(&mut self, focused: bool) {
        self.field.set_focused(focused);
    }

    /// The width a field needs to show its longest bound: the text and the
    /// plate's insets either side of it.
    #[must_use]
    pub fn preferred_width(&self, scale: Scale, theme: &Theme) -> u32 {
        let font = role_font(theme, scale, TextRole::Body);
        let mut digits = [0; 12];
        let widest = font
            .text_width(format_i32(self.min, &mut digits))
            .max(font.text_width(format_i32(self.max, &mut digits)))
            .max(font.text_width("0"));
        let edge = plate_border(theme, scale)
            .saturating_add(scale.scale_length(theme.metrics().control_inset));
        widest
            .saturating_add(edge.saturating_mul(2))
            .saturating_add(scale.scale_length(2))
    }

    /// The height a field takes: one line of text plate.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        TextField::height(scale, theme)
    }

    /// Paint the field into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        self.field.render(surface, bounds, scale, theme);
    }

    /// Feed a pointer event: a press places the caret, as in any text field,
    /// and the wheel steps a focused field a line a detent, each detent a
    /// settled step.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<NumberAction> {
        if let InputEvent::PointerScrolled { dy, .. } = *event {
            if !self.takes_keys() {
                return None;
            }
            let steps = wheel_steps(dy.saturating_neg(), 1, &mut self.wheel);
            let delta = i64::from(self.line).saturating_mul(steps);
            return self.step(delta, bounds, damage);
        }
        self.field.on_pointer(event, bounds, scale, theme, damage);
        None
    }

    /// Feed a key to a focused field. Up and Down step a line and Page Up and
    /// Page Down a page; Enter commits; Escape takes back what was typed or
    /// stepped since the last settle; a digit, and a minus sign where the
    /// bounds reach below zero, edit as text. Any other character is refused
    /// whole: the field takes nothing that cannot be part of a number. Tab and
    /// a key with nothing to do answer `None`, so the owner may carry focus
    /// on.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<NumberAction> {
        if !self.takes_keys() {
            return None;
        }
        match key {
            Key::Named(NamedKey::Up) => self.step(i64::from(self.line), bounds, damage),
            Key::Named(NamedKey::Down) => self.step(-i64::from(self.line), bounds, damage),
            Key::Named(NamedKey::PageUp) => self.step(i64::from(self.page), bounds, damage),
            Key::Named(NamedKey::PageDown) => self.step(-i64::from(self.page), bounds, damage),
            Key::Named(NamedKey::Enter) => self.commit(bounds, damage),
            Key::Named(NamedKey::Escape) => self.take_back(bounds, damage),
            Key::Named(NamedKey::Tab) => None,
            Key::Char(character) if !modifiers.ctrl && !modifiers.alt && !modifiers.meta => {
                if !self.admits(character) {
                    return None;
                }
                let action = self.field.on_key(key, modifiers, bounds, damage);
                self.edited(action)
            }
            _ => {
                let action = self.field.on_key(key, modifiers, bounds, damage);
                self.edited(action)
            }
        }
    }

    /// Paste `text` over the selection as typing it would, or nothing at all
    /// where any of it could not be part of a number or it would run past the
    /// field's length.
    pub fn insert_text(
        &mut self,
        text: &str,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<NumberAction> {
        if !self.takes_keys() || !text.chars().all(|character| self.admits(character)) {
            return None;
        }
        let kept = self
            .field
            .text()
            .len()
            .saturating_sub(self.field.selected_text().map_or(0, str::len));
        if kept.saturating_add(text.len()) > longest(self.min, self.max) {
            return None;
        }
        let action = self.field.insert_text(text, bounds, damage);
        self.edited(action)
    }

    /// Settle what the field holds, as Enter does and as an owner does when
    /// the focus leaves it: a number past a bound is held to it, and text
    /// that spells no number is replaced by the value it last held.
    pub fn commit(&mut self, bounds: Rect, damage: &mut Region) -> Option<NumberAction> {
        if let Some(typed) = self.typed() {
            self.value = i32::try_from(typed.clamp(i64::from(self.min), i64::from(self.max)))
                .unwrap_or(self.value);
        }
        if self.show_value() {
            damage.add(bounds);
        }
        self.settle()
    }

    /// Whether keys reach the field: it has the keyboard and may act.
    fn takes_keys(&self) -> bool {
        let state = self.field.state();
        state.focus.focused && state.is_actionable()
    }

    /// Whether `character` can stand in a number this field holds.
    fn admits(&self, character: char) -> bool {
        character.is_ascii_digit() || (character == '-' && self.min < 0)
    }

    /// Move the value `delta` and settle there, held to the bounds; nothing
    /// for a step that moves nothing.
    fn step(&mut self, delta: i64, bounds: Rect, damage: &mut Region) -> Option<NumberAction> {
        if delta == 0 {
            return None;
        }
        let stepped =
            (i64::from(self.value) + delta).clamp(i64::from(self.min), i64::from(self.max));
        let stepped = i32::try_from(stepped).unwrap_or(self.value);
        let typing = self.field.state().validation != ValidationState::Valid;
        if stepped == self.value && !typing {
            return None;
        }
        self.value = stepped;
        if self.show_value() {
            damage.add(bounds);
        }
        self.settle()
    }

    /// Take back typing and steps since the last settle: the field shows the
    /// settled value again. `None` when there is nothing to take back, so the
    /// owner may give Escape its own meaning.
    fn take_back(&mut self, bounds: Rect, damage: &mut Region) -> Option<NumberAction> {
        let mut digits = [0; 12];
        let pending = self.value != self.settled
            || self.field.text() != format_i32(self.settled, &mut digits);
        if !pending {
            return None;
        }
        self.value = self.settled;
        if self.show_value() {
            damage.add(bounds);
        }
        Some(NumberAction::Settled { value: self.value })
    }

    /// What an edit of the text concluded: the number it now spells, when
    /// that is in range, is the value, and anything else shows as invalid.
    fn edited(&mut self, action: Option<TextAction>) -> Option<NumberAction> {
        if action != Some(TextAction::Edited) {
            return None;
        }
        let in_range = self
            .typed()
            .filter(|typed| (i64::from(self.min)..=i64::from(self.max)).contains(typed))
            .and_then(|typed| i32::try_from(typed).ok());
        let state = self.field.state();
        self.field
            .set_state(state.with_validation(ValidationState::of(in_range.is_some())));
        let value = in_range?;
        if value == self.value {
            return None;
        }
        self.value = value;
        Some(NumberAction::Edited { value })
    }

    /// The number the text spells, however far past the bounds.
    fn typed(&self) -> Option<i64> {
        self.field.text().parse::<i64>().ok()
    }

    /// Show the value as its canonical digits, valid, answering whether that
    /// changed what the field draws.
    fn show_value(&mut self) -> bool {
        let mut digits = [0; 12];
        let text = format_i32(self.value, &mut digits);
        let state = self.field.state();
        let changed = self.field.text() != text || state.validation != ValidationState::Valid;
        if self.field.text() != text {
            self.field.set_text(text);
        }
        self.field
            .set_state(state.with_validation(ValidationState::Valid));
        changed
    }

    /// End the interaction on the value held: `Settled` when it moved since
    /// the last settle.
    fn settle(&mut self) -> Option<NumberAction> {
        if self.value == self.settled {
            return None;
        }
        self.settled = self.value;
        Some(NumberAction::Settled { value: self.value })
    }
}

/// The most characters a number between `min` and `max` is spelled in.
fn longest(min: i32, max: i32) -> usize {
    let mut digits = [0; 12];
    format_i32(min, &mut digits)
        .len()
        .max(format_i32(max, &mut digits).len())
}
