## NAME

Draw — editor de desenhos vetoriais

## SYNOPSIS

`Draw`

## DESCRIPTION

Desenha e edita desenhos vetoriais numa janela do ambiente de trabalho:
caminhos, retângulos, elipses, estrelas e polígonos, linhas, curvas à mão
livre e texto, em grupos e camadas, numa página. Lançado com um documento —
a partir do gestor de ficheiros, do ambiente de trabalho, ou largando um
ficheiro sobre o seu ícone na barra de ícones — abre uma janela com ele.
Lançado sozinho, abre um desenho novo numa página A4. Cada documento é uma
janela do único programa; fechar a última deixa-o na barra de ícones, e a
linha Sair do seu menu de ícone termina-o.

O Draw abre e guarda SVG. Um desenho mantém a sua página e as suas unidades,
as suas camadas, grupos e recortes, cada caminho, forma e texto, as cores
lisas e os gradientes lineares e radiais, a opacidade de cada objeto, os
tracejados, as junções, as pontas e as pontas de seta, as guias e a grelha, e
o título do desenho. Um ficheiro que contém o que um desenho não consegue
manter — efeitos de filtro, máscaras, preenchimentos de padrão, marcadores,
imagens incorporadas, estilos de texto mistos, animação ou scripts, entre
outros — abre só para leitura, com a faixa de estado a nomear o que contém,
e Guardar pede um nome novo para que o original nunca seja substituído.
Guardar uma cópia escreve o desenho como PDF, como PNG — a página no seu
próprio tamanho, até 8192 píxeis no lado mais comprido — ou como SVG, sem
tocar no ficheiro do documento.

A caixa de ferramentas desce pelo lado esquerdo da janela, o inspetor pelo
direito, e a faixa de estado corre no fundo: mostra a posição do ponteiro e o
tamanho da seleção nas unidades da página, e diz o que fez o último comando.
No topo, a barra diz como se usa a ferramenta em mão e mostra o zoom: premir
o zoom abre um menu de zooms. Réguas nas unidades da página correm ao longo
do topo e da esquerda da tela, e barras de deslocamento ao longo da direita e
do fundo. Parar sobre o botão de uma ferramenta nomeia-a, com a tecla que a
escolhe.

As ferramentas, e as teclas que as escolhem, são: seleção (`V`), nós (`A`),
retângulo (`R`), elipse (`E`), estrela e polígono (`S`), linha (`L`),
caneta (`B`), lápis (`P`), texto (`T`), conta-gotas (`I`), zoom (`Z`) e
mão (`H`).

A ferramenta de seleção pega num objeto com um clique, num grupo inteiro, e
Shift acrescenta um objeto à seleção ou retira-o. Um clique na tela vazia
arrasta um retângulo que seleciona o que fica inteiramente dentro dele, ou
com Alt tudo o que toca. Arrastar a seleção move-a, ao longo de um eixo com
Shift, e com Alt move uma cópia. As suas oito pegas escalam-na a partir da
pega oposta, um canto com Shift mantendo as proporções; arrastar logo fora de
um canto roda-a em torno do seu centro, com Shift em passos de 15 graus. As
teclas de seta movem-na um píxel, e dez com Shift.

A ferramenta de nós mostra os nós do caminho selecionado e as pegas das suas
curvas: arraste um nó para o mover, ou uma pega para dobrar a sua curva; a
outra pega de um nó suave segue alinhada. Retângulos, elipses, estrelas e
linhas desenham-se arrastando: Shift iguala os lados — um quadrado, um
círculo, uma linha num múltiplo de 15 graus — e Alt desenha a partir de onde
o arrasto começou. Uma estrela desenha-se a partir do seu centro; as suas
pontas, o raio interior e se é um polígono definem-se no separador Forma do
inspetor, tal como os cantos de um retângulo novo.

