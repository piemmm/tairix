# tairix-fuzzseed

**Stability tier: experimental** (test scaffolding; not part of any shipped
TAIRiX interface).

Shared host test-support seam for the harnesses that draw pseudo-random
inputs — the §19.6 fuzz harnesses, the §19.7 stateful proptest models, and the
filesystem soak. It is the single place (`AGENTS.md` §2.2) that:

- picks a per-run PRNG seed — **fresh entropy by default**, env-pinned for
  replay (`resolve_seed` / `start`);
- **logs the seed at the start of each test**, with the exact `VAR=value` to
  reproduce it (`announce` / `start`);
- bounds the work: a **single smoke iteration** by default, or a
  wall-clock soak loop (`budget_deadline` / `within_budget`) that a soak
  window's close (`SOAK_UNTIL_ENV`) cuts short, and the one rule a coarse
  soak uses to start no pass it cannot finish (`room_for_another_pass`);
- owns the **one generator** every harness and randomised test draws from,
  `Prng` (SplitMix64): every output bit is mixed, so `below`, `at_most`,
  `pick`, `next_u8`…`next_u64`, and `fill` may be used for any choice. A
  bounded draw spends one word whatever its bound, so a stream replays
  exactly from its seed. A private generator anywhere else is a defect;
- owns the **one metered allocator** a host test installs as its global
  allocator to hold a call to an allocation bound or a graceful refusal
  (`meter::Metered`, `meter::metered`, `meter::refusing_above`,
  `meter::refusing_after`, which grants a call its first requests and refuses
  the rest, as a heap running out partway does): every figure
  is the calling thread's, so neither the harness's own threads nor tests
  running beside a measurement charge it;
- owns the **one walk of a crate's own sources** the tests that pin code
  against them read (`sources::crate_sources`, `unlisted_assembly`,
  `is_assembly`): every `.s` file and every `.rs` file but a `*_tests.rs`
  module, so a pin's needles never match themselves;
- owns the **one lexer of inline-assembly invocations** those pins read
  (`asm::invocations`, `bindings`, `template_instructions`,
  `mnemonic`), so no port re-derives where a block ends or what it binds;
- owns the **one scratch directory** a host test writes a tree into
  (`scratch::Scratch`): made private to its user by a create that refuses
  anything already at its name, named apart from every other in the process
  however coarse the clock, and removed with what it holds once dropped;
- owns the **one rule for where the pinned Raspberry Pi firmware is**
  (`firmware::pi4_dtb`, `PI_FIRMWARE_ENV`): the directory the variable names,
  which must then hold the tree, else the checkout's own cache, so the gate
  that stages and names it can never pass a test that read nothing;
- owns the **one document generator** for a closed settings registry's fuzz
  harness (`registry::registry_document`): whole lines drawn from the
  registry's table, one document in `POISONED_ONE_IN` carrying a value its
  key refuses;
- with the `proptest` feature, owns the **one stateful-model runner**
  (`prop::drive`, `prop::config`): the seed resolved and logged before the
  first case, then the smoke run or a budgeted soak off one continuing RNG.

It lives under `tests/` — not `lib/`, which is reserved for code that ships
inside TAIRiX — and is consumed only as a `[dev-dependencies]` entry of the
harness crates, so it never enters a TAIRiX build graph. The seed is a
*test-input* seed, not a security seed, so it deliberately does not route
through `lib/crypto` / `lib/rng` (`AGENTS.md` §22).

## Replaying a failure

A failing harness prints, before its first input:

```
[fuzzseed] <test>: PRNG seed = 12345 (0x0000000000003039); replay with TAIRIX_FUZZ_SEED=12345
```

Reproduce it in a development environment by pinning that seed and forcing the
single-iteration smoke run:

```
TAIRIX_FUZZ_SEED=12345 cargo test -p <crate> --test <harness>
# or, through the orchestrator:
cargo xtask fuzz --target <harness> --seed 12345
```
