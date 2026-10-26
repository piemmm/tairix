# `tairix-drv-fs-udf` — UDF filesystem driver

Mounts OSTA UDF 1.02 to 2.60 volumes behind any
`tairix_abi::driver::block::Block` device through `FilesystemRead`,
`FilesystemWrite`, `FilesystemSecurity`, `FilesystemAttrs` and
`FilesystemStats`, and formats new ones (`Udf::format`).

## Supported volumes

- Read: physical, sparable, virtual (write-once media, through the virtual
  allocation table) and metadata partitions.
- Written in place: one physical partition of rewritable or overwritable
  media with a space bitmap; `Udf::write_refusal` names why any other volume
  is mounted read-only.
- Formatted: revisions 1.02 to 2.01, one overwritable partition.
- Hard links, symbolic links, holes, owners and modes; Macintosh Finder
  information as `mac.type`, `mac.creator` and `mac.finderflags`.

`docs/src/filesystem/udf.md` describes each structure, the write policy and
the bounds applied to an untrusted volume.

## Limitations

- Metadata-partition, sparable and virtual-partition volumes are read-only.
- Named streams, and so resource forks, are freed with their file but not
  read or written (`plans/FOREIGN-MEDIA.md`).
- No orphan record: a node removed while the kernel holds it keeps its
  blocks until its last unpin, and a crash before then leaves them allocated
  until the volume is checked.

## Capabilities

`register` requires `CAP_DRV_LOAD`. The crate's `Run` binary, built with the
`program` feature, serves a recognised volume from user space through
`lib/fsserve` (`Served`, binding `tairix,volume-udf`), with `CAP_FS_SERVE` and
`CAP_LOG_EMIT`, and the kernel mounts it under `/Storage`
(`docs/src/filesystem/served.md`).

## Stability

**experimental.**
