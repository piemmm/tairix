//! The generators and PLL channels driven over the register-level model.

use tairix_abi::DriverError;
use tairix_fuzzseed::Prng;

use crate::cprman::{Cprman, Halted, Source, MASH_MAX_HZ, MAX_DIVISOR, MIN_DIVISOR, PCM, PWM};
use crate::model::{
    pll_control, Model, Stopping, CHANNEL_DISABLED, CTL_ENABLE, CTL_KILL, MASH_FIRST_ORDER,
    OSCILLATOR, PCM_CTL, PCM_DIV, PLLC_CTRL, PLLC_FRAC, PLLC_PER, PLLD_ANA1, PLLD_CTRL, PLLD_FRAC,
    PLLD_PER, PLL_FEEDBACK_PREDIV, PLL_OUT_OF_RESET, PLL_POWER_DOWN, PWM_CTL, PWM_DIV,
    SOURCE_OSCILLATOR, SOURCE_PLLC, SOURCE_PLLD, SOURCE_TEST,
};

const PLLD_PER_HZ: u64 = 675_000_000;
const PASSWORD: u32 = 0x5A << 24;

fn whole(divisor: u32) -> u32 {
    divisor << 12
}

#[test]
fn a_pll_channel_runs_at_the_oscillator_times_its_feedback_over_its_dividers() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    assert_eq!(cprman.source_rate(Source::PlldPer), Ok(PLLD_PER_HZ));
    assert_eq!(cprman.source_rate(Source::Oscillator), Ok(OSCILLATOR));
    assert_eq!(cprman.source_rate(Source::Ground), Ok(0));

    // A Pi 4's PLLD: 3 GHz less what the 20-bit fraction cannot say.
    model.set(PLLD_CTRL, pll_control(1, 55));
    model.set(PLLD_FRAC, 582_542);
    let feedback = (55u128 << 20) + 582_542;
    let vco = (u128::from(OSCILLATOR) * feedback) >> 20;
    let per = u64::try_from(vco / 4).expect("fits");
    assert_eq!(cprman.source_rate(Source::PlldPer), Ok(per));
    assert_eq!(per, 749_999_997);

    model.set(PLLD_ANA1, PLL_FEEDBACK_PREDIV);
    assert_eq!(
        cprman.source_rate(Source::PlldPer),
        Ok(u64::try_from(vco * 2 / 4).expect("fits")),
        "the pre-divider doubles the feedback"
    );
    model.set(PLLD_ANA1, 0);
    model.set(PLLD_CTRL, pll_control(2, 50));
    model.set(PLLD_FRAC, 0);
    model.set(PLLD_PER, 0);
    assert_eq!(
        cprman.source_rate(Source::PlldPer),
        Ok(OSCILLATOR * 50 / 2 / 256),
        "a divider field of zero divides by 256"
    );

    for (control, channel) in [
        (pll_control(1, 50), 4 | CHANNEL_DISABLED),
        (pll_control(1, 50) & !PLL_OUT_OF_RESET, 4),
        (pll_control(1, 50) | PLL_POWER_DOWN, 4),
        (pll_control(0, 50), 4),
    ] {
        model.set(PLLD_CTRL, control);
        model.set(PLLD_PER, channel);
        assert_eq!(
            cprman.source_rate(Source::PlldPer),
            Ok(0),
            "{control:#x} {channel:#x}"
        );
    }
}

#[test]
fn the_nearest_setting_is_the_nearer_rate_then_a_whole_divisor_then_the_larger() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    let nearest = |hz| {
        cprman
            .nearest(hz)
            .map(|(setting, made)| (setting.source(), setting.divisor(), made))
    };
    // Both sources make it exactly through MASH; PLLD's jitter is the smaller.
    assert_eq!(
        nearest(3_072_000),
        Ok((Source::PlldPer, 900_000, 3_072_000))
    );
    // Both exactly, the oscillator by a whole divisor.
    assert_eq!(
        nearest(2_000_000),
        Ok((Source::Oscillator, whole(27), 2_000_000))
    );
    // Both by whole divisors: the larger.
    assert_eq!(
        nearest(9_000_000),
        Ok((Source::PlldPer, whole(75), 9_000_000))
    );
    // Faster than either can make: each at its smallest divisor.
    assert_eq!(
        nearest(1_000_000_000),
        Ok((Source::PlldPer, MIN_DIVISOR, PLLD_PER_HZ / 2))
    );
    // Slower: the oscillator at its largest gets nearer.
    assert_eq!(nearest(1), Ok((Source::Oscillator, MAX_DIVISOR, 13_187)));
    assert_eq!(nearest(0), Err(DriverError::OutOfRange));

    model.set(PLLD_PER, 4 | CHANNEL_DISABLED);
    assert_eq!(
        nearest(3_072_000),
        Ok((Source::Oscillator, 72_000, 3_072_000)),
        "a stopped PLL offers nothing"
    );
}

