//! Unit tests for the touch frame record.

use super::*;

fn screen() -> TouchFrame {
    TouchFrame::new(
        3,
        TouchSurface::Screen,
        TouchButtons::NONE,
        TouchExtent {
            width: 2_560,
            height: 1_440,
        },
    )
}

fn two_fingers() -> TouchFrame {
    let mut frame = screen();
    frame
        .push(Contact::finger(7, 100, 65_535))
        .expect("room for a contact");
    frame
        .push(Contact {
            kind: ContactKind::Palm,
            ..Contact::finger(9, 0, 4_000)
        })
        .expect("room for a contact");
    frame
}

#[test]
fn a_frame_round_trips_with_its_stamp_and_every_contact() {
    let frame = two_fingers().stamped(0xFEED_BEEF_0000_0001, 0x0123_4567_89AB_CDEF);
    let bytes = frame.to_le_bytes();
    assert_eq!(bytes.len(), TouchFrame::WIRE_LEN);
    let decoded = TouchFrame::from_bytes(&bytes).expect("a valid frame");
    assert_eq!(decoded, frame);
    assert_eq!(decoded.source(), 0xFEED_BEEF_0000_0001);
    assert_eq!(decoded.time_ns(), 0x0123_4567_89AB_CDEF);
    assert_eq!(decoded.contacts().len(), 2);
    assert_eq!(decoded.contacts()[1].kind, ContactKind::Palm);
    // A lift keeps its last position.
    let mut lifting = screen();
    lifting
        .push(Contact::finger(1, 5, 6).lifted())
        .expect("room for a contact");
    let decoded = TouchFrame::from_bytes(&lifting.to_le_bytes()).expect("a valid frame");
    assert_eq!(
        decoded.contacts(),
        &[Contact {
            phase: ContactPhase::Up,
            ..Contact::finger(1, 5, 6)
        }]
    );
}

#[test]
fn a_frame_carries_each_contact_once_and_no_more_than_ten() {
    let mut frame = screen();
    for id in 0..u16::try_from(TOUCH_CONTACTS_MAX).expect("ten") {
        frame
            .push(Contact::finger(id, id, id))
            .expect("room for ten");
    }
    assert_eq!(
        frame.push(Contact::finger(99, 0, 0)),
        Err(Errno::OutOfRange)
    );
    let mut repeated = screen();
    repeated.push(Contact::finger(4, 0, 0)).expect("room");
    assert_eq!(
        repeated.push(Contact::finger(4, 1, 1)),
        Err(Errno::OutOfRange),
        "one contact named twice"
    );
}

#[test]
fn every_malformed_field_is_refused_rather_than_read() {
    let bytes = two_fingers().to_le_bytes();
    let refused = |at: usize, value: u8, why: Errno| {
        let mut forged = bytes;
        forged[at] = value;
        assert_eq!(
            TouchFrame::from_bytes(&forged),
            Err(why),
            "byte {at} = {value}"
        );
    };
    refused(0, b'X', Errno::BadMagic);
    refused(4, 0x7F, Errno::AbiVersionUnsupported);
    refused(24, 0, Errno::OutOfRange);
    refused(24, 4, Errno::OutOfRange);
    refused(25, 0x08, Errno::OutOfRange);
    refused(26, 11, Errno::OutOfRange);
    refused(27, 1, Errno::BadMagic);
    // A contact flag this version does not define, and a dirty slot byte.
    refused(CONTACTS_OFFSET + 6, 0x04, Errno::OutOfRange);
    refused(CONTACTS_OFFSET + 7, 1, Errno::BadMagic);
    // A slot past the count is not a contact and must be zero.
    refused(CONTACTS_OFFSET + 2 * CONTACT_LEN + 3, 1, Errno::BadMagic);
    // A count that names a third slot holding the first's id repeats it.
    let mut repeated = bytes;
    repeated[26] = 3;
    repeated[CONTACTS_OFFSET + 2 * CONTACT_LEN..CONTACTS_OFFSET + 3 * CONTACT_LEN]
        .copy_from_slice(&bytes[CONTACTS_OFFSET..CONTACTS_OFFSET + CONTACT_LEN]);
    assert_eq!(TouchFrame::from_bytes(&repeated), Err(Errno::OutOfRange));
    assert_eq!(
        TouchFrame::from_bytes(&bytes[..TouchFrame::WIRE_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn a_surface_knows_whether_a_contact_names_a_place() {
    assert!(TouchSurface::Screen.is_direct());
    assert!(!TouchSurface::Touchpad.is_direct());
    assert!(!TouchSurface::Clickpad.is_direct());
    for surface in [
        TouchSurface::Touchpad,
        TouchSurface::Clickpad,
        TouchSurface::Screen,
    ] {
        assert_eq!(TouchSurface::from_code(surface.code()), Ok(surface));
    }
}

#[test]
fn buttons_hold_only_the_three_defined_bits() {
    let both =
        TouchButtons::from_bits(TouchButtons::PRIMARY | TouchButtons::MIDDLE).expect("defined");
    assert!(both.holds(TouchButtons::PRIMARY));
    assert!(!both.holds(TouchButtons::SECONDARY));
    assert_eq!(TouchButtons::from_bits(0x80), Err(Errno::OutOfRange));
}

#[test]
fn a_pinch_phase_round_trips_and_knows_when_it_is_over() {
    for phase in [
        PinchPhase::Begin,
        PinchPhase::Update,
        PinchPhase::End,
        PinchPhase::Cancel,
    ] {
        assert_eq!(PinchPhase::from_code(phase.code()), Ok(phase));
    }
    assert!(PinchPhase::End.ends() && PinchPhase::Cancel.ends());
    assert!(!PinchPhase::Begin.ends() && !PinchPhase::Update.ends());
    assert_eq!(PinchPhase::from_code(0), Err(Errno::OutOfRange));
    assert_eq!(PinchPhase::from_code(5), Err(Errno::OutOfRange));
}
