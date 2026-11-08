# `tairix-sandbox` — the parser-sandbox seam

`tairix_sandbox` (`lib/sandbox`) is TAIRiX's one user-space seam over the
kernel's parser-sandbox spawn mode
([the parser sandbox](../security/sandbox.md)): the typed request/reply
path every program runs a parser of untrusted input through. The kernel
primitive makes a minimum-capability worker process *exist*; this crate
makes it *usable* — and writes the containment discipline exactly once,
so no program re-derives it.

Stability tier: **experimental**.

## Shape

- **`proto`** — a deliberately tiny length-framed byte protocol over any
  bidirectional `Channel` (pipes in production, in-memory fakes in host
  tests). Frames are bounded by `MAX_FRAME` — a fixed validation bound on
  a hostile peer, not a growable capacity — and both sides refuse an
  oversize declared length *before* reading or allocating a payload byte.
  End-of-stream on a frame boundary is the clean end of a conversation;
  end-of-stream inside a frame is a typed failure, never silently
  shortened data.
- **`worker`** — the serve loop a sandboxed process runs: read a request
  frame, hand the payload to a total `Service`, write the reply frame,
  finish when the parent closes the request stream. A `Service` encodes
  "that request is malformed" as a typed error reply; the loop itself
  cannot be derailed by request content. Each request is wiped, its block's
  spare room too, once it is handled, since one may carry a secret such as a
  document's password.
- **`host`** — the calling program's side. `ParserSandbox::request` sends
  one payload and blocks for the reply, for no longer than the production
  launcher's reply deadline (`rt::REPLY_DEADLINE_NS`, two minutes: far past
  any parse the bounds admit). Every worker failure — crash, protocol
  violation, oversize reply, exit without answering, an answer not given in
  time — runs one containment path, a worker still running being killed
  before it is reaped: the caller receives `SandboxError::WorkerFailed`
  carrying how the worker ended, read from its exit status (`WorkerEnd`),
  and the event is logged with that cause under a stable id
  (`EventId(6000)` crashed, `EventId(6001)` unavailable; the crate owns the
  `6000..7000` range). Only a failure for want of memory is worth asking
  again (`SandboxError::out_of_memory`). The next request starts the
  **replacement**, so none is started that nobody asked for, and one that
  cannot start is that request's answer. Dropping the seam
  disposes of its live worker. A reply that frames correctly but that its
  service cannot believe is the same evidence of a broken or subverted
  worker: every client helper runs its exchange through
  `ParserSandbox::ask`, and a failure the service's `Unbelieved` impl
  judges unbelievable contains the worker exactly as a crash is — once,
  however deeply exchanges nest — while a refusal the worker was entitled
  to make leaves it serving.
  **The parent allocates fallibly; the worker as it needs.** A request is
  built in a `wire::Writer<Refusable>`, whose first refused reservation makes
  the whole request refused rather than any part of it sent; a reply is
  received into room reserved fallibly, and every client decodes it by
  borrowing (`Reader::str`) and copying fallibly. A refusal of this side's
  memory is said as that — `SandboxError::OutOfMemory`, or the client's own
  `NoMemory` — and lets the worker go without blaming it: no crash is logged
  and none is contained for it. A worker allocates as its work needs, under
  its kernel ceiling, and a worker that runs out ends with the runtime's
  out-of-memory exit (`WorkerEnd::OutOfMemory`). Each launcher states the
  longest document its workers may be handed (`Launcher::document_room`,
  `ParserSandbox::document_room`), so a longer one is refused here as too
  large rather than there for want of memory.
