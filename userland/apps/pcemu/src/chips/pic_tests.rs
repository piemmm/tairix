use super::*;

/// The IBM PC BIOS's initialisation: edge-triggered, single, ICW4, vectors
/// at 08h, buffered 8086 mode.
fn bios_initialised() -> Pic {
    let mut pic = Pic::new();
    pic.write(false, 0x13);
    pic.write(true, 0x08);
    pic.write(true, 0x09);
    pic.write(true, 0x00);
    pic
}

fn in_service(pic: &mut Pic) -> u8 {
    pic.write(false, 0x0B);
    let isr = pic.read(false);
    pic.write(false, 0x0A);
    isr
}

fn pulse(pic: &mut Pic, irq: u8) {
    pic.rising_edge(irq);
    pic.set_line(irq, false);
}

#[test]
fn a_rising_edge_requests_and_acknowledge_answers_its_vector() {
    let mut pic = bios_initialised();
    assert!(!pic.intr());
    pulse(&mut pic, 1);
    assert!(pic.intr());
    assert_eq!(pic.acknowledge(), 0x09);
    assert_eq!(in_service(&mut pic), 0x02);
    assert!(!pic.intr(), "IRQ 1 in service blocks itself");
    pic.write(false, 0x20);
    assert_eq!(in_service(&mut pic), 0);
}

#[test]
fn a_held_line_requests_once_per_edge() {
    let mut pic = bios_initialised();
    pic.set_line(0, true);
    assert_eq!(pic.acknowledge(), 0x08);
    pic.write(false, 0x20);
    pic.set_line(0, true);
    assert!(!pic.intr(), "no new edge while the line stays high");
    pic.rising_edge(0);
    assert!(pic.intr(), "an edge between two visits still latches");
}

#[test]
fn higher_priority_preempts_lower() {
    let mut pic = bios_initialised();
    pulse(&mut pic, 3);
    assert_eq!(pic.acknowledge(), 0x0B);
    pulse(&mut pic, 5);
    assert!(!pic.intr(), "IRQ 5 waits behind IRQ 3");
    pulse(&mut pic, 0);
    assert!(pic.intr(), "IRQ 0 preempts IRQ 3");
    assert_eq!(pic.acknowledge(), 0x08);
    pic.write(false, 0x20);
    pic.write(false, 0x20);
    assert_eq!(pic.acknowledge(), 0x0D);
}

#[test]
fn masked_lines_wait_and_unmask_delivers() {
    let mut pic = bios_initialised();
    pic.write(true, 0xFF);
    pulse(&mut pic, 2);
    assert!(!pic.intr());
    assert!(!pic.listens_to(2));
    pic.write(true, 0x00);
    assert!(pic.intr());
    assert_eq!(pic.read(true), 0);
}

#[test]
fn a_vanished_request_is_spurious_irq7() {
    let mut pic = Pic::new();
    pic.write(false, 0x1B);
    pic.write(true, 0x70);
    pic.write(true, 0x01);
    pic.set_line(4, true);
    assert!(pic.intr());
    pic.set_line(4, false);
    assert_eq!(pic.acknowledge(), 0x77);
    assert_eq!(
        in_service(&mut pic),
        0,
        "a spurious IRQ 7 sets no in-service bit"
    );
}

#[test]
fn rotation_makes_the_serviced_line_lowest() {
    let mut pic = bios_initialised();
    pulse(&mut pic, 0);
    assert_eq!(pic.acknowledge(), 0x08);
    pic.write(false, 0xA0);
    pulse(&mut pic, 0);
    pulse(&mut pic, 4);
    assert_eq!(pic.acknowledge(), 0x0C, "IRQ 0 now has the lowest priority");
}

#[test]
fn ocw3_selects_isr_reads_and_poll_mode() {
    let mut pic = bios_initialised();
    pulse(&mut pic, 6);
    assert_eq!(pic.read(false), 0x40);
    pic.write(false, 0x0B);
    assert_eq!(pic.read(false), 0);
    pic.write(false, 0x0C);
    assert_eq!(
        pic.read(true),
        0x86,
        "a poll answers on either port and acknowledges"
    );
    pic.write(false, 0x0B);
    assert_eq!(pic.read(false), 0x40);
}

#[test]
fn auto_eoi_leaves_nothing_in_service() {
    let mut pic = Pic::new();
    pic.write(false, 0x13);
    pic.write(true, 0x08);
    pic.write(true, 0x03);
    pulse(&mut pic, 1);
    assert_eq!(pic.acknowledge(), 0x09);
    assert_eq!(in_service(&mut pic), 0);
}

#[test]
fn special_fully_nested_mode_lets_a_level_in_again() {
    let mut pic = Pic::new();
    pic.write(false, 0x13);
    pic.write(true, 0x08);
    pic.write(true, 0x11);
    pic.write(true, 0x00);
    pulse(&mut pic, 2);
    assert_eq!(pic.acknowledge(), 0x0A);
    pulse(&mut pic, 2);
    assert!(pic.intr(), "the level in service may interrupt itself");
    pulse(&mut pic, 5);
    assert_eq!(pic.acknowledge(), 0x0A, "but lower levels still wait");
}

#[test]
fn special_mask_mode_releases_lower_levels() {
    let mut pic = bios_initialised();
    pulse(&mut pic, 1);
    assert_eq!(pic.acknowledge(), 0x09);
    pic.write(true, 0x02);
    pic.write(false, 0x68);
    pulse(&mut pic, 4);
    assert_eq!(pic.acknowledge(), 0x0C);
}

#[test]
fn mcs80_mode_drives_the_call_address_low_byte() {
    let mut pic = Pic::new();
    pic.write(false, 0xB6);
    pic.write(true, 0x12);
    pulse(&mut pic, 3);
    assert_eq!(
        pic.acknowledge(),
        0xA0 | (3 << 2),
        "interval four: A7-A5 from ICW1"
    );
    pic.write(false, 0xD2);
    pic.write(true, 0x12);
    pulse(&mut pic, 5);
    assert_eq!(
        pic.acknowledge(),
        0xC0 | (5 << 3),
        "interval eight: A7-A6 from ICW1"
    );
}

#[test]
fn level_triggered_requests_follow_the_line() {
    let mut pic = Pic::new();
    pic.write(false, 0x1B);
    pic.write(true, 0x08);
    pic.write(true, 0x01);
    pic.set_line(3, true);
    assert_eq!(pic.acknowledge(), 0x0B);
    pic.write(false, 0x20);
    assert!(pic.intr(), "a line still high requests again after EOI");
    pic.set_line(3, false);
    assert!(!pic.intr());
}
