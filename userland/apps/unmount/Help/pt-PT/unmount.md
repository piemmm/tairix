## NAME

unmount — desanexar um volume montado

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Retira de serviço o volume montado sob `name`: o sistema de ficheiros
e o dispositivo são despejados, a montagem sob `/Storage` é retirada
e a raiz durável `id::` do volume é revogada. `name` é o nome de
catálogo do volume (`usb1`), um diretório em que está montado
(`/Storage/usb1`, ou aquele a que `mount imagem diretório` o ligou), ou
o nome de ficheiro da imagem de disco a partir da qual foi montado
(`Workbench.adf`), comparado com as listas de montagens e de vistas da
API de informação do sistema; um nome de ficheiro que a imagem de outro
utilizador partilhe designa a imagem montada por si. Um nome de ficheiro
que possa designar mais de um volume, ou de que a lista guarde apenas o
início, não designa nenhum: indique então o volume pelo local onde está
montado.
Um caminho pode ser relativo ou terminar numa barra: é resolvido como
o `mount` resolve um. O volume privado de outro utilizador não mostra
nem o seu nome de ficheiro nem os diretórios a que está ligado, pelo que
se indica pelo local onde está montado em `/Storage`.

Um volume cujo dispositivo foi removido ainda com escritas por confirmar
permanece visível como `unavailable-dirty` na lista de montagens, e um
`unmount` simples recusa: os dados retidos são guardados para uma
reinserção verificada. O mesmo acontece com um volume reinserido sem que
se tenha podido demonstrar que ficou inalterado (`recovery-conflict`), o
qual ainda retém esses dados, e com um volume cujas escritas por
confirmar não puderam ser mantidas (`unavailable-lost`), para que a sua
perda seja reconhecida em vez de passar em claro. Um volume no qual
ainda há ficheiros abertos, ou que um programa tem como diretório de
trabalho, também recusa um `unmount` simples. `--force` é a saída
deliberada — os ficheiros ainda abertos no volume são desligados, um
programa que nele trabalhe fica num diretório que já não existe, os
dados retidos são descartados, o volume é retirado e aquilo de que se
prescindiu fica registado no registo de auditoria. Num volume saudável,
`--force` continua a despejar e desanexar de forma limpa; nada é
descartado quando uma confirmação limpa é possível.

Uma imagem de disco ou uma partilha de rede que tenha montado pode ser
desanexada por si; qualquer outro volume exige a autoridade de
montagem (`CAP_FS_MOUNT`). O núcleo verifica-a e audita cada decisão.
Os volumes de arranque permanentes e as ligações de vista do sistema
não são desanexáveis.

## OPTIONS

- `-f, --force` — desmontagem forçada: retirar o volume mesmo enquanto
  há ficheiros abertos nele, um programa trabalha nele ou os seus dados
  por confirmar não podem sê-lo, desligando os ficheiros abertos e
  descartando os dados retidos.
- `-h, -?, --help` — mostrar a ajuda curta deste comando.

## EXAMPLES

- `unmount usb1` — desanexar de forma limpa o volume montado como
  `usb1`.
- `unmount /Storage/usb1` — o mesmo, indicado pelo ponto de montagem.
- `unmount Workbench.adf` — desanexar a imagem de disco montada a
  partir de `Workbench.adf`.
- `unmount --force usb1` — retirar um volume indisponível descartando
  os seus dados retidos.

## EXIT STATUS

- `0` — o volume foi desanexado (ou a ajuda curta foi escrita).
- `1` — a linha de comandos não foi compreendida, ou quem chama não pode
  desanexar o volume.
- `2` — o serviço de informação do sistema, ou a escrita no terminal,
  falhou.
- `32` — o volume não estava montado ou não é desanexável, o caminho que
  o indica não pôde ser resolvido, ou a sua desanexação falhou.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `mount`
- `df`
- `man`
