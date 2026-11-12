# tairix-pcemu

The engine of `PCEmulator.app`: an IBM PC/XT-class machine built around the
8088, 8086, NEC V20 and V30, and Intel 80188 and 80186. The design and its
staging are `plans/PCEMU.md`; the user-facing description is
`docs/src/desktop/pcemulator.md`.

Stability: `experimental`.

- `cpu` — the decoder, the interpreter for every model, the 8086 family's
  microcode arithmetic, the NEC extensions and 8080 emulation, and timing
  calibrated against hardware bus traces; and in `cpu::jit` the translator
  that lowers hot blocks to `lib/codegen`'s IR, the code cache, the compiler
  sandbox's work and the native executor that runs installed code.
- `time` — emulated time and the exact rational clocks every device counts.
- `chips` — the 8259A, 8253, 8237, 8255 and the keyboard, and the 80186's
  integrated peripherals, each a register-level model with no board in it.
- `board` — the memory map, the I/O decode, interrupt routing, DRAM refresh
  and wait states, catching the chips up to the CPU lazily.
- `machine` — the CPU on its board, run in slices against emulated time, and
  the ready-made machine templates.
- `video` — the MDA, Hercules, CGA, EGA and VGA on their CRT controllers'
  timing, the monitors and the CGA's composite decode, and the first-party
  code page 437 character sets in `video/font/`.
- `storage` — block devices, raw and ImageDisk floppy media, the µPD765A
  floppy controller and its drives, and the XT fixed-disk adapter with its
  ST-506 drives, each moving its data over DMA byte by byte in emulated time.
- `sound` — the PC speaker, the YM3812, the Sound Blaster 2.0's DSP and the
  parallel port's DACs, mixed band-limited at the YM3812's rate and carried
  to the stream's by `lib/audio`'s resampler.
- `ports` — the 8250 and 16450 UARTs and the line to the Microsoft serial
  mouse, the parallel port with its printer and DACs, the game port, and the
  MSM6242B clock card.
- `ems` — the LIM expanded memory board: its pages and page registers, which
  `board` shows in the frame by page table.
- `fpu` — the 8087: exact 80-bit arithmetic rounded once, its transcendentals
  in 128-bit significands, every instruction with the 8087's exceptions and
  responses, and its numeric execution unit beside the CPU, each
  instruction's result committed when it ends.
- `debug` — the debugger's side in the worker: the probe on the board, its
  breakpoints, watches and stepping; the listing in MASM's syntax and the
  8080's; the 8087's values written exactly in decimal.
- `asm` — the x86-16 encoder the ROM stubs and test programs are emitted
  with, never assembled from text.
- `bios` — the system ROM, the EGA's and VGA's ROM and the fixed-disk ROM,
  their services host calls in stubs at the addresses PC software calls, the
  EGA's and VGA's modes kept as IBM's parameter table in `bios::modes`, and
  the LIM 4.0 expanded memory manager behind INT 67h.
- `dos` — TAIRiX DOS: the kernel behind INT 20h–2Fh, its host calls in the
  system ROM and its structures in guest memory, the built-in device drivers,
  `ANSI.SYS` and `MOUSE.COM`, `COMMAND.COM` on the program host the DOS
  programs' Rust behaviour runs in, and the volumes a system disk is made
  of.
- `snapshot` — a machine's whole state as bytes, for a machine suspended and
  resumed: each part kept field by field, or named as left as built, and
  checked as it is read back.
- `link` — the session protocol between the front-end and the machine worker,
  one codec both ways, total over hostile bytes, and the frame slots' layout.
- `worker` — the machine worker: the machine paced to the host's clock, its
  frames, sound and disk requests handed on, its sectors cached, its
  snapshot sent as it suspends and recalled as it resumes, its screen's text
  read back, and pasted text typed a character at a time as the guest reads
  it.
- `front` — the front-end's side of the session, which believes nothing the
  worker sends until it has checked it.
- `roles` — the sandbox roles the binary is started in, the machine's worker
  and the compiler, and the front-end's hold on a machine's compiler, played
  alike by `Run` and the cross-target verticals' fixture (freestanding only).
- `digest` — the reference run and the digest every target asserts of it.
- `app` — the application above the session: the machine library and its
  record, each machine's kept last screen and snapshot, the library window,
  the new-machine assistant and the settings dialog, the screen fitted to its
  window and drawn there straight or through the tube, the machine bar, its
  menu and how a handed-over image goes in, the window's input as the
  machine's, the disk served from an image, the sound on its way to the
  stream, the screen's text copied and the clipboard's typed through code
  page 437, and the debugger's window and its breakpoint dialog.
- `vectors` — the record format of the hardware-captured conformance vectors
  in `assets/cpu-vectors/` (test builds and the `vectors` feature only).

Guest code and guest data are untrusted: every path is total over them. The
engine denies `unsafe` but in `cpu::jit::native`, the entry into installed
code, which miri interprets through stand-in blocks; `Run` makes the native
executor's one promise as it builds one for the machine's worker.

## What runs where

The package is also the `Run` binary of `PCEmulator.app` (`src/run.rs`). Started
by the user it is the front-end: the library window and a window for each
running machine. Re-entered in the sandbox's session-worker role it is one
machine, holding its two pipes and the spawn share its frames are written
into, and nothing else. Re-entered as a one-shot worker serving `compiler` it
is a machine's compiler, holding nothing. Three worker threads of the
front-end's own do everything that waits, so the windows' loop never does:
one reads and writes the library, makes, copies and removes disks, keeps last
screens, and opens a machine's disks and sound and starts its worker; one
serves every running machine's sectors; and one has each machine's compiler
compile the blocks it asks for and installs their code in its worker.

## Capabilities

`CAP_SHM` for the window's frames and the region the machine's frames cross,
`CAP_SANDBOX_SPAWN` to start the machine's worker and its compiler,
`CAP_JIT_MAP_EXEC` to install what the compiler made in the worker — without
it every machine is interpreted — `CAP_CONSOLE_WRITE` for its diagnostics and
`CAP_LOG_EMIT` for a worker ended for what it sent. No
filesystem capability: the library, every machine's disks and its last screen
are blobs of the application's own store.
