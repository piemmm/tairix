## NAME

OpenMicro — Open Micro Emulator, emulador de microcomputadores de 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Executa microcomputadores de 8 bits em janelas do ambiente de trabalho — o
BBC Micro Model B, o Acorn Electron e o Commodore 64 — ciclo a ciclo à sua
própria velocidade, cada um equipado com o seu próprio sistema operativo e
BASIC escritos em sala limpa, no Model B com um sistema de ficheiros em disco
e no Commodore 64 com uma unidade de disquetes 1541 dotada do seu próprio DOS,
de modo que uma máquina arranca na linha de comandos do BASIC. A imagem é a de
um televisor, quatro unidades de largura por cada três de altura, e o som toca
pelo áudio do ambiente de trabalho.

A janela da biblioteca mostra cada máquina como um cartão com o seu último
ecrã ou, até ter funcionado, com o ecrã de arranque do seu modelo. Nova cria
uma máquina, com o nome pedido: um BBC Micro Model B, com um novo disco em
branco na unidade 0 ou sem nenhum; um Acorn Electron, com ou sem um Plus 1
instalado; ou um Commodore 64, PAL ou NTSC, com um SID 6581 ou 8580
e uma unidade 1541, duas ou nenhuma. Iniciar, um duplo clique ou a tecla
Return inicia a máquina escolhida numa janela própria, ou traz a sua janela
para a frente; guardam-se até vinte máquinas, e várias funcionam ao mesmo
tempo. Duplicar copia uma máquina com o seu estado guardado e a sua pasta, e
Eliminar elimina uma máquina e o seu estado guardado depois de confirmado,
deixando a sua pasta. Um clique com o botão direito no cartão de uma máquina
oferece Iniciar, Definições, Duplicar, Eliminar e Mostrar pasta para essa
máquina e, enquanto funciona, também o menu da máquina. Na primeira vez que é
iniciado, o emulador cria um BBC Micro Model B com um disco em branco.

Definições altera o nome de uma máquina, o seu volume, o carregamento rápido
de cassete, que põe a máquina a correr o mais depressa possível enquanto o
motor do gravador gira, e, num Commodore 64, a porta de controlo em que está o
seu joystick do teclado numérico. O que está instalado nela só muda enquanto
está desligada: a ROM de cada encaixe — o OS do Model B e as suas dezasseis
ranhuras laterais, o OS e o BASIC do Electron, a ROM e os cartuchos do seu
Plus 1 e, no Commodore 64, o KERNAL, o BASIC, os caracteres e o DOS de cada
unidade —, cada um com uma ROM incluída, RAM lateral numa ranhura do Model B,
nada numa ranhura ou cartucho, ou uma imagem de ROM sua, posta no armazém do
emulador com Instalar uma imagem; o Plus 1 do Electron; a norma de televisão,
o SID e as unidades do Commodore 64; e os jumpers do teclado do Model B, que
definem o seu modo de ecrã no arranque, como um disco arranca e os tempos de
disco.

Fechar a janela de uma máquina suspende-a: iniciada de novo, retoma onde
parou. Encerrar, no seu menu, retira os seus suportes e desliga-a, guardando
apenas o seu último ecrã.

Cada máquina tem uma pasta própria, Open Micro e depois o nome da máquina, na
sua pasta Documents. Novo disco, Nova disquete num Commodore 64, e
Nova cassete criam aí um disco, uma disquete ou uma cassete em branco e
inserem-no, Da pasta oferece os discos, as disquetes e as cassetes que já lá
estão, e Guardar instantâneo guarda aí a máquina inteira como um ficheiro
`.snapshot`. Mostrar pasta abre a pasta no Files, para que o que a máquina lá
guarda possa ser copiado para outros suportes ou para outro computador. Os
suportes da sua pasta que as suas unidades e o seu gravador contêm
ficam com a máquina: se foi desligada, volta a inseri-los quando arranca; se é
retomada, só escreve de volta naqueles que nenhum outro programa alterou desde
que foi suspensa.

