//! The fatal report: the two records a dying kernel ends with, the latch
//! every report path enters first, and the one writer the ports share for the
//! reports their own paths make.
//!
//! Every fatal report — the kernel's full post-mortem (`kernel/core`) and a
//! port's own report below — ends with exactly one record in the diagnostic
//! line shape (`lib/log`): [`KERNEL_PANIC`] or [`KERNEL_FAULT`]. Whatever reads
//! the console, a person or the QEMU harness, therefore has one line to find
//! and knows nothing of the report follows it.
//!
//! A machine dies once. Every report path takes an [`Entry`] from [`enter`]
//! before it reads anything: the first writes the full report, the next one
//! bare record, and every later one nothing. A report that fails while it is
//! being written — a panic in a panic message's `Display`, a fault in the
//! console — therefore ends in a record rather than recursing until its stack
//! overruns into whatever lies below it.
//!
//! A port writes its own report where no kernel stands behind it: a minimal
//! test kernel that links only the port, or a failure before the kernel
//! installed its handlers. Those reports carry the cause, the processor, and
//! the boot-stack guard's verdict; the register snapshot and backtrace are the
//! kernel's post-mortem to add.

use core::fmt::{self, Write};
use core::panic::Location;
use core::sync::atomic::{AtomicU32, Ordering};

use tairix_log::{write_diag_line, Event, EventId, Field, FieldValue, Level};

use crate::backtrace::BootStackGuard;

/// One of the two records a fatal report ends with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FatalRecord {
    /// The record's stable event id.
    pub id: EventId,
    /// The record's fixed message.
    pub message: &'static str,
    /// The message of the bare record an [`Entry::Nested`] writes under the
    /// same id, so a re-entered report is still attributable to its cause.
    pub nested: &'static str,
}

/// The record a kernel panic ends with.
pub const KERNEL_PANIC: FatalRecord = FatalRecord {
    id: EventId(4010),
    message: "kernel panic",
    nested: "kernel panic (nested — re-entered the fatal-report path)",
};

/// The record a fatal kernel-mode CPU exception ends with.
pub const KERNEL_FAULT: FatalRecord = FatalRecord {
    id: EventId(4011),
    message: "fatal kernel fault",
    nested: "fatal kernel fault (nested — re-entered the fatal-report path)",
};

/// What a fatal report may write, decided before it reads anything.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    /// The boot's first fatal entry: the full report.
    Report,
    /// The next: one bare record. It is either the report failing inside
    /// itself or a second processor failing while the first reports, and in
    /// both cases the full report's machinery is what cannot be trusted.
    Nested,
    /// Every later entry: nothing more can be written safely.
    Silent,
}

impl Entry {
    const fn after(taken: u32) -> Self {
        match taken {
            0 => Self::Report,
            1 => Self::Nested,
            _ => Self::Silent,
        }
    }
}

/// Fatal entries taken this boot. It only ever counts up — outside host tests
/// nothing resets it — so a non-zero count always means a report is under way
/// or done.
static ENTRIES: AtomicU32 = AtomicU32::new(0);

/// Enter the fatal-report path.
///
/// Shared by every report path in the image, the kernel's post-mortem and the
/// port's own reports alike, so two of them can never both write a full
/// report.
pub fn enter() -> Entry {
    let taken = ENTRIES
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
            Some(taken.saturating_add(1))
        })
        .unwrap_or_else(|taken| taken);
    Entry::after(taken)
}

/// [`enter`], for a memory system that cannot perform an atomic
/// read-modify-write at the moment — aarch64 with stage-1 translation off,
/// where every access is Device-nGnRnE and an exclusive monitor may never
/// grant, so the retry loop behind [`enter`] would spin forever.
///
/// A plain load and store: exact while one processor reports. A processor
/// racing it through [`enter`] can at worst leave two full reports side by
/// side, never an unbounded recursion on either.
pub fn enter_without_atomics() -> Entry {
    let taken = ENTRIES.load(Ordering::Relaxed);
    ENTRIES.store(taken.saturating_add(1), Ordering::Relaxed);
    Entry::after(taken)
}

/// Return the latch to its boot state, so each host test that drives a report
/// starts from a first entry.
#[cfg(any(test, feature = "host-tests"))]
pub fn reset_for_tests() {
    ENTRIES.store(0, Ordering::Release);
}

/// Bytes [`format_hex_word`] writes: `0x` and sixteen nibbles.
pub const HEX_WORD_LEN: usize = 18;

/// Render `value` into `buf` as `0x` and sixteen lowercase nibbles, and
/// return the whole buffer: the fixed width a fatal record spells a register
/// or an address in, so a column of them stays aligned.
#[must_use]
pub fn format_hex_word(value: u64, buf: &mut [u8; HEX_WORD_LEN]) -> &str {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    buf[0] = b'0';
    buf[1] = b'x';
    let mut rest = value;
    for slot in buf[2..].iter_mut().rev() {
        *slot = DIGITS[(rest & 0xf) as usize];
        rest >>= 4;
    }
    core::str::from_utf8(&buf[..]).unwrap_or("0x")
}

