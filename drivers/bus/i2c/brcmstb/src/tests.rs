//! The DDC controller against a simulated controller and the targets on its
//! bus: an E-DDC EEPROM behind its segment pointer, and targets that are
//! absent, stretch the clock, or hold the bus for good.
//!
//! The simulation runs each command when it is enabled and shows it done
//! only once its time on the wire has passed, so a driver that reads back
//! early, or frames a command wrongly, is caught on the wire it records.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::num::NonZeroU32;

use tairix_abi::driver::i2c::{
    BusFacts, Direction, I2cAddress, I2cFault, I2cMessage, I2cTransaction, RepeatedStart,
    I2C_BUFFER_LEN,
};
use tairix_abi::{
    CapabilityId, Delay, DriverError, DriverHost, DriverKind, HwMatchKey, RegisterBlock,
    HW_COMPATIBLE_MAX,
};
use tairix_i2cbus::{wire_ns, I2cBus, STALL_ALLOWANCE_NS};

use super::{
    register, speed, Ddc, AUTO_I2C_BLOCK_LEN, AUTO_I2C_CTRL0, AUTO_I2C_RELEASE_BSC, BIND_KEYS,
    CHIP_ADDRESS, CNT, CTL, CTLHI, CTLHI_DATAREG_SIZE, CTL_DIV_CLK, CTL_READ, DATA_IN, DATA_OUT,
    DDC_COMPATIBLE, EN_ENABLE, EN_INTRP, EN_NOACK, EN_NOSTART, EN_NOSTOP, EN_RESTART, IIC_ENABLE,
    REGISTER_BLOCK_LEN, REQUIRED_CAPABILITIES,
};

