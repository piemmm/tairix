//! The text-entry family: [`TextField`] and [`SearchField`] (spec §11.8).
//!
//! Both are single-line text controls built on a quiet Alloy Plate with a clear
//! focus ring, a caret, selection, and horizontally-scrolled clipped text. A
//! [`TextField`] is the general single-line entry; a [`SearchField`] is the same
//! editor behind a leading magnifier that reads as *active* when a query is
//! present (spec §11.8). Both resolve every colour/metric/radius from the active
//! [`Theme`] and [`Scale`], round their plate through the shared drawing core
//! the button/selector/value families use, and emit a typed [`TextAction`] — the
//! owning service enforces authority.
//!
//! A read-only field is enabled and legible (its text stays full-contrast and
//! selectable for copy) but refuses edits; that is deliberately distinct from a
//! disabled field (muted plate and text) and from an authority-denied field
//! (which keeps its value and shows an Authority Mark), per spec §13.
//!
//! A credential is typed into a [`SecretField`] instead: the same plate over a
//! bounded, self-erasing buffer, drawing the shared secret-entry marker every
//! text-mode password prompt draws (`tairix_vt::secret`) and nothing of what
//! was typed — not its characters, and not how many there are.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::mem;
use core::ops::Range;

use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{Palette, TextRole, Theme};
use tairix_util::secret::wipe;
use tairix_vt::secret::{SecretIndicator, SecretInput};

use crate::damage;
use crate::paint::{
    ground_fill, line_budget, paint_bead, paint_plate, paint_run, plate_border, resolve_bead,
    resolve_frame, role_font, run_width, surface_rect, text_plate_height, to_i32, withheld,
    ChromeLayer, PlateStyle, TextBlock,
};
use crate::scroll::{ScrollModel, ScrollOrientation, ScrollRange, ScrollView};
use crate::scrollbar::{ScrollAction, ScrollBar};
use crate::state::{
    ControlDisposition, ControlRole, ControlState, PointerState, RenderInvariant, ValidationState,
};

/// The outcome of feeding input to a text control.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TextAction {
    /// The control's text content changed; the owner reads it with
    /// [`TextField::text`] / [`SearchField::text`] and validates it.
    Edited,
    /// The user requested the field's value be committed (Enter).
    Submitted,
    /// The user dismissed the field (Escape). A search field additionally
    /// clears a non-empty query first, reporting [`TextAction::Edited`] for
    /// that clear and [`TextAction::Cancelled`] only when already empty.
    Cancelled,
}

/// Overwrite `range` of `text`'s bytes with zero, in place, without changing
/// the buffer's length or capacity.
///
/// Every caller passes a `char`-boundary-aligned range — a selection or a
/// caret byte index always is one — so replacing those complete scalars with
/// the single-byte `0x00` scalar can never leave `text` malformed UTF-8.
/// [`TextEditor::set_text`], [`TextEditor::clear`],
/// [`TextEditor::truncate_to_len`], and [`TextEditor`]'s `Drop` erase through
/// it; a removal that moves the tail erases through [`close_gap`]. It never
/// allocates: the buffer is moved out as a `Vec<u8>`, erased in place, and
/// moved back in, so there is no need for `String::as_mut_vec`'s `unsafe`.
///
/// The erasure is the workspace's shared [`wipe`], not a plain
/// `slice::fill(0)`: on the `Drop` path nothing reads the bytes back, so an
/// ordinary store is dead by the language's rules and a release build may
/// delete it, leaving the credential in the released block.
pub(crate) fn zeroize_range(text: &mut String, range: Range<usize>) {
    let mut bytes = mem::take(text).into_bytes();
    if let Some(slice) = bytes.get_mut(range) {
        wipe(slice);
    }
    restore(text, bytes);
}

/// Remove `gap` from `bytes` by moving the tail down over it, erase every
/// position the move vacates, and answer the length that remains.
///
/// The move leaves a copy of the tail's last bytes past the new end, where no
/// later erase of the buffer reaches; erasing them here is what keeps a
/// Backspaced character out of the block the buffer is eventually freed as.
pub(crate) fn close_gap(bytes: &mut [u8], gap: Range<usize>) -> usize {
    let len = bytes.len();
    if gap.start >= gap.end || gap.end > len {
        return len;
    }
    bytes.copy_within(gap.end..len, gap.start);
    let kept = len - (gap.end - gap.start);
    if let Some(vacated) = bytes.get_mut(kept..) {
        wipe(vacated);
    }
    kept
}

/// Put `bytes` back as `text`.
///
/// Always valid UTF-8 here, since every edit removes or zeroes whole scalars;
/// were that ever not so, the bytes are erased rather than freed as they are.
fn restore(text: &mut String, bytes: Vec<u8>) {
    *text = String::from_utf8(bytes).unwrap_or_else(|refused| {
        let mut bytes = refused.into_bytes();
        wipe(&mut bytes);
        String::new()
    });
}

/// A [`fmt::Debug`] stand-in for a secret buffer: it prints nothing of what
/// the buffer holds, its length and caret included, so a debug dump of a
/// masked field says no more than the screen does.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A single-line text buffer with a caret and a selection.
///
/// The [`caret`](Self::caret) and [`anchor`](Self::anchor) are byte indices
/// that always land on a `char` boundary of [`text`](Self::text); the selection
/// is the (possibly empty) range between them. Editing operations clamp to the
/// optional character limit and can never leave the caret mid-scalar, so a
/// renderer never has to defend against an invalid index (illegal states
/// unrepresentable).
///
/// [`secret`](Self::secret) makes the editor a credential's (see
/// [`SecretField`]). In either mode every byte an edit discards is erased —
/// through [`zeroize_range`] where it goes, [`close_gap`] where the tail moves
/// over it — since that is cheap and harmless for a plain field too.
#[derive(Eq, PartialEq)]
struct TextEditor {
    text: String,
    caret: usize,
    anchor: usize,
    max_len: Option<usize>,
    /// The byte bound of a secret editor, whose buffer is reserved for it up
    /// front and never grows (see [`fits`](Self::fits)); `None` for a plain
    /// one.
    secret: Option<usize>,
}

impl fmt::Debug for TextEditor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secret.is_some() {
            return f.debug_tuple("TextEditor").field(&Redacted).finish();
        }
        f.debug_struct("TextEditor")
            .field("text", &self.text)
            .field("caret", &self.caret)
            .field("anchor", &self.anchor)
            .field("max_len", &self.max_len)
            .finish()
    }
}

impl Clone for TextEditor {
    /// A copy holding what the original reserved. A secret copied into less
    /// room would reallocate on its next keystroke and strand the copy in the
    /// block it freed, so one whose room cannot be had copies nothing.
    fn clone(&self) -> Self {
        let mut text = String::new();
        let reserved = text.try_reserve_exact(self.text.capacity()).is_ok();
        let copied = reserved || self.secret.is_none();
        if copied {
            text.push_str(&self.text);
        }
        let (caret, anchor) = if copied {
            (self.caret, self.anchor)
        } else {
            (0, 0)
        };
        Self {
            text,
            caret,
            anchor,
            max_len: self.max_len,
            secret: self.secret,
        }
    }
}

/// Zeroes the buffer before it is freed, so a dropped field — secret or
/// not — leaves no plaintext behind in its former heap allocation.
impl Drop for TextEditor {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl TextEditor {
    /// An empty editor with no character limit.
    fn new() -> Self {
        Self {
            text: String::new(),
            caret: 0,
            anchor: 0,
            max_len: None,
            secret: None,
        }
    }

    /// Make this empty editor a secret one holding at most `max_bytes` bytes,
    /// its buffer reserved for all of them now so filling it never
    /// reallocates. A buffer whose room cannot be had takes nothing: see
    /// [`fits`](Self::fits).
    fn make_secret(&mut self, max_bytes: usize) {
        self.secret = Some(max_bytes);
        let _ = self
            .text
            .try_reserve_exact(max_bytes.saturating_sub(self.text.len()));
    }

    /// Set the character limit to `max`, truncating any existing content to
    /// fit and moving the caret to the end.
    fn set_max_len(&mut self, max: usize) {
        self.max_len = Some(max);
        self.truncate_to_len(max);
        self.caret = self.text.len();
        self.anchor = self.caret;
    }

    /// Whether `extra` more bytes may go in. A plain editor reserves them now,
    /// so a paste the allocator cannot hold is refused rather than aborting
    /// the program; a secret one takes them only within its bound and the room
    /// already reserved, so its buffer never moves and never leaves a copy in a
    /// block it freed.
    fn fits(&mut self, extra: usize) -> bool {
        let Some(max) = self.secret else {
            return self.text.try_reserve(extra).is_ok();
        };
        self.text
            .len()
            .checked_add(extra)
            .is_some_and(|len| len <= max.min(self.text.capacity()))
    }

    /// Zero the whole buffer without changing its length — the exact
    /// operation `Drop` performs, factored out into its own method so
    /// `Drop::drop` and every editor operation that discards the buffer
    /// share one definition and can never drift apart.
    fn zeroize(&mut self) {
        let len = self.text.len();
        zeroize_range(&mut self.text, 0..len);
    }

    /// Replace the whole buffer, placing the caret at the end and collapsing
    /// the selection. The text is truncated to any character limit, and the
    /// previous content is zeroised before it is discarded.
    fn set_text(&mut self, text: &str) {
        self.zeroize();
        self.text.clear();
        if self.fits(text.len()) {
            self.text.push_str(text);
        }
        if let Some(max) = self.max_len {
            self.truncate_to_len(max);
        }
        self.caret = self.text.len();
        self.anchor = self.caret;
    }

    /// Drop trailing characters until the buffer holds at most `max`
    /// scalars, zeroising the discarded tail first so no truncated scalar
    /// survives in the buffer's slack capacity.
    fn truncate_to_len(&mut self, max: usize) {
        if let Some((idx, _)) = self.text.char_indices().nth(max) {
            let len = self.text.len();
            zeroize_range(&mut self.text, idx..len);
            self.text.truncate(idx);
        }
    }

    /// The number of `char`s currently held.
    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// The selection as an ordered byte range, or `None` when it is empty.
    fn selection(&self) -> Option<(usize, usize)> {
        let (a, b) = (self.caret.min(self.anchor), self.caret.max(self.anchor));
        (a != b).then_some((a, b))
    }

