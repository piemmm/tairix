use super::*;
use crate::vectors::{self, Suite, Vector, REGISTERS};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

/// A flat megabyte of RAM with nothing on the I/O bus, as the vectors were
/// captured: reads of an empty port float high.
pub(super) struct FlatBus {
    pub ram: Vec<u8>,
    pub writes: Vec<u32>,
}

impl FlatBus {
    pub fn new() -> Self {
        Self {
            ram: vec![0; 1 << 20],
            writes: Vec::new(),
        }
    }
}

impl Bus for FlatBus {
    fn read(&mut self, addr: u32, _at: u64) -> u8 {
        self.ram[addr as usize & 0xF_FFFF]
    }
    fn peek(&self, addr: u32) -> u8 {
        self.ram[addr as usize & 0xF_FFFF]
    }
    fn write(&mut self, addr: u32, value: u8, _at: u64) {
        self.writes.push(addr);
        self.ram[addr as usize & 0xF_FFFF] = value;
    }
    fn input(&mut self, _port: u16, _at: u64) -> u8 {
        0xFF
    }
    fn output(&mut self, _port: u16, _value: u8, _at: u64) {}
    fn intr(&self) -> bool {
        false
    }
    fn acknowledge(&mut self) -> u8 {
        0
    }
    fn nmi(&self) -> bool {
        false
    }
    fn acknowledge_nmi(&mut self) {}
    fn host_code(&self, _addr: u32) -> bool {
        false
    }
    // No access waits, so none is ever pending.
    fn steal(&mut self, _cycles: u32) {}
}

const NAMES: [&str; REGISTERS] = [
    "ax", "bx", "cx", "dx", "cs", "ss", "ds", "es", "sp", "bp", "si", "di", "ip", "flags",
];

fn load(regs: &mut Regs, v: &[u16; REGISTERS]) {
    regs.gpr[gpr::AX] = v[0];
    regs.gpr[gpr::BX] = v[1];
    regs.gpr[gpr::CX] = v[2];
    regs.gpr[gpr::DX] = v[3];
    regs.seg[Seg::Cs.index()] = v[4];
    regs.seg[Seg::Ss.index()] = v[5];
    regs.seg[Seg::Ds.index()] = v[6];
    regs.seg[Seg::Es.index()] = v[7];
    regs.gpr[gpr::SP] = v[8];
    regs.gpr[gpr::BP] = v[9];
    regs.gpr[gpr::SI] = v[10];
    regs.gpr[gpr::DI] = v[11];
    regs.ip = v[12];
    regs.flags = v[13];
}

fn dump(regs: &Regs) -> [u16; REGISTERS] {
    [
        regs.gpr[gpr::AX],
        regs.gpr[gpr::BX],
        regs.gpr[gpr::CX],
        regs.gpr[gpr::DX],
        regs.seg[Seg::Cs.index()],
        regs.seg[Seg::Ss.index()],
        regs.seg[Seg::Ds.index()],
        regs.seg[Seg::Es.index()],
        regs.gpr[gpr::SP],
        regs.gpr[gpr::BP],
        regs.gpr[gpr::SI],
        regs.gpr[gpr::DI],
        regs.ip,
        regs.flags,
    ]
}

/// Runs one vector; `Err` describes the first mismatch, with flags compared
/// under `mask`.
fn run_vector(model: CpuModel, v: &Vector, mask: u16) -> Result<u64, String> {
    let mut bus = FlatBus::new();
    for &(addr, value) in &v.ram {
        bus.ram[addr as usize & 0xF_FFFF] = value;
    }
    let mut cpu = Cpu::new(model);
    load(cpu.regs_mut(), &v.regs);
    cpu.biu.fill = u8::try_from(v.queue.len())
        .unwrap_or(0)
        .min(cpu.queue.len());
    let start = cpu.cycles();
    while cpu.step(&mut bus, u64::MAX).is_none() && cpu.prefixes.count != 0 {}
    // The hardware's trace ends once the next instruction's first byte is
    // read from the queue, so a branch's count includes refetching it.
    let refetch = cpu.await_bytes(1);
    cpu.tick(refetch);
    let got = dump(cpu.regs());
    let mut expected = v.regs;
    for (slot, value) in expected.iter_mut().zip(v.final_regs) {
        if let Some(value) = value {
            *slot = value;
        }
    }
    let mut msg = String::new();
    for i in 0..REGISTERS {
        let (g, e) = if i == 13 {
            (got[i] & mask, expected[i] & mask)
        } else {
            (got[i], expected[i])
        };
        if g != e {
            let _ = write!(
                msg,
                "{}={:#06x} want {:#06x}; ",
                NAMES[i], got[i], expected[i]
            );
        }
    }
    for &(addr, value) in &v.final_ram {
        let g = bus.ram[addr as usize & 0xF_FFFF];
        if g != value {
            let _ = write!(msg, "[{addr:#07x}]={g:#04x} want {value:#04x}; ");
        }
    }
    for &addr in &bus.writes {
        if !v.final_ram.iter().any(|&(a, _)| a == addr) {
            let before = v.ram.iter().find(|&&(a, _)| a == addr).map(|&(_, b)| b);
            if before != Some(bus.ram[addr as usize]) {
                let _ = write!(msg, "stray write [{addr:#07x}]; ");
            }
        }
    }
    if msg.is_empty() {
        Ok(cpu.cycles() - start)
    } else {
        Err(msg)
    }
}

