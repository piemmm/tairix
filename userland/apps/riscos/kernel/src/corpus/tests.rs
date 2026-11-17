use super::*;
use alloc::vec;

fn kept(name: &str, offset: i32) -> Value {
    Value::Kept {
        name: name.into(),
        offset,
    }
}

#[test]
fn scenarios_parse_their_calls_kept_values_and_shows() {
    let text = "# areas\nscenario make-one\ncall XOS_DynamicArea r0=0 r1=-1 r2=&2000 r8=\"A title\" # made\nkeep r1 area\ncall XOS_DynamicArea r0=2 r1=$area r2=$area+&10\nshow r2 r5 v c\n\nscenario ours-one ours\ncall OS_ReadMonotonicTime\n";
    let scenarios = parse_scenarios(text).unwrap();
    assert_eq!(scenarios.len(), 2);
    assert_eq!(
        scenarios[0].steps,
        vec![
            Step::Call {
                swi: "XOS_DynamicArea".into(),
                registers: vec![
                    (0, Value::Number(0)),
                    (1, Value::Number(u32::MAX)),
                    (2, Value::Number(0x2000)),
                    (8, Value::Text("A title".into())),
                ],
            },
            Step::Keep {
                register: 1,
                name: "area".into()
            },
            Step::Call {
                swi: "XOS_DynamicArea".into(),
                registers: vec![
                    (0, Value::Number(2)),
                    (1, kept("area", 0)),
                    (2, kept("area", 0x10)),
                ],
            },
            Step::Show {
                items: vec![
                    Item::Register(2),
                    Item::Register(5),
                    Item::Flag(Flag::V),
                    Item::Flag(Flag::C),
                ]
            },
        ]
    );
    assert_eq!(scenarios[1].kind, Kind::Ours);
    assert_eq!(scenarios[0].kind, Kind::Replayed);
}

#[test]
fn a_scenario_is_marked_ours_or_reference_or_nothing() {
    let text = "scenario read reference
call OS_ReadMonotonicTime
";
    assert_eq!(parse_scenarios(text).unwrap()[0].kind, Kind::Reference);
    let refused = parse_scenarios(
        "scenario odd mine
",
    )
    .unwrap_err();
    assert_eq!(
        refused.reason,
        "a scenario is named, and may be marked ours or reference"
    );
}

#[test]
fn blocks_are_made_written_and_shown_from_their_addresses() {
    let text = "scenario s\nblock buffer 64\nwrite $buffer+4 w=&12345678 b=13 s=\"a#b\" # a comment\ncall OS_ConvertHex8 r0=1 r1=$buffer r2=64\nshow r0-buffer r1:4 $buffer:9 $buffer+&4:2 $buffer-&1:1 v\n";
    let scenario = &parse_scenarios(text).unwrap()[0];
    assert_eq!(
        scenario.steps[..2],
        [
            Step::Block {
                name: "buffer".into(),
                size: 64
            },
            Step::Write {
                at: kept("buffer", 4),
                bytes: vec![0x78, 0x56, 0x34, 0x12, 13, b'a', b'#', b'b'],
            },
        ]
    );
    let Step::Show { items } = &scenario.steps[3] else {
        panic!("a show");
    };
    let keys: Vec<String> = items.iter().map(Item::key).collect();
    assert_eq!(
        keys,
        [
            "r0-buffer",
            "r1:4",
            "$buffer:9",
            "$buffer+&4:2",
            "$buffer-&1:1",
            "v"
        ]
    );
    assert!(items[0].undefined_after_error() && items[1].undefined_after_error());
    assert!(!items[2].undefined_after_error());
}

#[test]
fn a_malformed_scenario_is_refused_with_its_line() {
    for (text, line) in [
        ("call X", 1),
        ("scenario a\nshow r1", 2),
        ("scenario a\ncall X r10=1", 2),
        ("scenario a\ncall X r1=$nothing", 2),
        ("scenario a\ncall X r1=1 r1=2", 2),
        ("scenario a\nscenario a", 2),
        ("scenario a b c", 1),
        ("scenario a\ncall X r8=\"open", 2),
        ("scenario a\nmangle", 2),
        ("scenario a\ncall X\nshow flags", 3),
        ("scenario a\nblock b 0", 2),
        ("scenario a\nblock b 65537", 2),
        ("scenario a\nblock b-c 4", 2),
        ("scenario a\nwrite $b w=1", 2),
        ("scenario a\nblock b 4\nwrite &8000 w=1", 3),
        ("scenario a\nblock b 4\nwrite $b", 3),
        ("scenario a\nblock b 4\nwrite $b b=256", 3),
        ("scenario a\nblock b 4\nwrite $b q=1", 3),
        ("scenario a\nblock b 4\ncall X\nshow $b:0", 4),
        ("scenario a\nblock b 4\ncall X\nshow $b:65", 4),
        ("scenario a\nblock b 4\ncall X\nshow $c:4", 4),
        ("scenario a\ncall X\nshow r1-nothing", 3),
    ] {
        assert_eq!(
            parse_scenarios(text).map_err(|e| e.line),
            Err(line),
            "{text}"
        );
    }
}

