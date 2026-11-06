# `tairix-label` — show or change a volume's title

Stability tier: **experimental**.

The `label` command app: `label NAME` prints the title of the volume `NAME`
names — the label its filesystem records — and `label NAME TITLE` sets it, an
empty `TITLE` clearing it where the format allows. `NAME` is read by the one
resolution `unmount` shares (`tairix_procinfo::named_volume`): a catalog name,
a directory the volume is mounted on, a disk image's file name, or its durable
root (`id::<volume-id>`, as the file manager hands it when it asks an
administrator to retitle a volume). The title is checked before anything is
asked; what each format stores is its driver's to answer.

The tool resolves and presents. The kernel's `volume_retitle` path decides
who may retitle the volume — a disk image's owner, the mount authority over
any device's volume, and the user at the seat over a removable one whose root
they may write; a share's title is its server's — and audits every decision.

The crate is both the pure `no_std` engine library (parser, engine, seams —
host-tested against in-memory fixtures) and the freestanding `Run` program
(`src/run.rs`), which wires the shared `tairix_procinfo::IpcTransport`, the
`volume_retitle` syscall wrapper and the `tairix_help::BundleHelp` short-help
source over the inherited standard streams.

`cargo test -p tairix-label` drives the parser, the engine (show, set, clear,
the durable root, the refusals the tool makes and the kernel's, a withheld
volume, a name naming none) and the help-document switch pinning across every
required locale.
