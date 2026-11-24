use super::*;

#[test]
fn an_access_string_sets_exactly_its_bits_as_risc_os_5_reads_it() {
    for (text, bits) in [
        (&b"WR/R"[..], 0x13),
        (b"", 0),
        (b"L", 0x08),
        (b"lwr/w", 0x2B),
        (b"R/RW", 0x31),
        (b"/", 0),
        (b"RR", 0x01),
    ] {
        assert_eq!(parse_attributes(text), Some(bits), "{text:?}");
    }
    assert_eq!(parse_attributes(b"Q"), None);
    assert_eq!(parse_attributes(b"WR/R/"), None);
    assert_eq!(parse_attributes(b"/L"), None);
}

fn info(load: u32, exec: u32) -> Info {
    Info {
        kind: call::FILE,
        load,
        exec,
        length: 1000,
        attributes: 0x33,
    }
}

fn range(r4: u32, r5: u32, r6: u32, r7: u32) -> Option<Range> {
    let mut regs = Regs::default();
    regs.r[3..8].copy_from_slice(&[flag::TIMES, r4, r5, r6, r7]);
    Range::of(&regs)
}

#[test]
fn a_date_range_is_low_words_then_high_bytes_and_holds_its_ends() {
    let (one, two) = (
        info(0xFFFF_FD45, 0x6789_0ABC),
        info(0xFFFF_FD45, 0x6789_0ABD),
    );
    let both = range(0x6789_0ABC, 0x45, 0x6789_0ABE, 0x45);
    assert!(Range::holds(both, &one) && Range::holds(both, &two));
    let exact = range(0x6789_0ABD, 0x45, 0x6789_0ABD, 0x45);
    assert!(!Range::holds(exact, &one) && Range::holds(exact, &two));
    let load_first = range(0xFFFF_FD45, 0x6789_0ABC, 0xFFFF_FD45, 0x6789_0ABE);
    assert!(!Range::holds(load_first, &one) && !Range::holds(load_first, &two));
    let up_to_one = range(0, 0, 0x6789_0ABC, 0x45);
    assert!(Range::holds(up_to_one, &one) && !Range::holds(up_to_one, &two));
}

#[test]
fn no_range_holds_everything() {
    let mut regs = Regs::default();
    regs.r[4] = u32::MAX;
    assert_eq!(Range::of(&regs), None);
    assert!(Range::holds(None, &info(0, 0)));
}

#[test]
fn a_question_left_unanswered_by_quiet_passes_or_abandons() {
    assert_eq!(Answer::No.fate(), Fate::Kept);
    assert_eq!(Answer::Abandon.fate(), Fate::Abandoned);
}

#[test]
fn a_count_sums_as_a_32_bit_register_would() {
    let mut totals = Totals {
        bytes: u32::MAX,
        files: 1,
    };
    totals.bytes = totals.bytes.wrapping_add(2);
    assert_eq!(totals.bytes, 1);
}
