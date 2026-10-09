## NAME

Paint — editor gráfico de imagens e sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Pinta e edita imagens numa janela do ambiente de trabalho, píxel a píxel ou
com pincéis e formas. Iniciado com um documento — a partir do gestor de
ficheiros, do ambiente de trabalho ou largando um ficheiro sobre o seu ícone
na barra de ícones — abre uma janela sobre ele. Iniciado sozinho, abre uma
nova imagem branca. Cada documento é uma janela do único programa; fechar a
última deixa-o na barra de ícones, e a linha Sair do seu menu de ícone
termina-o.

Abre todos os formatos de imagem que o sistema lê: PNG, JPEG, GIF, BMP,
TIFF, WebP, ícones do Windows, ficheiros de sprites do RISC OS e OpenRaster.
Escreve PNG, JPEG, GIF, BMP, TIFF, ficheiros de sprites e OpenRaster; uma
imagem lida de qualquer outro formato, ou de um ficheiro que guarda mais do
que a sua imagem, como um perfil de cor, é guardada como um ficheiro novo.
Nova imagem pergunta primeiro para que formato é a imagem e oferece as cores
que esse formato admite. Guardar como pergunta o formato e as suas próprias
definições — a qualidade de um JPEG, se um GIF é entrelaçado, a compressão
de um TIFF — e diz o que o formato não consegue guardar, antes de perguntar
onde. Uma paleta é mantida onde quer que o formato a admita, e também a
densidade de uma imagem.

Uma imagem pode ser feita de camadas, a de baixo primeiro, cada uma com um
nome, uma opacidade e se está visível; pinta-se numa camada de cada vez, e a
janela mostra-as sobrepostas. O OpenRaster guarda as camadas; qualquer outro
formato recebe-as sobrepostas. O menu Camadas acrescenta, copia, elimina,
sobe e desce camadas, junta uma à de baixo e achata a imagem, e as suas
Propriedades da camada mudam o nome de uma camada e definem quanto dela se
vê. Os ajustes, preenchimentos e traços mudam a camada em que se pinta; as
rotações, inversões, redimensionamentos e recortes mudam todas as camadas.
Uma imagem com paleta tem uma só camada.

Um ficheiro de sprites guarda qualquer número de sprites, cada um com o seu
nome, modo de ecrã, paleta e máscara. Cada profundidade é editada tal como
está guardada: 2, 4, 16 e 256 cores e milhões de cores. Um sprite sem paleta
própria mostra as cores do ambiente de trabalho do RISC OS — com 16 cores, a
cor n é a cor Wimp n; com 2 cores, as cores Wimp 0 e 7; com 4 cores, as
cores Wimp 0, 2, 4 e 7; com 256 cores, a disposição de tons do RISC OS —
nunca uma paleta de PC. Um sprite cujos píxeis são mais altos do que largos,
como no modo 12, é mostrado assim. Um sprite que este editor não consegue
ler, como um CMYK, é mantido exatamente como estava e escrito de volta sem
alterações. O menu Sprites vai para sprites, acrescenta-os, copia-os,
muda-lhes o nome, elimina-os e reordena-os. Um TIFF guarda qualquer número
de páginas, e para ele o menu Páginas vai para páginas, acrescenta-as,
copia-as, elimina-as e reordena-as.

O botão principal (esquerdo) pinta com a cor principal e o botão do meio
com a cor secundária; mantendo Alt premido apanha-se antes uma cor, tal como
as camadas a mostram. A caixa de ferramentas, no painel Ferramentas, dispõe
as ferramentas em duas colunas: seleção, lápis, pincel, aerógrafo, borracha,
clonar, preenchimento, gradiente, conta-gotas, texto, linha, retângulo,
elipse, polígono, recorte, mão e zoom. A barra no topo nomeia a ferramenta em
uso e guarda as suas definições — o tamanho, a dureza, a opacidade, o fluxo e
o espaçamento de um pincel, a tolerância de um preenchimento, a forma de um
gradiente, o tamanho do texto, os cantos de um retângulo — escritas ou
ajustadas com as setas, e os botões que ampliam e mostram a grelha de píxeis;
a faixa da paleta sob a imagem guarda a paleta da imagem, ou as cores do
ambiente de trabalho. O aerógrafo continua a pulverizar enquanto é mantido
parado. Mantendo Shift premido desenha-se um quadrado, um círculo ou uma
linha num múltiplo de 45 graus.

