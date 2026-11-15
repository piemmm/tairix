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
encoding: a fixed bit, a should-be bit, a bit of a named field, or a bit the
instruction ignores, which decodes alike whatever its value and encodes
clear.

- `a32` holds every A32 integer and system encoding from ARMv2 to ARMv7-A,
  with ARMv8-A's AArch32 integer additions (load-acquire and store-release,
  `SEVL`, `CSDB`, `SSBB`, `PSSBB`, `CRC32`).
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
- an FPA instruction meeting an exception whose trap the FPSR enables, past
  it, for the session to raise FPEmulator's error;
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
- CPACR holds the access fields of coprocessors 10 and 11 on a core with
  VFP, and nothing else; `ID_ISAR5` reports the Cryptographic Extension where
  the instruction set has it.

### The FPA

RISC OS 5 has no FPA hardware: FPEmulator, the module that takes FPA
instructions at the undefined instruction trap, defines every result its
software sees, so `fpa` computes FPEmulator 4's. `fpa::Fpa` is the state,
eight registers and the FPSR, and its operations; the processor runs FPA
instructions through it.

- **Registers** hold FPEmulator's internal format, `fpa::Internal`, the three
  words `SFM` stores. A single or double denormal, infinity or NaN keeps its
  own precision's form until an operation converts it; `URD` leaves an
  integer unnormalised for `NRM`.
- **Arithmetic** is `lib/softfloat`'s, rounded at the instruction's precision
  in its rounding mode; the "fast" `FML`, `FDV` and `FRD` are FPEmulator's.
  Underflow is tiny after rounding and flagged where denormalising loses
  bits; overflow gives the extended infinity, or the precision's largest
  number where the rounding mode rounds away from that infinity. An invalid
  operation's NaN is an
  extended quiet NaN carrying its reason in the mantissa.
- **The transcendental functions**, `SIN` to `POL`, run FPEmulator's own
  algorithms: each a sequence of FPA operations on an FPA of its own with
  traps disabled, over its constants, so each result is its bits.
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
and memory, runs under the module and natively, and must leave the same
registers (or the same exception dump, for a trap), memory, FPA registers
and FPSR. `--write-sample` writes 2,200 agreeing cases as vectors carrying
the FPA's state before and after, which the crate's test replays; the
module's bytes are never written, only the processor's own runs that
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

`tairix-riscos-kernel` (`userland/apps/riscos/kernel`) lays the guest's
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
the same frames, as RISC OS 3 and 4 had it. A 32-bit task sees at zero RISC OS
5's compatibility page instead: read-only, never run, four words of
`&FDFDFDFD` and then zeros, so a pointer read through null faults where it is
next used. The rest of a 32-bit task's low 32 KiB aborts. Each stack has an
unmapped page below it.

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

The manager calls the rest of the kernel through one seam: each handler call,
service call and upcall is made with the address space consistent, and may
itself make memory calls. A resize in progress refuses any other, as RISC OS's
does. Where RISC OS 5 leaves a case unguarded the manager refuses it: a
negative number, a system number made, renumbered onto or removed, an access
privilege it lacks, and an area its handler grows again while it is removed,
which stays rather than leaking its pages. The SWIs' registers, and the page
blocks and titles a handler or caller reads, are the kernel's to marshal.

`fuzz_areas` runs random slot, area, sparse and pool calls with hostile
arguments, under handlers that answer at random and call back in, holding
that every page in use is accounted for and every page an area reports reads
back at its address.

## The reference harness

`cargo xtask riscos --reference <dir>` records the corpus in
`userland/apps/riscos/kernel/tests/corpus/` on a genuine RISC OS. `<dir>` is
laid out as RPCEmu lays its own out: `roms/` the ROM images, `hostfs/` the
directory the machine boots from (`*Configure FileSystem HostFS`,
`*Configure Boot`).

- **Scenarios** are SWI calls with their registers, values kept from earlier
  calls, and what to record: registers, and only the flags the SWI defines,
  since the rest are its caller's. A scenario records nothing that depends on
  what else the machine holds, such as an area's number or base.
- **The harness** is a BASIC V program tokenised with `lib/bbcbasic`, with a
  `!Boot` that runs it. It writes each `show` as one line: the call's
  ordinal, `rN=&XXXXXXXX` for each register, `F=0` or `F=1` for each flag,
  and `error=&N` whenever V is set. Errors are recorded by number, never
  their text.
- **Running.** Where `<dir>` holds RPCEmu, it runs headless and the command
  waits on a named pipe, which the harness opens once it is done. Otherwise
  the harness is left in `hostfs/`; boot the machine from it and run the
  command again to import what it recorded.
- **Importing.** Each observation becomes its scenario's fixture, except for
  a scenario marked `ours`, whose fixture states this emulator's own
  behaviour. A recording is refused whole if it carries twelve bytes in a
  row of a ROM image that the corpus's own files do not hold.

The corpus was recorded from RISC OS Open's RISC OS 5.30 for IOMD under
RPCEmu 0.9.5. `locate-the-compatibility-page` is `ours`: the IOMD build keeps
zero page at zero, while the 32-bit profile, RISC OS 5's on ARMv7, has the
compatibility page there. A crate test holds every scenario to one fixture in
the harness's format. Replaying the scenarios on the kernel comes with its
SWIs.
