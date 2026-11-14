# RISCOS-EMULATOR — `RISCOSEmulator.app`: RISC OS applications on the TAIRiX desktop

Binding under `AGENTS.md`. This plan owns `RISCOSEmulator.app` (title *RISC
OS*). It has six parts:

- an ARM processor that runs 26-bit and 32-bit RISC OS code;
- a first-party RISC OS — its kernel, filing, graphics and font systems, BBC
  BASIC V and VI, the C library, the Wimp and the Toolbox — under which RISC OS
  applications run as TAIRiX desktop applications;
- machine mode: models of the Acorn chips, built in tiers, for programs that
  drive them directly (§12);
- the translator that runs hot code natively;
- the shared libraries this work hoists or adds (§13);
- the desktop facilities it needs (§14).

Read first (§15.18): `AGENTS.md` §2.2, §2.12, §2.16, §2.20, §5.4, §19.2, §19.5,
§19.6, §19.11, §24, §26, §27, §28; `plans/PCEMU.md` §0, §3 and §4 (the house
pattern: host calls, guest state in guest memory, three processes, the permanent
interpreter, BR26); `plans/BEEB.md` §0, §6 and §7 (the clean-room boundary,
an application's own documents folder, the reference harness);
`plans/OPENFILE.md` OF12 (the subtree grant); `plans/APPWIN.md`; `plans/COMPOSITOR-WORK.md`;
`plans/GUI-CONTROLS-DESIGN.md` §11.17–§11.18; `plans/NEW-MENUS.md`;
`plans/NEW-TASKBAR.md` T7; `plans/TOOLTIPS.md`; `plans/POINTING.md`;
`plans/DISPLAY.md` D13; `plans/FILES-INTERACTION.md`; `plans/SAVE-DIALOG.md`;
`plans/ICONS.md` I17–I18 and I21; `plans/VIEW.md` (the sprite decoder);
`plans/DRAW.md` DR6; `plans/APPDATA.md`; `plans/CAPABILITY_USE.md` CU6;
`plans/SOUND.md`; `plans/NETWORK.md`; `plans/FONT-SERVICE.md`;
`plans/DISK-IMAGES.md`; `plans/ARCHIVES.md`; `plans/ARXFS-METADATA.md`;
`docs/src/security/sandbox.md`.

## Ledger

| Item | What it is | Status |
|---|---|---|
| RO0 | This plan, its jump-sheet row and its `PLAN.md` row | done |
| RO1 | The `userland/apps/riscos` leaf subtree: its rows in the charter's layout and layering, and `deps-check`'s rule with its tests | planned |
| RO2 | One association path: the file manager and the session read the same stores, the picker's matcher is replaced by `applications_for`, the precedence rule is stated once, open and launch decisions are logged, and a test proves browsing never launches | planned |
| RO3 | RISC OS names defined once in `lib/fsmeta::preset::acorn`: leaf and path spelling, the `,xxx` suffix, filetype names; each content type's canonical filetype is defined beside `lib/browse`'s table; the archive, ADFS, icon, window-protocol, picker and document-host copies move onto it | planned |
| RO4 | RISC OS colours and font names defined once: the `&BBGGRR00` palette word in `lib/image`, the font-name map made public in `lib/drawing`; Draw's private copy is deleted | planned |
| RO5 | `lib/softfloat`: exact binary32, binary64 and double-extended arithmetic in every rounding mode with its exceptions, plus the FPA and x87 memory formats; the 8087 moves onto it with its digest unchanged | planned |
| RO6 | `lib/obey`: GSTrans, system variables, expressions, Obey parameters and `OS_ReadArgs` templates; `lib/icon`'s `!Boot` reader is rebuilt on its read-only mode | planned |
| RO7 | `lib/bbcbasic`: the BASIC II and BASIC V/VI token tables, tokenising and listing; OpenMicro's ROM builder emits its keyword table from it | planned |
| RO8 | `lib/vdu`: the Acorn VDU vocabulary, RISC OS's numbered screen modes and the 8×8 system character set; OpenMicro's MOS and `lib/image` move onto it | planned |
| RO9 | The ARM integer core: the one A32 table, the interpreter, the 26-bit and 32-bit programmer's models, the two profiles, exceptions, the disassembler and encoder, the conformance vectors | planned |
| RO10 | Thumb and Thumb-2, and interworking | planned |
| RO11 | The FPA: every instruction and precision, rounding and exceptions, the transcendental results | planned |
| RO12 | VFP and Advanced SIMD, through ARMv8-A's AArch32 additions | planned |
| RO13 | The guest address space: pages and permissions, aborts, task slots, the 26-bit window, dynamic areas, host pages, write tracking, the memory policy | planned |
| RO14 | The reference harness and the recorded observation corpus | planned |
| RO15 | The kernel: SWI dispatch and errors, vectors, services, events, callbacks and tickers, heaps, system variables, `OS_Byte` and `OS_Word`, environment handlers, and the kernel's remaining SWIs | planned |
| RO16 | Modules: the built-in inventory and its versions, guest modules in either mode, instances, `RMEnsure` and replacement | planned |
| RO17 | The command line: `OS_CLI`, Obey and Exec, each executable type's run action, every built-in module's `*` commands, the boot utilities | planned |
| RO18 | Territory, International, MessageTrans, ResourceFS and the built-in resources | planned |
| RO19 | FileSwitch and HostFS: the SWIs, the session's discs, the documents folder as the shared session's own disc with `!Boot`, Choices and the boot sequence, names and metadata, confinement, the RAM discs | planned |
| RO20 | BBC BASIC V and VI: workspace in guest memory, the language, numbers, the machine interface, the assembler, errors, the editor | planned |
| RO21 | The VDU drivers: modes, the VDU stream, `OS_Plot`, palettes, screen banks, screen memory | planned |
| RO22 | Sprites and JPEGs: `OS_SpriteOp`, SpriteExtend, output to sprites | planned |
| RO23 | ColourTrans | planned |
| RO24 | Draw and DrawFile | planned |
| RO25 | The Font Manager: TAIRiX faces through the font service, Acorn outline fonts, painting, the cache | planned |
| RO26 | SharedCLibrary; the FPEmulator, VFPSupport, CallASWI and DDEUtils SWIs | planned |
| RO27 | The session worker and its protocol: contexts, scheduling, pacing, the relays, containment | planned |
| RO28 | The front end: launches, single-tasking screens, fullscreen, input, settings, the session window, the bundle | planned |
| RO29 | Sound: SoundDMA, SoundChannels, SoundScheduler, SharedSound, the voices, `audio-v1` | planned |
| RO30 | The debugger: SWI and service trace, breakpoints and watches, stepping, the listing, the Debugger module | planned |
| RO31 | Machine mode, tier 1: the Archimedes running RISC OS 3.1 (its layout and version answers), VIDC1's colour registers and MEMC's video and cursor addresses on the frame clock, the boundary table, the refusal, and the offer to run again in machine mode | planned |
| RO32 | `!App` activation and the RISC OS application registry: double-click runs an application, directories are handed over as subtrees, declared types appear in Open With, resources are handed over at launch, isolation follows provenance | planned |
| RO33 | The window model the Wimp needs: borderless and attached windows, client-initiated move and resize, relative moves and move deltas, restacking among one's own windows, root-viewport scroll bars, furniture selection, the work area | planned |
| RO34 | Pointer: the middle button reaches window content; applications may set their own cursor images | planned |
| RO35 | Menus for a hosted environment: children built on demand, a window as a child, scrolling plates, rows that state no function, the popup grab | planned |
| RO36 | Hosted icon-bar slots, with clicks and menus answered on demand | planned |
| RO37 | Save drags started by an application, and asking the desktop to open a held document | planned |
| RO38 | Tooltips long enough for hosted interactive help | planned |
| RO39 | The Wimp: tasks, slots, polling, messages, the Task Manager | planned |
| RO40 | The Wimp: windows, redraw, nested windows and panes, icons, the caret, templates, error boxes | planned |
| RO41 | The Wimp: menus in the TAIRiX look | planned |
| RO42 | The Wimp: drags, data transfer and the clipboard, bridged to the desktop | planned |
| RO43 | The Wimp: the icon bar, interactive help, quitting with the session | planned |
| RO44 | The RISC OS look: furniture, tool sprites, menus and error boxes drawn by the emulator | planned |
| RO45 | The Toolbox, with every object module and gadget | planned |
| RO46.1 | The Internet module and Resolver over the front end's sockets | planned |
| RO46.2 | URL messages handed to the desktop's URL hand-off | blocked: the hand-off, `plans/BROWSER.md` BR4 |
| RO47 | Printing to PDF: PDriver over `lib/drawing` | planned |
| RO48 | TaskWindows, the command line in a window, and RISC OS programs run from the TAIRiX terminal | planned |
| RO49 | TextEdit opens and saves tokenised BASIC through `lib/bbcbasic` | planned |
| RO50 | Machine mode, tier 2, the timed machine: ARM2 and ARM3 cycles with MEMC's timing and DMA, IOC whole with the keyboard, the rest of VIDC1 and MEMC, raster effects, a modelled machine's speed for any 26-bit program | planned |
| RO51 | Machine mode, tier 3: the floppy controllers with raw disc images, I²C's CMOS RAM and clock, the RiscPC's IOMD and VIDC20 | planned |
| RO52 | The guest-translation runtime hoisted from PCEmulator into `lib/*`, with PCEmulator moved onto it | planned |
| RO53 | The ARM translator: A32 and T32 blocks lowered to `lib/codegen`, a code cache keyed by physical page, the compiler sandbox, differential fuzzing | planned |
| RO54 | Native floating point: binary32 and binary64 in `lib/codegen`'s IR and backends, with FPA, VFP and NEON lowered through them | planned |
| RO55 | QEMU verticals on every native target | planned |
| RO56 | Software acceptance over a recorded corpus | blocked: needs a corpus of real software a developer holds outside the tree |
| RO57 | Machine mode held to real hardware: RO14's harness run on an Archimedes and a RiscPC, their observations pinning what the data sheets leave open | blocked: needs an Archimedes and a RiscPC a developer holds |
| RO58 | wasm32 | blocked: the wasm32 port runs no userland (`plans/ARCHSUPPORT.md`), and its host cannot instantiate a generated module |

Items are built in ledger order. Each lands complete — tests, fuzz targets,
docs — before an item that depends on it starts. The gate runs once a session's
run of items is finished.

## 0. Binding decisions

1. **Applications first; a machine only for a program that drives one.** The
   emulator implements RISC OS's programming interface: the SWIs, vectors,
   services, `*` commands, file formats and data structures the PRMs document.
   - RISC OS applications' windows are TAIRiX windows, their files are TAIRiX
     files, and their sound goes through the TAIRiX audio path.
   - Acorn's chips exist only in **machine mode** (§12): a single-tasking
     program that drives them directly gets models of them for itself.
   - Those models are built in tiers, which `AGENTS.md` §27.5 permits once the
     user has been asked, and the user has authorised it. Each tier is complete
     within a boundary declared register by register: exact under its stated
     time model inside it, and refusing loudly beyond it.
   - A silent partial model stays forbidden: a register that does nothing, or
     an approximation that draws a plausible wrong picture.
   - A program that touches what no built tier models stops, and the front end
     names the chip and the register.
2. **One RISC OS: RISC OS 5's interface, with 26-bit compatibility.**
   - The kernel and every built-in module follow the current RISC OS 5
     release's documented interface. That release is fixed when RO15 lands,
     and each module records its version. It is a superset of earlier
     releases' interface for system-friendly software.
   - A 26-bit application or module runs with the processor in a 26-bit mode
     and gets the conventions RISC OS 3 and 4 gave it: the combined PC/PSR,
     flag preservation across SWIs, a slot of at most 28 MiB, and everything
     it is handed below 64 MiB (§3).
   - Version answers (`OS_Byte` 0 and 129, module versions, the Wimp version)
     are RISC OS 5's, except in machine mode, where they are the modelled
     machine's RISC OS (§12). The descriptive strings name the emulator, so the
     guest is told where it is.
3. **Every line is Rust, the guest's operating system included.** No binary,
   ROM image, font, sprite or message file from Acorn, Castle, RISC OS Ltd,
   RISC OS Developments or RISC OS Open is shipped or fetched.
   - The kernel, modules, BASIC and the Toolbox are Rust running as host calls
     (decision 5).
   - The ARM code the guest can see — vector defaults, library entry
     trampolines, BASIC's routine table — is emitted by the crate's ARM
     encoder, never assembled from text.
   - Resources (message files, the system font, the tool and file-type
     sprites) are authored in the tree.
   - A user may load their own modules and run their own copies of RISC OS
     components; they are guest code like any other.
4. **The clean-room boundary.** Implementation may read:
   - the published manuals: the PRMs, the RISC OS User Guide, the BBC BASIC
     Reference Manual and the Style Guide;
   - the ARM Architecture Reference Manuals and data sheets: ARM2, ARM3,
     ARM610/710, FPA10, SA-110, ARM7TDMI, and the ARMv7-A and ARMv8-A AArch32
     manuals;
   - sources whose licence permits it and is compatible, such as RISC OS 5's
     Apache-2.0 sources. These are read for behaviour, structure layouts and
     documented constants; no code is copied;
   - black-box observation of genuine RISC OS run as an opaque program
     (RO14).

   Nothing else is read: leaked or unlicensed sources (the `RiscOS_371` tree
   included), disassemblies, annotated ROM listings. Genuine ROM, module, font
   or message-file bytes never enter the tree, a fixture or a build.
5. **Host calls.** The kernel and the built-in modules are Rust, entered from
   guest code at a trap. There are two kinds of trap:
   - a SWI the dispatcher owns;
   - for an entry point the guest sees as an address (a vector's default
     claimant, a SharedCLibrary entry, a BASIC routine, a Toolbox handler), an
     encoding the architecture defines as permanently undefined.

   The second is honoured as a host call only when fetched from the emulator's
   own read-only host pages. Anywhere else it is the undefined instruction the
   modelled processor takes (`plans/PCEMU.md` decision 2).
6. **Guest state lives in guest memory where software reads it.** Each of the
   following is kept in guest memory in its documented layout:
   - system variables;
   - the module chain and each module's workspace;
   - the RMA and system heaps, in the `OS_Heap` format;
   - dynamic areas and sprite areas;
   - BASIC's program, variables, heap and stacks;
   - SharedCLibrary's `FILE` blocks, `ctype` tables, `errno` and locale data,
     as the library's headers lay them out;
   - MessageTrans file blocks and Toolbox objects.

   Rust holds only derived caches, rebuilt or invalidated from guest memory:
   a hash index over the variable chain, the decoded window tree, the Wimp's
   internal records that no program reads. A guest that writes over guest
   state therefore corrupts only itself. Nesting — a SWI calling a guest
   vector claimant that calls a SWI — is bounded and fails closed past the
   bound.
7. **The guest is hostile.**
   - Guest code, guest memory and every file a guest reads are untrusted. That
     covers images, modules, BASIC programs, sprites, templates, Toolbox
     resources, fonts, Draw files and message files. All of it is decoded and
     run only in the session worker, a capability-empty `lib/sandbox` worker.
   - The front end parses nothing a guest controls beyond the session framing.
     It validates every frame against what it asked for, and ends the session
     on the first frame that fails.
   - Every host call is total over guest state: a malformed request answers
     the error RISC OS gives. It never panics, loops without bound or
     allocates without bound.
   - Every chain or walk carries a hop bound: modules, vectors, callbacks,
     variables, heap blocks, menu trees, the window stack.
8. **Three processes.** They differ in what they hold:

   | Process | Does | Holds |
   |---|---|---|
   | Front end (`Run`) | windows, menus, slots, input, the audio stream, the file, socket and font relays, settings, the session window; relays compile requests and performs the BR26 install | the bundle's grant and the user's hand-overs |
   | Session worker | the ARM processor, guest memory, the kernel, modules and every task of the session; the interpreter and installed translations | nothing: a canonical sandbox with one spawn share |
   | Compiler | translates guest blocks into host code it stages | nothing: a canonical sandbox |

   The compiler never runs what it writes, and the worker never holds a mapping
   that is both writable and executable. The front end never reads compiled
   code.
9. **A RISC OS session is one guest world and one security domain.** Its tasks
   share one RISC OS world, as tasks on one RISC OS machine do: one set of
   system variables, one RMA and module chain, one Wimp task list and message
   bus, one clipboard owner, one filetype registry.
   - RISC OS has no file protection between tasks. Every grant any task of a
     session receives is therefore reachable by all of them.
   - What bounds a session is what it holds: the grants the user handed it,
     and the network only if allowed (§11.2). It ends with its last task.
   - The **shared session** hosts programs launched from the user's own
     storage.
   - A program launched from removable media, an attached disc image or
     archive, or a network share starts in an **isolated session** of its own,
     unless the user runs it in the shared session by an explicit choice at
     launch. The same applies to anything of unknown provenance, which is
     treated as foreign.
   - A setting can tighten this (isolate everything) but never loosen it.
   - An isolated session never receives the user's own RISC OS disc, the
     documents folder (§6).
   - Sessions interoperate only through the desktop's own protocols: drags,
     saves and the clipboard (§9.6–§9.7).
10. **Wimp tasks are co-operative; single-tasking programs get a screen of
    their own.**
    - Wimp tasks yield only at `Wimp_Poll`, as the Wimp makes them. A task
      that never polls freezes its session, not TAIRiX, and the session window
      offers to stop it.
    - A single-tasking program — a game, a BASIC program, a command-line tool —
      gets its own screen: its own VDU state, mode, palette and screen memory,
      in a window of its own. On RISC OS such a program would take over the
      desktop until it exited; here it takes a window.
    - It runs pre-emptively beside the Wimp's tasks, as a TaskWindow's program
      does. Each Wimp task set, TaskWindow and screen is a **context**.
    - Contexts switch on a slice of the emulated clock, and only where RISC OS
      could take a callback: in USR mode, with no SWI in progress. No module is
      ever re-entered.
11. **The interpreter is permanent; a translation is an exact replacement**
    (`plans/PCEMU.md` decision 6).
    - The interpreter defines the processor's behaviour. It runs whenever
      translation is refused or unavailable, and it is the oracle the
      translator is differentially fuzzed against.
    - A translated block replaces the interpreter exactly over its guest bytes,
      and any write to those bytes drops it. Self-modifying code therefore works
      without `OS_SynchroniseCodeAreas`, as it did on ARM2 and ARM3.
12. **Time.**
    - The guest's centisecond clock is the host's monotonic clock since the
      session began. The exception is a context on a timed machine (§12), whose
      clock is its own cycles, paced to the host at the modelled machine's
      speed.
    - Its real-time clock is the host's wall clock plus an offset the session
      keeps. A guest may set its own clock; the host's is never touched.
    - Idle costs nothing. When every task waits in `Wimp_Poll` for an event
      other than a null and no context can run, the worker waits on its pipe
      with the next deadline: a `Wimp_PollIdle` time, a ticker, an
      `OS_CallAfter`.
    - A task that polls for null events gets the next one at once if it did
      measurable work since its last poll. Otherwise it waits for the
      display's next frame. An idle spinner therefore costs one poll a frame,
      and a background worker keeps its throughput. The threshold is measured
      (RO39).
13. **Presentation and scale.**
    - The desktop mode a session's Wimp sees is the TAIRiX work area at 32
      bits per pixel.
    - Its eigen factor is the one that brings an OS unit nearest its nominal
      1/180 inch at the desktop's scale: 1 at the reference scale, 0 at double
      it. Eigen 0 is RISC OS's high-resolution form, in which the Wimp takes
      `!Sprites11`.
    - Nothing is resampled: the guest's pixels are the screen's, so text and
      lines stay sharp at every scale. An OS unit is near its nominal size, as
      on a RISC OS machine's own monitor.
    - A single-tasking screen keeps whatever mode its program selects. It is
      scaled sharp at the mode's pixel aspect through `lib/crt`, shared with
      OpenMicro and PCEmulator.
14. **Input.**
    - **Buttons.** Select, Menu and Adjust are the primary, secondary and
      middle buttons, since the desktop's own secondary click opens menus.
      RISC OS's physical order (left, middle, right) is a setting.
    - **Keys for the Wimp.** Wimp tasks receive RISC OS key codes from the
      window's typed input, so the user's layout types what its keys say.
    - **Raw keys.** `INKEY` with a negative number and `OS_Byte` 121 and 129
      read the raw keyboard's state by internal key number. A single-tasking
      screen takes the raw keyboard for its window (`plans/DISPLAY.md` D13);
      the session keeps Ctrl+Alt+Delete and the lone Meta tap.
    - **The pointer.** It is seamless until the guest moves it itself (`OS_Word`
      21's set-position reasons). The window then takes pointer lock: relative motion is
      integrated into the guest's pointer, clamped to its bounding box, until a
      lone Meta tap releases it.
15. **Determinism.** The worker is a pure function of three inputs: the bytes
    it is given, its input events stamped in emulated time, and the clock
    readings it is handed. A recorded session therefore replays exactly, which
    is what the reference harness and the cross-target digest (RO55) rely on.
    Snapshots are not kept (§18).
16. **No auto-run.** Nothing a guest supplies runs without an explicit user
    action.
    - Listing or browsing runs no guest code anywhere: `plans/ICONS.md` I17
      reads `!Boot` as data and never runs it.
    - An application's `!Boot` runs once per session, when the application is
      first launched, before its `!Run`.
    - A resource application handed over at launch (RO32, §14) has its `!Boot`
      run when it is handed over.
    - The boot sequence on the user's own RISC OS disc (§6) runs when the
      shared session starts. Starting it is the user's launch, and the disc is
      the user's own. An isolated session runs no boot sequence.

## 1. The crate subtree (RO1)

`userland/apps/riscos/` is a leaf subtree, as `userland/games/` is: its crates
compose each other and `lib/*`, and nothing outside it depends on them. RO1
does the following:

- amends `AGENTS.md` §3 and §17.4 with its row;
- adds the subtree to `deps-check`'s leaf-subtree rule, with tests;
- logs the amendment in `PLAN.md`.

The crates are `tairix-riscos-*`, listed here from the bottom of the dependency
graph up. Each owns one subsystem behind a stated interface:

| Crate | Owns |
|---|---|
| `arm` | The processor: the instruction tables, the interpreter, the programmer's models, the FPA, VFP and NEON units over `lib/softfloat`, the guest MMU mechanism (pages, permissions, aborts, write watch). It knows nothing of RISC OS. |
| `kernel` | The memory map and policy, SWI dispatch, vectors, services, callbacks, heaps, system variables, the module seam every other crate implements, `OS_Byte`/`OS_Word`, the CLI, Territory, MessageTrans, ResourceFS. |
| `filing` | FileSwitch, HostFS, the RAM discs, Choices. |
| `vdu` | Modes, the VDU drivers, `OS_Plot`, sprites, ColourTrans. |
| `machine` | Machine mode: the Acorn chip models, their boundary table, the machine clock and its event scheduler. It drives a screen context through `vdu`'s interface. |
| `draw` | Draw and DrawFile. |
| `font` | The Font Manager. |
| `basic` | BBC BASIC V and VI. |
| `clib` | SharedCLibrary and the floating-point support modules' SWIs. |
| `wimp` | The Window Manager, Templates, the Task Manager, drag modules, interactive help, the RISC OS look's renderer. |
| `toolbox` | The Toolbox and its object modules. |
| `sound` | The sound system and its voices. |
| `net` | The Internet module and Resolver. |
| `print` | PDriver. |
| `session` | The worker: contexts, scheduling, pacing, the protocol's worker side. |
| `jit` | The ARM translator's front end and code cache. |
| `app` | `Run` in its three roles, the front end, the bundle. |

Splitting or merging a crate later is in-place evolution. Moving a
responsibility across this table needs the plan updated in the same change.

## 2. The processor (`arm`; RO9–RO12)

### 2.1 Instruction sets and profiles

- **A32** from ARMv2 to ARMv7-A, plus ARMv8-A's AArch32 user-level additions:
  load-acquire and store-release, `SEVL`, CRC32, the floating-point additions,
  and the optional Cryptographic Extension. Each is reported through the ID
  registers the profile exposes.
- **T32**: Thumb and Thumb-2, with interworking as each architecture version
  defines it (RO10).
- **The FPA** (RO11), **VFPv2 to VFPv4 with D32**, and **Advanced SIMD**
  including ARMv8's AArch32 additions (RO12).
- Absent, and undefined as on a core without them: Jazelle (`BXJ` is `BX`),
  ThumbEE, the Security and Virtualization Extensions, LPAE, and any coprocessor
  not listed here.

A **profile** fixes every implementation-defined choice. It fixes the stored PC
offset, unaligned access, `SWP`, the ID registers, the exception vectors' base
and which programmer's models exist. There are exactly two outside machine
mode; machine mode's timed tier adds ARM2 and ARM3 profiles, which carry their
cycle timings (§12):

- **26-bit:** ARMv4 with the 26-bit models, as the SA-110 has them. A
  misaligned `LDR` returns the rotated word, as those cores did.
- **32-bit:** ARMv7-A with ARMv8-A's AArch32 additions, configured as RISC OS 5
  configures it. Alignment faults are on, so legacy rotated-load code faults as
  it does there.

A profile is chosen for each program a task runs:

- An AIF image's address-mode word or a module's flags declare it (§4.3).
- A program that declares nothing — an image without an AIF header, BASIC, an
  Obey file — runs on the 26-bit profile. The classic software that declares
  nothing predates the declaration.
- The user may choose the other profile for an application, as one chooses
  Rosetta for an application. The choice is kept with the application's
  registry entry (RO32).
- A program in machine mode runs on the modelled machine's profile (§12).

A module or callback keeps the profile it was loaded under.

### 2.2 One table

Each instruction set has one encoding table. Everything that needs the
encodings is derived from it, so there is one definition of what the bytes
mean (`plans/BEEB.md` §1; `plans/PCEMU.md` §1.1):

- the decoder;
- the interpreter's dispatch, through a pre-decoded form cached per guest page;
- the disassembler (the Debugger module, RO30);
- the encoder used by the host pages, the test programs and BASIC's assembler.

Each instruction set has an encode and decode round-trip test, and the decoder
is fuzzed.

### 2.3 Programmer's models

- **26-bit.** R15 holds N, Z, C, V, I and F in bits 31–26, the word PC in bits
  25–2, and the mode in bits 1–0 (USR26, FIQ26, IRQ26, SVC26).
  - The model covers the `TEQP` family's PSR writes, `MOVS pc, …` and
    `LDM …^` restoring flags, and the 64 MiB address space with the address
    exception.
  - Each 26-bit mode shares the banks of its 32-bit counterpart.
- **32-bit.** CPSR, an SPSR per exception mode, and the banks of FIQ, IRQ, SVC,
  ABT and UND. SYS shares USR's.
- **PC reads.** The PC reads as the instruction's address plus 8 in ARM state
  and plus 4 in Thumb state. Each write to the PC interworks or not, as the
  architecture version defines.
- **Exceptions** enter the guest's handlers through the kernel's vector model
  (§4.2), never through Rust panics.
  - Undefined instruction, prefetch abort, data abort and address exception
    reach the environment handlers.
  - An unclaimed coprocessor instruction reaches the undefined-instruction
    vector, as on hardware.
- **Exclusives.** The exclusive monitor is single-core, as RISC OS is. It is
  cleared on every exception and on every context switch.

### 2.4 Floating point and SIMD

- **The FPA.** RO11 covers:
  - every data operation and its "fast" single-precision forms, `URD` and
    `NRM` included;
  - the transfers, compares and loads and stores in single, double, extended
    and packed-decimal form, and `LFM`/`SFM`;
  - the four rounding modes and the five exceptions with their trap enables;
  - the FPSR's system ID.

  Arithmetic is `lib/softfloat`'s, with FPA's extended format and its
  unnormalised values. The transcendental results — `SIN` to `POL` — are
  FPEmulator's bits, pinned from recorded observation (RO14). An enabled trap
  enters the guest's floating-point exception path, as the FPE's would.
- **VFP.** FPSCR modes: rounding, flush-to-zero, default NaN, alternative half
  precision, the cumulative flags, and the short-vector `LEN`/`STRIDE` mode
  VFPv2 executes.
  - Every result is the bits `lib/softfloat` defines, so every target computes
    the same ones.
  - The interpreter may use the host FPU only where that is proven
    bit-identical: a correctly rounded operation in round-to-nearest, with
    ARM's NaN and flag rules applied to its result. A differential test holds
    that path to `lib/softfloat`.
- **Advanced SIMD.** Every instruction of ARMv7 and ARMv8's AArch32 set, with
  its saturation flag.

### 2.5 Memory interface

- Every load and store goes through the guest MMU (§3). An access the page
  forbids is the data or prefetch abort the profile defines.
- A store into a watched granule reports itself. Granules are watched for
  screen damage and for translated code.
- Instruction fetch reads the same pages, so there is no stale-code window to
  model.

### 2.6 Conformance

- **Vectors.** The conformance vectors are per instruction and per profile:
  registers, flags, memory and the abort taken. Their sources:
  - captured from genuine cores a developer holds — a 26-bit ARMv4 machine,
    and an ARMv7 or ARMv8 one running RISC OS 5 — by the harness program RO14
    builds;
  - where a public vector set's licence is compatible, that set, with its
    notice kept beside it (`plans/BEEB.md` §7's SingleStepTests precedent).

  `cargo xtask riscos-vectors` imports them; a crate test samples them.
- **Unit tests.** Each instruction class has them: shifter carry, the
  writeback forms of `LDM`/`STM`, the 26-bit PSR writes, interworking, and
  every floating-point exception.

## 3. The guest address space (RO13)

- **Pages.** 4 KiB pages through a two-level table owned by the worker, with a
  direct-mapped software TLB in front of it. Each page carries USR and SVC
  read/write permissions and whether it may be executed.
  - Unmapped pages abort. The low 32 KiB holds only the workspace software is
    known to read on the profile's RISC OS. The rest of it aborts, as RISC OS
    5's relocated zero page makes a null pointer abort.
  - **The host pages** are read-only and executable (decision 5).
- **Task slots.** Application space begins at `&8000` and is per task. A task
  switch swaps one subtree pointer, so it costs O(1) whatever the slot's size.
  - A 26-bit task's slot is at most 28 MiB.
  - A 32-bit task's slot grows to what the memory policy allows.
  - Translations survive switches because they are keyed by physical frame
    (§16).
- **The 26-bit window.** Everything a 26-bit context can be handed a pointer to
  lies below 64 MiB: the RMA, the system heap, the Wimp sprite pool, message
  file blocks, and the dynamic areas a 26-bit caller creates. A 32-bit task's
  slot never grows over the window while 26-bit code is resident in the
  session. A growth refused for that reason is the "no more memory" answer, and
  the session window says why.
- **Heaps and areas.**
  - The RMA and system heap are `OS_Heap` heaps in guest memory and grow on
    demand.
  - Dynamic areas implement `OS_DynamicArea` whole, including the guest
    handlers called around a resize.
  - Screen memory for single-tasking screens lives in guest memory and is
    write-tracked by granule into damage.
- **Machine mode's layout.** A program in machine mode is loaded into its
  machine's layout instead (§12), since software that drives the chips
  hard-codes it. The chips' I/O space is device pages, which answer to the chip
  models rather than to memory.
- **The memory policy.**
  - **The session's ceiling.** It is derived from the machine (a fraction of
    usable RAM) and from the user's limits, and is settable downward.
  - **What the guest is shown.** Its RAM size is the ceiling, and its free
    pool is the ceiling less what is used.
  - **Growth.** It happens before refusal (`AGENTS.md` §24.1). Exhausting the
    ceiling fails closed as the RISC OS error the call defines.
  - **Caches.** The font cache, the read cache and translated code are
    reclaimable under `lib/reclaim`'s pressure bands.

## 4. The kernel (RO15–RO18)

### 4.1 SWIs

- **Decoding.** A SWI number decodes into its fields: the owner bits, X (bit
  17), the chunk and the offset. `OS_WriteI` covers `&100`–`&1FF`.
- **Dispatch.** The dispatch table has one entry per chunk. Built-in modules
  register Rust handlers; guest modules register their SWI handler and decoding
  table.
- **Errors.** An error is V set with R0 pointing at an error block: a word for
  the number, then a terminated message. Errors are raised to the environment's
  error handler for a non-X SWI.
- **Flags.** Each mode keeps its own convention for N, Z and C. The 26-bit
  return goes through R14's combined PC/PSR; the 32-bit return keeps the
  caller's flags unless the SWI documents otherwise.
- **The rest of the dispatcher.** `OS_CallASWI`, `OS_CallASWIR12`,
  `OS_SWINumberToString` and `OS_SWINumberFromString` are complete.
- **Unknown SWIs.** A SWI no module provides is RISC OS's "SWI not known"
  error.
- **No partial modules.** A module is either provided whole or absent (§4.3).
  It is never partly provided.

### 4.2 Vectors, services, events, callbacks

- **Vectors.** Every software vector `OS_Claim` and `OS_AddToVector` serve:
  WrchV, RdchV, ByteV, WordV, FileV, ArgsV, FindV, GBPBV, FSCV, UpCallV,
  EventV, TickerV, KeyV, InsV, RemV, CnpV, MouseV, SpriteV, DrawV, ColourV,
  PaletteV, SerialV and the rest the PRMs list.
  - Claimants run in claim order.
  - The default claimant is a host-page entry into Rust.
- **Service calls.** Each one goes to every module's service handler. Where
  the module declares a service table, the call goes through it.
- **Events.** `OS_Byte` 13 and 14, delivered through EventV.
- **Callbacks.** Transient and permanent callbacks run only at callback-safe
  points.
- **Timed calls.** `OS_CallAfter`, `OS_CallEvery` and TickerV run on the
  emulated centisecond clock.
- **Masked interrupts.** The kernel's interrupt-driven work — tickers, events,
  keyboard input, vertical sync — is held while the guest masks IRQs, and taken
  when it unmasks them, as hardware holds an interrupt.

### 4.3 Executables and modules

- **Executable containers**, decoded in the worker:
  - **Absolute (`&FF8`).** Loaded at `&8000` and entered there. Its AIF header
    is read only to classify the image. The address-mode word gives 26 or 32,
    and an image without a header is 26-bit (§2.1). The workspace and size
    fields are checked as RISC OS 5 checks them. A compressed or
    self-relocating image runs its own header code, as on RISC OS.
  - **Utility (`&FFC`).** Run as RISC OS runs a transient utility: loaded into
    the RMA, entered with the documented registers, and freed when it returns.
  - **Module (`&FFA`).** The header's offsets are validated against the file
    before anything runs: start, initialisation, finalisation, service, title,
    help, command table, SWI chunk, handler, decoding table and code, message
    file, flags. The flags' 32-bit bit decides the module's mode.
  - **ELF (`&E1F`).** It runs exactly as on RISC OS, through the guest's own
    `!SharedLibs` when present. Otherwise it gets the error RISC OS gives. No
    ELF loader is built in.
- **The module inventory** (RO16). It classifies every module in the RISC OS 5
  ROM as one of:
  - **provided** (complete), with the version whose interface it implements;
  - **absent**: hardware drivers (podules, IDE, SCSI, USB, PS/2, IIC, SDIO,
    serial and parallel ports, Econet and its filing systems, CMOS) and modules
    only those serve;
  - **bridged**: provided, with its outside effect relayed to the desktop.

  The inventory is a table in the crate, documented on the docs page. An
  absent module is invisible to `OS_Module`, `RMEnsure` and SWI lookup.
- **Guest modules** may be loaded in either mode, with instances
  (`Module%Instance`), the full initialise, finalise and service protocol,
  `*` commands, help, SWIs and message files.
  - A guest module may replace a built-in one, as `RMLoad` replaces a ROM
    module.
  - The exception is a module that is a bridge: the kernel, FileSwitch and
    HostFS, the Window Manager, the Font Manager, SoundDMA, the Internet
    module. Loading over one of these is refused with an error naming the
    module. Without the bridge, the guest would have no way to reach the
    desktop.

### 4.4 System variables, GSTrans, expressions

- Variables of every type — string, number, macro, expanded, code (guest code)
  — live in guest memory. `OS_ReadVarVal` enumeration, wildcards and the
  `OS_SetVarVal` semantics are exact.
- GSTrans (`OS_GSInit`, `OS_GSRead`, `OS_GSTrans`), `OS_EvaluateExpression`,
  `OS_SubstituteArgs` and `OS_ReadArgs` are `lib/obey`'s (§13), run against the
  guest's variables.

### 4.5 `OS_Byte`, `OS_Word` and the rest

- **Reason codes.** Every documented reason of `OS_Byte` and `OS_Word` is
  provided. A reason that only concerns absent hardware answers as RISC OS 5
  does on a machine without that hardware.
- **Other SWIs.** The rest of the kernel's set is complete:
  - **Environment:** `OS_ChangeEnvironment` and every handler, `OS_Exit` and
    `OS_ExitAndDie` with `Sys$ReturnCode`, `OS_GetEnv` and the command tail.
    Escape is handled as `OS_Byte` 229 and 230 configure it.
  - **Information:** `OS_ReadSysInfo`, `OS_Memory`, `OS_PlatformFeatures`.
    The platform is "no hardware platform", and `OS_PlatformFeatures` reports
    exactly what the profile implements.
  - **Code areas:** `OS_SynchroniseCodeAreas` drops translations over its range.
  - **Text and conversion:** the `OS_Convert…` family, `OS_ReadLine`
    (including `OS_ReadLine32`), `OS_HeapSort` (including `OS_HeapSort32`),
    `OS_PrettyPrint`, `OS_Confirm`.
  - **Time and RAM discs:** `OS_ReadMonotonicTime`, `OS_ResyncTime`,
    `OS_ReadRAMFsLimits`.
- **Privileged calls.** Those that would reach hardware (`OS_Hardware`,
  `OS_ClaimDeviceVector`, `OS_ClaimProcessorVector`, `OS_MMUControl`,
  `OS_IICOp`) answer the error RISC OS 5 gives for a missing device or an
  unsupported reason.

### 4.6 Territory, messages, resources (RO18)

- The Territory Manager and its territory follow the TAIRiX locale. They
  provide dates and numbers, the alphabet and its tables, collation, and time
  zones through `lib/tz`.
- MessageTrans is complete, with file blocks in guest memory. ResourceFS holds
  the built-in resources:
  - each module's message file, authored in the tree;
  - the Wimp's file-type sprites, drawn at RISC OS's sizes from the TAIRiX
    file-type masters, so a Save box shows the desktop's own pictures;
  - the system font (`lib/vdu`).

  Error numbers are RISC OS's, and their texts are the documented messages.

## 5. The command line and scripts (RO17)

- **`OS_CLI` resolution** follows RISC OS's order:
  1. the modules' command tables;
  2. `Alias$…` aliases;
  3. a file found on `Run$Path`, run by type through `Alias$@RunType_XXX`.

  An unknown command is therefore the "not found" error the `Run$Path` fallback
  gives.
- **Run actions** for every executable type: Absolute, Utility, Module (by
  `RMRun`), BASIC, Obey, TaskObey, Command (`*Exec`), TaskExec and Desktop.
- **Obey.** The `Obey$Dir` scope, the `%0`–`%9` and `%*n` parameters, and the
  options. Exec and TaskExec are provided too, with TaskObey and TaskExec
  running in TaskWindows (RO48).
- **Commands.** Every `*` command of every built-in module. Also every command
  the RISC OS boot sequence's utilities provide, since applications rely on
  them: `IfThere`, `AddApp`, `X`, `Repeat` and their kin. They are first-party,
  found on `Run$Path` as they are on RISC OS.

## 6. Filing (RO19)

- **FileSwitch.** Every SWI of `OS_File`, `OS_Find`, `OS_GBPB`, `OS_Args`,
  `OS_BGet`/`OS_BPut` and `OS_FSControl`, each with every reason. Open-file
  handles go up to RISC OS's bound per session.
- **HostFS.** The session's files appear as one filing system, `HostFS`, whose
  discs are the session's roots:
  - **the shared session's own disc:** the application's documents folder,
    `Home/Documents/RISC OS/` (`plans/BEEB.md` §6). The desktop session grants
    it writable at each session start, without a picker;
  - every subtree handed over at launch: the application, the resource
    applications, a folder from the picker;
  - a `Transfer` disc holding each file delivered by a drop or an open, under a
    directory of its own;
  - `Scrap` and `RAM`, RAM discs in the worker bounded by the memory policy.

  ResourceFS is `Resources:`. A disc's name is its root's folder name, made
  unique within the session.
- **The user's own RISC OS disc.** The documents folder is where the user keeps
  RISC OS applications and files, and it holds `!Boot` as RISC OS 5 lays it
  out:
  - `Choices`, which is `Choices:`;
  - `Resources`: the user's own `!System`, `!Fonts` and their kin;
  - the boot sequence's `PreDesk` and `Tasks`, run when the shared session
    starts (decision 16).

  The emulator makes `!Boot`'s skeleton in a new folder and never writes over
  the user's files.
  - **Getting the folder.** The grant is asked for on one of the
    application's windows. A shared session therefore shows the session
    window while it starts, and asks on it before the boot sequence or any
    program runs.
  - **A refusal.** A refusal leaves the session with `Choices:` on a RAM disc,
    and the session window says why.
- **Names.**
  - **Spelling.** RISC OS and TAIRiX spellings convert through
    `lib/fsmeta::preset::acorn`'s one conversion: the `.`/`/` exchange, RISC
    OS Latin-1 ⇄ UTF-8, and the reserved bytes. A name that cannot round-trip
    is RISC OS's "bad name" error, never a mangled name.
  - **Case.** Lookups ignore case and preserve it, as FileCore's do. An exact
    match wins. Two names differing only in case, with no exact match, are
    refused as ambiguous, never guessed.
- **Metadata.** A file's filetype, load and exec addresses, attributes and
  datestamp come from one of three places, in order:
  1. its volume's `acorn.*` attributes, where the volume keeps them;
  2. the `,xxx` suffix, read and written;
  3. otherwise, the canonical filetype of its TAIRiX content type (RO3), and
     else Data.

  Stamps convert through `Time64` and are range-checked: a stamp a format
  cannot hold is an error, never truncated.
- **Confinement.** Everything resolves beneath its root through the front end's
  confined resolver. A link that leaves the root does not resolve, and a write
  is refused where its grant does not allow writing. TAIRiX errors map to RISC
  OS's: not found, locked, access violation, disc full, write-protected.
- **Transport.**
  - **Requests.** Bytes cross as bounded relay requests on handles the front
    end minted over the grants. The front end serves them on its file worker,
    never its window loop.
  - **The read cache.** The worker keeps a `lib/reclaim` read cache under a
    seed the front end draws.
  - **Writes and ensure.** A write is answered once the front end has written
    it, so a failed write is the guest's error. `OS_Args` ensure is a sync.
  - **Waiting.** A context waiting on a relay is parked, and the worker runs
    another.
- **Choices.** `Choices:` is `!Boot.Choices` on the user's own RISC OS disc, as
  on RISC OS 5, so a guest's choices are files the user sees and backs up with
  the rest. An isolated session's `Choices:` is a RAM disc that ends with it.

## 7. BBC BASIC V and VI (RO20)

- **Programs.**
  - Tokenising and listing are `lib/bbcbasic`'s (RO7). The program text is
    tokenised lines in BASIC V's layout: `&0D`, the line number, the length
    and the line's bytes, ending `&0D &FF`. That covers the line-reference
    encoding and the three extension tables.
  - `LOAD`, `SAVE`, `CHAIN`, `TEXTLOAD`, `TEXTSAVE`, `CRUNCH`, `LIST` and
    `LISTO` work byte-exactly on real programs. A text program is tokenised on
    load.
- **Workspace in guest memory.** PAGE, TOP, LOMEM, HIMEM and END; the heap and
  the BASIC stack; the resident integers and `@%`; the variable catalogue and
  every value format, laid out as BASIC V lays them out. Software reaches these
  through `CALL`'s parameter list, indirection, `DIM` blocks and string
  information blocks. The interpreter's name index is a derived cache.
- **The language.** Every statement, function, operator and command of RISC OS
  5's BASIC V. That includes:
  - whole-array operations, `SUM` and `SUMLEN`;
  - `CASE`, `WHILE`, `REPEAT`, multi-line `IF`;
  - `PROC` and `FN` with `LOCAL`, `RETURN` parameters and array parameters;
  - `LOCAL ERROR` and `RESTORE ERROR`, `LOCAL DATA` and `RESTORE DATA`;
  - `LIBRARY`, `INSTALL` and `OVERLAY`; `QUIT`;
  - the graphics statements (§8); `SOUND`, `BEATS`, `TEMPO`, `VOICES` and
    `STEREO` (§11.1); `MOUSE`;
  - files, `GET$#` and `INPUT#`, `TIME` and `TIME$`; `TRACE`.

  BASIC VI is the same language over eight-byte reals.
- **Numbers.** Integers are 32-bit. BASIC V's reals are five bytes, with its own
  arithmetic and functions to BASIC V's bits as recorded. BASIC VI's are
  eight-byte doubles, stored as BASIC VI stores them and computed through
  `lib/softfloat`. Conversions, `PRINT`, `@%`, `STR$` and `VAL` follow each
  dialect's observed results.
- **The machine interface.**
  - `CALL` and `USR` enter code with the documented registers: A%–H% in R0–R7,
    and BASIC's workspace, parameter list and count in R8–R10. The routine
    table BASIC offers called code is on host pages.
  - `SYS`: names resolve through `OS_SWINumberFromString`, with string and
    number arguments, and `TO` with the flags.
  - `OSCLI` and `*` lines.
- **The assembler.** `[` … `]` with `OPT`, labels, `ADR`, the `EQU` and `DC`
  directives and `ALIGN`, accepting the mnemonics RISC OS 5's BASIC assembler
  accepts. It encodes through `arm`'s tables (§2.2).
- **Errors and the editor.**
  - Error numbers and messages are BASIC V's; `REPORT$`, `ERR` and `ERL`;
    Escape.
  - The prompt runs in a screen window (decision 10), with `AUTO`, `RENUMBER`,
    `DELETE`, `EDIT` (BASIC's full-screen editor), `HELP`, `LVAR`, `NEW` and
    `OLD`.
- **Divergences.** Each deliberate divergence is recorded with its test, as in
  `plans/BEEB.md` §3. Examples are the copyright text and the routine
  addresses, which are not reproduced; the documented interfaces are.

## 8. Graphics (RO21–RO25)

- **Modes.**
  - Numbered modes come from `lib/vdu`'s one table, with their geometry.
    Mode strings and selectors are `OS_ScreenMode`'s.
  - Depths: 1, 2, 4 and 8 bits per pixel palettised, and every direct-colour
    format RISC OS 5 defines.
  - Screen banks: `OS_Byte` 112 and 113.
  - Screen memory for direct writers is in guest memory (§3). There is no
    VIDC (decision 1).
- **The VDU stream.**
  - Every code 0–31 and 127 with its parameter count from `lib/vdu`, VDU 23
    and its sub-codes, text and graphics windows, origins, the text and
    graphics cursors, paged mode, and output through WrchV.
  - Colours: `COLOUR`, `GCOL`, `TINT`, `OS_SetColour`, ECF patterns.
  - Characters: the system font, with characters redefinable.
- **`OS_Plot`.** Every plot code, each pixel rule pinned by recorded
  observation:
  - moves and dotted lines with their end rules; points;
  - triangles, rectangles and parallelograms;
  - circles, arcs, segments, sectors and ellipses;
  - the two flood fills; block copy and move;
  - every GCOL action.
- **Sprites** (RO22):
  - `OS_SpriteOp`, every reason, on system and user areas by name or pointer.
    Areas, headers, mode words, palettes and masks are read through `lib/image`'s
    sprite structures.
  - Plotting is the emulator's own path into the destination's format: masked,
    scaled and transformed, with translation tables and alpha.
  - Output redirection to a sprite or its mask; screen save and load.
  - SpriteExtend's JPEG SWIs, through `lib/image`'s JPEG decoder.
- **ColourTrans** (RO23): every SWI. Colours are matched as ColourTrans
  documents, pinned by observation. Calibration and the font colour calls are
  included.
- **Draw** (RO24):
  - `Draw_Fill`, `Draw_Stroke`, `Draw_StrokePath`, `Draw_FlattenPath`,
    `Draw_TransformPath`, and `Draw_ProcessPath` with every output option.
  - Fill styles with RISC OS's boundary and interior pixel bits; dashes, joins,
    caps and flatness; 16.16 matrices.
  - Paths flatten through `lib/bezier` and are scan-converted to RISC OS's
    pixel rules.
  - `DrawFile_Render`, `BBox` and `DeclareFonts` go through `lib/drawing`'s
    reader.
- **The Font Manager** (RO25): every `Font_` SWI.
  - **Names and faces.** Fonts are named by RISC OS identifier
    (`Family.Weight.Style`). The standard families map to TAIRiX faces through
    `lib/drawing`'s one map (RO4). Those faces' outlines and metrics come
    through the font service's glyph protocol, relayed by the front end.
  - **Guest fonts.** Acorn outline fonts and metrics a guest supplies on
    `Font$Path` are decoded and rasterised in the worker.
  - **Text.** Encodings (Base0, the Latin sets, UTF-8). `Font_Paint`'s control
    sequences, kerning and matrices.
  - **Colour and output.** Anti-aliasing to the colour ranges `Font_SetFontColours`
    and ColourTrans give. `Font_SwitchOutputToBuffer` writes to Draw objects.
  - **The cache.** Bounded by the memory policy.
  - **The desktop font.** It is the TAIRiX interface face.

## 9. The Wimp and the Toolbox (RO39–RO45)

### 9.1 Tasks, memory and polling (RO39)

- `Wimp_Initialise` takes every known version, with its message list.
  - Each task has its own slot (§3). `Wimp_SlotSize` draws on the free pool
    the memory policy gives.
  - `Wimp_StartTask` runs the new task until its first poll.
- `Wimp_Poll` and `Wimp_PollIdle` provide every reason with its mask, the poll
  word, and priority as the Wimp orders them.
- The Task Manager's SWIs are provided. **The session window** is a native
  TAIRiX window that does the Task Manager's job: each task's slot and the
  shared areas' use, quitting a task, the command line (RO48) and settings.

### 9.2 Messages

- **Delivery.** Sent, recorded and acknowledged messages, broadcasts and
  bounces, `Wimp_RegisterFilter`, and message lists.
- **Inside a session.** Every standard protocol runs as on RISC OS: data and
  RAM transfer, the clipboard, drag claims, interactive help, iconising,
  changes of mode, palette and font, TaskWindows, quitting.
- **To the desktop.** Messages meant for RISC OS's Filer, Pinboard and Switcher
  are bridged, since the desktop is TAIRiX's: `Filer_OpenDir` opens the file
  manager on the granted folder.

### 9.3 Windows (RO40)

- **Creating and opening.**
  - `Wimp_CreateWindow` takes every flag, the new-format bits included.
  - Opening, closing and deleting; extents and scroll offsets.
  - `Open_Window_Request` stays the guest's to answer. The user's moves and
    resizes reach it as the request RISC OS would send, and the window moves
    when the task opens it there.
- **Redraw and update.** `Wimp_RedrawWindow`, `Wimp_GetRectangle`,
  `Wimp_UpdateWindow`, `Wimp_ForceRedraw` and `Wimp_BlockCopy` work over each
  window's work area, held in its TAIRiX surface. Only the rectangles drawn
  cross to the front end.
- **The screen the guest sees.** It is virtual: the work area in OS units
  (decision 13), with each window at the place the guest gave it. Pointer
  positions are window-local coordinates plus that place. Move deltas the
  desktop reports (RO33) keep the guest's view of its windows true, and real
  screen positions are never revealed.
- **Nested windows** are composited by the emulator inside their parent's
  surface.
- **Panes** — windows with the pane flag, or opened just in front of another —
  are attached windows (RO33) that follow their parent.
- **Stacking.** Requests are honoured among the session's own windows through
  RO33's restacking. Raising above another application's window is the
  desktop's decision.
- **Window state.** `Wimp_GetWindowState`, `GetWindowInfo` and
  `GetWindowOutline` are exact, with furniture sizes reported in OS units for
  the look in use.
- **Furniture follows the window's flags in both looks.** A window without a
  close icon has no close control.
  - **TAIRiX look:** the window manager's frame with root-viewport scroll bars
    (RO33). The work-area colour is kept, the title-bar colours are the
    theme's, and a sprite in a title is not drawn.
  - **RISC OS look:** a borderless window whose furniture the emulator draws
    (RO44).
- **Error boxes.** `Wimp_ReportError` holds the calling task until it is
  answered, as it holds the RISC OS desktop. TAIRiX stays live meanwhile.
  - In the TAIRiX look it is a desktop alert titled "Error from …", with
    RISC OS's buttons and the extended custom ones.
  - In the RISC OS look it is a RISC OS error box.

### 9.4 Icons, the caret and templates

- **Icons.**
  - Every icon kind: text, sprite, or both; indirected.
  - Every validation-string command: allowed characters, display character,
    colours, keys, line spacing, pointer shape, border type, sprite names.
  - Every button type and ESG; selected and shaded states.
  - The Wimp's editing of writable icons, with the caret, cursor keys and
    `K`-key navigation.
  - `Wimp_PlotIcon`; and the sprite-pool icons (`file_xxx` and their kin).
- **The caret.** `Wimp_SetCaretPosition` and `Wimp_GetCaretPosition`. The
  input focus follows the TAIRiX window with the keyboard.
- **Templates.** `Wimp_OpenTemplate`, `Wimp_LoadTemplate` and
  `Wimp_CloseTemplate`: the template format, wildcard names, the relocation of
  indirected data, and font references. Template files are decoded in the
  worker and fuzzed.

### 9.5 Menus (RO41, RO44)

- `Wimp_CreateMenu`, `Wimp_CreateSubMenu`, `Wimp_DecodeMenu` and
  `Wimp_GetMenuState` work over menu blocks the guest owns, re-read whenever
  they are shown.
- **TAIRiX look:** the desktop's own menu chain, using RO35's extensions:
  - `Message_MenuWarning` asks for a child on demand;
  - a window submenu (a Save box, an Info box) is a window child;
  - long menus scroll;
  - a guest row states no function;
  - a writable item is a quick-entry field;
  - Adjust is the repeatable choice (`plans/NEW-MENUS.md` M11).
- **RISC OS look:** the emulator draws RISC OS menus in popups under RO35's
  grab, with RISC OS's keyboard navigation.
- `Message_MenusDeleted` follows the chain's end in both looks.

### 9.6 Drags (RO42)

- **Drag types.** `Wimp_DragBox` with every type, including the user-drawn
  boxes that call guest routines. `DragASprite` and `DragAnObject`, and
  `User_Drag_Box`.
- **Inside a session.** A drag between a session's own windows stays inside the
  session.
- **Leaving the session.** A drag that leaves the session's windows becomes a
  desktop drag, carrying the type's class picture. A save drag (the guest's
  `Message_DataSave` to a window under the pointer) proceeds as follows:
  1. it becomes an application-started save drag (RO37);
  2. the target's `SaveInto` location grant becomes a disc of the session;
  3. the guest is answered `DataSaveAck` with a path inside it;
  4. the guest writes the file and sends `DataLoad`.
- **Arriving from the desktop.** A drop of files (`FileOffered` and
  `PlaceDropped`, or several items) becomes `Message_DataLoad` to the task
  owning the window, with each file on the `Transfer` disc. A drop on a hosted
  slot goes to the task's icon-bar icon.

### 9.7 The clipboard

- `ClaimEntity`, `DataRequest` and the transfer that follows move data between
  a session's tasks as on RISC OS.
- With the desktop: text crosses as text. Pictures cross as PNG, converted from
  a sprite through `lib/image`'s encoder, or rendered from a Draw file.
- Other types stay within the session.

### 9.8 Interactive help, the icon bar and quitting (RO43)

- **Interactive help.** When the pointer settles over a new icon, the emulator
  sends `Message_HelpRequest` for it. It declares the `HelpReply` as the
  window's tooltip over that icon, multi-line (RO38), so the seat's own dwell
  shows it.
- **The icon bar.** Each icon-bar icon of a task is a hosted slot (RO36),
  carrying the icon's sprite and text.
  - Select and Adjust are `Mouse_Click` on window −2.
  - Menu asks the task, which opens its menu at the bar. That menu is the
    slot's menu.
  - Drops are `DataLoad` to the icon.
- **Quitting.** A desktop session's end sends `PreQuit` to each task. An
  objection holds the logout, as an unsaved document in a native application
  does. `Quit` follows.

### 9.9 The Toolbox (RO45)

- **The core.** The Toolbox: objects, events, Res files and messages.
- **Every object module:** Window with every gadget, Menu, Iconbar, ColourDbox,
  ColourMenu, DCS, FileInfo, FontDbox, FontMenu, PrintDbox, ProgInfo, Quit,
  SaveAs, Scale, and the text gadgets.
- **Res files.** Decoded in the worker and fuzzed.

## 10. The C library and support modules (RO26)

- **SharedCLibrary.** Every function of the ANSI library and the RISC OS
  kernel library (`_kernel_…`), through every initialisation the stubs use:
  APCS-R, APCS-32 and the module form.
  - The library's guest-visible data is laid out as its headers define, so
    macro expansions compiled into programs read the right fields: `FILE` and
    its buffers, the `ctype` tables, `errno`, the locale tables.
  - The heap lives in the program's memory.
- **Support modules.** The FPEmulator SWIs, VFPSupport's contexts, CallASWI and
  DDEUtils (long command lines, throwback).
- **Other modules** from the inventory (§4.3), each complete: Squash, over
  `lib/compress`'s LZW and the `SQSH` container; MimeMap, over the shared
  filetype table; Hourglass, as the desktop's busy cursor; Free; BufferManager;
  DeviceFS with SystemDevices' devices; PipeFS.

## 11. Sound, network, printing

### 11.1 Sound (RO29)

- **The modules.**
  - SoundDMA: its linear handler at the configured rate, with the 8-bit
    logarithmic channels mixed through the log-to-linear conversion.
  - SoundChannels: the voices, `Sound_ControlPacked`, tuning and the log tables.
  - SoundScheduler: queueing, tempo and beats.
  - SharedSound: its handlers, with rate conversion through
    `tairix_audio::resample`.
- **The voices.** WaveSynth, StringLib and Percussion, authored to their
  recorded output.
- **Output.** Mixed in the worker, carried in session frames to the front end's
  `audio-v1` stream. While sound plays, the stream's clock paces the guest's
  sound handlers (`plans/BEEB.md` decision 8).

### 11.2 Network (RO46.1, RO46.2)

- **The socket SWIs.** The Internet module's SWIs, with its events through
  EventV, are relayed to sockets the front end holds under `CAP_NET`. Buffered
  bytes per socket and sockets per session are fixed containment bounds.
- **The Resolver.** It works over `lib/resolver`.
- **What a guest cannot have.** There are no raw sockets: `CAP_NET_RAW` is not
  requested, and a raw socket is refused as RISC OS refuses one without
  privilege. `Inet$…` variables report the host's configuration, read-only.
- **Which sessions get the network.** The shared session uses the network if a
  setting allows it, which it does by default. An isolated session does not,
  unless the user allows it for that launch.
- **URLs.** A URL message no guest task claims goes to the desktop's URL
  hand-off (RO46.2, which waits for `plans/BROWSER.md` BR4).

### 11.3 Printing (RO47)

- PDriver's SWIs record each page's drawing into a `lib/drawing` document,
  saved as PDF through the Save panel. The drawing covers paths, text with its
  mapped faces, sprites and JPEGs.
- The emulator offers one printer, *PDF*. There is no TAIRiX print service, so
  nothing reaches paper (§18).

## 12. Machine mode: the Acorn chips (RO31, RO50, RO51)

A program that drives Acorn's chips directly runs in **machine mode**: a
single-tasking context given its own models of the chips. Inside the worker the
chips are guest state like any other, so nothing a program does to them reaches
the host. The boundary is about fidelity, not safety.

- **The boundary is one table.**
  - The table lives in the `machine` crate. Every register of the I/O space is
    either modelled, naming the tier that models it, or refused. A register one
    of whose bits a later tier models is listed bit by bit.
  - A test walks the whole space against the table.
  - **Exact inside.** A modelled register behaves as the data sheets define,
    under its tier's stated time model.
  - **Loud outside.** Reading or writing a refused register stops the program.
    The front end names the chip and the register, and the tier that models it
    where one does.
  - Nothing is a no-op, and nothing is approximated into a plausible wrong
    result.
  - **Cost.** The chips answer through device pages in the guest MMU (§3). A
    memory access that is not to a chip pays nothing for machine mode.
- **Machine mode is chosen before the program starts,** because it changes the
  layout the program is loaded into.
  - A program outside machine mode that touches the chips stops with the
    reason. The front end offers to run it again in machine mode.
  - The choice is kept with the application's registry entry (RO32), and can
    be made in advance.
  - Machine mode is for single-tasking programs. A Wimp task that touches the
    chips stops with the reason, because its windows are TAIRiX windows with no
    VIDC behind them.
- **The machine is the one such programs were written for.** That is an
  Archimedes running RISC OS 3.1, or, from tier 3, a RiscPC running RISC OS 3.5
  to 4. Each brings:
  - its processor: the 26-bit profile, tier 2's ARM2 and ARM3 profiles, and
    tier 3's ARM610, ARM710 and SA-110 timings;
  - its RISC OS's memory layout where software hard-codes it. On the
    Archimedes that is screen memory at the top of logical RAM, physical RAM at
    `&2000000` reachable only in supervisor mode, and the chips' I/O space
    above it;
  - its RISC OS's version answers (`OS_Byte` 0 and 129), which software
    checks to tell the machines apart;
  - its video chip's colour rules, such as VIDC1's sixteen palette registers
    and the fixed mixing its 256-colour modes use.

  The kernel keeps serving SWIs. The context's screen, palette, sound and
  keyboard pass through the chip models as far as the built tiers reach; one
  palette state serves both VDU 19 and VIDC.
- **Tier 1 (RO31): exact without cycle timing.**
  - **Modelled:** VIDC1's palette, border and cursor colour registers, and
    MEMC's video and cursor address generators (`Vinit`, `Vstart`, `Vend`,
    `Cinit`).
  - **Refused:** IOC, the rest of VIDC1 and MEMC, and writes to the processor
    vectors.
  - **Time** is the context's frame clock, at the rate the mode's timings give.
    A colour change takes effect from the next frame. `Vinit` takes effect at
    the next vertical sync, because MEMC itself loads it there.
  - **Raster effects are refused.** The same colour register changed twice in
    one frame is a raster effect, and tier 1 stops the program.
  - **The one documented difference:** a single change inside a frame shows
    from the next frame, where hardware would split the frame it lands in.
  - **The screen window** shows the picture inside its border, as a monitor
    shows the mode, and draws the pointer in VIDC's cursor colours.
- **Tier 2 (RO50): the timed machine.**
  - **Cycle time.** Each instruction takes its ARM2 or ARM3 data-sheet cycles
    at the modelled machine's clock. MEMC's N- and S-cycle timing and the
    cycles video, cursor and sound DMA take are included. The context is paced
    to the host at that speed, and the translator counts the same cycles (§16).
  - **IOC, whole:** the interrupt and fast-interrupt controllers, the four
    timers, the keyboard's serial link with the Archimedes keyboard's own
    protocol, and the control register's lines except I²C.
    - Interrupts reach the processor vectors the program installs, with the
      kernel's handler as the default claimant.
    - The kernel's timed services then run from IOC's timer and vertical sync
      in this context.
  - **VIDC1, whole:** its timing registers (the picture takes the geometry they
    program, overscan included), the hardware cursor's position, the stereo
    image, the sound frequency and the control register.
  - **MEMC, whole:** its control register; its sound DMA into VIDC1's eight
    logarithmic channels, through the sound system's log-to-linear conversion
    (RO29); and its page table with its protection levels.
  - **Raster effects** are then exact to the cycle model, and tier 1's refusal
    is lifted.
  - **Choosing a machine.** The modelled machine is part of the application's
    choice: an A3000's 8 MHz ARM2, an A5000's ARM3, and their kin. A 26-bit
    program outside machine mode may run at a modelled machine's speed too, for
    old software that times itself by the processor.
  - **A documented difference:** ARM3's cache replacement is pseudo-random on
    the silicon, so the model states its own.
- **Tier 3 (RO51): the rest of the machines.**
  - **Floppy controllers,** with raw disc images that record what a sector dump
    cannot, so copy-protected originals load. These are the WD1772 of the A3000
    and the A400 series, and the 765-family controller in the A5000 class's
    82C711. The latter is PCEmulator's µPD765A model, hoisted into `lib/*`
    rather than written twice (§13).
  - **I²C,** with the PCF8583's CMOS RAM and clock.
  - **The RiscPC,** for RISC OS 3.5 to 4 software: IOMD's interrupts, timers,
    keyboard and mouse ports and DMA; VIDC20; and the ARM610, ARM710 and SA-110
    timings.
  - **Still refused:** podules and expansion cards, Econet, and the hard-disc
    controllers (§18).
- **Information.** The tiers are built from the chips' documentation: VLSI's
  ARM family data manual for the ARM2, MEMC, VIDC and IOC; Acorn's technical
  reference manuals; and the RiscPC's chip data sheets. What documents leave
  open — cycle-level and undocumented behaviour — comes from RO14's harness run
  on a real Archimedes and RiscPC. RO57 waits for that hardware.

## 13. Shared libraries this plan hoists or adds

Each item moves every existing copy onto the one definition in the same change,
with tests that fail before and pass after (`AGENTS.md` §2.2, §2.18). A new
`lib/*` crate enters `AGENTS.md` §3 and `PLAN.md` in the change that adds it
(`AGENTS.md` §6).

- **RO3 — `lib/fsmeta::preset::acorn`:**
  - **Leaf and path spelling, in both directions.** It replaces four private
    compositions: the archive driver's `name.rs`, its two Spark and ArcFS
    `spelling()` copies, ADFS's `Crossed`, and `lib/icon`'s
    `within_application`. The two `spelling()` copies also skip the reserved-
    byte check.
  - **The `,xxx` suffix.** One writer replaces two: the window protocol's
    `comma_filetype` and the picker's `Typing::leaf`. A strict reader replaces
    the document host's `stem()`, which cuts at any `,` with no hex check.
  - **Filetype names.** RISC OS's type names (Text, Sprite, DrawFile, BASIC,
    Obey, …) are added. The save panel's "File type &XXX" fallback names a
    known type.
  - **Canonical filetypes.** Each content type `lib/browse` knows names one
    canonical RISC OS filetype, the reverse of `FILETYPE_TABLE`, defined beside
    it.
  - **Spark's directory test.** Spark's `names_directory` decodes the load word
    itself; it moves onto `decode_load_exec`.
