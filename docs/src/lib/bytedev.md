# `tairix-bytedev`

Byte-addressed I/O over a `tairix_abi::driver::block::Block` device: the one
reader the block-backed filesystem drivers that address their structures in
bytes or in a block size of their own (ext4, FAT32, ADFS, ISO 9660, UDF), the
volume probe (`lib/fsprobe`), the volume manager, the RAID driver and the
kernel's volume service read through. ARXFS, whose block is the device's,
addresses the device directly.

`ByteDevice::new` validates the device's geometry once — a power-of-two block
size in `MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE` and a capacity that fits a `u64` —
then serves `read`, `write` and `zero` at any byte offset:

- the whole device blocks of a range move straight between the caller's
  buffer and the device, in coalesced runs of at most
  `tairix_writeback::RUN_BYTES`;
- only a range's partial first and last blocks are staged, through one heap
  block, so no device-sized buffer ever lands on a kernel stack;
- a range that leaves the device is refused with `LengthOutOfRange` before
  any transfer, so a corrupt on-disk pointer never reaches the device.

`device`, `device_mut` and `into_inner` reach the wrapped device for what is
not byte I/O: its geometry, its disc layout, its flush.

The crate is `no_std` + `alloc` and forbids `unsafe`.
