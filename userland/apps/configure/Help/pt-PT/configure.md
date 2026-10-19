## NAME

configure — ler e alterar as definições da máquina

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Lista, mostra e altera as definições da máquina, guardadas em
`/System/Settings/Configuration/system.conf`. Sem operandos, cada
definição é listada com o seu valor atual; com uma chave apenas, o seu
valor é mostrado; com um ou mais pares `<key> <value>`, essas definições
são alteradas em conjunto.

Vários pares são uma única alteração, e não várias: cada par é
verificado antes de qualquer alteração, e as definições são depois
guardadas de uma só vez. Uma linha de comandos que indique uma chave
desconhecida, um valor fora do conjunto da sua chave ou a mesma chave
duas vezes não altera rigorosamente nada, pelo que um grupo de
definições nunca pode ficar aplicado a meio.

Só um administrador pode alterar uma definição: uma conta comum lê as
definições da máquina mas não altera nenhuma. O sistema traz definições
próprias, que vigoram até que um administrador altere alguma, e a
primeira alteração mantém todas as outras definições que o sistema
trouxe. Uma alteração produz efeito da próxima vez que arrancar a parte
do sistema que a lê (`os.loginType` no próximo início de sessão, os
comutadores `cache.*` no próximo arranque), salvo indicação em contrário
na sua chave, abaixo.

O conjunto de chaves é fechado: uma chave desconhecida, ou um valor
fora do conjunto de uma chave, é recusado com a indicação das escolhas
válidas e nada altera.

- `os.loginType` — `text` ou `graphical`: o tipo de sessão que o
  serviço de início de sessão inicia para um utilizador autenticado.
  `graphical` (a omissão) inicia diretamente a sessão de ambiente de
  trabalho após a autenticação, recuando para o início de sessão em
  texto numa máquina que não consegue executar uma; `text` inicia a
  shell da conta — o ambiente de trabalho só é executado a partir de um
  início de sessão gráfico.
- `cache.all` — `on` ou `off`: o comutador principal da cache. `on` (a
  omissão) deixa cada classe de cache abaixo seguir a sua própria
  opção; `off` é um teto que desativa toda a cache em memória
  independentemente das opções por classe.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` ou `off`: os comutadores por classe para as
  quatro caches de memória recuperáveis (as caches do sistema de
  ficheiros, do bloco de disco inteiro, do cluster descomprimido e do
  arranque de aplicações). `auto` (a omissão) deixa o gestor de pressão
  de memória governar a classe; `off` desativa-a por completo. Não há
  um `on` por classe: uma classe não pode ser forçada a ignorar a
  pressão de memória. Uma classe está efetivamente `off` sempre que
  `cache.all` estiver `off`.

Cada cache é um acelerador recuperável, nunca a fonte da verdade, por
isso desligar qualquer uma ou todas apenas torna mais lento o trabalho
afetado — nunca altera um resultado.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` ou `false`: os
  interruptores das famílias de endereços a nível da pilha. Ambos são
  `true` por predefinição. Uma família desativada não vincula
  endereços, não responde a pacotes e recusa um socket dessa família
  com um erro tipado — nunca um descarte silencioso.
- `net.ipv6.privacy` — `true` ou `false`: se a pilha forma endereços
  IPv6 temporários (de privacidade) além do estável. `false` (a
  predefinição) usa apenas o endereço SLAAC estável.
- `net.tcp.syncookies` — `auto` ou `always`: a defesa contra
  inundações SYN. `auto` (a predefinição) mantém uma fila semiaberta
  limitada e recorre a cookies sem estado em caso de transbordo;
  `always` responde a cada pedido de ligação sem estado. Não há `off`
  — uma fila de ligações indefesa não é uma definição.
- `net.tcp.keepalive` — `true` ou `false`: se as ligações TCP enviam
  sondas de manutenção numa ligação inativa. `false` (a predefinição)
  nunca sonda nem fecha uma ligação inativa; `true` sonda um par
  inativo após o intervalo habitual e fecha a ligação se este deixar de
  responder.
- `net.tcp.ecn` — `true` ou `false`: se as ligações TCP negoceiam a
  notificação explícita de congestão (ECN). `false` (a predefinição)
  deixa as ligações Not-ECT; `true` oferece ECN no aperto de mão e, a
  seguir, trata uma marca de congestão como um sinal para abrandar em
  vez de forçar a perda de um pacote.