- **RO4 — colours and font names:**
  - The `&BBGGRR00` palette word is defined once in `lib/image`, replacing
    three private copies: `lib/drawing`, the sprite decoder and the sprite
    encoder.
  - `lib/drawing`'s RISC OS font-name map is made the public definition. The
    Draw application's `family_key` copy is deleted, along with its re-derivation
    of the font service's generic families.
- **RO5 — `lib/softfloat`:**
  - Exact binary32, binary64 and double-extended arithmetic with unnormal
    values, as the FPA and the 8087 have them.
  - Every rounding mode and exception flag.
  - The FPA's memory formats (word order, extended, packed decimal) and the
    x87's.
  - PCEmulator's 8087 keeps its own value classes and moves onto the shared
    core with its digest unchanged. `lib/drawing`'s FPA word order moves onto
    it too.
- **RO6 — `lib/obey`:** GSTrans with every escape, the system-variable types
  and their expansion, `OS_EvaluateExpression`'s grammar, Obey parameter
  substitution and `OS_ReadArgs` templates. All of it is pure, over a
  variable-store trait.
  - `lib/icon`'s `!Boot` reader is rebuilt on its read-only mode, which runs no
    command and only reads `Set`, `SetMacro`, `Unset`, `If` and `IconSprites`.
  - RO32's registry extends that read-only reading to the type claims.
