# `tairix-util`

The strictly justified shared-utility crate: every item here is used by
**two or more** independent crates (`AGENTS.md` §2.3, §3), and promoting
code into it requires a `PLAN.md` note naming those callers. `no_std`
and panic-free throughout. Members hold text and buffers on the heap, so a
crate that must link no allocator takes nothing from here: the rule for a
hidden character it needs is `lib/visible`'s (`docs/src/lib/visible.md`).

## Members

* `conf` — the `#`-comment line grammar every line-oriented
  configuration store in the tree shares: `strip_comment` returns the
  portion of a line before its first `#`, leaving the surrounding
  whitespace for the caller to trim, so a blank result is a line
  carrying no setting. The boot-time system configuration
  (`lib/sysconfig`), the network configuration (`lib/netconfig`), and
  the service enrolment (`lib/enrolment`) all read that one definition, so
  a change to how a comment is recognised can never apply to some stores
  and not others. `comment_at` is where the marker sits and `setting_line`
  splits a `key value` line exactly as those stores read it, so an editor
  colouring a store and the store's own parser can never disagree about
  which bytes are the key. `Located` is a store's refusal together with
  the line that raised it, the shape every such parser returns. No store's
  keys or values
  may contain `#` — each store's own validators enforce that, which is
  what makes cutting at the first `#` unambiguous. `ValueShape` is the
  other half of that shared vocabulary: what a configuration key accepts,
  either a closed set of canonical spellings or a description of the free
  form its own parser admits. Both registries state it, and a tool
  refusing a value and a settings surface offering the choices read the
  same definition rather than each keeping a copy that would drift.
* `cfloat` — C-locale `printf(3)` floating-point rendering shared by the
  `seq`, `printf` and `ping` command apps (`plans/APPS.md`): one `FloatDirective`
  (the five printf flags, width, precision, `efga`/`EFGA` conversions)
  rendering an IEEE 754 `f64` exactly as C's `printf` renders a `double`
  in the C locale — rounding, padding, alternate forms, special values —
  so the tools that promise printf semantics share one definition. It
  writes into any `fmt::Write` in bounded memory: only the digits an `f64`
  carries are held, and the zeros past them and the padding are written as
  they go, so a sink that flushes takes a field of any width or precision
  without holding it. `rounded` answers the value a rendering reads back
  as, which `seq`'s last-value rule compares. A consumer whose GNU
  counterpart computes in `long double` documents that divergence at its
  own surface.
* `cnum` — C-locale `strtod(3)` scanning shared by `seq`, `printf`, `sleep`
  and `tail`, and by the numbers the SVG decoder and OpenRaster's layer stack
  read: `scan_double` reads the longest leading subject sequence (whitespace,
  sign, decimal or hexadecimal float, `inf`/`infinity`,
  `nan`/`nan(n-char-seq)`) with C's `endptr` measure, composing hex
  floats with exact one-step nearest-even rounding down to the
  subnormals — so `seq` demands full-token consumption and `printf`
  diagnoses a partial conversion over the one grammar.
* `fallible` — reserving a buffer whose size comes from the data before
  filling it, so a request the machine refuses is a typed refusal rather
  than an allocation abort. Userland's heap answers exhaustion with a
  null pointer, which `alloc` turns into a process abort, and a buffer
  sized by image geometry or a decoded payload — a window's surface is
  close to a megabyte — is exactly what a machine short of memory
  refuses. `filled` and `collected` reserve exactly what a one-shot
  buffer needs; `grow_to` reserves amortised for a scratch grown across
  uses, so repeated growth is not quadratic; `reserve` serves a buffer
  filled by pushing. The rasteriser's surfaces and resample plans
  (`lib/raster`), the compositor's scan-out frame and layer buffers
  (`userland/gui/wm`), and the PNG decoder's row and output buffers
  (`lib/image`) all reserve through this one definition, so a desktop
  under memory pressure degrades rather than dying in it. `read_snapshot`
  reads a source that copies all or nothing and never says how much it
  needs — the hardware tree — doubling its buffer from a starting size on
  each `BufferTooSmall`, so the device manager, the System Information
  service and the cooling service read a tree of any size the one way.
