# tairix-smb

Stability tier: **experimental**.

The SMB 3 protocol engine (`plans/SMB.md`): what the sandboxed `smbfs` worker
runs. Pure — no I/O, clock or randomness; the host passes in the bytes a
socket read and the random bytes a session needs.

## What it covers

- `wire`, `header`, `status` — the little-endian field codec, the 64-byte
  SMB2 header and its command set, NT status codes.
- `negotiate` — dialects 3.0, 3.0.2 and 3.1.1 only; the 3.1.1 contexts
  (SHA-512 pre-auth integrity, encryption, signing, net name); the client's
  acceptance policy, which refuses anything below the floor or unencrypted;
  validate-negotiate for 3.0 and 3.0.2.
- `session`, `tree`, `ioctl`, `simple` — `SESSION_SETUP`, `TREE_CONNECT`,
  `IOCTL`, and the four-byte bodies and error response.
- `crypt` — the pre-auth integrity hash, key derivation (3.0 and 3.1.1
  labels, the wide AES-256 keys), AES-CMAC and AES-GMAC signing, and the
  encryption transform under AES-128/256-CCM and -GCM, sealed and opened in
  place.
- `frame` — Direct TCP framing and compounds.
- `connection` — the client core: credits and message ids, mandatory sealing
  once a session holds keys, matching every response to its request,
  interim responses and cancellation, unsolicited breaks.

## Guarantees

- A response to a sealed request is accepted only sealed under the same
  session's keys and naming that session; anything malformed, unmatched or
  failing its check ends the connection, and every later call answers the
  first failure.
- Each direction seals under its own nonce sequence: a 64-bit counter from a
  random start, then random bytes from the host. A sequence never repeats a
  nonce, and a connection installs a session's keys once.
- Every decoder is total and bounded — a frame is at most 8 MiB, a compound
  32 messages — and nothing a server sends is allocated for infallibly.
- A frame is encoded once into the buffer it is sent from and sealed there; a
  received frame is opened in place, and one holding a single message is
  handed over in the buffer it arrived in.

## Tests

Unit tests run against Microsoft's published Windows traffic — the SMB 3.0
AES-CCM and the 3.1.1 AES-GCM and AES-CCM sessions — for key derivation, the
pre-auth hash chain, opening sealed traffic in both directions, verifying
signed responses, and byte-exact re-encoding of real negotiation and
authentication messages. `tests/fuzz_smb.rs` feeds noise, mutated messages and
tampered sealed frames to every decoder, the framer and the connection.

`no_std` + `alloc`, `#![forbid(unsafe_code)]`.
