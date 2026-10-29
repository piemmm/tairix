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
- `ntlm` — the NTLMv2 initiator: extended session security without key
  exchange, the response bound to the server's target information and to
  `cifs/<host>`, the MIC, the signatures SPNEGO's mechListMIC is made with.
- `spnego` — RFC 4178 tokens over `lib/der`, one mechanism offered.
- `client` — one principal's connection: negotiation, the session's
  authentication and re-authentication (naming the session it replaces after
  a dropped connection), tree connect and disconnect, logoff, and requests on
  a tree.
- `file` — create (v2 lease, v2 durable request and reconnect, maximal
  access, on-disk id), close, read, write, flush, directory listings,
  information classes, change notification, lease breaks and their
  acknowledgment; the name mapping both TAIRiX and SMB carry, and exact
  FILETIME times.
- `rpc` — connection-oriented DCE/RPC over a named pipe, NDR for
  `NetrShareEnum` level 1, and `ShareListing`, the whole share listing as
  requests on `IPC$`.

## Guarantees

- A response to a sealed request is accepted only sealed under the same
  session's keys and naming that session; anything malformed, unmatched or
  failing its check ends the connection, and every later call answers the
  first failure.
- Each direction seals under its own nonce sequence: a 64-bit counter from a
  random start, then random bytes from the host. A sequence never repeats a
  nonce, and a session's keys are derived and installed once — a
  re-authentication keeps them.
- A session is keyed only once the final `SESSION_SETUP` response verifies
  under the keys its authentication derived; on 3.1.1 those keys bind every
  byte of the negotiation and setup. Guest and anonymous sessions, a
  mechanism the client did not offer, and a server mechListMIC that does not
  verify are refused.
- On 3.0 and 3.0.2 no tree is handed out until validate-negotiate has passed,
  and a mismatch or a refusal ends the connection.
- A name crosses only when it is a TAIRiX component and an SMB filename at
  once, and a path is such names a single `\` apart: a listing can carry no
  separator, stream name or wildcard into a path. A time is converted exactly
  or refused, never rounded.
- A share listing's bind must be accepted as offered and its answer arrive
  under the call's ids; it keeps the first 256 disk shares, the special ones
  only when asked, and counts what it leaves out — those not asked for, those
  no tree connect could name, and those past the 256 — and a failed listing
  still closes its pipe.
- Every decoder is total and bounded — a frame is at most 8 MiB, a compound
  32 messages, a session 64 trees, a read or write 1 MiB, a listing 256 KiB,
  a share listing 256 KiB — and nothing a server sends is allocated for
  infallibly. The password-derived NTLM key is wiped once the response is
  made.
- A frame is encoded once into the buffer it is sent from and sealed there; a
  received frame is opened in place, and one holding a single message is
  handed over in the buffer it arrived in.

## Tests

Unit tests run against Microsoft's published Windows traffic — the SMB 3.0
AES-CCM and the 3.1.1 AES-GCM and AES-CCM sessions — for key derivation, the
pre-auth hash chain, opening sealed traffic in both directions, verifying
signed responses, byte-exact re-encoding of real negotiation and
authentication messages, and the NTLM MIC Windows computed; against
MS-NLMP §4.2.4 for NTLMv2; and against a QNAP NAS's Samba, captured on a LAN.
Windows' `WRITE` and `READ` decode and re-encode byte for byte, and the
`srvsvc` bind is the one every client sends. The client runs against the
scripted server behind the `test-support` feature, which checks the NTLMv2
proof, the MIC, the service name, the mechListMIC and — through the keys it
derives — the pre-auth hash independently; its in-memory disk share outlives
a connection, so file traffic, lease breaks, watches and durable reconnects
run end to end, and its `IPC$` serves `srvsvc`. `tests/fuzz_smb.rs` feeds
noise, mutated messages and tampered frames to every decoder, the framer,
the connection, the client and the share listing.

`no_std` + `alloc`, `#![forbid(unsafe_code)]`.
