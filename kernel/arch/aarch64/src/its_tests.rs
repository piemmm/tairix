//! Host tests for the ITS driver, over a model that reads the command queue
//! and tables from real memory and answers what a device's message raises.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::vec;
use std::vec::Vec;

use super::*;
use crate::test_frames::HostBlocks;

/// How firmware left the modelled service.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Handoff {
    /// Disabled and quiescent.
    Quiescent,
    /// Enabled; it quiesces once disabled.
    Enabled,
    /// Enabled, and it never quiesces.
    Stuck,
}

/// How the modelled service behaves.
#[derive(Clone, Copy)]
struct Config {
    pidr2: u32,
    typer: u64,
    /// Page sizes its tables take: 4, 16 and 64 KiB.
    page_sizes: [bool; 3],
    indirect: bool,
    shares: bool,
    handoff: Handoff,
    /// The highest LPI its `MAPTI` takes before it stalls.
    lpi_ceiling: u32,
}

/// `GITS_TYPER` with physical LPIs, 12-byte ITT entries, `events` `EventID`
/// bits, `devices` `DeviceID` bits, processor-number targets and no hardware
/// collections.
const fn typer(events: u64, devices: u64) -> u64 {
    TYPER_PHYSICAL | (11 << 4) | ((events - 1) << 8) | ((devices - 1) << 13)
}

const QEMU: Config = Config {
    pidr2: 0x3B,
    typer: typer(16, 16),
    page_sizes: [true; 3],
    indirect: true,
    shares: true,
    handoff: Handoff::Quiescent,
    lpi_ceiling: u32::MAX,
};

#[derive(Default)]
struct State {
    ctlr: u32,
    cbaser: u64,
    cwriter: u64,
    creadr: u64,
    basers: [u64; 8],
    devices: BTreeMap<u32, (u64, u64)>,
    collections: BTreeMap<u64, u64>,
    events: BTreeMap<(u32, u32), (u32, u64)>,
    syncs: Vec<u64>,
    commands: usize,
}

struct Model<'m> {
    config: Config,
    memory: &'m HostBlocks,
    state: Mutex<State>,
}

/// The device table at index 0, the collection table at index 1.
const DEVICE_BASER: u64 = (TYPE_DEVICES << BASER_TYPE_SHIFT) | (7 << BASER_ENTRY_SHIFT);
const COLLECTION_BASER: u64 = (TYPE_COLLECTIONS << BASER_TYPE_SHIFT) | (7 << BASER_ENTRY_SHIFT);
const RO: u64 = (0b111 << BASER_TYPE_SHIFT) | (0x1F << BASER_ENTRY_SHIFT);

impl<'m> Model<'m> {
    fn new(config: Config, memory: &'m HostBlocks) -> Self {
        let mut state = State {
            ctlr: if config.handoff == Handoff::Quiescent {
                CTLR_QUIESCENT
            } else {
                CTLR_ENABLED
            },
            ..State::default()
        };
        state.basers[0] = DEVICE_BASER | page_size_code(4);
        state.basers[1] = COLLECTION_BASER | page_size_code(4);
        Self {
            config,
            memory,
            state: Mutex::new(state),
        }
    }

    /// What `device`'s message carrying `event` raises, and where.
    fn raise(&self, device: u32, event: u32) -> Option<(u32, u64)> {
        let state = self.state.lock().unwrap();
        if state.ctlr & CTLR_ENABLED == 0 {
            return None;
        }
        let &(_, bits) = state.devices.get(&device)?;
        if u64::from(event) >= 1 << bits {
            return None;
        }
        let &(lpi, icid) = state.events.get(&(device, event))?;
        Some((lpi, *state.collections.get(&icid)?))
    }

    /// Entries the table at `baser` holds, reading a two-level table's
    /// first level from memory to see whether `id`'s leaf is there.
    fn holds(&self, baser: u64, id: u64) -> bool {
        if baser & BASER_VALID == 0 {
            return false;
        }
        let page = 4096u64
            << match (baser & BASER_PAGE_SIZE) >> BASER_PAGE_SIZE_SHIFT {
                0 => 0,
                1 => 2,
                _ => 4,
            };
        let entry = ((baser >> BASER_ENTRY_SHIFT) & 0x1F) + 1;
        let pages = (baser & 0xFF) + 1;
        let phys = baser & BASER_PHYS;
        if baser & BASER_INDIRECT == 0 {
            return id < pages * page / entry;
        }
        let leaf = id / (page / entry);
        leaf < pages * page / 8 && self.memory.read(phys + 8 * leaf) & BASER_VALID != 0
    }

