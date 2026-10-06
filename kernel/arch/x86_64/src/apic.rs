//! LAPIC and IO-APIC drivers.
//!
//! The local APIC is reached through its 4 KiB MMIO window in xAPIC mode and
//! through MSRs in x2APIC mode (Intel SDM Vol 3A §11.12); the IO-APIC through
//! its index/data window. Both sit behind traits — [`LapicMmio`] and
//! [`IoApicMmio`] — so the control flow is exercised by host unit tests
//! against in-memory mocks; [`LocalApic`] and [`VolatileIoApicMmio`] are the
//! bare-metal implementations.
//!
//! x2APIC mode is entered only alongside interrupt remapping in extended
//! mode, which alone can name its 32-bit destinations, or where firmware
//! handed over in it; Linux makes the same choice, x2APIC without remapping
//! being architecturally unsupported on bare metal.
//!
//! References:
//! * Intel SDM Volume 3A, Chapter 11 ("Advanced Programmable
//!   Interrupt Controller (APIC)").
//! * Intel 82093AA I/O Advanced Programmable Interrupt Controller
//!   data sheet (the "IO-APIC" reference).

/// LAPIC register offsets in the xAPIC MMIO window (Intel SDM Vol. 3A
/// §11.4.1, Table 11-1); x2APIC mode reaches each as MSR `0x800 + offset /
/// 16` (§11.12.1.2).
pub mod lapic_reg {
    /// Local APIC ID.
    pub const ID: u16 = 0x020;
    /// Local APIC version.
    pub const VERSION: u16 = 0x030;
    /// Task priority.
    pub const TPR: u16 = 0x080;
    /// End of interrupt.
    pub const EOI: u16 = 0x0B0;
    /// Spurious interrupt vector.
    pub const SPURIOUS: u16 = 0x0F0;
    /// Interrupt command, low half: writing it sends the command.
    pub const ICR_LOW: u16 = 0x300;
    /// Interrupt command, high half: the destination, in xAPIC mode only.
    pub const ICR_HIGH: u16 = 0x310;
    /// Timer local vector table entry.
    pub const TIMER_LVT: u16 = 0x320;
    /// Timer initial count.
    pub const TIMER_INITIAL_COUNT: u16 = 0x380;
    /// Timer current count.
    pub const TIMER_CURRENT_COUNT: u16 = 0x390;
    /// Timer divide configuration.
    pub const TIMER_DIVIDE_CONFIG: u16 = 0x3E0;
}

/// LAPIC base MMIO address: the architecturally-fixed value after reset,
/// and the prefix of every message address a device writes an interrupt to.
pub const LAPIC_BASE_PHYS: u64 = 0xFEE0_0000;

/// The LAPIC register block as the CPU reaches it in xAPIC mode: through the
/// direct physical map, which every translation root carries, since the
/// interrupt paths run under whichever root the interrupted task loaded.
pub const LAPIC_BASE_VIRT: u64 = crate::paging::physmap_virt(LAPIC_BASE_PHYS);

/// LAPIC Spurious Interrupt Vector Register bits (SDM §11.9).
pub mod spurious {
    /// Software-enable bit. Must be set for the LAPIC to deliver any
    /// interrupt; cleared by INIT.
    pub const ENABLE: u32 = 1 << 8;
}

/// `IA32_APIC_BASE` and the x2APIC interrupt command register (SDM
/// §11.12.1).
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
mod x2apic_msr {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    pub const APIC_BASE: u32 = 0x1B;
    pub const BASE_ENABLE: u64 = 1 << 11;
    pub const BASE_EXTENDED: u64 = 1 << 10;
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    pub const ICR: u32 = super::x2apic_register(super::lapic_reg::ICR_LOW);
}

/// The `IA32_APIC_BASE` values that take an APIC whose base reads `base` into
/// x2APIC mode, written in order: a disabled APIC is enabled first, as the
/// architecture forbids going from disabled to x2APIC in one write (SDM
/// §11.12.5); one already there needs none.
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
fn x2apic_transition(base: u64) -> impl Iterator<Item = u64> {
    let enabled = base | x2apic_msr::BASE_ENABLE;
    let extended = enabled | x2apic_msr::BASE_EXTENDED;
    let steps = match (
        base & x2apic_msr::BASE_EXTENDED != 0,
        base & x2apic_msr::BASE_ENABLE != 0,
    ) {
        (true, _) => [None, None],
        (false, true) => [Some(extended), None],
        (false, false) => [Some(enabled), Some(extended)],
    };
    steps.into_iter().flatten()
}

/// The x2APIC MSR that reaches the xAPIC register at `offset`.
#[must_use]
pub const fn x2apic_register(offset: u16) -> u32 {
    0x800 + (offset >> 4) as u32
}

/// Delivery modes for `ICR_LOW.delivery_mode` (SDM §11.6.1, Table 11-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Standard fixed-vector delivery.
    Fixed = 0b000,
    /// SMI — `vector` must be zero.
    Smi = 0b010,
    /// NMI — `vector` is ignored.
    Nmi = 0b100,
    /// INIT IPI; clears APIC state on target.
    Init = 0b101,
    /// Start-Up IPI; vector holds physical frame `vector * 0x1000`.
    StartUp = 0b110,
}

