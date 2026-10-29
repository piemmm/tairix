# SMB.md — Network shares: the SMB 3 client, `smbfs`, and mounting

Staged build plan for browsing, authenticating to, and mounting SMB/CIFS file
shares. **Binding under `AGENTS.md`** (read it first, especially §2, §4, §5,
§16, §19, §24, §26, §28). It consumes and never contradicts:

- `plans/ZEROCONF.md` — discovery, the catalogue (Z8), the `files.app`
  Network view (Z9) and WS-Discovery (Z10), which own their own status;
- `plans/FSSERVE.md` — the kernel↔user-space filesystem protocol a mounted
  share is served over;
- `plans/KERBEROS.md` — the Kerberos 5 client;
- `plans/NEW-NAMESPACE.md` NS-4a — `view_bind`, which places a share at an
  operator-chosen directory;
- `docs/src/filesystem/drives.md` — `Storage:`, aliases, projections.

`abi-v1` is not frozen: every ABI this plan adds is an ordinary pre-release
change (§2.13).

## Ledger

| id | What it is | Status |
|---|---|---|
| SMB1 | The shared display-safety policy for untrusted names (ZEROCONF §6.6): `lib/visible`'s rule over `lib/unicode`, applied by every surface and escaper | done |
| SMB2 | `lib/crypto` foreign-protocol constructions: the NTLMv2 functions over MD4/HMAC-MD5, AES-CMAC, AES-GMAC, AES-CCM, the SP 800-108 counter KDF, HMAC-SHA384, and the Kerberos AES-CTS/PBKDF2-HMAC-SHA1 set | done |
| SMB3 | `lib/der`: the bounded, fail-closed DER codec SPNEGO and Kerberos share | done |
| SMB4 | `lib/smb` transport: the SMB2/3 codec, 3.0–3.1.1 negotiation, pre-auth integrity and validate-negotiate, credits, signing, mandatory encryption, compounding; fuzzed | done |
| SMB5 | `lib/smb` authentication: SPNEGO with mechListMIC, NTLMv2 (target info, MIC, SPN), and the `client` — the session-setup, re-authentication and tree-connect state machine its mechanisms plug into; Kerberos joins it in KERBEROS K4 | done |
| SMB6 | `lib/smb` file operations: create/close/read/write/flush, directory enumeration, query/set info, leases, change notify, durable handles and reconnect | done |
| SMB7 | `lib/smb` share enumeration: DCE/RPC bind and NDR `NetrShareEnum` over `IPC$` | done |
| SMB8 | `netshare-v1`, the client protocol to `smbfs`, and the `lib/netshare` client `files.app` and the commands share | done |
| SMB9a | FSSERVE FSS6 for network shares: the kernel-minted offer and its attach by the named principal, the presentations, owner and server detach, server-reported liveness, `MountRecord`'s owner and class, the lanes and registry entry a multi-writer share needs; with NEW-NAMESPACE NS-4a's `view_bind` for an explicit target | done |
| SMB9b | `userland/net/smbfs`: the bundle and its account, the front's loop, the `netshare-v1` endpoint and its deferred replies, name resolution off the loop, one sandboxed `lib/smb` worker per connection, connections shared per (principal, pin), listing (the first 256 shares, the rest counted), mount and unmount, the login session's end withdrawing its mounts, the audit | done |
| SMB9c | The served filesystem: every driver-trait operation over SMB, node ids kept by a path-keyed map, concurrent calls, the front holding every byte not yet written | done |
| SMB9d | Caching under leases: attributes and listings under a directory lease or for one second, file data and write-back under a read/write lease, every dirty byte flushed before a break is acknowledged, on `fsync`, close and unmount | done |
| SMB9e | Reconnect: a dropped connection, a stack restart or a lost worker re-established with durable-v2 handles and the replay flag inside the grace window; past it the mount reports lost and is still tried | done |
| SMB10 | The commands: the `mount` bundle with `-t smb` (closes D1014 for network sources), `unmount` for served volumes, `smbtree` | planned |
| SMB11 | DFS referrals: following a namespace link to the server that holds it | planned |
| SMB12 | The test peers: an in-tree SMB 3 server and KDC for host tests, and the QEMU vertical that mounts a share end to end | planned |

