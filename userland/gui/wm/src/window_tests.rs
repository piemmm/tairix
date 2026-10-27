use super::*;

/// A surface whose rows hold the given alphas, each pixel grey at its alpha.
fn alphas(rows: &[&[u8]]) -> Surface {
    let width = u32::try_from(rows[0].len()).expect("a width");
    let height = u32::try_from(rows.len()).expect("a height");
    let mut surface = Surface::new(width, height).expect("a surface");
    for (y, alphas) in (0..height).zip(rows) {
        let (_, row) = surface.row_span_mut(y, 0, width).expect("a row");
        for (pixel, &a) in row.iter_mut().zip(alphas.iter()) {
            *pixel = Pixel {
                r: a,
                g: a,
                b: a,
                a,
            };
        }
    }
    surface
}

fn region(rect: Option<Rect>) -> Region {
    let mut region = Region::new();
    if let Some(rect) = rect {
        region.add(rect);
    }
    region
}

#[test]
fn each_row_is_boxed_where_it_is_not_opaque() {
    let rows: [&[u8]; 6] = [
        &[255, 255, 255, 255, 255, 255],
        &[128, 128, 128, 128, 128, 128],
        &[255, 255, 0, 0, 255, 255],
        &[255, 64, 0, 0, 200, 255],
        &[255, 255, 255, 255, 255, 0],
        &[0, 255, 255, 255, 255, 9],
    ];
    let content = alphas(&rows);
    let expected: [Option<Rect>; 6] = [
        None,
        Some(Rect::new(0, 1, 6, 1)),
        Some(Rect::new(2, 2, 2, 1)),
        Some(Rect::new(1, 3, 4, 1)),
        Some(Rect::new(5, 4, 1, 1)),
        Some(Rect::new(0, 5, 6, 1)),
    ];
    for (y, transparent) in (0..6).zip(expected) {
        let mut seen = Region::new();
        note_transparent(&content, Rect::new(0, y, 6, 1), &mut seen);
        assert_eq!(seen, region(transparent), "row {y}");
    }
}

#[test]
fn a_window_is_opaque_only_while_tracked_and_wholly_painted() {
    let mut window = Window::new(
        WindowId(1),
        Point::ORIGIN,
        alphas(&[&[255, 255], &[255, 255]]),
    );
    assert_eq!(
        window.is_opaque(),
        None,
        "nothing is known until it is tracked"
    );
    window.track_transparency();
    assert_eq!(window.is_opaque(), Some(true));

    let (fresh, established) = window.content_for_present(3, 2).expect("a fresh frame");
    assert!(established);
    fresh.fill_rect(0, 0, 2, 2, Color::rgb(9, 9, 9));
    window.content_changed(Rect::new(0, 0, 2, 2));
    assert_eq!(
        window.is_opaque(),
        Some(false),
        "the column the client left unpainted is clear"
    );
    let (held, established) = window.content_for_present(3, 2).expect("the same frame");
    assert!(!established);
    held.fill_rect(2, 0, 1, 2, Color::rgb(9, 9, 9));
    window.content_changed(Rect::new(2, 0, 1, 2));
    assert_eq!(window.is_opaque(), Some(true));

    let (held, _) = window.content_for_present(3, 2).expect("the same frame");
    held.fill_rect(1, 1, 1, 1, Color::rgba(9, 9, 9, 128));
    window.content_changed(Rect::new(1, 1, 1, 1));
    assert_eq!(
        window.is_opaque(),
        Some(false),
        "a rewrite is read again where it landed"
    );

    window.forget_transparency();
    assert_eq!(window.is_opaque(), None);
}
