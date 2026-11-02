# tairix-crc16

The CRC-16 with the reflected polynomial `0xA001` (`0x8005` read least
significant bit first), with no final complement: the checksum foreign formats
carry. ARC, Spark and ArcFS members are checked by it from zero (CRC-16/ARC),
ext4's legacy group-descriptor checksum continues it from all ones, and LAME
guards its MP3 stream tag with it, so the algorithm has one definition.

- `checksum(data)` — the CRC-16/ARC of one slice: from zero.
- `update(crc, data)` — `crc` carried on over `data`, for a checksum that
  starts elsewhere or arrives in pieces.

Table-driven, `no_std`, allocation-free, with no `unsafe`. The table is built
at compile time and tested against the bit-at-a-time definition and the
catalogued check value (`"123456789"` → `0xBB3D`).

## Stability

Tier: `stable`. The algorithm is fixed by the formats that carry it.
