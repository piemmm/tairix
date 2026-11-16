# tairix-obey

RISC OS's command-line machinery, pure and over a variable store:

- **GSTrans** (`gstrans`): `OS_GSInit`, `OS_GSRead` and `OS_GSTrans` with every
  `|` escape, quotes, `<n>` character codes and `<variable>` references, each
  variable delivered as its type says.
- **System variables** (`vars`): the `Variables` store trait, the bounded
  host-memory `Store`, `OS_SetVarVal`'s five value types and `OS_ReadVarVal`'s
  expanded read.
- **Expressions** (`expr`): `OS_EvaluateExpression`, with BBC BASIC's operators
  at their priorities and RISC OS's errors for malformed text.
- **Arguments**: `OS_SubstituteArgs` (`args`), as aliases and Obey files take
  theirs, and `OS_ReadArgs` (`readargs`), with its buffer laid out as the call
  leaves it.
- **Command lines** (`command`): `If`'s choice of command, and a command line
  or an Obey file read as data, a read-only mode that runs no command.

The behaviour is RISC OS 5's, as its PRMs state it and as genuine RISC OS 5.30
answers what they leave unstated, each answer recorded by `cargo xtask riscos
--reference` and replayed in `tests/riscos5.rs`. Where RISC OS 5.30 never
returns, the reading is refused instead. Every allocation is fallible, and
every reading of text is bounded by a `Budget` of variable references, however
its macros nest.

`no_std`, with no `unsafe`.

## Stability

Tier: `experimental`.
