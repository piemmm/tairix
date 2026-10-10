# `tairix-rt::io` — userland I/O abstraction

`tairix_rt::io` is TAIRiX's ergonomic userland I/O layer: the counterpart of
`std::io` for a first-party Rust program. It is a **pure-Rust convenience layer**
over the existing `abi-v1` `stream_read` / `stream_write` traps — it adds **no**
syscall, capability, or `lib/abi` type, and reaches no authority a program does
not already hold. An I/O object only ever names a descriptor the kernel already
gave the process; it never names a device (the standard-stream rule).

The staged plan is `plans/IO.md`. Stability tier: **experimental**
(`abi-v1` is not frozen; the surface grows as callers need it).

## One vocabulary, no duplication

There is exactly one `Read` / `Write` definition for the whole of userland, so
no program re-implements the short-write loop or "read until newline" logic
(`AGENTS.md` §2.2):

- `Read` provides the primitive `read`, the transfer loop `read_fill` (read
  until the buffer is full or the input ends, reporting how much arrived), and
  `read_exact` on top of it.
- `Write` provides the primitive `write`, the transfer loop `write_drain` (write
  until the buffer is drained or the sink stalls, reporting how much was taken),
  `write_all` on top of it, a `flush`, and `write_fmt` (so `write!` / `writeln!`
  work), rendering through a fixed adapter that surfaces a formatting failure as
  a typed `Error::Fmt` rather than a panic.

`read_fill` and `write_drain` are **the** two transfer loops in userland.
`read_exact`, `write_all`, `File`'s positional `read_at` / `write_at`, and their
raw-descriptor spellings `fs_read_full` / `fs_write_all` are all expressed over
them rather than carrying their own copy, so a short-read or short-write bug can
only exist in one place.

Every fd backing shares this one vocabulary. The four standard streams
(`Stdin`, `Stdout`, `Stderr`, `StdInfo`), a `Stream` over an arbitrary
descriptor, and an owning `File` — whether it is a path, a resource reference, a
pipe end, or a pty end — all go through the **identical** code path: the shared
`stream_read` / `stream_write` primitives. There is no separate "file I/O"
trait, because the kernel resolves every descriptor the process holds through
one table.
The module also carries the one `write_stderr_line` helper every command app's
`Run` binary reports diagnostics through (best-effort, never the data stream),
so the line-to-fd-2 loop is written once.

## fd-generic: one borrowed view, one owning handle

`Stream::new` views a descriptor the process already owns (a standard stream, or
a file / pipe / tty / resource-reference fd a spawner wired in or a subsystem
opened). It is **borrowed** and does not close the descriptor.

`File` is the **owning** handle and releases its descriptor on drop, whatever
the backing — the close trap is descriptor-generic, so one owner type covers
paths, resource references, pipe ends, and pty ends alike. A second owning fd
type alongside it would be the duplication this layer exists to prevent, so
there is no `OwnedStream`.

Obtaining a *new* fd — opening a file under a capability, resolving a resource
reference, creating a pipe — is owned by the filesystem and resource-reference
subsystems, not this layer; the trait module exposes no `open` / `resolve` and
so cannot widen authority.

## Sequential and positional

`File`'s `Read` / `Write` are **sequential**: they transfer at the shared
open-file-description cursor and advance it, so successive reads walk the file
and two descriptors cloned from one description (a spawn wire, a delegation)
interleave at one position instead of overwriting each other. This is the same
`stream_read` / `stream_write` trap the standard streams use, which is why a
file, a pipe, and a terminal are indistinguishable to a program that just wants
bytes.

`File::read_at` / `write_at` are **positional**: they take an explicit byte
offset and leave the shared cursor untouched, so two positional callers of one
description never contend over a position. The kernel serves both from a single
descriptor I/O path parameterised only by where the position comes from, so the
direction gate, capability checks, and copy boundaries cannot drift between
them.

## Whole-document reads

A consumer that wants a whole file rather than a stream — a settings document, a
program catalog, a wallpaper master — calls `read_fd_to_end(fd, cap)`, or
`read_path_to_end(path, cap)` to open it read-only first. It is the one
whole-file policy in the tree, and it exists so that how a document is read is
decided once. The `cap` is the bound of the format being read, named by the
caller, so no reader shares one bound across files of different kinds:

- The size the descriptor states (`fs_stat`) reserves the answer once, and every
  read lands straight in it, asking for as much as one syscall moves
  (`FS_IO_MAX`). A multi-megabyte document therefore costs a handful of traps
  and no copy beyond the kernel's own — no staging buffer and no copy out of
  one — and, while the stated size holds, no reallocation.