    /// The byte index of the `char` boundary before `byte`, or `byte` at the
    /// start.
    fn prev_boundary(&self, byte: usize) -> usize {
        self.text[..byte]
            .char_indices()
            .next_back()
            .map_or(byte, |(i, _)| i)
    }

    /// The byte index of the `char` boundary after `byte`, or `byte` at the
    /// end.
    fn next_boundary(&self, byte: usize) -> usize {
        self.text[byte..]
            .chars()
            .next()
            .map_or(byte, |c| byte + c.len_utf8())
    }

    /// Remove the `char`-aligned byte range `range`, erasing what it held and
    /// every position the tail vacates as it moves down (see [`close_gap`]).
    fn remove(&mut self, range: Range<usize>) {
        let mut bytes = mem::take(&mut self.text).into_bytes();
        let kept = close_gap(&mut bytes, range);
        bytes.truncate(kept);
        restore(&mut self.text, bytes);
    }

    /// Delete the current selection, leaving the caret at its start. Returns
    /// whether anything was removed.
    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            return false;
        };
        self.remove(a..b);
        self.caret = a;
        self.anchor = a;
        true
    }

    /// Insert one character at the caret (replacing any selection), honouring
    /// the character limit and, for a secret, its reserved room. Returns
    /// whether the buffer changed.
    fn insert_char(&mut self, ch: char) -> bool {
        let removed = self.delete_selection();
        let full = self.max_len.is_some_and(|max| self.char_count() >= max);
        if full || !self.fits(ch.len_utf8()) {
            return removed;
        }
        self.text.insert(self.caret, ch);
        self.caret += ch.len_utf8();
        self.anchor = self.caret;
        true
    }

    /// Insert `text` at the caret (replacing any selection) as typing it
    /// would: its control characters are dropped, and it stops at the
    /// character limit or, for a secret, its reserved room. Each run between
    /// control characters goes in whole, so a long paste costs one shift of
    /// the tail per run rather than per character. Returns whether the buffer
    /// changed.
    fn insert_str(&mut self, text: &str) -> bool {
        let mut changed = self.delete_selection();
        let mut room = self
            .max_len
            .map_or(usize::MAX, |max| max.saturating_sub(self.char_count()));
        for run in text.split(char::is_control) {
            if room == 0 {
                break;
            }
            let end = run.char_indices().nth(room).map_or(run.len(), |(at, _)| at);
            let taken = &run[..end];
            if taken.is_empty() {
                continue;
            }
            if !self.fits(taken.len()) {
                break;
            }
            room -= taken.chars().count();
            self.text.insert_str(self.caret, taken);
            self.caret += taken.len();
            changed = true;
        }
        self.anchor = self.caret;
        changed
    }

    /// Backspace: delete the selection, else the character before the caret.
    fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        let start = self.prev_boundary(self.caret);
        self.remove(start..self.caret);
        self.caret = start;
        self.anchor = start;
        true
    }

    /// Forward-delete: delete the selection, else the character at the caret.
    fn delete_forward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret >= self.text.len() {
            return false;
        }
        let end = self.next_boundary(self.caret);
        self.remove(self.caret..end);
        true
    }

    /// Move the caret one character left; `select` extends the selection,
    /// otherwise a non-empty selection collapses to its start.
    fn move_left(&mut self, select: bool) {
        if !select {
            if let Some((a, _)) = self.selection() {
                self.caret = a;
                self.anchor = a;
                return;
            }
        }
        self.caret = self.prev_boundary(self.caret);
        if !select {
            self.anchor = self.caret;
        }
    }

    /// Move the caret one character right; `select` extends the selection,
    /// otherwise a non-empty selection collapses to its end.
    fn move_right(&mut self, select: bool) {
        if !select {
            if let Some((_, b)) = self.selection() {
                self.caret = b;
                self.anchor = b;
                return;
            }
        }
        self.caret = self.next_boundary(self.caret);
        if !select {
            self.anchor = self.caret;
        }
    }

    /// Move the caret to the start; `select` extends the selection.
    fn home(&mut self, select: bool) {
        self.caret = 0;
        if !select {
            self.anchor = 0;
        }
    }

    /// Move the caret to the end; `select` extends the selection.
    fn end(&mut self, select: bool) {
        self.caret = self.text.len();
        if !select {
            self.anchor = self.caret;
        }
    }

    /// Select the whole buffer.
    fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Clear the buffer and reset the caret. Returns whether anything was
    /// removed.
    ///
    /// The discarded content is zeroised first, exactly like `set_text`.
    fn clear(&mut self) -> bool {
        let changed = !self.text.is_empty();
        self.zeroize();
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        changed
    }

    /// Set the caret to `byte` (clamped to a boundary), collapsing the
    /// selection unless `select` is set.
    fn place_caret(&mut self, byte: usize, select: bool) {
        let byte = byte.min(self.text.len());
        // Snap onto a boundary in case the hit test rounded into a scalar.
        let byte = if self.text.is_char_boundary(byte) {
            byte
        } else {
            self.prev_boundary(byte)
        };
        self.caret = byte;
        if !select {
            self.anchor = byte;
        }
    }
}

/// The resolved surface geometry of a field within its bounds: the field row
/// (the plate) and the clipped inner text region, plus the message row below.
struct FieldGeom {
    /// The field-row plate rectangle `(x, y, w, h)` in surface pixels.
    row: (u32, u32, u32, u32),
    /// The surface-x where clipped text begins (after border, inset, leading).
    text_x0: u32,
    /// The clipped text region width in pixels.
    avail_w: u32,
    /// The message-row rectangle below the field, if there is room for one.
    message: Option<(u32, u32, u32, u32)>,
}

/// Resolve a field's geometry for `bounds`, reserving `leading` pixels at the
/// start of the text region (a search magnifier), or `None` if it collapses.
fn field_geom(
    bounds: Rect,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
    leading: u32,
) -> Option<FieldGeom> {
    let (x, y, w, h) = surface_rect(bounds)?;
    if w == 0 || h == 0 {
        return None;
    }
    let metrics = theme.metrics();
    let border = plate_border(theme, scale);
    let pad = scale.scale_length(metrics.control_inset);
    let edge = border.saturating_add(pad);

    let control_h = scale.scale_length(metrics.control_height).max(1);
    let row_h = if control_h < h { control_h } else { h };

    let text_x0 = x + edge.saturating_add(leading).min(w);
    let avail_w = w.saturating_sub(edge.saturating_mul(2).saturating_add(leading));

    let message = {
        let below = h.saturating_sub(row_h);
        let glyph_h = font.glyph_height();
        let mw = w.saturating_sub(edge.saturating_mul(2));
        if below >= glyph_h.saturating_add(pad) && mw > 0 {
            let my = y + row_h + pad;
            Some((x + edge, my, mw, below.saturating_sub(pad)))
        } else {
            None
        }
    };

    Some(FieldGeom {
        row: (x, y, w, row_h),
        text_x0,
        avail_w,
        message,
    })
}

/// The horizontal text scroll (pixels hidden at the left) that keeps the caret
/// visible: zero until the caret would pass the right edge, then just enough to
/// pin the caret to that edge. Deterministic from the caret alone, so `render`
/// needs no stored scroll state.
fn text_scroll(font: BitmapFont, text: &str, caret: usize, avail_w: u32) -> u32 {
    font.width_to_offset(text, caret).saturating_sub(avail_w)
}

/// The byte index whose `char` boundary is nearest text-space x `rel` (pixels
/// from the text start, i.e. pointer-x minus the text origin plus the scroll).
fn byte_from_x(font: BitmapFont, text: &str, rel: i32) -> usize {
    font.offset_at_width(text, u32::try_from(rel.max(0)).unwrap_or(u32::MAX))
}

/// What a field's text region shows.
#[derive(Copy, Clone, Debug)]
enum Shown<'a> {
    /// The buffer itself, or the placeholder while it is empty.
    Buffer,
    /// The secret-entry marker, or the placeholder while there is none.
    Marker(Option<&'a str>),
}

/// The shared single-line field: editor, role, composed state, read-only flag,
/// placeholder, and inline message, plus the render and input behaviour every
/// text control reuses. [`TextField`] and [`SearchField`] wrap one of these so
/// the editing model, clipped scrolling, caret/selection drawing, and the spec §13
/// disposition rendering are defined once.
///
/// Sharing the core also gives both fields the same render-equivalence
/// equality: the text, caret, selection endpoints, role, visible state,
/// placeholder, and message all compare — while the pointer coordinate and the
/// selection-drag latch, which no render path reads, do not.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FieldCore {
    editor: TextEditor,
    role: ControlRole,
    state: ControlState,
    read_only: bool,
    placeholder: Option<String>,
    message: Option<String>,
    /// The last pointer position, mapped to a byte index when a press or a
    /// drag places the caret — hit-testing input, never a drawn property.
    pointer: RenderInvariant<Point>,
    /// Whether a press is still extending a selection; what that produces —
    /// the caret and the selection endpoints — lives in `editor`.
    selecting: RenderInvariant<bool>,
}

impl FieldCore {
    /// An empty, resting neutral field.
    fn new() -> Self {
        Self {
            editor: TextEditor::new(),
            role: ControlRole::Neutral,
            state: ControlState::idle(),
            read_only: false,
            placeholder: None,
            message: None,
            pointer: RenderInvariant::new(Point::ORIGIN),
            selecting: RenderInvariant::new(false),
        }
    }

    /// Whether `other` draws the same plate around its text: everything the
    /// field draws but the text itself, which a masked entry never shows.
    fn same_chrome(&self, other: &Self) -> bool {
        let Self {
            editor: _,
            role,
            state,
            read_only,
            placeholder,
            message,
            pointer: _,
            selecting: _,
        } = self;
        *role == other.role
            && *state == other.state
            && *read_only == other.read_only
            && *placeholder == other.placeholder
            && *message == other.message
    }

    /// Whether the caller may navigate/select within the field (enabled,
    /// allowed, not pending — the fail-closed gate every control uses).
    fn actionable(&self) -> bool {
        self.state.is_actionable()
    }

    /// Whether the field accepts edits: actionable and not read-only.
    fn editable(&self) -> bool {
        self.actionable() && !self.read_only
    }