The ledger is the one status record (§13). The catalogue, the Network view and
WS-Discovery are ZEROCONF Z8, Z9 and Z10, and the discovered-share mount Z11; the served-filesystem protocol and
network attachments are FSSERVE FSS1–FSS3 and FSS6; `view_bind` is NEW-NAMESPACE NS-4a; the
Kerberos client is KERBEROS K1–K4.

## 0. Decisions (binding)

**Protocol floor.** Dialects 3.0, 3.0.2 and 3.1.1 only. SMB1 and every 2.x
dialect are refused with a stated reason; a server offering nothing newer is
not reachable. Every session is **encrypted** — 3.1.1 offers AES-256-GCM,
AES-128-GCM, AES-256-CCM, AES-128-CCM in that order, 3.0/3.0.2 use AES-128-CCM
— and nothing is ever sent or accepted in the clear after session setup. 3.1.1
binds negotiation with SHA-512 pre-auth integrity; 3.0/3.0.2 send
`FSCTL_VALIDATE_NEGOTIATE_INFO` and fail closed on a mismatch. There is no
option that lowers any of this.

**Authentication.** Kerberos (`plans/KERBEROS.md`) and NTLMv2, both inside
SPNEGO. Guest and anonymous sessions, LM, and NTLMv1 are refused. `sec=auto`
(the default) uses Kerberos when the server offers it, the principal names a
realm, and the host was chosen by DNS name; otherwise NTLMv2. A Kerberos
**attempt that fails** does not fall back to NTLM — that is the downgrade an
attacker would force — it fails with the KDC's reason. NTLM's key exchange
(`NEGOTIATE_KEY_EXCH`) is not negotiated: with NTLMv2 it adds RC4 and no
security, since the exported key is recoverable from the same NT hash either
way. The final `SESSION_SETUP` response's signature is verified on every
dialect: for NTLM it is the only proof the server knew the password.

**Discovery.** Browsing is a query, never a path (ZEROCONF §6.1): mDNS/DNS-SD
`_smb._tcp` and WS-Discovery computers feed one catalogue (Z8), plus hosts the
user names in *Connect to Server*. NetBIOS and LLMNR never (ZEROCONF §1).

**Mounting.**

- *Anyone may mount, privately, by default.* A mount made by a principal
  without `CAP_FS_MOUNT` is presented with uniform metadata owned by that
  principal — directories `rwx------`, files `rw-------`, the write bit
  dropped where the server marks a file read-only — so no other user can
  reach the share through the owner's SMB credentials. The kernel enforces
  this presentation (FSSERVE §6); `smbfs` cannot widen it.
- *Administrators choose.* A `CAP_FS_MOUNT` holder may present a mount shared
  (`-o shared`: the `storage` group, `rwxrwx---`/`rw-rw----`) or with explicit
  `uid=`/`gid=`/`file_mode=`/`dir_mode=`, all accesses then made with the
  mounting account's SMB credentials, as a Linux non-multiuser CIFS mount.
- *Where.* By default `/Storage/<name>`, the name derived from the share and
  host through the shared volume-naming policy with its collision suffixes. An
  explicit target is placed by `view_bind` (NS-4a): a principal without
  `CAP_FS_MOUNT` may name an empty directory it owns anywhere under `/Users` or
  `/Storage`; an administrator may name any directory under `/Users` or
  `/Storage`. `/System` is refused to everyone, and the root view gains no
  entry (§16.1).
- *Always `noexec`, with no relaxation* (ZEROCONF §6.3). `ro` may be added.
- *Lifetime.* A private mount lives while the login session that made it does:
  `smbfs` watches the session's process (`PeerExit`) and withdraws the mount,
  flushing first, when it ends. An administrator's mount lives until
  unmounted. Persistence across reboot is the signed namespace policy's
  business (NS-3), not this plan's.

**Credentials and pinning** (ZEROCONF §6.4). Nothing is mounted and no
credential is asked for on an arrival; a prompt follows a user's choice. A
password never travels on a command line and never reaches a log or `stdinfo`.
A remembered credential is stored in the *requesting app's own* sealed
app-data scope (`plans/APPDATA.md` §3.7), bound to the endpoint pin — the
server GUID `NEGOTIATE` returned, the host name chosen, and the address it
resolved to — and is never offered to an endpoint whose pin differs: that host
presents as new and unknown. Kerberos mutual authentication additionally
proves the server holds its service key.

**Containment** (§19.5). Every byte a server sends is parsed in a
capability-empty sandbox worker: SMB messages, SPNEGO, NTLM, Kerberos replies,
DCE/RPC, and every decryption. The `smbfs` front owns the sockets, the served
filesystems and the control endpoint, and parses only its own typed frames.

