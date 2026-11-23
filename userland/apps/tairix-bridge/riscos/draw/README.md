# tairix-riscos-draw

TAIRiX Bridge's Draw and DrawFile modules (`plans/RISCOS-EMULATOR.md` §8,
RO24): RISC OS 5.30's Draw 1.22 and DrawFile 1.62, over the guest memory and
the screen the kernel and the VDU drivers give them.

- **Draw**: every SWI through `DrawV`, R8 its index, the convenience calls
  re-entering it as `Draw_ProcessPath`. A path streams through RISC OS's
  stages in its order — closed, flattened, dashed, thickened, flattened
  again, transformed — into an output buffer, a count, a box, the path
  itself, or the fill, which plots through the VDU drivers' spans. Every
  element is a step the host may stop a call at.
- **DrawFile**: a file checked whole before anything is drawn, then each
  object rendered through Draw, ColourTrans and SpriteExtend;
  `DrawFile_BBox`, `DrawFile_DeclareFonts` and `*Render`. Text is drawn as
  RISC OS 5.30 draws it with no Font Manager, until the Font Manager (RO25)
  answers.

Every rule is held to genuine RISC OS 5.30 by the screen corpus in
`tests/riscos5/` and by the cases recorded in the crate's own tests, but the
known differences `docs/src/desktop/tairix-bridge.md` states.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
