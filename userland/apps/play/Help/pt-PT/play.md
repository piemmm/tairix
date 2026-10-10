## NAME

play — reproduzir ficheiros de som

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Reproduz cada ficheiro à vez numa saída. Cada ficheiro é descodificado num
processo isolado sem qualquer autoridade, pelo que um ficheiro hostil pode, no
máximo, terminar a sua própria descodificação: é deixado de fora com o motivo
indicado e o resto da lista é reproduzido. São lidos ficheiros AU, WAV e FLAC, este
último nativo ou em Ogg.

Ficheiros consecutivos com a mesma frequência, o mesmo formato de amostra e a
mesma disposição de canais tocam sem intervalo, num só fluxo. Um ficheiro de
outra forma espera que o que está em fila seja tocado e depois abre o seu
próprio fluxo.

Quando a entrada padrão é um terminal, `play` desenha uma interface de ecrã
inteiro: o ficheiro a tocar, a posição, o nível e um medidor por canal, e a
lista. A reprodução não depende dela. Enviado para segundo plano, `play`
continua a tocar e devolve o terminal; trazido para primeiro plano, volta a
desenhar-se. Sem interface, indica o progresso numa linha na saída de erros de
um terminal.

A interface aceita estas teclas: Espaço ou `p` pausa e retoma; as setas
esquerda e direita saltam dez segundos; `n` ou `>` passa ao ficheiro seguinte;
`b` ou `<` volta ao início deste ficheiro, ou ao anterior nos seus três
primeiros segundos; `+`, `=` ou a seta para cima sobe três decibéis, e `-`,
`_` ou a seta para baixo desce-os; `q` ou Ctrl-C para; Ctrl-Z suspende, depois
de pausar o fluxo.

O nível de um fluxo é uma atenuação: um fluxo não pode passar da escala
completa, por isso `--gain` e as teclas de nível não vão além de 0 dB. Para
tocar mais alto, suba o volume da saída.

Um tempo escreve-se `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` ou
`12.5`.

Na informação padrão (fd 3), `play` escreve um registo `schema` por cada
ficheiro que reproduz, um registo `omission` por cada ficheiro deixado de fora
ou interrompido, e um registo `summary` no fim.

## OPTIONS

- `-q, --quiet` — sem interface nem linha de progresso.
- `-v, --verbose` — o formato e a duração de cada ficheiro na saída de erros.
- `--ui, --no-ui` — desenhar a interface, ou nunca; `--ui` sem terminal é
  recusado.
- `-d, --device <sink>` — a saída: `audio:sink/default`,
  `audio:sink/<id>` para este arranque, ou `audio:sink/<location>` onde
  quer que o dispositivo esteja, tal como `--list-devices` as nomeia.
- `-g, --gain <dB>` — o nível do fluxo, 0 ou menos, à centésima.
- `-s, --start <time>` — começar cada ficheiro neste tempo.
- `-t, --duration <time>` — reproduzir esta duração de cada ficheiro.
- `-l, --loop[=N]` — reproduzir a lista N vezes no total, ou sem N para sempre.
- `--list-devices` — nomear as saídas desta sessão, cada uma pelo seu
  identificador e pela sua localização, e sair.
- `-h, -?, --help` — mostrar a ajuda breve deste comando.
- `--version` — mostrar a versão e sair.

## EXAMPLES

- `play song.wav` — reproduzir um ficheiro, com a interface num terminal.
- `play -q intro.au song.wav &` — reproduzir uma lista em segundo plano.
- `play -s 1:30 -t 20 song.wav` — reproduzir vinte segundos a partir de minuto e meio.
- `play -l3 -g -6 loop.wav` — reproduzir um ficheiro três vezes, seis decibéis abaixo.
- `play --list-devices` — ver as saídas.

## EXIT STATUS

- `0` — cada ficheiro foi reproduzido, ou a reprodução foi parada sem nenhum deixado de fora.
- `1` — um ficheiro não pôde ser reproduzido, ou a reprodução não pôde continuar.
- `2` — a linha de comandos não foi compreendida.

## ENVIRONMENT

- `TERM` — o terminal para o qual a interface desenha.
- `LANG` — o idioma preferido da ajuda breve (uma etiqueta BCP-47 como
  `fr-FR`).

## SEE ALSO

- `man`