    /// Whether the caret should be drawn: focused and actionable.
    fn show_caret(&self) -> bool {
        self.state.focus.focused && self.actionable()
    }

    /// Paint the field plate, what `shown` names in the text region, the
    /// caret, the Signal Bead, and the inline message, reserving `leading`
    /// pixels for a search glyph.
    fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        (scale, theme): (Scale, &Theme),
        leading: u32,
        shown: Shown<'_>,
    ) {
        let font = role_font(theme, scale, TextRole::Body);
        let Some(geom) = field_geom(bounds, scale, theme, font, leading) else {
            return;
        };
        let (x, y, w, h) = geom.row;
        let palette = theme.palette();
        let metrics = theme.metrics();
        let border = plate_border(theme, scale);
        let radius = scale.scale_length(metrics.control_corner_radius).min(h / 2);
        let disposition = self.state.disposition();
        // The ground a field's plate takes, where the recipe's plate is a
        // plain background. A field the user may *write* in is a page — paper
        // on a light appearance — because the ground is the affordance; a
        // read-only one recesses onto the window ground so it reads as a value
        // shown rather than entered, keeping full-contrast text so it is still
        // not a muted disabled field. Neither substitutes a plate the recipe
        // put a colour on: a disabled, denied, or failed-closed field is
        // stating something there. `ground_fill` is what lets either ground
        // sit on floating chrome as glass rather than as an opaque patch.
        let ground = if self.editable() {
            Some(palette.document)
        } else if self.read_only && disposition != ControlDisposition::DisabledByState {
            Some(palette.surface)
        } else {
            None
        };
        let mut frame = resolve_frame(theme, self.role, self.state);
        if let Some(ground) = ground {
            frame = frame.grounded_on(Color::from(ground_fill(theme, ground, ChromeLayer::Plate)));
        }

        // Validation drives the rim segment on an otherwise-interactive field;
        // a denied/disabled/failed field keeps its disposition rim untouched.
        let rim = match disposition {
            ControlDisposition::Interactive
            | ControlDisposition::NeedsConfirmation
            | ControlDisposition::PendingCheck => match self.state.validation {
                ValidationState::Invalid => Color::from(palette.danger),
                ValidationState::Warning => Color::from(palette.warning),
                _ => frame.rim,
            },
            _ => frame.rim,
        };

        paint_plate(
            surface,
            (x, y, w, h),
            &PlateStyle {
                radius,
                border,
                plate: frame.plate,
                rim,
                focused: frame.focused,
                ring: Color::from(palette.rim_active),
            },
        );

        self.paint_text(surface, &geom, (scale, theme), (font, frame.label), shown);

        if let Some((color, shape)) = resolve_bead(theme, self.state) {
            let size = scale.scale_length(metrics.bead_size).max(3).min(w).min(h);
            paint_bead(
                surface,
                x + w - border - size,
                y + border,
                size,
                color,
                shape,
            );
        }

        self.paint_message(surface, &geom, theme, font);
    }

    /// Paint what `shown` names into the text region — the clipped,
    /// horizontally-scrolled buffer with its selection, the secret-entry
    /// marker, or the placeholder — and the caret after it.
    ///
    /// The marker is the whole of what a masked field draws: nothing it
    /// paints depends on the secret, its length included.
    fn paint_text(
        &self,
        surface: &mut Surface,
        geom: &FieldGeom,
        (scale, theme): (Scale, &Theme),
        (font, label): (BitmapFont, Color),
        shown: Shown<'_>,
    ) {
        let (_, y, _, row_h) = geom.row;
        let avail_w = geom.avail_w;
        if avail_w == 0 || row_h == 0 {
            return;
        }
        let Some(mut layer) = Surface::new(avail_w, row_h) else {
            return;
        };
        let palette = theme.palette();
        let baseline = to_i32(row_h.saturating_sub(font.glyph_height())) / 2;
        let caret = match shown {
            Shown::Marker(Some(marker)) => {
                let run = font.elide_to_width(marker, avail_w);
                paint_run(&mut layer, font, run, (0, baseline), label, None);
                Some(to_i32(run_width(font, run)))
            }
            Shown::Buffer if !self.editor.text.is_empty() => {
                self.paint_buffer(&mut layer, (font, label), baseline, palette)
            }
            Shown::Buffer | Shown::Marker(None) => {
                if let Some(placeholder) = &self.placeholder {
                    paint_run(
                        &mut layer,
                        font,
                        font.elide_to_width(placeholder, avail_w),
                        (0, baseline),
                        Color::from(palette.on_surface_muted),
                        None,
                    );
                }
                Some(0)
            }
        };

        if let Some(cx) = caret.filter(|_| self.show_caret()) {
            let caret_w = scale.scale_length(1).max(1);
            let cx = cx.clamp(0, to_i32(avail_w.saturating_sub(caret_w)));
            layer.fill_rect(
                u32::try_from(cx).unwrap_or(0),
                0,
                caret_w,
                row_h,
                Color::from(palette.on_surface),
            );
        }

        surface.blit(to_i32(geom.text_x0), to_i32(y), &layer);
    }

    /// Paint the non-empty buffer scrolled to keep the caret in view, with its
    /// selection, answering where the caret goes — `None` while a selection
    /// stands in for it.
    fn paint_buffer(
        &self,
        layer: &mut Surface,
        (font, label): (BitmapFont, Color),
        baseline: i32,
        palette: &Palette,
    ) -> Option<i32> {
        let text = self.editor.text.as_str();
        let (avail_w, row_h) = (layer.width(), layer.height());
        let scroll = text_scroll(font, text, self.editor.caret, avail_w);
        let base_x = -to_i32(scroll);
        let Some((a, b)) = self.editor.selection() else {
            font.draw_text(layer, base_x, baseline, text, label);
            return Some(to_i32(font.width_to_offset(text, self.editor.caret)) + base_x);
        };
        let sa = to_i32(font.width_to_offset(text, a)) + base_x;
        let sb = to_i32(font.width_to_offset(text, b)) + base_x;
        let clamped_a = sa.clamp(0, to_i32(avail_w));
        let clamped_b = sb.clamp(0, to_i32(avail_w));
        let sel_w = u32::try_from(clamped_b - clamped_a).unwrap_or(0);
        if sel_w > 0 {
            layer.fill_rect(
                u32::try_from(clamped_a).unwrap_or(0),
                0,
                sel_w,
                row_h,
                Color::from(palette.accent),
            );
        }
        font.draw_text(layer, base_x, baseline, &text[..a], label);
        font.draw_text(
            layer,
            sa,
            baseline,
            &text[a..b],
            Color::from(palette.on_accent),
        );
        font.draw_text(layer, sb, baseline, &text[b..], label);
        None
    }

    /// Paint the inline validation/help message below the field, coloured by
    /// the validation state (danger/warning), else a quiet hint.
    ///
    /// A message is prose — it says what is wrong and often what to do about
    /// it — so it wraps across the field's own width over as many lines as
    /// the band below the field holds, rather than being cut at the edge
    /// halfway through the instruction.
    fn paint_message(
        &self,
        surface: &mut Surface,
        geom: &FieldGeom,
        theme: &Theme,
        font: BitmapFont,
    ) {
        let Some(message) = &self.message else {
            return;
        };
        let Some((mx, my, mw, mh)) = geom.message else {
            return;
        };
        self.message_block(font, mw, line_budget(font, mh), theme)
            .paint(surface, message, (mx, my));
    }

    /// The block an inline message is laid out in: prose in the tone its
    /// validation state calls for — danger, warning, else a quiet hint.
    ///
    /// One definition for every member of the family, so a refused value
    /// reads the same under a one-line field and a multi-line box, and the
    /// height a box reserves for its message is the height it draws.
    fn message_block(
        &self,
        font: BitmapFont,
        width: u32,
        lines: usize,
        theme: &Theme,
    ) -> TextBlock {
        let palette = theme.palette();
        let color = match self.state.validation {
            ValidationState::Invalid => Color::from(palette.danger),
            ValidationState::Warning => Color::from(palette.warning),
            _ => Color::from(palette.on_surface_muted),
        };
        TextBlock::prose(font, width, lines, color)
    }

    /// Apply `edit` to the editor, reporting `bounds` when it changed anything
    /// the field draws, and pass on the edit's own answer.
    ///
    /// An edit changes the drawn field in two ways: the buffer, which the edit
    /// itself answers for, and the caret or selection, which the caret/anchor
    /// pair answers for. The buffer's bytes are deliberately **not** compared:
    /// a secret field's characters must not be copied anywhere, even into a
    /// temporary a comparison would drop.
    fn edit(
        &mut self,
        bounds: Rect,
        damage: &mut Region,
        edit: impl FnOnce(&mut TextEditor) -> bool,
    ) -> bool {
        let before = (self.editor.caret, self.editor.anchor);
        let changed = edit(&mut self.editor);
        if changed || (self.editor.caret, self.editor.anchor) != before {
            damage.add(bounds);
        }
        changed
    }

    /// The selected text, for a copy: `None` when nothing is selected.
    fn selected_text(&self) -> Option<&str> {
        self.editor
            .selection()
            .map(|(a, b)| &self.editor.text[a..b])
    }

    /// Replace the selection with `text`, as typing it would; a field that
    /// cannot be edited takes nothing.
    fn insert_text(&mut self, text: &str, bounds: Rect, damage: &mut Region) -> Option<TextAction> {
        if !self.editable() {
            return None;
        }
        self.edit(bounds, damage, |editor| editor.insert_str(text))
            .then_some(TextAction::Edited)
    }

    /// Remove the selection, for a cut; a field that cannot be edited keeps
    /// it.
    fn delete_selection(&mut self, bounds: Rect, damage: &mut Region) -> Option<TextAction> {
        if !self.editable() {
            return None;
        }
        self.edit(bounds, damage, TextEditor::delete_selection)
            .then_some(TextAction::Edited)
    }

    /// Feed a pointer event; a press places the caret (and starts a selection
    /// drag), motion while dragging extends the selection, release ends it.
    /// A denied/disabled/pending field ignores pointer editing (fail closed).
    fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        leading: u32,
        damage: &mut Region,
    ) -> Option<TextAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        // The face is a function of the theme and the scale, so the input path
        // asks for it rather than taking a derived value as an argument.
        let font = role_font(theme, scale, TextRole::Body);
        let geom = field_geom(bounds, scale, theme, font, leading)?;
        let inside = bounds.contains(*self.pointer);
        let hover_or_none = if inside {
            PointerState::Hover
        } else {
            PointerState::None
        };
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if inside && self.actionable() {
                    damage::set(
                        &mut self.state.pointer,
                        PointerState::Pressed,
                        bounds,
                        damage,
                    );
                    // A masked entry's caret stays at its end: nothing it
                    // draws says where a press between characters would be.
                    if self.editor.secret.is_none() {
                        *self.selecting = true;
                        let byte = self.byte_at(&geom, font);
                        self.edit(bounds, damage, |editor| {
                            editor.place_caret(byte, false);
                            false
                        });
                    }
                }
                None
            }
            InputEvent::PointerMoved { .. } => {
                if *self.selecting {
                    let byte = self.byte_at(&geom, font);
                    self.edit(bounds, damage, |editor| {
                        editor.place_caret(byte, true);
                        false
                    });
                } else {
                    damage::set(&mut self.state.pointer, hover_or_none, bounds, damage);
                }
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                *self.selecting = false;
                damage::set(&mut self.state.pointer, hover_or_none, bounds, damage);
                None
            }
            _ => None,
        }
    }

    /// The byte index the current pointer x maps to within the text region.
    fn byte_at(&self, geom: &FieldGeom, font: BitmapFont) -> usize {
        let text = self.editor.text.as_str();
        let scroll = text_scroll(font, text, self.editor.caret, geom.avail_w);
        let rel = self.pointer.x - to_i32(geom.text_x0) + to_i32(scroll);
        byte_from_x(font, text, rel)
    }

    /// Feed a key event. Editing keys require an editable field; navigation and
    /// selection require an actionable one; Enter/Escape report submit/cancel.
    /// `clear_on_escape` clears a non-empty buffer first (a search field).
    fn on_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        clear_on_escape: bool,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        if !self.state.focus.focused || !self.actionable() {
            return None;
        }
        match key {
            Key::Char('a' | 'A') if mods.ctrl => {
                self.edit(bounds, damage, |editor| {
                    editor.select_all();
                    false
                });
                None
            }
            Key::Char(ch) if self.editable() && !mods.ctrl && !mods.alt && !mods.meta => {
                if ch.is_control() {
                    return None;
                }
                self.edit(bounds, damage, |editor| editor.insert_char(ch))
                    .then_some(TextAction::Edited)
            }
            Key::Named(NamedKey::Backspace) if self.editable() => self
                .edit(bounds, damage, TextEditor::backspace)
                .then_some(TextAction::Edited),
            Key::Named(NamedKey::Delete) if self.editable() => self
                .edit(bounds, damage, TextEditor::delete_forward)
                .then_some(TextAction::Edited),
            Key::Named(NamedKey::Left) => {
                self.edit(bounds, damage, |editor| {
                    editor.move_left(mods.shift);
                    false
                });
                None
            }
            Key::Named(NamedKey::Right) => {
                self.edit(bounds, damage, |editor| {
                    editor.move_right(mods.shift);
                    false
                });
                None
            }
            Key::Named(NamedKey::Home) => {
                self.edit(bounds, damage, |editor| {
                    editor.home(mods.shift);
                    false
                });
                None
            }
            Key::Named(NamedKey::End) => {
                self.edit(bounds, damage, |editor| {
                    editor.end(mods.shift);
                    false
                });
                None
            }
            Key::Named(NamedKey::Enter) => Some(TextAction::Submitted),
            Key::Named(NamedKey::Escape) => {
                if clear_on_escape && self.edit(bounds, damage, TextEditor::clear) {
                    Some(TextAction::Edited)
                } else {
                    Some(TextAction::Cancelled)
                }
            }
            _ => None,
        }
    }
}

