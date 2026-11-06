## NAME

recorder — gravar o ecrã para o ambiente de trabalho

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Grava o ecrã num filme enquanto o ambiente de trabalho lho pedir. Não se
executa à mão: o ambiente de trabalho inicia-o quando começa uma gravação,
e termina quando a gravação termina.

O programa que o inicia indica como `SESSION` a sessão do ambiente de
trabalho para a qual grava.

Para gravar, prima `Meta+Shift+5` para abrir a barra de captura, escolha o
ecrã inteiro, uma janela ou uma área, e prima **Record** (Gravar). Um atalho
de captura, o indicador de gravação na área de notificações ou a duração
escolhida em **Options** (Opções) termina-a.

Uma gravação é guardada como `Screen Recording YYYY-MM-DD at HH.MM.SS.webm`
na pasta Desktop ou em `Home/Videos`, conforme escolhido em **Options**. É
um filme WebM: a imagem é vídeo VP8 até trinta fotogramas por segundo,
captados só quando algo muda no ecrã, e o som é Opus.

**Sound** (Som) em **Options** escolhe o que a gravação ouve: nada, o
microfone, o próprio som do ambiente de trabalho ou ambos misturados. A
gravação com som de um administrador começa de imediato. A de qualquer
outra conta pede primeiro a palavra-passe de um administrador, e o gravador
corre então como esse administrador para essa única gravação; cancelar não
grava nada, e um aviso indica-o.

Quando o disco fica cheio, a gravação termina aí e é guardada. Quando o
gravador falha, o que gravou é guardado e um aviso diz o que aconteceu.

## EXIT STATUS

Zero quando a gravação terminou, incluindo uma que terminou por o disco
ficar cheio. Diferente de zero quando não pôde começar ou terminou de
qualquer outra forma; o motivo é escrito na saída de erro padrão.

## SEE ALSO

`desktop`
