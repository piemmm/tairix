# `tairix-crc16`

The CRC-16 foreign formats specify: the reflected polynomial `0xA001` with no
final complement. ARC, Spark and ArcFS check every member's decoded bytes with
it from zero (the archive driver), ext4's legacy `gdt_csum` group descriptors
continue it from all ones over the volume's UUID (`drivers/filesystem/ext4`),
and LAME guards its MP3 stream tag with it (`lib/sound`), so all three reach
it here rather than each carrying a copy.

| Item | What it is |
|---|---|
| `checksum(data)` | The CRC-16/ARC of one slice: carried from zero. |
| `update(crc, data)` | `crc` carried on over `data`: a checksum started elsewhere, or over bytes that arrive in pieces. |

The 256-entry table is built at compile time from the polynomial. Tests hold
it to the bit-at-a-time definition at every length up to 600 bytes from three
start values and at every single byte, hold a carried checksum to the one-shot
form at every split, and check the catalogued values (`"123456789"` → `0xBB3D`
from zero, `0x4B37` from all ones).
