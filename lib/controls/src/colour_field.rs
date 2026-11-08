//! The colour field: one colour shown as a swatch and typed in hexadecimal.
//!
//! The swatch draws the colour as a picker's swatch does, over a checker where
//! it is translucent, and the entry beside it holds its `#rrggbb` — or
//! `#rrggbbaa`, translucent, in a field holding an opacity. Typing that spells
//! a colour takes effect at once; anything else shows as invalid and moves
//! nothing until it is fixed or committed. Enter and the keyboard leaving
//! settle it, and Escape takes typing back, as a number field's do.

use core::fmt::Write as _;

use tairix_colour::{parse_hex, Rgba};
use tairix_geometry::{to_i32, Rect, Region, Scale};
use tairix_inline::ArrayString;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::Surface;
use tairix_theme::{TextRole, Theme};

use crate::colour_picker::paint_sample;
use crate::paint::{plate_border, role_font};
use crate::state::{ControlDisposition, ControlState, ValidationState};
use crate::text::{TextAction, TextField};

/// What feeding input to a [`ColourField`] concluded.
///
/// As for a number field, the live colour and the settled one are distinct,
/// and durable work belongs on [`Settled`](Self::Settled) alone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ColourAction {
    /// The colour became `colour` while the entry continues: apply it live,
    /// and nothing more.
    Edited {
        /// The colour now held.
        colour: Rgba,
    },
    /// The entry finished on `colour`: Enter, Escape taking typing back, or
    /// the owner committing the field as the keyboard leaves it.
    Settled {
        /// The colour it finished on.
        colour: Rgba,
    },
}

impl ColourAction {
    /// The colour the field holds, entered or settled.
    #[must_use]
    pub const fn colour(self) -> Rgba {
        match self {
            Self::Edited { colour } | Self::Settled { colour } => colour,
        }
    }

    /// Whether the entry finished, so durable work may follow it.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Settled { .. })
    }
}

/// The longest spelling: `#` and eight digits.
const LONGEST: usize = 9;

type Spelt = ArrayString<LONGEST>;

/// A colour, shown as a swatch and typed as its hexadecimal notation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColourField {
    field: TextField,
    colour: Rgba,
    /// The colour the last interaction settled on, which Escape returns to.
    settled: Rgba,
    /// Whether the field holds an opacity, so a translucent colour may be
    /// typed; an opaque-only field drops any alpha it is given.
    opacity: bool,
}

impl ColourField {
    /// A field holding `colour`, with its alpha where it holds an `opacity`
    /// and opaque otherwise.
    #[must_use]
    pub fn new(colour: Rgba, opacity: bool) -> Self {
        let colour = if opacity {
            colour
        } else {
            colour.with_alpha(u8::MAX)
        };
        Self {
            field: TextField::new()
                .with_max_len(LONGEST)
                .with_text(spelt(colour).as_str()),
            colour,
            settled: colour,
            opacity,
        }
    }

    /// The colour held: the last spelling typed that read as one, or the last
    /// one committed or set.
    #[must_use]
    pub const fn colour(&self) -> Rgba {
        self.colour
    }

    /// Whether the field holds an opacity.
    #[must_use]
    pub const fn has_opacity(&self) -> bool {
        self.opacity
    }