Uma imagem de disco (`.ssd`, `.dsd`) ou de cassete (`.uef`, `.csw`), ou, para
um Commodore 64, de disquete (`.d64`, `.g64`), cassete (`.tap`),
cartucho (`.crt`) ou programa (`.prg`, `.t64`), aberta com o emulador vai para
a máquina ligada cuja janela esteve ativa por último, ou para outra máquina
ligada que a aceite, ou então para a primeira máquina da biblioteca que a
aceite, iniciada para a receber, e a janela dessa máquina vem para a frente.
Abrir um disco do BBC Micro no Files mostra os ficheiros que contém em vez de
iniciar uma máquina. Uma escolhida com Inserir disco, Inserir disquete ou
Inserir cassete entra nessa unidade ou no gravador, e uma largada na janela de
uma máquina entra na sua primeira unidade ou no gravador, protegida contra
escrita, e a janela fica com o teclado. O que a máquina escreve num disco ou
numa disquete volta ao seu ficheiro enquanto esse ficheiro puder ser escrito,
e, enquanto o suporte estiver na unidade, nem uma montagem nem outra máquina
podem escrever nesse ficheiro; um disco ou uma disquete cujo ficheiro não pode
ser escrito, ou que outro programa está a usar, fica protegido contra escrita,
e a janela diz porquê. Quando uma disquete `.d64` passa a precisar de um
`.g64`, oferece-se guardá-la como tal. Ejetar retira um disco ou uma disquete,
e Guardar disco como ou Guardar disquete como guarda uma cópia onde se quiser.
Uma cassete em que se gravou é guardada quando é retirada. Num Commodore 64,
Reproduzir, Gravar, Parar e Rebobinar, no menu Cassete, comandam o seu
datasette; Inserir cartucho encaixa um cartucho e Remover cartucho retira-o,
ambos reiniciando a máquina; e Carregar programa põe um programa na memória,
como faria LOAD, para que RUN o inicie. Guardar instantâneo guarda a máquina
inteira num ficheiro — na sua pasta ou, antes de ter uma, onde escolher —, e
Restaurar instantâneo recupera uma máquina assim guardada, com os seus
suportes.

O menu, aberto com um clique direito sobre a imagem, oferece cada unidade de
um Model B ou de um Commodore 64 e a cassete, e o cartucho e Carregar programa
de um Commodore 64; BREAK, ou RESTORE num Commodore 64; num Model B e num
Electron, SHIFT-BREAK, que prime BREAK com SHIFT premido para arrancar a
partir do disco ou da cassete; Repor, Pausa e a velocidade; ecrã inteiro,
escala nítida ou suave e linhas de varrimento; o mapeamento do teclado; Colar,
que escreve o texto da área de transferência, cada letra em maiúscula ou
minúscula conforme a máquina a mostra; os instantâneos; Mostrar pasta;
Definições; Biblioteca de máquinas; e Encerrar. Por carácter, as teclas
escrevem o que mostram, e o CAPS LOCK da máquina, ou num Commodore 64 o Shift,
decide se uma letra é maiúscula. Por posição, cada tecla é a tecla da máquina
no seu lugar. Num Model B, F1 a F9 são f1 a f9, F10 é f0, Home é SHIFT LOCK e
Insert é a tecla da barra invertida. Num Electron, F1 a F10 são FUNC com 1 a 9
e 0, Alt é FUNC e Caps Lock é CAPS LK. Em ambos, F12 é BREAK, End é COPY e as
setas são as teclas de cursor. Num Commodore 64, F1, F3, F5 e F7 são as suas
teclas de função e F2, F4, F6 e F8 as mesmas com SHIFT, Escape é RUN/STOP, Alt
é a tecla Commodore, Ctrl é CTRL, Home é CLR/HOME, F12 ou Page Up é RESTORE e
as setas são as teclas de cursor; com o seu joystick do teclado numérico
ligado, as setas do teclado numérico movem o joystick e o 0 e o 5 disparam.

Luzes ao fundo da janela de uma máquina mostram CAPS LOCK e o motor do
gravador e, num Model B, SHIFT LOCK e cada unidade; num Commodore 64 mostram o
motor do datasette e cada unidade. Num Model B e num Electron, o botão
SHIFT-BREAK, ao fundo à direita, faz o que faz a linha do menu. Cada máquina
corre num processo auxiliar próprio sem qualquer autoridade, pelo que nem o
seu software nem os seus suportes são de confiança; um processo que falha é
substituído, a máquina recomeça como ao ligar e o motivo é mostrado.

Fechar a janela da biblioteca deixa as máquinas ligadas a funcionar, e fechar
todas as janelas deixa o emulador na barra de ícones: um clique no seu ícone,
ou Biblioteca de máquinas no seu menu, traz sempre a biblioteca para a frente,
e Sair nesse menu suspende todas as máquinas ligadas e termina o emulador.

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair ser escolhido no menu do seu ícone na barra de ícones.
Diferente de zero quando o canal de janelas, a caixa de eventos ou a sessão do
ambiente de trabalho foram recusados; o motivo é indicado na saída de erro
padrão.