    fn run_commands(&self, state: &mut State) {
        let queue = state.cbaser & CBASER_PHYS;
        let bytes = ((state.cbaser & 0xFF) + 1) * 4096;
        while state.creadr != state.cwriter {
            let at = queue + state.creadr;
            let w: Vec<u64> = (0..4).map(|n| self.memory.read(at + 8 * n)).collect();
            if !self.command(state, &w) {
                state.creadr |= CREADR_STALLED;
                return;
            }
            state.commands += 1;
            state.creadr = (state.creadr + 32) % bytes;
        }
    }

    /// Apply one command, answering whether the service accepted it.
    fn command(&self, state: &mut State, w: &[u64]) -> bool {
        let device = u32::try_from(w[0] >> 32).unwrap();
        match w[0] & 0xFF {
            CMD_MAPD => {
                if !self.holds(state.basers[0], u64::from(device)) || w[2] & BASER_VALID == 0 {
                    return false;
                }
                state
                    .devices
                    .insert(device, (w[2] & 0x000F_FFFF_FFFF_FF00, (w[1] & 0x1F) + 1));
                true
            }
            CMD_MAPC => {
                let icid = w[2] & 0xFFFF;
                let hardware = (self.config.typer >> 24) & 0xFF;
                if icid >= hardware && !self.holds(state.basers[1], icid) {
                    return false;
                }
                state.collections.insert(icid, w[2] & 0x000F_FFFF_FFFF_0000);
                true
            }
            CMD_MAPTI => {
                let event = u32::try_from(w[1] & 0xFFFF_FFFF).unwrap();
                let lpi = u32::try_from(w[1] >> 32).unwrap();
                let icid = w[2] & 0xFFFF;
                let Some(&(_, bits)) = state.devices.get(&device) else {
                    return false;
                };
                if u64::from(event) >= 1 << bits
                    || lpi < FIRST_LPI
                    || lpi > self.config.lpi_ceiling
                    || !state.collections.contains_key(&icid)
                {
                    return false;
                }
                state.events.insert((device, event), (lpi, icid));
                true
            }
            CMD_SYNC => {
                state.syncs.push(w[2]);
                true
            }
            _ => false,
        }
    }
}

impl ItsMmio for &Model<'_> {
    fn read(&self, off: usize) -> u32 {
        let state = self.state.lock().unwrap();
        match off {
            GITS_CTLR => state.ctlr,
            PIDR2 => self.config.pidr2,
            _ => 0,
        }
    }
    fn read_u64(&self, off: usize) -> u64 {
        let state = self.state.lock().unwrap();
        match off {
            GITS_TYPER => self.config.typer,
            GITS_CBASER => state.cbaser,
            GITS_CWRITER => state.cwriter,
            GITS_CREADR => state.creadr,
            o if (GITS_BASER..GITS_BASER + 64).contains(&o) => state.basers[(o - GITS_BASER) / 8],
            _ => 0,
        }
    }
    fn write(&self, off: usize, value: u32) {
        let mut state = self.state.lock().unwrap();
        if off == GITS_CTLR {
            let enabling = value & CTLR_ENABLED != 0;
            assert!(
                !enabling || state.ctlr & CTLR_QUIESCENT != 0 || state.ctlr & CTLR_ENABLED != 0,
                "enabled before it quiesced"
            );
            state.ctlr = if enabling {
                CTLR_ENABLED
            } else if self.config.handoff != Handoff::Stuck {
                CTLR_QUIESCENT
            } else {
                0
            };
        }
    }
    fn write_u64(&self, off: usize, value: u64) {
        let mut state = self.state.lock().unwrap();
        let shares = |value: u64| {
            if self.config.shares {
                value
            } else {
                value & !TABLE_SHAREABILITY
            }
        };
        match off {
            GITS_CBASER => {
                assert_eq!(state.ctlr & CTLR_ENABLED, 0, "CBASER written while enabled");
                state.cbaser = shares(value);
                state.creadr = 0;
            }
            GITS_CWRITER => {
                state.cwriter = value & QUEUE_OFFSET;
                if state.ctlr & CTLR_ENABLED != 0 {
                    self.run_commands(&mut state);
                }
            }
            o if (GITS_BASER..GITS_BASER + 64).contains(&o) => {
                assert_eq!(state.ctlr & CTLR_ENABLED, 0, "BASER written while enabled");
                let n = (o - GITS_BASER) / 8;
                let old = state.basers[n];
                let mut stored = (value & !RO) | (old & RO);
                let code = (value & BASER_PAGE_SIZE) >> BASER_PAGE_SIZE_SHIFT;
                if !self.config.page_sizes[usize::try_from(code.min(2)).unwrap()] {
                    stored = (stored & !BASER_PAGE_SIZE) | (old & BASER_PAGE_SIZE);
                }
                if !self.config.indirect {
                    stored &= !BASER_INDIRECT;
                }
                state.basers[n] = shares(stored);
            }
            _ => {}
        }
    }
}

