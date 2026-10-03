## NAME

Paint — editor gráfico de imagens e sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Pinta e edita imagens numa janela do ambiente de trabalho, píxel a píxel ou
com pincéis e formas. Iniciado com um documento — a partir do gestor de
ficheiros, do ambiente de trabalho, ou largando um ficheiro sobre o seu
ícone na barra de ícones — abre uma janela sobre ele. Iniciado sozinho, abre
uma nova imagem branca. Cada documento é uma janela do único programa;
fechar a última deixa-o na barra de ícones, e a linha Sair do seu menu de
ícone termina-o.

Abre todos os formatos de imagem que o sistema lê: PNG, JPEG, GIF, BMP,
TIFF, WebP, ícones do Windows e ficheiros de sprites do RISC OS. Escreve
PNG, JPEG e ficheiros de sprites; uma imagem lida de qualquer outro formato
é guardada como um ficheiro novo. Um PNG mantém a sua paleta, e um JPEG é
escrito com a qualidade definida em Qualidade JPEG no menu Ficheiro.

Um ficheiro de sprites contém qualquer número de sprites, cada um com o seu
nome, modo de ecrã, paleta e máscara. Cada profundidade é editada tal como
está guardada: 2, 4, 16 e 256 cores e milhões de cores. Um sprite sem paleta
própria mostra as cores do ambiente de trabalho do RISC OS — com 16 cores, a
cor n é a cor Wimp n; com 2 cores, as cores Wimp 0 e 7; com 4 cores, as
cores Wimp 0, 2, 4 e 7; com 256 cores, a disposição de tons do RISC OS —
nunca uma paleta de PC. Um sprite cujos píxeis são mais altos do que largos,
como no modo 12, é mostrado assim. Um sprite que este editor não consegue
ler, como um CMYK, é mantido exatamente como estava e guardado de novo sem
alterações. O menu Sprites vai para um sprite, acrescenta, copia, muda o
nome, apaga e reordena sprites.

O botão principal (esquerdo) pinta com a cor principal e o botão do meio
com a cor secundária; mantendo Alt premido, recolhe-se antes uma cor. As
ferramentas são seleção, lápis, pincel, aerógrafo, borracha, preenchimento,
conta-gotas, linha, retângulo e elipse; o painel ao lado da imagem contém a
paleta da imagem ou as cores do ambiente de trabalho e as definições da
ferramenta em uso. O painel de cor à direita contém as cores principal e
secundária e um seletor de cor para a que for escolhida: clique numa cor
para a escolher e defina-a por matiz, saturação e valor, por vermelho,
verde e azul, pela sua escrita hexadecimal e, onde a imagem admite
transparência, pela sua opacidade. Ao lado fica a cor que tinha, e um
clique repõe-na. Numa imagem com paleta as cores são as suas entradas, pelo
que o seletor edita a paleta, e cada edição é uma alteração a anular. Mantendo Shift
premido desenha-se um quadrado, um círculo ou uma linha em múltiplos de 45
graus.

Com a ferramenta de seleção, arraste para marcar parte da imagem e depois
arraste a seleção para a mover; flutua até ser pousada, e movê-la é uma
única alteração a anular. As imagens copiadas passam pela área de
transferência como PNG, e o que é colado flutua até ser pousado.

O programa não detém nenhuma capacidade sobre o sistema de ficheiros. Edita
apenas o ficheiro que lhe foi entregue. Um ficheiro que o utilizador pode
alterar é entregue com permissão de escrita, e Guardar reescreve-o;
qualquer outro é só de leitura, e Guardar pergunta onde guardar uma cópia.
As imagens, também as coladas da área de transferência, são descodificadas
num processo de trabalho à parte sem qualquer alcance, e cada documento
recebe um novo: um ficheiro hostil não consegue tocar em nada do que o
programa consegue.

Premir o botão secundário (direito) do rato em qualquer ponto da janela abre
o seu menu: Cortar, Copiar, Colar, Selecionar tudo e Desselecionar, e depois
Ficheiro, Editar, Imagem, Cores, Sprites, Ver e Ferramentas, cada um com o
seu submenu. A janela não tem barra de menus. Fechar uma janela ou sair com
alterações por guardar pergunta primeiro.

* `Ctrl+N` — uma imagem nova; `Ctrl+O` — abrir um ficheiro
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — fechar a janela
* `Ctrl+Z` — anular; `Ctrl+Shift+Z` ou `Ctrl+Y` — refazer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, colar
* `Ctrl+A` — selecionar tudo; `Ctrl+D` — desselecionar
* `Enter` — pousar uma seleção flutuante; `Escape` — devolvê-la
* `Delete` — limpar a seleção
* `Ctrl+Shift+X` — recortar pela seleção
* `Ctrl+R` — redimensionar; `Ctrl+Shift+R` — tamanho da tela
* `Ctrl+[` / `Ctrl+]` — rodar para a esquerda ou para a direita
* `Ctrl+I` — inverter as cores
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — as ferramentas, por ordem
* `X` — trocar as cores principal e secundária
* `Tab` — para o painel de cor e pelas suas partes; `Escape` — de volta à imagem
* `+` / `-` — ampliar ou reduzir; `1` — tamanho real; `Ctrl+0` — ajustar
* `Ctrl` + roda — ampliar ou reduzir em torno do ponteiro
* Beliscar com dois dedos — ampliar ou reduzir de forma contínua; num ecrã tátil a imagem acompanha os dedos
* `G` — mostrar ou ocultar a grelha entre píxeis
* `Page Up` / `Page Down` — o sprite anterior ou seguinte
* teclas de seta — mover uma seleção flutuante um píxel; com `Shift`, dez

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair. Diferente de zero quando o canal da janela, a caixa de
eventos ou a sessão do ambiente de trabalho foram recusados; o motivo é
indicado na saída de erro padrão.
