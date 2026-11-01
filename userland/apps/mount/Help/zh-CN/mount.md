## NAME

mount — 列出已挂载的文件系统，或挂载磁盘映像

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

不带操作数时，列出已挂载的文件系统：卷每显示在一处，就输出一行
`source on target type fstype (options)`。此时 `-t` 只列出其逗号分隔
列表所列的类型；以 `no` 开头的列表列出其未列出的所有类型。

带 `image` 操作数时，挂载该磁盘映像文件所含的卷：ISO 9660 或 UDF
光盘（`.iso`）、Acorn ADFS 或 AmigaDOS 软盘
（`.adf`、`.adl`、`.adm`、`.ads`）、AmigaDOS 硬盘分区（`.hdf`），或 FAT32
卷。映像中包含什么，根据其内容识别，绝不根据其名称。该卷只为你一人
挂载，位于 `/Storage/name`，名称取自卷标，没有卷标时取自映像的文件名；
其他任何人都不能打开它。若你有权写入该映像且其格式支持写入，
则以读写方式挂载；否则以只读方式挂载，`mount` 会就此发出警告。从映像
挂载的卷绝不会运行其中的程序。

`target` 为 `/Storage/name` 时，以该名称挂载卷。其他任何
`target`（即 `/Users` 下或 `/Storage` 中某个卷上你自己的一个空目录）
会让该卷也显示在那里；FAT32 或 ADFS 卷上的目录无法显示它。

该卷会一直保持挂载，直到你用 `unmount` 将其分离，或你的登录会话结束。
若映像包含的是已分区的磁盘、RAID 阵列中的一块磁盘，或其记录的所有者
始终被遵循的文件系统（ext2、ext3、ext4、ARXFS），则不会被挂载，`mount`
会说明原因。

## OPTIONS

- `-r, --read-only` — 以只读方式挂载卷（同 `-o ro`）。
- `-w, --rw, --read-write` — 以读写方式挂载卷，否则不予挂载：你无权写入的
  映像，或其文件系统不支持写入的映像，都不会被挂载。之后给出的 `-r`
  会撤销此选项；之后给出的 `-o ro` 或 `-o rw` 则不会。卷是否为只读，由
  `-r`、`-w`、`-o ro` 和 `-o rw` 中最后给出的那个决定。
- `-t, --types <list>` — 以逗号分隔的文件系统，映像必须包含其中之一，以
  挂载列表所用的名称表示（`iso9660`、`udf`、`adfs`、`affs`、`fat32`），
  或以 `auto` 表示接受映像所含的任何文件系统；以 `no` 开头的列表列出映像
  不得包含的文件系统。映像中包含什么，根据其内容识别，因此列表中的顺序
  不起任何选择作用：同时包含 UDF 和 ISO 9660 的光盘作为 UDF 挂载，除非其
  ISO 9660 保留了 Rock Ridge 或 RISC OS 记录。映像若不包含其中任何一种，
  或列表未列出任何磁盘映像会包含的类型，则不会被挂载。
- `-o, --options <list>` — 以逗号分隔的列表，可包含 `ro`、`rw`（在可行时
  以读写方式挂载，为默认值）、`noexec`、`loop` 和 `name=name`（卷要采用
  的名称）。`noexec` 和 `loop` 本来就是映像的挂载方式。
- `-v, --verbose` — 说明卷挂载在何处。
- `-l, --show-labels` — 为列表而接受；列表没有可添加的标签。
- `-h, -?, --help` — 显示本命令自身的简短帮助。

## EXAMPLES

- `mount` — 列出已挂载的文件系统。
- `mount Workbench.adf` — 挂载一个 Amiga 软盘映像；若其卷标为
  workbench，则位于 `/Storage/workbench`。
- `mount -r disc.iso /Storage/disc` — 以只读方式将光盘映像挂载为 `disc`。
- `mount -v -t adfs Elite.adl` — 挂载一个 Acorn 软盘映像，并说明
  挂载在何处。

## EXIT STATUS

- `0` — 已输出列表，或卷已挂载（或已输出简短帮助）。
- `1` — 无法理解命令行。
- `2` — 无法读取挂载列表，或无法写入终端。
- `32` — 映像未被挂载。

## ENVIRONMENT

- `LANG` — 简短帮助的首选区域设置（BCP-47 标签，如 `fr-FR`）。

## SEE ALSO

- `unmount`
- `df`
- `man`
