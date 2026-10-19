# Machine-settings service (`settingsd`)

`settingsd` is the one writer of the machine's two settings documents: the
machine configuration, `system.conf` (`lib/sysconfig`), and the per-interface
network configuration, `network.conf` (`lib/netconfig`). Its crate is
`userland/system/settingsd`; the installed binary is
`/System/Services/settingsd.app/Run`, started by PID 1 on the boot floor and
restarted on failure.

## Two layers per document

| Layer | Machine configuration | Network configuration | Who writes it |
|---|---|---|---|
| The image's default | `/System/Defaults/Configuration/system.conf` | `/System/Defaults/Network/network.conf` | the image builder, read-only |
| The administrator's | `/System/Settings/Configuration/system.conf` | `/System/Settings/Network/network.conf` | `settingsd` alone |

A reader takes a document from the first layer that exists
(`SystemConfigFile::read_effective`): the administrator's replaces the
image's whole. A layer that exists but cannot be read is the answer, never a
reason to read the next, so no reader acts on the image's settings while the
administrator's stand. Before the root is mounted only the image's layer can
be reached, which the device manager reads over the pre-unlock store
endpoint.

`settingsd`'s account owns the two directories of the administrator's layer:
`Configuration` is `0755`, so every account reads the machine
configuration, and `Network` is `0750` for the services group alone, because
the network configuration names the machine's hardware and its addresses.
The image's `Defaults/Network` directory has the same reach. Those two
directories are the whole of the service's authority over the filesystem.

## The protocol (`settings-v1`)

`lib/abi/src/settings_ipc.rs`, on the reserved rendezvous
`SETTINGS_ENDPOINT`. Two requests:

- **Read** a document: answered with the document as it stands. This is how
  an administrator reads the network configuration.
- **Set** settings in a document, by the names its registry spells: answered
  with the document as the change left it, so a caller adopts what was
  written rather than what it asked for. The empty value clears a network
  setting, which has no default to return to.

A change is made to the document as it stands and checked whole by its
engine before anything is written; a name the registry does not hold, a
value it refuses, a name given twice, a resulting network configuration that
does not hold together, or a stored document the engine cannot read writes
nothing at all. The administrator's layer is then replaced whole and at once
(`tairix_rt::replace_file`), so a crash leaves the old document or the new.
Requests are served one at a time, so two programs changing one document at
once lose neither change.

Every decode fails closed: a foreign magic or version, an unknown operation
or document, a dirty reserved byte, a change of no settings or more than
`MAX_SETTINGS`, a name past `NAME_MAX`, values past `DOCUMENT_MAX`, text that
is not UTF-8 or carries a control character, or trailing bytes.

## Who may ask

The kernel decides: the rendezvous is bound requiring `CAP_SETTINGS_WRITE` of
every caller, which only the administrator ceiling carries, so only a program
an administrator runs elevated — `configure`, Settings' system tasks, the
Date & Time dialog — holds it. The service checks the capability again from
the caller's attested origin, so a rendezvous bound without the gate fails
closed. It never tells the running network stack about a change: the tool
that asked holds `CAP_NET_ADMIN` and does that itself.

## What it holds

`CAP_IPC_BIND_PRIVILEGED`, `CAP_FS_ACCESS` and `CAP_LOG_EMIT`, the
`SETTINGSD_CEILING`; no `CAP_SETTINGS_WRITE` (it enforces it), no
`CAP_FS_CHOWN`, no `CAP_NET_ADMIN`, no spawn authority. Compromising it yields
the machine's settings files and nothing they do not say.

## Audit events

`34_000`–`34_999` (`lib/log::registry::SETTINGSD`): readiness, a refusal to
serve, every change applied or refused and every read answered or refused —
each with the caller's attested uid, the document and the errno — a request
refused before it named anything, and a caller the kernel would not attest.
