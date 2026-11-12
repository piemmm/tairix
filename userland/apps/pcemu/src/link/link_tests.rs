//! The machine protocol: every command and event read back as written, every
//! configuration field and choice carried, the largest of each within the
//! session's bounds, and malformed payloads refused.

use super::config::{self, CONFIG_BYTES};
use super::layout::{Layout, PIXEL, SLOTS_PER_ADAPTER};
use super::*;
use crate::board::{
    BoardConfig, EmsConfig, FixedAdapter, FloppyAdapter, ParallelPort, PortsConfig, SerialPort,
    SoundCard, SoundConfig,
};
use crate::chips::ppi::Wiring;
use crate::cpu::CpuModel;
use crate::machine::Template;
use crate::ports::parallel::Device;
use crate::ports::serial::{Model, Plug};
use crate::storage::fdc::{DriveKind, Interface};
use crate::storage::hdc::{Recording, MAX_TRACKS};
use crate::video::monitor::{Monitor, Phosphor};
use crate::video::Adapter;
use alloc::vec;
use alloc::vec::Vec;
use tairix_sandbox::wire::Writer;

pub(super) fn command_bytes(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

pub(super) fn event_bytes(event: &Event<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    event.encode(&mut w);
    w.finish()
}

/// `payload` cut short at every length, and grown by a byte, each refused.
pub(super) fn refused_cut_or_grown(payload: &[u8], decode: impl Fn(&[u8]) -> bool) {
    for len in 0..payload.len() {
        assert!(!decode(&payload[..len]), "{payload:02X?} cut to {len}");
    }
    let mut grown = payload.to_vec();
    grown.push(0);
    assert!(!decode(&grown), "{payload:02X?} grown");
}

fn setup(config: BoardConfig) -> Setup {
    Setup {
        config,
        fixed: [
            Some(FixedDisk {
                medium: 1,
                sectors: 615 * 4 * 26,
                read_only: false,
                kind: crate::dos::setup::ST_238R,
                format: LowLevel {
                    interleave: 3,
                    bad_tracks: vec![7, 900],
                },
            }),
            None,
        ],
        audio_target: 2048,
        room: 96 << 20,
        seed: [0x5A; 16],
        band: PressureBand::Moderate,
        clock: Some(CivilTime {
            year: 1991,
            month: 6,
            day: 11,
            hour: 5,
            minute: 0,
            second: 59,
        }),
        resume: Some(Resume {
            len: 3_000_000,
            away: 86_400 * 1_000_000_000,
        }),
    }
}

#[test]
fn every_command_reads_back_as_written() {
    let sector = [0xA5u8; SECTOR];
    let commands = [
        Command::Start(setup(Template::TurboXt.config())),
        Command::Insert {
            drive: 1,
            medium: 9,
            form: Form::Sectors {
                count: 2880,
                read_only: true,
            },
        },
        Command::Insert {
            drive: 0,
            medium: 10,
            form: Form::Archive { bytes: 368_001 },
        },
        Command::Eject { drive: 3 },
        Command::Run,
        Command::Hold,
        Command::Reset,
        Command::Speed(CpuClock::TURBO_XT),
        Command::Key {
            scan: Scan::plain(0x1C),
            down: true,
        },
        Command::Key {
            scan: Scan::extended(0x48),
            down: false,
        },
        Command::ReleaseKeys,
        Command::Mouse {
            dx: -40_000,
            dy: 7,
            buttons: 0b11,
        },
        Command::Pointer {
            adapter: 1,
            x: 753,
            y: u16::MAX,
            buttons: 0b01,
        },
        Command::Sticks(Sticks {
            axes: [Some(0), None, Some(65_535), Some(1)],
            buttons: 0b1010,
        }),
        Command::Clock(CivilTime {
            year: -45,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 0,
        }),
        Command::Fetched {
            request: 77,
            answer: Ok(&sector),
        },
        Command::Fetched {
            request: 78,
            answer: Err(MediaError::Failed),
        },
        Command::Stored {
            request: 79,
            answer: Ok(()),
        },
        Command::Stored {
            request: 80,
            answer: Err(MediaError::WriteProtected),
        },
        Command::Released { slot: 3 },
        Command::Fill { frames: 4_000_000 },
        Command::Pressure(PressureBand::Critical),
        Command::Suspend,
        Command::Recalled {
            request: 81,
            bytes: &sector,
        },
        Command::ReadText,
        Command::Type(b"dir\r\t\x80\xFF ~"),
        Command::StopTyping,
        Command::Compiled {
            request: u32::MAX,
            code: Some(Installed {
                address: 0x4_0000_0000,
                len: MOST_CODE_BYTES as u64,
            }),
        },
        Command::Compiled {
            request: 0,
            code: None,
        },
        Command::Interpret,
    ];
    for command in &commands {
        let bytes = command_bytes(command);
        assert_eq!(Command::decode(&bytes).as_ref(), Ok(command), "{command:?}");
        refused_cut_or_grown(&bytes, |b| Command::decode(b).is_ok());
    }
}

/// One event of every kind and shape, over `sector` and `sound`.
fn every_event<'a>(sector: &'a [u8; SECTOR], sound: &'a [u8]) -> Vec<Event<'a>> {
    vec![
        Event::Ready,
        Event::Refused(Refusal::Board(ConfigError::Expanded)),
        Event::Refused(Refusal::Firmware),
        Event::Refused(Refusal::Fixed(1, AttachError::BadTrack)),
        Event::Refused(Refusal::Share),
        Event::Refused(Refusal::Memory),
        Event::Inserted {
            drive: 0,
            medium: 4,
            outcome: Ok(()),
        },
        Event::Inserted {
            drive: 1,
            medium: 5,
            outcome: Err(InsertRefusal::Archive),
        },
        Event::Ejected {
            drive: 1,
            medium: 5,
        },
        Event::Frame {
            slot: 2,
            width: 640,
            height: 400,
        },
        Event::OutOfRange { adapter: 1 },
        Event::Audio(sound),
        Event::Fetch {
            request: 1,
            medium: 2,
            first: u64::MAX,
            count: MAX_RUN,
        },
        Event::Store {
            request: 3,
            medium: 4,
            sector: 5,
            data: sector,
        },
        Event::Formatted {
            drive: 1,
            format: LowLevel {
                interleave: 1,
                bad_tracks: vec![0, 16_383],
            },
        },
        Event::Credit { applied: u32::MAX },
        Event::Pointing { follows: true },
        Event::Pointing { follows: false },
        Event::ClockSet(CivilTime {
            year: 1991,
            month: 6,
            day: 11,
            hour: 5,
            minute: 0,
            second: 59,
        }),
        Event::Lights(crate::board::Lights {
            floppy: 0b1001,
            fixed: 0b10,
        }),
        Event::Resumed(true),
        Event::Resumed(false),
        Event::Recall {
            request: 9,
            offset: 32_768,
            len: 512,
        },
        Event::Kept(sector),
        Event::Suspended(Ok(1_234_567)),
        Event::Suspended(Err(Unkept::Memory)),
        Event::Suspended(Err(Unkept::Size)),
        Event::Text(Ok(Text {
            columns: 3,
            rows: 2,
            codes: b"\x01ab c\xDB",
        })),
        Event::Text(Err(Unread::NoText)),
        Event::Text(Err(Unread::TooLarge)),
        Event::Text(Err(Unread::Memory)),
        Event::Typed(u32::MAX),
        Event::Interpreting,
        Event::TextShown(true),
        Event::TextShown(false),
        Event::Compile {
            request: 12,
            cs: 0xF000,
            ip: 0xFFF0,
            bytes: sound,
        },
    ]
}

#[test]
fn every_event_reads_back_as_written() {
    let sector = [0x3Cu8; SECTOR];
    let sound = [1u8, 2, 3, 4, 5, 6, 7, 8];
    for event in &every_event(&sector, &sound) {
        let bytes = event_bytes(event);
        assert_eq!(Event::decode(&bytes).as_ref(), Ok(event), "{event:?}");
        if !matches!(event, Event::Audio(_) | Event::Kept(_)) {
            refused_cut_or_grown(&bytes, |b| Event::decode(b).is_ok());
        }
    }
}

#[test]
fn sound_comes_only_in_whole_frames_and_never_more_than_a_frame_holds() {
    let decode = |len: usize| {
        let mut bytes = vec![tag::AUDIO];
        bytes.resize(1 + len, 0);
        Event::decode(&bytes).is_ok()
    };
    assert!(!decode(0), "no sound at all");
    assert!(!decode(AUDIO_FRAME + 1), "a split frame");
    assert!(decode(AUDIO_FRAME));
    assert!(decode(MAX_AUDIO_FRAMES * AUDIO_FRAME));
    assert!(!decode((MAX_AUDIO_FRAMES + 1) * AUDIO_FRAME));
}

/// Ports holding each serial model and plug, and each parallel device but
/// `Nothing`.
fn every_port() -> PortsConfig {
    PortsConfig {
        serial: [
            Some(SerialPort {
                model: Model::Ins8250,
                device: Plug::Mouse,
            }),
            Some(SerialPort {
                model: Model::Ns16450,
                device: Plug::Nothing,
            }),
            None,
            Some(SerialPort {
                model: Model::Ns16450,
                device: Plug::Mouse,
            }),
        ],
        parallel: [
            Some(ParallelPort {
                base: 0x3BC,
                device: Device::Covox,
            }),
            Some(ParallelPort {
                base: 0x378,
                device: Device::SoundSource,
            }),
            Some(ParallelPort {
                base: 0x278,
                device: Device::Printer,
            }),
        ],
        game: false,
        clock: true,
    }
}

#[test]
fn every_choice_a_configuration_has_is_carried() {
    let mut configs: Vec<BoardConfig> = [
        Template::IbmPc,
        Template::IbmXt,
        Template::TurboXt,
        Template::Workstation186,
    ]
    .iter()
    .map(|t| t.config())
    .collect();
    let base = Template::IbmXt.config();
    for cpu in [
        CpuModel::I8088,
        CpuModel::I8086,
        CpuModel::V20,
        CpuModel::V30,
        CpuModel::I80188,
        CpuModel::I80186,
    ] {
        configs.push(BoardConfig {
            cpu,
            wiring: Wiring::Pc,
            ..base
        });
    }
    for phosphor in [Phosphor::Green, Phosphor::Amber, Phosphor::White] {
        for monitor in [Monitor::Colour, Monitor::Composite, Monitor::Mono(phosphor)] {
            configs.push(BoardConfig {
                video: [Some(Adapter::Mda(phosphor)), Some(Adapter::Cga(monitor))],
                ..base
            });
            configs.push(BoardConfig {
                video: [
                    Some(Adapter::Hercules(phosphor)),
                    Some(Adapter::Ega(monitor)),
                ],
                ..base
            });
            configs.push(BoardConfig {
                video: [None, Some(Adapter::Vga(monitor))],
                ..base
            });
        }
    }
    for drive in [
        DriveKind::Dd525,
        DriveKind::Hd525,
        DriveKind::Dd35,
        DriveKind::Hd35,
    ] {
        configs.push(BoardConfig {
            floppy: Some(FloppyAdapter {
                interface: Interface::HighDensity,
                drives: [None, Some(drive), None, Some(drive)],
            }),
            fixed: Some(FixedAdapter {
                recording: Recording::Rll,
                jumpers: 0xA5,
            }),
            ..base
        });
    }
    configs.push(BoardConfig {
        floppy: None,
        fixed: None,
        sound: SoundConfig {
            stream_rate: 44_100,
            card: Some(SoundCard::Blaster {
                base: 0x240,
                irq: 7,
            }),
        },
        ports: every_port(),
        ems: Some(EmsConfig {
            port: 0x2E8,
            frame: 0xE_0000,
            pages: 512,
        }),
        ..base
    });
    configs.push(BoardConfig {
        sound: SoundConfig {
            stream_rate: 1,
            card: Some(SoundCard::AdLib),
        },
        ems: None,
        ..base
    });
    for config in &configs {
        let mut w = Writer::new();
        config::put_config(&mut w, config);
        let bytes = w.finish();
        assert!(bytes.len() <= CONFIG_BYTES, "{} bytes", bytes.len());
        let mut r = Reader::new(&bytes);
        assert_eq!(r.whole(config::config).as_ref(), Ok(config));
    }
}

#[test]
fn a_rate_of_zero_and_a_band_or_date_out_of_range_are_refused() {
    let speed = |num: u64, den: u64| {
        let mut bytes = command_bytes(&Command::Speed(CpuClock::PC));
        bytes[1..9].copy_from_slice(&num.to_le_bytes());
        bytes[9..17].copy_from_slice(&den.to_le_bytes());
        match Command::decode(&bytes) {
            Ok(Command::Speed(clock)) => Some(clock),
            _ => None,
        }
    };
    assert_eq!(speed(0, 1), None, "no numerator");
    assert_eq!(
        speed(210_472_505_638, 3),
        None,
        "a clock no processor runs at"
    );
    assert_eq!(speed(1 << 63, 1 << 40), None, "a ratio no crystal has");
    assert_eq!(speed(10_000_000, 1), Some(CpuClock::TURBO_XT));
    assert!(Command::decode(&[tag::PRESSURE, 5]).is_err());
    let mut clock = command_bytes(&Command::Clock(CivilTime {
        year: 2000,
        month: 2,
        day: 29,
        hour: 0,
        minute: 0,
        second: 0,
    }));
    for (at, bad) in [
        (9, 13),
        (9, 0),
        (10, 32),
        (10, 0),
        (11, 24),
        (12, 60),
        (13, 60),
    ] {
        let good = clock[at];
        clock[at] = bad;
        assert!(Command::decode(&clock).is_err(), "byte {at} as {bad}");
        clock[at] = good;
    }
    assert!(Command::decode(&clock).is_ok());
    assert!(
        Command::decode(&[tag::KEY, 0x1C, 2]).is_err(),
        "a key half down"
    );
    assert!(
        Command::decode(&[0x7F]).is_err()
            && Event::decode(&[0x7F]).is_err()
            && Command::decode(&[]).is_err()
    );
}

#[test]
fn an_archive_is_refused_empty_or_past_the_largest_diskette() {
    for (bytes, ok) in [
        (0, false),
        (1, true),
        (MAX_ARCHIVE, true),
        (MAX_ARCHIVE + 1, false),
    ] {
        let command = Command::Insert {
            drive: 0,
            medium: 1,
            form: Form::Archive { bytes },
        };
        assert_eq!(
            Command::decode(&command_bytes(&command)).is_ok(),
            ok,
            "{bytes}"
        );
    }
    assert_eq!(
        Form::Archive { bytes: 513 }.units(),
        2,
        "the last unit padded"
    );
    assert!(Form::Archive { bytes: 1 }.read_only());
}

#[test]
fn a_bad_track_list_longer_than_any_drive_is_refused_before_it_is_read() {
    let mut bytes = vec![tag::FORMATTED, 0, 3];
    bytes.extend(u32::try_from(MAX_TRACKS + 1).unwrap().to_le_bytes());
    bytes.resize(bytes.len() + 4 * (MAX_TRACKS + 1), 0);
    assert!(Event::decode(&bytes).is_err());
    let mut claimed = vec![tag::FORMATTED, 0, 3];
    claimed.extend(1000u32.to_le_bytes());
    assert!(
        Event::decode(&claimed).is_err(),
        "a count the payload does not hold"
    );
}

#[test]
fn a_snapshot_piece_is_never_empty_nor_more_than_a_piece() {
    let piece = vec![7u8; KEPT_PIECE];
    assert!(Event::decode(&event_bytes(&Event::Kept(&piece))).is_ok());
    assert!(Event::decode(&[0x91]).is_err(), "an empty piece");
    let mut over = event_bytes(&Event::Kept(&piece));
    over.push(7);
    assert!(Event::decode(&over).is_err(), "a piece too large");
    let too_long = vec![0u8; KEPT_PIECE + 1];
    let recalled = command_bytes(&Command::Recalled {
        request: 0,
        bytes: &too_long,
    });
    assert!(Command::decode(&recalled).is_err());
}

#[test]
fn text_to_type_is_never_empty_more_than_the_window_or_a_control() {
    let window = vec![b'a'; TYPE_WINDOW];
    assert!(Command::decode(&command_bytes(&Command::Type(&window))).is_ok());
    let over = vec![b'a'; TYPE_WINDOW + 1];
    assert!(Command::decode(&command_bytes(&Command::Type(&over))).is_err());
    assert!(Command::decode(&command_bytes(&Command::Type(&[]))).is_err());
    for code in 0..=u8::MAX {
        let bytes = command_bytes(&Command::Type(&[b'x', code]));
        assert_eq!(
            Command::decode(&bytes).is_ok(),
            typeable(code),
            "{code:#04x}"
        );
    }
    let admitted: Vec<u8> = (0..=u8::MAX).filter(|&code| typeable(code)).collect();
    assert_eq!(
        admitted.len(),
        2 + 95 + 128,
        "Tab, Enter, printable ASCII, the upper half"
    );
}

#[test]
fn a_text_screen_is_of_a_shape_some_card_shows_and_holds_every_cell() {
    let cells = vec![b' '; usize::from(MOST_TEXT.0) * usize::from(MOST_TEXT.1)];
    let text = |columns, rows, codes| {
        event_bytes(&Event::Text(Ok(Text {
            columns,
            rows,
            codes,
        })))
    };
    let (columns, rows) = MOST_TEXT;
    let largest = text(columns, rows, &cells);
    assert!(Event::decode(&largest).is_ok());
    assert!(2 * (FRAME_HEADER_LEN + largest.len()) <= FRONT_INBOUND);
    for (columns, rows) in [(0, 1), (1, 0), (columns + 1, 1), (1, rows + 1)] {
        let n = usize::from(columns) * usize::from(rows);
        let bytes = text(columns, rows, &cells[..n.min(cells.len())]);
        assert!(Event::decode(&bytes).is_err(), "{columns}x{rows}");
    }
    assert!(Event::decode(&text(2, 2, b"abc")).is_err(), "a cell short");
    assert!(Event::decode(&[0x93, 4]).is_err(), "no such refusal");
}

#[test]
fn a_snapshot_to_resume_is_neither_empty_nor_larger_than_any() {
    for len in [0, u32::try_from(most_kept() + 1).unwrap()] {
        let mut start = setup(Template::IbmPc.config());
        start.resume = Some(Resume { len, away: 0 });
        let bytes = command_bytes(&Command::Start(start));
        assert!(Command::decode(&bytes).is_err(), "{len}");
    }
}

#[test]
fn the_largest_messages_fit_the_sessions_bounds() {
    let every_track: Vec<u32> = (0..u32::try_from(MAX_TRACKS).unwrap()).collect();
    let disk = FixedDisk {
        medium: u16::MAX,
        sectors: u64::MAX,
        read_only: true,
        kind: crate::dos::setup::ST_4038,
        format: LowLevel {
            interleave: 0xFF,
            bad_tracks: every_track.clone(),
        },
    };
    let mut largest = setup(Template::Workstation186.config());
    largest.fixed = [Some(disk.clone()), Some(disk)];
    let start = command_bytes(&Command::Start(largest));
    assert!(
        FRAME_HEADER_LEN + start.len() <= WORKER_INBOUND,
        "{} bytes",
        start.len()
    );
    let run = vec![0u8; usize::from(MAX_RUN) * SECTOR];
    let fetched = command_bytes(&Command::Fetched {
        request: u32::MAX,
        answer: Ok(&run),
    });
    assert!(FRAME_HEADER_LEN + fetched.len() <= FETCHED_MOST);
    assert!(Command::decode(&fetched).is_ok());
    let piece = vec![0u8; KEPT_PIECE];
    let recalled = command_bytes(&Command::Recalled {
        request: u32::MAX,
        bytes: &piece,
    });
    assert!(FRAME_HEADER_LEN + recalled.len() <= FETCHED_MOST);
    let kept = event_bytes(&Event::Kept(&piece));
    assert!(2 * (FRAME_HEADER_LEN + kept.len()) <= FRONT_INBOUND);
    let formatted = event_bytes(&Event::Formatted {
        drive: 1,
        format: LowLevel {
            interleave: 1,
            bad_tracks: every_track,
        },
    });
    assert!(2 * (FRAME_HEADER_LEN + formatted.len()) <= FRONT_INBOUND);
    let insert = command_bytes(&Command::Insert {
        drive: 0,
        medium: 1,
        form: Form::Sectors {
            count: 1,
            read_only: false,
        },
    });
    let sticks = command_bytes(&Command::Sticks(Sticks {
        axes: [Some(1); 4],
        buttons: 0,
    }));
    let clock = command_bytes(&Command::Clock(CivilTime {
        year: 0,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    }));
    for credited in [
        insert,
        sticks,
        clock,
        command_bytes(&Command::Speed(CpuClock::PC)),
        command_bytes(&Command::ReadText),
        command_bytes(&Command::StopTyping),
    ] {
        assert!(FRAME_HEADER_LEN + credited.len() <= CREDITED_MOST);
    }
    let window = vec![b'a'; TYPE_WINDOW];
    let typed = command_bytes(&Command::Type(&window));
    assert!(FRAME_HEADER_LEN + typed.len() <= WORKER_INBOUND);
    assert!(FRAME_HEADER_LEN + typed.len() <= TYPED_MOST);
    let most = vec![0x90; MOST_BLOCK_BYTES];
    let compile = event_bytes(&Event::Compile {
        request: u32::MAX,
        cs: u16::MAX,
        ip: u16::MAX,
        bytes: &most,
    });
    assert!(compile.len() <= COMPILE_MOST);
    assert!(2 * (FRAME_HEADER_LEN + compile.len()) <= FRONT_INBOUND);
    let compiled = command_bytes(&Command::Compiled {
        request: u32::MAX,
        code: Some(Installed {
            address: u64::MAX,
            len: 1,
        }),
    });
    assert!(FRAME_HEADER_LEN + compiled.len() <= COMPILED_MOST);
}

#[test]
fn a_block_is_never_empty_nor_longer_than_any_and_its_code_never_none_or_too_long() {
    let compile = |bytes: &[u8]| {
        Event::decode(&event_bytes(&Event::Compile {
            request: 0,
            cs: 0,
            ip: 0,
            bytes,
        }))
        .is_ok()
    };
    assert!(compile(&vec![0x90; MOST_BLOCK_BYTES]));
    assert!(!compile(&[]));
    assert!(!compile(&vec![0x90; MOST_BLOCK_BYTES + 1]));
    let compiled = |len| {
        Command::decode(&command_bytes(&Command::Compiled {
            request: 0,
            code: Some(Installed {
                address: 0x1000,
                len,
            }),
        }))
        .is_ok()
    };
    assert!(compiled(1) && compiled(MOST_CODE_BYTES as u64));
    assert!(!compiled(0) && !compiled(MOST_CODE_BYTES as u64 + 1));
    let mut flagged = command_bytes(&Command::Compiled {
        request: 0,
        code: None,
    });
    *flagged.last_mut().unwrap() = 2;
    assert!(Command::decode(&flagged).is_err(), "a flag is 0 or 1");
}

#[test]
fn only_input_and_control_count_against_the_window() {
    assert!(Command::Run.credited() && Command::ReleaseKeys.credited());
    assert!(Command::Start(setup(Template::IbmPc.config())).credited());
    assert!(!Command::Fetched {
        request: 0,
        answer: Err(MediaError::Failed)
    }
    .credited());
    assert!(!Command::Stored {
        request: 0,
        answer: Ok(())
    }
    .credited());
    assert!(!Command::Released { slot: 0 }.credited());
    assert!(Command::Suspend.credited());
    assert!(!Command::Recalled {
        request: 0,
        bytes: &[0]
    }
    .credited());
    assert!(!Command::Fill { frames: 0 }.credited());
    assert!(!Command::Pressure(PressureBand::Normal).credited());
    assert!(Command::ReadText.credited() && Command::StopTyping.credited());
    assert!(
        !Command::Type(b"a").credited(),
        "the typing window bounds it"
    );
    assert!(
        !Command::Compiled {
            request: 0,
            code: None
        }
        .credited()
            && !Command::Interpret.credited(),
        "the block window bounds them"
    );
}

#[test]
fn the_share_holds_two_slots_of_each_fitted_adapters_largest_frame() {
    let layout = Layout::of(&[
        Some(Adapter::Mda(Phosphor::Amber)),
        Some(Adapter::Vga(Monitor::Colour)),
    ]);
    let slots: Vec<_> = layout.slots().collect();
    assert_eq!(slots.len(), 2 * SLOTS_PER_ADAPTER);
    let (mda, vga) = (
        Adapter::Mda(Phosphor::Amber).largest_frame(),
        Adapter::Vga(Monitor::Colour).largest_frame(),
    );
    let mda_bytes = mda.0 as usize * mda.1 as usize * PIXEL;
    assert_eq!(slots[1].1.offset, mda_bytes);
    assert_eq!((slots[2].1.adapter, slots[2].1.offset), (1, 2 * mda_bytes));
    assert_eq!(
        layout.len(),
        2 * mda_bytes + 2 * vga.0 as usize * vga.1 as usize * PIXEL
    );
    let vga_slot = layout.slot(3).unwrap();
    assert_eq!(vga_slot.frame_bytes(640, 400), Some(640 * 400 * PIXEL));
    assert_eq!(
        vga_slot.frame_bytes(vga.0 + 1, 1),
        None,
        "wider than the monitor shows"
    );
    assert_eq!(vga_slot.frame_bytes(0, 10), None);
    assert_eq!(layout.slot(4), None);
    let headless = Layout::of(&[None, Some(Adapter::Cga(Monitor::Composite))]);
    assert_eq!(
        (headless.slots().count(), headless.slot(0).unwrap().adapter),
        (2, 0),
        "counted among those fitted"
    );
    assert!(Layout::of(&[None, None]).is_empty());
}

#[test]
fn a_screen_reads_as_its_rows_trimmed_and_its_last_line_is_the_last_holding_anything() {
    let text = Text {
        columns: 8,
        rows: 4,
        codes: b"C:\\>VER TAIRiX  C:\\>            ",
    };
    let lines: Vec<&[u8]> = text.lines().collect();
    assert_eq!(lines, [&b"C:\\>VER"[..], b"TAIRiX", b"C:\\>", b""]);
    assert_eq!(text.last_line(), b"C:\\>");
    let blank = Text {
        columns: 4,
        rows: 2,
        codes: &[b' '; 8],
    };
    assert_eq!(blank.last_line(), b"");
}
