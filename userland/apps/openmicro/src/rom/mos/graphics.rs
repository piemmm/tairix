//! The VDU drivers' graphics side: the origin and window, PLOT's lines,
//! points, fills and triangles in the GCOL actions, CLG, text at the
//! graphics cursor, POINT, and VDU 23.
//!
//! External coordinates are 1280 by 1024 from the origin; internal ones
//! are pixels from the screen's bottom left, the window held in them as the
//! VDU variables document. A pixel is plotted through the OR and EOR masks
//! at `&D4` the PLOT's action and colour set up: `(screen | or) ^ eor` under
//! the pixel's mask makes every GCOL action.

use super::ws::{buffer, var, vdu, vdu_zp, vector};
use super::{copy, return_in_teletext};
use crate::machine::bbc::sheila;
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};

/// Where a queued command's last parameter lands.
const QUEUE_LAST: u16 = vdu::QUEUE_END;
/// A PLOT's code.
const PLOT_CODE: u16 = vdu::QUEUE_END - 4;

/// `address`'s offset into the VDU variables, for a point read through it.
const fn variable_offset(address: u16) -> u8 {
    assert!(address >= vdu::GRAPHICS_WINDOW && address + 3 <= vdu::GRAPHICS_WINDOW + 0xFF);
    let [offset, _] = (address - vdu::GRAPHICS_WINDOW).to_le_bytes();
    offset
}
/// The point being plotted, internal coordinates.
const PX: u16 = vdu::SCRATCH;
const PY: u16 = vdu::SCRATCH + 2;
/// A span's right end.
const X1: u16 = vdu::SCRATCH + 4;
/// A line's direction along each axis, 0 or &FF, at the axis's offset into
/// a point, x 0 and y 2; the major axis's offset and the minor's between
/// them; the minor axis's step, 1 or -1; and the dot pattern.
const LINE_SIGNS: u16 = vdu::SCRATCH + 4;
const MAJOR: u16 = LINE_SIGNS + 1;
const MINOR: u16 = LINE_SIGNS + 3;
const MINOR_STEP: u16 = vdu::SCRATCH + 8;
const DOTS: u16 = vdu::SCRATCH + 10;
/// VDU 5's glyph row, the bits of it still to plot, and whether the cell is
/// solid instead.
const GLYPH_ROW: u16 = vdu::SCRATCH + 8;
const GLYPH_BITS: u16 = vdu::SCRATCH + 9;
const GLYPH_SOLID: u16 = vdu::SCRATCH + 10;
/// Three corners, sorted by height; VDU 24's two converted corners.
const CORNERS: u16 = vdu::SCRATCH + 11;
/// A span's masks and byte count, where the VDU queue's first bytes go
/// unused while one is filled.
const SPAN_LEFT: u16 = vdu::QUEUE;
const SPAN_RIGHT: u16 = vdu::QUEUE + 1;
const SPAN_BYTES: u16 = vdu::QUEUE + 2;
/// A triangle's two edges, each its x, remainder, 2dx and 2dy: the long
/// one in the line workspace, the short one at a span's end; then its
/// corners, rising.
const LONG_EDGE: u16 = vdu::DELTAS;
const SHORT_EDGE: u16 = vdu::SCRATCH + 6;
const TRIANGLE: u16 = vdu::SCRATCH + 14;
const EDGE_X: u16 = 0;
const EDGE_R: u16 = 2;
const EDGE_DX2: u16 = 4;
const EDGE_DY2: u16 = 6;
/// A line's distance along each axis.
const DX: u16 = vdu::DELTAS;
const DY: u16 = vdu::DELTAS + 2;
/// A line's decision term and the points it has left to step, the count
/// inverted so that counting up to zero ends the run, in the temporaries a
/// pixel's address leaves alone.
const DECISION: u16 = vdu_zp::TEMP + 2;
const LINE_COUNT: u16 = vdu_zp::TEMP + 4;
/// The minor steps a line's window has room for, inverted likewise, and the
/// two steps its decision term takes, M - m with a minor step and -m without;
/// where a triangle keeps its corners.
const MINOR_ROOM: u16 = CORNERS;
const DECISION_STEPS: u16 = TRIANGLE + 4;

/// &367 as reset and OSBYTE 20 leave it: 128–255 in RAM, 32–127 in ROM.
const FONT_DEFAULT_FLAGS: u8 = 0x0F;
/// The zones OSBYTE 20 moves into RAM, in the order it takes them.
const EXPLODABLE: u8 = 6;
const EXPLOSION: [u8; EXPLODABLE as usize] = [5, 6, 7, 1, 2, 3];
pub fn emit(a: &mut Asm, model: Model) {
    origin_and_window(a, model);
    plot(a, model);
    point(a);
    line(a);
    fills(a);
    triangle(a);
    spans(a);
    clg(a, model);
    text_at_graphics_cursor(a);
    vdu_23(a, model);
    read_pixel(a);
}

fn origin_and_window(a: &mut Asm, model: Model) {
    // VDU 29, x, y: the origin moves under the cursor, which keeps its pixel.
    a.here("vdu_29");
    copy(
        a,
        &[
            (QUEUE_LAST - 3, &[vdu::ORIGIN]),
            (vdu::GRAPHICS_INTERNAL, &[PX]),
        ],
        4,
    );
    a.jmp("pixel_cursor");

    // The graphics window: the whole screen in pixels.
    a.here("graphics_window_default");
    a.lda(imm(0));
    a.ldx(imm(7));
    let clear = a.here_local();
    a.sta(abs(vdu::GRAPHICS_WINDOW).x());
    a.dex();
    a.bpl(clear);
    a.ldx(abs(vdu::MODE));
    a.lda(abs("mode_width_low").x());
    a.sta(abs(vdu::GRAPHICS_WINDOW + 4));
    a.lda(abs("mode_width_high").x());
    a.sta(abs(vdu::GRAPHICS_WINDOW + 5));
    a.lda(imm(0xFF));
    a.sta(abs(vdu::GRAPHICS_WINDOW + 6));
    a.rts();

    // VDU 24, left, bottom, right, top: from the origin, refused unless it
    // lies on the screen the right way up.
    a.here("vdu_24");
    return_in_teletext(a, model);
    let points = const {
        [
            variable_offset(QUEUE_LAST - 7),
            variable_offset(QUEUE_LAST - 3),
        ]
    };
    for (corner, point) in [0u16, 4].into_iter().zip(points) {
        a.ldx(imm(point));
        a.jsr("external_point");
        copy(a, &[(PX, &[CORNERS + corner])], 4);
    }
    a.ldx(abs(vdu::MODE));
    a.ldy(imm(4));
    let corner = a.here_local();
    a.lda(abs(CORNERS + 1).y());
    a.ora(abs(CORNERS + 3).y());
    a.bmi(RTS);
    a.lda(abs(CORNERS + 3).y());
    a.bne(RTS);
    a.lda(abs("mode_width_low").x());
    a.cmp(abs(CORNERS).y());
    a.lda(abs("mode_width_high").x());
    a.sbc(abs(CORNERS + 1).y());
    a.bcc(RTS);
    for _ in 0..4 {
        a.dey();
    }
    a.bpl(corner);
    a.lda(abs(CORNERS + 4));
    a.cmp(abs(CORNERS));
    a.lda(abs(CORNERS + 5));
    a.sbc(abs(CORNERS + 1));
    a.bcc(RTS);
    a.lda(abs(CORNERS + 6));
    a.cmp(abs(CORNERS + 2));
    a.bcc(RTS);
    a.ldy(imm(7));
    a.here("vdu_24_copy");
    a.lda(abs(CORNERS).y());
    a.sta(abs(vdu::GRAPHICS_WINDOW).y());
    a.dey();
    a.bpl("vdu_24_copy");
    a.rts();

    // (PX, PY) from the external point X bytes into the VDU variables.
    a.here("external_point");
    a.ldy(imm(0));
    let next = a.here_local();
    a.lda(abs(vdu::GRAPHICS_WINDOW).x());
    a.sta(abs(PX).y());
    a.inx();
    a.iny();
    a.cpy(imm(4));
    a.bne(next);
    external_to_internal(a);
}

