//! The protocol: every message encoding to bytes that decode to it, and
//! every payload outside its bounds refused.

use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn command_bytes(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

fn event_bytes(event: &Event<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    event.encode(&mut w);
    w.finish()
}

const SCREEN: Screen = Screen {
    width: 640,
    height: 480,
    x_eig: 1,
    y_eig: 1,
    border: 0x00_FF_00,
    blanked: false,
};

#[test]
fn every_command_decodes_to_itself() {
    let reply = vec![7; RELAY_MOST];
    let line = vec![b'a'; LINE_MOST];
    for command in [
        Command::Run,
        Command::Hold,
        Command::Input {
            context: 3,
            event: InputEvent::Key(0x8F),
        },
        Command::Input {
            context: 0,
            event: InputEvent::Transition {
                key: 0x2F,
                down: true,
            },
        },
        Command::Input {
            context: 1,
            event: InputEvent::Pointer {
                x: -1,
                y: 1024,
                buttons: 0b101,
            },
        },
        Command::Input {
            context: 1,
            event: InputEvent::Motion {
                dx: 4,
                dy: -4,
                buttons: 0,
            },
        },
        Command::Released { bytes: 1 << 20 },
        Command::Answer {
            serial: 9,
            reply: &reply,
        },
        Command::Answer {
            serial: 9,
            reply: &[],
        },
        Command::Unanswered { serial: 2 },
        Command::Launch {
            serial: 5,
            line: &line,
        },
        Command::Quit { context: 4 },
    ] {
        let bytes = command_bytes(&command);
        assert_eq!(Command::decode(&bytes), Ok(command), "{command:?}");
    }
}

#[test]
fn every_event_decodes_to_itself() {
    let request = vec![1; RELAY_MOST];
    let text = vec![b'e'; tairix_riscos_kernel::error::TEXT];
    for event in [
        Event::Credit {
            context: 2,
            applied: u32::MAX,
        },
        Event::Launched {
            serial: 1,
            context: 6,
        },
        Event::Refused {
            serial: 1,
            error: Some(Error {
                number: 0x1E6,
                text: b"Unknown command",
            }),
        },
        Event::Refused {
            serial: 1,
            error: None,
        },
        Event::Screen {
            context: 0,
            screen: SCREEN,
        },
        Event::Damage {
            context: 0,
            first: 10,
            lines: 470,
            offset: 0x40,
        },
        Event::Relay {
            serial: 3,
            context: 0,
            parked: false,
            side: Side::Files,
            request: &request,
        },
        Event::Relay {
            serial: 4,
            context: 2,
            parked: true,
            side: Side::Fonts,
            request: b"glyph",
        },
        Event::Ended {
            context: 1,
            ending: Ending::Exit { code: 7 },
        },
        Event::Ended {
            context: 1,
            ending: Ending::Error(Error {
                number: 0x11,
                text: &text,
            }),
        },
        Event::Ended {
            context: 1,
            ending: Ending::Fault,
        },
        Event::Ended {
            context: 1,
            ending: Ending::Reset { hard: true },
        },
        Event::Ended {
            context: 1,
            ending: Ending::Unmodelled(Unmodelled::System {
                register: [0, 1, 0, 2],
                write: true,
            }),
        },
        Event::Ended {
            context: 1,
            ending: Ending::Quit,
        },
    ] {
        let bytes = event_bytes(&event);
        assert_eq!(Event::decode(&bytes), Ok(event), "{event:?}");
    }
}

#[test]
fn a_payload_with_bytes_left_over_or_cut_short_is_refused() {
    let mut bytes = command_bytes(&Command::Quit { context: 1 });
    bytes.push(0);
    assert_eq!(Command::decode(&bytes), Err(WireError::Malformed));
    let bytes = event_bytes(&Event::Credit {
        context: 1,
        applied: 2,
    });
    for cut in 0..bytes.len() {
        assert!(Event::decode(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    assert!(Command::decode(&[]).is_err());
    assert_eq!(Command::decode(&[0]), Err(WireError::Malformed));
    assert_eq!(Event::decode(&[0xFF]), Err(WireError::Malformed));
}

#[test]
fn fields_outside_their_bounds_are_refused() {
    let long_reply = vec![0; RELAY_MOST + 1];
    let long_line = vec![b'a'; LINE_MOST + 1];
    for command in [
        Command::Answer {
            serial: 1,
            reply: &long_reply,
        },
        Command::Launch {
            serial: 1,
            line: &long_line,
        },
        Command::Launch {
            serial: 1,
            line: b"Run\rx",
        },
        Command::Input {
            context: 1,
            event: InputEvent::Pointer {
                x: 0,
                y: 0,
                buttons: 8,
            },
        },
    ] {
        assert!(
            Command::decode(&command_bytes(&command)).is_err(),
            "{command:?}"
        );
    }
    let long_text = vec![b'e'; tairix_riscos_kernel::error::TEXT + 1];
    for event in [
        Event::Screen {
            context: 0,
            screen: Screen { width: 0, ..SCREEN },
        },
        Event::Screen {
            context: 0,
            screen: Screen {
                height: SCREEN_MOST + 1,
                ..SCREEN
            },
        },
        Event::Screen {
            context: 0,
            screen: Screen { x_eig: 4, ..SCREEN },
        },
        Event::Screen {
            context: 0,
            screen: Screen {
                border: 0x100_0000,
                ..SCREEN
            },
        },
        Event::Damage {
            context: 0,
            first: 0,
            lines: 0,
            offset: 0,
        },
        Event::Damage {
            context: 0,
            first: SCREEN_MOST,
            lines: 1,
            offset: 0,
        },
        Event::Ended {
            context: 0,
            ending: Ending::Error(Error {
                number: 1,
                text: &long_text,
            }),
        },
        Event::Ended {
            context: 0,
            ending: Ending::Error(Error {
                number: 1,
                text: b"a\0b",
            }),
        },
        Event::Refused {
            serial: 1,
            error: Some(Error {
                number: 1,
                text: &long_text,
            }),
        },
    ] {
        assert!(Event::decode(&event_bytes(&event)).is_err(), "{event:?}");
    }
}

#[test]
fn the_largest_relay_is_the_largest_payload() {
    let most = vec![b'x'; RELAY_MOST];
    let relay = Event::Relay {
        serial: u32::MAX,
        context: u32::MAX,
        parked: true,
        side: Side::Fonts,
        request: &most,
    };
    assert_eq!(event_bytes(&relay).len(), PAYLOAD_MOST);
    let answer = Command::Answer {
        serial: u32::MAX,
        reply: &most,
    };
    assert!(command_bytes(&answer).len() <= PAYLOAD_MOST);
    let line = vec![b'x'; LINE_MOST];
    let launch = Command::Launch {
        serial: u32::MAX,
        line: &line,
    };
    assert!(command_bytes(&launch).len() <= PAYLOAD_MOST);
}

#[test]
fn a_line_with_a_control_character_or_too_long_is_not_carried() {
    assert!(carries(b"Run HostFS::Disc.$.!Edit"));
    assert!(carries(&[b'x'; LINE_MOST]));
    assert!(!carries(&[b'x'; LINE_MOST + 1]));
    for control in [0, b'\r', b'\n', 0x1F, 0x7F] {
        assert!(!carries(&[b'a', control, b'b']), "{control:#X}");
    }
}

#[test]
fn a_flag_is_nought_or_one_and_nothing_else() {
    let mut bytes = event_bytes(&Event::Ended {
        context: 0,
        ending: Ending::Reset { hard: false },
    });
    *bytes.last_mut().expect("its flag") = 2;
    assert_eq!(Event::decode(&bytes), Err(WireError::Malformed));
}

#[test]
fn an_ending_is_the_program_s() {
    assert_eq!(
        Ending::of(Ended::Exit { code: 3 }, b""),
        Ending::Exit { code: 3 }
    );
    assert_eq!(
        Ending::of(
            Ended::Error {
                number: 0x11,
                block: 0x8000
            },
            b"Escape"
        ),
        Ending::Error(Error {
            number: 0x11,
            text: b"Escape"
        })
    );
    assert_eq!(
        Ending::of(
            Ended::Reset {
                hard: false,
                reverse_boot: true
            },
            b""
        ),
        Ending::Reset { hard: false }
    );
    assert_eq!(Ending::of(Ended::Fault, b""), Ending::Fault);
}