/// The xAPIC physical-mode broadcast id, which names every APIC: an APIC id
/// xAPIC addresses lies below it.
pub const XAPIC_BROADCAST: u8 = u8::MAX;

/// `ICR_HIGH` naming APIC `destination` in xAPIC's physical mode, which holds
/// eight bits and reads [`XAPIC_BROADCAST`] as every APIC: [`None`] for an id
/// it cannot name, which no command is sent to rather than reach another, or
/// all.
#[must_use]
pub const fn xapic_destination(destination: u32) -> Option<u32> {
    if destination < XAPIC_BROADCAST as u32 {
        Some(destination << 24)
    } else {
        None
    }
}

/// The local APIC's registers.
///
/// The production implementation is [`LocalApic`]; tests use an in-memory
/// mock (the `#[cfg(test)]` `tests_support` module). The provided methods are
/// the xAPIC encodings, which an x2APIC implementation overrides.
pub trait LapicMmio {
    /// Read the 32-bit register at xAPIC offset `offset`, one of
    /// [`lapic_reg`]'s.
    fn read(&self, offset: u16) -> u32;

    /// Write `value` to the 32-bit register at xAPIC offset `offset`, one of
    /// [`lapic_reg`]'s.
    fn write(&mut self, offset: u16, value: u32);

    /// The local APIC's id.
    fn apic_id(&self) -> u32 {
        self.read(lapic_reg::ID) >> 24
    }

    /// Send the interrupt command whose low half is `low` to the APIC with
    /// id `destination`, where xAPIC can name it.
    fn send_command(&mut self, destination: u32, low: u32) {
        if let Some(high) = xapic_destination(destination) {
            self.write(lapic_reg::ICR_HIGH, high);
            self.write(lapic_reg::ICR_LOW, low);
        }
    }

    /// [`Self::send_command`] to each of `destinations`, in order.
    fn send_commands(&mut self, destinations: impl IntoIterator<Item = u32>, low: u32)
    where
        Self: Sized,
    {
        for destination in destinations {
            self.send_command(destination, low);
        }
    }
}

/// LAPIC driver (per-CPU). Holds a handle to the register accessor only;
/// no other state — the LAPIC itself is the source of truth.
#[derive(Debug)]
pub struct Lapic<M: LapicMmio> {
    mmio: M,
}

impl<M: LapicMmio> Lapic<M> {
    /// Construct over a register accessor.
    pub const fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// The CPU's local APIC id: the MADT's `apic_id` in xAPIC mode, its
    /// 32-bit x2APIC id in x2APIC mode.
    pub fn id(&self) -> u32 {
        self.mmio.apic_id()
    }

    /// LAPIC version (bottom byte of the VERSION register).
    pub fn version(&self) -> u8 {
        self.mmio.read(lapic_reg::VERSION).to_le_bytes()[0]
    }

    /// Software-enable the LAPIC and program its spurious-interrupt vector:
    /// the first write after the firmware hand-off, before any interrupt is
    /// unmasked (SDM §11.9). Every interrupt is sent in physical destination
    /// mode, so the logical destination registers are left as reset made
    /// them.
    pub fn software_enable(&mut self, spurious_vector: u8) {
        // TPR = 0: accept every priority.
        self.mmio.write(lapic_reg::TPR, 0);
        let svr = spurious::ENABLE | u32::from(spurious_vector);
        self.mmio.write(lapic_reg::SPURIOUS, svr);
    }

    /// End-of-interrupt: every interrupt handler must call this exactly
    /// once before `iretq`. Writing any value clears the in-service bit.
    pub fn eoi(&mut self) {
        self.mmio.write(lapic_reg::EOI, 0);
    }

    /// Send an IPI to `target_apic_id` with the requested delivery mode
    /// and vector.
    pub fn send_ipi(&mut self, target_apic_id: u32, mode: DeliveryMode, vector: u8) {
        self.send_ipis([target_apic_id], mode, vector);
    }

    /// [`Self::send_ipi`] to each of `targets`: one batch, which the local
    /// APIC orders after the sender's stores once rather than per target.
    pub fn send_ipis(
        &mut self,
        targets: impl IntoIterator<Item = u32>,
        mode: DeliveryMode,
        vector: u8,
    ) {
        let low = u32::from(vector)
            | ((mode as u32) << 8)
            // 1 << 14 = Level Assert (required for non-INIT IPIs;
            // harmless for INIT Assert).
            | (1 << 14);
        self.mmio.send_commands(targets, low);
    }

    /// Issue the INIT-deassert IPI used between INIT and SIPI during
    /// AP bring-up. Caller is responsible for the spec-mandated delays.
    pub fn send_init_deassert(&mut self, target_apic_id: u32) {
        // Mode = INIT, Level = de-assert (bit 14 = 0), Trigger = level
        // (bit 15 = 1).
        let low = ((DeliveryMode::Init as u32) << 8) | (1 << 15);
        self.mmio.send_command(target_apic_id, low);
    }

    /// Borrow the underlying register accessor; used by `apic_timer`.
    pub fn mmio_mut(&mut self) -> &mut M {
        &mut self.mmio
    }
}

// --- Mode ------------------------------------------------------------

