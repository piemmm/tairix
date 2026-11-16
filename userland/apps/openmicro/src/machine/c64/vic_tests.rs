use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// The chip's 16 KiB and colour RAM, flat.
struct Memory {
    bytes: Vec<u8>,
    colour: Vec<u8>,
}

impl Memory {
    fn new() -> Self {
        Self {
            bytes: vec![0; 0x4000],
            colour: vec![0; 0x400],
        }
    }
}

impl VicMemory for Memory {
    fn fetch(&self, addr: u16) -> u8 {
        self.bytes[usize::from(addr & 0x3FFF)]
    }

    fn colour(&self, addr: u16) -> u8 {
        self.colour[usize::from(addr & 0x3FF)]
    }
}

/// One cycle: the beam's place in its first half and BA there.
fn step(vic: &mut Vic, memory: &Memory) -> ((u16, u16), bool) {
    vic.begin_cycle(memory);
    let seen = (vic.beam(), vic.ba());
    vic.end_cycle(memory, 0xFF);
    seen
}

fn run(vic: &mut Vic, memory: &Memory, cycles: u64) {
    for _ in 0..cycles {
        step(vic, memory);
    }
}

const PAL_FRAME: u64 = 312 * 63;

/// A PAL screen as the KERNAL leaves it: 25 rows of 40 columns, the matrix
/// at $0400, the characters at $2000, colours as given.
fn pal_screen(border: u8, background: u8) -> Vic {
    let mut vic = Vic::new(Model::Mos6569);
    vic.write(reg::CR1, cr1::DEN | cr1::RSEL | 3);
    vic.write(reg::CR2, cr2::CSEL);
    vic.write(reg::MEMORY, 0x18);
    vic.write(reg::BORDER, border);
    vic.write(reg::BACKGROUND, background);
    vic
}

/// The canvas slot showing PAL pixel `x` of `line`.
fn pal_slot(vic: &Vic, line: u16, x: u16) -> u8 {
    let beam = (x + 504 - 404) % 504;
    let row = 2 * (usize::from(line) - 16 + 8);
    let column = 43 + 2 * (usize::from(beam) - 76);
    vic.shown()[row * CANVAS_WIDTH + column]
}

#[test]
fn the_pal_table_places_each_access_as_the_chip_does() {
    let t = &PAL;
    assert_eq!(
        (t[58].first, t[58].second),
        (First::Pointer(0), Second::Sprite(0))
    );
    assert_eq!(
        (t[59].first, t[59].second),
        (First::Sprite(0), Second::Sprite(0))
    );
    assert_eq!(t[1].first, First::Pointer(3));
    assert_eq!(t[10].first, First::Sprite(7));
    assert!((11..=15).all(|c| t[c].first == First::Refresh));
    assert!((16..=55).all(|c| t[c].first == First::Graphics));
    assert!((15..=54).all(|c| t[c].second == Second::Matrix));
    assert_eq!((t[56].first, t[57].first), (First::Idle, First::Idle));
    assert!((12..=54).all(|c| t[c].fetch_ba) && !t[11].fetch_ba && !t[55].fetch_ba);
    let ba: Vec<u8> = [54, 55, 57, 59, 63, 1, 2, 10, 11]
        .iter()
        .map(|&c| t[c].sprites_ba)
        .collect();
    assert_eq!(
        ba,
        [
            0,
            0b1,
            0b11,
            0b111,
            0b1_1100,
            0b1_1000,
            0b11_1000,
            0b1000_0000,
            0
        ]
    );
    let flagged = |f: u8| {
        (1..=63)
            .filter(|&c| t[c].flags & f != 0)
            .collect::<Vec<_>>()
    };
    assert_eq!(flagged(flag::SPRITE_DMA), [55, 56]);
    assert_eq!(flagged(flag::SPRITE_DISPLAY), [58]);
    assert_eq!(flagged(flag::UPDATE_RC), [58]);
}

