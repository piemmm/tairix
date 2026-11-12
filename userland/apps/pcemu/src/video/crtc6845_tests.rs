use super::*;
use crate::bios::modes::CRTC_80 as CGA_80X25;
use alloc::vec::Vec;

fn programmed(values: &[u8]) -> Crtc {
    let mut crtc = Crtc::new();
    for (index, &value) in (0u8..).zip(values) {
        crtc.select(index);
        crtc.write(value);
    }
    crtc
}

/// Steps until a new frame begins: the next clock is its first.
fn to_frame(crtc: &mut Crtc) {
    let fields = crtc.fields;
    while crtc.fields == fields {
        crtc.step();
    }
}

#[test]
fn the_cga_frame_is_262_lines_of_114_clocks() {
    let mut crtc = programmed(&CGA_80X25);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..114 * 262).map(|_| crtc.step()).collect();
    assert_eq!(ticks.iter().filter(|t| t.vsync).count(), 1);
    let vsync = ticks.iter().position(|t| t.vsync).unwrap();
    assert_eq!(
        vsync,
        224 * 114,
        "vertical sync starts at row 28's first line"
    );
    assert_eq!(ticks.iter().filter(|t| t.hsync).count(), 262);
    assert!(ticks[90].hsync && ticks[90].in_hsync && ticks[99].in_hsync && !ticks[100].in_hsync);
    assert_eq!(ticks.iter().filter(|t| t.display).count(), 80 * 200);
    assert_eq!(ticks.iter().filter(|t| t.in_vsync).count(), 16 * 114);
}

#[test]
fn each_row_starts_where_the_last_left_off() {
    let mut crtc = programmed(&CGA_80X25);
    crtc.select(12);
    crtc.write(0x01);
    crtc.select(13);
    crtc.write(0x23);
    to_frame(&mut crtc);
    let ticks: Vec<Tick> = (0..114 * 200).map(|_| crtc.step()).collect();
    for row in 0..25 {
        for line in 0..8 {
            let tick = ticks[(row * 8 + line) * 114];
            assert_eq!(
                (tick.ma, tick.ra),
                (
                    0x0123 + 80 * u16::try_from(row).unwrap(),
                    u8::try_from(line).unwrap()
                )
            );
        }
    }
}

#[test]
fn the_start_address_takes_effect_at_the_next_frame() {
    let mut crtc = programmed(&CGA_80X25);
    to_frame(&mut crtc);
    crtc.select(13);
    crtc.write(0x40);
    assert_eq!(crtc.step().ma, 0, "this frame keeps its start");
    to_frame(&mut crtc);
    assert_eq!(crtc.step().ma, 0x40);
}

#[test]
fn the_cursor_shows_on_its_lines_and_can_be_split_or_hidden() {
    let mut crtc = programmed(&CGA_80X25);
    crtc.select(15);
    crtc.write(5);
    to_frame(&mut crtc);
    let lit: Vec<u8> = (0..114 * 8)
        .map(|_| crtc.step())
        .filter(|t| t.cursor)
        .map(|t| t.ra)
        .collect();
    assert_eq!(lit, [6, 7], "BIOS cursor lines 6 and 7");
    crtc.select(10);
    crtc.write(0x07);
    crtc.select(11);
    crtc.write(0x01);
    to_frame(&mut crtc);
    let lit: Vec<u8> = (0..114 * 8)
        .map(|_| crtc.step())
        .filter(|t| t.cursor)
        .map(|t| t.ra)
        .collect();
    assert_eq!(lit, [0, 1, 7], "a start below the end splits the cursor");
    crtc.select(10);
    crtc.write(0x26);
    to_frame(&mut crtc);
    assert!(
        !(0..114 * 8).any(|_| crtc.step().cursor),
        "mode 01 hides it"
    );
}

#[test]
fn only_the_cursor_and_light_pen_read_back() {
    let mut crtc = programmed(&CGA_80X25);
    crtc.select(0);
    assert_eq!(crtc.read(), 0);
    crtc.select(14);
    crtc.write(0xFF);
    assert_eq!(crtc.read(), 0x3F, "fourteen address bits");
    to_frame(&mut crtc);
    for _ in 0..114 * 8 * 3 + 5 {
        crtc.step();
    }
    crtc.strobe_light_pen();
    crtc.select(17);
    assert_eq!(crtc.read(), 80 * 3 + 5);
}

#[test]
fn vertical_total_adjust_adds_its_lines() {
    let mut crtc = programmed(&[0x10, 0x08, 0x0A, 0x02, 0x03, 0x05, 0x02, 0x03, 0x00, 0x03]);
    while !crtc.step().vsync {}
    let mut period = 1;
    while !crtc.step().vsync {
        period += 1;
    }
    assert_eq!(
        period,
        17 * (4 * 4 + 5),
        "four rows of four lines, five adjust lines"
    );
}

#[test]
fn a_total_reprogrammed_below_the_counter_is_met_after_it_wraps() {
    let mut crtc = programmed(&CGA_80X25);
    to_frame(&mut crtc);
    for _ in 0..0x60 {
        crtc.step();
    }
    crtc.select(0);
    crtc.write(0x38);
    let mut rest = 0;
    while crtc.step().ra == 0 {
        rest += 1;
    }
    assert_eq!(
        rest,
        0x100 - 0x60 + 0x39,
        "on to 255, round, and up to the new total"
    );
    let mut line = 1;
    while crtc.step().ra == 1 {
        line += 1;
    }
    assert_eq!(line, 0x39);
}

/// The address of every character clock displayed on the first line of each
/// row, top first, as a whole frame scans them.
fn scanned_rows(crtc: &mut Crtc) -> Vec<Vec<u16>> {
    to_frame(crtc);
    let fields = crtc.fields;
    let mut rows: Vec<Vec<u16>> = Vec::new();
    let mut line = Vec::new();
    while crtc.fields == fields {
        let tick = crtc.step();
        if tick.display && tick.ra == 0 {
            line.push(tick.ma);
        }
        if crtc.hc == 0 && !line.is_empty() {
            rows.push(core::mem::take(&mut line));
        }
    }
    rows
}

#[test]
fn each_cells_address_is_the_one_the_scan_reads_it_from() {
    let mut past_total = CGA_80X25;
    past_total[1] = 120;
    for (values, start) in [
        (CGA_80X25, 0x0123u16),
        (CGA_80X25, 0x3FC0),
        (past_total, 0x0010),
    ] {
        let mut crtc = programmed(&values);
        let [high, low] = start.to_be_bytes();
        crtc.select(12);
        crtc.write(high);
        crtc.select(13);
        crtc.write(low);
        let (columns, rows) = crtc.text_shape();
        let read: Vec<Vec<u16>> = (0..rows)
            .map(|row| {
                (0..columns)
                    .map(|column| crtc.cell_address(row, column))
                    .collect()
            })
            .collect();
        assert_eq!(read, scanned_rows(&mut crtc), "start {start:#06X}");
    }
    assert_eq!(
        programmed(&past_total).text_shape(),
        (114, 25),
        "a row ends at the total"
    );
}
