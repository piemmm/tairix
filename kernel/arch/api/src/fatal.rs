//! The fatal report: the records a dying kernel ends with, the latch every
//! report path enters first, and the one writer the ports share for the
//! reports their own paths make.
//!
//! Every fatal report — the kernel's full post-mortem (`kernel/core`) and a
//! port's own report below — ends with exactly one record in the diagnostic
//! line shape (`lib/log`), one of [`FATAL_RECORDS`]. Whatever reads the
//! console, a person or the QEMU harness, therefore has one line to find and
//! knows nothing of the report follows it.
//!
//! A machine dies once. Every report path takes an [`Entry`] from [`enter`]
//! before it reads anything: the first writes the full report, the next one
//! bare record, and every later one nothing. A report that fails while it is
//! being written — a panic in a panic message's `Display`, a fault in the
//! console — therefore ends in a record rather than recursing until its stack
//! overruns into whatever lies below it. The latch keeps the cause the first
//! entry was taken for, so that bare record still names what killed the
//! machine as well as what broke its report.
//!
//! A port writes its own report where no kernel stands behind it: a minimal
//! test kernel that links only the port, or a failure before the kernel
//! installed its handlers. Those reports carry the cause, the processor, and
//! the boot-stack guard's verdict; the register snapshot and backtrace are the
//! kernel's post-mortem to add.

use core::fmt::{self, Write};
use core::mem::{offset_of, size_of};
use core::panic::Location;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use tairix_log::{write_diag_line, Event, EventId, Field, FieldValue, Level};

use crate::backtrace::BootStackGuard;

/// One of the records a fatal report ends with.
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

/// The record a task's corrupt kernel stack ends with.
pub const KERNEL_STACK_CORRUPT: FatalRecord = FatalRecord {
    id: EventId(4012),
    message: "kernel stack corrupt",
    nested: "kernel stack corrupt (nested — re-entered the fatal-report path)",
};

/// Every record a fatal report can end with.
pub const FATAL_RECORDS: [FatalRecord; 3] = [KERNEL_PANIC, KERNEL_FAULT, KERNEL_STACK_CORRUPT];

/// What a fatal report may write, decided before it reads anything.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    /// The boot's first fatal entry: the full report.
    Report,
    /// The next: one bare record. It is either the report failing inside
    /// itself or a second processor failing while the first reports, and in
    /// both cases the full report's machinery is what cannot be trusted.
    Nested {
        /// What the first entry was taken for, where it said.
        first: Option<Cause>,
    },
    /// Every later entry: nothing more can be written safely.
    Silent,
}

/// Bytes of a panic's source file a [`Cause`] keeps: the end of the path,
/// which names the file.
pub const FILE_TAIL: usize = 64;

// A tail's length is kept in a byte.
const _: () = assert!(FILE_TAIL <= u8::MAX as usize);

/// Why a fatal report was entered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Cause {
    /// A panic, raised where its location says, if it says.
    Panic {
        /// The end of the source file's path, `len` bytes of it.
        file: [u8; FILE_TAIL],
        /// Bytes of `file` in use.
        len: u8,
        /// The source line, or zero where none is known.
        line: u32,
        /// The source column, or zero where none is known.
        column: u32,
    },
    /// A fatal exception.
    Fault(KernelFault),
    /// A task's kernel stack found corrupt.
    Stack(StackCorruption),
}

impl Cause {
    /// A panic raised at `location`.
    #[must_use]
    pub fn panic(location: Option<&Location<'_>>) -> Self {
        let (file, len) = location.map_or(([0; FILE_TAIL], 0), |at| file_tail(at.file()));
        let (line, column) = location.map_or((0, 0), |at| (at.line(), at.column()));
        Self::Panic {
            file,
            len,
            line,
            column,
        }
    }

    /// The kind, as the bare record's `first` field names it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Panic { .. } => "panic",
            Self::Fault(_) => "fault",
            Self::Stack(_) => "stack",
        }
    }
}

impl fmt::Display for Cause {
    /// One line: the kind, then its fields under the keys a record uses.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind())?;
        match self {
            Self::Panic {
                file,
                len,
                line,
                column,
            } => {
                let file = file
                    .get(..usize::from(*len))
                    .and_then(|tail| core::str::from_utf8(tail).ok())
                    .filter(|tail| !tail.is_empty())
                    .unwrap_or("null");
                let [file_key, line_key, column_key] = PANIC_KEYS;
                write!(
                    f,
                    " {file_key}={file} {line_key}={line} {column_key}={column}"
                )
            }
            Self::Fault(fault) => write!(f, " {fault}"),
            Self::Stack(stack) => {
                write!(f, " {}={}", OWN_KEYS.check, stack.check.label())?;
                for (key, word) in OWN_KEYS.stack.iter().zip(stack.words()) {
                    write!(f, " {key}={word:#018x}")?;
                }
                Ok(())
            }
        }
    }
}