#[test]
fn the_ntsc_table_reads_the_sprites_a_cycle_later() {
    let t = &NTSC;
    let pointers: Vec<usize> = (0..8)
        .map(|n| {
            (1..=65)
                .find(|&c| t[c].first == First::Pointer(n))
                .expect("read")
        })
        .collect();
    assert_eq!(pointers, [59, 61, 63, 65, 2, 4, 6, 8]);
    assert_eq!(t[1].first, First::Sprite(3));
    assert!([10, 56, 57, 58].iter().all(|&c| t[c].first == First::Idle));
    let ba: Vec<u8> = [55, 56, 58, 60, 1, 9, 10]
        .iter()
        .map(|&c| t[c].sprites_ba)
        .collect();
    assert_eq!(ba, [0, 0b1, 0b11, 0b111, 0b11_1000, 0b1000_0000, 0]);
    let flagged = |f: u8| {
        (1..=65)
            .filter(|&c| t[c].flags & f != 0)
            .collect::<Vec<_>>()
    };
    assert_eq!(flagged(flag::SPRITE_DMA), [56, 57]);
    assert_eq!(flagged(flag::SPRITE_DISPLAY), [59]);
}

#[test]
fn the_x_coordinates_run_from_cycle_one_as_each_chip_counts_them() {
    let pal = Model::Mos6569;
    assert_eq!(pal.half_x(0), 0x194);
    assert_eq!((pal.half_x(24), pal.half_x(25)), (0x1F4, 0x000));
    assert_eq!(pal.half_x(124), 0x18C);
    let ntsc = Model::Mos6567R8;
    assert_eq!(ntsc.half_x(0), 0x19C);
    let held: Vec<u16> = (121..=129).map(|h| ntsc.half_x(h)).collect();
    assert_eq!(
        held,
        [0x180, 0x184, 0x184, 0x184, 0x188, 0x18C, 0x190, 0x194, 0x198]
    );
}

#[test]
fn a_bad_line_holds_ba_low_from_cycle_12_to_54() {
    let mut vic = pal_screen(14, 6);
    let memory = Memory::new();
    let mut held = Vec::new();
    for _ in 0..PAL_FRAME {
        let ((line, cycle), ba) = step(&mut vic, &memory);
        if ba && (0x33..=0x34).contains(&line) {
            held.push((line, cycle));
        }
    }
    let expected: Vec<(u16, u16)> = (12..=54).map(|cycle| (0x33, cycle)).collect();
    assert_eq!(held, expected);
}

#[test]
fn standard_text_draws_each_character_in_its_colour() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    memory.bytes[0x0400] = 1;
    memory.bytes[0x2008] = 0b1000_0001;
    memory.colour[0] = 2;
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 51, 23), 14, "border");
    assert_eq!(pal_slot(&vic, 51, 24), 2, "the glyph's first pixel");
    assert_eq!(pal_slot(&vic, 51, 25), 6, "background");
    assert_eq!(pal_slot(&vic, 51, 31), 2, "the glyph's last pixel");
    assert_eq!(pal_slot(&vic, 52, 24), 6, "the glyph's second row is clear");
    assert_eq!(pal_slot(&vic, 51, 343), 6, "the last column");
    assert_eq!(pal_slot(&vic, 51, 344), 14, "the right border");
    assert_eq!(pal_slot(&vic, 50, 100), 14, "the top border");
    assert_eq!(pal_slot(&vic, 251, 100), 14, "the bottom border");
}

#[test]
fn the_horizontal_scroll_moves_the_characters_right() {
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR2, cr2::CSEL | 3);
    let mut memory = Memory::new();
    memory.bytes[0x0400] = 1;
    memory.bytes[0x2008] = 0b1000_0000;
    memory.colour[0] = 2;
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 51, 24), 6);
    assert_eq!(pal_slot(&vic, 51, 27), 2);
}

