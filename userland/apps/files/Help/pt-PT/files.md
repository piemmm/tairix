## NAME

files — navegador gráfico do sistema de ficheiros

## SYNOPSIS

`files --desktop [--mount] [path] [-h | -?]`

## DESCRIPTION

O gestor de ficheiros do próprio ambiente de trabalho, que lista o sistema
de ficheiros em janelas. Uma janela abre-se na pasta que lhe é pedida — ou,
para um ficheiro, na pasta que contém esse ficheiro, com o ficheiro
selecionado — ou, se nada for indicado, na pasta que as definições
escolherem: a pasta `Home` do utilizador que o inicia, a menos que este
tenha escolhido a sua pasta `Desktop` ou o armazenamento ligado (o seu
diretório pessoal, se essa pasta não puder ser listada). O título da
janela indica o diretório atual; a janela lista as
suas entradas, cada entrada selecionada realçada com a cor de destaque do
tema ativo. Cada leitura de um diretório é uma listagem normal, sujeita a
permissões, com a identidade do utilizador que o inicia: um diretório
ilegível é recusado, nunca adivinhado.

O ambiente de trabalho inicia o gestor de ficheiros com a sessão e mantém-no
na barra de ícones, e este nunca termina: o menu do seu lugar lista os seus
próprios locais e tudo o que estiver montado, e escolher um abre lá uma
janela; *Rede* vem a seguir, e *Lixo* vem por último:
abre o seu Lixo e, quando não há uma pasta pessoal onde o guardar, fica a
cinzento e diz porquê. Um volume que pode
ejetar — uma pen USB, um cartão de memória ou outro disco amovível, ou uma
imagem de disco ou partilha que montou — abre um submenu com *Ejetar*, e o
de um volume cujo título pode alterar — uma imagem de disco sua, ou um disco
cujo formato guarda um título — contém *Título*: esta linha abre um campo
com o título, pronto a alterar, e premir `Return` escreve o novo no disco;
um título vazio apaga-o. Quando não pode alterar por si próprio o título de
um disco, o ambiente de trabalho pede o nome e a palavra-passe de um
administrador, e altera-o com o seu consentimento. Um clique no lugar abre uma na pasta que
as definições escolherem. Pedir uma pasta que já tem uma
janela traz essa janela para a frente, em vez de abrir outra. O menu não tem
a linha *Sair* — o gestor de ficheiros faz parte do ambiente de trabalho,
fechar as suas janelas apenas o arruma e, se alguma vez parar, o ambiente de
trabalho volta a iniciá-lo.

*Rede*, no menu do lugar logo acima de *Lixo*, abre uma janela com os
computadores da rede local que partilham ficheiros. Cada um é listado com o
seu nome, o endereço em que é alcançado, o seu grupo de trabalho ou domínio
e aquilo que é, e a lista vai-se preenchendo à medida que os computadores
respondem. A rede só é percorrida enquanto uma janela a mostra. Um
computador que deixou de responder fica a cinzento até ser esquecido; um
nome que mistura alfabetos como precisa a imitação de outro nome mostra um
sinal de aviso em vez da sua imagem; e quando um computador oferece mais do
que algumas entradas, as restantes são contadas numa linha própria em vez de
afastarem as outras. Os nomes na rede são escolhidos por quem gere cada
computador, por isso um nome nunca prova que computador está a ver. A
ligação às partilhas de um computador não está disponível neste sistema, e
abrir um di-lo.

Só funciona tal como o ambiente de trabalho o inicia: iniciado de qualquer
outra forma — a partir de uma shell pelo seu caminho,
`/System/Components/files.app` — recusa-se a arrancar, indicando o motivo no
fluxo de erro padrão. Precisa de uma sessão gráfica em curso: sem ela, o
canal de janelas fica inalcançável, e comunica a recusa no fluxo de erro
padrão e termina.

A janela é conduzida pelo teclado: as setas `Down` e `Up` movem a seleção,
`Enter` abre o diretório selecionado e `Backspace` sobe ao diretório
superior. `F5` volta a ler tanto a listagem como a coluna de locais; um
volume acabado de ligar aparece nela por si. `Ctrl+Shift+N` cria uma pasta
nova.