/// The last bytes of `path` that fit [`FILE_TAIL`], cut at a character
/// boundary, and how many there are.
fn file_tail(path: &str) -> ([u8; FILE_TAIL], u8) {
    let mut start = path.len().saturating_sub(FILE_TAIL);
    while !path.is_char_boundary(start) {
        start += 1;
    }
    let tail = &path.as_bytes()[start..];
    let mut file = [0; FILE_TAIL];
    file[..tail.len()].copy_from_slice(tail);
    // At most `FILE_TAIL` bytes, which fits.
    (file, u8::try_from(tail.len()).unwrap_or(u8::MAX))
}

/// The first entry's cause, published as that entry is taken.
///
/// Written by the entry the latch grants the full report, its words before
/// its kind, and read only after the kind, so a reader never sees a cause half
/// written. Two processors racing through [`enter_without_atomics`] can both be
/// granted it and interleave their stores, so it is exact only while one
/// processor reports, as that entry is.
///
/// Laid out in C order under [`KEPT_SYMBOL`], so a debugger can read it off a
/// stopped machine whose report never reached the console ([`decode_kept`]).
#[repr(C)]
struct Kept {
    kind: AtomicU8,
    /// Which of a fault's words the CPU gave, a bit each.
    given: AtomicU8,
    /// Zero, filling what would be padding, so every byte read is defined.
    filler: [AtomicU8; KEPT_FILLER],
    words: [AtomicU64; CAUSE_WORDS],
    file: [AtomicU8; FILE_TAIL],
}

/// The bytes between the two flag bytes and the word-aligned words.
const KEPT_FILLER: usize = size_of::<u64>() - 2;

const _: () =
    assert!(size_of::<Kept>() == size_of::<u64>() + CAUSE_WORDS * size_of::<u64>() + FILE_TAIL);

const KEPT_NONE: u8 = 0;
const KEPT_PANIC: u8 = 1;
const KEPT_FAULT: u8 = 2;
const KEPT_STACK_GUARD: u8 = 3;
const KEPT_STACK_SUSPENSION: u8 = 4;

/// The symbol the kept cause is exported under, once for both the attribute
/// and [`KEPT_SYMBOL`].
macro_rules! kept_symbol {
    () => {
        "tairix_fatal_first_cause"
    };
}

/// The symbol a kernel image exports the kept cause under.
pub const KEPT_SYMBOL: &str = kept_symbol!();

/// Bytes of the kept cause at [`KEPT_SYMBOL`], a whole number of words.
pub const KEPT_BYTES: usize = size_of::<Kept>();

const _: () = assert!(KEPT_BYTES.is_multiple_of(size_of::<u64>()));

/// A kept cause as plain values, from the latch's atomics or from the bytes a
/// stopped machine holds, so the two are decoded alike.
struct KeptImage {
    kind: u8,
    given: u8,
    words: [u64; CAUSE_WORDS],
    file: [u8; FILE_TAIL],
}

impl KeptImage {
    fn cause(&self) -> Option<Cause> {
        let word = |at: usize| self.words[at];
        match self.kind {
            KEPT_PANIC => Some(Cause::Panic {
                file: self.file,
                len: u8::try_from(word(2)).unwrap_or(0),
                line: u32::try_from(word(0)).unwrap_or(0),
                column: u32::try_from(word(1)).unwrap_or(0),
            }),
            KEPT_FAULT => {
                let at = |bit: usize| (self.given & (1 << bit) != 0).then(|| word(bit));
                Some(Cause::Fault(KernelFault {
                    syndrome: at(0),
                    address: at(1),
                    pc: word(2),
                    sp: at(3),
                }))
            }
            kind @ (KEPT_STACK_GUARD | KEPT_STACK_SUSPENSION) => {
                Some(Cause::Stack(StackCorruption {
                    check: if kind == KEPT_STACK_GUARD {
                        StackCheck::Guard
                    } else {
                        StackCheck::Suspension
                    },
                    task: word(0),
                    task_sp: word(1),
                    base: word(2),
                    top: word(3),
                }))
            }
            _ => None,
        }
    }
}

