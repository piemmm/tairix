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
  it, for the session to raise FPEmulator's error; with no floating point
  context current, every FPA instruction is undefined instead;
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
  FPEXC.EN clear, though never past CPACR. `Cpu::grant_simdfp` sets the grant
  as a privileged `MCR` writing both fields would, for VFPSupport.
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
enters the error handler in User mode, R0 its workspace and R1 to R14 as the
failing code had them. A block that cannot be one is replaced with "SWI &n
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
  module's Rust, so a whole copy of an image, as `*RMFaster` makes one, is the
  same built-in, freed as a module of the RMA is; the kernel itself calls a
  built-in's Rust directly. Rust acting for code that branched to an entry
  reaches what that code hands it with the rights of the code's mode, and a
  SWI's work with a privileged mode's; an access refused aborts as the entry's
  own instruction would, reported at the entry. A session's built-ins are the
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
| BlendTable | 0.02 | provided | RO23 |
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
| Free | 0.42 | provided | RO63 |
| Hourglass | 2.19 | bridged | RO61 |
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
as it does for RPCEmu's own HostFS on RISC OS 5. There the emulator lists its
HostFS (1.00, bridged, RO19.4), then VFPSupport (0.18) and CallASWI (0.15),
which RISC OS 5 loads from disc for a processor its ROM does not serve (RO26.7).
No CMOS bit unplugs them. R5 gives each one's chunk number, from 1 in the
card's order, as genuine RISC OS 5 numbers RPCEmu's. `*ROMModules` and
`*Unplug` walk the whole enumeration, the card after the ROM, and
`*ROMModules` lists the card's modules as `Podule 0`, numbered from 1 again,
as the PRM's example lists a card's.

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
  FileSwitch refuses (`&CC`) a name with no path — but on a filing system that
  takes an empty name, as `kbd:` does, which is handed one — and one whose part past the
  first is a directory's sign, begins with one, `:` or `|` — the parent `^`
  apart — or holds `$`, `:` or `|`, and a disc's name holding a colon, as RISC
  OS 5 refuses them; whatever else
  a filing system cannot hold is its own to refuse. A disc's operation
  (`OS_FSControl` 41 to 57) alone takes no path as the current directory.
- **Open files.** Handles are the highest free from 255 down, the one an
  open takes held while its filing system works, so a file that filing
  system opens meanwhile takes the next, as on RISC OS 5. `FSEntry_Open` is
  given the `OS_Find` mode in R7, `&CC` for FileSwitch's own opens to update
  and `&4C` to read. An open the filing system answers with no handle is
  "unable to open", `&D6`, where the mode asks for an error. A buffered file
  has one buffer of its natural block size in the RMA; FileSwitch keeps its
  pointer, extent and allocation, moves whole blocks within the extent
  straight between the filing system and the caller's memory and the rest
  through the buffer, extends a file as RISC OS 5 does — the block holding the
  old end zeroed, the space ensured, whole blocks after it written as zeros —
  and on closing a file written to sets its extent and renews its stamp. An
  unbuffered file's pointer and extent are its filing system's, which is told
  of each flush. The first read at a file's end answers with C set; the next is
  "End of file", from FileSwitch's own flag; a read of a file not opened to be
  read is `&413`. `OS_Args` 254's status word says read, written, modified, at
  its end, unbuffered or unallocated, and interactive and served by its filing
  system's own `FSEntry_GBPB` as the open's answer said. A close the filing
  system refuses leaves the file open; `*Shut` and `OS_FSControl` 22 close
  every file from the highest handle down, each still open when its turn
  comes, every one tried though one refuses and the first refusal answered,
  as RISC OS 5 closes them. A buffered
  file opened for update has its extent set as it closes. A file opened for
  update is written only where its filing system's answer permits it, which
  FileCore's for a locked file does not. A file is restamped only if modified
  — written, or refused an `OS_GBPB` write, which RISC OS 5 counts as a
  change. An ensure is not one. An openout of a missing file makes it by
  `FSEntry_File` 7 first; a make the filing system refuses goes unreported,
  the open after it answering, so one in a missing directory is FileSwitch's
  "unable to open", `&D6`, as on RISC OS 5.
- **Whole files.** Every reason of `OS_File` reads its object first: a missing
  one is never handed to the filing system, but by reasons 1 to 4 to a filing
  system always called to open a file, which is asked all the same, as RISC
  OS 5 asks `null:` and `pipe:`. Reasons 2 to 4 write through
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
  filing system; an empty name, on a filing system that takes one, is that
  filing system's own directory, as RISC OS 5 lists `null:`. Reason 12 does
  as RISC OS 5 does: the filing system's
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

The SWI and service numbers, file types, object kinds, attributes and
catalogue information, the listing layout and the information block a filing
system is declared with, which FileSwitch and the filing systems share with
the kernel, are the kernel's tables:
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

### DeviceFS

DeviceFS 0.73 is `devices:`, filing system `&35`: every registered driver's
devices as files of type `&FCC` in `$`, and the streams opened on them, held
to RISC OS 5's module as recorded.

- **Drivers.** `DeviceFS_Register` takes a device list, the driver's entry,
  private word and workspace, a validation string and the streams it takes
  each way (none, one, or any for -1); its handle is a block of the RMA. A
  block device is refused, `&13509`. Each device gets `DeviceFS$<name>$Options`,
  and with the list's bit 1 the macro `<name>$Path`, `devices#:$.<name>.`; a
  name registered again replaces its device. `DeviceFS_Deregister` closes the
  driver's streams and tells it with `Finalise` R2 0; the option variables are
  kept. `RegisterObjects` and `DeregisterObjects` add and remove devices,
  `CallDevice` reaches a driver by handle, by a path's device (R1 its name
  within the path) or every driver for nought, and the services
  (`DeviceFSStarting` on a callback at its start, `DeviceFSDying`,
  `DeviceDead`, `DeviceFSCloseRequest`) are issued as RISC OS 5 issues them.
- **Streams.** An open calls `Initialise` with the stream's handle — a
  block of the RMA, its poll word at `&34` — its flags, the file's handle,
  the special block and the open's mode; then, for a buffered device,
  `CreateBufferTX`/`RX` and a BufferManager buffer, a `bufferN` special field
  naming one to use; then `StreamCreated` and `UpCall` 10. A refused open
  gives `UpCall` 11 for the stream it tried. A close drains what it sends,
  then `EndOfData`, `UpCall` 7, `Finalise` — a driver's refusal answered and
  the stream left open — the buffer let go and `UpCall` 11.
