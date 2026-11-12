//! `MOUSE.COM` installed by `CONFIG.SYS` on a booted TAIRiX DOS, the serial
//! mouse on COM1 moved and its buttons pressed while the shell idles.

use super::*;
use crate::bios::run_until;
use crate::board::BoardConfig;
use crate::cpu::{gpr, Bus, Regs};
use crate::dos::harness::{screen, Dos, File, DATA, SECOND};
use crate::machine::Template;
use crate::ports::mouse::button;
use crate::ports::serial::Plug;

/// A system booted on `config` with `MOUSE.COM` installed.
fn installed_on(config: &BoardConfig) -> Dos {
    let driver = image().unwrap();
    let files = [
        File {
            name: b"CONFIG  SYS",
            bytes: b"INSTALL=A:\\MOUSE.COM\r\n",
        },
        File {
            name: b"MOUSE   COM",
            bytes: &driver,
        },
    ];
    Dos::shell_of(Dos::machine_with(config, &files))
}

fn installed() -> Dos {
    installed_on(&Template::IbmPc.config())
}

fn mouse(dos: &mut Dos, set: impl FnOnce(&mut Regs)) -> Regs {
    dos.int(0x33, set)
}

fn function(dos: &mut Dos, ax: u16) -> Regs {
    mouse(dos, |r| r.gpr[gpr::AX] = ax)
}

/// Moves the mouse by `dx`, `dy` with `buttons` held, and lets its packet
/// arrive.
fn move_by(dos: &mut Dos, dx: i32, dy: i32, buttons: u8) {
    assert!(
        dos.m.board_mut().mouse(dx, dy, buttons),
        "COM1 has the mouse"
    );
    let now = dos.m.now();
    run_until(&mut dos.m, now + SECOND / 10);
}

/// Motion counted as it comes, never doubled.
fn steady(dos: &mut Dos) {
    mouse(dos, |r| {
        r.gpr[gpr::AX] = 0x13;
        r.gpr[gpr::DX] = 0x7FFF;
    });
}

fn position(dos: &mut Dos) -> (u16, u16, u16) {
    let at = function(dos, 0x03);
    (at.gpr[gpr::BX], at.gpr[gpr::CX], at.gpr[gpr::DX])
}

#[test]
fn the_driver_installs_and_reports_itself() {
    let mut dos = installed();
    assert!(
        screen(&dos.m).contains("TAIRiX Mouse driver installed"),
        "{}",
        screen(&dos.m)
    );
    let reset = function(&mut dos, 0x00);
    assert_eq!((reset.gpr[gpr::AX], reset.gpr[gpr::BX]), (0xFFFF, 2));
    let version = function(&mut dos, 0x24);
    assert_eq!(version.gpr[gpr::BX], VERSION);
    assert_eq!(version.gpr[gpr::CX], 0x0204, "a serial mouse on IRQ 4");
    assert_eq!(
        position(&mut dos),
        (0, 320, 100),
        "the centre of the screen"
    );
}

#[test]
fn motion_moves_the_position_by_the_ratio_and_counts_its_mickeys() {
    let mut dos = installed();
    steady(&mut dos);
    move_by(&mut dos, 10, 10, 0);
    assert_eq!(
        position(&mut dos),
        (0, 330, 105),
        "eight mickeys a unit across, sixteen down"
    );
    let motion = function(&mut dos, 0x0B);
    assert_eq!((motion.gpr[gpr::CX], motion.gpr[gpr::DX]), (10, 10));
    let again = function(&mut dos, 0x0B);
    assert_eq!(
        (again.gpr[gpr::CX], again.gpr[gpr::DX]),
        (0, 0),
        "read, they start again"
    );
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x07;
        r.gpr[gpr::CX] = 400;
        r.gpr[gpr::DX] = 340;
    });
    assert_eq!(
        position(&mut dos).1,
        340,
        "kept to the range, its ends either way round"
    );
    move_by(&mut dos, -100, 0, 0);
    assert_eq!(position(&mut dos).1, 340);
}

#[test]
fn fast_motion_is_doubled_past_the_threshold() {
    let mut dos = installed();
    function(&mut dos, 0x00);
    move_by(&mut dos, 40, 0, 0);
    assert_eq!(
        position(&mut dos).1,
        400,
        "forty mickeys in a tick is past 64 a second"
    );
}