#[test]
fn thirty_eight_columns_and_24_rows_narrow_the_window() {
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR1, cr1::DEN | 3);
    vic.write(reg::CR2, 0);
    let memory = Memory::new();
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 60, 30), 14);
    assert_eq!(pal_slot(&vic, 60, 31), 6);
    assert_eq!(pal_slot(&vic, 60, 334), 6);
    assert_eq!(pal_slot(&vic, 60, 335), 14);
    assert_eq!(pal_slot(&vic, 54, 100), 14);
    assert_eq!(pal_slot(&vic, 55, 100), 6);
    assert_eq!(pal_slot(&vic, 246, 100), 6);
    assert_eq!(pal_slot(&vic, 247, 100), 14);
}

#[test]
fn without_den_the_border_covers_the_screen() {
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR1, cr1::RSEL | 3);
    let memory = Memory::new();
    let mut ba = false;
    for _ in 0..PAL_FRAME {
        ba |= step(&mut vic, &memory).1;
    }
    assert!(!ba, "no bad lines");
    assert_eq!(pal_slot(&vic, 100, 100), 14);
}

#[test]
fn idle_state_draws_the_bank_s_last_byte() {
    // With no vertical scroll the last row ends at line $F7: lines $F8 to
    // $FA are idle inside the window.
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR1, cr1::DEN | cr1::RSEL);
    let mut memory = Memory::new();
    memory.bytes[0x3FFF] = 0b1111_0000;
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 0xF9, 24), 0, "set bits are black");
    assert_eq!(pal_slot(&vic, 0xF9, 28), 6, "clear bits the background");
    assert_eq!(pal_slot(&vic, 0xF7, 24), 6, "the last row, from the matrix");
}

#[test]
fn bitmap_mode_takes_its_colours_from_the_matrix() {
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR1, cr1::DEN | cr1::RSEL | cr1::BMM | 3);
    let mut memory = Memory::new();
    memory.bytes[0x0400] = 0x25;
    memory.bytes[0x2000] = 0b1000_0000;
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 51, 24), 2, "set: the matrix's high nybble");
    assert_eq!(pal_slot(&vic, 51, 25), 5, "clear: its low nybble");
}

#[test]
fn multicolour_text_pairs_its_bits() {
    let mut vic = pal_screen(14, 6);
    vic.write(reg::CR2, cr2::CSEL | cr2::MCM);
    vic.write(reg::BACKGROUND + 1, 3);
    vic.write(reg::BACKGROUND + 2, 4);
    let mut memory = Memory::new();
    memory.bytes[0x0400] = 1;
    memory.bytes[0x2008] = 0b00_01_10_11;
    memory.colour[0] = 0x08 | 5;
    run(&mut vic, &memory, PAL_FRAME);
    let pixels: Vec<u8> = (24..32).map(|x| pal_slot(&vic, 51, x)).collect();
    assert_eq!(pixels, [6, 6, 3, 3, 4, 4, 5, 5]);
}

#[test]
fn the_raster_interrupt_fires_as_its_line_begins() {
    let mut vic = Vic::new(Model::Mos6569);
    vic.write(reg::RASTER, 100);
    vic.write(reg::IRQ_ENABLE, irq::RASTER);
    let memory = Memory::new();
    let fired = loop {
        let (beam, _) = step(&mut vic, &memory);
        if vic.irq() {
            break beam;
        }
    };
    assert_eq!(fired, (100, 1));
    assert_eq!(vic.read(reg::IRQ), irq::ANY | 0x70 | irq::RASTER);
    vic.write(reg::IRQ, irq::RASTER);
    assert!(!vic.irq());
    vic.write(reg::RASTER, 100);
    assert!(!vic.irq(), "once a line");
}

#[test]
fn line_0_s_raster_interrupt_comes_a_cycle_late() {
    let mut vic = Vic::new(Model::Mos6569);
    let memory = Memory::new();
    run(&mut vic, &memory, 100);
    vic.write(reg::CR1, 0);
    vic.write(reg::RASTER, 0);
    assert_eq!(
        vic.read(reg::IRQ) & irq::RASTER,
        irq::RASTER,
        "power-on's line 0 matched"
    );
    vic.write(reg::IRQ, irq::CAUSES);
    vic.write(reg::IRQ_ENABLE, irq::RASTER);
    let fired = loop {
        let (beam, _) = step(&mut vic, &memory);
        if vic.irq() {
            break beam;
        }
    };
    assert_eq!(fired, (0, 2));
}