- **Special fields.** A validation string's commands follow one another,
  each its keywords and one escape a word (`N` a number, `S` a switch, the
  keyword's place): the special field's `;`-separated items are matched
  without case, a keyword whole, then `DeviceFS$<name>$Options`, the first
  match winning. A number is read decimal past an `=`.
- **Bytes.** An unbuffered stream sends a byte through a one-byte slot,
  `WakeupTX` asked to take it with `TransmitCharacter`; one receives the
  last `ReceivedCharacter` gave. A buffered one moves bytes through its
  buffer, `TX`'s wake-up routine entered with interrupts off. Reads wait
  without `UpCall` 6 unless the stream's sleeping I/O is set; writes into a
  full buffer issue it each round; Escape
  ends a wait, acknowledged, with "Escape", a write's buffer emptied.
  `OS_Args` 9's I/O controls set a stream's non-blocking and sleeping I/O
  and timeout, or go to the driver; a driver whose register flag asks is
  called to monitor each stream as it waits.

Known differences from RISC OS 5, each deliberate:

- a stream whose device has gone answers "device not found" where RISC OS 5
  hangs, and still closes;
- `TransmitCharacter`, `ReceivedCharacter` and `Threshold` refuse a handle no
  stream has, and `Threshold` an unbuffered stream, where RISC OS 5 aborts;
- Escape during a write is "Escape", and a directory read of a device
  FileSwitch's "is a file", `&41C`, where RISC OS 5 answers each with a
  bogus error pointer;
- a stream that waits is monitored each second; a byte write is never cut
  short by its I/O controls; an I/O control of DeviceFS's own reason it does
  not know is "bad operation".

### SystemDevices

SystemDevices 1.34 is six filing systems, every one interactive, taking an
empty name and called to open whatever is not there: `null:` (`&D`),
`printer:` (`&E`), `vdu:` (`&11`), `rawvdu:` (`&12`), `kbd:` (`&13`) and
`rawkbd:` (`&14`). No device has files: every name reads as nothing there,
a load is "not found", a directory made or a deletion taken, and each
refuses what it cannot do, `&112`.

- **`null:`** opens every way, gives nothing — its reads at their end — and
  takes everything.
- **`vdu:` and `rawvdu:`** write each byte through `OS_WriteC` with
  `OS_Byte` 3 set to `&14`, the screen alone, put back after: `rawvdu:` as it
  is, `vdu:` as `*Type` shows it in the `DumpFormat` SystemDevices read at its
  start, tabs never expanded, a CR LF pair one newline across streams.
- **`kbd:`** opens once: a second open answers no handle. It reads a line by
  `OS_ReadLine` — 255 characters, every control character kept, its editing
  and echo `OS_ReadLine`'s — and gives it, then its Return; a Ctrl-D is its
  end, and what is left of a line goes with its close. **`rawkbd:`** opens
  any number of times and reads each key by `OS_ReadC`, unechoed. Escape
  during either is "Escape", acknowledged.
- **`printer:`** opens to write the file `PrinterType$n` names for the
  printer type its special field gives — `sink`/`null` 0, `parallel`/
  `centronics` 1, `serial`/`rs423` 2, `user` 3, or a number to 255 — or the
  configured type without one; a type no file is set for takes everything.
  A name it does not know is `&10E02`. Opens within opens stop at eight
  deep, so a `PrinterType$n` naming `printer:` itself cannot run away.

Known differences from RISC OS 5, each deliberate:

- RISC OS 5 on RPCEmu does not route `printer:` by its special field as the
  PRM describes; the PRM's routing is followed;
- a `printer:` stream whose file was closed under it still closes.

### PipeFS

PipeFS 0.25 is `pipe:`, filing system `&2F`: named pipes, each opened once
to read and once to write, which keep what is written until it is read.
A dotted name makes directories of its parts.

- **A pipe** is a ring of 256 bytes in the RMA, which grows by 256 when a
  write finds it full, and three poll words: the reader's, the writer's and
  one nought while a writer has it. A reader's pointer is the bytes taken
  since the pipe was made and its extent those left once no writer has it;
  a writer's pointer and extent are the bytes written, an extent set further
  written as noughts. A pipe stays with what it holds when both close, its
  writer appending; a reader that leaves it empty with no writer takes it
  away. Its catalogue length is what is left to read; `OS_File` 5's what was
  written. A save makes it afresh; a load takes it.
- **Waits.** A read of an empty pipe that a writer may still fill issues
  `UpCall` 6 on the reader's poll word; with no task switcher to take it, the
  read fails, `&12F00`. A write to a full pipe issues it on the writer's;
  untaken, the pipe grows. A reader's extent and `OS_File` 5 wait on the
  third while a writer has the pipe, the latter "pipe open", `&12FC2`, untaken.
  Each close and deletion issues `UpCall` 7 for the poll words it ends.
- **`*PipeCopy`** copies a file a byte at a time to one or two others.

Known differences from RISC OS 5, each deliberate:

- a poll word is non-zero exactly when its waiter may go on;
- a pipe past its first 256 bytes takes room in the RMA twice as fast as it
  grows, so a large one costs no more than its bytes to grow;
- `*PipeCopy` stops at Escape.

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
  them for its file's modes and makes the monitor type 7, which has no
  numbered mode of its own: one is shown as the file's mode nearest its size,
  at its depth, `Service_ModeTranslation`'s answer. A change issues
  `Service_PreModeChange`, `Service_ModeChanging` and `Service_ModeChange`.
  A selector's text not set by its variables is as many whole cells as fit
  the screen. One whose variables make no screen the drivers can draw — eigen
  factors past 3, colours its depth lacks, a pixel wider than a word or
  narrower than its colours, lines too short for their pixels, text past the
  screen, or Teletext in other than sixteen colours or past a page of 255 by
  255 cells — is refused as a bad mode. An eight-bit mode holds 256 palette
  entries of its own where its mode flags say so, whatever colour count it
  states, and VIDC1's groups of sixteen otherwise.
- **ScreenModes.** `*LoadModeFile` reads a monitor definition file as
  RISC OS 5 does: keys in any case, a line whose first character is `#` a
  comment, a mode's size its timings' display, which an `x_res` or `y_res`
  must agree with, the title kept to 31 characters, and each fault refused
  with the error RISC OS 5 gives it. `ScreenModes_ReadInfo` gives the title,
  the file's DPMS state and a display's speaker mask,
  `ScreenModes_EnumerateAudioFormats` a display's sound formats, and
  `ScreenModes_Features` RISC OS 5's one flag. No display describes itself
  here, so there are no speakers or formats. Their answers are held to
  RISC OS 5's by tests rather than the screen corpus, since a file loaded
  there would change the monitor for everything recorded after it.
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
  the screen's height less `y`, modulo 8. A graphics colour `OS_SetColour`
  sets reads back with an action of &60 beside its own, as RISC OS 5's does.
  What `OS_SetColour` sets is drawn with, but the VDU variables go on
  reporting the colours and tints `GCOL`, `COLOUR` and `TINT` gave. In a
  256-colour mode whose palette is grouped it takes a GCOL, its tint in the
  low two bits. A text colour it set reads back as given, one `COLOUR` set as
  its pixel repeated across the word.
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
it drew, which the replay holds the drivers to byte for byte. Its session
has Draw, which SpriteExtend's matrix plots call, as every session does.

## Sprites

Sprites are the `vdu` crate's `sprite` module: `OS_SpriteOp` through
`SpriteV`, whose default is the kernel's reasons and, while SpriteExtend is
active, SpriteExtend's in front of them; SpriteExtend's JPEG SWIs; and the
SpriteUtils commands. Areas', control blocks' and mode words' layouts, the
sprite types and the mode word's forms are `lib/image`'s sprite structures;
how a mode word's pixels are read is RISC OS 5.30's own, each rule recorded,
a type it does not support read as thirty-two bits. A sprite made in a
numbered mode RISC OS 5 lacks keeps that number, its pixels read from the
module that describes the mode.

- **Areas.** The system area is dynamic area 3, sized from
  `*Configure SpriteSize`, its header rewritten as it grows and its shrink
  held to what it uses; `OS_Byte` 163,242,65 and 66 read it, and `PLOT`
  &E8–&EF and `VDU 23,27` use the sprite it has selected. Names match
  case-blind in ASCII, twelve bytes at most, ended by any byte at or below a
  space. Every walk is held to the area's block before anything is trusted: a
  chain leaving it or a word is "Unrecognised sprite data" (&71E), where
  RISC OS 5 reads on or aborts, and a sprite claiming no length ends a
  lookup. A sprite is held whole before it is plotted or its shape changed —
  its rows' bits within a word and its words holding its width, its image
  and mask within it, and memory behind all of it — else &71E; reasons 41 to
  44 take its control block as it stands, adding its offsets in a word, as
  RISC OS 5's do. A module asked about a sprite's mode may change its area,
  so a change reads the control block again once it has answered and
  refuses one no longer as read; and a change first checks the area holds
  all of it and memory takes it, so a change refused leaves the area as it
  was.
- **Editing and files.** Rows and columns are inserted in colour nought
  under a clear mask; masks are created solid; palettes are made from the
  depth's default or the screen's, as RISC OS 5 makes each; alpha masks and
  channels convert both ways. A file is the area from its count word; one
  loaded is held to a saved area's form first, and a merge replaces the
  area's sprites of the file's names.
- **Taking from the screen** (14, 16): the rectangle, its corners inclusive,
  in the destination's own pixel form, a numbered mode's rows keeping the
  screen's word alignment; pixels outside the graphics window are the
  background colour.
- **Output to a sprite** (60–62): every drawing path writes the destination
  — the screen or a sprite or its mask — in its own pixel form, with
  `Service_SwitchingOutputToSprite`; save areas hold the VDU state in the
  drivers' own layout, validated before it is restored, every point in it
  within the drivers' reach and its origin a VDU 29 one, and a mode change
  returns output to the screen, announced as a switch to it between
  `Service_ModeChanging` and `Service_ModeChange`, as RISC OS 5 announces it.
- **The kernel's plots** (28, 34, 48, 49) copy the sprite's rows bit for bit
  whatever the depths, combined by the GCOL action, a mask pixel covering its
  image pixel's bits. A mask plot without a mask fills a rectangle of the
  sprite's size in pixels; an eight-bit alpha mask, asked for, is refused as
  a bad mode (&708).
- **SpriteExtend's plots.** A scaled plot's pixel takes the source pixel
  whose span reaches past its far edge (50, 51, 52, 65). A transformed plot
  (55, 56) draws a pixel whose centre lies strictly past the image's lower
  edges and on or within its upper ones, sampling the source on each row's
  centre line at a grid anchored to the image of the source rectangle's top
  left, columns stepped in 16.16, points from the graphics origin. The
  parallelogram form maps the rectangle's first corner to its first point,
  the whole sprite's first corner being its top left. The matrix form plots
  the parallelogram Draw makes: the rectangle, each corner held to the
  sprite, its first corner and the two beside it moved in place by
  `Draw_TransformPath` as a path of three moves below SpriteExtend's stack.
  R4 is the sprite's row in words, in bits where a source rectangle is
  named; R5 the plot's action, 8 for a mask plot; R7 the table it plots
  through, none for one that maps a sprite of the destination's depth to
  itself. A claimant of `DrawV` sees the corners and may move them, Draw's
  refusal — an overflow among them (&98A) — is the plot's, and with Draw
  gone the plot fails as its unknown SWI does (&1E6). No matrix leaves the
  corners where they are. Translation tables are read as bytes onto a palettised destination,
  wide or not; onto a deep one a narrow table gives colours in their
  colour-number form, and flag bit 4 lets a palette naming every pixel value
  stand for it; a deep sprite reaches a palettised destination only through
  a 32K table. A blend's weight is the alpha, 255 where none is blended,
  widened to 256 and scaled by the opacity, 256 less the translucency:
  `W = (A + (A >> 7)) × (256 − T)` 65536ths. Onto a deep destination each
  channel of the source pixel, in its own form where it is direct colour
  drawn without a mapping and else in the destination's, of `s` bits, meets
  the destination's of `d`,
  each share rounded down apart: an eight-bit source at 256ths,
  `((S × w) >> (8 + s − d)) + ((D × (256 − w)) >> 8)`, `w` being `W >> 8`;
  a narrower one at the whole weight, its bits repeated once below
  themselves where `d` is wider and `s` then counting them,
  `((S × W) >> (16 + s − d)) + ((D × (65536 − W)) >> 16)`. A colour mapping
  descriptor's transfer function is called once for each colour a deep
  sprite holds.
- **Onto a palettised destination** each source pixel is made the
  destination's colour number first, and that colour is mixed with the
  destination pixel's. The destination's colours are its palette's first
  flash states, as ColourTrans reads them, a sprite with no palette of its
  own showing its mode's default; its 32K table is the one ColourTrans gives
  a deep source.
  - With 256 colours, a translucency mixes the two as BlendTable does, and an
    alpha mixes their five-bit forms at the whole weight, as a narrower
    source meets a deep destination; the mix is found in the 32K table.
  - With fewer, the alpha's share is taken in steps of 32 after the
    translucency scales it, and the mix, as BlendTable makes it, takes the
    palette's nearest colour by plain squares, the last of equally near.
  - A colour mapping's colours, and a JPEG's, are found in the 32K table.
- **Pointer shapes** (36): a sprite of 256 colours or fewer, its pixels
  through the table to the pointer's four colours, scaled by the eigen
  factors where no factors are given, cut to 32 by 32; its palette entries 1
  to 3 set the pointer's colours as `VDU 19` sets them.
- **Screen save and load** (2, 3) save the graphics window as `screendump`,
  and plot a file's first sprite at the window's corner in its mode and
  palette.
- **JPEGs.** `JPEG_Info` and `JPEG_FileInfo` read the stored raster, the
  process, and the density, 90 times a pixel aspect where only that or none
  is stated, with RISC OS 5's workspace figure; `JPEG_Info` reads its header
  past the length given, as far as memory goes and 64 KiB at most, as RISC
  OS 5 does. A plot
  reads the whole length given, aborting where that runs past memory, and
  decodes the stored raster, EXIF orientation unapplied, through `lib/image`;
  a transformed plot is made only where its transform is a scaling, else
  "not a scaling" (&71B). `JPEG_PDriverIntercept`'s bit 0 passes the plots
  to `PDriver_JPEGSWI`, the JPEG SWI's offset in R8; an offset past it is
  out of range for the module.
- **SpriteUtils**: each command the `OS_SpriteOp` it names on the system
  area, its error the call's; `*SDelete` stops at the first it cannot delete.
  `*SList` and `*SInfo` give the names, and the area's size, free space and
  count, in the emulator's own words.

Known differences from RISC OS 5, each deliberate and accepted:

- a transformed plot that steps through source rows unevenly — a vertical
  shear, a turn other than a right angle, a parallelogram whose height does
  not divide evenly — can take the neighbouring source row at an exact tie,
  about one pixel in a hundred, RISC OS 5 rounding its row arithmetic where
  this is exact;
- a sixteen-bit sprite blended by its alpha onto a 32-bit destination keeps
  the destination's blue, where RISC OS 5 drops it;
- a translucent deep sprite plotted onto a palettised destination without a
  32K table draws nothing, where RISC OS 5 draws part of a row and aborts;
- a JPEG's pixels are `lib/image`'s decoder's, and are not dithered;
- a sprite taken from the screen has its wastage zeroed, and removing
  wastage fills with zeros, where RISC OS 5 leaves bytes from beyond the
  rectangle or the row;
- `JPEG_PDriverIntercept` starts at nought, where RISC OS 5's first answer is
  whatever its workspace held;
- output to a sprite no text cell fits keeps a one-cell window and draws no
  text, where RISC OS 5 writes outside the sprite; output to a sprite of no
  pixels is refused (&71E); a save area that is not one the drivers wrote is
  restored as a fresh state.

## ColourTrans, InverseTable and BlendTable

ColourTrans (`vdu/src/colourtrans.rs`) answers each of its 36 SWIs through
`ColourV`, R8 the call's offset, so a claimant may answer any of them.

- **The match.** A colour's nearest or furthest colour number and GCOL, in
  any mode with any palette, minimises each channel's squared error weighed
  by the loadings (`*ColourTransLoadings`, `MiscOp` 0; red 2, green 4, blue 1
  at first). The first of equally near and the last of equally far win. The
  default 256-colour palette is matched a tint at a time, any other grouped
  one at VIDC1's four bits a channel, and a direct-colour mode takes each
  channel's top bits. A grouped palette's GCOLs are its colour numbers'
  bits shuffled, a full palette's the numbers themselves; `SetGCOL` reports
  both in the shuffled form. The palette is one of four:
  - the current one, as last read;
  - a mode's default;
  - a block of words;
  - a sprite's.

  ColourTrans reads the current palette again only once it is made stale: by
  `InvalidateCache`, a mode change or output switching, never by a palette
  write. While output goes to a sprite with a palette, that palette is the
  current one; while it goes to one without, the screen's mode and palette
  are.
- **Calibration.** A table of three lists of device-level pairs maps a device
  colour to the standard colour it shows. Each channel is interpolated in
  16.16 between the pairs either side of it, and the three are summed before
  the sum is cut.
  - A palettised match looks for the wanted colour through the table; a
    direct-colour match and a 32K table do not use it.
  - A refused table restores the default, and the refusal is announced with
    `Service_CalibrationChanged`, as a table set is.
  - `*ColourTransMapSize` and `*ColourTransMap` set a table: sizes blue first,
    pairs last first, the pairs on one line or gathered over several until
    the sizes are filled, more than they give refused. `WriteCalibrationToFile`
    and a saved desktop write the table in that order, a table too large for
    one command line over as many lines as it needs.
- **Tables.** A table from a palettised source gives a byte for each source
  colour, a wide pixel for a deep destination, or the source's palette.
  - A deep source gives a palettised destination a 32K table. Each
    fifteen-bit colour maps to the entry nearest by the sum of its channels'
    differences in five-bit steps, the first of equally near. Each palette
    entry's own colour then takes the last entry holding it.
  - InverseTable builds its table by the same rule. BlendTable, and the
    sprite plots, JPEGs and colour mappings onto palettised destinations,
    find colours through it.
  - `SelectTable` reads its flags in R5 only for a sprite, `GenerateTable`
    always. A sprite with no palette gives its mode's default whatever bit 1
    asks.
- **Palettes.** `ReadPalette` and `WritePalette` work on words as they are
  held. `VDU 19` holds a colour bare, or with its low byte all ones where it
  has supremacy. A written word is held as given. `OS_ReadPalette` reports
  each entry with its supremacy bit, and how it shows, in its words' low
  byte.
  - The current palette cannot be written while output goes to a sprite.
  - A sprite written to without a palette is given one of its mode's size;
    one whose palette stays shorter than its mode's colours is refused
    (&A08) rather than written past.
  - A sprite's palette is read no further than its pixels' colours, however
    many entries it holds.
- **Models.** HSV, CMYK and CIE XYZ, all as 16.16 numbers. CIE uses RISC OS
  5's matrices, each sum cut toward minus infinity. An answer past a
  register is refused (&A01).
- **Font colours.** In a mode of up to sixteen colours the run is the
  longest, at most fourteen, of colour numbers stepping up or down from the
  foreground's. Each colour in it lies nearer than a whole step to the shade
  between background and foreground it stands for. In a deeper mode the words
  are left to the Font Manager: R2 is nought, and R3 must lie between -14 and
  14 (&206). `SetFontColours` sets them through the Font Manager: by
  `Font_SetFontColours`, or in a deeper mode by `Font_SetPalette` with the
  background's word in R1 and the words as given. Refused, it is made again
  as `Font_SetFontColours` with the registers as given, whose refusal is the
  call's. The words and offset asked for are recorded as the font's RGB
  colours.

InverseTable (`vdu/src/inversetable.rs`) gives a 256-colour palette's colours
in fifteen-bit form and its 32K table. It keeps the current destination's
tables until its palette differs, and writes any palette's or sprite's tables
into the caller's blocks; `SpriteTable` given the current mode and palette
hands back the kept tables as `Calculate` does. BlendTable
(`vdu/src/blendtable.rs`) mixes each source colour over each destination
colour in 256ths at a translucency up to 256, wholly transparent, a further
one refused (&94). Source and destination are a mode and palette, or a
sprite, as `ColourTrans_SelectTable` takes them. It finds each mix by the
destination's inverse table where the destination has 256 colours, else by
plain squares, the last of equally near. A table may be locked in the
module's own block until unlocked.

Known differences from RISC OS 5, each deliberate:

- an ECF for a mixed colour, and for a tint of the default 256-colour
  palette, places its cells by the emulator's own rule, which RISC OS 5's
  placement has not been pinned to;
- `MiscOp` 1's block holds only the current mode's depth. RISC OS 5's is
  internal, and it aborts in a deep mode;
- a GCOL or colour number past 255 takes its low byte, where RISC OS 5 reads
  past its table;
- the C flag on return is not modelled;
- the default 256-colour palette's 32K tables are built by ColourTrans's own
  match by the default loadings. RISC OS 5 hands back prebuilt tables:
  - InverseTable's differs in about three cells in a hundred, and blends onto
    that palette follow it;
  - ColourTrans's disagrees with ColourTrans's own match;
- `InverseTable_SpriteTable` given a mode selector builds the selector's
  mode's tables, where RISC OS 5 aborts;
- a 32K table for a palette given as nought takes the mode's default, where
  RISC OS 5 reads words from address nought;
- `ConvertDevicePalette` converts as documented, where RISC OS 5 branches
  through zero;
- `*ColourTransMap` sets the table it is given, over several lines where it
  must, where RISC OS 5 aborts or sets nothing;
- `WriteCalibrationToFile` writes `*ColourTransMapSize` with its sizes blue
  first. RISC OS 5 writes `*ColourTransMapColo`, with the sizes in the
  table's order.

## Draw and DrawFile

The `userland/apps/tairix-bridge/riscos/draw` crate is RISC OS 5.30's Draw
1.22 and DrawFile 1.62.

- **The vector.** Each Draw SWI calls `DrawV` with R8 its index, the
  caller's R8 kept; one past the sixteen names is out of range (&110)
  without a call. The vector's default takes each index. The convenience
  calls re-enter `DrawV` as `Draw_ProcessPath` with the registers RISC OS
  gives it — a fill, and a path transformed, undashed — so a claimant sees
  what RISC OS's does. The floating-point calls refuse (&9FF).
- **Processing.** A path is read through its continuations and passes RISC
  OS's stages in their order: subpaths closed, curves flattened by RISC OS's
  recorded bisection, dashed, thickened, flattened again, transformed. Each
  stage hands on what it makes as it makes it, so an error leaves what went
  before it written, as RISC OS's does. Refusals: reserved flags &982, a code
  past the last or a loop of continuations &984, an element out of order
  &985, a call that could change the path's length in place &986, a full
  buffer &987, a curve unflattened &988, a cap or join past its kinds &989,
  a point transformed out of a word &98A.
- **Destinations.** An output buffer: its room at +4, the end element apart,
  each element written while it fits and the end element always after the
  last. A count: the bytes, the end element's included. A box, where R7's top
  bit is set: from the extremes, R0 the last point's x. The path in place:
  each point transformed, the client bits and the end element kept. The fill.
- **The fill** scan-converts to RISC OS's four pixel classes — interior, its
  boundary, the exterior's boundary, exterior — by R1's style, non-zero or
  even-odd, a subpath at a time or all at once. It may make a clip, or plot
  within the one made. R0 is &7FFFFFFF on exit, &80000000 where the exterior
  is asked for. The segments it holds are bounded by the memory policy.
- **Dashing.** The start offset is walked through the pattern, whose lengths
  are unsigned, a count of nought acting as one and an odd count alternating
  on and off. The pattern starts afresh with each subpath and carries on
  across corners and through gaps. A run ending where a segment does hands on
  a piece of no length, and the closing segment's last run becomes the
  close.
- **Thickening.** Every shape winds anticlockwise. Joins are mitred to the
  limit, round or bevelled, and caps butt, round, square or triangular, the
  cap and join block read only as each field is used. A subpath of no length
  with round caps is a circle.
- **Steps.** Each element read, each element reaching a destination and each
  row the fill scans is a step of work toward the host's word, so the host
  can stop a call a small path makes endless: a line the coordinate range
  long dashed a unit at a time.
- **DrawFile** checks a file whole before drawing anything. A header
  refusal is &20C00, a later major version &20C01, sizes &20C05 to &20C07 at
  the last object accepted, a second font table &20C02, and a font no table
  names &20C03. Then each object is rendered:
  - paths through `Draw_Fill` and `Draw_Stroke`, each after
    `ColourTrans_SetGCOL`;
  - sprites through `OS_SpriteOp` 40, `ColourTrans_SelectTable` and 56;
  - JPEGs through `JPEG_PlotTransformed`;
  - text in a font its table names through the Font Manager: found at its
    sizes, coloured by `ColourTrans_SetFontColours`, painted in OS units
    through the render's matrix, a transformed text's own after it, and lost.
    Text in no font, or one the Font Manager refuses, the refusal kept, is
    drawn in the system font a character at a time through a work sprite,
    which leaves it unseen, as RISC OS 5.30's DrawFile does;
  - text areas laid out through the Font Manager as RISC OS 5.30's DrawFile
    lays them out. Each paragraph is built as Font Manager text and codes in
    a buffer of 255 bytes and broken into lines at its column's width by
    `Font_ScanString`, the columns filled in turn. Each line is painted by
    `Font_Paint` in millipoints through the render's matrix. Margins take
    effect from a line's first character, leading from the next line, and
    alignment as the line goes out. A font a `\F` defines is found there and
    lost at the end, an earlier font of the same number not. A malformed
    command is refused (&20C08), a font no `\F` defines (&20C09), a version
    but 1 (&20C0A) and a text with none (&20C0B);
  - with flag bit 0, each object's box, dotted.

  An object of an unknown type is passed over. Rendering goes on past an
  error, the first returned once it is done, and every register is kept.
  Each render and `DrawFile_DeclareFonts` asks `Font_CacheAddr` once the
  file is checked.
- **Nesting.** Each walk of a file's groups and tagged objects runs on the
  supervisor stack, as genuine DrawFile's does, and overflows it as RISC OS
  5.30's did called from BASIC: the check every call makes past 162 groups
  or 176 tagged objects, `DrawFile_DeclareFonts`' walk past 156 or 162, and
  `DrawFile_Render`'s past 131 or 140. Each level takes its kind's share of
  the stack, so a mixed chain overflows between the two, and the overflow
  aborts as RISC OS's does.
- **`DrawFile_BBox`** gives the header's box through the matrix.
  **`DrawFile_DeclareFonts`** calls `PDriver_DeclareFont` for each font in
  the table, and each a text area's `\F` names.
- **`*Render`** reads `file/A,a,b,c,d,e,f,bbox/S,suppress/S,flatness/K`, the
  matrix's scale in units and its translation and the flatness in OS units,
  loads the file into the RMA and renders it.

The SWI flags and command constructor Draw and DrawFile share with the
kernel are the kernel's: `tairix_riscos_kernel::swi` and
`tairix_riscos_kernel::module::Command`.

The corpus `draw/tests/riscos5/screens.txt` holds Draw's screens to RISC OS
5.30's, but the dashes along a slant, thick lines' widths and long mitres'
tips, which the crate's own tests hold to the cases recorded for them:
57 dashings, 205 thickenings and 16 Draw files.

Known differences from RISC OS 5, each deliberate and accepted:

- a thick line's edge, a long mitre's tip and a width of three units or
  less differ in low bits: 142 of the 205 recorded thickenings are exact,
  the rest within six units and a 20000th of the distance from the origin;
- a dash along a slant drifts up to six units from RISC OS 5's;
- a mitre on a line 2^20 units wide is tested without the overflow RISC OS
  5's limit test meets, so it mitres or bevels where RISC OS 5 does not;
- a pattern whose lengths sum to nought leaves the path undashed, where
  RISC OS 5 never returns;
- a loop of continuations is refused (&984), where RISC OS 5 follows it for
  ever;
- `Draw_FillClippedFP` and `Draw_StrokeClippedFP` refuse as floating point
  (&9FF);
- a fill that plots nothing leaves R0 as one that plots does, where RISC OS
  5 leaves other values;
- a Draw file object whose size is smaller than its header or not whole
  words is refused as too big (&20C05 to &20C07), where RISC OS 5 misreads
  what follows;
- `*Render`'s fractions are rounded to the nearest unit, which is not pinned
  to RISC OS 5;
- a text area's last command may end at the text's end, where RISC OS 5.30
  reads on past it for the command's end;
- a line too long for a text area's buffer is painted in pieces, and
  `Font_CacheAddr` is asked after the file is checked, each inferred from
  the recorded cases rather than recorded;
- SpriteExtend's R4 and R7 handed to Draw follow rules read from a handful
  of observations;
- nesting overflows at the depths RISC OS 5.30 overflowed at from BASIC with
  a path innermost, where RISC OS's own depend on what its stack already
  holds and on what the innermost object calls.

## The Font Manager

The `userland/apps/tairix-bridge/riscos/font` crate is RISC OS 5.30's Font
Manager 3.80 and `ROMFonts`, the fonts and encodings ResourceFS holds.

- **Fonts.** A font is found through the catalogue `Font$Path` reaches:
  each element's Messages file where it has one, else a scan of its tree.
  The catalogue reads at most the path's first 64 elements and 16,384
  entries of their directories and Messages files, a tree 16 levels deep and
  identifiers of 255 characters, and lists nothing past them; a directory
  read that finds nothing, or claims a record it did not complete, ends that
  directory's listing.
  RISC OS's own families in ResourceFS — Trinity, Homerton and Corpus — are
  TAIRiX faces. Each glyph is the font service's outline of the first
  character it stands for, asked by the generic kind `lib/drawing` maps the
  family to. A guest's Acorn font is its metrics file, its outlines or
  bitmaps, and its encoding files, read as hostile: a font whose files do not
  hold together is refused as it is found (&226).
- **The desktop font.** `Desktop` — `Desktop.Medium`, `Desktop.Bold` and
  their `.Oblique`s — is the TAIRiX desktop's interface face, in the family
  the session's host gives, the shipped themes' where it gives none. At its
  start the Font Manager sets `Wimp$Font` to `Desktop.Medium`, and
  `Wimp$FontSize` and `Wimp$FontWidth` to the size the host gives, each where
  it is unset; with no size, the Wimp's own twelve points stand. CMOS's
  desktop font setting resets to nought, the variables.
- **Measuring.** A string is walked as RISC OS 5.30 walks it: each
  character by its metrics, its kerning, the justification offsets and the
  matrices, the control sequences obeyed.
- **Painting.** A start in OS units is floored to its pixel. Each glyph is
  put at the pixel its pen is in, or at the quarter of it where `FontMax4`
  and `FontMax5` place its size so; in Draw objects always at the quarter.
  Glyphs are drawn at their font's resolution, a pixel to a pixel of the
  screen: from a bitmap file of their size where one serves, else
  anti-aliased in sixteen levels through the colour table up to `FontMax2`
  and `FontMax3`, in whole pixels past `FontMax2`, and past `FontMax3` filled
  as Draw fills their outlines. In a
  mode of up to sixteen colours the levels are a run of logical colours; in
  a deeper one they are the pseudo palette's entry for the foreground, which
  `Font_SetPalette` fills.
- **Draw objects.** `Font_SwitchOutputToBuffer` writes text as Draw path
  objects: a rub-out box between the nearest pixels, an underline's bar ahead
  of the first character it runs under, and each glyph's outline. An outline
  is drawn in 512ths of a pixel of its font's resolution, a matrix's
  translation moving it by the nearest 512th, and each point made Draw's
  units in one rounding. As RISC OS 5.30 writes them, an outline is copied
  into as many bytes as the pen's position across after the character is in
  millipoints. A character whose step ends at or near the screen's left edge
  fails (&1E4) with its object part written, and the word after the
  terminator is what the last copy left there.
- **The cache** is the session's, bounded by its memory policy; the area the
  guest sees is its size. A saved cache is the fonts the handles hold, in
  TAIRiX Bridge's own format.

The SWI flags and string control codes, command constructors and menu layout
the Font Manager shares with the kernel are the kernel's:
`tairix_riscos_kernel::swi::font`, `tairix_riscos_kernel::module::Command`
and `tairix_riscos_kernel::msgtrans::menus`.

The corpus `font/tests/riscos5/screens.txt` holds what the Font Manager reads
and writes to RISC OS 5.30's, through Draw objects rather than the screen,
whose anti-aliasing is RISC OS's own rasteriser.

Known differences from RISC OS 5, each deliberate and accepted:

- a glyph's anti-aliased levels are TAIRiX's scan converter's, not RISC OS's
  rasteriser's;
- the `Desktop` family and the variables set at the start are TAIRiX
  Bridge's own;
- in a mode of 256 colours or more, `Font_CurrentFont` gives the offset
  `Font_SetPalette` was given, where RISC OS 5.30 gives a smaller one once
  the colour table is read, by a rule not derived;
- `Font_SetColourTable` in a mode of up to sixteen colours leaves the
  offset as it was, where RISC OS 5.30 derives one from the table;
- `Font_MakeBitmap` for a font found in an encoding not its own writes its
  file in that encoding's directory, where painting reads it; RISC OS 5.30
  deletes the font directory's file and fails (&DE);
- outlines take no hints, having none, so `Font_SwitchOutputToBuffer`'s
  hints flag changes nothing;
- blending with supremacy in a mode of 4096 colours follows the 16 million
  colour modes' rule, which is the one recorded;
- `ColourTrans_SetFontColours` reaches the Font Manager directly, where RISC
  OS 5.30's ColourTrans calls its own SWIs through `ColourV` and reads the
  palette through `PaletteV` on the way, which a claimant of either sees.

## SharedCLibrary

The `userland/apps/tairix-bridge/riscos/clib` crate is RISC OS 5.30's
SharedCLibrary 6.23: its SWIs, the library kernel — chunk 1, its 48 entries —
the C library — chunk 2, the PRM's 183 and RISC OS 5's `_swi`, `_swix`,
`_vswi` and `_vswix` — and RISC OS 5's C99 chunks 4 to 9, as host routines a
client's vector reaches. The library answers as a release holding only the
chunks built: a stub naming one it does not hold is refused (&800E81).

- **Rights.** A routine reaches its client's memory with the client's mode's
  rights, as its code running in that mode would: a User mode client's
  `memset` of zero page, or a copy from a stack, aborts, reported at the
  library's entry the client called.
- **LibInit.** `LibInitAPCS_32` and `LibInitModuleAPCS_32` lay a client's
  vectors — `LDR PC` through a table after them — its statics and its root
  stack chunk, each chunk whole before the next is checked. APCS-A always,
  and APCS-R and the 26-bit module form from a 32-bit caller, are refused
  (&800E86).
- **Starting.** `_kernel_init` lays the statics, the root chunk, the command
  line's copy at its top and the 159-word extension stack above it, sets the
  ten handlers — the four exceptions' through trampolines in the statics —
  and enters each language's initialisation procedure, then the run
  procedure the first one offered: "no main program" (&800E02) where none
  did. `_kernel_entermodule` starts a module run as an application, its root
  chunk at &8000; `_kernel_moduleinit` readies a module's library to be
  called in Supervisor mode.
- **Traps** — a divisor of nought (&80000020), an exception, an error from a
  SWI without its X bit (its number) — are searched for among the languages
  by the PC, R14 and each frame's link, the register set 68 bytes below the
  SP: the trap procedures, then the uncaught ones. Past them the handlers in
  force before `_kernel_init` are put back and "uncaught trap" (&800E01) goes
  to the parent. A procedure is entered as a continuation, never a nested
  call, so one that never returns leaves nothing of the library behind.
- **Escapes and events.** An escape is acknowledged and searched for at the
  callback the library's escape handler asks for, among the event
  procedures then the unhandled ones, as a trap is; where SL names no stack
  chunk no procedure is entered. A real event reaches every language's fast
  event procedure at once, then the first event procedure found at the
  callback; no event is handed on. A new application's upcall runs the
  finalisation procedures on the root chunk's top and puts the handlers back;
  every upcall is passed on. Handlers are put back through
  `OS_ChangeEnvironment`, which leaves a word it is given as nought as it
  stands, so one whose R12 or buffer was nought keeps the library's, as on
  RISC OS 5.30.
- **The stack** is a chain of chunks. Extension takes a chunk of at least
  4 KiB — for a big frame, just the frame and SL's distance — the one kept in
  hand where it is big enough, else one through the client's allocator, run
  on the extension stack, or the heap's own, which never gives a chunk back.
  The frame that overflowed returns through the library, its real return kept
  in a frame record of the library's 104 bytes below it, so a backtrace walks
  through the extension; on that return the chunks past the one kept in hand
  are freed through the freer each remembers. A chunk that cannot be had is a
  trap (&80000021).
- **Unwinding** follows the APCS: R4 to R9 and F4 to F7 restored as the
  instructions that stored a frame saved them. A language's unwind and name
  procedures, where it gives them, are entered in `_kernel_unwind`'s and
  `_kernel_procname`'s place; without a name procedure no procedure is named.
- **The heap.** `_kernel_alloc` takes at least 2048 words from the heap above
  the extension stack, or what is left. Outside the desktop the heap ends
  where `_kernel_init` was told the workspace does, and the slot-extension
  procedure is never asked.
- **Sub-programs.** `_kernel_system` runs a command with the caller copied to
  the top of memory under a lowered memory limit, the library's own exit,
  error and upcall handlers taking the sub-program's end and copying the
  caller back; the current active object is left pointing into the library.
  Chained, the command replaces the program, which is not finalised.

### The C library

- **Starting.** `_clib_initialise` lays `errno`, `HUGE_VAL` (the largest
  double), the clock's start, the C locale, every signal's default, the
  random number generator seeded with one, the heap's first arena of 2048
  words — which the library kernel's stack chunks are then claimed from — and
  the sixteen streams: `stdin` to read, `stdout` and `stderr` to write, each
  with its extra block, the sixteen blocks the heap's first.
- **`_main`** reads the command line: words parted by spaces, a double quote
  turning quoting on or off anywhere, and a word starting with `<`, `0<`,
  `>`, `1>`, `>>`, `1>>`, `2>`, `2>>`, `>&` or `2>&1` a redirection, its
  file name in the same word or the next. The redirections are made in turn:
  `stderr` sent to a file of its own is buffered as any file is, and sent
  where `stdout` goes to a file the two share it, each line-buffered in a
  small buffer of its own so their lines reach it in the order they are
  delivered. `main` is entered with `argc` and `argv` as a continuation,
  its return going on into `exit`. A line that cannot be read, or a
  redirection that cannot be made, is reported on `stderr` and the program
  ends with return code 1.
- **Ending.** A language named C takes the C library's `Finalise` as its
  finalisation procedure where it gives none in its code, as the PRM's
  example language does: the functions `atexit` registered are called, the
  last first, then every stream is closed — delivered, its file closed, a
  temporary file removed. `_clib_finalisemodule` does the same for a module,
  then gives its heap and its workspace back to the RMA, clearing the private
  word so the kernel frees nothing again.
- **Streams.** Each `FILE` block, and the extra block beside it, is laid as
  the headers and RISC OS 5.30 lay them, so the `getc` and `putc` macros a
  program was compiled with work on it too. A file's 4 KiB buffer is taken
  at its first transfer; the console's output is unbuffered, each newline
  written by `OS_NewLine`, and its input read a line at a time by
  `OS_ReadLine32`, a line-buffered output stream delivered first. One
  character can be pushed back. An append stream's first transfer goes to
  the file's end; after it bytes go at the stream's position, so a
  positioning is honoured and a file grown behind its back is not followed.
  A stream in error refuses input until
  `clearerr` or `rewind`. `fclose` of anything but an open stream of the library's
  answers `EOF`; `system` delivers every stream before the command runs.
- **Formatted I/O.** `printf`'s family writes C99's conversions as RISC OS
  5.30 writes them: a floating-point value's seventeen significant digits
  correctly rounded, then rounded half to even to the precision, noughts past
  them; an unknown conversion its letter alone; the `_printf` family's float
  conversion the string at address nought. A width or precision is at most
  `INT_MAX`, and `%s` reads no further into a string than its precision.
  `scanf`'s family reads C99's
  directives with RISC OS 5.30's: a scanset of single characters with no
  ranges, `%u` refusing a sign, an integer kept to its low bits, one
  character of lookahead — so what a failed conversion took is lost — and a
  floating-point number read as `strtod` reads one.
- **`strtod`** reads C99's forms — decimal, hexadecimal, infinity and NaN —
  rounded correctly; a subnormal or out-of-range answer is a range error,
  but an exponent marker with no digit after it leaves the number unread.
- **`math.h`** is worked on an FPA: the elementary functions as its
  double-precision instructions, the hyperbolic ones by Cody and Waite's
  algorithms (*Software Manual for the Elementary Functions*, 1980). The
  errors are the exceptions the working raised: an invalid operation a
  domain error, `-HUGE_VAL` with `EDOM`; a division by nought, an overflow
  or an underflow to nought a range error, `±HUGE_VAL` or nought with
  `ERANGE`. A NaN argument answers a NaN, but `modf`'s fraction of one is
  +0; an infinity from an infinite argument is no error, but `pow`'s, which
  the FPA's power flags. `acos`, `asin`, `cos`, `sin`, `tan`, `exp`, `pow`,
  `fmod`, `fabs`, `frexp`, `modf`, and `atan2` of a nought or an infinity
  leave the client's flags as they were; the rest leave the flags their
  working raised but for an invalid operation, a division by nought or an
  overflow, which never trap, where an underflow or an inexact result traps
  as the client enables it.
- **`time.h`** keeps an unsigned word of seconds since 1970, every fourth
  year a leap year; `localtime` takes the zone from CMOS, `strftime` a
  territory's names and forms through the Territory Manager in its locale.

### The C99 library

- **Chunk 4** is the part the compiler calls: storage for variable length
  arrays, and 64-bit arithmetic and conversions. Arithmetic wraps; division
  truncates toward nought, a divisor of nought a trap; a shift's count is
  unsigned. A conversion to an integer truncates, the FPA's invalid
  operation where the value has no place in the type; the comparisons answer
  in the flags.
- **Chunk 5** is the C99 library: classification, `fenv.h` over the FPA's
  status, rounding and remainders, `snprintf`, `vsnprintf` and the `vscanf`
  family, the `long long` functions, the float and C99 elementary functions,
  the gamma and error functions, `fma` — its exact sum rounded to the FPA's
  extended precision, then to the answer's, as RISC OS 5.30's is — the
  complex arithmetic, 64-bit file positions, `aligned_alloc`, `quick_exit`
  and `timespec_get`. Chunks 7, 8 and 9 give its mathematics, its complex
  arithmetic and the rest again under their own numbers, each entry chunk
  5's very routine; chunk 9 adds an `atexit` of its own.
- **Chunk 6** is the atomic operations on objects of one, two, four and
  eight bytes — store, load, exchange, compare-and-exchange, the fetches, a
  fence, and the flag's test-and-set and clear — a memory order changing
  nothing.
- **Values and flags** are RISC OS 5.30's. A float function is the FPA's
  single-precision instruction where it has one — the circular functions,
  `expf`, `logf`, `log10f`, `sqrtf`, `powf` and `atan2f` — and otherwise
  works its double's form and narrows it, an exception it traps, or an
  overflow in the narrowing, raised with the client's flags as they were on
  entry; `tgammaf`'s overflow traps with its work's inexact flag raised.
  `llroundf` steps only its truncation's low word, so a value from −1 to −½
  answers 2³² − 1, and `log2f` of a power of two answers its exponent field
  less a double's bias, `log2f(1)` −896, as RISC OS 5.30's do.
- **Complex arithmetic** works a finite argument by its function's formula —
  Kahan's `CTANH` and his definitions of `cacos` and `cacosh` (W. Kahan,
  "Branch Cuts for Complex Elementary Functions", 1987) among them — and
  takes any other's special value from C99's Annex G, raising only the
  invalid operation the annex names. The compiler's multiply and divide
  helpers work the textbook forms at extended precision, each part rounded
  once. As RISC OS 5.30's do, `catanh` answers `casinh` for a real part whose
  sign bit is set, so `catan` answers `casin` where the imaginary part's is
  clear, and `cacosh` of a NaN over an infinite imaginary part answers that
  infinity as its own.