/// (PX, PY) external → internal: the origin added, then x scaled by the
/// mode's pixel width and y by four, keeping the sign.
fn external_to_internal(a: &mut Asm) {
    a.here("external_to_internal");
    a.ldx(imm(2));
    a.here("external_origin");
    a.clc();
    a.lda(abs(PX).x());
    a.adc(abs(vdu::ORIGIN).x());
    a.sta(abs(PX).x());
    a.lda(abs(PX + 1).x());
    a.adc(abs(vdu::ORIGIN + 1).x());
    a.sta(abs(PX + 1).x());
    a.dex();
    a.dex();
    a.bpl("external_origin");
    a.ldx(abs(vdu::MODE));
    a.ldy(abs("mode_x_shift").x());
    a.ldx(imm(0));
    a.jsr("external_shift");
    a.ldx(imm(2));
    a.ldy(imm(2));
    // The coordinate X bytes into (PX, PY) halved Y times, keeping its sign.
    a.here("external_shift");
    a.lda(abs(PX + 1).x());
    a.asl(Acc);
    a.ror(abs(PX + 1).x());
    a.ror(abs(PX).x());
    a.dey();
    a.bne("external_shift");
    a.rts();

    // (PX, PY) internal to external: scaled back up, less the origin.
    a.here("internal_to_external");
    a.ldx(abs(vdu::MODE));
    a.ldy(abs("mode_x_shift").x());
    a.here("internal_x_shift");
    a.asl(abs(PX));
    a.rol(abs(PX + 1));
    a.dey();
    a.bne("internal_x_shift");
    a.ldy(imm(2));
    let y_shift = a.here_local();
    a.asl(abs(PY));
    a.rol(abs(PY + 1));
    a.dey();
    a.bne(y_shift);
    a.ldx(imm(2));
    let origin = a.here_local();
    a.sec();
    a.lda(abs(PX).x());
    a.sbc(abs(vdu::ORIGIN).x());
    a.sta(abs(PX).x());
    a.lda(abs(PX + 1).x());
    a.sbc(abs(vdu::ORIGIN + 1).x());
    a.sta(abs(PX + 1).x());
    a.dex();
    a.dex();
    a.bpl(origin);
    a.rts();
}

/// PLOT k, x, y.
fn plot(a: &mut Asm, model: Model) {
    a.here("vdu_25");
    return_in_teletext(a, model);
    a.lda(abs(PLOT_CODE));
    a.cmp(imm(96));
    a.bcs("plot_unrecognised");
    a.cmp(imm(32));
    a.bcc("plot_recognised");
    a.cmp(imm(64));
    a.bcs("plot_recognised");
    a.here("plot_unrecognised");
    a.sec();
    a.jmp(abs(vector::VDU).ind());
    a.here("plot_recognised");
    // Relative coordinates add to the cursor's.
    a.and(imm(4));
    a.bne("plot_absolute");
    a.ldx(imm(2));
    let relative = a.here_local();
    a.clc();
    a.lda(abs(QUEUE_LAST - 3).x());
    a.adc(abs(vdu::GRAPHICS_CURSOR).x());
    a.sta(abs(QUEUE_LAST - 3).x());
    a.lda(abs(QUEUE_LAST - 2).x());
    a.adc(abs(vdu::GRAPHICS_CURSOR + 1).x());
    a.sta(abs(QUEUE_LAST - 2).x());
    a.dex();
    a.dex();
    a.bpl(relative);
    // The cursor takes the target's external coordinates as given.
    a.here("plot_absolute");
    copy(a, &[(QUEUE_LAST - 3, &[vdu::GRAPHICS_CURSOR])], 4);
    a.ldx(imm(const { variable_offset(vdu::GRAPHICS_CURSOR) }));
    a.jsr("external_point");
    // The target, internal, at &320.
    copy(a, &[(PX, &[vdu::PLOT_POINT])], 4);
    a.jsr("plot_masks");
    a.lda(abs(PLOT_CODE));
    a.and(imm(3));
    a.beq("plot_done");
    a.lda(abs(PLOT_CODE));
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.tax();
    a.lda(abs("plot_kind_low").x());
    a.sta(abs(vdu::JUMP));
    a.lda(abs("plot_kind_high").x());
    a.sta(abs(vdu::JUMP + 1));
    a.jsr("plot_dispatch");
    // The previous point is the cursor's, and the cursor the target.
    a.here("plot_done");
    copy(
        a,
        &[
            (vdu::GRAPHICS_INTERNAL, &[vdu::OLD_GRAPHICS_CURSOR]),
            (vdu::PLOT_POINT, &[vdu::GRAPHICS_INTERNAL]),
        ],
        4,
    );
    a.rts();
    a.here("plot_dispatch");
    a.jmp(abs(vdu::JUMP).ind());

    // The OR and EOR masks for the PLOT's colour and action: foreground,
    // inverting, or background.
    a.here("plot_masks");
    a.lda(abs(PLOT_CODE));
    a.and(imm(3));
    a.cmp(imm(2));
    a.beq("plot_masks_invert");
    a.bcs("plot_masks_background");
    a.lda(abs(vdu::GRAPHICS_FOREGROUND));
    a.ldx(abs(vdu::FOREGROUND_ACTION));
    a.jmp("action_masks");
    a.here("plot_masks_background");
    a.lda(abs(vdu::GRAPHICS_BACKGROUND));
    a.ldx(abs(vdu::BACKGROUND_ACTION));
    a.jmp("action_masks");
    a.here("plot_masks_invert");
    a.ldx(imm(4));
    action_masks(a);
}

/// A = colour byte, X = action: set, OR, AND, EOR, invert; any other
/// action leaves the screen as it is. Each mask is the colour, its
/// inverse or a constant: `(colour & M) ^ K`.
fn action_masks(a: &mut Asm) {
    a.here("action_masks");
    a.sta(zp(vdu_zp::TEMP));
    a.cpx(imm(5));
    a.bcc("action_known");
    a.ldx(imm(5));
    a.here("action_known");
    a.and(abs("action_or_m").x());
    a.eor(abs("action_or_k").x());
    a.sta(zp(vdu_zp::GRAPHICS_OR));
    a.lda(zp(vdu_zp::TEMP));
    a.and(abs("action_eor_m").x());
    a.eor(abs("action_eor_k").x());
    a.sta(zp(vdu_zp::GRAPHICS_EOR));
    a.rts();
    // set: (FF, ~c); OR: (c, 0); AND: (~c, ~c); EOR: (0, c); invert: (0, FF).
    a.here("action_or_m");
    a.bytes(&[0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00]);
    a.here("action_or_k");
    a.bytes(&[0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00]);
    a.here("action_eor_m");
    a.bytes(&[0xFF, 0x00, 0xFF, 0xFF, 0x00, 0x00]);
    a.here("action_eor_k");
    a.bytes(&[0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00]);

    a.here("plot_kind_low");
    for kind in PLOT_KINDS {
        a.byte(lo(kind));
    }
    a.here("plot_kind_high");
    for kind in PLOT_KINDS {
        a.byte(hi(kind));
    }
}

/// Each group of eight PLOT codes' routine, from 0 to 95.
const PLOT_KINDS: [&str; 12] = [
    "plot_line",
    "plot_line",
    "plot_line",
    "plot_line",
    "return",
    "return",
    "return",
    "return",
    "plot_target",
    "plot_fill_both",
    "plot_triangle",
    "plot_fill_right",
];

/// The pixel at (&D6) under its mask, through the action's masks.
fn pixel_write(a: &mut Asm) {
    a.ldy(imm(0));
    a.lda(zp(vdu_zp::GRAPHICS_OR));
    a.and(zp(vdu_zp::PIXEL_MASK));
    a.ora(zp(vdu_zp::GRAPHICS_CELL).ind_y());
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::GRAPHICS_EOR));
    a.and(zp(vdu_zp::PIXEL_MASK));
    a.eor(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::GRAPHICS_CELL).ind_y());
}

