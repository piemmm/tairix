use super::*;

const US: Nanos = 1_000;

fn plugged(axes: [Option<u16>; 4], buttons: u8) -> GamePort {
    let mut port = GamePort::default();
    port.set(Sticks { axes, buttons });
    port
}

#[test]
fn idle_one_shots_read_low_until_a_write_fires_them() {
    let port = plugged([Some(0); 4], 0);
    assert_eq!(port.input(0), 0xF0);
}

#[test]
fn each_one_shot_falls_after_its_potentiometers_charge_time() {
    let mut port = plugged([Some(0), Some(u16::MAX), Some(32_768), None], 0);
    port.output(10 * US);
    assert_eq!(port.input(10 * US), 0xFF);
    let low = 10 * US + 24_200;
    assert_eq!(port.input(low - 1) & 0x01, 0x01);
    assert_eq!(port.input(low) & 0x01, 0, "24.2 µs at no resistance");
    let high = 10 * US + 24_200 + 1_100_000;
    assert_eq!(port.input(high - 1) & 0x02, 0x02);
    assert_eq!(port.input(high) & 0x02, 0, "1.1 ms more at 100 kΩ");
    let middle = 10 * US + charge_time(32_768);
    assert!(middle > low && middle < high);
    assert_eq!(port.input(middle) & 0x04, 0);
    assert_eq!(
        port.input(1_000_000 * US) & 0x08,
        0x08,
        "nothing plugged in never falls"
    );
}

#[test]
fn a_running_one_shot_is_not_fired_again() {
    let mut port = plugged([Some(u16::MAX), None, None, None], 0);
    port.output(0);
    port.output(500 * US);
    let end = charge_time(u16::MAX);
    assert_eq!(port.input(end) & 0x01, 0, "the first firing's time stands");
    port.output(end);
    assert_eq!(port.input(end + 1) & 0x01, 0x01, "fired again once run out");
}

#[test]
fn a_new_position_takes_effect_at_the_next_firing() {
    let mut port = plugged([Some(u16::MAX), None, None, None], 0);
    port.output(0);
    port.set(Sticks {
        axes: [Some(0), None, None, None],
        buttons: 0,
    });
    assert_eq!(port.input(100 * US) & 0x01, 0x01, "still charging as fired");
    let end = charge_time(u16::MAX);
    port.output(end);
    assert_eq!(port.input(end + charge_time(0)) & 0x01, 0);
}

#[test]
fn buttons_read_low_while_held() {
    let port = plugged([None; 4], 0b0101);
    assert_eq!(port.input(0) >> 4, 0b1010);
}