/// Sprite 0 at (`x`, `y`), enabled, coloured 1, each of its rows `row`.
fn with_sprite(vic: &mut Vic, memory: &mut Memory, x: u16, y: u8, row: [u8; 3]) {
    let [low, high] = x.to_le_bytes();
    vic.write(reg::SPRITE_X, low);
    vic.write(reg::SPRITE_X_HIGH, high);
    vic.write(reg::SPRITE_Y, y);
    vic.write(reg::SPRITE_COLOUR, 1);
    vic.write(reg::SPRITE_ENABLE, 0x01);
    memory.bytes[0x07F8] = 0x80;
    for line in memory.bytes[0x2000..0x203F].as_chunks_mut::<3>().0 {
        line.copy_from_slice(&row);
    }
}

#[test]
fn a_sprite_s_dma_holds_ba_low_from_cycle_55() {
    let mut vic = Vic::new(Model::Mos6569);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 100, 100, [0xFF; 3]);
    let mut held = Vec::new();
    for _ in 0..PAL_FRAME {
        let ((line, cycle), ba) = step(&mut vic, &memory);
        if ba && (99..=101).contains(&line) {
            held.push((line, cycle));
        }
    }
    let line = |l| (55..=59).map(move |c| (l, c));
    assert_eq!(held, line(100).chain(line(101)).collect::<Vec<_>>());
}

#[test]
fn a_sprite_shows_from_the_line_after_its_y() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 100, 60, [0b1000_0001, 0, 0x01]);
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 60, 100), 6);
    assert_eq!(pal_slot(&vic, 61, 100), 1);
    assert_eq!(pal_slot(&vic, 61, 101), 6);
    assert_eq!(pal_slot(&vic, 61, 107), 1);
    assert_eq!(pal_slot(&vic, 61, 123), 1, "the third byte's last bit");
    assert_eq!(pal_slot(&vic, 61, 124), 6);
    assert_eq!(pal_slot(&vic, 81, 100), 1, "21 lines");
    assert_eq!(pal_slot(&vic, 82, 100), 6);
}

#[test]
fn an_expanded_sprite_doubles_each_pixel_and_line() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 100, 60, [0b1000_0000, 0, 0]);
    vic.write(reg::SPRITE_EXPAND_X, 0x01);
    vic.write(reg::SPRITE_EXPAND_Y, 0x01);
    run(&mut vic, &memory, PAL_FRAME);
    let pixels: Vec<u8> = (100..103).map(|x| pal_slot(&vic, 61, x)).collect();
    assert_eq!(pixels, [1, 1, 6]);
    assert_eq!(pal_slot(&vic, 102, 100), 1, "42 lines");
    assert_eq!(pal_slot(&vic, 103, 100), 6);
}

#[test]
fn a_sprite_behind_the_foreground_shows_only_through_the_background() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 24, 50, [0xFF, 0, 0]);
    vic.write(reg::SPRITE_PRIORITY, 0x01);
    memory.bytes[0x0400] = 1;
    memory.bytes[0x2008] = 0b1000_0000;
    memory.colour[0] = 2;
    run(&mut vic, &memory, PAL_FRAME);
    assert_eq!(pal_slot(&vic, 51, 24), 2, "the foreground covers it");
    assert_eq!(pal_slot(&vic, 51, 25), 1, "the background does not");
    assert_eq!(vic.read(reg::SPRITE_DATA), 0x01, "they met");
    assert_eq!(vic.read(reg::SPRITE_DATA), 0, "reading clears");
}

#[test]
fn the_first_collision_raises_its_interrupt() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 100, 60, [0xFF, 0, 0]);
    vic.write(reg::SPRITE_X + 2, 104);
    vic.write(reg::SPRITE_Y + 2, 60);
    vic.write(reg::SPRITE_ENABLE, 0x03);
    memory.bytes[0x07F9] = 0x80;
    vic.write(reg::IRQ_ENABLE, irq::SPRITE_SPRITE);
    run(&mut vic, &memory, PAL_FRAME);
    assert!(vic.irq());
    assert_eq!(vic.read(reg::SPRITE_SPRITE), 0x03);
}