const SEGMENT: u8 = 0x30;
const EEPROM: u8 = 0x50;
const DDC_HZ: u32 = 97_500;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Wire {
    Start { address: u8, read: bool },
    Restart { address: u8, read: bool },
    Byte(u8),
    Nack,
    Stop,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Target {
    /// Two 256-byte EDID segments, addressed by an offset written to it.
    Eeprom,
    /// The E-DDC segment pointer, cleared by every STOP.
    Segment,
    /// Holds the clock low for good once addressed.
    Wedged,
    /// Holds the clock low this long once addressed, then carries on.
    Stretches(u64),
}

/// What EDID byte `at` of the two segments holds.
fn edid(at: usize) -> u8 {
    u8::try_from(at % 251).expect("small") ^ 0x5A
}

#[derive(Copy, Clone)]
struct Running {
    started_us: u64,
    /// When the command shows done; never, for a bus held for good.
    done_us: Option<u64>,
    nack: bool,
    polled: bool,
}

#[derive(Default)]
struct State {
    chip_address: u32,
    data_in: [u32; 8],
    data_out: [u32; 8],
    cnt: u32,
    ctl: u32,
    enable: u32,
    ctlhi: u32,
    targets: Vec<(u8, Target)>,
    running: Option<Running>,
    /// Whether the last command left the bus held, without a STOP.
    held: bool,
    segment: usize,
    offset: usize,
    /// Bytes written to the EEPROM, in order, as it received them.
    written: Vec<u8>,
    wire: Vec<Wire>,
    commands: Vec<u32>,
    /// How long after each command started the driver first read it back.
    first_polls: Vec<u64>,
    now_us: u64,
    sleeps: usize,
}

impl State {
    fn target(&self, address: u8) -> Option<Target> {
        self.targets
            .iter()
            .find_map(|&(at, target)| (at == address).then_some(target))
    }

    fn stop(&mut self) {
        self.wire.push(Wire::Stop);
        self.held = false;
        self.segment = 0;
    }

    fn run(&mut self, enable: u32) {
        let conditions = enable & (EN_NOSTART | EN_NOSTOP | EN_RESTART);
        self.commands.push(conditions);
        let read = self.ctl & CTL_READ != 0;
        let address = u8::try_from(self.chip_address >> 1).expect("an address byte");
        assert_eq!(
            self.chip_address & 1 != 0,
            read,
            "the address byte's direction matches the command's"
        );
        let len = usize::try_from(self.cnt).expect("short");
        assert!((1..=32).contains(&len), "a command moves one to 32 bytes");
        let mut stall = 0;
        let fresh = conditions & EN_NOSTART == 0;
        if fresh {
            self.wire.push(if self.held {
                Wire::Restart { address, read }
            } else {
                Wire::Start { address, read }
            });
        }
        let target = self.target(address);
        let nack = match target {
            None => fresh,
            Some(Target::Wedged) => {
                self.running = Some(Running {
                    started_us: self.now_us,
                    done_us: None,
                    nack: false,
                    polled: false,
                });
                return;
            }
            Some(Target::Stretches(us)) => {
                stall = us;
                false
            }
            Some(_) => false,
        };
        if nack {
            self.wire.push(Wire::Nack);
            self.stop();
        } else {
            for at in 0..len {
                let byte = if read {
                    let byte = edid(self.segment * 256 + self.offset);
                    self.offset = (self.offset + 1) % 256;
                    let word = &mut self.data_out[at / 4];
                    *word |= u32::from(byte) << (8 * (at % 4));
                    byte
                } else {
                    let byte = self.data_in[at / 4].to_le_bytes()[at % 4];
                    match target {
                        Some(Target::Segment) => self.segment = usize::from(byte),
                        Some(Target::Eeprom) if fresh && at == 0 => self.offset = usize::from(byte),
                        Some(Target::Eeprom) => self.written.push(byte),
                        _ => {}
                    }
                    byte
                };
                self.wire.push(Wire::Byte(byte));
            }
            if conditions & EN_NOSTOP == 0 {
                self.stop();
            } else {
                self.held = true;
            }
        }
        let wire_us =
            wire_ns(u64::try_from(len).expect("short"), u64::from(DDC_HZ)).div_ceil(1_000);
        self.running = Some(Running {
            started_us: self.now_us,
            done_us: Some(self.now_us + wire_us + stall),
            nack,
            polled: false,
        });
    }
}

struct Sim(RefCell<State>);

impl Sim {
    fn new(targets: &[(u8, Target)]) -> Self {
        Self(RefCell::new(State {
            targets: targets.to_vec(),
            ..State::default()
        }))
    }

    fn edid() -> Self {
        Self::new(&[(SEGMENT, Target::Segment), (EEPROM, Target::Eeprom)])
    }

    fn wire(&self) -> Vec<Wire> {
        self.0.borrow().wire.clone()
    }

    fn idle(&self) -> bool {
        let state = self.0.borrow();
        state.running.is_none() && state.enable == 0 && state.cnt == 0
    }
}

impl RegisterBlock for Sim {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        let mut state = self.0.borrow_mut();
        Ok(match offset {
            IIC_ENABLE => {
                let now = state.now_us;
                let mut status = state.enable;
                if let Some(running) = state.running.as_mut() {
                    if !running.polled {
                        running.polled = true;
                        let gap = now - running.started_us;
                        state.first_polls.push(gap);
                    }
                }
                if let Some(running) = state.running {
                    if running.done_us.is_some_and(|done| now >= done) {
                        status |= EN_INTRP;
                        if running.nack {
                            status |= EN_NOACK;
                        }
                    }
                }
                status
            }
            at if (DATA_OUT..DATA_OUT + 32).contains(&at) => state.data_out[(at - DATA_OUT) / 4],
            CTL => state.ctl,
            CTLHI => state.ctlhi,
            CNT => state.cnt,
            CHIP_ADDRESS => state.chip_address,
            _ => 0,
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        let mut state = self.0.borrow_mut();
        match offset {
            IIC_ENABLE => {
                state.enable = value;
                if value == 0 {
                    state.running = None;
                } else if value & EN_ENABLE != 0 {
                    assert!(state.running.is_none(), "one command at a time");
                    state.data_out = [0; 8];
                    state.run(value);
                }
            }
            at if (DATA_IN..DATA_IN + 32).contains(&at) => {
                state.data_in[(at - DATA_IN) / 4] = value;
            }
            CTL => state.ctl = value,
            CTLHI => state.ctlhi = value,
            CNT => state.cnt = value,
            CHIP_ADDRESS => state.chip_address = value,
            _ => {}
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        REGISTER_BLOCK_LEN
    }
}

impl Delay for &Sim {
    fn delay_us(&self, us: u32) {
        let mut state = self.0.borrow_mut();
        state.now_us += u64::from(us);
        state.sleeps += 1;
    }

    fn now_us(&self) -> u64 {
        self.0.borrow().now_us
    }
}

/// The automatic I²C engine's window, recording what was written to it.
#[derive(Default)]
struct AutoI2c(RefCell<Vec<(usize, u32)>>);

impl RegisterBlock for AutoI2c {
    fn read32(&self, _: usize) -> Result<u32, DriverError> {
        Ok(0)
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        self.0.borrow_mut().push((offset, value));
        Ok(())
    }

    fn block_len(&self) -> usize {
        0x300
    }
}

fn hz(raw: u32) -> NonZeroU32 {
    NonZeroU32::new(raw).expect("non-zero")
}

fn controller(sim: &Sim) -> Ddc<'_, &Sim> {
    Ddc::new(sim, &AutoI2c::default(), sim, hz(DDC_HZ)).expect("brought up")
}

fn message(address: u8, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: I2cAddress::new(address).expect("usable"),
        direction,
        offset,
        len,
    }
}

fn transaction(messages: &[I2cMessage]) -> I2cTransaction {
    I2cTransaction::new(messages).expect("valid")
}

/// The E-DDC read of EDID block `block`: its segment, its offset, its bytes.
fn edid_block(block: u8) -> (I2cTransaction, Vec<u8>) {
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    buffer[0] = block / 2;
    buffer[1] = (block % 2) * 128;
    (
        transaction(&[
            message(SEGMENT, Direction::Write, 0, 1),
            message(EEPROM, Direction::Write, 1, 1),
            message(EEPROM, Direction::Read, 2, 128),
        ]),
        buffer,
    )
}

#[test]
fn an_edid_block_is_read_in_one_transaction_through_its_segment_pointer() {
    let sim = Sim::edid();
    let mut ddc = controller(&sim);
    let (read, mut buffer) = edid_block(3);
    assert_eq!(ddc.transfer(&read, &mut buffer), Ok(Ok(())));
    let expected: Vec<u8> = (256 + 128..512).map(edid).collect();
    assert_eq!(buffer[2..130], expected);
    let mut wire = vec![
        Wire::Start {
            address: SEGMENT,
            read: false,
        },
        Wire::Byte(1),
        Wire::Restart {
            address: EEPROM,
            read: false,
        },
        Wire::Byte(128),
        Wire::Restart {
            address: EEPROM,
            read: true,
        },
    ];
    wire.extend(expected.iter().copied().map(Wire::Byte));
    wire.push(Wire::Stop);
    assert_eq!(sim.wire(), wire);
    assert_eq!(
        sim.0.borrow().commands,
        [
            EN_RESTART | EN_NOSTOP,
            EN_RESTART | EN_NOSTOP,
            EN_NOSTOP,
            EN_NOSTART | EN_NOSTOP,
            EN_NOSTART | EN_NOSTOP,
            EN_NOSTART,
        ],
        "a read longer than a command continues its message to the STOP"
    );
    assert!(sim.idle());
}

#[test]
fn a_write_longer_than_a_command_continues_its_message_rather_than_readdressing_it() {
    let sim = Sim::new(&[(EEPROM, Target::Eeprom)]);
    let mut ddc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    buffer[0] = 0x10;
    let payload: Vec<u8> = (0..40).collect();
    buffer[1..41].copy_from_slice(&payload);
    let write_then_read = transaction(&[
        message(EEPROM, Direction::Write, 0, 41),
        message(EEPROM, Direction::Read, 41, 4),
    ]);
    assert_eq!(ddc.transfer(&write_then_read, &mut buffer), Ok(Ok(())));
    let state = sim.0.borrow();
    assert_eq!(
        state.written, payload,
        "every byte after the offset, in order"
    );
    assert_eq!(
        state
            .wire
            .iter()
            .filter(|event| matches!(event, Wire::Start { .. } | Wire::Restart { .. }))
            .count(),
        2,
        "the write addressed once, the read once"
    );
    assert_eq!(state.commands[1], EN_NOSTART | EN_RESTART | EN_NOSTOP);
}

#[test]
fn a_refusal_names_the_message_it_ended_in() {
    let sim = Sim::new(&[(EEPROM, Target::Eeprom)]);
    let mut ddc = controller(&sim);
    let (read, mut buffer) = edid_block(0);
    assert_eq!(
        ddc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 0 })),
        "a sink with no segment pointer"
    );
    assert!(sim.idle());

    let sim = Sim::new(&[(SEGMENT, Target::Segment)]);
    let mut ddc = controller(&sim);
    assert_eq!(
        ddc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::NoAck { message: 1 })),
        "no EDID behind it"
    );
    assert_eq!(sim.wire().last(), Some(&Wire::Stop), "the bus is let go");
}