The crate's tests run the probe programs that recorded RISC OS 5.30 — BBC
BASIC V assembling a C client — or replay their records through the
library's own entries, and hold the library to what they found.

Known differences from RISC OS 5.30, each deliberate:

- `_kernel_escape_seen` answers 1 after an escape, as the PRM says; RISC OS
  5.30 answered 0 after every escape recorded;
- `_kernel_stkovf_copyargs` and `_kernel_stkovf_copy0args` move the frame and
  its arguments as the PRM describes and return; RISC OS 5.30 moves them,
  then corrupts R4 to R9 and branches through zero;
- stack extension asked for in a privileged mode is an uncaught trap error at
  once; RISC OS 5.30 hangs;
- unwinding back across an extension puts SL in the chunk before, as the PRM
  says; RISC OS 5.30 leaves it as it was given;
- a trap the library raises itself dumps R0 to R3 as the caller had them,
  where RISC OS 5.30's hold its own;
- the kernel's and the C library's statics, past what a client is told of,
  are laid out as TAIRiX Bridge's own;
- `LibInit` refuses a vector whose limit lies below its base as out of date,
  where RISC OS 5.30's writes on until it faults, and reads at most 1024
  descriptions, a list not ended by then naming an unknown chunk;
- `sinh` and `cosh` of an argument between 1 and 19 in magnitude are within
  one place of RISC OS 5.30's, whose evaluation there is not known; the
  complex circular and hyperbolic functions are within two places of its
  where their hyperbolic argument lies there;
