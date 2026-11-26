## NAME

gpucheck — verificar cada GPU servida, o ecrã que mostra o que uma delas
desenha, e um kernel fornecido por si

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

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
consiga fazê-lo tem de responder abandonando o desenho. A seguir, pede à
GPU que execute um programa sobre mais grupos de trabalho do que
permite, ao que todo o dispositivo tem de responder abandonando esse
trabalho. Por fim, executa pequenos programas que, no seu conjunto, usam
todas as instruções admitidas num programa de GPU, e cada resposta tem
de corresponder à do dispositivo de software com a precisão que a
definição da sua instrução garante; uma GPU que não consiga executar
algum deles não fica sujeita a ele. Uma GPU concorda quando deixa os
mesmos bytes que o dispositivo de software, recusa esse preenchimento
pelo mesmo motivo, abandona o mesmo trabalho e dá a mesma resposta a
cada um desses programas.

O comando imprime uma linha por cada GPU, que nomeia o seu nó de
hardware: ou que a GPU concorda, ou a primeira diferença encontrada — o
trabalho recusado, e em que passo; um byte que se lê de outra forma;
uma resposta diferente ao preenchimento; um valor que um programa
gráfico deixou diferente; um byte de uma imagem que se lê de outra
forma; um byte de um desenho que se lê de outra forma; uma resposta
diferente ao desenho a partir de uma lista; uma resposta diferente ao
trabalho para além dos limites da GPU; ou uma instrução, num dos
pequenos programas, cuja resposta se afasta da resposta do dispositivo
de software mais do que a sua definição permite.

`sysinfo gpu` lista as GPU que o comando verifica.

Com `--present`, o comando mostra em vez disso uma imagem conhecida: um
fundo azul, uma faixa de cores, uma fila de mosaicos axadrezados e um
mosaico copiado para o canto oposto, numa janela que cobre todo o ecrã.
A imagem é desenhada pela primeira GPU que um controlador serve, ou pelo
dispositivo de software quando nenhuma é servida. O comando apresenta-a
uma vez por fotograma até se premir Escape ou se fechar a janela, e
depois imprime quantos fotogramas apresentou, o que os desenhou e se o
ecrã os mostrou todos.

Com `--shader`, o comando executa em vez disso um programa fornecido por
si: o kernel de computação contido em FILE, escrito em WGSL ou compilado
para SPIR-V, duas formas que se distinguem pelo início do ficheiro. O
código WGSL é compilado num processo separado que não consegue alcançar
mais nada, e em ambos os casos o kernel é verificado antes de ser
entregue a qualquer GPU. Um kernel recusado não é executado: a recusa é
impressa com a linha e a coluna a que se refere no código WGSL. O kernel
tem de ter um único ponto de entrada de computação, que usa um único
buffer de armazenamento no grupo 0, binding 0. O buffer contém
N palavras, 256 a menos que `--words` indique outro valor; a palavra *i*
tem inicialmente o valor *i*, e o kernel é executado uma vez por cada
palavra. O comando executa-o em cada GPU que um controlador serve e no
dispositivo de software; ao lado dele, em memória que pertence a um
programa à parte, a GPU guarda um marcador que o buffer do kernel nunca
pode vir a conter. Para cada GPU, o comando imprime ou que os dois
concordam — o buffer lê-se da mesma forma, ou ambos abandonaram um
kernel que ultrapassou o seu limite de tempo e continuaram a servir — ou
a primeira diferença: uma palavra que se lê de outra forma, o marcador
encontrado no buffer, o kernel recusado, abandonado ou deixado sem
resposta ao passo que o dispositivo de software o executou, ou uma GPU
que nada mais serviu depois de o ter abandonado.

## OPTIONS

- `--present` — mostrar a imagem em ecrã inteiro e verificar que o ecrã
  mostrou cada fotograma apresentado.
- `--shader FILE` — executar o kernel de computação contido em FILE,
  código WGSL ou módulo SPIR-V, em cada GPU servida e no dispositivo de
  software.
- `--words N` — com `--shader`, quantas palavras contém o buffer do
  kernel: de 1 a 65536, e 256 quando não é indicado.
- `-h, -?` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `gpucheck` — verificar cada GPU servida.
- `gpucheck --present` — mostrar a imagem em ecrã inteiro; premir Escape
  para parar.
- `gpucheck --shader double.wgsl` — executar um kernel WGSL sobre
  256 palavras em cada GPU servida.
- `gpucheck --shader kernel.spv --words 4096` — executar um kernel
  compilado sobre 4096 palavras.

## EXIT STATUS

- `0` — cada GPU servida concordou com o dispositivo de software; com
  `--present`, o ecrã mostrou cada fotograma apresentado; com
  `--shader`, cada GPU servida concordou com o dispositivo de software
  quanto ao kernel.
- `1` — uma GPU diferiu ou não pôde ser contactada, nenhuma GPU é
  servida, as GPU servidas não puderam ser listadas, ou o dispositivo de
  software não pôde executar o trabalho; com `--present`, um fotograma
  não foi mostrado, nenhum foi apresentado, ou a janela ou o dispositivo
  recusou um passo; com `--shader`, o ficheiro não pôde ser lido, o
  kernel foi recusado, ou uma GPU diferiu. Uma linha de comandos não
  compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
