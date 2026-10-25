## NAME

chown — alterar o proprietário e o grupo de um ficheiro

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Altera o utilizador e o grupo proprietários de cada operando de
ficheiro, por ordem. `OWNER` altera o utilizador e mantém o grupo,
`OWNER:GROUP` altera ambos e `:GROUP` altera só o grupo. `OWNER:` dá
também ao ficheiro o grupo primário do proprietário. Um operando vazio,
ou `:` sozinho, não altera nenhum dos dois. A grafia antiga
`OWNER.GROUP` funciona com um aviso, a não ser que o operando inteiro
nomeie uma conta. Com `--reference`, cada ficheiro recebe o proprietário
e o grupo daquilo que `RFILE` nomeia.

`OWNER` e `GROUP` são nomes de contas e de grupos, ou números: tenta-se
primeiro um nome, e um número só é aceite quando nada tem esse nome. Um
`+` inicial (`+1000`) significa sempre um número.

Dar um ficheiro a outra conta, ou a um grupo de que não é membro, exige
a capability `CAP_FS_CHOWN`; sem ela, pode alterar o grupo de um
ficheiro seu para um dos seus grupos. Uma alteração limpa o bit
set-user-ID de um ficheiro, e o seu bit set-group-ID quando o seu grupo
o pode executar; um diretório mantém ambos.

Uma ligação simbólica indicada como operando altera o ficheiro que ela
nomeia, ou, com `-h`, a própria ligação. Com `-R`, um operando de
diretório e tudo o que contém são alterados, cada diretório depois do
seu conteúdo. Uma ligação simbólica encontrada dentro da árvore é ela
própria alterada, e aquilo que nomeia nunca é tocado; com
`--dereference`, também a ligação fica intacta. Com `-R`, uma ligação
simbólica indicada como operando é ela própria alterada e não é seguida:
`--dereference` altera antes aquilo que ela nomeia, e `-H` percorre um
diretório que ela nomeie. Um ficheiro que não pode ser alcançado ou
alterado é reportado e os restantes são alterados mesmo assim. Um
relatório indica cada proprietário e grupo pelo nome, ou pelo número
quando nenhuma conta ou grupo o tem. `--` termina a análise de opções:
cada argumento posterior é um operando.

## OPTIONS

- `-c, --changes` — reportar apenas os ficheiros cuja propriedade mudou
  realmente.
- `-f, --silent, --quiet` — suprimir a maioria das mensagens de erro; a
  execução continua a falhar e o estado de saída reporta-o.
- `-v, --verbose` — reportar cada ficheiro processado, incluindo aquele
  cuja alteração foi recusada ou que não pôde ser alcançado.
- `--dereference` — alterar aquilo que uma ligação simbólica nomeia em
  vez da ligação; sem `-R`, é a omissão.
- `-h, --no-dereference` — alterar a própria ligação simbólica em vez
  daquilo que ela nomeia.
- `--preserve-root` — com `-R`, recusar alterar `/`, seja como for que
  se chegue a ele.
- `--no-preserve-root` — tratar `/` como qualquer outro diretório (a
  omissão).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — alterar um ficheiro só se o seu
  proprietário e o seu grupo atuais forem estes; omita um deles para
  aceitar qualquer um.
- `--reference=RFILE` — dar a cada ficheiro o proprietário e o grupo de
  `RFILE` em vez de os indicar.
- `-R, --recursive` — alterar ficheiros e diretórios recursivamente.
- `-H` — com `-R`, percorrer o diretório a que leva uma ligação
  simbólica indicada como operando.
- `-L` — com `-R`, seguir todas as ligações simbólicas para um
  diretório; recusado, porque uma alteração recursiva nunca segue uma
  ligação dentro da árvore.
- `-P` — com `-R`, não seguir nenhuma ligação simbólica (a omissão). O
  último indicado de `-H`, `-L` e `-P` vence.
- `-?, --help` — mostrar a ajuda curta deste próprio comando.
- `--version` — imprimir o nome e a versão da ferramenta.

## EXAMPLES

- `chown alice notes.txt` — dar `notes.txt` a `alice`, mantendo o seu
  grupo.
- `chown alice:staff notes.txt` — dá-lo a `alice` e ao grupo `staff`.
- `chown :staff shared.txt` — alterar só o grupo.
- `chown -R alice Projects` — dar a árvore `Projects` a `alice`.
- `chown -R --from=bob alice Shared` — dar a `alice` só aquilo que `bob`
  possui em `Shared`.
- `chown --reference=template.txt notes.txt` — dar a `notes.txt` o
  proprietário e o grupo de `template.txt`.

## EXIT STATUS

- `0` — todas as alterações tiveram êxito.
- `1` — um ficheiro não pôde ser alcançado ou alterado, ou a saída
  falhou; cada razão é impressa na saída de erro (suprimida sob `-f`).
  Uma linha de comandos não compreendida, ou um proprietário, grupo ou
  ficheiro de referência que não pôde ser lido, também termina com `1` e
  não altera nada.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
