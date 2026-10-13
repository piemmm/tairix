# Side-channel mitigations (Arch HAL)

Microarchitectural side channels — Meltdown, Spectre v1/v2, MDS, L1TF,
MMIO stale data — are defeated by mechanisms only an architecture port can
emit, each at one privilege boundary: kernel/user address-space separation
(KPTI-equivalent), speculation hardening on every user→kernel entry, a
buffer clear and a speculation barrier on every return to user mode, and an
indirect-branch-predictor barrier on context switch. `AGENTS.md` §19.1
makes this a closed set on the Arch HAL, in `kernel/arch/api`.

## The surface

`tairix_arch_api::sidechannel` defines the port's declaration, not the
mechanisms. A mechanism's position relative to the transition instruction
is what makes it a defence — a buffer clear must be the last kernel work
before the return, a straight-line-speculation barrier must follow it — so
each lives in the port's entry and return code, where no Rust call can
stand:

- `SideChannelMitigation` — the per-port handle; its one method returns the
  port's `profile`.
- `MitigationProfile` — one `Mitigation` per control:
  `address_space_isolation`, `user_entry_hardening`,
  `user_return_buffer_clear`, `user_return_speculation_barrier`, and
  `context_switch_indirect_branch_barrier`.
- `Mitigation` — one of three honest positions:
  - `Applied` — the port's code carries it on every path it covers.
  - `NotVulnerable(reason)` — a no-op, permitted **only** where the silicon
    is provably not exposed, with the justification recorded in the
    port's source.
  - `Pending(note)` — the silicon requires it but the port does not build
    it yet. Honest and tracked, but **not** release-ready.

`MitigationProfile::validate` refuses an unexplained non-applied slot;
`MitigationProfile::is_release_ready` additionally refuses any `Pending`.

## The conformance vertical and the source pins

`tairix_arch_api::sidechannel::conformance::run_all` runs on every port
(from its `passes_arch_hal_conformance_suite` host test) and asserts the
profile is honest. A declaration is only as good as the code behind it, so
each port with an `Applied` slot also carries `sidechannel_tests`, which
pins every applied mechanism onto every entry and return path in the
port's own sources: a profile that says `Applied` fails while any path
lacks the code. The entry and return stubs are assembly no host test can
run, so the pins read the sources the stubs are built from, as the ports'
other stub pins (`stub_align_tests`, `eret_tests`, `sret_tests`) do.

## Per-target declarations

| Mitigation | x86_64 | aarch64 | riscv64 | wasm32 |
| --- | --- | --- | --- | --- |
| Address-space isolation (KPTI) | Pending | Pending | NotVulnerable (in-order) | NotVulnerable (host-owned) |
| User-entry hardening | Applied (register scrub, fenced GS swaps) | Applied (register scrub) | NotVulnerable (in-order) | NotVulnerable (host-owned) |
| Return-to-user buffer clear | Applied (`verw`) | NotVulnerable (Intel-only) | NotVulnerable (Intel-only) | NotVulnerable (host-owned) |
| Return-to-user speculation barrier | Applied (`int3`) | Applied (`dsb nsh; isb`) | NotVulnerable (in-order) | NotVulnerable (host-owned) |
| Context-switch IBP barrier | Pending (CPUID/IBPB) | Pending (MIDR/SMCCC) | NotVulnerable (in-order) | NotVulnerable (host-owned) |

- **x86_64.** Every entry stub zeroes the general-purpose registers it
  saved before its first call into Rust, keeping only the System V
  argument registers it loads; the syscall stub passes its trampoline a
  pointer to the saved frame rather than the user's `rip` and `rbp`. The
  two GS swaps that depend on the interrupted privilege are followed by
  `lfence` (the SWAPGS variant). `verw` against the kernel data selector is
  the last instruction before every `sysretq` and every `iretq` to ring 3,
  and an `int3` follows each return. KPTI needs per-process user
  translation roots mapping only the entry trampolines; IBPB needs the
  `CPUID.7.0:EDX[26]` probe and a context-switch hook. Kernel-mode branch
  target injection (IBRS/eIBRS) and speculative store bypass (SSBD) are
  `plans/ARCHSUPPORT.md` A8.
- **aarch64.** The trap trampoline zeroes `x2`–`x28` once saved (`x0`/`x1`
  carry the kind and frame, the forced frame pointers make the handler's
  prologue replace `x29`, its `bl` replaces `x30`), and `dsb nsh; isb`
  follows every `eret` — the portable form of `sb`, which `FEAT_SB` adds
  only from Armv8.5. The buffer clear is a justified no-op; KPTI (for a
  Meltdown-exposed core) and the MIDR-specific Spectre-v2 sequence are
  `Pending`.
