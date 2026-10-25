## NAME

cp — copiar ficheiros e diretórios

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Copia cada operando de origem para um destino. Com uma única origem e
um destino que não nomeia um diretório, a origem é copiada para esse
caminho exato. Quando o destino nomeia um diretório existente — e
sempre que há mais de uma origem — cada origem é copiada *para dentro*
desse diretório sob o seu próprio nome base.

Uma origem que é diretório só é copiada com `-r`, que reproduz toda a
subárvore; sem `-r`, um operando de diretório é recusado. Um ficheiro de
destino existente é sobrescrito por omissão, saltado com `-n`, e
perguntado no fluxo de erro padrão com `-i` (uma pergunta recusada salta
essa cópia e faz falhar a execução, como o do GNU; uma resposta ilegível
nunca é tratada como consentimento). Um destino que é o próprio ficheiro
de origem — o mesmo nome, outro nome para ele ou uma ligação para ele —
é recusado por ser o mesmo ficheiro e fica como estava, exceto que `-l`
o considera já ligado. Uma ligação que `-f` põe no lugar de um nome
existente ocupa esse lugar num só passo, criada antes de qualquer coisa
ser substituída, pelo que uma que não possa ser criada deixa o nome no
lugar.

Um ficheiro que não pode ser lido, criado ou escrito é reportado e os
restantes são copiados mesmo assim, o resto do seu diretório também.
`--` termina a análise de opções: cada argumento posterior é um caminho.

Um ficheiro novo é criado com os bits de permissão da origem, sem os
bits set-ID e sticky, descontada a umask, pelo que a cópia de um
ficheiro privado continua privada; um destino que já existe mantém o seu
próprio modo. Um diretório novo fica no fim com os bits de permissão e
sticky da origem, descontada a umask; enquanto as suas entradas são
copiadas está aberto ao seu proprietário, pelo que a cópia fica completa
mesmo que o diretório de origem seja só de leitura ou que a umask retire
a escrita ao proprietário. Nunca se escreve através de uma ligação
simbólica encontrada dentro de uma árvore de destino.

O `-a`/`--archive` do GNU e qualquer `--preserve` além de
`--preserve=links,xattr` são **recusados**, não restringidos. `-a` é
`-dR --preserve=all`, e `--preserve=all` inclui as marcas temporais de
um nó, que nenhuma chamada deste sistema consegue definir — pelo que
honrar `-a` reportaria uma preservação que não aconteceu. Use `-dR` para
o resto.

Cada cópia leva os atributos estendidos da origem — um tipo de ficheiro
RISC OS, o tipo e o criador de um Mac — onde quer que o destino os possa
guardar, e mantém como buracos os buracos de um ficheiro esparso. Os
atributos que um destino não pode guardar são comunicados num único
aviso no fim da execução, e num registo `omission` no fluxo de
informação padrão; com `--preserve=xattr` essa cópia falha em vez disso.

Um diretório nunca é copiado para dentro de si próprio: um destino
dentro da origem — seja como for escrito, através de uma ligação, de um
`..` ou de outro nome do volume — é recusado antes de se escrever o que
quer que seja, e um diretório movido para a origem durante a cópia para
a cópia onde o percurso o encontra.

## OPTIONS

- `-r, -R, --recursive` — copiar diretórios e o seu conteúdo.
- `-f, --force` — quando um ficheiro de destino não pode ser criado,
  removê-lo e tentar a cópia uma vez mais; com `-l`, `-s` ou uma ligação
  que `-P` reproduz, pôr a ligação no lugar de um nome existente.
- `-i, --interactive` — perguntar antes de sobrescrever um ficheiro
  existente; só uma resposta a começar por `y`/`Y` consente, e uma
  pergunta recusada salta esse ficheiro e faz falhar a execução.
- `-n, --no-clobber` — nunca sobrescrever um ficheiro existente. O
  mais tardio de `-i` e `-n` vence.
- `-l, --link` — dar ao destino um segundo nome do nó da origem em vez
  de copiar os seus bytes, de modo que os dois nomes não possam divergir
  numa escrita posterior. Uma origem que é diretório continua a exigir
  `-r`.
- `-s, --symbolic-link` — criar uma ligação simbólica que nomeia a
  origem em vez de a copiar.
- `-P, --no-dereference` — reproduzir cada ligação simbólica entre as
  origens como uma ligação com o mesmo destino, literalmente, em vez de
  copiar aquilo para que aponta (assim uma ligação relativa ou pendente
  sobrevive à cópia).
- `-L, --dereference` — seguir cada ligação simbólica entre as origens e
  copiar aquilo para que aponta.
- `-H` — seguir uma ligação simbólica indicada como operando, mas
  reproduzir as encontradas dentro de uma árvore. Sem `-H`, `-L` nem
  `-P`, uma cópia recursiva reproduz cada ligação, incluindo as dos seus
  operandos, e qualquer outra cópia segue as ligações dos seus
  operandos, como a do GNU; `-l` liga aquilo para que aponta uma ligação
  seguida.
- `--preserve=links` — duas origens que nomeiam um só nó obtêm dois
  nomes no destino em vez de duas cópias, pelo que a cópia não duplica o
  armazenamento em silêncio.
- `--preserve=xattr` — todos os atributos estendidos têm de ser levados:
  uma cópia cujo destino não possa guardar um falha, indicando-o.
  `--preserve=links,xattr` pede ambos.
- `--no-preserve=xattr` — não copiar nenhum atributo estendido.
  `ownership` e `timestamps` também são aceites, e já não são
  preservados.
- `--no-preserve=mode` — criar cada ficheiro e diretório novo no modo
  que um novo recebe quando nada o indica, descontada a umask, em vez do
  modo da origem. `--no-preserve=all` também faz isto.
- `--sparse=WHEN` — como é copiado um ficheiro esparso: `auto`, a
  predefinição, mantém os seus buracos; `always` deixa também por
  escrever cada bloco de zeros que guarda; `never` escreve todos os
  bytes.
- `-d` — `-P` e `--preserve=links` juntas, como na ferramenta GNU.
- `-v, --verbose` — reportar cada cópia como `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copiar todas as origens para
  `dir`, que tem de ser um diretório existente. O valor segue colado
  (`-tdir`, `--target-directory=dir`) ou como o argumento seguinte.
- `-T, --no-target-directory` — tratar o destino como um ficheiro
  normal; permite-se exatamente uma origem. Não pode ser combinado com
  `-t`.
- `-h, -?, --help` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `cp notes.txt backup.txt` — copiar um ficheiro para um novo nome.
- `cp -r Projects Archive` — reproduzir a árvore `Projects` dentro de
  `Archive` (ou como `Archive` se este não existir).
- `cp -v -t Backup a.txt b.txt` — copiar ambos os ficheiros para
  `Backup`, reportando cada cópia.

## EXIT STATUS

- `0` — todas as cópias tiveram êxito (um salto por `-n` não é uma
  falha).
- `1` — um ficheiro não pôde ser copiado, uma pergunta `-i` foi
  recusada, ou o diálogo ou a saída falharam; a razão de cada falha é
  impressa no erro padrão. Uma linha de comandos não compreendida também
  termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
