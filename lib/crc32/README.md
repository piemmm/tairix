# tairix-crc32

CRC-32 as IEEE 802.3 defines it (reflected, polynomial `0xEDB8_8320`): the
framing checksum foreign formats carry. PNG chunks (`lib/image`) and GPT
headers and entry arrays (`lib/partition`) are checked and written through it,
so the algorithm has one definition.

It is not TAIRiX's own integrity checksum: TAIRiX's formats use CRC-32C
(`lib/crc32c`), a different polynomial with hardware acceleration. Neither is
a security primitive.

- `checksum(data)` — the CRC-32 of one slice.
- `Crc32` — the same checksum over bytes that arrive in pieces: `update` each
  piece in order, then `finish`.

Table-driven, `no_std`, allocation-free, with no `unsafe`. The table is built
at compile time and tested against the bit-at-a-time definition and the
standard's check value (`"123456789"` → `0xCBF43926`).

## Stability

Tier: `stable`. The algorithm is fixed by the formats that carry it.