- **`session`** — the **duplex, long-lived** seam beside that one-shot
  pair, for a worker that serves a protocol rather than answering a
  question. Three things differ and each is load-bearing: many frames are
  in flight each way (one inbound frame may be answered with none or
  several); the parent never blocks, driving every transport operation
  from its own wait-set over the two descriptors the transport reports
  (`Stream` on the reply end, `StreamRoom` on the request end), so one
  session can never stall another; and a failed worker ends the session
  rather than being replaced, because it held the connection's protocol
  state. Containment is one path — dispose, log `EventId(6002)`, latch,
  and refuse every later call without touching the transport. Both queues
  are bounded by `SessionBounds` and committed at admission, so the
  steady state allocates nothing and the cost of a session is known
  before it is admitted; the send ceiling is derived from the outbound
  bound, which is what makes `OutboundFull` provably transient (an
  accepted payload always fits an empty queue) and `FrameTooLarge`
  permanent. The queues are `lib/collections`' `ByteQueue`, which wipes
  its storage on drop, because a worker's keys cross in these frames.
  `wants_read` going false is total back-pressure: the owner
  disarms, the pipe fills, and the kernel blocks the worker.
  Deadlock-freedom is structural, not argued: the sandbox allow-list
  leaves the pipe as the worker's only wake source, so the worker may use
  the ordinary blocking `Channel` while the parent's two readiness legs
  guarantee it is always woken. The worker side is `serve_session` over a
  `SessionService`, emitting through `FrameOut` straight to the channel,
  so fanning one frame out to many allocates nothing per frame.
- **`loopback`** — the public in-process fakes: for the one-shot seam each
  "worker" is a fresh `Service` run inline; `LoopbackSession` is the same
  for the duplex one, running a `SessionService` behind a
  `SessionTransport`. Either way a consumer's host tests drive the full
  parent-side path (framing, containment, typed decode) under plain
  `cargo test`, exactly as the `Fs`/`Tty` seams take fakes.
  `PipeSessionLauncher` puts each duplex worker behind a model of the kernel's
  pipe pair: a write takes at most `PIPE_ROOM` bytes, and its `PipeGauge` says
  what a wait-set would report and counts every read of an empty pipe — which
  on a machine parks its reader — so an owner is tested under the readiness it
  will be woken by.
- **`decode`** — the first consumers behind the seam: executable-container
  summaries and file reports through [`tairix-binfmt`](./binfmt.md) and
  per-window instruction disassembly through [`tairix-disasm`](./disasm.md).
  The `DecodeService` runs inside the worker; the client helpers
  (`container_summary`, `file_report`, `disassemble`) marshal typed
  requests and validate every reply field **fail-closed** — a worker that
  has parsed hostile bytes is itself hostile, so list counts, name
  lengths, tags, and instruction lengths are all bounds-checked before
  the caller acts on them, a disassembly reply is held to the request it
  answers (instructions contiguous from the address asked, inside the bytes
  sent, no more than asked for), and truncation is reported honestly through
  `regions_truncated`/`symbols_truncated`, never silently. A disassembly reads
  one `CodeWindow`; one that says the code `continues` past its bytes has no
  instruction decoded from fewer than `Isa::longest_encoding` of them, so its
  end cuts none and the next window starts where it stopped. A decode worker is
  held to `decode::WORKER_MEMORY`, the one memory class for every launcher of
  the service. `Isa::of_arch`
  names the instruction set a kernel built for a given `Arch` runs, which an
  `rxe` — whose container names none — is read as on the machine it is
  installed on.
- **`listing`** — what the decode service answers, as text a person reads:
  `code_lines` gives an instruction a line with its address, encoding bytes
  and symbolised branch target, after a `<symbol>:` label for each symbol it
  starts; `region_line` a region's kind, extent and permissions; and
  `failure_reason` a refusal in words. A branch target is named by a symbol
  starting at the nearest address at or below it that covers it, so a short
  alias sharing a function's start cannot hide the function. Every line is
  written into a string reserved to its length, fallibly, so a listing the
  heap will not hold is `None`. The `fstree` pane and TextEdit's Code display
  both list through it, each saying a refused listing for want of memory.
- **`helpdoc`** — the sandboxed help-document render: the `HelpService`
  worker parses and renders a foreign bundle's document through
  [`tairix-help`](./help.md), and the client `render_help` re-parses the
  reply through the `tairix-vt` streaming parser, admitting only the
  closed op set a help render can contain (printable text, line feeds,
  the bold/underline SGR pairs) and re-encoding it canonically — a
  forbidden escape or a truncated trailing sequence refuses the whole
  reply, and a document-parse error round-trips typed (`HelpError`, code
  for code). `man` is the consumer: it reads the document with its own
  file authority (`tairix_help::load_raw`) and never parses it
  in-process.