- **riscv64.** The cores TAIRiX targets (QEMU `virt`, SiFive U54/U74)
  execute in order, so no instruction runs speculatively past a fault, a
  mispredict or a return: every slot is a justified no-op, and the port is
  release-ready. A memory-ordering `fence` would stop no speculation, so
  none is emitted in a speculation slot's name.
- **wasm32** delegates every microarchitectural defence to the
  Chrome-class host (site isolation, timer clamping, COOP/COEP) and
  isolates memory with one linear memory per worker. Every control is a
  justified host-owned no-op; the port is release-ready.

## User-chosen indices

A value a user chooses that the kernel checks against a table and then
loads from it is a Spectre v1 gadget: the core may run the load before the
check resolves. Every such index on the syscall path is clamped without a
branch as well as checked, through `tairix_abi::nospec::index_within` — the
`array_index_mask_nospec` construction behind two optimisation barriers,
which keep it a plain arithmetic data dependency, never folded away by the
compiler and never lowered to a `csel`/`cmov` whose flags an Arm core may
predict. Its users are the syscall number (`syscalls::spec_for`), a
capability id (`CapabilityId::index`, which every capability-summary bit
goes through), and a standard descriptor (`process::DescriptorTable`), whose
console byte would otherwise feed a second, dependent load. `BitSet256`,
the capability set's storage, sits below `lib/abi` and masks its
power-of-two index behind the same kind of barrier. The kernel's other
user-named tables — ports, open files, capability records — are B-trees,
which reach no slot by a raw index.

## Untrusted timer resolution

A high-resolution clock is the *measurement* half of every timing side
channel, so §19.1 coverage is not only about speculation barriers — it
also bounds how precisely untrusted code can tell the time. The
`clock_get` syscall is unprivileged by design (any task may read the
monotonic clock), so the precision is gated on a capability rather than
the call: a caller holding `CAP_TIME_HIRES` reads the raw nanosecond
value, and every other caller — including the §19.5 parser sandboxes
and untrusted `userland/apps` — reads it floored to
`COARSE_CLOCK_GRANULARITY_NS` (one microsecond), the single coarsening
constant in `lib/abi::time`. The flooring is value-only, so the frozen
`clock_get` ABI signature is untouched, and `coarsen_clock_ns` keeps the
reading monotonically non-decreasing. See
[the syscall reference](../architecture/syscalls.md#clock-resolution-and-side-channels).

## TSC as a validated clocksource (x86_64)

The x86_64 monotonic clock is the Time-Stamp Counter. `RDTSC` is only a
sound *cross-CPU* time base on a part that advertises an **Invariant
TSC** (constant rate across P-/C-/T-states, synchronised across cores);
without it a scheduler-migrated task could observe time moving
backwards. The boot path therefore does not silently trust the contract:
`tairix_arch_x86_64::tsc::detect_invariant_tsc` reads the CPUID
Invariant-TSC flag (leaf `0x8000_0007` EDX bit 8) inside the arch crate
(§17.2), the result is logged on every boot
(`KERNEL_BOOT_TSC_INVARIANCE`), and the kernel **fails closed**
(`BootError::TscNotInvariant`) before bringing up a second CPU on a part
that lacks it. A single-CPU boot proceeds regardless, because one TSC is
inherently self-monotonic. The frequency itself is never taken from
CPUID or firmware: it is measured empirically against the PIT
calibration window (`apic_timer::calibrate`).

## Remaining §19.1 work

KPTI on x86_64 and aarch64, the indirect-branch-predictor barriers, and
x86_64's IBRS/STIBP/SSBD (`plans/ARCHSUPPORT.md` A8).

The §19.1 constant-time requirement for `lib/crypto`'s secret-handling
code is **landed**: `tairix_crypto::ct_eq` compares secret byte strings
in content-independent time, and its tests prove the no-early-exit
property without the wall-clock timing `AGENTS.md` §7 forbids — an
instrumented iterator asserts that equal, first-byte-differing,
last-byte-differing, and all-differing inputs all traverse the full
length. `cargo xtask ci` re-runs the `tairix-crypto` tests under the
release profile (`-C opt-level=3`) so an optimiser-introduced branch
would fail the gate. See [`tairix-crypto`](../lib/crypto.md).
