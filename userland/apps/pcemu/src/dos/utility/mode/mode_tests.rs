//! `MODE`: its command line, then each request run at the shell's prompt on
//! the CGA, EGA and VGA machines.

use super::parse::{baud_of, request, Adjust, Device, Display, Framing, Problem, Request};
use super::{at, retry, text};
use crate::asm::Reg16;
use crate::cpu::Bus;
use crate::dos::harness::{self, screen, settle, type_ahead};
use crate::dos::utility::tests::{run, with_utilities, with_utilities_on};
use crate::machine::{Machine, Template};
use alloc::vec::Vec;

fn serial(baud: u32, parity: u8, data: u8, stop: u8) -> Framing {
    Framing {
        baud,
        parity,
        data,
        stop,
    }
}

#[test]
fn mode_reads_statuses_and_displays() {
    assert_eq!(request(b""), Ok(Request::Status(None)));
    assert_eq!(request(b" /sta"), Ok(Request::Status(None)));
    assert_eq!(
        request(b" lpt1 /status"),
        Ok(Request::Status(Some(Device::Printer(0))))
    );
    assert_eq!(request(b" con"), Ok(Request::Status(Some(Device::Console))));
    assert_eq!(
        request(b" com2:"),
        Ok(Request::Status(Some(Device::Serial(1))))
    );
    assert_eq!(
        request(b" co80"),
        Ok(Request::Display {
            display: Some(Display::Co80),
            adjust: None
        })
    );
    let shift = Some(Adjust::Shift {
        right: true,
        test: true,
    });
    assert_eq!(
        request(b" ,r,t"),
        Ok(Request::Display {
            display: None,
            adjust: shift
        })
    );
    assert_eq!(
        request(b" mono,l"),
        Ok(Request::Display {
            display: Some(Display::Mono),
            adjust: Some(Adjust::Shift {
                right: false,
                test: false
            })
        })
    );
    assert_eq!(
        request(b" 80,43"),
        Ok(Request::Display {
            display: Some(Display::Eighty),
            adjust: Some(Adjust::Rows(43))
        })
    );
    assert_eq!(request(b" co80,x"), Err(Problem::Invalid(b"x".to_vec())));
    assert_eq!(request(b" 80,44"), Err(Problem::Invalid(b"44".to_vec())));
    assert_eq!(request(b" ,r,t,z"), Err(Problem::Said(text::FIELDS)));
    assert_eq!(request(b" ,"), Err(Problem::Said(text::FIELDS)));
}

#[test]
fn mode_reads_the_console_and_printers() {
    let console = |columns, lines, typematic| {
        Ok(Request::Console {
            columns,
            lines,
            typematic,
        })
    };
    assert_eq!(
        request(b" con cols=40 lines=25"),
        console(Some(40), Some(25), None)
    );
    assert_eq!(
        request(b" con: rate = 20 del=2"),
        console(None, None, Some((20, 2)))
    );
    assert_eq!(
        request(b" con rate=32"),
        Err(Problem::Said(text::RATE_DELAY))
    );
    assert_eq!(
        request(b" con lines=24"),
        Err(Problem::Invalid(b"lines=24".to_vec()))
    );
    assert_eq!(request(b" con cp select=437"), Ok(Request::CodePage));
    let printer = |lpt, columns, lines, retry| {
        Ok(Request::Printer {
            lpt,
            columns,
            lines,
            retry,
        })
    };
    assert_eq!(
        request(b" lpt1:132,8,p"),
        printer(0, Some(132), Some(8), Some(retry::ALWAYS))
    );
    assert_eq!(
        request(b" lpt1:80"),
        printer(0, Some(80), None, Some(retry::NONE)),
        "no retry by position turns it off"
    );
    assert_eq!(
        request(b" lpt2 cols=80"),
        printer(1, Some(80), None, None),
        "nor by keyword keeps it"
    );
    assert_eq!(
        request(b" lpt3 retry=b lines=6"),
        printer(2, None, Some(6), Some(retry::BUSY))
    );
    assert_eq!(
        request(b" lpt180"),
        printer(0, Some(80), None, Some(retry::NONE)),
        "DOS's unparted spelling"
    );
    assert_eq!(request(b" lpt1:90"), Err(Problem::Invalid(b"90".to_vec())));
    assert_eq!(
        request(b" lpt1=com2"),
        Ok(Request::Reroute {
            lpt: 0,
            com: Some(1)
        })
    );
    assert_eq!(
        request(b" lpt1:=com2:"),
        Ok(Request::Reroute {
            lpt: 0,
            com: Some(1)
        })
    );
    assert_eq!(request(b" prn"), Ok(Request::Reroute { lpt: 0, com: None }));
    assert_eq!(request(b" lpt1=lpt2"), Err(Problem::Said(text::MUST_COM)));
    assert_eq!(request(b" lpt4"), Err(Problem::Invalid(b"lpt4".to_vec())));
    assert_eq!(request(b" aux"), Err(Problem::Invalid(b"aux".to_vec())));
}