#[test]
fn a_fractional_divisor_is_chosen_only_where_mash_keeps_within_its_limit() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    let nearest = |hz| {
        cprman
            .nearest(hz)
            .map(|(setting, made)| (setting.source(), setting.divisor(), made))
    };
    // PLLD by 27.466: its whole part keeps MASH at 25 MHz.
    assert_eq!(
        nearest(24_576_000),
        Ok((Source::PlldPer, 112_500, 24_576_000))
    );
    // PLLD by 25.96 would reach 27 MHz, so its whole divisor instead.
    assert_eq!(
        nearest(26_000_000),
        Ok((Source::PlldPer, whole(26), 25_961_538))
    );
    model.set(PLLD_PER, 4 | CHANNEL_DISABLED);
    // The oscillator by 2.197 would reach 27 MHz.
    assert_eq!(
        nearest(24_576_000),
        Ok((Source::Oscillator, whole(2), OSCILLATOR / 2))
    );
}

#[test]
fn no_divisor_of_either_source_comes_nearer_than_the_one_chosen() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "no_divisor_of_either_source_comes_nearer_than_the_one_chosen",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    // `|parent * 4096 - hz * divisor| / divisor`, compared without division.
    let distance = |parent: u64, hz: u64, divisor: u32| {
        (
            (u128::from(parent) << 12).abs_diff(u128::from(hz) * u128::from(divisor)),
            divisor,
        )
    };
    let nearer =
        |(a, ad): (u128, u32), (b, bd): (u128, u32)| a * u128::from(bd) < b * u128::from(ad);
    // A fractional divisor's shortest period is its whole part's.
    let mash_allows = |parent: u64, divisor: u32| {
        divisor.trailing_zeros() >= 12
            || u128::from(parent) <= u128::from(MASH_MAX_HZ) * u128::from(divisor >> 12)
    };
    for _ in 0..2_000 {
        let hz = 1 + rng.next_u64() % 400_000_000;
        let (setting, _) = cprman.nearest(hz).expect("a rate");
        let parent = match setting.source() {
            Source::Oscillator => OSCILLATOR,
            _ => PLLD_PER_HZ,
        };
        assert!(
            mash_allows(parent, setting.divisor()),
            "{hz} Hz: {setting:?} breaks MASH"
        );
        let chosen = distance(parent, hz, setting.divisor());
        for rival in [OSCILLATOR, PLLD_PER_HZ] {
            let exact = (u128::from(rival) << 12) / u128::from(hz);
            let whole = exact & !0xFFF;
            let near = (0..6u128).map(|step| (exact + step).saturating_sub(3));
            for divisor in near.chain([whole, whole + 0x1000]) {
                let Ok(divisor) = u32::try_from(divisor) else {
                    continue;
                };
                if !(MIN_DIVISOR..=MAX_DIVISOR).contains(&divisor) || !mash_allows(rival, divisor) {
                    continue;
                }
                assert!(
                    !nearer(distance(rival, hz, divisor), chosen),
                    "{hz} Hz: {rival} / {divisor} beats {setting:?}"
                );
            }
        }
    }
}

