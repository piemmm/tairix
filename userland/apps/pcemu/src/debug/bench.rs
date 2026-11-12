//! The debug module's test bench: a megabyte of RAM with a probe on it, as
//! the board carries one — told of every data and port cycle, asked at every
//! boundary — and a processor set to run code in it.

use alloc::vec;
use alloc::vec::Vec;

use super::probe::{Cycle, Go, Master, Probe};
use crate::bits::{hi, lo};
use crate::cpu::{gpr, linear, Bus, Cause, Cpu, CpuModel, Seg, OPEN_BUS};

pub(super) const CODE: u16 = 0x0050;
pub(super) const STACK: u16 = 0x0900;

pub(super) struct Bench {
    pub ram: Vec<u8>,
    pub probe: Probe,
    pub intr: bool,
    pub vector: u8,
    /// The emulator's own ROM.
    pub rom: core::ops::Range<u32>,
}

impl Bench {
    /// A bench holding nothing but zeros.
    pub fn new(model: CpuModel) -> Self {
        Self {
            ram: vec![0; 1 << 20],
            probe: Probe::new(model),
            intr: false,
            vector: 0,
            rom: 0..0,
        }
    }

    pub fn load(&mut self, segment: u16, offset: u16, bytes: &[u8]) {
        for (n, &byte) in (0u16..).zip(bytes) {
            self.ram[linear(segment, offset.wrapping_add(n)) as usize] = byte;
        }
    }

    pub fn vector_to(&mut self, vector: u8, segment: u16, offset: u16) {
        let at = usize::from(vector) * 4;
        self.ram[at..at + 4].copy_from_slice(&[lo(offset), hi(offset), lo(segment), hi(segment)]);
    }

    /// Sends `cpu` on as `go` says, as the board does.
    pub fn go(&mut self, go: Go, cpu: &Cpu) {
        let mut probe = core::mem::replace(&mut self.probe, Probe::new(cpu.model()));
        probe.go(go, cpu, self);
        self.probe = probe;
    }
}

/// `model`'s processor about to run `code` at `CODE:0000`, its stack at
/// `STACK:0100`.
pub(super) fn machine(model: CpuModel, code: &[u8]) -> (Cpu, Bench) {
    let mut bench = Bench::new(model);
    bench.load(CODE, 0, code);
    let mut cpu = Cpu::new(model);
    let regs = cpu.regs_mut();
    regs.set_sreg(Seg::Cs, CODE);
    regs.ip = 0;
    regs.set_sreg(Seg::Ss, STACK);
    regs.set_sreg(Seg::Ds, CODE);
    regs.set_sreg(Seg::Es, CODE);
    regs.gpr[gpr::SP] = 0x0100;
    (cpu, bench)
}

impl Bus for Bench {
    fn read(&mut self, addr: u32, _at: u64) -> u8 {
        let value = self.ram[addr as usize];
        self.probe.memory(addr, value, Cycle::Read, Master::Cpu);
        value
    }
    fn write(&mut self, addr: u32, value: u8, _at: u64) {
        self.ram[addr as usize] = value;
        self.probe.memory(addr, value, Cycle::Write, Master::Cpu);
    }
    fn fetch(&mut self, addr: u32, _at: u64) -> u8 {
        self.ram[addr as usize]
    }
    fn peek(&self, addr: u32) -> u8 {
        self.ram[addr as usize]
    }
    fn input(&mut self, port: u16, _at: u64) -> u8 {
        self.probe.port(port, OPEN_BUS, Cycle::In, Master::Cpu);
        OPEN_BUS
    }
    fn output(&mut self, port: u16, value: u8, _at: u64) {
        self.probe.port(port, value, Cycle::Out, Master::Cpu);
    }
    fn intr(&self) -> bool {
        self.intr
    }
    fn acknowledge(&mut self) -> u8 {
        self.intr = false;
        self.vector
    }
    fn nmi(&self) -> bool {
        false
    }
    fn acknowledge_nmi(&mut self) {}
    fn host_code(&self, addr: u32) -> bool {
        self.rom.contains(&addr)
    }
    fn steal(&mut self, _cycles: u32) {}
    fn begins(&mut self, cpu: &Cpu) -> bool {
        let mut probe = core::mem::replace(&mut self.probe, Probe::new(cpu.model()));
        let stops = probe.begins(cpu, self);
        self.probe = probe;
        stops
    }
    fn interrupts(&mut self, vector: u8, cpu: &Cpu, cause: Cause) {
        self.probe.interrupt(vector, cpu, cause);
    }
    fn watches_instructions(&self) -> bool {
        self.probe.watches_instructions()
    }
}
