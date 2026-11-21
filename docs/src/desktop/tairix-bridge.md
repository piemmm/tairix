# TAIRiX Bridge

TAIRiX Bridge (`TAIRiXBridge.app`) is to run RISC OS applications on the
TAIRiX desktop. Its staged plan, with the design every part follows, is
`plans/RISCOS-EMULATOR.md`. The engine is the
`userland/apps/tairix-bridge/riscos` subtree; this page describes the parts
built so far.

{{#include ../../../userland/apps/tairix-bridge/README.md:notices}}

## The processor

`tairix-riscos-arm` (`userland/apps/tairix-bridge/riscos/arm`) is the ARM processor. It
knows nothing of RISC OS: the session above it decides what its memory, its
SWIs and its host calls mean.

### The tables

Each instruction set has one table, and everything that needs its encodings
is derived from it. Each row is a pattern that spells every bit of the
encoding: a fixed bit, a should-be bit, a bit of a named field, or a bit the
instruction ignores, which decodes alike whatever its value and encodes
clear.

- `a32` holds every A32 integer and system encoding from ARMv2 to ARMv7-A,
  with ARMv8-A's AArch32 integer additions (load-acquire and store-release,
  `SEVL`, `CSDB`, `SSBB`, `PSSBB`, `CRC32`) and the Security and
  Virtualization Extensions' `SMC`, `HVC` and `ERET`, which BASIC's
  assembler lays down. No profile has those extensions, so the three are
  undefined on every profile and decode only on `Isa::ALL`.
- `t32` holds Thumb's 16-bit encodings from ARMv4T and Thumb-2's 32-bit
  encodings from ARMv6T2, through the same additions. Its encodings cut
  across one another, a register field holding the PC naming another
  instruction, so its rows may overlap and the first match decides.

`a32` also holds the FPA's encodings on coprocessors 1 and 2 (see *The
FPA*), ahead of the generic coprocessor forms, and every VFP, Advanced SIMD
and Cryptographic Extension encoding through ARMv8-A's AArch32 additions (see
*VFP and Advanced SIMD*):

- The floating-point and SIMD rows sit after each group's integer rows. Arm's
  encoding index resolves some of their overlaps by priority, one encoding
  naming another that takes its words, so the earlier row decides and a later
  row's build refuses the words it gave away; a test holds that none is
  wholly covered by an earlier one, and that none meets an integer row but a
  generic coprocessor form.
- The architecture reserves coprocessors 10 and 11 for the unit, so on a core
  with VFP no generic coprocessor form decodes there: a word in their space
  that is no VFP encoding is undefined. The FPA claims only the encodings
  FPEmulator implements, by its rows.
- T32's floating-point and SIMD encodings are A32's with the prefix moved
  (`111U 1111` for `1111 001U`, `1111 1001` for `1111 0100`) or the condition
  `AL`, so `t32` decodes and encodes them through `a32`'s rows, under the
  condition its `IT` block gives. ARMv8's unconditional additions and the
  Cryptographic Extension are UNPREDICTABLE in a block, as Arm defines them.
- Its generic coprocessor forms are A32's with the condition `AL`, or
  unconditional, so `t32` decodes and encodes them through `a32`'s generic
  coprocessor rows alone, the FPA's rows aside, as Thumb has no FPA, and holds
  them to its own rule for a PC base.

From the tables:

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
  `ADDEQS`), stack mnemonics on R13 (`STMFD R13!,{R0-R3,R14}`), `&` for hex,
  and UAL's for the floating-point and SIMD forms, the condition before the
  data type (`VADDEQ.F32 S0,S1,S2`, `VLD1.8 {D0-D3},[R0:128]!`);
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
| FPA | as FPEmulator implements it | the same |
| VFP and Advanced SIMD | none | VFPv4 with 32 doublewords and VFPv2's short vectors, Advanced SIMD, ARMv8's floating-point additions, the Cryptographic Extension |

Both take exceptions in the 32-bit modes and restore an aborted instruction's
base, as the SA-110 does. On the 26-bit profile an `STM` that writes back a
base it lists after its first register stores the written-back base, as the
ARM6 and ARM7 data sheets give it. On the 32-bit profile a mode it lacks,
named by `MSR` or by an exception return's SPSR, is refused as the
UNPREDICTABLE it is.

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
- `WFI` or `WFE` with nothing to wake it: an exception return is an event, as
  `SEV` and `Cpu::signal_event` are;
- an FPA instruction meeting an exception whose trap the FPSR enables, past
  it, for the session to raise FPEmulator's error;
- a CP15 register the processor does not model, before it executes.

An A32 instruction in the unconditional space executes; its condition field
means never only where the profile's architecture says so.

A state the host sets (`set_state`, `set_reg` of R15, `set_cpsr`) is held to
one the processor could hold: the PC aligned for the state the CPSR selects,
within the 26-bit address space in a 26-bit mode, and no IT state outside
Thumb nor a reserved one. A switch into a 26-bit mode holds the next
instruction within that space too.

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
- CPACR holds the access fields of coprocessors 10 and 11 on a core with
  VFP, and nothing else; `ID_ISAR5` reports the Cryptographic Extension where
  the instruction set has it.

### The FPA

RISC OS 5 has no FPA hardware: FPEmulator, the module that takes FPA
instructions at the undefined instruction trap, defines what its software
sees, so `fpa` answers as FPEmulator 4 does. Each rule comes from the FPA10
and ARM7500FE data sheets, or from FPEmulator observed black-box by
`cargo xtask riscos-fpa`; where FPEmulator is inexact, `fpa` rounds
correctly. `fpa::Fpa` is the state, eight registers and the FPSR, and its
operations; the processor runs FPA instructions through it.

- **Registers** hold FPEmulator's internal format, `fpa::Internal`, the three
  words `SFM` stores. A single or double denormal, infinity or NaN keeps its
  own precision's form until an operation converts it; `URD` leaves an
  integer unnormalised for `NRM`.
- **Arithmetic** is `lib/softfloat`'s, rounded at the instruction's precision
  in its rounding mode. `FML` multiplies its operands' top 32 mantissa bits
  alone; `FDV` and `FRD` divide in full. `RMF` is IEEE 754's remainder.
  Underflow is tiny after rounding and flagged where denormalising loses
  bits; overflow gives the extended infinity, or the precision's largest
  number where the rounding mode rounds away from that infinity. An invalid
  operation's NaN is an extended quiet NaN carrying its reason in the
  mantissa.
- **The transcendental functions**, `LOG` to `POL`, answer their value
  correctly rounded, from `lib/softfloat`'s elementary functions, where
  FPEmulator's lies a few places off. What has no value is FPEmulator's: each
  refusal's reason and sign, the bounds past which `SIN`, `COS` and `TAN`
  refuse an argument (`SIN` from (2^31 − ½)π, `COS` from (2^31 − 1)π, `TAN`
  from (2^31 − ½)π/2 and a place past −(2^31 + ½)π/2), the exact cases (`LOG`
  of 10^n, `LGN` and `LOG` of one, `ACS` of one) and the flags the special
  cases raise. An odd integer power of a negative number below 2^16 rounds as
  though bits lay below it, an infinity to such a power overflows, and the
  angle of (+∞, y) is a zero flagged inexact, as FPEmulator answers.
- **Packed decimal** is the data sheet's two formats through `lib/softfloat`'s
  `Packed`. A load reads the value exactly and rounds it to nearest; a store
  writes the digits the format guarantees, 17 or 21, correctly rounded, and
  the rest zero. What the data sheet leaves undefined is FPEmulator's: a
  digit above nine counts at its face value; an exponent from 16 640 (16 646
  144 expanded) at face value reads as an infinity or a NaN; a NaN's payload
  is its fraction's 62 bits taken as bits 10 to 0, 39 to 11 and 61 to 40,
  written as a decimal integer behind a quiet mark, and an expanded quiet
  NaN's load adds 1013 × 2^52 to it. A single or double infinity stores as a
  NaN without a payload, and a signalling NaN stores quiet, signalling
  nothing.
- **An enabled trap** writes nothing but the flags of exceptions raised before
  it: the destination, memory and a store's base stay as they were. A load
  writes its base back before converting, as FPEmulator's does, so a trapped
  packed load has.
- **The FPSR's system ID** is FPEmulator's, fixed; `WFS` writes the rest.
- **Transfers** address as FPEmulator does: P and W clear is at the base with
  no writeback, R15 as a base is the instruction's address plus 8 and never
  written back. From User mode each word moves as `LDRT` or `STRT` would,
  rotated by a misaligned address on `BIT26`; from a privileged mode the
  words move as a block, the address's low bits ignored there. An aborted
  transfer takes the data abort with its base restored.
- **Register transfers:** `FLT` reads R15 as the next instruction's address
  in User mode and as R14 in a privileged one; `WFS` reads it as the
  instruction's address plus 12; in a 26-bit mode both carry the PSR. `FIX`,
  `RFS` and a comparison to R15 set only its flags; a comparison to another
  register writes the flags there. In System mode FPEmulator restores R13
  and R14 as it returns, so a result or writeback to them is lost.
- **Decoding** claims exactly what FPEmulator claims: every addressing form
  of the two coprocessors' transfers, `MCRR`'s words among them, and
  coprocessor 1's data operations and register transfers; not precision 11,
  the dyadic opcodes 13 to 15, `WFC` and `RFC`, coprocessor 2's other forms,
  the condition `1111` where it marks the unconditional instructions, or
  Thumb state. What
  FPEmulator claims it executes, never UNPREDICTABLE; the bits it ignores
  decode alike. A comparison to a register other than R15 and an unindexed
  transfer have no FPA spelling, and are shown as the generic coprocessor
  instruction, as the Debugger shows them.

Where FPEmulator's answer is whatever its internal paths make of an input
the FPA's data sheet leaves undefined, the FPA takes the input at its value:
a `URD` result used by anything but `NRM`, and a single or double denormal's
marker with no fraction, which only `LFM` of hand-made words makes, is zero.
The few invalid operations whose NaN FPEmulator signs by the caller's R6 get
a positive one. FPEmulator does not survive FIQ mode and runs in Undefined
mode itself; there the registers are the current mode's, as the processor
banks them. The 26-bit conventions follow FPEmulator's 26-bit build.

### VFP and Advanced SIMD

`vfp::Vfp` is the register file the two share, the FPSCR and FPEXC: 32
doublewords, which are also the 16 quadwords Advanced SIMD names and, below
D16, the 32 singles VFP's single precision names. The FPSCR holds the
comparison flags, `QC`, the controls (`AHP`, `DN`, `FZ`, the rounding mode,
the short vectors' length and stride) and the cumulative flags; no exception
traps, which a `VMSR` cannot set. FPEXC holds only its enable. `VMRS` reads
FPSID (the common VFP subarchitecture, version 2) and the MVFRs, which report
every feature the profile has, short vectors among them.

- **Access.** CPACR must grant both coprocessors to the current mode, and
  FPEXC.EN enable the unit; otherwise the instruction takes the undefined
  instruction exception, as hardware does for RISC OS's lazy context switch.
  Privileged code reaches the identification registers and FPEXC with
  FPEXC.EN clear, though never past CPACR.
- **Arithmetic** is `lib/softfloat`'s under ARM's rules: tininess before
  rounding; flush-to-zero flushing a denormal operand (raising `IDC`) and a
  tiny single or double result (raising underflow alone), never a half's; the
  default NaN; and the alternative half-precision format, which has no
  infinity or NaN and reaches just under 2^17. VFP rounds as the FPSCR says
  except where an instruction names its rounding: a conversion to an integer
  or to fixed point by `VCVT` toward zero (`VCVTR` as the FPSCR says), one
  from fixed point to nearest, and ARMv8's directed forms (`VCVTA`, `VRINTM`
  and the rest) as they name. Advanced SIMD uses the standard controls: nearest,
  flushing, the default NaN. `VMLA` and its kin round twice, `VFMA` once; the
  reciprocal estimates and steps are the architecture's tables and its
  non-fused formulas.
- **Short vectors** run as VFPv2 defines them: `LEN` and `STRIDE` make a
  vector of up to eight singles or four doubles of the destination's bank, a
  first-bank destination (S0 to S7, D0 to D3, D16 to D19) a scalar and a
  first-bank last operand a scalar applied to each element; each operand
  wraps within its bank, and the elements are computed one after another. A
  reserved stride, a vector longer than its bank and a fused operation with
  either set are UNPREDICTABLE, and refused. The other operations are always
  scalars.
- **Integer lanes** are computed exactly and then wrapped or saturated; a
  saturating operation sets `QC`, which only a `VMSR` clears.
- **Transfers.** `VLDR`, `VSTR`, `VLDM` and `VSTM` move aligned words; an
  element transfer moves its elements under the profile's alignment rule and
  faults an address the alignment it names does not hold. A doubleword's
  words, and an element's bytes, follow the data endianness. A load writes no
  register until every access has landed, so an aborted one changes nothing.
- **The Cryptographic Extension** computes the AES round steps and the SHA-1
  and SHA-256 hash steps as Arm's pseudocode defines them, in constant time:
  the S-box is the inverse in GF(2^8) and the affine map, every byte at once,
  with no table indexed by data, so a guest's keys cannot leak through the
  host's caches.