- `net.sockets.mem` — `auto` ou um tamanho em bytes como `64M`: a
  memória que a pilha de rede pode manter em estado de sockets entre todos
  os principais. `auto` (a predefinição) dimensiona-a pela RAM da máquina,
  para que um servidor grande não fique preso a um número escolhido num
  pequeno; um tamanho substitui-o para uma carga que conhece melhor.
  Cada principal pode ter um dezasseis avos do orçamento efetivo, pelo
  que há sempre lugar para dezasseis. Bytes e não um número de sockets,
  porque o mesmo número de sockets são alguns kilobytes em repouso e
  megabytes com os buffers cheios: o orçamento leva muitas ligações
  tranquilas ou menos ligações ativas, conforme a carga realmente é.
- `time.servers` — `none` ou uma lista de servidores de hora de rede
  separada por vírgulas, cada um um nome de anfitrião ou um endereço.
  `none` (a predefinição) não indica nenhum servidor próprio: o relógio é
  então acertado a partir do servidor que a concessão de endereço da rede
  indica, ou senão do conjunto público `pool.ntp.org`. Os servidores
  indicados aqui têm precedência sobre ambos.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` ou `7d`: quanto tempo de
  funcionamento passa entre consultas ao relógio depois de a hora ser
  conhecida. `1d` é a predefinição. Um relógio não acertado, implausível
  ou muito desatualizado é corrigido assim que a rede o permita,
  independentemente desta definição.
- `time.network` — `on` (a predefinição) ou `off`: se o relógio é
  acertado a partir da rede. Com `off` não sai nenhum pedido de hora para
  nenhum servidor, e o relógio é acertado à mão ou a partir do chip de
  relógio da placa, lido em cada arranque de qualquer forma. Uma alteração
  aplica-se de imediato; a definição fica no disco do sistema, pelo que no
  início de um arranque vale a predefinição até esse disco ser alcançado.
- `time.zone` — `default` ou o nome de um fuso horário como
  `Europe/London`: o fuso horário da máquina, o que o ecrã de início de
  sessão mostra e o que usa uma conta que não escolheu nenhum. `default`
  é o fuso que o sistema traz como seu; um nome que a lista de fusos do
  sistema não contenha é ignorado como se fosse `default`. Uma alteração
  aplica-se de imediato.
- `input.mouse.debounce` — milissegundos inteiros, `25` por omissão, `0` para
  desativar, no máximo `100`: quanto tempo depois de largar um botão do rato a
  premência seguinte do mesmo botão é ignorada como ressalto do interruptor em
  vez de ser tratada como um novo clique. Um interruptor gasto pode comunicar
  uma segunda premência poucos milissegundos depois de largar quando
  pretendia um só clique. Ponha `0` num rato cujo modo de tiro rápido envia
  pares de cliques de propósito.
- `audio.output`, `audio.input` — `auto` por omissão, ou a localização de
  um terminal tal como o `audioctl` a mostra (dezasseis dígitos
  hexadecimais, um ponto e o índice do terminal): a saída e a entrada que
  esta máquina prefere por omissão. A escolha própria de uma sessão vem
  primeiro; esta é a escolha antes de qualquer início de sessão e numa
  máquina sem ambiente de trabalho. `auto` é o primeiro dispositivo
  encontrado.
- `audio.level` — em decibéis, `0dB` por omissão, ou uma atenuação como
  `-12dB` ou `-6.5dB`, com duas casas decimais no máximo: o nível com que
  começa cada saída e entrada. Nunca acima de `0dB`. O serviço de áudio
  adota um valor alterado no arranque seguinte; o `audioctl` altera a
  máquina em funcionamento.

Uma chave que não esteja na lista acima é procurada num segundo registo:
as definições de rede de cada interface, guardadas em
`/System/Settings/Network/network.conf`. As suas chaves escrevem-se
`<interface>.<setting>`, em que `<interface>` é o alias que um
administrador deu a uma interface de rede (`wan`, `lan0`) e `<setting>`
é um de `kind`, `match.mac`, `match.node`, `ipv4.method`,
`ipv4.address`, `ipv4.gateway`, `ipv6.method`, `ipv6.address`,
`ipv6.gateway`, `dns.servers`, `mtu`, `bond.members`, `bond.mode`,
`bond.monitor-interval` e `bond.primary`.

O registo da máquina acima é sempre consultado primeiro, pelo que um
alias de interface nunca pode apoderar-se do nome de uma definição da
máquina.

As definições de rede não têm predefinições, pelo que a listagem mostra
apenas as definições existentes, e mostrar uma inexistente responde com
uma linha vazia em vez de inventar um valor. Só um administrador pode
lê-las ou alterá-las: indicam o hardware de cada interface e os
endereços desta máquina. Para qualquer outra conta, a listagem mostra as
definições da máquina e avisa que as de rede não são mostradas.

Como esse registo não tem predefinições, uma definição é removida em vez
de reposta, e é com um **valor vazio** que se remove:
`configure wan.ipv4.address ""`. Nenhuma definição de interface aceita
um valor vazio, pelo que esta escrita nunca pode ser confundida com a
atribuição de um valor. É isto que permite a um só comando passar uma
interface de um endereço estático para DHCP —
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— uma alteração da qual nenhuma das metades, sozinha, descreve uma
configuração coerente. Uma interface cuja última definição é removida
deixa de estar declarada de todo.

Toda a configuração editada é verificada antes de se guardar o que quer
que seja: uma alteração que a deixaria incoerente (um método estático
sem endereço, uma agregação de ligações com menos de dois membros) é
recusada e nada é guardado.

Alterar uma definição `net.*`, ou uma de interface, guarda-a e entrega-a
à pilha de rede em funcionamento, pelo que produz efeito de imediato. Só
são entregues as interfaces que a alteração realmente afetou. Se a pilha
em funcionamento não aceitar uma entrega — nenhuma está em
funcionamento, ou a sua conta não pode administrar a rede —, a definição
é guardada mesmo assim e o `configure` indica-o; aplica-se então no
próximo arranque.

Dois limites são comunicados em vez de ocultados. Uma interface que
removeu da configuração continua a funcionar até ao próximo arranque com
a configuração que a pilha recebeu por último, porque a pilha não tem
nenhuma mensagem que retire uma interface. Uma interface guardada sem
`match.mac` nem `match.node` nunca pode ser associada a um dispositivo,
pelo que é guardada e a recusa é indicada.

## OPTIONS

- `-h, -?` — mostrar a ajuda breve deste comando.

## EXAMPLES

- `configure` — listar todas as opções.
- `configure os.loginType` — mostrar o tipo de sessão por omissão.
- `configure os.loginType graphical` — arrancar no início de sessão
  gráfico.
- `configure cache.all off` — desativar toda a cache em memória em todo
  o sistema.
- `configure cache.filesystem off` — desativar apenas a cache do
  sistema de ficheiros.
- `configure net.ipv6.enabled false` — desativar o IPv6 em toda a pilha.
- `configure time.servers 0.example.test,1.example.test` — definir os
  servidores de hora de rede com que o relógio é sincronizado.
- `configure time.zone Europe/London` — pôr a máquina na hora de
  Londres.
- `configure wan.ipv4.address` — mostrar o endereço IPv4 estático
  configurado para a interface chamada `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  dar à interface `wan` um endereço IPv4 estático e um gateway
  predefinido.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — passar a interface `wan` do endereçamento estático para DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — definir os
  servidores DNS recursivos a usar na interface `wan`.

## EXIT STATUS

- `0` — a listagem, o valor, a ajuda breve ou a alteração foi
  concluída.
- `1` — as definições não puderam ser lidas ou alteradas (por exemplo,
  quem chama não pode alterar as definições da máquina, ou não pode ler
  as suas definições de rede), ou a saída não pôde ser entregue.
- `2` — a linha de comandos não foi compreendida, a chave é
  desconhecida, o valor está fora do conjunto da chave, ou a alteração
  deixaria incoerente a configuração de rede.

## ENVIRONMENT

- `LANG` — o idioma preferido da ajuda breve (uma etiqueta BCP-47 como
  `fr-FR`).

## SEE ALSO

- `man`
