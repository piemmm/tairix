use super::*;

#[test]
fn a_rectangle_s_rows_cover_it_whichever_corner_comes_first() {
    let screen = Rect {
        left: 0,
        bottom: 0,
        right: 639,
        top: 479,
    };
    let rows: Vec<Span> = rectangle((5, 9), (2, 7), screen).collect();
    assert_eq!(rows, [(7, 2, 5), (8, 2, 5), (9, 2, 5)]);
    let window = Rect {
        left: 3,
        bottom: 8,
        right: 4,
        top: 8,
    };
    assert_eq!(
        rectangle((5, 9), (2, 7), window).collect::<Vec<_>>(),
        [(8, 3, 4)],
        "within the window alone"
    );
    assert_eq!(
        rectangle((-(1 << 28), -(1 << 28)), (1 << 28, 1 << 28), window).count(),
        1
    );
}

#[test]
fn a_glyph_row_s_lit_bits_are_its_runs_at_any_size() {
    for size in [0, 1, 3, 8, 13, 16, 32_768] {
        for bits in [0u8, 0xFF, 0x81, 0x55, 0x3C, 0xA5, 0x18] {
            let runs = lit_runs(bits, size);
            let lit: Vec<i32> = runs.iter().flat_map(|&(from, to)| from..=to).collect();
            let each: Vec<i32> = (0..size.min(64))
                .filter(|&i| bits & (0x80 >> (i * 8 / size)) != 0)
                .map(u32::cast_signed)
                .collect();
            if size <= 64 {
                assert_eq!(lit, each, "{bits:#04x} at {size}");
            }
        }
    }
    assert_eq!(
        lit_runs(0xFF, 32_768)[0],
        (0, 32_767),
        "the whole width at once"
    );
}

#[test]
fn a_box_widens_over_each_drawn_part() {
    let a = widen(
        None,
        Rect {
            left: 5,
            bottom: 5,
            right: 5,
            top: 5,
        },
    );
    let b = widen(
        Some(a),
        Rect {
            left: 1,
            bottom: 7,
            right: 3,
            top: 9,
        },
    );
    assert_eq!(
        b,
        Rect {
            left: 1,
            bottom: 5,
            right: 5,
            top: 9
        }
    );
}
