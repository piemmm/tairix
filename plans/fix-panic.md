# fix-panic — the intermittent x86_64 kernel fault hunt

Binding under `AGENTS.md`. This file records everything known about an
intermittent kernel fault on `x86_64` so no evidence is lost between sessions.
It is a hunt record, not a design: it states the symptom, the captured
evidence, where the capture files live, how to reproduce, the leads, and the
remaining fix/verify steps.

## Status ledger

| id | what it is | status |
|---|---|---|
| FP1 | Root-cause the intermittent `x86_64` kernel fault (iret to `RIP=0x3`) | done — the direction-flag entry-hygiene bug (see "Root cause") |
| FP2 | Land the structural fix with a regression test that fails before it | done — `clear_direction_flag!` in every async-entry stub + `stub_align_tests` |
| FP3 | Service kthreads run on a software-guard `BoxStack`, not a hardware-guarded kernel stack | refuted as cause (`boot_stack_guard=intact`); not a defect — the poison guard works |
| FP4 | A tripped stack/poison guard fails closed silently instead of reporting | observation to raise, not a cause; left for the user (not expanding a bugfix into it) |
| FP5 | The boot root (`CR3=0x102000`) identity-maps low RAM under the kthreads that run on it | refuted as cause; boot identity map, not a loadable segment — raise separately if at all |

FP3–FP5 were *hypotheses* formed while hunting FP1, before the cause was known.
The confirmed cause is FP1's direction-flag bug; the evidence refutes FP3 and
FP5 as the mechanism (the stacks were never overrun — `boot_stack_guard=intact`
— and the corruption is a backward string op, not a boot-map access). They are
recorded honestly here rather than silently dropped; none is a confirmed defect
this fix must carry, and FP4 (fail-loud on a guard trip) is noted for the user
to weigh rather than folded into a kernel bugfix by guesswork.

## Root cause (FP1)

`compiler_builtins`' x86_64 `copy_backward` (the backward half of `memmove`)
runs `std` … `rep movs` … `cld`, holding **`DF=1`** for that window. x86
interrupt, exception, and trap-gate delivery do **not** clear `DF` (only
`syscall` does, via `IA32_FMASK`). The five kernel entry stubs that `call` into
a Rust dispatcher did not clear it either, so an interrupt landing inside that
window entered Rust with `DF=1`. The compiler emits `memcpy`/`memset` as
`rep movs`/`rep stos`, which read `DF` for direction — so the dispatcher's own
string ops ran **backward**, overwriting the interrupted stack, including the
saved `RIP` word of the interrupt frame. The corrupted frame's `iretq` then
resumed at `RIP=0x3`; a later stray backward copy left the sticky `CR2` at a
user-looking address. Intermittent because the interrupt must fall inside the
brief `std`…`cld` window — matching ~1 failure in 4–6 boots.

The fix clears `DF` on every asynchronous entry before calling Rust (the
System V AMD64 ABI's `DF=0`-at-entry contract, the reason Linux `cld`s on
entry). The interrupted context's own `DF` is untouched — `iretq` restores it —
so a `copy_backward` interrupted mid-window resumes correctly.

## Symptom

In the `x86_64` QEMU matrix (`cargo xtask test --qemu --only x86`), roughly one
run in 4–6 (each pass ~4 min) ends with one vertical dying at the same boot
point: after `init`'s 4th `call_create`, during the root-unlock kthread's
`/System` bring-up, running on a `BoxStack` in the static kernel `HEAP`.

It surfaces in three shapes, all the same underlying corruption:

- a kernel `#UD` (invalid opcode) with `RIP=0x3`;
- a nested-fault silent halt (the first fault's report path itself faults);
- a `#PF` at a non-canonical/high address with a core-library panic location.

The vertical that shows it is not fixed — `supervisor-esc` and `autoload-input`
have both been the one to die; it is whichever x86 vertical happens to hit the
window.

## Captured evidence (do not delete)

Three capture directories under `/tmp`, each with `run.log`, every vertical's
`*.serial.log`, and the failing vertical's `*.cpu.txt` (register dump + 32 KiB
stack dump via the QEMU monitor):