#[test]
fn presses_and_releases_are_counted_where_they_happened() {
    let mut dos = installed();
    steady(&mut dos);
    move_by(&mut dos, 8, 0, button::LEFT);
    let pressed = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x05;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(pressed.gpr[gpr::AX], 1, "the left button is down");
    assert_eq!(
        (
            pressed.gpr[gpr::BX],
            pressed.gpr[gpr::CX],
            pressed.gpr[gpr::DX]
        ),
        (1, 328, 100)
    );
    move_by(&mut dos, 0, 0, button::RIGHT);
    let released = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x06;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!((released.gpr[gpr::AX], released.gpr[gpr::BX]), (2, 1));
    let right = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x05;
        r.gpr[gpr::BX] = 1;
    });
    assert_eq!(right.gpr[gpr::BX], 1);
    let none = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x05;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(none.gpr[gpr::BX], 0, "counted since last asked");
}

/// The attribute of the text cell at `row`, `column`.
fn attribute(dos: &Dos, row: u32, column: u32) -> u8 {
    dos.m.board().peek(0xB_8000 + (row * 80 + column) * 2 + 1)
}

#[test]
fn the_text_cursor_swaps_a_cells_colours_and_gives_them_back() {
    let mut dos = installed();
    function(&mut dos, 0x00);
    let before = attribute(&dos, 12, 40);
    function(&mut dos, 0x01);
    assert_eq!(
        attribute(&dos, 12, 40),
        (before & 0x77) ^ 0x77,
        "shown at the centre's cell"
    );
    steady(&mut dos);
    move_by(&mut dos, 16, 0, 0);
    assert_eq!(attribute(&dos, 12, 40), before, "put back as it moved on");
    assert_eq!(attribute(&dos, 12, 42), (before & 0x77) ^ 0x77);
    function(&mut dos, 0x02);
    assert_eq!(attribute(&dos, 12, 42), before, "hidden");
    function(&mut dos, 0x02);
    function(&mut dos, 0x01);
    assert_eq!(
        attribute(&dos, 12, 42),
        before,
        "hidden twice, shown once, still hidden"
    );
}

/// A colour the BIOS reads back at `x`, `y` in graphics.
fn pixel(dos: &mut Dos, x: u16, y: u16) -> u8 {
    let read = dos.int(0x10, |r| {
        r.gpr[gpr::AX] = 0x0D00;
        r.gpr[gpr::BX] = 0;
        r.gpr[gpr::CX] = x;
        r.gpr[gpr::DX] = y;
    });
    read.gpr[gpr::AX].to_le_bytes()[0]
}

/// Sets video mode `mode`, resets the driver and shows its cursor.
fn graphics(dos: &mut Dos, mode: u8) {
    dos.int(0x10, |r| r.gpr[gpr::AX] = u16::from(mode));
    function(dos, 0x00);
    function(dos, 0x01);
}

#[test]
fn the_arrow_is_drawn_in_each_graphics_layout_and_taken_away() {
    // Each mode's centre in pixels, a pixel of the arrow's body from there
    // and the colour it reads back as. A four-colour mode takes the masks
    // two bits a pixel, so its arrow is half as wide.
    for (template, mode, (x, y), (dx, dy), white) in [
        (Template::IbmPc, 0x04, (160, 100), (2, 9), 3),
        (Template::IbmPc, 0x06, (320, 100), (1, 2), 1),
        (Template::Workstation186, 0x0D, (160, 100), (1, 2), 15),
        (Template::Workstation186, 0x10, (320, 175), (1, 2), 15),
        (Template::TurboXt, 0x12, (320, 240), (1, 2), 15),
        (Template::TurboXt, 0x13, (160, 100), (1, 2), 15),
    ] {
        let mut dos = installed_on(&template.config());
        graphics(&mut dos, mode);
        assert_eq!(
            pixel(&mut dos, x + dx, y + dy),
            white,
            "mode {mode:02X}h: the arrow's body"
        );
        assert_eq!(
            pixel(&mut dos, x + 12, y + 2),
            0,
            "mode {mode:02X}h: beside it, untouched"
        );
        function(&mut dos, 0x02);
        assert_eq!(
            pixel(&mut dos, x + dx, y + dy),
            0,
            "mode {mode:02X}h: put back"
        );
    }
}

