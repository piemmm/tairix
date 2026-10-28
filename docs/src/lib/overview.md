# Shared libraries (Stage 1)

The `lib/` workspace tree is the only place TAIRiX publishes code that is
shared by more than one component. Every crate here is `no_std` and exposes a
deliberately narrow public surface; the table says which allocate.

| Crate                  | Purpose                                                          | Stability |
|------------------------|------------------------------------------------------------------|-----------|
| [`tairix-abi`](abi.md) | Frozen `abi-v1` types crossing the kernel/user boundary.         | frozen    |
| [`tairix-caps`](caps.md) | Capabilities, capability sets, and signed delegation tokens.   | stable    |
| [`tairix-collections`](collections.md) | Heap-backed containers: the hash, recency, range, radix and slot maps, the indexed heap, the spilling small vector, the byte queue, the stable list. | experimental |
| [`tairix-crypto`](crypto.md) | Audited wrappers (SHA-256, Ed25519 verification).          | stable    |
| [`tairix-hash`](hash.md) | Keyed SipHash-1-3, the XXH64 fast mixer, and the per-boot / per-process key. | experimental |
| [`tairix-inline`](inline.md) | Allocation-free containers: bounded vector and string, rings, the intrusive list, the fixed bitset. Links no allocator. | experimental |
| [`tairix-log`](log.md) | Structured, level-filtered, alloc-free logging.                  | stable    |
| [`tairix-rng`](rng.md) | CSPRNG (HMAC-DRBG), a fast unpredictable ChaCha12 generator, a predictable one, + the entropy/hardware-RNG seam. | experimental |
| [`tairix-util`](util.md) | Heap-backed shared utilities, each with two or more independent callers outside it, which `PLAN.md` names. | experimental |

These crates are the foundation Stage 2 (the kernel core) builds on. They
are intentionally small: every addition must justify its existence per
`AGENTS.md` §2.3.
