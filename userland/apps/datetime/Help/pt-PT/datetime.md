## NAME

datetime — a data, a hora e o fuso horário da máquina

## SYNOPSIS

`datetime`

## DESCRIPTION

Abre a janela **Date & Time** (Data e hora): o que o relógio da máquina
marca, de onde veio essa hora e os controlos que o acertam. Chega-se a
ela pelo menu do próprio relógio do ambiente de trabalho — clicar com o
botão direito no relógio da barra de ícones e escolher **Set Date &
Time…** (Definir data e hora…) — e, antes de a janela se abrir, o
ambiente de trabalho pede a palavra-passe de uma conta que possa alterar
estas definições.

O topo da janela mostra a hora ao segundo, a data e o fuso horário com a
sua abreviatura e o seu desvio em relação a UTC, tudo no fuso horário da
própria máquina.

A secção **Date and Time** (Data e hora) contém o relógio propriamente
dito.

- O interruptor **Set date and time automatically** (Definir data e hora
  automaticamente) mantém o relógio certo a partir de servidores de hora
  na rede. Por baixo dele, a janela indica de onde veio a hora do
  relógio: acertada a partir da rede, e quando; acertada à mão; lida do
  chip de relógio da placa quando a máquina arrancou; ou não acertada de
  todo. Ligar ou desligar o interruptor produz efeito de imediato.
- Enquanto o interruptor estiver ligado, o calendário e a hora são
  mostrados mas não podem ser alterados. Para acertar o relógio à mão, é
  preciso desligá-lo.
- Escolher um dia no calendário e escrever a hora ou ajustá-la passo a
  passo; depois, premir **Set** (Definir). As teclas de seta movem o dia
  escolhido um dia ou uma semana, `Page Up` e `Page Down` um mês e, com
  `Shift`, um ano; os botões ao lado do mês e do ano percorrem-nos sem
  mudar o dia. No campo da hora, `Tab` ou `:` passa das horas para os
  minutos e destes para os segundos, e `Up` e `Down` ajustam passo a
  passo a parte onde está o cursor. Num relógio de doze horas, a última
  parte é **AM** ou **PM**: clicar nela ou premir `A` ou `P`. `Enter`
  acerta o relógio.
- Enquanto não se escolher um dia nem se alterar a hora, o calendário e
  a hora acompanham o relógio à medida que este avança. A partir daí,
  mantêm o que se escolheu até ser definido ou até se fechar a janela.

A data e a hora que se definem são lidas no fuso horário da máquina. Uma
hora que os relógios saltam quando se adiantam é recusada, e a janela
indica quando os relógios mudam. Uma hora pela qual os relógios passam
duas vezes quando se atrasam dá lugar a uma pergunta: escolher se se
pretende a de antes da mudança ou a de depois. Uma data anterior à
criação deste sistema, ou mais de um século posterior a ela, só é
definida depois de confirmada, porque os certificados não seriam
considerados fiáveis e os ficheiros pareceriam mais antigos, ou mais
recentes, do que são.

Depois de o relógio ser acertado, o chip de relógio da placa é acertado
para a mesma hora, pelo que a máquina a mantém quando reinicia. Se a
máquina não tiver chip de relógio, a janela indica que a hora se perderá
no próximo reinício, a menos que a hora de rede esteja ligada.

A secção **Time Zone** (Fuso horário) indica o fuso que a máquina segue,
diz quando os seus relógios se adiantam ou atrasam da próxima vez e em
quanto, e oferece **Change…** (Alterar…), que abre uma lista de todos os
lugares que o sistema conhece: escrever parte do nome de uma cidade, de
uma região ou de um fuso para a restringir e, depois, escolher com
`Enter` ou com um duplo clique. Cada lugar mostra a hora atual nesse
lugar. Um novo fuso produz efeito de imediato.

O fuso da máquina é o que o ecrã de início de sessão mostra e o que é
usado por todas as contas que não escolheram um fuso próprio em
**Settings** (Definições).

Se a conta com que a janela é executada não puder fazer uma alteração, a
alteração é recusada, um alerta explica porquê e a janela mostra o que
está realmente em vigor. `Escape` ou **Close** (Fechar) fecha a janela.

## EXIT STATUS

Zero quando a janela é fechada, incluindo depois de uma alteração
recusada. Diferente de zero quando a janela não pôde ser aberta ou a sua
ligação ao ambiente de trabalho se perdeu; o motivo é indicado no fluxo
de erro padrão.

## SEE ALSO

`configure`