#[test]
fn mode_reads_serial_ports() {
    let port = |com, framing, retry| {
        Ok(Request::Serial {
            com,
            framing,
            retry,
        })
    };
    assert_eq!(
        request(b" com1:96,n,8,1"),
        port(0, serial(9600, b'n', 8, 10), Some(retry::NONE))
    );
    assert_eq!(
        request(b" com2 baud=12 parity=odd"),
        port(1, serial(1200, b'o', 7, 10), None)
    );
    assert_eq!(
        request(b" com1:11"),
        port(0, serial(110, b'e', 7, 20), Some(retry::NONE)),
        "110 baud stops with two bits"
    );
    assert_eq!(
        request(b" com1:300,,,2,e"),
        port(0, serial(300, b'e', 7, 20), Some(retry::ERROR))
    );
    assert_eq!(
        request(b" com1:19200"),
        Err(Problem::Unsupported(b"19200".to_vec()))
    );
    assert_eq!(
        request(b" com1 baud=19.2k"),
        Err(Problem::Unsupported(b"baud=19.2k".to_vec())),
        "the word as typed"
    );
    assert_eq!(
        request(b" com1:96,m"),
        Err(Problem::Unsupported(b"m".to_vec()))
    );
    assert_eq!(
        request(b" com1:96,n,5"),
        Err(Problem::Unsupported(b"5".to_vec()))
    );
    assert_eq!(
        request(b" com1:96,n,8,1.5"),
        Err(Problem::Unsupported(b"1.5".to_vec()))
    );
    assert_eq!(request(b" com1:,n"), Err(Problem::Said(text::NEED_BAUD)));
    assert_eq!(request(b" com1:97"), Err(Problem::Said(text::BAD_BAUD)));
    assert_eq!(
        request(b" com1:96,n,8,1,b,x"),
        Err(Problem::Said(text::FIELDS))
    );
    assert_eq!(request(b" com1:96,x"), Err(Problem::Invalid(b"x".to_vec())));
    for (typed, baud) in [
        (&b"96"[..], 9600),
        (b"19", 19200),
        (b"11", 110),
        (b"15", 150),
        (b"300", 300),
        (b"19.2", 19200),
    ] {
        assert_eq!(baud_of(typed), Some(baud), "{typed:?}");
    }
    assert_eq!(baud_of(b"97"), None);
    assert_eq!(baud_of(b"9601"), None);
}

#[test]
fn mode_says_its_problems_as_dos_does() {
    assert_eq!(
        Problem::Invalid(b"lpt4".to_vec()).text(),
        b"\r\nInvalid parameter - LPT4\r\n"
    );
    assert_eq!(
        Problem::Unsupported(b"19200".to_vec()).text(),
        b"\r\nFunction not supported on this computer - 19200\r\n"
    );
    assert_eq!(
        Problem::Said(text::NEED_BAUD).text(),
        b"\r\nBaud rate required\r\n"
    );
}

fn peek16(m: &Machine, at: u32) -> u16 {
    u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)])
}

/// Interrupt `vector`'s handler: its segment and offset.
fn vector(m: &Machine, vector: u32) -> (u16, u16) {
    (peek16(m, vector * 4 + 2), peek16(m, vector * 4))
}

/// Whether `segment` is a program's own block: its MCB names it the owner.
fn owns_itself(m: &Machine, segment: u16) -> bool {
    peek16(m, (u32::from(segment) - 1) * 16 + 1) == segment
}