- `acosh` and `asinh` at a few arguments are a place or two from RISC OS
  5.30's, which inherit a square root FPEmulator gives places off;
- `lgamma`, `tgamma`, `erf` and `erfc` are within a small part of RISC OS
  5.30's, whose approximations are not known: 2·10⁻¹² for `lgamma`,
  5·10⁻¹⁰ for `tgamma`, 10⁻¹³ for `erf` and `erfc`, 10⁻⁵ for the float gamma
  functions and two places for `erfcf` — a part of one rather than of the
  answer for `lgamma` and `lgammaf` below one, a logarithm erring absolutely
  by its Γ's relative error;
- a complex inverse function's part that is the logarithm of a magnitude
  within a rounding of one, as near a branch point, is within 2⁻⁶² of RISC
  OS 5.30's, which rests on FPEmulator's extended arithmetic;
- `cpow`'s parts are within 2⁻⁶⁰ of the answer's larger part of RISC OS
  5.30's, whose sines and cosines near a multiple of π/2 are FPEmulator's;
- `cacosh(±0 ± i0)` answers `+0 ± iπ/2` as C99 gives, where RISC OS 5.30's
  real part is 1.37·10⁻¹⁶;
- `cproj` answers the projection C99 gives, where RISC OS 5.30's returns
  without answering;