- **RO7 — `lib/bbcbasic`:** the token tables of BASIC II and of BASIC V and VI,
  sharing what the dialects define alike, plus tokenising, listing with `LISTO`
  and the line-reference encoding.
  - OpenMicro's ROM builder emits its keyword table from it.
  - Its other consumers are the emulator (RO20) and TextEdit (RO49).
- **RO8 — `lib/vdu`:**
  - The VDU control codes and their parameter counts, which BBC and RISC OS
    define alike, and VDU 23's sub-codes.
  - RISC OS's numbered modes with their depth, eigen factors and geometry.
    `lib/image`'s private mode tables move onto it.
  - The 8×8 system character set: OpenMicro's glyph designs, placed at 96 for
    the BBC's £ and at 163 for RISC OS's, with RISC OS's Latin-1 half added.
    OpenMicro's MOS, for the Model B and the Electron alike, emits its font
    from it.
- **RO51 — the µPD765A floppy controller.** PCEmulator's model moves into a
  `lib/*` crate, and PCEmulator moves onto it, before machine mode's A5000-class
  controller, a member of the same family, is built on it.
- **RO52 — the guest-translation runtime.** These parts of PCEmulator's
  translator are not x86-specific: the code cache's watched-granule
  invalidation and heat, the compiler role and the held compiler, and the
  native executor's entry, with its miri-enrolled `unsafe`. They move into a
  `lib/*` crate, and PCEmulator moves onto it.
