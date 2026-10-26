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
`Run` binary reports diagnostics through (best-effort, never the data stream):
formatted on the stack and written with its line ending in one write where it
fits the buffer, so the line-to-fd-2 loop is written once and a line built from
parts costs one syscall. `Stdout` and `Stderr` answer `at_terminal` from
whether the kernel attests the console's size, so a pipe, a file or a UART is
no terminal.

`Stdout` and `Stderr` also implement `tairix_util::output::Output`, the byte
stream every command engine writes through, so a `Run` binary hands its engine
the standard streams themselves rather than a wrapper of its own. A refused
write keeps the kernel's own `Errno` (`Error::as_errno`), and an advisory record
sent to either stream goes to fd 3. Where `Write` and `Output` are both in
scope, `stream.write_all` resolves to `Output`'s by-reference method, so a `Run`
binary that writes through `Write` does not import `Output`.

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

## Creating a file or directory

A node is born with the mode its creator names, less the process's file mode
creation mask, which the kernel applies on every create. `File::open` opens what
exists and refuses flags carrying `CREATE` before any call; `File::create(path,
flags, mode)` creates (an existing file keeps its own mode), as do `create` and
`create_new` with a mode, `fs_mkdir(path, mode)` for a directory, and
`replace_file`, whose mode is a first creation's (a replaced file keeps the
original's, and its temporary is born owner-only). A creator that wants no
narrower mode passes `tairix_abi::fs::FS_NEW_FILE_MODE` (`0o666`) or
`FS_NEW_DIR_MODE` (`0o777`), which the initial mask `FS_INITIAL_UMASK` (`022`)
narrows to `0o644` and `0o755`.
`umask(mask)` replaces the mask, keeping its permission bits, and answers the
one it replaced; `umask_get()` reads it. A child launched afterwards starts
with the same mask.

`Dir::create(path, mode)` and `Dir::make_dir(name, mode)` make a directory
with `CREATE | DIRECTORY | EXCLUSIVE` and answer it open with no read or
write access, so a mode set next through it (`dir.as_file().set_mode`) reaches
the directory made, whatever its name holds by then — the race between a
`mkdir` and a later `chmod` of the name is closed, which `mkdir -m` relies on.

## Paths, directories and descriptors

Every path-only function — `fs_open`, `fs_mkdir`, `fs_unlink`, `fs_rename`,
`fs_chdir`, `fs_set_mode`, `fs_set_owner`, `fs_symlink`, `fs_readlink`,
`fs_link`, `fs_realpath` and the openers built on them — passes `FD_CWD`, so a
relative path starts at the working directory. The working directory holds the
directory itself rather than a spelling of it: it follows the directory through
a rename, and a removed one answers `NotFound`.

A `Dir` is a place relative calls start from. `open_dir`, `open_file`,
`create_file`, `make_dir`, `mkdir`, `unlink`, `rename`, `link`, `symlink`,
`readlink`, `set_mode_at`, `set_owner_at` and `access` resolve a relative name
from the directory the handle holds, wherever it has been moved, and an
absolute one exactly as the free functions do; `chdir` makes it the working
directory. `open_dir` opens the child directory itself, never through a link
at its name, which answers `NotADirectory`. Each runs as the caller: a
directory descriptor confers no authority. `rename` and `link` name each end
by its own directory, carried to
the kernel in a `PathAt` record. `unlink` and `rename` take an optional
`NodeExpect`: with an `EXPECT_*` flag the kernel compares it under the volume's
lock and answers `Stale`, changing nothing, when the name no longer holds the
node it describes.

A descriptor's own node is reached through `EMPTY_PATH` and an empty path:
`File::set_mode`, `File::set_owner` and `Dir::chdir` are `fchmod`, `fchown`
and `fchdir`, and `File::access` asks about the node held under the
descriptor's authority. Extended attributes are reached
through a descriptor only — `File::attr_keys`, `attr_value`, `attr_set` and
`attr_remove` — so a caller meaning a link's own attributes opens it
`NO_FOLLOW`.

`tairix_rt::copy` puts the shared tree seam (`tairix_fscopy::tree`) over
these handles: a `File` is a held node, a `Dir` a held directory whose
children are reached by name and opened with `NO_FOLLOW` — through a final
link only by `enter` and `follow` — and `WorkingDirectory` the `Roots` a
tool's operands are opened from. A listing is decoded a batch at a time
into the entries it names, so it is never held twice. Every command tool
that walks or changes a tree runs through it, and so does `replace_file`.

`access(path, want, flags)` asks whether the caller may read, write or run a
node (`Access::READ`, `WRITE`, `EXEC`; empty asks only that it exists),
answered as the caller: the node's mode, ACL and capability gate under the
caller's identity, then the mount — writing on a read-only one is `ReadOnly`,
running a file on a `noexec` one `PermissionDenied`. `NO_FOLLOW` asks about a
link itself. The answer describes the moment it was given: it decides what a
program asks or shows, as `rm` decides whether to ask before removing a file it
may not write, and never stands in for the check the kernel makes again on the
call that follows.

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
`-LimitExceeded`. `Dir::for_each_batch` walks a listing it never holds whole,
every batch read into one caller buffer and handed over in turn under the same
end rule, until the caller stops it.

## Following one file

A service that applies a document as it changes — the time service and the
cooling service each follow `system.conf` — follows it through
`follow::Followed`. The file's directory is watched once it exists, and its
parent until then. While neither can be reached — absent, or on a volume no
driver serves yet, which answers `NotImplemented` — the mount table's notice
stands in, joined before the look so a mount between the two still wakes the
service, and left once the directory is watched; so a service started before
the root is unlocked follows the file once it is, however long that takes. A
directory that is there but refused, or is not a directory, is answered as
unwatched with why, never waited for, and so is one nothing could wake the
service for. A storm of changes reports at most once a second, the
engine's own pacing. `Followed::drain`
answers what a wake asks for: nothing, a re-read, or a re-follow because the
watched directory came or went; a directory still changing after a bounded
number of batches is simply read again. The verdict over one change batch
(`follow::verdict`) is pure and host-tested.

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
