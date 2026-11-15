# RISC OS Emulator

`RISCOSEmulator.app` is to run RISC OS applications on the TAIRiX desktop.
Its staged plan, with the design every part follows, is
`plans/RISCOS-EMULATOR.md`. The engine is the `userland/apps/riscos` subtree;
this page describes the parts built so far.

## The processor

`tairix-riscos-arm` (`userland/apps/riscos/arm`) is the ARM processor. It
knows nothing of RISC OS: the session above it decides what its memory, its
SWIs and its host calls mean.

### The tables

Each instruction set has one table, and everything that needs its encodings
is derived from it. Each row is a pattern that spells every bit of the
encoding: a fixed bit, a should-be bit, or a bit of a named field.

- `a32` holds every A32 integer and system encoding from ARMv2 to ARMv7-A,
  with ARMv8-A's AArch32 integer additions (load-acquire and store-release,
  `SEVL`, `CSDB`, `SSBB`, `PSSBB`, `CRC32`).
- `t32` holds Thumb's 16-bit encodings from ARMv4T and Thumb-2's 32-bit
  encodings from ARMv6T2, through the same additions. Its encodings cut
  across one another, a register field holding the PC naming another
  instruction, so its rows may overlap and the first match decides.

The floating-point and Advanced SIMD encodings are RO11's and RO12's. From
the tables:

- `a32::decode(word, isa)` and `t32::decode(first, second, isa, position)`
  give an `Instruction`, or `None` where the bits are undefined on that
  instruction set. A T32 instruction's condition comes from the `IT` block it
  stands in, which also decides whether a 16-bit operation sets the flags, so
  its decoding takes its `t32::Position`;
- `a32::encode(&instruction)` and `t32::encode(&instruction, position, size)`
  give back an encoding that decodes to it, and refuse operands no encoding
  holds. T32's `size` asks for the narrowest encoding, or for 16 or 32 bits
  as UAL's `.N` and `.W` do; an encoding whose effect is defined is preferred
  to one the architecture leaves UNPREDICTABLE where it stands;
- `Instruction::text(set, address)` spells it as BBC BASIC's assembler and the
  Debugger do: the condition before an older mnemonic's suffix (`LDREQB`,
  `ADDEQS`), stack mnemonics on R13 (`STMFD R13!,{R0-R3,R14}`), `&` for hex;
- `a32::unpredictable(&instruction, isa)` says whether the architecture leaves
  an A32 encoding's effect UNPREDICTABLE, and `t32::Decoded::unpredictable`
  the same of a T32 one where it stands in an `IT` block;
- the interpreter dispatches on the decoded `Instruction`.

A word whose should-be bits are not their required values still decodes, as
an UNPREDICTABLE one; the encoder writes them canonically.

T32's UNPREDICTABLE rules are ARMv8-A's, which permit R13 where ARMv7 forbade
it: executing such an encoding is among the behaviours ARMv7 allowed.

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
- **Thumb state.** The PC reads as the instruction's address plus 4, and
  word-aligned for literal addressing and `ADR`. An exception is taken in ARM
  state, its SPSR holding T, and returns to the Thumb code: an undefined
  instruction's return is its address plus 2, a breakpoint's plus 4.
- **Writes to the PC** interwork as the architecture version defines: `BX`
  and `BLX` always, loads from ARMv5, data processing from ARMv7 in ARM state
  and never in Thumb state.

### Running

`cpu::Cpu::run(bus, budget)` steps through a `bus::Bus`, which fetches, reads
and writes naturally aligned units with a given privilege and may refuse any
of them. A step takes a pending interrupt, fetches, and executes: an A32 word,
or in Thumb state one or two halfwords, the second fetched from the next word
when the first ends one. A run stops for:

- the budget;
- a `SWI`, which the session performs or enters through `Cpu::enter`;
- a `UDF` on a host page in ARM state, which calls the host;
- `WFI` or `WFE` with nothing to wake it;
- a CP15 register the processor does not model, before it executes.

An A32 instruction in the unconditional space executes; its condition field
means never only where the profile's architecture says so.

### IT blocks

`IT` gives up to four following Thumb instructions their condition. The
state lives in the CPSR's IT bits: each instruction executed, its condition
passing or not, moves it on, except `IT` itself, exception entry and an
exception return, which write it. A `SWI` in a block moves it on before it
stops, so the SPSR its vector is entered with holds the block's next
instruction. An exception return restores an IT state only into Thumb state,
and never a reserved one, as ARMv8 defines it. Where the profile takes UNPREDICTABLE encodings as undefined, so
it takes the block's own: an instruction forbidden in a block, a write to the
PC before the block's last instruction, and a condition no `IT` gives.

Every other exception is taken through the profile's vectors: undefined
instructions (unclaimed coprocessors among them), prefetch and data aborts
with the fault recorded in CP15, IRQ and FIQ from `set_irq` and `set_fiq`.
The exclusive monitor is single-core and every exception clears it.

### Decoded pages

Each instruction's decoding is kept in a slot of its guest page, beside the
bits it was decoded from: a word for A32, and for T32 its halfwords and where
it stands in an `IT` block. A fetch always reads memory, and a slot is used
only while what was fetched there still matches, so rewritten code, a
remapped page or a place shared with another page simply decodes again:
nothing has to announce a change. Each instruction set's pages held are
bounded by a count the session gives, and `Cpu::release_memo` gives their
memory back.

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
suite (MIT), which the NanoBoyAdvance emulator generated. Its ARM-state files
are held to `BIT26`, an ARMv4 profile; its Thumb files and `BX` to `BIT32`,
whose ARMv7 defines what ARMv4T does wherever ARMv4T defines it. A vector is
kept where the older architecture defines what it checks and the profile
chooses the same: not where the encoding, the mode or the data makes the
result UNPREDICTABLE (a misaligned halfword, a branch to an unaligned address,
a write of an invalid mode), not where it stores R15 (the ARM7TDMI stores
PC+12), not a coprocessor the profile has, and not the carry and overflow a
multiply leaves. On `BIT32` neither is a misaligned access, which faults
there, nor where ARMv4T and ARMv7 part: `BLX` by a register, a load to the PC
selecting ARM state, and a Thumb block transfer naming its base. The
conditional branch with the condition `1110`, which ARMv4T leaves undefined
and the suite runs as a branch, is not replayed. Every kept vector passes:
about 730,000 on `BIT26` and 840,000 on `BIT32`. `--write-sample` writes the
samples the crate's test replays in `ci`, 2,432 for `BIT26` and 2,944 for
`BIT32`, with the suite's licence and the record of what was kept beside
them. Each instruction class has unit tests, and `fuzz_arm` fuzzes both
decoders and encoders and the interpreter in either state.
