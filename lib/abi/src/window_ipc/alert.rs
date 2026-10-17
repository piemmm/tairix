//! Alerts: the error, warning, notice or question an application asks the
//! desktop to put to the user in a window of its own
//! ([`WindowRequest::OpenAlert`](super::WindowRequest::OpenAlert)).
//!
//! The application states only what is said and which answers are offered.
//! Who is speaking is the desktop's to say, from the caller's attested
//! identity, so nothing here names a title.

use crate::bounded_text::{BoundedText, WideText};
use crate::le::{put_u16, read_u16};
use crate::Errno;

/// Most bytes an alert's headline may hold: one sentence saying what
/// happened.
pub const ALERT_HEADLINE_MAX: usize = 160;

/// Most bytes an alert's detail may hold: the paragraph explaining the
/// headline, with line feeds where it breaks.
pub const ALERT_DETAIL_MAX: usize = 1024;

/// Most bytes one button's label may hold.
pub const ALERT_LABEL_MAX: usize = 32;

/// Most buttons one alert may offer.
pub const ALERT_MAX_BUTTONS: usize = 4;

/// Most alerts one application may have up at once.
///
/// A containment bound: no application may stack windows on the user faster
/// than they can be answered.
pub const ALERT_MAX_OPEN: usize = 4;

/// An alert's headline: one line, never empty.
pub type AlertHeadline = BoundedText<1, ALERT_HEADLINE_MAX>;

/// An alert's detail: any number of lines, possibly none.
pub type AlertDetail = WideText<0, ALERT_DETAIL_MAX, true>;

/// One button's label: one line, never empty.
pub type AlertLabel = BoundedText<1, ALERT_LABEL_MAX>;

/// What kind of thing an alert says, which decides its mark and the word its
/// title is spoken with.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AlertSeverity {
    /// Something the user asked for did not happen.
    Error,
    /// Something will go wrong, or has partly.
    Warning,
    /// Something the user should know.
    Information,
    /// Something only the user can decide.
    Question,
}

