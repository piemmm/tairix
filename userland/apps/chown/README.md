# `tairix-chown` — change file owner and group

`chown` gives each file operand the owner and group an `[OWNER][:[GROUP]]`
operand names, or those of a `--reference` file, with GNU's command line.
It ships as the `/System/Commands/chown.app` bundle. The crate is `no_std`
(with `alloc`), has no `unsafe`, and no `unwrap`/`expect`/`panic!` in
production paths; it depends only on `lib/*` crates.

## Usage

```
chown [OPTION]... [OWNER][:[GROUP]] file...
chown [OPTION]... --reference=RFILE file...

  -c, --changes          report only files whose ownership actually changed
  -f, --silent, --quiet  suppress most error messages
  -v, --verbose          report every file processed
      --dereference      change what a symbolic link names
  -h, --no-dereference   change a symbolic link itself
      --from=CURRENT_OWNER:CURRENT_GROUP
                         change only a file owned as this says
      --reference=RFILE  use RFILE's owner and group
  -R, --recursive        change files and directories recursively
  -H                     with -R, follow a link operand naming a directory
  -L                     with -R, follow every link (refused)
  -P                     with -R, follow no link (the default)
  -?, --help             show the short help
      --version          show the version
```

Options may stand anywhere among the operands, a long one shortened to any
unambiguous prefix; `--` ends them. The later of `-c`/`-v`, of
`--dereference`/`-h`, and of `-H`/`-L`/`-P` wins.

## The owner operand

`OWNER` changes the user, `OWNER:GROUP` both, `:GROUP` the group, and an
empty operand or `:` neither. `OWNER:` also gives an owner named by name its
login group. A field is looked up in the account or group directory first
and read as a decimal number only when nothing has that name; a leading `+`
makes it a number without the lookup. Without a `:`, a `.` separates the
fields, with GNU's warning, unless the whole operand names an account. Every
name is read before any file is touched: an unknown name, an unreadable
directory or an unreadable reference changes nothing.

The login group `OWNER:` gives is the one the account directory lists for
that account, so it is read in the same lookup as the owner.

## Links and walks

Without `-R` a link operand changes what it names, or under `-h` the link
itself. With `-R` a link met in the tree is changed itself and never
followed, or under `--dereference` left alone with what it names; a link
named as an operand is changed itself (`-P`), what it names under
`--dereference`, or walked under `-H` when it names a directory. `-L` under
`-R` is refused, since a walk never follows a link it meets. Each directory
is changed after its contents, so giving one away cannot shut the walk out
of it.

## Seams

`run` reaches the system only through:

- `tairix_fscopy::tree::Roots` — operands opened and trees walked through the
  directories it holds, each change made through the descriptor the node's
  old owner was read from (`tairix_rt::copy::WorkingDirectory` when running,
  the fixture's `Tree` in tests);
- `Accounts` — the ungated account and group directories, each account
  listed with its login group;
- `Output`, and `lib/help`'s `HelpSource` for the bundle's own `Help/` tree.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_FS_ACCESS`, and `CAP_FS_CHOWN`, which the kernel
asks of a change that gives a node to another account or to a group the
caller is not in. Without it an account may still give a file it owns to one
of its own groups; every other change is refused by the kernel and reported.

## Fail closed

A refused option, a missing operand, and `-L` under `-R` are reported with
`Try 'chown --help' for more information.`. A node that cannot be reached,
listed or changed is reported in GNU's words and the rest are still
changed; under `-v` a refused change is followed by `failed to change
ownership of … from … to …` and an unreachable node by `failed to change
ownership of … to …`. The run exits 1 when anything failed.

## Tests

`cargo test -p tairix-chown` drives the parser, the owner grammar and the
engine against `lib/fscopy`'s in-memory tree: GNU's option matrix, names
and numbers, `OWNER:`, the `.` separator, `--from`, `--reference`, every
link case, the order a tree is changed in, the fail-closed paths, and
attacker-hook sweeps that swap a name for a link before each seam call in
turn and prove no change leaves the tree.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md)
for the full documentation.
