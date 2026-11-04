# BROWSER.md — the first-party web browser

Binding under `AGENTS.md`. TAIRiX ships no web browser. This plan owns one:
first-party Rust, written from the web's specifications onto TAIRiX's capability
model, with a JavaScript engine whose interpreter is the permanent base every
compiled tier is added to.

Read first (§15.18): `AGENTS.md` §2.12 (roll your own), §2.16 (performance),
§5.4 (entry points), §19.2 (W^X and `CAP_JIT_MAP_EXEC`), §19.3 (supply chain),
§19.5 (parser sandboxing), §19.11 (the oracles), §26 (operating conditions),
§27 (complete, not minimal), §28 (interactive surfaces); `plans/SSH.md`
§1.1–§1.3 (a monitor that parses nothing, owns the key exchange, and consumes a
parse it did not perform), `docs/src/security/sandbox.md` (the allow-list and
the drawing sandbox), `plans/UNIVERSAL.md` U2 (the Wasm validator and
compiler), `plans/RUST.md` R5 (one capability ceiling per bundle),
`plans/APPDATA.md` §3.8 (an app's bulk data and its bounds), `plans/GPU.md` §8,
`plans/SVG.md` S27, `plans/FONT-SERVICE.md`, `plans/SAVE-DIALOG.md`,
`plans/FIX-DESKTOP.md`, `plans/RECDB.md`, `plans/SOUND.md`.

## Ledger

| # | Item | Status |
|---|---|---|
| BR0 | This plan, its jump-sheet row, its `PLAN.md` row, and the pointers from `plans/TELNET.md` T2, `plans/SVG.md` S27 and `plans/NETMAP.md` NM2e | done |
| BR1 | `lib/tls`: the pure TLS client engine, split so the key exchange, the trust decision and record protection run where nothing is parsed; TLS 1.3, and 1.2 with ECDHE and AEAD suites only; X.509 path validation over `lib/der` against the vendored root store; Certificate Transparency; revocation from a pushed data set | planned |
| BR2 | The HTTP client engine in `lib/net`: HTTP/1.1 persistent connections and HTTP/2 with HPACK and flow control, as strict as today's reader; Brotli (RFC 7932) in `lib/compress` | planned |
| BR3 | The shared text engine: OpenType `GSUB`/`GPOS` shaping with the complex-script shapers in `lib/fontface`; UAX #9 bidi, UAX #14 line breaking and UAX #29 segmentation in `lib/unicode`; font fallback by script coverage | planned |
| BR4 | `lib/url`: the WHATWG URL parser and serialiser, UTS #46 IDNA, the vendored Public Suffix List; and the desktop URL hand-off — a scheme a bundle's manifest declares it opens, and an open request that carries a URL | planned |
| BR5 | Renderer spawns: the browser's renderer admitted into a drawing spawn, and shared regions handed to a sandboxed child after spawn | blocked: decision D2 |
| BR6 | The process model: the broker, the protocol workers, the per-site renderers, the internal protocol between them, and crash containment | blocked: decisions D1 and D2 |
| BR7 | The conformance harnesses: test262 and the web-platform-tests at pinned, hash-checked revisions, run by `cargo xtask`, the passing set recorded so a regression fails the gate | planned |
| BR8 | HTML and the DOM: the WHATWG tokenizer and tree builder, the DOM core, and the engine heap — value encoding, shapes, barriers, safepoints — that the DOM and JavaScript share | planned |
| BR9 | CSS: syntax, selectors, the cascade with layers and custom properties, computed values, media queries, incremental restyle | planned |
| BR10 | Layout: block and inline formatting, floats, positioning, flexbox, incremental relayout | planned |
| BR11 | Paint: display lists, tiled CPU raster through `lib/raster`, images through `lib/image`, web fonts decoded in the renderer, system fonts by relay as `lib/saver` relays them, composited scrolling | planned |
| BR12 | The browser application: windows, tabs, the address bar and its spoofing defences, history, bookmarks, downloads through the Save panel, site permissions, error pages — a complete scripting-disabled browser | planned |
| BR13 | The JavaScript front end and the tier contract: the parser, scope analysis and bytecode compiler; the bytecode, frame layout, feedback slots and interrupt checks every tier shares | planned |
| BR14 | The JavaScript runtime: the interpreter, inline caches, the ECMA-262 built-ins, the regular-expression engine, generators, async functions and modules | planned |
| BR15 | Web bindings and the event loop: the WebIDL binding generator, tasks, microtasks and rendering steps, DOM events, timers, script loading | planned |
| BR16 | Fetch and web security policy: the Fetch Standard, the same-origin policy, CORS, opaque-response blocking, CORP/COEP/COOP, mixed-content blocking, HSTS with the preload list, CSP, Referrer-Policy, partitioned cookies | planned |
| BR17 | Storage: Web Storage, IndexedDB over `lib/recdb`, the Cache API, the HTTP disk cache, per-site quotas and clearing | blocked: decision D3; `plans/RECDB.md` RD1–RD6 |
| BR18 | Workers: dedicated, shared and service workers, `MessageChannel`, structured clone and transfer, `SharedArrayBuffer` and `Atomics` under cross-origin isolation | planned |
| BR19 | ECMA-402 `Intl` over vendored CLDR data | planned |
| BR20 | Layout breadth: tables, grid, multi-column, writing modes and vertical text | planned |
| BR21 | Graphics: Canvas 2D over `lib/raster`, SVG in the DOM, transforms, transitions and animations on the compositor | planned |
| BR22 | Media: `<audio>`, `<video>`, Media Source Extensions and Web Audio; decode in the sandbox; AV1 and VP9 decoders beside `lib/sound`'s audio codecs | planned |
| BR23 | Network breadth: WebSockets, server-sent events, HTTP/3 over QUIC | planned |
| BR24 | Accessibility: the page's accessibility tree exposed to the desktop | blocked: the desktop has no accessibility interface |
| BR25 | Developer tools: the inspector, the console, the network log, and a debugger on the interpreter | planned |
| BR26 | Code installation: the kernel primitive that publishes a compiler-filled region read-execute into exactly one renderer by an atomic flip, its `CAP_JIT_MAP_EXEC` gate, and the compiler sandbox | planned |
| BR27 | WebAssembly: `plans/UNIVERSAL.md` U2's validator and compiler run in the compiler sandbox at load time, the JavaScript API, and Wasm GC objects in the engine heap | blocked: `plans/UNIVERSAL.md` U2 |
| BR28 | The baseline JIT: one machine-code template per bytecode, no speculation, frames identical to the interpreter's, backends for x86_64, aarch64 and riscv64 | planned |
| BR29 | An optimising tier | blocked: a measured case that BR28 is not enough, and an approved machine-checked verification method |

Items are built in ledger order, with two refinements. Shared surface lands with
its first consumer, never ahead of it (§2.3): BR1 with `plans/TELNET.md` T2 or
BR6, whichever comes first; BR3 with `plans/SVG.md` S27 or BR10; BR2, BR4 and
BR5 with BR6; BR7 with BR8, test262 joining it with BR13. And an item is
complete — tests, fuzz targets, docs, green gate — before an item that depends
on it starts.

## 0. Binding decisions

1. **First-party, from the specifications.** Nothing from Chromium, WebKit,
   Gecko, V8, SpiderMonkey or JavaScriptCore is ported, translated or linked:
   they are C++ (§1), and a translated fork could not take its upstream's weekly
   security fixes (§2.17). Servo's Rust components are not adopted either:
   §2.12 exempts only cryptography, and Servo's JavaScript engine is
   SpiderMonkey. The engine is written from the WHATWG and W3C standards,
   ECMA-262, ECMA-402 and the IETF RFCs, and measured by test262 and the
   web-platform-tests (BR7).

2. **Authority and parsing never share a process.** A bundle's processes share
   one capability ceiling (`plans/RUST.md` R5), so the separation that counts is
   between the one process that holds authority and parses nothing, and the
   sandboxes that parse and hold nothing.

   | Process | How many | Does | Holds |
   |---|---|---|---|
   | Broker (`Run`) | one | windows, tabs and the address bar; sockets and the cookie jar; each connection's key exchange, certificate decision and record protection; the profile's stored state | the bundle's grant: `CAP_NET`, its app-data store, sandbox spawns |
   | Protocol worker | one per network partition and destination site | decodes DNS replies, TLS handshake messages, certificates, HTTP/1.1, HTTP/2 and HPACK | nothing: a canonical sandbox |
   | Renderer | one per site | HTML, CSS, layout, paint and JavaScript; decodes images, fonts and media | nothing: a drawing sandbox |
   | Compiler | one per renderer, from BR26 | compiles WebAssembly and JavaScript | nothing: a canonical sandbox |

   - **The broker parses nothing but its own protocol with its sandboxes**, as
     `sshd` parses nothing but its control protocol (`plans/SSH.md` §1.1). Of
     network bytes it reads only the fixed five-byte TLS record header,
     bounds-checked, before an AEAD open authenticates the record. A sandbox
     reports typed records whose every field the broker validates, re-encoding
     and comparing wherever a record claims to describe bytes the broker holds
     (`plans/SSH.md` §1.3).
   - **The broker owns each connection's key exchange and trust decision**, as
     the SSH monitor owns its exchange hash (`plans/SSH.md` §1.2). It draws the
     ephemeral key, keeps the transcript hash, verifies the server's signature
     over it, and validates the chain from the worker's re-encoded parse. A
     compromised worker cannot make an untrusted certificate trusted.
   - **The broker keeps every key.** It seals and opens each record itself, so
     a worker parses only plaintext it is given and can put nothing on the wire
     the broker did not encrypt. This differs from SSH, whose worker holds its
     session keys, because a browser's worker is reused across connections: one
     compromised before a handshake must not read the next connection's
     secrets. Record protection runs on the broker's own threads, sized from
     the discovered CPU count and never on the window loop (§24, §28).
   - **Workers are keyed by network partition and destination site**, the key
     connection pools and partitioned cookies already use, so a compromised
     worker reaches only what one destination serves within one partition.
     Their number is bounded, idle ones are reaped, and requests beyond the
     bound queue (§24).
   - **A site is a scheme and a registrable domain** (the Public Suffix List,
     BR4). Every frame renders in its own site's renderer, cross-site frames
     included, from BR6 on, and the broker composites a page from its
     renderers' surfaces. Process isolation is the boundary
     speculative-execution attacks respect, and retrofitting it into a
     shared-process engine is the rewrite this plan is shaped to avoid.
   - **Cookies are the broker's.** It attaches them to requests, so an
     `HttpOnly` cookie never reaches a renderer.
   - **Every helper is the browser's own binary**, re-entered in its role
     through `SPAWN_SELF` (`docs/src/security/sandbox.md`).
   - **A crash is contained.** A sandbox that ends is reported in its tab, and
     the broker and every other site carry on (§2.24, §19.5). A renderer that
     stops answering is reported and can be ended.

3. **Memory safety by construction.**
   - The DOM and JavaScript share one precise, incremental, garbage-collected
     heap from BR8. A cycle between script and the DOM is ordinary garbage, and
     no node is reference-counted.
   - Engine code reaches the heap only through references branded to the
     collection context, so the borrow checker refuses one held across a
     collection point. A reference that must outlive one — held by the internal
     protocol, a timer, an external owner — is a generation-checked handle that
     fails its check when stale, as `kernel/mem`'s tagged slab handles do. A
     use-after-free becomes a refused lookup.
   - `unsafe` lives only in named cores — the collector, the shared surfaces and
     rings, the entry into installed code — each with its `// SAFETY:` proof,
     tests and miri enrolment; a concurrent core carries a loom model (§19.11).

4. **The heap contract, fixed in BR8 before any script exists.** The DOM is born
   in the heap JavaScript will use, so script arrives without converting it.
   - One 64-bit value encoding for the interpreter, compiled frames, the heap
     and the bindings.
   - Every object, DOM nodes included, has a shape with a transition tree, so
     script reaches a node without a wrapper layer.
   - The collector's write barrier and safepoints exist from the first
     collector, so every tier emits them and the collector can become
     generational or concurrent without a tier changing.

5. **The tier contract, fixed in BR13: the interpreter is permanent, and every
   compiled tier is an addition.**
   - **Bytecode is the contract.** Every tier compiles from it. The interpreter
     defines its meaning and is never removed: it runs cold code, every compiled
     frame falls back to it, it runs alone when compilation is off and on
     wasm32, and it is the oracle every tier is fuzzed against.
   - **Inline caches.** Property access goes through inline caches whose
     feedback lives beside the bytecode: the interpreter's speed, and every
     compiled tier's input.
   - **Explicit frames.** Frames live on a JavaScript stack whose layout
     compiled frames mirror; deoptimisation rebuilds interpreter frames from
     metadata the compiler emits; a generator or async function suspends by
     keeping its frame. Recursion is bounded by that stack's limit, never by the
     Rust stack.
   - **One interrupt word**, checked at every loop back-edge and call, so
     termination, the slow-script watchdog and collection requests reach every
     tier the same way.

6. **No code is generated in a renderer.**
   - No process ever holds a writable and executable mapping, and a renderer
     never holds the authority to make one.
   - Compiled code (BR27–BR29) is made by a compiler sandbox paired with one
     renderer, and published by BR26's atomic flip, which unmaps the region from
     the compiler and maps it read-execute into that renderer alone. The flip's
     gate, `CAP_JIT_MAP_EXEC`, is the broker's: it pairs the two and never reads
     the code. A compromised compiler reaches only the renderer it already
     serves.
   - Compiled code is never written to disk.
   - BR26 is the primitive `plans/UNIVERSAL.md` U2 needs too, and one code
     generator — encoders, register allocator, lowering — serves U2's compiler,
     BR27 and BR28. Its backend is chosen by the discovered architecture, never
     by a `cfg` (§17.2).

7. **Time and side channels.** Script-visible clocks are coarsened to the web
   platform's resolutions, beyond the kernel's 1 µs sandbox floor
   (`coarsen_clock_ns`), and `SharedArrayBuffer` exists only in
   cross-origin-isolated contexts (BR18).

8. **The broker's window loop never waits (§28).** It performs no network,
   storage or renderer round trip: it requests and collects
   (`plans/FIX-DESKTOP.md`). Scrolling, compositor animations and tab switching
   proceed while a page's script runs, and a renderer presents at most one frame
   per display present.

9. **Memory and scale (§26).** A renderer's caches are `lib/reclaim` classes
   under its budget. A background tab's timers are coalesced; under pressure its
   renderer is discarded and the tab reloads when shown. BR12's acceptance
   includes a QEMU vertical at the 1 GiB floor (§26.7).

10. **Privacy by default.** Cookies, storage, caches and connection pools are
    partitioned by top-level site, and there are no unpartitioned third-party
    cookies. The browser contacts nothing the user did not navigate to: no
    telemetry, and no remote lookup the user has not turned on.

11. **Downloads go through the Save panel** (`plans/SAVE-DIALOG.md`). The user
    places each file, and the panel's grant is the browser's only write outside
    its own store (§16.5). There is no `Downloads` folder: §16.3 fixes the
    home's shape, and adding one is a charter amendment.

12. **Updates and vendored data.** The browser updates only with the system
    (§19.3). The root store (Mozilla's root program, whose inclusion process is
    public), the Public Suffix List, the HSTS preload list and CLDR are vendored
    at pinned revisions, licence-checked, compiled at build time as the zone
    rules are (`plans/TIMEZONES.md`), and updated the same way.

## 1. Stages

- **Prerequisites (BR1–BR5).** OS facilities rather than browser parts: `lib/tls`
  is also `plans/TELNET.md` T2's transport, the text engine is also
  `plans/SVG.md` S27's, and the URL hand-off is also `plans/NETMAP.md` NM2e's.
- **The scripting-disabled browser (BR6–BR12).** Done, it conforms to the HTML
  Standard's *scripting disabled* mode: `<noscript>` content renders and no
  script runs, as that mode defines, so it is complete for what it claims. It
  proves the process model, the heap and site isolation before any script
  exists.
- **The scripted web (BR13–BR16).** Script reaches users only once BR16 is done.
  A browser that runs script without CORS, CSP and mixed-content blocking never
  ships, not even between items.
- **Breadth (BR17–BR25).** Independent of one another, each complete for the
  specifications it names.
- **Compiled code (BR26–BR29).** Additive by construction: nothing here changes
  what BR8 and BR13 fixed. A change one needs there is a defect in the contract,
  fixed in the contract for every tier.

## 2. Open decisions

- **D1 — Where the engine's crates live.** The engine is too large for one
  crate, and §17.4 lets a `userland/*` crate depend only on `lib/*`. One option
  is a leaf subtree, `userland/apps/browser/*`, whose crates compose each other
  and which nothing outside depends on. That is the shape §17.4 already grants
  `userland/games/*`, so it needs that carve-out extended and §3 amended. The
  other is `lib/*` crates, which put a single application's engine into the OS
  libraries' trusted base. Recommended: the leaf subtree. BR1–BR4 are OS
  facilities and live in `lib/*` either way. Blocks BR6.
- **D2 — The renderer's sandbox.** A renderer needs a drawing spawn, and the
  load gate admits only the `screensaver` kind into one
  (`docs/src/security/sandbox.md`, `plans/SCREENSAVERS.md`). It also needs
  shared regions after spawn: a surface that grows with its window, and response
  bodies handed over rather than copied through pipes. Today a sandbox maps
  exactly one region, at spawn. Recommended: admit an application's own binary
  into a drawing spawn it starts itself, and add one allow-listed call that maps
  a region the spawner granted to that child's attested process, as `fd_grant`
  binds a delegation (`plans/APPDATA.md` §3.8). Both widen the sandbox and need
  the capability-minimalism review. Blocks BR5.
- **D3 — Room for web storage.** An app's bulk data is bounded at
  `APPDATA_BLOB_MAX_COUNT` (64) blobs of at most `APPDATA_BULK_FILE_MAX_BYTES`
  (64 MiB) (`plans/APPDATA.md` §3.8). These are fixed containment bounds, and
  they cannot hold an HTTP cache and per-site IndexedDB. Either
  `plans/APPDATA.md` gains a bulk scope with a per-app quota the app declares
  and the user sees and clears in Settings, or web storage is held to what the
  bounds allow. Recommended: the quota scope. Blocks BR17.
- **D4 — GPU APIs for pages.** `plans/GPU.md` §8 refuses implementing WebGPU
  conformantly and any GL-shaped layer, so this plan carries neither. WebGPU's
  object model is `lib/gpu`'s and its shading language is `plans/SHADER.md`
  SH5's; WebGL is an OpenGL ES state machine. Recommended: amend §8 to admit a
  conformant WebGPU inside the browser, and keep WebGL refused, knowing that
  sites which need WebGL will not work. Deciding for WebGPU adds a row.
- **D5 — Patent-encumbered codecs.** H.264, HEVC and AAC need patent licences;
  AV1, VP9, Opus and Vorbis do not. Some sites serve only H.264. Recommended:
  royalty-free codecs only (BR22) unless licensing is arranged.

## 3. Readings this plan relies on

- **`plans/UNIVERSAL.md` §3 refuses "a bespoke TAIRiX bytecode or first-party
  VM/verifier"** as the form applications are distributed in. The JavaScript
  engine's bytecode is not that: the engine produces it from source, keeps it in
  memory, and never accepts it as input or writes it out. WebAssembly in pages
  goes through U2's validator and compiler, not a second Wasm engine.
- **§19.3 forbids the kernel, drivers, the installer and system services from
  fetching executable code.** The browser is an application, and the code it
  fetches runs only in capability-empty renderers. It is never installed, and
  never stored as native code.

## 4. Refused by name

- Porting, translating or linking any existing engine (decision 1).
- Code generation in a renderer, a writable-and-executable mapping in any
  process, and compiled code on disk (decision 6).
- A conservative or reference-counted collector, and a DOM outside the engine
  heap (decision 3).
- An optimising tier without the measurement and verification BR29 names.
- Native plugins and native-code extensions.
- Content-decryption modules (EME): Widevine is a closed per-platform binary
  that is not offered for TAIRiX, and the browser runs no code it cannot build
  and audit.
- WebGL; and WebGPU until D4 is decided (`plans/GPU.md` §8).
- H.264, HEVC and AAC until D5 is decided.
- Unpartitioned third-party cookies, and telemetry.
- An update channel of the browser's own (§19.3).

## 5. Verification (binding)

- Every parser gets its fuzz target in the change that adds it (§19.6): the HTML
  tokenizer and tree builder, CSS, the JavaScript parser, the
  regular-expression compiler, URLs, TLS records and handshake messages,
  certificates, HTTP/1.1, HTTP/2 and HPACK, Brotli, WOFF2, and the internal
  protocol at every process boundary.
- test262 and the web-platform-tests (BR7) gate every engine item: a test that
  passed and now fails is a regression (§2.5).
- Every compiled tier is fuzzed differentially against the interpreter — the
  same program, its observable results compared — and every backend's encoder
  is round-tripped through `lib/disasm`.
- Every `unsafe` core is enrolled in miri; the shared-memory rings and any
  concurrent collector phase have loom models (§19.11).
- QEMU verticals on every Tier-1 target run the process model end to end: a page
  loads, a cross-site frame renders in its own renderer, and a crashed renderer
  is contained.
- Each item updates its rustdoc, `docs/src/userland/browser.md` and the
  `README.md` support matrix, and ends with the whole-project gate (§2.15).

## 6. Cross-references

- `plans/SSH.md` — the broker's privilege separation, key-exchange ownership,
  and re-encode-and-compare.
- `docs/src/security/sandbox.md`, `plans/SCREENSAVERS.md` — the sandbox and the
  drawing spawn BR5 extends (D2).
- `plans/UNIVERSAL.md` — U2's validator and compiler (BR27) and the code
  installation primitive (BR26).
- `plans/RUST.md` — R5's single capability ceiling per bundle.
- `plans/APPDATA.md` — the browser's store and its bounds (D3).
- `plans/GPU.md`, `plans/SHADER.md` — the refusals behind D4, and the WGSL
  front end.
- `plans/SVG.md` S27, `plans/FONT-SERVICE.md` — the text engine's other
  consumer, and system fonts.
- `plans/TELNET.md` T2, `plans/NETMAP.md` NM2e — consumers of BR1 and BR4.
- `plans/RECDB.md` — IndexedDB's store (BR17).
- `plans/SOUND.md` — audio decoding and output (BR22).
- `plans/SAVE-DIALOG.md`, `plans/FIX-DESKTOP.md` — downloads, and the loop that
  never waits.
- `plans/TIMEZONES.md` — the vendored-data pattern.