- the complex division helpers answer NaN parts for a NaN operand, where
  RISC OS 5.30's real part is one or nought;
- `ctanh` and `ctan` flag an underflow where a square they work underflows,
  where RISC OS 5.30's flag none if both parts are nonzero;
- a NaN answer's sign, which C leaves unspecified, can differ;
- `%a` writes a double's hexadecimal digits exactly, rounded half to even to
  a precision, where RISC OS 5.30's often add one to the thirteenth;
- `strftime`'s `%Z` names the host's zone;
- `asctime` and `strftime` write `???` for a weekday or month out of range;
- `strerror`'s texts and the library's own reports are in TAIRiX Bridge's
  words;
- a write straight after a read on an update stream, which C leaves
  undefined, is written, where RISC OS 5.30 refuses it;
- `aligned_alloc` of no size asks for a block of the least size, four
  bytes, where RISC OS 5.30's can leave an aligned block of no size, which
  `free` has no list for;
- `qsort` given a comparison that orders inconsistently sorts no further a
  range its scan ran out of, where RISC OS 5.30's sorts on past the array.

## Support modules

### FPEmulator

FPEmulator 4.39, the multiple-context emulator its help string's `(1.13M)`
names, as its ten SWIs answer on RISC OS 5.30. The FPA is the processor's;
the module keeps the floating point contexts its state is switched between.

