# `tairix-ln` — create links between files

A `plans/SYMLINKS.md` S4/S6 command app, shipped as the self-contained store
bundle `/System/Commands/ln.app/` so the shell resolves the bare word `ln` to
it. `ln` is the GNU coreutils tool: it creates a link naming a target, in
every operand shape the GNU tool accepts — `ln target`,
`ln target link_name`, `ln target... directory`, and `-t`/`-T` — with `-s`,
`-L`, `-P`, `-d`/`-F`, `-f`, `-i`, `-n`, `-v`, and `--`. `-h`/`-?`/`--help`
render the tool's own short help from its bundled `Help/` tree through the
shared `lib/help` engine, in the locale the inherited `LANG` variable names.

## Both kinds of link

Without `-s` the link is a **hard** one (`fs_link`): a second directory entry
for the target's own inode, so both names reach one file and its storage
survives until the last name goes. `-L` gives the second name to what a
symbolic target *names*; `-P`, the default, links the target as spelled and
follows no final link — POSIX `link()` against `linkat(AT_SYMLINK_FOLLOW)`.
With `-s` the link is a **symbolic** one (`fs_symlink`), whose target is
stored verbatim and never resolved.

`-d`/`-F` accept a directory operand, matching the GNU tool, but the link is
still refused with `IsADirectory`: no principal may give a directory a second
name, because the tree staying a tree is what makes the resolver's physical
`..` well-defined. Both names must also lie on one volume — a directory entry
addresses an inode in its own backing — so a pair that crosses one is
`CrossVolume`.

`-r`/`--relative` stores a symbolic link's target relative to the link's own
directory. It asks the **kernel** to canonicalise both halves first
(`fs_realpath`) and then spells the difference: two canonical paths hold no
`..` and no link, so the arithmetic is exact. Doing it on the operands as
typed would be the lexical-`..` collapse the resolver forbids
(`plans/SYMLINKS.md` decision 4) — it would name a different node the moment
a link were involved. `-r` without `-s` is a usage error, because a hard link
stores no target to make relative.

One GNU switch group stays refused rather than approximated, for a reason
that is not about links: `-b`/`--backup` and `-S`/`--suffix`, because this
workspace has no backup machinery at all (`cp` and `mv` omit them for the
same reason), so a "backup" would be a name the tool invented. The refusal
is a usage error naming the switch; nothing is silently ignored, and the
divergence is documented in the tool's `Help/` documents.

## A target is data, not a path the tool walks

The target is stored **verbatim**: it may be relative, may carry `..`, and
may name nothing at all, so `ln -s` can legitimately create a dangling link
and never pre-validates what it names. Its *grammar* is checked kernel-side
before it is stored, so a target no resolver could walk is refused rather
than written. Creating a link grants no authority over what it names —
authority is decided at each later use, per component, under the caller's
attested identity.

## Replacing a name

`-f` (and an approved `-i`) put the new link in the existing name's place in
one step: it is made under a staging name and renamed over the old one
(`tairix_rt::replace_with_link`), so the name is never missing, a link that
cannot be made leaves it, and nothing travels through a link already there
to whatever it points at. A hard link's target is reached first, as GNU
reaches it. A directory is never replaced: a directory destination receives
links inside it, and one that still blocks the name (under `-T`) is
reported, not removed. A link that would take the place of the very file it
names is refused as the same file; a symbolic target is judged as the link
will read it, from its own directory, where GNU judges it from the working
directory and so replaces some files with a link to themselves. An
unanswerable `-i` question is never consent, and a declined one fails the
run, as GNU's does.

A link that cannot be made is reported and the next is still made, as
GNU's are; the run exits 1 when any failed.

## Layering & safety

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths. Its dependencies are the
shared `tairix-abi`, `tairix-path`, `tairix-util` and `tairix-help` crates,
and `tairix-rt` in its `Run` binary, so it never
links a kernel or driver crate. Its manifest (`AppInfo.toml`) requests
`CAP_CONSOLE_WRITE`, `CAP_CONSOLE_READ` (the one line `-i` reads), and
`CAP_FS_ACCESS` — within the session baseline — and the secured VFS still
authorises every path per-inode under the caller's attested identity.

## Usage

```
ln [-srLPdFfinvT] [-t dir] [--] target... [link_name]

  -s, --symbolic             make symbolic links
  -r, --relative             store the target relative to the link (needs -s)
  -L, --logical              hard-link what a link target names
  -P, --physical             hard-link the target as spelled (default)
  -d, -F, --directory        accept a directory operand (still refused)
  -f, --force                replace an existing link name
  -i, --interactive          ask before replacing an existing link name
  -n, --no-dereference       treat a link-to-directory destination as a name
  -v, --verbose              report each link made
  -t dir, --target-directory=dir
                             create every link in dir
  -T, --no-target-directory  treat the destination as a link name
  -h, -?, --help             show this command's own short help
```

## Exit status

- `0` — every link was created (or the short help was written).
- `1` — anything else, a declined `-i` question included, with any reason on
  standard error. GNU `ln` has no separate usage status, so a malformed
  command line exits `1` too.

## Stability

`stable` — the tool follows its GNU coreutils counterpart; the divergences
above are deliberate and documented in its `Help/` documents.
