# `tairix-sysinfod` — System Information service

Stage 6 deliverable (`AGENTS.md` §16.6). The user-space service that
answers the `sysinfo-v1` API defined in `tairix_abi::sysinfo`. Installed
to `/System/Services/sysinfod.app/Run`.

TAIRiX has **no `/proc` and no `/sys`**. Every piece of live system
information those trees would have exposed is served here, through typed,
versioned, capability-checked queries — never through a virtual
filesystem (`AGENTS.md` §16.1). `sysinfod` is the only server of the API
and the kernel exposes no path that bypasses it.

## What this crate is

The **dispatcher** — the policy layer. It owns no data of its own. For
each request `serve` performs, in order, failing closed at the first
problem (`AGENTS.md` §5.4):

1. Decode the `SysinfoRequestHeader` and any typed payload.
2. Look the query up in the frozen registry (`SYSINFO_QUERIES`).
3. Enforce the query's declared capability against the caller's
   `CapabilityQuery` view **before** touching any state.
4. Emit a `lib/log` audit record for every invocation of an audited
   query, and for every capability denial.
5. Page and encode the answer supplied by the injected `SysinfoSource`.

The live data is read through the `SysinfoSource` seam, so the
security-relevant dispatch code is independent of any kernel plumbing
and is fully testable against an in-memory fixture.

## Queries served (`sysinfo-v1`)

The served queries, the capability each requires, whether it is audited, and
its response shape are tabled once, in `docs/src/userland/sysinfod.md`; the
registry itself is `SYSINFO_QUERIES` in `lib/abi/src/sysinfo.rs`.

`MEMORY_PRESSURE_BAND` and `MEMORY_TOTAL` are the two ungated,
unaudited self-regulation reads: the published pressure band, and the
machine's total usable physical RAM in bytes. Both are deliberately the
smallest useful answer, so a process can shrink its own caches and size
them against the real machine without holding `CAP_SYSINFO_KERNEL`. The
total is the same figure `KERNEL_MEMORY_STATS` reports as
`KernelMemoryStats::total_bytes` — the kernel derives both from one
usable-frame census — and a zero answer means *unknown*, which admits
nothing. The detailed `MEMORY_PRESSURE` view stays gated and audited.

## The two submissions

Two figures cannot be measured from outside the process that holds them: a
userland cache's ledger, and a compositor's frame accounting. Both are
therefore submitted rather than read — `CACHE_REPORT` and
`DESKTOP_FRAME_REPORT` — and retained here, in `SelfReports`, never in the
kernel: the kernel's own per-class reclaim sum gates a real decision, and
keeping self-reported figures on this side of the syscall boundary makes it
structurally impossible for a process to steer it.

Both are ungated and unaudited, because a process describing itself grants
nothing and reads nothing — and auditing them would hand every process a way
to write the hash-chained journal. Both share one table type, so the keying
(the caller's unforgeable process instance), the RAM-derived capacity, the
liveness sweep before a new reporter is admitted, and the refusal of a
kernel-domain principal are written once. The matching reads —
`CACHE_LEDGERS` and `DESKTOP_FRAME_STATS` — are gated and audited as their
capability implies. See `docs/src/userland/sysinfod.md`.

## Response encoding

There is no response envelope: the typed payload *is* the response.

- List queries pack zero or more fixed-size records back-to-back. The
  caller pages with the request's `offset`/`limit` and detects the end of
  the list when it receives fewer than `limit` records. Every page of one
  walk names the same `walk` and is answered from one reading of the list,
  held by `Walks` until the walk's short page; a walk let go of is
  answered `Interrupted`. Paging bounds live in the dispatcher (one
  place), not in the source.
- Scalar queries return the little-endian wire image of their struct.
- The hardware-tree query checks the source's snapshot against its own
  header and answers that header followed by the requested page of whole
  nodes; its generation, not a held walk, tells a walk the tree changed.

## `SysinfoSource` seam

`init` injects a `SysinfoSource` when it starts the service. On a running
kernel this is a thin shim over the kernel's process table, memory
accounting, hardware tree, and identity; in tests it is an in-memory
fixture. The `Caller` passed to every method carries the kernel-provided
`uid` and capability view — never caller-supplied bytes (`AGENTS.md`
§5.4.1). `ProcessScope` (`Caller` vs `Global`) is chosen by the
dispatcher from the query id, so a self-scoped request can never widen
into a global one without the global query's capability gate.

## Audit events

Reserved `EventId` range `8000..9000`:

- `8001 QUERY_SERVED` — an audited query was invoked (Debug: a polling
  monitor emits it continuously, so it sits below the default `Info`
  filter; lower the filter to capture it).
- `8002 QUERY_DENIED` — capability check failed (Warn).
- `8003 REQUEST_MALFORMED` — header/payload decode failed (Warn).
- `8004 QUERY_UNAVAILABLE` — reserved-but-unassigned query id (Warn).

## Layering & safety

`no_std`, depends only on `lib/*` crates, so a userland service never
links a kernel or driver crate (`AGENTS.md` §17.4). No `unsafe`, no `unwrap`/`expect`/`panic!` in production paths
(`AGENTS.md` §2.9).

## Test surface

`cargo test -p tairix-sysinfod`:

- self-scoped list needs no capability and is not audited;
- paging by `offset`/`limit`, and an empty page past the end;
- a walk reading its list once, neither skipping nor repeating a record of
  a list that changes under it, and a page of a walk not held answered
  `Interrupted`; the walk store's per-caller share and byte budget;
- reading a kernel list whole from one reading, and refusing a partial
  record, a peer page past its bounds, and a peer list that never ends;
- global list denied without `CAP_SYSINFO_GLOBAL` (with the denial
  audit record);
- an audited query emitting exactly one `QUERY_SERVED` record;
- the hardware-tree pass-through, gated by `CAP_SYSINFO_HW`;
- the ungated scalar queries (`UPTIME`, `SYSTEM_IDENTITY`) round-tripping
  with no audit record;
- malformed header, truncated payload, unassigned query id, and an
  undersized response buffer all failing closed;
- the `EventId` range/uniqueness invariants.