/// Published once every local APIC runs in x2APIC mode: by the boot CPU
/// before any other CPU starts, and never withdrawn, since leaving x2APIC
/// mode means disabling the APIC.
static X2APIC: tairix_sync::Once<()> = tairix_sync::Once::new();

/// Whether the local APICs run in x2APIC mode.
#[must_use]
pub fn x2apic() -> bool {
    matches!(X2APIC.get(), Ok(Some(())))
}

/// Whether the kernel may put local APICs firmware left in xAPIC mode into
/// x2APIC mode: where the CPU has it, firmware does not ask the OS to keep
/// away from it, and interrupt remapping in extended mode can name its
/// destinations. An APIC firmware left in x2APIC mode stays there, since
/// leaving it means disabling the APIC.
#[must_use]
pub const fn may_enter_x2apic(capable: bool, opted_out: bool, extended_remapping: bool) -> bool {
    capable && !opted_out && extended_remapping
}

/// Whether the CPU implements x2APIC mode (`CPUID.01H:ECX[21]`).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub fn x2apic_capable() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 21) != 0
}

/// Whether the calling CPU's APIC is in x2APIC mode already: firmware left
/// it there.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub fn in_x2apic() -> bool {
    // SAFETY: `IA32_APIC_BASE` exists on every CPU with a local APIC, every
    // x86_64 CPU, and the kernel runs at ring 0.
    let base = unsafe { crate::msr::read(x2apic_msr::APIC_BASE) };
    base & x2apic_msr::BASE_EXTENDED != 0
}

/// Put the calling CPU's local APIC in x2APIC mode, and every later register
/// access with it. Its state carries across (SDM §11.12.5).
///
/// # Safety
///
/// Ring 0, interrupts masked, on a CPU [`x2apic_capable`] says has it, with
/// no other CPU reaching its APIC in xAPIC mode from here on: the boot CPU
/// before any other starts, or each other CPU as it starts.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn enter_x2apic() {
    // SAFETY: the caller's contract; each value `x2apic_transition` gives
    // is one valid step of the architecture's state machine from the last.
    unsafe {
        let base = crate::msr::read(x2apic_msr::APIC_BASE);
        for value in x2apic_transition(base) {
            crate::msr::write(x2apic_msr::APIC_BASE, value);
        }
    }
    // Each later CPU finds it published already.
    let _ = X2APIC.call_once_infallible(|| ());
}

// --- The running CPU's local APIC ------------------------------------

/// Whether the register at xAPIC offset `offset` is one the kernel drives
/// that takes `access` in the mode the APICs run in: x2APIC's rules, the
/// stricter, in both, and no interrupt-command half in x2APIC mode, where the
/// command is one register `local_command` writes whole.
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
fn reaches(offset: u16, write: bool) -> bool {
    match offset {
        lapic_reg::ID | lapic_reg::VERSION | lapic_reg::TIMER_CURRENT_COUNT => !write,
        lapic_reg::EOI => write,
        lapic_reg::TPR
        | lapic_reg::SPURIOUS
        | lapic_reg::TIMER_LVT
        | lapic_reg::TIMER_INITIAL_COUNT
        | lapic_reg::TIMER_DIVIDE_CONFIG => true,
        lapic_reg::ICR_LOW | lapic_reg::ICR_HIGH => write && !x2apic(),
        _ => false,
    }
}

/// The bits a write to the register at `offset` may set (Intel SDM vol. 3
/// §11.12.1): in x2APIC mode a write setting any other `#GP`s.
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
const fn writable_bits(offset: u16) -> u32 {
    match offset {
        lapic_reg::TPR => 0xFF,
        // The vector and the software enable.
        lapic_reg::SPURIOUS => 0x1FF,
        // The vector, the mask and the timer mode.
        lapic_reg::TIMER_LVT => 0x7_00FF,
        lapic_reg::TIMER_INITIAL_COUNT | lapic_reg::ICR_LOW | lapic_reg::ICR_HIGH => u32::MAX,
        lapic_reg::TIMER_DIVIDE_CONFIG => 0b1011,
        _ => 0,
    }
}

/// Whether `value` may be written to the register at `offset`.
#[cfg(any(all(target_arch = "x86_64", target_os = "none"), test))]
fn admits(offset: u16, value: u32) -> bool {
    reaches(offset, true) && value & !writable_bits(offset) == 0
}

/// Read the calling CPU's local APIC register at xAPIC offset `offset`, one
/// of [`lapic_reg`]'s that can be read; any other reads zero, reaching
/// nothing.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
#[inline]
pub fn local_read(offset: u16) -> u32 {
    if !reaches(offset, false) {
        return 0;
    }
    if x2apic() {
        // SAFETY: `reaches` admits only registers x2APIC mode implements as
        // readable MSRs, and the kernel runs at ring 0.
        let value = unsafe { crate::msr::read(x2apic_register(offset)) };
        // A 32-bit register: its high half reads zero.
        crate::msr::halves(value).0
    } else {
        // SAFETY: the register block is reachable through the direct map
        // under every root, `reaches` admits only aligned registers inside
        // it, and the read has no side effect.
        unsafe { core::ptr::read_volatile((LAPIC_BASE_VIRT + u64::from(offset)) as *const u32) }
    }
}