### The MMU

`tairix_riscos_arm::mmu::Mmu` is the bus the processor runs through.

- **Frames.** Guest memory is 4 KiB frames numbered as physical pages, at
  most the limit in use. A frame lives while it is held or mapped and starts
  zeroed; the lowest free number is taken first.
- **Spaces.** Below the current space's extent its own table answers, above
  it the global one. A task switch swaps one pointer, and TLB entries carry
  the space they were found in, so it flushes nothing. A global page is never
  placed below any space's extent, so no space hides one.
- **Access.** Each page grants User mode and the privileged modes nothing,
  reading, or reading and writing, and may be executable where readable. An
  access its page refuses is a permission abort, one to nothing a translation
  abort; CP15 records either as the profile's fault status.
- **Host frames** hold the emulator's own code. No mapping lets the guest
  write one, and a fetch from one is marked as host code.
- **The TLB** is direct-mapped, 1,024 entries. Mapping, unmapping or
  protecting a page forgets its entry; destroying a space forgets that space's.
- **Reservations.** A range's tables can be made ahead and kept, so mapping
  inside it never needs memory and a multi-page change cannot fail halfway.
- **The watch.** A store into a watched 256-byte granule reports it to each
  watcher watching it, once until that watcher takes its reports. A host write
  or a freed frame reports every granule watched in it. The display's damage
  and the translator's code watch independently.

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
them.

`cargo xtask riscos-fpa <module>` holds the FPA to a developer's own copy of
FPEmulator 4, run as an opaque program on the processor itself: the 32-bit
profile without the FPA, so each FPA instruction traps to the module, with
the SWIs its initialisation and error path call answered. Each case, a random
FPA instruction from a random caller mode over random registers, FPA state
and memory (a packed load mostly reading a packed value the data sheet
defines), runs under the module and natively, and must leave the same
registers (or the same exception dump, for a trap), memory, FPA registers
and FPSR. A case may differ only where FPEmulator is the less accurate:

- a transcendental value within FPEmulator's error: 16 places, twice the
  places the function magnifies its arguments' last place into, and a place
  of the precision, which that error can carry a result across;
- a packed load within 256 places of the FPA's, and a packed store whose
  FPA digits round the value correctly while FPEmulator's lie within half a
  digit and 256 places of it, FPEmulator perhaps flagging or trapping on an
  inexact conversion the FPA's exact one is not. FPEmulator's packed
  conversions stray by up to some 150 places at the ends of the exponent
  range, measured over 400 000 of them.

A case FPEmulator is no reference for is not held to it, and the harness
counts those that differed: a transcendental of an unnormal, or of or to a
value in the denormal range, which FPEmulator leaves unnormalised or as
garbage, flagging no underflow (`SIN` of an extended denormal is an
infinity); a remainder of an uncommon operand, which it misreads; and a packed
load of a digit above nine, which the data sheet leaves undefined.
`--write-sample` writes 2,200 cases that agreed in every bit as vectors
carrying the FPA's state before and after, which the crate's test replays;
the module's bytes are never written, only the processor's own runs that
FPEmulator matched. The 26-bit builds need RISC OS 3's kernel around them,
which the harness does not model.

Each instruction class has unit tests. `fuzz_arm` fuzzes both decoders and
encoders and the interpreter in either state; `fuzz_fpa` every FPA operation
over any register values and FPSR, and FPA programs from any state;
`fuzz_simdfp` any VFP or Advanced SIMD instruction in either set from any
register file, FPSCR (short vectors included) and mode, with the unit granted
or withheld, holding that the FPSCR gains no bit a `VMSR` cannot write and
that its flags only gather; `fuzz_mmu` random runs of frame, mapping, space,
watch and access operations over pages that share TLB entries, each answer
held to a reference model that has no TLB.

## The address space

`tairix-riscos-kernel` (`userland/apps/tairix-bridge/riscos/kernel`) lays the guest's
address space out over the MMU, sizes it, and keeps its task slots and
dynamic areas.

### Layouts

A session is made with one of two layouts.

| | Shared session | 32-bit isolated session |
|---|---|---|
| 26-bit code | runs | refused |
| A 26-bit slot | to 28 MiB | — |
| A 32-bit slot | to 28 MiB, or into the window's free space while no 26-bit code is resident | to 512 MiB |
| RMA | 42 MiB, 16 MiB reserved | `&20000000`, 256 MiB |
| System heap | 58 MiB, 4 MiB | `&30000000`, 32 MiB |
| Stacks | below the host pages | RISC OS 5's |
| Host pages | the top MiB below 64 MiB | `&FC000000` |
| Areas a 26-bit caller makes | the window, placed downward from the RMA | — |
| Other areas | above 64 MiB, placed upward | above 64 MiB and the application limit, around the RMA and system heap |

Both have zero page at `&FFFF0000`, holding the vectors the 32-bit profile
takes, and ScratchSpace at `&4000`. A 26-bit task also sees zero page at zero,
the same frames, as RISC OS 3 and 4 had it. A 32-bit task sees a
compatibility page at zero instead, as RISC OS 5 on ARMv7 has one: read-only
and never run, it holds a copy of the processor vectors, at `&20` a warning
that a string read through a null pointer reads, and zeros. `OS_Memory` 20
removes it from every 32-bit task at once and restores it, answering the state
it leaves; a 26-bit caller is told there is none. The rest of a 32-bit task's
low 32 KiB aborts. Each stack has an unmapped page below it.

### The memory policy

A session's ceiling is a quarter of the machine's usable RAM, lowered by the
user's limit on the worker and by the setting; nothing raises it. It is the
RAM the guest is told it has, and its free pool is the ceiling less what is in
use, the fixed pages included. Memory is taken only as the guest asks for it.

### Task slots

Each task's application space is a space of its own from `&8000`, switched in
O(1). A slot grows wholly or not at all, its tables reserved before any page
is mapped. A 32-bit slot that the 26-bit window rule refuses says so, for the
session window to explain; `OS_DynamicArea` 26's application limit caps every
slot.

### Dynamic areas

`OS_DynamicArea`, `OS_ChangeDynamicArea` and `OS_ReadDynamicArea` are RISC OS
5's, reason by reason: numbering (the quick numbers `&100`–`&1FF` first, most
recently freed first), titles in a case-blind order with the newest first
among equals, clamps, the change signature, placement, all-or-nothing grows
and partial shrinks, shrinkable areas giving up memory, the free pool as the
lever for application space with the current-active-object checks, grows
naming their pages in chunks of 63 (a page another holder has is replaced
with a copy of its contents), doubly mapped areas, sparse areas, and physical
memory pools.

`OS_Memory` 23 reserves physical pages, `&1CA` refusing a page reserved
already. A reserved page stays free until something that knows takes it: an
area's pre-grow handler that names it only by answering `RESV`, and a pool
that locks it only with the reserved flag; both are refused with `&1CA`
otherwise, and `OS_Memory` 12 recommends past it. `OS_ReadMemMapEntries`
marks a reserved page with bit 7, and a free page carries the free pool's
flags, the pool being a physical memory pool of pages neither cacheable nor
bufferable, as RISC OS 5's is.

The manager calls the rest of the kernel through one seam: each handler call,
service call and upcall is made with the address space consistent, and may
itself make memory calls. A resize refuses any other while its handler decides
and its pages move, as RISC OS's does. Guest code runs at other points too —
while room is made, after a resize, in a service call — so whatever was checked
before it runs is checked again after, and an area is followed by its id and
the sequence it was made at: one removed and replaced meanwhile is never taken
for it. Where RISC OS 5 leaves a case unguarded the manager refuses it: a
negative number, a system number made, renumbered onto or removed, an access
privilege it lacks, an initial size past the area's maximum, and an area its
handler grows again while it is removed, which stays rather than leaking its
pages. A sparse area is never doubly mapped, given particular pages or shrunk,
and a pool is never shrunk, whatever their flags ask. A session holds at most
4096 areas. The SWIs' registers, and the page blocks and titles a handler or
caller reads, are the kernel's to marshal.

`fuzz_areas` runs random slot, area, sparse and pool calls with hostile
arguments, under handlers that answer at random and call back in, resizing,
removing, replacing and making areas. It holds that every page in use is
accounted for, every page an area reports reads back at its address, no area
passes its maximum or meets another, and every area's number is one a caller
reads as positive.

## The kernel

The rest of `tairix-riscos-kernel` is RISC OS 5's kernel, its SWIs performed
in Rust over the guest's own memory, so a program sees its structures — heap
blocks, variable nodes, vector claims, error blocks — where RISC OS keeps
them. Its texts come from its message file, `resources/Kernel/Messages`,
authored as RISC OS documents them and read through MessageTrans once
MessageTrans has opened it; before then, and while MessageTrans is gone, the
kernel uses the same texts compiled in.

### The core

A `Kernel` owns the address space and a `World` of the rest; `Os` is the view
every SWI and built-in module works through. The `Host` trait is all the
kernel asks of its session: a monotonic clock in centiseconds, the real time,
input events, a word between slices of work, a wait until input or a
deadline, and a relay carrying a request to the side that holds the session's
files, HostFS's. Given the same host answers the kernel runs the same. A
session is also made with the machine's memory pressure and a log, which the
caches its modules keep give way to and record their decisions in.

- **Running.** `Kernel::run` runs the guest for an instruction budget, from
  the transfer of control a call into the kernel left pending where there is
  one — a program a command started. A SWI stops the processor and is
  performed in place; the kernel's routines on
  the host pages — vector defaults, handlers, the exception entries — are
  each one `UDF` it recognises.
- **Calls into the guest.** A claimant, handler, ticker or callback is called
  in the mode RISC OS calls it in, returning through a host-page address; a
  26-bit callee is given a return address carrying the PSR. Calls nest to a
  bound, past which the supervisor stack overflows.
- **Unwinding.** A transfer of control (an exit, an error handler entered, an
  exception), a wait, the host's stop and the program's end unwind to the
  outermost run as values; nothing is lost on the way.
- **Interrupts.** The clock asserts the interrupt line when it moves on,
  checked at the start of each run and between slices; the processor takes it
  where its mask allows, and the kernel's handler does what RISC OS's does:
  the keyboard's input, `MetroGnome`, the interval timer, `TickerV`, the timed
  calls, `UnthreadV`. A key therefore reaches the kernel, Escape included,
  while a program reads nothing.
  Work is never polled: an idle guest's `WFI` waits on the host until the next
  deadline or input, and a wait's waking asserts the line, so input the host
  gives before the clock moves is taken at once.
- **Waiting for input.** A program's own SWI that finds nothing to read, has
  run no guest code and is not one the kernel performs for another, unwinds
  with the processor back at it and is performed again when input comes,
  carrying on from what it had done — a timed read keeps its deadline, a line
  read keeps its characters. Deeper, the host waits in place, so a command
  reading lines keeps what it has done, and the processor takes the
  interrupts the clock raises meanwhile, as RISC OS's waiting SWIs enable
  them. No callback runs while a SWI waits. The host is told whether only
  input can end a wait, so a scripted host with nothing left to type stops
  one rather than running the clock on through the timed work due meanwhile.

### SWIs and errors

A SWI's results come back with V clear, N, Z and C as it set them. An error
comes back with V set and R0 at its block if the SWI's X bit is set;
otherwise it is offered as `Service_Error`, then to `ErrorV`, whose default
enters the error handler. A block that cannot be one is replaced with "SWI &n
returned an invalid error pointer", but for `XOS_GenerateError`'s. A SWI below
`&200` the kernel has not is "SWI &n not known"; above, no module having it,
it goes to `UKSWIV`. `OS_CallASWI` and `OS_CallASWIR12` perform the SWI they
carry, its X bit deciding; a chain of them stops after sixteen. Errors are
written into a ring of sixteen 256-byte buffers in the system heap, so a block
survives the next few errors. The numbers of the OS SWIs, their reasons and
flags are one table, `swi::os`, which the SWI dispatch table is keyed by; the
built-in modules' SWI chunks are another, `swi::chunk`, and the sound modules'
SWIs a third, `swi::sound`. The service calls' numbers and reasons are one
table, `service`, and a dynamic area handler's reasons another,
`memory::handler_reason`. File types are `filing::filetype`, an object's kind
and attributes `filing::fs`, the clocks' `OS_Word` numbers `word`,
a `*Configure` or `*Status` listing's value column `module::VALUE_COLUMN`, and
the page the processor's, `tairix_riscos_arm::mmu::PAGE_SIZE`. The kernel, its
modules, FileSwitch, BASIC, the VDU drivers and their tests all read them.

An error's number is the one genuine RISC OS 5 was recorded giving. A
condition it was never seen to report — it faults, or does not check — takes
the lowest number in the PRM's range for its family that no recorded error
uses; a call with no range of its own reuses the recorded error of the same
meaning, so `OS_AMBControl`'s unknown reason is the heap's "No such reason
code", as genuine RISC OS 5's `OS_NVMemory` gives.

### Vectors, the environment and callbacks

