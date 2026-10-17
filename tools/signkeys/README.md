# tairix-signkeys

Stability: experimental. Host-only.

The build's signing key set: the three secret Ed25519 seeds a TAIRiX build
signs with.

| Seed | Signs | Verified by |
|---|---|---|
| `driver-signing` | every driver manifest, baked or bundled | the kernel's driver-load anchor |
| `app-signing` | every system application's `AppInfo` | the kernel's application anchor |
| `app-publisher` | the first-party publisher certificate | the app-data service's per-app pin |

Whoever holds the key set can sign drivers and programs every kernel built
from it admits, so it is a secret input to the build and never part of the
source. The kernel's build script derives the two public anchors from it and
embeds only those; the image pipeline (`tools/xtask`'s `image_apps` and
`image_drivers`) signs with it. A build without a key set fails, naming the
command that creates one. There is no shared fallback key.

## Creating and finding it

`cargo xtask keys init` draws a key set from the host's entropy source and
writes it owner-only (0600, in a 0700 directory) to the first of:

1. `$TAIRIX_SIGNING_KEYS`, an absolute path;
2. `$XDG_DATA_HOME/tairix/signing-keys`;
3. `$HOME/.local/share/tairix/signing-keys`.

It never replaces a key set that is already there: the kernels built with one
trust only what it signs, so losing it means rebuilding them. Back it up.

## What a load refuses

- a file inside the workspace, where it could be committed;
- a file its owner's group or anyone else can read (Unix hosts);
- a missing, repeated, unknown or malformed field;
- a seed once published in the source tree, which anybody can sign with;
- a seed of one repeated byte, which no working entropy source draws;
- two domains sharing a seed, which would make one an authority over another.

The seeds are held in buffers erased when dropped (`tairix_util::secret`), and
the file is read straight into one.

## Reproducibility

Ed25519 signatures are deterministic, so two builds of one tree with one key
set produce identical images. A build with a different key set produces an
image only its own kernels admit.
