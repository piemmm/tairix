//! The MOS against the behaviour of MOS 1.20 it reproduces, as seen from
//! outside: a language ROM made with the builder drives it through its
//! entry points, and each test reads the machine's memory back.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use super::model_b;
use super::ws::{buffer, page2, page_d, var, vdu, vector};
use crate::cpu::STACK_PAGE;
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::Machine;
use crate::media::tape::Tape;
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm};
use crate::rom::os::entry::{
    GSINIT, GSREAD, OSARGS, OSBPUT, OSBYTE, OSCLI, OSFIND, OSRDCH, OSWORD, OSWRCH,
};
use crate::rom::os::{vector::BRK as BRKV, ERROR_POINTER};
use crate::scenario::{mode7_text, wait};

mod commands;
mod electron;
mod keyboard;
mod system;

/// Where the language marks that it has run to its end, keeps the command
/// it is on, and records each command's error number, 0 for none.
const DONE: u16 = 0x0070;
const DONE_MARK: u8 = 0x5A;
const INDEX: u16 = 0x0071;
const ERRORS: u16 = 0x0080;
/// The flags a test reads back.
const CARRY: u8 = 0x01;
const INTERRUPTS_OFF: u8 = 0x04;
const OVERFLOW: u8 = 0x40;
const NEGATIVE: u8 = 0x80;
/// How long the machine may take to boot and run a language, in cycles.
const BUDGET: u64 = 20_000_000;
const BAD_COMMAND: u8 = 0xFE;
const BAD_KEY: u8 = 0xFB;

/// A ROM's header: a language entering at `entry`, and a service entry at
/// `service` where it has one.
fn header(a: &mut Asm, service: Option<&'static str>) {
    a.jmp("entry");
    a.jmp(service.unwrap_or("entry"));
    a.byte(if service.is_some() { 0xC0 } else { 0x40 });
    a.byte(lo("copyright"));
    a.byte(1);
    a.text("TEST");
    a.here("copyright");
    a.byte(0);
    a.text("(C)");
    a.byte(0);
}

/// A language ROM that runs what `body` assembles and marks `DONE`, with a
/// service entry at `service` where `body` assembles one.
fn program(service: Option<&'static str>, body: impl FnOnce(&mut Asm)) -> Vec<u8> {
    let mut a = Asm::new(0x8000, 0x4000);
    header(&mut a, service);
    a.here("entry");
    body(&mut a);
    a.lda(imm(DONE_MARK));
    a.sta(abs(DONE));
    a.here("halt");
    a.jmp("halt");
    a.finish().expect("the test program assembles").bytes
}

/// A machine with the MOS and `rom` in slot 15, not yet run.
fn boot_rom(rom: &[u8]) -> Machine {
    let os = model_b().expect("the MOS assembles");
    let mut machine = Machine::new(&os.bytes).expect("a 16 KiB OS");
    machine.insert_rom(15, rom).expect("slot 15 exists");
    machine
}

/// Run `rom`, a [`program`], on the MOS to its end.
fn run_rom(rom: &[u8]) -> Machine {
    let mut machine = boot_rom(rom);
    finish(&mut machine, BUDGET);
    machine
}

/// Pass `command` to OSCLI from inline text, the program carrying on after.
fn oscli(a: &mut Asm, command: &'static str) {
    let (text, after) = (a.local(), a.local());
    a.ldx(imm(lo(text)));
    a.ldy(imm(hi(text)));
    a.jsr(OSCLI);
    a.jmp(after);
    a.bind(text);
    a.text(command);
    a.byte(b'\r');
    a.bind(after);
}

/// A language ROM that writes `vdu` through OSWRCH, passes each of
/// `commands` to OSCLI, catching the error each raises, then marks `DONE`.
fn language(vdu: &[u8], commands: &[&str]) -> Vec<u8> {
    let mut a = Asm::new(0x8000, 0x4000);
    header(&mut a, None);
    a.here("entry");
    a.lda(imm(lo("error")));
    a.sta(abs(BRKV));
    a.lda(imm(hi("error")));
    a.sta(abs(BRKV + 1));
    for &byte in vdu {
        a.lda(imm(byte));
        a.jsr(OSWRCH);
    }
    a.ldx(imm(0));
    a.stx(zp(INDEX));
    a.here("command");
    a.ldx(zp(INDEX));
    a.cpx(imm(u8::try_from(commands.len()).expect("a few commands")));
    a.beq("finished");
    a.lda(imm(0));
    a.sta(zp(ERRORS).x());
    a.ldy(abs("command_high").x());
    a.lda(abs("command_low").x());
    a.tax();
    a.jsr(OSCLI);
    a.here("next");
    a.inc(zp(INDEX));
    a.jmp("command");
    a.here("error");
    a.ldx(imm(0xFF));
    a.txs();
    a.ldy(imm(0));
    a.lda(zp(ERROR_POINTER).ind_y());
    a.ldx(zp(INDEX));
    a.sta(zp(ERRORS).x());
    a.jmp("next");
    a.here("finished");
    a.lda(imm(DONE_MARK));
    a.sta(abs(DONE));
    a.here("halt");
    a.jmp("halt");
    let texts: Vec<_> = commands.iter().map(|_| a.local()).collect();
    a.here("command_low");
    for &text in &texts {
        a.byte(lo(text));
    }
    a.here("command_high");
    for &text in &texts {
        a.byte(hi(text));
    }
    for (&text, command) in texts.iter().zip(commands) {
        a.bind(text);
        a.text(command);
        a.byte(b'\r');
    }
    a.finish().expect("the test language assembles").bytes
}

/// A machine with the MOS and `language(vdu, commands)`, not yet run.
fn boot(vdu: &[u8], commands: &[&str]) -> Machine {
    boot_rom(&language(vdu, commands))
}

/// Run `machine` until its language marks `DONE`, within `budget` cycles.
fn finish(machine: &mut Machine, budget: u64) {
    let mut cycles = 0;
    while machine.peek(DONE) != DONE_MARK {
        assert!(cycles < budget, "the language ran to its end");
        cycles += 100_000;
        machine.run_until(cycles);
    }
}

/// A machine booted into `language(vdu, commands)` and run to its end.
fn run(vdu: &[u8], commands: &[&str]) -> Machine {
    let mut machine = boot(vdu, commands);
    finish(&mut machine, BUDGET);
    machine
}

/// The error number each of `commands` raises, 0 for none.
fn command_errors(commands: &[&str]) -> Vec<u8> {
    let machine = run(&[], commands);
    (ERRORS..)
        .take(commands.len())
        .map(|address| machine.peek(address))
        .collect()
}

/// VDU 25, `code`, x; y;.
fn plot(code: u8, x: i16, y: i16) -> Vec<u8> {
    let [x0, x1] = x.to_le_bytes();
    let [y0, y1] = y.to_le_bytes();
    alloc::vec![25, code, x0, x1, y0, y1]
}

/// VDU 24, left; bottom; right; top;.
fn window(left: i16, bottom: i16, right: i16, top: i16) -> Vec<u8> {
    let mut bytes = alloc::vec![24];
    for value in [left, bottom, right, top] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// A signed word of the VDU variables.
fn word(machine: &Machine, address: u16) -> i16 {
    i16::from_le_bytes([machine.peek(address), machine.peek(address + 1)])
}

/// Mode 4's pixel (x, y), in pixels from the bottom left, on an unscrolled
/// screen.
fn mode4_lit(machine: &Machine, x: i32, y: i32) -> bool {
    let down = 255 - y;
    let offset = (down >> 3) * 320 + (x >> 3) * 8 + (down & 7);
    let byte = machine.peek(0x5800 + u16::try_from(offset).expect("on the screen"));
    byte & (0x80 >> (x & 7)) != 0
}

/// The points a line from `from` to `to` passes through, in the order MOS
/// 1.20 plots them: from whichever end comes first along the longer axis,
/// and a diagonal from the target unless it rises to the right.
fn line_points(from: (i32, i32), to: (i32, i32)) -> Vec<(i32, i32)> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let reversed = match dx.abs().cmp(&dy.abs()) {
        core::cmp::Ordering::Greater => dx < 0,
        core::cmp::Ordering::Less => dy < 0,
        core::cmp::Ordering::Equal => dx < 0 || dy < 0,
    };
    let (start, end) = if reversed { (to, from) } else { (from, to) };
    let (dx, dy) = ((end.0 - start.0).abs(), (end.1 - start.1).abs());
    let step = |a: i32, b: i32| if b >= a { 1 } else { -1 };
    let (sx, sy) = (step(start.0, end.0), step(start.1, end.1));
    let (mut x, mut y, mut error) = (start.0, start.1, dx - dy);
    let mut points = alloc::vec![(x, y)];
    while (x, y) != end {
        let twice = 2 * error;
        if twice > -dy {
            error -= dy;
            x += sx;
        }
        if twice < dx {
            error += dx;
            y += sy;
        }
        points.push((x, y));
    }
    points
}

