# RAR fixtures

`rarfile/` holds test archives from the rarfile project, written by the RAR
encoder, redistributed under its ISC licence (`rarfile/LICENSE`): RAR 1.5
and 2 archives with embedded comments, RAR 3 solid, Unicode-named and
symlinked archives, RAR 5 archives checked by CRC-32 and by BLAKE2sp, with
hard links, file copies and a symlink-traversal attempt, and encrypted
members and headers.

`synth/` holds RAR 3 streams synthesised for these tests from seeded random
data and checked against libarchive: the RGB, audio and delta filters,
their bytecode forged to the standard programs' checksum and length so it
carries none of the programs themselves, and PPMd at orders 6 and 25 and
with the format's largest model, 256 MiB.
