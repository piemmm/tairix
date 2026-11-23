//! `cargo xtask fuzz` — drive the in-tree fuzz harnesses.
//!
//! TAIRiX does not pull in an external fuzz runner: the
//! per-crate harnesses are seeded, allocation-free Rust tests, which the
//! charter sanctions as the "equivalent in-tree harness". This
//! orchestrator is the single place that runs every such harness for a
//! wall-clock budget, so a PR and a nightly soak share one definition of the
//! target set.
//!
//! Each [`Target`] names an existing `cargo test` integration harness. The
//! orchestrator exports `TAIRIX_FUZZ_BUDGET_SECS`, which the harness reads to
//! keep drawing fresh inputs from its continuing PRNG stream until the budget
//! elapses (a plain `cargo test` leaves the variable unset and runs the fast,
//! fixed-iteration smoke sweep instead). It also exports a per-harness PRNG
//! seed (`commands::seed`): by default a *fresh* seed each run, so two soaks
//! never replay the same input stream; with `--seed N`, a deterministic
//! seed that reproduces a logged crash. The chosen seed is logged with each
//! job. A harness that crashes, hangs, or fails its invariant fails the
//! command: it fails closed.
//!
//! Adding a harness means adding a [`Target`] here, never teaching `ci`
//! about it directly. The burn-down now covers the wire decoders,
//! the syscall dispatcher, the `lib/net` protocol parsers, the
//! capability-checked IPC port endpoint, and the realm protocol's decoders,
//! handshake, and record transport.

use std::ffi::OsString;

use crate::commands::parallel::{self, Job};
use crate::commands::seed;
use crate::commands::soak::{self, Budget, Flags, Mode};
use crate::Context;

/// One in-tree fuzz harness the orchestrator knows how to run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// Workspace package that owns the harness (`cargo test -p`).
    pub package: &'static str,
    /// Integration-test binary name (the `tests/<name>.rs` file stem).
    pub test: &'static str,
    /// One-line description shown by `cargo xtask fuzz --list`.
    pub description: &'static str,
}