- **RO54 — `lib/codegen` floating point.** IEEE binary32 and binary64 values
  and operations, with the rounding and NaN behaviour defined by the IR and
  held to `lib/softfloat`. This is shared with `plans/UNIVERSAL.md` U2's Wasm
  compiler.

`lib/binfmt` is unchanged: the emulator loads no ELF itself (§4.3).

## 14. Desktop facilities this plan adds

Each item amends the plan that owns the facility in the same change, and keeps
that plan's guarantees: real screen positions are never revealed; window
management stays the window manager's; identity stays manifest-attested.

- **RO2 — one association path.** This fixes defects found in review:
  - **Divergent candidate sets.** The file manager offers handlers from the
    system stores only (`MACHINE_ROOTS`), while the session adds the user's
    own. Both will read the same stores.
  - **A second matcher.** The picker's `folder_selection` matches exact
    strings with no parent chain; it is replaced by `applications_for`.
  - **Ordering.** The precedence rule — system stores before user stores, then
    a stated tie rule — is defined once. Today the session's breadth-first walk
    can rank a nested system bundle after a user one.
  - **Logging.** Open, launch and refusal decisions carry stable event ids.
  - **A test.** An explicit test proves listing and browsing never launch.
- **RO32 — RISC OS applications on the desktop.**
  - **Activation.** A `!Name` directory holding `!Run` is a runnable unit.
    Double-click launches it through the handler that claims RISC OS
    applications. Shift+double-click, or the context menu, opens it as a
    folder, as RISC OS's Filer does. The directory is handed over as a subtree
    — read-write where the user may write it — which a launch hand-off cannot
    carry today.
  - **The RISC OS application registry.** Per user, kept by the desktop. For
    each RISC OS application it holds:
    - its location and provenance;
    - the filetypes it claims;
    - the resources it provides (`System$Path`, `Font$Path` additions);
    - the user's choices for it: its processor profile, machine mode, and the
      machine modelled (§2.1, §12).

    It is built by reading `!Boot` as data through `lib/obey`'s read-only mode,
    never running it. How an application is registered depends on where it
    lives:
    - on the user's own storage, it is registered when a file surface first
      shows it, as RISC OS's Filer "sees" it;
    - on removable media, an image, an archive or a share, only when the user
      launches it.
  - **Using the registry.** `applications_for` consults it below every signed
    claim, so Open With lists RISC OS applications with their sprites.
    Double-clicking a RISC OS document hands the emulator two things: the
    claiming application's directory and the document.
  - **Launches.** A launch selects the shared or an isolated session per
    decision 9, and hands over the registered resource applications kept on
    the user's own storage outside the documents folder.
    - An isolated session gets each resource application read-only, including
      those in the documents folder's `!Boot.Resources`. It never gets the
      folder itself.
  - **The emulator's claims.** Its manifest claims RISC OS applications and the
    executable and script types (Absolute, Utility, Module, BASIC, Obey,
    TaskObey, TaskExec, Command, Desktop). It also claims a generic RISC OS
    document type, through which every filetype no native application claims
    resolves to it, ahead of TextEdit's `application/octet-stream` fallback.
