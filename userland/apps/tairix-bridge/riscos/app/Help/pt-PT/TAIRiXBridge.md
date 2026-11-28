## NAME

TAIRiXBridge — TAIRiX Bridge, programas RISC OS no ambiente de trabalho TAIRiX

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Executa programas RISC OS — programas BASIC, ferramentas de linha de comandos
e jogos de tarefa única — no ambiente de trabalho TAIRiX, cada um a mostrar o
seu ecrã numa janela própria. O RISC OS em que correm é o do TAIRiX Bridge,
escrito a partir de documentação publicada; não contém código do RISC OS.

Os programas correm em sessões. A sessão partilhada é a máquina RISC OS que a
janela TAIRiX Bridge mostra: o seu disco próprio é a pasta RISC OS nos seus
Documentos, a sua sequência de arranque corre quando inicia, e um programa do
seu próprio armazenamento corre nela. Um programa de qualquer outro lugar —
suportes amovíveis, uma imagem de disco, uma partilha de rede — ou de origem
desconhecida corre numa sessão isolada própria, que nunca vê a sua pasta RISC
OS e termina com o seu último programa. Executar na sessão partilhada executa
um ficheiro na sessão partilhada, venha de onde vier.

A janela de uma sessão lista as suas tarefas, cada uma com a sua memória e se
está em pausa ou ocupada, e o que ocupam as áreas partilhadas da sessão. Executar escolhe
um ficheiro para executar; um ficheiro largado na janela, ou aberto com o
TAIRiX Bridge, também é executado. Escolher uma tarefa e depois Pausa ou Sair
suspende-a ou termina-a.

O ecrã de cada programa é uma janela própria, com a imagem ajustada à janela
nas proporções do seu modo. O botão Ecrã abre o menu da janela: Ecrã inteiro,
escala nítida ou suave, Pausa e Repor, que termina o programa e volta a
executá-lo na mesma janela. O ecrã fica com o teclado inteiro, cada tecla com
o número que o RISC OS lhe dá. Select, Menu e Adjust são os botões principal,
secundário e do meio. Assim que um programa move o rato por si, um clique
captura o rato para ele, e um único toque na tecla Meta liberta-o.

Definições escolhe a ordem dos botões, a memória que cada sessão pode tomar,
se cada programa é isolado e como os ecrãs são ajustados. O processo de
trabalho de uma sessão não detém qualquer autoridade: os ficheiros que lê e
escreve são-lhe transmitidos, e um processo que falha termina a sua sessão,
com o motivo mostrado na janela da sessão.

## OPTIONS

`-h`, `-?`, `--help`
: Escrever esta ajuda na saída padrão e sair.

## EXIT STATUS

Zero depois de fechada a janela da última sessão. Diferente de zero quando o
canal de janelas, a caixa de eventos ou a sessão do ambiente de trabalho foi
recusado; o motivo é indicado no fluxo de erro padrão.
