## NAME

chmod — alterar os bits de modo de um ficheiro

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Altera os bits de permissão de cada operando de ficheiro para `MODE`,
por ordem. `MODE` é um valor octal absoluto (`644`, `0755`, …) que
substitui por completo os bits de permissão, ou uma lista de cláusulas
simbólicas separadas por vírgulas `[ugoa]*([-+=]([rwxXst]*|[ugo]))+` ou
`[-+=][0-7]+` (`g+w`, `o-rx`, `a=rx`, `u+s`, `g=u`, `=644`) que
transformam os bits atuais do ficheiro. O `X` simbólico concede execução
apenas a um diretório ou a um ficheiro que já tenha um bit de execução,
`g=u` dá ao grupo os bits do proprietário, `s` define os bits set-ID do
proprietário e do grupo, e `t` define o sticky bit. Uma cláusula que não
nomeia ninguém (`+x`, `=r`) altera todos os campos exceto os bits que a
umask contém.

Num diretório, os bits set-user-ID e set-group-ID ficam como estão, a
não ser que o modo os nomeie: com `s` (`g-s`), com dígitos octais depois
de um operador (`=755`) ou com um valor octal de cinco ou mais dígitos
(`00755`). É o bit set-group-ID de um diretório que dá às suas novas
entradas o grupo do diretório, pelo que `chmod 755` o mantém.

Só o proprietário de um ficheiro pode alterar o seu modo; o núcleo
recusa qualquer outro, e possuir uma capability não concede qualquer
privilégio. Uma ligação simbólica indicada como operando altera o
ficheiro que ela nomeia. Com `-R` um operando de diretório é alterado e
depois o seu conteúdo é alterado recursivamente; uma ligação simbólica
encontrada dentro da árvore é deixada intacta, tal como aquilo que
nomeia. Um ficheiro que não pode ser alcançado ou alterado é reportado e
os restantes são alterados mesmo assim. `--` termina a análise de
opções: cada argumento posterior é um operando. Para um modo que comece
por `-`, escreva-o sem o traço (`a-w`) ou termine primeiro as opções
(`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — alterar ficheiros e diretórios
  recursivamente.
- `-c, --changes` — reportar apenas os ficheiros cujo modo mudou
  realmente.
- `-v, --verbose` — reportar cada ficheiro processado, incluindo aquele
  cuja alteração foi recusada ou que não pôde ser alcançado.
- `-f, --silent, --quiet` — suprimir a maioria das mensagens de
  erro; a execução continua a falhar e o estado de saída reporta-o.
- `--preserve-root` — com `-R`, recusar alterar `/`, seja como for que
  se chegue a ele.
- `--no-preserve-root` — tratar `/` como qualquer outro diretório (a
  omissão).
- `-h, -?, --help` — mostrar a ajuda curta deste comando.

## EXAMPLES

- `chmod 644 notes.txt` — leitura/escrita para o proprietário,
  apenas leitura para os restantes.
- `chmod g+w shared.txt` — acrescentar escrita de grupo aos bits
  atuais.
- `chmod -R a=rx Docs` — tornar a árvore `Docs` legível e
  atravessável por todos.
- `chmod +x build.sh` — tornar um script executável por todos os que a
  umask permite.

## EXIT STATUS

- `0` — todas as alterações de modo tiveram êxito.
- `1` — um ficheiro não pôde ser alcançado ou alterado, ou a saída
  falhou; cada razão é impressa na saída de erro (suprimida sob `-f`).
  Uma linha de comandos não compreendida, ou cujo operando de modo não
  era octal nem simbólico, também termina com `1`.

## ENVIRONMENT

- `LANG` — a locale preferida para a ajuda curta (uma etiqueta
  BCP-47 como `pt-PT`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
