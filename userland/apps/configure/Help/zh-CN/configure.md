## NAME

configure — 读取并更改本机设置

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

列出、显示并更改本机设置，这些设置保存在
`/System/Settings/Configuration/system.conf`。不带操作数时，列出每项设置
及其当前值；只给出键时，显示该设置的值；给出一个或多个 `<key> <value>`
对时，一并更改这些设置。

多个键值对构成一次更改，而不是多次：在做任何更改之前先检查每一对，然后一
次性保存这些设置。命令行中只要出现未知的键、超出其键取值集合的值，或同一
个键出现两次，就完全不做任何更改，因此一组设置绝不会只应用了一半。

更改设置需要管理员权限：普通账户可以读取本机设置，但不能更改其中任何一
项。系统自带一套设置，在管理员更改其中某项之前一直有效；第一次更改会保留
系统自带的其余所有设置。更改在读取它的那部分系统下次启动时生效
（`os.loginType` 在下次登录时，`cache.*` 开关在下次启动时），除非下文该
键另有说明。

键的集合是封闭的：未知的键，或超出某键取值集合的值，会在指明有效
选项后被拒绝，且不做任何更改。

- `os.loginType` — `text` 或 `graphical`：登录服务为已认证用户启动的
  会话类型。`graphical`（默认）在认证后直接启动桌面会话，在无法运行
  桌面的机器上退回文本登录；`text` 启动账户的 shell——桌面只能从
  图形登录启动。
- `cache.all` — `on` 或 `off`：缓存的总开关。`on`（默认）让下面每
  个缓存类别遵循各自的设置；`off` 是一个上限，无论各类别的
  设置如何，都禁用所有内存缓存。
- `cache.filesystem`、`cache.block`、`cache.transform`、
  `cache.semantic` — `auto` 或 `off`：针对四个可回收内存缓存
  （文件系统、整盘块、解压后的簇以及应用启动缓存）的分
  类开关。`auto`（默认）让内存压力管理器控制该类别；`off`
  完全禁用它。没有分类的 `on`：无法强制某个类别忽略内存
  压力。只要 `cache.all` 为 `off`，该类别实际上就是 `off`。

每个缓存都是可回收的加速器，绝不是真实来源，因此关闭其中
任何一个或全部，只会使受影响的工作变慢——绝不会改变结果。

- `net.ipv4.enabled`、`net.ipv6.enabled` — `true` 或 `false`：
  全栈地址族开关。两者默认均为 `true`。被禁用的地址族不绑定地址、
  不回应任何报文，并以带类型的错误拒绝该地址族的套接字——绝不
  静默丢弃。
- `net.ipv6.privacy` — `true` 或 `false`：栈是否在稳定地址之外
  生成临时（隐私）IPv6 地址。`false`（默认）仅使用稳定的 SLAAC
  地址。
- `net.tcp.syncookies` — `auto` 或 `always`：SYN 洪水防御策略。
  `auto`（默认）保持有界的半开队列，并在溢出时回退到无状态
  cookie；`always` 对每个连接请求都以无状态方式回应。没有 `off`
  ——不设防的连接队列不是一种设置。
- `net.tcp.keepalive` — `true` 或 `false`：TCP 连接是否在空闲链路上
  发送保活探测。`false`（默认）从不探测，也不断开空闲连接；`true`
  在通常的间隔后探测空闲对端，若其停止响应则断开连接。
- `net.tcp.ecn` — `true` 或 `false`：TCP 连接是否协商显式拥塞通知
  （ECN）。`false`（默认）使连接保持 Not-ECT；`true` 在握手中提供
  ECN，此后将拥塞标记视为减速信号，而不是强制丢包。
- `net.sockets.mem` — `auto` 或形如 `64M` 的字节大小：网络栈在所有主体
  之间可用于套接字状态的内存。`auto`（默认）依据本机内存推导，使大型服务器
  不受限于在小机器上选定的数字；给出大小则由更了解负载的管理者覆盖它。每
  个主体可持有实际预算的十六分之一，因此总能容纳十六个主体。以字节而非套
  接字数量计，因为同样数量的套接字在空闲时只有几千字节，缓冲满时则达数兆
  字节：预算按负载的真实形态，或承载许多安静连接，或承载较少的繁忙连接。
- `time.servers` — `none` 或以逗号分隔的网络时间服务器列表，每项为主机名
  或地址。`none`（默认）表示不指定自己的服务器：时钟将依据网络地址租约所
  指定的服务器设置，否则依据公共服务器池 `pool.ntp.org`。此处指定的服务器
  优先于这两者。
- `time.refresh` — `6h`、`12h`、`1d`、`2d` 或 `7d`：时间已知后，两次时钟
  查询之间经过的运行时间。默认为 `1d`。未设置、不合理或过于陈旧的时钟，
  无论此设置为何，都会在网络可用后立即校正。
- `time.network` — `on`（默认）或 `off`：是否通过网络设置时钟。设为 `off`
  时不会向任何服务器发出时间查询，时钟由手动设置或由主板的时钟芯片设置；
  无论如何，每次启动都会读取该芯片。更改立即生效；此设置保存在系统磁盘上，
  因此启动之初、尚未访问该磁盘时适用默认值。
- `time.zone` — `default` 或时区名称，如 `Europe/London`：本机所用的时
  区，即登录界面显示的时区，也是未自行选择时区的账户所用的时区。
  `default` 是系统自带的时区；系统时区列表中没有的名称会被忽略，视同
  `default`。更改立即生效。
