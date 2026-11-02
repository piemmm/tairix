# `tairix-drv-fs-archive` — archive filesystem driver

Serves an archive file's members as a volume: zip, tar (plain, gzip'd or
bzip2'd), a lone gzip or bzip2 stream, Spark and plain ARC, and ArcFS read
and written; RAR 1.5 to 7 read. Each member's RISC OS load, exec and
attribute words travel as its `acorn.*` attributes wherever the format has a
place for them.

Stability: `experimental`.

The formats, what each reads and writes, and how members are decoded and
committed are in `docs/src/filesystem/archive.md`; the staged design is
`plans/ARCHIVES.md`.

## Authority

The driver is a served-filesystem process holding nothing but its own
attachment: the archive, read by byte, and for a writable one its two work
files ([Disk images](../../../docs/src/filesystem/images.md)). It needs no
capability of its own. It never writes the archive in place: the kernel
replaces the archive's file with the next archive the driver writes, only
while the file's name still names it.

## Limitations

- An archive must begin with its signature: a self-extracting program, or a
  RAR appended to another file, is not opened. A zip is found from its end,
  so a self-extractor ahead of one is kept.
- One volume of several (zip, RAR) is refused.
- RAR is read only. Encrypted RAR headers are refused; an encrypted member
  of any format is listed and refused when read. RAR 3's filters run only
  as their six standard programs, recognised by checksum and length; a
  member needing any other is refused.
- A tar's devices, pipes, sparse members (GNU's pax sparse members among
  them) and volume labels have no volume node and are left out, which leaves
  that archive read only, as does any member left out for a name or path no
  volume serves, a tar global header saying anything of its members, two tar
  members of one path, or anything past a tar's end marker.
- A commit that fails stops the archive being written; every flush after it
  answers why.
- RAR 7's compression is decoded as RARLAB documents it; no archive written
  with it has been found to test against.

## Tests

`cargo test -p tairix-drv-fs-archive`: every format read and written over
archives the tests build, from the least archives and packed streams
`tests/integration/archive_image` lays out, foreign tools' output pinned
where one exists, and RAR over the fixtures in `tests/fixtures/rar` (see its
README). `tests/fuzz_archive_open.rs` is the fuzz harness `cargo xtask fuzz`
runs. The disk-image QEMU vertical (`tairix-test-images-qemu-aarch64`) mounts
an archive of each format, writes where the driver writes, and reads it back.
