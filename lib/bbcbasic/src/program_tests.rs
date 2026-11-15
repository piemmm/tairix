use alloc::vec::Vec;

use super::*;

#[test]
fn an_image_is_each_line_s_cr_number_length_and_text_then_cr_ff() {
    let lines = [
        Line {
            number: 10,
            text: b"\xF1\"A\"",
        },
        Line {
            number: 300,
            text: b"",
        },
    ];
    let built = image(lines).unwrap();
    assert_eq!(
        built,
        [0x0D, 0, 10, 8, 0xF1, b'"', b'A', b'"', 0x0D, 1, 44, 4, 0x0D, 0xFF]
    );
    let read: Vec<Line<'_>> = super::lines(&built).map(Result::unwrap).collect();
    assert_eq!(read, lines);
    assert_eq!(image([]), Ok(Vec::from([0x0D, 0xFF])));
}

#[test]
fn a_program_whose_lines_do_not_chain_is_bad() {
    for bad in [
        &[0x0D, 0, 10, 3, 0x0D, 0xFF][..],
        &[0x0D, 0, 10, 9, b'x', 0x0D, 0xFF],
        &[0x0D, 0, 10, 5, b'x', b'y', 0x0D, 0xFF],
        &[0x0D, 0, 10],
        b"x",
        &[],
    ] {
        let read: Vec<_> = lines(bad).collect();
        assert_eq!(read.last(), Some(&Err(Error::BadProgram)), "{bad:02X?}");
    }
    assert_eq!(
        lines(&[0x0D, 0xFF, 0x0D]).count(),
        0,
        "nothing past the end"
    );
}

#[test]
fn an_image_refuses_lines_out_of_order_too_long_or_numbered_past_the_end() {
    let line = |number, text: &'static [u8]| Line { number, text };
    assert_eq!(
        image([line(20, b""), line(10, b"")]),
        Err(Error::BadProgram)
    );
    assert_eq!(
        image([line(10, b""), line(10, b"")]),
        Err(Error::BadProgram)
    );
    assert_eq!(image([line(0xFF00, b"")]), Err(Error::BadProgram));
    assert_eq!(
        image([line(1, &[b'x'; MOST_TEXT + 1])]),
        Err(Error::LineTooLong)
    );
    assert!(image([line(1, &[b'x'; MOST_TEXT])]).is_ok());
    assert_eq!(last_line(Dialect::BasicII), 32_767);
    assert_eq!(last_line(Dialect::BasicV), 65_279);
}

#[test]
fn a_stored_line_is_trimmed_and_its_first_else_made_the_block_one_by_basic_v_only() {
    let text = b"  \x8B X  ";
    assert_eq!(
        stored(Dialect::BasicII, text, Options::default()),
        Ok(text.to_vec())
    );
    assert_eq!(
        stored(Dialect::BasicV, text, Options::default()),
        Ok(b"  \xCC X".to_vec())
    );
    assert_eq!(
        stored(Dialect::BasicV, text, Options::new(2)),
        Ok(b"\xCC X".to_vec())
    );
    assert_eq!(
        stored(Dialect::BasicV, b"X \x8B", Options::default()),
        Ok(b"X \x8B".to_vec())
    );
    assert_eq!(
        stored(Dialect::BasicV, b"   ", Options::new(1)),
        Ok(Vec::new())
    );
    assert_eq!(
        stored(Dialect::BasicV, b"   ", Options::default()),
        Ok(b" ".to_vec()),
        "the first byte stays"
    );
    assert_eq!(
        stored(Dialect::BasicV, b"A  ", Options::default()),
        Ok(b"A".to_vec())
    );
    assert_eq!(
        stored(Dialect::BasicV, &[b'x'; MOST_TEXT + 1], Options::default()),
        Err(Error::LineTooLong)
    );
}
