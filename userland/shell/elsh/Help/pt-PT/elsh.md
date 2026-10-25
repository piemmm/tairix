## NAME

elsh — a shell de comandos do TAIRiX

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Executa uma shell de comandos interativa — um ciclo ler-avaliar-imprimir
sobre os fluxos padrão herdados. Uma palavra de comando escrita é
resolvida primeiro contra os builtins da shell, depois contra a loja de
comandos do sistema (`/System/Commands`), a loja de aplicações do
sistema (`/System/Applications`), a loja de comandos (`<home>/Commands`)
e a loja de aplicações (`<home>/Applications`) do próprio utilizador e
depois contra os diretórios da variável `PATH`; estas quatro lojas
formam um prefixo fixo que o utilizador não pode reordenar nem
substituir, pelo que `PATH` nunca pode sombrear um comando do sistema.
Uma palavra não resolvida sai com `127`; um pacote resolvido mas não
executável sai com `126`.

Os builtins:

- `cd [-L | -P [-e]] [directory]`, `cd -` — mudar o diretório de
  trabalho: para `directory`, para `HOME` quando não é indicado nenhum,
  ou com `-` de volta para `OLDPWD`, imprimindo-o. `-L`, a opção por
  omissão, aplica `directory` a `PWD` tal como está escrito enquanto
  `PWD` ainda nomear o diretório de trabalho, pelo que `cd link` seguido
  de `cd ..` volta para onde está a ligação; `-P` segue primeiro as
  ligações, e `PWD` passa a ser o caminho do próprio diretório (`-e`
  falha então quando esse caminho não pode ser lido). `/..` é `/`, e
  abaixo de uma raiz de alias (`Home:/`) um `..` nunca sobe acima dela.
- `pwd [-L | -P]` — imprimir o diretório de trabalho: `PWD` enquanto ainda
  o nomear (`-L`, a opção por omissão), ou o caminho do próprio diretório
  (`-P`).
- `echo ...` — imprimir os seus operandos.
- `export NAME=value`, `unset NAME` — editar o ambiente exportado.
- `jobs`, `fg`, `bg` — controlo de tarefas.
- `ulimit [-a] [-H | -S] [resource [value]]` — ler e impor limites de
  recursos; `ulimit -n [value]` lê ou define o limite de ficheiros abertos.
- `umask [-S] [mode]` — mostrar a máscara de criação de modos de ficheiro
  (`0022`), ou com `-S` as permissões que ela deixa (`u=rwx,g=rx,o=rx`),
  ou defini-la: um `mode` octal é a própria máscara, um simbólico (`g-w`,
  `u=rwx,g=rx,o=`) edita as permissões que ela deixa. Cada comando lançado
  depois aplica-a aos ficheiros que cria, e um redireccionamento cria o
  seu ficheiro com `0666` menos a máscara.
- `elevate <user> <program> [argument ...]` — executar um só comando,
  com os argumentos indicados, reautenticado através do supervisor de
  início de sessão da consola.
- `shopt [-pqsu] [-o] [name ...]` — mostrar ou definir as opções dos
  padrões de nomes de ficheiros.
- `help` — listar os builtins.
- `exit [code]` — terminar a sessão.

Uma palavra com um `*`, `?` ou `[...]` sem aspas é um padrão de nomes de
ficheiros, substituído, como no bash, pelos nomes ordenados que lhe
correspondem: `cp *.tmp /tmp`. Um `.` inicial só corresponde a um `.`, e
um padrão sem correspondências fica tal como foi escrito. As chavetas
expandem-se primeiro: `a{b,c}` dá `ab ac`, e `{1..5}` conta. As aspas
tornam literal qualquer destes caracteres, e o valor de uma variável
nunca é lido como padrão. `shopt -s` ativa as opções do bash: `nullglob`
(sem correspondências não fica palavra), `failglob` (sem
correspondências é um erro), `dotglob`, `nocaseglob`, `globstar` (`**`
corresponde a qualquer profundidade) e `extglob` (`?(…)`, `*(…)`,
`+(…)`, `@(…)`, `!(…)`); `shopt -o -s noglob` desativa os padrões de
nomes de ficheiros.

Num terminal, a shell oferece um editor de linha interativo com as
teclas habituais do bash/zsh: Cima/Baixo (ou `Ctrl-P`/`Ctrl-N`)
percorrem o histórico de comandos da sessão, `Ctrl-R` pesquisa-o de
forma incremental, Esquerda/Direita/Home/End e `Ctrl-A`/`Ctrl-E` movem o
cursor, `Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` cortam e colam, `Ctrl-T`
troca dois caracteres, `Ctrl-L` redesenha a linha num ecrã limpo,
`Ctrl-C` descarta a linha em edição e `Ctrl-D` numa linha vazia termina
a sessão. O Tab completa a palavra sob o cursor: nomes de comandos
(builtins e aplicações de comando instaladas), caminhos de ficheiros e —
para um destino de redireccionamento ou uma palavra com a forma de uma
referência — referências de recursos como `sys:random`. Um espaço de
nomes completa os seus seletores registados segmento a segmento
(`state:` → `net/` → `wan/` → `link`); onde o registo conhece apenas a
forma — um nome de interface, uma linha de interrupção —, o Tab oferece
os nomes reais da máquina, ou nada se esta sessão não os puder listar.
`info:`/`state:`/`stats:` aparecem como argumentos e depois de `<`, que
lê o valor (`cat < info:mem/physical`), mas nunca depois de `>`: tal
recurso nunca se escreve por redireccionamento. A entrada vinda de um
pipe ou de um script contorna o editor e comporta-se da mesma forma com
ou sem terminal.

A shell não aceita operandos: a execução de scripts ainda não faz parte
da sua gramática.

## OPTIONS

- `-h, -?` — mostrar a ajuda curta deste próprio comando e sair.

## EXIT STATUS

- O código do builtin `exit`, ou `0` quando o fluxo de entrada termina
  (ou a ajuda curta foi mostrada).
- `2` — a invocação não foi compreendida.

## ENVIRONMENT

- `HOME` — para onde vai `cd` sem operando.
- `PWD`, `OLDPWD` — o diretório de trabalho tal como `cd` o alcançou, e
  o anterior; `cd` define ambas. Um `PWD` com que a shell é iniciada é
  mantido enquanto nomear o diretório de trabalho e, caso contrário,
  passa a ser o caminho do próprio diretório.
- `PATH` — os diretórios procurados depois do prefixo fixo de lojas.
- `GLOBIGNORE` — padrões separados por `:` cujas correspondências um
  padrão de nomes de ficheiros descarta. Enquanto a variável não estiver
  vazia, um padrão de nomes de ficheiros corresponde também a nomes que
  começam por `.`, pelo que `rm *` remove então também `.profile`.
- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`), exportada para cada comando lançado.

## SEE ALSO

- `man`
