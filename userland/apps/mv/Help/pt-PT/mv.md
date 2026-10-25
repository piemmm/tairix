## NAME

mv — mover (renomear) ficheiros e diretórios

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Move cada operando de origem para um destino. Com uma única origem e
um destino que não nomeia um diretório, a origem é renomeada para esse
caminho exato. Quando o destino nomeia um diretório existente — e
sempre que há mais de uma origem — cada origem é movida *para dentro*
desse diretório sob o seu próprio nome base.

Um movimento dentro de um volume é uma mudança de nome atómica que
preserva a identidade do nó. Um movimento entre volumes cumpre a mesma
promessa: a cópia é construída ao lado do destino, legível apenas pelo
seu dono até estar completa, escrita no disco e renomeada para o seu
lugar num só passo, e só então aquilo que foi copiado é removido — assim
o destino guarda o que continha até o movimento estar completo, e nem
uma falha nem uma queda perdem nada. Um ficheiro acrescentado à origem,
ou alterado, enquanto era copiado fica onde estava, com o diretório que
o contém, e é relatado; num volume que não regista uma contagem de
alterações, ou que guarda os tempos com pouca precisão (os dois segundos
do FAT), uma reescrita do mesmo tamanho feita dentro dessa margem pode
passar despercebida. Em ambos os casos, um diretório só substitui um
diretório vazio, nada mais substitui um diretório e nenhum diretório é
movido para a sua própria subárvore.

Um destino existente é sobrescrito por omissão, mantido com `-n` (que
indica `not replacing` e faz falhar a execução, como o do GNU), e
perguntado no fluxo de erro padrão com `-i` (uma pergunta recusada salta
esse movimento e faz falhar a execução; uma resposta ilegível nunca é
tratada como consentimento). Um operando que não possa ser movido é
relatado, e o seguinte é movido na mesma. `--` termina a análise de
opções: cada argumento posterior é um caminho.

A cópia preserva os buracos de cada ficheiro, os seus atributos
estendidos — um tipo de ficheiro RISC OS, um tipo e um criador Mac —, as
permissões e o dono sempre que o destino os guarde, e move uma ligação
simbólica como uma ligação com o mesmo destino, nunca aquilo para que
aponta. Um ficheiro devolvido a outro dono chega sem os bits set-ID, que
o sistema limpa quando o dono muda; um que quem o move não pode devolver
passa a pertencer a quem o move, mantendo o grupo que tinha quando quem
o move o pode manter, e sem os bits set-ID de um dono ou de um grupo que
não tenha mantido. As marcas temporais não são preservadas: nenhuma
chamada deste sistema as define. Os atributos que o destino não consegue
guardar são relatados num único aviso no fim da execução, e num registo
`omission` no fluxo de informação padrão, antes de as origens
desaparecerem de vez.

## OPTIONS

- `-f, --force` — nunca perguntar antes de substituir. Prevalece o
  último de `-f`, `-i` e `-n`.
- `-i, --interactive` — perguntar antes de sobrescrever um destino
  existente; só uma resposta a começar por `y`/`Y` consente.
- `-n, --no-clobber` — nunca sobrescrever um destino existente; cada
  destino mantido é relatado como `not replacing 'dest'`.
- `-v, --verbose` — relatar cada movimento como
  `renamed 'source' -> 'dest'`, e um movimento entre volumes entrada a
  entrada, como faz o GNU: `created directory 'dest'` para cada
  diretório e `copied 'source' -> 'dest'` para tudo o resto à medida que
  é copiado, e depois `removed 'source'` ou `removed directory 'source'`
  para cada um à medida que a origem desaparece.
- `-t dir, --target-directory=dir` — mover todas as origens para
  `dir`, que tem de ser um diretório existente. O valor segue colado
  (`-tdir`, `--target-directory=dir`) ou como o argumento seguinte.
- `-T, --no-target-directory` — tratar o destino como um ficheiro
  normal; permite-se exatamente uma origem. Não pode ser combinado com
  `-t`.
- `-h, -?, --help` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `mv draft.txt final.txt` — renomear um ficheiro.
- `mv -v a.txt b.txt Archive` — mover ambos os ficheiros para
  `Archive`, reportando cada movimento.
- `mv -n new.cfg current.cfg` — instalar um ficheiro apenas se o destino
  ainda não existir (indicando `not replacing` se existir).

## EXIT STATUS

- `0` — todos os movimentos tiveram êxito.
- `1` — um operando não pôde ser movido, `-n` manteve um destino, uma
  pergunta `-i` foi recusada, algo numa origem mudou enquanto era movido
  e foi mantido, ou o diálogo ou a saída falharam; a razão de cada falha
  é impressa no erro padrão. Uma linha de comandos não compreendida
  também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
