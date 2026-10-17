use super::*;
use crate::window_ipc::{event_endpoint_for, WindowEvent, WindowRequest, WINDOW_ENDPOINT};

fn button(label: &str, role: AlertButtonRole) -> AlertButton {
    AlertButton::new(label, role).expect("a valid label")
}

fn ok() -> AlertButton {
    button("OK", AlertButtonRole::Default)
}

fn spec(detail: &str, buttons: &[AlertButton]) -> AlertSpec {
    AlertSpec::new(
        AlertSeverity::Error,
        "\u{201c}report.pdf\u{201d} could not be moved",
        detail,
        buttons,
    )
    .expect("a valid spec")
}

fn open(alert: &AlertSpec) -> WindowRequest {
    WindowRequest::OpenAlert {
        event_endpoint: event_endpoint_for(42),
        owner_window_id: 7,
        alert: *alert,
    }
}

/// An encoded request, with one byte of room past its end.
struct Frame {
    bytes: [u8; WindowRequest::MAX_WIRE_LEN + 1],
    len: usize,
}

impl Frame {
    fn of(request: &WindowRequest) -> Self {
        let mut bytes = [0u8; WindowRequest::MAX_WIRE_LEN + 1];
        let len = request.encode(&mut bytes).expect("encodes");
        Self { bytes, len }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// The frame with byte `at` set to `value`, decoded.
    fn decoded_with(&self, at: usize, value: u8) -> Result<WindowRequest, Errno> {
        let mut bytes = self.bytes;
        bytes[at] = value;
        WindowRequest::from_bytes(&bytes[..self.len])
    }
}

/// The byte offset of the spec's own block within an encoded open.
const SPEC: usize = 24;

#[test]
fn an_alert_round_trips_with_every_field_it_carries() {
    let detail = "The volume is full.\nFree 12 MB and try again.";
    let request = open(&spec(
        detail,
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Delete", AlertButtonRole::Destructive),
            button("Keep Both", AlertButtonRole::Plain),
            button("Replace", AlertButtonRole::Default),
        ],
    ));
    let frame = Frame::of(&request);
    assert_eq!(frame.len, request.wire_len());
    let decoded = WindowRequest::from_bytes(frame.bytes()).expect("decodes");
    assert_eq!(decoded, request);
    let WindowRequest::OpenAlert { alert, .. } = decoded else {
        panic!("decoded as another operation");
    };
    assert_eq!(alert.detail(), detail);
    assert!(alert.buttons().map(AlertButton::label).eq([
        "Cancel",
        "Delete",
        "Keep Both",
        "Replace"
    ]));
}

#[test]
fn an_alert_costs_what_it_says() {
    let short = open(&spec("", &[ok()]));
    let long = open(&spec("A longer explanation.", &[ok()]));
    assert_eq!(
        long.wire_len() - short.wire_len(),
        "A longer explanation.".len()
    );
}

fn as_str(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).expect("ascii")
}

#[test]
fn the_widest_alert_fits_the_channel_bound() {
    let headline = [b'h'; ALERT_HEADLINE_MAX];
    let detail = [b'd'; ALERT_DETAIL_MAX];
    let label = [b'l'; ALERT_LABEL_MAX];
    let buttons = [
        button(as_str(&label), AlertButtonRole::Plain),
        button(as_str(&label), AlertButtonRole::Plain),
        button(as_str(&label), AlertButtonRole::Plain),
        button(as_str(&label), AlertButtonRole::Plain),
    ];
    let alert = AlertSpec::new(
        AlertSeverity::Question,
        as_str(&headline),
        as_str(&detail),
        &buttons,
    )
    .expect("the widest spec");
    let request = open(&alert);
    assert_eq!(request.wire_len(), SPEC + MAX_WIRE_LEN);
    assert!(request.wire_len() <= WindowRequest::MAX_WIRE_LEN);
    assert_eq!(
        WindowRequest::from_bytes(Frame::of(&request).bytes()),
        Ok(request)
    );
}

