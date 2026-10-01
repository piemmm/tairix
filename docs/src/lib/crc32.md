# `tairix-crc32`

The CRC-32 foreign formats specify: IEEE 802.3's reflected polynomial
`0xEDB8_8320`, started from all ones and complemented at the end. PNG frames
every chunk with it and GPT checks its header and entry array with it, so
`lib/image` and `lib/partition` both reach it here rather than each carrying a
copy.

It is a framing checksum, not TAIRiX's own: the integrity checksum of TAIRiX's
formats is CRC-32C (`lib/crc32c`), a different polynomial.

| Item | What it is |
|---|---|
| `checksum(data)` | The CRC-32 of one slice. |
| `Crc32` | The same over bytes that arrive in pieces: `new`, then `update` each piece in order, then `finish`. A PNG chunk's checksum covers its type and its payload, which are separate slices. |

The 256-entry table is built at compile time from the polynomial. Tests hold
it to the bit-at-a-time definition at every length up to 600 bytes and every
single byte, hold the streaming form to the one-shot form at every split, and
check the standard's check value (`"123456789"` → `0xCBF43926`).