Ao longo dos dois lados da janela correm painéis: por omissão o painel
Ferramentas à esquerda e o painel Cor à direita, com o painel Ajuste por baixo
logo que se abre um ajuste. Cada um tem no topo uma faixa fina que o nomeia,
com um controlo que o enrola na sua faixa e uma marca que o fecha; Ver ▸
Painéis volta a mostrar um painel fechado, e Repor painéis põe cada painel
como uma janela nova o tem. Arrastar uma faixa move o seu painel no seu lado
ou para o outro, marcando pelo caminho onde vai pousar; largado longe de ambos
os lados, ou arrastado para fora da janela, o painel flutua numa pequena
janela sua, movida pela sua faixa, mantida sobre a imagem e fechada pela sua
marca; arrastado de novo sobre um lado, volta a acoplar-se ali.

A ferramenta de seleção marca um retângulo, uma elipse, um laço à mão
livre, um polígono clicado canto a canto ou, com a varinha mágica, os píxeis
ligados a um por cores semelhantes. Shift acrescenta à seleção, Alt
retira-lhe, e ambos guardam só o que as duas partilham; Esbater suaviza a
sua margem. Enquanto há uma seleção, todas as ferramentas, preenchimentos e
ajustes ficam presos a ela. Arrastar dentro dela levanta-a e move-a: flutua
até ser pousada, e movê-la é uma só alteração a desfazer. As imagens
copiadas viajam pela área de transferência como PNG, e o que se cola flutua
até ser pousado.

A ferramenta de clonar pinta o que está noutro lugar da imagem: Alt e
clique onde copiar, e depois pintar. A ferramenta de gradiente funde a cor
principal na secundária ao longo de um arrasto, em faixas ou em anéis. A
ferramenta de texto põe as palavras escritas onde se clica; Enter começa uma
nova linha, outro clique ou outra ferramenta pousa-as, e Escape descarta-as.
Os cantos da ferramenta de polígono são clicados à vez, e um clique no
primeiro ou Enter fecha-o. A ferramenta de recorte marca a parte a guardar,
as suas pegas movem as suas margens, e Enter recorta. A mão arrasta a imagem
pela janela, tal como Espaço com qualquer ferramenta; o zoom amplia com um
clique, ou com Alt reduz, e uma caixa arrastada enche a janela.

O menu Ajustes abre um ajuste no painel Ajuste, onde todas as outras
ferramentas, painéis e menus continuam à mão: brilho e contraste, tonalidade e
saturação, equilíbrio de cor, níveis, curvas, equilíbrio de brancos,
posterizar, limiar, desfocar, nitidez, pixelizar e adicionar ruído;
dessaturar e encontrar contornos, que não têm definições, aplicam-se de
imediato. A imagem mostra o ajuste à medida que as suas definições se movem,
Pré-visualizar desliga-o e liga-o para comparar, Repor volta a pôr as suas
definições e Aplicar guarda-o como uma só alteração a anular; pintar, preencher
ou escolher outro ajuste aplica-o primeiro. Os níveis fixam os pontos preto,
cinzento e branco sobre um histograma da camada, para todos os canais juntos
ou cada um sozinho, com conta-gotas que os tiram da imagem e Auto; as curvas
dobram os tons de um canal por pontos arrastados sobre o seu histograma; o
equilíbrio de brancos fixa a temperatura e o matiz da luz, a partir de um
píxel neutro escolhido ou por Auto; tonalidade e saturação rodam, reforçam e
aclaram todas as cores ou uma gama delas; o equilíbrio de cor leva as sombras,
os meios-tons e as altas luzes para o vermelho, o verde ou o azul, mantendo a
sua luminosidade se pedido. Tudo fica limitado à seleção. Numa imagem com
paleta, um ajuste muda a sua paleta, e os que precisam de cores vizinhas não
são oferecidos.

O painel Cor guarda as cores principal e secundária e um seletor de cor para
a que estiver escolhida: um clique numa cor escolhe-a, e depois apanha-se num
quadrado de saturação e valor ao lado de uma faixa de tonalidades, numa roda
de tonalidades em volta de um triângulo, ou num cursor por canal, e escreve-se
em RGB, HSV, HSL, CMYK, Lab, LCh ou como cinzento, ou pela sua escrita
hexadecimal e, onde a imagem guarda transparência, a sua opacidade. Uma cor
Lab ou LCh que o ecrã não consegue mostrar é mostrada o mais perto possível, e
assinalada. Trocar troca as duas cores, Repor torna-as preta e branca, e
Apanhar tira a próxima cor clicada na imagem. A cor que tinha fica ao lado, um
clique recupera-a, e as últimas cores escolhidas esperam por baixo para serem
escolhidas de novo. Numa imagem com paleta as cores são as suas entradas,
pelo que o seletor edita a paleta, e cada edição é uma só alteração a anular.

