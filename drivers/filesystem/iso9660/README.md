# `tairix-drv-fs-iso9660` — ISO 9660 filesystem driver (read-only)

Mounts CD and DVD volumes behind any `tairix_abi::driver::block::Block`
device through `FilesystemRead`, `FilesystemSecurity`, `FilesystemAttrs` and
`FilesystemStats`; every `FilesystemWrite` call is refused with `ReadOnly`.

## Supported volumes

- ECMA-119 (ISO 9660) and High Sierra, read from the disc's last session.
- Joliet levels 1–3 and the ISO 9660:1999 enhanced tree.
- Rock Ridge (SUSP and RRIP): POSIX names, owners, modes, link counts,
  symbolic links, relocated directories, time stamps, zisofs compression.
- Multi-extent and interleaved files, extended attribute records.
- RISC OS `ARCHIMEDES` fields, Apple `AA`/`BA` entries and Amiga `AS`
  entries, surfaced as the shared `acorn.*`, `mac.*` and `amiga.*` attribute
  keys, so a copy onto ARXFS keeps them.

Which tree is mounted, how names are listed, and the bounds applied to an
untrusted disc are in `docs/src/filesystem/iso9660.md`.

## Limitations

- Read-only: the format records a finished disc.
- A UDF bridge disc is mounted as UDF unless its ISO 9660 tree carries Rock
  Ridge or RISC OS records (`lib/fsprobe`).

## Capabilities

`register` requires `CAP_DRV_LOAD`. The driver is linked into the kernel's
volume service, which mounts a recognised disc under `/Storage`.

## Stability

**experimental.**
