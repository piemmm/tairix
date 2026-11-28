use alloc::vec;
use alloc::vec::Vec;

use super::*;

const WHITE: u32 = 0x00FF_FFFF;
const RED: u32 = 0x00FF_0000;

/// A small screen, its pixels twice as wide as they are high in OS units.
const SMALL: Screen = Screen {
    width: 8,
    height: 4,
    x_eig: 1,
    y_eig: 2,
    border: 0,
    blanked: false,
};

fn surface() -> Surface {
    Surface::new(SMALL.width, SMALL.height).unwrap()
}

/// `lines` lines of one colour, as a damage record holds them.
fn record(lines: u32, colour: u32) -> Vec<u8> {
    (0..lines * SMALL.width)
        .flat_map(|_| colour.to_le_bytes())
        .collect()
}

fn at(picture: &Surface, x: u32, y: u32) -> Pixel {
    let (_, row) = picture.row_span(y, x, 1).unwrap();
    row[0]
}

fn painted() -> (Picture, Surface) {
    let mut screen = Picture::new(SMALL).unwrap();
    let mut picture = surface();
    screen.damage(0, &record(SMALL.height, WHITE));
    screen.paint(&mut picture).unwrap();
    (screen, picture)
}

#[test]
fn a_new_screen_paints_black_and_whole() {
    let mut screen = Picture::new(SMALL).unwrap();
    let mut picture = surface();
    let damage = screen.paint(&mut picture).unwrap();
    assert_eq!(
        damage.rects(),
        &[DamageRect {
            x: 0,
            y: 0,
            width_px: 8,
            height_px: 4
        }]
    );
    assert_eq!(at(&picture, 3, 2), BLACK);
    assert_eq!(screen.paint(&mut picture), None, "nothing changed since");
}

#[test]
fn damage_paints_its_lines_and_names_them() {
    let (mut screen, mut picture) = painted();
    screen.damage(1, &record(2, RED));
    let damage = screen.paint(&mut picture).unwrap();
    assert_eq!(
        damage.rects(),
        &[DamageRect {
            x: 0,
            y: 1,
            width_px: 8,
            height_px: 2
        }]
    );
    assert_eq!(at(&picture, 0, 0), opaque(WHITE));
    assert_eq!(at(&picture, 7, 2), opaque(RED));
    assert_eq!(at(&picture, 0, 3), opaque(WHITE));
}

#[test]
fn the_cursor_inverts_its_lines_of_the_cell_and_never_the_guest_s() {
    let (mut screen, mut picture) = painted();
    let cursor = Cursor {
        x: 2,
        y: 0,
        width: 2,
        top: 2,
        bottom: 3,
        flash: Flash::Steady,
    };
    screen.set_cursor(Some(cursor));
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 2, 2), BLACK, "white inverted");
    assert_eq!(at(&picture, 3, 3), BLACK);
    assert_eq!(at(&picture, 4, 2), opaque(WHITE), "beside the cell");
    assert_eq!(at(&picture, 2, 1), opaque(WHITE), "above its lines");
    screen.set_cursor(None);
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 2, 2), opaque(WHITE), "the guest's again");
}

#[test]
fn damage_under_the_cursor_keeps_it_drawn() {
    let (mut screen, mut picture) = painted();
    screen.set_cursor(Some(Cursor {
        x: 0,
        y: 0,
        width: 1,
        top: 3,
        bottom: 3,
        flash: Flash::Steady,
    }));
    screen.paint(&mut picture).unwrap();
    screen.damage(3, &record(1, RED));
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 3), opaque(0x0000_FFFF), "red inverted");
    assert_eq!(at(&picture, 1, 3), opaque(RED));
}

