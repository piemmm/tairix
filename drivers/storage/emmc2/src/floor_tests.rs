//! The floor bring-up's supplies and clock over the protocol-faithful mock
//! firmware, and its refusals, each recorded.

extern crate std;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use std::sync::Mutex;

use tairix_abi::blkio::BlkReissue;
use tairix_log::FieldValue;

use tairix_abi::driver::mailbox::{MailboxChannel, MAILBOX_PROPERTY_WORDS};
use tairix_abi::driver::supply::{IoRail, PowerRail};
use tairix_abi::driver::timing::{CompletionWait, Delay};
use tairix_abi::driver::{CompletionSignal, DriverKind};
use tairix_abi::{CapabilityId, DriverError, DriverHost, HwDeviceClass, HwNode, HwResource};
use tairix_log::{Event, Sink};
use tairix_vcmailbox::mock::MockFirmware;
use tairix_vcmailbox::{MailboxError, MailboxTransport};

use super::{base_clock_hz, open, FailureLog, FirmwareSupply, BRING_UP_FAILED, TRANSFER_FAILED};
use crate::regs;
use crate::trace::{FaultReport, Trace, TransferFault, WaitEnd};
use crate::{CardSupply, SignalVoltage};

const IO_LINE: u8 = 4;
const POWER_LINE: u8 = 6;

/// The mock firmware behind the channel a host lends.
struct Firmware(RefCell<MockFirmware>);

impl MailboxChannel for Firmware {
    fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        self.0
            .borrow_mut()
            .exchange(message)
            .map_err(MailboxError::as_driver_error)
    }
}

/// A delay that only advances a virtual clock and records each wait.
#[derive(Default)]
struct RecordingDelay {
    now_us: Cell<u64>,
    waits: RefCell<Vec<u32>>,
}

impl Delay for RecordingDelay {
    fn delay_us(&self, us: u32) {
        self.now_us.set(self.now_us.get() + u64::from(us));
        self.waits.borrow_mut().push(us);
    }

    fn now_us(&self) -> u64 {
        self.now_us.get()
    }
}

/// A completion wait for a bring-up that never reaches the controller.
struct Unreached;

impl Delay for Unreached {
    fn delay_us(&self, _us: u32) {}

    fn now_us(&self) -> u64 {
        0
    }
}

impl CompletionWait for Unreached {
    fn await_irq(&self) -> CompletionSignal {
        CompletionSignal::TimedOut
    }
}

/// The ids of every event recorded.
#[derive(Default)]
struct Recorder(Mutex<Vec<u32>>);

impl Sink for Recorder {
    fn write_event(&self, event: &Event<'_>) {
        self.0.lock().expect("recorder").push(event.id.0);
    }
}

impl Recorder {
    /// A recorder for the life of the test binary, as a driver keeps its log.
    fn leaked() -> &'static Self {
        Box::leak(Box::default())
    }

    fn ids(&self) -> Vec<u32> {
        self.0.lock().expect("recorder").clone()
    }
}

/// A host granting `MMIO_MAP` as told, with no mapper, DMA or firmware.
struct BareHost {
    mmio_map: bool,
}

impl DriverHost for BareHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::MMIO_MAP && self.mmio_map
    }

    fn kind(&self) -> DriverKind {
        DriverKind::InKernel
    }
}

/// The card's rails as the Pi 4's tree wires them: line 4 high selects
/// 1.8 V and settles in 5 ms, line 6 high powers the card.
fn rails(off_on_us: u32) -> (IoRail, PowerRail) {
    (
        IoRail {
            line: IO_LINE,
            level_1v8: Some(true),
            level_3v3: Some(false),
            settle_us: 5000,
        },
        PowerRail {
            line: POWER_LINE,
            active_low: false,
            startup_us: 0,
            off_on_us,
        },
    )
}

/// The SD host's node, stating `io` and `power` where given and the register
/// window `window` where given.
fn sd_node(io: Option<IoRail>, power: Option<PowerRail>, window: bool) -> HwNode {
    let mut node = HwNode::new(3, 0, HwDeviceClass::Storage);
    if let Some(io) = io {
        node.push_resource(HwResource::io_rail(io)).expect("room");
    }
    if let Some(power) = power {
        let fact = HwResource::power_rail(power).expect("fits");
        node.push_resource(fact).expect("room");
    }
    if window {
        node.push_resource(HwResource::mmio(0xfe34_0000, 0x100))
            .expect("room");
    }
    node
}