/// The cause a stopped machine's latch kept, from the [`KEPT_BYTES`] read at
/// its [`KEPT_SYMBOL`] in memory order, its words in the byte order the image
/// was built for. `None` for too few bytes, or nothing kept.
#[must_use]
pub fn decode_kept(bytes: &[u8], little_endian: bool) -> Option<Cause> {
    let bytes = bytes.get(..KEPT_BYTES)?;
    let byte = |at: usize| bytes.get(at).copied().unwrap_or(0);
    let words_at = offset_of!(Kept, words);
    let file_at = offset_of!(Kept, file);
    KeptImage {
        kind: byte(offset_of!(Kept, kind)),
        given: byte(offset_of!(Kept, given)),
        words: core::array::from_fn(|at| {
            let start = words_at + at * size_of::<u64>();
            let word: [u8; 8] = core::array::from_fn(|i| byte(start + i));
            if little_endian {
                u64::from_le_bytes(word)
            } else {
                u64::from_be_bytes(word)
            }
        }),
        file: core::array::from_fn(|at| byte(file_at + at)),
    }
    .cause()
}

/// The bytes a machine holds at [`KEPT_SYMBOL`] once its latch has kept
/// `cause`, in this machine's byte order.
#[cfg(any(test, feature = "host-tests"))]
#[must_use]
pub fn kept_image(cause: &Cause) -> [u8; KEPT_BYTES] {
    let kept = Kept::new();
    kept.keep(cause);
    // SAFETY: `Kept` is `repr(C)` with no padding, so all `KEPT_BYTES` of it
    // are initialised integer bytes, and nothing stores to `kept` while the
    // borrowed slice lives.
    let bytes =
        unsafe { core::slice::from_raw_parts(core::ptr::from_ref(&kept).cast::<u8>(), KEPT_BYTES) };
    let mut image = [0; KEPT_BYTES];
    image.copy_from_slice(bytes);
    image
}

impl Kept {
    const fn new() -> Self {
        Self {
            kind: AtomicU8::new(KEPT_NONE),
            given: AtomicU8::new(0),
            filler: [const { AtomicU8::new(0) }; KEPT_FILLER],
            words: [const { AtomicU64::new(0) }; CAUSE_WORDS],
            file: [const { AtomicU8::new(0) }; FILE_TAIL],
        }
    }

    fn keep(&self, cause: &Cause) {
        let kind = match *cause {
            Cause::Panic {
                file,
                len,
                line,
                column,
            } => {
                for (slot, byte) in self.file.iter().zip(file) {
                    slot.store(byte, Ordering::Relaxed);
                }
                self.words[0].store(u64::from(line), Ordering::Relaxed);
                self.words[1].store(u64::from(column), Ordering::Relaxed);
                self.words[2].store(u64::from(len), Ordering::Relaxed);
                KEPT_PANIC
            }
            Cause::Fault(fault) => {
                let mut given = 0;
                for (at, (slot, word)) in self.words.iter().zip(fault.words()).enumerate() {
                    if let Some(value) = word {
                        slot.store(value, Ordering::Relaxed);
                        given |= 1 << at;
                    }
                }
                self.given.store(given, Ordering::Relaxed);
                KEPT_FAULT
            }
            Cause::Stack(stack) => {
                for (slot, word) in self.words.iter().zip(stack.words()) {
                    slot.store(word, Ordering::Relaxed);
                }
                match stack.check {
                    StackCheck::Guard => KEPT_STACK_GUARD,
                    StackCheck::Suspension => KEPT_STACK_SUSPENSION,
                }
            }
        };
        self.kind.store(kind, Ordering::Release);
    }

    fn kept(&self) -> Option<Cause> {
        let kind = self.kind.load(Ordering::Acquire);
        KeptImage {
            kind,
            given: self.given.load(Ordering::Relaxed),
            words: core::array::from_fn(|at| self.words[at].load(Ordering::Relaxed)),
            file: core::array::from_fn(|at| self.file[at].load(Ordering::Relaxed)),
        }
        .cause()
    }
}

/// Fatal entries taken this boot. It only ever counts up — outside host tests
/// nothing resets it — so a non-zero count always means a report is under way
/// or done.
static ENTRIES: AtomicU32 = AtomicU32::new(0);

/// What the boot's first fatal entry was taken for.
#[export_name = kept_symbol!()]
static FIRST: Kept = Kept::new();