/// A taken-over service: its model and its memory.
fn taken_over<'m>(model: &'m Model<'m>, frames: &'m HostBlocks) -> ItsUnit<'m, &'m Model<'m>> {
    Its::new(model).take_over(frames, 1).expect("takes over")
}

const TARGET: u64 = 5 << 16;

fn route(device: u32, event: u32, lpi: u32) -> ItsRoute {
    ItsRoute { device, event, lpi }
}

#[test]
fn a_device_raises_only_the_lpis_mapped_for_its_own_events() {
    let frames = HostBlocks::default();
    let model = Model::new(QEMU, &frames);
    let mut its = taken_over(&model, &frames);
    its.map(
        0,
        TARGET,
        14,
        &mut [
            route(0x10, 1, 8194),
            route(0x08, 0, 8192),
            route(0x10, 0, 8193),
        ],
    )
    .expect("maps");
    assert_eq!(model.raise(0x08, 0), Some((8192, TARGET)));
    assert_eq!(model.raise(0x10, 0), Some((8193, TARGET)));
    assert_eq!(model.raise(0x10, 1), Some((8194, TARGET)));
    assert_eq!(
        model.raise(0x08, 1),
        None,
        "an event the device was not given"
    );
    assert_eq!(model.raise(0x20, 0), None, "a device given nothing");
    assert_eq!(
        model.state.lock().unwrap().syncs,
        [TARGET],
        "synced last, once"
    );
}

#[test]
fn a_two_level_device_table_draws_the_leaf_a_device_s_entry_lives_in() {
    let frames = HostBlocks::default();
    let model = Model::new(QEMU, &frames);
    let mut its = taken_over(&model, &frames);
    let baser = model.read_u64_now(GITS_BASER);
    assert_ne!(baser & BASER_INDIRECT, 0, "65536 DeviceIDs need two levels");
    assert_eq!(
        (baser & BASER_PAGE_SIZE) >> BASER_PAGE_SIZE_SHIFT,
        0,
        "4 KiB pages"
    );
    let before = frames.live();
    its.map(0, TARGET, 14, &mut [route(0xBEEF, 0, 8192)])
        .expect("maps");
    assert_eq!(model.raise(0xBEEF, 0), Some((8192, TARGET)));
    assert_eq!(frames.live(), before + 2, "a leaf and an ITT frame");
}

#[test]
fn without_two_levels_the_device_table_is_flat_and_whole() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            indirect: false,
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    let baser = model.read_u64_now(GITS_BASER);
    assert_eq!(baser & BASER_INDIRECT, 0);
    assert_eq!(
        (baser & 0xFF) + 1,
        128,
        "65536 eight-byte entries in 4 KiB pages"
    );
    its.map(0, TARGET, 14, &mut [route(0xFFFF, 0, 8192)])
        .expect("the last DeviceID maps");
    assert_eq!(model.raise(0xFFFF, 0), Some((8192, TARGET)));
}

#[test]
fn a_table_is_given_the_smallest_page_its_service_takes() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            page_sizes: [false, false, true],
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    let baser = model.read_u64_now(GITS_BASER);
    assert_eq!(
        (baser & BASER_PAGE_SIZE) >> BASER_PAGE_SIZE_SHIFT,
        0b10,
        "64 KiB pages"
    );
    assert_eq!((baser & BASER_PHYS) % 0x1_0000, 0, "aligned to its page");
    its.map(0, TARGET, 14, &mut [route(0x0101, 0, 8192)])
        .expect("maps");
    assert_eq!(model.raise(0x0101, 0), Some((8192, TARGET)));
}

#[test]
fn a_service_that_shares_nothing_reads_its_queue_uncached_and_cleaned() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            shares: false,
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    let cbaser = model.read_u64_now(GITS_CBASER);
    assert_eq!(cbaser & TABLE_SHAREABILITY, 0);
    assert_eq!(
        cbaser & (0b111 << 59),
        INNER_NON_CACHEABLE,
        "the queue is read uncached"
    );
    let _ = crate::paging::take_recorded_poc_sweeps();
    its.map(0, TARGET, 14, &mut [route(0x08, 0, 8192)])
        .expect("maps");
    let queue = frames.block_at(cbaser & CBASER_PHYS, QUEUE_ORDER).unwrap() as u64;
    assert!(
        crate::paging::take_recorded_poc_sweeps()
            .iter()
            .any(|&(base, len)| base == queue && len == 4 * COMMAND_BYTES),
        "each command reaches the point of coherency before the service is told of it"
    );
    assert_eq!(model.raise(0x08, 0), Some((8192, TARGET)));
}

