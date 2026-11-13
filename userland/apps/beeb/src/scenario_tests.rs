use alloc::string::String;
use alloc::vec;

use super::*;

const CORPUS: &str = "# A comment.

scenario first
discs testdisc blank80
protected
check 0
check 1
script type:CLS\\n;wait:1

# The second, ours.
scenario second
ours
script wait:0.5
";

#[test]
fn a_corpus_reads_each_scenario_and_its_fields() {
    let scenarios = parse_corpus(CORPUS).expect("the corpus reads");
    assert_eq!(scenarios.len(), 2);
    let first = &scenarios[0];
    assert_eq!(first.name, "first");
    assert_eq!(first.discs, ["testdisc", "blank80"]);
    assert!(first.protected && !first.ours);
    assert_eq!(first.checked, [PhysicalDrive::Zero, PhysicalDrive::One]);
    assert_eq!(first.script, "type:CLS\\n;wait:1");
    assert!(scenarios[1].ours && scenarios[1].discs.is_empty());
}

#[test]
fn a_corpus_refuses_what_it_cannot_hold() {
    for (text, line) in [
        ("discs testdisc", 1),
        ("scenario a\nscript wait:1\nscenario a\nscript wait:1", 3),
        ("scenario a\nwhere x", 2),
        ("scenario a\ndiscs a b c", 2),
        ("scenario a\ndiscs a  b", 2),
        ("scenario a\ncheck 2", 2),
        ("scenario a\ncheck 0\ncheck 0", 3),
        ("scenario a\nscript wait:1\nscript wait:2", 3),
        ("scenario", 1),
    ] {
        assert_eq!(
            parse_corpus(text),
            Err(ScenarioError::Line(line)),
            "{text:?}"
        );
    }
    assert_eq!(
        parse_corpus("scenario a\ndiscs blank80"),
        Err(ScenarioError::Unmatched(String::from("a")))
    );
}

#[test]
fn fixtures_survive_being_written_and_read_back() {
    let fixtures = vec![
        (
            String::from("first"),
            Observation {
                rows: vec![String::from(">P.\\X"), String::from("\u{1}\u{7f} end")],
                discs: vec![(PhysicalDrive::One, [0xA5; 32])],
            },
        ),
        (
            String::from("second"),
            Observation {
                rows: vec![String::from(">")],
                discs: vec![],
            },
        ),
    ];
    let text = format_fixtures(&["Recorded."], &fixtures);
    assert!(text.starts_with("# Recorded.\n"));
    assert!(
        text.contains("row >P.\\\\X\nrow \\x01\\x7f end\n"),
        "{text}"
    );
    assert_eq!(parse_fixtures(&text), Ok(fixtures));
}

#[test]
fn fixtures_refuse_what_they_cannot_hold() {
    let digest = "a5".repeat(32);
    for (text, line) in [
        (String::from("row x"), 1),
        (String::from("scenario a\nrow bad\\q"), 2),
        (String::from("scenario a\nrow bad\\x4"), 2),
        (String::from("scenario a\ndisc 2 ") + &digest, 2),
        (String::from("scenario a\ndisc 0 abc"), 2),
        (String::from("scenario a\ndisc 0 ") + &"zz".repeat(32), 2),
        (String::from("scenario a\nscenario a"), 2),
    ] {
        assert_eq!(
            parse_fixtures(&text),
            Err(ScenarioError::Line(line)),
            "{text:?}"
        );
    }
}

#[test]
fn own_names_replace_the_originals() {
    assert_eq!(
        own_names("BBC Computer\nAcorn DFS\n"),
        "BBC Computer\nTAIRiX DFS\n"
    );
}

#[test]
fn a_wait_is_seconds_to_the_centisecond_and_ten_minutes_at_most() {
    assert_eq!(centiseconds("2"), Ok(200));
    assert_eq!(centiseconds("0.5"), Ok(50));
    assert_eq!(centiseconds("0.05"), Ok(5));
    assert_eq!(centiseconds("600"), Ok(60_000));
    for refused in ["1.234", "x", "1.x", "601", "184467440737095516", ""] {
        assert!(centiseconds(refused).is_err(), "{refused}");
    }
}

#[test]
fn a_step_no_script_may_take_is_refused() {
    let os = crate::rom::mos::build().expect("the MOS assembles").bytes;
    let mut machine = Machine::new(&os).expect("a 16 KiB OS");
    for step in ["jump:1", "press:NOKEY", "type:lower", "swap:nodisc"] {
        assert!(step_on(&mut machine, step).is_err(), "{step}");
    }
    assert_eq!(step_on(&mut machine, ""), Ok(()));
}

#[test]
fn each_disc_named_is_an_image_of_its_sectors() {
    for (name, sectors) in [
        ("testdisc", 800),
        ("blank40", 400),
        ("nogap", 800),
        ("bigcapped", 800),
    ] {
        let image = discs::named(name).expect("a disc of that name");
        assert_eq!(image.len(), sectors * 256, "{name}");
    }
    assert_eq!(discs::named("nothing"), None);
}

#[test]
fn hex_is_two_lower_case_digits_a_byte() {
    assert_eq!(hex(&[0x00, 0xA5, 0x0F]), "00a50f");
}
