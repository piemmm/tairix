# PC Emulator

`PCEmulator.app` emulates the IBM PC/XT-class machine built around the 8088,
8086, NEC V20 and V30, and Intel 80188 and 80186. Its staged plan, with the
design every part follows, is `plans/PCEMU.md`. The engine is the
`tairix-pcemu` crate (`userland/apps/pcemu`), pure computation over guest
state, tested on the host.

## The processor

`tairix_pcemu::cpu::Cpu` is one interpreter for every modelled CPU, chosen by
`CpuModel`. A model fixes the bus width, the prefetch queue's length (four
bytes on an 8-bit bus, six on a 16-bit one), the instruction set, the timing
table and the undocumented behaviour.

- **One decoder.** `cpu::decode::decode` turns bytes into an `Insn` for a
  model. A unit is one prefix byte or one instruction, so each step does
  bounded work however many prefixes hostile code strings together; the CPU
  carries the prefixes between steps and takes no interrupt between a prefix
  and its instruction.
- **Instruction sets.** The 8086 family decodes every documented form and the
  undocumented ones software relies on: `POP CS`, the `60h`–`6Fh` jump
  aliases, `SALC`, `SETMO`/`SETMOC`, two-bit segment fields, register-operand
  LEA/LDS/LES (which expose the last address and operand), REP negating IMUL
  and IDIV, and an interrupted string instruction resuming at its last prefix
  — whether the interrupt came inside it or at a pause the machine's own
  time slicing made in it, which the program otherwise never sees.
  The 80186 adds its instruction set, masks shift counts to five bits, and
  traps unused opcodes (`0F`, `63h`–`67h`, `F1`, `FE /7`, `FF /7`) to
  interrupt 6 at the opcode. The V-series adds the 80186 set and NEC's
  extensions (`TEST1`, `SET1`, `CLR1`, `NOT1`, `ADD4S`, `SUB4S`, `CMP4S`,
  `ROL4`, `ROR4`, `INS`, `EXT`, `REPC`, `REPNC`, `BRKEM`), and the 8080
  emulation mode, where A, BC, DE, HL and SP live in AL, CX, DX, BX and BP.
- **Arithmetic.** The 8086 family's MUL, IMUL, DIV and IDIV run the
  microcode's own algorithms (CORX and CORD, with their sign handling), so
  results, flags and the flags a divide error pushes are what the hardware
  leaves.
- **The prefetch queue** is modelled for its two observable effects. A store
  into bytes already queued changes memory but not what executes; and the
  queue's fill sets timing, since an instruction waits for its bytes and the
  queue refills only while the execution unit leaves the bus idle.
- **Interrupts.** NMI, then INTR when IF is set, between instructions. STI
  and any segment-register load (SS only, on the 80186) shadow the next
  instruction; TF traps after each instruction it was set before. HLT waits
  for an interrupt, and the V-series also leaves HLT on INTR with IF clear.
- **The host call.** `FE 38 lo hi` executed from the emulator's own ROM stops
  the CPU with `Stop::HostCall`, so the BIOS and DOS run as Rust. Anywhere
  else it is the modelled CPU's own `FE /7`.

### The translator

`cpu::jit::translate` lowers a run of guest code to the IR of
[`tairix-codegen`](../lib/codegen.md), and the code that compiles to leaves
the machine exactly as `Cpu::step` would wherever it can stop: registers and
flags, the cycle count, the queue's fill and idle credit, the 8086's latched
address and operand, pending prefixes, the interrupt shadow.

- **A block** begins where `Cpu::can_enter_block` allows — a step boundary, no
  prefix pending, no stale queue byte, TF clear, native mode — and holds at
  most 32 instructions. It ends at a control transfer, at STI, at a segment
  load that shadows the next instruction, and before one it does not take:
  port I/O, interrupt entry and return, HLT, ESC and WAIT, POPF, BOUND,
  ENTER, INS and OUTS, the NEC extensions, the host call, an undefined
  opcode, AAM by zero on Intel's parts, the V-series' far transfer through a
  byte register, LOCK, a repeated string instruction, and a load of CS other
  than a jump's.
- **The frame** carries the state in and out: `Cpu::frame` builds it and
  `Cpu::adopt` takes it back. An instruction stores to it what it changed,
  once, as it ends; until then the frame holds the machine as it was, so a
  check failing within an instruction leaves through the one return every
  such exit shares.
- **Memory** is reached through the page map: per 256-byte page, where its
  bytes lie in the host or nowhere, their wait states, and whether a store
  may be made — never on a page holding translated code. Each access is checked
  before the instruction changes anything; device memory, a refused store, a
  word across a page or a segment's end, or a division that would fault
  leaves before the instruction, for the interpreter to take.
- **Timing** is the interpreter's: each code page's fetch waits, the queue's
  wait, overlap and refill, a word's second bus cycle, data waits, and DRAM
  refresh charged through the bus where its deadline falls. The block
  leaves once the cycle limit is reached.
- **Flags** are kept as the operation that last set them. A condition is
  read straight from its operands — a compare and branch is one host compare
  — and the word is computed only where something reads all of it, by the
  interpreter's own `alu` functions; a block may leave the operation for
  `Cpu::adopt` to finish. Shifts, multiplication and division with their
  timing, and the decimal adjusts are the interpreter's functions, called as
  helpers.
- **Folding** keeps a value constant while every input to it is — an
  instruction's costs, the queue after a jump — and tracks the bits each
  computed value may have, so a mask clearing none of them is never made.
- **Held to the interpreter.** `cpu::jit::differential` draws a random
  machine — every model, pages of every kind and speed, the bus unit
  anywhere a step boundary allows — and random code at its CS:IP, and runs
  the block there through the code generator's interpreter and the x86_64,
  aarch64 and riscv64 backends' machine models, with and without each
  extension the backends lower to; each must leave the
  processor, memory, pending waits and refresh as the CPU interpreter does
  stepping to the same cycle, calling the same helpers with the same
  arguments. The models also refuse a register read before it is set, a
  callee-saved register or the stack left changed, a call with the stack
  misaligned and a missing frame record. A unit test holds it to every exit
  and every instruction kind the translator takes; `fuzz_translator` draws
  on for as long as it is given.

### The code cache

`cpu::jit::dispatch::run` is `Cpu::run` with blocks: where
`Cpu::can_enter_block` allows, and the bus is owed no wait — a block charges
only its own — it enters the block installed for CS:IP, and otherwise counts
the interpreter's arrival there.

- **Heat.** A place reached 32 times is hot, and its block is requested: its
  bytes, read only from memory a block may be translated from — RAM or ROM,
  never a card's, the control block's or the emulator's own ROM. The heat
  counters and the 4096 block slots are direct-mapped, so a guest running
  from more places than they hold costs only its own speed.
- **Watching.** From the request on, the 256-byte granules of the board's
  store the block's bytes and the queue's read-ahead came from are watched.
  A write to one by anything — the CPU, DMA, the expanded memory manager, a
  snapshot's restore — or a bank switch or the control block's move away
  from what was read makes it stale, through whatever address it was
  reached by, for a bank can show one page in two windows. Every block and
  request over a stale granule is dropped before the next block is entered.
  A granule gone stale four times is left to the interpreter until its
  churn subsides.
- **The page map** gives each 256-byte page of the address space where its
  bytes lie in the store, their wait states, and whether translated code
  may store there — never into a watched page, so a block writing code
  exits before the store for the interpreter to make it. It is kept as the
  memory changes, a 4 KiB page at a time.
- **Executors.** What runs a block is an `Execute`, which names the code to
  the cache by a handle. A request is translated from its bytes alone, as
  the compiler sandbox translates it. `cpu::jit::interpreted` runs each
  block's IR on the code generator's interpreter, so a whole machine can be
  run through its blocks and held to the CPU interpreter at every slice. A
  block that answers no exit leaves the processor as it was entered, and no
  block is entered again.

### The compiler sandbox

A machine's hot blocks are compiled outside its worker, by `PCEmulator.app`'s
binary re-entered a third time as a one-shot sandbox holding nothing
(`cpu::jit::compiler`), and run in the worker natively (`cpu::jit::native`).

- **Asking.** The worker numbers each block as it asks for it, sending its
  bytes and where they begin, and keeps at most four unanswered. An answer
  installs only the request it answers: a block asked for again since,
  because its bytes changed, never takes the code of the bytes before.
- **Compiling.** The front-end hands each request to its compile worker
  thread, never its window loop. There a machine's compiler is started the
  first time it is needed; told the host's processor — the architecture the
  kernel attests, and the extensions every core has that the code generator
  uses (BMI2 on x86_64, Zbb on riscv64) — it translates and compiles the
  bytes alone, refuses a block
  whose code would pass 256 KiB, and writes the code into fresh pages of its
  own memory. The front-end moves those pages into the machine's worker with
  `code_install` and tells the worker where they landed; it never reads the
  code. A compiler installs into the one worker it was paired with.
- **Failing.** A compiler that crashes, overruns its ten seconds or answers
  what cannot be believed is ended and replaced, the block left to the
  interpreter. At its third failure, or when none can be started or the
  platform installs no code, the machine is told to interpret and asks for
  no more. A session not allowed `CAP_JIT_MAP_EXEC`, or a processor with no
  backend, interprets every machine from the start. A translated block that
  answers no exit — a code generator's defect — leaves its machine
  interpreted from then on; the worker says so once, and the application
  reports it as it reports a compiler that keeps failing and lets the
  machine's compiler go. The machine is as exact either way, only slower.