/// One pixel: clipped to the window, then plotted through the masks.
fn point(a: &mut Asm) {
    a.here("plot_target");
    a.jsr("target_point");

    a.here("plot_pixel");
    a.jsr("pixel_inside");
    a.bcc(RTS);
    a.jsr("pixel_address");
    // The pixel at (&D6) under its mask.
    // A span's end byte, under mask A.
    a.here("span_apply");
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.here("plot_cell");
    pixel_write(a);
    a.rts();

    // C set if (PX, PY) lies within the graphics window; X is kept.
    a.here("pixel_inside");
    a.ldy(imm(2));
    a.jsr("pixel_inside_axis");
    a.bcc(RTS);
    a.ldy(imm(0));
    // C set if the coordinate Y bytes into (PX, PY) lies within the window's
    // span of that axis.
    a.here("pixel_inside_axis");
    a.lda(abs(PX + 1).y());
    a.bmi("pixel_outside");
    a.lda(abs(PX).y());
    a.cmp(abs(vdu::GRAPHICS_WINDOW).y());
    a.lda(abs(PX + 1).y());
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 1).y());
    a.bcc(RTS);
    a.lda(abs(vdu::GRAPHICS_WINDOW + 4).y());
    a.cmp(abs(PX).y());
    a.lda(abs(vdu::GRAPHICS_WINDOW + 5).y());
    a.sbc(abs(PX + 1).y());
    a.rts();
    a.here("pixel_outside");
    a.clc();
    a.rts();

    // (&D6) and &D1: the byte holding pixel (PX, PY) and the pixel's mask.
    // Its cell row counts from the top, its line within the cell likewise,
    // kept at &31A, and its byte is eight on per byte of pixels across.
    a.here("pixel_address");
    a.lda(imm(0xFF));
    a.sec();
    a.sbc(abs(PY));
    a.pha();
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.asl(Acc);
    a.tay();
    // The byte across: x over pixels a byte, times eight.
    a.lda(abs(PX));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(abs(PX + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.ldx(abs(vdu::PIXELS_PER_BYTE));
    a.here("pixel_address_divide");
    a.lsr(zp(vdu_zp::TEMP + 1));
    a.ror(zp(vdu_zp::TEMP));
    a.txa();
    a.lsr(Acc);
    a.tax();
    a.bne("pixel_address_divide");
    for _ in 0..3 {
        a.asl(zp(vdu_zp::TEMP));
        a.rol(zp(vdu_zp::TEMP + 1));
    }
    a.lda(zp(vdu_zp::TEMP));
    a.clc();
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP));
    a.iny();
    a.lda(zp(vdu_zp::TEMP + 1));
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP + 1));
    a.pla();
    a.and(imm(7));
    a.sta(abs(vdu::PIXEL_LINE));
    // The screen's start moves a row at a time, so its low byte holds no
    // line within a cell.
    a.ora(abs(vdu::SCREEN_START));
    a.ldx(abs(vdu::SCREEN_START + 1));
    a.jsr("pointer_add");
    a.lda(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::GRAPHICS_CELL));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sta(zp(vdu_zp::GRAPHICS_CELL + 1));
    // The mask: the left pixel's, moved across by x within its byte.
    a.lda(abs(PX));
    a.and(abs(vdu::PIXELS_PER_BYTE));
    a.tax();
    a.lda(abs(vdu::LEFT_MASK));
    a.here("pixel_mask_move");
    a.dex();
    a.bmi("pixel_mask_done");
    a.lsr(Acc);
    a.bne("pixel_mask_move");
    a.here("pixel_mask_done");
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.rts();
}

/// Lines by Bresenham's method, solid or every other point.
///
/// A line runs from whichever end comes first along its longer axis, so
/// both directions draw the same points; a diagonal runs from the target
/// unless it rises to the right. The points before the window are stepped
/// past by their coordinates, and the run inside it by its screen address
/// until it reaches the window's edge. Codes 8–15 and 24–31 plot the target
/// again afterwards, which an inverting action undoes.
fn line(a: &mut Asm) {
    a.here("plot_line");
    a.ldx(imm(0xFF));
    a.lda(abs(PLOT_CODE));
    a.and(imm(0x10));
    a.beq("line_solid");
    a.ldx(imm(0xAA));
    a.here("line_solid");
    a.stx(abs(DOTS));
    // Each axis's distance and direction from the cursor to the target.
    a.ldx(imm(2));
    a.here("line_delta");
    a.sec();
    a.lda(abs(vdu::PLOT_POINT).x());
    a.sbc(abs(vdu::GRAPHICS_INTERNAL).x());
    a.sta(abs(DX).x());
    a.lda(abs(vdu::PLOT_POINT + 1).x());
    a.sbc(abs(vdu::GRAPHICS_INTERNAL + 1).x());
    a.sta(abs(DX + 1).x());
    a.bpl("line_delta_forward");
    a.sec();
    a.lda(imm(0));
    a.sbc(abs(DX).x());
    a.sta(abs(DX).x());
    a.lda(imm(0));
    a.sbc(abs(DX + 1).x());
    a.sta(abs(DX + 1).x());
    a.lda(imm(0xFF));
    a.bne("line_delta_signed");
    a.here("line_delta_forward");
    a.lda(imm(0));
    a.here("line_delta_signed");
    a.sta(abs(LINE_SIGNS).x());
    a.dex();
    a.dex();
    a.bpl("line_delta");
    // The major axis is the longer, or on a diagonal x unless the line falls
    // to the right.
    a.sec();
    a.lda(abs(DY));
    a.sbc(abs(DX));
    a.tax();
    a.lda(abs(DY + 1));
    a.sbc(abs(DX + 1));
    a.bmi("line_x_major");
    a.bne("line_y_major");
    a.txa();
    a.bne("line_y_major");
    a.lda(abs(LINE_SIGNS));
    a.eor(imm(0xFF));
    a.and(abs(LINE_SIGNS + 2));
    a.bpl("line_x_major");
    a.here("line_y_major");
    a.lda(imm(2));
    a.bne("line_major_set");
    a.here("line_x_major");
    a.lda(imm(0));
    a.here("line_major_set");
    a.sta(abs(MAJOR));
    a.tay();
    a.eor(imm(2));
    a.sta(abs(MINOR));
    a.tax();
    line_start(a);
    line_seek(a);
    line_draw(a);
    cell_step_x(a);
    cell_add(a);
}

/// A line whose major axis runs backwards starts from the target, its
/// minor axis turned round with it.
fn line_start(a: &mut Asm) {
    a.lda(abs(LINE_SIGNS).x());
    a.eor(abs(LINE_SIGNS).y());
    a.ora(imm(1));
    a.sta(abs(MINOR_STEP));
    a.ldx(imm(lo(vdu::GRAPHICS_INTERNAL - vdu::PLOT_POINT + 3)));
    a.lda(abs(LINE_SIGNS).y());
    a.bpl("line_start");
    a.ldx(imm(3));
    a.here("line_start");
    a.ldy(imm(3));
    a.here("line_start_copy");
    a.lda(abs(vdu::PLOT_POINT).x());
    a.sta(abs(PX).y());
    a.dex();
    a.dey();
    a.bpl("line_start_copy");
    // With M the major distance and m the minor: the steps the decision
    // term takes, M - m and -m; M points to step past, inverted; and the term
    // itself, from M / 2 - m.
    a.ldy(abs(MAJOR));
    a.ldx(abs(MINOR));
    a.sec();
    a.lda(imm(0));
    a.sbc(abs(DX).x());
    a.sta(abs(DECISION_STEPS + 2));
    a.lda(imm(0));
    a.sbc(abs(DX + 1).x());
    a.sta(abs(DECISION_STEPS + 3));
    a.clc();
    a.lda(abs(DX).y());
    a.adc(abs(DECISION_STEPS + 2));
    a.sta(abs(DECISION_STEPS));
    a.lda(abs(DX + 1).y());
    a.adc(abs(DECISION_STEPS + 3));
    a.sta(abs(DECISION_STEPS + 1));
    a.lda(abs(DX).y());
    a.eor(imm(0xFF));
    a.sta(zp(LINE_COUNT));
    a.lda(abs(DX + 1).y());
    a.eor(imm(0xFF));
    a.sta(zp(LINE_COUNT + 1));
    a.lda(abs(DX + 1).y());
    a.lsr(Acc);
    a.tax();
    a.lda(abs(DX).y());
    a.ror(Acc);
    a.clc();
    a.adc(abs(DECISION_STEPS + 2));
    a.sta(zp(DECISION));
    a.txa();
    a.adc(abs(DECISION_STEPS + 3));
    a.sta(zp(DECISION + 1));
}