* `fmt` — no-allocation numeric formatters that render identifiers, counts,
  addresses and byte runs into `lib/log`'s structured field values without
  touching an allocator on the hot path: the kernel's audit and syscall
  fields, the drivers' and services' log fields, and the QEMU verticals'
  expected lines.
* `size` — the GNU coreutils size vocabulary shared by the `du` and
  `df` command apps (`plans/APPS.md`): the `-B`/`--block-size` grammar
  (`512`, `1K`, `1MiB`, `1GB`, `c`/`w`/`b` byte suffixes, and the
  `human-readable`/`si` rendering words, parsed fail-closed into a
  `SizeScale`), ceiling block scaling (`blocks_ceil` — a partially used
  block is a used block, so usage is never under-reported), and the
  GNU's human-readable sizes, rounded up (`format_human`: one decimal below ten
  units, an integer otherwise, re-tiering a rounded-up amount, in
  powers of 1024 or 1000). Values are `u128` internally so a 100 TB+
  volume's byte totals can never overflow (`AGENTS.md` §26.6).
  Beside it, the *desktop's* prose rendering of the same quantity —
  `format_binary` (`512 B`, `1.9 GiB`) over `binary_scale` and
  `format_at_scale` — shared by the Switchboard's resource pages and the
  Settings storage pane, so one disk cannot read two ways. The two
  renderings are deliberately not each other's default: `df` is bound to
  the GNU spelling and its ceiling rounding, while a surface with room for
  the unit spells it out and truncates, because it reports what is there
  rather than what must not be under-reported. The ladder reaches `EiB`
  because a byte count is a `u64` throughout the ABI.
* `count` — the GNU count grammar for the `-c`/`-n` values shared by the
  `head` and `tail` command apps (`plans/APPS.md`): `parse_decimal`
  reads a plain digit run and `parse_suffixed` a digit run with the GNU
  multiplier alphabet (`b` = 512; `k`/`K`/`M`/`G`/… as powers of 1024, or
  of 1000 with a trailing `B`). A malformed spelling or unknown suffix is
  rejected as `None`; an in-grammar count beyond any possible input
  saturates at `u64::MAX` rather than wrapping, since a count larger than
  the input is served exactly by "all of it". The tool-specific sign
  handling (`head`'s leading `-`, `tail`'s `+`) stays in each tool.
* `secret` — the one definition of "the secret is gone": `wipe` overwrites a
  byte slice through volatile writes and fences afterwards, a word at a time
  across its aligned middle; `wipe_with` does the same for a slice of any
  plain type, given a blank that covers a whole element (an enum's widest
  variant, a type with no padding), so a cache's pixels or a freed kernel
  stack are erased the same way; and `Wiped<N>` is a fixed-size buffer that
  wipes itself at the end of its scope, and `WipedBuf` its twin sized at run
  time, which never grows past the block it was made with, since a buffer
  that moved to a larger block would leave a copy in the one it outgrew.
  Neither shows what it holds in `Debug`. A
  plain `fill(0)` before the bytes are freed or reused is a dead store the
  optimiser may delete outright, so every credential, key and secret buffer
  in the tree — the elevation client, prompt and broker, the greeter's and
  the lock's passwords, the masked entry in `lib/controls`, the build's
  signing keys, the network services' cookie keys — and every cache a later
  holder must not read erases through this one implementation rather than
  its own. Its volatile stores are why `cargo xtask miri` interprets the crate's
  suite, bar the four tests that hold `mathf` to the host's libm, whose results
  the interpreter perturbs on purpose.