fn firmware() -> Firmware {
    Firmware(RefCell::new(MockFirmware::healthy()))
}

#[test]
fn a_node_stating_both_rails_and_no_fewer_switches_the_card() {
    let firmware = firmware();
    let delay = RecordingDelay::default();
    let (io, power) = rails(0);
    for (io, power, switches) in [
        (Some(io), Some(power), true),
        (Some(io), None, false),
        (None, Some(power), false),
        (None, None, false),
    ] {
        let node = sd_node(io, power, true);
        assert_eq!(
            FirmwareSupply::new(&firmware, &node, &delay).is_some(),
            switches,
            "{io:?} {power:?}"
        );
    }
}

#[test]
fn a_zero_clock_is_no_clock() {
    let firmware = firmware();
    assert_eq!(base_clock_hz(&firmware), Some(100_000_000));
    firmware.0.borrow_mut().emmc2.hz = 0;
    assert_eq!(base_clock_hz(&firmware), None);
}

#[test]
fn signalling_drives_the_select_line_and_waits_for_it_to_settle() {
    let firmware = firmware();
    let delay = RecordingDelay::default();
    let (io, power) = rails(0);
    let node = sd_node(Some(io), Some(power), true);
    let mut supply = FirmwareSupply::new(&firmware, &node, &delay).expect("both rails");
    supply
        .set_signal_voltage(SignalVoltage::V1_8)
        .expect("1.8 V");
    assert_eq!(*delay.waits.borrow(), [5000]);
    assert!(firmware.0.borrow().gpio_high(IO_LINE), "high selects 1.8 V");

    let without_3v3 = IoRail {
        level_3v3: None,
        ..io
    };
    let node = sd_node(Some(without_3v3), Some(power), true);
    let mut supply = FirmwareSupply::new(&firmware, &node, &delay).expect("both rails");
    assert_eq!(
        supply.set_signal_voltage(SignalVoltage::V3_3),
        Err(DriverError::Unsupported),
        "a voltage the rail has no state for"
    );
}

#[test]
fn power_back_on_waits_out_the_rest_of_the_off_on_delay() {
    let firmware = firmware();
    firmware.0.borrow_mut().gpio_levels = 1 << POWER_LINE;
    let delay = RecordingDelay::default();
    let (io, power) = rails(3000);
    let node = sd_node(Some(io), Some(power), true);
    let mut supply = FirmwareSupply::new(&firmware, &node, &delay).expect("both rails");
    supply.set_card_power(false).expect("off");
    delay.delay_us(1000);
    supply.set_card_power(true).expect("on");
    assert_eq!(
        *delay.waits.borrow(),
        [1000, 2000, 0],
        "the rest of the off-on delay, then the undeclared start-up"
    );
    assert!(
        firmware.0.borrow().gpio_high(POWER_LINE),
        "active high: on is high"
    );
}