#[test]
fn a_graphics_cursor_set_by_function_9_is_drawn_at_its_hot_spot() {
    let mut dos = installed_on(&Template::TurboXt.config());
    graphics(&mut dos, 0x13);
    let mut masks = [0u8; 64];
    for (row, pair) in masks[32..].chunks_mut(2).enumerate() {
        pair.copy_from_slice(&(if row == 0 { 0x8000u16 } else { 0 }).to_le_bytes());
    }
    masks[..32].fill(0xFF);
    dos.put(DATA, &masks);
    let psp = dos.psp;
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x09;
        r.gpr[gpr::BX] = 0;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = DATA;
        r.set_sreg(crate::cpu::Seg::Es, psp);
    });
    assert_eq!(
        pixel(&mut dos, 160, 100),
        0x0F,
        "the one pixel the cursor sets"
    );
    assert_eq!(pixel(&mut dos, 161, 101), 0, "and no other");
}

/// A far procedure at `DATA + 0x100` in the shell's segment that keeps AX,
/// BX, CX and DX at `DATA` and returns.
fn recording_handler(dos: &mut Dos) -> u16 {
    let [low, high] = DATA.to_le_bytes();
    let at = |n: u16| (DATA + n).to_le_bytes();
    let (bx, cx, dx) = (at(2), at(4), at(6));
    // mov cs:[DATA], ax; mov cs:[DATA+2], bx; mov cs:[DATA+4], cx;
    // mov cs:[DATA+6], dx; retf
    let code = [
        0x2E, 0xA3, low, high, 0x2E, 0x89, 0x1E, bx[0], bx[1], 0x2E, 0x89, 0x0E, cx[0], cx[1],
        0x2E, 0x89, 0x16, dx[0], dx[1], 0xCB,
    ];
    dos.put(DATA + 0x100, &code);
    dos.put(DATA, &[0; 8]);
    DATA + 0x100
}

fn recorded(dos: &Dos) -> [u16; 4] {
    let bytes = dos.get(DATA, 8);
    [0, 2, 4, 6].map(|n| u16::from_le_bytes([bytes[n], bytes[n + 1]]))
}

#[test]
fn an_event_handler_is_called_for_the_events_it_asks_for() {
    let mut dos = installed();
    steady(&mut dos);
    let handler = recording_handler(&mut dos);
    let psp = dos.psp;
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x0C;
        r.gpr[gpr::CX] = event::LEFT_DOWN;
        r.gpr[gpr::DX] = handler;
        r.set_sreg(crate::cpu::Seg::Es, psp);
    });
    move_by(&mut dos, 8, 0, 0);
    assert_eq!(recorded(&dos), [0; 4], "motion was not asked for");
    move_by(&mut dos, 0, 0, button::LEFT);
    assert_eq!(recorded(&dos), [event::LEFT_DOWN, 1, 328, 100]);
    let exchanged = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x14;
        r.gpr[gpr::CX] = 0;
        r.gpr[gpr::DX] = 0;
        r.set_sreg(crate::cpu::Seg::Es, 0);
    });
    assert_eq!(
        (
            exchanged.gpr[gpr::CX],
            exchanged.gpr[gpr::DX],
            exchanged.sreg(crate::cpu::Seg::Es)
        ),
        (event::LEFT_DOWN, handler, psp)
    );
}

#[test]
fn an_alternate_handler_is_set_found_and_cleared_by_its_shift_keys() {
    let mut dos = installed();
    let set = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x18;
        r.gpr[gpr::CX] = 0x20 | event::MOVED;
        r.gpr[gpr::DX] = 0x1234;
        r.set_sreg(crate::cpu::Seg::Es, 0x5678);
    });
    assert_eq!(set.gpr[gpr::AX], 0x18);
    let found = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x19;
        r.gpr[gpr::CX] = 0x20 | event::MOVED;
    });
    assert_eq!(
        (found.gpr[gpr::CX], found.gpr[gpr::BX], found.gpr[gpr::DX]),
        (0x21, 0x5678, 0x1234)
    );
    let refused = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x18;
        r.gpr[gpr::CX] = event::MOVED;
    });
    assert_eq!(
        refused.gpr[gpr::AX],
        0xFFFF,
        "an alternate needs a shift key"
    );
}