- **RO33 — the window model.**
  - **Borderless windows.** Top-level windows with no frame.
  - **Attached windows.** A window placed relative to its parent and kept there
    as the parent moves, re-placeable by its owner, at any depth, stacked and
    hidden with its family.
  - **Client-initiated move and resize.** Allowed only during a press the
    window received; the window manager performs them.
  - **Moves.** Relative moves of one's own top-level windows, clamped to the
    work area; the user's moves are reported as deltas.
  - **Stacking.** Restacking among one's own windows, never above another
    application's.
  - **Scroll bars.** Root-viewport scroll bars declared over the channel:
    extent, visible area, scroll offsets and scroll requests, the window
    manager drawing them (`plans/GUI-CONTROLS-DESIGN.md` §11.17).
  - **Furniture.** Per-window furniture selection: which controls are shown,
    or no title bar at all.
  - **The work area's size.**
  - **Who may use what.** Borderless and attached windows are admitted to a
    bundle whose signed manifest declares that it hosts a foreign desktop. So
    is the RISC OS look's drawing of its own furniture, the one exception to
    "no application draws its own chrome" (`plans/COMPOSITOR-WORK.md` §0).
    Moves, resizes, stacking and minimising stay requests the window manager
    performs.
- **RO34 — pointer.**
  - The middle button reaches window content as a third client button.
  - An application may set its own cursor image: bounded, validated, its
    hotspot inside the image, composited by the seat, and shown only over its
    window's content.