/// Write `value` to the calling CPU's local APIC register at xAPIC offset
/// `offset`, one of [`lapic_reg`]'s that can be written; a write to any
/// other, or setting a bit the register does not take, reaches nothing.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline]
pub fn local_write(offset: u16, value: u32) {
    if !admits(offset, value) {
        return;
    }
    if x2apic() {
        // SAFETY: `admits` takes only registers x2APIC mode implements as
        // writable MSRs, and only values setting none of their reserved
        // bits; the kernel runs at ring 0.
        unsafe { crate::msr::write(x2apic_register(offset), u64::from(value)) }
    } else {
        // SAFETY: as for `local_read`, a register `reaches` admits as
        // writable; its effect is the caller's to want.
        unsafe {
            core::ptr::write_volatile((LAPIC_BASE_VIRT + u64::from(offset)) as *mut u32, value);
        }
    }
}

/// End the calling CPU's in-service interrupt.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline]
pub fn local_eoi() {
    local_write(lapic_reg::EOI, 0);
}

/// The calling CPU's local APIC id.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub fn local_apic_id() -> u32 {
    if x2apic() {
        local_read(lapic_reg::ID)
    } else {
        local_read(lapic_reg::ID) >> 24
    }
}

/// Send the interrupt command whose low half is `low` from the calling CPU
/// to the APIC with id `destination`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn local_command(destination: u32, low: u32) {
    local_commands([destination], low);
}

/// Send the interrupt command whose low half is `low` from the calling CPU
/// to each APIC in `destinations`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn local_commands(destinations: impl IntoIterator<Item = u32>, low: u32) {
    let x2apic = x2apic();
    if x2apic {
        // A write to an x2APIC register is not serialising: without the
        // fence an IPI can overtake the stores its target is sent to read,
        // as Linux's `weak_wrmsr_fence` guards against. One fence orders
        // them before every command after it.
        // SAFETY: `mfence; lfence` only orders this CPU's accesses.
        unsafe { core::arch::asm!("mfence", "lfence", options(nostack, preserves_flags)) };
    }
    for destination in destinations {
        if x2apic {
            // SAFETY: the x2APIC ICR takes the destination in its high half
            // and the command in its low one, and the kernel runs at ring 0.
            unsafe {
                crate::msr::write(x2apic_msr::ICR, crate::msr::joined(low, destination));
            }
        } else if let Some(high) = xapic_destination(destination) {
            local_write(lapic_reg::ICR_HIGH, high);
            local_write(lapic_reg::ICR_LOW, low);
        }
    }
}

/// The calling CPU's local APIC, reached as [`x2apic`] says.
#[cfg(any(target_os = "none", doc))]
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalApic;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
impl LapicMmio for LocalApic {
    fn read(&self, offset: u16) -> u32 {
        local_read(offset)
    }

    fn write(&mut self, offset: u16, value: u32) {
        local_write(offset, value);
    }

    fn apic_id(&self) -> u32 {
        local_apic_id()
    }

    fn send_command(&mut self, destination: u32, low: u32) {
        local_command(destination, low);
    }

    fn send_commands(&mut self, destinations: impl IntoIterator<Item = u32>, low: u32) {
        local_commands(destinations, low);
    }
}

// --- IO-APIC ---------------------------------------------------------

/// Trait wrapping the two IO-APIC MMIO ports (`IOREGSEL` at +0x00 and
/// `IOWIN` at +0x10).
pub trait IoApicMmio {
    /// Read the 32-bit indirect register `reg`.
    fn read(&mut self, reg: u8) -> u32;
    /// Write `value` to the 32-bit indirect register `reg`.
    fn write(&mut self, reg: u8, value: u32);
    /// Write `vector` to the EOI register an IO-APIC of version 0x20 or
    /// later has: every entry naming that vector has its remote IRR cleared.
    fn end_of_interrupt(&mut self, vector: u8);
}

/// The first IO-APIC version with an EOI register.
pub const IOAPIC_EOI_VERSION: u8 = 0x20;

/// The offset of that EOI register in an IO-APIC's window.
const IOAPIC_EOI: usize = 0x40;

/// The bytes of an IO-APIC's window the kernel reaches: the index and data
/// registers, and the EOI register past them.
pub const IOAPIC_WINDOW_BYTES: u64 = IOAPIC_EOI as u64 + 4;

/// A redirection entry's mask bit (Intel 82093AA §3.2.4).
pub const REDIRECTION_MASKED: u64 = 1 << 16;

/// How an IO-APIC input is wired: what the MADT's interrupt source overrides
/// (ACPI 6.5 §5.2.12.5) say of it, or the bus's convention.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PinWiring {
    /// Level-triggered rather than edge.
    pub level: bool,
    /// Asserted low rather than high.
    pub active_low: bool,
}

impl PinWiring {
    /// The wiring of global system interrupt `gsi` given the override
    /// `flags` (ACPI MPS INTI flags) where one names it: polarity in bits 1:0
    /// and trigger in bits 3:2, each `0b00` for the bus's convention, `0b01`
    /// for high or edge and `0b11` for low or level. An override's source is
    /// an ISA interrupt, edge-triggered and active high by convention
    /// whichever GSI it lands on; a GSI no override names is ISA below 16 and
    /// PCI, level-triggered and active low, above.
    #[must_use]
    pub const fn of(gsi: u32, flags: Option<u16>) -> Self {
        let (isa, polarity, trigger) = match flags {
            Some(flags) => (true, flags & 0b11, (flags >> 2) & 0b11),
            None => (gsi < 16, 0, 0),
        };
        Self {
            level: match trigger {
                0b11 => true,
                0b01 => false,
                _ => !isa,
            },
            active_low: match polarity {
                0b11 => true,
                0b01 => false,
                _ => !isa,
            },
        }
    }

