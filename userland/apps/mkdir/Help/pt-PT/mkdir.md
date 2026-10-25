## NAME

mkdir — criar diretórios

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] directory...`

## DESCRIPTION

Cria cada operando de diretório, por ordem. Sem `-p`, o pai de cada
operando tem de existir já e o próprio operando não pode existir; um
operando que não pode ser criado é reportado e o seguinte é criado mesmo
assim.

Com `-p`, cada antepassado em falta é criado primeiro, do mais exterior
para dentro, e um operando (ou antepassado) que já exista como
diretório não é um erro. Um antepassado que exista como ficheiro
continua a falhar: nada é jamais substituído em silêncio.

Um diretório novo recebe o modo `a=rwx` menos a umask. Com `-m`, cada
operando de diretório criado recebe, em vez disso, exatamente `MODE`,
seja qual for a umask: um valor octal ou cláusulas simbólicas, tal como
o `chmod` os lê, partindo de `a=rwx`. Um antepassado em falta que `-p`
cria mantém as permissões de escrita e de pesquisa do seu proprietário,
diga a umask o que disser, pelo que o diretório abaixo dele pode sempre
ser criado. Um diretório criado dentro de um diretório set-group-ID
mantém o bit que herda, a não ser que `MODE` o nomeie. Um operando que
já exista mantém o seu modo. `--` termina a análise de opções: cada
argumento posterior é um caminho.

## OPTIONS

- `-m, --mode=MODE` — atribuir a cada operando de diretório criado o
  modo `MODE`, octal ou simbólico como no `chmod`, em vez de `a=rwx`
  menos a umask.
- `-p, --parents` — criar os diretórios pai em falta; um operando que
  já seja um diretório não é um erro.
- `-v, --verbose` — reportar cada diretório criado como
  `mkdir: created directory 'dir'`.
- `-h, -?` — mostrar a ajuda curta deste próprio comando (também
  `--help`).

## EXAMPLES

- `mkdir Notes` — criar um diretório no diretório atual.
- `mkdir -p Projects/os/build` — criar toda a cadeia, saltando as
  partes que já existem.
- `mkdir -pv Home:/tools/bin` — criar sob uma raiz de alias, reportando
  cada diretório novo.
- `mkdir -m 700 Private` — criar um diretório que só o seu proprietário
  pode usar.

## EXIT STATUS

- `0` — todos os diretórios foram criados (ou, com `-p`, já existiam).
- `1` — uma falha do sistema de ficheiros ou da saída, ou um modo que
  não pôde ser atribuído; a razão é impressa no erro padrão. Uma linha
  de comandos não compreendida, ou um `MODE` que não é um modo, também
  termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

chmod, rmdir, rm, ls