#[test]
fn a_spec_offers_one_to_four_answers() {
    let five = [
        ok(),
        button("b", AlertButtonRole::Plain),
        button("c", AlertButtonRole::Plain),
        button("d", AlertButtonRole::Plain),
        button("e", AlertButtonRole::Plain),
    ];
    assert_eq!(
        AlertSpec::new(AlertSeverity::Error, "x", "", &[]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        AlertSpec::new(AlertSeverity::Error, "x", "", &five),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_spec_has_at_most_one_default_and_one_cancel() {
    let defaults = [ok(), button("Also", AlertButtonRole::Default)];
    let cancels = [
        button("No", AlertButtonRole::Cancel),
        button("Never", AlertButtonRole::Cancel),
    ];
    assert_eq!(
        AlertSpec::new(AlertSeverity::Question, "x", "", &defaults),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        AlertSpec::new(AlertSeverity::Question, "x", "", &cancels),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn only_the_detail_may_break_lines() {
    assert_eq!(
        AlertSpec::new(AlertSeverity::Error, "two\nlines", "", &[ok()]),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        AlertSpec::new(AlertSeverity::Error, "", "", &[ok()]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        AlertSpec::new(AlertSeverity::Error, "x", "a\tb", &[ok()]),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        AlertButton::new("O\nK", AlertButtonRole::Plain),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        AlertButton::new("", AlertButtonRole::Plain),
        Err(Errno::LengthOutOfRange)
    );
    assert!(AlertSpec::new(AlertSeverity::Error, "x", "a\nb", &[ok()]).is_ok());
}

#[test]
fn a_frame_one_byte_short_or_long_is_refused() {
    let frame = Frame::of(&open(&spec("detail", &[ok()])));
    assert_eq!(
        WindowRequest::from_bytes(&frame.bytes()[..frame.len - 1]),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        WindowRequest::from_bytes(&frame.bytes[..=frame.len]),
        Err(Errno::BadMagic)
    );
}

#[test]
fn a_dirty_or_unknown_field_is_refused() {
    let frame = Frame::of(&open(&spec("detail", &[ok()])));
    assert_eq!(
        frame.decoded_with(SPEC + SEVERITY_AT, 0),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        frame.decoded_with(SPEC + SEVERITY_AT, 5),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        frame.decoded_with(SPEC + RESERVED_AT, 1),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        frame.decoded_with(SPEC + COUNT_AT, 0),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        frame.decoded_with(SPEC + COUNT_AT, 5),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        frame.decoded_with(SPEC + BUTTONS_AT, 9),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn text_that_is_not_utf8_is_refused() {
    let frame = Frame::of(&open(&spec("", &[ok()])));
    // The headline's first byte, after the one button record.
    assert_eq!(
        frame.decoded_with(SPEC + BUTTONS_AT + BUTTON_RECORD_LEN, 0xFF),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_reserved_endpoint_is_refused() {
    let mut frame = Frame::of(&open(&spec("", &[ok()])));
    frame.bytes[8..16].copy_from_slice(&WINDOW_ENDPOINT.to_le_bytes());
    assert_eq!(
        WindowRequest::from_bytes(frame.bytes()),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_withdrawal_names_one_alert() {
    let request = WindowRequest::CloseAlert { alert_id: 9 };
    let mut frame = Frame::of(&request);
    assert_eq!(WindowRequest::from_bytes(frame.bytes()), Ok(request));
    frame.bytes[8..16].fill(0);
    assert_eq!(
        WindowRequest::from_bytes(frame.bytes()),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn every_answer_round_trips() {
    for outcome in [
        AlertOutcome::Chosen { index: 0 },
        AlertOutcome::Chosen { index: 3 },
        AlertOutcome::Dismissed,
    ] {
        let event = WindowEvent::AlertClosed {
            alert_id: 11,
            outcome,
        };
        assert_eq!(WindowEvent::from_bytes(&event.to_le_bytes()), Ok(event));
        assert_eq!(event.window_id(), None);
    }
}

#[test]
fn an_answer_no_alert_could_have_given_is_refused() {
    let dismissed = WindowEvent::AlertClosed {
        alert_id: 11,
        outcome: AlertOutcome::Dismissed,
    }
    .to_le_bytes();
    let refused = |mut frame: [u8; WindowEvent::WIRE_LEN], at: usize, value: u8| {
        frame[at] = value;
        WindowEvent::from_bytes(&frame)
    };
    assert_eq!(refused(dismissed, 8, 1), Err(Errno::OutOfRange));
    assert_eq!(
        refused(dismissed, ALERT_CLOSED_OUTCOME_OFFSET + 2, 1),
        Err(Errno::OutOfRange)
    );
    assert!(refused(dismissed, ALERT_CLOSED_WIRE_END, 1).is_err());
    let chosen = WindowEvent::AlertClosed {
        alert_id: 11,
        outcome: AlertOutcome::Chosen { index: 0 },
    }
    .to_le_bytes();
    assert_eq!(
        refused(chosen, ALERT_CLOSED_OUTCOME_OFFSET + 2, 4),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn an_answer_names_its_alert() {
    let mut frame = WindowEvent::AlertClosed {
        alert_id: 11,
        outcome: AlertOutcome::Dismissed,
    }
    .to_le_bytes();
    frame[16..24].fill(0);
    assert_eq!(WindowEvent::from_bytes(&frame), Err(Errno::OutOfRange));
}

#[test]
fn a_fitted_alert_says_what_fits_and_ends_in_an_ellipsis() {
    let long = [b'n'; 400];
    let name = as_str(&long);
    let headline = [b'h'; ALERT_HEADLINE_MAX];
    let alert = AlertSpec::fitted(AlertSeverity::Question, name, "fits", &[ok()]).expect("fitted");
    assert!(alert.headline().len() <= ALERT_HEADLINE_MAX);
    assert!(alert.headline().ends_with('\u{2026}'));
    assert!(alert.headline().starts_with("nnn"));
    let exact =
        AlertSpec::fitted(AlertSeverity::Error, as_str(&headline), "", &[ok()]).expect("fitted");
    assert_eq!(
        exact.headline(),
        as_str(&headline),
        "a text that fits is untouched"
    );
}

#[test]
fn a_fitted_alert_never_cuts_a_character_in_two() {
    // 3-byte characters across the 160-byte bound.
    let mut text = [0u8; 300];
    for chunk in text.as_chunks_mut::<3>().0 {
        chunk.copy_from_slice("\u{2603}".as_bytes());
    }
    let alert =
        AlertSpec::fitted(AlertSeverity::Error, as_str(&text), "", &[ok()]).expect("fitted");
    assert!(alert
        .headline()
        .chars()
        .all(|ch| ch == '\u{2603}' || ch == '\u{2026}'));
}

#[test]
fn a_fitted_alert_shows_what_it_may_not_carry_as_a_replacement() {
    let alert = AlertSpec::fitted(
        AlertSeverity::Error,
        "bad\nname\u{1b}[31m",
        "line one\nline\ttwo",
        &[ok()],
    )
    .expect("fitted");
    assert_eq!(alert.headline(), "bad\u{fffd}name\u{fffd}[31m");
    assert_eq!(alert.detail(), "line one\nline\u{fffd}two");
    let empty = AlertSpec::fitted(AlertSeverity::Error, "", "", &[ok()]).expect("fitted");
    assert_eq!(
        empty.headline(),
        "\u{2026}",
        "an alert always says something"
    );
}

#[test]
fn an_acknowledged_alert_is_answered_by_its_one_default_button() {
    let alert = AlertSpec::acknowledged(AlertSeverity::Warning, "Low on space", "Free some.")
        .expect("acknowledged");
    assert_eq!(alert.severity(), AlertSeverity::Warning);
    assert_eq!(alert.button_count(), 1);
    let only = alert.buttons().next().expect("one button");
    assert_eq!(
        (only.label(), only.role()),
        ("OK", AlertButtonRole::Default)
    );
}

#[test]
fn an_alert_that_cannot_be_declined_is_refused() {
    let delete = button("Delete", AlertButtonRole::Destructive);
    let erase = button("Erase", AlertButtonRole::Destructive);
    for buttons in [&[delete][..], &[delete, erase][..]] {
        assert_eq!(
            AlertSpec::new(AlertSeverity::Warning, "Erase the disk?", "", buttons),
            Err(Errno::OutOfRange)
        );
    }
    assert!(AlertSpec::new(
        AlertSeverity::Warning,
        "Erase the disk?",
        "",
        &[button("Cancel", AlertButtonRole::Cancel), delete]
    )
    .is_ok());
}