pub(super) fn suite(bytes: &[u8]) -> Suite {
    let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut out = vec![0u8; len];
    let n =
        tairix_compress::zlib::decompress_into(&bytes[4..], &mut out).expect("vectors decompress");
    assert_eq!(n, len);
    vectors::decode(&out).expect("vectors decode")
}

pub(super) const V8088: &[u8] = include_bytes!("../../assets/cpu-vectors/8088.pcv");
pub(super) const V8086: &[u8] = include_bytes!("../../assets/cpu-vectors/8086.pcv");
pub(super) const VV20: &[u8] = include_bytes!("../../assets/cpu-vectors/v20.pcv");

/// The V20 groups whose officially undefined flags come from internal
/// multiplier and divider state no observable rule reproduces: their defined
/// flags are compared, as the upstream masks them. Every other group is
/// compared on all sixteen bits, undefined flags included.
const UNMODELLED_UNDEFINED_FLAGS: [&str; 8] =
    ["F6.5", "F7.5", "F6.6", "F6.7", "F7.6", "F7.7", "69", "6B"];

fn flags_mask(model: CpuModel, group: &str, upstream: u16) -> u16 {
    if model.family() == Family::Nec && UNMODELLED_UNDEFINED_FLAGS.contains(&group) {
        upstream
    } else {
        0xFFFF
    }
}

/// Runs every vector of a suite on `model`, reporting each failing group's
/// first mismatch.
fn conformance(model: CpuModel, bytes: &[u8]) {
    let suite = suite(bytes);
    let mut failures = String::new();
    let mut failed = 0usize;
    let mut total = 0usize;
    for group in &suite.groups {
        let mut group_failed = 0usize;
        let mut first = None;
        for v in &group.vectors {
            total += 1;
            if let Err(e) = run_vector(model, v, flags_mask(model, &group.name, group.flags_mask)) {
                group_failed += 1;
                if first.is_none() {
                    let mut state = String::new();
                    for (name, value) in NAMES.iter().zip(v.regs) {
                        let _ = write!(state, "{name}={value:04x} ");
                    }
                    first = Some((v.bytes.clone(), alloc::format!("{e}\n      from {state}")));
                }
            }
        }
        if group_failed > 0 {
            failed += group_failed;
            let (bytes, e) = first.unwrap_or_default();
            let _ = writeln!(
                failures,
                "{} [{group_failed}/{}] {:02x?}: {e}",
                group.name,
                group.vectors.len(),
                bytes
            );
        }
    }
    assert!(
        failed == 0,
        "{} on {failed} of {total} vectors:\n{failures}",
        model.name()
    );
}

#[test]
fn the_8088_matches_the_hardware() {
    conformance(CpuModel::I8088, V8088);
}

#[test]
fn the_8086_matches_the_hardware() {
    conformance(CpuModel::I8086, V8086);
}

#[test]
fn the_v20_matches_the_hardware() {
    conformance(CpuModel::V20, VV20);
}

/// Mean absolute cycle deviation from the hardware over a suite, in tenths
/// of a cycle per instruction.
fn mean_deviation(model: CpuModel, bytes: &[u8]) -> u64 {
    let suite = suite(bytes);
    let (mut total, mut count) = (0u64, 0u64);
    for group in &suite.groups {
        for v in &group.vectors {
            if let Ok(cycles) = run_vector(model, v, 0) {
                total += cycles.abs_diff(u64::from(v.cycles));
                count += 1;
            }
        }
    }
    total * 10 / count.max(1)
}

#[test]
fn timing_tracks_the_hardware() {
    // Instruction-level timing with the prefetch fill model, against bus
    // traces whose residue is T-state arbitration the model does not see.
    assert!(
        mean_deviation(CpuModel::I8086, V8086) <= 4,
        "8086 timing regressed"
    );
    assert!(
        mean_deviation(CpuModel::I8088, V8088) <= 29,
        "8088 timing regressed"
    );
    assert!(
        mean_deviation(CpuModel::V20, VV20) <= 28,
        "V20 timing regressed"
    );
}

#[test]
fn a_prefetch_queue_based_past_the_address_space_is_refused_from_a_snapshot() {
    use crate::snapshot::{In, Out, State, Torn};
    let restored = |base: u32| {
        let mut queue = queue::Queue::new(4);
        queue.set_window(base, 0x100);
        let mut out = Out::refusable();
        queue.save(&mut out);
        let bytes = out.finish().unwrap();
        queue::Queue::new(4).restore(&mut In::new(&bytes))
    };
    assert_eq!(restored(0xF_FFF0), Ok(()), "the last segment's base");
    assert_eq!(restored(0xFFFF_FFFF), Err(Torn::Malformed));
    assert_eq!(
        restored(0x1_0001),
        Err(Torn::Malformed),
        "no segment's base"
    );
}