A caneta traça um caminho nó a nó: clique para um canto, ou arraste para um
nó suave e a sua curva. Um clique no primeiro nó fecha o caminho, e Enter, ou
escolher outra ferramenta, termina-o aberto; `Delete` ou `Backspace` retira
o último nó, e Escape descarta o caminho. O lápis desenha à mão livre, e a
sua linha é ajustada a curvas suaves quando é largado. A ferramenta de texto
começa um texto onde se clica, ou edita o texto clicado: escreva, Enter
começa uma linha, as teclas de seta, Home e End movem o cursor, e Escape, ou
um clique noutro sítio, termina a escrita como uma única alteração; um texto
deixado vazio é removido. O conta-gotas dá à seleção, e ao que for desenhado
a seguir, o estilo do objeto clicado. A ferramenta de zoom aproxima onde se
clica, afasta com Alt, e uma caixa arrastada com ela enche a janela. A mão
arrasta a tela, tal como um arrasto com o botão do meio ou com Espaço
premido, com qualquer ferramenta.

A roda desloca a tela, na horizontal com Shift; com Ctrl faz zoom em torno do
ponteiro, como um gesto de pinça num touchpad ou num ecrã tátil. Ver ▸
Ajustar página e Ajustar seleção encaixam-nas na janela, e Tamanho real mostra
o desenho a 100 por cento; o zoom vai de 1 por cento a 25600.

O inspetor tem quatro separadores. Estilo define o preenchimento e o contorno
da seleção — e, sem nada selecionado, do que for desenhado a seguir: escolha
Preenchimento ou Contorno, depois Nenhum ou uma cor do seletor de cor, que
oferece um quadrado de saturação e valor ao lado de uma faixa de tons, uma
roda ou cursores, e os valores e a opacidade da cor; depois a espessura do
contorno, as suas junções, em bico, redondas ou chanfradas, as suas pontas,
retas, redondas ou quadradas, os seus tracejados, contínuo, tracejado,
pontilhado ou traço-ponto, as pontas de seta no início e no fim — cada
clique passa por nenhuma, triângulo, seta, círculo e losango —, a regra de
preenchimento, não nula ou par-ímpar, e a opacidade de cada objeto
selecionado. Camadas lista as camadas, a de cima primeiro: prima uma para
desenhar nela, e o seu botão Visível para a mostrar ou ocultar; acima da
lista, Nova acrescenta uma camada, Remover remove a camada em mão, Bloquear
protege-a de alterações, e Subir e Descer movem-na. Objeto coloca a seleção
pela sua esquerda e pelo seu topo e escala-a para uma largura e uma altura,
alinha-a — pelos bordos esquerdos, pelos centros ou pelos bordos direitos, ou
pelo topo, pelo meio ou pelo fundo — e distribui três ou mais objetos por
igual na horizontal ou na vertical. Os objetos alinham-se dentro da sua
seleção; um só objeto, ou qualquer número com Alinhar à página ativo, dentro
da página. Forma contém os cantos de um retângulo, as pontas e o raio
interior de uma estrela e se é um polígono, e a família do texto — sem
serifas, com serifas ou monoespaçada —, o tamanho, o negrito, o itálico e o
alinhamento na sua origem. Um separador mais alto do que a janela desloca-se.
Uma cor a ser escolhida, uma espessura escrita ou ajustada passo a passo e uma
opacidade aparecem na tela à medida que mudam e tornam-se uma única alteração
a anular quando assentam; Tab passa ao campo seguinte e Shift+Tab ao
anterior.

Um arrasto a partir de uma régua coloca uma guia horizontal ou vertical na
tela; arraste uma guia para a mover, ou devolva-a a uma régua para a remover.
Ver ▸ Grelha mostra a grelha da página. Enquanto Atração está ativa, o
ponteiro é atraído ao desenhar uma forma, colocar um nó com a caneta ou mover
ou escalar a seleção: às guias, aos bordos e ao centro da página, e à grelha
enquanto se mostra. Ver ▸ Contornos mostra cada objeto como o seu contorno, o
que torna legível um desenho cheio. O menu Página define o papel da página —
A4, A3, A5, A6, A2, A1, A0, B5, B4, US Letter, US Legal, Tabloide,
Executivo, dois tamanhos de ecrã e um ícone —, roda-a entre vertical e
horizontal, dá-lhe um fundo branco ou nenhum, e define as suas unidades:
píxeis, pontos, paicas, milímetros, centímetros ou polegadas.