- **RO35 — menus for a hosted environment.**
  - **Children on demand.** The chain asks the owner for a child when the
    pointer reaches its row, with a settle deadline; an unanswered or refused
    request leaves the row childless.
  - **A window as a child.** A child may be the owner's own window, placed
    where a submenu hangs and living within the chain. It concludes the chain
    when its owner says so. This brings back what `plans/NEW-MENUS.md` D19
    deleted for want of a client; RISC OS's Save and Info boxes are that
    client.
  - **Scrolling plates.** A plate may scroll past the rows it shows, with every
    request still bounded.
  - **Unstated rows.** A hosted environment's rows may state no function — the
    one exception to M10, since the host cannot know a guest row's function.
  - **The popup grab.** For the RISC OS look's own menus: pointer and keyboard
    go to the owner while its popups are up, and a press elsewhere dismisses
    them.
- **RO36 — hosted icon-bar slots.** A hosting bundle may present a slot per
  guest icon. Each carries the guest's label, bounded and validated picture
  pixels, its own windows, and clicks and menus answered on demand.
  - Every hosted slot is attested to the host. Its Info panel is the host's
    signed manifest, naming the guest application as hosted, so a slot cannot
    pass for a native application.
- **RO37 — hand-offs started by an application.**
  - **Save drags.** An application may start a save drag of a document it
    holds (name, type, picture). A drop target answers with `SaveInto` and its
    location grant, as for the Save panel's drag.
  - **Opening a held document.** An application may ask the desktop to open a
    document it holds a descriptor for with that document's default handler.
    The recipient gets the same descriptor; no path is opened on the caller's
    behalf. The emulator uses this for a guest's `Filer_Run` of a type no guest
    claims, and for `!Help` files.