* `defer` — the one way an interactive surface hands a piece of slow work to
  a worker: `JobDesk<Req, Ans>` holds one request waiting, one in flight, and
  one answer landed, and nothing about it blocks, locks, or performs I/O (the
  embedder supplies the exclusion and the parking). Two properties are why it
  is not a queue. **Latest-wins**: a submission made while a job is in flight
  replaces any earlier waiting one, so an interaction that settles repeatedly
  costs at most one further job — a queue would make the surface's own
  responsiveness the thing that generated the backlog. **At most one in
  flight**: two concurrent writes to the same store would race for what it ends
  up saying, so a job is handed out only once the previous one has been
  answered. An answer a newer submission superseded is dropped rather than
  delivered, and what a submission *displaced* is handed back, so a caller
  waiting on the displaced request can be told it was superseded instead of
  left waiting for an answer that will never come. `JobQueue<Req, Ans>` is
  its sibling for work where every request is its own — opening each document
  the user asked for — so latest-wins would drop one: requests are answered in
  the order asked, each once, and everything not yet collected counts against
  a capacity reserved when the queue is made, so a burst is refused rather
  than grown and no later step allocates. An owner whose bound follows what
  it serves grows and shrinks the capacity (`grow`, `shrink`), withdraws
  requests it no longer awaits (`retain_waiting`), lands the answer of a
  job it carried out itself (`carry_out`), and counts the answers a pass is to
  collect (`landed_for_collection`). A delivery owes the loop a wake when it is
  the first since the loop last counted or collected, so one landing once a
  pass has counted — before or while it collects — is never left without one.
  The desk is only the
  bookkeeping; the exclusion, the parked worker thread, and the wake that
  reaches a loop's wait-set are `tairix_rt::work`, which every app-side
  consumer drives it through. Consumed by the terminal's settings publisher
  and the Settings application's applier (both via `tairix_rt::work`), the
  desktop session's settings publisher, the session's program-catalogue scan,
  the file manager's bundle scan, the document host's queue
  (`tairix_window::docapp`, which TextEdit and Paint run in), Paint's decode
  queue, the Date & Time, Music and `netmap` work queues, and WinterSun's
  terrain desk.
* `banner` — the `==> name <==` banner `head` and `tail` set before each of
  several sources: `Banners` writes a blank line before every banner but the
  first, names standard input as GNU does, and shows a file's name as the
  output shows names (`quote::output_style`), so a hostile name escapes only
  at a terminal. Consumers: `head`, `tail`.
* `argv` — `choose` resolves a word argument (`--total=au`) to its one
  choice — the word spelled exactly, or the only one it prefixes — and
  `BadWord` refuses one as GNU does, `ambiguous argument 'e' for
  '--output-error'` or `invalid argument …`, then `Valid arguments are:`
  and each word on its line, read from the same table `choose` read.
  Consumers: `tail`, `tee`, `wc`.
* `getopt` — GNU `getopt_long`'s reading of a command line, once for every
  command app. A tool lists its options in an `Opt` table and `Scan` walks
  its arguments: short options clustered (`-sf`), a value attached (`-tdir`,
  `--target-directory=dir`) or following (`-t dir`), a long option by any
  unambiguous prefix (`--verb`, read by the same prefix rule as `choose`),
  `--` ending the options, and operands among them unless the tool's `Order`
  ends them at the first operand. An argument it cannot read is a `Refusal`
  worded as GNU's: `invalid option -- 'x'`, `unrecognized option '--name'`,
  `option requires an argument -- 'f'`, `option '--quiet' doesn't allow an
  argument`, `option '--s' is ambiguous; possibilities: …`. Consumers: every
  command app that takes options — `cat`, `cp`, `df`, `du`, `head`, `ls`,
  `mount`, `ps`, `seq`, `tail`, `useradd`, `servicectl`, `sysinfo` and the
  rest, the desktop session's command line (`applib`) and the file
  manager's.