#[test]
fn only_a_flashing_cursor_turns_over() {
    let (mut screen, mut picture) = painted();
    let cursor = |flash| Cursor {
        x: 0,
        y: 0,
        width: 1,
        top: 0,
        bottom: 0,
        flash,
    };
    screen.set_cursor(Some(cursor(Flash::Steady)));
    screen.paint(&mut picture).unwrap();
    screen.flash();
    assert_eq!(screen.paint(&mut picture), None, "a steady cursor stands");
    screen.set_cursor(Some(cursor(Flash::Fast)));
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 0), BLACK, "lit afresh");
    screen.flash();
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 0), opaque(WHITE), "unlit");
    screen.flash();
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 0), BLACK, "lit again");
}

#[test]
fn the_pointer_is_drawn_in_its_colours_clear_where_nought_and_clipped() {
    let (mut screen, mut picture) = painted();
    let pixels = [1, 0, 2, 3];
    let shape = Shape {
        width: 2,
        height: 2,
        active: (1, 1),
        colours: [RED, 0x0000_FF00, 0x0000_00FF],
        pixels: &pixels,
    };
    assert!(screen.set_pointer(Some(&shape)));
    assert!(screen.shows_pointer());
    screen.set_pointer_at(4, 2);
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 3, 1), opaque(RED));
    assert_eq!(at(&picture, 4, 1), opaque(WHITE), "nought is clear");
    assert_eq!(at(&picture, 3, 2), opaque(0x0000_FF00));
    assert_eq!(at(&picture, 4, 2), opaque(0x0000_00FF));
    screen.set_pointer_at(0, 0);
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 3, 1), opaque(WHITE), "its old place restored");
    assert_eq!(
        at(&picture, 0, 0),
        opaque(0x0000_00FF),
        "off the screen's edge"
    );
    screen.set_pointer(None);
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 0), opaque(WHITE));
    assert!(!screen.shows_pointer());
}

#[test]
fn a_blanked_screen_is_black_and_shows_nothing_over_it() {
    let (mut screen, mut picture) = painted();
    screen.set_cursor(Some(Cursor {
        x: 0,
        y: 0,
        width: 1,
        top: 0,
        bottom: 0,
        flash: Flash::Steady,
    }));
    assert!(screen.reshape(Screen {
        blanked: true,
        ..SMALL
    }));
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 0, 0), BLACK);
    assert_eq!(at(&picture, 5, 3), BLACK);
    assert!(screen.reshape(SMALL));
    screen.paint(&mut picture).unwrap();
    assert_eq!(at(&picture, 5, 3), opaque(WHITE), "the lines were kept");
}

#[test]
fn a_new_size_starts_black_and_a_picture_of_the_old_paints_nothing() {
    let (mut screen, mut picture) = painted();
    let larger = Screen { width: 16, ..SMALL };
    assert!(screen.reshape(larger));
    assert_eq!(screen.paint(&mut picture), None, "the old size's picture");
    let mut picture = Surface::new(16, 4).unwrap();
    let damage = screen.paint(&mut picture).unwrap();
    assert_eq!(damage.rects()[0].width_px, 16);
    assert_eq!(at(&picture, 3, 0), BLACK);
}

#[test]
fn the_picture_is_placed_at_the_mode_s_pixel_aspect() {
    let screen = Picture::new(Screen {
        width: 640,
        height: 256,
        ..SMALL
    })
    .unwrap();
    let placed = screen.placement((1000, 1000)).unwrap();
    assert_eq!(
        (placed.x, placed.y, placed.width, placed.height),
        (0, 100, 1000, 800),
        "1280 by 1024 OS units"
    );
}

#[test]
fn damage_past_the_screen_is_ignored() {
    let mut screen = Picture::new(SMALL).unwrap();
    screen.damage(9, &record(1, RED));
    screen.damage(u32::MAX, &record(1, RED));
    let mut picture = surface();
    screen.paint(&mut picture).unwrap();
    let rows: Vec<Pixel> = (0..SMALL.height).map(|row| at(&picture, 0, row)).collect();
    assert_eq!(rows, vec![BLACK; 4]);
}