#[test]
fn a_service_firmware_left_on_is_quiesced_before_it_is_repointed() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            handoff: Handoff::Enabled,
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    its.map(0, TARGET, 14, &mut [route(0x08, 0, 8192)])
        .expect("maps");
    assert_eq!(model.raise(0x08, 0), Some((8192, TARGET)));

    let stuck = HostBlocks::default();
    let model = Model::new(
        Config {
            handoff: Handoff::Stuck,
            ..QEMU
        },
        &stuck,
    );
    assert_eq!(
        Its::new(&model).take_over(&stuck, 1).err(),
        Some(ItsError::Unresponsive)
    );
    assert_eq!(
        stuck.live(),
        0,
        "nothing is drawn for a service that never stops"
    );
}

#[test]
fn only_an_its_translating_to_physical_lpis_is_taken() {
    let frames = HostBlocks::default();
    for config in [
        Config {
            pidr2: 0x2B,
            ..QEMU
        },
        Config {
            typer: QEMU.typer & !TYPER_PHYSICAL,
            ..QEMU
        },
    ] {
        let model = Model::new(config, &frames);
        assert_eq!(
            Its::new(&model).take_over(&frames, 1).err(),
            Some(ItsError::NotAnIts)
        );
    }
    assert_eq!(frames.live(), 0);
}

#[test]
fn a_service_holding_its_collections_itself_is_given_no_collection_table() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            typer: QEMU.typer | (1 << 24),
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    assert_eq!(model.read_u64_now(GITS_BASER + 8) & BASER_VALID, 0);
    its.map(0, TARGET, 14, &mut [route(0x08, 0, 8192)])
        .expect("maps");
    assert_eq!(model.raise(0x08, 0), Some((8192, TARGET)));
}

#[test]
fn a_route_the_service_cannot_name_maps_nothing() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            typer: typer(4, 8),
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    for mut routes in [
        vec![route(0x08, 0, 8191)],
        vec![route(0x08, 0, 1 << 14)],
        vec![route(0x08, 16, 8192)],
        vec![route(0x100, 0, 8192)],
        vec![route(0x08, 0, 8192), route(0x08, 0, 8193)],
    ] {
        assert_eq!(
            its.map(0, TARGET, 14, &mut routes),
            Err(ItsError::OutOfRange),
            "{routes:?}"
        );
    }
    assert_eq!(
        its.map(1, TARGET, 14, &mut [route(0x08, 0, 8192)]),
        Err(ItsError::OutOfRange),
        "a collection the service holds no entry for"
    );
    assert_eq!(
        model.state.lock().unwrap().commands,
        0,
        "nothing reached the service"
    );
    its.map(0, TARGET, 14, &mut [route(0x08, 0, 8192)])
        .expect("maps");
    assert_eq!(
        its.map(0, TARGET, 14, &mut [route(0x09, 0, 8193)]),
        Err(ItsError::Mapped),
        "a service's devices are mapped once, together"
    );
}

#[test]
fn a_command_the_service_refuses_stalls_the_mapping_and_says_so() {
    let frames = HostBlocks::default();
    let model = Model::new(
        Config {
            lpi_ceiling: 8192,
            ..QEMU
        },
        &frames,
    );
    let mut its = taken_over(&model, &frames);
    assert_eq!(
        its.map(0, TARGET, 14, &mut [route(0x08, 0, 8193)]),
        Err(ItsError::Stalled)
    );
}

#[test]
fn more_commands_than_the_queue_holds_go_in_turns_around_its_end() {
    let frames = HostBlocks::default();
    let model = Model::new(QEMU, &frames);
    let mut its = taken_over(&model, &frames);
    let mut routes: Vec<ItsRoute> = (0..3000)
        .map(|event| route(0x08, event, 8192 + event))
        .collect();
    its.map(0, TARGET, 14, &mut routes).expect("maps");
    let state = model.state.lock().unwrap();
    assert_eq!(state.commands, 3003, "every command, in two turns");
    assert_eq!(
        state.creadr,
        (3003 * 32) % (4096 << QUEUE_ORDER),
        "past the queue's end"
    );
    drop(state);
    assert!((0..3000).all(|event| model.raise(0x08, event) == Some((8192 + event, TARGET))));
}

impl Model<'_> {
    fn read_u64_now(&self, off: usize) -> u64 {
        (&self).read_u64(off)
    }
}
