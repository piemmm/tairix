## NAME

mount — listar os sistemas de ficheiros montados ou montar uma imagem de disco ou um arquivo

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Sem operandos, lista os sistemas de ficheiros montados, uma linha
`source on target type fstype (options)` por cada lugar onde um volume
é mostrado. `-t` lista então apenas os tipos que a sua lista separada
por vírgulas nomeia; uma lista que comece por `no` lista todos os tipos
que não nomeia.

Com um operando `image`, monta o volume que esse ficheiro de imagem de
disco contém: um disco ótico ISO 9660 ou UDF (`.iso`), uma disquete
Acorn ADFS ou AmigaDOS (`.adf`, `.adl`, `.adm`, `.ads`), uma partição
de disco rígido AmigaDOS (`.hdf`) ou um volume FAT32. O que a imagem
contém é reconhecido pelo seu conteúdo, nunca pelo seu nome. O volume
é montado só para si, em `/Storage/name`, com o nome tirado da
etiqueta do volume ou, na falta desta, do nome de ficheiro da imagem;
mais ninguém o pode abrir. É montado em leitura e escrita quando pode
escrever na imagem e o seu formato admite escrita, e só de leitura caso
contrário, o que o `mount` assinala com um aviso. Um volume montado a
partir de uma imagem nunca executa programas.

Um `target` da forma `/Storage/name` monta o volume sob esse nome.
Qualquer outro `target`, um diretório vazio que lhe pertença sob
`/Users` ou num volume em `/Storage`, mostra o volume também aí; um
diretório num volume FAT32 ou ADFS não o pode mostrar.

Um arquivo é montado da mesma forma, e os seus elementos passam a ser os
ficheiros e as pastas do volume: um arquivo zip (`.zip`), um arquivo
tar, sem compressão ou comprimido com gzip ou bzip2 (`.tar`, `.tar.gz`,
`.tgz`, `.tar.bz2`), um único ficheiro comprimido com gzip ou bzip2
(`.gz`, `.bz2`), um arquivo RISC OS Spark ou ArcFS (`.spk`, `.arc`) ou
um arquivo RAR (`.rar`). Os tipos de ficheiro e os atributos RISC OS
acompanham cada elemento. As alterações são guardadas substituindo o
ficheiro do arquivo, pelo que um arquivo só é montado em leitura e
escrita quando pode escrever tanto no ficheiro como na pasta onde este
se encontra. Um arquivo RAR é sempre montado só de leitura. Um arquivo
dividido em vários volumes, ou com os nomes dos elementos cifrados, não
é montado, e um elemento cifrado não pode ser lido.

O volume permanece montado até o desanexar com `unmount` ou até a sua
sessão terminar. Uma imagem que contenha um disco particionado, um dos
discos de uma matriz RAID ou um sistema de ficheiros cujos donos
registados são sempre respeitados (ext2, ext3, ext4, ARXFS) não é
montada, e o `mount` diz porquê.

## OPTIONS

- `-r, --read-only` — montar o volume só de leitura (como `-o ro`).
- `-w, --rw, --read-write` — montar o volume em leitura e escrita, ou
  não o montar de todo: uma imagem em que não possa escrever, ou cujo
  sistema de ficheiros não admita escrita, não é montada. Um `-r`
  posterior anula esta opção; um `-o ro` ou `-o rw` posterior não. O
  último indicado de `-r`, `-w`, `-o ro` e `-o rw` decide se o volume
  fica só de leitura.
- `-t, --types <list>` — os sistemas de ficheiros, separados por
  vírgulas, um dos quais a imagem tem de conter, tal como a listagem de
  montagens os designa (`iso9660`, `udf`, `adfs`, `affs`, `fat32`,
  `zip`, `tar`, `tar.gz`, `tar.bz2`, `gzip`, `bzip2`, `spark`, `arcfs`,
  `rar`), ou `auto` para o que quer que contenha; uma lista que comece
  por `no` nomeia os que não pode conter. O que a imagem contém é
  reconhecido pelo seu conteúdo, pelo que a ordem não escolhe nada: um
  disco ótico que contenha tanto UDF como ISO 9660 é montado como UDF, a
  menos que o seu ISO 9660 guarde registos Rock Ridge ou RISC OS. Uma
  imagem que não contenha nenhum deles, ou uma lista que não nomeie
  nenhum tipo que uma imagem de disco contenha, não é montada.
- `-o, --options <list>` — uma lista separada por vírgulas de `ro`,
  `rw` (leitura e escrita quando for possível, a predefinição),
  `noexec`, `loop` e `name=name`, o nome que o volume deve receber.
  `noexec` e `loop` são, de qualquer modo, a forma como uma imagem é
  montada.
- `-v, --verbose` — indicar onde o volume foi montado.
- `-l, --show-labels` — aceite para a listagem, que não tem etiquetas
  a acrescentar.
- `-h, -?, --help` — mostrar a ajuda curta deste comando.

## EXAMPLES

- `mount` — listar os sistemas de ficheiros montados.
- `mount Workbench.adf` — montar uma imagem de disquete Amiga, em
  `/Storage/workbench` se essa for a sua etiqueta.
- `mount -r disc.iso /Storage/disc` — montar só de leitura uma imagem
  de disco ótico como `disc`.
- `mount -v -t adfs Elite.adl` — montar uma imagem de disquete Acorn,
  indicando onde.
- `mount Photos.zip` — montar um arquivo zip, em leitura e escrita
  quando pode escrever nele e na sua pasta.
- `mount -r backup.tar.gz /Storage/backup` — montar só de leitura um
  arquivo tar comprimido como `backup`.

## EXIT STATUS

- `0` — a listagem foi escrita ou o volume foi montado (ou a ajuda curta
  foi escrita).
- `1` — a linha de comandos não foi compreendida.
- `2` — falhou a leitura da listagem de montagens ou a escrita no
  terminal.
- `32` — a imagem não foi montada.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