/// The closed set of fuzz harnesses, in run order.
///
/// Every entry is a deterministic `cargo test` harness; this registry is
/// what wires them into the budgeted runs. The set covers the wire
/// decoders (`lib/abi`), the syscall dispatcher, the `lib/net`
/// protocol parsers, and the capability-checked IPC port endpoint.
pub const TARGETS: &[Target] = &[
    Target {
        package: "tairix-arch-api",
        test: "fuzz_backtrace",
        description: "Arch HAL frame-pointer stack unwinder (panic backtrace)",
    },
    Target {
        package: "tairix-arch-x86_64",
        test: "fuzz_acpi",
        description: "x86_64 DMA-remapping tables firmware hands the port (DMAR, IVRS, VIOT)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_decode",
        description: "lib/abi wire decoders (IPC, manifest headers, the spawn attach block)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_audio",
        description: "lib/abi audio wire surface (audio-v1 client protocol, audiochan-v1 device channel, the PCM ring's peer-written positions)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_dmaengine",
        description: "lib/abi dmaengine-v1 surface (controller-endpoint requests, their replies, the DMA request-line and controller-duty records)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_clock",
        description: "lib/abi clock-v1 surface (clock-controller requests naming a clock link, and their replies)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_i2c",
        description: "lib/abi i2c-v1 surface (I2C controller requests naming a target or bus link, their transactions, and their replies)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_encoder",
        description: "lib/abi encoder-v1 surface (encoder requests naming an encoder link, their timings, and the connector states replied)",
    },
    Target {
        package: "tairix-edid",
        test: "fuzz_edid",
        description: "lib/edid sink descriptions (EDID base blocks and their CTA-861 and DisplayID extensions, block counts, HD Audio ELDs)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_irqline",
        description: "lib/abi irqline-v1 surface (interrupt-line waits naming a line link, their replies) and the driver's own node record",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_cooling",
        description: "lib/abi cooling surface (thermal-sensor-v1 reads, gpio-header-v1 requests and line tables, the thermal-v1 status)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_codec",
        description: "lib/abi codec-v1 surface (codec requests naming a codec link, and their replies)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_gpu_channel",
        description: "lib/abi gpu-v1 frame (requests, replies to every op, region spans, the served-GPU record)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_net_echo",
        description: "lib/abi SocketEcho delivery decoder (echo replies and ICMP errors, the reply hop limit in the frame's spare byte)",
    },
    Target {
        package: "tairix-netmap",
        test: "fuzz_probe",
        description: "netmap active reachability probe engine (arbitrary answer/outcome/hop-limit/token streams; bounded in-flight, termination)",
    },
    Target {
        package: "tairix-abi",
        test: "fuzz_desktop_layer",
        description: "lib/abi desktop-layer surface wire surface (the capability-gated requests, the two feed events, the terrain reply)",
    },
    Target {
        package: "tairix-audio",
        test: "fuzz_engine",
        description: "lib/audio engine (sample conversion, the channel matrix, the mixer's shared accumulator, the resampler, the clock fit, and the volume model) over hostile samples, gains, layouts and ratios",
    },
    Target {
        package: "tairix-collections",
        test: "fuzz_collections",
        description: "lib/collections heap-backed tier (hash map, recency map, range map and set, spilling small vector) against naive models",
    },
    Target {
        package: "tairix-obey",
        test: "fuzz_obey",
        description: "lib/obey: hostile strings, expressions, templates and Obey files through GSTrans, the evaluator, OS_ReadArgs, OS_SubstituteArgs and the read-only reading; escapes round-trip, arithmetic agrees",
    },
    Target {
        package: "tairix-openbasic",
        test: "fuzz_openbasic",
        description: "lib/openbasic: hostile text tokenised and hostile tokens listed in BASIC II and BASIC V under every LISTO, bounded however deep the structures; hostile program images read, and read lines imaged back",
    },
    Target {
        package: "tairix-riscos-arm",
        test: "fuzz_arm",
        description: "userland/apps/tairix-bridge/riscos/arm: any word decoded on every profile, encoded back exactly and given its text; any program run from any registers and mode without leaving a mode the profile lacks, and over the guest MMU without writing a host page or storing into a watched granule unreported",
    },
    Target {
        package: "tairix-riscos-arm",
        test: "fuzz_fpa",
        description: "userland/apps/tairix-bridge/riscos/arm: the FPA's every operation, load and store on any register values and FPSR, trapping only where enabled; any FPA program run from any state",
    },
    Target {
        package: "tairix-riscos-basic",
        test: "fuzz_numbers",
        description: "userland/apps/tairix-bridge/riscos/basic: any bytes read as a number by BASIC V and BASIC VI without reading past them; every operation and function on any operands, BASIC VI's under any floating-point status, answering or faulting; any value laid out under any @% in bounded text, STR$'s own layout reading back as the value",
    },
    Target {
        package: "tairix-riscos-basic",
        test: "fuzz_basic",
        description: "userland/apps/tairix-bridge/riscos/basic: hostile programs of BASIC's keywords and operands, or any printable text, run by BASIC V and BASIC VI to an end, a wait or their budget; hostile assembler statements under any OPT raising an error or moving P% and O% on by no more than a string after an alignment",
    },
    Target {
        package: "tairix-riscos-arm",
        test: "fuzz_simdfp",
        description: "userland/apps/tairix-bridge/riscos/arm: any VFP or Advanced SIMD instruction, A32 or T32, run from any register file, FPSCR and mode with the unit granted or withheld; the FPSCR keeps its reserved bits clear and its flags only gather; any built with hostile numbers encoded, shown and judged without a panic",
    },
    Target {
        package: "tairix-riscos-arm",
        test: "fuzz_mmu",
        description: "userland/apps/tairix-bridge/riscos/arm guest MMU: random runs of frame, mapping, space, watch and access operations over TLB-aliasing pages and frame numbers past any limit, each answer held to a TLB-free reference model",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_areas",
        description: "userland/apps/tairix-bridge/riscos/kernel address space: random slot, dynamic-area, sparse and pool calls with hostile arguments under handlers that answer at random and call back in; every page in use stays accounted for and every reported page mapped",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_cli",
        description: "userland/apps/tairix-bridge/riscos/kernel command line: random lines through OS_CLI — the kernel's commands and abbreviations with hostile parameters, aliases with random templates, prefixes, redirection groups, GSTrans escapes, conditionals and overlong lines; every refusal a value, nothing endless past the host's word, the interpreter left to run the next line",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_international",
        description: "userland/apps/tairix-bridge/riscos/kernel international surfaces: hostile message files and ResourceFS blocks through every MessageTrans call, the Territory Manager's conversions, collation and symbols over hostile strings, times and formats, and key sequences through the key handler in every keyboard's layout and alphabet; every refusal a value, nothing written past a call's room, the kernel still answering",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_modules",
        description: "userland/apps/tairix-bridge/riscos/kernel module chain: module images with random headers, entries, service tables, decoding tables and squeezed data linked through OS_Module, then random reasons, service calls and module SWIs; nothing endless, the kernel answering, the chain walking to its end",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_squeeze",
        description: "userland/apps/tairix-bridge/riscos/kernel squeezed-module expander: the corpus's squeezed modules mutated, and noise carrying a squeezed length, expanded under random bounds; every refusal a value, every expansion whole pairs of words within its trailer's size and the bound, the same each time",
    },
    Target {
        package: "tairix-riscos-kernel",
        test: "fuzz_swis",
        description: "userland/apps/tairix-bridge/riscos/kernel SWIs: random SWIs with hostile registers (pointers into application space, at the top of memory, nowhere, misaligned) and random guest code salted with SWIs; every refusal a value, nothing endless past the host's word, the kernel still answering",
    },
    Target {
        package: "tairix-riscos-vdu",
        test: "fuzz_vdu",
        description: "userland/apps/tairix-bridge/riscos/vdu: hostile VDU streams — every control code with any parameters, modes no monitor shows, windows off the screen or inside out, shapes reaching the ends of the coordinate range — and the drivers' SWIs, OS_Byte and OS_Word reasons with hostile registers and blocks; every refusal a value, every error block readable, the drivers still answering",
    },
    Target {
        package: "tairix-riscos-vdu",
        test: "fuzz_sprites",
        description: "userland/apps/tairix-bridge/riscos/vdu sprites: every OS_SpriteOp reason with each naming over areas whose blocks and sprites are salted, factors, matrices, parallelograms and tables pointing anywhere, output switched to sprites, the JPEG SWIs over JPEGs with bytes changed, PLOT &E8-&EF, VDU 23,27 and the SpriteUtils commands, sprite files loaded and merged whole, salted or noise, and control block words made hostile under the calls after; every refusal a value, every error block readable, the drivers still answering",
    },
    Target {
        package: "tairix-riscos-vdu",
        test: "fuzz_screenmodes",
        description: "userland/apps/tairix-bridge/riscos/vdu ScreenModes: hostile monitor definition files — a good file's lines dropped, repeated, reordered or garbled, unknown keys, numbers past any range, modes past the most kept, or random bytes — loaded with *LoadModeFile, then ScreenModes' SWIs at every offset with hostile registers, the screen modes enumerated and selected, and the mode extension service with hostile selectors; every refusal a value, every error block readable, the drivers still answering",
    },
    Target {
        package: "tairix-riscos-vdu",
        test: "fuzz_colourtrans",
        description: "userland/apps/tairix-bridge/riscos/vdu ColourTrans, InverseTable and BlendTable: every SWI with hostile registers over salted palettes, calibration tables and buffers pointing anywhere but the kernel's own memory, sizes asked and translucencies past any range, in screens of every depth, and *ColourTransMap, *ColourTransMapSize and *ColourTransLoadings with hostile numbers; every refusal a value, every error block readable, the drivers still answering, the modules re-initialised holding none of the RMA they took",
    },
    Target {
        package: "tairix-riscos-draw",
        test: "fuzz_draw",
        description: "userland/apps/tairix-bridge/riscos/draw Draw: every SWI, and DrawV at any index, with hostile registers over salted paths — codes, orders and continuations broken, points at the ends of the coordinate range — cap and join blocks, dash patterns of any count and length, matrices and output buffers pointing anywhere but the kernel's own memory, in screens of every depth; every refusal a value, a call asking endless work stopped by its host, every error block readable, the drivers still answering, the module re-initialised holding none of the RMA it took",
    },
    Target {
        package: "tairix-riscos-draw",
        test: "fuzz_drawfile",
        description: "userland/apps/tairix-bridge/riscos/draw DrawFile: Draw files of every object type with hostile fields — sizes past what holds them, font tables twice or naming nothing, sprites of any mode, groups and tagged objects nesting deep, text areas with any commands — or a sound file's bytes changed, rendered, measured and their fonts declared under hostile flags, matrices, clips and lengths, and *Render with hostile arguments; every refusal a value, a call asking endless work stopped by its host, every error block readable, the drivers still answering, the modules a render reaches re-initialised holding none of the RMA they took",
    },
    Target {
        package: "tairix-riscos-filing",
        test: "fuzz_hostfs",
        description: "userland/apps/tairix-bridge/riscos/filing HostFS relay: hostile requests from the session worker against the server, over a tree with roots and files beside them, each answered by a reply that answers it and nothing changed outside the writable root; every message that decodes re-encoding to its own bytes; a session whose relay fails or damages its replies still answering",
    },
    Target {
        package: "tairix-riscos-filing",
        test: "fuzz_ramfs",
        description: "userland/apps/tairix-bridge/riscos/filing FileCore over the RAM disc: its map, root and pages scribbled on as a privileged guest may, then saves, loads, opens, transfers, extents, renames, wipes, listings and namings on hostile names; and files run by *Run and *Obey with hostile lines and parameters, an Obey file obeying itself; every refusal a value, never an abort, the free space within the disc, the session still answering",
    },
    Target {
        package: "tairix-riscos-filing",
        test: "fuzz_bootcommands",
        description: "userland/apps/tairix-bridge/riscos/filing BootCommands and the boot sequence: every command on hostile parameters (quotation marks, keywords in odd places, sizes past what a size holds, walks that become the application), and the boot over a user's own disc whose !Boot holds hostile Obey files, programs and directories; every refusal a value, every program run to its end and the boot going on after it, every kept failure naming its line, nothing outside the own disc changed, the session still answering",
    },
    Target {
        package: "tairix-softfloat",
        test: "fuzz_decimal",
        description: "lib/softfloat decimal conversion and the FPA's packed decimal: any value and any words, their brackets and round trips",
    },
    Target {
        package: "tairix-gpu",
        test: "fuzz_gpu",
        description: "lib/gpu submission model on the software device: copies, fills and updates over buffers sharing allocations against a byte-array reference; image descriptions of every shape, and the copies and clears of images against a texel reference; bind sets, dispatches and passes, direct and indirect, sound or carrying faults from a stated catalogue, each refusal at its first faulty command, each admitted one alike on a twin of several participants drawing every indirect record directly",
    },
    Target {
        package: "tairix-gpu",
        test: "fuzz_served",
        description: "lib/gpu served device over a loopback channel: the submission sweeps through gpu-v1 against their references, and the requests a client sent sent again, changed and by another client, each answered once and leaving nothing held",
    },
    Target {
        package: "tairix-gpu",
        test: "fuzz_shader",
        description: "lib/gpu shader interpreter over generated compute modules (arithmetic of every kind, vectors, selections and data-dependent loops with early breaks, calls, barriers, subgroup operations, memory reached past every object) and their admitted mutants: total, never faulting, writing nothing outside its range, the same bytes on one core and several",
    },
    Target {
        package: "tairix-spirv",
        test: "fuzz_spirv",
        description: "lib/spirv decoder and validator over mutated shader modules (bits, boundary words, insertions, deletions, truncations, either byte order): both total, refusals within the module, accepted modules re-encoding exactly, validation deterministic, admitted entry points agreeing with their own layout; and the builder over random structured programs, every one admitted and built the same twice",
    },
    Target {
        package: "tairix-fontface",
        test: "fuzz_face",
        description: "lib/fontface TrueType parser + outline rasteriser",
    },
    Target {
        package: "tairix-hash",
        test: "fuzz_hash",
        description: "lib/hash keyed SipHash-1-3 + XXH64 over arbitrary bytes and chunkings",
    },
    Target {
        package: "tairix-inline",
        test: "fuzz_inline",
        description: "lib/inline allocation-free tier (bounded vector and string, rings) against naive models",
    },
    Target {
        package: "tairix-inline",
        test: "fuzz_intrusive",
        description: "lib/inline intrusive list, two lists over one store, against naive order models",
    },
    Target {
        package: "tairix-kernel-syscall",
        test: "fuzz_args",
        description: "syscall dispatcher argument validation",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_eth",
        description: "lib/net Ethernet/ARP/IPv4/ICMP wire codecs",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_addr",
        description: "lib/net address scope/zone rules + Internet-checksum properties",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_ipv4",
        description: "lib/net IPv4 codec, emit fragmentation, fragment reassembly budgets",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_ipv6",
        description: "lib/net IPv6 codec + extension-header chain walk",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_icmp",
        description: "lib/net ICMP/ICMPv6 echo + error codecs, error rate limiter",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_nd",
        description: "lib/net Neighbour Discovery codecs + neighbour-table glue",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_stack",
        description: "lib/net dual-stack host engine frame entry point",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_udp",
        description: "lib/net dual-stack UDP codec (pseudo-header checksum, length bounds)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_tcp",
        description: "lib/net TCP segment codec (header, options, pseudo-header checksum)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_igmp",
        description: "lib/net IGMPv2 multicast-membership codec (checksum, type, length bounds)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_mld",
        description: "lib/net MLDv2 query decode + report encode (floating max-response, bounds)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_dhcp",
        description: "lib/net DHCPv4 client codec + state machine (reply decode, transitions)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_dhcpv6",
        description: "lib/net DHCPv6 client codec + state machine (RFC 8415 reply decode, transitions)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_dns",
        description: "lib/net DNS stub resolver (RFC 1035 name/message decode, resolver state machine)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_mdns",
        description: "lib/net multicast DNS (RFC 6762 message codec, record cache bounds, responder/querier) and the RFC 6763 DNS-SD name + TXT grammar over it",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_wsd",
        description: "lib/net WS-Discovery (SOAP envelope and metadata decode over the namespace-aware XML scanner, the HTTP/1.1 response reader, the per-link engine's bounds and fetch steering)",
    },
    Target {
        package: "tairix-net",
        test: "fuzz_net_ntp",
        description: "lib/net NTP client (RFC 5905 header decode, origin-nonce anti-spoof gate, poll/backoff state machine)",
    },
    Target {
        package: "tairix-telnet",
        test: "fuzz_telnet",
        description: "telnet client receive path (RFC 854 parser bounds, negotiation, LINEMODE)",
    },
    Target {
        package: "tairix-ssh",
        test: "fuzz_ssh_wire",
        description: "SSH wire codec (RFC 4251 string/mpint/name-list: totality, canonical re-encoding)",
    },
    Target {
        package: "tairix-ssh",
        test: "fuzz_ssh_ident",
        description: "SSH identification exchange (line bounds, banner rules, chunking invariance)",
    },
    Target {
        package: "tairix-ssh",
        test: "fuzz_ssh_packet",
        description:
            "SSH binary packet protocol + transport (every framing, damage, strict KEX, held traffic across rekeys)",
    },
    Target {
        package: "tairix-netstack",
        test: "fuzz_net_sockabi",
        description: "netstack socket serve path (decode + CAP_NET gate + dispatch, accounting)",
    },
    Target {
        package: "tairix-smbfs",
        test: "fuzz_smbfs",
        description: "smbfs front/worker channel (canonical frame codecs, and the front against workers whose frames are dropped, repeated, corrupted or replaced: every call answered once, nothing left behind)",
    },
    Target {
        package: "tairix-discoveryd",
        test: "fuzz_discoveryd",
        description: "discoveryd front/decoder channel (canonical frame codecs, the sandboxed decoder over hostile datagrams, and the front against a hostile decoder: containment, replacement, tick pacing, back-pressure)",
    },
    Target {
        package: "tairix-confd",
        test: "fuzz_vault",
        description: "app-data sealed-scope records (master secret + sealed document)",
    },
    Target {
        package: "tairix-appconf",
        test: "fuzz_appconf",
        description: "per-app configuration engine (grammar, parse/render fixed point, bounds)",
    },
    Target {
        package: "tairix-enrolment",
        test: "fuzz_enrolment",
        description: "service enrolment overrides (untrusted /System/Settings/Services/overrides bytes)",
    },
    Target {
        package: "tairix-syntax",
        test: "fuzz_syntax",
        description: "document syntax (every lexer's span contract and state totality, detection, store validation)",
    },
    Target {
        package: "tairix-netconfig",
        test: "fuzz_netconfig",
        description: "network.conf store engine (per-interface parse, bounds, render round-trip)",
    },
    Target {
        package: "tairix-proglib",
        test: "fuzz_proglib",
        description: "program-library catalog store and the account's own record (entry grammar, \
                      patch merge, render round-trip, edits, bounds)",
    },
    Target {
        package: "tairix-wallpaper",
        test: "fuzz_wallpaper_settings",
        description:
            "pinboard settings store (closed key registry grammar, render round-trip, bounds)",
    },
    Target {
        package: "tairix-soundtheme",
        test: "fuzz_sound_settings",
        description:
            "sound settings document (closed key registry, both readings, render round-trip)",
    },
    Target {
        package: "tairix-kernel-ipc",
        test: "fuzz_port",
        description: "IPC port send dispatch (capability + size + capacity)",
    },
    Target {
        package: "tairix-kernel-mem",
        test: "fuzz_swap",
        description: "encrypted-swap restore path (untrusted swap-device bytes)",
    },
    Target {
        package: "tairix-kernel-mem",
        test: "fuzz_ramzip",
        description: "ramzip compressed-entry restore (tampered/truncated sealed entries)",
    },
    Target {
        package: "tairix-vcmailbox",
        test: "fuzz_admit",
        description: "VideoCore mailbox admission (a caller-chosen property buffer)",
    },
    Target {
        package: "tairix-drv-fs-arxfs",
        test: "fuzz_mount",
        description:
            "arxfs mount / metadata + directory decode (superblock ring, root, trees, dirents)",
    },
    Target {
        package: "tairix-drv-fs-ext4",
        test: "fuzz_ext4_mount",
        description:
            "ext4 mount / journal replay / walk and write (descriptors, extents, htrees, ACLs)",
    },
    Target {
        package: "tairix-drv-fs-adfs",
        test: "fuzz_adfs_mount",
        description:
            "ADFS mount / decode (maps, disc records, boot block, fixed + big directories)",
    },
    Target {
        package: "tairix-drv-fs-archive",
        test: "fuzz_archive_open",
        description:
            "archive open / walk / commit (zip, tar plain and compressed, gzip, Spark, ArcFS)",
    },
    Target {
        package: "tairix-drv-fs-affs",
        test: "fuzz_affs_mount",
        description:
            "AmigaDOS mount / walk / change (root, hash chains, extensions, OFS blocks, links, bitmap)",
    },
    Target {
        package: "tairix-drv-fs-iso9660",
        test: "fuzz_iso9660_mount",
        description:
            "ISO 9660 mount / walk (descriptors, records, Rock Ridge, RISC OS, zisofs, XARs)",
    },
    Target {
        package: "tairix-drv-fs-udf",
        test: "fuzz_udf_mount",
        description:
            "UDF mount / walk / write (anchors, sequences, maps, entries, descriptors, names)",
    },
    Target {
        package: "tairix-raidmeta",
        test: "fuzz_superblock",
        description:
            "RAID array-superblock decode (untrusted on-disk member metadata: magic, bounds, CRC)",
    },
    Target {
        package: "tairix-raidmeta",
        test: "fuzz_maintenance",
        description:
            "RAID maintenance-record decode (untrusted on-disk scrub/rebuild cursors: flags, canonical fields, CRC)",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_compress",
        description: "first-party LZ decode (untrusted compressed-record bytes)",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_inflate",
        description:
            "DEFLATE decode (untrusted foreign streams: blocks, huffman tables, back-references) + encoder round trip",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_zlib",
        description:
            "zlib envelope decode (untrusted header, body, and Adler-32 trailer) + streaming round trip",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_gzip",
        description: "gzip member decode (untrusted header, deflate body, CRC-32 and length trailer, member walk)",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_bzip2",
        description: "bzip2 decode (untrusted blocks, tables, selectors) + round trip + block resume",
    },
    Target {
        package: "tairix-compress",
        test: "fuzz_arc",
        description: "ARC family decode (packed, squeezed, crunched, LZW) + dynamic LZW round trip",
    },
    Target {
        package: "tairix-crc32c",
        test: "fuzz_crc32c",
        description: "CRC-32C accelerated candidates vs the portable reference (bit-identity)",
    },
    Target {
        package: "tairix-pagezero",
        test: "fuzz_pagezero",
        description:
            "page-zero accelerated candidates vs the portable reference (zeroes exactly the region, touches nothing past it)",
    },
    Target {
        package: "tairix-raster",
        test: "fuzz_kernels",
        description:
            "lib/raster vector blend, box blur, frost and run-scan kernels vs the portable per-pixel arithmetic (bit-identity, nothing written past a block)",
    },
    Target {
        package: "tairix-colour",
        test: "fuzz_colour",
        description:
            "colour notation and coordinates (hex spellings from SVG assets and settings documents, HSV/HSL totality)",
    },
    Target {
        package: "tairix-svg",
        test: "fuzz_svg",
        description: "SVG asset decode (untrusted /System/Graphics image bytes)",
    },
    Target {
        package: "tairix-bezier",
        test: "fuzz_bezier",
        description:
            "plane geometry over drawing-document paths (boolean operations, stroke outlines, dashes, offsets, fitting, intersection)",
    },
    Target {
        package: "tairix-unicode",
        test: "fuzz_unicode",
        description:
            "normalization and SASLprep over arbitrary text (totality, buffer bounds, idempotence, the forms' relations)",
    },
    Target {
        package: "tairix-pdf",
        test: "fuzz_pdf",
        description:
            "the PDF reader over untrusted documents: structure, repair, filters, encryption, content, images",
    },
    Target {
        package: "tairix-fat",
        test: "fuzz_fat",
        description: "the FAT format's boot sector, directory and table decoders over hostile volumes",
    },
    Target {
        package: "tairix-dfs",
        test: "fuzz_dfs",
        description: "the DFS format's catalogue and entry decoders and image layouts over hostile discs",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_cpu",
        description: "PC emulator CPU: hostile guest code on every model, decoder bounds and cycle limits",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_imd",
        description: "PC emulator ImageDisk parser: hostile floppy archives, track and sector queries",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_disks",
        description: "PC emulator disk controllers: hostile port traffic, DMA programming and media changes",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_sound_devices",
        description: "PC emulator sound: hostile DSP commands over DMA, YM3812 registers and parallel DACs",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_ports",
        description: "PC emulator ports: hostile UART, printer, game port and clock card traffic, mouse motion",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_ems",
        description: "PC emulator EMS manager: hostile INT 67h calls and structures, page accounting",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_dos",
        description: "TAIRiX DOS: hostile INT 21h, 25h and 26h calls and EXEC images on a booted system",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_mouse",
        description: "MOUSE.COM: hostile INT 33h calls under mouse motion, buttons and mode changes",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_bios",
        description: "PC emulator BIOS: hostile service calls over a written-over data area, on every adapter",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_shell",
        description: "COMMAND.COM: hostile command lines and batch files typed at a booted system",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_link",
        description: "PC emulator session: hostile worker events into the host, hostile commands and answers into the worker",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_machine_snapshot",
        description: "PC emulator snapshots: real snapshots changed in place, restored, and the machine run on",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_fpu",
        description: "PC emulator 8087: hostile numeric instruction streams over hostile state and memory, snapshots, IEEE agreement",
    },
    Target {
        package: "tairix-pcemu",
        test: "fuzz_translator",
        description: "PC emulator translator: hostile blocks run by the IR interpreter and every backend's model, held to the CPU interpreter",
    },
    Target {
        package: "tairix-drawing",
        test: "fuzz_drawing",
        description:
            "the drawing's SVG reader over opened and pasted documents, and the writer reading back what it wrote",
    },
    Target {
        package: "tairix-xml",
        test: "fuzz_xml",
        description: "XML element scan (SVG documents, OpenRaster layer stacks)",
    },
    Target {
        package: "tairix-icon",
        test: "fuzz_riscos",
        description:
            "RISC OS application !Boot scan and sprite names (untrusted Obey text from a browsed volume)",
    },
    Target {
        package: "tairix-openmicro",
        test: "fuzz_media",
        description:
            "Open Micro disc, tape, cartridge and program image decode (untrusted SSD/DSD, D64/G64, UEF chunks, CSW pulses stored or zlib-compressed, gzip members, TAP pulses, CRT chips, PRG and T64) + disc, D64, UEF and TAP round trips",
    },
    Target {
        package: "tairix-openmicro",
        test: "fuzz_snapshot",
        description:
            "Open Micro snapshot restore on the Model B, the Electron and the Commodore 64 (untrusted container, sections, every device's fields and times, drives, disks, tapes, cartridges) + hostile guest code driving every device",
    },
    Target {
        package: "tairix-openmicro",
        test: "fuzz_worker",
        description:
            "Open Micro machine worker protocol for every model (untrusted requests into the sandboxed worker's service, untrusted replies into the owner's client) + canonical encodings",
    },
    Target {
        package: "tairix-machines",
        test: "fuzz_library",
        description:
            "the emulators' machine library store (damaged records of every machine, loads of any listing, damaged last screens and snapshots) + record and state round trips",
    },
    Target {
        package: "tairix-image",
        test: "fuzz_image",
        description:
            "raster-image decode and structure report (untrusted picture bytes: chunk framing, filters, interlace, marker segments, Huffman scans, EXIF directories)",
    },
    Target {
        package: "tairix-video",
        test: "fuzz_vp8",
        description:
            "VP8 stream decode (untrusted frame bytes: tags and headers, probability updates, modes and vectors, residual tokens, references replaced and copied, decodes refused their memory and left as they were)",
    },
    Target {
        package: "tairix-video",
        test: "fuzz_webm",
        description:
            "WebM demux (untrusted recording bytes: EBML element ids and sizes, unknown-size Segments and Clusters, the seek index, blocks retried through a miss)",
    },
    Target {
        package: "tairix-sound",
        test: "fuzz_sound",
        description:
            "sound-file decode (untrusted AU + WAVE bytes: headers, chunk walks, RF64, ADPCM blocks, decode retried through a miss)",
    },
    Target {
        package: "tairix-virtio",
        test: "fuzz_virtqueue",
        description:
            "split-virtqueue completion path and the many-requests ticket queue (hostile device-written used ring / descriptors, answers in any order)",
    },
    Target {
        package: "tairix-drv-display-gpu-virtio",
        test: "fuzz_gpu_answers",
        description:
            "virtio-gpu answer decoding: the verdict and fence, display information, EDID, capsets and a mapping's caching (hostile device-written bytes)",
    },
    Target {
        package: "tairix-drv-display-gpu-virtio",
        test: "fuzz_venus_replies",
        description:
            "Venus reply decoding: results, objects made, physical devices, properties, queue families, memory and requirements, and the statement narrowed from them (hostile renderer-written bytes)",
    },
    Target {
        package: "tairix-usb",
        test: "fuzz_descriptors",
        description:
            "lib/usb device, configuration, hub, and string descriptor decoders (hostile device-written enumeration bytes)",
    },
    Target {
        package: "tairix-hid",
        test: "fuzz_hid_report",
        description:
            "lib/hid Report Descriptor parser, report normaliser, and boot keyboard/mouse decoders (hostile device-written HID bytes)",
    },
    Target {
        package: "tairix-drv-input-usb-hid",
        test: "fuzz_usb_hid",
        description:
            "usb_hid configuration-descriptor reading: the HID interface, its stated report length, its interrupt endpoint (hostile device-written bytes)",
    },
    Target {
        package: "tairix-virtio-input",
        test: "fuzz_virtio_input",
        description:
            "lib/virtio_input configuration probes and event decoders: keys, motion, wheels, the multi-touch slot protocol (hostile device-written bytes)",
    },
    Target {
        package: "tairix-touch",
        test: "fuzz_touch",
        description:
            "lib/touch gesture recogniser (injector-supplied touch frames: press balance, pinch life, deadlines)",
    },
    Target {
        package: "tairix-vt",
        test: "fuzz_vt",
        description: "lib/vt escape-sequence parser (untrusted terminal / remote-host bytes)",
    },
    Target {
        package: "tairix-fdt",
        test: "fuzz_fdt",
        description: "lib/fdt device-tree reader (untrusted firmware/bootloader DTB bytes)",
    },
    Target {
        package: "tairix-partition",
        test: "fuzz_partition",
        description: "lib/partition MBR + GPT table parsers (untrusted on-disk partition bytes)",
    },
    Target {
        package: "tairix-fsprobe",
        test: "fuzz_fsprobe",
        description: "lib/fsprobe filesystem-signature probe (untrusted removable-media bytes)",
    },
    Target {
        package: "tairix-fsmeta",
        test: "fuzz_fsmeta",
        description:
            "lib/fsmeta key-grammar parser + attribute-set decoder (untrusted stored/foreign bytes)",
    },
    Target {
        package: "tairix-curses",
        test: "fuzz_curses_input",
        description: "lib/curses input decoder (untrusted key/mouse/paste bytes)",
    },
    Target {
        package: "tairix-users",
        test: "fuzz_users",
        description: "lib/users database parser (untrusted /System/Security/Users bytes)",
    },
    Target {
        package: "tairix-users",
        test: "fuzz_groups",
        description: "lib/users group-database parser (untrusted /System/Security/Groups bytes)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_field",
        description: "lib/log typed-field value decoder (untrusted log-record field bytes)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_segment",
        description: "lib/log segment decoder (untrusted /System/Logs segment bytes)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_record",
        description: "lib/log logical-record decoder (untrusted /System/Logs record bytes)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_dict",
        description:
            "lib/log segment string-dictionary codec (untrusted /System/Logs dictionary bytes)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_bootring",
        description:
            "lib/log early-boot ring FIFO (push/pop/evict ordering + loss-range accounting)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_ingress",
        description:
            "lib/log record-ingress admission (attested-origin vs caller stream/source spoof + append sequencing)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_journal",
        description:
            "lib/log journal engine (admit/commit/import_boot: rotation, chaining, sealing; every persisted segment must verify)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_ratelimit",
        description:
            "lib/log ingress rate limiter (token-bucket admit/drop + coalesced loss reporting: every drop is accounted, never panics)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_render",
        description:
            "lib/log boot-console renderer (hostile caller text/fields: rendered line is always control-byte-free, never panics)",
    },
    Target {
        package: "tairix-log",
        test: "fuzz_report",
        description:
            "lib/log rich renderers (JSON/Markdown/table over hostile records: output is always control-byte-free, JSON is valid, never panics)",
    },
    Target {
        package: "tairix-glob",
        test: "fuzz_glob",
        description: "lib/glob pattern compiler + match loop (untrusted glob-pattern bytes)",
    },
    Target {
        package: "tairix-elsh",
        test: "fuzz_expand",
        description:
            "elsh word expansion (hostile command lines: pattern groups, braces, parameters, filename generation under every glob option; never panics, argv stays bounded)",
    },
    Target {
        package: "tairix-help",
        test: "fuzz_help",
        description:
            "lib/help document parser + short/full renderers (untrusted help-document bytes: never panics, fails closed, printed output is control-free)",
    },
    Target {
        package: "tairix-path",
        test: "fuzz_path",
        description: "lib/path path-string parser + canonical-spelling round-trip (untrusted path bytes)",
    },
    Target {
        package: "tairix-der",
        test: "fuzz_der",
        description:
            "lib/der reader + writer (hostile DER from SPNEGO and Kerberos peers: never panics, accepted encodings re-encode byte for byte)",
    },
    Target {
        package: "tairix-smb",
        test: "fuzz_smb",
        description:
            "lib/smb decoders (session, tree, file, listing, information-class, change and lease messages), SPNEGO and NTLM tokens, framer, connection core and client (hostile SMB 3 server bytes: never panics, accepted messages and classes re-encode stably, listings and change records walk to their end, a tampered sealed frame is never accepted, a session is never keyed from a changed final response or 3.1.1 negotiation, a 3.0 downgrade never yields a tree)",
    },
    Target {
        package: "tairix-resref",
        test: "fuzz_resref",
        description:
            "lib/resref resource-reference parser + canonical-spelling round-trip (untrusted reference bytes)",
    },
    Target {
        package: "tairix-procinfo",
        test: "fuzz_resinfo",
        description:
            "lib/procinfo info:/stats: resolver (untrusted reference bytes + hostile sysinfod replies: never panics, fails closed, envelope invariants hold)",
    },
    Target {
        package: "tairix-tz",
        test: "fuzz_zoneinfo",
        description:
            "lib/tz zone-store reader (any bytes validated or refused, every query of an accepted store, the local-time round trip)",
    },
    Target {
        package: "tairix-devids",
        test: "fuzz_devids",
        description:
            "lib/devids pci.ids/usb.ids vetting parser + compact-table decoder (untrusted upstream-download and table bytes)",
    },
    Target {
        package: "tairix-binfmt",
        test: "fuzz_rxe",
        description:
            "lib/binfmt rxe inspection view + the report of every TAIRiX program, driver image and manifest (untrusted executable-file bytes)",
    },
    Target {
        package: "tairix-binfmt",
        test: "fuzz_elf",
        description: "lib/binfmt ELF64 view (untrusted executable-file bytes)",
    },
    Target {
        package: "tairix-binfmt",
        test: "fuzz_wasm",
        description: "lib/binfmt wasm module-structure view (untrusted executable-file bytes)",
    },
    Target {
        package: "tairix-bootload",
        test: "fuzz_plan",
        description: "boot-chain loader core (untrusted kernel-image ELF -> LoadPlan validation)",
    },
    Target {
        package: "tairix-multiboot2",
        test: "fuzz_info",
        description: "Multiboot2 information-structure parse + builder round-trip (untrusted boot-info bytes)",
    },
    Target {
        package: "tairix-disasm",
        test: "fuzz_riscv64",
        description: "lib/disasm RV64GC decoder (untrusted executable-file bytes: never panics, always makes forward progress)",
    },
    Target {
        package: "tairix-disasm",
        test: "fuzz_aarch64",
        description: "lib/disasm A64 decoder (untrusted executable-file bytes: never panics, always makes forward progress)",
    },
    Target {
        package: "tairix-disasm",
        test: "fuzz_wasm_isa",
        description: "lib/disasm wasm code-body decoder (untrusted executable-file bytes: never panics, always makes forward progress)",
    },
    Target {
        package: "tairix-codegen",
        test: "fuzz_codegen",
        description: "lib/codegen generator (every backend's code on its machine model against the IR interpreter; any built function refused by the verifier or compiled, never a panic)",
    },
    Target {
        package: "tairix-disasm",
        test: "fuzz_x86_64",
        description: "lib/disasm x86_64 decoder (untrusted executable-file bytes: never panics, always makes forward progress, 15-byte cap)",
    },
    Target {
        package: "tairix-sandbox",
        test: "fuzz_sandbox",
        description: "lib/sandbox decode seam (hostile input files and NTP replies through the sandboxed decode/timesync services, sound files through the audio decode worker against an in-process decode, and hostile worker replies into the fail-closed client decoders)",
    },
    Target {
        package: "tairix-wintersun-net",
        test: "fuzz_wire",
        description:
            "WinterSun realm message decoders (hostile client/server frames: bounds, canonical re-encode, never panics)",
    },
    Target {
        package: "tairix-wintersun-net",
        test: "fuzz_handshake",
        description:
            "WinterSun session handshake (hostile pre-key bytes: transcript binding, realm-identity pinning, stated refusals)",
    },
    Target {
        package: "tairix-wintersun-net",
        test: "fuzz_session",
        description:
            "WinterSun sealed record transport (reorder/replay/truncate/oversize/reflect/flip: refused, and the session ends)",
    },
    Target {
        package: "tairix-wintersun-figure",
        test: "fuzz_identity",
        description:
            "WinterSun figure record decoder (hostile character records: total, one spelling, every admitted figure builds and places)",
    },
    Target {
        package: "tairix-wintersun-figure",
        test: "fuzz_design",
        description:
            "WinterSun figure designer (any edits keep a record, show or change nothing, settle into one write; plausible draws build)",
    },
    Target {
        package: "tairix-supervisor",
        test: "fuzz_repl",
        description:
            "pre-boot Supervisor REPL line/command parser (untrusted physical-console bytes: never panics, always terminates)",
    },
];

