# `tairix-util`

Stability tier: **experimental**.

The strictly justified shared-utility crate: every item is used by two or more
independent crates, and `PLAN.md`'s `lib/util` table names them — promoting
code into the crate means adding its row there. `no_std` and panic-free
throughout; every member carries its unit tests next to the code. Members hold
text and buffers on the heap, so a crate that must link no allocator takes
nothing from here: the rule for a hidden character it needs is `lib/visible`'s.

## Members

* `argv` — a word argument resolved to its one choice, the exact word or the
  only one it prefixes (`choose`), refused in GNU's words with the valid ones
  listed (`BadWord`).
* `banner` — the `==> name <==` banner before each of several sources, its
  name shown as the output shows names.
* `cfloat` — C-locale `printf(3)` floating-point rendering (`FloatDirective`:
  flags, width, precision, `efga`/`EFGA` conversions, rendered exactly as C
  prints a `double`).
* `cnum` — C-locale `strtod(3)` scanning (`scan_double`: longest-prefix
  `endptr` semantics, decimal and hexadecimal floats with exact one-step
  rounding, `inf`/`nan`).
* `conf` — the `#`-comment line grammar every line-oriented configuration
  store shares (`strip_comment`, `comment_at`, `setting_line`), a store's
  refusal with its line (`Located`), and the values a key accepts
  (`ValueShape`).
* `count` — the GNU count grammar for `-c`/`-n` values (`parse_decimal`, and
  `parse_suffixed` for the multiplier alphabet).
* `defer` — handing slow work off an interactive loop (`JobDesk`: one request
  waiting, one in flight, one answer landed, latest-wins, the displaced
  request handed back; `JobQueue`: every request answered, in turn).
* `fallible` — reserving a data-sized buffer before filling it (`filled`,
  `collected`, `grow_to`, `reserve`, `read_snapshot`), so an allocation the
  machine refuses is a typed refusal rather than an abort.
* `flags!` — a set of bit flags over one unsigned integer, as a field carries
  it (`NONE`, `with`, `without`, `contains`, `from_bits`, `bits`).
* `getopt` — a command line read as GNU's `getopt_long` reads it (`Scan`
  over a tool's `Opt` table: short options clustered, a value attached or
  following, a long option by any unambiguous prefix, `--` ending the
  options, operands among them unless the tool's `Order` ends them at the
  first), and an argument it cannot read refused in GNU's words (`Refusal`).
* `fmt` — numeric formatters that allocate nothing, for structured-log field
  values.
* `hex` — two lowercase hex digits a byte (`digits`), and the bytes a hex
  string spells written straight into the caller's buffer (`decode_into`), so
  a key is never staged where its holder cannot erase it.
* `hexdump` — the canonical offset/hex/ASCII dump row.
* `join` — one bounded line from a list: every item when they all fit, else
  the longest run of whole items that leaves room for `, and N more`.
* `lanes` — finding a byte eight lanes of a word at a time.
* `mathf` — bounded, total `f64` maths for `no_std` geometry, with no external
  libm and the same bits on every target, the one clamped `smoothstep` (with
  its `f32` twin), and `Phasor`, a sine swept along a row a turn at a time.
* `mode` — a file mode as `chmod`, `mkdir -m` and the shell's `umask` spell it
  (`Mode::parse`: an octal value, or GNU's symbolic clauses with copies and
  octal operators), the bits it gives a node (`Mode::resolve`: a clause naming
  no field sparing the umask, a directory keeping set-ID bits the mode does
  not name).
* `output` — the byte stream a command engine writes through (`Output`: every
  byte or the stream's own refusal, a best-effort fd-3 advisory record, and
  whether it is an attested terminal), `report_failure`'s one `tool: failure`
  line, and `Carried`, the run that reports each `Failure` it carries past —
  an operand or a walk's entry — remembers that one happened, and hands back
  the fatal ones.
* `quote` — GNU coreutils' quoting styles (`Quoted`, `Style`) and the three
  ways a command names text from elsewhere: `quote` inside a sentence,
  `quote_subject` leading a `name: reason` line, and `output_style` on its
  standard output — escaped at an attested terminal, the name's own bytes
  anywhere else; `quote_if_needed` is `printf`'s `%q`.
* `report` — a file's structure as a person reads it (`Builder`,
  `ReportLine`), capped at `MAX_REPORT_LINES` lines of `MAX_LINE_BYTES`
  bytes, with the text a file carries shown escaped.
* `retry` — `RetryLadder`, a bounded doubling one-shot schedule for waiting on
  something with no readiness event, and `RestartPacer`, a capped doubling
  delay between restarts of something that keeps dying, forgotten after a
  stable window.
* `seconds` — the decimal-seconds operand a network tool's timing option takes
  (`parse_seconds`), as nanoseconds, bounded to `MAX_SECONDS`.
* `secret` — erasing secret bytes so the optimiser cannot delete the erasure
  (`wipe`, `wipe_with`), and the buffers that erase themselves when dropped
  (`Wiped<N>`, and `WipedBuf`, which never grows past its block).
* `size` — the GNU coreutils size vocabulary: `-B`/`--block-size` parsing
  (`SizeScale`), ceiling block scaling (`blocks_ceil`), GNU's human-readable
  sizes rounded up (`format_human`), and the desktop's prose rendering
  (`format_binary`).
* `space` — three-dimensional vectors, orthonormal frames and rigid poses
  (`Vec3`, `Frame`, `Pose`).
* `tailwindow` — the bounded rolling "keep the last N bytes/lines" windows
  (`ByteWindow`, `LineWindow`), so a last-N view costs memory in N rather
  than in the input.
* `utf8` — which bytes begin a UTF-8 sequence and how long it is
  (`sequence_len`, Unicode Table 3-7), a name as it is shown, as
  `lib/visible` shows text from elsewhere (`shown`), whether a name mixes
  scripts the way an impersonation needs (`mixes_scripts`), and the letters
  and order a reader looks names up by (`fold`, `folded`, `reader_order`).

Long-form documentation: `docs/src/lib/util.md`.