impl AlertSeverity {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Information => 3,
            Self::Question => 4,
        }
    }

    const fn from_wire(raw: u8) -> Result<Self, Errno> {
        match raw {
            1 => Ok(Self::Error),
            2 => Ok(Self::Warning),
            3 => Ok(Self::Information),
            4 => Ok(Self::Question),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// What a button does beyond answering its index.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AlertButtonRole {
    /// An answer like any other.
    Plain,
    /// The answer the keyboard starts on, so the one `Enter` gives unless the
    /// user moves on; drawn warm.
    Default,
    /// The answer `Escape` gives: leave things as they were.
    Cancel,
    /// An answer that destroys something. The keyboard never starts on it,
    /// so a stray `Enter` never gives it: only a deliberate move to it does.
    Destructive,
}

impl AlertButtonRole {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Plain => 1,
            Self::Default => 2,
            Self::Cancel => 3,
            Self::Destructive => 4,
        }
    }

    const fn from_wire(raw: u8) -> Result<Self, Errno> {
        match raw {
            1 => Ok(Self::Plain),
            2 => Ok(Self::Default),
            3 => Ok(Self::Cancel),
            4 => Ok(Self::Destructive),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One answer an alert offers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AlertButton {
    label: AlertLabel,
    role: AlertButtonRole,
}

impl AlertButton {
    /// A button reading `label`.
    ///
    /// # Errors
    ///
    /// The label's own refusal: empty, over [`ALERT_LABEL_MAX`] bytes, or
    /// holding a control character.
    pub fn new(label: &str, role: AlertButtonRole) -> Result<Self, Errno> {
        Ok(Self {
            label: AlertLabel::new(label)?,
            role,
        })
    }

    /// What the button reads.
    #[must_use]
    pub fn label(&self) -> &str {
        self.label.as_str()
    }

    /// What the button does beyond answering its index.
    #[must_use]
    pub const fn role(&self) -> AlertButtonRole {
        self.role
    }
}

/// Everything an application says in one alert.
///
/// Valid by construction: one to [`ALERT_MAX_BUTTONS`] buttons, at most one
/// [`Default`](AlertButtonRole::Default), at most one
/// [`Cancel`](AlertButtonRole::Cancel), and at least one that destroys
/// nothing, so every alert can be declined. A spec breaking a rule is refused
/// whole, here and at decode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AlertSpec {
    severity: AlertSeverity,
    headline: AlertHeadline,
    detail: AlertDetail,
    /// Packed from the left: the first `count` slots hold buttons.
    buttons: [Option<AlertButton>; ALERT_MAX_BUTTONS],
    count: u8,
}

impl AlertSpec {
    /// An alert of `severity` saying `headline`, explained by `detail`, and
    /// answered by one of `buttons`.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — no buttons, more than
    ///   [`ALERT_MAX_BUTTONS`], or a headline or detail outside its bounds.
    /// * [`Errno::OutOfRange`] — a second default or cancel button, every
    ///   button destructive, or a control character where none is allowed.
    pub fn new(
        severity: AlertSeverity,
        headline: &str,
        detail: &str,
        buttons: &[AlertButton],
    ) -> Result<Self, Errno> {
        if buttons.len() > ALERT_MAX_BUTTONS {
            return Err(Errno::LengthOutOfRange);
        }
        let mut slots = [None; ALERT_MAX_BUTTONS];
        for (slot, button) in slots.iter_mut().zip(buttons) {
            *slot = Some(*button);
        }
        Self::assemble(
            severity,
            AlertHeadline::new(headline)?,
            &AlertDetail::new(detail)?,
            slots,
        )
    }

    /// [`new`](Self::new) for text that may not fit: a headline or detail
    /// past its bound is shortened at a character boundary and ends in an
    /// ellipsis, and a character neither may carry — a control character in
    /// a file name, say — reads as U+FFFD. What an application quotes is never
    /// its to vouch for, so a long or strange name still gets its alert.
    ///
    /// # Errors
    ///
    /// The buttons' refusals only, as [`new`](Self::new) states them.
    pub fn fitted(
        severity: AlertSeverity,
        headline: &str,
        detail: &str,
        buttons: &[AlertButton],
    ) -> Result<Self, Errno> {
        let mut head = [0u8; ALERT_HEADLINE_MAX];
        let mut body = [0u8; ALERT_DETAIL_MAX];
        let headline = fit(headline, false, &mut head);
        let headline = if headline.is_empty() {
            "\u{2026}"
        } else {
            headline
        };
        Self::new(severity, headline, fit(detail, true, &mut body), buttons)
    }

    /// [`fitted`](Self::fitted) for an alert there is nothing to decide
    /// about: one *OK* button, the default, acknowledges it.
    ///
    /// # Errors
    ///
    /// The *OK* label's refusal, which a well-formed label never gives.
    pub fn acknowledged(
        severity: AlertSeverity,
        headline: &str,
        detail: &str,
    ) -> Result<Self, Errno> {
        let ok = AlertButton::new("OK", AlertButtonRole::Default)?;
        Self::fitted(severity, headline, detail, &[ok])
    }

    /// The one validator both construction and decode go through.
    fn assemble(
        severity: AlertSeverity,
        headline: AlertHeadline,
        detail: &AlertDetail,
        buttons: [Option<AlertButton>; ALERT_MAX_BUTTONS],
    ) -> Result<Self, Errno> {
        let count = buttons.iter().take_while(|slot| slot.is_some()).count();
        if count == 0 || buttons[count..].iter().any(Option::is_some) {
            return Err(Errno::LengthOutOfRange);
        }
        let holding = |role| {
            buttons
                .iter()
                .flatten()
                .filter(|button| button.role == role)
                .count()
        };
        if holding(AlertButtonRole::Default) > 1
            || holding(AlertButtonRole::Cancel) > 1
            || holding(AlertButtonRole::Destructive) == count
        {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            severity,
            headline,
            detail: *detail,
            buttons,
            count: u8::try_from(count).map_err(|_| Errno::LengthOutOfRange)?,
        })
    }

    /// What kind of thing the alert says.
    #[must_use]
    pub const fn severity(&self) -> AlertSeverity {
        self.severity
    }

    /// The one sentence saying what happened.
    #[must_use]
    pub fn headline(&self) -> &str {
        self.headline.as_str()
    }

    /// The paragraph explaining it; empty when the headline says it all.
    #[must_use]
    pub fn detail(&self) -> &str {
        self.detail.as_str()
    }

    /// The answers offered, in the order they are drawn from the left.
    pub fn buttons(&self) -> impl Iterator<Item = &AlertButton> {
        self.buttons.iter().flatten()
    }

    /// How many answers are offered.
    #[must_use]
    pub fn button_count(&self) -> usize {
        usize::from(self.count)
    }
}

/// The ellipsis a shortened text ends in.
const ELLIPSIS: char = '\u{2026}';