/// A single-line text entry (spec §11.8).
///
/// A `TextField` owns its typed [`ControlState`], its [`ControlRole`], and its
/// text buffer; it renders itself into a [`Surface`] and consumes pointer and
/// keyboard input, emitting a [`TextAction`] when the content changes or the
/// user submits/cancels. It performs no privileged work — the owning container
/// validates the value and enforces authority. A read-only
/// field stays legible and selectable but refuses edits, distinct from a
/// disabled field (muted) and a denied field (Authority Mark), per spec §13.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextField {
    core: FieldCore,
}

impl Default for TextField {
    fn default() -> Self {
        Self::new()
    }
}

impl TextField {
    /// An empty, resting neutral text field.
    #[must_use]
    pub fn new() -> Self {
        Self {
            core: FieldCore::new(),
        }
    }

    /// This field with a non-default role (e.g. destructive).
    #[must_use]
    pub fn with_role(mut self, role: ControlRole) -> Self {
        self.core.role = role;
        self
    }

    /// This field pre-filled with `text` (caret at the end).
    #[must_use]
    pub fn with_text(mut self, text: impl AsRef<str>) -> Self {
        self.core.editor.set_text(text.as_ref());
        self
    }

    /// This field with placeholder text shown while it is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.core.placeholder = Some(placeholder.into());
        self
    }

    /// This field limited to at most `max` characters (existing content is
    /// truncated to fit).
    #[must_use]
    pub fn with_max_len(mut self, max: usize) -> Self {
        self.core.editor.set_max_len(max);
        self
    }

    /// This field marked read-only: legible and selectable, but not editable.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.core.read_only = read_only;
        self
    }

    /// This field with an inline validation/help message shown below it.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.core.message = Some(message.into());
        self
    }

    /// The height one line of field occupies at `scale`: the shared
    /// text-plate height every one-line control takes.
    ///
    /// Exposed because a field is not always laid out by a panel that already
    /// knows this — a menu chain's own entry surface sizes itself to one, and
    /// the file manager grows an item's name band to it so an in-place editor
    /// is legible over a short row.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        text_plate_height(theme, scale, TextRole::Body)
    }

    /// The field's current text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.core.editor.text
    }

    /// Replace the field's text (caret to the end), e.g. after the owner
    /// commits a change.
    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.core.editor.set_text(text.as_ref());
    }

    /// Whether the field is read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.core.read_only
    }

    /// The field's role.
    #[must_use]
    pub fn role(&self) -> ControlRole {
        self.core.role
    }

    /// The field's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.core.state
    }

    /// Replace the field's composed state (e.g. from a model update).
    pub fn set_state(&mut self, state: ControlState) {
        self.core.state = state;
    }

    /// Set the field's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.core.state.focus.focused = focused;
    }

    /// Set the inline validation/help message (or clear it with `None`).
    pub fn set_message(&mut self, message: Option<String>) {
        self.core.message = message;
    }

    /// Paint the field into `surface` at `bounds` for the active theme.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        self.core
            .render(surface, bounds, (scale, theme), 0, Shown::Buffer);
    }

    /// Feed a pointer event: a primary press positions the caret under the
    /// pointer and starts a selection, motion while pressed extends it, and
    /// release ends it. A denied/disabled/pending field ignores it (fail
    /// closed).
    ///
    /// The field reports `bounds` into `damage` when the event changed what it
    /// draws — the text, the caret, the selection, or its pointer look. A
    /// sample that stays inside a field it is already hovering reports nothing.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.on_pointer(event, bounds, scale, theme, 0, damage)
    }

    /// Feed a key event: printable keys insert (replacing any selection),
    /// Backspace/Delete remove, arrows/Home/End move the caret (Shift extends
    /// the selection), Ctrl+A selects all, Enter submits, and Escape cancels.
    /// Editing keys require an editable (not read-only, not denied) field.
    ///
    /// A key that edits the buffer or moves the caret reports `bounds`; one
    /// that only submits or cancels, or moves a caret already at the end it
    /// moves toward, reports nothing.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.on_key(key, modifiers, false, bounds, damage)
    }

    /// The selected text, for a copy: `None` when nothing is selected.
    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        self.core.selected_text()
    }

    /// Replace the selection with `text`, as typing it would — its control
    /// characters dropped, and stopping at the field's limit — reporting
    /// `bounds` when it changed. A field that cannot be edited takes nothing.
    pub fn insert_text(
        &mut self,
        text: &str,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.insert_text(text, bounds, damage)
    }

    /// Remove the selection, for a cut, reporting `bounds` when it did.
    pub fn delete_selection(&mut self, bounds: Rect, damage: &mut Region) -> Option<TextAction> {
        self.core.delete_selection(bounds, damage)
    }
}

/// One key press as a control that times its own feedback takes it: the key,
/// the modifiers held, and when its owner took it on the monotonic clock the
/// owner parks by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Keystroke {
    /// The key that went down.
    pub key: Key,
    /// The modifiers held while it did.
    pub modifiers: Modifiers,
    /// Monotonic nanoseconds at which the owner took it.
    pub at_ns: u64,
}

impl Keystroke {
    /// The keystroke `event` is when it is a key press, taken at `at_ns`.
    #[must_use]
    pub const fn pressed(event: InputEvent, at_ns: u64) -> Option<Self> {
        match event {
            InputEvent::KeyPressed { key, modifiers } => Some(Self {
                key,
                modifiers,
                at_ns,
            }),
            _ => None,
        }
    }
}

/// A single-line masked entry for a credential — a password, a passphrase, a
/// PIN — that shows the shared secret-entry marker and nothing typed.
///
/// Once a character is in, the field reads `[input active.]`, its dots cycling
/// on the cadence every text-mode password prompt uses (`tairix_vt::secret`),
/// and `[input complete]` once the secret is submitted: neither the characters
/// nor how many there are ever reach the screen. Editing is the line
/// discipline's — characters append, Backspace erases the last, Enter submits
/// — and nothing moves the caret or selects, because an edit nobody can see is
/// one nobody can check. The first edit after a submission begins a new
/// secret, which is what the marker then drawn says.
///
/// The dots move only while the owner keeps time: it hands each key over as a
/// [`Keystroke`], parks no later than [`deadline_ns`](Self::deadline_ns), and
/// calls [`advance`](Self::advance) once that passes. Under reduced motion the
/// marker stands still and no deadline is armed.
///
/// The buffer is bounded in bytes and reserved up front, so filling it never
/// reallocates and strands a copy of the credential in a freed block; every
/// byte it discards — a Backspace, a clear, `Drop` — is erased through the
/// shared volatile wipe. Typing past the bound is kept count of rather than
/// dropped silently, and makes the whole entry unofferable (see
/// [`secret`](Self::secret)). Nothing reveals the buffer through the control:
/// not its equality, which compares only what is drawn, and not a debug dump.
#[derive(Clone)]
pub struct SecretField {
    core: FieldCore,
    marker: SecretIndicator,
    /// Characters typed past the bound, which the buffer does not hold.
    overflow: usize,
}

