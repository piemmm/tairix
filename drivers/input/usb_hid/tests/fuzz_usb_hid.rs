//! Deterministic fuzz harness for the USB HID class driver's reading of a
//! device's configuration descriptor — device-written bytes it trusts no
//! further than it checks them: `HidInterface::find` and the report length
//! its HID descriptor states.
//!
//! Held to: no input panics; a configuration assembled from interfaces in any
//! order, with the HID descriptor before or after its endpoint and unrelated
//! descriptors between, is read back exactly as it was assembled; and a found
//! interface's interrupt endpoint is one an endpoint address can name, however
//! the bytes were mutated, truncated or made up.
//!
//! A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh,
//! logged seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_drv_input_usb_hid::interface::{HidInterface, InterfaceError};
use tairix_fuzzseed::Prng;
use tairix_usb::descriptor::{
    DESC_TYPE_ENDPOINT, DESC_TYPE_INTERFACE, ENDPOINT_ADDR_DIR_IN, ENDPOINT_ADDR_NUMBER_MASK,
    ENDPOINT_ATTR_INTERRUPT,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// A HID descriptor's type (USB HID 1.11 §6.2.1), and the report
/// descriptor's, as its class-descriptor list names it.
const DESC_TYPE_HID: u8 = 0x21;
const DESC_TYPE_REPORT: u8 = 0x22;

/// One interface as assembled: its number, class code, the report length its
/// HID descriptor states, its interrupt-IN endpoint, and whether the HID
/// descriptor follows the endpoint rather than preceding it.
#[derive(Clone, Copy, Debug)]
struct Spec {
    number: u8,
    class: [u8; 3],
    report_len: Option<u16>,
    endpoint: u8,
    hid_after: bool,
}

/// A configuration descriptor of `body`, its total length filled in.
fn configuration(body: &[Vec<u8>]) -> Vec<u8> {
    let mut config = vec![9u8, 0x02, 0, 0, 2, 1, 0, 0xA0, 50];
    for descriptor in body {
        config.extend_from_slice(descriptor);
    }
    let total = u16::try_from(config.len()).unwrap_or(u16::MAX);
    config[2..4].copy_from_slice(&total.to_le_bytes());
    config
}

/// `spec`'s descriptors, with `extra` unrelated ones after its interface.
fn described(spec: Spec, rng: &mut Prng) -> Vec<Vec<u8>> {
    let mut out = vec![vec![
        9,
        DESC_TYPE_INTERFACE,
        spec.number,
        0,
        1,
        spec.class[0],
        spec.class[1],
        spec.class[2],
        0,
    ]];
    for _ in 0..rng.below(3) {
        let len = 2 + rng.below(10);
        let mut other = vec![0u8; len];
        rng.fill(&mut other);
        other[0] = u8::try_from(len).unwrap_or(2);
        other[1] = *rng.pick(&[0x24, 0x25, 0x30, 0x0B]);
        out.push(other);
    }
    let hid = spec.report_len.map(|len| {
        let [low, high] = len.to_le_bytes();
        let physical = [0x23, 9, 0];
        let mut hid = vec![0u8, DESC_TYPE_HID, 0x11, 0x01, 0];
        if rng.below(3) == 0 {
            hid.push(2);
            hid.extend_from_slice(&physical);
        } else {
            hid.push(1);
        }
        hid.extend_from_slice(&[DESC_TYPE_REPORT, low, high]);
        hid[0] = u8::try_from(hid.len()).unwrap_or(9);
        hid
    });
    let endpoint = vec![
        7,
        DESC_TYPE_ENDPOINT,
        ENDPOINT_ADDR_DIR_IN | spec.endpoint,
        ENDPOINT_ATTR_INTERRUPT,
        8,
        0,
        10,
    ];
    match (hid, spec.hid_after) {
        (Some(hid), true) => out.extend([endpoint, hid]),
        (Some(hid), false) => out.extend([hid, endpoint]),
        (None, _) => out.push(endpoint),
    }
    out
}

/// One to four interfaces of distinct numbers.
fn specs(rng: &mut Prng) -> Vec<Spec> {
    let mut specs: Vec<Spec> = Vec::new();
    for _ in 0..=rng.below(4) {
        let number = rng.next_u8() % 8;
        if specs.iter().any(|spec| spec.number == number) {
            continue;
        }
        specs.push(Spec {
            number,
            class: [3, rng.next_u8() % 2, rng.next_u8() % 3],
            report_len: (rng.below(4) != 0).then(|| rng.next_u16()),
            endpoint: 1 + rng.next_u8() % 15,
            hid_after: rng.below(2) == 0,
        });
    }
    specs
}

/// Hold a result to what any configuration may yield.
fn check(found: Result<HidInterface, InterfaceError>) {
    if let Ok(interface) = found {
        let endpoint = interface.interrupt_endpoint;
        assert!(
            endpoint != 0 && endpoint & !ENDPOINT_ADDR_NUMBER_MASK == 0,
            "no endpoint address names {endpoint}: {interface:?}"
        );
    }
}

#[test]
fn an_assembled_configuration_reads_back_as_it_was_assembled() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "an_assembled_configuration_reads_back_as_it_was_assembled",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let specs = specs(&mut rng);
        let body: Vec<Vec<u8>> = specs
            .iter()
            .flat_map(|spec| described(*spec, &mut rng))
            .collect();
        let config = configuration(&body);
        for spec in &specs {
            assert_eq!(
                HidInterface::find(&config, spec.number),
                Ok(HidInterface {
                    class: spec.class,
                    report_descriptor_len: spec.report_len,
                    interrupt_endpoint: spec.endpoint,
                }),
                "{spec:?} in {config:02x?}"
            );
        }
        let absent = (0..=u8::MAX).find(|number| specs.iter().all(|spec| spec.number != *number));
        if let Some(absent) = absent {
            assert_eq!(
                HidInterface::find(&config, absent),
                Err(InterfaceError::NotFound)
            );
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn a_mutated_truncated_or_made_up_configuration_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_mutated_truncated_or_made_up_configuration_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut config = if rng.below(4) == 0 {
            let mut noise = vec![0u8; rng.at_most(96)];
            rng.fill(&mut noise);
            noise
        } else {
            let specs = specs(&mut rng);
            let body: Vec<Vec<u8>> = specs
                .iter()
                .flat_map(|spec| described(*spec, &mut rng))
                .collect();
            configuration(&body)
        };
        for _ in 0..rng.below(4) {
            if config.is_empty() {
                break;
            }
            let at = rng.below(config.len());
            config[at] = match rng.below(3) {
                0 => rng.next_u8(),
                1 => 0,
                _ => u8::MAX,
            };
        }
        if rng.below(3) == 0 {
            config.truncate(rng.at_most(config.len()));
        }
        for number in [0, 1, 2, rng.next_u8()] {
            check(HidInterface::find(&config, number));
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
