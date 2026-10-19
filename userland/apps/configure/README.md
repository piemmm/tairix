# configure

The TAIRiX machine-settings command (the `sysctl`-shaped settings tool):
list, show, and set the machine configuration
(`/System/Settings/Configuration/system.conf`) and each interface's
network configuration (`/System/Settings/Network/network.conf`).

- `configure` — list every setting and its current value.
- `configure <key>` — show one setting's current value.
- `configure <key> <value> [<key> <value>]...` — set one or more settings
  together, each document changed all or none (e.g.
  `configure os.loginType graphical`).

A key resolves against the flat machine registry (`lib/sysconfig`: `os.*`,
`cache.*`, `net.*`, `time.*`, `input.*`, `audio.*`) first, then against the
per-interface `<iface>.<suffix>` registry (`lib/netconfig`), whose empty
value clears a setting. Both engines are the ones every consumer reads
through, so this tool and those readers can never diverge.

Each document has two layers: the image's default under `/System/Defaults`,
and the administrator's under `/System/Settings`, which replaces it whole
once it exists. The tool reads the machine configuration's layers itself;
the settings service (`userland/system/settingsd`) reads the network
configuration — which names the machine's hardware and addresses — and
makes every change, to the document as it stands. The service admits only a
caller holding `CAP_SETTINGS_WRITE`, so an ordinary account lists the
machine settings and is told the network ones are not shown, and only an
elevated administrator changes a setting. Every pair is checked against the
documents before the service is asked for anything, so a refused pair
changes nothing and the refusal names the key.

A changed `net.*` key or interface is then handed to the running network
stack over its `CAP_NET_ADMIN` admin endpoint, from the document the
service wrote; a refusal there leaves the change saved for the next boot and
says so.

The tool requests `CAP_CONSOLE_WRITE` (its output and short help),
`CAP_FS_ACCESS` (the machine configuration and its own `Help/` tree),
`CAP_SETTINGS_WRITE` (the settings service), and `CAP_NET_ADMIN` (the
running stack), nothing else.
