## NAME

patience — Klondike, FreeCell e Spider, com um solucionador por trás de cada dica

## SYNOPSIS

`patience`

## DESCRIPTION

Abre uma janela do ambiente de trabalho preparada para uma partida de paciência,
os jogos de cartas também chamados solitários. Há seis jogos no menu Game, e as
teclas `1` a `6` escolhem-nos: o Klondike tirando uma carta ou três, o FreeCell
e o Spider com um, dois ou quatro naipes.

No Klondike, construa as sete colunas em ordem descendente alternando as cores,
e leve cada naipe à sua fundação, do ás ao rei. Clique no baralho para virar
cartas para o monte de descarte; quando estiver vazio, clique no seu lugar para
voltar a virar o descarte. No FreeCell todas as cartas são dadas viradas para
cima e cada uma das quatro células livres guarda uma carta; uma sequência
move-se de uma vez através das células livres e das colunas vazias enquanto
houver espaço para ela. No Spider, construa em ordem descendente com qualquer
naipe, mas mova uma sequência apenas se for toda do mesmo naipe; uma sequência
completa do rei ao ás vai sozinha para as fundações. Clique no baralho para dar
uma carta a cada coluna, o que exige antes que cada coluna tenha uma.

Arraste as cartas para onde quiser, ou clique nelas: um clique envia as cartas
para o melhor lugar para onde possam ir, primeiro para as fundações, depois para
outra coluna, depois para uma coluna vazia e depois para uma célula livre. As
cartas que não podem ir para lado nenhum dão um pequeno salto onde estão. As
cartas de que nenhuma jogada posterior poderia precisar vão sozinhas para as
fundações, e assim que nada possa impedir a vitória vão todas. Com Send cards
home desligado, Finish (`F`) envia-as.

Cada dica vem de um solucionador que joga a partida à sua frente, por isso uma
dica (`H`) é uma jogada que mantém a partida ganhável, e não apenas uma que as
regras permitem. O solucionador examina cada posição a que chega. Quando prova
que já não resta nenhuma linha vencedora, a barra de estado di-lo, e Back to
last winnable (`Ctrl+B`) leva-o de volta à última posição a partir da qual a
partida ainda podia ser ganha.

Com Winnable deals only ligado, como de início, cada nova distribuição é uma que
o solucionador já ganhou, e um visto junto ao número da distribuição indica-o. O
solucionador desiste de uma distribuição que não consiga decidir depressa, e a
que lhe dá então fica marcada como não verificada; o Spider de quatro naipes é
muitas vezes demasiado profundo para ser decidido a tempo. Cada distribuição tem
o seu número na barra de estado, e o menu próprio da mesa, aberto com o botão
secundário, joga uma distribuição pelo seu número. As distribuições do FreeCell
são numeradas como noutros jogos de FreeCell, pelo que uma distribuição numerada
é a mesma partida em toda a parte.

O Klondike é pontuado da forma padrão, ou à maneira de Las Vegas, em que cada
partida custa 52 $ e cada carta enviada para as fundações ganha 5 $, com uma só
passagem pelo baralho tirando uma carta e três tirando três, ou à maneira de Las
Vegas cumulativa, que transporta os ganhos de partida para partida. O Spider
começa com 500 pontos, perde um por cada jogada e ganha 100 por cada sequência
enviada para as fundações. Para cada jogo, as estatísticas guardam as partidas
jogadas e ganhas, a série atual de vitórias ou derrotas, a série de vitórias
mais longa, a vitória mais rápida e a vitória com menos jogadas. Voltar a dar
cartas antes de ganhar uma partida conta-a como perdida.

A partida em curso é guardada: fechar a janela ou sair deixa-a para a vez
seguinte, e o relógio para enquanto a janela estiver escondida. O menu do jogo
na barra de ícones e o menu próprio da mesa reúnem as opções: as distribuições
ganháveis, a pontuação, o envio de cartas para as fundações, os naipes a quatro
cores, os versos das cartas e a mesa.

O teclado faz tudo o que o ponteiro faz. As teclas de seta movem um anel de
pilha em pilha e ao longo das cartas de uma coluna; `Space` apanha as cartas e
pousa-as onde estiver o anel, `Enter` envia-as para o melhor lugar e `Escape`
devolve-as. `D` vira o baralho, `Ctrl+Z` desfaz uma jogada e `Ctrl+Y` refá-la,
`N` dá uma partida nova, `R` volta a dar esta e `S` mostra as estatísticas.

O jogo é iniciado a partir da biblioteca de programas do ambiente de trabalho,
em Games, ou pelo nome a partir de uma shell. Requer uma sessão gráfica em
execução: sem ela o canal da janela fica inacessível e o jogo comunica a recusa
no fluxo de erro padrão e termina.

## EXIT STATUS

Zero após uma saída limpa; diferente de zero quando o canal da janela ou a
região de imagem partilhada foi recusada (o motivo é indicado no fluxo de erro
padrão).