* `mathf` — bounded, total `f64` maths for `no_std` geometry (`floor`,
  `sqrt`, `sin`, `atan2`, `exp`, `ln`, …) with no external libm, so the glyph
  rasteriser (`lib/fontface`), the SVG decoder (`lib/svg`), the raster
  engine, the audio engine, the desktop's screensavers, the desktop companion
  and WinterSun round and rotate identically, and on every target the same
  bits. The square root and integer rounding are the toolchain's correctly
  rounded forms — an instruction where the target has one, else the compiler
  runtime's routine (the SSE2 `x86_64` baseline has no rounding instruction)
  — so IEEE 754 fixes their answer; the transcendentals are fdlibm's range
  reductions and minimax kernels in one fixed order with no fused
  multiply-add, within an ulp of the true value. An angle past 2^20 quarter
  turns is reduced by Payne and Hanek's method in integers, so every finite
  angle reduces exactly, however large. Every function returns a finite
  answer for every finite input, so no caller guards against a `NaN`: an
  argument past the double's range saturates, and `ln` of zero, a negative
  or a `NaN` answers `f64::MIN`. `smoothstep` is the one clamped easing
  every consumer ramps with; `smoothstepf` is its `f32` twin. `Phasor` sweeps
  a sine along a row by turning its phase one rotation a step, so a row of
  samples pays one four-multiply rotation a sample rather than a series; the ribbon of
  light (`lib/ribbon`) and the retro games screensaver's floor both sweep
  through it.
* `mode` — the one reading of a file mode a command line spells, shared by
  `chmod`, `mkdir -m` and the shell's `umask`. `Mode::parse` takes an octal
  value of any length up to `7777`, or GNU's symbolic grammar
  `[ugoa]*([-+=]([rwxXst]*|[ugo]))+|[-+=][0-7]+`: permission letters, a
  field's bits copied (`g=u`), or octal after an operator (`=644`), clauses
  joined by commas. `Mode::resolve` gives the bits it makes of a node's: a
  clause naming no field changes every field but the bits the umask holds
  (though `=` still clears them all first), as GNU's `chmod` does; `s` sets
  the owner's and the group's set-ID bits and `t` the sticky bit, each only
  for the field carrying it; and on a directory the set-ID bits stay as they
  are unless the mode names them — with `s`, octal after an operator, or an
  octal value of five digits or more — so a group-inheriting directory stays
  one. A creator that wants no narrower mode passes `lib/abi`'s
  `FS_NEW_FILE_MODE` (`0o666`) or `FS_NEW_DIR_MODE` (`0o777`), which the
  kernel narrows by the creator's umask, and `mkdir -m` resolves its mode
  from the second. `umask` keeps only what a mask can hold: an octal value's
  permission bits, or clauses of `r`, `w` and `x`.
* `retry` — the two retry schedules. `RetryLadder` is for waiting on
  something that has not appeared yet and has no readiness event: a
  bounded, doubling one-shot ladder, so a boot on which the thing never
  appears ends after the ladder's finite length rather than a poll loop.
  The clock service's server selection and RTC read climb it; what waits on
  a volume being mounted waits on the mount table's notice instead.
  `RestartPacer` is for restarting
  something that keeps dying: a capped, doubling, saturating delay,
  forgotten once a restart stays up for a stable window. What fails may be
  killed on purpose by a crafted input, and restarting it at once would
  hand the sender a process spawn per input. The service manager's restart
  policy, the device manager's driver reloads, `lib/sandbox`'s supervised
  worker and the desktop session's retry of a refused lock pace through it.