Uma listagem abre sem nada selecionado. Um clique seleciona um item, um
clique com `Ctrl` acrescenta ou retira um, e um clique com `Shift` seleciona
a sequência a partir do último item escolhido; um clique num espaço vazio
limpa a seleção. Um duplo clique abre um item, e o que se abre noutro
lugar — um documento na sua aplicação, uma aplicação, uma imagem de disco
como volume — fica desselecionado depois de aberto, enquanto o que não
pôde ser aberto continua selecionado. Mantendo `Shift` premido, lista o
conteúdo de uma aplicação em vez de a iniciar. `Enter` abre a seleção e
mantém-na. Arrastar sobre um espaço vazio
desenha uma caixa que seleciona tudo o que toca à medida que cresce; mantida
no topo ou no fundo da listagem, esta desloca-se, e `Escape` desfaz o que a
caixa selecionou.

O espaço à volta de cada ícone e do seu nome conta como vazio, por isso uma
caixa pode começar em qualquer ponto entre os itens. `Ctrl+A` seleciona tudo
na listagem e `Ctrl+Shift+A` limpa a seleção; ambos estão também no menu de
contexto.

O nome de um item é mostrado por inteiro, em duas linhas quando delas
precisa; um nome demasiado longo mesmo para elas mantém o início e o fim,
com `…` no meio, para que a extensão esteja sempre à vista. Uma aplicação
é mostrada sem a sua extensão `.app`, e as definições podem mostrá-la, ou
ocultar também as extensões dos tipos de ficheiro que o sistema conhece.
Uma extensão continua à vista sempre que ocultá-la deixaria um nome que se
lê como outro tipo de ficheiro ou outra aplicação — `invoice.pdf.app` é
mostrado por inteiro — e uma janela ordena os seus itens pelos
nomes tal como são mostrados.
Os itens cujo nome começa por um ponto só são listados quando as
definições o pedem.

`F2` muda o nome do item selecionado no próprio lugar, tal como clicar no
nome do único item selecionado e esperar: o nome abre-se para edição assim
que o clique já não pode ser um duplo clique. Fica selecionada a parte antes
da extensão, por isso o que se escreve substitui o nome e mantém a extensão;
uma extensão que a janela não mostra também se mantém. `Enter`, ou um clique
fora do nome, mantém o nome novo — um nome que o volume recusa fica aberto
com o motivo — e `Escape` abandona-o. Um clique que mudou o nome do item não
faz mais nada, já que a pasta pode ter sido reordenada debaixo do ponteiro.

Arrastar os itens selecionados para outra janela do gestor de ficheiros,
para uma pasta nela ou para o ambiente de trabalho copia-os para lá;
mantendo `Shift` premido, move-os em vez disso. O ponteiro mostra um sinal
de mais enquanto largar copiaria e uma seta enquanto moveria, e a pasta onde
o largar iria parar fica realçada. Um único ficheiro arrastado para o lugar
de uma aplicação na barra de ícones é aberto lá. Uma unidade arrastada para
fora de `/Storage` nunca é copiada nem movida: deixa um atalho onde é
largada, e o ponteiro mostra uma seta curva. O atalho abre a unidade
enquanto estiver ligada; quando não estiver, abri-lo diz isso, e no ambiente
de trabalho é desenhado esbatido.
Copiar ou mover mantém os buracos de cada ficheiro e os seus atributos
estendidos, como um tipo de ficheiro RISC OS; qualquer atributo que o
destino não consiga guardar é indicado num aviso no fim da operação.