- `/tmp/capture-supervisor-esc/` — the run where the **fatal report printed**
  (id=4011). The harness line records the decoded fault:
  - `fault_pc=0x0000000000000003`, `fault_addr=null`,
    `syndrome=0x0000000600000000`, `fault_sp=0xffffffff811b9a88`,
    `root=0x102000`, `boot_stack_guard=intact`.
  - reporting-context regs: `pc=0xffffffff80540922` (the exception entry path),
    `rsp=0xffffffff811b6210`, `rbp=0xffffffff811b68f0`.
- `/tmp/capture-autoload-hang/` — a **hung** run (`autoload-input`), `*.cpu.txt`
  taken after the guest fell silent. Parked in `park_cpu` after a nested fault.
- `/tmp/hunt-5/` — a **hung** run (`supervisor-esc`) with the fullest dump:
  32 KiB stack, two `#UD` exception frames. This is the richest capture.

### `/tmp/hunt-5` register state (CPU#0, halted in `park_cpu`)

```
RIP=ffffffff8053eed1  (tairix_arch_x86_64::reset::park_cpu)
RFL=00000446 [D--Z-P-]   CPL=0  HLT=1
RSP=ffffffff811d52f0  RBP=ffffffff811d52f0
R12=0000000000000005
CR0=80000033  CR2=0000001800001000  CR3=0000000000102000  CR4=00000620
```

Key facts across all three captures:

- **`CR2=0x0000001800001000`** every time — a *user* linear address, accessed
  while **`CR3=0x102000`** (the boot root, which has no user mappings). A kernel
  access to a user address under the boot root.
- **`RIP`/`fault_pc` = 3** — the CPU was made to resume at linear address 3.
  With the boot root identity-mapping low RAM, address 3 is *mapped* (so the
  fetch gives `#UD` on the bytes there, not `#PF`).
- The pushed exception frame in the stack dump decodes as
  `RIP=3, CS=8, RFLAGS=0x10402 (+RF), RSP=<kernel stack>` with
  `[RSP-8]=0` — i.e. the CPU reached `RIP=3` via an **`iretq` through a
  corrupted/misdirected frame**, not a `call`/`ret`.

### `/tmp/hunt-5` symbolised stack (against the capture's own image)

Image: `target/x86_64-tairix-none/debug/tairix-test-supervisor-esc-qemu-x86-64`
(not rebuilt since the capture; addresses still resolve). Return addresses up
the dump, innermost first:

```
park_cpu                                   reset.rs:51      (final halt)
panic::enter_report::<BinArch>             panic.rs:787
panic::dump::<BinArch>                      panic.rs:801
panic::panic_dump::<BinArch>                panic.rs:381
fatal_bridge::report_kernel_fault           (report path)
exceptions::exception_routes                exceptions.rs:340
tairix_arch_x86_64_exception_dispatch
isr_invalid_opcode                          (the #UD vector)
panic::report_nested                        panic.rs:722     <-- nested: a
KernelFault::fields_under                   fatal.rs:397          second fault
...                                                               during the
                                                                  first report
```

Below the nested #UD frame the dump holds an **earlier** frame cluster around
`0xffffffff811db248` whose return addresses walk waitq / btree / irq-timer
code:

```
init::SchedWaitQueueArch::now_ns            init.rs:534
IrqTable::now_ns                            table.rs
apic_timer::Calibration::tsc_ticks_to_ns    apic_timer.rs:156
btree::node::slice_remove / OccupiedEntry::remove_kv   (waitq WakeKey map)
RawTable::probe (HashMap<u64,u32>)          raw.rs:278
```

i.e. the original (pre-corruption) activity was wait-queue / timer bookkeeping
(a `WakeKey` btree map and a `HashMap<u64,u32>`) on the kthread, consistent with
the root-unlock bring-up parking/waking.

## Reproduction

`/tmp/mut/hunt-x86.sh N` loops `cargo xtask test --qemu --only x86` up to N
times; on the first non-zero exit it copies `run.log` and every
`*.serial.log` / `*.cpu.txt` into `/tmp/hunt-<i>/` and stops. ~4 min/pass,
~1 failure per 4–6 passes. Kill stray `qemu-system-*` before looping.

## Leads

