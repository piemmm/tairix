use alloc::vec::Vec;

use super::crtc::{reg, Character, Crtc};

/// Ten characters a line, six displayed, a two-character HSYNC at 7; four
/// rows of two lines, two displayed, a two-line VSYNC at row 3.
const SMALL: [(usize, u8); 12] = [
    (reg::H_TOTAL, 9),
    (reg::H_DISPLAYED, 6),
    (reg::H_SYNC_POS, 7),
    (reg::SYNC_WIDTH, 0x22),
    (reg::V_TOTAL, 3),
    (reg::V_ADJUST, 0),
    (reg::V_DISPLAYED, 2),
    (reg::V_SYNC_POS, 3),
    (reg::MODE, 0),
    (reg::MAX_RASTER, 1),
    (reg::START_HIGH, 0x01),
    (reg::START_LOW, 0x00),
];
const LINE: usize = 10;
const FRAME_LINES: usize = 8;

fn programmed(registers: &[(usize, u8)]) -> Crtc {
    let mut crtc = Crtc::new();
    for &(register, value) in registers {
        crtc.select(u8::try_from(register).unwrap());
        crtc.write(value);
    }
    crtc
}

/// Skip the frame the chip powers up into, whose rows start at address 0,
/// and return the lines of the next.
fn next_frame(crtc: &mut Crtc, lines: usize, line: usize) -> Vec<Vec<Character>> {
    for _ in 0..lines * line {
        crtc.clock();
    }
    (0..lines)
        .map(|_| (0..line).map(|_| crtc.clock()).collect())
        .collect()
}

#[test]
fn a_line_is_the_total_plus_one_with_display_then_sync_where_programmed() {
    let mut crtc = programmed(&SMALL);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    let first = &frame[0];
    let display: Vec<bool> = first.iter().map(|c| c.display).collect();
    assert_eq!(
        display,
        [true, true, true, true, true, true, false, false, false, false]
    );
    let hsync: Vec<bool> = first.iter().map(|c| c.hsync).collect();
    assert_eq!(
        hsync,
        [false, false, false, false, false, false, false, true, true, false]
    );
}

#[test]
fn rows_count_their_lines_and_start_a_row_on_from_the_last() {
    let mut crtc = programmed(&SMALL);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    let starts: Vec<(u16, u8)> = frame
        .iter()
        .map(|line| (line[0].address, line[0].raster))
        .collect();
    assert_eq!(
        starts,
        [
            (0x100, 0),
            (0x100, 1),
            (0x106, 0),
            (0x106, 1),
            (0x10C, 0),
            (0x10C, 1),
            (0x112, 0),
            (0x112, 1)
        ]
    );
    // Two rows are displayed.
    let displayed = frame.iter().filter(|line| line[0].display).count();
    assert_eq!(displayed, 4);
}

