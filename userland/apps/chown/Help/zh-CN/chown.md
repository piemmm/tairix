## NAME

chown — 更改文件的所有者和组

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

按顺序更改每个文件操作数的所属用户和组。`OWNER` 更改用户并保留组，
`OWNER:GROUP` 同时更改两者，`:GROUP` 只更改组。`OWNER:` 还会把所有者的主
组赋给文件。空操作数或单独的 `:` 两者都不更改。旧的 `OWNER.GROUP` 写法仍
可使用，但会给出警告，除非整个操作数就是一个账户名。带 `--reference` 时，
每个文件获得 `RFILE` 所指对象的所有者和组。

`OWNER` 和 `GROUP` 是账户名和组名，或数字：先尝试名称，只有没有任何对象
叫这个名称时才接受数字。开头的 `+`（`+1000`）总是表示数字。

把文件交给另一个账户，或交给你不是其成员的组，需要 `CAP_FS_CHOWN`
capability；没有它时，你可以把自己拥有的文件的组改为你自己的某个组。更改
会清除文件的 set-user-ID 位，并在其组可以执行该文件时清除其 set-group-ID
位；目录则两者都保留。

作为操作数给出的符号链接，更改的是它所指向的文件，带 `-h` 时则更改链接本
身。带 `-R` 时，目录操作数及其中的一切都会被更改，每个目录都在其内容之后
更改。在树中遇到的符号链接会更改其本身，它所指向的对象绝不会被触及；带
`--dereference` 时链接本身也保持不变。带 `-R` 时，作为操作数给出的符号链
接会更改其本身而不被跟随：`--dereference` 改为更改它所指向的对象，`-H`
则遍历它所指向的目录。无法到达或无法更改的文件会被报告，其余的仍会被更
改。报告中每个所有者和组都以名称表示，没有账户或组拥有该号码时则以号码表
示。`--` 结束选项解析：之后的每个参数都是操作数。

## OPTIONS

- `-c, --changes` — 只报告所有权确实发生变化的文件。
- `-f, --silent, --quiet` — 抑制大多数错误消息；运行仍会失败，退出状态会
  报告这一点。
- `-v, --verbose` — 报告每个处理过的文件，包括更改被拒绝或无法到达的文
  件。
- `--dereference` — 更改符号链接所指向的对象，而不是链接本身；不带 `-R`
  时这是默认行为。
- `-h, --no-dereference` — 更改符号链接本身，而不是它所指向的对象。
- `--preserve-root` — 带 `-R` 时，无论经由何种途径到达，都拒绝更改 `/`。
- `--no-preserve-root` — 把 `/` 当作任何其他目录对待（默认）。
- `--from=CURRENT_OWNER:CURRENT_GROUP` — 仅当文件当前的所有者和组正是这
  些时才更改它；省略其中之一即可接受任意值。
- `--reference=RFILE` — 不指定名称，而是把 `RFILE` 的所有者和组赋给每个
  文件。
- `-R, --recursive` — 递归更改文件和目录。
- `-H` — 带 `-R` 时，遍历作为操作数给出的符号链接所通向的目录。
- `-L` — 带 `-R` 时，跟随每个指向目录的符号链接；会被拒绝，因为递归更改
  从不跟随树内的链接。
- `-P` — 带 `-R` 时，不跟随任何符号链接（默认）。`-H`、`-L` 和 `-P` 以最
  后给出者为准。
- `-?, --help` — 显示本命令自身的简短帮助。
- `--version` — 打印工具名称和版本。

## EXAMPLES

- `chown alice notes.txt` — 把 `notes.txt` 交给 `alice`，保留其组。
- `chown alice:staff notes.txt` — 把它交给 `alice` 和组 `staff`。
- `chown :staff shared.txt` — 只更改组。
- `chown -R alice Projects` — 把 `Projects` 树交给 `alice`。
- `chown -R --from=bob alice Shared` — 只把 `Shared` 中 `bob` 拥有的东西
  交给 `alice`。
- `chown --reference=template.txt notes.txt` — 把 `template.txt` 的所有
  者和组赋给 `notes.txt`。

## EXIT STATUS

- `0` — 每次更改都成功。
- `1` — 有文件无法到达或无法更改，或输出失败；每个原因都打印到标准错误
  （`-f` 下被抑制）。无法理解的命令行，或无法读取的所有者、组或参考文
  件，同样以 `1` 退出，并且不做任何更改。

## ENVIRONMENT

- `LANG` — 简短帮助的首选区域设置（BCP-47 标签，例如 `zh-CN`）。

## SEE ALSO

- `chmod`
- `ls`
- `stat`