/// What the entry taken after `taken` others may write, keeping `cause` when
/// it is the first.
fn entry_after(taken: u32, cause: &Cause) -> Entry {
    match taken {
        0 => {
            FIRST.keep(cause);
            Entry::Report
        }
        1 => Entry::Nested {
            first: FIRST.kept(),
        },
        _ => Entry::Silent,
    }
}

/// Enter the fatal-report path for `cause`.
///
/// Shared by every report path in the image, the kernel's post-mortem and the
/// port's own reports alike, so two of them can never both write a full
/// report.
pub fn enter(cause: &Cause) -> Entry {
    let taken = ENTRIES
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
            Some(taken.saturating_add(1))
        })
        .unwrap_or_else(|taken| taken);
    entry_after(taken, cause)
}

/// [`enter`], for a memory system that cannot perform an atomic
/// read-modify-write at the moment — aarch64 with stage-1 translation off,
/// where every access is Device-nGnRnE and an exclusive monitor may never
/// grant, so the retry loop behind [`enter`] would spin forever.
///
/// A plain load and store: exact while one processor reports. A processor
/// racing it through [`enter`] can at worst leave two full reports side by
/// side, never an unbounded recursion on either.
pub fn enter_without_atomics(cause: &Cause) -> Entry {
    let taken = ENTRIES.load(Ordering::Relaxed);
    ENTRIES.store(taken.saturating_add(1), Ordering::Relaxed);
    entry_after(taken, cause)
}

/// Return the latch to its boot state, so each host test that drives a report
/// starts from a first entry.
#[cfg(any(test, feature = "host-tests"))]
pub fn reset_for_tests() {
    ENTRIES.store(0, Ordering::Release);
    FIRST.kind.store(KEPT_NONE, Ordering::Release);
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

/// What the dispatcher's kernel-stack check found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StackCheck {
    /// The canary below the usable stack is disturbed: the task ran off its
    /// bottom.
    Guard,
    /// The task's saved stack pointer lies off its own stack.
    Suspension,
}

impl StackCheck {
    /// The record's one-word name for the check.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Guard => "guard",
            Self::Suspension => "suspension",
        }
    }
}

/// A task's kernel stack the dispatcher found corrupt. Memory beside it may
/// already be damaged, so the kernel stops rather than go on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StackCorruption {
    /// What the check found.
    pub check: StackCheck,
    /// The task whose stack it is.
    pub task: u64,
    /// The task's saved kernel stack pointer.
    pub task_sp: u64,
    /// The usable stack's lowest byte.
    pub base: u64,
    /// One past the usable stack's highest byte.
    pub top: u64,
}

/// Words a [`StackCorruption`] record carries: the task, its saved stack
/// pointer, and the usable stack's bounds.
const STACK_WORDS: usize = 4;

/// Fields a [`StackCorruption`] record carries: the check, then its words.
pub const STACK_FIELDS: usize = 1 + STACK_WORDS;

/// Most fields one cause carries.
pub const MAX_CAUSE_FIELDS: usize = max(max(PANIC_KEYS.len(), FAULT_WORDS), STACK_FIELDS);

/// Most words one cause spells in hex.
const CAUSE_WORDS: usize = max(FAULT_WORDS, STACK_WORDS);

const fn max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

/// The keys a fault's words are recorded under, in [`KernelFault`]'s order.
pub const FAULT_KEYS: [&str; FAULT_WORDS] = ["syndrome", "fault_addr", "fault_pc", "fault_sp"];

/// The keys a panic's location is recorded under: its file, line and column.
pub const PANIC_KEYS: [&str; 3] = ["file", "line", "column"];