/// Past the points before the window, by their coordinates.
fn line_seek(a: &mut Asm) {
    a.here("line_seek");
    a.jsr("pixel_inside");
    a.bcs("line_found");
    a.inc(zp(LINE_COUNT));
    a.bne("line_seek_step");
    a.inc(zp(LINE_COUNT + 1));
    a.beq("line_done");
    a.here("line_seek_step");
    a.asl(abs(DOTS));
    a.bcc("line_seek_dark");
    a.inc(abs(DOTS));
    a.here("line_seek_dark");
    a.ldy(abs(MAJOR));
    a.ldx(imm(1));
    a.jsr("point_step");
    a.jsr("line_decide");
    a.bcs("line_seek");
    a.ldy(abs(MINOR));
    a.ldx(abs(MINOR_STEP));
    a.jsr("point_step");
    a.jmp("line_seek");
    a.here("line_done");
    a.lda(abs(PLOT_CODE));
    a.and(imm(8));
    a.beq(RTS);
    a.jmp("plot_target");
    a.rts();

    // The run inside: to the window's edge along the major axis, and as many
    // minor steps as reach the edge the minor axis moves towards. Each room
    // is held inverted, edge distance plus one below zero.
    a.here("line_found");
    a.jsr("pixel_address");
    a.ldx(abs(MAJOR));
    a.clc();
    a.lda(abs(PX).x());
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 4).x());
    a.tay();
    a.lda(abs(PX + 1).x());
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 5).x());
    a.cpy(zp(LINE_COUNT));
    a.pha();
    a.sbc(zp(LINE_COUNT + 1));
    a.pla();
    a.bcc("line_count_kept");
    a.sty(zp(LINE_COUNT));
    a.sta(zp(LINE_COUNT + 1));
    a.here("line_count_kept");
    a.ldx(abs(MINOR));
    a.clc();
    a.lda(abs(MINOR_STEP));
    a.bmi("line_room_down");
    a.lda(abs(PX).x());
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 4).x());
    a.sta(abs(MINOR_ROOM));
    a.lda(abs(PX + 1).x());
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 5).x());
    a.sta(abs(MINOR_ROOM + 1));
    a.bcc("line_draw");
    a.here("line_room_down");
    a.lda(abs(vdu::GRAPHICS_WINDOW).x());
    a.sbc(abs(PX).x());
    a.sta(abs(MINOR_ROOM));
    a.lda(abs(vdu::GRAPHICS_WINDOW + 1).x());
    a.sbc(abs(PX + 1).x());
    a.sta(abs(MINOR_ROOM + 1));
}

/// Plotted through the stepped address, the major axis always forwards:
/// right or up.
fn line_draw(a: &mut Asm) {
    a.here("line_draw");
    a.asl(abs(DOTS));
    a.bcc("line_dark");
    a.inc(abs(DOTS));
    pixel_write(a);
    a.here("line_dark");
    a.inc(zp(LINE_COUNT));
    a.bne("line_forward");
    a.inc(zp(LINE_COUNT + 1));
    a.here("line_draw_ended");
    a.beq("line_done");
    a.here("line_forward");
    a.lda(abs(MAJOR));
    a.bne("line_up");
    a.lsr(zp(vdu_zp::PIXEL_MASK));
    a.bcc("line_stepped");
    a.jsr("cell_step_right_byte");
    a.jmp("line_stepped");
    a.here("line_up");
    a.lda(zp(vdu_zp::GRAPHICS_CELL));
    a.and(imm(7));
    a.bne("line_up_line");
    a.jsr("cell_step_up_row");
    a.jmp("line_stepped");
    a.here("line_up_line");
    a.dec(zp(vdu_zp::GRAPHICS_CELL));
    a.here("line_stepped");
    decision_step(a, "line_draw_decided");
    a.txa();
    a.bne("line_draw");
    a.inc(abs(MINOR_ROOM));
    a.bne("line_draw_minor");
    a.inc(abs(MINOR_ROOM + 1));
    a.beq("line_draw_ended");
    a.here("line_draw_minor");
    a.ldy(abs(MINOR));
    a.ldx(abs(MINOR_STEP));
    a.jsr("cell_step");
    a.jmp("line_draw");

    // The decision term steps on; C clear if the minor axis steps with it.
    a.here("line_decide");
    decision_step(a, "line_decided");
    a.cpx(imm(1));
    a.rts();
}

/// The pixel left or right, X -1 or 1: the mask moves along its byte, or
/// on to the next byte's end.
fn cell_step_x(a: &mut Asm) {
    a.here("cell_step_x");
    a.lda(zp(vdu_zp::PIXEL_MASK));
    a.cpx(imm(1));
    a.bne("cell_step_left");
    a.cmp(abs(vdu::RIGHT_MASK));
    a.beq("cell_step_right_byte");
    a.lsr(Acc);
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.rts();
    a.here("cell_step_right_byte");
    a.lda(abs(vdu::LEFT_MASK));
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.lda(imm(8));
    a.ldy(imm(0));
    a.beq("cell_add");
    a.here("cell_step_left");
    a.cmp(abs(vdu::LEFT_MASK));
    a.beq("cell_step_left_byte");
    a.asl(Acc);
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.rts();
    a.here("cell_step_left_byte");
    a.lda(abs(vdu::RIGHT_MASK));
    a.sta(zp(vdu_zp::PIXEL_MASK));
    a.lda(imm(0xF8));
    a.ldy(imm(0xFF));
    a.bne("cell_add");

    // The pixel's address one step along axis Y, x 0 or y 2, by X, 1 or -1.
    a.here("cell_step");
    a.cpy(imm(2));
    a.bne("cell_step_x");

    // The pixel up or down, X 1 or -1: the next line of the character row,
    // or the row beyond's last or first.
    a.here("cell_step_y");
    a.lda(zp(vdu_zp::GRAPHICS_CELL));
    a.and(imm(7));
    a.cpx(imm(1));
    a.bne("cell_step_down");
    a.cmp(imm(0));
    a.beq("cell_step_up_row");
    a.dec(zp(vdu_zp::GRAPHICS_CELL));
    a.rts();
    a.here("cell_step_up_row");
    a.lda(imm(7));
    a.sec();
    a.sbc(abs(vdu::BYTES_PER_ROW));
    a.tax();
    a.lda(imm(0));
    a.sbc(abs(vdu::BYTES_PER_ROW + 1));
    a.tay();
    a.txa();
    a.jmp("cell_add");
    a.here("cell_step_down");
    a.cmp(imm(7));
    a.beq("cell_step_down_row");
    a.inc(zp(vdu_zp::GRAPHICS_CELL));
    a.rts();
    a.here("cell_step_down_row");
    a.lda(abs(vdu::BYTES_PER_ROW));
    a.sec();
    a.sbc(imm(7));
    a.tax();
    a.lda(abs(vdu::BYTES_PER_ROW + 1));
    a.sbc(imm(0));
    a.tay();
    a.txa();
}

/// The cell's address += YA, kept within the screen's memory.
fn cell_add(a: &mut Asm) {
    a.here("cell_add");
    a.clc();
    a.adc(zp(vdu_zp::GRAPHICS_CELL));
    a.sta(zp(vdu_zp::GRAPHICS_CELL));
    a.tya();
    a.adc(zp(vdu_zp::GRAPHICS_CELL + 1));
    a.bpl("cell_add_below_top");
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.bcs("cell_add_set");
    a.here("cell_add_below_top");
    a.cmp(abs(vdu::SCREEN_BOTTOM));
    a.bcs("cell_add_set");
    a.adc(abs(vdu::SCREEN_SIZE));
    a.here("cell_add_set");
    a.sta(zp(vdu_zp::GRAPHICS_CELL + 1));
    a.rts();

    // The point one step along axis Y, x 0 or y 2, by X, 1 or -1.
    a.here("step_px");
    a.ldy(imm(0));
    a.beq("point_step");
    a.here("step_py");
    a.ldy(imm(2));
    a.here("point_step");
    a.txa();
    a.clc();
    a.adc(abs(PX).y());
    a.sta(abs(PX).y());
    a.txa();
    a.and(imm(0x80));
    a.beq("point_step_carry");
    a.lda(imm(0xFF));
    a.here("point_step_carry");
    a.adc(abs(PX + 1).y());
    a.sta(abs(PX + 1).y());
    a.rts();
}

/// The decision term's step: by M - m, X 0, while it is negative, the minor
/// axis then stepping too; otherwise by -m, X 2.
fn decision_step(a: &mut Asm, decided: &'static str) {
    a.ldx(imm(2));
    a.lda(zp(DECISION + 1));
    a.bpl(decided);
    a.ldx(imm(0));
    a.here(decided);
    a.clc();
    a.lda(zp(DECISION));
    a.adc(abs(DECISION_STEPS).x());
    a.sta(zp(DECISION));
    a.lda(zp(DECISION + 1));
    a.adc(abs(DECISION_STEPS + 1).x());
    a.sta(zp(DECISION + 1));
}