Se já houver lá um item com o mesmo nome, a própria janela da operação
pergunta o que fazer com ele: *Substituir* põe o novo no seu lugar, *Ignorar*
deixa o que já lá está e *Parar* termina a operação, mantendo o que já fez.
Uma pasta que encontra outra pasta com o mesmo nome oferece *Juntar* em vez de
*Substituir*: o que contém vai para a pasta que já lá está, e pergunta-se, um
a um, por cada ficheiro dentro dela que já lá esteja. Assinalar *Fazer o mesmo
para cada ficheiro que já exista* dá a mesma resposta a todas as perguntas
seguintes sobre um ficheiro. Quando uma pasta encontra uma pasta, a caixa diz
*Fazer o mesmo para cada pasta que já exista* e só responde às pastas
seguintes, pelo que se continua a perguntar por cada ficheiro dentro de uma
pasta juntada. `Escape` é *Parar*, e `Enter` dá a escolha em que o teclado
repousa — de início *Ignorar*; `Tab` passa da caixa às escolhas, e `Space`
assinala a caixa. No ambiente de trabalho, um item substituído fica onde foi
largado, enquanto um ignorado ou juntado fica onde estava.

O menu de contexto começa por *Visualização*. Por baixo, um submenu nomeia
aquilo sobre que o menu atua — *Ficheiro «notes.txt»*, um nome longo
encurtado ao meio para que a extensão continue visível, ou *Seleção* quando
há vários itens selecionados — e contém *Abrir*, *Abrir com…*, *Montar só de
leitura*, *Ejetar*, *Obter informações…*, *Mudar o nome*, *Cortar*, *Copiar*
e *Eliminar*; uma linha que ali não pode atuar fica a cinzento e diz porquê.
Seguem-se *Novo*, depois *Selecionar tudo* e *Limpar seleção*, e *Colar*.
No Lixo, *Esvaziar o Lixo* vem por último: assim que o confirmar, remove
definitivamente tudo o que está no Lixo. Um clique com o botão direito
num espaço vazio limpa a seleção,
pelo que o submenu do item fica a cinzento.

Abrir uma imagem de disco ou um arquivo — um ficheiro `.iso`, `.adf`,
`.hdf`, `.zip` e afins — monta-o para si em `/Storage`, gravável onde tanto
o ficheiro como o seu formato o permitem, e abre a sua janela;
*Montar só de leitura* monta-o, em vez disso, só de leitura. Abrir um no
ambiente de trabalho faz o mesmo. Quando já nenhuma janela o mostra, é
desmontado de novo, de modo que `/Storage` contém apenas o que está em uso;
se outra coisa ainda o estiver a usar — um documento dele aberto noutra
aplicação, por exemplo —, fica montado, e volta-se a tentar desmontá-lo
assim que isso o possa ter libertado. As definições podem, em vez disso,
manter montado o que abrir até o ejetar ou a sua sessão
terminar. *Ejetar* retira um volume que montou, ou um disco amovível
enquanto está a usar o próprio computador, a partir do seu ícone, da sua
linha na barra de locais, cujo menu de contexto próprio contém *Abrir* e
*Ejetar*, ou do menu do lugar na barra de ícones; as suas janelas nele
passam primeiro para `/Storage`. Um disco amovível com um ficheiro ainda
aberto não é ejetado, e é-lhe dito porquê. Quando não tem permissão para
ejetar um disco amovível — não é a pessoa que está ao computador —, o
ambiente de trabalho pede o nome e a palavra-passe de um administrador, e
ejeta-o com o seu consentimento. Um volume
que desapareceu — removido o seu disco, ou perdido o seu servidor — antes
de tudo o que nele escreveu ter sido guardado sai da barra, mas mantém o
seu ícone em `/Storage`, e o que não foi guardado perde-se; *Ejetar* nesse
ícone pergunta antes de o deixar ir.

O submenu *Novo* do menu de contexto cria uma pasta, ou um documento vazio
de cada tipo que um editor instalado escreve, e abre o seu nome para edição.

O submenu *Visualização* do menu de contexto mostra os itens como ícones
grandes, a predefinição; como ícones pequenos, cada nome numa linha ao lado
da sua imagem; ou como lista, uma linha por item com a sua imagem, o
tamanho, a data da última alteração e o tipo. Uma unidade listada entre
eles mostra, em vez disso, o espaço livre, o sistema de ficheiros e o
estado do seu funcionamento. Por baixo das visualizações, o submenu ordena
a janela por nome, tipo, tamanho ou data da última alteração — ou, numa
lista de unidades, por nome, espaço livre, sistema de ficheiros ou estado —
por ordem crescente ou decrescente. Um clique no título de uma coluna
também ordena a lista por ela, e um segundo clique inverte a ordem. As
pastas ficam no topo seja qual for a ordem escolhida, a menos que as
definições as coloquem entre os ficheiros.