/// A fatal CPU exception taken in **kernel** mode, as the port's trap path
/// saw it.
///
/// The same words on every port, spelled differently per architecture:
/// `ESR_EL1` / `FAR_EL1` / `ELR_EL1` on aarch64, the packed vector and error
/// code / `CR2` / `RIP` on x86_64, and `scause` / `stval` / `sepc` on riscv64.
/// The port names them and decides which the CPU actually gave; everything
/// above it reads the neutral record.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct KernelFault {
    /// The port's exception syndrome — why the CPU trapped — or `None` for an
    /// entry the CPU records no syndrome for (an aarch64 FIQ).
    pub syndrome: Option<u64>,
    /// The address the faulting access could not reach, or `None` where the
    /// exception supplies none. Never a stale or reset register value.
    pub address: Option<u64>,
    /// The faulting instruction, or the interrupted one for an asynchronous
    /// entry.
    pub pc: u64,
    /// The kernel stack pointer the interrupted code was running on, or
    /// `None` when it was running in user mode or the architecture leaves the
    /// saved one undefined (an x86_64 `#DF`).
    pub sp: Option<u64>,
}

/// Words a [`KernelFault`] record carries: the syndrome, the fault address,
/// the faulting PC and the interrupted kernel stack pointer.
pub const FAULT_WORDS: usize = 4;

/// Stack storage [`KernelFault::fields`] spells its words into, so a fatal
/// report allocates nothing.
#[derive(Debug)]
pub struct FaultWords([[u8; HEX_WORD_LEN]; FAULT_WORDS]);

impl FaultWords {
    /// Storage for one record's words.
    #[must_use]
    pub const fn new() -> Self {
        Self([[0; HEX_WORD_LEN]; FAULT_WORDS])
    }
}

impl Default for FaultWords {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelFault {
    /// The four words in the order and under the keys every record uses.
    fn words(&self) -> [(&'static str, Option<u64>); FAULT_WORDS] {
        [
            ("syndrome", self.syndrome),
            ("fault_addr", self.address),
            ("fault_pc", Some(self.pc)),
            ("fault_sp", self.sp),
        ]
    }

    /// The record's cause fields, spelled into `words`: a word the CPU did not
    /// give is `null`, never a placeholder that reads as a value.
    #[must_use]
    pub fn fields<'b>(&self, words: &'b mut FaultWords) -> [Field<'b>; FAULT_WORDS] {
        let mut fields = [NULL_FIELD; FAULT_WORDS];
        for ((field, (key, word)), buf) in fields.iter_mut().zip(self.words()).zip(&mut words.0) {
            *field = Field {
                key,
                value: word.map_or(FieldValue::Null, |value| {
                    FieldValue::Str(format_hex_word(value, buf))
                }),
            };
        }
        fields
    }
}

impl fmt::Display for KernelFault {
    /// One line, in the field order and spelling the fatal record uses, so a
    /// prose report and the record read alike.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for (key, word) in self.words() {
            match word {
                Some(value) => write!(f, "{separator}{key}={value:#018x}")?,
                None => write!(f, "{separator}{key}=null")?,
            }
            separator = " ";
        }
        Ok(())
    }
}

/// An unset slot in a field list under construction.
const NULL_FIELD: Field<'static> = Field {
    key: "",
    value: FieldValue::Null,
};

/// The processor a report names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Processor {
    /// The kernel's dense CPU id: the `cpu` every other record names.
    Cpu(u32),
    /// A hardware identity the port's own report cannot map to a dense id,
    /// under the key the architecture names it by (`hart`, `apic_id`), so it is
    /// never mistaken for one.
    Hardware {
        /// The record key, and the name the prose uses.
        key: &'static str,
        /// The identity.
        id: u64,
    },
}

impl Processor {
    fn field(self) -> Field<'static> {
        match self {
            Self::Cpu(cpu) => Field {
                key: "cpu",
                value: FieldValue::UnsignedInt(u64::from(cpu)),
            },
            Self::Hardware { key, id } => Field {
                key,
                value: FieldValue::UnsignedInt(id),
            },
        }
    }
}

impl fmt::Display for Processor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Cpu(cpu) => write!(f, "CPU {cpu}"),
            Self::Hardware { key, id } => write!(f, "{key} {id}"),
        }
    }
}

/// How a port names itself in a report.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Reporter {
    /// The port, as the report's cause line names it (`aarch64`).
    pub port: &'static str,
}

