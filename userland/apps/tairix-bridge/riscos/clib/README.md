# tairix-riscos-clib

TAIRiX Bridge's SharedCLibrary (`plans/RISCOS-EMULATOR.md` §10, RO26): RISC
OS 5.30's SharedCLibrary 6.23 — its `LibInit` SWIs, the library kernel's 48
entries, the C library's 187 and RISC OS 5's C99 chunks 4 to 9, as host
routines over guest memory a client's vector reaches.

- **The library kernel.** A client's start, its languages' procedures,
  traps, escapes and events searched for among them, stack extension through
  the default or a client's allocator, the heap, unwinding, sub-programs, and
  the OS interface functions, each held to what RISC OS 5.30 recorded.
- **The C library.** `_clib_initialise` and `_main`, the heap in the
  program's memory, `string.h`, `ctype.h`, `stdlib.h`, `setjmp.h`, signals,
  locales, `math.h` on an FPA, `time.h`, and `stdio.h`'s streams through
  `FILE` blocks laid as the headers lay them.
- **The C99 library.** Chunk 4's 64-bit arithmetic and conversions, chunk
  5's C99 functions — `fenv.h`, the float and C99 mathematics, the complex
  arithmetic, the `long long` and 64-bit file functions — chunk 6's atomic
  operations, and chunks 7 to 9, which give parts of chunk 5 again.
- **Chunks.** The library answers as a release holding only the chunks
  built.

The crate's tests run the probe programs that recorded RISC OS 5.30 — BBC
BASIC V assembling a C client — or replay their records through the
library's own entries, and hold the library to what they found, but for the
known differences `docs/src/desktop/tairix-bridge.md` states.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