`Ctrl+,` abre as
definições do explorador na aplicação Definições, em *Ficheiros*: onde se
abre uma janela nova, como as janelas mostram e ordenam os seus itens e se
uma nova mostra a coluna de locais e a barra de ferramentas; se as pastas
ficam no topo; que extensões de nomes e que itens ficam ocultos; se uma
imagem mostra o seu próprio conteúdo e uma pasta o que contém; se mover
itens para o Lixo pergunta primeiro; e se uma imagem de disco ou um arquivo
que abriu é desmontado depois de fechadas as suas janelas. Uma alteração
aplica-se a todas as janelas de uma vez e fica memorizada; uma janela mantém
uma visualização ou uma ordem que tenha escolhido nela pessoalmente.

`Alt+Enter` abre a janela de *informações* do item selecionado, tal como
*Obter informações…* no seu submenu do menu de contexto. É uma janela própria, pelo que
podem estar várias abertas ao mesmo tempo e a listagem continua utilizável
entretanto: mostra o que é o item, o seu tamanho, as suas marcas temporais,
para onde aponta um alias, as suas permissões e o seu proprietário, e os
atributos estendidos que o volume guarda para ele. Permissões, proprietário
e atributos podem ser alterados lá, cada um como uma escrita normal, sujeita
a permissões, com a sua própria identidade — uma recusa diz porquê e não
altera nada. Reatribuir um proprietário exige a capacidade `CAP_FS_CHOWN`;
uma sessão sem ela vê o proprietário e o grupo marcados com um cadeado e uma
linha que diz porquê. A janela de *informações* de uma unidade abre na sua
secção *Unidade*, cujo campo *Título* altera o título do volume da mesma
forma: clique nele ou prima `Tab`, escreva o novo título e prima `Return`;
`Escape` repõe-no.

As setas `Left` e `Right` passam de uma secção da janela para outra. Em
*Permissões*, `Down` ou `Tab` entra nos seus controlos: as setas passam de
um para outro, `Space` ativa ou desativa uma permissão ou abre o
proprietário ou o grupo para edição, e `Tab` ou `Escape` regressa às
secções.

O operando `path` é tratado como entrada não fidedigna: tem de ser um
caminho absoluto dentro do limite de comprimento de caminho do sistema,
e cada um dos seus componentes tem de ser um nome verdadeiro — `.` e
`..` não o são, pelo que uma escrita nunca pode significar outro lugar
que não aquele que se lê. Um caminho que infrinja alguma dessas regras,
ou que o utilizador que o iniciou não possa listar, é recusado com a
razão no fluxo de erro padrão e a janela abre-se antes na pasta `Home`,
para que um argumento errado nunca deixe o utilizador sem janela. Um
segundo operando é recusado de todo em vez de ignorado.

## OPTIONS

- `--desktop` — executar como o gestor de ficheiros do próprio ambiente de
  trabalho: um lugar permanente na barra de ícones que oferece os seus locais
  e os volumes montados, e nenhuma forma de sair. A sessão do ambiente de
  trabalho passa esta opção sempre que inicia o gestor de ficheiros, que só
  funciona com ela; um `path` junto dela abre lá uma janela.
- `--mount` — abrir `path` como imagem de disco ou arquivo, tal como acontece
  ao abri-lo numa janela: é montado para si e a sua janela abre-se, e é
  desmontado de novo depois de fechadas as suas janelas, conforme indicam
  as definições. Esta opção precisa de um `path`.
- `-h, -?` — mostrar a ajuda curta deste próprio comando e sair.

## EXIT STATUS

Não termina enquanto o ambiente de trabalho estiver em execução. Zero
depois de mostrada a ajuda curta; `2` quando a linha de comandos não foi
compreendida ou não indicou `--desktop`; caso contrário, diferente de zero
quando o canal de janela ou a região de fotogramas partilhada foi recusada
(o motivo é indicado no fluxo de erro padrão).