/// Horizontal line fills from the target, left and right over background or
/// right over anything else. The run's start becomes the previous point and
/// its end the cursor; with nothing filled the target is the cursor.
fn fills(a: &mut Asm) {
    a.here("plot_fill_both");
    a.jsr("target_point");
    a.jsr("pixel_inside");
    a.bcc("fill_none");
    a.jsr("pixel_is_background");
    a.bne("fill_none");
    // Find the left end, then fill right to the first non-background pixel.
    a.here("fill_left");
    a.ldx(imm(0xFF));
    a.jsr("step_px");
    a.jsr("pixel_inside");
    a.bcc("fill_left_end");
    a.jsr("pixel_is_background");
    a.beq("fill_left");
    a.here("fill_left_end");
    a.ldx(imm(1));
    a.jsr("step_px");
    a.lda(imm(0));
    a.beq("fill_from");

    a.here("plot_fill_right");
    a.jsr("target_point");
    a.jsr("pixel_inside");
    a.bcc("fill_none");
    a.lda(imm(0xFF));

    // Plot along from PX while the pixel is (A zero) or is not (A &FF)
    // background, stopping at the window's edge.
    a.here("fill_from");
    a.sta(abs(DOTS));
    a.jsr("point_internal");
    a.here("fill_run_next");
    a.jsr("pixel_inside");
    a.bcc("fill_run_end");
    a.jsr("pixel_is_background");
    a.php();
    a.lda(abs(DOTS));
    a.plp();
    a.beq("fill_run_background");
    a.eor(imm(0xFF));
    a.here("fill_run_background");
    a.cmp(imm(0));
    a.bne("fill_run_end");
    a.jsr("plot_pixel");
    a.ldx(imm(1));
    a.jsr("step_px");
    a.jmp("fill_run_next");
    a.here("fill_run_end");
    a.ldx(imm(0xFF));
    a.jsr("step_px");
    a.jmp("fill_cursor");

    // Nothing filled: the previous point is the target a line up, carried
    // into no high byte, as MOS 1.20 leaves it.
    a.here("fill_none");
    a.jsr("point_internal");
    a.inc(abs(vdu::GRAPHICS_INTERNAL + 2));
    // The cursor is PX, its external form scaled back from the pixel.
    a.here("fill_cursor");
    copy(a, &[(PX, &[vdu::PLOT_POINT])], 2);
    // The external cursor, scaled back from the pixel (PX, PY).
    a.here("pixel_cursor");
    a.jsr("internal_to_external");
    copy(a, &[(PX, &[vdu::GRAPHICS_CURSOR])], 4);
    a.rts();

    // The internal cursor = (PX, PY).
    a.here("point_internal");
    copy(a, &[(PX, &[vdu::GRAPHICS_INTERNAL])], 4);
    a.rts();

    // (PX, PY) = the target.
    a.here("target_point");
    copy(a, &[(vdu::PLOT_POINT, &[PX])], 4);
    a.rts();

    // Z set if pixel (PX, PY) is the graphics background colour.
    a.here("pixel_is_background");
    a.jsr("pixel_address");
    a.ldy(imm(0));
    a.lda(zp(vdu_zp::GRAPHICS_CELL).ind_y());
    a.eor(abs(vdu::GRAPHICS_BACKGROUND));
    a.and(zp(vdu_zp::PIXEL_MASK));
    a.rts();
}

/// A triangle from the last two points and the target, filled a line at a
/// time between its edges.
fn triangle(a: &mut Asm) {
    a.here("plot_triangle");
    // Corners: the previous point, the cursor and the target, then in
    // order of height.
    copy(
        a,
        &[
            (vdu::OLD_GRAPHICS_CURSOR, &[TRIANGLE]),
            (vdu::GRAPHICS_INTERNAL, &[TRIANGLE + 4]),
            (vdu::PLOT_POINT, &[TRIANGLE + 8]),
        ],
        4,
    );
    for pair in [0u8, 4, 0] {
        a.ldy(imm(pair));
        a.jsr("triangle_order");
    }
    // The long edge from the lowest corner to the highest, the short one
    // from the lowest to the middle; up a line at a time from the lowest.
    copy(a, &[(TRIANGLE + 2, &[PY])], 2);
    a.ldx(imm(0));
    a.ldy(imm(8));
    a.jsr("edge_start_long");
    a.ldx(imm(0));
    a.ldy(imm(4));
    a.jsr("edge_start_short");
    a.here("triangle_line");
    // At the middle corner's line the short edge turns, unless that is the
    // top.
    a.lda(abs(PY));
    a.cmp(abs(TRIANGLE + 6));
    a.bne("triangle_fill");
    a.lda(abs(PY + 1));
    a.cmp(abs(TRIANGLE + 7));
    a.bne("triangle_fill");
    a.lda(abs(TRIANGLE + 6));
    a.cmp(abs(TRIANGLE + 10));
    a.bne("triangle_turn");
    a.lda(abs(TRIANGLE + 7));
    a.cmp(abs(TRIANGLE + 11));
    a.beq("triangle_fill");
    a.here("triangle_turn");
    a.ldx(imm(4));
    a.ldy(imm(8));
    a.jsr("edge_start_short");
    a.here("triangle_fill");
    copy(a, &[(LONG_EDGE, &[X1]), (SHORT_EDGE, &[PX])], 2);
    a.jsr("span_fill");
    // Done at the top corner's line.
    a.lda(abs(PY));
    a.cmp(abs(TRIANGLE + 10));
    a.bne("triangle_next");
    a.lda(abs(PY + 1));
    a.cmp(abs(TRIANGLE + 11));
    a.bne("triangle_next");
    a.rts();
    a.here("triangle_next");
    a.ldx(imm(1));
    a.jsr("step_py");
    a.ldx(imm(0));
    a.jsr("edge_step");
    a.ldx(imm(lo(SHORT_EDGE - LONG_EDGE)));
    a.jsr("edge_step");
    a.jmp("triangle_line");

    // Swap corners Y/4 and Y/4 + 1 if they are out of height order.
    a.here("triangle_order");
    a.lda(abs(TRIANGLE + 6).y());
    a.cmp(abs(TRIANGLE + 2).y());
    a.lda(abs(TRIANGLE + 7).y());
    a.sbc(abs(TRIANGLE + 3).y());
    a.bvc("triangle_order_compared");
    a.eor(imm(0x80));
    a.here("triangle_order_compared");
    a.bpl(RTS);
    a.ldx(imm(4));
    a.here("triangle_order_swap");
    a.lda(abs(TRIANGLE).y());
    a.pha();
    a.lda(abs(TRIANGLE + 4).y());
    a.sta(abs(TRIANGLE).y());
    a.pla();
    a.sta(abs(TRIANGLE + 4).y());
    a.iny();
    a.dex();
    a.bne("triangle_order_swap");
    a.rts();
    edge_start_short(a);
    edge_step(a);
}