#[test]
fn vsync_runs_from_its_row_for_the_programmed_lines_and_zero_means_sixteen() {
    let mut crtc = programmed(&SMALL);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    let vsync: Vec<bool> = frame.iter().map(|line| line[0].vsync).collect();
    assert_eq!(
        vsync,
        [false, false, false, false, false, false, true, true]
    );

    let mut tall = SMALL;
    tall[3] = (reg::SYNC_WIDTH, 0x02);
    tall[4] = (reg::V_TOTAL, 9);
    let mut crtc = programmed(&tall);
    let lines = next_frame(&mut crtc, 3 * 20, LINE);
    let mut longest = 0;
    let mut run = 0;
    for line in &lines {
        run = if line[0].vsync { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    assert_eq!(longest, 16);
}

#[test]
fn the_vertical_adjust_adds_lines_after_the_last_row() {
    let mut adjusted = SMALL;
    adjusted[5] = (reg::V_ADJUST, 3);
    let mut crtc = programmed(&adjusted);
    let frame = next_frame(&mut crtc, FRAME_LINES + 3, LINE);
    let rasters: Vec<u8> = frame[FRAME_LINES..]
        .iter()
        .map(|line| line[0].raster)
        .collect();
    assert_eq!(rasters, [0, 1, 2]);
    assert!(frame[FRAME_LINES..].iter().all(|line| !line[0].display));
}

#[test]
fn display_skew_delays_display_enable_by_whole_characters() {
    let mut skewed = SMALL;
    skewed[8] = (reg::MODE, 0x10);
    let mut crtc = programmed(&skewed);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    let display: Vec<bool> = frame[1].iter().map(|c| c.display).collect();
    assert_eq!(
        display,
        [false, true, true, true, true, true, true, false, false, false]
    );
}

#[test]
fn the_cursor_covers_its_address_on_its_lines_and_blinks_as_programmed() {
    let mut with_cursor = SMALL.to_vec();
    with_cursor.extend([
        (reg::CURSOR_HIGH, 0x01),
        (reg::CURSOR_LOW, 0x08),
        (reg::CURSOR_START, 0x01),
        (reg::CURSOR_END, 0x01),
    ]);
    let mut crtc = programmed(&with_cursor);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    let cursor: Vec<(usize, usize)> = frame
        .iter()
        .enumerate()
        .flat_map(|(line, chars)| {
            chars
                .iter()
                .enumerate()
                .filter(|(_, c)| c.cursor)
                .map(move |(at, _)| (line, at))
        })
        .collect();
    // Address &108 is the third character of row 1; only its line 1 shows it.
    assert_eq!(cursor, [(3, 2)]);

    with_cursor[14] = (reg::CURSOR_START, 0x21);
    let mut crtc = programmed(&with_cursor);
    let frame = next_frame(&mut crtc, FRAME_LINES, LINE);
    assert!(
        frame.iter().flatten().all(|c| !c.cursor),
        "mode 1 hides the cursor"
    );
}

#[test]
fn interlaced_sync_starts_the_odd_field_s_vsync_half_a_line_late() {
    let mut interlaced = SMALL;
    interlaced[8] = (reg::MODE, 0x01);
    let mut crtc = programmed(&interlaced);
    // Find VSYNC's start in two successive fields, and whether each said so.
    let mut starts = Vec::new();
    let mut was = false;
    for _ in 0..4 * FRAME_LINES * LINE {
        let hc = crtc.next_character();
        let c = crtc.clock();
        if c.vsync && !was {
            starts.push((hc, crtc.vsync_late()));
        }
        was = c.vsync;
    }
    // Half of the ten-character line on: the pulse first shows at 5.
    assert_eq!(&starts[..2], [(0, false), (5, true)]);
}

#[test]
fn interlaced_video_steps_the_scan_line_by_two_from_the_field_s_parity() {
    let mut video = SMALL;
    video[8] = (reg::MODE, 0x03);
    video[9] = (reg::MAX_RASTER, 3);
    let mut crtc = programmed(&video);
    let mut rasters = Vec::new();
    for _ in 0..2 * FRAME_LINES * LINE * 2 {
        let c = crtc.clock();
        if crtc.next_character() == 1 {
            rasters.push(c.raster);
        }
    }
    let odd = crtc.odd_field();
    assert!(
        rasters.windows(2).any(|pair| pair == [0, 2])
            && rasters.windows(2).any(|pair| pair == [1, 3]),
        "{rasters:?} odd {odd}"
    );
}

#[test]
fn only_the_cursor_and_light_pen_registers_read_back_and_the_pen_s_are_read_only() {
    let mut crtc = programmed(&SMALL);
    for register in [reg::H_TOTAL, reg::START_LOW] {
        crtc.select(u8::try_from(register).unwrap());
        assert_eq!(crtc.read(), 0);
    }
    crtc.select(14);
    crtc.write(0xFF);
    assert_eq!(crtc.read(), 0x3F, "fourteen address bits");
    crtc.select(16);
    crtc.write(0x12);
    assert_eq!(crtc.read(), 0);
    for _ in 0..3 {
        crtc.clock();
    }
    crtc.strobe_light_pen();
    let latched = u16::from_be_bytes([crtc.register(reg::PEN_HIGH), crtc.register(reg::PEN_LOW)]);
    assert_eq!(latched, 3);
}