#[test]
fn the_state_is_saved_and_restored() {
    let mut dos = installed();
    let size = function(&mut dos, 0x15).gpr[gpr::BX];
    assert_eq!(size, state::LEN);
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x04;
        r.gpr[gpr::CX] = 100;
        r.gpr[gpr::DX] = 50;
    });
    let psp = dos.psp;
    let at = |r: &mut Regs, ax: u16| {
        r.gpr[gpr::AX] = ax;
        r.gpr[gpr::DX] = DATA;
        r.set_sreg(crate::cpu::Seg::Es, psp);
    };
    mouse(&mut dos, |r| at(r, 0x16));
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x04;
        r.gpr[gpr::CX] = 200;
        r.gpr[gpr::DX] = 80;
    });
    mouse(&mut dos, |r| at(r, 0x17));
    assert_eq!(position(&mut dos), (0, 100, 50));
}

#[test]
fn disabled_the_driver_lets_its_port_go_and_enabled_takes_it_back() {
    let mut dos = installed();
    let ours = (peek16(&dos, 0, 0x0C * 4), peek16(&dos, 0, 0x0C * 4 + 2));
    let off = function(&mut dos, 0x1F);
    assert_eq!(off.gpr[gpr::AX], 0x1F);
    assert_eq!(
        (off.gpr[gpr::BX], off.sreg(crate::cpu::Seg::Es)),
        (0, 0),
        "INT 33h's vector before"
    );
    assert_ne!(
        (peek16(&dos, 0, 0x0C * 4), peek16(&dos, 0, 0x0C * 4 + 2)),
        ours,
        "IRQ 4 given back"
    );
    steady(&mut dos);
    move_by(&mut dos, 16, 0, 0);
    assert_eq!(position(&mut dos).1, 320, "the mouse is not heard");
    assert_eq!(
        function(&mut dos, 0x1F).gpr[gpr::AX],
        0xFFFF,
        "already disabled"
    );
    function(&mut dos, 0x20);
    assert_eq!(
        (peek16(&dos, 0, 0x0C * 4), peek16(&dos, 0, 0x0C * 4 + 2)),
        ours
    );
    move_by(&mut dos, 16, 0, 0);
    assert_eq!(position(&mut dos).1, 336);
}

fn peek16(dos: &Dos, segment: u16, offset: u16) -> u16 {
    let at = crate::cpu::linear(segment, offset);
    u16::from_le_bytes([dos.m.board().peek(at), dos.m.board().peek(at + 1)])
}

#[test]
fn with_no_mouse_on_a_port_nothing_stays() {
    let mut config = Template::IbmPc.config();
    for port in config.ports.serial.iter_mut().flatten() {
        port.device = Plug::Nothing;
    }
    let dos = installed_on(&config);
    assert!(
        screen(&dos.m).contains("Mouse not found: driver not installed"),
        "{}",
        screen(&dos.m)
    );
    assert_eq!(
        (peek16(&dos, 0, 0x33 * 4), peek16(&dos, 0, 0x33 * 4 + 2)),
        (0, 0)
    );
}

#[test]
fn a_second_copy_finds_the_first_and_leaves() {
    let driver = image().unwrap();
    let files = [
        File {
            name: b"CONFIG  SYS",
            bytes: b"INSTALL=A:\\MOUSE.COM\r\nINSTALL=A:\\MOUSE.COM\r\n",
        },
        File {
            name: b"MOUSE   COM",
            bytes: &driver,
        },
    ];
    let dos = Dos::shell_of(Dos::machine(&files));
    let shown = screen(&dos.m);
    assert!(
        shown.contains("TAIRiX Mouse driver installed")
            && shown.contains("A mouse driver is already installed"),
        "{shown}"
    );
}

#[test]
fn the_installer_fits_before_the_handlers() {
    assert!(installer().unwrap().len() <= usize::from(at::INT33 - at::START));
    assert!(handler(entry::MOUSE).unwrap().len() <= usize::from(at::IRQ - at::INT33));
    assert!(handler(entry::MOUSE_IRQ).unwrap().len() <= usize::from(at::PORT - at::IRQ));
}

