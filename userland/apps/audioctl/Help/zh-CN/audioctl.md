## NAME

audioctl — 列出声音设备并更改其控制

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

列出本会话可见的输出和输入：每个设备本次启动的编号、是否为其方向的默认设备、
它的音量和静音、运行的速率、丢失的帧数、位置和名称。`streams` 列出你自己的
音频流，加上 `--all` 则列出所有主体的音频流。

`default`、`level`、`mute` 和 `unmute` 更改一个设备的控制。设备以 `audio:`
引用命名：`audio:sink/default` 或 `audio:source/default` 表示当前的默认设备，
`audio:sink/<id>` 表示本次启动中的设备，`audio:sink/<location>` 则无论设备在
何处都指向它，均以列表中的名称为准。音量以分贝为单位，精确到百分之一，为 0
或以下，例如 `-6` 或 `-3.5`；负的音量无需 `--`。

设备的控制属于它所服务的房间。持有该房间的会话可以更改它们；房间无人占用时
任何人都可以；房间被扣留时谁都不行，被拒绝时会说明原因。会话所设的值归它自己：
另一会话持有房间期间，这些值暂时让位，它回来时又重新生效。每个设备起始的音量
以及优先作为默认的设备，是机器设置 `audio.level`、`audio.output` 和
`audio.input`，由 `configure` 设置。

在标准信息（fd 3）上，`audioctl streams` 只列出你自己的音频流时会写一条
`omission` 记录。

## OPTIONS

- `-a, --all` — 与 `streams` 一起，列出所有主体的音频流；需要 `CAP_SYSINFO_GLOBAL`。
- `-h, -?, --help` — 显示本命令自己的简短帮助。
- `--version` — 显示版本并退出。

## EXAMPLES

- `audioctl` — 列出输出和输入。
- `audioctl level audio:sink/default -10` — 把默认输出设为比满音量低十分贝。
- `audioctl mute audio:source/default` — 将默认输入静音。
- `audioctl default audio:sink/2` — 把输出 2 设为默认。
- `audioctl streams --all` — 列出所有主体的音频流。

## EXIT STATUS

- `0` — 命令已完成。
- `1` — 命令被拒绝，或无法执行。
- `2` — 无法理解命令行。

## ENVIRONMENT

- `LANG` — 简短帮助的首选语言区域（BCP-47 标签，例如 `fr-FR`）。

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
