# The SingleStepTests sample

`sst6502.bin` is a deterministic sample of the `6502/v1` cases of the
SingleStepTests 65x02 suite (<https://github.com/SingleStepTests/65x02>, commit
`2f6980a2d95757486c7bee24355c360e40e2a224`), re-encoded by
`cargo xtask openmicro --vectors <dir> --write-sample`: sixteen evenly spaced cases
of every opcode, and forty-eight further decimal-mode cases of each opcode
decimal mode changes. The full suite is checked by the same command without
`--write-sample`.

The suite is MIT-licensed; its notice is `LICENSE`.