/// The pixels PLOT `code` lights from `from` to `to` on a clear screen, in
/// the window `(left, bottom, right, top)`: a dotted line every other point,
/// and an open line's target plotted once more, which inverting undoes.
fn line_pixels(
    code: u8,
    from: (i32, i32),
    to: (i32, i32),
    window: (i32, i32, i32, i32),
) -> BTreeSet<(i32, i32)> {
    let inverting = code & 3 == 2;
    let mut lit = BTreeSet::new();
    let mut plot = |point: (i32, i32)| {
        let inside =
            (window.0..=window.2).contains(&point.0) && (window.1..=window.3).contains(&point.1);
        if inside && !(inverting && lit.remove(&point)) {
            lit.insert(point);
        }
    };
    for (index, point) in line_points(from, to).into_iter().enumerate() {
        if code & 0x10 == 0 || index % 2 == 0 {
            plot(point);
        }
    }
    if code & 8 != 0 {
        plot(to);
    }
    lit
}

/// Every lit pixel within `x` × `y`.
fn mode4_lit_in(
    machine: &Machine,
    x: core::ops::RangeInclusive<i32>,
    y: core::ops::RangeInclusive<i32>,
) -> BTreeSet<(i32, i32)> {
    x.flat_map(|x| y.clone().map(move |y| (x, y)))
        .filter(|&(x, y)| mode4_lit(machine, x, y))
        .collect()
}

#[test]
fn a_line_plots_the_same_points_whichever_end_it_starts_from() {
    // External coordinates in multiples of four, so a point is one pixel.
    let lines: [((i32, i32), (i32, i32)); 8] = [
        ((640, 512), (0, 1020)),
        ((100, 500), (300, 20)),
        ((400, 400), (560, 480)),
        ((400, 400), (240, 320)),
        ((400, 400), (480, 560)),
        ((400, 400), (564, 236)),
        ((400, 400), (236, 564)),
        ((400, 400), (400, 400)),
    ];
    for code in [5u8, 6, 13, 14, 21, 22, 29, 30] {
        for (from, to) in lines {
            for (a, b) in [(from, to), (to, from)] {
                let [ax, ay, bx, by] =
                    [a.0, a.1, b.0, b.1].map(|v| i16::try_from(v).expect("a coordinate"));
                let mut vdu = alloc::vec![22, 4];
                vdu.extend(plot(4, ax, ay));
                vdu.extend(plot(code, bx, by));
                let machine = run(&vdu, &[]);
                let (pa, pb) = ((a.0 / 4, a.1 / 4), (b.0 / 4, b.1 / 4));
                let expected = line_pixels(code, pa, pb, (0, 0, 319, 255));
                let (x0, x1) = (pa.0.min(pb.0), pa.0.max(pb.0));
                let (y0, y1) = (pa.1.min(pb.1), pa.1.max(pb.1));
                assert_eq!(
                    mode4_lit_in(&machine, x0..=x1, y0..=y1),
                    expected,
                    "PLOT {code} from {a:?} to {b:?}"
                );
            }
        }
    }
}

#[test]
fn a_clipped_line_plots_its_points_inside_the_window_in_their_dot_phase() {
    // Window (64, 16)–(200, 120) in pixels; lines entering, leaving, crossing
    // and missing it, solid and dotted, with endpoints far outside.
    let cases: [((i16, i16), (i16, i16)); 6] = [
        ((-4000, 300), (700, 360)),
        ((900, 30), (100, 470)),
        ((-30000, 200), (30000, 280)),
        ((300, -3000), (500, 3000)),
        ((0, 0), (1276, 1020)),
        ((0, 1000), (1200, 900)),
    ];
    for code in [5u8, 21, 29] {
        for (a, b) in cases {
            let mut vdu = alloc::vec![22, 4];
            vdu.extend(window(256, 64, 803, 483));
            vdu.extend(plot(4, a.0, a.1));
            vdu.extend(plot(code, b.0, b.1));
            let machine = run(&vdu, &[]);
            let internal = |p: (i16, i16)| (i32::from(p.0) >> 2, i32::from(p.1) >> 2);
            let expected = line_pixels(code, internal(a), internal(b), (64, 16, 200, 120));
            assert_eq!(
                mode4_lit_in(&machine, 60..=204, 12..=124),
                expected,
                "PLOT {code} from {a:?} to {b:?}"
            );
        }
    }
}

#[test]
fn plot_keeps_the_cursor_s_external_coordinates_as_given() {
    let mut vdu = alloc::vec![22, 4, 29, 1, 0, 0, 0];
    vdu.extend(plot(4, 1057, 568));
    let machine = run(&vdu, &[]);
    assert_eq!(
        (
            word(&machine, vdu::GRAPHICS_CURSOR),
            word(&machine, vdu::GRAPHICS_CURSOR + 2)
        ),
        (1057, 568)
    );
    vdu.extend(plot(0, 3, 1));
    let machine = run(&vdu, &[]);
    assert_eq!(
        (
            word(&machine, vdu::GRAPHICS_CURSOR),
            word(&machine, vdu::GRAPHICS_CURSOR + 2)
        ),
        (1060, 569)
    );
}

#[test]
fn a_fill_leaves_its_run_s_ends_as_the_previous_point_and_the_cursor() {
    // From (25, 25), left and right to a point at (100, 25).
    let mut vdu = alloc::vec![22, 4];
    vdu.extend(plot(69, 400, 101));
    vdu.extend(plot(77, 101, 101));
    let machine = run(&vdu, &[]);
    let old = (
        word(&machine, vdu::OLD_GRAPHICS_CURSOR),
        word(&machine, vdu::OLD_GRAPHICS_CURSOR + 2),
    );
    let cursor = (
        word(&machine, vdu::GRAPHICS_CURSOR),
        word(&machine, vdu::GRAPHICS_CURSOR + 2),
    );
    assert_eq!((old, cursor), ((0, 25), (396, 100)));
}

#[test]
fn a_fill_that_fills_nothing_leaves_the_target_a_line_up_as_the_previous_point() {
    // A lit target, and one outside the window: the cursor is the target,
    // the previous point the target a line up, carried into no high byte.
    for (lit, target, old) in [
        (true, (401, 801), (100, 201)),
        (false, (1500, 801), (375, 201)),
        (true, (401, 1023), (100, 0)),
    ] {
        let mut vdu = alloc::vec![22, 4];
        if lit {
            vdu.extend(plot(69, target.0, target.1));
        }
        vdu.extend(plot(77, target.0, target.1));
        let machine = run(&vdu, &[]);
        let previous = (
            word(&machine, vdu::OLD_GRAPHICS_CURSOR),
            word(&machine, vdu::OLD_GRAPHICS_CURSOR + 2),
        );
        assert_eq!(previous, old, "PLOT 77 at {target:?}");
        let cursor = (
            word(&machine, vdu::GRAPHICS_CURSOR),
            word(&machine, vdu::GRAPHICS_CURSOR + 2),
        );
        assert_eq!(
            cursor,
            (target.0 & !3, target.1 & !3),
            "PLOT 77 at {target:?}"
        );
    }
}

/// A bitmap mode's lit pixels on line `y`, from the bottom, as x from the
/// left, on an unscrolled screen.
fn lit_row(machine: &Machine, mode: u8, y: i32) -> BTreeSet<i32> {
    let (base, width, pixels_per_byte): (u16, i32, i32) = match mode {
        0 => (0x3000, 640, 8),
        1 => (0x3000, 320, 4),
        2 => (0x3000, 160, 2),
        4 => (0x5800, 320, 8),
        5 => (0x5800, 160, 4),
        _ => unreachable!("mode {mode} has no bitmap"),
    };
    let left_mask: u8 = match pixels_per_byte {
        8 => 0x80,
        4 => 0x88,
        _ => 0xAA,
    };
    let down = 255 - y;
    let row_bytes = width / pixels_per_byte * 8;
    (0..width)
        .filter(|&x| {
            let offset = (down >> 3) * row_bytes + x / pixels_per_byte * 8 + (down & 7);
            let byte = machine.peek(base + u16::try_from(offset).expect("on the screen"));
            byte & (left_mask >> (x % pixels_per_byte)) != 0
        })
        .collect()
}

/// Pixel (x, y)'s external coordinates in a mode `scale` units a pixel
/// across.
fn external(scale: i32, x: i32, y: i32) -> (i16, i16) {
    let at = |value: i32| i16::try_from(value).expect("a coordinate");
    (at(x * scale), at(y * 4))
}

/// PLOT `code` at pixel (x, y).
fn plot_pixel_at(code: u8, scale: i32, x: i32, y: i32) -> Vec<u8> {
    let (x, y) = external(scale, x, y);
    plot(code, x, y)
}

