use super::*;

fn regs() -> Regs {
    let mut regs = Regs::default();
    regs.gpr[gpr::SI] = 0x0080;
    regs.gpr[gpr::SP] = 0xFFFE;
    regs.set_sreg(Seg::Ds, 0x1234);
    regs.set_sreg(Seg::Ss, 0x2000);
    regs.ip = 0x0100;
    regs
}

#[test]
fn an_address_is_numbers_or_registers_either_side_of_a_colon() {
    let regs = regs();
    for (text, expected) in [
        ("1234:0100", Address::Far(0x1234, 0x0100)),
        ("f000:e05b", Address::Far(0xF000, 0xE05B)),
        ("0F000h:0E05Bh", Address::Far(0xF000, 0xE05B)),
        ("ds:si", Address::Far(0x1234, 0x0080)),
        (" SS : SP ", Address::Far(0x2000, 0xFFFE)),
        ("ip", Address::Near(0x0100)),
        ("80", Address::Near(0x0080)),
        ("B8000", Address::Physical(0xB_8000)),
        ("FFFFF", Address::Physical(0xF_FFFF)),
    ] {
        assert_eq!(parse(text, &regs), Some(expected), "{text}");
    }
}

#[test]
fn what_names_no_address_is_refused() {
    let regs = regs();
    for text in [
        "", ":", "1234:", ":0100", "10000:0", "0:10000", "100000", "XX", "AL", "1:2:3", "12 34",
        "-1", "+5",
    ] {
        assert_eq!(parse(text, &regs), None, "{text:?}");
    }
    assert_eq!(
        parse(&"1".repeat(MOST_TYPED + 1), &regs),
        None,
        "too long to be one"
    );
}

#[test]
fn an_address_resolves_in_the_segment_its_view_reads() {
    assert_eq!(Address::Near(0x20).resolve(0x0050), (0x0050, 0x20));
    assert_eq!(Address::Far(1, 2).resolve(0x0050), (1, 2));
    assert_eq!(Address::Physical(0xB_8001).resolve(0), (0xB800, 1));
}