/// `text` written into `out` as a text an alert may carry: each character a
/// bounded text refuses replaced by U+FFFD — every control character, and
/// the line feed too unless `lines` — and, when the whole does not fit,
/// shortened at a character boundary to leave room for an ellipsis.
fn fit<'a>(text: &str, lines: bool, out: &'a mut [u8]) -> &'a str {
    let shown = |ch: char| {
        if crate::bounded_text::is_forbidden_character(ch, lines) {
            char::REPLACEMENT_CHARACTER
        } else {
            ch
        }
    };
    let whole: usize = text.chars().map(|ch| shown(ch).len_utf8()).sum();
    let shortened = whole > out.len();
    let room = if shortened {
        out.len().saturating_sub(ELLIPSIS.len_utf8())
    } else {
        out.len()
    };
    let mut len = 0;
    for ch in text.chars().map(shown) {
        if len + ch.len_utf8() > room {
            break;
        }
        len += ch.encode_utf8(&mut out[len..]).len();
    }
    if shortened {
        len += ELLIPSIS.encode_utf8(&mut out[len..]).len();
    }
    core::str::from_utf8(&out[..len]).unwrap_or("")
}

/// How an alert ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AlertOutcome {
    /// The user chose the button at `index`, counted from the left.
    Chosen {
        /// The chosen button's index in the spec.
        index: u8,
    },
    /// The alert came down unanswered: its application withdrew it, its
    /// owner window closed, or the desktop took it down.
    Dismissed,
}

/// Wire discriminant of [`AlertOutcome::Chosen`].
const OUTCOME_CHOSEN: u16 = 1;
/// Wire discriminant of [`AlertOutcome::Dismissed`].
const OUTCOME_DISMISSED: u16 = 2;

/// Byte offset, within an `AlertClosed` event frame, of the outcome.
pub(super) const ALERT_CLOSED_OUTCOME_OFFSET: usize = 24;
/// Byte offset of the chosen index, zero unless the outcome is `Chosen`.
const ALERT_CLOSED_INDEX_OFFSET: usize = ALERT_CLOSED_OUTCOME_OFFSET + 2;
/// End of an `AlertClosed`'s own block; the rest of the frame is reserved.
pub(super) const ALERT_CLOSED_WIRE_END: usize = ALERT_CLOSED_INDEX_OFFSET + 2;

/// Write `outcome` into an event frame.
pub(super) fn write_outcome(out: &mut [u8], outcome: AlertOutcome) {
    let (kind, index) = match outcome {
        AlertOutcome::Chosen { index } => (OUTCOME_CHOSEN, u16::from(index)),
        AlertOutcome::Dismissed => (OUTCOME_DISMISSED, 0),
    };
    put_u16(out, ALERT_CLOSED_OUTCOME_OFFSET, kind);
    put_u16(out, ALERT_CLOSED_INDEX_OFFSET, index);
}

/// Read an event frame's outcome. A dismissal names no index, and a choice
/// names one a spec could have offered.
pub(super) fn read_outcome(bytes: &[u8]) -> Result<AlertOutcome, Errno> {
    let index = read_u16(bytes, ALERT_CLOSED_INDEX_OFFSET);
    match read_u16(bytes, ALERT_CLOSED_OUTCOME_OFFSET) {
        OUTCOME_CHOSEN if usize::from(index) < ALERT_MAX_BUTTONS => Ok(AlertOutcome::Chosen {
            index: u8::try_from(index).map_err(|_| Errno::OutOfRange)?,
        }),
        OUTCOME_DISMISSED if index == 0 => Ok(AlertOutcome::Dismissed),
        _ => Err(Errno::OutOfRange),
    }
}

/// Byte offset of a spec's severity within its operand block.
const SEVERITY_AT: usize = 0;
/// Byte offset of its button count.
const COUNT_AT: usize = 1;
/// Byte offset of its headline's length.
const HEADLINE_LEN_AT: usize = 2;
/// Byte offset of a reserved byte, required zero.
const RESERVED_AT: usize = 3;
/// Byte offset of its detail's length.
const DETAIL_LEN_AT: usize = 4;
/// Byte offset of its first button record: a role then a label length.
const BUTTONS_AT: usize = 6;
/// Bytes one button record takes.
const BUTTON_RECORD_LEN: usize = 2;

/// Encoded size of `spec`'s operand block: the fixed fields, one record per
/// button, then every text it carries, so an alert costs what it says.
pub(super) const fn wire_len(spec: &AlertSpec) -> usize {
    let mut len = BUTTONS_AT
        + spec.count as usize * BUTTON_RECORD_LEN
        + spec.headline.len_byte() as usize
        + spec.detail.len_u16() as usize;
    let mut index = 0;
    while index < ALERT_MAX_BUTTONS {
        if let Some(button) = &spec.buttons[index] {
            len += button.label.len_byte() as usize;
        }
        index += 1;
    }
    len
}