#[test]
fn entering_the_region_function_10h_sets_hides_the_cursor() {
    let mut dos = installed();
    steady(&mut dos);
    function(&mut dos, 0x00);
    function(&mut dos, 0x01);
    let before = attribute(&dos, 12, 39);
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x10;
        r.gpr[gpr::CX] = 300;
        r.gpr[gpr::DX] = 90;
        r.gpr[gpr::SI] = 330;
        r.gpr[gpr::DI] = 110;
    });
    assert_eq!(attribute(&dos, 12, 40), before, "hidden inside the region");
    function(&mut dos, 0x01);
    assert_ne!(
        attribute(&dos, 12, 40),
        before,
        "function 01h shows it again"
    );
}

#[test]
fn a_cell_written_under_the_text_cursor_keeps_what_was_written() {
    let mut dos = installed();
    function(&mut dos, 0x00);
    function(&mut dos, 0x01);
    dos.m
        .board_mut()
        .write(0xB_8000 + (12 * 80 + 40) * 2, b'Z', 0);
    dos.m
        .board_mut()
        .write(0xB_8000 + (12 * 80 + 40) * 2 + 1, 0x1E, 0);
    function(&mut dos, 0x02);
    assert_eq!(attribute(&dos, 12, 40), 0x1E);
    assert_eq!(dos.m.board().peek(0xB_8000 + (12 * 80 + 40) * 2), b'Z');
}

#[test]
fn a_mode_set_under_the_cursor_is_not_drawn_over() {
    let mut dos = installed_on(&Template::TurboXt.config());
    graphics(&mut dos, 0x13);
    dos.int(0x10, |r| r.gpr[gpr::AX] = 0x0012);
    function(&mut dos, 0x02);
    for (x, y) in [(160, 100), (161, 102), (162, 105)] {
        assert_eq!(
            pixel(&mut dos, x, y),
            0,
            "nothing of mode 13h's put back over mode 12h"
        );
    }
}

/// Reads graphics controller register `index` on the VGA.
fn gc(dos: &mut Dos, index: u8) -> u8 {
    let at = dos.m.board().cycles_at(dos.m.now());
    dos.m.board_mut().output(0x3CE, index, at);
    dos.m.board_mut().input(0x3CF, at)
}

#[test]
fn drawing_on_the_vga_leaves_its_registers_as_the_program_set_them() {
    let mut dos = installed_on(&Template::TurboXt.config());
    graphics(&mut dos, 0x12);
    function(&mut dos, 0x02);
    let at = dos.m.board().cycles_at(dos.m.now());
    for (index, value) in [(0u8, 0x05u8), (1, 0x0F), (3, 0x18), (5, 0x02), (8, 0x3C)] {
        dos.m.board_mut().output(0x3CE, index, at);
        dos.m.board_mut().output(0x3CF, value, at);
    }
    dos.m.board_mut().output(0x3C4, 2, at);
    dos.m.board_mut().output(0x3C5, 0x06, at);
    dos.m.board_mut().output(0x3CE, 5, at);
    function(&mut dos, 0x01);
    function(&mut dos, 0x02);
    let at = dos.m.board().cycles_at(dos.m.now());
    let index = dos.m.board_mut().input(0x3CE, at);
    assert_eq!(index, 5, "the index it had");
    for (index, value) in [(0u8, 0x05u8), (1, 0x0F), (3, 0x18), (5, 0x02), (8, 0x3C)] {
        assert_eq!(
            gc(&mut dos, index),
            value,
            "graphics controller register {index}"
        );
    }
    let at = dos.m.board().cycles_at(dos.m.now());
    dos.m.board_mut().output(0x3C4, 2, at);
    assert_eq!(dos.m.board_mut().input(0x3C5, at), 0x06, "the map mask");
}

