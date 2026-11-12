use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::snapshot::{In, Out, State};

/// A megabyte the coprocessor reaches, counting its transfers.
struct Ram {
    bytes: Vec<u8>,
    transfers: usize,
}

impl Ram {
    fn new() -> Self {
        Self {
            bytes: vec![0; 1 << 20],
            transfers: 0,
        }
    }

    fn put(&mut self, addr: u32, data: &[u8]) {
        let at = addr as usize;
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }

    fn get(&self, addr: u32, len: usize) -> &[u8] {
        let at = addr as usize;
        &self.bytes[at..at + len]
    }
}

impl Memory for Ram {
    fn read(&mut self, addr: u32) -> u8 {
        self.transfers += 1;
        self.bytes[(addr & 0xF_FFFF) as usize]
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.transfers += 1;
        self.bytes[(addr & 0xF_FFFF) as usize] = value;
    }
}

/// The ESC instruction `D8+group modrm`, at `0x1000`, its memory operand at
/// `operand` where it has one.
fn esc(group: u8, modrm: u8, operand: Option<u32>, ram: &Ram) -> Escape {
    Escape {
        opcode: (u16::from(group & 7) << 8) | u16::from(modrm),
        address: 0x1000,
        operand: operand.map(|addr| {
            let word = u16::from_le_bytes([ram.get(addr, 2)[0], ram.get(addr, 2)[1]]);
            (addr, word)
        }),
    }
}

/// Runs `group modrm` at `at` and lets it end.
fn run(fpu: &mut Fpu, ram: &mut Ram, (group, modrm): (u8, u8), operand: Option<u32>, at: u64) {
    let escape = esc(group, modrm, operand, ram);
    fpu.escape(escape, at, ram);
    fpu.complete(ram);
}

/// `FLD QWORD PTR [addr]` of `value`.
fn load_double(fpu: &mut Fpu, ram: &mut Ram, value: f64, addr: u32) {
    ram.put(addr, &value.to_bits().to_le_bytes());
    run(fpu, ram, (5, 0x06), Some(addr), 0);
}

/// `FSTP QWORD PTR [addr]`, read back as a double's bits.
fn store_double(fpu: &mut Fpu, ram: &mut Ram, addr: u32) -> u64 {
    run(fpu, ram, (5, 0x1E), Some(addr), 0);
    let mut bits = [0u8; 8];
    bits.copy_from_slice(ram.get(addr, 8));
    u64::from_le_bytes(bits)
}

#[test]
fn a_snapshot_holds_the_state_and_the_instruction_in_flight() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    load_double(&mut fpu, &mut ram, 1.5, 0x2000);
    // FSTP leaves a store in flight.
    let escape = esc(5, 0x1E, Some(0x3000), &ram);
    fpu.escape(escape, 100, &mut ram);
    assert!(fpu.busy());
    let mut out = Out::refusable();
    fpu.save(&mut out);
    let bytes = out.finish().unwrap();
    let mut twin = Fpu::new();
    let mut input = In::new(&bytes);
    twin.restore(&mut input).unwrap();
    assert!(input.is_exhausted());
    assert_eq!(twin, fpu);
    twin.complete(&mut ram);
    assert_eq!(ram.get(0x3000, 8), &1.5f64.to_bits().to_le_bytes());
}

#[test]
fn an_instruction_commits_only_when_it_ends() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    ram.put(0x2000, &2.0f64.to_bits().to_le_bytes());
    fpu.escape(esc(5, 0x06, Some(0x2000), &ram), 1_000, &mut ram);
    // FLD m64 takes 46 clocks; until then nothing shows and BUSY holds.
    assert_eq!(fpu.ends(), Some(1_046));
    assert!(fpu.busy());
    assert_eq!(fpu.ready_at(1_000), 1_046);
    assert_eq!(fpu.registers().tags, 0xFFFF);
    assert_eq!(fpu.status() & 0x8000, 0x8000, "B set while busy");
    fpu.complete(&mut ram);
    assert!(!fpu.busy());
    assert_eq!(fpu.ready_at(2_000), 2_000);
    assert_eq!(fpu.status() >> 11 & 7, 7, "TOP after the push");
    assert_eq!(store_double(&mut fpu, &mut ram, 0x3000), 2.0f64.to_bits());
}

#[test]
fn the_control_unit_acts_at_once_and_fninit_abandons_the_flight() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    ram.put(0x2000, &2.0f64.to_bits().to_le_bytes());
    fpu.escape(esc(5, 0x06, Some(0x2000), &ram), 0, &mut ram);
    // FNSTSW [4000h] while the load runs: B set, TOP still 0.
    fpu.escape(esc(5, 0x3E, Some(0x4000), &ram), 10, &mut ram);
    assert_eq!(
        u16::from_le_bytes([ram.get(0x4000, 2)[0], ram.get(0x4000, 2)[1]]),
        0x8000
    );
    assert!(fpu.busy(), "FNSTSW leaves the load in flight");
    // FNINIT: the load never lands.
    fpu.escape(esc(3, 0xE3, None, &ram), 20, &mut ram);
    assert!(!fpu.busy());
    assert_eq!(fpu.registers().tags, 0xFFFF);
    assert_eq!(fpu.registers().control, CONTROL_INIT);
}

