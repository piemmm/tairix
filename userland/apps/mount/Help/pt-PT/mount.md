## NAME

mount — listar os sistemas de ficheiros montados ou montar uma imagem de disco

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
`/Users`, mostra o volume também aí.

O volume permanece montado até o desanexar com `unmount` ou até a sua
sessão terminar. Uma imagem que contenha um disco particionado, um dos
discos de uma matriz RAID ou um sistema de ficheiros cujos donos
registados são sempre respeitados (ext2, ext3, ext4, ARXFS) não é
montada, e o `mount` diz porquê.

## OPTIONS

- `-r, --read-only` — montar o volume só de leitura (como `-o ro`).
- `-w, --rw, --read-write` — montar o volume em leitura e escrita, ou
  não o montar de todo: uma imagem em que não possa escrever, ou cujo
  sistema de ficheiros não admita escrita, não é montada. O último
  indicado de `-r`, `-w`, `-o ro` e `-o rw` vence.
- `-t, --types <type>` — o sistema de ficheiros que a imagem tem de
  conter, tal como a listagem de montagens o designa (`iso9660`, `udf`,
  `adfs`, `affs`, `fat32`), ou `auto` para o que quer que contenha.
  Uma imagem que contenha outro, ou para a qual se indique um tipo que
  nenhuma imagem de disco contenha, não é montada.
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
