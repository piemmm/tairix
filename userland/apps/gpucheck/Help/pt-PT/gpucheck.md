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
GPU guarda para si própria e de volta, e depois pede um preenchimento
para além do fim de um buffer, que todo o dispositivo tem de recusar.
Em seguida, executa cada um dos programas gráficos do próprio sistema
sobre uma imagem conhecida. Quando a GPU consegue guardar todos os
tipos de imagem que o passo seguinte usa, copia imagens de e para a
memória e de umas para as outras, limpa-as e executa um programa que
lê, escreve e amostra imagens. Quando a GPU consegue desenhar, desenha
em imagens formas, pontos e uma linha de cores conhecidas, ocultando o
que fica atrás, omitindo as formas viradas para o lado oposto,
misturando cores e deixando algumas intactas, e junta quatro amostras de
cada píxel numa só. Também desenha a partir de uma lista em memória que
nomeia a primeira cópia de uma forma a desenhar, ao que uma GPU que não
consiga fazê-lo tem de responder abandonando o desenho. Por fim, pede à
GPU que execute um programa sobre mais grupos de trabalho do que
permite, ao que todo o dispositivo tem de responder abandonando esse
trabalho. Uma GPU concorda quando deixa os mesmos bytes que o
dispositivo de software, recusa esse preenchimento pelo mesmo motivo e
abandona o mesmo trabalho.

O comando imprime uma linha por cada GPU, que nomeia o seu nó de
hardware: ou que a GPU concorda, ou a primeira diferença encontrada — o
trabalho recusado, e em que passo; um byte que se lê de outra forma;
uma resposta diferente ao preenchimento; um valor que um programa
gráfico deixou diferente; um byte de uma imagem que se lê de outra
forma; um byte de um desenho que se lê de outra forma; uma resposta
diferente ao desenho a partir de uma lista; ou uma resposta diferente ao
trabalho para além dos limites da GPU.

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