#[test]
fn the_hardware_text_cursor_takes_the_crtcs_cursor_and_gives_it_back() {
    let mut dos = installed();
    function(&mut dos, 0x00);
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x0A;
        r.gpr[gpr::BX] = 1;
        r.gpr[gpr::CX] = 2;
        r.gpr[gpr::DX] = 5;
    });
    function(&mut dos, 0x01);
    let crtc = |dos: &mut Dos, register: u8| {
        let at = dos.m.board().cycles_at(dos.m.now());
        dos.m.board_mut().output(0x3D4, register, at);
        dos.m.board_mut().input(0x3D5, at)
    };
    let place = u16::from_le_bytes([crtc(&mut dos, 0x0F), crtc(&mut dos, 0x0E)]);
    assert_eq!(place, 12 * 80 + 40, "on the centre's cell");
    function(&mut dos, 0x02);
    let place = u16::from_le_bytes([crtc(&mut dos, 0x0F), crtc(&mut dos, 0x0E)]);
    let [column, row] = peek16(&dos, 0x40, 0x50).to_le_bytes();
    assert_eq!(
        place,
        u16::from(row) * 80 + u16::from(column),
        "back where the BIOS has it"
    );
}

/// The host's pointer put at fractions `x`, `y` of the picture, and its
/// packet let arrive.
fn point_at(dos: &mut Dos, x: u16, y: u16, buttons: u8) {
    assert!(
        dos.m
            .board_mut()
            .point(crate::ports::mouse::HostPointer { x, y }, buttons),
        "COM1 has the mouse"
    );
    let now = dos.m.now();
    run_until(&mut dos.m, now + SECOND / 10);
}

#[test]
fn the_driver_takes_its_position_from_the_host_pointer() {
    let mut dos = installed();
    point_at(&mut dos, 0, 0, 0);
    assert_eq!(position(&mut dos), (0, 0, 0), "the top left");
    point_at(&mut dos, u16::MAX, u16::MAX, 0);
    assert_eq!(position(&mut dos), (0, 639, 199), "the bottom right");
    point_at(&mut dos, 0x8000, 0x4000, button::LEFT);
    assert_eq!(
        position(&mut dos),
        (1, 320, 50),
        "a quarter of the way down"
    );
    let pressed = mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x05;
        r.gpr[gpr::BX] = 0;
    });
    assert_eq!(
        (
            pressed.gpr[gpr::BX],
            pressed.gpr[gpr::CX],
            pressed.gpr[gpr::DX]
        ),
        (1, 320, 50),
        "the press counted where the host's pointer put it"
    );
    point_at(&mut dos, 0x8001, 0x4000, button::LEFT);
    assert_eq!(
        position(&mut dos),
        (1, 320, 50),
        "a move inside one virtual dot is still a packet, and lands"
    );
    mouse(&mut dos, |r| {
        r.gpr[gpr::AX] = 0x07;
        r.gpr[gpr::CX] = 100;
        r.gpr[gpr::DX] = 200;
    });
    point_at(&mut dos, 0, 0, 0);
    assert_eq!(position(&mut dos).1, 100, "kept to the range");
}

#[test]
fn moving_the_mouse_lets_go_of_the_host_pointer() {
    let mut dos = installed();
    steady(&mut dos);
    point_at(&mut dos, 0x8000, 0x8000, 0);
    move_by(&mut dos, 8, 0, 0);
    assert_eq!(dos.m.board().host_pointer(), None);
    assert_eq!(position(&mut dos), (0, 328, 100), "the mouse's own motion");
}

#[test]
fn the_follower_knows_the_driver_only_while_it_holds_both_vectors() {
    let follower = Follower::new().unwrap();
    let mut dos = installed();
    assert!(follower.follows(dos.m.board()));
    function(&mut dos, 0x1F);
    assert!(!follower.follows(dos.m.board()), "disabled");
    function(&mut dos, 0x20);
    assert!(follower.follows(dos.m.board()), "enabled again");
    let irq = dos.m.board().peek(0x30);
    dos.m.board_mut().write(0x30, irq ^ 1, 0);
    assert!(
        !follower.follows(dos.m.board()),
        "a program has the port's interrupt"
    );
    dos.m.board_mut().write(0x30, irq, 0);
    let int33 = dos.m.board().peek(0xCC);
    dos.m.board_mut().write(0xCC, int33.wrapping_add(1), 0);
    assert!(!follower.follows(dos.m.board()), "and INT 33h");
    let bare = Dos::shell_of(Dos::machine_with(&Template::IbmPc.config(), &[]));
    assert!(!follower.follows(bare.m.board()), "no driver at all");
}
