# RISC OS Emulator

`RISCOSEmulator.app` is to run RISC OS applications on the TAIRiX desktop.
Its staged plan, with the design every part follows, is
`plans/RISCOS-EMULATOR.md`. The engine is the `userland/apps/riscos` subtree;
this page describes the parts built so far.

## The processor

`tairix-riscos-arm` (`userland/apps/riscos/arm`) is the ARM processor. It
knows nothing of RISC OS: the session above it decides what its memory, its
SWIs and its host calls mean.

### One table

`a32` holds every A32 integer and system encoding from ARMv2 to ARMv7-A, with
ARMv8-A's AArch32 integer additions (load-acquire and store-release, `SEVL`,
`CSDB`, `CRC32`), in one table; the floating-point and Advanced SIMD encodings
are RO11's and RO12's. Each row is a pattern that spells all 32 bits: a fixed bit, a
should-be bit, or a bit of a named field. Everything that needs the encodings
is derived from it:

- `a32::decode(word, isa)` gives an `Instruction` (`None` when the word is
  undefined on that instruction set);
- `Instruction::encode` gives back the one word that decodes to it, and
  refuses operands no encoding holds;
- `Instruction::text(address)` spells it as BBC BASIC's assembler and the
  Debugger do: the condition before an older mnemonic's suffix (`LDREQB`,
  `ADDEQS`), stack mnemonics on R13 (`STMFD R13!,{R0-R3,R14}`), `&` for hex;
- `Instruction::unpredictable(isa)` says whether the architecture leaves the
  encoding's effect UNPREDICTABLE;
- the interpreter dispatches on the decoded `Instruction`.

A word whose should-be bits are not their required values still decodes, as
an UNPREDICTABLE one; the encoder writes them canonically.

### Profiles

A `profile::Profile` fixes every choice the architecture leaves open. There
are two:

| | `BIT26` | `BIT32` |
|---|---|---|
| Architecture | ARMv4, as the SA-110 | ARMv7-A with ARMv8-A's AArch32 additions |
| Modes | the 26-bit modes and the 32-bit ones | the 32-bit ones |
| Misaligned access | a word load rotates, a store ignores the low bits | an alignment fault |
| `STR` of R15 | the instruction's address plus 8 | the same |
| `SWP` | yes | no, as ARMv8 removed it |
| Condition `1111` | never | the unconditional instructions |
| Vectors | at zero | at `&FFFF0000` |
| UNPREDICTABLE | executes as a core without a pipeline would | the undefined instruction exception |
| CP15 | the SA-110's | an ARMv7 processor's, configured as RISC OS 5 runs it |

Both take exceptions in the 32-bit modes and restore an aborted instruction's
base, as the SA-110 does.

### The programmer's models

- **26-bit.** R15 holds the flags, I, F and the mode beside the word PC, which
  wraps within 64 MiB. As a second operand or a stored value R15 carries its
  PSR bits; as a first operand or a base it does not. `TEQP` and its kin
  write those bits from their result, `MOVS PC,…` and `LDM …^` with R15
  restore them, and `BL` keeps them in R14. In User mode only the flags
  change.
- **32-bit.** The CPSR, an SPSR per exception mode, and the banks of FIQ,
  IRQ, Supervisor, Abort and Undefined mode; System shares User's.
  `cpu::State` is the register file as the hardware holds it, every bank
  together, and loads back as itself.
- **Writes to the PC** interwork as the architecture version defines: `BX`
  and `BLX` always, loads from ARMv5, data processing from ARMv7.

### Running

`cpu::Cpu::run(bus, budget)` steps through a `bus::Bus`, which fetches, reads
and writes naturally aligned units with a given privilege and may refuse any
of them. A step takes a pending interrupt, fetches, and executes. A run stops
for:

- the budget;
- a `SWI`, which the session performs or enters through `Cpu::enter`;
- a `UDF` on a host page, which calls the host;
- `WFI` or `WFE` with nothing to wake it;
- something the processor does not model, which has not executed: a CP15
  register it does not model, or Thumb state, which RO10 brings.

Every other exception is taken through the profile's vectors: undefined
instructions (unclaimed coprocessors among them), prefetch and data aborts
with the fault recorded in CP15, IRQ and FIQ from `set_irq` and `set_fiq`.
The exclusive monitor is single-core and every exception clears it.

### Decoded pages

Each word's decoding is kept in a slot of its guest page, beside the word it
was decoded from. A fetch always reads memory, and a slot is used only while
the word fetched there still matches, so rewritten code, a remapped page or a
place shared with another page simply decodes again: nothing has to announce
a change. The pages held are bounded by a count the session gives, and
`Cpu::release_memo` gives their memory back.

### CP15

The SA-110's identity and control register; an ARMv7 processor's identity
(an implementer of zero, reserved for software), its feature registers as the
profile's instruction set has them, the fault and thread registers.

- Cache and TLB maintenance completes: no cache is modelled, so nothing is
  stale.
- The cache enables may change; any other change to the control register is
  refused.
- The registers that would program a guest MMU (the translation tables, the
  domains, the memory attributes, address translation) stop the run, named,
  since translation is the bus's.
- From User mode only the thread registers and the CP15 barriers are
  reachable.

### Conformance

`cargo xtask riscos-vectors <dir>` replays the `SingleStepTests` ARM7TDMI
suite (MIT), which the NanoBoyAdvance emulator generated, on `BIT26`. A
vector is kept where ARMv4 defines what it checks: not where the encoding,
the mode or the data makes the result UNPREDICTABLE (a misaligned halfword, a
branch to an unaligned address, a write of an invalid mode), not where it
stores R15 (the ARM7TDMI stores PC+12), not `BX`, not a coprocessor the
profile has, and not the carry and overflow a multiply leaves. All of the about 730,000 kept vectors pass.
`--write-sample` writes the 2,432 the crate's test replays in `ci`, with the
suite's licence and the record of what was kept beside them. Each instruction
class has unit tests, and `fuzz_arm` fuzzes the decoder, the encoder and the
interpreter.
