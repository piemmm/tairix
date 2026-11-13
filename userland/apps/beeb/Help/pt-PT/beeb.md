## NAME

beeb — emulador do BBC Micro modelo B

## SYNOPSIS

`beeb`

## DESCRIPTION

Executa um BBC Micro modelo B numa janela do ambiente de trabalho, ciclo a
ciclo à sua própria velocidade, com o seu próprio MOS, BBC BASIC e DFS
escritos em sala limpa, de modo que arranca na linha de comandos do BASIC com
um sistema de ficheiros em disco. A imagem é a de um televisor, quatro
unidades de largura por cada três de altura, e o som toca pelo áudio do
ambiente de trabalho.

Uma imagem de disco (`.ssd`, `.dsd`) ou de cassete (`.uef`, `.csw`) aberta
com o emulador, largada na sua janela ou escolhida com Inserir disco ou
Inserir cassete no seu menu entra na unidade ou no gravador. O que a máquina
escreve num disco volta ao ficheiro do disco enquanto esse ficheiro puder ser
escrito; um disco cujo ficheiro não pode ser escrito fica protegido contra
escrita. Ejetar retira um disco, e Guardar disco como guarda uma cópia onde
se quiser. Uma cassete em que se gravou é guardada quando é retirada. Guardar
instantâneo guarda a máquina inteira num ficheiro, e Restaurar instantâneo
recupera-a, com os seus discos e a sua cassete.

O menu, aberto com um clique direito sobre a imagem ou a partir do ícone do
emulador na barra de ícones, oferece ainda BREAK, Repor, Pausa e a
velocidade; ecrã inteiro, escala nítida ou suave e linhas de varrimento; o
mapeamento do teclado; e Colar, que escreve o texto da área de transferência.
Por carácter, as teclas escrevem o que mostram, e o CAPS LOCK da máquina
decide se uma letra é maiúscula. Por posição, cada tecla é a tecla do BBC no
seu lugar: F1 a F9 são f1 a f9, F10 é f0, F12 é BREAK, End é COPY e Home é
SHIFT LOCK.

Definições escolhe o que contêm o encaixe do sistema e as dezasseis ranhuras
laterais — as ROM incluídas, RAM lateral ou imagens de ROM próprias,
instaladas no armazém do emulador —, o modo de ecrã, o arranque e os tempos
de disco dos jumpers do teclado, a imagem, as teclas, o volume e o
carregamento rápido de cassete, que põe a máquina a correr o mais depressa
possível enquanto o motor do gravador gira. As alterações às ROM e aos
jumpers têm efeito na reposição seguinte.

Luzes ao fundo da janela mostram CAPS LOCK, SHIFT LOCK, o motor do gravador e
cada unidade. A máquina corre num processo auxiliar sem qualquer autoridade,
pelo que nem o seu software nem os seus suportes são de confiança; um
processo que falha é substituído, a máquina recomeça como ao ligar e o motivo
é mostrado.

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de a sua janela ser fechada ou de Sair ser escolhido no menu do seu ícone na barra de ícones. Diferente de zero quando o canal de janelas, a caixa de eventos ou a sessão do ambiente de trabalho foram recusados; o motivo é indicado na saída de erro padrão.