* `space` — three-dimensional vectors, orthonormal frames and rigid poses.
  `Vec3` is a point, a direction or any triple combined component by
  component; `Frame` an orthonormal basis, built from a normal (Duff et al.'s
  branch-free basis), from a yaw and a tilt, as the turn about an axis
  (Rodrigues' formula), or carrying one direction onto another, and composed
  with another; `Pose` a rigid placement taking points into and out of a
  shape's own coordinates. The ray tracer (`lib/raytrace`) traces in it and the
  Retro Games screensaver turns and places its wireframe
  craft in it, so the two share one arithmetic rather than a copy each.
* `tailwindow` — the bounded rolling "keep the last N bytes/lines"
  windows shared by the same two apps: `ByteWindow` and `LineWindow`
  retain only the trailing N units of a stream, so `head`'s `-c -N` /
  `-n -N` elide modes and `tail`'s `-c N` / `-n N` last-N modes are two
  policies over one mechanism whose memory cost is N, never the input
  size — a 100 TB+ file is a constant-memory read.
* `hex` — hexadecimal text for byte strings: `digits` spells a byte as two
  lowercase digits, and `decode_into` reads a string in either case straight
  into the caller's buffer, answering whether it spelled exactly that many
  bytes, so a key is never staged where its holder cannot erase it. The
  build's signing key set, `mkimage`'s root key file, `tools/cc`'s checksum
  pins and the zone-store compiler's source pins read the one definition.
* `hexdump` — the classic hex dump's shape as every hex view draws it:
  `BYTES_PER_ROW` bytes a row, an offset column `offset_digits` wide (never
  narrower than eight digits, wider only past 4 GiB), and `ascii_of`, which
  shows a byte in the character column only when it prints, so no byte from
  a file reaches a display as a control. `fstree`'s hex view and TextEdit's
  hex view read the one definition.
* `join` — `join_within`, one bounded line from a list: every item joined with
  `, ` when they all fit, else the longest run of whole items from the front
  that leaves room for `, and N more` (or `N more` alone when no item fits
  beside it), so a list is never cut mid-item and never runs past its bound.
  `lib/procinfo`'s multi-valued readings (routes, neighbours, servers) and
  `netmap`'s host tooltip read the one rule.
* `lanes` — the portable eight-bytes-at-a-time byte search, for a target
  whose vector unit is off and whose byte loops are therefore not vectorised:
  `equal_lanes` marks a word's lanes holding a byte exactly (no borrow can
  forge a match in the next lane), and `count`, `nth` and `span` — how many,
  where the nth, and where the first and last lie in one pass — are built on
  it. `lib/collections`' group matching and TextEdit's piece table and line
  lookups use it.
* `output` — `Output`, the byte stream a command engine writes its output and
  diagnostics through, so a host test captures exactly what it renders and a
  refused write is an ordinary result: `write_all` takes every byte or answers
  the stream's own refusal, and `info` emits a best-effort advisory record on
  fd 3, dropped by default. `lib/rt` implements it for `Stdout` and `Stderr`,
  keeping the kernel's reason for a refused write and routing either stream's
  advisory to fd 3, so a `Run` binary hands its engine the standard streams
  themselves. `lib/procinfo` reads every byte stream as a line sink.
  `report_failure` writes the one `tool: failure` line every command app
  names a failure by, and `Carried` is the run built on it: `absorb` and
  `carry` report a `Failure` the run carries past — a failed operand, or an
  entry of a walk, as GNU's tools carry on — and remember that one
  happened, so the run ends with status 1, while a fatal one (the command
  line, the terminal, a prompt) is handed back to end the run;
  `silenced` keeps the report and not the status, for `-f`. A refused
  report is itself fatal, through `Failure::unreported`. `at_terminal` says
  whether the stream is a terminal, which decides how a name from elsewhere
  is written to it (`quote::output_style`): `false` by default, and answered
  for `Stdout`/`Stderr` in `lib/rt` by the kernel's terminal-size
  attestation, the probe `ls` decides by.