    /// The redirection-entry bits the wiring sets: trigger mode (bit 15) and
    /// input polarity (bit 13).
    #[must_use]
    pub const fn bits(self) -> u64 {
        ((self.level as u64) << 15) | ((self.active_low as u64) << 13)
    }
}

/// A compatibility-format redirection entry delivering `vector` to the APIC
/// at `destination`, fixed and in physical destination mode, for a pin wired
/// as `wiring`, unmasked.
#[must_use]
pub const fn compatibility_entry(vector: u8, destination: u8, wiring: PinWiring) -> u64 {
    (vector as u64) | wiring.bits() | ((destination as u64) << 56)
}

/// IO-APIC driver.
#[derive(Debug)]
pub struct IoApic<M: IoApicMmio> {
    mmio: M,
}

impl<M: IoApicMmio> IoApic<M> {
    /// Construct over a previously-mapped MMIO window.
    pub const fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// Decoded ID (bits 24..28 of the `IOAPICID` register).
    pub fn id(&mut self) -> u8 {
        ((self.mmio.read(0x00) >> 24) & 0x0F) as u8
    }

    /// Maximum redirection entry index (highest valid IRQ on this
    /// controller is `max_redirection_entry()`).
    pub fn max_redirection_entry(&mut self) -> u8 {
        ((self.mmio.read(0x01) >> 16) & 0xFF) as u8
    }

    /// The version (bits 0..8 of `IOAPICVER`).
    pub fn version(&mut self) -> u8 {
        self.mmio.read(0x01).to_le_bytes()[0]
    }

    /// Clear the remote IRR of every entry naming `vector`, through the EOI
    /// register: only on an IO-APIC of [`IOAPIC_EOI_VERSION`] or later.
    pub fn end_of_interrupt(&mut self, vector: u8) {
        self.mmio.end_of_interrupt(vector);
    }

    /// Write redirection entry `pin` whole, in the order that never leaves it
    /// unmasked half-written: a masked entry's low half, which holds the mask,
    /// first; an unmasked entry's high half first.
    pub fn write_redirection_entry(&mut self, pin: u8, entry: u64) {
        let Some(reg) = redirection_register(pin) else {
            return;
        };
        let (low, high) = crate::msr::halves(entry);
        if entry & REDIRECTION_MASKED != 0 {
            self.mmio.write(reg, low);
            self.mmio.write(reg.saturating_add(1), high);
        } else {
            self.mmio.write(reg.saturating_add(1), high);
            self.mmio.write(reg, low);
        }
    }

    /// Write only the low half of redirection entry `pin`, which holds its
    /// vector, trigger, polarity and mask: masking or unmasking a pin leaves
    /// its destination as it is.
    pub fn write_redirection_low(&mut self, pin: u8, low: u32) {
        if let Some(reg) = redirection_register(pin) {
            self.mmio.write(reg, low);
        }
    }

    /// Read the low half of redirection entry `pin` through the
    /// underlying [`IoApicMmio`].
    ///
    /// Bit 16 is the mask bit; bits 0..7 carry the vector. Used by
    /// the kernel-binary `IoApicController::read_pin_low` accessor
    /// (Stage 4.D Item 2-tail.2 QEMU validation) to re-read the
    /// hardware mask state after `IrqTable::fire`; that path is
    /// the evidence trail for the mask-before-wake invariant
    /// documented in `docs/src/security/irq.md`.
    pub fn read_redirection_entry_low(&mut self, pin: u8) -> u32 {
        redirection_register(pin).map_or(crate::msr::halves(REDIRECTION_MASKED).0, |reg| {
            self.mmio.read(reg)
        })
    }
}

/// The most redirection entries an IO-APIC's eight-bit register select
/// reaches: entry `n`'s halves are registers `0x10 + 2n` and `0x11 + 2n`.
pub const IOAPIC_ADDRESSABLE_PINS: u32 = 120;

/// The register holding the low half of redirection entry `pin`; [`None`]
/// past [`IOAPIC_ADDRESSABLE_PINS`], whose entry no register selects.
fn redirection_register(pin: u8) -> Option<u8> {
    let reg = pin.checked_mul(2)?.checked_add(0x10)?;
    reg.checked_add(1).map(|_| reg)
}

/// Volatile-MMIO impl of [`IoApicMmio`] for a real IO-APIC.
#[cfg(any(target_os = "none", doc))]
#[derive(Debug)]
pub struct VolatileIoApicMmio {
    base: *mut u32,
}

#[cfg(any(target_os = "none", doc))]
// SAFETY: the pointer names the IO-APIC's own register window, reached only
// through volatile accesses under the controller's lock, so moving the handle
// between CPUs is sound.
unsafe impl Send for VolatileIoApicMmio {}