Objeto ▸ Agrupar reúne a seleção num grupo e Desagrupar desfá-lo; Trazer para
a frente, Subir, Descer e Enviar para trás reordenam os objetos dentro da sua
camada; Inverter e Rodar espelham a seleção ou rodam-na um quarto de volta.
Bloquear e Ocultar põem objetos fora de alcance, e Desbloquear tudo e Mostrar
tudo na camada trazem de volta os da camada em mão. O menu Caminho une as
formas selecionadas numa só (União), recorta as de cima da mais baixa
(Diferença), mantém onde se sobrepõem (Interseção) ou onde não (Exclusão),
transforma formas e texto em caminhos cujos nós se podem editar, e
transforma contornos em formas preenchidas. Estas operações calculam-se à
parte da janela, que continua livre entretanto; um desenho alterado antes de
o resultado chegar mantém a sua alteração, e o resultado não é aplicado.

Cada alteração é um passo a anular, e Anular e Refazer nomeiam o passo que
dão; guardam-se até 1000 passos, menos quando falta memória. Copiar põe a
seleção na área de transferência como SVG, e Colar acrescenta o que a área de
transferência contém à camada em mão, no seu lugar na página, selecionado.

O Draw não detém nenhuma capacidade sobre o sistema de ficheiros. Só edita o
ficheiro que lhe foi entregue: um ficheiro que o utilizador pode alterar é
entregue com escrita e Guardar volta a escrevê-lo, e qualquer outro abre só
para leitura e Guardar pergunta onde o guardar. Os documentos, e os desenhos
colados a partir da área de transferência, são lidos num processo de
trabalho separado sem qualquer alcance, pelo que um ficheiro hostil não pode
tocar em nada do que o Draw alcança.

Premir o botão secundário (direito) do rato em qualquer ponto da janela abre
o seu menu: Ficheiro, Página, Editar, Objeto, Caminho, Camada, Ver e
Ferramentas, cada um um submenu. A janela não tem barra de menus. Fechar uma
janela, ou sair, com alterações por guardar pergunta primeiro. Fechar uma
janela com o botão secundário no seu botão de fechar também mostra o seu
ficheiro no gestor de ficheiros, na pasta que o contém.

* `Ctrl+N` — uma janela nova; `Ctrl+O` — abrir um ficheiro
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como; `Ctrl+Shift+E` — guardar uma cópia como PDF, PNG ou SVG
* `Ctrl+W` — fechar a janela
* `Ctrl+Z` — anular; `Ctrl+Shift+Z` ou `Ctrl+Y` — refazer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, colar; `Ctrl+D` — duplicar
* `Delete` ou `Backspace` — eliminar a seleção
* `Ctrl+A` — selecionar tudo; `Ctrl+Shift+A` — não selecionar nada; `Escape` — não selecionar nada, ou descartar o caminho em curso
* `Ctrl+G` — agrupar; `Ctrl+Shift+G` — desagrupar
* `Page Up` / `Page Down` — subir ou descer; `Home` / `End` — trazer para a frente ou enviar para trás
* `Ctrl+[` / `Ctrl+]` — rodar para a esquerda ou para a direita
* `Ctrl+L` — bloquear; `Ctrl+Shift+L` — desbloquear tudo na camada
* `Ctrl+H` — ocultar; `Ctrl+Shift+H` — mostrar tudo na camada
* `Ctrl+Shift+C` — objeto em caminho
* `Ctrl+Shift+N` — uma camada nova; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — subir ou descer a camada
* `Ctrl+=` ou `Ctrl++` / `Ctrl+-` — aproximar ou afastar; `Ctrl+1` — tamanho real; `Ctrl+0` — ajustar a página
* `Ctrl+'` — mostrar ou ocultar a grelha; `%` — atração sim ou não
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `I`, `Z`, `H` — as ferramentas, por ordem
* teclas de seta — mover a seleção um píxel; com `Shift`, dez
* `Space` — premido, arrastar a tela com qualquer ferramenta
* `Ctrl` + roda — zoom em torno do ponteiro; `Shift` + roda — deslocar na horizontal

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair. Diferente de zero quando o canal da janela, a caixa de
eventos ou a sessão do ambiente de trabalho foi recusado; o motivo é indicado
na saída de erro padrão.