/// The keys one cause's fields are recorded under.
struct CauseKeys {
    fault: [&'static str; FAULT_WORDS],
    panic: [&'static str; 3],
    check: &'static str,
    stack: [&'static str; STACK_WORDS],
}

/// A record's own cause.
const OWN_KEYS: CauseKeys = CauseKeys {
    fault: FAULT_KEYS,
    panic: PANIC_KEYS,
    check: "check",
    stack: ["task", "task_sp", "stack_base", "stack_top"],
};

/// The first entry's cause, as a bare record names it.
const FIRST_KEYS: CauseKeys = CauseKeys {
    fault: [
        "first_syndrome",
        "first_fault_addr",
        "first_fault_pc",
        "first_fault_sp",
    ],
    panic: ["first_file", "first_line", "first_column"],
    check: "first_check",
    stack: [
        "first_task",
        "first_task_sp",
        "first_stack_base",
        "first_stack_top",
    ],
};

/// Stack storage a cause's words are spelled into, so a fatal report
/// allocates nothing.
#[derive(Debug)]
pub struct CauseWords([[u8; HEX_WORD_LEN]; CAUSE_WORDS]);

impl CauseWords {
    /// Storage for one record's words.
    #[must_use]
    pub const fn new() -> Self {
        Self([[0; HEX_WORD_LEN]; CAUSE_WORDS])
    }
}

impl Default for CauseWords {
    fn default() -> Self {
        Self::new()
    }
}

/// `words` under `keys`, spelled into `bufs`: a word not given is `null`,
/// never a placeholder that reads as a value.
fn hex_fields<'b, const N: usize>(
    keys: &[&'static str; N],
    words: [Option<u64>; N],
    bufs: &'b mut CauseWords,
) -> [Field<'b>; N] {
    const { assert!(N <= CAUSE_WORDS) };
    let mut fields = [Field::UNSET; N];
    let named = keys.iter().copied().zip(words);
    for ((field, (key, word)), buf) in fields.iter_mut().zip(named).zip(&mut bufs.0) {
        *field = Field {
            key,
            value: word.map_or(FieldValue::Null, |value| {
                FieldValue::Str(format_hex_word(value, buf))
            }),
        };
    }
    fields
}

impl KernelFault {
    /// The four words in [`FAULT_KEYS`]' order.
    const fn words(&self) -> [Option<u64>; FAULT_WORDS] {
        [self.syndrome, self.address, Some(self.pc), self.sp]
    }

    /// The record's cause fields, spelled into `words`: a word the CPU did not
    /// give is `null`, never a placeholder that reads as a value.
    #[must_use]
    pub fn fields<'b>(&self, words: &'b mut CauseWords) -> [Field<'b>; FAULT_WORDS] {
        hex_fields(&FAULT_KEYS, self.words(), words)
    }
}

impl StackCorruption {
    /// The four words in the order the record names them.
    const fn words(&self) -> [u64; STACK_WORDS] {
        [self.task, self.task_sp, self.base, self.top]
    }

    /// The record's cause fields, spelled into `words`.
    #[must_use]
    pub fn fields<'b>(&self, words: &'b mut CauseWords) -> [Field<'b>; STACK_FIELDS] {
        self.fields_under(&OWN_KEYS, words)
    }

    fn fields_under<'b>(
        &self,
        keys: &CauseKeys,
        words: &'b mut CauseWords,
    ) -> [Field<'b>; STACK_FIELDS] {
        let [task, task_sp, base, top] = hex_fields(&keys.stack, self.words().map(Some), words);
        [
            Field {
                key: keys.check,
                value: FieldValue::Str(self.check.label()),
            },
            task,
            task_sp,
            base,
            top,
        ]
    }
}