/// An edge from corner X to corner Y (offsets into the corners): its x
/// is round(x0 + n·dx/dy) on its nth line, kept as x and a remainder r
/// with 2·dy·x + r = 2·x0·dy + 2n·dx + dy and 0 <= r < 2·dy. A level
/// edge's x is its far end's.
fn edge_start_short(a: &mut Asm) {
    a.here("edge_start_short");
    a.lda(imm(lo(SHORT_EDGE - LONG_EDGE)));
    a.bne("edge_start");
    a.here("edge_start_long");
    a.lda(imm(0));
    a.here("edge_start");
    a.sta(zp(vdu_zp::TEMP + 2));
    a.stx(zp(vdu_zp::TEMP));
    a.sty(zp(vdu_zp::TEMP + 1));
    for (field, coordinate) in [(EDGE_DX2, 0u16), (EDGE_DY2, 2)] {
        a.ldx(zp(vdu_zp::TEMP));
        a.ldy(zp(vdu_zp::TEMP + 1));
        a.sec();
        a.lda(abs(TRIANGLE + coordinate).y());
        a.sbc(abs(TRIANGLE + coordinate).x());
        a.pha();
        a.lda(abs(TRIANGLE + coordinate + 1).y());
        a.sbc(abs(TRIANGLE + coordinate + 1).x());
        a.ldx(zp(vdu_zp::TEMP + 2));
        a.sta(abs(LONG_EDGE + field + 1).x());
        a.pla();
        a.asl(Acc);
        a.sta(abs(LONG_EDGE + field).x());
        a.rol(abs(LONG_EDGE + field + 1).x());
    }
    // r = dy, x = x0, so x rounds to nearest; less one where x rises, so a
    // half rounds back toward x0 either way. A level edge's x is x1.
    a.lda(abs(LONG_EDGE + EDGE_DY2 + 1).x());
    a.lsr(Acc);
    a.sta(abs(LONG_EDGE + EDGE_R + 1).x());
    a.lda(abs(LONG_EDGE + EDGE_DY2).x());
    a.ror(Acc);
    a.sta(abs(LONG_EDGE + EDGE_R).x());
    a.ora(abs(LONG_EDGE + EDGE_R + 1).x());
    a.beq("edge_start_level");
    a.lda(abs(LONG_EDGE + EDGE_DX2 + 1).x());
    a.bmi("edge_start_falling");
    a.lda(abs(LONG_EDGE + EDGE_R).x());
    a.bne("edge_start_low");
    a.dec(abs(LONG_EDGE + EDGE_R + 1).x());
    a.here("edge_start_low");
    a.dec(abs(LONG_EDGE + EDGE_R).x());
    a.here("edge_start_falling");
    a.lda(imm(1));
    a.here("edge_start_level");
    a.ldy(zp(vdu_zp::TEMP));
    a.cmp(imm(0));
    a.bne("edge_start_x");
    a.ldy(zp(vdu_zp::TEMP + 1));
    a.here("edge_start_x");
    a.lda(abs(TRIANGLE).y());
    a.sta(abs(LONG_EDGE + EDGE_X).x());
    a.lda(abs(TRIANGLE + 1).y());
    a.sta(abs(LONG_EDGE + EDGE_X + 1).x());
    a.rts();
}

/// The edge X on from the long one, a line on: r += 2dx, then x moves while r
/// leaves [0, 2dy).
fn edge_step(a: &mut Asm) {
    a.here("edge_step");
    a.lda(abs(LONG_EDGE + EDGE_DY2).x());
    a.ora(abs(LONG_EDGE + EDGE_DY2 + 1).x());
    a.beq(RTS);
    a.clc();
    a.lda(abs(LONG_EDGE + EDGE_R).x());
    a.adc(abs(LONG_EDGE + EDGE_DX2).x());
    a.sta(abs(LONG_EDGE + EDGE_R).x());
    a.lda(abs(LONG_EDGE + EDGE_R + 1).x());
    a.adc(abs(LONG_EDGE + EDGE_DX2 + 1).x());
    a.sta(abs(LONG_EDGE + EDGE_R + 1).x());
    a.here("edge_step_up");
    // r >= 2dy: r -= 2dy, x += 1.
    a.lda(abs(LONG_EDGE + EDGE_R).x());
    a.cmp(abs(LONG_EDGE + EDGE_DY2).x());
    a.lda(abs(LONG_EDGE + EDGE_R + 1).x());
    a.sbc(abs(LONG_EDGE + EDGE_DY2 + 1).x());
    a.bvc("edge_step_up_compared");
    a.eor(imm(0x80));
    a.here("edge_step_up_compared");
    a.bmi("edge_step_down");
    a.sec();
    a.lda(abs(LONG_EDGE + EDGE_R).x());
    a.sbc(abs(LONG_EDGE + EDGE_DY2).x());
    a.sta(abs(LONG_EDGE + EDGE_R).x());
    a.lda(abs(LONG_EDGE + EDGE_R + 1).x());
    a.sbc(abs(LONG_EDGE + EDGE_DY2 + 1).x());
    a.sta(abs(LONG_EDGE + EDGE_R + 1).x());
    a.inc(abs(LONG_EDGE + EDGE_X).x());
    a.bne("edge_step_up");
    a.inc(abs(LONG_EDGE + EDGE_X + 1).x());
    a.jmp("edge_step_up");
    // r < 0: r += 2dy, x -= 1.
    a.here("edge_step_down");
    a.lda(abs(LONG_EDGE + EDGE_R + 1).x());
    a.bpl(RTS);
    a.clc();
    a.lda(abs(LONG_EDGE + EDGE_R).x());
    a.adc(abs(LONG_EDGE + EDGE_DY2).x());
    a.sta(abs(LONG_EDGE + EDGE_R).x());
    a.lda(abs(LONG_EDGE + EDGE_R + 1).x());
    a.adc(abs(LONG_EDGE + EDGE_DY2 + 1).x());
    a.sta(abs(LONG_EDGE + EDGE_R + 1).x());
    a.lda(abs(LONG_EDGE + EDGE_X).x());
    a.bne("edge_step_down_low");
    a.dec(abs(LONG_EDGE + EDGE_X + 1).x());
    a.here("edge_step_down_low");
    a.dec(abs(LONG_EDGE + EDGE_X).x());
    a.jmp("edge_step_down");
}

/// A horizontal span, from X1 to PX, on line PY.
fn spans(a: &mut Asm) {
    a.here("span_fill");
    a.jsr("span_ends");
    a.bvc("span_compared");
    a.eor(imm(0x80));
    a.here("span_compared");
    a.bpl("span_ordered");
    for offset in 0..2 {
        a.lda(abs(PX + offset));
        a.ldx(abs(X1 + offset));
        a.sta(abs(X1 + offset));
        a.stx(abs(PX + offset));
    }
    a.here("span_ordered");
    // Clip: the line within the window's height, the ends within its width.
    a.lda(abs(PY + 1));
    a.bne(RTS);
    a.lda(abs(PY));
    a.cmp(abs(vdu::GRAPHICS_WINDOW + 2));
    a.bcc(RTS);
    a.lda(abs(vdu::GRAPHICS_WINDOW + 6));
    a.cmp(abs(PY));
    a.bcc(RTS);
    // left = max(X1, window left)
    a.lda(abs(X1));
    a.cmp(abs(vdu::GRAPHICS_WINDOW));
    a.lda(abs(X1 + 1));
    a.sbc(abs(vdu::GRAPHICS_WINDOW + 1));
    a.bvc("span_left_compared");
    a.eor(imm(0x80));
    a.here("span_left_compared");
    a.bpl("span_left_in");
    a.lda(abs(vdu::GRAPHICS_WINDOW));
    a.sta(abs(X1));
    a.lda(abs(vdu::GRAPHICS_WINDOW + 1));
    a.sta(abs(X1 + 1));
    a.here("span_left_in");
    // right = min(PX, window right)
    a.lda(abs(vdu::GRAPHICS_WINDOW + 4));
    a.cmp(abs(PX));
    a.lda(abs(vdu::GRAPHICS_WINDOW + 5));
    a.sbc(abs(PX + 1));
    a.bvc("span_right_compared");
    a.eor(imm(0x80));
    a.here("span_right_compared");
    a.bpl("span_right_in");
    a.lda(abs(vdu::GRAPHICS_WINDOW + 4));
    a.sta(abs(PX));
    a.lda(abs(vdu::GRAPHICS_WINDOW + 5));
    a.sta(abs(PX + 1));
    a.here("span_right_in");
    // Nothing left when left > right.
    a.jsr("span_ends");
    a.bpl("span_some");
    a.rts();
    // PX less X1, in the flags.
    a.here("span_ends");
    a.lda(abs(PX));
    a.cmp(abs(X1));
    a.lda(abs(PX + 1));
    a.sbc(abs(X1 + 1));
    a.rts();
    a.here("span_some");
    span_masks(a);
    span_plot(a);
}

