## NAME

clear — limpar o ecrã do terminal

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Escreve a sequência que move o cursor para o canto superior esquerdo e
apaga todo o ecrã, deixando-o vazio. Qual sequência é escrita decide-o o
terminal indicado em `TERM`; um terminal que não sabe limpar (um `TERM`
desconhecido degrada para a base «dumb») faz o comando falhar em vez de
imprimir bytes que o terminal mostraria como lixo.

Onde o terminal o permite, as linhas que guarda acima do ecrã — o
histórico de deslocamento — são também apagadas, pelo que nada do que
estava no ecrã pode ser recuperado deslocando para trás.

## OPTIONS

- `-x` — manter o histórico: apagar apenas o ecrã.
- `-h, -?` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `clear` — limpar o ecrã.

## EXIT STATUS

- `0` — a sequência de limpeza foi escrita.
- `1` — o terminal não sabe limpar, ou a saída não pôde ser entregue.
  Uma linha de comandos não compreendida também termina com `1`.

## ENVIRONMENT

- `TERM` — o terminal cuja sequência de limpeza é escrita.
- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `reset`
- `man`
