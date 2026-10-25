# `tairix-mkdir` — make directories

A `plans/APPS.md` §12.1 Stage C command app, shipped as the self-contained
store bundle `/System/Commands/mkdir.app/` so the shell resolves the bare
word `mkdir` to it. `mkdir` is the GNU coreutils tool: it creates each
directory operand under the caller's attested identity with an exclusive
directory create (`CREATE | DIRECTORY | EXCLUSIVE`) that answers the new
directory open. The implemented GNU surface is `-m`/`--mode` (each
operand made gets exactly the mode named, octal or symbolic, read by the
`chmod` grammar `tairix_util::mode` from `a=rwx`), `-p`/`--parents`
(create missing ancestors, each keeping its owner's write and search
whatever the umask, and tolerate an operand that is already a
directory), `-v`/`--verbose` (GNU-worded `mkdir: created directory 'dir'`
reports), and `--`. `-h`/`-?`/`--help` render the tool's own short help
from its bundled `Help/` tree through the shared `lib/help` engine, in
the locale the inherited `LANG` variable names.

The kernel creates a directory with the mode asked less the umask, and
gives it the set-group-ID bit of a parent carrying one. Under `-m` the
mode is read back and set where it falls short of the one named, keeping an
inherited set-group-ID bit the mode does not name, as GNU's does; a parent
`-p` makes has its owner's write and search put back the same way. Both the
read and the set go through the directory the create answered, held, so no
other process can swap the name for something else between the creation and
the mode change.

The `-p` walk goes through the shared tree seam (`tairix_fscopy::tree`):
each level is made in, or entered from, the one before it, held, so a name
swapped for a link between two levels cannot carry the walk elsewhere, and
each `..` leaves the directory the walk reached. The operand's steps come
from the shared path grammar (`tairix_path::steps`), so an alias-rooted
operand (`Home:/tools/bin`) walks correctly and the tool carries no second
path parser; an operand the grammar cannot read is handed to the kernel
whole, which stays the one validator.

The crate is `no_std` (with `alloc`), has no `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths. Its dependencies are the
shared `tairix-abi`, `tairix-fscopy`, `tairix-path`, `tairix-help` and
`tairix-util` crates, so it never links a kernel or driver crate. Its manifest
(`AppInfo.toml`) requests
`CAP_CONSOLE_WRITE` and `CAP_FS_ACCESS` — within the session baseline —
and the secured VFS still authorises every creation per-inode under the
caller's attested identity.

## Usage

```
mkdir [-pv] [-m MODE] [--] directory...

  -m, --mode=MODE  give each new directory MODE (as in chmod), not
                   a=rwx less the umask
  -p, --parents    make missing parent directories; an operand that is
                   already a directory is not an error
  -v, --verbose    report each created directory
  -h, -?           show this command's own short help
```

## Exit status

- `0` — every directory was created (or, under `-p`, already existed).
- `1` — a filesystem or output failure, a mode that could not be given
  (`cannot set permissions of 'x': …`), or a command line that was not
  understood: `mkdir: reason`, then `Try 'mkdir --help' for more
  information.`, as GNU's `mkdir` reports it; a `MODE` that is no mode is
  `mkdir: invalid mode 'x'` alone, as GNU's is.

## Stability

`stable` — the tool follows its GNU coreutils counterpart; divergences are
deliberate and documented in its `Help/` documents.