/// Parsed `fuzz` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// How long each harness runs; `None` is one smoke iteration.
    pub budget: Option<Budget>,
    /// Optional harness filter (`--target <name>`); runs all when `None`.
    pub only: Option<String>,
    /// `--list`: print the registry and exit without running anything.
    pub list: bool,
    /// Reproduce an earlier run by fixing its PRNG seed (`--seed <n>`).
    ///
    /// Unset (the default, including in `ci`) draws a fresh per-harness seed
    /// each run, so consecutive soaks explore new inputs.
    /// Setting it replays the exact stream the orchestrator logged for a
    /// reported crash.
    pub seed: Option<u64>,
}

/// Parse `fuzz` arguments. `--quick` is the default when no budget flag is
/// given.
///
/// # Errors
/// Returns a usage error for an unknown flag, a missing or unreadable flag
/// value, or two budget flags that contradict each other.
pub fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut budget = Flags::default();
    let mut only: Option<String> = None;
    let mut list = false;
    let mut seed: Option<u64> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let Some(flag) = arg.to_str() else {
            return Err(format!(
                "fuzz: argument {} is not valid UTF-8",
                arg.display()
            ));
        };
        if budget.take("fuzz", flag, &mut iter)? {
            continue;
        }
        match flag {
            "--once" => budget.set_mode("fuzz", Mode::Once)?,
            "--list" => list = true,
            "--target" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "fuzz: `--target` requires a harness name".to_string())?;
                let name = value
                    .to_str()
                    .ok_or_else(|| "fuzz: `--target` value is not valid UTF-8".to_string())?;
                only = Some(name.to_string());
            }
            "--seed" => seed = Some(soak::parse_u64("fuzz", "--seed", iter.next())?),
            other => {
                return Err(format!(
                    "fuzz: unexpected argument {other:?}; usage: \
                     cargo xtask fuzz [--once | --quick | --soak] [--target NAME] \
                     [--secs N | --until T (with --target)] [--seed N] [--list]"
                ));
            }
        }
    }

    Ok(Options {
        budget: budget.budget("fuzz", Mode::Quick)?,
        only,
        list,
        seed,
    })
}