- **Running.** A block is entered with its frame, the page map's words and
  the helper table, its refresh helper reaching the bus through the one
  pointer the code hands back. The code is a cache within a sixteenth of the
  worker's room, given up whole once it passes that, and from moderate
  memory pressure on; what is given up is unmapped. A reset keeps nothing
  translated from the machine before it.
- **Ending.** A machine turned off has its worker ended at once but not
  reaped until the compile worker has forgotten it, so no process can take
  the worker's number while code may still be installed in it.

## The board

`tairix_pcemu::machine::Machine` is a CPU on a `board::Board`, built from a
`BoardConfig`: the CPU model and its clock (a `CpuClock`, from 1 MHz to the
25 MHz of the 80186 family's fastest part; the worker refuses any other), the
IBM board's wiring (PC or XT), 64–640 KiB of conventional memory, the display
adapters, the floppy and fixed-disk adapters, and whether an 8087 is fitted. `Template` gives the IBM
PC, the PC/XT, a 10 MHz turbo XT with a V20, and an 8 MHz 80186
workstation, each with the drives it was sold with; the turbo and 80186
machines have an 8087.

- **One digest everywhere.** `digest::reference_run` boots the turbo XT on its
  default disk, interpreted, to past DOS's prompt and folds its whole state
  every half second into `REFERENCE_DIGEST`; the host suite and a QEMU
  vertical on every native target assert the same constant, the vertical's
  fixture then running a program from a diskette under DOS, its loop
  translated, installed and run natively.
- **Time.** Emulated time is nanoseconds since power-on; every clock converts
  from it by an exact rational, its denominator at most 2³² so no conversion
  overflows, and the PIT, the CPU and the 80186's timers never drift apart. Turbo changes the CPU's clock and nothing else, so a
  guest's sense of time stays true.
- **Lazy devices.** No chip runs on its own. The board catches the chips up
  whenever the CPU touches one, and `Machine::run_until` runs the CPU in
  slices that end where a device next needs to act: an unmasked timer edge,
  a keystroke's byte, an 80186 timer, a byte a disk moves over DMA. A write
  to a device ends the slice early so its new schedule is read. A halted CPU skips straight to the next
  event, so an idle guest costs no host time.
- **Memory.** 256 pages of 4 KiB, each a window onto one store or the open
  bus, which reads `FFh`. ROM ignores writes, and only the emulator's own
  ROM honours the host call.
- **Expanded memory.** A LIM board of 1 to 512 pages of 16 KiB (8 MiB), its
  frame at `D000h` or `E000h` and its four page registers at one of `208h`,
  `218h`, `258h`, `268h`, `2A8h`, `2B8h` or `2E8h`. A register shows any
  page in its 16 KiB window by changing four page-table entries, never by
  copying. The board holds one page more for its manager, which no register
  can show. The turbo XT has 2 MiB and the 80186 workstation 8 MiB.
- **Ports.** The board decodes ten address bits, so every port repeats each
  kilobyte; the planar answers in 32-port blocks below `100h`. Every I/O
  cycle on the board carries one wait state.
- **The chips.** The 8259A with every initialisation and operation word,
  rotation, special mask and special fully nested modes, and MCS-80 vectors;
  the 8253 in all six modes with BCD and latching, each counter's output and
  count a function of when it was loaded; the 8237 with the PC's write-only
  page registers, where channels 0 and 1 share one; the 8255 in either IBM
  wiring, showing the configuration switches the BIOS reads; and the
  keyboard with its own typematic, self-test and 16-byte buffer.
- **The keyboard** is the enhanced 101-key keyboard in XT mode, sending scan
  code set 1. An XT BIOS reads an `E0` prefix as the release of a key it does
  not have, so the grey keys reach it as the keypad's; the keyboard keeps them
  true to their labels with the fake shifts IBM's enhanced keyboard sends — a
  Shift let go of round a grey key while it is held, one pressed round it
  while Num Lock is on — counting Num Lock's presses itself, as an XT-mode
  keyboard must, and leaving the count where Ctrl makes Num Lock Pause. Pause
  sends Ctrl and Num Lock under `E1` and nothing as it comes up, Ctrl+Pause
  the 83-key keyboard's Break, and Print Screen a fake Shift with the
  keypad's `*`, or System Request under Alt. A key fits the buffer only where
  its whole sequence does.
- **Refresh and DMA.** Timer 1 requests DMA channel 0 to refresh DRAM, as
  the BIOS programs them, and each refresh takes five cycles of the CPU's bus
  as it runs — about 7% on a 4.77 MHz machine. Every byte a disk moves over
  DMA costs the CPU the same five cycles, and a transfer reaches a card's
  memory as a CPU write would.
- **The 80186.** Its peripheral control block sits at `FF00h` in I/O space
  until the relocation register moves it, into memory space if asked, and
  answers word accesses inside the CPU. Its interrupt controller takes the
  8259A on INT0 in cascade mode; firmware sets INT0 special fully nested at
  the lowest priority, so PC software that ends interrupts only at the 8259A
  keeps working. Its timers count a quarter of the CPU's clock, its DMA runs
  unsynchronised or on timer 2's requests, and the chip selects insert their
  wait states — three on the top kilobyte from reset.

## The 8087

`tairix_pcemu::fpu::Fpu` is the numeric coprocessor, modelled from Intel's
8087 data sheet (205835) and the 80287 programmer's reference, whose numerics
the 8087 shares; where both are silent, the 80387 reference's lists of how it
differs from them decide.

- **The interface.** The CPU computes an ESC's operand address and makes its
  dummy read, then hands the bus a `cpu::bus::Escape`: the eleven opcode bits,
  the ESC opcode's physical address (prefixes not counted, as the 8087 records
  it), and the operand's address with the word the dummy read carried. The
  8087 reads the rest of an operand at once and writes a result when its
  instruction ends, each time as bus master: four clocks of request/grant
  exchange, then four clocks and the page's wait states a transfer, taken
  from the CPU. WAIT costs `3 + 5n` clocks, n the times it looked at TEST and
  found it high; while TEST stays high the CPU idles as a halted one does,
  and an interrupt meanwhile returns to the WAIT.
- **Concurrency.** The numeric execution unit runs one instruction at a time
  beside the CPU. An instruction's result is computed when it is issued and
  committed when it ends — registers, status, the memory it stores, the
  interrupt it raises — that instant a board event, so a program that reads a
  result before the 8087 has written it reads what was there. The control
  unit's instructions (FNINIT, FNCLEX, FNSTSW, FNSTCW, FNENI, FNDISI) act at
  once and never hold BUSY. A numeric instruction issued before the last has
  ended, which no assembler's output does, ends that one early.
- **Arithmetic** is exact over the 64-bit significand and rounded once, in
  the control word's mode at its precision for add, subtract, multiply,
  divide and square root. The 8087's own semantics hold where it predates the
  finished IEEE 754: projective infinity by default; one kind of NaN, any
  arithmetic operand of which is invalid; unnormals and pseudo-zeros as valid
  numbers, a short or long denormal loaded as the equivalent unnormal, with
  the 80287 reference's unnormal arithmetic; infinity for a masked overflow
  in chop mode; constants truncated whatever the rounding mode; tags honoured
  over a register's bits.
- **Exceptions.** Invalid, zero divide and denormal are found before an
  operation and, unmasked, abandon it; an unmasked overflow or underflow
  leaves a register its result with the exponent moved by 24 576 and refuses
  a memory store. An unmasked exception sets IR, and with interrupts enabled
  raises INT and holds BUSY until it is cleared.
- **Transcendentals** are computed to 128 bits and rounded once: correctly
  rounded unless the true value lies within 2^-120 of a boundary. Inside the
  operand ranges the data sheet gives, they answer as the 8087 documents —
  FPTAN its ratio as tan x over one; outside them, where its answer is
  undefined, as the 80387 does.
- **Timing** is the typical clocks of Intel's table for each instruction,
  FMUL's faster row by a short operand, and the penalties an exception adds.
- **NMI.** Port `A0h` bit 7 lets NMI through, and the 8087's INT is its one
  source, behind the configuration switch that reports the 8087 — as on IBM's
  planar, nothing raises NMI on a machine without one. The line's rising edge
  is latched until the CPU takes it; the 80186 halts its DMA as it does, until
  the IRET. An empty socket leaves TEST high: a WAIT on a machine with no
  8087 waits forever, interrupts still served, as it did on a PC.

Two things the model does not reproduce: the wrong FPREM quotient bits the
80387 reference reports for some reductions, without saying what they were;
and the exact terms of FPTAN's ratio.

## Video

A machine has up to two display adapters, one monochrome and one colour, as
period machines driving two monitors did; two that would decode the same
ports or memory are refused, and a window a card later grows over another's
leaves that memory with the card that had it.

- **The beam.** Every card renders lazily, one character clock at a time, up
  to the moment the CPU touches it: a register or memory write first lets the
  beam draw everything it has passed, so a change made mid-frame appears
  where the beam was. Frames are opaque RGBA at the card's own dot rate.
