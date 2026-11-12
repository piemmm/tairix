# BBC Micro (`beeb.app`)

`beeb.app` is to be an emulated BBC Micro Model B run as an ordinary
sandboxed desktop application, shipping its own clean-room ROMs. What exists
today is its core, the `tairix-beeb` crate: the machine, the media it reads,
its snapshots, and the Rust 6502 builder with the clean-room MOS built on it.
The front end, the sandboxed machine worker and the bundle are staged in
`plans/BEEB.md` (BB13 to BB16), as are the clean-room BASIC and DFS.

## The machine

The machine is a 32K Model B, cycle-exact: every 6502 bus cycle, dummy
accesses included, happens at its true 2 MHz cycle, and each device is brought
up to the cycle of any access that can observe it. It holds the NMOS 6502, the
System and User VIAs with the keyboard matrix and its links, the 6845, the
video ULA and the SAA5050, the SN76489, the uPD7002, the 6850 and the serial
ULA, the 8271 with two double-sided drives, the cassette interface, and
sixteen sideways slots of ROM or RAM. It is a pure function of its ROMs, media
and timed input, so a run replays exactly.

`tairix_beeb::machine::Machine` is the whole interface: run it to a cycle,
press keys, insert and take back discs and tapes, and collect its picture, its
sound and its printer and RS423 output. A bus monitor records accesses to the
I/O pages while asked.

## Media and snapshots

Discs are SSD and DSD images, written back at their own length, longer only
as far as a guest wrote past it. Tapes are UEF or CSW images, either of which
may arrive gzipped; a tape is recorded over and encodes back to UEF. A
snapshot is the whole machine: every register, RAM, each device's state and
the media in the drives, restored only onto the ROMs it was taken with.

Every image and snapshot is untrusted input. Each decoder holds what it reads
and allocates to fixed bounds, checks every field before it is used, and
refuses the whole input on any failure. A snapshot changes nothing until all
of it has passed, and never names a file. The crate holds no `unsafe` code.

## The ROMs

Each ROM is Rust driving `tairix_beeb::rom::asm::Asm`, which encodes from the
CPU's own instruction table, checks every branch and placement, and takes
hardware addresses from the machine, so a register address is defined once
for the emulation and the firmware.

`tairix_beeb::rom::mos::build` assembles the MOS: the whole MOS 1.20
interface — the vectors and extended vectors, sideways ROM services, OSBYTE,
OSWORD, OSCLI and its commands, the VDU drivers in all eight modes, the
keyboard with soft keys and auto-repeat, buffers and events, sound, the serial
and printer streams, the ADC, and the cassette and ROM filing systems — with
its documented workspace at the documented addresses. It is written from the
published manuals and from genuine ROMs observed only as opaque programs;
no original ROM's code is read, and no genuine ROM byte enters the tree.

Where it differs from MOS 1.20 on purpose: its character set is its own
design, and it does not reproduce &31A for a clipped line's start. The
behaviour pinned against MOS 1.20, and what is left, is in the ROMs section of
`plans/BEEB.md`.

## Testing

- The crate's tests cover every device, the media codecs, snapshots taken and
  restored mid-run, the builder, and the MOS driven through its entry points
  by test language ROMs.
- `tests/cpu_vectors.rs` replays a committed sample of the SingleStepTests
  65x02 suite; `cargo xtask beeb --vectors <dir>` runs the whole suite.
- `fuzz_media` and `fuzz_snapshot` are registered with `cargo xtask fuzz`.