#[test]
fn a_refused_switch_is_the_firmwares_error() {
    let firmware = firmware();
    firmware.0.borrow_mut().gpio_lines = IO_LINE;
    let delay = RecordingDelay::default();
    let (io, power) = rails(0);
    let node = sd_node(Some(io), Some(power), true);
    let mut supply = FirmwareSupply::new(&firmware, &node, &delay).expect("both rails");
    assert_eq!(
        supply.set_signal_voltage(SignalVoltage::V1_8),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(supply.set_card_power(false), Err(DriverError::DeviceFault));
    assert!(
        delay.waits.borrow().is_empty(),
        "nothing settles that never moved"
    );
}

#[test]
fn open_refuses_a_node_naming_no_register_window_and_says_so() {
    let audit = Recorder::leaked();
    let wait: &'static Unreached = Box::leak(Box::new(Unreached));
    let host = BareHost { mmio_map: true };
    let refused = open(&host, &sd_node(None, None, false), wait, audit);
    assert_eq!(refused.err(), Some(DriverError::NotFound));
    assert_eq!(audit.ids(), [BRING_UP_FAILED.0]);
}

#[test]
fn open_refuses_a_host_that_may_not_map_the_window_and_says_so() {
    let audit = Recorder::leaked();
    let wait: &'static Unreached = Box::leak(Box::new(Unreached));
    let host = BareHost { mmio_map: false };
    let refused = open(&host, &sd_node(None, None, true), wait, audit);
    assert_eq!(refused.err(), Some(DriverError::PermissionDenied));
    assert_eq!(audit.ids(), [BRING_UP_FAILED.0]);
}

/// Every event recorded, as its id and its fields rendered `key=value`.
#[derive(Default)]
struct FieldRecorder(Mutex<Vec<(u32, Vec<String>)>>);

impl Sink for FieldRecorder {
    fn write_event(&self, event: &Event<'_>) {
        let fields = event
            .fields
            .iter()
            .map(|field| format!("{}={:?}", field.key, field.value))
            .collect();
        self.0.lock().expect("recorder").push((event.id.0, fields));
    }
}

impl FieldRecorder {
    fn leaked() -> &'static Self {
        Box::leak(Box::default())
    }

    fn events(&self) -> Vec<(u32, Vec<String>)> {
        self.0.lock().expect("recorder").clone()
    }
}

/// A read of `blocks` at `lba` the bus corrupted on `CMD18`, the controller
/// back in a known state.
fn corrupted_read(lba: u64, blocks: u64) -> Trace {
    Trace::TransferFailed {
        write: false,
        lba,
        blocks,
        fault: Some(TransferFault {
            command: 18,
            report: FaultReport::Wait {
                register: regs::REG_INTERRUPT,
                value: regs::INT_ERROR | regs::INT_DATA_CRC,
                end: WaitEnd::Error,
            },
        }),
        error: DriverError::TransferCorrupted,
        reissue: BlkReissue::Now,
    }
}

#[test]
fn a_failed_transfer_is_recorded_with_the_step_and_what_the_controller_saw() {
    let audit = FieldRecorder::leaked();
    let mut log = FailureLog::new(audit);
    log.note(&corrupted_read(4096, 512));
    let events = audit.events();
    let [(id, fields)] = &events[..] else {
        panic!("one record, got {events:?}");
    };
    assert_eq!(*id, TRANSFER_FAILED.0);
    let status = u64::from(regs::INT_ERROR | regs::INT_DATA_CRC);
    let register = regs::REG_INTERRUPT as u64;
    assert_eq!(
        *fields,
        [
            "op=Str(\"read\")".to_string(),
            "lba=UnsignedInt(4096)".to_string(),
            "blocks=UnsignedInt(512)".to_string(),
            "command=UnsignedInt(18)".to_string(),
            "report=Str(\"controller\")".to_string(),
            format!("register=UnsignedInt({register})"),
            format!("status=UnsignedInt({status})"),
            format!(
                "error={:?}",
                FieldValue::Error(DriverError::TransferCorrupted.as_errno())
            ),
            "reissue=Str(\"now\")".to_string(),
            "failed=UnsignedInt(1)".to_string(),
        ]
    );
}

#[test]
fn only_a_failed_transfer_is_counted() {
    let audit = FieldRecorder::leaked();
    let mut log = FailureLog::new(audit);
    log.note(&Trace::Command { index: 18, arg: 0 });
    log.note(&Trace::Response {
        index: 18,
        response: 0,
    });
    assert!(audit.events().is_empty());
}

#[test]
fn a_card_failing_every_transfer_is_recorded_each_time_the_count_doubles() {
    let audit = FieldRecorder::leaked();
    let mut log = FailureLog::new(audit);
    for lba in 0..9 {
        log.note(&corrupted_read(lba, 1));
    }
    let counts: Vec<String> = audit
        .events()
        .into_iter()
        .filter_map(|(_, fields)| fields.into_iter().find(|f| f.starts_with("failed=")))
        .collect();
    assert_eq!(
        counts,
        [
            "failed=UnsignedInt(1)",
            "failed=UnsignedInt(2)",
            "failed=UnsignedInt(4)",
            "failed=UnsignedInt(8)",
        ]
    );
}