#[cfg(any(target_os = "none", doc))]
impl VolatileIoApicMmio {
    /// Wrap an existing kernel-mapped IO-APIC base address.
    ///
    /// # Safety
    ///
    /// `base` must be a valid kernel-mapped virtual address of the
    /// IO-APIC's MMIO window (typically `0xFEC0_0000` physical).
    pub const unsafe fn new(base: *mut u32) -> Self {
        Self { base }
    }
}

#[cfg(any(target_os = "none", doc))]
impl IoApicMmio for VolatileIoApicMmio {
    fn read(&mut self, reg: u8) -> u32 {
        // SAFETY: IOREGSEL and IOWIN are at fixed +0x00 and +0x10
        // offsets within the IO-APIC's MMIO window; the constructor's
        // contract covers this.
        unsafe {
            core::ptr::write_volatile(self.base, u32::from(reg));
            core::ptr::read_volatile(self.base.byte_add(0x10))
        }
    }
    fn write(&mut self, reg: u8, value: u32) {
        // SAFETY: as for `read`.
        unsafe {
            core::ptr::write_volatile(self.base, u32::from(reg));
            core::ptr::write_volatile(self.base.byte_add(0x10), value);
        }
    }
    fn end_of_interrupt(&mut self, vector: u8) {
        // SAFETY: the EOI register sits at `IOAPIC_EOI` in the same window; the
        // caller has checked the version that defines it.
        unsafe { core::ptr::write_volatile(self.base.byte_add(IOAPIC_EOI), u32::from(vector)) };
    }
}

// --- Test support (shared with `apic_timer::tests`) ------------------
//
// `tests_support` is `#[cfg(test)]`-only and `pub(crate)`. It exists so
// the LAPIC mock is defined exactly once and reused by `apic_timer`
// (no duplication).

#[cfg(test)]
pub(crate) mod tests_support {
    extern crate std;
    use super::{IoApicMmio, LapicMmio};
    use std::collections::HashMap;
    use std::vec::Vec;

    /// Mock LAPIC backing store: a `HashMap` keyed by register offset
    /// plus a write log so tests can assert the order of operations.
    #[derive(Default)]
    pub struct MockLapicMmio {
        pub regs: HashMap<u16, u32>,
        pub writes: Vec<(u16, u32)>,
    }
    impl LapicMmio for MockLapicMmio {
        fn read(&self, off: u16) -> u32 {
            *self.regs.get(&off).unwrap_or(&0)
        }
        fn write(&mut self, off: u16, val: u32) {
            self.regs.insert(off, val);
            self.writes.push((off, val));
        }
    }

    #[derive(Default)]
    pub struct MockIoApicMmio {
        pub regs: HashMap<u8, u32>,
        pub writes: Vec<(u8, u32)>,
        pub eois: Vec<u8>,
    }
    impl IoApicMmio for MockIoApicMmio {
        fn read(&mut self, reg: u8) -> u32 {
            *self.regs.get(&reg).unwrap_or(&0)
        }
        fn write(&mut self, reg: u8, val: u32) {
            self.regs.insert(reg, val);
            self.writes.push((reg, val));
        }
        fn end_of_interrupt(&mut self, vector: u8) {
            self.eois.push(vector);
        }
    }
}

// --- Tests -----------------------------------------------------------

#[cfg(test)]
mod tests {
    /// A command is sent only to an APIC xAPIC can name: an id past eight
    /// bits would reach another, and the broadcast id every one.
    #[test]
    fn a_command_xapic_cannot_address_is_never_sent() {
        use super::tests_support::MockLapicMmio;
        use super::LapicMmio;
        let mut mock = MockLapicMmio::default();
        for destination in [0xFF, 0x100, 0x1FF] {
            mock.send_command(destination, 0x4041);
        }
        assert!(mock.writes.is_empty(), "{:?}", mock.writes);
        mock.send_command(0xFE, 0x4041);
        assert_eq!(mock.writes.len(), 2);
    }

    /// A batch reaches each target in order with one command each.
    #[test]
    fn a_batch_of_ipis_reaches_each_target_once() {
        use super::tests_support::MockLapicMmio;
        let mut lapic = super::Lapic::new(MockLapicMmio::default());
        lapic.send_ipis([3, 5], super::DeliveryMode::Fixed, 0x40);
        let sent = lapic
            .mmio_mut()
            .writes
            .iter()
            .filter(|(offset, _)| *offset == super::lapic_reg::ICR_HIGH)
            .map(|&(_, value)| value);
        assert!(sent.eq([3 << 24, 5 << 24]));
    }

    /// A disabled APIC is enabled before it is extended, never both at once;
    /// an enabled one is extended; one already extended is left alone.
    #[test]
    fn x2apic_mode_is_reached_by_valid_steps_alone() {
        use super::x2apic_msr::{BASE_ENABLE, BASE_EXTENDED};
        let base = 0xFEE0_0000;
        let extended = base | BASE_ENABLE | BASE_EXTENDED;
        assert!(super::x2apic_transition(base).eq([base | BASE_ENABLE, extended]));
        assert!(super::x2apic_transition(base | BASE_ENABLE).eq([extended]));
        assert_eq!(super::x2apic_transition(extended).count(), 0);
    }

    use super::tests_support::{MockIoApicMmio, MockLapicMmio};
    use super::*;