- **Contexts.** A context is a block of 136 bytes, `FPEmulator_ContextLength`'s
  answer, laid out as FPEmulator lays one: each register's three words in its
  internal format and a nought, then the FPSR and a nought. While a context is
  current its state is the FPA's; `DeactivateContext` keeps it in the block,
  `ActivateContext` loads one, and `ChangeContext` does both, answering the
  context it replaced. −1 names the default context, the module's own and
  current from the start; −2 the current context, where a call takes it; 0
  none, under which every FPA instruction is an undefined instruction on
  either profile's processor.
- **A fresh context**, as `InitContext` lays one and the default starts, has
  every register nought and its invalid operation, division by zero and
  overflow traps enabled.
- **Saving and loading.** `SaveContext` writes a context's FPSR, then each
  register as `STFE` stores it, raising nothing; `LoadContext` reads the same
  form, the FPSR's system ID kept as `WFS` keeps it.
- **A trap** an enabled exception takes writes the registers as the
  instruction left them, R15 past it, into the exception register dump, and
  the FPA's in `SaveContext`'s form into the dump `ExceptionDump` answers,
  then raises FPEmulator's error, &80000200 onward.

Known differences from RISC OS 5.30, each deliberate:

- the processor executes each FPA instruction whole, so `FPEmulator_Abort`
  finds nothing to abandon and answers nought, and `SaveContext` never has a
  pending exception to report.

