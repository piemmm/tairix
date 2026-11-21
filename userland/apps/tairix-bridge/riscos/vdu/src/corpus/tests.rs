use alloc::string::String;
use alloc::vec;

use super::*;

#[test]
fn a_case_s_lines_lie_between_the_frame_s_and_ascend() {
    assert_eq!(
        cases("case a\n10 MODE 1\n20 PRINT\n"),
        Ok(vec![Case {
            name: "a".into(),
            text: "10 MODE 1\n20 PRINT\n".into()
        }])
    );
    for refused in [
        "case a\n5 MODE 1\n",
        "case a\n9000 MODE 1\n",
        "case a\n20 MODE 1\n10 PRINT\n",
        "case a\ncase b\n10 PRINT\n",
        "case A\n10 PRINT\n",
        "10 PRINT\n",
        "case a\n",
    ] {
        assert!(cases(refused).is_err(), "{refused:?}");
    }
}

#[test]
fn the_program_defines_every_character_runs_the_case_then_records() {
    let case = Case {
        name: "a".into(),
        text: "10 CLS\n".into(),
    };
    let program = program(&case, 3);
    let lines: alloc::vec::Vec<&str> = program.lines().collect();
    assert!(lines[0].contains("HostFS:$.ScrRec3"));
    let at = lines.iter().position(|l| *l == "10 CLS").expect("the case");
    assert!(lines[at + 1].starts_with("9000 "));
    assert!(program.contains("HostFS:$.ScrDump3"));
    let data: alloc::vec::Vec<u32> = lines
        .iter()
        .filter_map(|l| l.split_once(" DATA "))
        .flat_map(|(_, values)| values.split(',').map(|v| v.parse().expect("a byte")))
        .collect();
    let font: alloc::vec::Vec<u32> = tairix_vdu::font::RISC_OS
        .iter()
        .flatten()
        .map(|&b| u32::from(b))
        .collect();
    assert_eq!(
        data, font,
        "every glyph from 32 to 255, the first-party font's"
    );
    assert!(
        lines.iter().all(|l| l.len() <= 238),
        "each line BASIC can hold"
    );
}

#[test]
fn a_screen_line_is_written_as_runs_and_bytes_and_read_back() {
    let bytes = [0, 0, 0, 0, 0xFF, 0x12, 7, 7, 7, 7, 7, 0x0F, 0x0F];
    let text = encode_row(&bytes, 1);
    assert_eq!(text, "4x00 FF12 5x07 0F0F");
    assert_eq!(decode_row(&text, bytes.len(), 1), Ok(bytes.to_vec()));
    assert!(
        decode_row(&text, bytes.len() - 1, 1).is_err(),
        "longer than a line"
    );
    assert!(decode_row("0F0", 4, 1).is_err());
    assert!(decode_row("3xZZ", 4, 1).is_err());
    let pixels = [0x1F, 0, 0x1F, 0, 0x1F, 0, 0xE0, 3];
    let text = encode_row(&pixels, 2);
    assert_eq!(text, "3x1F00 E003", "runs of whole 16-bit pixels");
    assert_eq!(decode_row(&text, pixels.len(), 2), Ok(pixels.to_vec()));
    assert!(
        decode_row("3x1F", 8, 2).is_err(),
        "a run of part of a pixel"
    );
    assert_eq!(unit(5), 4);
    assert_eq!(unit(0), 1);
}

#[test]
fn what_a_case_recorded_goes_to_the_answers_and_comes_back() {
    let screen = Screen {
        width: 8,
        height: 5,
        log2_bpp: 0,
        line_length: 4,
        bytes: vec![0, 0, 0, 0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 9, 0],
    };
    let record = b"pos 5\nvpos 2\nscreen 8 5 0 4\n";
    let recorded = recorded(record, Some(&screen.bytes)).expect("recorded");
    assert_eq!(recorded.lines, ["pos 5", "vpos 2"]);
    assert_eq!(recorded.screen.as_ref(), Some(&screen));
    let case = Case {
        name: "a".into(),
        text: "10 CLS\n".into(),
    };
    let text = answers(
        &[String::from("version 530")],
        &[case],
        core::slice::from_ref(&recorded),
    );
    assert!(text.starts_with("# version 530\n"));
    assert!(!text.contains("row 0 "), "a line wholly zero is left out");
    assert!(text.contains("row 1-3 01020304\n"), "{text}");
    let answers = parse_answers(&text).expect("read back");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].name, "a");
    assert_eq!(answers[0].recorded, recorded);
}

#[test]
fn a_recording_short_of_its_screen_or_its_font_is_refused() {
    assert!(recorded(b"screen 8 3 0 4\n", Some(&[0; 11])).is_err());
    assert!(recorded(b"screen 8 3 0 4\n", None).is_err());
    assert!(recorded(b"font lost\n", None).is_err());
    assert!(recorded(&[0xFF], None).is_err());
    assert!(
        parse_answers("r x 1\n").is_err(),
        "a reading outside a case"
    );
    assert!(
        parse_answers("case a\nrow 0 00\n").is_err(),
        "a line before its size"
    );
    assert!(
        parse_answers("case a\nscreen 8 1 0 1\nrow 1 00\n").is_err(),
        "a line past the screen"
    );
    assert!(parse_answers("case a\nscreen 8 2 0 1\nrow 1-0 00\n").is_err());
    assert!(parse_answers("case a\nscreen 8 2 0 1\nrow 0-2 00\n").is_err());
}