#[test]
fn a_target_holding_the_bus_times_out_and_the_controller_is_left_idle() {
    let sim = Sim::new(&[(EEPROM, Target::Wedged)]);
    let mut ddc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let read = transaction(&[message(EEPROM, Direction::Read, 0, 8)]);
    assert_eq!(
        ddc.transfer(&read, &mut buffer),
        Ok(Err(I2cFault::TimedOut { message: 0 }))
    );
    assert!(sim.idle());
    let state = sim.0.borrow();
    assert!(
        state.now_us * 1_000 >= STALL_ALLOWANCE_NS,
        "only once the budget is spent"
    );
    let interval = wire_ns(8, u64::from(DDC_HZ)).div_ceil(1_000);
    assert!(
        u64::try_from(state.sleeps).expect("small") <= state.now_us / interval + 1,
        "slept a command's time on the wire between looks, never spun"
    );
}

#[test]
fn a_stall_within_the_allowance_is_waited_out() {
    let sim = Sim::new(&[(EEPROM, Target::Stretches(STALL_ALLOWANCE_NS / 2_000))]);
    let mut ddc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let read = transaction(&[message(EEPROM, Direction::Read, 0, 8)]);
    assert_eq!(ddc.transfer(&read, &mut buffer), Ok(Ok(())));
}