/// A program that takes INT `vector` with AX `ax` and DX `dx`, keeps the AX
/// it returns at `0000:04F0` and ends.
fn probe(vector: u8, ax: u16, dx: u16) -> Vec<u8> {
    harness::com(|asm| {
        asm.mov_imm16(Reg16::Ax, ax);
        asm.mov_imm16(Reg16::Dx, dx);
        asm.int(vector);
        // PUSH AX; XOR AX,AX; MOV ES,AX; POP AX; MOV ES:[04F0h],AX
        asm.bytes(&[0x50, 0x31, 0xC0, 0x8E, 0xC0, 0x58, 0x26, 0xA3, 0xF0, 0x04]);
        asm.mov_imm16(Reg16::Ax, 0x4C00);
        asm.int(0x21);
    })
}

/// What `probe` kept, after the line running it.
fn probed(m: &mut Machine, line: &str) -> u16 {
    run(m, line);
    peek16(m, 0x4F0)
}

const ERRORLEVEL: &[u8] = b"@echo off\r\nmode %1 %2 %3 >nul\r\nif errorlevel 1 echo failed\r\nif not errorlevel 1 echo fine\r\n";

#[test]
fn mode_shows_every_device_s_status() {
    let mut m = with_utilities(&[("CODE.BAT", ERRORLEVEL)]);
    let text = run(&mut m, "mode lpt1 /status");
    let lpt1 = "\nStatus for device LPT1:\n-----------------------\nLPT1: not rerouted\nRETRY=NONE\nCode page operation not supported on this device\n";
    assert!(text.contains(lpt1), "{text}");
    let text = run(&mut m, "mode");
    assert!(text.contains("\nStatus for device LPT2:\n-----------------------\nLPT2: not rerouted\n\nStatus for device LPT3:\n"), "a printer the BIOS did not find shows only its rerouting:\n{text}");
    assert!(text.contains("\nStatus for device CON:\n----------------------\nCode page operation not supported on this device\n"), "{text}");
    assert!(
        text.contains("\nStatus for device COM1:\n-----------------------\nRETRY=NONE\n"),
        "{text}"
    );
    assert!(
        !text.contains("COM2"),
        "only the ports the BIOS found:\n{text}"
    );
    assert!(
        run(&mut m, "mode com2 /status").contains("Illegal device name"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "code").contains("failed"),
        "the code pages it cannot show fail it, as DOS's do"
    );
    assert!(
        run(&mut m, "code com1 /status").contains("fine"),
        "{}",
        screen(&m)
    );
}

