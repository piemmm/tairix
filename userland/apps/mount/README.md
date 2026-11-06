# `tairix-mount` — list filesystems, and mount disk images

Stability tier: **experimental**.

The `mount` command app. With no operands it lists the mount table through
the System Information API's ungated `MOUNT_LIST` and `VIEW_LIST` queries,
one `source on target type fstype (options)` line per view binding. With an
`IMAGE` operand it mounts the volume that disk-image file holds, privately,
for the user who runs it (`plans/DISK-IMAGES.md` DI6). The kernel recognises
the image, decides whether it may be mounted, and audits the decision. The
tool asks for `CAP_FS_MOUNT` for the answers for held writes below alone: its
attach is always private, and the kernel binds an image's target under its
owner's rules — an empty directory of their own, never the catalog — whoever
runs it. A device or volume source is not mounted
(`plans/OPEN-DEFECTS.md` D1014). `--list-held` lists the volumes holding
writes a re-insert could not prove belong on their medium, and
`--replay-held NAME` or `--discard-held NAME` answers for one: the kernel
takes the answer from the mount authority for any volume, and from the seat
for a removable one (`docs/src/filesystem/drives.md` §8).

## Usage

```
mount [-lrvw] [-t LIST] [-o OPTIONS] [--] [IMAGE [TARGET]]
mount [-v] --list-held | --replay-held NAME | --discard-held NAME

  (no operands)            list the mounted filesystems, of the types -t names
  IMAGE                    mount the disk image IMAGE, at /Storage/NAME
  IMAGE /Storage/NAME      mount it as NAME
  IMAGE DIRECTORY          mount it, and show it at DIRECTORY too
  -r, --read-only          mount read-only (as -o ro)
  -w, --rw, --read-write   mount read-write, or not at all
  -t, --types LIST         the filesystems IMAGE must hold one of, or auto
  -o, --options LIST       comma-separated: ro,rw,noexec,loop,name=NAME
  -v, --verbose            say where it was mounted
  -l, --show-labels        accepted for the listing, which carries no labels
  --list-held              list only the volumes holding writes awaiting an answer
  --replay-held NAME       write the held writes of the volume NAME to its medium
  --discard-held NAME      give up the held writes of the volume NAME
  -h, -?, --help           show the short help
```

The options follow util-linux: the last of `-r`, `-w`, `-o ro` and `-o rw`
wins, `-w` never falls back to read-only unless a later `-r` takes it back,
and `-t` is a util-linux type list — the filesystems an image must hold one
of, each as the mount table names it, or `auto`; with no operands, the types
the listing is filtered by. Every target is resolved and refused as itself,
named as resolved, never as the image. An empty or unknown `-o` element is refused, as
are `nosuid`, `nodev` and `exec`, which have nothing to restrict or are never
granted. The exit status is util-linux's: `1` for a command line not
understood or an answer the caller may not give, `2` for a listing or
terminal that failed, `32` for a mount or an answer that failed — an unknown
filesystem type among them — each failure a sentence ending in a full stop.

## Structure

The crate is the pure `no_std` engine library — parser, listing, mount,
errors — over injected seams (`lib/procinfo`'s `Transport`, the `Mounter`,
`lib/help`'s `HelpSource`), host-tested against in-memory fixtures, and the
freestanding `Run` program, which wires `tairix_procinfo::IpcTransport`,
`tairix_rt::image_attach_path`, `tairix_rt::volume_replay` and
`tairix_rt::realpath`, and `tairix_help::BundleHelp` over the inherited
standard streams. No `unsafe`,
and no `unwrap`/`expect`/`panic!` in production paths.

`cargo test -p tairix-mount` drives the grammar, the listing and the held
listing, the answers and each of their refusals, the attach and every field
it carries, targets, the verbose line and the read-only warning,
every refusal and its exit status, and the thirteen-locale `OPTIONS` pinning
of the bundled `Help/` documents.

See [`docs/src/userland/utilities.md`](../../../docs/src/userland/utilities.md).