/// Equal exactly when the two draw the same pixels: what was typed never
/// reaches the screen, so it is never compared.
impl PartialEq for SecretField {
    fn eq(&self, other: &Self) -> bool {
        self.marker.marker() == other.marker.marker() && self.core.same_chrome(&other.core)
    }
}

impl Eq for SecretField {}

impl fmt::Debug for SecretField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretField")
            .field("secret", &Redacted)
            .field("state", &self.core.state)
            .finish_non_exhaustive()
    }
}

impl SecretField {
    /// An empty masked entry holding at most `max_bytes` bytes of UTF-8, its
    /// buffer reserved for them now.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        let mut core = FieldCore::new();
        core.editor.make_secret(max_bytes);
        Self {
            core,
            marker: SecretIndicator::new(),
            overflow: 0,
        }
    }

    /// This field with placeholder text shown while it is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.core.placeholder = Some(placeholder.into());
        self
    }

    /// This field with an inline help message shown below it.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.core.message = Some(message.into());
        self
    }

    /// The secret as typed, or `None` when more was typed than the field
    /// holds: an entry longer than any credential can be is refused whole,
    /// never offered as its prefix.
    ///
    /// A caller reads it to perform one exchange and lets it go; it is never
    /// stored, logged, or copied into a buffer that outlives the call.
    #[must_use]
    pub fn secret(&self) -> Option<&str> {
        (self.overflow == 0).then_some(self.core.editor.text.as_str())
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core.editor.text.is_empty() && self.overflow == 0
    }

    /// Erase the secret and take the marker down.
    pub fn clear(&mut self) {
        let _ = self.core.editor.clear();
        self.overflow = 0;
        self.marker = SecretIndicator::new();
    }

    /// Record that the owner offered the secret, as Enter does: the field
    /// reads `[input complete]`, and its next edit begins a new secret.
    pub(crate) fn submit(&mut self) {
        let _ = self.marker.submit();
    }

    /// The field's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.core.state
    }

    /// Replace the field's composed state.
    pub fn set_state(&mut self, state: ControlState) {
        self.core.state = state;
    }

    /// Set the field's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.core.state.focus.focused = focused;
    }

    /// Set the inline help message, or clear it with `None`.
    pub fn set_message(&mut self, message: Option<String>) {
        self.core.message = message;
    }

    /// The next moment the marker's dots move, on the clock the keystrokes
    /// were taken by, or `None` while they are still.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.marker.deadline_ns()
    }

    /// Move the dots through every frame due by `now_ns`, answering whether
    /// what the field draws changed.
    pub fn advance(&mut self, now_ns: u64) -> bool {
        let shown = self.marker.marker();
        while let Some(due) = self.deadline_ns().filter(|due| *due <= now_ns) {
            let _ = self.marker.tick(due);
        }
        self.marker.marker() != shown
    }

    /// Paint the field into `surface` at `bounds` for the active theme.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let marker = self.marker.marker();
        let text = marker
            .as_ref()
            .and_then(|marker| core::str::from_utf8(marker.bytes()).ok());
        self.core
            .render(surface, bounds, (scale, theme), 0, Shown::Marker(text));
    }

    /// Feed a pointer event. A press takes the field's pressed look and places
    /// nothing: the caret stays at the end. A denied, disabled or pending field
    /// ignores it.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.on_pointer(event, bounds, scale, theme, 0, damage)
    }

    /// Feed a key press: a printable character appends, Backspace erases the
    /// last, Enter submits, Escape cancels, and nothing else does anything.
    ///
    /// `theme` decides, for the keystroke, whether the dots may move. A key
    /// reports `bounds` only when it changed what the field draws, so neither
    /// the damage nor the presents it causes count the characters.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        bounds: Rect,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        if !self.core.state.focus.focused || !self.core.actionable() {
            return None;
        }
        let Keystroke {
            key,
            modifiers,
            at_ns,
        } = stroke;
        let shown = self.marker.marker();
        let action = match key {
            Key::Named(NamedKey::Enter) => {
                self.submit();
                Some(TextAction::Submitted)
            }
            Key::Named(NamedKey::Escape) => Some(TextAction::Cancelled),
            _ if !self.core.editable() => None,
            Key::Named(NamedKey::Backspace) if self.marker.submitted() => {
                self.clear();
                Some(TextAction::Edited)
            }
            Key::Named(NamedKey::Backspace) => self.erase_last().then(|| {
                let line_empty = self.is_empty();
                self.took(SecretInput::Erased { line_empty }, at_ns, theme);
                TextAction::Edited
            }),
            Key::Char(ch)
                if !ch.is_control() && !modifiers.ctrl && !modifiers.alt && !modifiers.meta =>
            {
                if self.marker.submitted() {
                    self.clear();
                }
                if !self.core.editor.insert_char(ch) {
                    self.overflow = self.overflow.saturating_add(1);
                }
                self.took(SecretInput::Typed, at_ns, theme);
                Some(TextAction::Edited)
            }
            _ => None,
        };
        if self.marker.marker() != shown {
            damage.add(bounds);
        }
        action
    }

    /// Erase the last character typed — one past the bound first, since the
    /// buffer holds none of those — answering whether there was one.
    fn erase_last(&mut self) -> bool {
        if self.overflow > 0 {
            self.overflow -= 1;
            return true;
        }
        self.core.editor.backspace()
    }

    /// Feed the marker one edit taken at `at_ns`. Under reduced motion the
    /// dots then stand still, arming nothing, so no stale deadline is left to
    /// replay once motion returns.
    fn took(&mut self, input: SecretInput, at_ns: u64, theme: &Theme) {
        let _ = self.marker.input(input, at_ns);
        if theme.motion().reduced_motion() {
            self.marker.freeze();
        }
    }
}

/// Test-only: the masked field's backing buffer's address and byte capacity,
/// so a test can prove that filling it up to its limit never reallocates (a
/// reallocation would leave a copy of the credential behind in a freed heap
/// block).
#[cfg(test)]
pub(crate) fn debug_buffer_identity(field: &SecretField) -> (*const u8, usize) {
    (
        field.core.editor.text.as_ptr(),
        field.core.editor.text.capacity(),
    )
}

/// Test-only: a copy of the masked field's raw buffer bytes, including any
/// bytes [`zeroize_range`] has overwritten — [`SecretField::secret`] cannot
/// show that, since a zeroised buffer is always truncated or replaced before a
/// caller could read it back.
#[cfg(test)]
pub(crate) fn debug_bytes(field: &SecretField) -> alloc::vec::Vec<u8> {
    field.core.editor.text.as_bytes().to_vec()
}

/// Test-only: zero the field's buffer without dropping it, through the exact
/// method [`TextEditor`]'s `Drop` implementation calls.
///
/// A dropped `String`'s allocation cannot be read afterwards without
/// `unsafe`, which this crate forbids outright, so a test cannot observe a
/// real drop's effect directly. This hook instead proves the two are the
/// same operation: [`TextEditor::drop`] delegates to the private `zeroize`
/// method, and this is that same method, called without triggering an
/// actual drop.
#[cfg(test)]
pub(crate) fn debug_zeroize(field: &mut SecretField) {
    field.core.editor.zeroize();
}

/// Draw a magnifier glyph (a ring with a short handle) of `size` at `(x, y)`,
/// the search field's leading affordance. `hole` is the plate colour showing
/// through the ring.
fn paint_magnifier(surface: &mut Surface, x: u32, y: u32, size: u32, color: Color, hole: Color) {
    if size < 4 {
        return;
    }
    let ring = (size * 3 / 4).max(3);
    let rx = x + (size - ring) / 2;
    let ry = y + (size - ring) / 2;
    surface.fill_round_rect(rx, ry, ring, ring, ring / 2, color);
    let t = (ring / 5).max(1);
    let inner = ring.saturating_sub(t.saturating_mul(2));
    if inner > 0 {
        surface.fill_round_rect(rx + t, ry + t, inner, inner, inner / 2, hole);
    }
    let hs = (size / 3).max(2);
    let hx = (rx + ring).saturating_sub(hs / 2).min(x + size - hs);
    let hy = (ry + ring).saturating_sub(hs / 2).min(y + size - hs);
    surface.fill_round_rect(hx, hy, hs, hs, hs / 4, color);
}

/// A single-line text entry specialised for queries (spec §11.8).
///
/// A `SearchField` is a [`TextField`] behind a leading magnifier that reads as
/// *active* (accent-tinted) when a query is present and quiet when it is empty,
/// so the query state is legible from the leading affordance. Escape clears a
/// non-empty query (reporting [`TextAction::Edited`]) before dismissing the
/// field; every other behaviour matches [`TextField`], over one shared editing
/// and rendering core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchField {
    core: FieldCore,
}

impl Default for SearchField {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchField {
    /// An empty, resting search field.
    #[must_use]
    pub fn new() -> Self {
        Self {
            core: FieldCore::new(),
        }
    }

    /// This search field pre-filled with a query (caret at the end).
    #[must_use]
    pub fn with_text(mut self, text: impl AsRef<str>) -> Self {
        self.core.editor.set_text(text.as_ref());
        self
    }

    /// This search field with placeholder text shown while it is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.core.placeholder = Some(placeholder.into());
        self
    }

    /// This search field limited to at most `max` characters.
    #[must_use]
    pub fn with_max_len(mut self, max: usize) -> Self {
        self.core.editor.set_max_len(max);
        self
    }