#[test]
fn mode_sets_a_serial_port_and_keeps_its_retry_resident() {
    let mut m = with_utilities(&[("STATUS.COM", &probe(0x14, 0x0300, 0))]);
    let text = run(&mut m, "mode com1:96,n,8,1,e");
    assert!(
        text.contains("\nResident portion of MODE loaded\n\nCOM1: 9600,n,8,1,e\n"),
        "{text}"
    );
    let (segment, offset) = vector(&m, 0x14);
    assert_eq!(
        offset,
        crate::dos::program::handler_entry(super::handler::SERIAL)
    );
    assert_eq!(
        vector(&m, 0x17).0,
        segment,
        "both vectors lead to the one resident part"
    );
    assert!(owns_itself(&m, segment));
    assert_eq!(
        (peek16(&m, 0x532), peek16(&m, 0x530)),
        (segment, at::OLD_17),
        "its tables' address"
    );
    assert_eq!(
        probed(&mut m, "status"),
        0x8E00,
        "a port in error, as retry E reports it"
    );
    assert!(
        run(&mut m, "mode com1 /status").contains("RETRY=E"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "mode com1:96,n,8,1,r");
    assert!(
        !text.contains("Resident portion"),
        "the resident part is found again:\n{text}"
    );
    assert!(text.contains("COM1: 9600,n,8,1,r"), "{text}");
    assert_eq!(probed(&mut m, "status"), 0x6030, "a ready port");
    assert!(
        run(&mut m, "mode com1 baud=12").contains("COM1: 1200,e,7,1,r"),
        "a keyword form keeps the retry:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode com1:24").contains("COM1: 2400,e,7,1,-"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode com1 /status").contains("RETRY=NONE"),
        "{}",
        screen(&m)
    );
    let actual = probed(&mut m, "status");
    assert_eq!(
        actual & 0x6000,
        0x6000,
        "the port's own status, its transmitter idle: {actual:04X}"
    );
    assert_eq!(
        vector(&m, 0x14).0,
        segment,
        "one resident part, however often MODE runs"
    );
    assert!(run(&mut m, "mem /c").contains("MODE"), "{}", screen(&m));
}

#[test]
fn a_resident_mode_chains_to_the_bios_whatever_its_memory_held() {
    // The program host never cleared a program's abandon flag as it began,
    // so a copy loaded over a stale one ran its end again as it stayed
    // resident: its vectors were hooked over its own and its handlers
    // chained to themselves.
    let mut m = with_utilities(&[("RECV.COM", &probe(0x14, 0x0200, 0))]);
    for block in harness::blocks_of(&m, 0) {
        let (mcb, abandon) = (u32::from(block) * 16, crate::dos::program::at::ABANDON);
        if peek16(&m, mcb + 3) > abandon / 16 {
            m.board_mut().write(mcb + 16 + u32::from(abandon), 0xFF, 0);
        }
    }
    let text = run(&mut m, "mode lpt2=com1");
    assert!(text.contains("LPT2: rerouted to COM1:"), "{text}");
    let base = u32::from(vector(&m, 0x14).0) * 16;
    for old in [at::OLD_14, at::OLD_17] {
        assert_eq!(
            peek16(&m, base + u32::from(old) + 2),
            0xF000,
            "the BIOS's handler is the one chained to"
        );
    }
    let received = probed(&mut m, "recv");
    assert_ne!(
        received & 0x8000,
        0,
        "a receive with nothing sent times out through it: {received:04X}"
    );
}

#[test]
fn mode_sets_a_printer_and_its_retry() {
    let mut m = with_utilities(&[]);
    let text = run(&mut m, "mode lpt1:132,8");
    assert!(text.contains("\nLPT1: not rerouted\n\nLPT1: set for 132\n\nPrinter lines per inch set\n\nNo retry on parallel printer time-out\n"), "{text}");
    let mut printed = [0u8; 8];
    let len = m.board_mut().take_printed(0, &mut printed);
    assert_eq!(
        &printed[..len],
        [0x0F, 0x1B, b'0'],
        "condensed, then eight lines an inch"
    );
    assert_eq!(
        vector(&m, 0x17).0,
        0xF000,
        "no retry needs no resident part"
    );
    let text = run(&mut m, "mode lpt1:,,p");
    assert!(
        text.contains(
            "\nResident portion of MODE loaded\n\nInfinite retry on parallel printer time-out\n"
        ),
        "{text}"
    );
    assert!(
        run(&mut m, "mode lpt1 /status").contains("RETRY=B"),
        "continual retry passes the printer's status on:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode lpt1 retry=e").contains("Infinite retry"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode lpt1 /status").contains("RETRY=E"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "mode lpt1 cols=80");
    assert!(
        text.contains("LPT1: set for 80") && text.contains("Infinite retry"),
        "a keyword form keeps the retry:\n{text}"
    );
    assert!(
        run(&mut m, "mode lpt1:80").contains("No retry"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode lpt1 /status").contains("RETRY=NONE"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode lpt2:80").contains("Illegal device name"),
        "{}",
        screen(&m)
    );
}

#[test]
fn mode_reroutes_a_printer_to_a_serial_port() {
    let files: [(&str, &[u8]); 3] = [
        ("INIT.COM", &probe(0x17, 0x0100, 1)),
        ("LOOK.COM", &probe(0x17, 0x0200, 1)),
        ("LINES.COM", &probe(0x14, 0x0300, 0)),
    ];
    let mut m = with_utilities(&files);
    assert_eq!(peek16(&m, 0x40A), 0, "no second printer");
    let text = run(&mut m, "mode lpt2=com1");
    assert!(
        text.contains("\nResident portion of MODE loaded\n\nLPT2: rerouted to COM1:\n"),
        "{text}"
    );
    assert_eq!(peek16(&m, 0x40A), 1, "a port word BASIC opens it by");
    assert_eq!(
        probed(&mut m, "init"),
        0x8000,
        "initialising it is done at once, AL kept"
    );
    let lines = probed(&mut m, "lines") & 0x30;
    let shown = match lines {
        0 => 0x29,
        0x30 => 0x90,
        _ => 0x10,
    };
    assert_eq!(
        probed(&mut m, "look") >> 8,
        shown,
        "the port's lines as a printer's status"
    );
    assert!(
        run(&mut m, "mode lpt2 /status").contains("LPT2: rerouted to COM1:"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "mode lpt2");
    assert!(
        text.contains("LPT2: not rerouted") && !text.contains("retry"),
        "{text}"
    );
    assert_eq!(peek16(&m, 0x40A), 0, "the word MODE gave it taken away");
    assert!(
        run(&mut m, "mode lpt1=lpt2").contains("Must specify COM1, COM2, COM3 or COM4"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode lpt1=com2").contains("Illegal device name"),
        "{}",
        screen(&m)
    );
}

#[test]
fn mode_sets_the_display_modes_the_adapter_shows() {
    let mut m = with_utilities(&[("CODE.BAT", ERRORLEVEL)]);
    run(&mut m, "mode bw80");
    assert_eq!(m.board().peek(0x449), 2, "{}", screen(&m));
    assert_eq!(
        m.board().peek(0x410) & 0x30,
        0x20,
        "the equipment word's display"
    );
    run(&mut m, "mode 40");
    assert_eq!(m.board().peek(0x449), 0, "the grey mode at 40 columns");
    run(&mut m, "mode co80");
    assert_eq!(m.board().peek(0x449), 3);
    assert!(
        run(&mut m, "mode mono").contains("Function not supported on this computer - MONO"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "code mono").contains("failed"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode con lines=43")
            .contains("ANSI.SYS must be installed to perform requested function"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode 80,43").contains("ANSI.SYS must be installed"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "mode con rate=32 delay=1");
    assert!(
        !text.contains("Function") && !text.contains("Invalid"),
        "a BIOS without the function ignores it:\n{text}"
    );
}

/// The 80-column horizontal sync position in the table INT 1Dh leads to.
fn sync_80(m: &Machine) -> u8 {
    let (segment, offset) = vector(m, 0x1D);
    m.board()
        .peek(u32::from(segment) * 16 + u32::from(offset) + 0x12)
}

#[test]
fn mode_shifts_the_picture_through_its_own_copy_of_the_table() {
    let mut m = with_utilities(&[]);
    let rom = sync_80(&m);
    run(&mut m, "mode co80,r > shift.txt");
    assert!(
        run(&mut m, "type shift.txt").contains("Resident portion of MODE loaded"),
        "the mode set clears what it said from the screen:\n{}",
        screen(&m)
    );
    let (segment, offset) = vector(&m, 0x1D);
    assert_eq!(offset, at::VIDEO_TABLE, "{segment:04X}:{offset:04X}");
    assert!(
        owns_itself(&m, segment),
        "the table stays with MODE's resident part"
    );
    assert_eq!(sync_80(&m), rom - 2);
    assert!(
        !run(&mut m, "mode ,r").contains("Resident portion"),
        "{}",
        screen(&m)
    );
    assert_eq!(sync_80(&m), rom - 4, "the same table, moved again");
    assert_eq!(vector(&m, 0x1D).0, segment);
    run(&mut m, "mode ,l");
    assert_eq!(sync_80(&m), rom - 2);
    let text = run(&mut m, "mode co80,l,t");
    assert!(
        text.contains("01234567890123456789")
            && text.contains("Do you see the rightmost 9? (YES/NO)"),
        "{text}"
    );
    type_ahead(&mut m, "n");
    settle(&mut m);
    assert!(
        screen(&m).contains("Do you see the rightmost 9?"),
        "asked again after another shift:\n{}",
        screen(&m)
    );
    type_ahead(&mut m, "y");
    settle(&mut m);
    assert_eq!(sync_80(&m), rom + 2, "two shifts left");
    assert!(screen(&m).trim_end().ends_with("A:\\>"), "{}", screen(&m));
}

#[test]
fn ctrl_c_at_the_shift_s_question_leaves_mode_resident() {
    // A copy that leads INT 1Dh into its own memory must never end without
    // staying: the next mode set would read whatever was loaded there.
    let mut m = with_utilities(&[]);
    run(&mut m, "mode co80,r,t");
    assert!(
        screen(&m).contains("Do you see the leftmost 0?"),
        "{}",
        screen(&m)
    );
    harness::control(&mut m, harness::C);
    settle(&mut m);
    let (segment, _) = vector(&m, 0x1D);
    assert!(owns_itself(&m, segment), "{}", screen(&m));
    assert_eq!(vector(&m, 0x17).0, segment, "hooked as it stayed");
    assert!(run(&mut m, "mem /c").contains("MODE"), "{}", screen(&m));
}

#[test]
fn an_input_with_no_answer_left_takes_the_shift_as_it_stands() {
    let mut m = with_utilities(&[]);
    let rom = sync_80(&m);
    let text = run(&mut m, "mode co80,r,t <nul");
    assert_eq!(
        text.matches("Do you see the leftmost 0?").count(),
        1,
        "asked once:\n{text}"
    );
    assert_eq!(sync_80(&m), rom - 2, "the one shift kept");
    let (segment, _) = vector(&m, 0x1D);
    assert!(owns_itself(&m, segment), "{}", screen(&m));
}

/// A machine whose CONFIG.SYS loads ANSI.SYS, with the utilities.
fn with_ansi(template: Template) -> Machine {
    let driver = crate::dos::ansi_sys();
    let config: &[u8] = b"DEVICE=ANSI.SYS\r\n";
    let files: [(&str, &[u8]); 2] = [("CONFIG.SYS", config), ("ANSI.SYS", &driver)];
    let volume = if template == Template::IbmPc {
        harness::diskette()
    } else {
        crate::dos::formats::Diskette::K1440.volume(0x1993_0601)
    };
    with_utilities_on(&template.config(), &volume, &files)
}

#[test]
fn mode_sets_the_vga_s_lines_through_ansi_sys() {
    let mut m = with_ansi(Template::TurboXt);
    assert!(
        run(&mut m, "mode con").contains("\nCOLUMNS=80\nLINES=25\nCode page operation"),
        "{}",
        screen(&m)
    );
    run(&mut m, "mode con lines=50");
    assert_eq!(m.board().peek(0x484), 49, "{}", screen(&m));
    assert!(
        run(&mut m, "mode con").contains("LINES=50"),
        "{}",
        screen(&m)
    );
    run(&mut m, "mode con lines=43");
    assert_eq!(m.board().peek(0x484), 42);
    run(&mut m, "mode 80,25");
    assert_eq!(m.board().peek(0x484), 24);
    run(&mut m, "mode con cols=40");
    assert_eq!(m.board().peek(0x44A), 40);
    run(&mut m, "mode 80");
    assert_eq!(m.board().peek(0x44A), 80);
}

#[test]
fn ansi_sys_refuses_lines_the_adapter_cannot_show() {
    let mut m = with_ansi(Template::IbmPc);
    assert!(
        run(&mut m, "mode con").contains("COLUMNS=80\nLINES=25"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode con lines=43")
            .contains("Function not supported on this computer - LINES=43"),
        "{}",
        screen(&m)
    );
    let mut m = with_ansi(Template::Workstation186);
    run(&mut m, "mode con lines=43");
    assert_eq!(
        m.board().peek(0x484),
        42,
        "the enhanced colour display's 43 lines:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode con lines=50").contains("Function not supported"),
        "{}",
        screen(&m)
    );
    run(&mut m, "mode con lines=25");
    assert_eq!(m.board().peek(0x484), 24);
}

#[test]
fn mode_says_what_it_does_not_take() {
    let mut m = with_utilities(&[]);
    assert!(
        run(&mut m, "mode lpt4").contains("Invalid parameter - LPT4"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode com1:19200").contains("Function not supported on this computer - 19200"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode com1:97").contains("Invalid baud rate specified"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode com1:,n").contains("Baud rate required"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode con cp select=437")
            .contains("Code page operation not supported on this device"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "mode con rate=1").contains("RATE and DELAY must be specified together"),
        "{}",
        screen(&m)
    );
}