/// A run is found along its line and plotted as a span: exactly the pixels
/// between the nearest lit ones, or the window's edges, whatever the mode
/// packs into a byte; filling right over lit pixels stops at the first unlit
/// one, and from an unlit one fills nothing.
#[test]
fn a_fill_lights_the_run_between_lit_pixels_or_the_window_s_edges() {
    for (mode, width, scale) in [
        (0u8, 640, 2),
        (1, 320, 4),
        (2, 160, 8),
        (4, 320, 4),
        (5, 160, 8),
    ] {
        let (left, right) = (5, width - 7);
        let (a, b) = (13, width / 2 + 3);
        let mut vdu = alloc::vec![22, mode];
        let ((x0, y0), (x1, y1)) = (external(scale, left, 8), external(scale, right, 247));
        vdu.extend(window(x0, y0, x1, y1));
        for (code, x, y) in [
            (69, a, 100),
            (69, b, 100),
            (77, a + 5, 100),
            (77, b + 9, 100),
            (77, a + 1, 101),
            (69, a, 102),
            (69, b, 102),
            (77, a + 5, 102),
            (95, a + 3, 102),
            (93, a + 5, 103),
        ] {
            vdu.extend(plot_pixel_at(code, scale, x, y));
        }
        let machine = run(&vdu, &[]);
        let lines: [(i32, BTreeSet<i32>); 6] = [
            (99, BTreeSet::new()),
            (100, (a..=right).collect()),
            (101, (left..=right).collect()),
            (102, (a..=a + 2).collect()),
            (103, BTreeSet::new()),
            (104, BTreeSet::new()),
        ];
        for (y, lit) in lines {
            assert_eq!(lit_row(&machine, mode, y), lit, "mode {mode}, line {y}");
        }
    }
}

/// The regression: a fill stepped a pixel at a time through its whole
/// address, near half a million cycles a MODE 4 line where MOS 1.20 takes
/// some six thousand. Sixty-four lines filled across, against the same
/// lines' single points, are held to a bound the walk along the line meets.
#[test]
fn a_fill_walks_its_line_rather_than_addressing_each_pixel() {
    let cycles = |code: u8| {
        let mut vdu = alloc::vec![22, 4];
        for y in 0..64 {
            vdu.extend(plot_pixel_at(code, 4, 160, 100 + y));
        }
        let mut machine = boot(&vdu, &[]);
        finish(&mut machine, BUDGET);
        machine.cycles()
    };
    let filling = cycles(77) - cycles(69);
    assert!(filling < 3_000_000, "64 lines filled in {filling} cycles");
}

#[test]
fn vdu_29_moves_the_origin_under_the_cursor_and_vdu_26_restores_them() {
    let mut vdu = alloc::vec![22, 4];
    vdu.extend(plot(4, 501, 301));
    vdu.extend([29, 101, 0, 9, 1]);
    let machine = run(&vdu, &[]);
    assert_eq!(
        (
            word(&machine, vdu::GRAPHICS_CURSOR),
            word(&machine, vdu::GRAPHICS_CURSOR + 2)
        ),
        (399, 35)
    );
    assert_eq!(
        (
            word(&machine, vdu::GRAPHICS_INTERNAL),
            word(&machine, vdu::GRAPHICS_INTERNAL + 2)
        ),
        (125, 75)
    );
    vdu.extend(plot(5, 601, 401));
    vdu.push(26);
    let machine = run(&vdu, &[]);
    for point in [
        vdu::ORIGIN,
        vdu::GRAPHICS_CURSOR,
        vdu::OLD_GRAPHICS_CURSOR,
        vdu::GRAPHICS_INTERNAL,
    ] {
        assert_eq!(
            (word(&machine, point), word(&machine, point + 2)),
            (0, 0),
            "&{point:03X}"
        );
    }
}

#[test]
fn star_fx_and_star_opt_take_every_parameter() {
    let machine = run(&[], &["FX255,6,0", "OPT1,2", "OPT3,7"]);
    assert_eq!(machine.peek(var::STARTUP_OPTIONS), 6);
    assert_eq!(
        machine.peek(0xE3),
        0xED,
        "long messages for both operations"
    );
    assert_eq!(machine.peek(0x03D1), 7, "the inter-block gap");
}

