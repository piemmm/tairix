## NAME

files — navegador gráfico do sistema de ficheiros

## SYNOPSIS

`files [--desktop] [path] [-h | -?]`

## DESCRIPTION

Abre uma janela do ambiente de trabalho que lista o sistema de ficheiros,
a começar pela pasta que `path` indica na linha de comandos — ou, se
indicar um ficheiro, pela pasta que contém esse ficheiro, com o ficheiro
selecionado — ou, se nenhum for indicado, pela pasta que as definições
escolherem: a pasta `Home` do utilizador que o inicia, a menos que este
tenha escolhido a sua pasta `Desktop` ou o armazenamento ligado (o seu
diretório pessoal, se essa pasta não puder ser listada). O título da
janela indica o diretório atual; a janela lista as
suas entradas, cada entrada selecionada realçada com a cor de destaque do
tema ativo. Cada leitura de um diretório é uma listagem normal, sujeita a
permissões, com a identidade do utilizador que o inicia: um diretório
ilegível é recusado, nunca adivinhado.

O ambiente de trabalho inicia o explorador por si e mantém-no na barra de
ícones: o menu do seu lugar lista os seus próprios locais e tudo o que
estiver montado, e escolher um abre lá uma janela; *Definições…* é a
última linha. Um clique no lugar abre uma na pasta que as definições
escolherem. Pedir uma pasta que já tem uma janela traz
essa janela para a frente, em vez de abrir outra. Esta instância não tem a
linha *Sair* — faz parte do ambiente de trabalho, e fechar as suas janelas
apenas a arruma.

Iniciado pelo nome a partir de uma shell (ou aberto numa pasta a partir do
ambiente de trabalho), é antes uma aplicação normal: uma janela, que termina
quando a fecha. Em qualquer dos casos, precisa de uma sessão gráfica em
curso: sem ela, o canal de janelas fica inalcançável, e o explorador
comunica a recusa no fluxo de erro padrão e termina.

A janela é conduzida pelo teclado: `Baixo` e `Cima` movem a seleção, `Enter`
abre o diretório selecionado e `Backspace` sobe ao diretório superior. `F5`
volta a ler tanto a listagem como a coluna de locais; um volume acabado de
ligar aparece nela por si. `Ctrl+Shift+N` cria uma pasta nova.

Uma listagem abre sem nada selecionado. Um clique seleciona um item, um
clique com `Ctrl` acrescenta ou retira um, e um clique com `Shift` seleciona
a sequência a partir do último item escolhido; um clique num espaço vazio
limpa a seleção. Um duplo clique abre um item; mantendo `Shift` premido,
lista o conteúdo de uma aplicação em vez de a iniciar, e deixa
desselecionado tudo o que abre noutro lugar. Arrastar sobre um espaço vazio
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
de uma aplicação na barra de ícones é aberto lá.

O menu de contexto começa por *Visualização*. Por baixo, um submenu nomeia
aquilo sobre que o menu atua — *Ficheiro «notes.txt»*, um nome longo
encurtado ao meio para que a extensão continue visível, ou *Seleção* quando
há vários itens selecionados — e contém *Abrir*, *Abrir com…*, *Obter
informações…*, *Mudar o nome*, *Cortar*, *Copiar* e *Eliminar*; uma linha que
ali não pode atuar fica a cinzento e diz porquê. Seguem-se *Novo*, depois
*Selecionar tudo* e *Limpar seleção*, e por fim *Colar*. Um clique com o
botão direito num espaço vazio limpa a seleção, pelo que o submenu do item
fica a cinzento.

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

*Definições…* no menu do lugar na barra de ícones, ou `Ctrl+,`, abre as
definições do explorador: onde se abre uma janela nova, como mostra e
ordena os seus itens e se mostra a coluna de locais e a barra de
ferramentas; se as pastas ficam no topo; que extensões de nomes e que itens
ficam ocultos; se uma imagem mostra o seu próprio conteúdo e uma pasta o
que contém; e se mover itens para o Lixo pergunta primeiro. *Restaurar
predefinições* repõe todas as definições. Uma alteração aplica-se a todas
as janelas de uma vez e fica memorizada; uma janela já aberta mantém a
visualização e a ordem que tem.

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
linha que diz porquê.

`Esquerda` e `Direita` passam de uma secção da janela para outra. Em
*Permissões*, `Baixo` ou `Tab` entra nos seus controlos: as setas passam de
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

- `--desktop` — executar como o componente de gestor de ficheiros do próprio
  ambiente de trabalho: um lugar permanente na barra de ícones que oferece os
  seus locais e os volumes montados, nenhuma janela até que uma seja pedida,
  e nenhuma forma de sair. A sessão do ambiente de trabalho passa esta opção
  no arranque; nomear um `path` junto dela é recusado, porque um componente
  não abre nenhuma janela onde o pôr.
- `-h, -?` — mostrar a ajuda curta deste próprio comando e sair.

## EXIT STATUS

Zero após um fecho limpo, ou depois de mostrada a ajuda curta; `2`
quando a linha de comandos não foi compreendida; caso contrário,
diferente de zero quando o canal de janela, a região de fotogramas
partilhada ou a listagem inicial do diretório foi recusada (o motivo é
indicado no fluxo de erro padrão).