    /// The current query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.core.editor.text
    }

    /// Replace the query text (caret to the end).
    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.core.editor.set_text(text.as_ref());
    }

    /// Whether a query is present (non-empty).
    #[must_use]
    pub fn has_query(&self) -> bool {
        !self.core.editor.text.is_empty()
    }

    /// The field's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.core.state
    }

    /// Replace the field's composed state.
    pub fn set_state(&mut self, state: ControlState) {
        self.core.state = state;
    }

    /// Set the field's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.core.state.focus.focused = focused;
    }

    /// The leading magnifier region width for `bounds` (a square the height of
    /// the field row, capped to half the width so text always has room).
    fn leading(bounds: Rect, scale: Scale, theme: &Theme, font: BitmapFont) -> u32 {
        field_geom(bounds, scale, theme, font, 0).map_or(0, |g| {
            let (_, _, w, row_h) = g.row;
            row_h.min(w / 2)
        })
    }

    /// Paint the search field into `surface` at `bounds` for the active theme.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let leading = Self::leading(bounds, scale, theme, font);
        self.core
            .render(surface, bounds, (scale, theme), leading, Shown::Buffer);

        let Some(geom) = field_geom(bounds, scale, theme, font, leading) else {
            return;
        };
        if leading == 0 {
            return;
        }
        let (x, y, _, row_h) = geom.row;
        let palette = theme.palette();
        let border = plate_border(theme, scale);
        let pad = scale.scale_length(theme.metrics().control_inset);
        let edge = border.saturating_add(pad);
        // The magnifier reads as active (accent) when a query is present and
        // the field is actionable, quiet (muted) otherwise.
        let color = if self.has_query() && self.core.actionable() {
            Color::from(palette.accent)
        } else {
            Color::from(palette.on_surface_muted)
        };
        let size = leading
            .saturating_sub(pad)
            .min(row_h.saturating_sub(border.saturating_mul(2)));
        let gx = x + edge;
        let gy = y + (row_h.saturating_sub(size)) / 2;
        paint_magnifier(surface, gx, gy, size, color, Color::from(palette.surface));
    }

    /// Feed a pointer event; see [`TextField::on_pointer`].
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        let font = role_font(theme, scale, TextRole::Body);
        let leading = Self::leading(bounds, scale, theme, font);
        self.core
            .on_pointer(event, bounds, scale, theme, leading, damage)
    }

    /// Feed a key event; Escape clears a non-empty query before dismissing.
    /// Reports like [`TextField::on_key`], and a cleared query reports too —
    /// the magnifier goes quiet with it.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.on_key(key, modifiers, true, bounds, damage)
    }

    /// The selected query text, for a copy.
    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        self.core.selected_text()
    }

    /// Replace the selection with `text`, as [`TextField::insert_text`].
    pub fn insert_text(
        &mut self,
        text: &str,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<TextAction> {
        self.core.insert_text(text, bounds, damage)
    }

    /// Remove the selection, for a cut.
    pub fn delete_selection(&mut self, bounds: Rect, damage: &mut Region) -> Option<TextAction> {
        self.core.delete_selection(bounds, damage)
    }
}

// --- TextArea ---------------------------------------------------------------

/// The most lines of wrapped message a multi-line entry reserves beneath
/// itself: one sentence about the text above it, not a document.
const AREA_MESSAGE_LINES: usize = 2;

/// The resolved surface geometry of a [`TextArea`] within its bounds.
struct AreaGeom {
    /// The plate rectangle `(x, y, w, h)` in surface pixels.
    plate: (u32, u32, u32, u32),
    /// The text viewport inside the plate, past the scrollbar gutter.
    text: (u32, u32, u32, u32),
    /// The height of one wrapped line, in pixels.
    line: u32,
    /// How many whole wrapped lines the viewport shows.
    rows: usize,
    /// How many wrapped lines the text takes at the viewport's width.
    lines: usize,
    /// The scrollbar's rectangle, when the text does not fit the viewport.
    bar: Option<Rect>,
    /// The message band below the plate, where there is one and it fits.
    message: Option<(u32, u32, u32, u32)>,
}

impl AreaGeom {
    /// The height every wrapped line takes together, in pixels.
    fn content(&self) -> u32 {
        u32::try_from(self.lines)
            .unwrap_or(u32::MAX)
            .saturating_mul(self.line)
    }

    /// The scroll offset `want`, in pixels, clamped to what the viewport can
    /// show.
    fn clamp_scroll(&self, want: u32) -> u32 {
        want.min(self.content().saturating_sub(self.text.3))
    }

    /// The text viewport, scrolled `offset` pixels down the wrapped lines.
    fn view(&self, offset: u32) -> ScrollView {
        let (tx, ty, tw, th) = self.text;
        ScrollView::new(
            ScrollOrientation::Vertical,
            Rect::new(to_i32(tx), to_i32(ty), tw, th),
            u64::from(self.clamp_scroll(offset)),
        )
    }
}

/// A multi-line text entry that wraps its text (spec §11.42).
///
/// A `TextArea` is the text-entry family's multi-line member: the same plate,
/// caret, selection, read-only/denied/validation rendering and typed
/// [`TextAction`] as a [`TextField`], over a text that **wraps at the box's
/// own width** rather than scrolling sideways. That is the difference between
/// the two, and it is why both exist: a single-line field holds a value and
/// scrolls; a box that holds a paragraph wraps it, because a paragraph read
/// through a one-line window is not read at all.
///
/// - **Wrapping is the behaviour, not an option.** There is no horizontal
///   scroll and no wrap toggle: the text is laid out to the viewport's width
///   through the one shared fitter, breaking at whitespace, and a newline the
///   user typed is a forced break.
/// - **The caret and selection work in *visual* lines.** Up and Down move
///   between the lines the reader sees and keep the column they started from,
///   Home and End go to the ends of the visual line, and Ctrl+Home/Ctrl+End
///   to the ends of the text. A click lands on the character nearest the
///   pointer on the line it fell on.
/// - **Enter inserts a newline** and reports [`TextAction::Edited`]; it does
///   not submit, because in a box that holds paragraphs Enter is a paragraph.
///   Escape still reports [`TextAction::Cancelled`].
/// - **It scrolls vertically, and shows that it does.** The caret is kept in
///   view as it moves, the wheel and PageUp/PageDown scroll the viewport, and
///   a text longer than the box grows the shared [`ScrollBar`] in a trailing
///   gutter — so a reader can see there is more.
/// - **There is no masked mode.** A credential is a single value, so masking
///   belongs to [`SecretField`]; a multi-line masked box would be a
///   credential no one could check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextArea {
    core: FieldCore,
    /// How far down the wrapped lines the viewport is scrolled, in pixels.
    scroll: u32,
    /// The scrollbar drawn when the text outgrows the viewport. Its own
    /// hover and drag state lives here; the offset it reports is applied to
    /// [`scroll`](Self::scroll), which stays the one answer.
    bar: ScrollBar,
    /// The x a vertical caret move aims at, in pixels from the line's start.
    /// Kept so walking Up and Down through a short line does not strand the
    /// caret at that line's end — nothing drawn reads it.
    goal_x: RenderInvariant<Option<u32>>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

impl TextArea {
    /// An empty, resting neutral text area.
    #[must_use]
    pub fn new() -> Self {
        Self {
            core: FieldCore::new(),
            scroll: 0,
            bar: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::EMPTY, 1),
            ),
            goal_x: RenderInvariant::new(None),
        }
    }

    /// This area with a non-default role (e.g. destructive).
    #[must_use]
    pub fn with_role(mut self, role: ControlRole) -> Self {
        self.core.role = role;
        self
    }

    /// This area pre-filled with `text` (caret at the end).
    #[must_use]
    pub fn with_text(mut self, text: impl AsRef<str>) -> Self {
        self.core.editor.set_text(text.as_ref());
        self
    }

    /// This area with placeholder text shown while it is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.core.placeholder = Some(placeholder.into());
        self
    }

    /// This area limited to at most `max` characters (existing content is
    /// truncated to fit).
    #[must_use]
    pub fn with_max_len(mut self, max: usize) -> Self {
        self.core.editor.set_max_len(max);
        self
    }

    /// This area marked read-only: legible and selectable, but not editable.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.core.read_only = read_only;
        self
    }

    /// This area with an inline validation/help message shown below it.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.core.message = Some(message.into());
        self
    }

    /// The area's current text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.core.editor.text
    }

    /// Replace the area's text (caret to the end), e.g. after the owner
    /// commits a change. The viewport returns to the top.
    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.core.editor.set_text(text.as_ref());
        self.scroll = 0;
        *self.goal_x = None;
    }

    /// Whether the area is read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.core.read_only
    }

    /// The area's role.
    #[must_use]
    pub fn role(&self) -> ControlRole {
        self.core.role
    }

    /// The area's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.core.state
    }

    /// Replace the area's composed state (e.g. from a model update).
    pub fn set_state(&mut self, state: ControlState) {
        self.core.state = state;
        self.bar.set_state(state);
    }

    /// Set the area's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.core.state.focus.focused = focused;
    }

    /// Set the inline validation/help message (or clear it with `None`).
    pub fn set_message(&mut self, message: Option<String>) {
        self.core.message = message;
    }

    /// How far down the wrapped lines the viewport is scrolled, in pixels.
    #[must_use]
    pub fn scroll_offset(&self) -> u32 {
        self.scroll
    }

    /// The height an area showing `rows` whole lines of text needs, message
    /// band included.
    ///
    /// An owner seats a box by how much of the text it wants visible, which
    /// is the only figure it can sensibly choose: how *tall* that is depends
    /// on the theme's type ladder and the DPI scale, and this is where that
    /// arithmetic lives.
    #[must_use]
    pub fn measured_height(&self, rows: u32, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let font = role_font(theme, scale, TextRole::Body);
        let pad = scale.scale_length(theme.metrics().control_inset);
        let edge = plate_border(theme, scale).saturating_add(pad);
        let plate = font
            .line_height()
            .saturating_mul(rows.max(1))
            .saturating_add(edge.saturating_mul(2));
        let message = self.core.message.as_ref().map_or(0, |message| {
            let width = width.saturating_sub(edge.saturating_mul(2));
            pad.saturating_add(
                self.core
                    .message_block(font, width, AREA_MESSAGE_LINES, theme)
                    .height(message),
            )
        });
        plate.saturating_add(message)
    }
}

