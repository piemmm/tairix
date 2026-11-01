## NAME

groupadd — criar um grupo

## SYNOPSIS

`groupadd [-g GID] [--] NAME`

## DESCRIPTION

Acrescenta um único grupo ao registo de grupos. O nome do grupo tem de
corresponder a `[a-z_][a-z0-9_-]*` e o id é um valor decimal. Criar um
grupo é uma operação administrativa: o registo recusa um chamador sem a
capacidade de administração de utilizadores.

Quando `-g` é omitido, o id do grupo é atribuído automaticamente: um
acima do mais alto dos ids existentes iguais ou superiores a 1000, ou
1000 quando nenhum grupo tem um id assim. Os ids abaixo de 1000 são do
sistema e nunca são atribuídos, e, quando já não resta nenhum id acima
do mais alto, o grupo não é criado. Um id pedido que já esteja ocupado é
recusado; o registo é a autoridade sobre colisões.

`--` termina a análise de opções: cada argumento posterior é um
operando.

## OPTIONS

- `-g, --gid GID` — id numérico do grupo; atribuído automaticamente
  quando omitido (um acima do mais alto dos ids existentes iguais ou
  superiores a 1000, senão 1000).
- `-h, -?, --help` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `groupadd staff` — criar `staff` com um id atribuído automaticamente.
- `groupadd -g 100 staff` — criar `staff` com o id `100`.

## EXIT STATUS

- `0` — o grupo foi criado.
- `1` — o registo recusou ou falhou a criação (por exemplo uma
  capacidade em falta ou um id duplicado); a razão é impressa no erro
  padrão. Uma linha de comandos não compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `useradd`
- `users`