#[test]
fn a_command_is_read_back_only_after_its_time_on_the_wire() {
    let sim = Sim::edid();
    let mut ddc = controller(&sim);
    let (read, mut buffer) = edid_block(1);
    ddc.transfer(&read, &mut buffer)
        .expect("ran")
        .expect("answered");
    let state = sim.0.borrow();
    let lens = [1u64, 1, 32, 32, 32, 32];
    assert_eq!(state.first_polls.len(), lens.len());
    for (gap, len) in state.first_polls.iter().zip(lens) {
        assert!(*gap * 1_000 >= wire_ns(len, u64::from(DDC_HZ)));
    }
}

#[test]
fn an_empty_message_is_refused_before_the_bus_is_touched() {
    let sim = Sim::edid();
    let mut ddc = controller(&sim);
    let mut buffer = vec![0u8; I2C_BUFFER_LEN];
    let probe = transaction(&[message(EEPROM, Direction::Write, 0, 0)]);
    assert_eq!(
        ddc.transfer(&probe, &mut buffer),
        Err(DriverError::Unsupported)
    );
    assert!(sim.wire().is_empty());
    assert!(sim.idle());
}

#[test]
fn the_bus_runs_at_the_fastest_clock_at_or_below_its_declared_speed() {
    let selected = |declared| speed(hz(declared)).map(|(hz, _)| hz);
    assert_eq!(selected(97_500), Some(97_500));
    assert_eq!(selected(100_000), Some(97_500));
    assert_eq!(selected(400_000), Some(390_000));
    assert_eq!(selected(46_875), Some(46_875));
    assert_eq!(selected(46_874), None);
    assert_eq!(
        speed(hz(97_500)).map(|(_, bits)| bits),
        Some(1 << 4 | CTL_DIV_CLK)
    );

    let sim = Sim::edid();
    assert!(matches!(
        Ddc::new(&sim, &AutoI2c::default(), &sim, hz(40_000)),
        Err(DriverError::OutOfRange)
    ));
    let ddc = controller(&sim);
    assert_eq!(sim.0.borrow().ctl, 1 << 4 | CTL_DIV_CLK);
    assert_eq!(
        ddc.facts(),
        BusFacts {
            speed_hz: DDC_HZ,
            repeated_start: RepeatedStart::Any,
        }
    );
}