- **`textsyntax`** — a text editor's colouring, format detection and
  settings validation through [`tairix-syntax`](./syntax.md). The client
  helpers (`lex_lines`, `validate_document`, `detect`) bound what they send
  (`MAX_LEX_BATCH_LINES`/`_BYTES`, `MAX_VALIDATE_LEN`, `MAX_HEAD_LEN`) and
  believe a reply only once it holds against the request: every span in its
  line's bounds, ascending, non-overlapping and of a real role, every
  diagnostic on a line the document has and saying something printable. A
  reply that breaks any of it is `SyntaxFailure::ReplyMalformed`, never
  partly adopted, and retires the worker. Each bound is derived and a
  const assertion holds every request and its largest reply inside one
  frame: `MAX_VALIDATE_LEN` is one byte past the longest store, so the
  store's own parser refuses an over-long one, and a validation answers at
  most `tairix_syntax::MAX_DIAGNOSTICS`. `TextEdit.app` is the consumer.
- **`imagerender`** — the desktop's image decodes: an icon, sent whole in one
  request; the wallpaper, sent with `send_document`; a thumbnail and a RISC OS
  application's sprite (`application_sprite`), each streamed in and let go once
  drawn; and a document session,
  whose file is streamed in through `upload_document` rather than held whole,
  read straight into one push frame of at most `MAX_DOCUMENT_CHUNK` bytes and
  no more than the document, and never copied. A document is refused before
  a byte is sent when it is longer than the launcher's `document_room`; the
  worker reserves it whole, fallibly, and answers
  `DocumentRefusal::OutOfMemory` where its ceiling will not hold it. A
  drawing whose lettering the worker has no glyphs for is answered with the
  faces and scalars it wants; the caller's font seam furnishes them, sent
  once as a table held to `MAX_FACES` and `MAX_SCALARS` with each face's
  glyphs ascending by scalar, and the worker draws.
- **`imageedit`** — an editor's document over the same upload: one picture,
  a TIFF's pages, a sprite area's sprites or an OpenRaster document's layers
  (`EditKind`), each entry at its own depth with the density its file states,
  what the file held beside them (`Unkept`) and how it was written
  (`Written`), a sprite the decoder cannot read kept as its bytes, and a layer
  with its name, offset, opacity and visibility (`EditLayer`) over the canvas
  the document declares. Every answer is held by the caller to
  `MAX_EDIT_SIDE`, `MAX_EDIT_PIXELS` and `MAX_EDIT_ENTRIES`, a layer's name to
  `MAX_LAYER_NAME`, a stack to `MOST_ORA_LAYERS`, and a kept sprite to the
  length of the document handed over (`open_edit`'s `handed`,
  `EditDocument::length`), since it is part of it. `Paint.app` is the
  consumer, and reads each document through a fresh worker
  (`ParserSandbox::release`).
- **`imageinspect`** — a picture's structure report over the same upload: the
  records its container holds and every EXIF tag (`tairix_image::inspect`),
  answered as lines the caller holds to `MAX_REPORT_LINES`, `MAX_LINE_BYTES`
  and printable UTF-8 — the one report reply every report op shares, as
  `decode`'s `file_report` does. The image worker serves it beside its other ops, and
  `InspectService` alone, in a document worker. `TextEdit.app`'s
  Data display is the consumer.