- **Vectors.** Ninety-six chains, the newest claimant first, each ending at
  the kernel's default. `OS_Claim` removes identical claims, `OS_AddToVector`
  keeps them, `OS_Release` removes the newest; each claim holds a system heap
  node. A claimant may release itself, or others, while it is called.
- **The environment.** Seventeen handlers with RISC OS's defaults;
  `OS_ChangeEnvironment` never sets one to zero. `OS_Exit` takes a return
  code beside `ABEX`, checked against `Sys$RCLimit`; the default exit handler
  ends the program. The default error handler prints the error as RISC OS
  does, returns the exit handlers to their defaults as an exit returns them,
  and ends it.
- **Callbacks.** Transient callbacks run newest first on the way back to User
  mode with interrupts enabled; an error postpones them once, so its caller
  reads its block first.
- **Events.** `OS_GenerateEvent` reaches `EventV` for an event enabled by
  `OS_Byte` 14, and always for numbers from 32.

### Memory SWIs

`OS_Heap` is RISC OS 5's heap manager in guest memory: first fit, blocks
split from the top, every link and size checked before it is followed, an
overwritten heap reported rather than walked. An aligned claim (reasons 7 and
8) takes an alignment that is a power of two or none and a boundary that is a
power of two no smaller than the block or the alignment, and any claim
refused leaves R2 zero, as RISC OS 5's does. The system heap and the RMA are
dynamic areas the kernel grows when a claim does not fit. `OS_Memory`,
`OS_ReadMemMapInfo`, `OS_FindMemMapEntries` and `OS_ValidateAddress` answer
from the address space. `OS_Memory` 12 recommends free contiguous pages at an
alignment and, where the flags give one, within a physical range; 19 hands a
DMA transfer's regions to the caller's input and output functions, joining
what runs on and marking for a bounce buffer the partial cache lines a write
leaves; 20 is the compatibility page's; 23 reserves pages; 64 converts pages
with wide addresses. `OS_AMBControl` is RISC OS 5's Wimp's own call, which
no program is to use, and the emulator's Wimp keeps its tasks' memory itself:
reason 9 succeeds and changes nothing, as genuine RISC OS 5 answers a program,
and any other reason is refused as unknown, where genuine RISC OS 5 hangs or
faults.

### Variables, input and output

- **System variables** are RISC OS's nodes in the system heap, kept in name
  order: strings GSTrans'd on the way in, numbers, macros, expressions, and
  code variables whose entries are called. Wildcards and the context walk them
  as `*Show` does.
- **Input** is RISC OS's buffers, filled from the host's events: characters,
  key transitions through `KeyV` and the key handler (the keyboard, below),
  and the pointer, with escape, soft keys, `*Exec` and input redirection.
  `OS_ReadLine` reads through `OS_ReadC`, storing, deleting and echoing as
  RISC OS's does. It keeps RISC OS 3 and 4's flags in R0 for a 26-bit caller,
  and ignores a 32-bit caller's, returning R0 without them, as RISC OS 5
  does.
- **Output** goes through `WrchV` to the spool file, the serial stream, the
  VDU drivers and the printer as `OS_Byte` 3 selects.

### OS_Byte, OS_Word and CMOS

The `OS_Byte` reasons the PRM documents, the variables under RISC OS's rule
in zero page at `&08FA` plus their number, where `OS_Byte` 166 and 167 say
and genuine RISC OS 5 keeps them; the reasons the kernel does not know passed
to modules as `Service_UKByte` and otherwise refused. `OS_Word` likewise, an
unknown one, 23 among them, passed on as `Service_UKWord` without error. CMOS
is 256 bytes by logical address, with its checksum. A reset gives the
settings the kernel's own keywords and start-up read genuine RISC OS 5's
reset values, but for the filing system (HostFS), the country (the UK) and
the monitor type and sync (automatic, the display being a window), and zeroes
the rest, the settings of modules the emulator does not provide among them.
`OS_NVMemory` 7, which sets the station number given a passphrase, accepts
none.

### Information, conversions and text

`OS_ReadSysInfo` and `OS_PlatformFeatures` describe a HAL machine whose
processor features are read from its ID registers. The number conversions,
`OS_ConvertVariform`, `OS_ReadUnsigned`, `OS_EvaluateExpression`, GSTrans,
`OS_SubstituteArgs` and `OS_ReadArgs` are RISC OS's. `OS_HeapSort` runs
Knuth's algorithm H step for step as RISC OS does, so equal keys, and every
comparison a guest routine sees, match; it moves sorted blocks a cycle at a
time through the temporary slot. `OS_CRC`, `OS_PrettyPrint` and `OS_Confirm`
complete the set. `OS_CRC` carries all 32 bits of R0 as RISC OS 5's register
does, so a start past 16 bits shifts down into the result. `OS_PrettyPrint`
expands a caller's dictionary, or with R1 zero the kernel's own: the PRM's
table, token 22 empty as RISC OS 5's is, held in the host pages where a
program may read it. The byte after an escape is the escape's own even where
it is zero, which brings in R2's string.

### Resources and messages

- **ResourceFS** keeps RISC OS 5's blocks of files, registered newest first,
  each entry's offsets and lengths checked before they are followed. A name is
  matched without regard to case through the territory's upper-case table,
  from a sorted index built when it is first wanted; a directory is any name
  before a full stop. Its services and `UpCall` are RISC OS's. It registers
  `Resources:` with FileSwitch, read-only as RISC OS 5's is: a file opens to
  be read and nothing else, its root has attributes `&19`, directories list in
  name order, and every change is "Resources: cannot be changed".
- **The Messages module** lays every built-in's files, and those no one module
  owns, into one block in the host pages, stamped as genuine RISC OS 5.30's
  ROM files are, and registers it as it starts and again whenever ResourceFS does. Among them are the Wimp's
  file-type sprites, `Wimp.Sprites22`: each type the content-type registry
  knows, and the untyped file, drawn from the TAIRiX desktop's own icon master
  at RISC OS's sizes — `file_xxx` 34 pixels square, `small_xxx` 18, at 90 dots
  an inch, in sixteen-bit colour with an alpha mask. RISC OS plots a sprite by
  its own resolution, so one file serves every mode; it fits the 26-bit
  layout's megabyte of host pages, where 32-bit pixels at both resolutions
  would not. `cargo xtask riscos --write-sprites` draws it, and the
  `riscos-sprites` gate holds the committed file to the masters.
- **MessageTrans** keeps descriptors in guest memory, a file held in place in
  ResourceFS or copied into the RMA or the caller's buffer, and an index for a
  large file. Tokens match as the PRM sets out — `?` wildcards, alternatives
  by `/` or on following lines, a comment only at a line's start, a value
  running to its line feed — then the global file, then the caller's default;
  `%0` to `%3` and `%%` are substituted. An escape and the byte after it are
  expanded from a dictionary as RISC OS 5's are, from one of the emulator's
  own, which is empty, since RISC OS 5's words are not carried. Errors go into
  the PRM's ten foreground and two interrupt buffers, used in turn. A file held in place is found
  again when ResourceFS changes, and its holders are told when it goes; a file
  outside ResourceFS is read through FileSwitch's SWIs (RO19), which
  `messagetrans.scenarios` holds to RISC OS 5 on a RAM disc's file. Where RISC OS's
  copies are unbounded — a lookup into no room, an error into less than its
  number and a terminator, menus past their buffer — nothing is written past
  the room the caller gave, and "Buffer overflow" is the answer. A SWI of its
  chunk past its table is error &110, as the other modules' are.

### Territories and International

- **The Territory Manager** registers territories, each a table of the PRM's
  43 entries, and passes each territory SWI to the one it names with R0 to
  R9, the entry's index in R11. Once the current territory is registered it
  selects that territory's keyboard handler and defines its alphabet through
  `Service_International` 5, as the PRM's set-up does; `Territory_SelectAlphabet`
  defines the alphabet too, leaving `OS_Byte` 71's number as it was, as RISC
  OS 5 does. The time zone in force is the CMOS's standard offset and
  daylight saving bit: the current territory's zone where its standard offset
  is the CMOS's, a zone of the Manager's own naming otherwise. It answers
  `*Territories` and `*Configure`'s `Territory`, `DST`, `NoDST` and `TimeZone`,
  and takes the territory's own zone names as `DST` and `NoDST`. A SWI no
  territory answers, a table's reserved entries among them, is error &1E8,
  RISC OS 5's number where the PRM gives &43040. Its SWIs past the PRM's are
  named as RISC OS 5.30's are: `Territory_IME`, `Territory_DaylightRules`,
  `Territory_Reserved1` to `Territory_Reserved17`, `Territory_Select`,
  `Territory_DaylightSaving` and `Territory_ConvertTimeFormats`.
- **Daylight saving.** `Territory_DaylightSaving` reads and sets the state in
  CMOS — in force, bit 7 of &DC, and kept by the rules, bit 0 of &10, as
  `*Configure DST`, `NoDST` and `AutoDST` set it — and answers whether
  daylight saving is in force at a time and a year's bounds by asking the
  territory's `Territory_DaylightRules` entry, so a guest territory's rules
  are its own. A question about a time never fails; a refused year is &197.
  The UK territory's rules are the session's zone's, in force wherever its
  offset differs from its standard offset, for the years 1968 to 2099 genuine
  RISC OS 5's rules cover; it refuses another year or time zone with &AC2.
  Kept by the rules, daylight saving changes over on the clock at a year's
  bounds and at each new year, and at start-up, each change announced by
  `Service_TimeZoneChanged`.
- **Conversions and selection.** `Territory_ConvertTimeFormats` converts
  between local and UTC ordinals and five-byte times by the territory's time
  zone and daylight saving as asked, as its rules say, or by an adjustment in
  centiseconds; it reads a local time as standard time to ask the rules, as
  RISC OS 5 does, and refuses an unknown form or flag with &1E8, another time
  zone with &196 and ordinals naming no time with &195. RISC OS 5.30 finds its
  name but writes the SWI as a number; the emulator names it both ways.
  `Territory_IME` gives no input method. `Territory_Select` and `*Territory`
  make a loaded territory current and issue `Service_TerritoryStarted`, setting
  nothing else up; `*Territory` alone names the current territory.
- **The UK territory** has RISC OS's names, formats and tables, as recorded
  from RISC OS 5, and the session's time zone, `Config::zone`, from `lib/tz`:
  its standard and summer names and offsets. A CMOS reset takes the locale's
  standard offset and whether its summer time is in force. A zone without
  summer time gives a summer reading an hour ahead under its own name.
  Collation is three passes — letters, then accents, then case, case weighed
  on the unaccented letters where accents are ignored — with its ligatures,
  and runs of digits as numbers where R3's bit 2 asks, as RISC OS 5's does.
  Ordinals carry a field past its range into the next; a time string is read
  writing each of the time's fields as it is read, so a refused one leaves
  those before it.
- **International** answers for the countries, alphabets and keyboards —
  their names and numbers, each country's alphabet and ISO 3166 code, as
  recorded from RISC OS 5 — and for each alphabet's characters, as a table of
  code points: ISO 8859's upper halves, with the PRM's Table D characters at
  &80 to &9F for Latin2 to Latin4, and Latin2's there, less those already
  held, for the alphabets past the PRM's. A code an alphabet leaves out keeps
  its previous definition, as the PRM says. It redefines the
  system font for an alphabet from `lib/vdu`'s glyphs, which draw every
  character of every alphabet, those past Latin1 composed from shared accents
  and letters. `OS_Byte` 70 and 71 and `*Alphabet`, `*Alphabets`,
  `*Country`, `*Countries`, `*Keyboard` and `*Configure Country` reach it.

### The keyboard

- **The kernel's part** is RISC OS's key-handler protocol behind `KeyV`'s
  default: the handler's tables read, bounded, at each use; two keys rolled
  over; the first code at once, since the session's keyboard reports clean
  transitions and needs no debounce, then repeats at `OS_Byte` 11 and 12's
  delay and rate on the clock's deadline; the status, the LEDs as the PRM
  numbers them, Break's and the mouse buttons' exits; and `INKEY` and the key
  scans through the handler's internal-key table. The session's keyboard
  arrives as a PC's, `Service_KeyHandler` telling the modules, as it does
  again after `OS_InstallKeyHandler`.
- **InternationalKeyboard** installs a handler whose tables are in its
  workspace for programs to read, its special keys' code Rust. Its layouts are
  TAIRiX's own, one for each keyboard RISC OS's has — the UK's, Europe's,
  Greece's, Russia's, Israel's, Turkey's, Latin America's, Japan's, Korea's,
  Taiwan's, Dvorak's and Colemak's — each drawn from its country's standard
  with an Alt layer; any other keyboard has the UK's. A dead key's accent is
  composed with the next character by Unicode's canonical composition, the
  space after it giving the accent alone. Caps Lock pairs a letter with its
  shifted capital, so Turkish keeps its dotted and dotless i. Ctrl gives the
  control code of the key's Latin letter or `@[\]^_`, whatever the layer, or
  a digit's value. A character is
  given in the alphabet selected, through its table, or as UTF-8. Alt with the
  keypad types a character by its decimal number, as the PRM's Table D says;
  Alt, Ctrl and F1 select the UK keyboard, F2 the configured one, and F12 the
  country whose international dialling code Alt and the keypad then type.
  Shift Enable reverses Shift only while Caps Lock is on, and the next key
  down or up shows on the LEDs the locks `OS_Byte` 202 wrote, as the PRM sets
  out.