### VFPSupport

VFPSupport 0.18, the interface the ROOL wiki documents: its eleven SWIs, its
contexts, the lazy context switch and the elementary functions. RPCEmu has no
VFP, so nothing of it could be recorded; where the wiki leaves a value open,
TAIRiX Bridge chooses it. The unit is the 32-bit profile's processor's,
whichever profile is running.

- **Contexts.** `CheckContext` sizes one: 20 bytes and 8 a doubleword, for 1
  to 32 doublewords and the user-mode and application-space flags.
  `CreateContext` lays one in the caller's word-aligned buffer, or in the RMA
  where R2 is nought, with R3 as its FPSCR, as `VMSR` writes it, and FPEXC.EN
  clear, so its first activation loads no data registers. The layout is VFPSupport's own, as the
  wiki allows: a mark, the flags and the register count, then the FPSCR,
  FPEXC and each doubleword low word first, which `ExamineContext`'s
  descriptor gives (fields 0, 1 and 5).
- **Activation.** While a context is active its state is the unit's, loaded
  with FPEXC.EN set, and CPACR grants the unit to the privileged modes, or to
  every mode for a context made with the user-mode flag. `ChangeContext` saves the
  context the unit holds and loads the new one, or with the lazy flag only
  denies the unit; changing application space saves a context there at once.
  With no context active, or one awaiting lazy activation, CPACR grants the
  unit to no mode.
- **The lazy switch.** VFPSupport claims the undefined instruction vector.
  Where a context awaits activation and the instruction the processor found
  undefined is one of the unit's (A32 or T32), it saves the context the unit
  held, loads the awaited one, and runs the instruction again. Lazily back to
  the context the unit still holds saves and loads nothing. Anything else is
  passed on to the vector's owner before it. An access of the switch that
  aborts aborts the instruction, reported at its address; an awaited context
  no longer there leaves the null context active and the instruction
  undefined.
