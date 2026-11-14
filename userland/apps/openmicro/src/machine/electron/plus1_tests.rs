use super::*;
use crate::snapshot::{self, Reader, SnapshotError, Writer};

#[test]
fn a_conversion_ends_after_its_time_with_the_channels_level() {
    let mut plus1 = Plus1::new();
    plus1.set_input(0, 2, 0xA0FF);
    plus1.write(100, ADC, 0x06);
    let converting =
        |plus1: &mut Plus1, now| plus1.read(now, STATUS).unwrap() & status::CONVERTING != 0;
    assert!(converting(&mut plus1, 100 + CONVERSION - 1));
    assert!(!converting(&mut plus1, 100 + CONVERSION), "ended");
    assert_eq!(
        plus1.read(200, ADC),
        Some(0xA0),
        "channel 3's top eight bits"
    );
}

#[test]
fn an_input_moved_during_a_conversion_is_read_at_its_end() {
    let mut plus1 = Plus1::new();
    plus1.write(0, ADC, 0x04);
    plus1.set_input(CONVERSION - 1, 0, 0x4000);
    plus1.set_input(CONVERSION + 5, 0, 0xFF00);
    assert_eq!(plus1.read(CONVERSION + 10, ADC), Some(0x40));
}

#[test]
fn the_printer_is_busy_from_each_strobe_and_takes_each_byte() {
    let mut plus1 = Plus1::new();
    plus1.write(10, PRINTER, b'A');
    let busy = |plus1: &mut Plus1, now| plus1.read(now, STATUS).unwrap() & status::BUSY != 0;
    assert!(busy(&mut plus1, 10));
    assert!(busy(&mut plus1, 10 + STROBE_BUSY - 1));
    assert!(!busy(&mut plus1, 10 + STROBE_BUSY));
    plus1.write(50, PRINTER, b'B');
    assert_eq!(plus1.take_output().collect::<alloc::vec::Vec<_>>(), b"AB");
    assert_eq!(
        plus1.read(60, PRINTER),
        None,
        "the data latch reads nothing"
    );
}

#[test]
fn the_fire_buttons_read_low_while_held() {
    let mut plus1 = Plus1::new();
    let fire =
        |plus1: &mut Plus1| plus1.read(0, STATUS).unwrap() & (status::FIRE_1 | status::FIRE_2);
    assert_eq!(fire(&mut plus1), status::FIRE_1 | status::FIRE_2);
    plus1.set_fire(0x01);
    assert_eq!(fire(&mut plus1), status::FIRE_2);
    plus1.set_fire(0xFF);
    assert_eq!(fire(&mut plus1), 0, "only the two buttons");
}

#[test]
fn a_restored_plus_1_holds_only_what_its_time_allows() {
    let restore = |plus1: &Plus1, clock: u64| -> Result<Plus1, SnapshotError> {
        let mut w = Writer::new();
        w.put(plus1);
        let body = snapshot::open(&w.finish()?)?;
        let mut r = Reader::new(&body);
        r.set_clock(clock)?;
        r.get()
    };
    let mut plus1 = Plus1::new();
    plus1.write(1000, ADC, 0x05);
    plus1.write(1000, PRINTER, 0);
    let mut kept = plus1.clone();
    kept.take_output();
    assert_eq!(
        restore(&plus1, 1000),
        Ok(kept),
        "what was printed is not kept"
    );
    assert_eq!(
        restore(&plus1, 900),
        Err(SnapshotError::Malformed),
        "a conversion further off than one takes"
    );
    assert_eq!(
        restore(&plus1, 1100),
        Err(SnapshotError::Malformed),
        "one already over"
    );
}
