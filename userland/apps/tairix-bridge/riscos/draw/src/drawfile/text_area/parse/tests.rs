use alloc::vec::Vec;

use super::*;

/// Every piece of `text`, or the refusal reading it met.
fn pieces(text: &[u8]) -> Result<Vec<Piece<'_>>, Refusal> {
    let mut reader = Reader::new(text);
    let mut all = Vec::new();
    while let Some(piece) = reader.next()? {
        all.push(piece);
    }
    Ok(all)
}

fn command(text: &[u8]) -> Result<Command<'_>, Refusal> {
    match pieces(text)?.as_slice() {
        [Piece::Command(command)] => Ok(*command),
        other => panic!("one command, not {other:?}"),
    }
}

#[test]
fn the_version_reads_as_nothing_and_must_be_one() {
    assert_eq!(pieces(b"\\! 1\na"), Ok(alloc::vec![Piece::Char(b'a')]));
    assert_eq!(pieces(b"\\! 1/a"), Ok(alloc::vec![Piece::Char(b'a')]));
    assert_eq!(pieces(b"\\! 2\na"), Err(Refusal::Version(2)));
    assert!(Reader::new(b"\\! 1").versioned());
    assert!(!Reader::new(b"Hello").versioned());
}

#[test]
fn text_keeps_its_characters_and_drops_controls_but_tab_and_newline() {
    // RISC OS 5.30 painted "aa\x01bb\x7Fcc\x0Ddd" as "aabb\x7Fccdd".
    let read = pieces(b"a\x01b\x7F\x0Dc\td\ne").expect("read");
    assert_eq!(
        read,
        [b'a', b'b', 0x7F, b'c', b' ', b'd']
            .map(Piece::Char)
            .into_iter()
            .chain([Piece::Newline, Piece::Char(b'e')])
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_escapes_without_arguments() {
    assert_eq!(
        pieces(b"\\-\\\n\\\\\\;ignored\nx"),
        Ok(alloc::vec![
            Piece::SoftHyphen,
            Piece::Break,
            Piece::Char(b'\\'),
            Piece::Char(b'x')
        ])
    );
}

#[test]
fn numbers_may_be_signed_and_must_have_digits() {
    assert_eq!(command(b"\\L -5/"), Ok(Command::Leading(-5)));
    assert_eq!(command(b"\\L +5/"), Ok(Command::Leading(5)));
    assert_eq!(command(b"\\P -5/"), Ok(Command::Paragraph(-5)));
    assert_eq!(command(b"\\M -1 2/"), Ok(Command::Margins(-1, 2)));
    assert_eq!(command(b"\\L/"), Err(Refusal::Command(b'L')));
    assert_eq!(
        command(b"\\L 10.5/"),
        Err(Refusal::Command(b'L')),
        "no fractions"
    );
    assert_eq!(
        command(b"\\M 1/"),
        Err(Refusal::Command(b'M')),
        "two numbers"
    );
}

#[test]
fn colours_take_three_channels_each_within_a_byte() {
    assert_eq!(
        command(b"\\C 300 -5 999/"),
        Ok(Command::Colour {
            background: false,
            rgb: [255, 0, 255]
        })
    );
    assert_eq!(
        command(b"\\B 1 2 3\n"),
        Ok(Command::Colour {
            background: true,
            rgb: [1, 2, 3]
        })
    );
    assert_eq!(command(b"\\C 255 0/"), Err(Refusal::Command(b'C')));
    assert_eq!(command(b"\\C/"), Err(Refusal::Command(b'C')));
}

#[test]
fn underlining_takes_a_position_and_thickness_or_a_full_stop() {
    assert_eq!(
        command(b"\\U 200 300/"),
        Ok(Command::Underline(Some((127, 255))))
    );
    assert_eq!(
        command(b"\\U -129 20/"),
        Ok(Command::Underline(Some((-128, 20))))
    );
    assert_eq!(command(b"\\U./"), Ok(Command::Underline(None)));
    assert_eq!(command(b"\\U ./"), Ok(Command::Underline(None)));
    assert_eq!(command(b"\\U.\n"), Ok(Command::Underline(None)));
    assert_eq!(command(b"\\U 10/"), Err(Refusal::Command(b'U')));
    assert_eq!(command(b"\\U 10 20 30/"), Err(Refusal::Command(b'U')));
    assert_eq!(
        pieces(b"\\U. bb"),
        Err(Refusal::Command(b'U')),
        "nothing but an end after the stop"
    );
    assert_eq!(pieces(b"\\U.x"), Err(Refusal::Command(b'U')));
}

#[test]
fn a_move_takes_digits_and_an_optional_solidus() {
    assert_eq!(command(b"\\V 33"), Ok(Command::Move(33)));
    assert_eq!(command(b"\\V -3/"), Ok(Command::Move(-3)));
    assert_eq!(command(b"\\V+3"), Ok(Command::Move(3)));
    assert_eq!(
        pieces(b"\\V 3.5"),
        Ok(alloc::vec![
            Piece::Command(Command::Move(3)),
            Piece::Char(b'.'),
            Piece::Char(b'5')
        ])
    );
    assert_eq!(pieces(b"\\V- 3/"), Err(Refusal::Command(b'V')));
    assert_eq!(pieces(b"\\V-"), Err(Refusal::Command(b'V')));
    assert_eq!(pieces(b"\\Vx"), Err(Refusal::Command(b'V')));
}

#[test]
fn alignment_codes_are_one_letter_of_either_case() {
    assert_eq!(command(b"\\AR"), Ok(Command::Align(Align::Right)));
    assert_eq!(command(b"\\Ac/"), Ok(Command::Align(Align::Centre)));
    assert_eq!(command(b"\\AD"), Ok(Command::Align(Align::Double)));
    assert_eq!(command(b"\\Al"), Ok(Command::Align(Align::Left)));
    assert_eq!(pieces(b"\\AX"), Err(Refusal::Command(b'A')));
    assert_eq!(pieces(b"\\A"), Err(Refusal::Command(b'A')));
}

#[test]
fn a_font_is_defined_by_number_name_height_and_width() {
    assert_eq!(
        command(b"\\F 2 Lay 20 12/"),
        Ok(Command::Define {
            number: 2,
            name: b"Lay",
            width: 192,
            height: 320
        })
    );
    assert_eq!(
        command(b"\\F02 Homerton.Medium\\ELatin1 10\n"),
        Ok(Command::Define {
            number: 2,
            name: b"Homerton.Medium\\ELatin1",
            width: 160,
            height: 160
        })
    );
    // Sizes below a point are a point; numbers past 99 are 99.
    assert_eq!(
        command(b"\\F 100 Lay 0/"),
        Ok(Command::Define {
            number: 99,
            name: b"Lay",
            width: 16,
            height: 16
        })
    );
    assert_eq!(
        command(b"\\F 2 Lay -5/"),
        Ok(Command::Define {
            number: 2,
            name: b"Lay",
            width: 16,
            height: 16
        })
    );
    for refused in [
        &b"\\F 2 Lay/"[..],
        b"\\F x Lay 10/",
        b"\\F 1 Lay 10.5/",
        b"\\F 1 Lay 10 /",
        b"\\F 2  Lay  10  12 /",
    ] {
        assert_eq!(pieces(refused), Err(Refusal::Command(b'F')), "{refused:?}");
    }
}

#[test]
fn a_font_change_takes_its_digits_up_to_99_and_an_optional_solidus() {
    assert_eq!(command(b"\\1"), Ok(Command::Select(1)));
    assert_eq!(command(b"\\02/"), Ok(Command::Select(2)));
    assert_eq!(command(b"\\200"), Ok(Command::Select(99)));
}

#[test]
fn a_foreign_command_is_refused_and_a_final_one_may_lack_its_end() {
    assert_eq!(pieces(b"\\Q"), Err(Refusal::Command(b'Q')));
    assert_eq!(command(b"\\L 10"), Ok(Command::Leading(10)));
    assert_eq!(pieces(b"a\\"), Ok(alloc::vec![Piece::Char(b'a')]));
}