/// The right end's mask: its byte's pixels up to its own.
fn span_masks(a: &mut Asm) {
    a.lda(abs(PX));
    a.and(abs(vdu::PIXELS_PER_BYTE));
    a.tax();
    a.lda(abs(vdu::LEFT_MASK));
    a.sta(zp(vdu_zp::TEMP + 3));
    a.lda(imm(0));
    a.here("span_right_mask");
    a.ora(zp(vdu_zp::TEMP + 3));
    a.lsr(zp(vdu_zp::TEMP + 3));
    a.dex();
    a.bpl("span_right_mask");
    a.sta(abs(SPAN_RIGHT));
    // The left end's: its own pixel and those after it in its byte.
    a.lda(abs(X1));
    a.and(abs(vdu::PIXELS_PER_BYTE));
    a.tax();
    a.lda(abs(vdu::LEFT_MASK));
    a.here("span_left_shift");
    a.dex();
    a.bmi("span_left_shifted");
    a.lsr(Acc);
    a.jmp("span_left_shift");
    a.here("span_left_shifted");
    a.sta(zp(vdu_zp::TEMP + 3));
    a.lda(abs(X1));
    a.and(abs(vdu::PIXELS_PER_BYTE));
    a.eor(abs(vdu::PIXELS_PER_BYTE));
    a.tax();
    a.lda(imm(0));
    a.here("span_left_mask");
    a.ora(zp(vdu_zp::TEMP + 3));
    a.lsr(zp(vdu_zp::TEMP + 3));
    a.dex();
    a.bpl("span_left_mask");
    a.sta(abs(SPAN_LEFT));
    // The left end's byte.
    a.lda(abs(PX));
    a.sta(zp(vdu_zp::TEMP + 4));
    a.lda(abs(PX + 1));
    a.sta(zp(vdu_zp::TEMP + 5));
    for offset in 0..2 {
        a.lda(abs(X1 + offset));
        a.sta(abs(PX + offset));
    }
    a.jsr("pixel_address");
    // Bytes across from the left end's to the right end's.
    a.ldx(abs(vdu::PIXELS_PER_BYTE));
    a.here("span_count_divide");
    for value in [vdu_zp::TEMP + 4, PX] {
        a.lsr(abs(value + 1));
        a.ror(abs(value));
    }
    a.txa();
    a.lsr(Acc);
    a.tax();
    a.bne("span_count_divide");
    a.lda(zp(vdu_zp::TEMP + 4));
    a.sec();
    a.sbc(abs(PX));
    a.sta(abs(SPAN_BYTES));
}

/// Plot: one byte under both masks, or the left, whole bytes, the right.
fn span_plot(a: &mut Asm) {
    a.lda(abs(SPAN_LEFT));
    a.ldx(abs(SPAN_BYTES));
    a.bne("span_left_byte");
    a.and(abs(SPAN_RIGHT));
    a.jmp("span_apply");
    a.here("span_left_byte");
    a.jsr("span_apply_step");
    // The whole bytes between, each through the masks directly.
    a.ldy(imm(0));
    a.here("span_middle");
    a.dec(abs(SPAN_BYTES));
    a.beq("span_last");
    a.lda(zp(vdu_zp::GRAPHICS_CELL).ind_y());
    a.ora(zp(vdu_zp::GRAPHICS_OR));
    a.eor(zp(vdu_zp::GRAPHICS_EOR));
    a.sta(zp(vdu_zp::GRAPHICS_CELL).ind_y());
    a.lda(imm(8));
    a.clc();
    a.adc(zp(vdu_zp::GRAPHICS_CELL));
    a.sta(zp(vdu_zp::GRAPHICS_CELL));
    a.bcc("span_middle");
    a.inc(zp(vdu_zp::GRAPHICS_CELL + 1));
    a.bpl("span_middle");
    a.lda(zp(vdu_zp::GRAPHICS_CELL + 1));
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(vdu_zp::GRAPHICS_CELL + 1));
    a.jmp("span_middle");
    a.here("span_last");
    a.lda(abs(SPAN_RIGHT));
    a.jmp("span_apply");
    // Apply under mask A, then the next byte across: eight on, wrapped.
    a.here("span_apply_step");
    a.jsr("span_apply");
    a.lda(imm(8));
    a.ldy(imm(0));
    a.jmp("cell_add");
}

/// CLG: the graphics window in the background colour and action, a line at
/// a time from its top down, as MOS 1.20 wipes it.
fn clg(a: &mut Asm, model: Model) {
    a.here("vdu_16");
    return_in_teletext(a, model);
    a.lda(abs(vdu::GRAPHICS_BACKGROUND));
    a.ldx(abs(vdu::BACKGROUND_ACTION));
    a.jsr("action_masks");
    for offset in 0..2 {
        a.lda(abs(vdu::GRAPHICS_WINDOW + 6 + offset));
        a.sta(abs(PY + offset));
    }
    a.here("clg_line");
    for offset in 0..2 {
        a.lda(abs(vdu::GRAPHICS_WINDOW + offset));
        a.sta(abs(X1 + offset));
        a.lda(abs(vdu::GRAPHICS_WINDOW + 4 + offset));
        a.sta(abs(PX + offset));
    }
    a.jsr("span_fill");
    a.lda(abs(vdu::GRAPHICS_WINDOW + 2));
    a.cmp(abs(PY));
    a.lda(abs(vdu::GRAPHICS_WINDOW + 3));
    a.sbc(abs(PY + 1));
    a.bcs(RTS);
    a.ldx(imm(0xFF));
    a.jsr("step_py");
    a.jmp("clg_line");
    a.rts();
}

/// VDU 5: the graphics cursor as the text cursor, kept in pixels. A
/// character's foreground pixels are plotted with their top left at the
/// cursor, which then moves on a cell; the cursor controls move it a cell at
/// a time, a cell beyond the window's edge wrapping to the cell at its other
/// edge, and an x wrap carrying it a line on. Each move leaves the external
/// cursor scaled back from the pixel, as MOS 1.20 does.
fn text_at_graphics_cursor(a: &mut Asm) {
    a.here("vdu5_print");
    a.jsr("glyph_address");
    a.lda(abs(vdu::GRAPHICS_FOREGROUND));
    a.ldx(abs(vdu::FOREGROUND_ACTION));
    a.jsr("action_masks");
    a.lda(imm(0));
    a.jsr("vdu5_cell");
    a.ldx(imm(9));

    // VDU 8 to 13: back, forward, down and up a cell, CLG and home, and to
    // the window's left.
    a.here("vdu5_control");
    a.jsr("vdu5_cursor");
    a.cpx(imm(12));
    a.bcs("vdu5_page");
    // 8 to 11: the axis is bit 1, forwards bit 0.
    a.txa();
    a.and(imm(2));
    a.tay();
    a.txa();
    a.lsr(Acc);
    a.ldx(imm(0xF8));
    a.bcc("vdu5_backwards");
    a.ldx(imm(8));
    a.here("vdu5_backwards");
    a.jsr("vdu5_step");
    a.jmp("vdu5_save");
    a.here("vdu5_page");
    a.bne("vdu5_return");
    a.jsr("vdu_16");
    // VDU 30: the window's top left.
    a.here("vdu5_home");
    copy(a, &[(vdu::GRAPHICS_WINDOW + 6, &[PY])], 2);
    a.here("vdu5_return");
    copy(a, &[(vdu::GRAPHICS_WINDOW, &[PX])], 2);
    a.here("vdu5_save");
    a.jsr("point_internal");
    a.jmp("pixel_cursor");

    // DEL: back a cell, which is then filled in the background colour.
    a.here("vdu5_delete");
    a.jsr("vdu5_cursor");
    a.ldy(imm(0));
    a.ldx(imm(0xF8));
    a.jsr("vdu5_step");
    a.jsr("vdu5_save");
    a.lda(abs(vdu::GRAPHICS_BACKGROUND));
    a.ldx(imm(0));
    a.jsr("action_masks");
    a.lda(imm(0xFF));
    vdu5_cell(a);
    vdu5_step(a);
}

/// The cell at the cursor: the glyph at (&DE)'s pixels, or every pixel
/// with A &FF.
fn vdu5_cell(a: &mut Asm) {
    a.here("vdu5_cell");
    a.sta(abs(GLYPH_SOLID));
    a.jsr("vdu5_cursor");
    a.ldy(imm(0));
    a.here("vdu5_row");
    a.sty(abs(GLYPH_ROW));
    a.lda(abs(GLYPH_SOLID));
    a.bne("vdu5_row_bits");
    a.lda(zp(vdu_zp::TEMP + 4).ind_y());
    a.here("vdu5_row_bits");
    a.sta(abs(GLYPH_BITS));
    for offset in 0..2 {
        a.lda(abs(PX + offset));
        a.pha();
    }
    a.ldx(imm(8));
    a.here("vdu5_pixel");
    a.asl(abs(GLYPH_BITS));
    a.bcc("vdu5_skip");
    a.txa();
    a.pha();
    a.jsr("plot_pixel");
    a.pla();
    a.tax();
    a.here("vdu5_skip");
    a.txa();
    a.pha();
    a.ldx(imm(1));
    a.jsr("step_px");
    a.pla();
    a.tax();
    a.dex();
    a.bne("vdu5_pixel");
    a.pla();
    a.sta(abs(PX + 1));
    a.pla();
    a.sta(abs(PX));
    a.ldx(imm(0xFF));
    a.jsr("step_py");
    a.ldy(abs(GLYPH_ROW));
    a.iny();
    a.cpy(imm(8));
    a.bne("vdu5_row");
    a.rts();

    // (PX, PY) = the cursor, in pixels; X is kept.
    a.here("vdu5_cursor");
    a.ldy(imm(3));
    let next = a.here_local();
    a.lda(abs(vdu::GRAPHICS_INTERNAL).y());
    a.sta(abs(PX).y());
    a.dey();
    a.bpl(next);
    a.rts();
}