    /// An xAPIC offset reaches the x2APIC MSR SDM Table 11-6 names.
    #[test]
    fn each_register_reaches_its_x2apic_msr() {
        assert_eq!(LAPIC_BASE_PHYS, 0xFEE0_0000);
        assert_eq!(lapic_reg::EOI, 0xB0);
        assert_eq!(x2apic_register(lapic_reg::ID), 0x802);
        assert_eq!(x2apic_register(lapic_reg::EOI), 0x80B);
        assert_eq!(x2apic_register(lapic_reg::SPURIOUS), 0x80F);
        assert_eq!(x2apic_register(lapic_reg::ICR_LOW), 0x830);
        assert_eq!(x2apic_register(lapic_reg::TIMER_LVT), 0x832);
        assert_eq!(x2apic_register(lapic_reg::TIMER_INITIAL_COUNT), 0x838);
        assert_eq!(x2apic_register(lapic_reg::TIMER_CURRENT_COUNT), 0x839);
        assert_eq!(x2apic_register(lapic_reg::TIMER_DIVIDE_CONFIG), 0x83E);
    }

    /// x2APIC is entered only with remapping that can name its
    /// destinations, on a CPU that has it, unless firmware asks otherwise.
    #[test]
    fn x2apic_is_entered_only_where_remapping_can_name_its_destinations() {
        assert!(may_enter_x2apic(true, false, true));
        assert!(
            !may_enter_x2apic(true, false, false),
            "no extended remapping"
        );
        assert!(!may_enter_x2apic(false, false, true), "no x2APIC");
        assert!(!may_enter_x2apic(true, true, true), "firmware opted out");
    }

    /// A command reaches an xAPIC as its destination's high half and then
    /// the low half that sends it.
    #[test]
    fn an_xapic_command_writes_its_destination_before_sending() {
        let mut lapic = Lapic::new(tests_support::MockLapicMmio::default());
        lapic.send_ipi(3, DeliveryMode::Fixed, 0x41);
        let writes = lapic.mmio_mut().writes.clone();
        assert_eq!(
            writes,
            [
                (lapic_reg::ICR_HIGH, 3 << 24),
                (lapic_reg::ICR_LOW, 0x41 | (1 << 14))
            ]
        );
    }

    /// An ISA interrupt is edge and active high by convention, a PCI one
    /// level and active low, and an override's flags decide each where they
    /// say.
    #[test]
    fn a_pin_is_wired_as_its_override_or_its_bus_says() {
        let edge_high = PinWiring {
            level: false,
            active_low: false,
        };
        let level_low = PinWiring {
            level: true,
            active_low: true,
        };
        assert_eq!(PinWiring::of(4, None), edge_high);
        assert_eq!(PinWiring::of(16, None), level_low);
        assert_eq!(PinWiring::of(2, Some(0b0000)), edge_high, "conforms to ISA");
        assert_eq!(
            PinWiring::of(9, Some(0b1101)),
            PinWiring {
                level: true,
                active_low: false
            }
        );
        assert_eq!(PinWiring::of(20, Some(0b0101)), edge_high);
        assert_eq!(PinWiring::of(9, Some(0b1111)), level_low);
        assert_eq!(
            PinWiring::of(20, Some(0b0000)),
            edge_high,
            "an override conforms to its ISA source wherever it lands"
        );
        assert_eq!(level_low.bits(), (1 << 15) | (1 << 13));
        assert_eq!(
            compatibility_entry(0x31, 2, level_low),
            0x31 | (1 << 15) | (1 << 13) | (2 << 56)
        );
    }

    #[test]
    fn the_version_and_end_of_interrupt_reach_their_registers() {
        let mut mock = MockIoApicMmio::default();
        mock.regs.insert(0x01, 0x0017_0020);
        let mut ioapic = IoApic::new(mock);
        assert_eq!(ioapic.version(), IOAPIC_EOI_VERSION);
        assert_eq!(ioapic.max_redirection_entry(), 0x17);
        ioapic.end_of_interrupt(0x09);
        assert_eq!(ioapic.mmio.eois, [0x09]);
        assert!(
            ioapic.mmio.writes.is_empty(),
            "no indexed register was touched"
        );
    }

    /// A pin is never left unmasked with half of its entry written.
    #[test]
    fn a_redirection_entry_is_written_mask_first_and_unmask_last() {
        let mut ioapic = IoApic::new(MockIoApicMmio::default());
        let entry = compatibility_entry(0x40, 7, PinWiring::default());
        ioapic.write_redirection_entry(3, entry | REDIRECTION_MASKED);
        ioapic.write_redirection_entry(3, entry);
        assert_eq!(
            ioapic.mmio.writes,
            [
                (0x16, 0x40 | (1 << 16)),
                (0x17, 7 << 24),
                (0x17, 7 << 24),
                (0x16, 0x40),
            ]
        );
    }

    #[test]
    fn lapic_id_decodes_high_byte() {
        let mut mock = MockLapicMmio::default();
        mock.regs.insert(lapic_reg::ID, 0x07 << 24);
        let lapic = Lapic::new(mock);
        assert_eq!(lapic.id(), 7);
    }

