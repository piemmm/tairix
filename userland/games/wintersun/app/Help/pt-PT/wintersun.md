## NAME

wintersun — percorrer um mundo invernal gerado proceduralmente

## SYNOPSIS

`wintersun [--reference-scene]`

## DESCRIPTION

Abre uma janela do ambiente de trabalho sobre um mundo gerado: uma vista de
cima de um terreno que a máquina sintetiza em vez de distribuir, iluminado por
um sol baixo que lança sombras compridas por cada encosta.

Nada deste mundo é guardado como ilustração. Cada material de que o solo é
feito — neve, rocha, cascalho, urzal, tundra — é um punhado de números que o
cliente transforma em textura ao desenhar, pelo que o mundo tem o mesmo
aspecto em qualquer máquina e quase não ocupa espaço em disco. Os caminhos
desgastam-se no que atravessam em vez de assentarem por cima.

O terreno que ainda não foi gerado é desenhado como a falha que é e preenche-
se à medida que chega. O cliente desenha o que tem em vez de parar para
esperar, por isso a janela continua a responder enquanto o mundo recupera.

As setas ou `W`, `A`, `S`, `D` caminham. Duas teclas premidas ao mesmo tempo
percorrem a diagonal entre elas à mesma velocidade, e teclas opostas anulam-
se. A vista segue-o e pára na extremidade do mundo em vez de sair dele.
Encostas demasiado íngremes e água demasiado funda desviam-no.

`+` e `-` aproximam e afastam a vista, em cinco passos, de uma célula do mundo
com oito pixéis de largura até uma com cento e vinte e oito.

`F11` põe a janela em ecrã inteiro e devolve-a depois ao que era: uma janela
maximizada volta maximizada. `Esc` restaura-a. `Q` sai.

Cada detalhe é desenhado no nível mais fino até escolher outra coisa. A linha
*Settings…* do menu do jogo na barra de ícones abre a janela de definições,
onde a qualidade é *Ultra*, cada detalhe no nível mais fino; *Basic*, cada
detalhe no nível mais simples, no tamanho completo da janela; *Custom*, a sua
própria escolha da iluminação, das sombras, da textura do terreno e da escala
de renderização, cada uma no seu cursor; ou *Auto*. Mover um cursor torna a
escolha *Custom*. A sua escolha é guardada para a próxima vez que jogar.

Em *Auto*, o cliente reduz o detalhe quando os fotogramas se atrasam durante
algum tempo — primeiro a iluminação, depois as sombras, depois a escala de
renderização — e devolve-o, passo a passo, à medida que recuperam. Avalia ao
longo de segundos e não de fotogramas isolados, pelo que um momento de outro
trabalho na máquina não custa nada e uma janela maior não o leva ao nível mais
simples, e nunca desenha as figuras demasiado pequenas para se lerem.

Uma janela maior do que o renderizador por software consegue preencher é
desenhada no máximo a 2560×1440 e ampliada até ao tamanho da janela.

## OPTIONS

- `-h, -?, --help` — mostrar a ajuda curta deste comando.
- `--reference-scene` — desenhar a cena de referência fixa e mantê-la parada:
  um só mundo, as mesmas personagens e o mesmo instante, idênticos em todas as
  máquinas, para que uma imagem da janela possa ser comparada com outra
  desenhada noutro lado. `F11` e `Esc` continuam a mudar o tamanho da janela;
  nada mais se mexe.

## EXIT STATUS

`0` quando sai. Um estado diferente de zero indica o motivo na saída de erro:
o mundo não pôde ser gerado, a janela não pôde ser aberta, ou o canal de
eventos da sessão perdeu-se.

- `2` — a linha de comandos não foi compreendida.
- `87` — a cena de referência não pôde ser desenhada.

## SEE ALSO

`sapper`
