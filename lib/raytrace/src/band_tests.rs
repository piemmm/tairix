use super::*;
use tairix_parallel::{Reversed, Threaded, SERIAL};

#[test]
fn every_band_is_filled_once_with_its_own_number() {
    let runners: [&dyn JobRunner; 3] = [&SERIAL, &Reversed::new(4), &Threaded::new(4)];
    for runner in runners {
        let mut values = [0usize; 23];
        for_each(runner, &mut values, (7, 5), &|number, band| {
            for value in band {
                *value += number;
            }
        });
        for (index, value) in values.iter().enumerate() {
            assert_eq!(*value, 7 + index / 5, "{index}");
        }
    }
}

#[test]
fn nothing_to_fill_visits_nothing_and_a_zero_width_band_is_one_wide() {
    for_each(&SERIAL, &mut [0u8; 0], (0, 4), &|_, _| {
        unreachable!("no bands")
    });
    let mut values = [0usize; 3];
    for_each(&Reversed::new(2), &mut values, (1, 0), &|number, band| {
        assert_eq!(band.len(), 1);
        band[0] = number;
    });
    assert_eq!(values, [1, 2, 3]);
}