* `quote` — GNU coreutils' quoting styles and the three ways a command names
  text from elsewhere. `quote` names a file inside a message's sentence, a
  `-v` report or a prompt (always quoted, hidden characters as `$'…'`);
  `quote_subject` names the file a `name: reason` line leads with (quoted
  where a shell needs it or where it holds a `:` of its own, which would pass
  for the one ending the name); `output_style` is how a command writes a name
  to its standard output — escaped where a shell needs it at a terminal the
  kernel attests (`output::Output::at_terminal`), the name's own bytes
  anywhere else, so a pipe carries what is there; and `quote_if_needed` is
  `printf`'s `%q`. A `Quoted` in any `Style` is `ls`'s `--quoting-style`,
  `hiding` is its `-q`, and `marking` names the characters a context must keep
  apart from the text around the name (the `:` of an `ls -R` heading or a
  `name: reason` line, `-b`'s space, an indicator style's suffixes), which
  force the shell styles to quote and take a backslash in the C ones. Each
  formats into its destination without allocating. Three deliberate
  departures from GNU 9.4: what is escaped is what `lib/visible`'s `Faithful`
  escapes — the hidden characters, and a mark stacked or doubled past telling
  apart — so a name holding a bidirectional or zero-width character or a
  doubled accent is escaped where GNU leaves it raw; the shell styles keep
  GNU's form save its two faults with a name holding a `'` that ends in an
  escape — a needless leading `''`, and,
  when such a name also begins with an escape, a dropped `$` that stops the
  word reading back as the name; and a marked character takes no backslash
  inside `"…"`, where a shell keeps the backslash. Every command app's
  messages, `ls` and `printf` write names through it, so a name a foreign
  volume carries never reaches a terminal raw.
* `report` — a file's structure as a person reads it: `Builder` writes heading
  lines and records (offset, name, length, what it declares) with facts
  beneath them, and holds a report to `MAX_REPORT_LINES` lines of at most
  `MAX_LINE_BYTES` bytes, saying once that it stops when it fills; `clean`
  shows text a file carries with its stray bytes and hidden characters
  escaped, and `digest` a key
  or hash as one run of hex. The picture reports (`lib/image`) and the
  executable and manifest reports (`lib/binfmt`) build with it, and
  `lib/sandbox` carries every report over its one reply, so each is cut,
  escaped and capped by the same rule; `fstree` and TextEdit show the
  `ReportLine`s it holds.
* `seconds` — `parse_seconds`, the decimal-seconds operand a network tool's
  timing option takes (`ping -i 0.2`, `traceroute -w 2.5`), as nanoseconds:
  whole seconds and up to nine fractional digits, no sign, exponent or unit,
  bounded to `MAX_SECONDS` (a year), so its nanosecond form cannot overflow
  and a typo cannot become a multi-day wait.
* `utf8` — `sequence_len`, the length of the UTF-8 sequence a byte begins,
  or `None` for a byte no well-formed sequence begins with (Unicode Table
  3-7: a continuation byte, the always-overlong `0xC0`/`0xC1`, or a lead
  past U+10FFFF). `lib/syntax`'s lexers step over one character by it,
  `wc` counts characters by it, and TextEdit decodes its grid by it, so no
  hand-written decoder admits a lead the standard rejects. `shown` answers a
  name as a surface shows it, as `lib/visible` shows text from elsewhere — each
  hidden character U+FFFD, and a combining sequence cut at one U+FFFD where it
  would carry more than four nonspacing marks or one twice, judged over
  canonical decompositions so `é` then U+0301 is a doubled acute — and
  borrowed where nothing needs showing: the rows `procinfo::field_lossy`
  decodes (process names, volume labels, mount targets) and the names the file
  manager and the document host show pass through it. `mixes_scripts` says
  when a name mixes scripts the way an impersonation needs (UTS #39's
  minimally restrictive level: Latin with Cyrillic or Greek among others), so
  a surface can mark it — the file manager's Network view draws such an
  endpoint with a caution in place of its picture. `fold` and `folded` are
  the letters a reader looks a name up by — a Latin-1 letter folded to the
  letter it is written on, every letter in lower case — and `reader_order`
  the order a list of names is read in: the folded letters, then the bytes,
  so distinct names never compare equal. The place chooser and the program
  library's search match by the fold, and `lib/tz`'s place listing, Settings'
  region list and the program library sort by the order.

## How to grow the crate

Promoting code into `lib/util` requires a `PLAN.md` note documenting the
two-or-more concrete callers, an entry in the member list above, and
unit tests next to the new item per `AGENTS.md` §7.