- **MDA and Hercules.** A 6845 at 16.257 MHz: nine-dot cells with the line
  pieces' ninth column, the MDA's attribute rules (underline, reverse,
  intensity, blink), and the Hercules card's 720×348 bitmap, second page and
  configuration switch. Bit 7 of the status port tells them apart, as
  software does.
- **CGA.** A 6845 at 14.318 MHz: 40- and 80-column text, the three
  320×200 palettes, 640×200, the border colour, the light pen latch and the
  retrace status; a CPU access waits for the card's next memory slot.
- **EGA and VGA.** Four planes of 64 KiB behind the sequencer and graphics
  controller (write modes 0–3, colour compare, odd/even and chain-4), the
  attribute controller, the VGA's DAC and 256-colour modes, split screen,
  pixel panning, loadable fonts in plane 2, and the vertical interrupt on
  IRQ 2. The EGA is IBM's with its memory expansion fully populated. The
  visible window is wherever the CRT controller's blanking leaves the beam
  lit, so every mode shows its own border. Their memory answers without wait
  states, and only while the misc output's RAM enable connects it; the VGA's
  enable register at `3C3h` disconnects the whole card but itself. The VGA's
  cursor lights its start line to its end line; the EGA's stops before its
  end line, so an end above the start splits the cursor round the lines
  between and an end past the cell lights all of it, as IBM's card is
  measured to.
- **Monitors.** A colour monitor (the 5153's brown, the 5154's 64 colours,
  an analog VGA display), monochrome green, amber or white phosphors — on an
  EGA the secondary blue line is the display's video and the secondary green
  its intensity — and a
  CGA's composite output decoded as a television would: the dot clock is
  four times the colour carrier, so fine 640-pixel patterns become artifact
  colour, and the mode register's mono bit drops the burst. A monitor follows
  a sync pulse only within its oscillators' hold range, so the storm of
  pulses an unprogrammed controller makes is ignored and the picture free
  runs, as on the hardware.
- **Character sets.** First-party code page 437 at 8×8 (CGA), 8×14 (MDA and
  EGA) and 8×16 (VGA), authored as reviewable text and parsed when the crate
  compiles; every line piece is checked to meet its neighbours in all three.

## Storage

- **Floppy drives.** 5¼" 360 KiB (40 tracks at 300 RPM, no disk-change
  line) and 1.2 MiB (80 tracks at 360 RPM), and 3½" 720 KiB and 1.44 MiB.
  The adapter's digital output register selects the drive — one answers only
  while its motor runs — and gates DMA and IRQ 6. A disk reads nothing until
  it is up to speed, 400 ms after its motor starts; a 40-track disk in an
  80-track drive lies under the even tracks, and a 40-track drive cannot
  read an 80-track disk. The PC and XT adapter runs at 250 kb/s; the
  high-density adapter adds the AT's data-rate select and disk-change line
  at `3F7h`.