**Not in this plan.** An SMB *server* (sharing is `plans/SHARING.md`'s, which
does not exist yet); multichannel, RDMA, QUIC transport and 3.1.1 compression
(performance features, stated absences, never silent ones); NFS.

## 1. Architecture

```text
 files.app ─┐            netshare-v1            fsserve-v1 (per share)
 mount ─────┼─ lib/netshare ──────────▶ smbfs front ◀──────────────▶ kernel ServedFs ─▶ VFS
 smbtree ───┘                           │      ▲
                         sockets (CAP_NET) │      │ typed frames (pipes)
                                        ▼      │
                                    netstack   sandboxed workers
                                               (lib/smb, lib/krb5)
```

- **One worker per server connection**, a plain `lib/sandbox` session: the
  worker holds that connection's keys and every piece of parsed server state,
  which no owner could rebuild, so a lost worker is a lost connection,
  re-established like a dropped one (SMB9e). The front holds sockets, sends
  ciphertext the worker produced, hands the worker the bytes it read,
  unread, and supplies the randomness a sandbox may not draw — nonce seeds,
  NTLM's client challenge, the client GUID. A crashed worker costs its own
  connection, never the service.
- **Connections are shared per principal, endpoint and account**: listing
  shares, then mounting two of them, is one TCP connection and one session. A
  connection serves only calls naming its account with the same password (the
  derived keys compared in constant time), so no call rides a session it did
  not prove it may open; a remembered pin is checked against each call.
- **The front's typed frames** are bounded and validated before anything they
  carry reaches the kernel; the kernel validates the served replies again
  (FSSERVE §3) — a compromised worker gains no authority.
- **Data path.** A read crosses socket → front → worker (decrypt) → front →
  the served filesystem's shared window → caller. The sandbox costs two copies;
  that is the price of not parsing a stranger's bytes in a process with
  authority, and it is measured rather than assumed (§8).

## 2. The engine (`lib/smb`)

Pure, `no_std` + `alloc`, `#![forbid(unsafe_code)]`, no I/O, clock or
randomness — the `lib/ssh` and `lib/net` shape. The host supplies received
bytes, the time, and CSPRNG output; the engine returns bytes to send, a folded
next deadline, and typed events. Every decoder is total, bounded and fuzzed.

The transport (SMB4):

- `wire`, `header`, `status` — the field codec, whose positions count from
  each message's own header so a body encodes the same alone or compounded;
  the 64-byte SYNC/ASYNC header and command set; NT status codes.
- `negotiate` — dialect selection within the floor, the 3.1.1 context set
  (pre-auth integrity, encryption, signing, net name), the acceptance policy,
  validate-negotiate.
- `session`, `tree`, `ioctl`, `simple` — the `SESSION_SETUP`, `TREE_CONNECT`
  and `IOCTL` codecs, the four-byte bodies, the error response.
- `crypt` — the pre-auth integrity hash, key derivation (SP800-108: 3.0/3.0.2
  labels, 3.1.1 labels over the pre-auth hash, AES-256 from the whole session
  key), AES-CMAC/AES-GMAC signing, and the transform sealed and opened in
  place. Each direction's nonces are a 64-bit counter from a random start, then
  random bytes, both drawn from the host's CSPRNG when the keys are derived; a
  sequence never repeats a nonce, and refuses to seal rather than wrap.
- `frame`, `connection` — Direct TCP framing, compounds, credits, the in-flight
  table keyed by `MessageId`, at most one interim response per request and the
  async id it names, cancellation, unsolicited breaks, and mandatory sealing:
  a session's keys are installed once, and a response to a sealed request is
  accepted only sealed under that session's keys and naming that session.
  Bodies are encoded straight into the frame they are sent in; a received
  frame is opened in place and a single message handed over in its buffer.

Authentication (SMB5):

