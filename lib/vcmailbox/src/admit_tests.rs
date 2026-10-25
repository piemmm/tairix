//! The admission policy against every driver's working set, against callers
//! that are not the driver of the device a tag concerns, against the tags no
//! driver may send, and against hostile buffer shapes.

use tairix_abi::driver::display::DisplayFormat;
use tairix_abi::driver::supply::{IoRail, PowerRail};
use tairix_abi::{Errno, HwDeviceClass, HwMatchKey, HwNode, HwResource};

use super::{admit_request, Refusal};
use crate::{
    encode_clock_rate_query, encode_clock_rate_write, encode_display_done,
    encode_display_size_query, encode_firmware_revision_query, encode_gpio_state_write,
    encode_rtc_register_query, encode_rtc_register_write, encode_xhci_reset, request,
    ClockRateQuery, FirmwareClock, FramebufferRequest, RtcRegister, DISPLAY_ENGINE_KEY,
    FIRMWARE_CLOCKS_KEY, PROPERTY_WORDS, RPI_RTC_KEY, SD_HOST_KEY, SKIP_SETTING_TURBO,
    TAG_SET_CLOCK_RATE, TAG_SET_RTC_REG, VL805_KEY,
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

/// The expander lines the SD host's node states switch its I/O and power
/// rails.
const SD_IO_LINE: u8 = 4;
const SD_POWER_LINE: u8 = 6;

/// The SD host's node as discovery publishes it, with its rails when `rails`.
fn sd_host(rails: bool) -> HwNode {
    let mut host = node(HwDeviceClass::Storage, &[SD_HOST_KEY]);
    if rails {
        host.push_resource(HwResource::io_rail(IoRail {
            line: SD_IO_LINE,
            level_1v8: Some(true),
            level_3v3: Some(false),
            settle_us: 5000,
        }))
        .expect("room");
        let power = PowerRail {
            line: SD_POWER_LINE,
            active_low: false,
            startup_us: 0,
            off_on_us: 0,
        };
        host.push_resource(HwResource::power_rail(power).expect("fits"))
            .expect("room");
    }
    host
}

/// The five drivers the service answers, each loaded for its device's node.
fn clients() -> [HwNode; 5] {
    [
        node(HwDeviceClass::Bus, &[VL805_KEY]),
        node(HwDeviceClass::Rtc, &[RPI_RTC_KEY]),
        node(HwDeviceClass::Other, &[FIRMWARE_CLOCKS_KEY]),
        node(HwDeviceClass::Display, &[DISPLAY_ENGINE_KEY]),
        sd_host(true),
    ]
}

/// Each driver's working set: the index of its driver in [`clients`], and one
/// request of it.
fn working_sets() -> [(usize, [u32; PROPERTY_WORDS]); 27] {
    [
        (
            4,
            encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current),
        ),
        (4, encode_gpio_state_write(SD_IO_LINE, true)),
        (4, encode_gpio_state_write(SD_IO_LINE, false)),
        (4, encode_gpio_state_write(SD_POWER_LINE, true)),
        (4, encode_gpio_state_write(SD_POWER_LINE, false)),
        (3, encode_display_done()),
        (3, encode_display_size_query()),
        (0, encode_xhci_reset()),
        (1, encode_rtc_register_query(RtcRegister::Time)),
        (1, encode_rtc_register_query(RtcRegister::BackupVolts)),
        (
            1,
            encode_rtc_register_write(RtcRegister::Time, 1_700_000_000),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Current),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Max),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Min),
        ),
        (
            2,
            encode_clock_rate_write(FirmwareClock::Arm, 1_500_000_000),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Core, ClockRateQuery::Current),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Core, ClockRateQuery::Max),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::Core, ClockRateQuery::Min),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::M2mc, ClockRateQuery::Current),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::M2mc, ClockRateQuery::Max),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::M2mc, ClockRateQuery::Min),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::PixelBvb, ClockRateQuery::Current),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::PixelBvb, ClockRateQuery::Max),
        ),
        (
            2,
            encode_clock_rate_query(FirmwareClock::PixelBvb, ClockRateQuery::Min),
        ),
        (2, encode_clock_rate_write(FirmwareClock::Core, 500_000_000)),
        (2, encode_clock_rate_write(FirmwareClock::M2mc, 150_000_000)),
        (
            2,
            encode_clock_rate_write(FirmwareClock::PixelBvb, 150_000_000),
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

/// Code execution, GPU memory, voltage, one-time-programmable bits, a GPIO
/// line no rail of the caller's names, the framebuffer's allocation and even
/// the harmless revision probe are no driver's, so no caller may send them,
/// not even one loaded for a node carrying every client's key.
#[test]
fn every_tag_outside_the_working_sets_is_refused_to_every_caller() {
    const ALLOCATE_MEMORY: u32 = 0x0003_000c;
    const LOCK_MEMORY: u32 = 0x0003_000d;
    const SET_VOLTAGE: u32 = 0x0003_8003;
    const SET_CUSTOMER_OTP: u32 = 0x0003_8021;
    let everything = node(
        HwDeviceClass::Other,
        &[VL805_KEY, RPI_RTC_KEY, FIRMWARE_CLOCKS_KEY, SD_HOST_KEY],
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

/// Regression: the SD host's bring-up asks for its base clock, which no rule
/// answered while the kernel drove the doorbell unchecked.
#[test]
fn the_sd_host_may_read_its_own_clock() {
    let query = encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current);
    assert_eq!(admit_request(&query, Some(&sd_host(false))), Ok(()));
}

/// A switch is the SD host's only for a line its own node's rails name, and
/// its clock is the one it may read, never set or another's.
#[test]
fn the_sd_host_may_switch_only_the_lines_its_node_names() {
    let host = sd_host(true);
    let bare = sd_host(false);
    let another_line = encode_gpio_state_write(SD_IO_LINE + 1, true);
    assert_eq!(
        admit_request(&another_line, Some(&host)),
        refused(&another_line, Errno::PermissionDenied)
    );
    let own_line = encode_gpio_state_write(SD_IO_LINE, true);
    assert_eq!(
        admit_request(&own_line, Some(&bare)),
        refused(&own_line, Errno::PermissionDenied),
        "a node stating no rails switches nothing"
    );
    let mut stray_level = own_line;
    stray_level[6] = 2;
    for message in [
        stray_level,
        encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Max),
        encode_clock_rate_write(FirmwareClock::Emmc2, 200_000_000),
        encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Current),
    ] {
        assert_eq!(
            admit_request(&message, Some(&host)),
            refused(&message, Errno::PermissionDenied),
            "{message:x?}"
        );
    }
}