impl fmt::Display for KernelFault {
    /// One line, in the field order and spelling the fatal record uses, so a
    /// prose report and the record read alike.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for (key, word) in FAULT_KEYS.iter().zip(self.words()) {
            match word {
                Some(value) => write!(f, "{separator}{key}={value:#018x}")?,
                None => write!(f, "{separator}{key}=null")?,
            }
            separator = " ";
        }
        Ok(())
    }
}

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
            nested(w, entry, KERNEL_PANIC, on, &Cause::panic(location));
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
        let [file_key, line_key, column_key] = PANIC_KEYS;
        record(
            w,
            KERNEL_PANIC,
            on,
            &[
                Field {
                    key: file_key,
                    value: FieldValue::Str(file),
                },
                Field {
                    key: line_key,
                    value: FieldValue::UnsignedInt(u64::from(line)),
                },
                Field {
                    key: column_key,
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
            nested(w, entry, KERNEL_FAULT, on, &Cause::Fault(*fault));
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
        let mut words = CauseWords::new();
        record(w, KERNEL_FAULT, on, &fault.fields(&mut words), guard);
    }

    /// Write the report of a task's corrupt kernel stack found on `on`,
    /// ending with its [`KERNEL_STACK_CORRUPT`] record, or the bare record or
    /// nothing as `entry` allows. `guard` is asked only for the full report.
    pub fn stack_corruption(
        self,
        w: &mut dyn Write,
        entry: Entry,
        on: Processor,
        corruption: &StackCorruption,
        guard: impl FnOnce() -> Option<BootStackGuard>,
    ) {
        if entry != Entry::Report {
            nested(
                w,
                entry,
                KERNEL_STACK_CORRUPT,
                on,
                &Cause::Stack(*corruption),
            );
            return;
        }
        let guard = guard();
        banner(
            w,
            "STACK CORRUPTION",
            on,
            format_args!(
                "{} task {:#x} failed its kernel-stack {} check on {on}",
                self.port,
                corruption.task,
                corruption.check.label()
            ),
            guard,
        );
        let mut words = CauseWords::new();
        record(
            w,
            KERNEL_STACK_CORRUPT,
            on,
            &corruption.fields(&mut words),
            guard,
        );
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

/// Write `kind`'s bare record for a nested entry taken for `own`; a silent
/// one writes nothing.
fn nested(w: &mut dyn Write, entry: Entry, kind: FatalRecord, on: Processor, own: &Cause) {
    if let Entry::Nested { first } = entry {
        let mut words = NestedWords::new();
        let fields = nested_fields(on, own, first.as_ref(), &mut words);
        write_diag_line(
            w,
            None,
            false,
            &Event {
                level: Level::Error,
                id: kind.id,
                message: kind.nested,
                fields: fields.as_slice(),
            },
        );
    }
}

/// Fields a bare record can carry: the processor, the re-entering cause, and
/// the first entry's kind and cause.
pub const NESTED_FIELDS: usize = 1 + MAX_CAUSE_FIELDS + 1 + MAX_CAUSE_FIELDS;

/// Stack storage a bare record's words are spelled into, so it allocates
/// nothing.
#[derive(Debug, Default)]
pub struct NestedWords {
    own: CauseWords,
    first: CauseWords,
}

impl NestedWords {
    /// Storage for one bare record's words.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            own: CauseWords::new(),
            first: CauseWords::new(),
        }
    }
}

/// A bare record's fields, as [`nested_fields`] spells them.
#[derive(Debug)]
pub struct NestedFields<'b> {
    fields: [Field<'b>; NESTED_FIELDS],
    len: usize,
}

impl<'b> NestedFields<'b> {
    /// The fields, in the order the record carries them.
    #[must_use]
    pub fn as_slice(&self) -> &[Field<'b>] {
        &self.fields[..self.len]
    }

    fn push(&mut self, field: Field<'b>) {
        if let Some(slot) = self.fields.get_mut(self.len) {
            *slot = field;
            self.len += 1;
        }
    }

    fn push_cause(&mut self, cause: &'b Cause, keys: &CauseKeys, words: &'b mut CauseWords) {
        match cause {
            Cause::Fault(fault) => {
                for field in hex_fields(&keys.fault, fault.words(), words) {
                    self.push(field);
                }
            }
            Cause::Stack(stack) => {
                for field in stack.fields_under(keys, words) {
                    self.push(field);
                }
            }
            Cause::Panic {
                file,
                len,
                line,
                column,
            } => {
                let tail = file.get(..usize::from(*len)).unwrap_or(&[]);
                let [file_key, line_key, column_key] = keys.panic;
                self.push(Field {
                    key: file_key,
                    value: match core::str::from_utf8(tail) {
                        Ok(tail) if !tail.is_empty() => FieldValue::Str(tail),
                        _ => FieldValue::Null,
                    },
                });
                self.push(Field {
                    key: line_key,
                    value: FieldValue::UnsignedInt(u64::from(*line)),
                });
                self.push(Field {
                    key: column_key,
                    value: FieldValue::UnsignedInt(u64::from(*column)),
                });
            }
        }
    }
}

/// The fields of the bare record an entry on `on` for `own` writes when a
/// report is already under way: the processor, what re-entered, and, under
/// `first`, what the first entry was taken for, so a report that died inside
/// itself still names the machine's cause beside what broke the report.
pub fn nested_fields<'b>(
    on: Processor,
    own: &'b Cause,
    first: Option<&'b Cause>,
    words: &'b mut NestedWords,
) -> NestedFields<'b> {
    let mut fields = NestedFields {
        fields: [Field::UNSET; NESTED_FIELDS],
        len: 0,
    };
    fields.push(on.field());
    fields.push_cause(own, &OWN_KEYS, &mut words.own);
    fields.push(Field {
        key: "first",
        value: first.map_or(FieldValue::Null, |first| FieldValue::Str(first.kind())),
    });
    if let Some(first) = first {
        fields.push_cause(first, &FIRST_KEYS, &mut words.first);
    }
    fields
}

/// Fields a record can carry: the processor, the widest cause's fields, and
/// the guard's verdict and overrun depth.
const RECORD_FIELDS: usize = 1 + MAX_CAUSE_FIELDS + 2;

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
    let mut fields = [Field::UNSET; RECORD_FIELDS];
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
