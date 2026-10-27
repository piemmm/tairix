# `tairix-netshare` — the network-share client

`lib/netshare` is how a program lists an endpoint's shares, has one mounted,
withdraws a mount, and lists its own, through the network-share service over
[`netshare-v1`](../abi/netshare.md). Stability tier: **experimental**.

## Shape

- **`Client`** — the pure client over an injected `Transport`: `shares`,
  `mount`, `unmount` and `mounts`, one call each. A listing and the mounts
  borrow the client's reply buffer until its next request; a mount answers a
  `Mounted`, the pin the connection proved and the offer the caller attaches.
- **`Error`** — why a request did not succeed: refused by the protocol before
  it was sent (`Invalid`), no service running (`Unavailable`), a program
  without `CAP_NET`, which the kernel refuses (`NotPermitted`), the service's
  own reason (`Refused`), a request it could not read (`Unreadable`), the call
  failing (`Transport`), a reply it never sends (`Malformed`), or no memory for
  the reply (`OutOfMemory`). Its
  `Display` is the one wording of each reason the commands and `files.app`
  show.
- **`RtTransport`** (the `program` feature) — the service's endpoint, called
  through the kernel.

## Guarantees

- The request, and the password in it, is built in a buffer erased as soon as
  the call returns, on every path. The caller's own copy of the password is
  the caller's to erase.
- The reply buffer is sized to the operation's bound (`Op::max_reply`), grown
  once and kept, so a program that lists shares again does not allocate again.
- An answer is accepted only for the request it answers: a refusal of another
  operation, or another operation's answer, is `Malformed`.
- A call waits for the network work the service does on it. An interactive
  surface makes it from a worker, never from its own loop.

## Tests

Host tests run the client against a scripted service: each request crosses as
the protocol encodes it, each answer is accepted only for its request, every
failure is reported as what it is, a request the protocol refuses is never
sent, and every failure states a reason of its own.
