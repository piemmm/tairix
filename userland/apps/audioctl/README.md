# audioctl

Stability tier: **experimental**.

The sound devices' controls from a terminal or a headless machine
(`plans/SOUND.md` SND15). `audioctl` lists the sinks and sources the audio
service shows the caller, and the streams playing or recording on them; it
makes a device its direction's default, sets its level, or mutes it. A device
is named by an `audio:` reference, by its id for this boot or by its location.

A change asks for no capability. The audio service admits it for the login
session holding the room the device serves, for anybody while the room is
unclaimed, and for nobody while it is withheld, so a remote login cannot turn
down somebody else's room. A refusal is reported with its reason.

| Module | What it is |
|---|---|
| `lib` | The command line and the engine; the audio service and the System Information API are seams, so it is host-tested. |
| `run` | The `Run` binary and the live seams. |

Capabilities, and why each, are in `AppInfo.toml`; the page is
`docs/src/userland/audioctl.md`.
