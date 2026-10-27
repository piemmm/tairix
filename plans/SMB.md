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
| SMB8 | `netshare-v1`, the client protocol to `smbfs`, and the `lib/netshare` client `files.app` and the commands share | planned |
| SMB9 | `userland/net/smbfs`: the socket-owning front, the sandboxed engine workers, one served filesystem per mounted share, the caches, credentials and tickets, endpoint pinning, session-bound lifetime, audit | planned |
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

- **One worker per server connection**, a `lib/sandbox` supervised session.
  The worker holds that connection's keys and every piece of parsed server
  state; the front holds sockets, sends ciphertext the worker produced, and
  hands the worker the bytes it read, unread. A crashed worker costs its own
  connection, never the service.
- **Connections are shared per (principal, endpoint pin)**: listing shares,
  then mounting two of them, is one TCP connection and one session.
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
  answer at most 256 KiB and 256 shares. A share whose name no tree connect
  takes (`tree::is_share_name`: one to 80 characters, no separator or control)
  is left out and counted; a remark is display text, a bad unit U+FFFD.

Every module is held to Microsoft's published Windows traffic where it exists
(`docs/src/lib/smb.md`) and fuzzed by `tests/fuzz_smb.rs`.

## 3. The service (`userland/net/smbfs`)

A `service` bundle (`os.tairix.smbfs`) under `/System/Services/`, holding
`CAP_NET` and `CAP_FS_SERVE` (FSSERVE §6) and nothing else.

- **`netshare-v1` requests**: `Shares` (a host pin and credentials → the disk
  shares, special `$` shares only when asked), `Mount` (pin, share,
  credentials, presentation, name hint, optional target) → an *offer* the
  requester attaches itself (FSSERVE §6), `Unmount`, `Mounts` (the caller's
  own), `Forget` (drop the caller's cached tickets). The caller's identity and
  login session come from the kernel-attested origin, never from the request.
- **One served filesystem per mounted share** (`lib/fsserve`), its node
  handles keeping the driver traits' contract (FSSERVE §0): an id names one
  node for as long as it keeps its name, held across reconnects by a
  path-keyed map, and answers `Stale` once another writer has removed or
  replaced it.
- **Caching follows leases.** Attributes and directory listings are cached
  under a directory lease where the server grants one and for one second
  otherwise (the CIFS `actimeo` default); file data is cached and written back
  only under a read/write lease, and every dirty byte is flushed before a lease
  break is acknowledged, on `fsync`, on close, and before unmount.
- **Reconnect.** A dropped connection is re-established with durable-v2
  handles and the replay flag, inside FIX-IO's grace window; past it the mount
  reports `UnavailableLost` and every operation answers the device error.
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
connection, 4096 entries per directory snapshot page, 255-byte names after
mapping, 256 shares listed per host. A principal holds at most 32 mounts and
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

**SMB8 — `netshare-v1`.** The ABI, its codec, the client, and the fuzz target
`smbfs` decodes it with.

**SMB9 — `smbfs`.** As §3, over FSSERVE and SMB4–SMB8.

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