#[test]
fn a_block_may_hold_the_largest_table_a_call_writes() {
    let text = alloc::format!("scenario s\nblock table {LARGEST_BLOCK}\n");
    assert!(parse_scenarios(&text).is_ok());
}

fn observed(lines: &[&str]) -> Observation {
    Observation {
        name: "s".into(),
        lines: lines.iter().map(|line| (*line).to_string()).collect(),
    }
}

#[test]
fn an_observation_is_held_to_its_scenarios_shows() {
    let text = "scenario s\ncall X\nshow r0 r1 v c\ncall Y\nkeep r1 k\nshow\n";
    let scenario = &parse_scenarios(text).unwrap()[0];
    for good in [
        ["1 r0=&00000001 r1=&00000002 v=0 c=1", "2"],
        [
            "1 r1=&00000002 v=1 c=0 error=&000001C1",
            "2 error=&00000105",
        ],
    ] {
        assert_eq!(check_observation(scenario, &observed(&good)), Ok(()));
    }
    let shown = "1 r0=&00000001 r1=&00000002 v=0 c=1";
    for (bad, line) in [
        (vec![shown], 2),
        (vec![shown, "2", "3"], 3),
        (vec!["2 r0=&00000001 r1=&00000002 v=0 c=1", "2"], 1),
        (vec!["1 r1=&00000002 r0=&00000001 v=0 c=1", "2"], 1),
        (vec!["1 r0=&1 r1=&00000002 v=0 c=1", "2"], 1),
        (vec!["1 r0=&0000000a r1=&00000002 v=0 c=1", "2"], 1),
        (vec!["1 r0=&00000001 r1=&00000002 v=1 c=1", "2"], 1),
        (vec!["1 r1=&00000002 v=0 c=1 error=&00000105", "2"], 1),
        (vec!["1 r0=&00000001 r1=&00000002 v=0 c=2", "2"], 1),
        (vec!["1 r0=&00000001 r1=&00000002 v=0 c=1 Unknown", "2"], 1),
        (vec![shown, "2 error=&105"], 2),
    ] {
        assert_eq!(
            check_observation(scenario, &observed(&bad)).map_err(|e| e.line),
            Err(line),
            "{bad:?}"
        );
    }
}

#[test]
fn shown_memory_and_differences_are_held_to_their_lengths_and_r0() {
    let text = "scenario s\nblock b 8\ncall X\nshow r0-b r1-b r0:2 $b+&2:3 v\n";
    let scenario = &parse_scenarios(text).unwrap()[0];
    for good in [
        "1 r0-b=&00000004 r1-b=&FFFFFFFC r0:2=0D0A $b+&2:3=EEEE41 v=0",
        "1 r1-b=&00000000 $b+&2:3=EEEEEE v=1 error=&000001E4",
    ] {
        assert_eq!(check_observation(scenario, &observed(&[good])), Ok(()));
    }
    let scenario = &parse_scenarios("scenario s\nblock b 8\ncall X\nshow r1:2 r1 v\n").unwrap()[0];
    assert_eq!(
        check_observation(scenario, &observed(&["1 r1=&00000000 v=1 error=&000001E4"])),
        Ok(()),
        "memory at a register is not shown after an error"
    );
    let scenario = &parse_scenarios(text).unwrap()[0];
    for bad in [
        "1 r0-b=&00000004 r1-b=&FFFFFFFC r0:2=0D0 $b+&2:3=EEEE41 v=0",
        "1 r0-b=&00000004 r1-b=&FFFFFFFC r0:2=0d0a $b+&2:3=EEEE41 v=0",
        "1 r0-b=&00000004 r1-b=&FFFFFFFC r0:2=0D0A $b:3=EEEE41 v=0",
        "1 r1-b=&00000000 r0:2=0D0A $b+&2:3=EEEEEE v=1 error=&000001E4",
    ] {
        assert!(
            check_observation(scenario, &observed(&[bad])).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_kept_value_is_named_so_an_offset_can_follow_it() {
    assert!(parse_scenarios("scenario s\ncall X\nkeep r1 base_1").is_ok());
    assert_eq!(
        parse_scenarios("scenario s\ncall X\nkeep r1 a-b").map_err(|e| e.line),
        Err(3),
        "$a-b-1 could not name it"
    );
}

#[test]
fn observations_round_trip_through_their_format() {
    let observations = vec![
        Observation {
            name: "one".into(),
            lines: vec!["1 r1=&00000100 v=0 c=0 z=0 n=0".into()],
        },
        Observation {
            name: "two".into(),
            lines: vec![
                "1 error=&000001C1 r1=&00000000".into(),
                "2 r2=&00002000".into(),
            ],
        },
    ];
    let text = format_observations(&["Recorded from RISC OS 5.30."], &observations);
    assert!(text.starts_with("# Recorded from RISC OS 5.30.\n"));
    assert_eq!(parse_observations(&text).unwrap(), observations);
    assert!(
        parse_observations("1 r1=&0").is_err(),
        "a line outside a scenario"
    );
}
