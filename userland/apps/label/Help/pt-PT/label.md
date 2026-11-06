## NAME

label — mostrar ou alterar o título de um volume

## SYNOPSIS

`label [option...] name [title]`

## DESCRIPTION

Mostra o título do volume que `name` designa ou, se for dado `title`,
define-o. O título de um volume é a etiqueta que o seu sistema de
ficheiros regista: o nome com que é mostrado e o que recebe em
`/Storage` da próxima vez que for montado. `name` é o nome de catálogo
do volume (`usb1`), um diretório onde está montado (`/Storage/usb1`),
o nome do ficheiro da imagem de disco a partir da qual foi montado
(`Workbench.adf`) ou a sua raiz duradoura (`id::…`), designados como
para `unmount`. Um `title` vazio apaga o título onde o formato do
volume o permite.

Enquanto o volume se mantiver montado, o seu nome em `/Storage` não
muda, pelo que nada que o designe aí deixa de funcionar; o novo título
designa-o a partir da próxima montagem. A sua identidade e a dos seus
ficheiros mantêm-se.

Cada formato escreve um título à sua maneira, e um título que não
consegue guardar é recusado em vez de encurtado. Um volume FAT aceita
até onze letras maiúsculas, algarismos, espaços e a pontuação que um
nome de ficheiro curto permite, sem começar nem terminar com um espaço;
um volume ext2, ext3 ou ext4 até dezasseis bytes; um volume ADFS até
dez carateres; um volume AmigaDOS de um a trinta; um volume UDF até
126; e um volume ARXFS até 252 bytes, a menos que esteja cifrado. Um
disco gravado com título, um arquivo e uma partilha de rede não aceitam
um novo.

Uma imagem de disco que tenha montado é sua para mudar de título. Um
disco amovível — uma pen USB, um cartão — é seu para mudar de título
enquanto usa o próprio computador, se puder alterar o que está na sua
pasta de topo; qualquer outro volume exige a autoridade de montagem
(`CAP_FS_MOUNT`), que um administrador tem. O núcleo verifica-o e
regista cada decisão no registo de auditoria.

## OPTIONS

- `-h, -?, --help` — mostrar a ajuda breve deste comando.
- `--version` — mostrar o nome e a versão deste comando.

## EXAMPLES

- `label usb1` — mostrar o título do volume montado como `usb1`.
- `label usb1 HOLIDAY` — dar-lhe o título `HOLIDAY`.
- `label /Storage/backup 'Backup 2026'` — dar um título ao volume
  montado em `/Storage/backup`.
- `label usb1 ''` — apagar o seu título.

## EXIT STATUS

- `0` — o título foi mostrado ou definido (ou foi escrita a ajuda ou a
  versão).
- `1` — a linha de comandos não foi compreendida, o nome não designava
  nenhum volume, o título não o era, ou o volume recusou-o.
- `2` — o serviço de informações do sistema, ou a escrita no terminal,
  falhou.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda breve (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `mount`
- `unmount`
- `df`