/// The widest operand block any spec encodes to.
pub(super) const MAX_WIRE_LEN: usize = BUTTONS_AT
    + ALERT_MAX_BUTTONS * (BUTTON_RECORD_LEN + ALERT_LABEL_MAX)
    + ALERT_HEADLINE_MAX
    + ALERT_DETAIL_MAX;

/// Write `spec`'s operand block at the front of `out`, which is exactly
/// [`wire_len`] bytes and already zeroed.
pub(super) fn write(out: &mut [u8], spec: &AlertSpec) {
    out[SEVERITY_AT] = spec.severity.to_wire();
    out[COUNT_AT] = spec.count;
    out[HEADLINE_LEN_AT] = spec.headline.len_byte();
    put_u16(out, DETAIL_LEN_AT, spec.detail.len_u16());
    let mut text = BUTTONS_AT + spec.button_count() * BUTTON_RECORD_LEN;
    for (index, button) in spec.buttons().enumerate() {
        let record = BUTTONS_AT + index * BUTTON_RECORD_LEN;
        out[record] = button.role.to_wire();
        out[record + 1] = button.label.len_byte();
    }
    for piece in [spec.headline(), spec.detail()]
        .into_iter()
        .chain(spec.buttons().map(AlertButton::label))
    {
        out[text..text + piece.len()].copy_from_slice(piece.as_bytes());
        text += piece.len();
    }
}

/// Decode a spec from `block`, which must be exactly the operand block its
/// own lengths describe: short is truncation, long is smuggling, and both
/// are refused.
pub(super) fn read(block: &[u8]) -> Result<AlertSpec, Errno> {
    let fixed = block.get(..BUTTONS_AT).ok_or(Errno::BufferTooSmall)?;
    if fixed[RESERVED_AT] != 0 {
        return Err(Errno::BadMagic);
    }
    let severity = AlertSeverity::from_wire(fixed[SEVERITY_AT])?;
    let count = usize::from(fixed[COUNT_AT]);
    if count == 0 || count > ALERT_MAX_BUTTONS {
        return Err(Errno::LengthOutOfRange);
    }
    let headline_len = fixed[HEADLINE_LEN_AT];
    let detail_len = read_u16(fixed, DETAIL_LEN_AT);
    let records_end = BUTTONS_AT + count * BUTTON_RECORD_LEN;
    let records = block
        .get(BUTTONS_AT..records_end)
        .ok_or(Errno::BufferTooSmall)?;
    let (records, _) = records.as_chunks::<BUTTON_RECORD_LEN>();
    let labels_len: usize = records.iter().map(|record| usize::from(record[1])).sum();
    let expected = records_end + usize::from(headline_len) + usize::from(detail_len) + labels_len;
    match block.len().cmp(&expected) {
        core::cmp::Ordering::Less => return Err(Errno::BufferTooSmall),
        core::cmp::Ordering::Greater => return Err(Errno::BadMagic),
        core::cmp::Ordering::Equal => {}
    }
    let mut cursor = records_end;
    let mut take = |len: usize| {
        let piece = &block[cursor..cursor + len];
        cursor += len;
        piece
    };
    let headline = narrow::<1, ALERT_HEADLINE_MAX>(take(usize::from(headline_len)))?;
    let detail = wide(take(usize::from(detail_len)))?;
    let mut buttons = [None; ALERT_MAX_BUTTONS];
    for (slot, record) in buttons.iter_mut().zip(records) {
        *slot = Some(AlertButton {
            role: AlertButtonRole::from_wire(record[0])?,
            label: narrow::<1, ALERT_LABEL_MAX>(take(usize::from(record[1])))?,
        });
    }
    AlertSpec::assemble(severity, headline, &detail, buttons)
}

/// A one-line text from exactly its own bytes.
fn narrow<const MIN: usize, const MAX: usize>(
    bytes: &[u8],
) -> Result<BoundedText<MIN, MAX>, Errno> {
    let len = u8::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?;
    let mut image = [0u8; MAX];
    image
        .get_mut(..bytes.len())
        .ok_or(Errno::LengthOutOfRange)?
        .copy_from_slice(bytes);
    BoundedText::from_wire(len, &image)
}

/// The detail from exactly its own bytes.
fn wide(bytes: &[u8]) -> Result<AlertDetail, Errno> {
    let len = u16::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?;
    let mut image = [0u8; ALERT_DETAIL_MAX];
    image
        .get_mut(..bytes.len())
        .ok_or(Errno::LengthOutOfRange)?
        .copy_from_slice(bytes);
    AlertDetail::from_wire(len, &image)
}

#[cfg(test)]
#[path = "alert_tests.rs"]
mod tests;
