# `tairix-settingsd` — the machine-settings service

Stability tier: **experimental**.

`settingsd` is the one writer of the administrator's layer of the machine's two
settings documents: the machine configuration (`system.conf`, `lib/sysconfig`)
and the per-interface network configuration (`network.conf`, `lib/netconfig`).
An administrator's program, run elevated — `configure`, the Date & Time
dialog — asks it to read a document or to change one over `settings-v1`
(`lib/abi/src/settings_ipc.rs`). The installed binary lives at
`/System/Services/settingsd.app/Run`, started by PID 1.

## Who may ask

The kernel decides: the reserved rendezvous is bound requiring
`CAP_SETTINGS_WRITE` of every caller, which only the administrator ceiling
carries and only an elevated program holds. The service checks it again, so a
rendezvous bound without the gate fails closed. Every read and every change is
recorded with the caller's attested uid.

## What a change does

A change names its document and the settings it sets, by the names that
document's registry spells; the empty value clears a network setting. The
service takes the document as it stands — the administrator's layer at
`/System/Settings/…` where one exists, else the image's default at
`/System/Defaults/…` — applies every setting through the document's own engine,
and replaces the administrator's layer whole and at once (`tairix_rt::replace_file`).
Any setting refused, a name given twice, or a document its engine cannot read
writes nothing at all. Requests are served one at a time, so two programs
changing one document at once lose neither change.

The service never tells the running network stack about a change: the tool that
asked holds `CAP_NET_ADMIN` and does that itself.

## What it holds, and what it deliberately does not

`CAP_IPC_BIND_PRIVILEGED` (the reserved rendezvous), `CAP_FS_ACCESS` (both
layers of its two documents), `CAP_LOG_EMIT` (its audit records). Its account,
`settingsd`, owns `/System/Settings/Configuration` (`0755`) and
`/System/Settings/Network` (`0750`, the services group alone, because the
network document names the machine's hardware and addresses); those two
directories are the whole of its reach. It holds no `CAP_SETTINGS_WRITE`, no
`CAP_FS_CHOWN`, no `CAP_NET_ADMIN`, and no spawn authority.

## Layout

- `src/lib.rs` — the engine, over an injected `DocumentStore`, host-tested in
  `src/tests.rs`.
- `src/events.rs` — the stable audit event ids (`34_000`–`34_999`).
- `src/run.rs` — the freestanding `Run` binary: the VFS store and the serve loop.

See `docs/src/userland/settingsd.md`.