- **RO38 — tooltips for hosted help.** A tooltip may carry multi-line text up
  to the RISC OS `HelpReply`'s bound. The bound is set by the format it
  carries.

## 15. The session, the worker and the front end (RO27, RO28)

### 15.1 The session worker

- **One worker per session.** It is `Run` re-entered in its session role over
  a `lib/sandbox` duplex session, single-threaded, with one spawn share.
- **Scheduling.** It runs contexts in slices of emulated time and waits on its
  pipe with the next deadline in between (decision 12).
- **Crashes.** A worker that crashes, overruns its memory or sends a frame
  that fails validation ends its session. The front end tells the user why and
  which tasks were running. Nothing is silently restarted, because a fresh
  worker would not be the world the user had.

### 15.2 The protocol

The protocol is one total codec both ways, every message decoded whole against
fixed bounds:

- **The run loop:** run and stop; input events, credited so a context that
  reads no input never closes the window behind them; timers.
- **Windows and screens:** create, open, close, the furniture and the look;
  damage rectangles naming bytes in the share. The share is a damage ring,
  whose space the front end releases as it copies each rectangle out.
- **Interaction:** menus and their answers, slots, drags, the clipboard,
  alerts and their answers, tooltips.
- **The relays:** file requests on minted handles; socket requests; glyph
  requests; sound frames.