impl Reporter {
    /// Write the report of a panic on `on`, ending with its [`KERNEL_PANIC`]
    /// record, or the bare record or nothing as `entry` allows.
    ///
    /// `message` is what the panic said — a port passes its `PanicInfo`, whose
    /// display carries the message and the location — and `location` is where
    /// it was raised, for the record. `guard` is asked for the boot-stack
    /// guard's verdict only when the full report is written: a panic whose
    /// real cause was an overrun otherwise reads as an unexplained corruption
    /// of whatever sat below the stack.
    pub fn panic(
        self,
        w: &mut dyn Write,
        entry: Entry,
        on: Processor,
        message: &dyn fmt::Display,
        location: Option<&Location<'_>>,
        guard: impl FnOnce() -> Option<BootStackGuard>,
    ) {
        if entry != Entry::Report {
            nested(w, entry, KERNEL_PANIC, on);
            return;
        }
        let guard = guard();
        banner(
            w,
            "PANIC",
            on,
            format_args!("{} panic on {on}: {message}", self.port),
            guard,
        );
        let (file, line, column) = location.map_or(("<unknown>", 0, 0), |at| {
            (at.file(), at.line(), at.column())
        });
        record(
            w,
            KERNEL_PANIC,
            on,
            &[
                Field {
                    key: "file",
                    value: FieldValue::Str(file),
                },
                Field {
                    key: "line",
                    value: FieldValue::UnsignedInt(u64::from(line)),
                },
                Field {
                    key: "column",
                    value: FieldValue::UnsignedInt(u64::from(column)),
                },
            ],
            guard,
        );
    }

    /// Write the report of a fatal exception on `on` that no handler claimed,
    /// ending with its [`KERNEL_FAULT`] record, or the bare record or nothing
    /// as `entry` allows.
    ///
    /// `decoded` is the port's own reading of the words — the register names,
    /// the exception class, the privilege it was taken at — which the neutral
    /// record cannot carry. `guard` is asked only for the full report.
    pub fn fault(
        self,
        w: &mut dyn Write,
        entry: Entry,
        on: Processor,
        fault: &KernelFault,
        decoded: fmt::Arguments<'_>,
        guard: impl FnOnce() -> Option<BootStackGuard>,
    ) {
        if entry != Entry::Report {
            nested(w, entry, KERNEL_FAULT, on);
            return;
        }
        let guard = guard();
        banner(
            w,
            "FAULT",
            on,
            format_args!(
                "{} exception on {on} with no fault handler installed: {decoded}",
                self.port
            ),
            guard,
        );
        let mut words = FaultWords::new();
        record(w, KERNEL_FAULT, on, &fault.fields(&mut words), guard);
    }
}

/// The prose half of a report, for a person reading the console.
fn banner(
    w: &mut dyn Write,
    kind: &str,
    on: Processor,
    cause: fmt::Arguments<'_>,
    guard: Option<BootStackGuard>,
) {
    let _ = writeln!(
        w,
        "\n==================== TAIRiX KERNEL {kind} ===================="
    );
    let _ = writeln!(w, "[tairix-kernel] {cause}");
    if let Some(verdict) = guard {
        let _ = writeln!(w, "boot-stack guard: {verdict}");
    }
    let _ = writeln!(
        w,
        "{on} halted; the kernel is non-recoverable in production."
    );
    let _ = writeln!(
        w,
        "============================================================="
    );
}

/// Write `kind`'s bare record for a nested entry; a silent one writes nothing.
fn nested(w: &mut dyn Write, entry: Entry, kind: FatalRecord, on: Processor) {
    if entry == Entry::Nested {
        write_diag_line(
            w,
            None,
            false,
            &Event {
                level: Level::Error,
                id: kind.id,
                message: kind.nested,
                fields: &[on.field()],
            },
        );
    }
}

/// Fields a record can carry: the processor, a fault's words (a panic has
/// three), and the guard's verdict and overrun depth.
const RECORD_FIELDS: usize = 1 + FAULT_WORDS + 2;

/// Write `kind`'s record: the processor, the cause fields, and the guard's
/// verdict, in the keys the kernel's own post-mortem uses.
fn record<const CAUSE: usize>(
    w: &mut dyn Write,
    kind: FatalRecord,
    on: Processor,
    cause: &[Field<'_>; CAUSE],
    guard: Option<BootStackGuard>,
) {
    // Refused at build time: a wider cause would drop the record's last field.
    const { assert!(1 + CAUSE + 2 <= RECORD_FIELDS) };
    let mut overrun = [0u8; HEX_WORD_LEN];
    let mut fields = [NULL_FIELD; RECORD_FIELDS];
    let mut used = 0;
    let mut push = |field| {
        if let Some(slot) = fields.get_mut(used) {
            *slot = field;
            used += 1;
        }
    };
    push(on.field());
    cause.iter().copied().for_each(&mut push);
    if let Some(verdict) = guard {
        push(Field {
            key: "boot_stack_guard",
            value: FieldValue::Str(verdict.label()),
        });
        if let BootStackGuard::BelowStack { bytes, .. } = verdict {
            push(Field {
                key: "boot_stack_overrun_bytes",
                value: FieldValue::Str(format_hex_word(bytes, &mut overrun)),
            });
        }
    }
    write_diag_line(
        w,
        None,
        false,
        &Event {
            level: Level::Error,
            id: kind.id,
            message: kind.message,
            fields: &fields[..used],
        },
    );
}

#[cfg(test)]
#[path = "fatal_tests.rs"]
mod tests;
