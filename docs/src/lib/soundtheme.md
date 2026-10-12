# `tairix-soundtheme`

The desktop's sounds as policy: which asset each event plays, the themes and
the bounds a theme is read within, the sound settings and the user's own layer
over them, and the client a program asks the cue authority with. It decodes
nothing (`lib/sound`) and mixes nothing (`lib/audio`). The design is
`plans/SOUND.md` §Default desktop sounds.

The settings, the user's layer and the resolution over them are the default
`settings` feature, a registry over `lib/appconf`. A program that only cues —
init, the login service, the device manager, the terminal, the Switchboard —
turns default features off and links the vocabulary, the catalog and the cue
client alone, so no document engine, nor anything it enables in a shared
crate, reaches what it is built beside: PID 1 is linked into kernel test
images that carry no allocator.

## Events and assets

`SoundEvent` is wire vocabulary, so it is defined with the cue protocol
([`cue-v1`](../abi/sound-cue.md)) and re-exported here. Each event names who may
cue it (`CueAuthority`), and `SoundAsset::of` names the asset it plays:

| Event | Asset | Cued by |
|---|---|---|
| `Startup` | `startup` | lifecycle: init, PID 1 |
| `Shutdown` | `shutdown` | lifecycle: a holder of `CAP_SYSTEM_POWER` |
| `Login`, `Logout` | `login`, `logout` | lifecycle: the login service |
| `DeviceAttached`, `DeviceDetached` | `usb_connected`, `usb_disconnected` | lifecycle: the device manager |
| `Notification`, `Bell` | `notification_soft` | application |
| `NotificationMessage` | `notification_message` | application |
| `NotificationReminder` | `notification_reminder` | application |
| `Error`, `Warning`, `Info` | `error`, `warning`, `info` | application |

An asset is stored as `<name>.flac`. A file named for no asset is an orphan, and
the build refuses it.

## Themes

A theme is a directory under `SOUND_STORE` (`/System/Audio/Sounds`) whose name
is a plain leaf name (`is_theme_name`), which is also the label a chooser draws.
`asset_path` spells where a theme keeps an asset. `DEFAULT_THEME` (`TAIRiX`) is
the shipped one and the one every other falls back to, so the build requires it
to carry every asset.

`MAX_SOUND_BYTES` (1 MiB) is a containment bound on untrusted input: a theme's
file is read in full before it is decoded. The largest shipped asset is a tenth
of it, and a test holds it to four fifths. `MAX_CUE_SAMPLES` (2^20 samples
across a sound's channels) bounds what one decodes to, since a FLAC holds a
long silence in a few hundred bytes, and `MAX_CUE_SECONDS` (10) how long it
may sound, however few samples a slow rate spends; the build holds every
shipped asset to both.

`MACHINE_SETTINGS` (`/System/Settings/Sound/sound.conf`) is the machine's sound
document, administrator policy read through the same registry as a user's.
`themes` turns a listing of the store into the themes a chooser offers: each
legal name once, in name order.

## The shipped set

`assets/TAIRiX/` holds the twelve assets, 48 kHz stereo 16-bit FLAC, about
554 KiB against 3.1 MiB of raw samples. `cargo xtask sound-encode
[--theme <name>] <master.wav>...` encodes an authored master into a theme with
the first-party encoder, deterministically, and decodes the result back sample
for sample before writing it; from then on the FLAC is the master.

`tools/syshelp` holds every asset to the authoring contract as it plants it
under `/System/Audio/Sounds/<Theme>/`: decoded in full, 48 kHz stereo 16-bit,
proven against its own `STREAMINFO` digest, silent in its first and last frame
so it neither clicks in nor out, and within the bound. A bad sound plays as
silence at runtime, which looks exactly like working, so the build looks harder
than it does for artwork.

## Settings

`SoundSettings` is a closed registry over a `lib/appconf` document:

| Key | Value | Default |
|---|---|---|
| `sound.enabled` | `true` / `false` (or `on` / `off`) | `true` |
| `sound.theme` | a theme name | `TAIRiX` |
| `sound.<event>.enabled` | `true` / `false` | `true` |
| `sound.<event>.gain` | an attenuation, `-6.5dB` | unity |
| `sound.<event>.file` | an absolute path, or empty for none | none |

`<event>` is the event's stable name (`device_attached`, `bell`, …). A gain is
an `AudioGain`, so it can only turn an event down: unity plays the asset
bit-exact. A file is a `SettingPath`; whoever opens it reads untrusted bytes
under its own identity.

The same registry is read at two layers, the machine's document beneath and
the user's own over it while their session holds the seat. A stored document
is read tolerantly (`Registry::load`: a refused value costs only its key); one
that arrives over a channel strictly (`merge_pushed`: refused whole on its
first fault).

## The user's layer

`layered(event)` says whether a user's own choices reach an event: every one
but `Startup`, `Login` and `Logout`, which sound when no session holds the seat
and so are the machine document's alone. `pushable(key)` says whether a
document a session hands over may set a key — never a file, which travels as
a descriptor, nor a setting of an event no user's choices reach — and
`PUSHABLE` lists those keys in registry order. `merge_pushed(machine, text)` is
the strict reading the cue authority applies to a push.

`UserChoices` is what a user set and nothing they left, so the machine's
document still decides every setting they did not. Each value is kept in its
canonical spelling. `load` reads a stored document, naming every setting it
leaves out; `apply` lays a pane's document over the choices or refuses it
whole; `choose` and `use_theme` set and clear an event's file; `pushed` is the
document handed to the cue authority, every choice but the files; `over` is the
settings in effect above a machine document; and `document` is the whole of it,
files and all, as the session stores it.

## Resolution

`resolve(event, settings, own, holds)` is the one order a cue resolves in:
silence if the event or every sound is off, the file chosen for the event where
there is one — the session's own, or the machine's replacement — the active
theme's asset, the shipped theme's, else silence (`NoAsset`). `holds` answers whether a theme holds an asset, and is asked about
the shipped theme only when the active one has none. It is a pure function of
its inputs, tested over the whole cross-product.

## The cue client

`cue(transport, event, answer)` asks the cue authority for one sound over a
`CueTransport`, answered once it starts or once it has played out
(`Answer`), so the client is host-tested against a mock authority; `RtCue`
(feature `rt`) is the reserved rendezvous. A cue that made no sound still
answers, with the `Silence` that says why.

A program that parks on a wait-set of its own never blocks on the authority:
`PostedCues` posts through a `CuePost` (`RtCuePost`, feature `rt`), at most
`MAX_POSTED` cues at a time, each given up on after the deadline its caller
names — `CUE_ANSWER_DEADLINE_NS`, past the longest a healthy authority takes
to start a sound, or `PLAYED_OUT_WAIT_NS` for one to be answered once played
out: the longest cue and the authority's own `DRAIN_SLACK_NS` — and collects
whatever answers have arrived when its loop next passes, without waiting for
one that has not. A session's push of its user's choices is posted there too
(`push`), one at a time and given up on after `POLICY_ANSWER_DEADLINE_NS`, each
file a read-only handle minted to the authority, its status what `collect`
answers once it arrives: one caller's answers on the rendezvous are one wake,
so all of them are collected in one place. While an answer is outstanding the call is a member of the caller's
wait-set, and it is withdrawn once the last is collected, since a reply
member stays ready once its endpoint is gone; a post whose answer cannot be
watched is cancelled at once.