/// Axis Y's coordinate a cell on by X, 8 or -8. A step that takes the
/// cursor out of the window wraps it to the cell at the other edge: a
/// cell's top left, so seven in from the right edge or up from the bottom
/// one; an x wrap carries y a line the other way.
fn vdu5_step(a: &mut Asm) {
    a.here("vdu5_step");
    a.tya();
    a.pha();
    a.jsr("pixel_inside");
    a.pla();
    a.tay();
    a.php();
    a.jsr("point_step");
    a.plp();
    a.bcc(RTS);
    a.tya();
    a.pha();
    a.jsr("pixel_inside");
    a.pla();
    a.tay();
    a.bcs(RTS);
    a.txa();
    a.bmi("vdu5_step_back");
    a.lda(abs(vdu::GRAPHICS_WINDOW).y());
    a.sta(abs(PX).y());
    a.lda(abs(vdu::GRAPHICS_WINDOW + 1).y());
    a.sta(abs(PX + 1).y());
    a.cpy(imm(2));
    a.bne("vdu5_step_x_wrapped");
    a.ldx(imm(7));
    a.jmp("point_step");
    a.here("vdu5_step_x_wrapped");
    a.ldx(imm(0xF8));
    a.bne("vdu5_step_line");
    a.here("vdu5_step_back");
    a.lda(abs(vdu::GRAPHICS_WINDOW + 4).y());
    a.sta(abs(PX).y());
    a.lda(abs(vdu::GRAPHICS_WINDOW + 5).y());
    a.sta(abs(PX + 1).y());
    a.cpy(imm(2));
    a.beq(RTS);
    a.ldx(imm(0xF9));
    a.jsr("point_step");
    a.ldx(imm(8));
    a.here("vdu5_step_line");
    a.ldy(imm(2));
    a.bne("vdu5_step");
}

/// VDU 23: a character's definition, or the Model B's 6845 registers, which
/// the Electron has none of, and the cursor; the rest to VDUV.
fn vdu_23(a: &mut Asm, model: Model) {
    a.here("vdu_23");
    a.lda(abs(QUEUE_LAST - 8));
    a.cmp(imm(32));
    a.bcs("define_character");
    a.cmp(imm(1));
    a.beq("vdu_23_cursor");
    a.bcs("vdu_23_other");
    // VDU 23, 0, register, value.
    if model == Model::ModelB {
        a.lda(abs(QUEUE_LAST - 7));
        a.sta(abs(sheila::CRTC_ADDRESS));
        a.lda(abs(QUEUE_LAST - 6));
        a.sta(abs(sheila::CRTC_DATA));
    }
    a.rts();
    a.here("vdu_23_cursor");
    a.lda(abs(QUEUE_LAST - 7));
    a.bne("vdu_23_cursor_on");
    a.jmp("vdu_cursor_hide");
    a.here("vdu_23_cursor_on");
    a.jmp("vdu_cursor_show");
    a.here("vdu_23_other");
    a.clc();
    a.jmp(abs(vector::VDU).ind());

    // A character's zone comes into RAM, its ROM definitions with it, the
    // first time one of them is defined.
    a.here("define_character");
    a.pha();
    for _ in 0..5 {
        a.lsr(Acc);
    }
    a.tax();
    a.lda(abs("zone_bit").x());
    a.bit(abs(vdu::FONT_FLAGS));
    a.bne("define_in_ram");
    a.ora(abs(vdu::FONT_FLAGS));
    a.sta(abs(vdu::FONT_FLAGS));
    a.lda(abs(vdu::FONT_ZONES - 1).x());
    a.sta(zp(vdu_zp::TEMP + 1));
    a.jsr("font_rom_page");
    a.sta(zp(vdu_zp::TEMP + 3));
    a.lda(imm(0));
    a.sta(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::TEMP + 2));
    a.tay();
    a.here("define_copy_zone");
    a.lda(zp(vdu_zp::TEMP + 2).ind_y());
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.iny();
    a.bne("define_copy_zone");
    a.here("define_in_ram");
    a.pla();
    a.jsr("glyph_address");
    a.ldy(imm(7));
    a.here("define_store");
    a.lda(abs(QUEUE_LAST - 7).y());
    a.sta(zp(vdu_zp::TEMP + 4).ind_y());
    a.dey();
    a.bpl("define_store");
    a.rts();

    // A = the ROM font's page for zone X: 160–255 repeat 32–127.
    a.here("font_rom_page");
    a.txa();
    a.sec();
    a.sbc(imm(1));
    a.and(imm(3));
    a.clc();
    a.adc(imm(hi("font")));
    a.rts();
    font_explode(a);
}

/// OSBYTE 20: every zone back on page &C, 128–255's in RAM there; then
/// X zones, 160 onward first and 32–127 last, each given a page from
/// OSHWM, which rises past them and returns in Y.
fn font_explode(a: &mut Asm) {
    a.here("font_explode");
    a.txa();
    a.pha();
    a.lda(imm(crate::rom::os::service::FONT));
    a.jsr("service_call");
    a.jsr("font_implode");
    a.lda(abs(var::PRIMARY_OSHWM));
    a.sta(abs(var::OSHWM));
    // The count applied, at most six, is the explosion OSBYTE 182 reads.
    a.pla();
    a.cmp(imm(EXPLODABLE + 1));
    a.bcc("font_explode_count");
    a.lda(imm(EXPLODABLE));
    a.here("font_explode_count");
    a.sta(abs(var::FONT_STATE));
    a.sta(zp(vdu_zp::TEMP));
    a.tay();
    a.beq("font_exploded");
    a.ldy(imm(0));
    a.here("font_explode_zone");
    a.ldx(abs("font_explosion").y());
    a.lda(abs(var::OSHWM));
    a.sta(abs(vdu::FONT_ZONES - 1).x());
    a.inc(abs(var::OSHWM));
    a.iny();
    a.cpy(zp(vdu_zp::TEMP));
    a.bne("font_explode_zone");
    a.here("font_exploded");
    a.ldy(abs(var::OSHWM));
    a.ldx(imm(0xFF));
    a.rts();
    a.here("font_explosion");
    a.bytes(&EXPLOSION);

    a.here("font_implode");
    a.lda(imm(FONT_DEFAULT_FLAGS));
    a.sta(abs(vdu::FONT_FLAGS));
    a.ldx(imm(7));
    a.lda(imm(hi(buffer::FONT_RAM)));
    a.here("font_implode_zone");
    a.sta(abs(vdu::FONT_ZONES - 1).x());
    a.dex();
    a.bne("font_implode_zone");
    a.rts();
}

/// POINT: A = the logical colour of pixel (PX, PY), or &FF off the window.
fn read_pixel(a: &mut Asm) {
    a.here("read_pixel");
    a.jsr("pixel_inside");
    a.bcs("read_pixel_inside");
    a.lda(imm(0xFF));
    a.rts();
    a.here("read_pixel_inside");
    a.jsr("pixel_address");
    a.ldy(imm(0));
    a.lda(zp(vdu_zp::GRAPHICS_CELL).ind_y());
    a.and(zp(vdu_zp::PIXEL_MASK));
    a.sta(zp(vdu_zp::TEMP));
    // Collect the pixel's bits, highest first, into a colour.
    a.lda(imm(0));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.lda(zp(vdu_zp::PIXEL_MASK));
    a.sta(zp(vdu_zp::TEMP + 2));
    a.ldx(imm(8));
    a.here("read_pixel_bit");
    a.asl(zp(vdu_zp::TEMP));
    a.php();
    a.asl(zp(vdu_zp::TEMP + 2));
    a.bcc("read_pixel_not_its");
    a.plp();
    a.rol(zp(vdu_zp::TEMP + 1));
    a.jmp("read_pixel_next");
    a.here("read_pixel_not_its");
    a.plp();
    a.here("read_pixel_next");
    a.dex();
    a.bne("read_pixel_bit");
    a.lda(zp(vdu_zp::TEMP + 1));
    a.rts();
}