- **The rest.** `DestroyContext` activates the replacement where the context
  was active and frees one VFPSupport made. `FastAPI` answers the four
  routines, which behave as their SWIs. `Features` answers the unit's FPSID,
  MVFR0, MVFR1 and MVFR2, no trapped exceptions, and short vectors in
  hardware. `ExceptionDump` keeps the copy a caller makes until it is
  cleared. `ElementaryFunctions` answers the two tables of twelve; each entry
  executes one of the unit's instructions, so the unit's gate stands before a
  function as before any instruction, then computes from the first registers
  into the first under the FPSCR's controls, gathering its flags. Every value
  is correctly rounded, through `lib/softfloat`'s IEEE 754 functions.
- **Killing it** saves the context the unit holds into its area, frees any
  exception dump and gives the vector back; while a later claimant holds the
  vector, VFPSupport refuses to die.

Known differences from RISC OS 5's VFPSupport, each deliberate:

- the version, the context layout, its mark, and the error numbers are TAIRiX
  Bridge's. A malformed call gives "Invalid parameters" (&1EA) and an unknown
  `Features` reason the heap's "No such reason code" (&180), the kernel's own
  errors of the same meaning;
- the unit traps no exception, so `Features` 1 answers nought and the dump
  holds only what a caller made;
- the unit's gate is CPACR, so with no context active, or one awaiting
  activation, privileged code reaches neither FPSID nor FPEXC, where FPEXC.EN
  clear would leave both reachable;
- `ExamineContext`'s serialisation leaves the context's state in its area
  alone, the unit holding none of it, so a debugger's change to the area is
  what the next activation loads;
- `CreateContext` checks its flags and register count even with a buffer
  given;
- `FastAPI`'s workspace pointer is nought: the routines need none.

### CallASWI

CallASWI 0.15, the version whose SWIs the ROOL wiki lists as newest, is
present for an `*RMEnsure` to find. RISC OS 5's kernel performs `OS_CallASWI`
and every other SWI the module brings to older systems itself, so the module
provides nothing of its own.

### Squash

Squash 0.31's `Squash_Compress` and `Squash_Decompress` are `lib/compress`'s
Squash ([`tairix-compress`](../lib/compress.md)), which writes and reads the
stream as RISC OS 5.30's module does: byte for byte, and call by call the
input taken, output made and status answered, held to the streams and call
traces the module was recorded making.

- **The stream** is Unix `compress`'s with 12-bit codes, after its
  three-byte header, written and read a group of eight codes at a time: a
  call takes input only in whole groups while more follows, and holds a
  finished group its output has no room for until the next.
- **Two compressors.** A stream's first call decides how its table is
  cleared once full: a call given the whole input and room for half as much
  again and the header clears at once; any other clears every 32 KiB of
  input. Later calls keep the first one's way.
- **The workspace** holds the whole of a stream, &7C00 bytes to compress
  and &4400 to decompress, as the query (R0 bit 3) answers; a compression's
  R1 answers the most R1 bytes compress to, half as much again and twelve,
  −1 staying −1, and a decompression's −1. A call carrying on (bit 0) finds
  its stream there, its state checked before it is believed, since a
  program may have written over it.
- **Errors** are the module's numbers: &921 for memory the call cannot
  reach, &922 for input that is no Squash stream, &923 for a workspace
  holding nothing to carry on, &924 for flags out of shape.

A Squash file — `SQSH`, the file's length, load and execution addresses,
then the stream — opens on the desktop as a volume of its one file
([Archive driver](../filesystem/archive.md)), so a file a program squashes
is the user's to open.

Known differences from RISC OS 5.30, each deliberate:

- an empty input compresses to the stream's header alone, which decompresses
  to nothing, where the module aborts; and a header alone at the end of
  input is a finished stream, where the module asks for more;
- a first call that writes nothing — no input with more to come, or no room
  for the header — loses nothing: the header is written with the first byte
  taken, where the module's stream then holds its header alone;
- a code no table could hold is refused (&922) rather than decoded as
  whatever the table holds;
- bit 2, "the output fits", changes nothing a call makes, so a call never
  writes past the room it is given, where the module overruns it;
- an unaligned workspace is refused (&923) where the module hangs.

### BufferManager

BufferManager 0.39 holds the buffers device drivers and DeviceFS share,
reached through `InsV`, `RemV` and `CnpV`, whose default it is while it is
active; a buffer it does not know goes to the kernel's own, the keyboard's
and the five of buffers 4 to 8. A buffer is a ring in guest memory holding a
byte fewer than its size, its record charged 44 bytes of the RMA, its
internal ID its record's place, as RISC OS 5 numbers them.

- **Its SWIs** (`&42940` to `&42949`) make a buffer in the RMA or register
  the caller's memory, from handle `&100` or the one asked for; remove,
  deregister, read and change flags and information, link and unlink a
  device's wake-up routine and owner, set a threshold, and hand out
  `Buffer_InternalInfo`'s service routine, whose reasons 0 to 9 insert,
  remove, examine and count bytes and blocks, purge, and find the next
  filled run.
- **At its start** it makes the mouse (9), serial input (1) and output (2)
  and printer (3) buffers, in that order, the printer's as large as
  `*Configure PrinterBufferSize` says.
- **Its flags** wake a dormant buffer's device as data comes, raise
  `Event_OutputEmpty` and `Event_InputFull`, and issue `UpCall` 8 and 9 as
  its free space crosses its threshold.

Known differences from RISC OS 5, each deliberate:

- the kernel's serial, printer and mouse buffers are BufferManager's, in the
  RMA, where RISC OS 5's lie in kernel workspace;
- a reason past 9 of the service routine is refused where RISC OS 5's jumps
  out of its table, and a block insertion into the kernel's own buffers is
  taken where RISC OS 5's aborts.

### MimeMap

MimeMap 0.19's `MimeMap_Translate` and `*MimeMap` answer from TAIRiX's own
type registry: `lib/browse`'s media types, each with its MIME type, its
extensions and the RISC OS file type a file of it is given, and FileSwitch's
names for file types. The guest and the desktop therefore give a type one
meaning.

- **`MimeMap_Translate`** translates R1, in the form R0 names, into the
  form R2 names: 0 a file type, 1 a file type's name, 2 a MIME type, 3 an
  extension. A file type is answered in R3; the others are written at R3 and
  ended with a zero, as RISC OS 5.30 writes them, with no length given.
  Names are the system's, an unnamed type's `&xxx`. A MIME type is read to
  its first control character or space, an extension only to its zero, its
  leading dot optional, and both, like a name, without regard to case. A
  form R0 does not know is refused (&B00001); one R2 does not know answers
  nothing; a value nothing maps is refused (&B00002).
- **`*MimeMap`** shows the mapping a file type (`&xxx`), extension (`.ext`),
  MIME type or file type name has — its MIME type, its file type and name,
  then its extensions — or with nothing given, every mapping.
  `*ReadMimeMap` succeeds: the mappings are built in.

Known differences from RISC OS 5.30, each deliberate:

- the mappings are TAIRiX's, not those of the file `Inet$MimeMappings`
  names, so `*ReadMimeMap` has nothing to read and no file is missed;
- each answer is the one mapping's that matches, where the module takes
  each field from the first line of its file holding one;
- a type number past &FFF maps nothing, where a file's `*` line maps every
  number.

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
- **Screens.** Then BASIC V runs each screen case, the VDU drivers'
  (`vdu/tests/riscos5/screens.txt`), the Draw modules'
  (`draw/tests/riscos5/screens.txt`) and the Font Manager's
  (`font/tests/riscos5/screens.txt`), each corpus numbered on from the one
  before, each framed as `tairix_riscos_vdu::corpus` frames it; what each
  read and the screen it left go to its corpus's `drawn.txt`
  (`tools/xtask/src/commands/riscos/screens.rs`). The Font Manager's cases
  paint from the first-party fonts `tairix_riscos_font::build` makes, staged
  as `FntLib`.
  A case prints a pointer only as a token for what it points at, so neither
  its place in the run nor what the machine holds moves a reading.
- **Running.** Where `<dir>` holds RPCEmu, it runs headless, as one of the
  command's own children, until the harness, once done, saves a byte to a
  named pipe (HostFS opens none to write it a byte at a time), the emulator
  exits, or twenty minutes pass. Otherwise the
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