#[test]
fn the_light_pen_latches_the_beam_at_the_end_of_its_cycle_once_a_frame() {
    let mut vic = Vic::new(Model::Mos6569);
    let memory = Memory::new();
    while vic.beam() != (100, 19) {
        step(&mut vic, &memory);
    }
    vic.begin_cycle(&memory);
    vic.set_light_pen(false);
    vic.end_cycle(&memory, 0xFF);
    assert_eq!((vic.read(reg::LPX), vic.read(reg::LPY)), (0x1E, 100));
    vic.set_light_pen(true);
    run(&mut vic, &memory, 63);
    vic.begin_cycle(&memory);
    vic.set_light_pen(false);
    vic.end_cycle(&memory, 0xFF);
    assert_eq!(vic.read(reg::LPY), 100, "once a frame");
}

#[test]
fn unused_bits_and_registers_read_set() {
    let mut vic = Vic::new(Model::Mos6569);
    vic.write(reg::BORDER, 0x0E);
    assert_eq!(vic.read(reg::BORDER), 0xFE);
    assert_eq!(vic.read(reg::CR2), 0xC0);
    assert_eq!(vic.read(reg::IRQ_ENABLE), 0xF0);
    assert_eq!(vic.read(0x2F), 0xFF);
    assert_eq!(vic.read(0x3F), 0xFF);
    assert_eq!(vic.read(0x40 | reg::BORDER), 0xFE, "the registers repeat");
}

#[test]
fn an_ntsc_frame_shows_lines_0_to_12_at_its_foot() {
    let mut vic = Vic::new(Model::Mos6567R8);
    vic.write(reg::BORDER, 7);
    let memory = Memory::new();
    run(&mut vic, &memory, 263 * 65);
    assert_eq!(vic.frames(), 1);
    // Line 12 is the last shown: the 235th row from the top margin.
    let row = 2 * (32 + 234);
    assert_eq!(vic.shown()[row * CANVAS_WIDTH + 400], 7);
    assert_eq!(
        vic.shown()[(row + 2) * CANVAS_WIDTH + 400],
        0,
        "below the picture"
    );
}

#[test]
fn a_sprite_expanded_to_its_last_pixel_restores() {
    let sprite = Sprite {
        pointer: 0,
        mc: 63,
        mcbase: 63,
        dma: true,
        display: true,
        advance: true,
        crunch: false,
        data: 0xFF_FFFF,
        shifter: 0,
        shown: Some(48),
    };
    let mut w = snapshot::Writer::new();
    w.put(&sprite);
    let body = snapshot::open(&w.finish().expect("fits")).expect("opens");
    assert_eq!(
        snapshot::Reader::new(&body).get::<Sprite>(),
        Ok(sprite),
        "every pixel of an X-expanded sprite shown is a state the chip reaches"
    );
}

#[test]
fn a_snapshot_mid_frame_restores_the_chip() {
    let mut vic = pal_screen(14, 6);
    let mut memory = Memory::new();
    with_sprite(&mut vic, &mut memory, 100, 60, [0xFF, 0, 0]);
    let cycles = PAL_FRAME + 70 * 63 + 30;
    run(&mut vic, &memory, cycles);
    vic.begin_cycle(&memory);
    let save = |vic: &Vic| {
        let mut w = snapshot::Writer::new();
        w.put(vic);
        snapshot::open(&w.finish().expect("fits")).expect("opens")
    };
    let body = save(&vic);
    let mut r = snapshot::Reader::new(&body);
    r.set_clock(cycles).expect("in range");
    let restored: Vic = r.get().expect("restores");
    assert_eq!(save(&restored), body, "everything the chip holds came back");
    let mut r = snapshot::Reader::new(&body);
    r.set_clock(cycles + 1).expect("in range");
    assert!(r.get::<Vic>().is_err(), "a beam elsewhere is refused");
}