- **The session:** launches with the grants they bring, quitting a task,
  settings, diagnostics.

The front end believes a reply only when it answers what was asked, within what
that request could produce.

### 15.3 The front end

- **Responsiveness.** The window loop never waits on a worker, a file, a socket
  or the font service. File, socket and glyph relays run on workers of their
  own, and a burst of guest damage is copied and presented once per pass of
  the loop (`AGENTS.md` §28).
- **Launches** arrive from RO32's hand-offs, from the picker, from drops, and
  from the terminal (RO48).
- **Each guest window** is a TAIRiX window in the chosen look. Each
  single-tasking screen is a window scaled through `lib/crt`, with Full Screen
  (`SetSizeState(Fullscreen)`), sharp or smooth scaling, pause, and Reset of
  that context.
- **Settings.** Kept in `lib/appdata` and edited in a `lib/controls`
  `FormDialog`, as both emulators' settings are: the look, the button order,
  the memory ceiling, the network for the shared session, always-isolate, and
  the territory if it should differ from the locale.
- **The bundle.**
  - **Manifest.** `AppInfo` requests `CAP_SANDBOX_SPAWN`, `CAP_SHM`,
    `CAP_JIT_MAP_EXEC`, `CAP_NET`, `CAP_LOG_EMIT` and `CAP_CONSOLE_WRITE`.
    It declares the hosted-desktop property (RO33), the claims (RO32), and
    `documents-folder = "RISC OS"`, with `document-access = "read-write"`.
  - **Contents.** The icon, Help in every required locale, and the docs page
    (`docs/src/desktop/riscos.md`).
- **Diagnostics.** Events go into the `RISCOS` range of `lib/log`: a session
  started or ended and why, a worker ended for what it sent, a stop at machine
  mode's boundary with its chip and register, a refused module replacement, and
  the isolation chosen at launch.

## 16. The translator (RO52–RO54)

- **The front end.**
  - A block of A32 or T32 instructions is lowered to one `lib/codegen`
    function, entered with the processor frame, the page map and the helper
    table.
  - The block ends at a control transfer, at a SWI, at a mode or PSR change,
    and before an instruction it does not translate. An instruction whose
    access would abort or touch a watched granule exits the block before it,
    leaving the interpreter to take it.
  - Flags are kept as the operation that set them, computed only where read.
  - **Cycle counting.** In a context on a timed machine (§12), a block counts
    its cycles exactly as the interpreter does. It exits once the next machine
    event falls due, so the chips and interrupts see the same instant under
    either engine.
- **Floating point and SIMD.** Lowered to calls of the interpreter's own
  `lib/softfloat` functions, which are exact, until RO54 lowers binary32 and
  binary64 natively. RO54 lands when measurement shows the calls dominate.
- **The code cache.** In the worker, keyed by physical frame and offset,
  profile and Thumb state. The same module code in every task therefore shares
  one translation, and slot switches invalidate nothing. Granules are watched
  from the request on; a store into one drops every block over it.
  Translations are held within the memory policy's `TransformCache` share.
- **The compiler sandbox.** `Run`'s third role, holding nothing. The front end
  installs its output into the worker with `code_install` under
  `CAP_JIT_MAP_EXEC` (`plans/PCEMU.md` §4).
  - A compiler that crashes, overruns its deadline or answers what cannot be
    believed is replaced, and the block stays interpreted.
  - Repeated failure, a platform that refuses the install, or a session
    without the capability runs interpreted throughout.
- **Proof.** A differential harness runs random machines and code through the
  IR interpreter and each target's model, held to the ARM interpreter: state,
  memory, aborts and every helper call. `fuzz_translator` is its fuzz target.
  The only `unsafe` is the native entry, enrolled in miri.

## 17. Verification

- **Processor.** Vectors per profile (§2.6), `lib/softfloat`'s own suite, and
  FPA, VFP and NEON vectors.
- **Kernel, modules and BASIC.** Scenario tests in the host suite. Programs
  built with the crate's encoder and `lib/bbcbasic` run on the emulated kernel,
  and their screen, memory and files are compared with expectations from the
  PRMs and the recorded corpus.
- **Machine mode.**
  - Every modelled register is tested against its data sheet's behaviour.
  - The boundary table is walked whole: each address is either modelled with
    its tests, or refused, and the refusal names its chip, register and tier.
  - Scenario programs drive each tier's chips.
  - RO57 holds the tiers to observations of real hardware once a developer has
    some.
- **The reference harness (RO14).** `cargo xtask riscos --reference <dir>`
  works like this:
  1. It builds the harness program, a RISC OS program written with the crate's
     encoder and `lib/bbcbasic`.
  2. The program runs the corpus scenarios on a genuine RISC OS machine a
     developer holds, and records what each leaves: screens, files, registers,
     errors.
  3. The recorded observations are imported into fixtures, with the emulator's
     own names put in.
  4. A scenario marked `ours` keeps the fixture that states the emulator's own
     behaviour.

  The import refuses an observation carrying twelve bytes in a row of genuine
  ROM, module or font content that the emulator's own resources do not hold
  (`plans/BEEB.md` §7).
- **Fuzzing.** Every decoder of untrusted input has a registered harness in the
  change that adds it: AIF and module headers, templates, Res files, message
  files, Acorn fonts, BASIC programs, Squash, the session protocol in both
  directions, and the translator. So does every service a guest calls with
  hostile registers: the kernel's SWIs, FileSwitch, the Wimp, BASIC, the C
  library.
- **QEMU (RO55).** `tests/integration/riscos_qemu_{aarch64,riscv64,x86_64}`
  boot the production image and do the following:
  1. A first-party `!App` test application, a BASIC Wimp task built by the
     fixture, launches from a test volume.
  2. The vertical checks its window's dump, its hosted slot and a menu choice,
     then that it quits cleanly.
  3. A scripted session's digest must equal the constant the host suite
     asserts.
  4. A hot loop must run translated and installed.
- **Performance.** Claims are evidence-backed and machine-independent:
  translated against interpreted cost on one machine, host instructions per
  guest instruction class, and idle CPU at rest. A test never asserts an
  absolute time.
- **Acceptance (RO56).** A recorded corpus of real software, named with its
  hash, outcome and the session's settings.

## 18. Non-goals

- **Podules and expansion cards, Econet, and the hard-disc controllers.** No
  tier models them (§12), so a program that drives one stops with the device
  named.
- **Genuine RISC OS ROMs.** Machine mode hosts the first-party RISC OS. Running
  a ROM would need the whole machine, firmware included, which no tier aims at.
- **A RISC OS desktop of its own.** TAIRiX's file manager, pinboard and icon bar
  are the desktop, and RISC OS messages meant for them are bridged.
- **Snapshots and suspension.** A session holds grants and sockets that cannot
  be restored.
- **Printing on paper.** There is no TAIRiX print service.
- **Patching built-in modules' code, or calling their undocumented internal
  entry points.** There is no RISC OS code to patch, so such software fails
  loudly.
