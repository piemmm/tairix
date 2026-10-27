# `tairix-smb` — the SMB 3 protocol engine

`lib/smb` speaks SMB 3 (MS-SMB2) for the `smbfs` service: the message codec,
negotiation, key derivation, signing, the encryption transform, the
connection core, SPNEGO and NTLMv2 authentication, and the client that holds
one principal's session and trees. It is pure — no socket, clock or random
source — so the code the tests and the fuzzer drive is the code the
capability-empty sandbox worker runs: the worker hands it the bytes the front
read and the random bytes a session needs, and writes back what it returns.
`plans/SMB.md` stages file operations and share enumeration on top, and
`plans/KERBEROS.md` K4 the Kerberos mechanism beside NTLM.

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
connection, and every later call answers that first failure. So does a
response that leaves the client no credit with nothing outstanding: no later
response could grant one, so the connection could never send again.

## Authentication

A session authenticates inside SPNEGO (RFC 4178), which the client uses to
offer exactly one mechanism, so there is no choice for an attacker to steer:
a mechanism that fails is never traded for another. The server's `NEGOTIATE`
hints are read first, and a client holding only NTLM refuses a server that
does not offer it (`Refusal::Mechanism`) before sending anything; empty hints
leave the choice to the client.

NTLMv2 (`ntlm`) is spoken with extended session security and without key
exchange — with NTLMv2 an exchanged key adds RC4 and no security, since it is
recoverable from the same NT hash. A challenge that withholds extended
session security, target information, signing or 128-bit keys is
`Refusal::Ntlm`. The response echoes the server's target information,
announces the MIC, binds no channel, and names the service the client means
to reach, `cifs/<host>`, so it cannot be relayed to another; the MIC covers
all three messages. The user name is upper-cased as Windows and Samba do it,
one UTF-16 unit for one under Unicode's simple mapping. An empty user is an
anonymous logon and refused. The password-derived key is wiped the moment the
response is made.

## The client

`Client` is one principal's connection to one server — connections are shared
per principal and endpoint, so no worker holds two principals' keys. It
queues the `NEGOTIATE` on creation; then bytes the socket read go in, bytes to
write come out, and what happened arrives as `Event`s.

- **Negotiation.** The accepted negotiation is `Client::negotiated`. On 3.1.1
  the pre-auth integrity hash covers the request and response.
- **Session setup.** `authenticate` sends the first leg; the server's answer
  is chained into the session's pre-auth hash, the mechanism's answer and its
  mechListMIC go back, and the final response is checked: a guest or
  anonymous session is `Refusal::Guest`, and the keys are derived — on 3.1.1
  from the hash over every message but the last — and installed only once the
  final response's signature verifies under them. Under NTLM that signature
  is the only proof the server knew the password. The server's mechListMIC is
  verified whenever it sends one.
- **Refusals and failures.** A server status refusing the authentication —
  a wrong password, a disabled account — is `Event::Rejected`, and another
  attempt may follow on the same connection. Everything else wrong ends it.
- **Validation.** On 3.0 and 3.0.2 the first tree to connect carries
  `FSCTL_VALIDATE_NEGOTIATE_INFO`, sealed, and every tree is held until it
  passes. An answer disagreeing with the negotiation is
  `Refusal::Downgrade`; an error answer is `Refusal::Validation` — a server
  that cannot confirm the negotiation is not trusted to have received it
  untampered. A validation owed when no credit is free goes once one frees.
- **Trees.** `connect_tree` builds `\\<server>\<share>` from the name dialled,
  refusing a share name holding a separator or a control character; a refused
  share is `Event::TreeRefused`. A session holds at most 64 trees.
- **Requests.** `send` puts requests on a handed-out tree, sealed and
  compounded, filling in the session and tree; each answer is an
  `Event::Response`. The client's own commands — negotiate, session setup,
  tree connect and disconnect, logoff, cancel — are refused there.
- **Expiry and renewal.** A response saying the session expired is reported
  once as `Event::Expired`; `reauthenticate` runs a fresh exchange under the
  session's existing keys, which are never derived again, so no nonce
  sequence restarts.
- **Logoff** waits for nothing to be outstanding, and drops the session's
  keys with its answer. A failure drops them at once.

The data path copies as little as it can. A request is encoded once, its
body written by a callback straight into the frame it is sent in, and sealed
there. A received frame that ends the bytes buffered is handed over in the
buffer it arrived in — the framer reserves a frame's full length once its
prefix arrives, so a large read is never regrown — opened in place, and
delivered as a `Message` over that same buffer. Only a compound's messages,
or a frame followed by more bytes, cost a copy, and then of the shorter part.

## Bounds

A frame is at most 8 MiB and a compound 32 messages; negotiate contexts and
their ids, tree paths, IOCTL outputs and error data carry their own caps, as
do NTLM's target information (4 KiB) and names (512 bytes of UTF-16) and the
mechanisms a server's hints may list (16). These are security bounds on a
stranger's bytes, not capacities. Nothing a server sends is allocated for
infallibly: exhaustion is `SmbError::OutOfMemory`.

## Tests

The unit tests are held to Microsoft's published traffic between Windows
clients and servers — an SMB 3.0 AES-128-CCM session and SMB 3.1.1 sessions
under AES-128-GCM and AES-128-CCM: the four keys of each, the 3.1.1 pre-auth
hash after every negotiation and authentication message, every sealed request
and response opened in both directions, the signed final `SESSION_SETUP`
responses verified, and Windows' negotiation and authentication messages
decoded and re-encoded byte for byte. The connection accepts a Windows
server's sealed answer end to end. NTLMv2 is held to MS-NLMP §4.2.4's worked
example and to the MIC Windows computed over its own exchange; SPNEGO to the
tokens Windows framed; and both, through the client, to a QNAP NAS's Samba
captured on a LAN.

The client runs against a scripted server (the `test-support` feature's
`peer`) that checks what a real one would — the NTLMv2 proof, the MIC, the
service name, the mechListMIC, and through the keys it derives the pre-auth
hash — under every dialect and cipher, and with each misbehaviour it can be
asked for: a guest session, an unsigned or forged final response, a forged
mechListMIC, another mechanism, a weakened challenge, a downgraded or refused
validation, an expiring session, starved credits.

`tests/fuzz_smb.rs` feeds noise, mutated messages and splices to every
decoder, holds every accepted message to re-encode stably, splits frame
streams at random, and requires that a sealed frame with any byte changed is
never accepted. Against the scripted server with one of its frames changed,
the client never establishes a session whose final response was changed, nor
on 3.1.1 one whose negotiation or setup was, and on 3.0 and 3.0.2 never hands
out a tree after a changed dialect, capability, GUID or security mode.
