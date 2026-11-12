use super::*;
use alloc::vec::Vec;

const POWERED: Control = Control {
    dtr: true,
    rts: true,
};

/// A mouse powered at 0 that has sent its `M`.
fn awake() -> Mouse {
    let mut mouse = Mouse::new();
    mouse.control(POWERED, 0);
    assert_eq!(mouse.send(WAKE), Some((IDENTITY, FORMAT)));
    mouse
}

/// Everything the mouse sends from `t` on, and when the last byte left.
fn drain(mouse: &mut Mouse) -> (Vec<u8>, Nanos) {
    let (mut bytes, mut last) = (Vec::new(), 0);
    while let Some(at) = mouse.next_send() {
        let (byte, format) = mouse.send(at).unwrap();
        assert_eq!(format, FORMAT);
        bytes.push(byte);
        last = at;
    }
    (bytes, last)
}

#[test]
fn rts_and_dtr_power_it_and_it_answers_m_after_waking() {
    let mut mouse = Mouse::new();
    mouse.control(
        Control {
            dtr: true,
            rts: false,
        },
        0,
    );
    assert_eq!(mouse.next_send(), None, "unpowered without RTS");
    mouse.control(POWERED, 5_000);
    assert_eq!(mouse.next_send(), Some(5_000 + WAKE));
    assert_eq!(
        mouse.status(),
        Status::default(),
        "it drives no status line"
    );
}

#[test]
fn a_packet_carries_the_buttons_and_both_movements_in_seven_bit_bytes() {
    let mut mouse = awake();
    mouse.motion(5, -3, button::LEFT, false, WAKE + 1);
    let (bytes, _) = drain(&mut mouse);
    assert_eq!(bytes, [0x6C, 0x05, 0x3D]);
    assert!(bytes.iter().all(|byte| byte & 0x80 == 0));
}

#[test]
fn bytes_leave_a_character_apart() {
    let mut mouse = awake();
    let moved = WAKE + FORMAT.character_time();
    mouse.motion(1, 1, 0, false, moved);
    let mut times = Vec::new();
    while let Some(at) = mouse.next_send() {
        times.push(at);
        mouse.send(at);
    }
    let step = FORMAT.character_time();
    assert_eq!(times, [moved, moved + step, moved + 2 * step]);
}

#[test]
fn movement_beyond_a_packets_reach_is_lost_as_its_counters_saturate() {
    let mut mouse = awake();
    let moved = |bytes: &[u8]| {
        let x = i8::from_le_bytes([((bytes[0] & 0x03) << 6) | bytes[1]]);
        let y = i8::from_le_bytes([((bytes[0] & 0x0C) << 4) | bytes[2]]);
        (x, y)
    };
    mouse.motion(300, -200, 0, false, WAKE + 1);
    let (bytes, last) = drain(&mut mouse);
    assert_eq!(bytes.len(), 3, "one packet; the rest is lost");
    assert_eq!(moved(&bytes), (127, -127));
    mouse.motion(i32::MAX, i32::MIN, 0, false, last + WAKE);
    let (bytes, _) = drain(&mut mouse);
    assert_eq!(bytes.len(), 3, "however far it went");
    assert_eq!(moved(&bytes), (127, -127));
}

#[test]
fn motion_while_a_packet_goes_out_comes_in_the_next() {
    let mut mouse = awake();
    mouse.motion(10, 0, 0, false, WAKE + 1);
    let first = mouse.next_send().unwrap();
    mouse.send(first);
    mouse.motion(-4, 6, 0, false, first + 1);
    let (bytes, _) = drain(&mut mouse);
    assert_eq!(
        bytes.len(),
        5,
        "the rest of the first packet, then the second"
    );
    assert_eq!(&bytes[2..], [0x40 | 0x03, 0x3C, 0x06]);
}

#[test]
fn a_button_alone_sends_a_packet_and_stillness_sends_none() {
    let mut mouse = awake();
    mouse.motion(0, 0, 0, false, WAKE + 1);
    assert_eq!(mouse.next_send(), None);
    mouse.motion(0, 0, button::RIGHT, false, WAKE + 2);
    let (bytes, _) = drain(&mut mouse);
    assert_eq!(bytes, [0x50, 0, 0]);
}

#[test]
fn motion_unpowered_is_forgotten_and_losing_power_drops_what_it_held() {
    let mut mouse = Mouse::new();
    mouse.motion(10, 10, button::LEFT, false, 0);
    assert_eq!(mouse.next_send(), None);
    mouse.control(POWERED, 1);
    let (bytes, _) = drain(&mut mouse);
    assert_eq!(bytes, [IDENTITY], "no stale packet after the M");
    mouse.motion(4, 0, 0, false, 2 * WAKE);
    mouse.control(
        Control {
            dtr: false,
            rts: true,
        },
        2 * WAKE + 1,
    );
    assert_eq!(mouse.next_send(), None);
}

#[test]
fn a_report_is_owed_a_packet_whatever_moved() {
    let mut mouse = awake();
    mouse.motion(0, 0, 0, false, WAKE + 1);
    assert_eq!(mouse.next_send(), None, "nothing changed, nothing sent");
    mouse.motion(0, 0, 0, true, WAKE + 2);
    assert_eq!(drain(&mut mouse).0, [0x40, 0, 0], "a packet of no motion");
    assert_eq!(mouse.next_send(), None, "and the debt paid");
}
