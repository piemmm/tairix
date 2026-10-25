## NAME

unmount — desanexar um volume montado

## SYNOPSIS

`unmount [option...] name`

## DESCRIPTION

Retira de serviço o volume montado sob `name`: o sistema de ficheiros
e o dispositivo são despejados, a montagem sob `/Storage` é retirada
e a raiz durável `id::` do volume é revogada. `name` é o nome de
catálogo do volume (`usb1`) ou o seu ponto de montagem
(`/Storage/usb1`), comparado com a lista de montagens da API de
informação do sistema.

Um volume cujo dispositivo foi removido ainda com escritas por
confirmar permanece visível como `unavailable-dirty` (ou
`unavailable-lost`) na lista de montagens, e um `unmount` simples
recusa: os dados retidos são guardados para uma reinserção verificada.
Um volume no qual ainda há ficheiros abertos, ou que um programa tem
como diretório de trabalho, também recusa um `unmount` simples.
`--force` é a saída deliberada — os ficheiros ainda abertos no volume
são desligados, um programa que nele trabalhe fica num diretório que já
não existe, os dados retidos são descartados, o volume é retirado e
aquilo de que se prescindiu fica registado no registo de auditoria. Num
volume saudável, `--force` continua a despejar e desanexar de forma
limpa; nada é descartado quando uma confirmação limpa é possível.

Desanexar exige a autoridade de montagem (`CAP_FS_MOUNT`); o núcleo
verifica-a e audita cada decisão. Os volumes de arranque permanentes
e as ligações de vista do sistema não são desanexáveis.

## OPTIONS

- `-f, --force` — desmontagem forçada: retirar o volume mesmo enquanto
  há ficheiros abertos nele, um programa trabalha nele ou os seus dados
  por confirmar não podem sê-lo, desligando os ficheiros abertos e
  descartando os dados retidos.
- `-?, --help` — mostrar a ajuda curta deste comando.

## EXAMPLES

- `unmount usb1` — desanexar de forma limpa o volume montado como
  `usb1`.
- `unmount /Storage/usb1` — o mesmo, indicado pelo ponto de montagem.
- `unmount --force usb1` — retirar um volume indisponível descartando
  os seus dados retidos.

## EXIT STATUS

- `0` — o volume foi desanexado (ou a ajuda curta foi escrita).
- `1` — o volume não foi encontrado, não é desanexável ou o núcleo
  recusou a desanexação. Uma linha de comandos não compreendida também
  termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `mount`
- `df`
- `man`