- `input.mouse.debounce` — 整毫秒，默认 `25`，`0` 表示关闭，最大 `100`：
  鼠标按键松开之后，多长时间内同一按键的下一次按下会被当作开关抖动忽略，
  而不是当作一次新的点击。磨损的微动开关可能在松开后几毫秒内报告第二次
  按下，而它本意只是一次点击。若鼠标的连点模式会刻意发送成对的点击，请
  设为 `0`。
- `audio.output`、`audio.input` — 默认 `auto`，或 `audioctl` 列出的端点位置
  （十六位十六进制数字、一个点和端点序号）：本机默认优先使用的输出和输入。
  会话自己的选择优先；这是任何人登录之前以及没有桌面的机器上的选择。
  `auto` 表示最先发现的设备。
- `audio.level` — 以分贝为单位，默认 `0dB`，或 `-12dB`、`-6.5dB` 这样的
  衰减，最多两位小数：每个输出和输入的起始电平。绝不高于 `0dB`。音频服务
  在下次启动时采用更改后的值；`audioctl` 用于更改正在运行的机器。
- `cooling.fan.mode` — 默认 `off`，或 `on`、`auto`：电路板 GPIO 排针上的
  CPU 风扇是不驱动、一直运转，还是按处理器温度开关。
- `cooling.fan.line` — 默认 `none`，或风扇所接的 GPIO 线，从 `0` 到 `63`。
  只能驱动电路板排针出借的线。
- `cooling.fan.signal` — 默认 `active-high`，或 `active-low`：线的哪个电平
  让风扇运转。
- `cooling.fan.start`、`cooling.fan.hysteresis` — 摄氏整度，默认 `65` 和
  `10`：自动风扇在 `start`（`40` 到 `85`）时启动，处理器降温
  `hysteresis`（`2` 到 `20`）后停止。风扇设置的更改立即生效。

不在上述列表中的键，会到第二个登记表中查找：每个接口的网络设置，保存在
`/System/Settings/Network/network.conf`。其键写作
`<interface>.<setting>`，其中 `<interface>` 是管理员为网络接口起的别名
（`wan`、`lan0`），`<setting>` 是 `kind`、`match.mac`、`match.node`、
`ipv4.method`、`ipv4.address`、`ipv4.gateway`、`ipv6.method`、
`ipv6.address`、`ipv6.gateway`、`dns.servers`、`mtu`、`bond.members`、
`bond.mode`、`bond.monitor-interval` 和 `bond.primary` 之一。

总是先查找上述本机登记表，因此接口别名永远无法占用本机设置的名称。

网络设置没有默认值，因此列出时只显示已设置的项；显示未设置的项时以空行作
答，而不是编造一个值。读取或更改这些设置需要管理员权限：它们指明了每个接
口的硬件和本机的地址。其他任何人列出时，只显示本机设置，并说明未显示网络
设置。

由于该登记表没有默认值，设置只能删除，不能重置，删除的方式是给出
**空值**：`configure wan.ipv4.address ""`。没有任何接口设置接受空值，因
此这种写法绝不会被误认为是在设置某个值。正因如此，一条命令就能把接口从静
态地址改为 DHCP：
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`。
这一更改的任何一半单独来看，都描述不出一个自洽的配置。最后一项设置被删除
的接口，就完全不再被声明。

保存任何内容之前，都会先检查编辑后的整个配置：凡是会使配置不一致的更改
（静态方法却没有地址，或成员少于两个的链路聚合），都会被拒绝，且不保存任
何内容。

更改 `net.*` 设置或某个接口的设置，会保存该设置并将其送达正在运行的网络
栈，因此立即生效。只有该更改实际影响到的接口才会被送达。如果正在运行的网
络栈不接受送达（没有网络栈在运行，或你的账户无权管理网络），该设置仍会被
保存，`configure` 也会说明这一点；之后它会在下次启动时生效。

有两项限制会如实报告，而不是隐藏起来。你从配置中移除的接口，在下次启动之
前会继续按网络栈最近一次收到的配置运行，因为网络栈没有用于撤下接口的消
息。保存时既没有 `match.mac` 也没有 `match.node` 的接口永远无法绑定到设
备，因此它会被保存，同时注明此项被拒绝。

## OPTIONS

- `-h, -?` — 显示本命令自己的简短帮助。

## EXAMPLES

- `configure` — 列出全部设置。
- `configure os.loginType` — 显示默认会话类型。
- `configure os.loginType graphical` — 启动到图形登录。
- `configure cache.all off` — 禁用全系统的所有内存缓存。
- `configure cache.filesystem off` — 仅禁用文件系统缓存。
- `configure net.ipv6.enabled false` — 在全栈范围关闭 IPv6。
- `configure time.servers 0.example.test,1.example.test` — 设置时钟同步
  所用的网络时间服务器。
- `configure time.zone Europe/London` — 让本机使用伦敦时间。
- `configure wan.ipv4.address` — 显示为名为 `wan` 的接口配置的静态 IPv4
  地址。
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  为 `wan` 设置静态 IPv4 地址和默认网关。
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — 将 `wan` 从静态寻址改为 DHCP。
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — 设置在 `wan` 上使用
  的递归 DNS 服务器。

## EXIT STATUS

- `0` — 列表、取值、简短帮助或更改已完成。
- `1` — 无法读取或更改设置（例如调用者无权更改本机设置，或无权读取其网络
  设置）、键未知、值超出该键的取值集合、该更改会使网络配置不一致，或输出无
  法送达。无法理解的命令行同样以 `1` 退出。

## ENVIRONMENT

- `LANG` — 简短帮助的首选语言（BCP-47 标签，如 `fr-FR`）。

## SEE ALSO

- `man`
