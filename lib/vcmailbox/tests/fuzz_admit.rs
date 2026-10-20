//! Deterministic fuzz harness for the mailbox service's admission policy.
//!
//! The property buffer a driver posts is untrusted: a compromised client
//! chooses every word, and the firmware it reaches runs code on the GPU, which
//! masters the bus. The invariants driven here:
//!
//! * judging any buffer never panics and never reads out of bounds;
//! * anything admitted is one tag from the drivers' working set, framed exactly,
//!   and only from the driver of the device that tag concerns;
//! * a caller loaded for no device is never admitted anything.
//!
//! The mutations are structure-aware as well as blind: beside flipping bits
//! and pure noise, they rewrite the fields a framing walk trusts — the
//! declared length, a value length, the codes, the tag id — so the harness
//! reaches past the first length check on every iteration.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` mutates each
//! driver's real requests and feeds pure noise. A plain `cargo test` runs the
//! fixed smoke sweep; `cargo xtask fuzz` extends the loop to a wall-clock
//! budget.

use tairix_abi::{HwDeviceClass, HwMatchKey, HwNode};
use tairix_fuzzseed::Prng;
use tairix_vcmailbox::{
    admit_request, encode_clock_rate_query, encode_clock_rate_write, encode_display_done,
    encode_display_size_query, encode_rtc_register_query, encode_rtc_register_write,
    encode_xhci_reset, ClockRateQuery, FirmwareClock, RtcRegister, DISPLAY_ENGINE_KEY,
    FIRMWARE_CLOCKS_KEY, PROPERTY_WORDS, RPI_RTC_KEY, VL805_KEY,
};

/// Iterations a plain `cargo test` runs.
const SMOKE_ITERATIONS: u64 = 4_096;

/// Each device whose driver the service answers, with the tags its driver may
/// send, pinned here independently of the table they are checked against.
const DEVICES: [(HwMatchKey, &[u32]); 4] = [
    (DISPLAY_ENGINE_KEY, &[0x0003_0066, DISPLAY_SIZE]),
    (VL805_KEY, &[0x0003_0058]),
    (RPI_RTC_KEY, &[0x0003_0087, 0x0003_8087]),
    (
        FIRMWARE_CLOCKS_KEY,
        &[0x0003_0002, 0x0003_0004, 0x0003_0007, 0x0003_8002],
    ),
];

/// The query of the screen size the firmware lit, which carries two slots the
/// firmware answers into.
const DISPLAY_SIZE: u32 = 0x0004_0003;

/// The clocks the firmware-clocks node's driver may name — the ARM, core, HDMI
/// state-machine and HDMI pixel-path clocks — pinned by the firmware's own
/// selectors.
const FIRMWARE_CLOCKS_DRIVEN: [u32; 4] = [3, 4, 13, 14];

fn seeds() -> [[u32; PROPERTY_WORDS]; 10] {
    [
        encode_display_done(),
        encode_display_size_query(),
        encode_xhci_reset(),
        encode_rtc_register_query(RtcRegister::Time),
        encode_rtc_register_write(RtcRegister::Time, 1_700_000_000),
        encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Max),
        encode_clock_rate_write(FirmwareClock::Arm, 1_500_000_000),
        encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current),
        encode_clock_rate_query(FirmwareClock::M2mc, ClockRateQuery::Min),
        encode_clock_rate_write(FirmwareClock::Core, 500_000_000),
    ]
}

fn driver_of(key: HwMatchKey) -> HwNode {
    let mut node = HwNode::new(7, 1, HwDeviceClass::Other);
    node.push_match_key(key).expect("a node holds its key");
    node
}

/// Check the invariants for `message`, from every client and from a caller
/// loaded for no device.
fn judge(message: &[u32; PROPERTY_WORDS], drivers: &[HwNode; DEVICES.len()]) {
    assert!(admit_request(message, None).is_err(), "{message:x?}");
    for (driver, (_, tags)) in drivers.iter().zip(DEVICES) {
        if admit_request(message, Some(driver)).is_err() {
            continue;
        }
        let words = usize::try_from(message[0] / 4).expect("an admitted length");
        let values = usize::try_from(message[3] / 4).expect("an admitted value length");
        assert!(
            message[0].is_multiple_of(4) && message[1] == 0 && message[4] == 0,
            "{message:x?}"
        );
        assert_eq!(words, 2 + 3 + values + 1, "one tag, exactly framed");
        assert_eq!(message[words - 1], 0, "an end tag last");
        assert!(tags.contains(&message[2]), "admitted {message:x?}");
        if message[2] == DISPLAY_SIZE {
            assert_eq!(
                message[5..7],
                [0, 0],
                "admitted a size query with its slots filled: {message:x?}"
            );
        }
        if driver.match_keys().contains(&FIRMWARE_CLOCKS_KEY) {
            assert!(
                FIRMWARE_CLOCKS_DRIVEN.contains(&message[5]),
                "admitted a clock no driver drives: {message:x?}"
            );
        }
    }
}

/// Rewrite one field a framing walk trusts.
fn mutate_structure(message: &mut [u32; PROPERTY_WORDS], rng: &mut Prng) {
    let field = rng.below(5);
    let value = match rng.below(3) {
        0 => rng.next_u32(),
        1 => u32::try_from(rng.below(PROPERTY_WORDS * 4 + 8)).expect("small"),
        _ => message[field].wrapping_add(4),
    };
    message[field] = value;
}

#[test]
fn judging_any_property_buffer_never_admits_beyond_each_driver_s_working_set() {
    let seeds = seeds();
    let drivers = DEVICES.map(|(key, _)| driver_of(key));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "judging_any_property_buffer_never_admits_beyond_each_driver_s_working_set",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut mutated = *rng.pick(&seeds);
        for _ in 0..=rng.below(3) {
            if rng.below(2) == 0 {
                mutate_structure(&mut mutated, &mut rng);
            } else {
                let at = rng.below(PROPERTY_WORDS);
                mutated[at] = if rng.below(2) == 0 {
                    mutated[at] ^ (1 << rng.below(32))
                } else {
                    rng.next_u32()
                };
            }
        }
        judge(&mutated, &drivers);

        let mut noise = [0u32; PROPERTY_WORDS];
        for word in &mut noise {
            *word = rng.next_u32();
        }
        judge(&noise, &drivers);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