#[test]
fn a_tag_in_a_shape_its_driver_never_sends_is_refused() {
    let [vl805, rtc, clocks, ..] = clients();
    let arm = FirmwareClock::Arm.as_u32();
    // The clock write with its turbo word missing answered a lowering with the
    // ceiling; with the word clear it performs the turbo transition.
    let no_turbo_word = request(&[(TAG_SET_CLOCK_RATE, &[arm, 600_000_000])]);
    let turbo = request(&[(TAG_SET_CLOCK_RATE, &[arm, 600_000_000, 0])]);
    let peripheral_clock = encode_clock_rate_write(FirmwareClock::Emmc2, 200_000_000);
    let peripheral_query = encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current);
    // The 3D engine's clock feeds a device another driver owns.
    let v3d = request(&[(TAG_SET_CLOCK_RATE, &[5, 500_000_000, SKIP_SETTING_TURBO])]);
    let mut stray_slot = encode_clock_rate_query(FirmwareClock::Arm, ClockRateQuery::Max);
    stray_slot[6] = 1;
    for message in [
        no_turbo_word,
        turbo,
        peripheral_clock,
        peripheral_query,
        v3d,
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

    let [.., engine, _] = clients();
    let mut stray_size_slot = encode_display_size_query();
    stray_size_slot[6] = 1920;
    assert_eq!(
        admit_request(&stray_size_slot, Some(&engine)),
        refused(&stray_size_slot, Errno::PermissionDenied)
    );
}

#[test]
fn a_request_carries_exactly_one_tag() {
    let [_, rtc, ..] = clients();
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
    let [_, rtc, ..] = clients();
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
