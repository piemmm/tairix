## NAME

rm — remover ficheiros e diretórios

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Remove cada operando de ficheiro, por ordem. Um operando que não é
diretório é desligado — uma ligação simbólica é ela própria removida,
nunca aquilo que nomeia; um operando de diretório só é removido com `-r`
(que remove o seu conteúdo em profundidade primeiro e depois o próprio
diretório) ou, quando vazio, com `-d`. Em ambos os casos, um operando
cujo último componente seja `.` ou `..` é recusado, como o POSIX exige.

Com `-f`, um operando que não existe é saltado em silêncio e nunca se
faz pergunta alguma. `-i` pergunta no fluxo de erro padrão antes de
cada remoção, indicando o que removeria
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), e antes
de descer para um diretório que contenha alguma coisa; `-I` pergunta uma
vez no início, antes de remover mais de três operandos ou antes de uma
remoção recursiva. Uma pergunta recusada mantém o objeto (ou salta toda
a execução, no caso de `-I`) sem erro; uma resposta ilegível nunca é
tratada como consentimento. O mais tardio de `-f`, `-i` e `-I` vence.

Sem `-f`, quando a entrada padrão é um terminal, antes de remover um
ficheiro em que o chamador não pode escrever é feita uma pergunta
(`remove write-protected regular file 'x'?`), e o ficheiro só é removido
com uma resposta a começar por `y` ou `Y`; por uma ligação simbólica
nunca se pergunta. Com `-i`, a pergunta também indica a proteção.

Um caminho que não pode ser inspecionado, lido ou removido é reportado e
os restantes são removidos mesmo assim; um diretório que fique com
alguma coisa dentro é mantido sem dizer nada. Uma remoção recursiva de
`/`, seja como for escrito, é recusada com `--preserve-root`, a omissão.
`--` termina a análise de opções: cada argumento posterior é um caminho.

## OPTIONS

- `-r, -R, --recursive` — remover diretórios e o seu conteúdo.
- `-f, --force` — ignorar operandos que não existem; nunca perguntar.
- `-d, --dir` — remover diretórios vazios.
- `-i, --interactive` — perguntar antes de cada remoção; só uma
  resposta a começar por `y`/`Y` consente.
- `-I` — perguntar uma vez antes de remover mais de três operandos ou
  antes de uma remoção recursiva.
- `-v, --verbose` — reportar cada remoção como `removed 'file'`.
- `--preserve-root` — recusar remover `/` recursivamente (a omissão).
- `--no-preserve-root` — permitir remover `/`. Só é aceite escrito por
  extenso.
- `-h, -?, --help` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `rm notes.txt` — remover um ficheiro.
- `rm -r Scratch` — remover a árvore `Scratch` e tudo o que contém.
- `rm -I a b c d` — perguntar uma vez e remover os quatro ficheiros com
  um `y`.

## EXIT STATUS

- `0` — todas as remoções tiveram êxito (uma pergunta recusada e um
  salto por `-f` não são falhas).
- `1` — um caminho não pôde ser removido, ou o diálogo ou a saída
  falharam; cada razão é impressa no erro padrão. Uma linha de comandos
  não compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
