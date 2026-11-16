# tairix-riscos-arm

The RISC OS emulator's ARM processor: the A32 and T32 encoding tables, the
decoders, encoders and assembler's text derived from them, the 26-bit and
32-bit programmer's models with Thumb state and `IT` blocks, the two profiles,
architectural exceptions, the FPA as FPEmulator answers, VFP, Advanced
SIMD and the Cryptographic Extension through ARMv8-A's AArch32 additions, and
the guest MMU. It knows nothing of RISC OS. The design and its staging are
`plans/RISCOS-EMULATOR.md` §2; the description is
`docs/src/desktop/riscos-emulator.md`.

Stability: `experimental`.

- `a32`, `t32` — each instruction set's table, `decode`, `encode` and the
  UNPREDICTABLE judgement; T32's take the instruction's place in an `IT`
  block.
- `instruction` — the vocabulary both decode to, and `Instruction::text`.
- `isa`, `profile` — the features an encoding needs, and the profiles that
  fix every implementation-defined choice.
- `cpu` — the processor: registers and banks, the step loop, the decoded
  pages, exceptions, the FPA's and the VFP and Advanced SIMD instructions,
  and why a run stops.
- `fpa` — the FPA: FPEmulator's internal format, the arithmetic, the
  transcendental functions correctly rounded, the transfers' conversions,
  packed decimal among them, the FPSR and traps.
- `vfp` — VFP and Advanced SIMD: the register file, the FPSCR, FPEXC and
  the identification registers; the floating-point unit over `lib/softfloat`
  with ARM's rules; the integer lane arithmetic; and the Cryptographic
  Extension's steps, in constant time.
- `bus` — the memory a processor runs in.
- `mmu` — the guest MMU behind that bus: 4 KiB frames, a global table and
  per-task spaces switched in O(1), page permissions per privilege, host
  frames the guest can never write, the aborts, a direct-mapped TLB tagged
  by space, and the write watch the display and translator take reports
  from.
- `psr`, `reg`, `cond` — the programmer's model's vocabulary.
- `vectors` — the conformance vectors' encoding and runner, an FPA
  instruction's carrying the FPA's state; the committed samples, one per
  profile, are in `tests/vectors/` with their licence and provenance, and
  FPEmulator's in `tests/fpa/` with its provenance.

It holds no `unsafe` code. The address space's layout and policy are the
kernel crate's.