/// Resolve the harnesses an [`Options`] selects, preserving registry order.
///
/// # Errors
/// Returns an error if `--target` names a harness that is not registered, or
/// a window would soak more than one ([`soak::one_target_per_window`]).
pub fn selected(opts: &Options) -> Result<Vec<Target>, String> {
    let targets = match opts.only.as_deref() {
        None => TARGETS.to_vec(),
        Some(name) => match TARGETS.iter().find(|t| t.test == name) {
            Some(t) => vec![*t],
            None => {
                return Err(format!(
                    "fuzz: unknown target `{name}`; known targets: {}",
                    TARGETS
                        .iter()
                        .map(|t| t.test)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        },
    };
    soak::one_target_per_window("fuzz", opts.budget, targets.len())?;
    Ok(targets)
}

/// Run the selected harnesses for their budget.
pub fn run(ctx: &Context, opts: &Options) -> Result<(), String> {
    if opts.list {
        for t in TARGETS {
            outln!("{:<24} {}  [{}]", t.test, t.description, t.package)?;
        }
        return Ok(());
    }

    let targets = selected(opts)?;
    // Each harness is an independent, budget-bounded host process, so the
    // registry runs concurrently rather than paying the sum of every
    // harness's budget. The shared runner caps concurrency at the host's
    // parallelism and fails closed (`commands::parallel`).
    let jobs: Vec<Job> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| job_for(ctx, t, opts.budget, opts.seed, i))
        .collect();
    let concurrency = parallel::default_concurrency(jobs.len());
    parallel::run(&ctx.children, jobs, concurrency)
}

/// Build the `cargo test` [`Job`] that runs one fuzz harness.
///
/// `budget` is the wall-clock soak budget exported to the harness (`None`
/// runs the single fixed-iteration smoke sweep, as `--once` does). `seed` and
/// `index` pick the per-job PRNG seed (`None` draws a fresh entropy seed each
/// call, so repeated runs explore new inputs; `Some(base)` reproduces a logged
/// stream). The chosen seed is baked into the job label so it reaches the log.
///
/// Shared by [`run`] and the long-CI flake hunt ([`super::ci_long`]) so both
/// build a harness job the one way.
pub(crate) fn job_for(
    ctx: &Context,
    target: &Target,
    budget: Option<Budget>,
    seed: Option<u64>,
    index: usize,
) -> Job {
    // The harness reads this seed instead of its built-in constant, so a fresh
    // seed explores new inputs while an explicit one reproduces a logged crash.
    let job_seed = seed::job_seed(seed, index);
    let label = format!(
        "fuzz {} ({}, seed {job_seed})",
        target.test,
        soak::describe(budget)
    );
    let harness = soak::Harness {
        package: target.package,
        test: target.test,
        filter: &[],
        budget_env: tairix_fuzzseed::FUZZ_BUDGET_ENV,
    };
    soak::job(
        ctx,
        label,
        &harness,
        budget,
        &[(seed::FUZZ_SEED_ENV, job_seed.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::{parse, selected, TARGETS};
    use crate::commands::soak::{Budget, NIGHTLY, QUICK};
    use std::ffi::OsString;
    use std::time::{Duration, UNIX_EPOCH};

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn defaults_to_quick_running_every_target() {
        let opts = parse(&argv(&[])).expect("empty args parse");
        assert_eq!(opts.budget, Some(Budget::For(QUICK)));
        assert!(opts.only.is_none());
        assert!(!opts.list);
        assert_eq!(selected(&opts).expect("all targets").len(), TARGETS.len());
    }

    #[test]
    fn once_flag_selects_a_single_iteration_with_no_budget() {
        let opts = parse(&argv(&["--once"])).expect("once parses");
        assert_eq!(opts.budget, None);
    }

    #[test]
    fn soak_flag_selects_the_soak_budget() {
        let opts = parse(&argv(&["--soak"])).expect("soak parses");
        assert_eq!(opts.budget, Some(Budget::For(NIGHTLY)));
    }

    #[test]
    fn until_names_the_window_close() {
        let opts = parse(&argv(&["--soak", "--until", "1700000000"])).expect("until parses");
        assert_eq!(
            opts.budget,
            Some(Budget::Until(
                UNIX_EPOCH + Duration::from_secs(1_700_000_000)
            ))
        );
    }

    /// A window over the registry is refused, so no target queues past the
    /// close to make one smoke iteration and pass; one named target soaks.
    #[test]
    fn a_window_over_the_registry_is_refused() {
        let opts = parse(&argv(&["--until", "1700000000"])).expect("until parses");
        assert!(selected(&opts).is_err_and(|why| why.contains("--target")));
        let opts = parse(&argv(&["--until", "1700000000", "--target", "fuzz_decode"]))
            .expect("until with a target parses");
        let selected = selected(&opts).expect("one target soaks to the close");
        assert_eq!(
            selected.iter().map(|t| t.test).collect::<Vec<_>>(),
            ["fuzz_decode"]
        );
    }

    #[test]
    fn target_filter_selects_one_known_harness() {
        let opts = parse(&argv(&["--target", "fuzz_decode"])).expect("target parses");
        let chosen = selected(&opts).expect("known target");
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].test, "fuzz_decode");
    }

    #[test]
    fn unknown_target_fails_closed() {
        let opts = parse(&argv(&["--target", "no_such_harness"])).expect("flag parses");
        assert!(selected(&opts).is_err());
    }

    #[test]
    fn conflicting_budget_flags_are_rejected() {
        assert!(parse(&argv(&["--quick", "--soak"])).is_err());
        assert!(parse(&argv(&["--once", "--soak"])).is_err());
    }

    #[test]
    fn repeating_the_same_budget_flag_is_accepted() {
        let opts = parse(&argv(&["--quick", "--quick"])).expect("idempotent flag");
        assert_eq!(opts.budget, Some(Budget::For(QUICK)));
    }

    #[test]
    fn secs_override_parses_and_wins() {
        let opts = parse(&argv(&["--soak", "--secs", "3"])).expect("secs parses");
        assert_eq!(opts.budget, Some(Budget::For(Duration::from_secs(3))));
    }

    #[test]
    fn secs_requires_a_number() {
        assert!(parse(&argv(&["--secs", "soon"])).is_err());
        assert!(parse(&argv(&["--secs"])).is_err());
    }

    #[test]
    fn seed_defaults_to_none_so_each_run_is_fresh() {
        // No `--seed` means the orchestrator draws a fresh seed per run, so
        // consecutive soaks explore new inputs.
        let opts = parse(&argv(&[])).expect("empty args parse");
        assert_eq!(opts.seed, None);
    }

    #[test]
    fn seed_override_parses() {
        let opts = parse(&argv(&["--seed", "12345"])).expect("seed parses");
        assert_eq!(opts.seed, Some(12345));
    }

    #[test]
    fn seed_requires_a_number() {
        assert!(parse(&argv(&["--seed", "later"])).is_err());
        assert!(parse(&argv(&["--seed"])).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&argv(&["--turbo"])).is_err());
    }

    #[test]
    fn list_flag_is_parsed() {
        let opts = parse(&argv(&["--list"])).expect("list parses");
        assert!(opts.list);
    }

    #[test]
    fn every_registered_target_has_a_unique_test_name() {
        for (i, a) in TARGETS.iter().enumerate() {
            for b in &TARGETS[i + 1..] {
                assert_ne!(a.test, b.test, "duplicate fuzz target name");
            }
        }
    }

    /// Collects each harness (`fuzz_<name>` integration-test file) a crate
    /// publishes under its own `tests/` directory, with that crate's package.
    fn harnesses_below(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let package = dir
            .file_name()
            .is_some_and(|name| name == "tests")
            .then(|| dir.parent().and_then(package_of))
            .flatten();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                if name != "target" {
                    harnesses_below(&path, found);
                }
            } else if let Some(package) = &package {
                if let Some(stem) = name.strip_suffix(".rs") {
                    if stem.starts_with("fuzz_") {
                        found.push((stem.to_owned(), package.clone()));
                    }
                }
            }
        }
    }

    /// The package `crate_dir`'s manifest names.
    fn package_of(crate_dir: &std::path::Path) -> Option<String> {
        let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
        let table = &manifest[manifest.find("[package]")?..];
        let table = &table[..table[1..].find("\n[").map_or(table.len(), |end| end + 1)];
        table.lines().find_map(|line| {
            line.strip_prefix("name = \"")?
                .strip_suffix('"')
                .map(String::from)
        })
    }

    /// A harness that exists on disk but is absent from the registry is never
    /// run by the gate, so its subject is unfuzzed however green CI looks; one
    /// registered under another package runs nothing.
    #[test]
    fn every_harness_in_the_tree_is_registered_under_its_own_package() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root");
        let mut found = Vec::new();
        for area in ["kernel", "drivers", "lib", "userland", "tools", "tests"] {
            harnesses_below(&root.join(area), &mut found);
        }
        assert!(
            !found.is_empty(),
            "walked {} but found no harnesses at all",
            root.display()
        );
        let misregistered: Vec<&(String, String)> = found
            .iter()
            .filter(|(name, package)| {
                let chosen = parse(&argv(&["--target", name]))
                    .ok()
                    .and_then(|opts| selected(&opts).ok());
                !chosen.is_some_and(|chosen| chosen.len() == 1 && chosen[0].package == package)
            })
            .collect();
        assert!(
            misregistered.is_empty(),
            "fuzz harnesses missing from the registry or under another package: {misregistered:?}"
        );
    }

    #[test]
    fn registry_covers_the_burn_down_endpoints() {
        // wire decoders, dispatcher, lib/net protocol parsers, IPC port.
        for required in ["fuzz_decode", "fuzz_args", "fuzz_net_icmp", "fuzz_port"] {
            assert!(
                TARGETS.iter().any(|t| t.test == required),
                "missing required fuzz harness {required}"
            );
        }
    }
}
