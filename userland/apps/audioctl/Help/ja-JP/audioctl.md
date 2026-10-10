## NAME

audioctl — サウンドデバイスを一覧し、その設定を変更する

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

このセッションから見える出力先と入力元を一覧します。各デバイスの今回の起動で
の ID、その方向の既定かどうか、レベルとミュート、動作中のレート、失われた
フレーム数、位置、名前を示します。`streams` は自分のストリームを一覧し、
`--all` を付けるとすべての主体のストリームを一覧します。

`default`、`level`、`mute`、`unmute` は一つのデバイスの設定を変更します。
デバイスは `audio:` 参照で指定します。`audio:sink/default` または
`audio:source/default` はその時点の既定、`audio:sink/<id>` は今回の起動での
デバイス、`audio:sink/<location>` はデバイスがどこにあってもそれを指し、いずれ
も一覧が示す名前です。レベルは 0 以下のデシベルで 100 分の 1 まで指定し、
`-6` や `-3.5` のように書きます。負のレベルに `--` は要りません。

デバイスの設定は、そのデバイスが受け持つルームのものです。ルームを持つセッ
ションは変更でき、ルームが誰のものでもないときは誰でも変更でき、保留中は誰も
変更できません。拒否された場合はその旨を示します。セッションが設定した値はそ
のセッションのものです。別のセッションがルームを持つ間は退き、戻ると再び有効
になります。各デバイスの初期レベルと既定として優先するデバイスは、マシンの設
定 `audio.level`、`audio.output`、`audio.input` で、`configure` が設定します。

標準情報（fd 3）には、自分のストリームだけを一覧したとき、`audioctl
streams` が `omission` レコードを書き込みます。

## OPTIONS

- `-a, --all` — `streams` と共に使い、すべての主体のストリームを一覧する。`CAP_SYSINFO_GLOBAL` が必要。
- `-h, -?, --help` — このコマンド自身の短いヘルプを表示する。
- `--version` — バージョンを表示して終了する。

## EXAMPLES

- `audioctl` — 出力先と入力元を一覧する。
- `audioctl level audio:sink/default -10` — 既定の出力先を最大より 10 デシベル低くする。
- `audioctl mute audio:source/default` — 既定の入力元をミュートする。
- `audioctl default audio:sink/2` — 出力先 2 を既定にする。
- `audioctl streams --all` — すべての主体のストリームを一覧する。

## EXIT STATUS

- `0` — コマンドが完了した。
- `1` — 拒否された、または実行できなかった。
- `2` — コマンドラインを理解できなかった。

## ENVIRONMENT

- `LANG` — 短いヘルプに使う言語（`fr-FR` のような BCP-47 タグ）。

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
