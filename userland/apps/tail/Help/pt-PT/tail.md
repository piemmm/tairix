## NAME

tail — mostrar a última parte dos ficheiros

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Imprime as últimas 10 linhas de cada `file` na saída padrão. Com mais do
que um `file`, cada parte é precedida de um cabeçalho `==> file <==`. Sem
`file`, ou quando `file` é `-`, é lida a entrada padrão.

`-n` e `-c` alteram quanto é impresso: uma contagem simples (ou escrita
com um `-` inicial) imprime as últimas `num` linhas ou bytes; uma
contagem escrita com um `+` inicial imprime tudo **a partir** da linha ou
do byte `num` (a contar de 1) até ao fim. Uma contagem pode ter um
sufixo multiplicador: `b` (512), `kB` (1000), `K` (1024), `MB`, `M`,
`GB`, `G`, e assim por diante para `T`, `P`, `E`, `Z`, `Y`, `R`, `Q` (uma
letra sozinha multiplica por potências de 1024; com `B` por potências de
1000; com `iB` por potências de 1024).

A forma histórica em primeiro argumento `tail -num` / `tail +num` (com uma
letra final `b`/`c`/`l` opcional) é aceite, tal como na ferramenta GNU.

O modo de seguimento mantém cada ficheiro aberto e imprime os dados novos
à medida que o ficheiro cresce. `-f` segue o ficheiro aberto pelo
descritor — depois de mudar de nome ou ser movido, continua a seguir o
mesmo ficheiro. `-F` segue o *nome*: quando o ficheiro é rodado
(renomeado para outro lado e substituído por um ficheiro novo no seu
lugar) reabre o ficheiro novo, e `--retry` continua a tentar um nome que
ainda não existe. Um seguimento bloqueia até o ficheiro mudar de facto —
nunca põe o CPU a girar em vão. `--pid=PID` termina o seguimento assim
que o processo `PID` sai, verificado a cada `--sleep-interval` (segundos,
1 por omissão). `--max-unchanged-stats=N` controla quantos ciclos
parados passam antes de `-F` voltar a verificar o nome à procura de uma
rotação (5 por omissão). Um truncamento (o ficheiro encolhe) é reportado
e o ficheiro volta a ser seguido a partir do seu novo início.

Quando conteúdo inicial não é mostrado, é escrito um registo informativo
no fluxo de informação padrão (fd 3); nunca altera a saída nem o estado
de saída. Um ficheiro que não pode ser lido é reportado na saída de erro
e a execução continua com o ficheiro seguinte.

Num terminal, cada nome que a saída mostra é posto entre aspas onde uma
shell precisaria delas, e qualquer carácter de controlo ou outro
carácter invisível nele contido é escrito como sequência de escape, de
modo que um nome não consegue comandar nem reordenar o ecrã; o GNU
mostra-o sem alterações. Um pipe ou um ficheiro recebe o nome byte a
byte.

## OPTIONS

- `-c, --bytes <num>` — imprimir os últimos `num` bytes de cada ficheiro;
  com um `+` inicial, tudo a partir do byte `num`.
- `-n, --lines <num>` — imprimir as últimas `num` linhas de cada
  ficheiro; com um `+` inicial, tudo a partir da linha `num`.
- `-q, --quiet, --silent` — nunca imprimir os cabeçalhos `==> file <==`.
- `-v, --verbose` — imprimir sempre os cabeçalhos `==> file <==`.
- `-z, --zero-terminated` — as linhas são delimitadas por NUL em vez da
  mudança de linha.
- `-f, --follow[=descriptor]` — manter o ficheiro aberto e imprimir os
  dados à medida que são acrescentados, seguindo-o pelo descritor.
- `-F` — seguir pelo nome (`--follow=name --retry`): reabrir o ficheiro
  quando é rodado.
- `--follow=name` — seguir o nome em vez do descritor.
- `--retry` — continuar a tentar abrir um ficheiro ainda não acessível.
- `--pid <PID>` — durante um seguimento, parar assim que o processo
  `PID` morre.
- `--sleep-interval <N>` — segundos entre as reverificações de
  `--pid`/rotação (frações permitidas; 1 por omissão).
- `--max-unchanged-stats <N>` — ciclos parados antes de `-F` voltar a
  verificar o nome à procura de uma rotação (5 por omissão).
- `-h, -?` — mostrar a ajuda breve deste comando.

## EXAMPLES

- `tail log.txt` — imprimir as últimas 10 linhas de `log.txt`.
- `tail -n 3 a b` — imprimir as últimas 3 linhas de `a` e de `b`, cada
  uma sob o seu cabeçalho.
- `tail -c 1K image` — imprimir os últimos 1024 bytes de `image`.
- `tail -n +5 notes` — imprimir `notes` a partir da sua 5.ª linha.
- `tail -f log.txt` — imprimir as últimas 10 linhas e depois as linhas
  novas à medida que são escritas.
- `tail -F /System/Logs/system` — seguir o registo do sistema através
  das rotações.
- `tail -f --pid 1234 out` — seguir `out` até o processo 1234 sair.

## EXIT STATUS

- `0` — cada ficheiro foi impresso (ou a ajuda breve foi escrita).
- `1` — um ficheiro não pôde ser lido, ou a saída não pôde ser entregue.
  Uma linha de comandos não compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a localização preferida para a ajuda breve (uma etiqueta
  BCP-47 como `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
