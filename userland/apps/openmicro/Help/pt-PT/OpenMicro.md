## NAME

OpenMicro — Open Micro Emulator, emulador de microcomputadores de 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Executa microcomputadores de 8 bits em janelas do ambiente de trabalho — o
BBC Micro Model B e o Acorn Electron — ciclo a ciclo à sua própria velocidade,
cada um equipado com o seu próprio sistema operativo e BASIC escritos em sala
limpa e, no Model B, com um sistema de ficheiros em disco, de modo que uma
máquina arranca na linha de comandos do BASIC. A imagem é a de um televisor,
quatro unidades de largura por cada três de altura, e o som toca pelo áudio do
ambiente de trabalho.

A janela da biblioteca mostra cada máquina como um cartão com o seu último
ecrã. Nova cria uma máquina, com o nome pedido: um BBC Micro Model B, com um
novo disco em branco na unidade 0 ou sem nenhum, ou um Acorn Electron, com ou
sem um Plus 1 instalado. Iniciar, um duplo clique ou a tecla Return inicia a
máquina escolhida numa janela própria, ou traz a sua janela para a frente;
guardam-se até vinte máquinas, e várias funcionam ao mesmo tempo. Duplicar
copia uma máquina com o seu estado guardado e a sua pasta, e Eliminar elimina
uma máquina e o seu estado guardado depois de confirmado, deixando a sua pasta.
Um clique com o botão direito no cartão de uma máquina oferece Iniciar,
Definições, Duplicar e Eliminar para essa máquina e, enquanto funciona, também
o menu da máquina. Na primeira vez que é iniciado, o emulador cria um
BBC Micro Model B com um disco em branco.

Definições altera o nome de uma máquina, o seu volume e o carregamento rápido
de cassete, que põe a máquina a correr o mais depressa possível enquanto o
motor do gravador gira. O que está instalado nela só muda enquanto está
desligada: a ROM de cada encaixe — o OS do Model B e as suas dezasseis ranhuras
laterais, o OS e o BASIC do Electron, e a ROM e os cartuchos do seu Plus 1 —,
cada um com uma ROM incluída, RAM lateral numa ranhura do Model B, nada numa
ranhura ou cartucho, ou uma imagem de ROM sua, posta no armazém do emulador com
Instalar uma imagem; o Plus 1 do Electron; e os jumpers do teclado do Model B,
que definem o seu modo de ecrã no arranque, como um disco arranca e os tempos
de disco.

Fechar a janela de uma máquina suspende-a: iniciada de novo, retoma onde parou.
Encerrar, no seu menu, retira os seus discos e a sua cassete e desliga-a,
guardando apenas o seu último ecrã.

Cada máquina tem uma pasta própria, Open Micro e depois o nome da máquina, na
sua pasta Documents. Novo disco e Nova cassete criam aí um disco ou uma cassete
em branco e inserem-no, e Da pasta oferece os discos e as cassetes que já lá
estão. Os discos e as cassetes da sua pasta que as suas unidades e o seu
gravador contêm ficam com a máquina: se foi desligada, volta a inseri-los
quando arranca; se é retomada, só escreve de volta naqueles que nenhum outro
programa alterou desde que foi suspensa.

Uma imagem de disco (`.ssd`, `.dsd`) ou de cassete (`.uef`, `.csw`) aberta com
o emulador vai para a máquina ligada cuja janela esteve ativa por último, ou
para outra máquina ligada que a aceite, ou então para a primeira máquina da
biblioteca que a aceite, iniciada para a receber. Uma escolhida com
Inserir disco ou Inserir cassete entra nessa unidade ou no gravador, e uma
largada na janela de uma máquina entra na unidade 0 ou no gravador, protegida
contra escrita. O que a máquina escreve num disco volta ao ficheiro do disco
enquanto esse ficheiro puder ser escrito; um disco cujo ficheiro não pode ser
escrito fica protegido contra escrita. Ejetar retira um disco, e
Guardar disco como guarda uma cópia onde se quiser. Uma cassete em que se
gravou é guardada quando é retirada. Guardar instantâneo guarda a máquina
inteira num ficheiro, e Restaurar instantâneo recupera-a, com os seus discos e
a sua cassete.

O menu, aberto com um clique direito sobre a imagem, oferece cada unidade num
Model B e a cassete; BREAK, Repor, Pausa e a velocidade; ecrã inteiro, escala
nítida ou suave e linhas de varrimento; o mapeamento do teclado; Colar, que
escreve o texto da área de transferência; os instantâneos; Definições;
Biblioteca de máquinas; e Encerrar. Por carácter, as teclas escrevem o que
mostram, e o CAPS LOCK da máquina decide se uma letra é maiúscula. Por posição,
cada tecla é a tecla da máquina no seu lugar. Num Model B, F1 a F9 são f1 a f9,
F10 é f0, Home é SHIFT LOCK e Insert é a tecla da barra invertida. Num
Electron, F1 a F10 são FUNC com 1 a 9 e 0, Alt é FUNC e Caps Lock é CAPS LK. Em
ambos, F12 é BREAK, End é COPY e as setas são as teclas de cursor.

Luzes ao fundo da janela de uma máquina mostram CAPS LOCK e o motor do gravador
e, num Model B, SHIFT LOCK e cada unidade. Cada máquina corre num processo
auxiliar próprio sem qualquer autoridade, pelo que nem o seu software nem os
seus suportes são de confiança; um processo que falha é substituído, a máquina
recomeça como ao ligar e o motivo é mostrado.

Fechar a janela da biblioteca deixa as máquinas ligadas a funcionar, e fechar
todas as janelas deixa o emulador na barra de ícones: um clique no seu ícone,
ou Biblioteca de máquinas no seu menu, volta a abrir a biblioteca, e Sair nesse
menu suspende todas as máquinas ligadas e termina o emulador.

## OPTIONS

`-h`, `-?`, `--help`
: Escreve esta ajuda na saída padrão e termina.

## EXIT STATUS

Zero depois de Sair ser escolhido no menu do seu ícone na barra de ícones.
Diferente de zero quando o canal de janelas, a caixa de eventos ou a sessão do
ambiente de trabalho foram recusados; o motivo é indicado na saída de erro
padrão.