### Exceptions

An exception with no claimant enters its environment handler, whose default
raises an error naming the address. `OS_AbortTrap` traps answer the loads and
stores an aborting instruction makes in their range, the instruction replayed
against them, for translation and permission faults only, as RISC OS 5's do:
every trap and abortable area covering an access is asked in turn before it is
declined, an area's handler given its allocated end in R5, and an instruction
fetch or an exclusive access is offered as a request to map the page rather
than replayed. `OS_ClaimProcessorVector` claims and releases the processor
vectors, refusing the FIQ vector and one past the last with &1A0 and a release
naming a routine not current with &1A1.

### Modules

The module chain is RISC OS 5's: modules in the order they were linked, each
with its incarnations, the preferred first. Every incarnation has a node in the
system heap holding its private word and then its postfix, so R12 + 4 is its
postfix, as on RISC OS. `OS_Module` performs every reason RISC OS 5 has, and
`OS_ExitAndDie` kills a module on its way out.

- **Built-in modules** are Rust behind a module image in the host pages:
  header, title, help string, command table, SWI decoding table and flags, the
  images one ROM chain. Each entry jumps to a host routine that enters the
  module's Rust, so a copy, as `*RMFaster` makes one, enters the same Rust; the
  kernel itself calls a built-in's Rust directly. A session's built-ins are the
  kernel's own and those its other crates pass in `Config::modules`, each a
  provided module of the inventory. They start at reset in ROM order, but for
  those CMOS unplugs. The `UtilityModule` comes first: it holds the module
  commands and `ARM_IMB` and `ARM_IMBRange`, and cannot be killed.
- **Guest modules** come from a file (`OS_Module` 0 and 1), from memory (10 and
  11) or through a Podule manager the guest itself provides (17). A module that
  declares itself 32-bit is checked as RISC OS 5 checks one. One that does not
  is a 26-bit module, admitted only where 26-bit code is, with a header as short
  as RISC OS 2 allowed. A squeezed module is expanded into the RMA as it is
  first initialised, from a file or from memory alike, and the expanded header
  is checked again.
- **Squeezed modules** are expanded as genuine RISC OS 5.30 is observed to
  expand them (`tests/corpus/squeeze.scenarios`). The module keeps its header
  whole, its initialisation word's top bit set and the rest its squeezed
  length; five words end that length — the expanded size, the bytes of codes
  and of tables, and the entries of the short and the long table of common
  words. The tables, read forwards, hold each entry as a delta from the last;
  the codes, read backwards from the tables, give each pair of words by a
  byte's two nibbles — zero, four bytes, a long-table entry above a low byte,
  or a short-table entry — filling the module from its end. A length past the
  module given is refused with &101, as RISC OS refuses it, and codes that
  stop short of the expanded size make the module, as RISC OS's do. What RISC
  OS hangs or faults on — a trailer off its word, an expanded size no multiple
  of eight, codes or tables reaching past their bytes — is refused with &10E.
- **Replacement.** A module whose title another already has kills that one first
  and takes its place in the chain. A bridge — the kernel, and the modules the
  guest reaches the desktop through — cannot be replaced: loading over one is
  refused with "Module … refuses to be replaced", number &103.
- **Initialisation and finalisation** follow RISC OS 5's protocol:
  `Service_ModulePreInit` for a module in RAM; R10 the environment, R11 the
  incarnations it has or its hardware base, R12 the private word;
  `Service_ModulePostInit` and `Service_ModulePostFinal` afterwards, R3 zero for
  the base incarnation. Finalisation is always fatal. It is refused while the
  current active object lies in the module's code or its workspace, and a module
  that refuses runs on as it was.
- **Services** go down their number's chain first: the incarnations whose
  module's service table lists it, R1 the number or, where the table asks, its
  place in the table. Then they go to the modules with no table. Each chain is
  in the order its incarnations registered. The walk finds each handler
  afresh, so a handler that kills or loads modules leaves it sound, and one that
  registered after the call began waits for the next call. The kernel keeps
  RISC OS's part in claiming FIQ. While the exit handler is the default,
  `Service_NewApplication` returns every handler to its default.
- **SWIs** of a chunk go to the module that registered it last, if that module
  has an incarnation, and otherwise to `UKSWIV`. Their names come from the
  module's decoding table, its decoding code or its title.
- **Commands.** `*Modules`, `*ROMModules`, `*RMLoad`, `*RMRun`, `*RMKill`,
  `*RMReInit`, `*RMClear`, `*RMTidy`, `*RMFaster`, `*RMEnsure`, `*Unplug` and
  `*RMInsert` are the `UtilityModule`'s, among the kernel's commands (the
  command line, below).

Where RISC OS 5's module handler leaks or fails, the emulator does what the
handler means to do:

- a failed load or initialisation frees everything it claimed;
- the base incarnation can be renamed, and so a new incarnation of a ROM module
  that is not active can be made;
- a module with no help string is too old for `*RMEnsure`;
- the service chains are walked soundly whatever a handler changes;
- an incarnation that initialised but cannot be recorded for want of memory is
  finalised again.

#### The inventory

The inventory lists every module of RISC OS 5.30's ROM for IOMD, in the ROM's
order, with its version, as `OS_Module` 20 reads them on RISC OS 5, which
makes a module's place its chunk number and names its unplug bit in CMOS. The
corpus's `rom-modules` reference scenario records that listing from the
running machine — each version, and each title's length and CRC — and a test
holds the table to it. A
provided or bridged module is present once its item lands. No lookup finds an
absent one. Every built-in's help string carries the date its ROM files are stamped
with.

| Module | Version | Held | Item |
|---|---|---|---|
| UtilityModule | 5.30 | bridged | RO15-RO17 |
| Podule | 1.72 | absent | — |
| FileSwitch | 2.92 | bridged | RO19 |
| ResourceFS | 0.26 | provided | RO18 |
| TerritoryManager | 0.58 | provided | RO18 |
| Messages | 1.12 | provided | RO18 |
| MessageTrans | 0.49 | provided | RO18 |
| UK | 0.64 | provided | RO18 |
| WindowManager | 5.87 | bridged | RO39-RO44 |
| TaskManager | 1.54 | bridged | RO39 |
| Desktop | 2.77 | provided | RO39 |
| SharedCLibrary | 6.23 | provided | RO26 |
| ScreenModes | 0.74 | provided | RO21 |
| VIDC20Video | 0.09 | absent | — |
| Mouse | 0.11 | absent | — |
| PS2Driver | 0.40 | absent | — |
| ADFSFiler | 1.16 | absent | — |
| BASIC | 1.85 | provided | RO20 |
| BASIC64 | 1.85 | provided | RO20 |
| BlendTable | 0.02 | provided | RO22 |
| BufferManager | 0.39 | provided | RO26 |
| ColourTrans | 1.97 | provided | RO23 |
| Debugger | 2.08 | provided | RO30 |
| DeviceFS | 0.73 | provided | RO26 |
| DisplayManager | 0.44 | bridged | RO43 |
| DMAManager | 0.23 | absent | — |
| DragASprite | 0.21 | provided | RO42 |
| DragAnObject | 0.10 | provided | RO42 |
| Draw | 1.22 | provided | RO24 |
| BBCEconet | 0.20 | absent | — |
| FileCore | 3.78 | provided | RO19 |
| ADFS | 3.56 | absent | — |
| RamFS | 2.33 | provided | RO19 |
| Filer | 2.49 | bridged | RO39 |
| FilerSWIs | 0.05 | bridged | RO39 |
| FSLock | 1.26 | absent | — |
| FontManager | 3.80 | bridged | RO25 |
| FPEmulator | 4.39 | provided | RO26 |
| Free | 0.42 | provided | RO26 |
| Hourglass | 2.19 | bridged | RO26 |
| IIC | 0.18 | absent | — |
| International | 1.70 | provided | RO18 |
| InternationalKeyboard | 1.00 | provided | RO18 |
| InverseTable | 0.18 | provided | RO23 |
| Joystick | 0.29 | absent | — |
| NetFS | 6.00 | absent | — |
| NetFiler | 0.87 | absent | — |
| NetPrint | 5.61 | absent | — |
| NetStatus | 2.09 | absent | — |
| Obey | 0.41 | provided | RO19 |
| ParallelDeviceDriver | 0.79 | absent | — |
| Pinboard | 1.07 | bridged | RO39 |
| PipeFS | 0.25 | provided | RO26 |
| RAMFSFiler | 0.41 | bridged | RO39 |
| ResourceFiler | 0.20 | bridged | RO39 |
| ROMFonts | 0.78 | provided | RO25 |
| RTC | 0.02 | absent | — |
| ScreenBlanker | 2.34 | bridged | RO43 |
| ScrSaver | 0.14 | bridged | RO43 |
| SerialDeviceDriver | 0.48 | absent | — |
| SerialDeviceSupport | 0.26 | absent | — |
| SerialMouse | 0.10 | absent | — |
| ShellCLI | 0.39 | bridged | RO48 |
| SoundDMA | 1.72 | bridged | RO29 |
| SoundChannels | 1.43 | provided | RO29 |
| SoundScheduler | 1.34 | provided | RO29 |
| SpriteExtend | 1.86 | provided | RO22 |
| SpriteUtils | 1.13 | provided | RO22 |
| Squash | 0.31 | provided | RO26 |
| SuperSample | 0.16 | provided | RO25 |
| SystemDevices | 1.34 | provided | RO26 |
| TaskWindow | 0.85 | provided | RO48 |
| WindowUtils | 2.53 | provided | RO45 |
| FilterManager | 0.30 | provided | RO39 |
| WaveSynth | 1.25 | provided | RO29 |
| StringLib | 1.20 | provided | RO29 |
| Percussion | 1.19 | provided | RO29 |
| SharedSound | 1.20 | provided | RO29 |
| Filer_Action | 0.64 | bridged | RO39 |
| DOSFS | 1.15 | absent | — |
| ColourPicker | 0.59 | provided | RO45 |
| DrawFile | 1.62 | provided | RO24 |
| BootCommands | 1.53 | provided | RO19 |
| WindowScroll | 0.02 | provided | RO40 |
| MbufManager | 0.30 | absent | — |
| Internet | 5.67 | bridged | RO46.1 |
| Resolver | 0.74 | provided | RO46.1 |
| Net | 6.27 | absent | — |
| BootNet | 0.97 | absent | — |
| Freeway | 0.42 | absent | — |
| ShareFS | 3.62 | absent | — |
| MimeMap | 0.19 | provided | RO26 |
| LanManFS | 2.70 | absent | — |
| DHCP | 0.27 | absent | — |
| !Edit | 1.75 | absent | — |
| !Draw | 1.46 | absent | — |
| !Paint | 2.54 | absent | — |
| !Alarm | 2.96 | absent | — |
| !Chars | 2.06 | absent | — |
| !Help | 3.27 | absent | — |
| Toolbox | 1.60 | provided | RO45 |
| Window | 1.81 | provided | RO45 |
| ToolAction | 0.38 | provided | RO45 |
| Menu | 0.42 | provided | RO45 |
| Iconbar | 1.23 | provided | RO45 |
| ColourDbox | 0.23 | provided | RO45 |
| ColourMenu | 0.23 | provided | RO45 |
| DCS | 1.14 | provided | RO45 |
| FileInfo | 0.21 | provided | RO45 |
| FontDbox | 0.19 | provided | RO45 |
| FontMenu | 0.26 | provided | RO45 |
| PrintDbox | 0.18 | provided | RO45 |
| ProgInfo | 0.19 | provided | RO45 |
| SaveAs | 0.21 | provided | RO45 |
| Scale | 0.18 | provided | RO45 |
| TextGadgets | 0.45 | provided | RO45 |
| CDFSDriver | 2.42 | absent | — |
| CDFSSoftATAPI | 1.44 | absent | — |
| CDFS | 2.63 | absent | — |
| CDFSFiler | 2.53 | absent | — |
| UnSqueezeAIF | 0.10 | provided | RO17 |

After the ROM, `OS_Module` 20 goes on into section 0, an expansion card's ROM,
as it does for RPCEmu's own HostFS on RISC OS 5: there the emulator lists its
HostFS (1.00, bridged, RO19.4), which no CMOS bit unplugs.

### The command line

`OS_CLI` is RISC OS 5's interpreter, through `CliV`.

- **A line** loses its leading spaces and `*`s; `|` starts a comment; past
  1023 characters it is "Too long". The first `{ < file }`, `{ > file }` or
  `{ >> file }` group redirects the line's input or output while it runs, and a
  write the file refuses ends the redirection with the filing system's error.
- **Prefixes.** A filing system's prefix is the filing system's to set for the
  line. `module:`, `MODULE#module:` and `-module-` run a command in that module
  alone. `/` runs a file, `%` skips the aliases, and `.` alone catalogues.
- **Resolution** is RISC OS's order: an alias; the kernel's commands, then each
  module's in the order their commands came (an abbreviation in chain order),
  the old numbered commands such as `*FX0` among them; the current and the
  secondary filing systems' commands; a module answering `Service_UKCommand`;
  and last, the filing system running a file.
