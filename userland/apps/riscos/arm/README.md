# tairix-riscos-arm

The RISC OS emulator's ARM processor: the one A32 encoding table, the
decoder, encoder and assembler's text derived from it, the 26-bit and 32-bit
programmer's models, the two profiles and architectural exceptions. It knows
nothing of RISC OS. The design and its staging are
`plans/RISCOS-EMULATOR.md` §2; the description is
`docs/src/desktop/riscos-emulator.md`.

Stability: `experimental`.

- `a32` — the table, `decode`, `Instruction::encode`, `Instruction::text`
  and `Instruction::unpredictable`.
- `isa`, `profile` — the features an encoding needs, and the profiles that
  fix every implementation-defined choice.
- `cpu` — the processor: registers and banks, the step loop, the decoded
  pages, exceptions, and why a run stops.
- `bus` — the memory a processor runs in.
- `psr`, `reg`, `cond` — the programmer's model's vocabulary.
- `vectors` — the conformance vectors' encoding and runner; the committed
  sample is in `tests/vectors/` with its licence and provenance.

It holds no `unsafe` code. Thumb, the FPA, VFP and Advanced SIMD, and the
guest address space are later items of the plan.