#[test]
fn an_instruction_issued_while_busy_ends_the_last_one_early() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    ram.put(0x2000, &3.0f64.to_bits().to_le_bytes());
    fpu.escape(esc(5, 0x06, Some(0x2000), &ram), 0, &mut ram);
    // FLD ST(0) at once, with no WAIT between: both loads land.
    fpu.escape(esc(1, 0xC0, None, &ram), 5, &mut ram);
    fpu.complete(&mut ram);
    assert_eq!(fpu.status() >> 11 & 7, 6);
    assert_eq!(store_double(&mut fpu, &mut ram, 0x3000), 3.0f64.to_bits());
    assert_eq!(store_double(&mut fpu, &mut ram, 0x3000), 3.0f64.to_bits());
}

#[test]
fn an_unmasked_exception_raises_int_and_holds_busy_until_cleared() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    // FLDCW: every exception masked but zero divide, interrupts enabled.
    ram.put(0x2000, &0x037Bu16.to_le_bytes());
    run(&mut fpu, &mut ram, (1, 0x2E), Some(0x2000), 0);
    load_double(&mut fpu, &mut ram, 1.0, 0x2100);
    // FLDZ, then FDIV ST(1),ST: 1/0.
    run(&mut fpu, &mut ram, (1, 0xEE), None, 0);
    run(&mut fpu, &mut ram, (4, 0xF9), None, 0);
    assert!(fpu.interrupting());
    assert!(fpu.busy(), "BUSY held by the interrupt");
    assert_eq!(fpu.ready_at(0), u64::MAX);
    assert_eq!(fpu.status() & 0x80FF, 0x8084, "B, IR and ZE");
    // FNCLEX drops INT, BUSY and the flag.
    fpu.escape(esc(3, 0xE2, None, &ram), 0, &mut ram);
    assert!(!fpu.interrupting() && !fpu.busy());
    // FDISI: an unmasked exception then flags IR but raises no INT.
    fpu.escape(esc(3, 0xE1, None, &ram), 0, &mut ram);
    run(&mut fpu, &mut ram, (4, 0xF9), None, 0);
    assert!(!fpu.interrupting() && !fpu.busy());
    assert_eq!(fpu.status() & 0x0080, 0x0080);
}

#[test]
fn fnclex_while_busy_keeps_what_the_flight_raises() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    load_double(&mut fpu, &mut ram, 1.0, 0x2000);
    run(&mut fpu, &mut ram, (1, 0xEE), None, 0);
    // A masked zero divide in flight; an older flag cleared under it.
    fpu.escape(esc(4, 0xF9, None, &ram), 0, &mut ram);
    fpu.escape(esc(3, 0xE2, None, &ram), 1, &mut ram);
    fpu.complete(&mut ram);
    assert_eq!(fpu.status() & 0x3F, 0x04);
}

#[test]
fn fsave_stores_the_state_and_initialises_and_frstor_takes_it_back() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    load_double(&mut fpu, &mut ram, 2.5, 0x2000);
    load_double(&mut fpu, &mut ram, -0.75, 0x2000);
    let before = *fpu.registers();
    run(&mut fpu, &mut ram, (5, 0x36), Some(0x5000), 0);
    assert_eq!(fpu.registers().tags, 0xFFFF, "initialised");
    let image = ram.get(0x5000, STATE_BYTES).to_vec();
    assert_eq!(u16::from_le_bytes([image[0], image[1]]), before.control);
    // ST(0) first: -0.75.
    assert_eq!(
        &image[14..24],
        &before.data[usize::from(before.top())].to_bytes()
    );
    // The pointers: the last numeric instruction, the FLD at 1000h.
    assert_eq!(u16::from_le_bytes([image[6], image[7]]), 0x1000);
    assert_eq!(u16::from_le_bytes([image[8], image[9]]) & 0x7FF, 0x506);
    assert_eq!(u16::from_le_bytes([image[10], image[11]]), 0x2000);
    run(&mut fpu, &mut ram, (5, 0x26), Some(0x5000), 0);
    assert_eq!(*fpu.registers(), before);
    assert_eq!(
        store_double(&mut fpu, &mut ram, 0x3000),
        (-0.75f64).to_bits()
    );
}

#[test]
fn fstenv_stores_the_environment_and_masks_every_exception() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    ram.put(0x2000, &0x0360u16.to_le_bytes());
    run(&mut fpu, &mut ram, (1, 0x2E), Some(0x2000), 0);
    run(&mut fpu, &mut ram, (1, 0x36), Some(0x6000), 0);
    assert_eq!(
        u16::from_le_bytes([ram.get(0x6000, 2)[0], ram.get(0x6000, 2)[1]]),
        0x0360
    );
    assert_eq!(fpu.registers().control, 0x037F);
    // FLDENV takes the stored control word back.
    run(&mut fpu, &mut ram, (1, 0x26), Some(0x6000), 0);
    assert_eq!(fpu.registers().control, 0x0360);
}

#[test]
fn a_load_reads_its_words_after_the_dummy_read_as_bus_master() {
    let mut fpu = Fpu::new();
    let mut ram = Ram::new();
    ram.put(0x2000, &7.0f64.to_bits().to_le_bytes());
    let escape = esc(5, 0x06, Some(0x2000), &ram);
    ram.transfers = 0;
    fpu.escape(escape, 0, &mut ram);
    assert_eq!(
        ram.transfers, 6,
        "the dummy read carried two of eight bytes"
    );
}
