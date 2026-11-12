## NAME

PCEmulator — emulador de máquinas IBM PC e da classe XT

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Executa PC da época do 8088 ao 80186 em janelas do ambiente de trabalho: de
cada máquina, o processador, a placa gráfica, os discos, o som e as portas, à
velocidade que a máquina tinha.

A janela da biblioteca mostra cada máquina como um cartão com o seu último
ecrã. Nova cria uma máquina a partir de um de quatro modelos — o IBM PC, o IBM
PC XT, um XT turbo e uma estação de trabalho 80186 — com o nome pedido e, num
modelo com controlador de disco rígido, com um disco rígido de 30 MB que
arranca o TAIRiX DOS. Iniciar, ou um duplo clique, inicia a máquina escolhida
numa janela própria, ou traz a sua janela para a frente; várias máquinas
funcionam ao mesmo tempo. Definições altera o hardware de uma máquina enquanto
está desligada, Duplicar copia uma máquina com os seus discos, e Eliminar
elimina uma com os seus discos depois de confirmado. Um clique com o botão
direito no cartão de uma máquina oferece estas opções para essa máquina e,
enquanto funciona, também o menu da máquina. Na primeira vez que é
iniciado, o PCEmulator cria um XT turbo. O que uma máquina escreve nos seus
discos, e a hora a que o seu relógio está acertado, mantêm-se de uma execução
para a seguinte.

O XT turbo e a estação de trabalho 80186 têm um coprocessador numérico 8087, e
Definições instala um em qualquer máquina ou retira-o. Um programa que espera
pelo coprocessador sem antes verificar que existe um para numa máquina que não
o tem, como acontecia no hardware real.

A imagem é a do monitor, quatro unidades de largura por cada três de altura, e
cada ponto da placa gráfica mantém as arestas nítidas, seja qual for o tamanho
da janela. Fechar a janela de uma máquina suspende-a: iniciada de novo, retoma
onde parou, com as disquetes fora das unidades e o relógio adiantado pelo
tempo em que esteve ausente. O hardware de uma máquina suspensa muda-se depois
de a iniciar e encerrar.

No fundo da janela de uma máquina, cada uma das suas unidades tem uma luz
que acende enquanto a unidade trabalha. Premir aí uma unidade de disquetes
oferece Inserir disco, que escolhe uma imagem de disquete para a unidade, e
Ejetar. Uma imagem de disquete largada sobre a janela entra na unidade sobre
a qual é largada, ou então na primeira, e aí não pode ser escrita.

No fim dessa barra, o botão Máquina abre o menu da máquina, que a põe em
pausa, a reinicia, lhe envia Ctrl+Alt+Delete e define a velocidade do seu
processador durante a execução.
Visualização liga e desliga as linhas de varrimento, o brilho, a persistência
do fósforo, o ruído e a ondulação da imagem, e a máquina guarda o que for
escolhido. Fazer captura de ecrã guarda o ecrã da máquina como imagem PNG onde
o painel Guardar indicar. Ecrã inteiro enche o ecrã com a janela e deixa a
barra no lugar, onde o mesmo menu devolve o ecrã. Encerrar desliga a máquina e
guarda apenas o seu último ecrã.

Copiar o texto do ecrã põe na área de transferência o texto que o ecrã da
máquina mostra, enquanto mostra texto. Colar escreve o texto da área de
transferência com o teclado da máquina, cada carácter depois de o programa ter
lido o anterior. O que a máquina não consegue escrever tal como é — as caras e
as setas do PC, ou uma letra que não conhece — fica de fora, e a janela di-lo.
Parar de colar, ou uma tecla premida na janela da máquina, para a escrita.

Partilhar uma pasta empresta à máquina uma pasta sua, escolhida no seletor,
como uma unidade própria: a primeira letra que os seus discos deixam livre,
depois de o TAIRiX DOS arrancar. Um nome que o DOS não consegue conter é
encurtado, como o Windows encurta um nome longo, e uma pasta partilhada só
para leitura, ou um ficheiro que não pode alterar, não pode ser alterado a
partir da máquina. Cada pasta partilhada tem um lugar na barra que indica a
sua unidade; premi-lo oferece Deixar de partilhar. Uma máquina partilha até
quatro pastas, e nenhuma depois de suspensa ou desligada.

Depurador, no menu da máquina, abre uma janela sobre a máquina: os seus
registos, o seu 8087, o seu código e a sua memória tal como estavam quando
parou, e os seus pontos de paragem. Interromper para a máquina onde está;
Continuar, Passo, Passar por cima e Sair do procedimento põem-na de novo a
correr, tal como F5, F11, F10 e Shift+F11, e F5 também para uma máquina que
corre. Premir ao lado de uma linha de código coloca aí um ponto de paragem ou
retira-o, tal como F9 faz para a linha atual, e Adicionar ponto de paragem
para antes a máquina quando escreve ou lê a memória, lê ou escreve uma porta,
ou entra numa interrupção. O tempo da máquina fica parado enquanto ela está
parada, e fechar o depurador deixa-a continuar.

Enquanto a janela de uma máquina tem o teclado, cada tecla chega à máquina
como uma tecla do seu próprio teclado, incluindo Ctrl, Alt e as teclas de
bloqueio; o ambiente de trabalho reserva Ctrl+Alt+Delete, os seus atalhos de
captura e a tecla Meta. Um programa que usa o rato através do controlador de
rato do DOS segue o ponteiro do ambiente de trabalho onde quer que esteja
sobre a imagem. Um programa que lê o rato por si próprio fica com ele quando
se clica na imagem e devolve-o quando se toca brevemente na tecla Meta.

Fechar a janela da biblioteca deixa as máquinas ligadas a funcionar, e fechar
todas as janelas deixa o emulador na barra de ícones: um clique no seu ícone, ou
Biblioteca de máquinas no seu menu, volta a abrir a biblioteca, e Sair nesse
menu suspende todas as máquinas ligadas e termina o emulador. Cada
máquina corre num processo de trabalho próprio que não detém qualquer
autoridade: nunca se confia nos seus programas nem nos seus discos.

## OPTIONS

`-h`, `-?`, `--help`
: Escrever esta ajuda na saída padrão e sair.

## EXIT STATUS

Zero depois de se escolher Sair no seu menu da barra de ícones. Diferente de
zero quando o canal da janela, a caixa de eventos ou a sessão do ambiente de
trabalho foram recusados; o motivo é indicado na saída de erro padrão.