- **The µPD765A.** Every command: READ DATA and READ DELETED DATA with skip,
  READ TRACK, READ ID, WRITE DATA and WRITE DELETED DATA, FORMAT, the three
  SCANs, SEEK, RECALIBRATE (77 pulses, then an equipment check), SENSE
  INTERRUPT (each unit's status, lowest first), SENSE DRIVE STATUS, and
  SPECIFY's step, head-load and head-unload times at the data rate's clock.
  A sector is found where it passes the head and each data byte moves as it
  passes: over DMA channel 2, where terminal count ends the transfer and an
  unanswered request overruns, or in non-DMA mode through the data register
  with an interrupt a byte. A search gives up at the second index pulse, and
  with no disk turning a command waits for one.
- **Floppy media.** Raw images of the seven standard formats, and ImageDisk
  (`.IMD`) archives with each track's own encoding, density, sector IDs,
  deleted marks and CRC errors; an archive is parsed as hostile input and
  kept write-protected. A raw image refuses, as a protected disk, a write it
  cannot record: a deleted mark, or a format other than its own layout. Bytes
  READ TRACK takes past a sector's recorded data read as gap filler.
- **Fixed disks.** The XT adapter at `320h`–`323h`, on DMA channel 3 and
  IRQ 5: IBM's six-byte command block and the controller's command set (test
  ready, recalibrate, request sense, format drive, track and bad track,
  read, write, verify, seek, initialize drive characteristics, the sector
  buffer, the diagnostics, read and write long), the completion status and
  the sense bytes. Up to two ST-506 drives at 3600 RPM, recorded MFM (17
  sectors a track) or RLL (26), seek at ST-506's 3 ms a step or, with
  buffered steps, at the drive's own pace. The controller reads a sector
  into its one buffer and then hands it to the host, at the DMA controller's
  pace or through the data port, so an interleave of 3 reads a track in
  three revolutions and anything tighter waits a revolution for each next
  sector.
- **What an image keeps.** A fixed disk's image holds its sectors; the
  interleave and the tracks marked bad are its low-level format, which its
  owner keeps beside the image (`Board::fixed_format`) and hands back when
  the drive attaches (`Board::attach_fixed`). The ECC READ LONG and WRITE
  LONG carry reads as zeros and is not kept.
- **The default disk** (`dos::setup::default_disk`) is the 30 MB drive a new
  machine comes with: a Seagate ST-238R (615 cylinders, 4 heads, 26 sectors)
  on an RLL controller, or an ST-4038 (733 cylinders, 5 heads, 17 sectors) on
  an MFM one, formatted at an interleave of 3. It is set up as DOS 5's SETUP
  leaves a disk: one active FAT16 partition from the second track to the last
  cylinder the BIOS offers, labelled `TAIRIX DOS`, with `COMMAND.COM`,
  `CONFIG.SYS` (`FILES=30`, `BUFFERS=20`) and `AUTOEXEC.BAT` (`PROMPT $P$G`,
  `PATH C:\DOS`, the mouse driver loaded) in the root, and every utility,
  `ANSI.SYS` and `MOUSE.COM` in `C:\DOS`. It is made, not shipped, on a sparse
  medium (`storage::sparse::SparseDisk`) that holds only sectors with data in
  them, so the image costs what is written to it.

## Sound

Every source is mixed at the YM3812's own rate — the bus oscillator over 288,
49.716 kHz — and carried to the audio stream's rate by `lib/audio`'s one
resampler. A source that holds a level and changes it at an instant (the
speaker, a DAC) hands the mixer the change and its instant, which is drawn
band-limited with the resampler's own windowed sinc, so an edge folds no alias
into the band; the YM3812's samples are already at the mix's rate. The mix is
AC-coupled as every card's output is. The board renders every 10 ms of
emulated time with every source caught up, so a change never arrives after
its samples have gone, and the front-end's ring is held at its fill by
steering the resampling ratio, at most ±0.5 %.

- **The PC speaker.** Timer 2's output gated by port `61h`'s data bit, edge by
  edge. A tone above the mix's Nyquist — the ultrasonic carrier a program
  parks the speaker on, the pulse train PWM sound rides on — is heard as its
  duty, the level the cone settles to.
- **The YM3812**, at `388h` on an AdLib and on the Sound Blaster at its own
  ports too: nine two-operator channels or six and five rhythm voices, the
  four waveforms behind their enable, feedback and both connections, vibrato
  and tremolo, key scaling of rate and level, the noise generator, the two
  timers and their status (which reads as an OPL2's), composite sine mode,
  and the YM3014B's floating-point output. Its log-sine and exponential
  tables are computed from their formulas, and its envelope's attack and
  decay take the times the application manual gives.
- **The Sound Blaster 2.0** at `220h` or `240h`, on DMA channel 1 and IRQ 2,
  3, 5 or 7, its DSP modelled on firmware version 2.02: direct output,
  single-cycle, auto-initialised and high-speed 8-bit DMA in both directions
  (the ADC records silence), 4-, 2.6- and 2-bit ADPCM with the decoder state
  the firmware carries between blocks, silence, the speaker's ramps, the
  identification, version, test and DMA-identification commands, MIDI's
  modes, and the reset's cold and warm paths. Its quirks are kept: every
  command pauses the stream until a handler resumes it, a host byte waiting
  makes a sample tick play nothing, a high-speed transfer takes a host byte
  as a sample and no command until it ends, and the rate and block size
  cannot change while a transfer runs. The firmware waits at its one output
  latch, so a guest that never reads holds it back rather than filling the
  host's memory. The on-chip RAM playback plays the bytes loaded and then
  silence, and the ROM checksum and RAM test report a passing card.
- **A parallel port's DACs**: a Covox Speech Thing on its data lines, or a
  Disney Sound Source — a byte latched by a short pulse on Select In into a
  16-byte FIFO its own 7 kHz clock drains, Ack reporting the FIFO full while
  Init powers the flag, and the device powering down when Select In is held
  low.

## Ports

Each template has what the period's multifunction cards gave a machine: a
Microsoft mouse on COM1, a printer at `378h`, a game port and a clock card.
An interrupt line two adapters share is high while either drives it, as ISA's
tri-stated adapters share one.

- **Serial ports**: an 8250 or 16450 at COM1 to COM4 (`3F8h`, `2F8h`, `3E8h`
  and `2E8h`, on IRQ 4, 3, 4 and 3), its interrupt gated by OUT2 as on IBM's
  adapter and held off in loopback. The line is levels in time: a device
  sends at its own rate and in its own framing, and the UART samples each
  character at the rate and framing its own registers set. A port set wrongly
  therefore receives what the hardware would: shifted bits, framing and
  parity errors, breaks and overruns, resynchronising as the real one does.
- **The Microsoft serial mouse**: powered by RTS and DTR, it answers `M`,
  then reports its two buttons and its movement in three seven-bit bytes at
  1200 bits a second, as fast as the line carries them. Its counters saturate
  at a packet's reach, so movement faster than the line can report is lost
  rather than arriving late. Where the host's pointer stands over the picture
  is a channel beside it that no modelled hardware has: each change to it
  makes the mouse send a packet, motion and all, and only the emulator's own
  driver reads where the pointer is.
- **The printer**, on a parallel port at `3BCh`, `378h` or `278h`: a byte
  latched on Strobe, Busy until its Ack pulse, Init resetting it, and IRQ 7
  (5 at `278h`) following Ack while the control register enables it. What it
  prints is kept for the application to save; once 64 KiB wait it stays Busy
  until they are drained, so no byte it took is lost.
- **The game port** at `201h`: four one-shots timed as IBM's adapter times
  them, 24.2 µs plus 0.011 µs an ohm of a 100 kΩ potentiometer, an unplugged
  axis never falling, and four buttons.
- **The clock card**: an OKI MSM6242B at `2C0h`, counting from seconds to a
  two-digit year in BCD with every fourth year a leap year; HOLD and BUSY,
  STOP and RESET, the 24/12-hour switch taken up at RESET's release, 30-second
  adjust and the fixed-period flag.

## The BIOS

The firmware is the emulator's own, built in Rust when a machine is made:
the system ROM at `F0000h` with its entry points where PC software calls
them, and the option ROMs of the EGA or VGA at `C0000h` and of the
fixed-disk adapter at `C8000h`. Each service is a host call in a short stub,
honoured only from the emulator's own ROM; a service that waits — for a key,
a disk's interrupt, a timer tick — halts the CPU with interrupts on, so an
idle guest costs no host time. One that waits on a port — a printer's Busy, a
modem's lines, an expanded memory move's time — reads it as firmware does
with interrupts enabled, its reads further apart the longer it waits, steps
aside for any interrupt that comes, and stops where the machine's run ends,
its stub asking again at once as the machine runs on: no service holds the
machine from its window for longer than a slice, whatever it waits for.
Services keep their state in guest memory: the BIOS data area, and
bytes their stub reserves on the caller's stack. A program may have written
anything there or in its registers, so every service is total over them:
addresses and port numbers wrap within 16 bits as the hardware's do, and a
transfer past 64 KiB is the DMA-boundary error.

- **POST.** Memory sized a kilobyte at a time and cleared, unless
  Ctrl+Alt+Del asked for a warm start; the 8259A, 8253, 8237 and 8255; the
  configuration switches read the PC's or the XT's way; the option ROMs found
  from `C0000h` to `F4000h` by their signature and checksum; the serial
  ports found by their interrupt identification, the parallel ports by their
  data latch and the game port by its idle one-shots, listed in the data area
  and counted in the equipment word; the tick count started at the clock
  card's time of day; then the bootstrap from the first diskette, else the
  first fixed disk.
- **Services.** INT 08h (the tick, the diskette motors' timeout, INT 1Ch),
  09h (scan codes through the shift states and locks, Alt's keypad codes,
  Ctrl+Break, Pause, Shift+PrtSc, Ctrl+Alt+Del, a full buffer's beep with
  the keyboard's interrupt held off as the XT holds it), 10h,
  11h, 12h, 13h (diskettes, with media detection by data rate and the
  disk-change line, and fixed disks), 14h (a port's rate and framing set;
  sending once DSR and CTS come, receiving once DSR does; status; each wait
  bounded by the port's timeout in the data area), 15h (as an XT answers),
  16h (with the enhanced keyboard's functions), 17h (printing once Busy
  clears, initialising, status), 18h, 19h, and 1Ah: the tick count, and the
  time and date read and set on the clock card, for years 1980 to 2079; the
  card has no alarm.
- **The expanded memory manager** answers INT 67h on a machine with the
  board, LIM EMS 4.0 complete, with its device header named `EMMXXXX0` where
  programs look for one. It has 255 handles, with names, saved maps and
  every page's owner kept in the board's spare page, so the manager costs no
  conventional memory. It does every function: allocation and reallocation,
  mapping one page or a list by number or segment, the whole and partial
  page maps, map-and-jump and map-and-call, move and exchange between
  conventional and expanded memory, the mappable windows and hardware
  report, alternate map register set 0 with a save area the operating system
  gives, and the operating system's function set under its access key.
- **The EGA's and VGA's ROM.** Modes are set from IBM's video parameter
  table, reached through the save pointer table at `40:A8h`, so a program's
  own parameter table, alpha and graphics font overrides, palette profile and
  dynamic save area are honoured. The VGA shows a text mode at 200, 350 or
  400 lines as AH 12h asks for the next mode set. Cursor emulation moves the
  CGA's 8-line shapes into the cell — the upper half keeps its place and the
  lower half its distance from the foot, which taller cells keep clear — and
  can be turned off. Also: the palette and DAC with grey summing, the
  character generator and its fonts, palette loading, video and refresh
  switched off and on, write string, the display combination, the state
  report and the save and restore of the card's state, and the print screen
  of every row.

## TAIRiX DOS

The DOS the default disk boots is the emulator's own, TAIRiX DOS 5.00, under
its own OEM number: a kernel answering INT 20h–2Fh as MS-DOS 5.00 does on a
machine with no `SHARE`, a folder the user shares served as a network drive
(*Shared folders*, below), its code host calls in the system ROM and its
state in guest memory where MS-DOS documents it, so software that walks or
patches DOS's structures finds them. Any DOS or boot disk the
user supplies boots instead, as it would on the hardware.

- **Boot.** A system disk's boot record is x86 code that checks for the
  ROM's signature and far-jumps to its loader; on another machine it says so
  and waits for a key. The loader lays out DOS's data at segment `0070h`,
  initialises the built-in devices through their headers and reads
  `CONFIG.SYS`: `FILES`, `BUFFERS` with its look-ahead count, `FCBS`,
  `LASTDRIVE`, `STACKS`, `COUNTRY` (code page 437), `BREAK`, `DOS`, `DEVICE`
  and `DEVICEHIGH` — each driver loaded and every header in it initialised
  and linked after `NUL`, a character driver named `CON` or `CLOCK$`
  becoming the system's — `INSTALL` and `SHELL`. A line it cannot take is
  reported with its number. It opens the standard handles (`CON` as 0 to 2,
  `AUX` as 3, `PRN` as 4), runs the `INSTALL` programs, then the shell:
  `COMMAND.COM` unless `SHELL` names another, whose name is asked for when it
  cannot be started.
- **Structures.** The List of Lists with `NUL` in it; the swappable data
  area; the current directory structure and a DPB a drive; the system file
  tables `FILES` sizes; the FCB table, whose least recently used entry past
  the protected ones is reused, an FCB whose entry was taken reopened by its
  name; the `BUFFERS` disk buffers, reused least recently used first; PSPs,
  the memory control block chain and environments.
- **When the buffers reach the disk.** As under DOS: a buffer is written
  when it is reused, when a file is closed or committed, at a disk reset,
  at the end of every call that changes a directory (making, removing,
  deleting and renaming, a new entry, and their FCB forms), and when a
  process ends, every drive's then, its errors failed without asking. A
  machine switched off at the prompt keeps what was done.
- **Calls in progress.** A call is a stack of frames in the swappable data
  area, each one routine's phase and locals. When a routine needs guest code
  — a driver's strategy and interrupt entries, INT 13h, 16h, 1Ah, 23h, 24h,
  28h — the call is made as the CPU would make it, returning to a landing pad
  in the ROM. Nothing of a call is held in Rust, so a machine saved while a
  program waits at a prompt resumes it there. A call made while another waits
  — from an INT 24h or 28h handler, or a TSR — nests, three deep, each level
  with its own guest stack and scratch; a level whose stack a handler has
  run through halts the machine saying "Internal stack overflow".
- **INT 21h**: console input and output with the line editor's template,
  FCBs, handles, directories and searches, attributes, stamps, commit,
  temporary and extended opens, memory with the three allocation strategies,
  EXEC of COM and MZ programs to run, load or overlay, termination and its
  return code, programs kept resident, vectors, the clock, the version, break
  and verify flags, country information for thirteen countries with
  function `65h`'s tables and upper-casing, extended error information, and
  canonical names. A drive's current directory outlives a change of disk: a
  program asking for it (`47h`) has the medium checked and the directory
  looked up again on the new disk, the drive left at its root if it is not
  there. A device or disk error is the INT 24h DOS raises, with the handler's
  answer honoured: a call it fails keeps its error, and function `59h` then
  says it was failed there. INT 23h restarts the interrupted call or ends the
  program by how its handler returns. INT 25h and 26h read and write
  sectors, INT 27h keeps a program resident, INT 28h is called while console
  input waits, INT 29h writes to the console, and INT 2Fh answers the
  kernel's function `12h` services. Line input edits and echoes a line read
  from a file or another device as it does one typed, an LF before the first
  key dropped and a later one only a new line on the screen, as DOS 4.0's
  does, and the input's end ends the line.
- **Devices.** `NUL`; `CON`, reading through INT 16h and writing through INT
  29h; `CLOCK$`; `AUX`, `COM1`–`COM4`, `PRN` and `LPT1`–`LPT3` through INT
  14h and 17h, each COM port set to 2,400 baud, 8 data bits, no parity and
  one stop bit as DOS starts. IOCTL reaches every device's information and
  requests.
- **The block driver** keeps a unit a floppy drive — two on a machine with
  one, taking turns at it — and a DOS partition of each fixed disk, the first
  primary then each logical drive of the extended one; a partition with no
  file system yet is a drive with the BPB DOS 4 would format it with. Every
  sector moves through the guest's INT 13h vector, never across a DMA
  boundary, the diskette parameter table DOS's copy in RAM with each
  transfer's last sector the medium's own. A failed call is made again after
  a reset, three times in all, but a write-protected or empty drive fails at
  once. Media are checked by the change line and told apart by their serial
  number, label and BPB, so a diskette put back is known from a stranger, and
  a transfer a change meets midway is refused rather than finished on another
  disk. The generic IOCTL gets and sets a drive's parameters — the BPB DOS
  builds for the drive, or the one a formatter sets, until the drive's is set
  again — reads and writes tracks, verifies them several a call, formats a
  diskette's through the BIOS and a fixed disk's by verifying them, asks the
  BIOS whether a drive takes a format, and gets and sets the medium's ID and
  the access flag, which reads clear for a partition holding no volume DOS
  can read until a BPB is set for it. A track formatted, through the IOCTL or by
  any program through INT 13h, which DOS hooks, makes the drive's next media
  check say the disk changed.
- **`STACKS=n,s`** runs the NMI and IRQ 0 to 6 on one of `n` stacks of `s`
  bytes kept by DOS; with none free the machine halts saying "Internal stack
  overflow", as DOS does.
- **`ANSI.SYS`** is a loadable console driver: cursor placement, movement,
  saving and the position report, erasing, graphics renditions, screen modes
  and wrap, and key reassignment.
- **`MOUSE.COM`**, run from an `INSTALL=` line or the command line, finds the
  Microsoft serial mouse on the first COM port it answers on and stays
  resident with INT 33h functions `00h`–`24h` as version 6.26 has them: the
  cursor in every text and graphics mode the BIOS sets — a software or
  hardware text cursor, the graphics cursor's masks and hot spot — motion as
  mickeys and as a position within the range set, presses and releases
  counted, event handlers with alternates for the shift keys, sensitivity and
  the double-speed threshold, the region that hides the cursor, its state
  saved and restored, and the driver disabled and enabled. On a VGA it puts
  back the graphics controller's registers it changes to draw; an EGA's
  cannot be read, so they are left as the BIOS sets them. While the host's
  pointer stands over the picture, each packet places the driver's pointer
  where it stands, on the 640-dot-wide virtual screen of the mode, kept to the
  range set; the packet's motion is still counted in mickeys.
- **Volumes.** `dos::volume` lays out a FAT12 or FAT16 volume holding a tree
  of files under TAIRiX DOS's boot record; the default disk is made with it.
- **Shared folders** (`dos::redirect`). A folder the user shares is a network
  drive: its CDS marks it one, with `X:\` its root and no DPB, and it takes
  the first letter within `LASTDRIVE` no disk has, once the boot has given
  the disks theirs — at once for a folder shared later. Every call naming a
  path on it, every handle and FCB call on a file of it, and every search of
  it is answered by the front-end's server in one step, the machine stopped
  meanwhile: no redirector runs in the guest, and INT 2Fh's `11h` services
  are not offered. A host name DOS could not hold is reached by an alias —
  up to six bytes of its base, `~` and a number, then the first three of its
  last ending — kept, with each entry's place, for as long as its directory's
  listing is, so a search under way keeps its place, in the DTA or in an
  FCB's search fields, its drive byte marked remote. A file the host keeps
  from being changed is read-only; every file carries the archive bit, which
  DOS may set or clear to no effect, and any other attribute is set only
  where the host already has it. A file's date and time
  are its modification instant on the machine's clock, and one `57h` sets is
  kept at the close; free space is the host volume's, counted in clusters of
  at most 128 sectors; a seek from the end asks the file's length now, which
  another program may have moved. The drive's volume label is the share's,
  made from the folder's name. A file open on a share withdrawn answers
  "network name deleted" and closes
  without error. Functions `1Fh` and `32h` find no DPB on a share, and the
  drive IOCTLs answer "invalid function", as DOS answers for a network drive.

## COMMAND.COM

The shell is TAIRiX DOS's own `COMMAND.COM`: a stub on the disk whose
behaviour is Rust, reaching DOS only through INT 21h as any program does, its
state in its own memory.

- **Starting.** `/P` makes the primary shell, which runs `AUTOEXEC.BAT` from
  the root of the drive DOS booted from, or asks for the date and time
  without one, and never ends; `/C` runs one command and ends with its code;
  any other shell ends at `EXIT`, or once its redirected input has no line
  left. `/E:n` asks for an environment of 160 to
  32,768 bytes, and a shell given a larger one keeps all of it, saying "Out
  of environment space" only past 32,768. `COMSPEC` names the directory the
  shell is told it is in, once `COMMAND.COM` is found there ("Specified
  COMMAND search directory bad" otherwise), else keeps the one the shell was
  given, else names the boot drive's root.
- **Commands.** `BREAK`, `CALL`, `CD`/`CHDIR`, `CHCP`, `CLS`, `COPY`,
  `CTTY`, `DATE`, `DEL`/`ERASE`, `DIR`, `ECHO`, `EXIT`, `FOR`, `GOTO`, `IF`,
  `LH`/`LOADHIGH`, `MD`/`MKDIR`, `PATH`, `PAUSE`, `PROMPT`, `RD`/`RMDIR`,
  `REM`, `REN`/`RENAME`, `SET`, `SHIFT`, `TIME`, `TRUENAME`, `TYPE`, `VER`,
  `VERIFY` and `VOL`, with MS-DOS 5's messages. `DIR` takes `/P`, `/W`,
  `/S`, `/B`, `/L`, `/O` with its sort keys and `/A` with its attributes,
  each undone by `/-`, its defaults from `DIRCMD`. `COPY` joins files with
  `+`, takes `/A` and `/B` from where each appears, reads a device as text,
  keeps a source's date and time, and never copies a file onto itself.
  `CTTY` takes only a character device. `LH` runs a program as any other is
  run, there being no upper memory.
- **Programs.** A word that is no internal command is looked for as named,
  then with `.COM`, `.EXE` and `.BAT`, in its own directory if it names one,
  else the current directory and each of `PATH`'s.
- **Lines.** `<`, `>` and `>>` redirect a command's standard input and
  output, and `|` runs a pipeline through temporary files in `TEMP`'s
  directory, or the current drive's root, each deleted once read.
- **Batch files.** `%0` to `%9` with `SHIFT`, `%NAME%` from the environment,
  `@` and `ECHO`, labels and `GOTO`, `IF` with `ERRORLEVEL`, `EXIST`, `==`
  and `NOT`, `FOR %%v IN (set) DO` over words and wildcards, `CALL` running
  one batch file inside another, and `PAUSE`. A batch file is reopened for
  each line where the last one ended, as DOS reads it, so it may be changed
  while it runs.
- **The prompt.** `PROMPT`'s `$` codes, `$N$G` without one: the drive and
  directory, the date, time and version, `<`, `>`, `|`, `=` and `$`, escape,
  a new line, and a backspace that rubs out. `$P` on a drive DOS cannot read
  says "Current drive is no longer valid". With echo on, a new line comes
  before each prompt typed at and each batch line echoed, except after an
  empty line, so a command's output stands apart as in MS-DOS.
- **Errors and breaks.** The shell's INT 24h says what failed — the error,
  reading or writing, the drive or device — and asks for the answers DOS
  allows; its INT 23h asks whether a batch file is to end, then ends the
  command broken into, the shell's own going back to the prompt and a
  program's ended by DOS. Both ask on the console however the command's
  standard input and output are redirected, and take only a key pressed
  after the question. A failure answered there ends the command, nothing
  more asked or said of it, and memory a command held is given back however
  it ends. A console with no answer left — `CTTY` moved to a device whose
  input has ended — fails the call where DOS allows it, else aborts it, and
  ends the batch file.
- **Not provided:** INT 2Eh, the back door through which a program runs a
  line in the primary shell.

## The utilities

Each utility is a program like `COMMAND.COM`, a stub file whose behaviour is
Rust reaching DOS only through INT 21h, 25h and 26h and the block driver's
generic IOCTL, so it works on any drive DOS has, an installed driver's
included. Each has its MS-DOS counterpart's switches, messages and exit codes,
and a line it cannot take gets the messages of DOS's common parser. A question
asked until it is answered stops when a redirected input runs out — a Ctrl+Z,
or an empty line from a device or at a file's end: a yes or no is then no, `MODE`'s shift
test keeps the display as it stands, a disk `SYS` or `FORMAT /S` would ask for
is said to be missing, and `XCOPY`'s file-or-directory question, `CHKDSK`'s,
whose no would free what it found, and a disk `DISKCOPY` waits for end the
utility as Ctrl+C would.

- **Text.** `FIND` with `/V`, `/C`, `/N` and `/I`, ending with 0 once a line
  was found, 1 when none was and 2 when a file could not be searched. `MORE`,
  its keys taken from the console however its text arrives. `SORT` with `/R`
  and `/+n`, in the country's collating order, equal lines kept in order, its
  input up to 64 KiB.
- **Files.** `ATTRIB` shows and changes the read-only, archive, system and
  hidden attributes, with `/S` in every directory below. `DELTREE` deletes
  files and whole directories whatever their attributes, asking about each
  match unless `/Y`. `MOVE` renames on one drive and copies then deletes
  across two, telling each move, and overwrites only when told to or given
  `/Y`, on its line or in `COPYCMD`. `XCOPY` copies files and, with `/S` and
  `/E`, the directories below, taking `/A`, `/M`, `/D:date`, `/P`, `/V` and
  `/W`, and asks whether a destination it cannot tell is a file or a
  directory. `FC` compares two files as text a line at a time, with `/A`,
  `/C`, `/L`, `/LBn`, `/N`, `/T`, `/W` and `/nnnn`, or a byte at a time for a
  program's file or with `/B`.
- **Disks.** `LABEL` makes, changes and deletes a volume label. `TREE` draws
  a directory's tree, with `/F` its files and with `/A` in plain characters.
  `CHKDSK` checks the FAT against the directories as MS-DOS 4's does —
  lost chains, cross-links, sizes, bad links and entries, damaged directories
  — and with `/F` mends them, lost chains made files or freed; `/V` names
  every file, and a named file is checked for contiguity. `SYS` writes TAIRiX
  DOS's boot record over a disk's own, keeping its BPB, and copies
  `COMMAND.COM` to its root: the kernel is in the ROM, so there is nothing
  more to place. `DISKCOPY` copies a diskette track by track as MS-DOS 4's
  does, through two drives or one taking turns, as many cylinders a pass as
  memory holds and with `/1` one side; a target whose format differs is
  formatted as it is written, the copy is given a serial number of its own, a
  track that cannot be read or written is told and counted, and it ends with
  DOS 5's codes.
- **`FDISK`** sets up the fixed disks' partitions through MS-DOS 4's menus:
  a primary DOS partition of the whole disk, the first disk's made active,
  or of a size typed in megabytes or as a share; an extended partition and logical drives in
  it; the active partition; each deleted, a logical drive only once its
  label is typed; and the tables and drives shown. Given a disk and `/PRI`,
  `/EXT` or `/LOG` sizes it makes them at once. A partition's system area is
  verified first, a bad track moving it past. Nothing is written until it
  ends, and a change that may move drive letters restarts the machine unless
  `/Q`, with which it ends with 1 while the first disk has no DOS partition
  and 2 when nothing was asked for or changed.
- **`FORMAT`** lays a disk out as MS-DOS 4's does, with `/S`, `/V`, `/B`
  (which reserves nothing), `/1`, `/4`, `/8`, `/T` with `/N`, and `/F` with
  every size the drive takes, each checked against the others and the drive.
  Each track is formatted and verified, the share done shown as it grows; a
  bad track's clusters are marked, a fixed disk's only those that cannot be
  read again. Then come the serial number, made from the date and time, the
  boot record, FATs and root, the label asked for unless `/V` gave one or
  `/8` allows none, the space report and, of a diskette, "Format another
  (Y/N)?". A fixed disk's label, if it has one, must be typed, and the
  warning answered, before anything is written, and its partition's type is
  set to the FAT kind the volume got. Ctrl+C puts the drive's parameters
  back before it ends.
- **`DOSKEY`** stays resident, keeping the commands typed in a buffer of
  `/BUFSIZE` bytes, 512 unless asked. ↑ and ↓, Page Up and Page Down recall
  them; ←, →, Home, End and Ctrl with ← and → move along the line; Ins, or
  `/INSERT` from the start, puts text in rather than over it; Esc clears it;
  and DOS's F1 to F6 work on the last line typed. F7 lists the commands, F8
  finds the last one beginning as the line does, F9 takes one by its number,
  and Alt+F7 and Alt+F10 forget the commands and the macros. `DOSKEY
  name=text` defines a macro the shell's prompt runs, with `$1` to `$9`, `$*`,
  `$T` between its commands, and `$G`, `$L`, `$B` and `$$`; a line beginning
  with a blank runs the command a macro shadows. `/MACROS` and `/HISTORY`
  list them. A line any program reads from the console through function
  `0Ah` is edited and recalled too.
- **`EDIT`** is MS-DOS 5's full-screen editor: `EDIT [file] [/B] [/G] [/H]
  [/NOHI]`, a file named that is not there made by its first save, `/B` for a
  monochrome monitor, `/H` for the most rows an EGA or VGA shows, `/NOHI` for
  one without bright colours. The File, Edit, Search, Options and Help menus
  open with Alt pressed and let go, F10, Alt and a menu's letter, or the mouse.
  Open lists a pattern's files, the directories and the drives; Save As goes
  to a directory or saves, asking before it replaces another file; Find and
  Change match case and whole words, Change asking at each place or changing
  all; Print sends the document or the selection to the printer; Display sets
  the colours, the scroll bars and the tab stops, kept in `EDIT.INI` beside
  `EDIT.COM`. Shift with any move selects, the keypad's included; Shift+Del,
  Ctrl+Ins and Shift+Ins cut, copy and paste; Tab and Shift+Tab indent and
  outdent the selected lines; WordStar's Ctrl keys, Ctrl+Q and Ctrl+K among
  them, move, delete and set four bookmarks, and Ctrl+P puts in a control
  character. The mouse places the cursor, drags a selection, double-clicks a
  word and works the scroll bars, the menus and the dialogs. A save writes the
  whole document to a new file before the old one is replaced, so a full or
  write-protected disk leaves the file as it was, and says why. Changing a
  document and leaving it asks first. The screen it found is put back when it
  ends.
- **System.** `MEM` with `/PROGRAM`, `/DEBUG` and `/CLASSIFY`, reading DOS's
  chains where they lie, every walk bounded against a damaged one. `MODE`
  sets the display's mode and shift, the console's columns, lines and typing
  rate, a printer's columns, lines and retry, a printer rerouted to a serial
  port, and a serial port's rate and framing, and reports each device's
  status; what needs a resident part keeps one, its tables found at
  `0000:0530`.

## The machine worker

The application holds the machine's media and the user's grants; the
machine runs in a worker, the application's own binary re-entered as a
capability-empty sandbox. Everything the worker sends is checked before it is
acted on (`front::Host`), and the first event that cannot be believed ends
the worker.

- **The session** (`link`). The application starts the machine with its
  configuration and fixed disks, then runs, holds, resets and feeds it input;
  the worker answers with frames, sound, requests for sectors, the state of
  its drives and the blocks it asks to be compiled. Both directions are one
  codec, total over hostile bytes.
- **Frames** cross through the one region the application shares with the
  worker: two slots for each adapter, each as large as the adapter's monitor
  shows — 754 by 358 for the MDA and Hercules card, 768 by 232 for the CGA,
  800 by 600 for the EGA and VGA. The worker writes a finished frame into a
  slot it holds and names it; the application copies it out and hands the
  slot back. A frame the same as the adapter's last is not sent, and one
  larger than its monitor shows is reported once as out of range. Each frame
  knows the picture its card displayed inside the border, which the host's
  pointer is placed against.
- **Sound** crosses as 16-bit stereo, and the application reports how much
  its own buffer holds, which the mix is steered by.
- **Sectors** cross on request: a run of up to 64 from the block the guest
  read, or one sector written. At most one request is outstanding, and the
  machine stops while it waits, so the guest never sees how slow the host's
  storage is; a write is answered only once the application has written it,
  so a failed write is the guest's write fault. What was fetched is kept in a
  reclaimable cache of clean file data, a sixteenth of the media within the
  worker's room, keyed under a seed the application draws, and the last run
  fetched is kept whatever memory pressure takes from the cache. An
  ImageDisk archive is fetched whole and parsed in the worker. A fixed disk's
  new low-level format is reported for the application to keep beside the
  image.
- **Input** is credited: the application sends at most 64 commands beyond
  what the worker last said it applied, and holds the rest in order, motion
  run together and a key pressed into a full queue dropped with its release.
  The worker gives keys to the keyboard as fast as its 16-byte buffer takes
  them, so a burst is typed rather than overrun, and credits each as it
  arrives, so a guest that reads no keys never holds the window shut against
  a reset; a press beyond what it holds is dropped with its release, and a
  release of every key is never refused.
- **Shared folders** cross as requests too (`folder`): what a node is, a
  search's next entry, an open, a read or write of at most 32 KiB, a file's
  length asked or set, a close with the stamp DOS set, a commit, a removal, a
  rename, a new directory, and the volume's space, every path in DOS's
  spelling and checked
  as it is decoded. The machine asks in host terms — a length, an instant —
  and makes every DOS conversion itself. A file read in small pieces is
  fetched a whole piece at a time; what was fetched is forgotten once its
  file is written or opened again, and a read reaching past it asks, so a
  file another program grows is read to its new end. The front-end serves
  each share on the disk worker, the folder's directory capability held
  there alone: every name it serves is an entry of a directory beneath the
  folder, so nothing a share does reaches outside it, and a request of a
  share stopped meanwhile is answered that it is gone, unseen.
- **Pacing.** The worker runs the machine in slices of at most 4 ms of
  emulated time, catching up with the host's clock whenever a command
  arrives and at the latest once it is 4 ms behind, and waits on its pipe
  between them, so input is taken as it arrives and a held machine costs
  nothing. It is never let fall more than 100 ms behind: a host
  too slow to keep up runs the guest slower than real time, never faster,
  and time spent waiting on a request is not caught up.

## The application

`PCEmulator.app` is one binary in three roles. Started by the user it is the
front-end; re-entered through `SPAWN_SELF` in the session-worker role it is
one machine, in a canonical sandbox holding its two pipes and the spawn share
and nothing else; re-entered as a one-shot worker serving `compiler` it is a
machine's compiler. The front-end holds no filesystem capability: every
machine lives in the application's own store. Several machines run at once,
each in a window of its own over a worker of its own. Nothing that waits runs
on the loop that owes the windows their frames: the **library worker** reads
and writes the library, makes, copies and removes disks, keeps each machine's
last screen, and opens a machine's disks and sound and starts its worker; the
**disk worker** serves every running machine's sectors, so a long copy never
stalls a running guest; the **compile worker** has each machine's compiler
compile the blocks it asks for and installs their code. Each answers in the
order it was asked, which is what keeps a machine from starting on a disk
still being copied.

### The store

- **The library record** holds every machine's number, name, configuration,
  fixed disks with their drives and low-level formats, and clock offset, in
  the session's own codec closed by a CRC-32. It is written alternately as
  `library-a` and `library-b`, a generation ordering the two, and read as the
  newer copy that checks out and sits in the blob its generation belongs to.
  One write is under way at a time, and the next goes to the other blob only
  once the last is on the store, so a write cut short leaves the newest whole
  copy standing; changes made meanwhile are written once, together.
- **Each machine** adds `disk-<n>-<drive>` for each fixed disk it has and
  `state-<n>` for its last screen, compressed with the first-party LZ codec,
  and, while it is suspended, its snapshot and when it was suspended. A
  header naming both is checked on its own and each part on its own, so a
  card's screen is read without the snapshot behind it.
  Numbers are minted once and never reused. A machine is made disk first,
  record after; it is deleted record first, files after, once a record
  without it is on the store. A load sweeps away every file no machine in the
  record owns: what a creation or a deletion cut short left.
- **A record that cannot be read** is said as such. Neither copy checking out
  is an empty library; that load sweeps nothing, and no number a file carries
  is minted again, so nothing is overwritten on a guess.
- **The first run** makes the Turbo XT with the default 30 MB disk.

### The windows

- **The library window** shows each machine as a card of its last screen and
  its name, the commands — *New…*, *Start*, *Settings…*, *Duplicate* and
  *Delete…* — above, and the chosen machine's parts and whether it is off,
  running, being kept or suspended in the band beneath. The cards scroll when
  there are more than the window shows. A double-click or *Start* starts a
  machine, resuming one suspended, or raises its window; a running machine is
  neither copied nor deleted, one being kept is left alone until it is, and
  *Delete…* asks first. A suspended machine is copied with its snapshot. A
  secondary press on a card chooses it and opens its menu: *Start* (*Resume*
  while it is suspended, *Show Window* while it runs), *Settings…*,
  *Duplicate* and *Delete…*, and while it runs the machine's own rows (below).
  A change repaints only the cards it touched. Closing the library window
  leaves running machines running, and closing every window leaves the program
  on the icon bar: a click on its slot (`AppBarClick::RaiseOrOpen`) or the
  *Machine Library* row brings the library back, and only *Quit* ends it,
  suspending every running machine first. A suspending machine's window stays
  open, its bar saying *Saving…*, until its snapshot and the images it was
  taken against are kept (`app::keeping::Suspends`), so a session logging out —
  which waits on open windows and then ends what is left — never ends the
  program part-way through writing one.
- **The new-machine assistant** and **Settings** are owner-modal dialogs over
  the library window, of the shared controls in one field column: a choice
  list stays open under the pointer until a choice or a press elsewhere takes
  it away, and the press that takes it away presses nothing beneath it. The
  assistant offers the four machines, the name, and whether the default disk
  is made; a machine with no fixed-disk controller — the IBM PC — is offered
  none. Settings change a
  machine's hardware but never its disks, and open read-only unless it is
  off: a suspended machine is started and shut down to be changed.
- **A machine window** opens to show a VGA screen at one pixel a dot,
  640×480 above the drive bar, no larger than the screen holds. It shows the
  picture at the monitor's shape, four units wide for every three high,
  centred and bordered in black: each frame is
  enlarged through `lib/raster`'s frame scaler, each of the card's dots a block
  of whole pixels, and only the rows a changed frame reaches are drawn again
  and presented. Closing it suspends the machine (below); the machine's menu
  shuts it down.
- **The drive bar** along a machine window's foot has a lamp for each drive
  the machine has, lit while the drive works — a diskette drive's while its
  motor runs, a fixed disk's while a command is under way — and held lit for
  60 ms of the machine's time after, so a single access is seen; the worker
  sends a lamp only when what it shows changes, and the front-end ends a
  worker that lights a drive the machine does not have. Pressing a diskette
  drive opens its menu: *Insert Disk…* asks the trusted picker for an image,
  writable where the picker hands it over so; *Eject* takes it out. A disk
  image dropped on the window goes into the diskette drive under it, or the
  first, read-only; only files the content-type registry calls disk images
  are offered the drop. An image goes in as an ImageDisk archive, by its
  magic, read whole by the worker, or as a raw image of one of the seven
  standard diskette sizes; anything else is refused before the machine sees
  it. A press that begins on the bar never reaches the guest, and one the
  guest took ends with the guest.
- **The machine's menu** opens from the *Machine* button at the bar's
  trailing end — a display and a down chevron, *Paused* with a pause sign
  while the machine is, *Saving…* as it suspends — on a primary or a
  secondary click; a secondary click on any slot of the bar opens that slot's
  menu. It holds *Pause*, *Reset* (off and on), *Send
  Ctrl+Alt+Delete* — the desktop keeps the chord, so the window offers it —
  *Copy Screen Text* and *Paste* (below), the processor's *Speed*, chosen
  from the speeds the settings offer, as a turbo switch would for the run,
  *Display*, *Share a Folder…* (below), *Take Screenshot…*, *Full Screen*,
  *Debugger* (below), checked while its window is open, and *Shut Down*,
  which turns it off and keeps only its last screen. A row acting on the
  machine in place — *Pause*, *Reset*, the speeds and the tube's effects
  among them — may be chosen with the menu kept up
  ([menus](./menus.md#choosing-with-the-menu-kept-up)). A menu up — the
  bar's, a drive's or a card's in the library — shows the machine as it now
  stands: once each pass of events is in, the rows it would build are
  compared with those it was given and only a change is restated, so a
  diskette ejected, a paste ending or the window filling the display reaches
  it however it came about. The bar stays in full screen, since only the
  application can leave it.
- **Sharing a folder.** *Share a Folder…* asks the trusted picker for a
  folder, handed over confined beneath itself (`docs/src/desktop/session.md`)
  and writable only where the picker says so. It is served as the machine's
  next share, up to four, labelled by the first eight bytes of its name DOS
  could hold; the clock it reports times on moves with the machine's offset.
  Each share takes a slot on the bar after the drives, reading its drive —
  or that it has none, until DOS gives it one — and whether it is read-only;
  pressing it offers *Stop Sharing*, which takes the drive away and cuts off
  the files open on it. A reset keeps a machine's shares, each taking its
  drive again as DOS boots; a snapshot keeps none, so a machine resumed or
  started again shares nothing until asked.
- **Copy and paste** are in code page 437 (`lib/cp437`). *Copy Screen Text*,
  offered while the picture shows a text mode — the worker says when the
  first adapter goes between text and graphics — puts the screen on the
  clipboard as it draws: each cell's glyph, the faces and arrows of
  `01h`–`1Fh` included, a line a row, each row's trailing blanks and the blank
  rows below the last left off. The worker reads the cells as the card's
  controller scans them — from its start address, across a split, on a
  second Hercules page — and the window does only the conversion. *Paste*
  types the clipboard's text on the machine's keyboard: a line break of any
  spelling presses Enter and a tab Tab; each other character is its key on
  the US layout, Shift held where Caps Lock and the layout call for it, or
  Alt and its code on the keypad, as a DOS user typed the upper half. Typing
  takes only what arrives as itself: the faces and arrows are control codes
  to a PC — the heart is Ctrl+C, the left arrow Escape — so they are left
  out, as is what the code page lacks once typographic quotes, dashes, spaces
  and the ellipsis are plainly spelt and a letter's accents the page lacks are
  dropped; a combining accent changes the letter before it where the page has
  the two as one, and what draws nothing passes unseen. A character is begun
  only once the guest has read every key before it — none held, none on its
  way from the keyboard, none in the BIOS's buffer — so a program that reads
  slowly, or empties the buffer before it reads, loses nothing. Text crosses
  to the worker within a window of 1024 characters; a paste reads at most
  65536 characters and types at most as many, and the window says what was
  left out. While text is being typed the row reads *Stop Pasting*; pressing
  a key on the machine's keyboard stops it too, and a reset or a suspend drops
  what is left.
- **Suspending.** Closing a machine's window, or quitting, asks its worker
  to stop: the bar's slot reads *Saving…* and asks for nothing more, and the
  worker sends the machine whole — processor, memory, every chip, card and
  drive — compressed with the first-party LZ codec, in pieces. Once it has
  come the window closes, what the machine wrote to its images is made
  durable, and only then is the snapshot kept, so no machine is ever resumed
  against disks older than it. A worker that has not sent its snapshot
  within ten seconds is turned off. Quitting waits, for at most five seconds,
  for every machine to be kept.
- **Resuming.** Starting a suspended machine reads its snapshot on the
  library worker and starts its worker to take it back before it says it is
  ready; its window shows the screen it was suspended with. The snapshot is
  then set aside — the state rewritten without it — and only once that is on
  the store does the machine run, so a machine cut short is never resumed
  again from under what it went on to write. Diskettes come back out of
  their drives, the drives' disk-change lines set, since the images were the
  front-end's; the clock card is set again from the host's clock; and the
  BIOS moves its tick count on by the time the machine was away, as an APM
  BIOS does at resume, each midnight passed counted in the rollover byte,
  which TAIRiX DOS adds to its date. A speed chosen from the menu was for the
  run before. A snapshot that does not take, or a saved state that does not
  check out, starts the machine afresh and says so.
- **The snapshot** is the worker's own: a version, the configuration and
  fixed-disk drives it was taken of, then every part in order. Each part's
  fields are each kept or named as left as built — the destructuring makes a
  field added without saying which a compile error — and every value is
  checked as it is read, so a snapshot of another build or another version is
  refused and one that does not read leaves the machine to be built again.
  What each page and port maps to is worked out again from the devices.
  Before one is taken every device is brought up to the processor's time,
  and a restore requires every device's position within a second of it and
  no more bus time owed than a second, so no device is caught up over an
  unbounded span.
- **The picture tube** (`lib/crt`, the terminal's too) draws a machine's
  picture with scan lines, a glow, a phosphor afterglow, a noise floor or a
  wobble, each turned on from *Display* at a period monitor's strength and
  kept with the machine in the library. With an effect on, the picture is
  drawn into a surface of its own, the tube's stages over it, and only what
  they changed is copied into the window; a moving effect steps on a one-shot
  frame deadline, never faster than twenty a second, and a picture with none
  on costs nothing more than it did. The stages spread across a pool started
  the first time one draws.
- **A screenshot** is the screen as it stood when it was asked for, at the
  monitor's shape, each dot a block of whole pixels and at least 640 wide,
  written as a PNG to where the trusted Save panel says, on the library worker.
- **The keyboard** is read raw while a machine window holds it: each key's
  going down and coming up is the machine keyboard's key at its place,
  modifiers and locks included, the guest's typematic making the repeats. The
  desktop keeps Ctrl+Alt+Delete, its capture chords and the Meta key; a key
  the enhanced keyboard lacks reaches nothing. Losing the keyboard lets every
  held key go.
- **The mouse** follows the desktop's pointer while TAIRiX DOS's own INT 33h
  driver holds both INT 33h and its port's interrupt: the pointer is hidden
  over the picture and the driver takes its place from where it stands.
  Whenever either vector is someone else's, the program is reading the mouse
  itself: a click on the picture asks the desktop for the pointer — on its
  release, since a held press keeps the seat — and the mouse then moves by
  the pointer's own motion until the Meta key is tapped, the title saying
  so. The middle button reaches nothing: the mouse has two.
- **Sound** plays through a stream of each machine's own, opened at 44.1 kHz,
  16-bit stereo, the machine mixed to the rate the stream was granted. What
  the stream's ring cannot yet take waits, bounded, its oldest dropped beyond
  the bound, and the machine's mix is steered by how much the two hold. A
  seat with no sound runs the machine silent, and says so.
- **The clock card** reads the host's clock plus the machine's own offset.
  Setting the time in the guest moves the offset, as a real card's battery
  kept what it was set to, and the library keeps it; the zone a reader is in
  is not needed, so the application reads no file. A fixed disk the guest
  low-level formats has its new format kept beside its image the same way.
- **A machine that cannot start, or stops**, is said in an alert over the
  library window.

## The debugger

*Debugger* in a machine's menu opens a window of its own on the machine: its
processor, its 8087, its code, memory and stack as they stood at the last
stop, its breakpoints, and the controls that stop it and send it on.

- **The probe.** Opening the window puts the probe (`debug::probe`) on the
  machine's board, as an in-circuit emulator is put on a bus; closing it
  takes the probe off and the machine goes on from any stop. A snapshot never
  holds it. It holds up to 32 breakpoints: an instruction executed at an
  address, memory written or read or written over a range, ports read or
  written over a range, and an interrupt entered, with the function in AH
  where one is given. Memory watches see every master's cycles — the
  processor's, the 8087's, DMA's, and the firmware's acting for the processor
  — but not code fetches; port watches see the 80186's control block too.
  A watched page is withheld from translated code, which leaves for the
  interpreter at a watched access and runs on elsewhere.
- **Where it stops.** The processor asks the probe at each instruction it
  begins, after the interrupts due there are taken. A code breakpoint stops
  it before its instruction; a watch, or an interrupt breakpoint, at the next
  instruction, the report saying who made which cycle with what value. A
  repeated string instruction the emulator paused between slices begins
  nothing, so a breakpoint on it fires once. While a code breakpoint or a
  step is held, translated blocks do not run: they have no instruction
  boundaries to ask at.
- **Stepping.** *Step* runs one instruction, into what it calls, but runs a
  hardware interrupt's handler taken on the way to its return. *Step Over*
  runs a CALL, INT, INTO, BRKEM, a repeated string instruction, or in 8080
  mode a CALL, call on condition, RST or CALLN, to the instruction after it,
  the stack at or above where it stood. *Step Out* runs until the procedure
  the processor is in returns — RET, RETF, IRET, RETEM, the 8080's RET or a
  return on condition taken — at its own frame, stopping where it returns to.
  A stop holds the machine's time still; a break asked of a paused machine
  runs it only to the next instruction. A reset keeps the breakpoints and
  lets go of the stop.
- **What it shows.** The worker reads and writes out everything the window
  shows — the 8087's values exactly, to nineteen significant digits, the
  listing in MASM's Intel syntax, by the processor's own decoder, the 8087's,
  and in 8080 mode the 8080's mnemonics — and the front-end checks every
  answer against what it asked before showing it. The listing shows lines
  before the code address too, found by voting across every start within a
  reach of up to 128 bytes for the boundaries most decodings agree on.
- **The window.** Continue, Break, Step, Step Over, Step Out and *Add
  Breakpoint…* run along the top beside why the machine stopped; the listing,
  its current line marked and a press in its gutter adding or removing a
  breakpoint there; memory as bytes and text; the registers and the 8087, a
  value that changed since the stop before in the accent colour; the stack;
  the breakpoints, each removed from its cross. F5 continues or breaks, F9
  toggles the current line's breakpoint, F10 steps over, F11 steps and
  Shift+F11 steps out. The code and memory panes go where an address typed in
  their fields says — `segment:offset`, an offset alone, a physical address,
  or register names such as `DS:SI` — and scroll with the wheel. Each look the
  window asks for is answered before the next is asked, the latest view
  winning, and each change repaints only the panes it reaches.

## Conformance against the hardware

The CPU is tested against 32,192 instructions executed on physical
processors — an AMD 8088, an Intel 80C86 and an NEC V20 — from the
`SingleStepTests` suites (MIT). The committed subset, its provenance and its
licence are in `userland/apps/pcemu/assets/cpu-vectors/`;
`cargo xtask pcemu-vectors --import <dir>` regenerates it from a local copy of
the suites and is never part of a build.

- Every register, every byte of memory and every flag must match, including
  the flags Intel and NEC document as undefined. The exception is the V20's
  IMUL, IMUL-immediate and successful DIV/IDIV, whose undefined flags come
  from internal datapath state; there the flags NEC defines are compared.
- Timing must stay within a mean of 0.4 cycles per instruction of the
  hardware on the 8086, and 2.9 on the 8088 and 2.8 on the V20, where the
  residue is T-state bus arbitration the instruction-level model does not
  see.

The 80186 and 80188 have no captured suite; their timing follows Intel's
data sheet.

The 8087's arithmetic is held to independent oracles: IEEE doubles and
singles for sums, products, quotients and roots rounded to nearest at 53 and
24 bits, reference rounding of exact 128-bit results at every precision and
mode, square roots bracketing their radicands, and transcendentals at points
computed to 80 digits and against the host's double functions across their
ranges. `fuzz_fpu` runs hostile instruction streams over hostile state and
memory, and IEEE agreement through the whole instruction path.
