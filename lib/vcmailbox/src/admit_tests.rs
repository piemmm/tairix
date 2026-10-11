//! The admission policy against every driver's working set, against callers
//! that are not the driver of the device a tag concerns, against the tags no
//! driver may send, and against hostile buffer shapes.

use tairix_abi::driver::display::DisplayFormat;
use tairix_abi::{Errno, HwDeviceClass, HwMatchKey, HwNode};

use super::{admit_request, Refusal};
use crate::{
    encode_blank_screen, encode_clock_rate_query, encode_clock_rate_write,
    encode_firmware_revision_query, encode_gpio_state_write, encode_rtc_register_query,
    encode_rtc_register_write, encode_xhci_reset, request, ClockRateQuery, FirmwareClock,
    FramebufferRequest, RtcRegister, FIRMWARE_CLOCKS_KEY, FIRMWARE_FRAMEBUFFER_KEY, PROPERTY_WORDS,
    RPI_RTC_KEY, TAG_SET_CLOCK_RATE, TAG_SET_RTC_REG, VL805_KEY,
};

const EXECUTE_CODE: u32 = 0x0003_0010;

/// The node a driver is loaded for, carrying `keys` as discovery publishes
/// them.
fn node(class: HwDeviceClass, keys: &[HwMatchKey]) -> HwNode {
    let mut node = HwNode::new(7, 1, class);
    for &key in keys {
        node.push_match_key(key).expect("a node holds its keys");
    }
    node
}

/// The four drivers the service answers, each loaded for its device's node.
fn clients() -> [HwNode; 4] {
    let simple_framebuffer = HwMatchKey::compatible(b"simple-framebuffer").expect("fits");
    [
        node(
            HwDeviceClass::Display,
            &[FIRMWARE_FRAMEBUFFER_KEY, simple_framebuffer],
        ),
        node(HwDeviceClass::Bus, &[VL805_KEY]),
        node(HwDeviceClass::Rtc, &[RPI_RTC_KEY]),
        node(HwDeviceClass::Other, &[FIRMWARE_CLOCKS_KEY]),
    ]
}

/// Each driver's working set: the index of its driver in [`clients`], and one
/// request of it.
fn working_sets() -> [(usize, [u32; PROPERTY_WORDS]); 10] {
    [
        (0, encode_blank_screen(true)),
        (0, encode_blank_screen(false)),
        (1, encode_xhci_reset()),
        (2, encode_rtc_register_query(RtcRegister::Time)),
        (2, encode_rtc_register_query(RtcRegister::BackupVolts)),
        (
            2,
            encode_rtc_register_write(RtcRegister::Time, 1_700_000_000),
        ),
        (
            3,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Current),
        ),
        (
            3,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Max),
        ),
        (
            3,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Min),
        ),
        (
            3,
            encode_clock_rate_write(FirmwareClock::Arm, 1_500_000_000),
        ),
    ]
}

/// A refusal naming `message`'s tag, the only one it carries.
fn refused(message: &[u32; PROPERTY_WORDS], errno: Errno) -> Result<(), Refusal> {
    Err(Refusal {
        tag: Some(message[2]),
        errno,
    })
}

#[test]
fn each_driver_working_set_passes_from_the_driver_of_its_device() {
    let clients = clients();
    for (owner, message) in working_sets() {
        assert_eq!(
            admit_request(&message, Some(&clients[owner])),
            Ok(()),
            "{message:x?}"
        );
    }
}

/// The real-time clock's driver may not move the core clock, nor the
/// framebuffer's reload the USB controller: each tag is its own device's.
#[test]
fn a_driver_is_refused_every_other_device_working_set() {
    let clients = clients();
    for (owner, message) in working_sets() {
        for (other, client) in clients.iter().enumerate() {
            if other != owner {
                assert_eq!(
                    admit_request(&message, Some(client)),
                    refused(&message, Errno::PermissionDenied),
                    "{message:x?} from client {other}"
                );
            }
        }
    }
}

#[test]
fn a_caller_loaded_for_no_device_is_refused_and_the_tag_it_named_is_reported() {
    for (_, message) in working_sets() {
        assert_eq!(
            admit_request(&message, None),
            refused(&message, Errno::PermissionDenied),
            "{message:x?}"
        );
    }
}

/// Code execution, GPU memory, voltage, one-time-programmable bits, GPIO, the
/// framebuffer's allocation and even the harmless revision probe are no
/// driver's, so no caller may send them, not even one loaded for a node
/// carrying every client's key.
#[test]
fn every_tag_outside_the_working_sets_is_refused_to_every_caller() {
    const ALLOCATE_MEMORY: u32 = 0x0003_000c;
    const LOCK_MEMORY: u32 = 0x0003_000d;
    const SET_VOLTAGE: u32 = 0x0003_8003;
    const SET_CUSTOMER_OTP: u32 = 0x0003_8021;
    let everything = node(
        HwDeviceClass::Other,
        &[
            FIRMWARE_FRAMEBUFFER_KEY,
            VL805_KEY,
            RPI_RTC_KEY,
            FIRMWARE_CLOCKS_KEY,
        ],
    );
    let refused_tags = [
        request(&[(EXECUTE_CODE, &[0x1000, 0, 0, 0, 0, 0, 0])]),
        request(&[(ALLOCATE_MEMORY, &[4096, 4096, 0])]),
        request(&[(LOCK_MEMORY, &[1])]),
        request(&[(SET_VOLTAGE, &[1, 8])]),
        request(&[(SET_CUSTOMER_OTP, &[0, 1, 0xffff_ffff])]),
        encode_gpio_state_write(0, true),
        encode_firmware_revision_query(),
    ];
    for message in refused_tags {
        assert_eq!(
            admit_request(&message, Some(&everything)),
            refused(&message, Errno::PermissionDenied),
            "{message:x?}"
        );
    }
    let allocation = FramebufferRequest {
        width_px: 640,
        height_px: 480,
        format: DisplayFormat::Bgra8888,
    }
    .encode()
    .expect("encode");
    assert_eq!(
        admit_request(&allocation, Some(&everything)),
        refused(&allocation, Errno::PermissionDenied)
    );
}

