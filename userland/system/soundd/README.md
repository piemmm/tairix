# tairix-soundd — the cue authority

Stability: experimental.

## What this crate is

The one machine service that makes the desktop's sounds (`plans/SOUND.md`
SND21). It serves `cue-v1` on the reserved `SOUND_CUE_ENDPOINT`, decides from
the caller's kernel-attested identity whether it may cue the event it names,
resolves the event to a theme's asset or to silence, decodes the asset in the
parser sandbox, and plays it through `audiod` as a notification — for the
machine, or for the caller's session.

## Capabilities

`CAP_IPC_BIND_PRIVILEGED`, `CAP_SHM`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` and
`CAP_FS_ACCESS`: what any player holds. The authority to play for the machine or
another session is `audiod`'s to grant, on this service's attested account.

## Layout

| Module | What |
|---|---|
| `service` | The authority: classes, resolution, bounds, playing, answers |
| `bounds` | Per-principal rate and retrigger record, the concurrent cap |
| `decode` | One sound decoded whole in a supervised sandbox worker |
| `pcm` | A decoded sound and the reclaimable cache of them |
| `store`, `speaker` | The volume and stream seams |
| `events` | Audit event ids (22 000–22 007) |

The `Run` binary is its own decode worker in the sandbox's session role.

## Testing

`cargo test -p tairix-soundd` drives the whole authority over in-memory seams
and the real in-process decoder. The `audio_qemu_*` cue verticals assert the
host capture is sample-exact against the shipped master.

See `docs/src/userland/soundd.md`.