#[test]
fn bring_up_takes_the_controller_from_the_automatic_engine() {
    let sim = Sim::edid();
    sim.0.borrow_mut().enable = EN_INTRP;
    let auto = AutoI2c::default();
    let _ = Ddc::new(&sim, &auto, &sim, hz(DDC_HZ)).expect("brought up");
    assert_eq!(*auto.0.borrow(), [(AUTO_I2C_CTRL0, AUTO_I2C_RELEASE_BSC)]);
    let state = sim.0.borrow();
    assert_eq!(state.ctlhi, CTLHI_DATAREG_SIZE, "four-byte data registers");
    assert_eq!(state.enable, 0);
}

#[test]
fn a_window_shorter_than_the_registers_is_refused() {
    struct Short(usize);
    impl RegisterBlock for Short {
        fn read32(&self, _: usize) -> Result<u32, DriverError> {
            Ok(0)
        }
        fn write32(&self, _: usize, _: u32) -> Result<(), DriverError> {
            Ok(())
        }
        fn block_len(&self) -> usize {
            self.0
        }
    }
    let sim = Sim::edid();
    let bsc = Short(REGISTER_BLOCK_LEN - 4);
    assert!(matches!(
        Ddc::new(&bsc, &AutoI2c::default(), &sim, hz(DDC_HZ)),
        Err(DriverError::LengthOutOfRange)
    ));
    let auto = Short(AUTO_I2C_BLOCK_LEN - 4);
    assert!(matches!(
        Ddc::new(&sim, &auto, &sim, hz(DDC_HZ)),
        Err(DriverError::LengthOutOfRange)
    ));
}

struct Host {
    caps: &'static [CapabilityId],
}

impl DriverHost for Host {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.caps.contains(&cap)
    }

    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

#[test]
fn register_requires_the_load_capability() {
    assert!(register(&Host {
        caps: &[CapabilityId::DRV_LOAD]
    })
    .is_ok());
    assert_eq!(
        register(&Host { caps: &[] }).err(),
        Some(DriverError::PermissionDenied)
    );
}

#[test]
fn the_bind_table_names_the_controller_and_fits_the_abi_bound() {
    assert_eq!(BIND_KEYS.len(), 1);
    assert!(DDC_COMPATIBLE.len() <= HW_COMPATIBLE_MAX);
    assert_eq!(
        BIND_KEYS[0].key,
        HwMatchKey::compatible(DDC_COMPATIBLE).expect("fits")
    );
}

/// A polled controller binds no interrupt and needs no clock link; its
/// channels' buffers and its log it does need.
#[test]
fn the_manifest_requests_what_a_polled_controller_needs_and_nothing_else() {
    for cap in [
        CapabilityId::SHM,
        CapabilityId::MMIO_MAP,
        CapabilityId::LOG_EMIT,
    ] {
        assert!(REQUIRED_CAPABILITIES.contains(&cap));
    }
    for cap in [CapabilityId::IRQ_BIND, CapabilityId::IPC_ENDPOINT] {
        assert!(!REQUIRED_CAPABILITIES.contains(&cap));
    }
}