    /// What the field shows.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        self.field.text()
    }

    /// Hold `colour` — opaque, where the field holds no opacity — and show
    /// it, without reporting: the owner reports the repaint. It is settled,
    /// and any typing the field held is replaced.
    pub fn set_colour(&mut self, colour: Rgba) {
        self.colour = self.held(colour);
        self.settled = self.colour;
        let _ = self.show_colour();
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

    /// The width the field needs: its swatch, the gap after it, and an entry
    /// wide enough for its longest spelling.
    #[must_use]
    pub fn preferred_width(&self, scale: Scale, theme: &Theme) -> u32 {
        let font = role_font(theme, scale, TextRole::Body);
        let widest = if self.opacity { "#00000000" } else { "#000000" };
        let edge = plate_border(theme, scale)
            .saturating_add(scale.scale_length(theme.metrics().control_inset));
        let entry = font
            .text_width(widest)
            .saturating_add(edge.saturating_mul(2))
            .saturating_add(scale.scale_length(2));
        Self::height(scale, theme)
            .saturating_add(gap(scale, theme))
            .saturating_add(entry)
    }

    /// The height the field takes: one line of text plate.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        TextField::height(scale, theme)
    }

    /// Paint the swatch and the entry into `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        let (swatch, entry) = parts(bounds, scale, theme);
        let disabled = self.state().disposition() == ControlDisposition::DisabledByState;
        paint_sample(
            surface,
            swatch,
            self.colour,
            (plate_border(theme, scale).max(1), disabled),
            (scale, theme),
        );
        self.field.render(surface, entry, scale, theme);
    }

    /// Feed a pointer event: a press on the entry places its caret, as in any
    /// text field.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ColourAction> {
        let (_, entry) = parts(bounds, scale, theme);
        self.field.on_pointer(event, entry, scale, theme, damage);
        None
    }

    /// Feed a key to a focused field. Enter commits; Escape takes back what
    /// was typed since the last settle; a hex digit or `#` edits as text, and
    /// any other character is refused whole. Tab and a key with nothing to do
    /// answer `None`, so the owner may carry focus on.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<ColourAction> {
        if !self.takes_keys() {
            return None;
        }
        match key {
            Key::Named(NamedKey::Enter) => self.commit(bounds, damage),
            Key::Named(NamedKey::Escape) => self.take_back(bounds, damage),
            Key::Named(NamedKey::Tab) => None,
            Key::Char(character) if !modifiers.ctrl && !modifiers.alt && !modifiers.meta => {
                if !admits(character) {
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
    /// where any of it could not be part of a spelling or it would run past
    /// the longest one.
    pub fn insert_text(
        &mut self,
        text: &str,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<ColourAction> {
        if !self.takes_keys() || !text.chars().all(admits) {
            return None;
        }
        let kept = self
            .field
            .text()
            .len()
            .saturating_sub(self.field.selected_text().map_or(0, str::len));
        if kept.saturating_add(text.len()) > LONGEST {
            return None;
        }
        let action = self.field.insert_text(text, bounds, damage);
        self.edited(action)
    }

    /// Settle what the field holds, as Enter does and as an owner does when
    /// the focus leaves it: a spelling that reads as no colour is replaced by
    /// the colour it last held.
    pub fn commit(&mut self, bounds: Rect, damage: &mut Region) -> Option<ColourAction> {
        if let Some(typed) = self.typed() {
            self.colour = typed;
        }
        if self.show_colour() {
            damage.add(bounds);
        }
        self.settle()
    }

    /// Whether keys reach the field: it has the keyboard and may act.
    fn takes_keys(&self) -> bool {
        let state = self.field.state();
        state.focus.focused && state.is_actionable()
    }

    /// `colour` as this field holds it: opaque unless it holds an opacity.
    const fn held(&self, colour: Rgba) -> Rgba {
        if self.opacity {
            colour
        } else {
            colour.with_alpha(u8::MAX)
        }
    }

    /// End the interaction on the colour held: `Settled` when it moved since
    /// the last settle.
    fn settle(&mut self) -> Option<ColourAction> {
        if self.colour == self.settled {
            return None;
        }
        self.settled = self.colour;
        Some(ColourAction::Settled {
            colour: self.colour,
        })
    }

    /// Take back typing since the last settle: the field shows the settled
    /// colour again. `None` when there is nothing to take back, so the owner
    /// may give Escape its own meaning.
    fn take_back(&mut self, bounds: Rect, damage: &mut Region) -> Option<ColourAction> {
        let pending =
            self.colour != self.settled || self.field.text() != spelt(self.settled).as_str();
        if !pending {
            return None;
        }
        self.colour = self.settled;
        if self.show_colour() {
            damage.add(bounds);
        }
        Some(ColourAction::Settled {
            colour: self.colour,
        })
    }

    /// What an edit of the text concluded: a spelling that reads as a colour
    /// is the colour, and anything else shows as invalid.
    fn edited(&mut self, action: Option<TextAction>) -> Option<ColourAction> {
        if action != Some(TextAction::Edited) {
            return None;
        }
        let typed = self.typed();
        let state = self.field.state();
        self.field
            .set_state(state.with_validation(ValidationState::of(typed.is_some())));
        let colour = typed?;
        if colour == self.colour {
            return None;
        }
        self.colour = colour;
        Some(ColourAction::Edited { colour })
    }

    /// The colour the text spells, `#` optional: three or six digits, or with
    /// an opacity four or eight.
    fn typed(&self) -> Option<Rgba> {
        let text = self.field.text();
        let digits = text.strip_prefix('#').unwrap_or(text);
        let (colour, form) = parse_hex(digits)?;
        (self.opacity || !form.has_alpha()).then_some(colour)
    }

    /// Show the colour as its canonical spelling, valid, answering whether
    /// that changed what the field draws.
    fn show_colour(&mut self) -> bool {
        let text = spelt(self.colour);
        let state = self.field.state();
        let changed =
            self.field.text() != text.as_str() || state.validation != ValidationState::Valid;
        if self.field.text() != text.as_str() {
            self.field.set_text(text.as_str());
        }
        self.field
            .set_state(state.with_validation(ValidationState::Valid));
        changed
    }
}

/// Whether `character` can stand in a spelling.
const fn admits(character: char) -> bool {
    character == '#' || character.is_ascii_hexdigit()
}

/// `colour` spelt as the field shows it: `#rrggbb`, or `#rrggbbaa` where it
/// is translucent.
fn spelt(colour: Rgba) -> Spelt {
    let mut out = Spelt::new();
    // Nine characters at most, which the spelling never exceeds.
    let _ = write!(out, "{}", colour.hex().hashed());
    out
}

/// The gap between the swatch and the entry.
fn gap(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1) / 2
}

/// Where the swatch and the entry stand in `bounds`: a square swatch as tall
/// as the field at its leading edge, the entry in the rest.
fn parts(bounds: Rect, scale: Scale, theme: &Theme) -> (Rect, Rect) {
    let side = ColourField::height(scale, theme)
        .min(bounds.height)
        .min(bounds.width);
    let top = bounds.top() + to_i32(bounds.height.saturating_sub(side) / 2);
    let swatch = Rect::new(bounds.left(), top, side, side);
    let after = side.saturating_add(gap(scale, theme)).min(bounds.width);
    let entry = Rect::new(
        bounds.left().saturating_add(to_i32(after)),
        bounds.top(),
        bounds.width - after,
        bounds.height,
    );
    (swatch, entry)
}

#[cfg(test)]
#[path = "colour_field_tests.rs"]
mod tests;