Definições, no menu da barra de ícones, abre a janela de definições do Paint:
a ferramenta com que começa uma janela nova e se uma imagem abre ajustada à
janela ou em tamanho real; o tamanho, o formato, as cores e o fundo que Nova
imagem oferece; o espaçamento, o desvio, a cor, a opacidade e o estilo da
grelha — linhas, traços, pontos ou cruzamentos —, se uma janela nova a mostra,
o ajuste a ela e o zoom a partir do qual aparece a grelha entre píxeis; o
tamanho e os tons do xadrez e o que rodeia a imagem; e os painéis com que abre
uma janela nova. Uma alteração aplica-se logo a todas as janelas e fica para a
próxima vez; Repor predefinições volta a pô-las todas. Ver ▸ Grelha mostra a
grelha de uma janela. Enquanto está visível e o ajuste está ligado, as formas,
as seleções e as molduras de recorte cobrem células inteiras, as pontas de uma
linha e de um gradiente e os cantos de um polígono pousam nos seus
cruzamentos, e uma seleção arrastada pousa com o canto num deles.

O programa não tem nenhuma permissão sobre o sistema de ficheiros. Só edita
o ficheiro que lhe foi entregue. Um ficheiro que o utilizador pode alterar é
entregue com escrita, e Guardar escreve-o de volta; qualquer outro é só de
leitura, e Guardar pergunta onde guardar uma cópia. As imagens, e as coladas
da área de transferência, são descodificadas num processo de trabalho à
parte sem qualquer alcance, e cada documento recebe um novo: um ficheiro
hostil não consegue tocar em nada do que o programa alcança.

Premir o botão secundário (direito) do rato em qualquer ponto da janela abre
o seu menu: Cortar, Copiar, Colar, Selecionar tudo e Desselecionar, e depois
Ficheiro, Editar, Imagem, Camadas, Cores, Ajustes, Sprites ou Páginas, Ver e
Ferramentas, cada um com o seu próprio submenu. A janela não tem barra de
menus. Fechar uma janela ou sair com alterações por guardar pergunta
primeiro.

* `Ctrl+N` — uma nova imagem; `Ctrl+O` — abrir um ficheiro
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — fechar a janela
* `Ctrl+Z` — desfazer; `Ctrl+Shift+Z` ou `Ctrl+Y` — refazer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, colar
* `Ctrl+A` — selecionar tudo; `Ctrl+D` — desselecionar
* `Enter` — pousar uma seleção flutuante, fechar um polígono ou recortar; `Escape` — voltar atrás
* `Delete` — limpar a seleção; `Alt+Backspace` — preenchê-la com a cor principal
* `Ctrl+Shift+X` — recortar para a seleção
* `Ctrl+R` — redimensionar; `Ctrl+Shift+R` — tamanho da tela
* `Ctrl+[` / `Ctrl+]` — rodar para a esquerda ou para a direita
* `Ctrl+I` — inverter as cores
* `Ctrl+Shift+N` — uma nova camada; `Ctrl+E` — juntar com a de baixo; `Ctrl+Shift+E` — achatar
* `Ctrl+Page Up` / `Ctrl+Page Down` — pintar na camada de cima ou de baixo
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — subir ou descer a camada
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — as ferramentas, por ordem
* `Space` — premido, arrastar a imagem com qualquer ferramenta
* `X` — trocar as cores principal e secundária
* `Tab` — pelas definições da ferramenta, pela faixa da paleta e pelo painel de cor; `Shift+Tab` — para trás; `Escape` — de volta à imagem
* `+` / `-` — ampliar ou reduzir; `1` — tamanho real; `Ctrl+0` — ajustar
* `Ctrl` + roda — ampliar ou reduzir em torno do ponteiro
* Beliscar com dois dedos — ampliar ou reduzir suavemente; num ecrã tátil a imagem segue os dedos
* `G` — mostrar ou ocultar a grelha entre píxeis
* `Ctrl+'` — mostrar ou ocultar a grelha
* `Page Up` / `Page Down` — o sprite ou a página anterior ou seguinte
* teclas de seta — mover uma seleção flutuante um píxel; com `Shift`, dez; na faixa da paleta, percorrer as suas cores

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair. Diferente de zero quando o canal da janela, a caixa de
eventos ou a sessão do ambiente de trabalho foi recusado; o motivo é
indicado na saída de erro padrão.
