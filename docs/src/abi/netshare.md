# Network shares (`netshare-v1`)

`netshare-v1` is how a program reaches the network-share service, `smbfs`
(`lib/abi/src/netshare_ipc.rs`, `plans/SMB.md`). The client is
[`lib/netshare`](../lib/netshare.md). Every request is one call to the
reserved endpoint `NETSHARE_ENDPOINT` and one reply; the caller's identity and
login session are its kernel-attested origin, never a field.

Only a program that may reach the network itself may call: the endpoint's
send capability is `CAP_NET`, which the kernel enforces before the service
reads a byte. Everything the service does for a caller is network work, and a
request's names cross the wire — the host in a lookup, the user and domain in
the sign-in — so without the rule a program with no network authority could
probe hosts and ports, or carry data out, through the service.

## Requests

| Op | Request   | Carries                                   | Answer |
|----|-----------|-------------------------------------------|--------|
| 1  | `Shares`  | the endpoint, how to sign in, `special`   | the pin the connection reported, the counts of the special shares left out, the shares no tree connect could name and those past the first `SHARES_MAX`, and the disk shares |
| 2  | `Mount`   | the endpoint, how to sign in, the share, `read_only` | the pin, and the `OfferId` the caller attaches |
| 3  | `Unmount` | an `OfferId` the caller holds             | nothing |
| 4  | `Mounts`  | nothing                                   | the caller's mounts: offer, host, share, read-only, and whether it is serving, reconnecting or lost |

A mount is answered with an offer, never a volume: the caller attaches it
itself (`plans/FSSERVE.md` FSS6), so where it goes and how it is presented are
decided by the kernel against the real caller.

## The endpoint and the sign-in

- **Host** — one to `HOST_MAX` (255) bytes, none a control character,
  whitespace or a path separator: the name the service name and the tree path
  are built from.
- **Address** — the unicast address shown when the host was chosen, or none
  for the service to resolve the name. An IPv6 link-local address carries the
  interface it was found on; a port is never zero.
- **Pin** — the server GUID a `NEGOTIATE` answered with and the address it was
  reached at. A remembered credential is offered only with the pin it was
  remembered under; the service checks what the connection reports against it
  before any credential leaves, and a host that differs is `PinMismatch`,
  carrying what it reported instead. The pin catches a different host
  answering, not an impostor, which can repeat both an unauthenticated GUID
  and an address; only Kerberos proves the server (`plans/SMB.md`).
- **Floor** — the oldest dialect accepted: 3.0, 3.0.2 or 3.1.1. Nothing lowers
  it below 3.0.
- **Credentials** — an NTLMv2 user (never empty), domain (empty for the
  server's own accounts), and password, each at most 256 bytes. The password
  is a `Secret`, which no `Debug` shows. Kerberos credentials join the request
  with `plans/KERBEROS.md` K4.

## Replies

A reply opens with its magic, version, the request's operation (`0` when the
service could not read the request: `Unreadable`), an outcome and a detail.
Outcome `0` is an answer; the refusals are:

| Outcome | Refusal | Detail |
|---|---|---|
| 1 | `UnknownHost` — the name resolves to no address | |
| 2 | `Unreachable` — no connection could be made | |
| 3 | `PinMismatch` — followed by the pin reported | |
| 4 | `Unsupported` — the server falls short | `Dialect`, `Encryption`, `Signing`, `Authentication`, `Guest` |
| 5 | `Tampered` — the connection failed its integrity checks | |
| 6 | `Protocol` — the server broke the protocol | |
| 7 | `TooLarge` — the server answered with more than this system accepts | |
| 8 | `Rejected` — the server refused the credentials | `Credentials`, `Disabled`, `Locked`, `Expired`, `Restricted` |
| 9 | `NoSuchShare` | |
| 10 | `Denied` — the account may not use the share | |
| 11 | `NoSession` — the caller has no login session for a mount to end with | |
| 12 | `NoSuchMount` — the caller made no such mount | |
| 13 | `Limit` — the caller holds as many mounts or connections as it may | |
| 14 | `Failed` — the service failed | |
| 15 | `AlreadyMounted` — the caller has that server's share mounted already | |

## Bounds

A request is at most `NETSHARE_MAX_REQUEST` (1431 bytes). `Op::max_reply`
bounds each operation's reply, its refusals included, and is the buffer a
caller receives it in: a listing of at most `SHARES_MAX` (256) shares, the
rest counted, each name at most `SHARE_NAME_MAX` (80) characters and each
remark `REMARK_MAX` (1024) bytes, is at most `NETSHARE_MAX_REPLY` (345,400
bytes); a principal's
mounts, at most `MOUNTS_MAX` (32), at most 18,859 bytes; a mount's answer 50
bytes and an unmount's 42.

## Decoding

Both ends decode with the one codec, fail closed, and accept only what the
encoder writes: every field is shape-checked, a field an operation does not
use is zero, a flag it does not carry is refused, and a frame ends exactly
where its last field does. A listing a decoder accepted is read again as it is
walked, never copied. The decoders are fuzzed in `lib/abi/tests/fuzz_decode.rs`,
from random bytes and from every bit of a canonical frame of each kind flipped.