#[test]
fn a_tag_in_a_shape_its_driver_never_sends_is_refused() {
    let [_, vl805, rtc, clocks] = clients();
    let arm = FirmwareClock::Arm.as_u32();
    // The clock write with its turbo word missing answered a lowering with the
    // ceiling; with the word clear it performs the turbo transition.
    let no_turbo_word = request(&[(TAG_SET_CLOCK_RATE, &[arm, 600_000_000])]);
    let turbo = request(&[(TAG_SET_CLOCK_RATE, &[arm, 600_000_000, 0])]);
    let peripheral_clock = encode_clock_rate_write(FirmwareClock::Emmc2, 200_000_000);
    let peripheral_query = encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current);
    let mut stray_slot = encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Max);
    stray_slot[6] = 1;
    for message in [
        no_turbo_word,
        turbo,
        peripheral_clock,
        peripheral_query,
        stray_slot,
    ] {
        assert_eq!(
            admit_request(&message, Some(&clocks)),
            refused(&message, Errno::PermissionDenied),
            "{message:x?}"
        );
    }

    let charging = encode_rtc_register_write(RtcRegister::BackupVolts, 3300);
    let mut stray_rtc_slot = encode_rtc_register_query(RtcRegister::Time);
    stray_rtc_slot[6] = 9;
    let alarm = request(&[(TAG_SET_RTC_REG, &[1, 0])]);
    for message in [charging, stray_rtc_slot, alarm] {
        assert_eq!(
            admit_request(&message, Some(&rtc)),
            refused(&message, Errno::PermissionDenied),
            "{message:x?}"
        );
    }

    let mut another_controller = encode_xhci_reset();
    another_controller[5] = 0x0020_0000;
    assert_eq!(
        admit_request(&another_controller, Some(&vl805)),
        refused(&another_controller, Errno::PermissionDenied)
    );
}

#[test]
fn a_blank_state_outside_the_two_its_encoder_spells_is_refused() {
    let [framebuffer, ..] = clients();
    let mut message = encode_blank_screen(false);
    message[5] = 2;
    assert_eq!(
        admit_request(&message, Some(&framebuffer)),
        refused(&message, Errno::PermissionDenied)
    );
}

#[test]
fn a_request_carries_exactly_one_tag() {
    let [.., rtc, _] = clients();
    let time = encode_rtc_register_query(RtcRegister::Time);
    let twice = request(&[(time[2], &[0, 0]), (time[2], &[0, 0])]);
    assert_eq!(
        admit_request(&twice, Some(&rtc)),
        refused(&twice, Errno::PermissionDenied)
    );
    let smuggled = request(&[
        (time[2], &[0, 0]),
        (EXECUTE_CODE, &[0x1000, 0, 0, 0, 0, 0, 0]),
    ]);
    assert_eq!(
        admit_request(&smuggled, Some(&rtc)),
        refused(&smuggled, Errno::PermissionDenied)
    );
    let empty = request(&[]);
    assert_eq!(
        admit_request(&empty, Some(&rtc)),
        Err(Refusal {
            tag: None,
            errno: Errno::PermissionDenied
        })
    );
}

#[test]
fn a_malformed_buffer_is_refused_before_any_tag_is_trusted() {
    let [.., rtc, _] = clients();
    let good = encode_rtc_register_query(RtcRegister::Time);
    let with = |edit: &dyn Fn(&mut [u32; PROPERTY_WORDS])| {
        let mut message = good;
        edit(&mut message);
        admit_request(&message, Some(&rtc)).map_err(|refusal| refusal.errno)
    };
    assert_eq!(
        with(&|m| m[0] = 13),
        Err(Errno::LengthOutOfRange),
        "not whole words"
    );
    assert_eq!(
        with(&|m| m[0] = 8),
        Err(Errno::LengthOutOfRange),
        "no room for an end tag"
    );
    assert_eq!(
        with(&|m| m[0] = u32::try_from(PROPERTY_WORDS * 4 + 4).expect("fits")),
        Err(Errno::LengthOutOfRange),
        "longer than the buffer"
    );
    assert_eq!(
        with(&|m| m[1] = 0x8000_0000),
        Err(Errno::OutOfRange),
        "a response code"
    );
    assert_eq!(
        with(&|m| m[4] = 0x8000_0008),
        Err(Errno::OutOfRange),
        "a tag answered already"
    );
    assert_eq!(
        with(&|m| m[3] = 6),
        Err(Errno::LengthOutOfRange),
        "a part-word value"
    );
    assert_eq!(
        with(&|m| m[3] = 0x7fff_fffc),
        Err(Errno::LengthOutOfRange),
        "a tag that overruns"
    );
    assert_eq!(
        with(&|m| m[0] = 6 * 4),
        Err(Errno::LengthOutOfRange),
        "declared to end before the end tag"
    );
    assert_eq!(
        with(&|m| m[0] += 4),
        Err(Errno::LengthOutOfRange),
        "declared to run past the end tag"
    );
}
