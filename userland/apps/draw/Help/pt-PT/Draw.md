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

O Draw abre e guarda SVG, e abre ficheiros Draw do RISC OS, que guarda como
SVG. Um desenho mantém a sua página e as suas unidades, as suas camadas,
grupos e recortes, cada caminho, forma, texto e imagem, as cores lisas e os
gradientes lineares e radiais, a opacidade de cada objeto, os tracejados, as
junções, as pontas e as pontas de seta, as guias e a grelha, e o título do
desenho. Um ficheiro que contém o que um desenho não consegue manter — efeitos
de filtro, máscaras, preenchimentos de padrão, marcadores, imagens ilegíveis,
estilos ou caminhos de texto que um texto não pode guardar, animação ou
scripts, entre outros — abre só para leitura, com a faixa de estado a nomear o
que contém, e Guardar pede um nome novo para que o original nunca seja
substituído. Guardar uma cópia escreve o desenho como PDF; como EPS, com a
transparência tornada opaca, como o PostScript a pinta; como PNG, JPEG ou
TIFF, à resolução de cópia da página e até 8192 píxeis no lado mais comprido;
como SVG; ou como ficheiro Draw do RISC OS, nomeando o que o formato não
conseguiu guardar do desenho, como os gradientes; todas sem tocar no ficheiro
do documento.

A caixa de ferramentas desce pelo lado esquerdo da janela, o inspetor pelo
direito, e a faixa de estado corre no fundo: mostra a posição do ponteiro e o
tamanho da seleção nas unidades da página, e diz o que fez o último comando.
No topo, a barra diz como se usa a ferramenta em mão e mostra o zoom: premir
o zoom abre um menu de zooms. Réguas nas unidades da página correm ao longo
do topo e da esquerda da tela, e barras de deslocamento ao longo da direita e
do fundo. Parar sobre o botão de uma ferramenta nomeia-a, com a tecla que a
escolhe.

As ferramentas, e as teclas que as escolhem, são: seleção (`V`), nós (`A`),
retângulo (`R`), elipse (`E`), estrela e polígono (`S`), linha (`L`), caneta
(`B`), lápis (`P`), texto (`T`), gradiente (`G`), conta-gotas (`I`), zoom
(`Z`) e mão (`H`).

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

A caneta traça um caminho nó a nó: clique para um canto, ou arraste para um nó
suave e a sua curva. Um clique no primeiro nó fecha o caminho, e Enter, ou
escolher outra ferramenta, termina-o aberto; `Delete` ou `Backspace` retira o
último nó, e Escape descarta o caminho. O lápis desenha à mão livre, e a sua
linha é ajustada a curvas suaves quando é largado. A ferramenta de texto
começa um texto onde se clica, ou edita o texto clicado: escreva, Enter começa
uma linha, as teclas de seta, Home e End movem o cursor, e Escape, ou um
clique noutro sítio, termina a escrita como uma única alteração; um texto
deixado vazio é removido. O conta-gotas dá à seleção, e ao que for desenhado a
seguir, o estilo do objeto clicado. A ferramenta de gradiente põe um gradiente
ao longo do arrasto na seleção ou, sem nada selecionado, no objeto premido:
Shift roda-o em passos de 15°, e Alt, ou um gradiente já radial, põe-no radial
em torno de onde o arrasto começou; enquanto a ferramenta está em mão, o
gradiente do objeto selecionado aparece como uma linha entre duas pegas. A
ferramenta de zoom aproxima onde se clica, afasta com Alt, e uma caixa
arrastada com ela enche a janela. A mão arrasta a tela, tal como um arrasto
com o botão do meio ou com Espaço premido, com qualquer ferramenta.

Premida sobre um texto, a ferramenta de texto põe o cursor onde foi premida.
Shift com as setas, Início e Fim, ou um arrasto pelo texto, seleciona
carateres, e `Ctrl+A` seleciona-os todos; o que se escrever então toma o seu
lugar, no estilo do primeiro deles. Enquanto há carateres selecionados, um
tipo de letra, um tamanho, o negrito, o itálico, o espaçamento entre letras, a
elevação ou uma cor escolhidos no inspetor definem só esses; sem nenhum
selecionado, definem o texto inteiro, cada um dos seus carateres. O separador
Forma guarda o espaçamento entre letras de um texto, quanto os seus carateres
estão elevados, em percentagem do seu tamanho, e o seu entrelinhamento — ou,
num texto ao longo de um caminho, a que distância ao longo do caminho começa —
e, enquanto se escreve, o inspetor mostra como está definido o primeiro
caráter selecionado. Um gradiente pinta sempre um texto inteiro.

