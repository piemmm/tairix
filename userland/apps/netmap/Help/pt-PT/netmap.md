## NAME

netmap — mapa e inventário da rede

## SYNOPSIS

`netmap`

## DESCRIPTION

Abre uma janela que faz o mapa das redes a que esta máquina está ligada: as
suas interfaces de rede, as sub-redes em que estão, os anfitriões que a sua
cache de vizinhos viu em cada uma, os routers por onde passam as suas rotas e
o gateway predefinido que leva para lá da rede local. Cada elemento é
desenhado como um ícone num disco da cor daquilo que é — computadores a azul,
routers a laranja, impressoras a roxo, armazenamento a verde-azulado,
telefones a verde — e cada linha toma a cor daquilo a que leva. Um anfitrião
cujo sistema operativo o mapa reconhece usa o respetivo emblema, e uma luz
junto a um anfitrião ou interface mostra se estava alcançável da última vez
que foi visto (verde) ou não (vermelho).

Arraste com o botão principal, rode a roda ou percorra com dois dedos num
painel tátil para se deslocar pelo mapa. Mantenha `Ctrl` e rode a roda, ou
belisque num painel ou ecrã tátil, para ampliar em torno do ponto sob o
ponteiro; prima `Home` para que todo o mapa volte a caber na janela. Clique
num nó para ver o que se sabe sobre ele — os seus endereços, o endereço de
hardware e o seu fabricante, a alcançabilidade, o tipo de dispositivo que
parece ser e o que executa, com a certeza que o mapa tem e porquê — e clique
num espaço vazio para fechar os detalhes. Os nomes aparecem onde houver espaço
para eles.

O mapa é lido através da API de informação do sistema e do serviço de
descoberta, e os seus anfitriões recebem o nome dado pelos servidores DNS
configurados ou o que os próprios dispositivos se dão; nunca contacta ele
próprio um anfitrião, e é lido de novo a cada poucos segundos enquanto a sua
janela está aberta. Ao lado dos anfitriões mostra os serviços que os
dispositivos anunciam na ligação local — impressoras, partilhas de ficheiros,
leitores multimédia —, cada um sob o anfitrião que o oferece. Os endereços das
interfaces e as rotas exigem `CAP_SYSINFO_GLOBAL`; os endereços de hardware e
a cache de vizinhos exigem `CAP_SYSINFO_HW`; os serviços anunciados exigem
`CAP_NET_DISCOVER_ALL`; dar nome aos anfitriões por DNS exige `CAP_NET`. Sem
alguma delas, o mapa diz o que não pode mostrar e desenha o resto.

O netmap inicia-se a partir da Biblioteca de programas do ambiente de trabalho
ou pelo nome a partir de uma shell, e precisa de uma sessão gráfica em
execução; sem ela, comunica a recusa no fluxo de erro padrão e termina.

## EXIT STATUS

Zero após Sair; diferente de zero quando o canal da janela ou a região de
imagem partilhada foi recusado (o motivo é indicado no fluxo de erro padrão).
