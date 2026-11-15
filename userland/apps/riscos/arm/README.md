# tairix-riscos-arm

The RISC OS emulator's ARM processor: the A32 and T32 encoding tables, the
decoders, encoders and assembler's text derived from them, the 26-bit and
32-bit programmer's models with Thumb state and `IT` blocks, the two profiles
and architectural exceptions. It knows
nothing of RISC OS. The design and its staging are
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
  pages, exceptions, and why a run stops.
- `bus` — the memory a processor runs in.
- `psr`, `reg`, `cond` — the programmer's model's vocabulary.
- `vectors` — the conformance vectors' encoding and runner; the committed
  samples, one per profile, are in `tests/vectors/` with their licence and
  provenance.

It holds no `unsafe` code. The FPA, VFP and Advanced SIMD, and the guest
address space are later items of the plan.
