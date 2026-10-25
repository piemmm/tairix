# `tairix-du` — estimate file space usage

A `plans/APPS.md` command app (`AGENTS.md` §3 `userland/apps/`),
registered as the system command store bundle `du.app` so the shell resolves
the bare word `du` to it. `du` walks each of its path operands and
reports, per directory (post-order), the on-disk storage its tree
occupies; with no operand it walks the current directory (`.`). The
option surface follows GNU coreutils (`AGENTS.md` §16.7): `-a` adds a
row per file, `-s` reports only the operands, `-c` appends a grand
total, `-d` bounds the reported depth, `-S` excludes subdirectories from
a directory's own row, `-l` counts a multiply-named file once per name,
`-P` (the default) counts a link as itself while `-H`/`-D` follow the
operands' links and `-L` every link, `--apparent-size`/`-b` measure apparent byte
lengths, `-k`/`-m`/`-h`/`--si`/`-B <size>` select the reporting scale,
and `-0` NUL-terminates rows. `-?`/`--help` render the tool's own short
help from its bundled `Help/` tree through the shared `lib/help` engine
(`plans/APPS.md` §4).

The default measure is each node's **allocated** on-disk bytes (the
`fs_stat` `allocated` field the mounted format reports), so sparse or
compressed files report what they really occupy; block counts round up
through the shared GNU size vocabulary in `lib/util`
(`tairix_util::size`), the same definition `df` renders with.

A node met a second time — another name for a file, a repeated or nested
operand, a node `-L` reaches twice — is left out with no row, as the GNU
tool does, and `-l`/`--count-links` opts out. The key is the node
identity every `fs_readdir` and `fs_stat` record carries, hashed under
the process key since a hostile volume chooses it. Only what can be met
twice is remembered: every node under `-L` or with more than one operand,
otherwise just a file whose name count exceeds one, because one operand's
tree reaches every other node once — so a single operand's seen-set holds
the hard links the walk actually meets rather than one entry per node on
the volume (`AGENTS.md` §24.1, §26.6). Under `-L` a link back to a
directory the walk is inside is never entered, even under `-l`. The set
grows on demand from no fixed ceiling; a heap that refuses to grow it
makes the walk count every sighting and report that on standard error
(exit `1`), never a silently wrong total.
Documented divergences from GNU `du`: there are no device ids, so
`-x`/`--one-file-system` is staged behind that kernel work rather than
stubbed; and the `DU_BLOCK_SIZE`-family environment
variables are not read — the scale is selected by options alone.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9). Its
dependencies are the audited `tairix-abi` vocabulary, the shared
`tairix-help` engine, the shared `tairix-util` size vocabulary, the
`tairix-path` grammar, and the keyed set of `tairix-collections` and
`tairix-hash`, so it never links a kernel or driver crate. Its
manifest requests `CAP_CONSOLE_WRITE` plus `CAP_FS_ACCESS` — within the
session baseline — and the secured VFS authorises every path per-inode
under the caller's attested identity. An unreachable path is diagnosed
on standard error and the walk continues (exit `1`), never a partial
guess; the walk uses an explicit frame stack, so a deep tree cannot
exhaust the call stack.

## Usage

```
du [-a | -s] [-clS0] [-h | -k | -m | -b | --si | -B <size>]
   [--apparent-size] [-d <n>] [--] [file...]
```

## Layout

* `src/lib.rs` — crate front matter and the module map.
* `src/command.rs` — the option grammar and its parser.
* `src/io.rs` — the `Walk`/`Output` seams.
* `src/client.rs` — the iterative post-order walk and row rendering.
* `src/run.rs` — the freestanding `Run` binary (host stub elsewhere).
* `Help/<locale>/du.md` — the bundled help documents (thirteen locales),
  the single help source (`plans/APPS.md` §6.1).
