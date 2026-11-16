# tairix-riscos-kernel

The RISC OS emulator's kernel. So far it is the guest address space — the two
layouts a session is made with, the memory policy that sizes a session, task
slots switched in O(1) under the 26-bit window rule, and the dynamic-area
manager, `OS_DynamicArea`, `OS_ChangeDynamicArea` and `OS_ReadDynamicArea`
reason by reason, over the processor's MMU — and the observation corpus it is
held to. The design and its staging are
`plans/RISCOS-EMULATOR.md` §3 and §4; the description is
`docs/src/desktop/riscos-emulator.md`.

Stability: `experimental`.

- `memory` — the session's address space: its fixed parts (zero page,
  ScratchSpace, the compatibility page, the stacks, the host pages), the
  page records, and the seam its handler, service and upcall calls go
  through.
- `memory::layout` — the shared session's 26-bit-capable layout and RISC OS
  5's own.
- `memory::policy` — the ceiling: a quarter of usable RAM, only ever lowered.
- `memory` slots — each task's application space in a space of its own.
- `memory` areas — the dynamic areas: numbering, placement, resizing,
  particular pages and their replacement, doubly mapped and sparse areas,
  and physical memory pools.
- `corpus` — the recorded observation corpus: its scenarios of SWI calls and
  the fixtures genuine RISC OS left, which `tests/corpus.rs` pairs.

It holds no `unsafe` code. Registers, guest error blocks, page blocks and
titles in guest memory, and running handlers are the SWI layer's.