- **Aliases** are `Alias$` variables: an exact name, or an abbreviation of the
  first alias in name order, the value's lines run with the parameters
  `OS_SubstituteArgs32` substitutes.
- **Parameters** are counted as RISC OS counts them, a quoted one with its
  spaces, and those a table's map names are GSTrans'd first. A count outside
  the command's range gives its syntax message, or a module's answer to
  `Service_SyntaxError`.
- **Bounds.** Nesting stops 32 deep with "No room on supervisor stack".
  Aliases and maps are expanded into RISC OS's ring of sixteen 1 KiB buffers;
  a line whose buffer was handed out again while it ran ends "Expansion too
  complex". An error is made only to be returned, so an expansion's own never
  takes the ring buffer of the error on its way out.

The kernel's commands are the `UtilityModule`'s, each reaching the kernel
through its SWIs as RISC OS's do. Their output is laid out as RISC OS lays it
out; their help is the emulator's own wording.

- **Output and variables:** `*Echo`, `*Error`, `*Eval`, `*Show` (paged, a value
  longer than its buffer read again whole), `*Set`, `*SetMacro`, `*SetEval`,
  `*Unset` (code variables passed by, as RISC OS passes them), `*Key`, `*IF` and
  `*Time`.
- **Files:** `*Type`, `*List` and `*Print`, each byte shown as GSTrans reads it
  back in the configured `DumpFormat`; `*Dump`; `*Build` and `*Append`;
  `*Load`, `*Save`, `*Create`, `*Delete`, `*Remove` and `*Close`; `*Exec`,
  `*Spool` and `*SpoolOn` — each through the filing system's SWIs.
- **The machine:** `*FX`, `*TV`, `*Opt`, `*GO`, `*Quit`, `*Ignore`, `*Shadow`,
  `*Cache`, `*ChangeDynamicArea` and the module commands.
- **`*Help`** gives each table entry and module title the keywords name (`.`
  names everything) and the kernel's topics `Commands`, `FileCommands`,
  `Configure` and `Modules`, unless a module answers `Service_Help`.
- **`*Configure` and `*Status`** keep the kernel's 34 keywords in CMOS in the
  PRM's fields — `Num` and `NoNum` in a bit it reserves, where genuine RISC OS
  5 keeps them — alphabetically, each followed by its opposites, so an
  abbreviation names the first it matches. Any other keyword goes to the
  modules' configuration keywords, then to `Service_UKConfig` or
  `Service_UKStatus`. `Ignore` takes the number its value starts with, none
  being nought, and turns the ignore character on only for a number that ends
  the line, a comma straight after it allowed, as RISC OS 5 does.
- **`*GOS`** enters the Supervisor: a prompt, `CLI$Prompt` or `*`, at which each
  line typed is run until a command starts another application.

A command that runs for as long as its work does takes the interrupt due at
each step, as RISC OS takes one in a command, so Escape stops a listing.

The interpreter and commands do what they mean to: a `-module-` prefix names a
module; nesting is bounded, where an alias that runs itself brings genuine
RISC OS 5 down; `*TV` alone and an `ELSE` ending a line work; a module's configuration keyword is matched as a
command is; a negative `MouseStep` is stored; `MonitorType EDID` refuses what
follows it; a refused `*Configure` changes nothing; and a listing turns paged
mode off however it ends.

### Testing

Each subsystem's tests run guest code and SWIs on a booted kernel with a host
whose clock and input they set. The file commands are tested against a
filing system of one file built from vector claimants. `fuzz_swis` performs random SWIs with hostile
registers — pointers into application space, at the top of memory, nowhere,
misaligned — and runs random guest code salted with SWIs, holding that every
refusal is a value, nothing runs for ever and the kernel still answers.
`fuzz_modules` links module images with random headers, service tables and
squeezed data, and performs `OS_Module`'s reasons, service calls and the
module SWIs on them, holding the same. `fuzz_squeeze` expands the corpus's
squeezed modules mutated, and noise carrying a squeezed length, under random
bounds, holding that every refusal is a value and every expansion whole pairs
of words within its trailer's size and the bound. `fuzz_cli` gives `OS_CLI` random lines:
the kernel's commands and their abbreviations with hostile parameters, aliases
with random templates, prefixes, redirection, GSTrans escapes and overlong
lines, holding the same and that the interpreter runs the next line.
`fuzz_international` gives MessageTrans hostile message files and ResourceFS
blocks through every call, the Territory Manager hostile strings, times and
formats, and the key handler random key sequences in every layout and
alphabet, holding the same and that nothing is written past a call's room.

## Filing

`tairix-riscos-filing` is FileSwitch and the filing systems it calls. While
FileSwitch is active it is the default of the seven filing vectors, in the
kernel's place: a built-in module's `vectors` name the vectors whose default it
is, so `OS_File` and its kin reach its Rust with no call through the processor.
Its state is the session's, kept through the kernel's store for other crates'
modules; its own texts are `resources/FileSwitch/Messages`, and the errors it
shares with the vectors' defaults are the kernel's.

- **Filing systems.** `OS_FSControl` 12 registers one from its information
  block, which FileSwitch copies into the RMA with its entries made absolute,
  as RISC OS 5 does; a filing system registered again takes its old place. An
  entry is called in Supervisor mode with R12 its workspace — a built-in
  module's as its Rust directly, guest code through the processor — and what
  it is handed, a path, a special field, a block to fill, is built below the
  supervisor stack, so a filing system that calls FileSwitch again finds its
  own untouched.
