# `tairix-smb` — the SMB 3 protocol engine

`lib/smb` speaks SMB 3 (MS-SMB2) for the `smbfs` service: the message codec,
negotiation, key derivation, signing, the encryption transform, and the client
connection core. It is pure — no socket, clock or random source — so the code
the tests and the fuzzer drive is the code the capability-empty sandbox worker
runs: the worker hands it the bytes the front read and the random bytes a
session needs, and writes back what it returns. `plans/SMB.md` stages the rest
of the client — authentication, file operations, share enumeration — on top.

## The protocol floor

Only dialects 3.0, 3.0.2 and 3.1.1 are offered, and a response choosing
anything else is refused with `Refusal::Dialect`. Every session is encrypted:
a 3.1.1 server must choose a cipher from AES-256-GCM, AES-128-GCM,
AES-256-CCM and AES-128-CCM and SHA-512 pre-auth integrity, and a 3.0 or 3.0.2
server must advertise the encryption capability; either failing is a
`Refusal` naming what fell short. 3.1.1 binds the keys to every byte of the
negotiation and authentication through the pre-auth integrity hash; 3.0 and
3.0.2 send `FSCTL_VALIDATE_NEGOTIATE_INFO`, and an answer disagreeing with the
negotiation is `Refusal::Downgrade`.

## Keys, nonces and the transform

`SessionKeys::derive` takes the authentication's session key, the negotiated
dialect, cipher and signing algorithm, the pre-auth hash, the role, and
twelve bytes of fresh randomness. It derives the four keys (signing, each
direction's cipher key, the application key) with the SP 800-108 counter KDF
under the dialect's labels — 3.1.1 over the pre-auth hash, AES-256 keyed from
the whole session key — and wipes them on drop.

Each direction seals under its own nonce sequence. The transform header's
nonce field holds a 64-bit counter, starting at a random value, then four
random bytes (three under CCM's eleven-byte nonce), then zeros. The counter
never repeats within the sequence, the random bytes keep two sequences apart
should one key ever be derived twice, and a sequence that would come back to
its start refuses to seal again rather than reuse a nonce. A connection
installs a session's keys once, so no sequence restarts under the same key.

`seal` encrypts the message or compound in place behind the 52-byte transform
header it writes, binding the header from the nonce on as associated data;
`open` checks the tag and decrypts in place. Signing — AES-CMAC for 3.0 and
3.0.2 and for 3.1.1 by default, AES-GMAC where 3.1.1 negotiated it — is what
the final `SESSION_SETUP` response is checked with: under NTLM, the only proof
that the server knew the password.

## The connection

`Connection` frames requests on Direct TCP (a zero byte and a 24-bit length),
spends and accounts credits, assigns message ids, and matches every response
to its request:

- Once a session holds keys, everything sent on it is sealed, and a response
  to a sealed request is accepted only sealed under that session's keys and
  naming that session in its header.
- An interim `STATUS_PENDING` response, at most one per request, moves the
  request to the async id it names; the final response must carry the same
  id, and `cancel` addresses it.
- An unsolicited oplock or lease break — message id `u64::MAX` — is delivered
  even unsealed: honouring a forged break costs a client its cache, while
  ignoring a genuine one would serve stale data.
- Credits are capped at `MAX_CREDITS` (8192) whatever a server grants.

Anything malformed, unmatched, or failing its integrity check ends the
connection, and every later call answers that first failure.

The data path copies as little as it can. A request is encoded once, its
body written by a callback straight into the frame it is sent in, and sealed
there. A received frame that ends the bytes buffered is handed over in the
buffer it arrived in — the framer reserves a frame's full length once its
prefix arrives, so a large read is never regrown — opened in place, and
delivered as a `Message` over that same buffer. Only a compound's messages,
or a frame followed by more bytes, cost a copy, and then of the shorter part.

## Bounds

A frame is at most 8 MiB and a compound 32 messages; negotiate contexts and
their ids, tree paths, IOCTL outputs and error data carry their own caps. These are security
bounds on a stranger's bytes, not capacities. Nothing a server sends is
allocated for infallibly: exhaustion is `SmbError::OutOfMemory`.

## Tests

The unit tests are held to Microsoft's published traffic between Windows
clients and servers — an SMB 3.0 AES-128-CCM session and SMB 3.1.1 sessions
under AES-128-GCM and AES-128-CCM: the four keys of each, the 3.1.1 pre-auth
hash after every negotiation and authentication message, every sealed request
and response opened in both directions, the signed final `SESSION_SETUP`
responses verified, and Windows' negotiation and authentication messages
decoded and re-encoded byte for byte. The connection accepts a Windows
server's sealed answer end to end. `tests/fuzz_smb.rs` feeds noise, mutated
messages and splices to every decoder, holds every accepted message to
re-encode stably, splits frame streams at random, and requires that a sealed
frame with any byte changed is never accepted.
