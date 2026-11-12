use super::*;

const MS: Nanos = NANOS_PER_MILLISECOND;

fn fixed(bits: u8) -> Lights {
    Lights {
        floppy: 0,
        fixed: bits,
    }
}

#[test]
fn a_lamp_is_told_lit_once_and_dark_once_its_hold_has_passed() {
    let mut lamps = Lamps::default();
    assert_eq!(lamps.observe(Lights::default(), 0), None, "dark, as told");
    assert_eq!(lamps.observe(fixed(1), 4 * MS), Some(fixed(1)));
    assert_eq!(
        lamps.observe(fixed(1), 8 * MS),
        None,
        "still lit: nothing new"
    );
    assert_eq!(
        lamps.observe(Lights::default(), 12 * MS),
        None,
        "an access just over stays lit"
    );
    assert_eq!(lamps.observe(Lights::default(), 8 * MS + HOLD - 1), None);
    assert_eq!(
        lamps.observe(Lights::default(), 8 * MS + HOLD),
        Some(Lights::default())
    );
}

#[test]
fn the_lamps_are_held_apart_and_a_floppy_motor_shows_on_its_own_drive() {
    let mut lamps = Lamps::default();
    let both = Lights {
        floppy: 0b10,
        fixed: 0b01,
    };
    assert_eq!(lamps.observe(both, 0), Some(both));
    let floppy_only = Lights {
        floppy: 0b10,
        fixed: 0,
    };
    assert_eq!(
        lamps.observe(floppy_only, HOLD),
        Some(floppy_only),
        "the fixed disk's hold ran out"
    );
    assert_eq!(lamps.observe(floppy_only, 2 * HOLD), None);
}

#[test]
fn a_paused_machine_keeps_its_lamps_as_they_stood() {
    let mut lamps = Lamps::default();
    lamps.observe(fixed(0b10), 100 * MS);
    for _ in 0..10 {
        assert_eq!(
            lamps.observe(Lights::default(), 100 * MS),
            None,
            "emulated time stands still"
        );
    }
}