- **Names.** A prefix is `fs:`, `-fs-`, `fs#special:`, `#special:` or a path
  variable's `var:`. It serves its own name alone: a name with none is on the
  temporary filing system, the current one but while `OS_FSControl` 11 has
  set another from a line's prefix, until 19 puts the current one back; so
  are `OS_Args` 0 and `OS_Find` 0 given no handle, `OS_GBPB` 5 to 8 and
  `OS_FSControl` 10, 20 and 30. `OS_FSControl` 33 names no filing system for
  nought. A filing system is only ever handed an absolute path —
  `:disc.$` and the rest — its special field apart. `@`, `\`, `&`, `%` and `^`
  are FileSwitch's: it keeps each filing system's current, previous, user root
  and library directories as the paths they were set to (`OS_FSControl` 0, 1,
  39, 40, 43 to 45, 53, 54). A directory never set is its disc's root, the
  disc asked of the filing system by `FSEntry_Func` 0 and 11 as RISC OS 5 asks
  it, each time; the library is `&.Library`, else `$.Library`, else the
  current directory, looked for with no special field whatever the name
  gave. A wildcarded part is matched against its directory's
  names, read by `FSEntry_Func` 14, and the first match taken: the directory
  is read first, read up from where it is not there, then listed whatever it
  is, and one its filing system will not list matches nothing. An object not
  found has each directory above it read in turn, as RISC OS 5 reads them.
  Path variables, `File$Path` among them, are tried element by element,
  naming each other to a bound; a canonical name (`OS_FSControl` 37) reads
  every element but the last. A special field given to a filing system that
  takes none is refused, `&40F`, at the call that would hand it over, so a
  name that needs no call is still made canonical; an empty one roots its
  path at `$` without asking for the disc. A name ends at its first space.
  FileSwitch refuses (`&CC`) a name with no path, and one whose part past the
  first is a directory's sign, begins with one, `:` or `|` — the parent `^`
  apart — or holds `$`, `:` or `|`, and a disc's name holding a colon, as RISC
  OS 5 refuses them; whatever else
  a filing system cannot hold is its own to refuse. A disc's operation
  (`OS_FSControl` 41 to 57) alone takes no path as the current directory.
- **Open files.** Handles are the highest free from 255 down. A buffered file
  has one buffer of its natural block size in the RMA; FileSwitch keeps its
  pointer, extent and allocation, moves whole blocks within the extent
  straight between the filing system and the caller's memory and the rest
  through the buffer, extends a file as RISC OS 5 does — the block holding the
  old end zeroed, the space ensured, whole blocks after it written as zeros —
  and on closing a file written to sets its extent and renews its stamp. An
  unbuffered file's pointer and extent are its filing system's, which is told
  of each flush. The first read at a file's end answers with C set; the next is
  "End of file", from FileSwitch's own flag. `OS_Args` 254's status word says
  read, written, modified, at its end, unbuffered or unallocated. A buffered
  file opened for update has its extent set as it closes. A file opened for
  update is written only where its filing system's answer permits it, which
  FileCore's for a locked file does not. A file is restamped only if modified
  — written, or refused an `OS_GBPB` write, which RISC OS 5 counts as a
  change. An ensure is not one. An openout of a missing file makes it by
  `FSEntry_File` 7 first; a make the filing system refuses goes unreported,
  the open after it answering, so one in a missing directory is FileSwitch's
  "not found", `&D6`, as on RISC OS 5.
- **Whole files.** Every reason of `OS_File` reads its object first: a missing
  one is never handed to the filing system. Reasons 2 to 4 write through
  `FSEntry_File` 1 with the object's other information. Loads and saves go
  through `FSEntry_Open` and the buffer where the filing system's information
  word asks, `FSEntry_File` 255 and 0 otherwise. An empty file made over a
  directory, by reason 7 or 11, is "exists, and is not what was asked for",
  `&AF`, as a save over one is.
- **The information word.** A filing system with `FSEntry_File` 9 is read by
  it wherever the length is not wanted, and a directory a wipe or count was
  named or matched to is read by it again before it is walked. One always
  called to open a file is called whether or not the file exists, and makes
  any it creates itself. Read-only and interactive filing systems are left to
  enforce what they say, as RISC OS 5 leaves them.
- **Directories.** `OS_FSControl` 0, 1 and 39 with no name at all set the
  directory to `&`, the user root's to `$`; setting the current directory
  makes the one it replaces the previous one, and unsetting one leaves the
  previous one as it was. A filing system removed and registered again keeps
  its directories.
- **Copying, wiping, counting.** `OS_FSControl` 26 to 28 walk a directory an
  entry at a time, re-reading an offset after deleting what was there, and
  read a wildcard's matches in batches, as `*Access` (24) does; R3's flags
  and the time range in R4 to R7 decide what is touched, `C` and `V` asking
  and telling through `OS_ReadC` and the screen. A locked object is unlocked
  by `FSEntry_File` 1 before it is written over or deleted. A wildcarded leaf
  is looked up as any name is, then its directory walked, its filing system
  free to refuse one that is not there; a wipe whose directory lists nothing
  at all is refused, `&416`, one none of whose entries match is not, and a
  count is never refused for it. A quick copy and one that prompts for each
  change of medium are taken and change nothing: no copy uses application
  space, and no medium changes.
- **Directory reads.** `OS_GBPB` 9 to 11 hand the caller's buffer to the
  filing system. Reason 12 does as RISC OS 5 does: the filing system's
  records are read a word in for each record the buffer could hold, then each
  is moved down with its type added, the padding after its name left as it
  was.
- **Listings.** `*Cat`, `*Ex`, `*LCat`, `*LEx`, `*Info` and `*FileInfo`
  (`OS_FSControl` 5 to 9, 32) lay their lines out in RISC OS 5's columns, the
  header's disc and boot option read as RISC OS 5 reads them. `*Opt 1`
  prints each object a load, save or creation names, as levels 1 to 3 ask;
  RISC OS 5 names a save's object from a null pointer, which prints whatever
  is there, where the emulator prints its name.
- **Image filing systems.** `OS_FSControl` 35 and 36 add and remove one by
  file type. A file of that type is a directory to every caller: FileSwitch
  opens it on its host, then hands the image filing system paths within it.
  A host whose information word says it serves images makes discs and special
  fields canonical by `FSEntry_Func` 23, and its disc reasons (`OS_FSControl`
  41 to 57) reach `FSEntry_Func` 25 to 37.
- **Commands.** FileSwitch's 26 commands and its `FileSystem` and `Truncate`
  configuration keywords are each the call they stand for. `*Copy`, `*Wipe`
  and `*Count` take their options from `Copy$Options`, `Wipe$Options` and
  `Count$Options`, which FileSwitch sets at its start, then from the command;
  a letter none takes is error `&601`.
- **Services and upcalls.** `Service_CloseFile` closes what is open at or
  beneath a name; `Service_FSRedeclare` asks every filing system to register
  at FileSwitch's start; `Service_LookupFileType` names a type no variable
  does. `UpCall_ModifyingFile` comes before each change a call makes.
- **Its texts.** Messages FileSwitch alone gives are in its own words; the
  numbers are RISC OS 5's.

The SWI and service numbers, file types, object kinds and attributes, and
listing layout FileSwitch shares with the kernel are the kernel's tables:
`tairix_riscos_kernel::swi::os`, `tairix_riscos_kernel::service`,
`tairix_riscos_kernel::filing::{filetype, fs}` and
`tairix_riscos_kernel::module::VALUE_COLUMN`.

### HostFS

HostFS is the session's files: filing system `&99`, RPCEmu's own HostFS's
number, a host of image filing systems loading and saving through its open
files. Each root the front end hands the session is a disc named for its
folder and made unique ignoring case, the first the disc a name with none
reaches; `Transfer` holds each file delivered by a drop or an open, in a
directory of its own named by its number; `Scrap` is a disc in the worker's
memory, an eighth of the session's, which `Wimp$ScrapDir` names. The shared
session's own disc, the user's RISC OS documents folder, comes first, as the
roots reply names it; a session with none keeps `Choices` on a second memory
disc, held within `Scrap`'s bound.

- **The relay.** A call crosses to the side holding the files as a request
  of `hostfs::protocol`, carried by the kernel's `Host::relay`, and is
  answered there by `hostfs::server` over `lib/fscopy`'s tree seam: every name
  is reached from a held root, so nothing resolves outside one. Every field is
  bounded either way, and a reply is believed only where it answers what was
  asked within what that request could produce; one that does not, or a relay
  that fails, fails the call and the session goes on. `Scrap` is answered by a
  server of the worker's own over its in-memory tree.
- **Names.** A leaf crosses through `lib/fsmeta`'s one RISC OS conversion, a
  dot and a slash trading places as a space and a hard space do, so a host
  name with a space is listed with a hard space. A lookup ignores case as RISC
  OS's Latin-1 folds it: an exact name wins, two differing only in case are
  refused as ambiguous, and a host name RISC OS could not hold is left out of
  a listing.
- **Metadata.** A file's load and execution addresses and attributes are its
  volume's `acorn.*` attributes where it keeps them; else its type is a `,xxx`
  suffix, then the canonical filetype of what its name says it holds, then
  Data, its stamp its modification time and its attributes its mode bits. A
  change is kept the same way, and one nothing could keep — an untyped stamp
  or a lock where the volume keeps no attributes — is refused before a save
  empties the file it would replace. A file is opened only as its attributes
  allow, and a locked one is neither saved over, deleted nor renamed.
- **What may change.** A root the session may not write refuses every change
  as a protected disc does. Nothing on `Transfer` is made, deleted or renamed,
  and a delivered file is written only where it was delivered writable; where
  its volume keeps no attributes, its type stays the one its name gives.
- **The read cache.** A file's bytes cross a 4 KiB block at a time and are
  kept in a reclaimable cache under the version of its data its volume names —
  its identity and content generation — so every open of that version finds
  them and a file changed since is never answered from them; a file whose
  volume names no version, or one HostFS is writing, keeps them under a name
  of its open's own. `Scrap`'s files are read straight from the worker's
  memory.
- **Handles.** The server's handles carry the session's epoch, so one from an
  earlier session reaches nothing, and an open the worker cannot keep is let
  go at the server.

`fuzz_hostfs` sends the server hostile requests from a worker it cannot trust,
holds every decoded message to its own bytes, and runs a session whose relay
fails or damages its replies.

The corpus in `userland/apps/tairix-bridge/riscos/filing/tests/corpus/` holds FileSwitch to
RISC OS 5 call by call: a logging filing system registered from application
space as `TxLog`, loading and saving through `FSEntry_Open`, `TxRaw`, through
`FSEntry_File`, `TxBit`, an image filing system's host, and once for each
information-word flag — read-only, no special fields, always open,
`FSEntry_File` 9 and interactive — and as the image filing system of files of
type `&ABC`, records each entry
call it is given, its reason, names, counts and offsets but never an address,
a handle or the time, and answers by the name's last character. An `UpCallV`
claimant records the upcalls, and a `WrchV` claimant what a command prints.
Its scenarios run every name form, the directories, buffered and unbuffered
files, whole-file reasons, directory reads, every `OS_FSControl` reason,
copying, wiping, counting and renaming with their options, image files and
their hosts, the commands, stamping, and ResourceFS's files, directories
and refusals on genuine RISC OS 5 and on the emulator, and the logs must
match byte for byte.

### Running files

`*Run` and `OS_FSControl` 4 run the object a name finds along `Run$Path` as
RISC OS 5 runs it:

- **By kind.** An application directory, its leaf beginning `!`, runs its
  `!Run`; a directory of any other name, like a missing object, is not found,
  `&D6`.
- **An untyped file** is loaded at its load address and entered at its
  execution address. **An Absolute file**, type `&FF8`, is loaded and entered
  at `&8000`. Each runs as the application: FileSwitch starts one, then leaves
  the call for it. One that application space cannot hold, below `&8000` or
  past the memory limit, is refused before anything is started.
- **A Utility**, type `&FFC`, is loaded into the RMA with 1K of workspace
  after it and called in User mode: R0 the name as given, R1 its parameters,
  R3 its code, R12 the workspace and R13 its end. Its block goes once it
  returns.
- **Any other type** runs its `@RunType_XXX` alias, handed the file's
  canonical name and the parameters after it, which the alias's unused
  arguments pass on; a type with none is refused, `&409`.

FileSwitch starts with the run and load actions RISC OS 5 starts with — Obey,
Command, Text, BASIC, Module, Sprite, Desktop, TaskObey and TaskExec files —
whether or not the session has the command each one names.

The Obey module's `*Obey [-c] [-v] [<filename> [<parameters>]]` runs a file
of commands a line at a time through `OS_CLI`, reading each line from the
open file as the line before it finishes, or the whole file first where `-c`
asks; `-v` shows each line before it runs. A line ends at a CR, an LF or a
NUL, and the last one runs whether or not it ends. `%0` to `%9` and `%*0` to
`%*9` are the parameters, put in as `lib/obey` puts them; `Obey$Dir` is the
name as given up to its last full stop, `@` where it has none, and is left
set. The first line that fails ends the file with its error. `*Obey` with no
file does nothing. A program a line runs becomes the application and the
file goes on once it exits, as on RISC OS 5; a program that fails ends the
files it was started from.

Obey files, `*Repeat`'s walk and the boot sequence are scripts of the one
engine (`filing::script`): each runs its lines through `OS_CLI`, and one whose
line starts a program is held until the program exits, FileSwitch's exit
handler resuming the innermost first and ending each through `OS_Exit` as it
finishes, so the handler chain returns to whatever started it.

`run.scenarios` holds this to RISC OS 5: the run and load actions it starts
with, each way an Obey file is run and the parameters and `Obey$Dir` each
leaves, the lines an Obey file may end in, `*Run`'s refusals, `!Run`,
`Run$Path`, and a Utility's registers, shown as what they are to each other.

### BootCommands

BootCommands is the module RISC OS 5 keeps the boot sequence's and
applications' commands in, "Boot Commands" to `*Help`; each answers as RISC OS
5 was recorded answering.

- **`*X <command>`** runs the command and swallows its error, keeping its text
  in `X$Error` unless that is set already.
- **`*IfThere <object> Then <command> [Else <command>]`** runs the command for
  whether `OS_File` 17 finds the object, a name it refuses being not there;
  `Then` and `Else` are whole words in either case, split at the first `Else`.
- **`*Do <command>`** GSTranses the command once, then runs it.
- **`*Repeat <command> <directory> [<switches>]`** runs
  `<command> <directory>.<leaf>` on each object `-Directories`,
  `-Applications`, `-Files` and `-Type` choose between them, every object with
  none, a switch any leading part of its name in either case. As RISC OS 5's,
  it becomes the application to do it, so a program it runs hands the walk
  back its turn when it exits, and what called it is not returned to. RISC OS
  5 was not seen to return from a switch it does not know nor from
  `-CommandTail`: here an unknown switch is the syntax error, and the tail the
  next parameter, as the PRM gives it.
- **`*AddApp [<directory>.]<pattern>`** adds each object the pattern matches
  to `Resources:$.Apps` as a directory of Obey files stamped with the time,
  each handing on to the object: `!Run` always, `!Boot` running its `!Boot` or
  loading its `!Sprites`, and `!Help` opening its `!Help`, where it has them.
  Adding a leaf again replaces it, and what is added stays in ResourceFS for
  the session, through the module's restart, as on RISC OS 5.
- **The RMA and application space.** `*AddToRMA` changes the RMA by whole
  pages; `*ShrinkRMA` gives back every free page at its top, as `*AppSize`
  does whatever its size; `*AppSlot` makes application space its size from
  the free pool, and `*FreePool` gives it all to the pool. A size is read as
  RISC OS 5 reads one: the digits, `K` or `M` after them and whatever else
  follows let be, in 32 bits, so a size past half the address space asks for
  a shrink.
- **`*SaveCMOS <filename>`** writes the CMOS RAM's 256 bytes in logical order
  and a word naming the release, 530, as RISC OS 5 does: the file opened as
  `OS_Find` opens one to write afresh, written, closed and typed `&FF2`, a
  refused write unreported, so a locked file keeps its bytes and takes the
  type. `*LoadCMOS` leaves the running CMOS as it is, as RISC OS 5 was recorded
  leaving it.
- **`*SafeLogon`** hands the logon to the filing system's own `*Logon`.

`bootcommands.scenarios` holds them to RISC OS 5, `*Repeat` apart since it
ends the harness; `fuzz_bootcommands` runs each on hostile parameters, and the
boot sequence over hostile discs.

### The boot sequence

`filing::boot::start` readies the session's home once its modules are up. On
the shared session's own disc, the user's documents folder:

1. `!Boot` is laid out as RISC OS 5 lays it out — `Choices` with
   `Boot.PreDesk` and `Boot.Tasks`, `Resources` and `Library` — where the disc
   has none, and never written over where it has one. A `!Boot` that is a
   file is run in place of the rest.
2. The boot variables are set as the PRM's boot application sets them:
   `Boot$Dir`, `Boot$Path`, `Boot$OSVersion` (530), `Boot$State`
   (`commands`), `Boot$Unique` (`Local`), `Boot$ToBeLoaded`,
   `Boot$ToBeTasks`, and `Boot:Library.` after `Run$Path`; `Choices$Dir` and
   `Choices$Write` are `!Boot.Choices`, and `Choices$Path` reads from there.
3. `!Boot.Choices.Boot.PreDesktop` is obeyed, then each object in `PreDesk`
   run as the boot application runs it: files first, by type — Obey obeyed,
   Absolute and Utility run, sprites added with `*IconSprites`, modules
   loaded, BASIC run to its end, anything else `*Load`ed — then directories,
   by their `!Run`.

A line that fails is kept with its error and the boot goes on, the first 64
failures kept and the rest counted. A program a line starts becomes the
application and the boot resumes when it exits, its later failures read by
`boot::reported`. A session with no disc of its own
keeps `Choices:` on its memory disc and runs no boot. The desktop half,
`Resources` booted and `Tasks` started, is the Wimp's (RO39).

### FileCore

FileCore is each client's discs as a filing system, over
[`lib/filecore`](../lib/filecore.md), the format the ADFS driver shares. A
client — a module that moves sectors, RamFS among them — registers with
`FileCore_Create`, which makes an incarnation of FileCore named after the
client's filing system, `FileCore%RAM` for RamFS, ahead of the others, as RISC
OS 5 does. The incarnation's private word is the instance's, whose address the
client hands FileCore's SWIs in R8; FileCore's own, `FileCore%Base`, holds
none.

- **Discs.** A drive's disc is mounted through the client's `MiscOp` entry and
  its structures found as FileCore finds them, then kept between operations.
  A call into a drive while it is in use — from the client's own entries — is
  refused, "in use". A transfer moves whole sectors through the client's
  `DiscOp` entry and a part sector through the instance's buffer, reading only
  the sectors a write covers in part; it learns the sector size from the disc,
  mounted first where FileCore has not seen it, or from the record the caller
  names.
- **Names.** A disc of one character is the drive it numbers, or letters from
  `A`; a longer one is a disc's name, matched ignoring case as RISC OS folds
  Latin-1, wildcards and all. A part holding `@`, `^`, `\`, `&` or `%` is
  refused as a bad name; FileSwitch refuses the rest.
- **Objects** are placed, grown, settled and moved as `lib/filecore` does it,
  which is RISC OS 5's FileCore: allocation in whole sectors, small objects
  sharing a fragment, an open file taking every free byte, its entry giving
  that allocation as its length, and trimmed when it closes. A locked object
  is neither deleted, saved over nor renamed; an open one is neither deleted
  nor renamed, though the directory holding it may be; reading needs its
  owner's read access, writing nothing. A name changed only in case is
  re-spelt where it stands, and any other moves the entry to its sorted
  place.
- **Open files.** A file open on any handle to be written is open on no
  other, while one open only to be read may be on many; a locked file opened
  for update is opened only to be read. An open file holds its entry's place
  and its allocation, never a copy of its catalogue information, so a type or
  attributes given while it is open stand once it closes.
- **Paths that reach nothing.** A path through a missing directory or a file
  reads as nothing there: read, deleted or opened, it is not found without
  an error, while saving, making or moving to it is FileCore's "not found".
- **Errors** are numbered beside the client's filing system,
  `&10000 + number × &100 + code`, where its descriptor asks, as RamFS's does:
  "Locked" is `&117C3` on `RAM:`. FileCore's own, of its SWIs that name no
  client, are numbered beside its module, `&108xx`.
- **What holds a disc offset** (`OS_FSControl` 52): R2 answers 0 for free
  space or a defect, 1 for space no entry holds — the map, the root — and 2
  for an object, the first entry met whose fragment holds the offset. The
  name goes to R3's buffer as RISC OS 5 writes it: the root's path,
  `RAM::Probe.$`, unterminated; then, searching depth first, each directory's
  path as the search enters it, and the object's terminated, every write from
  the buffer's start, so what lies past a shorter one is an earlier one's.
  A byte of the root's path needs two more bytes of room past it, else "Buffer
  overflow"; one of the search's needs one more, and a separator or
  terminator without it is FileCore's "Buffer", `&A5`, while a name running
  out of room ends the search there, answering 2. Each directory is entered
  once, so a broken disc naming one from within itself is searched once
  rather than round its loop.
- **Defects** (`*Defect`, `OS_FSControl` 41, 42, 56 and 57). A defective
  sector is recorded as `lib/filecore` records one: listed in the boot block
  where the disc has one, and mapped out of free space. `*Defect` takes a hex
  sector start within the disc, else "Bad parameters", and leaves a sector an
  object or the disc's structures hold, saying what holds it;
  `OS_FSControl` 42 and 57 round the address down to its sector and refuse
  such a sector as FileCore's "Defect", `&94`. `OS_FSControl` 41 writes a word
  for each — the boot block's byte address, or, on a disc without one, the
  map's sector number — then `&20000000`; 56 each address in two words, with
  no end. One that does not fit is "Too many defects", what fits written. An
  old map records no defect, and refuses one.
- **Compacting a new map** (`*Compact`) moves fragments as RISC OS 5 moves
  them (`lib/filecore`'s `compact`); an old map's files slide down into the
  free space below each, but for one held open.
- **SWIs.** `DiscOp`, `SectorDiscOp` and `DiscOp64`, `Drives`, `FreeSpace` and
  `FreeSpace64`, `DescribeDisc`, `MiscOp` (reason 6 answered by FileCore
  itself), `Create`, `Features` (3), `DiscardReadSectorsCache`,
  `FloppyStructure`, `DiscFormat` and `LayoutStructure`.
- **Commands.** `*Backup`, `*Bye`, `*CheckMap`, `*Compact`, `*Defect`,
  `*Dismount`, `*Drive`, `*Free`, `*Map`, `*Mount`, `*NameDisc` and
  `*NameDisk`, and `*Verify`. A disc parameter may begin with a colon. `*Free`
  and `*NameDisc` read it as FileSwitch reads a disc, so a path on the disc
  may follow and a name FileSwitch would not hand on is its bad name, `&CC`;
  the rest read it as FileCore does, a colon or full stop within it FileCore's
  bad name, as RISC OS 5's commands read theirs.
- **Structures.** `DiscFormat` fills in each format's layout — sector size,
  gaps, sectors a track, density, options, skews, fill and tracks — before its
  vetting SWI adjusts it. `FloppyStructure` lays a floppy's start out as RISC
  OS 5 does, whatever the record's zones: an old map where R2's bit 6 or 7
  asks, the root in the old layout where bit 7 does and marked `Hugo` where
  bit 6 does, and a new map laid as one zone's, its fragments run through the
  map's bits as one stream and the defects R3 lists swept in order.
  `LayoutStructure` lays an empty new-map disc out in an image file with no
  boot block, its fixed root named after the disc, its defects mapped out.
  A record describing more disc than its zones, to `FloppyStructure`, or
  more than an image file's 32-bit offsets reach, to `LayoutStructure`, is
  "Bad parameters".

Where RISC OS 5 itself goes wrong, the bridge does not follow it:
`LayoutStructure` lays out zone 0 of the map, which RISC OS 5 5.30 leaves
without a fragment, and refuses an old-map record, which it lays out as a
malformed new map; a defect list past the 111 a boot block holds is "Too many
defects", where RISC OS 5 writes on over the boot record; and a 64-bit
`OS_FSControl` 57 address past 512 MB, a FloppyStructure defect at or past the
disc's end, and `OS_FSControl` 52's search running out of room while it
searches the map's own space are answered as the rest are, where RISC OS 5
aborts or lays out free space past the disc. `DiscOp64`'s block is read as a
drive, then the address's low and high words: no guest can reach a FileCore
instance to record it. `filecore.scenarios` in the same corpus holds the rest
to RISC OS 5: object names at offsets and their buffer's bounds, both kinds of
defect list, defects mapped out against what bounds them, compaction over
nine layouts, the floppy structures, the parts of a laid-out structure RISC OS
5 lays out soundly, and a big directory's stored name through renames.

### RamFS

RamFS is FileCore's client for the RAM disc: area 5, "RAM disc", a pool of
pages the kernel makes at reset as large as `*Configure RAMFsSize` says, its
handler the kernel's and its workspace the area's base, as RISC OS 5's is. The
handler refuses a resize, `&1C3`, while the disc holds anything, a file or a
directory; otherwise it stops RamFS, resizes the pool — the window's pages
showing what it gives up unmapped first — and starts RamFS again on the new
size, where RamFS lays a fresh disc out. An empty area is no disc: RamFS
refuses to start on one, and `RAM:` is gone until the area grows. RamFS maps
a page of the disc into the area's window only when it first reaches it.

The disc is RISC OS 5's RamFS's, laid out by `lib/filecore`'s RAM disc
variant, its id fresh at every format.

`userland/apps/tairix-bridge/riscos/filing/tests/corpus/ramfs.scenarios` holds RamFS and
FileCore to RISC OS 5: the disc laid out at every size from 256K to 24M,
where files are placed and in what they are allocated, how an open file grows
and is settled, the errors and renames, which names FileSwitch and FileCore
refuse, files moving between directories, the resizing and the disc's name
and boot option, and the disc specifiers — shown without a stamp, the disc's
id or a name a ROM holds, and replayed byte for byte. `open-files.scenarios`
holds open files and paths that reach nothing to it too. `fuzz_ramfs`
scribbles on the disc's map, root and pages and drives the filing calls over
it.

## BASIC

BBC BASIC V and VI are the `userland/apps/tairix-bridge/riscos/basic` crate:
the BASIC and BASIC64 modules, each started as the application by `*BASIC`
or `*BASIC64` with `-help`, `-chain`, `-load` or `-quit`.

- **A resumable machine.** The program, the variables, the heap and the
  BASIC stack lie in guest memory as BASIC V lays them out, the workspace at
  the offsets `CALL`'s routine table gives, its exception word holding the
  Escape flag (`&80`) and the trace flag (`&8000`). Every value an
  expression is working on and every statement waiting on one is on that
  stack, so BASIC never recurses on the host and can stop at any statement.
- **Its memory is the program's to overwrite.** A workspace word or stack
  record a program writes over meets an error where BASIC next uses it — a
  stack it did not leave is "Corruption of stack", a wild address a data
  abort — or wraps as ARM arithmetic does; it never ends the session.
- **Errors** carry BASIC V's numbers, each message the BBC BASIC Reference
  Manual's text where it gives one and TAIRiX Bridge's own words otherwise.
  An untrapped error's report opens a line of its own in an immediate line;
  a program's follows whatever it printed last, then gives the line.
- **The machine interface.** `CALL` and `USR` enter code with A% to H% in
  R0 to R7 and the routine table at R14; `SYS` takes a SWI by number or
  name, an unknown name raised through the OS.

### BASIC V's reals

A real is five bytes: a 32-bit mantissa, little-endian, its top bit standing
for the sign, then an excess-128 exponent. Only the all-zero pattern is zero,
so there is no negative zero and 2^-129 has no form of its own.

- **Arithmetic** rounds each exact result once, to nearest with ties to odd,
  but for a subtraction whose result keeps the larger operand's exponent,
  which BASIC V truncates; an operand more than 32 places below the other is
  ignored. A result past the exponent's range is `Number too big`; one below
  it is zero.
- **Conversions.** An assignment to an integer truncates and takes −2^31;
  `INT` floors and refuses any magnitude of 2^31. An integer product that
  overflows becomes a real, rounded as arithmetic rounds.
- **Powers.** An integer exponent from −64 to 63 — or a real one holding
  such an integer — is taken by squares, multiplied from the highest set
  bit down, after the reciprocal for a negative one. Any other is a real
  power, whose faults follow BASIC V's two parts: below 64 in magnitude its
  integer part by squares first, then the logarithm of the base.
- **Functions.** `SIN`, `COS`, `TAN`, `ATN`, `ASN`, `ACS`, `EXP`, `LN`, `LOG`
  and the real power are rounded correctly, from `lib/softfloat`'s elementary
  functions, where BASIC V's own are a place or two off and further near a
  multiple of π/2. Each refuses where BASIC V does: `Accuracy lost` from
  2^23, `Exponent range` from 89.5, `Number too big` past one for an
  arcsine or arccosine, `Logarithm range` at or below zero. `RAD` and `DEG`
  multiply by BASIC V's own `PI`/180 and 180/`PI`, exactly.

### BASIC VI's reals

A real is a double kept as the FPA transfers one, the high word first. Every
operation and function is the FPA's instruction in double precision (see
[The FPA](#the-fpa)), run in an FPA of BASIC VI's own whose status the
program's is carried in and out of: an exception that status traps is the
floating-point emulator's error, and only a division by zero is BASIC's own.
Its unary minus is zero less the value, so it gives no negative zero.

### Numbers as text

- **Reading.** `VAL` skips spaces, takes a sign, then digits with perhaps a
  point and an exponent: an upper-case `E`, perhaps a sign, and two digits
  in BASIC V or three in BASIC VI. BASIC V reads every digit, rounded
  correctly; BASIC V's own reading keeps a little less than every bit, and a
  value at or just above a tie it reads a place low. BASIC VI reads the
  first 19 through the FPA's packed decimal load, rounded to a double.
- **Writing.** BASIC V's digits are its exact value rounded half up; BASIC
  VI's are the FPA's packed decimal store's 17, rounded half up again to
  fewer. `@%`'s byte 2 chooses the layout — 1 exponent, 2 fixed, anything
  else general — its top bit a decimal comma; byte 1 gives the digits, none
  or too many meaning the most, 11 or 18. The general layout is fixed
  notation from 0.01 to the digits' reach; the exponent layout keeps every
  digit and left-justifies the exponent in three places, four in BASIC VI; a
  fixed layout whose places would pass the most digits is the general one.
  `STR$` takes `@%`'s layout only where its bit 24 is set.
- **`@%` as a string.** `@%="+G10.9"` is read as genuine BASIC reads it: a
  leading `+` sets bit 24 and its absence clears it; then `G`, `E` or `F` in
  either case the form, keeping the decimal comma; the field width; and after
  `.` or `,`, which clear or set the comma, the digits, each a byte and each
  only where given. The first byte out of place ends it, keeping what it had
  not reached.

### The prompt and its commands

The prompt reads a line from the keyboard: a numbered one is stored, any
other run. It starts a line of its own only where the VDU drivers report the
cursor mid-line, never by `COUNT`, so under output redirection it never does.
`LISTO` starts at 0. Every command is a syntax error in a running program;
at the prompt:

- **`AUTO [start][,step]`** prompts with each line's number, right-aligned in
  five places, and stores what is typed under it, an empty line deleting it.
  It ends past line 65279, at Escape or at an error.
- **`RENUMBER [start][,step]`** renumbers every line and line reference,
  reporting a reference to a missing line by the line's new number. A step
  of nought, a number past 65279 or numbers that would pass it are refused,
  as **`DELETE first,last`** refuses a number past 65279.
- **`LISTO n`** takes 0 to 31; **`CRUNCH n`** strips as its bits ask.
- **`LVAR`** lists `@%` as the string that would set it and the resident
  integers, then the variables with their values or bounds, the procedures
  and functions known and each library's first line: four columns of 20, an
  item at the next column past the one before, numbers in nine digits'
  general form whatever `@%` says.
- **`HELP`** gives a keyword's topic, every keyword (`HELP .`) or those a
  letter begins in rows of eight, `@%`'s topic, the assembler's (`HELP [`),
  or for anything else the general text and the program's sizes, all in
  TAIRiX Bridge's own words; a keyword it has no topic for, such as `TWIN`,
  is reported in the manual's words. `TWIN` and `TWINO` are `Mistake`, as
  RISC OS 5 has no Twin.
- **`EDIT`** issues the two commands RISC OS 5's BASIC does: `RMEnsure
  ARMBasicEditor 0 RMLoad System:Modules.BasicEdit`, then `ARMBE`, which
  starts the separate ARM BASIC Editor module. RISC OS 5 provides no such
  module and neither does TAIRiX Bridge; without one the OS reports what it
  lacks.

### The assembler

`[` … `]` assembles what RISC OS 5's BASIC assembler accepts — ARMv2 to
ARMv8's AArch32 with the FPA, VFP and Advanced SIMD, but not the
Cryptographic Extension — encoding each instruction through `arm`'s tables.

- **A statement is parsed afresh** each time an operand's value arrives, from
  its text and the values read so far, so an operand may call a function that
  runs BASIC. `OPT`'s bits list (1), report errors (2), lay code at `O%` (4)
  and stop at `L%` (8).
- **Its quirks are genuine BASIC's**, among them: a stem followed by a
  condition wins over a longer stem (`BLE` is `B` with `LE`); instructions
  align `P%` and `O%`, data never; with errors suppressed an unknown variable
  is `P%`, a bad immediate lays down its word ORed with the value, and a bad
  offset becomes nought; `SMC` and `HVC` refuse a number past their field, and
  `HVC` any condition but `AL`; a bitfield must lie within the word, and an
  insertion or a clear of no bits may not start at bit nought, errors
  suppressed or not.
- **The listing** under `OPT` 1 gives each statement's address, up to four
  bytes as a number, and its text as `LIST` spells it, a label in a field of
  ten; only the text counts toward `COUNT` and `WIDTH`.
- **One divergence**: a nested `[` inside a function an operand calls
  assembles as it would anywhere, where genuine BASIC corrupts the outer
  instruction.

### The replays

Each corpus in the crate's `tests/riscos5/` is recorded from genuine BASIC V
and BASIC VI by the reference harness and replayed against both dialects:

- `numbers.txt` holds what they answered the harness's cases, replayed by
  `tests/riscos5.rs` exactly, but for a transcendental, held to two places
  and those its condition number magnifies an argument's last into; a reading
  at a tie, held to BASIC V's place within 2^-6 of a place above it; and
  BASIC VI's functions and packed decimal conversions, held to FPEmulator's
  error.
- `programs.txt` and `printed.txt` hold programs run with `-quit` and what
  each printed, byte for byte (`tests/riscos5_language.rs`).
- `assembler.txt` and `assembled.txt` hold assembler lines, each assembled
  alone at `P%`, at `O%` as at `&10000`, or with errors suppressed, and the
  bytes each laid down or the error it met (`tests/riscos5_assembler.rs`).
- `sessions.txt` and `typed.txt` hold lines typed at the prompt and what was
  printed, each message told by the kind a calibration session shows it to
  be, so neither BASIC's messages nor the emulator's own words for them are
  compared (`tests/riscos5_sessions.rs`).

`fuzz_numbers` reads any text as a number, runs every operation on any
operands, and holds `STR$`'s own layout to reading back as the value it
wrote. `fuzz_basic` runs programs of BASIC's keywords and operands, or any
text, in both dialects, and holds any assembler statement under any `OPT` to
raising an error or moving `P%` and `O%` on by no more than a string after an
alignment.

## The VDU drivers

The VDU drivers are the `userland/apps/tairix-bridge/riscos/vdu` crate, which
the kernel reaches through its `VduDrivers` seam: they start at reset, before
any module, take the VDU stream, perform the VDU SWIs, answer their `OS_Byte`
and `OS_Word` reasons, count the screen's frames on the clock's interrupt and
are `PaletteV`'s default. Their state is the session's screen; what software
reads in zero page — the colour blocks at `&1480`, `&14C0` and `&1500`, the
ECF offsets at `&11F8` and `&11FC`, the changed box at `&165C` — is kept
there.

- **Modes.** Numbered modes come from `lib/vdu`, selectors and mode strings
  from `OS_ScreenMode`. The current monitor type starts as the configured one;
  on a VGA monitor a numbered mode it lacks is shown as the 640 by 480 mode of
  its depth where that keeps its eigen factors and flags, else as a 640 by 480
  selector keeping them. A selector is shown where the monitor has timings for
  its size, or a module gives them; ScreenModes, after `*LoadModeFile`, gives
  them for its file's modes and makes the monitor type 7. A change issues
  `Service_PreModeChange`, `Service_ModeChanging` and `Service_ModeChange`.
  A selector's text not set by its variables is as many whole cells as fit
  the screen. One whose variables make no screen the drivers can draw — eigen
  factors past 3, colours its depth lacks, a pixel wider than a word or
  narrower than its colours, lines too short for their pixels, text past the
  screen, or Teletext in other than sixteen colours or past a page of 255 by
  255 cells — is refused as a bad mode.
- **Screen memory.** Dynamic area 2, mapped twice, so a screen a hardware
  scroll has moved reads on past the area's end; it grows for a mode that
  needs more and shrinks no further than the current screen's banks.
  `OS_Byte` 112 and 113 choose the banks, `OS_Word` 22 their starts; every
  page is watched, and the display takes the lines written since it last
  asked.
- **Text.** The text window and cursor run the directions `VDU 23,16` sets;
  VDU 5 prints at the graphics cursor in the graphics colour, wrapping where
  the cursor leaves the window. A full-screen window scrolls by moving the
  screen; any other by copying. Paged mode waits for Shift before about three
  quarters of a window, and Ctrl with Shift holds any scroll. The copy cursor
  turns with the window's axes as the text cursor does, and copy-cursor
  editing ends when `VDU 28` or `VDU 26` sets a window.
- **Colours.** A colour number is masked to the mode's colours below eight
  bits; in eight bits it is the VIDC1 form, the tint the two low bits; in
  sixteen and thirty-two bits that form's default colour, packed. The colour
  blocks are each GCOL action on the colour or ECF pattern row, rotated for
  the ECF origin: the shift is `-(x << Log2BPC)` modulo 32, the row offset
  the screen's height less `y`, modulo 8.
- **`OS_Plot`.** Lines walk from their leftmost or lowest end; polygons fill
  between their edges' lines; circles soften their squared radius by its
  root. Ellipses are worked in RISC OS's fixed point — the width's ratio to
  the height to eight fractional bits, each row's root to eight, the shear
  stepped at sixteen — so a tall, narrow ellipse is drawn a little narrower
  than its width point says. An arc ends where the ray toward its end point
  leaves the circle; a sector's rays run one pixel past a corner of the
  circle where the sweep closes on them; a segment's chord runs to where the
  end direction meets the circle, each coordinate cut toward nought. Every
  shape is worked out within the window alone: a line's pixels and a
  polygon's edges from their places along them, a round shape's rows as each
  is asked for, and where a ray leaves a circle by halving, so a shape far
  larger than the screen costs what the window holds. A point more than 2²⁸
  pixels from the screen's corner is held there, where genuine RISC OS's
  sums would wrap. Then the flood and line fills, block copy and move, and
  the dotted lines, whose dots restart when the pattern or its length is set
  and run on across a line's part beyond the window.
- **Segments** differ from genuine RISC OS by a pixel or a few at the
  chord's end in about one geometry in eight, mostly where the arc ends in
  the second or fourth quadrant: a known difference
  (`plans/RISCOS-EMULATOR.md` RO21.2).
- **The cursor** is drawn by the front end over the screen, never into its
  memory, so a program reading its screen never finds it there, where genuine
  RISC OS EORs it in.
- **Teletext.** Mode 7, or a Teletext selector, is a page of characters drawn
  into a sixteen-colour screen from `lib/teletext`, each cell 16 by 20 pixels
  whatever the mode's flags, twice over, the second frame without its
  flashing characters; a page taller than the screen is drawn to its bottom
  line.

The screen corpus, `vdu/tests/riscos5/screens.txt`, is BASIC programs genuine
RISC OS 5.30 ran on its screen for the reference harness, each defining the
whole character set from the first-party font first, so no RISC OS glyph is
in any screen; `drawn.txt` holds what each read and every line of the screen
it drew, which the replay holds the drivers to byte for byte.

## The reference harness

`cargo xtask riscos --reference <dir>` records each crate's corpus —
`userland/apps/tairix-bridge/riscos/kernel/tests/corpus/` and
`userland/apps/tairix-bridge/riscos/filing/tests/corpus/` — on a genuine RISC OS. `<dir>` is
laid out as RPCEmu lays its own out: `roms/` the ROM images, `hostfs/` the
directory the machine boots from (`*Configure FileSystem HostFS`,
`*Configure Boot`).

- **Scenarios** are SWI calls with their registers, values kept from earlier
  calls, blocks of memory made for them (every byte &EE until written) and
  bytes written into those, and what to record: registers, only the flags the
  SWI defines, since the rest are its caller's, a register less a kept
  address, bytes of memory at a kept address or a register's, and a word
  there less a kept address — a pointer into a block. A scenario records
  nothing that depends on what else the machine holds, such as an area's
  number or a block's address, and memory at a register only where the call
  succeeded. The format is documented in the kernel's `corpus` module.
- **The harness** is a BASIC V program tokenised with `lib/openbasic`, with a
  `!Boot` that runs it, never written over a `!Boot` of the machine's own and
  removed with the harness once the recording is taken. It writes each `show`
  as one line: the call's ordinal, each item as `KEY=VALUE` — a register,
  difference or pointer as `&XXXXXXXX`, a flag as `0` or `1`, bytes as hex —
  and `error=&N` whenever V is set. After the scenarios it runs `lib/obey`'s
  cases (`lib/obey/tests/riscos5/cases.txt`), each a call or a command, and
  writes each answer beside its case in `answers.txt` for the crate's replay;
  the kinds and the answers' form are documented in
  `tools/xtask/src/commands/riscos/obey.rs`. Errors are recorded by number,
  never their text.
- **BASIC V.** Before the harness, the `!Boot` has BASIC V type
  `lib/openbasic`'s programs (`lib/openbasic/tests/riscos5/programs.txt`),
  saving and listing each. What BASIC made of every line and listing goes to
  `answers.txt` beside them, for the crate's replay
  (`tools/xtask/src/commands/riscos/basic.rs`). A calibration program gives
  each message BASIC prints, and every later one is recorded by the kind it
  matches, never as its text.
- **BASIC's numbers.** Then BASIC V and BASIC VI evaluate the cases
  `tools/xtask/src/commands/riscos/numbers.rs` draws from a fixed seed —
  arithmetic, conversions, powers, functions and their refusals, `VAL`,
  literals and `STR$` under `@%` — each answer written as the bytes of its
  value, its text or its error's number, BASIC VI's `STR$` with the words of
  the packed decimal store behind it. An operand given as text is recorded as
  the bytes BASIC read it as.
- **BASIC's language, assembler and prompt.** Then each dialect runs the
  crate's programs with `-quit`, each printing only its own text and numbers
  (`tools/xtask/src/commands/riscos/language.rs`); assembles its assembler
  lines in programs of a thousand built by the crate's own probe program,
  writing each line's bytes or error (`riscos/assembler.rs`); and types its
  sessions at the prompt after a calibration session, telling every message
  by the kind the calibration shows it to be (`riscos/session.rs`). The
  formats are the crate's `corpus` module's, so the harness and the replays
  read them alike.
- **Running.** Where `<dir>` holds RPCEmu, it runs headless, as one of the
  command's own children, until the harness writes a byte to a named pipe
  once it is done, the emulator exits, or twenty minutes pass. Otherwise the
  harness is left in `hostfs/`; boot the machine from it and run the command
  again to import what it recorded. Every scenario runs on the one machine in
  the corpus's order, but one marked `lasting`, which changes the machine in a
  way RISC OS gives no means to undo, runs after all the others.
- **Importing.** Each observation becomes its scenario's fixture. A scenario
  marked `ours`, whose fixture states this emulator's own behaviour, is left
  out of the harness, so nothing it would do to genuine RISC OS stops a run.
  One marked `reference` reads the machine itself: it is recorded and never
  replayed, a test of its own holding what the emulator records of the
  machine to it. A recording is refused whole if it carries twelve bytes in a
  row of a ROM image in `roms/` that the corpus's own files do not hold: in
  its text, in the words a step shows in consecutive registers, or in the
  bytes the machine wrote into a scenario's memory or a `lib/obey` case's
  buffer — what lies between the bytes left &EE. A run mostly of
  `&00`, spaces and `&FF` is a small value or a listing's layout any ROM
  holds, not its content, and is not counted.
- **Provenance.** Each file the harness writes opens with the RISC OS version
  it ran on and each ROM image's SHA-256.
- **Texts.** A text the guest produces from RISC OS's own message files or
  tables is never recorded: a scenario compares it in the guest, with
  `Territory_Collate` against the text the emulator expects, or records its
  length, and only that answer is kept. A scenario that needs a SWI by number
  issues it from a vector's claimant written into its own block.

The corpus was recorded under RPCEmu 0.9.5, whose machine runs RISC OS's IOMD
build. `locate-the-compatibility-page` and `compatibility-page-state` are
`ours`: that build keeps zero page at zero, readable from User mode, while
the 32-bit profile has a compatibility page there; and the access privileges
past the four every core has are the ARMv7 profile's, which that build lacks,
so the corpus holds only those four. `squeeze-length-past-the-end` is `ours`
too: genuine RISC OS 5 reads past the copy it makes of that module, and
recorded alone it refuses it as this does, but after other scenarios it
faults on whatever lies beyond. `bootcommands-add-app` is `lasting`: the
applications `*AddApp` puts in ResourceFS stay there for the session, and
after it has added three from the RAM disc, a later RAM disc's shrink can
fault in FileCore, how readily depending on what the RMA has held.
`fileswitch-images` is `lasting` too: once its host is removed with an image
open on it, RISC OS 5 keeps a record of the host that nothing removes. The
replay runs in the UK's time, as the reference machine's territory does. A
crate test holds every scenario to one
fixture in the harness's format, and another replays every scenario on the
kernel's own SWIs, the names resolved by its `OS_SWINumberFromString`, and
holds what each leaves to its fixture.