1. **The `iretq`-to-`RIP=3` mechanism.** The pushed frame's `RIP` word was
   overwritten with 3 (and `[RSP-8]=0`), so an exception/interrupt *return* sent
   the CPU to 3. Either the interrupt frame on the kthread stack was overwritten
   (stack overrun past a software-only guard — see FP3), or a dispatch path
   wrote the wrong `rip_slot`. The kernel-mode `#PF` → uaccess-fixup path
   (`fault.rs` `tairix_arch_x86_64_page_fault_dispatch`, which rewrites
   `*rip_slot`) is the one place the kernel deliberately rewrites a frame's RIP;
   it must be ruled in or out.

2. **Kernel access to a user address under the boot root** (`CR2=0x1800001000`,
   `CR3=0x102000`). A kthread or syscall body touching user memory without the
   process root loaded would `#PF` on an unmapped user address; the boot root
   has no user mappings, so the demand-paging resolver cannot satisfy it and the
   path must fail closed. `DF=1` in one capture suggests a backward `rep movs`
   (a `memmove`) was mid-flight at the fault.

3. **Direction-flag / entry-state hygiene.** The exception/interrupt entry
   stubs in `kernel/arch/x86_64/src` were searched for `cld`; only the AP
   trampoline (`ap_trampoline.s`) has one. Whether the C-ABI dispatchers the
   stubs `call` can run with `DF` set (the SysV ABI requires `DF=0` on entry to
   a function) is open and must be checked against the stub macros
   (`define_isr!`, the `#PF` stub, `scrub_nonargument_gprs!`,
   `iret_clearing_for_ring3!`). This is a lead, **not** a confirmed cause.

## Instrumentation already landed (kept)

The fatal latch now captures the *first* cause before any nested fault can
overwrite it, so a nested fault's report names both its own cause and the first:

- `kernel/arch/api/src/fatal.rs`: `Cause` enum (`Panic{file,len,line,column}` /
  `Fault(KernelFault)`), the `Kept`/`FIRST` atomics, `enter(cause)`,
  `nested_fields(...)`, `FILE_TAIL`.
- `kernel/core/src/panic.rs`: `Fatal::cause()`, `enter_report` records the
  first cause, `report_nested` emits own + first-cause fields.
- Ports (`kernel/arch/{x86_64,aarch64,riscv64}/src/panic.rs`) feed `Cause` in.
- Harness (`tools/qemu/src/lib.rs`): `Outcome::Fatal{record,serial,cpu_state}`,
  `cpu_report(...)` dumps each core's registers + a 32 KiB stack via the QEMU
  monitor on a fatal/hung stop; `tools/xtask/src/commands/qemu_tests.rs` writes
  the `*.cpu.txt` sidecar; `tools/ci/lib.sh` collects it.
- Audit rows 4010/4011 and `docs/src/architecture/panic-diagnostics.md` updated.

## Fix landed

- `kernel/arch/x86_64/src/sidechannel.rs`: new `clear_direction_flag!` template
  macro (emits `cld`), sibling to `scrub_nonargument_gprs!`, with the rationale.
- Spliced into every stub entered by interrupt/exception/trap-gate delivery,
  after the saves and before the Rust call: `define_isr!` and
  `exception_isr_body!` and `tairix_arch_x86_64_isr_default` (interrupts.rs),
  `page_fault_isr` (fault.rs), `tairix_arch_x86_64_external_irq_common`
  (irq.rs). The timer/preemption ISR is a `define_isr!` instance, so it is
  covered. The `syscall` stub is deliberately untouched (`IA32_FMASK`).
- Regression tests in `kernel/arch/x86_64/src/stub_align_tests.rs`:
  `every_asynchronous_entry_stub_clears_the_direction_flag_before_rust`
  (fails before the fix — no stub contained `cld`) and
  `the_syscall_stub_relies_on_fmask_for_the_direction_flag`.

## Remaining work

- The single whole-project gate (`AGENTS.md` §7) over the full tree, then the
  completion report with the adversarial and performance reviews and the oracle
  statement. An extended `/tmp/mut/hunt-x86.sh` loop can confirm non-recurrence,
  but the fix is structurally complete and deterministically tested.
