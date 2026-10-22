# tairix-bytedev

Byte-addressed I/O over a `tairix_abi::driver::block::Block` device: the one
reader the filesystem drivers that address a device in bytes or in a block
size of their own (ext4, FAT32, ADFS, ISO 9660, UDF), the volume probe, the
volume manager, the RAID driver and the kernel's volume service read through.

`ByteDevice` validates the device geometry once (a power-of-two block size in
`MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE`), then serves `read`, `write` and `zero` at
any byte offset. The whole device blocks of a range move directly between the
caller's buffer and the device in coalesced runs of at most `RUN_BYTES`; only a
range's partial first and last blocks are staged, through one heap block kept
off the kernel stack. A range that leaves the device is refused with
`LengthOutOfRange` before any transfer.

## Stability

**experimental.**