Caminho ▸ Pôr texto no caminho, com um texto e uma forma selecionados, põe o
texto ao longo do contorno da forma, que se junta a ele: cada caráter assenta
no caminho, rodado segundo a sua direção, um texto ao longo de um caminho é
uma só linha e um caráter que cai para lá de uma ponta não é desenhado. Soltar
do caminho volta a pôr o texto em linhas a partir de onde o seu caminho
começava e devolve o caminho como um objeto próprio. Objeto para caminho
transforma um texto pintado de mais de uma maneira num grupo de um caminho
para cada maneira. A ferramenta de nós edita o caminho ao longo do qual um
texto está posto como edita qualquer caminho.

A roda desloca a tela, na horizontal com Shift; com Ctrl faz zoom em torno do
ponteiro, como um gesto de pinça num touchpad ou num ecrã tátil. Ver ▸
Ajustar página e Ajustar seleção encaixam-nas na janela, e Tamanho real mostra
o desenho a 100 por cento; o zoom vai de 1 por cento a 25600.

O inspetor tem cinco separadores. Estilo define o preenchimento e o contorno
da seleção — e, sem nada selecionado, do que for desenhado a seguir: escolha
Preenchimento ou Contorno, depois Nenhum ou uma cor do seletor de cor, que
oferece um quadrado de saturação e valor ao lado de uma faixa de tons, uma
roda ou cursores, e os valores e a opacidade da cor; depois a espessura do
contorno, as suas junções, em bico, redondas ou chanfradas, as suas pontas,
retas, redondas ou quadradas, os seus tracejados, contínuo, tracejado,
pontilhado ou traço-ponto, as pontas de seta no início e no fim — cada clique
passa por nenhuma, triângulo, seta, círculo e losango —, a regra de
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
da página. Forma contém os cantos de um retângulo, as pontas e o raio interior
de uma estrela e se é um polígono, e a família do texto — sem serifas, com
serifas ou monoespaçada —, o tamanho, o negrito, o itálico e o alinhamento na
sua origem. Um separador mais alto do que a janela desloca-se. Uma cor a ser
escolhida, uma espessura escrita ou ajustada passo a passo e uma opacidade
aparecem na tela à medida que mudam e tornam-se uma única alteração a anular
quando assentam; Tab passa ao campo seguinte e Shift+Tab ao anterior.

Em Estilo, Linear e Radial pintam o preenchimento ou o contorno com um
gradiente ajustado a cada objeto, da sua cor ao branco, ou ao preto onde é
clara. A barra por baixo mostra o gradiente sobre um xadrez, com uma pega para
cada paragem: prima uma pega para tomar essa paragem em mão — a sua cor passa
então ao seletor de cor — e arraste-a ao longo da barra; prima a barra para aí
acrescentar uma paragem. Delete, ou Remover paragem, remove a paragem em mão;
Esquerda e Direita tomam a paragem ao lado e, com Shift, movem-na uma
centésima. Paragem em coloca a paragem em mão, Ângulo roda um gradiente
linear, Prolongar, Refletir e Repetir dizem como um gradiente continua para lá
das suas pontas, e Inverter fá-lo correr ao contrário. Por baixo do seletor,
as amostras — cinzentos, depois doze matizes em cor plena, clara e escura,
depois as do próprio desenho — dão a sua cor à cor ou à paragem em mão;
Adicionar amostra guarda a cor em mão com o desenho e Remover amostra larga
uma das suas. Em Objeto, Rotação roda a seleção no sentido contrário ao dos
ponteiros do relógio: um objeto até ao ângulo dado, vários em conjunto segundo
ele.

Página define a largura e a altura da página na sua unidade, e se fica na
vertical ou na horizontal; a unidade — px, pt, pc, mm, cm ou in; o espaçamento
da grelha nessa unidade, e em quantas partes se divide cada lado dos seus
quadrados; os ppp de cópia, a resolução a que se fazem as suas cópias PNG,
JPEG e TIFF; e o fundo da página, Nenhum ou uma cor de um seletor de cor
próprio, mostrada na tela enquanto é escolhida. Uma página sem fundo aparece
como papel branco, e uma cópia sua em PNG é transparente onde nada está
desenhado.

Um arrasto a partir de uma régua coloca uma guia horizontal ou vertical na
tela; arraste uma guia para a mover, ou devolva-a a uma régua para a remover.
Ver ▸ Grelha mostra a grelha da página. Enquanto Atração está ativa, o
ponteiro é atraído ao desenhar uma forma, colocar um nó com a caneta ou mover
ou escalar a seleção: às guias, aos bordos e ao centro da página, e à grelha
enquanto se mostra. Ver ▸ Contornos mostra cada objeto como o seu contorno, o
que torna legível um desenho cheio. Configurar página, no menu Página, mostra
o separador Página, e o menu define o papel da página — A4, A3, A5, A6, A2,
A1, A0, B5, B4, US Letter, US Legal, Tabloide, Executivo, dois tamanhos de
ecrã e um ícone —, roda-a entre vertical e horizontal, dá-lhe o fundo do
separador Página ou nenhum, e define as suas unidades: píxeis, pontos, paicas,
milímetros, centímetros ou polegadas.

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