- **`drawedit`** — a drawing editor's documents read into the editable
  drawing (`tairix-drawing`, [`docs`](./drawing.md)) over the same upload.
  `read_drawing` asks the worker to read the document, which it reads as a
  RISC OS Draw file, a PDF or SVG by its content (`tairix_drawing::read`),
  never by a name the editor could be wrong about, a PDF at the page and with
  the password `Access` asks; `read_pages` asks first for a document's pages,
  a PDF's count and as many of their sizes as a reply holds. Either request
  carries the room the worker may decode into — what its share holds beside
  the document — and the password, at most `MAX_PASSWORD_BYTES`, the request
  wiped once sent. The worker answers the page,
  grid, guides, title, what it could not keep and how many pictures the
  drawing shows. The pictures come next, each once however many objects show
  it: its size and what the bytes it was read from state, then its
  premultiplied pixels and those bytes as many as a frame holds at a time —
  every chunk but the last a full frame, so a picture comes in as few
  exchanges as its size allows — each channel held to its pixel's alpha as
  it lands. Together the pictures
  of one read hold no more pixels than a drawing's may and no more bytes
  than the document handed over (`read_drawing`'s `handed`) or the room its
  streams decoded into, whichever is more, since every picture's bytes are
  part of the one or of the other. Then the drawing as records a batch at a time —
  every gradient and style once, referred to by number, then the tree depth
  first, a group's clip before its children, a picture by its number, a long
  path's nodes cut across records so every record fits a frame, and a text
  whole in one record — its baseline's origin or its path of at most
  `MAX_TEXT_PATH_NODES` nodes, and its runs, each the span and the overrides
  its mask names — the text's own check holding them to their form. The editor
  rebuilds the drawing as the records arrive, with an explicit stack, every
  value through the drawing's own constructors and every picture's levels
  built on its own side; a reference to a definition or picture not sent, a
  count past the drawing's bounds, a group given more or fewer children than
  it declared, a batch that brings nothing, or a record with bytes left over
  is not believed; a record that fails for want of the editor's memory is
  answered as that, never as a reply that cannot be believed, so an honest
  worker is not condemned for it. `read_placed` decodes the document handed over as a
  picture to place instead — any still picture `tairix_image` reads, by its
  content (`tairix_drawing::decode_placed`), the first page or sprite of a
  file of several — and the worker answers the size its pixels span, held to
  the drawing's coordinates, before the picture comes across as a drawing's
  does, kept with its bytes only as a PNG or a JPEG; what is no picture is
  `DrawRefusal::NotAPicture`.
  `DrawEditService` serves it in a worker held to `WorkerMemory::Documents`.
  `Draw.app` is the consumer, for opened documents, pasted drawings and
  placed pictures.
- **`audiodecode`** — a sound file decoded through
  [`tairix-sound`](./sound.md) by a long-lived worker over a supervised
  session. The owner hands over the file's length, never the file: the
  worker asks for the pages its decoder reads (`DecodeEvent::Need`, page
  aligned, at most `MAX_NEED_BYTES`), the owner reads and supplies them, and
  the worker answers the request it was serving. One request is outstanding
  at a time: open, decode (at most `MAX_BLOCK_FRAMES`), seek or next link,
  then any number of need and supply exchanges, then its one answer. A chained
  file is read a link at a time: an open names the link it opens and its
  answer states where that link begins, `next_link` asks for the one after
  the link that ended (`DecodeEvent::Opened` with its stream, or
  `DecodeEvent::LastLink`), and the client believes a link only where its
  place follows from what was asked — the link named, or the next of the one
  held, further into the file. The worker holds
  up to `CACHE_PAGES` pages in a least-recently-used cache keyed by a seed the
  owner draws (`rt::draw_seed`, the one draw every owner takes it from), so a
  file cannot choose which of its offsets collide. The
  bound is the decoders' own statement of what one decode reads at the
  limits (`tairix_sound::max_working_set`: an Opus seek's pre-roll in
  packets of the mapping's largest, at most) with the read-ahead behind it, and pages are held only as a
  file needs them; a need asks
  for the read that missed plus the pages a sequential decode reads next, so
  a stream costs one exchange a window. A decoder call that misses changes
  nothing and is run again once its pages arrive, and every page a request
  has read stays held until it is answered, so each retry progresses and a
  request needing more of the file at once than the cache holds is refused
  (`AudioRefusal::WorkingSetExceeded`) instead of asking for ever. The open
  answer's cover range is believed only inside the file's length; the owner
  reads the picture's bytes itself, so it never crosses the worker.
  `AudioDecodeClient` is the owner's side: it encodes every request, and
  believes a reply only once it holds against what was asked — a need inside
  the file and no more needs a request than its held pages can account for,
  a block at the stream's position of the frames asked for and no more than
  the stream states, stream facts of a real format, encoding, rate,
  channel map and sample format, metadata within `LIMITS` — and a reply that
  does not is `AudioDecodeError::Unbelievable`, for the owner to condemn the
  worker. After a replacement, `restart` opens the file again in it at the
  link the stream was in, refuses a stream that now reads as another, and
  returns the replacement to the stream's position by seeking or, for a stream
  that cannot seek, by decoding up to it; a stream that fails its worker at the same position twice is
  given up (`CannotResume`). `session_bounds` is the session an owner admits
  it under: one largest frame each way. `takes_request` says whether the
  client is free for a request — none outstanding or waiting to be sent, with
  a file open or none, which is all an open needs — and
  `supervise::send_waiting` is the one step every owner, of a sound or a
  movie, hands a request to its worker through: it queues the waiting frame
  and writes what the pipe takes, a write that cannot park, since a request
  goes only once the last is answered. The file is read through `paged`, the
  one page cache both decode workers share.
- **The spawn share** (`Share`, `ShareWrite` in `session`) — a region an
  owner hands its session worker at spawn (`SpawnAttach::with_share`), so the
  worker's output reaches the owner with no copy through the pipes. The owner
  reads it only by copying out of it (`Share::copy_out`), since the worker may
  be writing it. `rt::RtSessionLauncher::sharing` makes a fresh region for each
  worker it starts, kept by the transport and let go with it, so a
  replacement never writes where its predecessor's output is still read;
  `rt::spawn_share` maps it in the worker, whole. `loopback::HeapShare` and
  `LoopbackShareLauncher` stand in for both sides in host tests.
- **`moviedecode`** — a `WebM` recording decoded through `tairix-video` and
  [`tairix-sound`](./sound.md) by the same kind of long-lived worker, its
  file read through `paged` as a sound's is. One request at a time — open,
  next, sound or seek — then need and supply exchanges, then its answer. The
  pictures and the sound are walked through the file apart, each from its own
  place, so the sound runs far ahead of the pictures. The worker is handed a
  ring at spawn: `RING_SLOTS` slots, each the RGBA8 picture of the box the open
  states (`RingLayout`, the owner's screen). `next` draws the pictures' next
  unit into a slot the owner does not hold — at the stream's own size, scaled
  down only where it is larger than a slot — and answers a head of slot, size,
  time and keyframe; or it answers a picture decoded and not drawn because it
  was due before the instant asked, or the end. Each `next` states the slots
  the owner holds, which is how a slot is handed back: no frame of its own,
  nothing to cross the request it governs. `sound` answers a block of Opus
  sound as `F32` past the codec delay and short of the padding, or the end;
  `seek` moves both walks to the latest keyframe at or before a target, from
  the file's index or by walking it. The open states the worker's ceiling, and
  the worker weighs `movie_bytes` — the decoder at its peak, the ring, a
  picture too large for a slot at its own size, each walk's payload, the page
  cache, the sound's buffers — against it before reserving anything, refusing
  a stream too large (`MovieRefusal::TooLarge`) and an open with no ring its
  size (`NoRing`). `MovieDecodeClient` believes a head only where it names a
  slot of the ring the request left free and a size a slot holds, before a
  pixel is read, and `restart` brings a replacement, in a ring of its own,
  back to the latest keyframe passed, giving up where it fails twice there.
  `is_movie` knows a recording by its signature; the `fixture` feature muxes
  recordings in memory for the tests of the crates that play one.
- **`rt`** (feature `program`, freestanding targets only) — the
  production transport. `RtLauncher` spawns the program's **own binary**
  in a worker role: two fresh pipes wired to the child's fd 0/1 through
  `SpawnAttach::sandbox`, the shared `--parser-sandbox-worker` argv
  marker, and a blocking reap on disposal. Every launcher names what its
  worker parses (`WorkerMemory`), and the worker is spawned under that kind's
  `AddressSpaceBytes` ceiling, its stack included, on one thread: a message parser its two frames
  and a baseline, a document decoder a quarter of the machine's RAM and never
  less than `DOCUMENT_FLOOR_BYTES` (64 MiB) of document beside them, and an icon
  and thumbnail decoder that floor of document and the most a thumbnail's
  decode may cost, on any machine. The document a worker may be handed is what
  its ceiling leaves beside its frames and, for an artwork decoder, its
  thumbnail (`WorkerMemory::document_bytes`, `rt::document_room`, and the
  launcher's `document_room`): a document grows with the machine, and none is
  refused for a figure fixed below what the worker could hold. An
  `RtLauncher` reads the machine's RAM once, so the ceiling its workers run
  under and the document they are handed are always one share. A
  program pooling artwork decoders runs as many as fit in one document
  decoder's share and never fewer than one (`WorkerMemory::pooled`), so a pool
  of more than one leaves the machine what one decoder would; below 512 MiB of
  RAM, where none fits, the one may map more. A decode that outgrows it ends with
  the runtime's memory-bound exit, which the host reports as
  `WorkerEnd::OverBound`: not a shortage, so not worth asking again. The worker side
  (`worker_role` + `serve_stdio`) serves over fd 0/1 — exactly the
  surface the kernel sandbox allow-list admits — and ends with
  `ServeEnd::exit_code`, the one mapping of how its conversation ended to
  the status it exits with. A program whose binary serves more than one
  one-shot service gives each a worker of its own, so no two parsers share an
  address space: `RtLauncher::serving(name)` passes the service's name after
  the role marker, and the worker reads it back with `serves(name)`; a worker
  asked for a service its binary does not serve exits `WorkerExit::Unserved`,
  which the host reads as its parent breaking the protocol. `RtSessionChannel` is the
  duplex transport over that same spawn (one shared pipe-pair-and-attach
  path, `tairix_rt::spawn_piped`, which the login broker's wired launch
  shares; `--sandbox-session-worker`), reporting its two descriptor numbers
  so the owner can register them, and `session_worker_role` +
  `serve_session_stdio` are its worker half. Ending a session kills its
  worker and reaps it at once if it has gone; one still running is handed to
  the channel's `Reaper` where the owner gave it one (`reaped_by`), so a loop
  that answers a person never waits for a worker's teardown, and is waited
  for otherwise (`let_go`). A channel taken up from pipe ends another
  process handed over (`RtSessionChannel::redeemed`) only closes them as it
  ends: its peer is that process's to end and reap.

## Security posture

The seam adds no authority: the worker holds only the two pipe ends its
parent wired at spawn, and the kernel enforces the capability-empty brand
and the syscall allow-list. Nothing a worker replies is trusted beyond
the frame bound and the typed field validation, and the request payloads
never carry secrets or capability tokens.

## Testing

Unit tests cover the framing (round-trips, every truncation point,
oversize both ways), the serve loop, the containment discipline (typed
error, reap, replacement, logged events, frozen event ids, Drop
disposal), and hostile-reply refusal. The session seam adds its own:
ordered duplex exchange, a frame split across reads, fan-out, both send
refusals leaving the queue byte-identical, the readiness transitions in
each direction, clean end-of-stream against mid-frame truncation, an
oversize worker declaration refused before it is copied, containment
(disposed, logged, latched, never replaced), and a ten-thousand-round-trip
run asserting neither queue grows. The `fuzz_sandbox` harness (in
`cargo xtask fuzz`) drives mutated containers, pure noise under every
ISA, a hostile worker framing noise as replies, and the session's inbound
codec over the same noise a byte-run at a time — all through the public
client path. The aarch64 QEMU vertical
(`tests/integration/sandbox_program` + `sandbox_qemu_aarch64`) proves the
whole seam over the real syscalls: sandboxed decode of valid and
malformed inputs, real-process crash containment with a surviving caller,
the syscall wall probed from inside a live sandbox, and a duplex session
driven entirely from a wait-set that pushes twice a pipe's worth of frames
at a worker answering none of them — which completes only if the
`StreamRoom` wake fires. The audio decode client's turns are tested in every
state it can be in, the send step for a frame queued, none waiting and no
worker to take it, and the pipe model for a pipe's room a write and a read of
an empty pipe counted; the engine, the tag reader and the cue authority each
run over it.

Every client is held to fallibility by an allocation sweep: its whole
exchange is run with this side's heap refused at each of its requests in
turn (`tairix_fuzzseed::meter::refused_at_each`), and every run must complete
or be refused as want of this side's memory, holding nothing past its answer.
A request/reply client's worker runs on a thread of its own
(`testing::swept`), whose allocations the per-thread meter never refuses; a
session client replays the frames a real worker sent once
(`testing::Recorder`, `Replay`), so no worker allocates beside it.