- The size is only a hint: the read ends on end-of-file, so a file that changed
  under it is still read whole, and a size too large to reserve starts the read
  at `FILE_STREAM_CHUNK` (64 KiB) instead of failing it. Past the stated size,
  or with none stated, the answer doubles from at least that much — never past
  one byte beyond `cap` — so a long file costs few reallocations.
- Each byte of the answer is zeroed once, however short the reads, and a read
  reporting more than it was handed is refused (`-OutOfRange`) rather than its
  claim kept as data.
- It answers *one byte past* `cap` rather than truncating at it, so a caller can
  tell an oversize document from one that exactly fits: a length above `cap` is
  the whole-document refusal to state, never a silently shortened answer the
  caller would go on to parse. No read asks for more than that one byte.
- A refused read surfaces the kernel's `-errno` unchanged, and an answer that
  cannot be reserved is `-OutOfMemory` rather than an abort.

Directory listings have their own shared policy. `Dir::read` is one batch
from the open description's listing position; `Dir::read_all` and
`read_dir_all` join `READDIR_BATCH_MAX` batches from the start
(`read_batches`). The kernel fills a batch until the next record does not fit,
so a batch with room for the longest record ends the listing without a further
call. `Dir::read_all_within` refuses a listing past a caller's byte bound with
`-LimitExceeded`.

## Buffering

- `BufWriter` coalesces many small writes into a single underlying write. Its
  buffer is a fixed-capacity inline array (no heap allocation), flushed when
  full, on an explicit `flush`, and best-effort on drop. A write at least as
  large as the buffer bypasses it and goes straight through, untorn.
- `BufReader` buffers reads and offers line-oriented reading for a REPL:
  `read_until`, `read_line`, and a `lines` iterator that strips the trailing
  `\n` (and a preceding `\r`).

The buffer capacity is a const generic (`CAP`) defaulting to
`DEFAULT_BUF_CAPACITY` (4096 bytes).

## `stdinfo` (fd 3) semantics

`StdInfo`'s `Write` is best-effort: fd 3 is optional and ignorable (there may be
no consumer), so it reports the buffer fully consumed regardless of how many
bytes the kernel accepted. It never surfaces a short write that could stall
`write_all` or an error a program depends on (`AGENTS.md` §20.1). The structured
`StdInfoRecord` framing itself lives in `lib/abi`; this layer only carries the
bytes.

## Fail closed, fail loud

No path panics or uses `unwrap` / `expect`. A short read or write is looped over
by the provided helpers, `write_all` fails closed with `Error::WriteZero` if a
sink stops accepting bytes (never an infinite loop), and `read_exact` fails
closed with `Error::UnexpectedEof`.

A **kernel refusal is never disguised as end-of-input.** `Error::Os` carries the
kernel's own `Errno` — a descriptor that is not open in the requested direction,
a missing capability, a broken pipe, a faulted buffer, an elapsed read bound —
so `Ok(0)` from a read means end-of-input and nothing else. This matters because
the universal shape of a consumer is "read until it returns zero": folding a
failure into a zero-length read would make a revoked capability look like a
complete input and let the consumer silently truncate what it processed, which
is precisely the quiet, wrong-answer failure the charter's fail-loud rule
forbids. `Error::as_errno` converts back for an interface that speaks the
kernel's vocabulary, keeping the kernel's code when there is one; a condition
this layer raised on its own reports `NotImplemented` rather than borrowing an
unrelated code that would misdescribe the kernel.

`Stream::read_timeout` / `Stdin::read_timeout` are the bounded companions of
`read`, so a full-screen program parks on its input and still refreshes on a
cadence instead of busy-polling; an elapsed bound arrives as
`Error::Os(Errno::TimedOut)` and is therefore distinguishable from a dead
console.

## The keyboard beside other wakes

A console-backed standard input cannot join a wait-set, so a program that must
wait on its keyboard and on anything else at once — a connection, an audio
stream — starts `keys::KeyRelay`. Its detached thread blocks in a read of
standard input and posts what arrived to a private mailbox the program's
wait-set watches; `take` hands back `Keys::Typed` or `Keys::Ended`, and
believes a message only from this process. Refused for want of the
foreground, the reader parks on the terminal's foreground edge and reads again
when the hands next change, so a program sent to the background keeps running
and takes its keyboard back when it returns. A full mailbox is waited on, not
polled. The input discipline is set before the relay starts, since a keystroke
read under the cooked one is already echoed.

`stop_self` stops the process as a terminal's suspend key would, returning once
it is continued: a full-screen program receives that key as a byte while its
input is raw, gives its terminal back, and then stops itself.

## Not a log path, not a C `stdio`

Structured and audited log *records* travel through `lib/log`, never these
traits; a `log`-viewing tool renders its text to the standard streams through
this layer like any other program. TAIRiX ships no system-wide C `stdio`; the
C-ABI runtime class stays minimal, and a third-party C program brings its own
libc in its app bundle.