    #[test]
    fn software_enable_writes_canonical_sequence() {
        let mut lapic = Lapic::new(MockLapicMmio::default());
        lapic.software_enable(0xFF);
        assert_eq!(
            lapic.mmio.writes,
            [
                (lapic_reg::TPR, 0),
                (lapic_reg::SPURIOUS, spurious::ENABLE | 0xFF)
            ],
            "no logical destination register is written, which x2APIC mode refuses"
        );
    }

    #[test]
    fn eoi_writes_zero() {
        let mut lapic = Lapic::new(MockLapicMmio::default());
        lapic.eoi();
        assert_eq!(lapic.mmio.writes, [(lapic_reg::EOI, 0)]);
    }

    #[test]
    fn send_ipi_writes_high_before_low() {
        let mut lapic = Lapic::new(MockLapicMmio::default());
        lapic.send_ipi(0x03, DeliveryMode::Fixed, 0x20);
        let w = &lapic.mmio.writes;
        assert_eq!(w[0], (lapic_reg::ICR_HIGH, 0x03 << 24));
        // Vector 0x20, mode Fixed (0), level-assert bit set.
        assert_eq!(w[1], (lapic_reg::ICR_LOW, 0x20 | (1 << 14)));
    }

    #[test]
    fn init_sipi_sequence_uses_correct_modes() {
        let mut lapic = Lapic::new(MockLapicMmio::default());
        lapic.send_ipi(0x01, DeliveryMode::Init, 0);
        lapic.send_init_deassert(0x01);
        // SIPI: vector encodes physical frame; for frame 0x8000 that's 8.
        lapic.send_ipi(0x01, DeliveryMode::StartUp, 0x08);

        let writes = &lapic.mmio.writes;
        // Six writes total: high+low for each of the three IPIs.
        assert_eq!(writes.len(), 6);

        // Each high write must target apic 0x01 in bits 24..32.
        for (i, (off, val)) in writes.iter().enumerate() {
            if i % 2 == 0 {
                assert_eq!(*off, lapic_reg::ICR_HIGH);
                assert_eq!(*val >> 24, 0x01);
            }
        }

        // INIT IPI low: mode=5 (Init) in bits 8..11, assert.
        assert_eq!(writes[1].1 & 0x0700, (DeliveryMode::Init as u32) << 8);
        // SIPI low: mode=6, vector=0x08.
        assert_eq!(writes[5].1 & 0xFF, 0x08);
        assert_eq!(writes[5].1 & 0x0700, (DeliveryMode::StartUp as u32) << 8);
    }

    /// No access strays from the registers the kernel drives, nor goes the
    /// way a register does not: reading end-of-interrupt, or writing a count
    /// the timer keeps, faults in x2APIC mode.
    #[test]
    fn a_value_setting_a_bit_its_register_does_not_take_is_never_written() {
        assert!(admits(lapic_reg::EOI, 0));
        assert!(!admits(lapic_reg::EOI, 1), "x2APIC takes only zero");
        assert!(admits(lapic_reg::TIMER_LVT, 0x2_0040 | (1 << 16)));
        assert!(
            !admits(lapic_reg::TIMER_LVT, 1 << 12),
            "delivery status reads only"
        );
        assert!(admits(lapic_reg::TIMER_INITIAL_COUNT, u32::MAX));
        assert!(!admits(lapic_reg::TIMER_DIVIDE_CONFIG, 0b0100));
        assert!(!admits(lapic_reg::ID, 0), "not writable at all");
    }

    #[test]
    fn only_a_driven_register_is_reached_and_only_its_way() {
        assert!(reaches(lapic_reg::EOI, true) && !reaches(lapic_reg::EOI, false));
        assert!(reaches(lapic_reg::ID, false) && !reaches(lapic_reg::ID, true));
        assert!(!reaches(lapic_reg::TIMER_CURRENT_COUNT, true));
        assert!(reaches(lapic_reg::TIMER_INITIAL_COUNT, true));
        assert!(
            !reaches(0xFFF0, false) && !reaches(0xFFF0, true),
            "past the block"
        );
        assert!(
            !reaches(lapic_reg::ID + 4, false),
            "inside it, but no register"
        );
        assert!(!reaches(lapic_reg::ICR_LOW, false));
    }

    /// An IO-APIC claiming more entries than its register select reaches is
    /// programmed only where it can be: the rest read masked, written nowhere.
    #[test]
    fn an_entry_past_the_register_space_is_neither_written_nor_read() {
        assert_eq!(redirection_register(119), Some(0xFE));
        assert_eq!(redirection_register(120), None);
        assert_eq!(redirection_register(u8::MAX), None);
        let mut ioapic = IoApic::new(MockIoApicMmio::default());
        ioapic.write_redirection_entry(200, REDIRECTION_MASKED);
        ioapic.write_redirection_low(120, 0);
        assert_eq!(
            u64::from(ioapic.read_redirection_entry_low(254)),
            REDIRECTION_MASKED
        );
        assert!(ioapic.mmio.writes.is_empty());
    }

    #[test]
    fn ioapic_decodes_id_and_max_entry() {
        let mut mock = MockIoApicMmio::default();
        mock.regs.insert(0x00, 0x0F << 24);
        mock.regs.insert(0x01, 23 << 16);
        let mut io = IoApic::new(mock);
        assert_eq!(io.id(), 0x0F);
        assert_eq!(io.max_redirection_entry(), 23);
    }
}