- `ntlm` — NTLMSSP NEGOTIATE/CHALLENGE/AUTHENTICATE, NTLMv2 with the
  server's target info, `MsvAvTimestamp`, the MIC, `MsvAvTargetName =
  cifs/<host>`, the message signature mechListMIC needs. The user name is
  upper-cased as Windows and Samba do it (simple mapping, one UTF-16 unit for
  one); the password-derived key is wiped once the response is made.
- `spnego` — RFC 4178 negTokenInit/Resp over `lib/der`, one mechanism
  offered; mechListMIC.
- `client` — one principal's connection and its one session, over SPNEGO:
  it chains the pre-auth hash over exactly the messages it covers, derives
  the keys once, checks the final response's signature before installing
  them, never re-derives on re-authentication (so no nonce sequence restarts
  under a session's key), and for 3.0 and 3.0.2 holds every tree until
  validate-negotiate has passed once, failing closed on a mismatch or an
  error answer. A server refusing an authentication is an event and the
  connection stays usable for another attempt; anything else wrong ends it.
  `Mechanism` is the seam: NTLM now, Kerberos with KERBEROS K4. `send` puts
  requests on a handed-out tree. After a dropped connection, `authenticate`'s
  `previous` names the replaced session on every leg, so the server keeps its
  durable opens for the new one.
- `peer` (the `test-support` feature) — the scripted server the client's
  tests and fuzz harness drive it against, verifying what a real server
  would. Its disk share (`Share`) is held in memory and outlives a
  connection, with opens, leases and their breaks, watches, related
  compounds and durable opens; its `IPC$` serves `srvsvc`.

File operations (SMB6):

- `file` — the request and response codecs for create, close, read, write,
  flush, query directory, query/set info, change notify, lease breaks and
  their acknowledgment, each decoder total and bounded. Names cross only when
  both TAIRiX and SMB carry them (`name`: a TAIRiX component holding no
  character an SMB filename may not; a path is such names a single `\`
  apart, at most 32,760 units), and FILETIME ↔ `Time64` is exact or refused.
  Create carries a v2 lease, a v2 durable request or reconnect, maximal access
  and the on-disk id, its context list at most 16 contexts, aligned and named
  once; a granted lease and the lease oplock come together. Reads and writes
  move at most 1 MiB, borrowed from or written into their frames. A listing
  (`FileIdBothDirectoryInformation`, at most 256 KiB) and a change response
  (at most 64 KiB) are walked in place, a name neither side carries costing
  its entry or record. A break may arrive unsigned (MS-SMB2 §5.1) and only
  takes caching away. A durable open is reclaimed over a new connection with
  the same client GUID, by a reconnect carrying its lease key; the id the
  answer carries is what it is known by after.

Share enumeration (SMB7):

- `rpc` — connection-oriented DCE/RPC (C706): bind, acknowledgment,
  rejection, request, response and fault, version 5.0 in little-endian NDR,
  never authenticated; NDR for `NetrShareEnum` level 1. `ShareListing` is the
  exchange as requests on `IPC$`: open `srvsvc`, bind by write and read, call
  by `FSCTL_PIPE_TRANSCEIVE`, read on any further fragments, close — the pipe
  closed even when the listing fails. The bind must be accepted as offered,
  answers must carry the call's ids, fragments are at most 4280 bytes and an
  answer at most 256 KiB. It keeps disk shares only, the special ones when
  asked, and the first 256 of them — `netshare-v1`'s listing — counting a
  special share not asked for, a share whose name no tree connect takes
  (`netshare_ipc::is_share_name`: one to 80 characters, no separator or
  control), and a share past the 256; a remark is display text, a bad unit
  U+FFFD, cut at its last character within `netshare-v1`'s 1024 bytes.

Every module is held to Microsoft's published Windows traffic where it exists
(`docs/src/lib/smb.md`) and fuzzed by `tests/fuzz_smb.rs`.

## 3. The service (`userland/net/smbfs`)

A `service` bundle (`os.tairix.smbfs`) under `/System/Services/`, holding
`CAP_NET`, `CAP_FS_SERVE` (FSSERVE §6), `CAP_IPC_BIND_PRIVILEGED` (its
reserved endpoint, bound with a send capability), `CAP_SANDBOX_SPAWN` (its
workers) and `CAP_LOG_EMIT` (its audit), and nothing else. It is enrolled
without `requires=network-up`: a stack restart drops its connections, which
reconnect inside the grace window (SMB9e) rather than taking every mount
down.

- **`netshare-v1`** (`lib/abi/src/netshare_ipc.rs`, client `lib/netshare`):
  `Shares` (the endpoint, its pin when the credentials were remembered, the
  dialect floor and the credentials → the pin reported and the disk shares,
  special `$` shares only when asked, the first 256 listed and the rest
  counted), `Mount` (the same and the share,
  read-only) → the pin and an *offer* the requester attaches itself (FSSERVE
  §6), `Unmount`, `Mounts` (the caller's own). The presentation, the name and
  the target belong to the kernel's attach call, which judges them against the
  real caller; the Kerberos credential and `Forget` (drop the caller's cached
  tickets) join with KERBEROS K4. The caller's identity and login session come
  from the kernel-attested origin, never from the request. A request the
  service cannot read is answered `Unreadable`.
- **Admission.** The endpoint's send capability is `CAP_NET`, so the kernel
  refuses a caller without network authority before `smbfs` reads a byte:
  the service's work is network work and a request's names cross the wire (a
  lookup, NTLM's user and domain), so a program that cannot reach the network
  itself cannot probe a host or port, or carry data out, through it.
  `files.app` already holds `CAP_NET`; `mount` and `smbtree` declare it.
- **One served volume per mounted share**, served by the front itself rather
  than through `lib/fsserve`'s one-call-at-a-time runtime: every call waits on
  the network, and the kernel's lanes run concurrently, so each call is held
  and answered by one compound on the share's tree when its answer arrives.
  Its node ids keep the driver traits' contract (FSSERVE §0): an id names one
  node for as long as it keeps its name, held across reconnects by a
  path-keyed map whose paths compare as the share compares names (ASCII case
  folded), and answers `Stale` once another writer has removed or replaced
  it.
- **Caching follows leases.** Attributes and directory listings are cached
  under a directory lease where the server grants one and for one second
  otherwise (the CIFS `actimeo` default); file data is cached and written back
  only under a read/write lease, and every dirty byte is flushed before a lease
  break is acknowledged, on `fsync`, on close, and before unmount.
- **Reconnect.** A connection mounts depend on that drops — its socket closing,
  its worker ending or lying, or, while a call, a file held open or a directory
  lease rests on it, a silence an SMB2 `ECHO` goes unanswered through — is made
  again with the same client GUID and a session naming the one it replaces, its
  trees connected again and its held files' durable-v2 opens reclaimed or, an
  answer lost, replayed. Its mounts wait a grace window: the longest durable
  timeout the server granted their files, else MS-SMB2's 60 s. Past it they
  report `MountState::Lost` and every call fails at once, while attempts go on
  (backing off to a call's deadline) and a connection made again serves them
  again; the kernel's `UnavailableLost` is only FSSERVE's for a volume
  withdrawn holding unpublished writes. A server refusing the account or
  answering under another pin ends them instead.
- **Tickets.** Kerberos credentials are held per (uid, principal) in memory,
  zeroed on expiry, `Forget`, and the session's end; never written anywhere.
- **Audit.** Every mount, unmount, refusal, pin mismatch and authentication
  failure is a `lib/log` event in `smbfs`'s own range (35_000–36_000), naming
  the endpoint and resolved address, never a credential.

## 4. Commands

- `mount -t smb [-r] [-o OPTIONS] //HOST/SHARE [TARGET]`. Options: `user=`,
  `domain=`, `sec=auto|krb5|ntlmv2`, `credentials=FILE` (refused unless the
  file is the caller's and mode `0600`), `port=`, `vers=3.0|3.0.2|3.1.1`
  (raises the floor only), `name=`; administrators also `shared`, `uid=`,
  `gid=`, `file_mode=`, `dir_mode=`. A password is read from the terminal with
  echo off, as `elevate` reads one; there is no `password=`. With no operands
  `mount` lists mounts as today.
- `unmount NAME|PATH` detaches a served volume its caller owns, or any with
  `CAP_FS_MOUNT`.
- `smbtree [-a] [HOST…]` lists the catalogue's hosts, and a host's shares;
  omissions (per-source quota, hidden shares) are `omission` records on fd 3.

Each ships its `Help/` in every required locale and a `docs/src/userland/`
page (§16.7: `mount` keeps util-linux's grammar).

## 5. `files.app`

The Network view and its **Network** row (`AppMenuSymbol::Network`, directly
above *Settings…* in both the window and the desktop-component menus) are
ZEROCONF Z9 and keep every rule of ZEROCONF §6.5. What this plan adds to it:
the host → share → mount journey behind `Activation::MountEndpoint`; an
owner-modal credential window (user, domain, password in a `SecretField`,
*Remember*); a *Connect to Server…* window; and *Unmount* on a mounted share's
rail row and context menu.

## 6. Bounds (fixed, §24.4)

Server-supplied sizes are capped before use: one SMB message 8 MiB (the
sandbox frame), read/write 1 MiB per request, 64 credits in flight per
connection, a directory page 64 KiB and so at most 1024 entries, 255-byte
names after mapping, 256 shares listed per host and the rest counted. A principal holds
at most 32 mounts and
16 connections. Each bound fails closed with a typed error.

## 7. Tests

- Every decoder fuzzed (`cargo xtask fuzz` targets per message family).
- Published vectors: MS-NLMP §4.2.4 (NTLMv2), MS-SMB2's KDF, signing and
  encryption examples, RFC 3962/8009 (in KERBEROS.md).
- Engine against the in-tree server (SMB12) on the host: negotiation per
  dialect, downgrade attempts refused, tampered and replayed messages refused,
  pin mismatch, lease breaks with dirty data, reconnect, every bound.
- The QEMU vertical: discover the host peer, list its shares, mount one
  privately, write and read back through `/Storage/<name>`, prove another user
  is refused, unmount, and prove `/` never touched an interface to find it.

## 8. Performance

Measured, never assumed (§2.16): throughput of a large sequential read and
write and the latency of a cold and a warm directory listing, against the host
peer, reported in the crate docs. The socket ABI's 8 KiB events are the
current ceiling (NETWORK.md §2.4); that path is the stack's to widen.

## 9. Layout

```text
lib/smb/                 the pure SMB 3 engine (wire, negotiate, session, ntlm, spnego, file, rpc)
lib/der/                 the shared DER codec
lib/netshare/            the netshare-v1 client
lib/abi/src/netshare_ipc.rs   the netshare-v1 ABI
userland/net/smbfs/      the service: front + workers
userland/apps/mount/     the mount bundle
userland/apps/smbtree/   the browse command
tests/smbpeer/           the in-tree SMB 3 server and KDC test peers
```

## 10. Stages

**SMB1 — display safety.** One policy for showing text a stranger authored,
`lib/visible`'s (`Shown`, lazily `shown_chars`; `tairix_util::utf8::shown` is
the allocating form), over the character properties of `lib/unicode`,
generated from a pinned Unicode 18.0.0 UCD snapshot by `cargo xtask unicode`:

- every `is_hidden` character — C0/C1 controls, line and paragraph
  separators, interlinear-annotation controls, and every
  `Default_Ignorable_Code_Point` — shown as U+FFFD, never as a gap a real
  space would look like;
- a combining sequence cut at one U+FFFD where it would carry more than four
  nonspacing marks or a mark an earlier character of it already carries,
  judged over canonical decompositions (UTS #39 §5.4, `MarkRun`);
- `mixes_scripts`, UTS #39's minimally restrictive level, for a surface to
  mark a name whose scripts could impersonate another (Z9 does).

The length bound is each surface's layout budget — the file manager's
middle cut that keeps an extension, the title bar's 512-character keep, a
protocol's decode bound — and is always drawn as `…`: truncation is where
two long names may coincide, so it must show. The file manager's names and
titles (`lib/browse`), the title bar, the Switchboard and the TUI layers
(`lib/vt`'s print, `lib/curses`' cells) apply it; the renderings that escape
rather than replace — the log renderer, the report escaper, GNU quoting and
DNS display labels — escape what its lossless twin `Faithful` names, each in
its own spelling; the JSON log form carries text as data. A FAT32 test
follows a hostile long name from the volume to its shown form.

**SMB2 — crypto.** The audited RustCrypto `md4`, `md-5`, `cmac`, `ccm` and
`cts` crates, pinned. The broken digests are reachable only through
`tairix_crypto::ntlm`'s constructions (`ntowfv2`, `ntlm_hmac`,
`ntlm_signing_key`, `ntlm_checksum`), never as a digest. The strong
standard ones join their modules: AES-CMAC and AES-GMAC in `mac`, AES-CCM in
`aead`, AES-CTS in `cipher`, PBKDF2-HMAC-SHA1 and the SP 800-108 counter
KDF in `kdf`, HMAC-SHA384 for RFC 8009. Published vectors and the
constant-time gate.

**SMB3 — `lib/der`.** Definite-length DER only, depth and length bounded,
tag-checked readers and a writer; fuzzed.

**SMB4–SMB7 — the engine**, each with its tests and fuzz targets as §2 lists.

**SMB8 — `netshare-v1`.** The ABI and its fail-closed, canonical codec
(`netshare_ipc`, with `Op::max_reply` bounding each operation's reply and
`netattach::OfferId` naming an offer), the reserved endpoint, the share-name rule
and listing bounds `lib/smb` takes from it, the `Secret` a password crosses
as, the `lib/netshare` client (each request built in an erased buffer, each
answer accepted only for its request, one wording per reason) and the decoders
fuzzed in `lib/abi/tests/fuzz_decode.rs`.

**SMB9a — network attachments.** FSSERVE FSS6 as its §6 states, and
NEW-NAMESPACE NS-4a's `view_bind` and `view_unbind`, so a mount may be placed
at an explicit target from the start.

**SMB9b — the service.** §1 and §3's front, workers, connections, listing,
mount and unmount, session-bound lifetime and audit, over FSS6.

**SMB9c — the served filesystem.** §3's served filesystem, uncached: every
operation the share carries reaches the server as one compound — open, act,
close — calls in flight concurrently across the kernel's lanes, and a listing
keeps its directory open between pages. Links, punched holes, security
records and named attributes are unsupported; ownership is unrecorded.

**SMB9d — caching.** A file the kernel holds open is opened on the server
under a read/handle/write lease and kept open until its last hold goes, its
calls run in order through one step queue. Reads fill 64 KiB pages in the
process's clean-file-data reclaim cache, keyed by the lease's generation.
Writes are held in 4 KiB blocks that record which of their bytes are held, in
a radix index per file, and go out as stretches of at most 64 KiB, eight in
flight per file: by the one-second window, for the bound, before a truncate, a
sync, a close or a break's acknowledgment, and before an unmount or the
session's end, which wait at most a call's deadline. What the blocks and
index pin is held to `tairix_writeback::WritebackBound` over the service's one
`PinnedShare`; a volume holding nothing always admits one write, and nothing
is held at critical pressure. Each held file's next deadline — its window, or
a call's deadline without an answer — is one entry in its volume's indexed
heap. A short write's rest goes again; a write taking nothing, refused, or
given up is lost: logged, owed to the volume's next flush, and its file served
uncached until it is opened again. The front's own removes and renames of a
held file carry its lease key; removing an open file renames it aside and
removes that; a rename over a held file waits for its close. Attributes and
whole listings of up to 4096 entries stand a second in file-metadata caches,
or while the directory lease the listing's open took holds: at most 16 per
volume, the least recently used let go first, and each let go after 30 s
unused (CIFS's `dir_cache_timeout`).

**SMB9e — reconnect.** A drop is detected from the socket, the worker (a lie,
or its end, which the link reports), or — while a call, a file held open or
a directory lease rests on the connection, so an idle mount takes no wakeups —
ten seconds of silence an `ECHO` outside any session then goes ten more
unanswered. The connection keeps its client GUID and established session id
across attempts; on a drop each volume is suspended — idempotent calls kept to
be asked again, a create, remove or rename in flight answered `DeviceOffline`,
listings set to restart past what was handed out, directory leases dropped —
and each held file keeps the step the drop interrupted and is marked to reclaim
(DH2C), or to replay its ask (DH2Q under its create GUID with
`SMB2_FLAGS_REPLAY_OPERATION`), once the trees are connected again
(`Phase::Rejoining`). A refused reclaim falls back to a fresh open; a refused
replay leaves the file reached by path, its bytes lost. Calls wait while the
volume is suspended and fail at once past the window, when held bytes are lost
and owed to the next flush. Attempts back off from one second, doubling to a
call's deadline, and a connection that drops within that of recovering waits as
a failed attempt. `lib/smb` provides `Client::echo`, and its peer an
unsessioned `ECHO`, replay detection, durable timeouts (`Share::advance`),
`Share::restart` and `PeerConfig::server_guid`.

**SMB10 — the commands**, with their help and docs.

**SMB11 — DFS.** `STATUS_PATH_NOT_COVERED` answered with
`FSCTL_DFS_GET_REFERRALS`, the referral followed to its target under the same
pin and credential rules; a referral naming a host the user did not choose is
presented for confirmation, never followed silently.

**SMB12 — test peers.** A host-side SMB 3 server over a directory and a KDC,
both first-party Rust under `tests/smbpeer/`, used by the engine tests and run
by `cargo xtask` beside QEMU for the vertical. The server grows from
`lib/smb`'s scripted `peer` rather than beside it, so the protocol's server
side is written once.