Objeto ▸ Recortar pelo objeto de cima mostra o resto da seleção apenas dentro
do seu objeto mais alto, e Libertar recorte volta a mostrar inteiro um grupo
recortado, com o objeto que o recortava de novo por cima do que continha.
Caminho ▸ Dividir corta as formas selecionadas nos pedaços que as suas
sobreposições formam. Expandir e Contrair aumentam ou reduzem cada forma
selecionada pelo Deslocamento do separador Forma, Simplificar volta a traçar
cada uma com tão poucos nós quantos a mantenham dentro da Suavidade do
separador, e Inverter sentido faz um caminho correr ao contrário, para que as
suas pontas de seta e os seus tracejados o sigam.

Cada alteração é um passo a anular, e Anular e Refazer nomeiam o passo que
dão; guardam-se até 1000 passos, menos quando falta memória. Copiar põe a
seleção na área de transferência como SVG, e Colar acrescenta o que a área de
transferência contém à camada em mão, no seu lugar na página, selecionado.

Ficheiro ▸ Colocar imagem (`Ctrl+Shift+P`) pede uma imagem — um ficheiro PNG,
JPEG, GIF, BMP, TIFF, WebP, de ícone do Windows ou OpenRaster, ou um ficheiro
de sprites do RISC OS, a primeira página ou o primeiro sprite de um que tenha
vários — e pousa-a na camada em mão como um só passo, selecionada e com o nome
do seu ficheiro, no meio do que a tela mostra: no seu próprio tamanho, ou
reduzida a quatro quintos da tela onde é maior. O ficheiro é só lido, nunca
alterado. Uma imagem move-se, escala-se, roda, recorta-se e copia-se como
qualquer objeto; não tem estilo nem nós próprios, e onde formas se unem,
cortam ou dividem participa como o seu retângulo. Guarda-se com o desenho: o
SVG incorpora-a, uma cópia PDF ou EPS desenha-a e um ficheiro Draw guarda-a
como sprite ou como JPEG. As imagens de um desenho contêm juntas no máximo 268
milhões de píxeis, cada uma contada uma vez por muito que se mostre; uma
imagem que as levaria além disso é recusada, e a faixa de estado diz porquê.

Definições, no menu da barra de ícones, abre a janela de definições do Draw: a
ferramenta com que começa uma janela nova, e se mostra a grelha, se ajusta e
desenha só contornos; o papel, a orientação, as unidades e a grelha com que
começa um desenho novo; o preenchimento e o traço com que se desenham os
objetos novos, cada um nenhum ou uma cor, e a espessura do traço; quanto as
teclas de seta deslocam, um píxel se não definido; a que distância um
duplicado fica do que copia; e quanto o ponteiro tem de se aproximar de uma
pega, de um nó ou de uma guia para os agarrar. Uma alteração aplica-se logo e
fica para a próxima vez; Repor predefinições volta a pô-las todas.

O Draw não detém nenhuma capacidade sobre o sistema de ficheiros. Só edita o
ficheiro que lhe foi entregue: um ficheiro que o utilizador pode alterar é
entregue com escrita e Guardar volta a escrevê-lo, e qualquer outro abre só
para leitura e Guardar pergunta onde o guardar. Os documentos, os desenhos
colados a partir da área de transferência e as imagens colocadas são lidos num
processo de trabalho separado sem qualquer alcance, pelo que um ficheiro
hostil não pode tocar em nada do que o Draw alcança.

Premir o botão secundário (direito) do rato em qualquer ponto da janela abre
o seu menu: Ficheiro, Página, Editar, Objeto, Caminho, Camada, Ver e
Ferramentas, cada um um submenu. A janela não tem barra de menus. Fechar uma
janela, ou sair, com alterações por guardar pergunta primeiro. Fechar uma
janela com o botão secundário no seu botão de fechar também mostra o seu
ficheiro no gestor de ficheiros, na pasta que o contém.

* `Ctrl+N` — uma janela nova; `Ctrl+O` — abrir um ficheiro; `Ctrl+Shift+P` — colocar uma imagem
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como; `Ctrl+Shift+E` — guardar uma cópia
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
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `G`, `I`, `Z`, `H` — as ferramentas, por ordem
* teclas de seta — mover a seleção quanto dizem as definições, um píxel se não definido; com `Shift`, dez vezes mais
* `Space` — premido, arrastar a tela com qualquer ferramenta
* `Ctrl` + roda — zoom em torno do ponteiro; `Shift` + roda — deslocar na horizontal

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair. Diferente de zero quando o canal da janela, a caixa de
eventos ou a sessão do ambiente de trabalho foi recusado; o motivo é indicado
na saída de erro padrão.
