## NAME

audioctl — listar os dispositivos de som e alterar os seus controlos

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Lista as saídas e as entradas que esta sessão vê: o identificador de cada
uma para este arranque, se é a predefinida do seu sentido, o seu nível e o seu
silêncio, a frequência a que funciona, as tramas perdidas, a sua localização e
o seu nome. `streams` lista os seus próprios fluxos e, com `--all`, os de
todos os principais.

`default`, `level`, `mute` e `unmute` alteram os controlos de um
dispositivo. Um dispositivo é indicado por uma referência `audio:`:
`audio:sink/default` ou `audio:source/default` para a predefinida nesse
momento, `audio:sink/<id>` para este arranque, ou `audio:sink/<location>` onde
quer que o dispositivo esteja, tal como a lista os nomeia. Um nível é em
decibéis à centésima, 0 ou menos, como `-6` ou `-3.5`; um nível negativo não
precisa de `--`.

Os controlos de um dispositivo pertencem à sala que serve. A sessão que ocupa
essa sala pode alterá-los, qualquer um pode enquanto a sala está livre, e
ninguém enquanto está retida; uma recusa di-lo. O que uma sessão define é seu:
enquanto outra sessão ocupa a sala, afasta-se, e volta quando ela regressa. O
nível com que cada dispositivo começa e os dispositivos preferidos como
predefinidos são as definições da máquina `audio.level`, `audio.output` e
`audio.input`, que o `configure` define.

Na informação padrão (fd 3), `audioctl streams` escreve um registo `omission`
quando lista apenas os seus próprios fluxos.

## OPTIONS

- `-a, --all` — com `streams`, os fluxos de todos os principais; requer `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — mostrar a ajuda breve deste comando.
- `--version` — mostrar a versão e sair.

## EXAMPLES

- `audioctl` — listar as saídas e as entradas.
- `audioctl level audio:sink/default -10` — pôr a saída predefinida dez decibéis abaixo do máximo.
- `audioctl mute audio:source/default` — silenciar a entrada predefinida.
- `audioctl default audio:sink/2` — tornar a saída 2 a predefinida.
- `audioctl streams --all` — listar os fluxos de todos os principais.

## EXIT STATUS

- `0` — o comando foi concluído.
- `1` — foi recusado, ou não pôde ser executado.
- `2` — a linha de comandos não foi compreendida.

## ENVIRONMENT

- `LANG` — a localização preferida para a ajuda breve (uma etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
