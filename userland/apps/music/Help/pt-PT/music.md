## NAME

music — o leitor de música do ambiente de trabalho

## SYNOPSIS

`music`

## DESCRIPTION

Reproduz uma lista de faixas numa janela. Abra ficheiros ou uma pasta inteira
com o seletor de ficheiros do ambiente de trabalho, ou abra uma faixa a partir
do gestor de ficheiros: junta-se à lista do leitor já aberto. As faixas com a
mesma frequência, o mesmo formato de amostra e a mesma disposição de canais
sucedem-se sem pausa.

O leitor não detém nenhuma capacidade sobre o sistema de ficheiros. A sessão
do ambiente de trabalho navega por ele e delega-lhe, uma única vez e só para
leitura, exatamente os ficheiros que o utilizador escolhe — para uma pasta, os
ficheiros que contém e que este leitor abre. Nenhum ficheiro é descodificado
dentro do leitor: o som e a capa são descodificados, cada um, por um processo
à parte sem qualquer alcance, pelo que um ficheiro malformado ou hostil não
consegue chegar a nada a que o leitor chegue.

O topo da janela mostra o que está a tocar: a capa, o título, o artista e o
álbum, o formato, a posição e um medidor de nível por canal. Por baixo ficam o
transporte, os controlos de aleatório e repetição e o volume, e por baixo
destes a lista. Arraste o controlo de posição para saltar e o de volume para
definir o nível; ambos atuam onde os soltar. Faça duplo clique numa faixa para
a reproduzir. Um toque secundário na lista abre o menu do leitor, que também
escolhe a saída e se as faixas são niveladas pela intensidade que as suas
próprias etiquetas indicam.

Um ficheiro que o leitor não consegue ler fica de fora, com o motivo na linha
de estado.

* `Space` — reproduzir ou pausar
* `Enter` — reproduzir a faixa selecionada
* `Left` / `Right` — dez segundos para trás ou para a frente
* `Ctrl` + `Left` / `Right` — a faixa anterior ou a seguinte
* `Up` / `Down` — selecionar a faixa acima ou abaixo
* `Alt` + `Up` / `Down` — mover a faixa selecionada
* `Delete` — tirar da lista a faixa selecionada
* `+` / `-` — três decibéis mais alto ou mais baixo
* `S` — aleatório ou por ordem
* `R` — não repetir nada, repetir a lista ou a faixa
* `Ctrl` + `O` — abrir ficheiros
* `Ctrl` + `Shift` + `O` — abrir uma pasta

## OPTIONS

`-h`, `-?`, `--help`
: Escrever esta ajuda na saída padrão e sair.

## EXIT STATUS

Zero depois de a janela ser fechada ou de se escolher Sair. Diferente de zero
quando o canal da janela, a região de imagem partilhada ou a sessão do
ambiente de trabalho foi recusado; o motivo é indicado na saída de erro
padrão.