#[test]
fn a_generator_reports_its_rate_from_its_source_divisor_and_mash() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    assert_eq!(cprman.rate(PCM), Ok(0), "stopped");
    model.running(PCM_CTL, SOURCE_OSCILLATOR | MASH_FIRST_ORDER, 72_000);
    assert_eq!(cprman.rate(PCM), Ok(3_072_000));
    model.running(PCM_CTL, SOURCE_OSCILLATOR, 72_000);
    assert_eq!(
        cprman.rate(PCM),
        Ok(3_176_471),
        "without MASH the fraction is ignored: 54 MHz / 17, rounded"
    );
    model.set(PLLC_CTRL, pll_control(1, 37));
    model.set(PLLC_FRAC, 0);
    model.set(PLLC_PER, 2);
    model.running(PWM_CTL, SOURCE_PLLC, whole(9));
    assert_eq!(cprman.rate(PWM), Ok(OSCILLATOR * 37 / 2 / 9));
    model.running(PWM_CTL, SOURCE_TEST, whole(9));
    assert_eq!(cprman.rate(PWM), Err(DriverError::Unsupported));
    model.running(PWM_CTL, SOURCE_PLLD, 0x0FFF);
    assert_eq!(
        cprman.rate(PWM),
        Err(DriverError::Unsupported),
        "no whole divisor"
    );
}

#[test]
fn running_a_generator_stops_it_then_sets_it_up_before_enabling_it() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    model.running(PWM_CTL, SOURCE_OSCILLATOR, whole(9));
    let (setting, made) = cprman.nearest(3_072_000).expect("a rate");
    assert_eq!(setting.divisor(), 900_000);
    assert_eq!(cprman.run(PWM, setting), Ok(Halted::Finished));
    assert_eq!(
        model.writes(),
        [
            (PWM_CTL, PASSWORD | SOURCE_OSCILLATOR),
            (PWM_DIV, PASSWORD | setting.divisor()),
            (PWM_CTL, PASSWORD | MASH_FIRST_ORDER | SOURCE_PLLD),
            (
                PWM_CTL,
                PASSWORD | MASH_FIRST_ORDER | SOURCE_PLLD | CTL_ENABLE
            ),
        ]
    );
    assert_eq!(cprman.rate(PWM), Ok(made));

    model.clear_writes();
    let (whole_setting, _) = cprman.nearest(9_000_000).expect("a rate");
    assert_eq!(cprman.run(PWM, whole_setting), Ok(Halted::Finished));
    assert_eq!(
        model.writes()[2],
        (PWM_CTL, PASSWORD | SOURCE_PLLD),
        "a whole divisor runs without MASH"
    );
}

#[test]
fn a_stopped_generator_is_left_alone() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    assert_eq!(cprman.stop(PCM), Ok(Halted::Finished));
    assert!(model.writes().is_empty());
}

#[test]
fn a_generator_that_will_not_finish_is_reset_and_one_that_ignores_the_reset_is_refused() {
    let model = Model::new();
    let cprman = Cprman::new(&model, OSCILLATOR).expect("valid");
    model.running(PCM_CTL, SOURCE_OSCILLATOR, whole(9));
    model.stopping(PCM_CTL, Stopping::NeedsKill);
    assert_eq!(cprman.stop(PCM), Ok(Halted::Killed));
    let writes = model.writes();
    assert_eq!(
        writes[1],
        (PCM_CTL, PASSWORD | SOURCE_OSCILLATOR | CTL_KILL)
    );
    assert_eq!(
        writes.last(),
        Some(&(PCM_CTL, PASSWORD | SOURCE_OSCILLATOR)),
        "the reset is released"
    );

    model.clear_writes();
    model.running(PCM_CTL, SOURCE_OSCILLATOR, whole(9));
    model.stopping(PCM_CTL, Stopping::Never);
    let (setting, _) = cprman.nearest(3_072_000).expect("a rate");
    assert_eq!(cprman.run(PCM, setting), Err(DriverError::DeviceFault));
    assert!(
        model.writes().iter().all(|&(offset, _)| offset == PCM_CTL),
        "a generator still running is never reconfigured"
    );
    assert_eq!(model.get(PCM_DIV), whole(9));
}

#[test]
fn a_window_short_of_the_pll_registers_or_an_oscillator_at_no_rate_is_refused() {
    struct Short;
    impl tairix_abi::RegisterBlock for Short {
        fn read32(&self, _offset: usize) -> Result<u32, DriverError> {
            Ok(0)
        }
        fn write32(&self, _offset: usize, _value: u32) -> Result<(), DriverError> {
            Ok(())
        }
        fn block_len(&self) -> usize {
            0x1000
        }
    }
    assert!(matches!(
        Cprman::new(&Short, OSCILLATOR),
        Err(DriverError::LengthOutOfRange)
    ));
    let model = Model::new();
    assert!(matches!(
        Cprman::new(&model, 0),
        Err(DriverError::OutOfRange)
    ));
}