impl TextArea {
    /// Resolve the area's geometry for `bounds`: the plate, the text
    /// viewport, the scrollbar gutter, and the message band.
    ///
    /// The bar appears only when the text does not fit the rows the box has.
    /// Reserving its gutter narrows the column, which can only *add* wrapped
    /// lines, so a text that overflowed the full width still overflows the
    /// narrowed one — the decision settles in one pass and cannot flicker.
    fn geom(
        &self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) -> Option<AreaGeom> {
        let (x, y, w, h) = surface_rect(bounds)?;
        if w == 0 || h == 0 {
            return None;
        }
        let metrics = theme.metrics();
        let pad = scale.scale_length(metrics.control_inset);
        let edge = plate_border(theme, scale).saturating_add(pad);
        let line = font.line_height().max(1);

        let inner_w = w.saturating_sub(edge.saturating_mul(2));
        // The message takes the lines it needs out of the bottom, but never
        // out of the box itself: a note about the text is worth less than one
        // line of the text.
        let box_floor = edge.saturating_mul(2).saturating_add(line);
        let message_h = self.core.message.as_ref().map_or(0, |message| {
            let wanted = self
                .core
                .message_block(font, inner_w, AREA_MESSAGE_LINES, theme)
                .height(message);
            if wanted == 0 {
                return 0;
            }
            pad.saturating_add(wanted)
                .min(h.saturating_sub(box_floor.min(h)))
        });
        let plate_h = h.saturating_sub(message_h);
        let (tx, ty, full_w, text_h) = (
            x.saturating_add(edge),
            y.saturating_add(edge),
            inner_w,
            plate_h.saturating_sub(edge.saturating_mul(2)),
        );
        let rows = (text_h / line) as usize;
        if rows == 0 || full_w == 0 {
            return None;
        }

        // The overflow test stops one line past the viewport, so a text that
        // fits is never walked beyond what is drawn — and its line count is
        // that same answer. Only a text that does outgrow the box pays for
        // the full count, which is what its proportional thumb needs.
        let breadth = scale.scale_length(metrics.scrollbar_breadth).max(1);
        let probe = font
            .lines_to_width(self.text(), full_w)
            .take(rows.saturating_add(1))
            .count();
        let (text_w, bar) = if probe > rows && full_w > breadth.saturating_mul(2) {
            (
                full_w.saturating_sub(breadth),
                Some(Rect::new(
                    to_i32(tx.saturating_add(full_w).saturating_sub(breadth)),
                    to_i32(ty),
                    breadth,
                    text_h,
                )),
            )
        } else {
            (full_w, None)
        };
        let lines = if probe > rows {
            font.lines_to_width(self.text(), text_w).count()
        } else {
            probe
        };

        let message = self.core.message.as_ref().and_then(|_| {
            (message_h > pad).then_some((
                x.saturating_add(edge),
                y.saturating_add(plate_h).saturating_add(pad),
                inner_w,
                message_h.saturating_sub(pad),
            ))
        });
        Some(AreaGeom {
            plate: (x, y, w, plate_h),
            text: (tx, ty, text_w, text_h),
            line,
            rows,
            lines,
            bar,
            message,
        })
    }

    /// The index of the wrapped line the caret sits on, and where that line
    /// starts.
    ///
    /// The lines tile the text, so a caret inside it is on the line whose
    /// span holds it; a caret at the very end is on the **last** line, which
    /// is what puts it on the empty line a trailing newline opens rather than
    /// back at the end of the line before it. Every position therefore
    /// resolves, and to exactly one line.
    fn caret_line(&self, font: BitmapFont, width: u32) -> (usize, usize) {
        let caret = self.core.editor.caret;
        let mut last = (0, 0);
        for (index, line) in font.lines_to_width(self.text(), width).enumerate() {
            last = (index, line.start);
            if caret < line.end() {
                return last;
            }
        }
        last
    }

    /// The byte offset of the visible end of the wrapped line at `index`,
    /// and its start — the two positions Home and End move the caret to.
    fn line_bounds(&self, font: BitmapFont, width: u32, index: usize) -> Option<(usize, usize)> {
        let line = font.lines_to_width(self.text(), width).nth(index)?;
        Some((
            line.start,
            line.start.saturating_add(line.text.trim_end().len()),
        ))
    }

    /// The byte offset nearest `x` pixels along the wrapped line at `index`,
    /// clamped to the line's visible text so a click past the end of a line
    /// does not land on the next one.
    fn byte_at(&self, font: BitmapFont, width: u32, index: usize, x: i32) -> usize {
        let Some(line) = font.lines_to_width(self.text(), width).nth(index) else {
            return self.text().len();
        };
        let visible = line.text.trim_end();
        line.start.saturating_add(byte_from_x(font, visible, x))
    }

    /// The caret's x offset along its own wrapped line, in pixels.
    fn caret_x(&self, font: BitmapFont, width: u32) -> u32 {
        let (_, start) = self.caret_line(font, width);
        let line = self.text().get(start..).unwrap_or_default();
        font.width_to_offset(line, self.core.editor.caret.saturating_sub(start))
    }
}

impl TextArea {
    /// Paint the area into `surface` at `bounds` for the active theme: the
    /// plate, the wrapped text with its selection and caret, the scrollbar
    /// when the text outgrows the box, and the inline message below it.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let Some(geom) = self.geom(bounds, scale, theme, font) else {
            return;
        };
        let (x, y, w, h) = geom.plate;
        let palette = theme.palette();
        let metrics = theme.metrics();
        let border = plate_border(theme, scale);
        let radius = scale.scale_length(metrics.control_corner_radius).min(h / 2);
        let disposition = self.core.state.disposition();
        // A box the user may write in is a page, exactly as a one-line field
        // is; a read-only one recesses onto the window ground so it reads as
        // text shown rather than text entered.
        let ground = if self.core.editable() {
            Some(palette.document)
        } else if self.core.read_only && disposition != ControlDisposition::DisabledByState {
            Some(palette.surface)
        } else {
            None
        };
        let mut frame = resolve_frame(theme, self.core.role, self.core.state);
        if let Some(ground) = ground {
            frame = frame.grounded_on(Color::from(ground_fill(theme, ground, ChromeLayer::Plate)));
        }
        let rim = match disposition {
            ControlDisposition::Interactive
            | ControlDisposition::NeedsConfirmation
            | ControlDisposition::PendingCheck => match self.core.state.validation {
                ValidationState::Invalid => Color::from(palette.danger),
                ValidationState::Warning => Color::from(palette.warning),
                _ => frame.rim,
            },
            _ => frame.rim,
        };
        paint_plate(
            surface,
            (x, y, w, h),
            &PlateStyle {
                radius,
                border,
                plate: frame.plate,
                rim,
                focused: frame.focused,
                ring: Color::from(palette.rim_active),
            },
        );

        geom.view(self.scroll).paint(surface, |surface| {
            self.paint_text(surface, &geom, scale, theme, font, frame.label);
        });

        if let Some(rect) = geom.bar {
            let mut bar = self.bar;
            bar.set_model(self.scroll_model(&geom));
            bar.render(surface, rect, scale, theme);
        }

        if let Some((mx, my, mw, mh)) = geom.message {
            if let Some(message) = &self.core.message {
                self.core
                    .message_block(font, mw, line_budget(font, mh), theme)
                    .paint(surface, message, (mx, my));
            }
        }
    }

    /// The scroll model for `geom`: every wrapped line's height against the
    /// viewport's, in pixels, stepping a line at a time.
    fn scroll_model(&self, geom: &AreaGeom) -> ScrollModel {
        ScrollModel::in_pixels(
            ScrollRange::new(
                u64::from(geom.content()),
                u64::from(geom.text.3),
                u64::from(geom.clamp_scroll(self.scroll)),
            ),
            u64::from(geom.line),
        )
    }

    /// Paint the wrapped text — or the placeholder — with its selection
    /// highlight and caret, laid out unscrolled from the viewport's top: the
    /// caller's [`ScrollView`] shifts and confines it.
    ///
    /// Only the lines the viewport shows are laid out: the layout is a lazy
    /// walk, so a long note costs the lines above the viewport and the lines
    /// in it, never the whole text.
    fn paint_text(
        &self,
        surface: &mut Surface,
        geom: &AreaGeom,
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
        label: Color,
    ) {
        let (tx, ty, tw, _) = geom.text;
        let palette = theme.palette();
        let line_h = font.line_height().max(1);
        if self.text().is_empty() {
            if let Some(placeholder) = &self.core.placeholder {
                TextBlock::prose(font, tw, geom.rows, Color::from(palette.on_surface_muted)).paint(
                    surface,
                    placeholder,
                    (tx, ty),
                );
            }
        }
        let selection = self.core.editor.selection();
        let scroll = geom.clamp_scroll(self.scroll);
        let caret_w = scale.scale_length(1).max(1);
        // Resolved once: the caret is on one line, and asking each drawn line
        // whether it holds it invites two of them to say yes.
        let caret_row =
            (self.core.show_caret() && selection.is_none()).then(|| self.caret_line(font, tw).0);
        let shown = geom.view(scroll).lines(line_h, geom.lines);
        for (row, line) in font
            .lines_to_width(self.text(), tw)
            .enumerate()
            .skip(shown.start)
            .take(shown.len())
        {
            let top = ty.saturating_add(
                u32::try_from(row)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(line_h),
            );
            let visible = line.text.trim_end();
            let pen = to_i32(tx);
            if let Some((a, b)) = selection {
                // The highlight covers this line's share of the selection,
                // which for a line wholly inside it is the whole line.
                let from = a.clamp(line.start, line.end());
                let to = b.clamp(line.start, line.end());
                let sa = font.width_to_offset(visible, from.saturating_sub(line.start));
                let sb = font.width_to_offset(visible, to.saturating_sub(line.start));
                if sb > sa {
                    surface.fill_rect(
                        tx.saturating_add(sa),
                        top,
                        sb - sa,
                        line_h,
                        Color::from(palette.accent),
                    );
                }
                let head = visible
                    .get(..from.saturating_sub(line.start).min(visible.len()))
                    .unwrap_or_default();
                let body = visible
                    .get(
                        from.saturating_sub(line.start).min(visible.len())
                            ..to.saturating_sub(line.start).min(visible.len()),
                    )
                    .unwrap_or_default();
                let tail = visible
                    .get(to.saturating_sub(line.start).min(visible.len())..)
                    .unwrap_or_default();
                font.draw_text(surface, pen, to_i32(top), head, label);
                font.draw_text(
                    surface,
                    pen + to_i32(sa),
                    to_i32(top),
                    body,
                    Color::from(palette.on_accent),
                );
                font.draw_text(surface, pen + to_i32(sb), to_i32(top), tail, label);
            } else {
                font.draw_text(surface, pen, to_i32(top), visible, label);
            }
            if caret_row == Some(row) {
                let cx = font
                    .width_to_offset(visible, self.core.editor.caret.saturating_sub(line.start));
                surface.fill_rect(
                    tx.saturating_add(cx.min(tw.saturating_sub(caret_w))),
                    top,
                    caret_w,
                    line_h,
                    Color::from(palette.on_surface),
                );
            }
        }
    }
}