/// Where VDU 5's cursor starts, the codes sent it, and where it ends, in
/// mode 4's external coordinates.
type CursorMove = ((i16, i16), &'static [u8], (i16, i16));

#[test]
fn vdu_5_moves_the_graphics_cursor_a_cell_wrapping_at_the_window() {
    let cases: [CursorMove; 10] = [
        ((500, 500), &[8], (468, 500)),
        ((31, 500), &[8], (1248, 532)),
        ((1248, 500), &[9], (0, 468)),
        ((1247, 500), &[9], (1276, 500)),
        ((500, 10), &[10], (500, 1020)),
        ((500, 992), &[11], (500, 28)),
        ((500, 500), &[13], (0, 500)),
        ((500, 500), &[30], (0, 1020)),
        ((1248, 2000), &[9], (1280, 2000)),
        ((1260, 500), &[127, 9, 9], (0, 468)),
    ];
    for (start, codes, end) in cases {
        let mut vdu = alloc::vec![22, 4];
        vdu.extend(plot(4, start.0, start.1));
        vdu.push(5);
        vdu.extend_from_slice(codes);
        vdu.push(4);
        let machine = run(&vdu, &[]);
        let cursor = (
            word(&machine, vdu::GRAPHICS_CURSOR),
            word(&machine, vdu::GRAPHICS_CURSOR + 2),
        );
        assert_eq!(cursor, end, "from {start:?} by {codes:?}");
    }
}

#[test]
fn vdu_5_prints_into_the_cell_and_delete_fills_it_with_the_background_colour() {
    // A defined character at (125, 125): its rows, then after DEL the cell
    // solid in background colour 1 whatever the action.
    let glyph = [0x81u8, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81];
    let mut vdu = alloc::vec![22, 4, 23, 224];
    vdu.extend_from_slice(&glyph);
    vdu.extend(plot(4, 500, 500));
    vdu.extend([5, 224, 4]);
    let machine = run(&vdu, &[]);
    for (row, &bits) in (0..).zip(&glyph) {
        let lit: u8 = (0..8).fold(0, |acc, column| {
            acc << 1 | u8::from(mode4_lit(&machine, 125 + column, 125 - row))
        });
        assert_eq!(lit, bits, "row {row}");
    }
    vdu.splice(vdu.len() - 1.., [18, 3, 129, 127, 4]);
    let machine = run(&vdu, &[]);
    let filled = mode4_lit_in(&machine, 124..=133, 117..=126);
    let cell: BTreeSet<_> = (125..133)
        .flat_map(|x| (118..126).map(move |y| (x, y)))
        .collect();
    assert_eq!(filled, cell);
}

#[test]
fn a_numeric_command_takes_its_numbers_and_nothing_else() {
    let cases: [(&str, u8); 32] = [
        ("FX4,1 X", BAD_COMMAND),
        ("FX4,1,", BAD_COMMAND),
        ("FX 4 , 1", 0),
        ("FX4 1", 0),
        ("FX4,1,2,3", BAD_COMMAND),
        ("FX4,,1", BAD_COMMAND),
        ("FX4X", BAD_COMMAND),
        ("FX,4", BAD_COMMAND),
        ("FX4,256", BAD_COMMAND),
        ("FX256", BAD_COMMAND),
        ("FX 4,-1", BAD_COMMAND),
        ("FX4,&1", BAD_COMMAND),
        ("FX  4,  1  ", 0),
        ("FX", BAD_COMMAND),
        ("FX004,1", 0),
        ("FX4", 0),
        ("OPT", 0),
        ("OPT1,2,3", BAD_COMMAND),
        ("OPT1,2 X", BAD_COMMAND),
        ("OPT 1,256", BAD_COMMAND),
        ("OPT,1", BAD_COMMAND),
        ("OPT1,", BAD_COMMAND),
        ("OPT1 ,2", 0),
        ("OPT X", BAD_COMMAND),
        ("TV255,1,1", BAD_COMMAND),
        ("TV", 0),
        ("TAPE3 X", BAD_COMMAND),
        ("TAPE3,1", 0),
        ("MOTOR1 X", BAD_COMMAND),
        ("MOTOR256", BAD_COMMAND),
        ("CODE1,2,3", BAD_COMMAND),
        ("KEY300 X", BAD_KEY),
    ];
    let commands: Vec<&str> = cases.iter().map(|&(command, _)| command).collect();
    let expected: Vec<u8> = cases.iter().map(|&(_, error)| error).collect();
    assert_eq!(command_errors(&commands), expected);
}

#[test]
fn osbyte_20_records_how_many_zones_it_exploded() {
    for (command, zones) in [("FX20,3", 3), ("FX20,7", 6), ("FX20", 0)] {
        let machine = run(&[], &[command]);
        assert_eq!(machine.peek(var::FONT_STATE), zones, "*{command}");
    }
}

#[test]
fn a_key_sets_the_keyboard_semaphore_which_must_still_be_set_to_rearm_it() {
    // A held through the language's start; then B, which the keyboard hears
    // unless the semaphore was cleared while A was down.
    for (commands, rearmed) in [(&[][..], true), (&["FX178,0"][..], false)] {
        let mut machine = boot(&[], commands);
        machine.set_key(Key::A, true);
        finish(&mut machine, BUDGET);
        assert_eq!(
            machine.peek(var::KEYBOARD_SEMAPHORE),
            if rearmed { 0xFF } else { 0 },
            "{commands:?}"
        );
        machine.set_key(Key::A, false);
        wait(&mut machine, 10);
        assert_eq!(machine.peek(var::KEYBOARD_SEMAPHORE), 0, "{commands:?}");
        let typed = machine.peek(page2::BUFFER_END);
        machine.set_key(Key::B, true);
        wait(&mut machine, 10);
        machine.set_key(Key::B, false);
        wait(&mut machine, 10);
        assert_eq!(
            machine.peek(page2::BUFFER_END) != typed,
            rearmed,
            "{commands:?}"
        );
    }
}

#[test]
fn save_s_addresses_default_to_its_start_and_load_takes_one_given() {
    // Saved from the language's own first bytes, RETURN held for the record
    // prompt; then loaded elsewhere by a second machine.
    const SAVE_BUDGET: u64 = 80_000_000;
    let mut saving = boot(&[], &["OPT1,2", "SAVE PROG 8000 +17"]);
    saving.insert_tape(Tape::default());
    saving.set_tape_record(true);
    saving.set_key(Key::RETURN, true);
    finish(&mut saving, SAVE_BUDGET);
    saving.set_key(Key::RETURN, false);
    assert_eq!(saving.peek(ERRORS + 1), 0, "SAVE raised no error");
    assert!(
        mode7_text(&saving)
            .iter()
            .any(|line| line == "PROG       00 0017    00008000 00008000"),
        "the long message: {:?}",
        mode7_text(&saving)
    );
    let tape = saving.eject_tape().expect("the tape saved to");
    let mut loading = boot(&[], &["LOAD PROG 2A00"]);
    loading.insert_tape(tape);
    loading.wind_tape(0);
    finish(&mut loading, SAVE_BUDGET);
    assert_eq!(loading.peek(ERRORS), 0, "LOAD raised no error");
    let image = language(&[], &["OPT1,2", "SAVE PROG 8000 +17"]);
    let loaded: Vec<u8> = (0x2A00..0x2A17)
        .map(|address| loading.peek(address))
        .collect();
    assert_eq!(loaded, image[..0x17]);
}

/// Where a test's filing system logs its calls, and how many it has.
const LOG: u16 = 0x0400;
const LOGGED: u16 = 0x0072;

/// A filing system's OSFIND that logs A and Y and answers handle &C3.
fn logging_find(a: &mut Asm) {
    a.here("find");
    a.pha();
    a.txa();
    a.pha();
    a.tsx();
    a.lda(abs(STACK_PAGE + 2).x());
    a.ldx(zp(LOGGED));
    a.sta(abs(LOG).x());
    a.tya();
    a.sta(abs(LOG + 1).x());
    a.inx();
    a.inx();
    a.stx(zp(LOGGED));
    a.pla();
    a.tax();
    a.pla();
    a.lda(imm(0xC3));
    a.rts();
}

/// The regression: closing the old file left Y at its handle, so `*SPOOL`
/// and `*EXEC` read on along the line from there and opened a file named by
/// whatever lay at that offset.
#[test]
fn spool_and_exec_close_the_old_file_and_open_only_what_they_name() {
    use super::ws::vector;
    let rom = program(None, |a| {
        a.lda(imm(lo("find")));
        a.sta(abs(vector::FIND));
        a.lda(imm(hi("find")));
        a.sta(abs(vector::FIND + 1));
        for command in ["SPOOL F", "SPOOL", "EXEC F", "EXEC"] {
            oscli(a, command);
        }
        a.lda(imm(DONE_MARK));
        a.sta(abs(DONE));
        a.here("idle");
        a.jmp("idle");
        logging_find(a);
    });
    let machine = run_rom(&rom);
    let calls: Vec<u8> = (0..machine.peek(LOGGED))
        .step_by(2)
        .map(|at| machine.peek(LOG + u16::from(at)))
        .collect();
    assert_eq!(calls, [0x80, 0x00, 0x40, 0x00], "open, close, open, close");
    assert_eq!(
        (machine.peek(LOG + 3), machine.peek(LOG + 7)),
        (0xC3, 0xC3),
        "each closes its own"
    );
}

/// The regression: `*HELP` printed its banner through Y, so the ROMs were
/// told of a topic starting at the word HELP itself.
#[test]
fn help_tells_the_roms_the_topic_after_the_word() {
    const TOPICS: u16 = 0x0074;
    let rom = program(Some("service"), |a| {
        a.lda(imm(0));
        a.sta(zp(TOPICS));
        oscli(a, "HELP");
        oscli(a, "HELP  TOPIC");
        a.lda(imm(DONE_MARK));
        a.sta(abs(DONE));
        a.here("idle");
        a.jmp("idle");
        // Service 9: keep the first character of the topic.
        a.here("service");
        a.cmp(imm(9));
        a.bne("unclaimed");
        a.pha();
        a.txa();
        a.pha();
        a.lda(zp(0xF2).ind_y());
        a.ldx(zp(TOPICS));
        a.sta(zp(TOPICS + 1).x());
        a.inc(zp(TOPICS));
        a.pla();
        a.tax();
        a.pla();
        a.here("unclaimed");
        a.rts();
    });
    let machine = run_rom(&rom);
    assert_eq!(machine.peek(TOPICS), 2);
    assert_eq!(
        (machine.peek(TOPICS + 1), machine.peek(TOPICS + 2)),
        (b'\r', b'T')
    );
}

/// The regression: inline text was printed through the error pointer, so a
/// message such as `*HELP`'s left a language's REPORT naming ROM bytes.
#[test]
fn printing_a_message_leaves_the_error_pointer_naming_the_last_error() {
    let machine = run(&[], &["FOO", "HELP"]);
    assert_eq!(machine.peek(ERRORS), BAD_COMMAND);
    let pointer =
        u16::from_le_bytes([machine.peek(ERROR_POINTER), machine.peek(ERROR_POINTER + 1)]);
    let bad_command = model_b().expect("the MOS assembles").symbol("bad_command");
    assert_eq!(
        Some(pointer),
        bad_command.map(|brk| brk + 1),
        "it still names Bad command"
    );
}

/// What GSREAD gives for `text` after GSINIT with C clear: its characters
/// and Y at the end, or the number of the error it raised.
fn gs_read(text: &'static str) -> Result<(Vec<u8>, u8), u8> {
    const COUNT: u16 = 0x0074;
    const END: u16 = 0x0075;
    const ERROR: u16 = 0x0076;
    const READ: u16 = 0x0900;
    let rom = program(None, |a| {
        a.lda(imm(lo("caught")));
        a.sta(abs(BRKV));
        a.lda(imm(hi("caught")));
        a.sta(abs(BRKV + 1));
        a.lda(imm(lo("text")));
        a.sta(zp(0xF2));
        a.lda(imm(hi("text")));
        a.sta(zp(0xF3));
        a.ldy(imm(0));
        a.sty(zp(COUNT));
        a.sty(zp(ERROR));
        a.clc();
        a.jsr(GSINIT);
        a.here("character");
        a.jsr(GSREAD);
        a.bcs("ended");
        a.ldx(zp(COUNT));
        a.sta(abs(READ).x());
        a.inc(zp(COUNT));
        a.jmp("character");
        a.here("ended");
        a.sty(zp(END));
        a.jmp("finished");
        a.here("caught");
        a.ldx(imm(0xFF));
        a.txs();
        a.ldy(imm(0));
        a.lda(zp(ERROR_POINTER).ind_y());
        a.sta(zp(ERROR));
        a.jmp("finished");
        a.here("text");
        a.text(text);
        a.byte(b'\r');
        a.here("finished");
    });
    let machine = run_rom(&rom);
    match machine.peek(ERROR) {
        0 => Ok((
            (0..u16::from(machine.peek(COUNT)))
                .map(|at| machine.peek(READ + at))
                .collect(),
            machine.peek(END),
        )),
        error => Err(error),
    }
}

/// The regressions: a quoted string ended at a space, the space ending a
/// plain one was left under Y, and a bar before the CR read on past the
/// line's end.
#[test]
fn gsread_reads_strings_as_mos_1_20_does() {
    const BAD_STRING: u8 = 0xFD;
    type Read = Result<(&'static [u8], u8), u8>;
    let cases: [(&str, Read); 9] = [
        ("\"A B\" C", Ok((b"A B", 6))),
        ("A B", Ok((b"A", 2))),
        ("A  B", Ok((b"A", 3))),
        ("\"A\"  B", Ok((b"A", 5))),
        ("\"A\"\"B\"", Ok((b"A\"B", 6))),
        (
            "A|!B|?|\"|@|J",
            Ok((&[b'A', 0xC2, 0x7F, b'"', 0, 0x0A], 12)),
        ),
        ("\"A|\"\"", Ok((b"A\"", 5))),
        ("A|", Err(BAD_STRING)),
        ("\"A", Err(BAD_STRING)),
    ];
    for (text, expected) in cases {
        let expected = expected.map(|(read, end)| (read.to_vec(), end));
        assert_eq!(gs_read(text), expected, "{text}");
    }
}

/// The regression: OSWORD 0 handed back the caller's Y, not the length of
/// the line it read.
#[test]
fn osword_0_returns_the_length_read_and_whether_escape_ended_it() {
    const BLOCK: u16 = 0x0074;
    const LINE: u16 = 0x0900;
    const WAITING: u16 = 0x0079;
    // Y and the flags after each of two lines.
    const RESULTS: u16 = 0x007A;
    let [line_low, line_high] = LINE.to_le_bytes();
    let [block_low, block_high] = BLOCK.to_le_bytes();
    let rom = program(None, |a| {
        for (offset, byte) in (0..).zip([line_low, line_high, 30, b' ', b'~']) {
            a.lda(imm(byte));
            a.sta(zp(BLOCK + offset));
        }
        for (line, typed) in [(0, &b"HEY\r"[..]), (1, b"AB")] {
            for &byte in typed {
                a.lda(imm(138));
                a.ldx(imm(0));
                a.ldy(imm(byte));
                a.jsr(OSBYTE);
            }
            a.lda(imm(line));
            a.sta(zp(WAITING));
            a.lda(imm(0));
            a.ldx(imm(block_low));
            a.ldy(imm(block_high));
            a.jsr(OSWORD);
            a.php();
            a.sty(zp(RESULTS + 2 * u16::from(line)));
            a.pla();
            a.sta(zp(RESULTS + 2 * u16::from(line) + 1));
        }
    });
    let mut machine = boot_rom(&rom);
    let mut cycles = 0;
    while machine.peek(WAITING) != 1 {
        assert!(cycles < BUDGET, "the second line is awaited");
        cycles += 100_000;
        machine.run_until(cycles);
    }
    wait(&mut machine, 10);
    machine.set_key(Key::ESCAPE, true);
    wait(&mut machine, 10);
    machine.set_key(Key::ESCAPE, false);
    finish(&mut machine, BUDGET);
    let carry = |line: u16| machine.peek(RESULTS + 2 * line + 1) & 1 != 0;
    assert_eq!(
        (machine.peek(RESULTS), carry(0)),
        (3, false),
        "HEY, ended by RETURN"
    );
    assert_eq!(
        (machine.peek(RESULTS + 2), carry(1)),
        (2, true),
        "AB, ended by Escape"
    );
    let read: Vec<u8> = (LINE..LINE + 4).map(|at| machine.peek(at)).collect();
    assert_eq!(read, b"ABY\r", "the second line over the first");
}

/// The regression: while copy editing, a typed character was drawn at the
/// copy cursor, its cell not followed to the input position.
#[test]
fn a_character_typed_while_copy_editing_lands_at_the_input_position() {
    const BLOCK: u16 = 0x0074;
    const LINE: u16 = 0x0900;
    const CURSOR_UP: u8 = 0x8F;
    const TELETEXT: u16 = 0x7C00;
    let [line_low, line_high] = LINE.to_le_bytes();
    let [block_low, block_high] = BLOCK.to_le_bytes();
    let rom = program(None, |a| {
        for byte in [22, 7, 31, 0, 5, b'A', b'B'] {
            a.lda(imm(byte));
            a.jsr(OSWRCH);
        }
        for byte in [CURSOR_UP, b'X', b'\r'] {
            a.lda(imm(138));
            a.ldx(imm(0));
            a.ldy(imm(byte));
            a.jsr(OSBYTE);
        }
        for (offset, byte) in (0..).zip([line_low, line_high, 30, b' ', b'~']) {
            a.lda(imm(byte));
            a.sta(zp(BLOCK + offset));
        }
        a.lda(imm(0));
        a.ldx(imm(block_low));
        a.ldy(imm(block_high));
        a.jsr(OSWORD);
    });
    let machine = run_rom(&rom);
    let cell = |row: u16, column: u16| machine.peek(TELETEXT + 40 * row + column);
    assert_eq!(cell(5, 2), b'X', "at the input position");
    assert_eq!(cell(4, 2), b' ', "not at the copy cursor above it");
    assert_eq!(machine.peek(LINE), b'X', "and read as the line");
}

/// The regression: OSWORD 5 and 6 turned interrupts off and left them off.
#[test]
fn osword_5_and_6_reach_memory_and_leave_interrupts_on() {
    const BLOCK: u16 = 0x0074;
    const FLAGS: u16 = 0x0079;
    const TARGET: u16 = 0x3000;
    let [block_low, block_high] = BLOCK.to_le_bytes();
    let [target_low, target_high] = TARGET.to_le_bytes();
    let rom = program(None, |a| {
        for (call, byte, flags) in [(6, 0xA5, FLAGS), (5, 0, FLAGS + 1)] {
            for (offset, byte) in (0..).zip([target_low, target_high, 0xFF, 0xFF, byte]) {
                a.lda(imm(byte));
                a.sta(zp(BLOCK + offset));
            }
            a.cli();
            a.lda(imm(call));
            a.ldx(imm(block_low));
            a.ldy(imm(block_high));
            a.jsr(OSWORD);
            a.php();
            a.pla();
            a.sta(zp(flags));
        }
    });
    let machine = run_rom(&rom);
    assert_eq!(machine.peek(TARGET), 0xA5, "OSWORD 6 wrote the byte");
    assert_eq!(machine.peek(BLOCK + 4), 0xA5, "OSWORD 5 read it back");
    for (call, flags) in [(6, FLAGS), (5, FLAGS + 1)] {
        assert_eq!(
            machine.peek(flags) & INTERRUPTS_OFF,
            0,
            "OSWORD {call} left I clear"
        );
    }
}

/// Keep A, X, Y and the flags as a call left them, from `at` on; A is left
/// the flags.
fn record(a: &mut Asm, at: u16) {
    a.php();
    a.sta(abs(at));
    a.stx(abs(at + 1));
    a.sty(abs(at + 2));
    a.pla();
    a.sta(abs(at + 3));
}

/// What [`record`] kept at `at`.
fn recorded(machine: &Machine, at: u16) -> [u8; 4] {
    [0, 1, 2, 3].map(|offset| machine.peek(at + offset))
}

/// Point vector `vector` at `handler`, keeping what it held at `old`.
fn claim(a: &mut Asm, vector: u16, handler: &'static str, old: u16) {
    a.sei();
    for offset in 0..2 {
        a.lda(abs(vector + offset));
        a.sta(zp(old + offset));
    }
    a.lda(imm(lo(handler)));
    a.sta(abs(vector));
    a.lda(imm(hi(handler)));
    a.sta(abs(vector + 1));
    a.cli();
}

/// The regressions: an OSBYTE or OSWORD nothing takes kept the caller's X
/// and flags, where MOS 1.20 refuses it with X &FF, N and V set and C clear,
/// Y 0 for an OSBYTE below &75; and OSWORD &E0 on was offered to the ROMs,
/// not passed to USERV.
#[test]
fn calls_nothing_takes_are_refused_and_user_oswords_go_to_userv() {
    const RECORDS: u16 = 0x0900;
    const USER_SAW: u16 = 0x0940;
    const CALLS: [(u16, u8); 4] = [
        (OSBYTE, 0x30),
        (OSBYTE, 0xA1),
        (OSWORD, 0x20),
        (OSWORD, 0xE0),
    ];
    let rom = program(None, |a| {
        a.lda(imm(lo("user")));
        a.sta(abs(vector::USER));
        a.lda(imm(hi("user")));
        a.sta(abs(vector::USER + 1));
        for (record_at, (entry, call)) in (RECORDS..).step_by(4).zip(CALLS) {
            a.lda(imm(call));
            a.ldx(imm(5));
            a.ldy(imm(7));
            a.sec();
            a.jsr(entry);
            record(a, record_at);
        }
        a.jmp("calls_made");
        a.here("user");
        a.sta(abs(USER_SAW));
        a.stx(abs(USER_SAW + 1));
        a.sty(abs(USER_SAW + 2));
        a.rts();
        a.here("calls_made");
    });
    let machine = run_rom(&rom);
    for (record_at, (call, y)) in (RECORDS..)
        .step_by(4)
        .zip([(0x30, 0), (0xA1, 7), (0x20, 7)])
    {
        let [a, x, y_out, flags] = recorded(&machine, record_at);
        assert_eq!((a, x, y_out), (call, 0xFF, y), "call &{call:02X}");
        assert_eq!(
            flags & (NEGATIVE | OVERFLOW | CARRY),
            NEGATIVE | OVERFLOW,
            "call &{call:02X}"
        );
    }
    let saw: Vec<u8> = (USER_SAW..USER_SAW + 3)
        .map(|at| machine.peek(at))
        .collect();
    assert_eq!(saw, [0xE0, 5, 7], "USERV given OSWORD &E0's A, X and Y");
}

/// The regressions: INKEY(-256) answered OSBYTE 0's version, not the &FF
/// that names MOS 1.20; OSBYTE 0 kept Y and OSBYTE 18 X and Y, where MOS
/// 1.20 returns them 0.
#[test]
fn osbyte_identifies_mos_1_20_and_resetting_the_keys_returns_zeroes() {
    const RECORDS: u16 = 0x0900;
    const CALLS: [(u8, u8, u8); 3] = [(0x81, 0, 0xFF), (0, 1, 0x81), (18, 5, 7)];
    let rom = program(None, |a| {
        for (record_at, (call, x, y)) in (RECORDS..).step_by(4).zip(CALLS) {
            a.lda(imm(call));
            a.ldx(imm(x));
            a.ldy(imm(y));
            a.jsr(OSBYTE);
            record(a, record_at);
        }
    });
    let machine = run_rom(&rom);
    let returned = |record_at: u16| {
        let [_, x, y, _] = recorded(&machine, record_at);
        (x, y)
    };
    assert_eq!(returned(RECORDS), (0xFF, 0xFF), "INKEY(-256)");
    assert_eq!(returned(RECORDS + 4), (1, 0), "OSBYTE 0");
    assert_eq!(returned(RECORDS + 8), (0, 0), "OSBYTE 18");
}

/// The regressions: *FX118 handed back the caller's X and Y with C set;
/// OSBYTE &79 and &7A the caller's Y; a negative INKEY's C was set for a key
/// up; and OSBYTE &80 with X 0 left C clear. MOS 1.20 returns X 1, Y &7F and
/// C clear from *FX118, Y &7F from a key's test and &EE from a scan, C as
/// the key is from INKEY, and C set from the fire buttons' read.
#[test]
fn keyboard_osbytes_return_what_mos_1_20_does() {
    const RECORDS: u16 = 0x0900;
    const CALLS: [(u8, u8, u8); 6] = [
        (0x76, 5, 9),
        (0x79, 0x80, 3),
        (0x79, 0x10, 3),
        (0x7A, 0, 3),
        (0x81, 0xFF, 0xFF),
        (0x80, 0, 3),
    ];
    let rom = program(None, |a| {
        for (record_at, (call, x, y)) in (RECORDS..).step_by(4).zip(CALLS) {
            a.lda(imm(call));
            a.ldx(imm(x));
            a.ldy(imm(y));
            a.jsr(OSBYTE);
            record(a, record_at);
        }
    });
    let machine = run_rom(&rom);
    let returned = |call: u16| {
        let [_, x, y, flags] = recorded(&machine, RECORDS + 4 * call);
        (x, y, flags & CARRY != 0)
    };
    assert_eq!(returned(0), (1, 0x7F, false), "*FX118");
    assert_eq!(returned(1), (0, 0x7F, true), "SHIFT tested, up");
    assert_eq!(returned(2), (0xFF, 0xEE, true), "a scan finding nothing");
    assert_eq!(returned(3), (0xFF, 0xEE, true), "a scan from 16");
    assert_eq!(returned(4), (0, 0, false), "INKEY(-1), SHIFT up");
    let (buttons, _, carry) = returned(5);
    assert_eq!((buttons, carry), (0, true), "the fire buttons");
}

/// The regression: INSV returned the buffer's new end in A, not the
/// character it took.
#[test]
fn insv_keeps_the_character_it_inserts() {
    const RECORD: u16 = 0x0900;
    let rom = program(None, |a| {
        a.lda(imm(b'Q'));
        a.ldx(imm(0));
        a.jsr("insert");
        record(a, RECORD);
        a.jmp("inserted");
        a.here("insert");
        a.jmp(abs(vector::INS).ind());
        a.here("inserted");
    });
    let [a, x, _, flags] = recorded(&run_rom(&rom), RECORD);
    assert_eq!((a, x, flags & CARRY), (b'Q', 0, 0));
}

/// The regressions: the cassette's OSARGS returned its own X, and OSFIND its
/// own X and Y, where MOS 1.20 keeps both.
#[test]
fn osargs_and_osfind_keep_x_and_y() {
    const RECORDS: u16 = 0x0900;
    const CLOSE_BUDGET: u64 = 80_000_000;
    let rom = program(None, |a| {
        oscli(a, "OPT1,0");
        a.lda(imm(0));
        a.ldx(imm(0x5A));
        a.ldy(imm(0));
        a.jsr(OSARGS);
        record(a, RECORDS);
        a.lda(imm(0x80));
        a.ldx(imm(lo("name")));
        a.ldy(imm(hi("name")));
        a.jsr(OSFIND);
        record(a, RECORDS + 4);
        a.ldy(abs(RECORDS + 4));
        a.lda(imm(0));
        a.ldx(imm(0x3C));
        a.jsr(OSFIND);
        record(a, RECORDS + 8);
        a.jmp("closed");
        a.here("name");
        a.text("F");
        a.byte(b'\r');
        a.here("closed");
    });
    let mut machine = boot_rom(&rom);
    machine.insert_tape(Tape::default());
    machine.set_tape_record(true);
    finish(&mut machine, CLOSE_BUDGET);
    let [filing_system, x, y, _] = recorded(&machine, RECORDS);
    assert_eq!(
        (filing_system, x, y),
        (1, 0x5A, 0),
        "OSARGS 0, 0: the 1200 baud CFS"
    );
    let [handle, x, y, _] = recorded(&machine, RECORDS + 4);
    let name = u16::from_le_bytes([x, y])
        .checked_sub(0x8000)
        .map(usize::from);
    assert_eq!(
        name.and_then(|at| rom.get(at..at + 2)),
        Some(&b"F\r"[..]),
        "OSFIND kept X and Y naming the file"
    );
    let [_, x, y, _] = recorded(&machine, RECORDS + 8);
    assert_eq!((x, y), (0x3C, handle), "closing kept X and Y");
}

/// The soft keys' pointers, &B00 to &B10, and the text after them.
fn soft_keys(machine: &Machine, text: u16) -> (Vec<u8>, Vec<u8>) {
    let pointers = (buffer::SOFT_KEYS..=buffer::SOFT_KEYS + 16).map(|at| machine.peek(at));
    let after = buffer::SOFT_KEYS + 17;
    let text = (after..after + text).map(|at| machine.peek(at));
    (pointers.collect(), text.collect())
}

/// The regression: the soft keys kept their text in key order, offsets
/// counted from &B11, and gathered a new definition in RS423's input buffer
/// at &A00. MOS 1.20 counts from &B01, keeps text in the order it was
/// defined with an empty key pointing at the end, reads a definition straight
/// into place, and sets the whole page to &10 to empty them.
#[test]
fn soft_keys_lie_where_mos_1_20_lays_them() {
    let with = |end: u8, keys: &[(usize, u8)]| {
        let mut pointers = alloc::vec![end; 17];
        for &(key, at) in keys {
            pointers[key] = at;
        }
        pointers
    };
    let (pointers, _) = soft_keys(&run(&[], &[]), 0);
    assert_eq!(pointers, [0x10; 17], "empty from power on");
    let defined = run(&[], &["KEY3 ABC", "KEY7 XY"]);
    assert_eq!(
        soft_keys(&defined, 5),
        (with(0x15, &[(3, 0x10), (7, 0x13)]), b"ABCXY".to_vec())
    );
    assert!(
        (0x0A00..0x0B00).all(|at| defined.peek(at) == 0),
        "&A00 untouched"
    );

    let too_long = alloc::format!("KEY3 {}", "B".repeat(240));
    let commands = ["KEY3 ABC", "KEY7 XY", too_long.as_str(), "KEY9 Q"];
    let refused = run(&[], &commands[..3]);
    assert_eq!(refused.peek(ERRORS + 2), BAD_KEY);
    assert_eq!(
        soft_keys(&refused, 2),
        (with(0x12, &[(7, 0x10)]), b"XY".to_vec()),
        "the old text gone"
    );
    let appended = run(&[], &commands);
    assert_eq!(
        soft_keys(&appended, 3),
        (with(0x13, &[(7, 0x10), (9, 0x12)]), b"XYQ".to_vec())
    );

    let reset = run(&[], &["KEY3 ABC", "FX18"]);
    assert!((buffer::SOFT_KEYS..buffer::SOFT_KEYS + 0x100).all(|at| reset.peek(at) == 0x10));

    let full = alloc::format!("KEY0 {}", "C".repeat(238));
    let overfull = alloc::format!("KEY1 {}", "C".repeat(239));
    let capacity = run(&[], &[full.as_str()]);
    assert_eq!(
        (capacity.peek(ERRORS), capacity.peek(buffer::SOFT_KEYS + 16)),
        (0, 0xFE)
    );
    assert_eq!(
        run(&[], &[overfull.as_str()]).peek(ERRORS),
        BAD_KEY,
        "239 bytes do not fit"
    );
}

/// The regression: expanding a soft key took its length from the next key's
/// pointer, which in definition order may lie anywhere.
#[test]
fn a_soft_key_expands_to_its_own_text_wherever_it_lies() {
    const READ: u16 = 0x0900;
    const STATE: u16 = 0x0910;
    let rom = program(None, |a| {
        for command in ["KEY1 ABC", "KEY0 XY", "KEY1 HELLO"] {
            oscli(a, command);
        }
        for code in [0x81u8, 0x80] {
            a.lda(imm(138));
            a.ldx(imm(0));
            a.ldy(imm(code));
            a.jsr(OSBYTE);
        }
        a.ldx(imm(0));
        a.here("read");
        a.txa();
        a.pha();
        a.jsr(OSRDCH);
        a.tay();
        a.pla();
        a.tax();
        a.tya();
        a.sta(abs(READ).x());
        a.cpx(imm(0));
        a.bne("read_on");
        a.lda(abs(page2::SOFT_KEY_POINTER));
        a.sta(abs(STATE));
        a.lda(abs(var::SOFT_KEY_LENGTH));
        a.sta(abs(STATE + 1));
        a.here("read_on");
        a.inx();
        a.cpx(imm(7));
        a.bne("read");
    });
    let machine = run_rom(&rom);
    let read: Vec<u8> = (READ..READ + 7).map(|at| machine.peek(at)).collect();
    assert_eq!(read, b"HELLOXY");
    assert_eq!(
        (machine.peek(STATE), machine.peek(STATE + 1)),
        (0x13, 4),
        "past H, four to go"
    );
    let (pointers, text) = soft_keys(&machine, 7);
    assert_eq!((pointers[0], pointers[1], pointers[16]), (0x10, 0x12, 0x17));
    assert_eq!(text, b"XYHELLO");
}

/// The regressions: a hard reset's memory clear wiped the RTI an unclaimed
/// NMI returns through; and entering a language left the error pointer at
/// nothing, where MOS 1.20 leaves it at the language's copyright, which
/// REPORT then prints.
#[test]
fn a_hard_reset_keeps_the_nmi_rti_and_a_language_finds_its_copyright() {
    let machine = run(&[], &[]);
    assert_eq!(machine.peek(page_d::NMI), 0x40, "RTI");
    let copyright = language(&[], &[])[7];
    let pointer =
        u16::from_le_bytes([machine.peek(ERROR_POINTER), machine.peek(ERROR_POINTER + 1)]);
    assert_eq!(pointer, 0x8000 + u16::from(copyright));
}

/// A program that counts each entry to vector `vector` at `counts` plus its
/// flags' C and V bits, then idles.
fn counting(vector: u16) -> Vec<u8> {
    const OLD: u16 = 0x0076;
    program(None, |a| {
        claim(a, vector, "count", OLD);
        a.jmp("counting");
        a.here("count");
        a.php();
        a.pha();
        a.txa();
        a.pha();
        a.tsx();
        a.lda(abs(STACK_PAGE + 3).x());
        a.and(imm(OVERFLOW | CARRY));
        a.tax();
        a.inc(abs(COUNTS).x());
        a.pla();
        a.tax();
        a.pla();
        a.plp();
        a.jmp(abs(OLD).ind());
        a.here("counting");
    })
}

/// Where [`counting`] counts.
const COUNTS: u16 = 0x0900;

/// The regressions: a key pressed reached the keyboard's handler without
/// KEYV; the tick entered KEYV every 10 ms however idle the keyboard; and
/// the keyboard, armed again, took a second key press from a stale flag.
#[test]
fn keyv_hears_a_press_once_and_ticks_only_while_the_keyboard_is_busy() {
    let mut machine = boot_rom(&counting(vector::KEY));
    finish(&mut machine, BUDGET);
    wait(&mut machine, 20);
    let count = |machine: &Machine, flags: u8| machine.peek(COUNTS + u16::from(flags));
    assert_eq!(count(&machine, OVERFLOW | CARRY), 0, "no tick while idle");
    machine.set_key(Key::Z, true);
    wait(&mut machine, 5);
    machine.set_key(Key::Z, false);
    wait(&mut machine, 30);
    assert_eq!(count(&machine, OVERFLOW), 1, "one key press");
    let ticks = count(&machine, OVERFLOW | CARRY);
    assert!(
        ticks >= 4,
        "the tick while Z was down and until it rearmed: {ticks}"
    );
    wait(&mut machine, 50);
    assert_eq!(
        count(&machine, OVERFLOW | CARRY),
        ticks,
        "no tick once rearmed"
    );
    assert_eq!(count(&machine, OVERFLOW), 1, "and no second press");
}

/// The regression: every tick examined each silent channel's empty queue
/// through REMV, which MOS 1.20 never calls while nothing is queued.
#[test]
fn silent_sound_channels_leave_remv_alone() {
    let mut machine = boot_rom(&counting(vector::REM));
    finish(&mut machine, BUDGET);
    // The power-on bell's note is taken through REMV as it plays.
    wait(&mut machine, 50);
    let calls = |machine: &Machine| {
        [0, CARRY, OVERFLOW, OVERFLOW | CARRY].map(|flags| machine.peek(COUNTS + u16::from(flags)))
    };
    let settled = calls(&machine);
    wait(&mut machine, 50);
    assert_eq!(calls(&machine), settled, "half a second silent");
}

/// A silent channel skipping its empty queue still hears a note queued on
/// it, and the note sounds.
#[test]
fn a_note_queued_on_a_silent_channel_sounds() {
    const BLOCK: u16 = 0x0074;
    let [block_low, block_high] = BLOCK.to_le_bytes();
    // SOUND 1, -15, 100, 40.
    let rom = program(None, |a| {
        for (offset, byte) in (0..).zip([1u8, 0, 0xF1, 0xFF, 100, 0, 40, 0]) {
            a.lda(imm(byte));
            a.sta(zp(BLOCK + offset));
        }
        a.lda(imm(7));
        a.ldx(imm(block_low));
        a.ldy(imm(block_high));
        a.jsr(OSWORD);
    });
    let mut machine = run_rom(&rom);
    machine.set_sound_output(true);
    wait(&mut machine, 50);
    let loudest = machine
        .take_audio()
        .fold(0.0f32, |most, sample| most.max(sample.abs()));
    assert!(loudest > 0.1, "the note sounds: {loudest}");
    wait(&mut machine, 300);
    machine.take_audio();
    wait(&mut machine, 10);
    let after = machine
        .take_audio()
        .fold(0.0f32, |most, sample| most.max(sample.abs()));
    assert!(after < loudest / 10.0, "and ends: {after}");
}

/// The regressions: OPENOUT's prompt kept its line, a written file's first
/// block took a line of its own, and its last ended none, where MOS 1.20
/// ends the prompt's line, shows the block straight under it and ends the
/// file with a new line; and the file's addresses were 0, not &FFFFFFFF.
#[test]
fn a_file_written_a_byte_at_a_time_shows_as_mos_1_20_shows_it() {
    const WRITE_BUDGET: u64 = 80_000_000;
    let rom = program(None, |a| {
        oscli(a, "OPT1,2");
        a.lda(imm(0x80));
        a.ldx(imm(lo("name")));
        a.ldy(imm(hi("name")));
        a.jsr(OSFIND);
        a.tay();
        a.lda(imm(b'A'));
        a.jsr(OSBPUT);
        a.lda(imm(0));
        a.jsr(OSFIND);
        a.jmp("written");
        a.here("name");
        a.text("F");
        a.byte(b'\r');
        a.here("written");
    });
    let mut machine = boot_rom(&rom);
    machine.insert_tape(Tape::default());
    machine.set_tape_record(true);
    machine.set_key(Key::RETURN, true);
    finish(&mut machine, WRITE_BUDGET);
    let text = mode7_text(&machine);
    let prompt = text
        .iter()
        .position(|line| line == "RECORD then RETURN")
        .unwrap_or_else(|| panic!("the prompt on a line of its own: {text:?}"));
    assert_eq!(text[prompt + 1], "F          00 0001    FFFFFFFF FFFFFFFF");
    let cursor = (machine.peek(vdu::CURSOR_X), machine.peek(vdu::CURSOR_Y));
    assert_eq!(
        cursor,
        (0, u8::try_from(prompt + 2).expect("a row")),
        "the file's line ended"
    );
}

/// The regression: OPENOUT on the cassette while *SPOOL held its channel
/// wrote the spooled file out, then failed with Channel. MOS 1.20 abandons
/// that file unwritten and opens the new one on the same channel, so the
/// spool carries on into it: here the new file's prompt and the byte put.
#[test]
fn openout_while_spooling_takes_over_the_cassette_channel() {
    const WRITE_BUDGET: u64 = 120_000_000;
    const HANDLE: u16 = 0x0072;
    let rom = program(None, |a| {
        oscli(a, "OPT1,2");
        oscli(a, "SPOOL S");
        a.lda(imm(0x80));
        a.ldx(imm(lo("name")));
        a.ldy(imm(hi("name")));
        a.jsr(OSFIND);
        a.sta(zp(HANDLE));
        a.tay();
        a.lda(imm(b'A'));
        a.jsr(OSBPUT);
        oscli(a, "SPOOL");
        a.jmp("written");
        a.here("name");
        a.text("G");
        a.byte(b'\r');
        a.here("written");
    });
    let mut machine = boot_rom(&rom);
    machine.insert_tape(Tape::default());
    machine.set_tape_record(true);
    machine.set_key(Key::RETURN, true);
    finish(&mut machine, WRITE_BUDGET);
    assert_eq!(machine.peek(HANDLE), 2, "the cassette's one output channel");
    let text = mode7_text(&machine);
    // "RECORD then RETURN", its new line and the A.
    assert!(
        text.iter()
            .any(|line| line == "G          00 0015    FFFFFFFF FFFFFFFF"),
        "G written: {text:?}"
    );
    assert!(
        !text.iter().any(|line| line.starts_with("S ")),
        "S abandoned: {text:?}"
    );
}

/// The SN76489's writes once `body` queues notes, after a second for the
/// boot to fall quiet: each as centiseconds from the first and the byte, a
/// write that repeats a channel's attenuation left out.
fn chip_writes(body: impl FnOnce(&mut Asm), centiseconds: u64) -> Vec<(u64, u8)> {
    use crate::machine::bbc::sheila::SYSTEM_VIA;
    use crate::machine::bbc::via::reg;
    const READY: u16 = 0x0072;
    const FRAMES: u16 = 0x0073;
    const CENTISECOND: u64 = 20_000;
    let rom = program(None, |a| {
        a.lda(imm(50));
        a.sta(zp(FRAMES));
        let frame = a.here_local();
        a.lda(imm(19));
        a.jsr(OSBYTE);
        a.dec(zp(FRAMES));
        a.bne(frame);
        a.lda(imm(1));
        a.sta(zp(READY));
        body(a);
    });
    let mut machine = boot_rom(&rom);
    while machine.peek(READY) != 1 {
        assert!(machine.cycles() < BUDGET, "the boot falls quiet");
        machine.run_until(machine.cycles() + 1_000);
    }
    machine.monitor_io(true);
    let mut accesses = Vec::new();
    for _ in 0..centiseconds {
        machine.run_until(machine.cycles() + CENTISECOND);
        accesses.extend(machine.take_io_trace());
    }
    let (mut data, mut writes, mut attenuations) = (None, Vec::new(), [None; 4]);
    for access in accesses.iter().filter(|access| access.write) {
        let Ok(register) = u8::try_from(access.addr.wrapping_sub(SYSTEM_VIA)) else {
            continue;
        };
        match register {
            reg::ORA | reg::ORA_NH => data = Some(access.value),
            reg::ORB if access.value == 0 => {
                let Some(byte) = data else { continue };
                if byte & 0x90 == 0x90 {
                    let channel = usize::from((byte >> 5) & 3);
                    if attenuations[channel] == Some(byte) {
                        continue;
                    }
                    attenuations[channel] = Some(byte);
                }
                writes.push((access.cycle, byte));
            }
            _ => {}
        }
    }
    let first = writes.first().map_or(0, |&(cycle, _)| cycle);
    writes
        .into_iter()
        .map(|(cycle, byte)| ((cycle - first + CENTISECOND / 2) / CENTISECOND, byte))
        .collect()
}

/// `centiseconds:byte` pairs, as the reference observations were recorded.
fn recorded_writes(text: &str) -> Vec<(u64, u8)> {
    text.split_whitespace()
        .map(|pair| {
            let (time, byte) = pair.split_once(':').expect("time:byte");
            (
                time.parse().expect("centiseconds"),
                u8::from_str_radix(byte, 16).expect("a hex byte"),
            )
        })
        .collect()
}

/// OSWORD `call` on a parameter block of `bytes` at &74.
fn osword_with(a: &mut Asm, call: u8, bytes: &[u8]) {
    const BLOCK: u16 = 0x0074;
    let [low, high] = BLOCK.to_le_bytes();
    for (offset, &byte) in (0..).zip(bytes) {
        a.lda(imm(byte));
        a.sta(zp(BLOCK + offset));
    }
    a.lda(imm(call));
    a.ldx(imm(low));
    a.ldy(imm(high));
    a.jsr(OSWORD);
}

/// SOUND channel, amplitude, pitch, duration through OSWORD 7.
fn sound(a: &mut Asm, channel: u16, amplitude: i16, pitch: u8, duration: u8) {
    let [channel_low, channel_high] = channel.to_le_bytes();
    let [amplitude_low, amplitude_high] = amplitude.to_le_bytes();
    osword_with(
        a,
        7,
        &[
            channel_low,
            channel_high,
            amplitude_low,
            amplitude_high,
            pitch,
            0,
            duration,
            0,
        ],
    );
}

/// ENVELOPE's fourteen parameters through OSWORD 8.
fn envelope(a: &mut Asm, parameters: [i8; 14]) {
    osword_with(a, 8, &parameters.map(i8::cast_unsigned));
}

/// The regressions: durations counted on one clock for every channel, so a
/// note's length hung on when it started; an envelope's first step a
/// centisecond late; attack and decay ending on their targets, not once past
/// them; pitch sections passed over within a step and repeated from where
/// they ended; an envelope's attack starting from silence however the
/// channel sounded; and a held note's release missing a step as it took
/// over. Each scene's writes are MOS 1.20's, as recorded.
#[test]
fn notes_and_envelopes_drive_the_chip_as_mos_1_20_does() {
    type Scene = (fn(&mut Asm), &'static str);
    let scenes: [Scene; 6] = [
        (
            |a| {
                sound(a, 1, -15, 100, 1);
                sound(a, 1, -10, 120, 1);
                sound(a, 1, -5, 140, 1);
            },
            "0:D0 0:CD 0:0E 4:D5 4:C2 4:0B 9:DA 9:C5 9:08 14:DF",
        ),
        (
            |a| {
                envelope(a, [1, 2, 4, -4, 0, 3, 3, 0, 126, -20, 0, -30, 126, 0]);
                sound(a, 1, 1, 100, 3);
            },
            "0:CD 0:0E 0:D0 0:C0 0:0E 2:C3 2:0D 4:D2 4:C8 4:0C 6:D5 6:C3 6:0D \
             8:D7 8:C0 8:0E 10:DA 10:CD 10:0E 12:DC 14:DF 14:C0 14:0E",
        ),
        (
            |a| {
                envelope(a, [1, 1, 0, 0, 0, 0, 0, 0, 30, -7, 0, 0, 100, 50]);
                sound(a, 1, 1, 100, 3);
            },
            "0:CD 0:0E 0:DC 1:D8 2:D4 3:D3 4:D4 5:D5 7:D6 8:D7 9:D8 10:D9",
        ),
        (
            |a| {
                envelope(a, [1, 2, 1, 0, 0, 2, 0, 0, 0, 10, -5, -5, 126, 60]);
                sound(a, 1, 1, 100, 4);
            },
            "0:CD 0:0E 0:D0 0:CA 0:0E 2:D8 2:C7 2:0E 6:D9 8:DA 8:CA 8:0E 10:C7 \
             10:0E 12:DB 14:DC 16:CA 16:0E 18:DD 18:C7 18:0E 22:DE 24:DF 24:CA \
             24:0E 26:C7 26:0E",
        ),
        (
            |a| {
                envelope(a, [1, 1, 0, 0, 0, 0, 0, 0, 126, -5, -5, -5, 126, 100]);
                sound(a, 1, 1, 100, 1);
                sound(a, 0x1001, 1, 120, 4);
                sound(a, 1, -10, 140, 1);
            },
            "0:CD 0:0E 0:D0 3:D1 5:D2 6:D3 8:D4 9:D5 11:D6 13:D7 14:D8 16:D9 \
             17:DA 19:DB 21:DC 22:DD 24:D5 24:C5 24:08 29:DF",
        ),
        (
            |a| {
                envelope(a, [1, 4, 0, 0, 0, 0, 0, 0, 63, -21, 0, -9, 126, 60]);
                sound(a, 1, 1, 100, 6);
                sound(a, 1, 1, 120, 6);
            },
            "0:CD 0:0E 0:D8 4:D0 12:D2 16:D5 20:D7 24:D8 29:C2 29:0B 29:D0 \
             37:D2 41:D5 45:D7 49:D8 61:D9 65:DA 69:DB 73:DC 77:DD 81:DF",
        ),
    ];
    for (index, (body, recorded)) in scenes.into_iter().enumerate() {
        assert_eq!(
            chip_writes(body, 150),
            recorded_writes(recorded),
            "scene {index}"
        );
    }
}

/// The regression: starting a synchronised note with an envelope overwrote
/// the S the pass was starting, so the notes waiting with it on the lower
/// channels never started, and their queues filled until SOUND waited for
/// ever.
#[test]
fn synchronised_notes_start_together_behind_an_envelope() {
    let writes = chip_writes(
        |a| {
            envelope(a, [1, 1, 0, 0, 0, 0, 0, 0, 126, 0, 0, -126, 126, 126]);
            sound(a, 0x0201, -15, 100, 2);
            sound(a, 0x0202, -15, 120, 2);
            sound(a, 0x0203, 1, 140, 2);
        },
        20,
    );
    let volume_written = |channel: u8| {
        let volume = 0x90 | (3 - channel) << 5;
        writes
            .iter()
            .find(|&&(_, byte)| byte & 0xF0 == volume)
            .map(|&(time, _)| time)
    };
    assert_eq!([1, 2, 3].map(volume_written), [Some(0); 3], "{writes:?}");
}
