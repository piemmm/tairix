## NAME

gpucheck — verificar cada GPU servida face ao dispositivo de software

## SYNOPSIS

`gpucheck`

## DESCRIPTION

Executa o mesmo trabalho curto em cada GPU que um controlador serve e no
dispositivo de software do sistema, e indica para cada GPU se os dois
concordam.

O trabalho escreve um padrão conhecido em memória que o comando consegue
ler, preenche a memória ao lado, copia ambos através de memória que a
GPU guarda para si própria e de volta, e por fim pede um preenchimento
para além do fim de um buffer, que todo o dispositivo tem de recusar.
Uma GPU concorda quando deixa os mesmos bytes que o dispositivo de
software e recusa esse preenchimento pelo mesmo motivo.

O comando imprime uma linha por cada GPU, que nomeia o seu nó de
hardware: ou que a GPU concorda, ou a primeira diferença encontrada — um
byte que se lê de outra forma, o trabalho recusado, ou uma resposta
diferente ao preenchimento.

`sysinfo gpu` lista as GPU que o comando verifica.

## OPTIONS

- `-h, -?` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `gpucheck` — verificar cada GPU servida.

## EXIT STATUS

- `0` — cada GPU servida concordou com o dispositivo de software.
- `1` — uma GPU diferiu ou não pôde ser contactada, nenhuma GPU é
  servida, as GPU servidas não puderam ser listadas, ou o dispositivo de
  software não pôde executar o trabalho. Uma linha de comandos não compreendida
  também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