impl TextArea {
    /// Feed a pointer event: a primary press inside the text places the caret
    /// on the line and character under the pointer and starts a selection,
    /// motion while pressed extends it, release ends it, and the wheel
    /// scrolls the viewport without moving the caret. A press on the
    /// scrollbar is the bar's. A denied/disabled/pending area ignores it
    /// (fail closed).
    ///
    /// The area reports `bounds` into `damage` when the event changed what it
    /// draws — the caret, the selection, or the viewport's position.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.core.pointer = *to;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let geom = self.geom(bounds, scale, theme, font)?;
        let (tx, ty, tw, _) = geom.text;
        let inside = bounds.contains(*self.core.pointer);
        let hover_or_none = if inside {
            PointerState::Hover
        } else {
            PointerState::None
        };

        // The bar owns the gutter and anything its drag is still holding, so
        // a pointer that slid off the thumb keeps scrolling rather than
        // placing a caret.
        if let Some(rect) = geom.bar {
            let on_bar = rect.contains(*self.core.pointer);
            if on_bar
                || matches!(event, InputEvent::PointerScrolled { .. })
                || self.bar.is_pressing()
            {
                self.bar.set_model(self.scroll_model(&geom));
                let scrolled = match event {
                    InputEvent::PointerScrolled { dx, dy } if inside => {
                        self.bar.wheel(*dx, *dy, scale, rect, damage)
                    }
                    _ => self.bar.on_pointer(event, rect, scale, theme, damage),
                };
                if let Some(ScrollAction::ScrollTo { offset }) = scrolled {
                    self.set_scroll(u32::try_from(offset).unwrap_or(u32::MAX), bounds, damage);
                }
                if on_bar || self.bar.is_pressing() {
                    return None;
                }
            }
        }

        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if inside && self.core.actionable() {
                    *self.core.selecting = true;
                    damage::set(
                        &mut self.core.state.pointer,
                        PointerState::Pressed,
                        bounds,
                        damage,
                    );
                    self.place_at_pointer(&geom, font, (tx, ty, tw), false, bounds, damage);
                }
                None
            }
            InputEvent::PointerMoved { .. } => {
                if *self.core.selecting {
                    self.place_at_pointer(&geom, font, (tx, ty, tw), true, bounds, damage);
                } else {
                    damage::set(&mut self.core.state.pointer, hover_or_none, bounds, damage);
                }
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                *self.core.selecting = false;
                damage::set(&mut self.core.state.pointer, hover_or_none, bounds, damage);
                None
            }
            _ => None,
        }
    }

    /// Place the caret at the pointer, extending the selection when `select`.
    fn place_at_pointer(
        &mut self,
        geom: &AreaGeom,
        font: BitmapFont,
        text: (u32, u32, u32),
        select: bool,
        bounds: Rect,
        damage: &mut Region,
    ) {
        let (tx, ty, tw) = text;
        let down = self.core.pointer.y.saturating_sub(to_i32(ty)).max(0);
        let into = u32::try_from(down)
            .unwrap_or(0)
            .saturating_add(geom.clamp_scroll(self.scroll));
        let row = usize::try_from(into / geom.line.max(1))
            .unwrap_or(usize::MAX)
            .min(geom.lines.saturating_sub(1));
        let byte = self.byte_at(font, tw, row, self.core.pointer.x - to_i32(tx));
        self.core.edit(bounds, damage, |editor| {
            editor.place_caret(byte, select);
            false
        });
        *self.goal_x = None;
        self.reveal_caret(geom, font, bounds, damage);
    }

    /// Feed a key event.
    ///
    /// Printable keys insert (replacing any selection) and **Enter inserts a
    /// newline**; Backspace/Delete remove; Left/Right move by character and
    /// Up/Down by *visual* line, keeping the column they set out from;
    /// Home/End go to the ends of the visual line and Ctrl+Home/Ctrl+End to
    /// the ends of the text; PageUp/PageDown move by a viewport; Ctrl+A
    /// selects all; Shift extends the selection with every one of those
    /// moves; Escape reports [`TextAction::Cancelled`]. Editing keys need an
    /// editable (not read-only, not denied) area.
    ///
    /// A key that edits the text or moves the caret or the viewport reports
    /// `bounds`; one that moves a caret already at the end it moves toward
    /// reports nothing.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TextAction> {
        if !self.core.state.focus.focused || !self.core.actionable() {
            return None;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let geom = self.geom(bounds, scale, theme, font)?;
        let width = geom.text.2;
        let action = match key {
            Key::Named(NamedKey::Enter) if self.core.editable() => self
                .core
                .edit(bounds, damage, |editor| editor.insert_char('\n'))
                .then_some(TextAction::Edited),
            Key::Named(NamedKey::Up | NamedKey::Down) => {
                self.move_line(&geom, font, key, modifiers.shift, bounds, damage);
                None
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown) => {
                self.move_page(&geom, font, key, modifiers.shift, bounds, damage);
                None
            }
            Key::Named(NamedKey::Home | NamedKey::End) if !modifiers.ctrl => {
                let (row, _) = self.caret_line(font, width);
                let ends = self.line_bounds(font, width, row);
                if let Some((start, end)) = ends {
                    let to = if key == Key::Named(NamedKey::Home) {
                        start
                    } else {
                        end
                    };
                    self.core.edit(bounds, damage, |editor| {
                        editor.place_caret(to, modifiers.shift);
                        false
                    });
                }
                *self.goal_x = None;
                None
            }
            _ => {
                let action = self
                    .core
                    .on_key(key, modifiers, false, bounds, damage)
                    .filter(|action| *action != TextAction::Submitted);
                *self.goal_x = None;
                action
            }
        };
        self.reveal_caret(&geom, font, bounds, damage);
        action
    }

    /// Move the caret one visual line up or down, keeping the column it set
    /// out from so a walk through a short line does not strand it there.
    fn move_line(
        &mut self,
        geom: &AreaGeom,
        font: BitmapFont,
        key: Key,
        select: bool,
        bounds: Rect,
        damage: &mut Region,
    ) {
        let width = geom.text.2;
        let goal = self.goal_x.unwrap_or_else(|| self.caret_x(font, width));
        let (row, _) = self.caret_line(font, width);
        let next = if key == Key::Named(NamedKey::Up) {
            row.checked_sub(1)
        } else {
            (row + 1 < geom.lines).then_some(row + 1)
        };
        if let Some(next) = next {
            let byte = self.byte_at(font, width, next, to_i32(goal));
            self.core.edit(bounds, damage, |editor| {
                editor.place_caret(byte, select);
                false
            });
        }
        *self.goal_x = Some(goal);
    }

    /// Move the caret a viewport's worth of lines, keeping its column.
    fn move_page(
        &mut self,
        geom: &AreaGeom,
        font: BitmapFont,
        key: Key,
        select: bool,
        bounds: Rect,
        damage: &mut Region,
    ) {
        let width = geom.text.2;
        let goal = self.goal_x.unwrap_or_else(|| self.caret_x(font, width));
        let (row, _) = self.caret_line(font, width);
        let next = if key == Key::Named(NamedKey::PageUp) {
            row.saturating_sub(geom.rows)
        } else {
            row.saturating_add(geom.rows)
                .min(geom.lines.saturating_sub(1))
        };
        let byte = self.byte_at(font, width, next, to_i32(goal));
        self.core.edit(bounds, damage, |editor| {
            editor.place_caret(byte, select);
            false
        });
        *self.goal_x = Some(goal);
    }

    /// Scroll so the caret's line is inside the viewport, reporting `bounds`
    /// when the viewport actually moved.
    ///
    /// The viewport follows the caret rather than the caret being confined to
    /// the viewport: typing at the end of a long note brings the end into
    /// view, which is what makes the box usable at all.
    fn reveal_caret(
        &mut self,
        geom: &AreaGeom,
        font: BitmapFont,
        bounds: Rect,
        damage: &mut Region,
    ) {
        let (row, _) = self.caret_line(font, geom.text.2);
        let top = u64::try_from(row)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::from(geom.line));
        let revealed = self
            .scroll_model(geom)
            .revealing(top, u64::from(geom.line))
            .offset();
        self.set_scroll(u32::try_from(revealed).unwrap_or(u32::MAX), bounds, damage);
    }

    /// Adopt `offset` pixels as the viewport's position, reporting `bounds`
    /// when it changed what is drawn.
    fn set_scroll(&mut self, offset: u32, bounds: Rect, damage: &mut Region) {
        if self.scroll != offset {
            self.scroll = offset;
            damage.add(bounds);
        }
    }
}

/// Test-only: whether `field` would take `extra` more bytes.
#[cfg(test)]
pub(crate) fn debug_fits(field: &mut TextField, extra: usize) -> bool {
    field.core.editor.fits(extra)
}

/// Test-only: a [`TextArea`]'s laid-out geometry for `bounds` — the text
/// viewport, the scrollbar's rectangle where the text outgrew it, and the
/// rows and wrapped lines behind that decision.
///
/// Taken from the exact layout [`TextArea::render`] draws and
/// [`TextArea::on_pointer`] hit-tests through, so a test aiming a click at a
/// line, or looking for the bar's gutter, cannot drift from where the box
/// actually put them.
#[cfg(test)]
pub(crate) fn debug_area_layout(
    area: &TextArea,
    bounds: Rect,
    scale: Scale,
    theme: &Theme,
) -> Option<(Rect, Option<Rect>, usize, usize)> {
    let font = role_font(theme, scale, TextRole::Body);
    let geom = area.geom(bounds, scale, theme, font)?;
    let (tx, ty, tw, th) = geom.text;
    Some((
        Rect::new(to_i32(tx), to_i32(ty), tw, th),
        geom.bar,
        geom.rows,
        geom.lines,
    ))
}
