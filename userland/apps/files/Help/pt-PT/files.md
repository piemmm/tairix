## NAME

files — navegador gráfico do sistema de ficheiros

## SYNOPSIS

`files [--desktop] [directory] [-h | -?]`

## DESCRIPTION

Abre uma janela do ambiente de trabalho que lista o sistema de ficheiros, a
começar pelo `directory` indicado na linha de comandos ou, se nenhum for
indicado, pela pasta `UserFiles` do utilizador que o inicia (o seu diretório
pessoal, se essa pasta não puder ser listada). O título da janela indica o
diretório atual; a janela lista as suas entradas, cada entrada selecionada
realçada com a cor de destaque do tema ativo. Cada leitura de um diretório é
uma listagem normal, sujeita a permissões, com a identidade do utilizador
que o inicia: um diretório ilegível é recusado, nunca adivinhado.

O ambiente de trabalho inicia o explorador por si e mantém-no na barra de
ícones: o menu do seu lugar lista os seus próprios locais e tudo o que
estiver montado, e escolher um abre lá uma janela. Um clique no lugar abre
uma na sua pasta `UserFiles`. Pedir uma pasta que já tem uma janela traz
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
limpa a seleção. Arrastar sobre um espaço vazio desenha uma caixa que
seleciona tudo o que toca à medida que cresce; mantida no topo ou no fundo
da listagem, esta desloca-se, e `Escape` desfaz o que a caixa selecionou.

Arrastar os itens selecionados para outra janela do gestor de ficheiros,
para uma pasta nela ou para o ambiente de trabalho copia-os para lá;
mantendo `Shift` premido, move-os em vez disso. O ponteiro mostra um sinal
de mais enquanto largar copiaria e uma seta enquanto moveria, e a pasta onde
o largar iria parar fica realçada. Um único ficheiro arrastado para o lugar
de uma aplicação na barra de ícones é aberto lá.

O submenu *Novo* do menu de contexto cria uma pasta, ou um documento vazio
de cada tipo que um editor instalado escreve, e abre o seu nome para edição.

`Alt+Enter` abre uma janela de *Propriedades* do item selecionado, tal como
a linha *Propriedades* do menu de contexto. É uma janela própria, pelo que
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

O operando `directory` é tratado como entrada não fidedigna: tem de ser
um caminho absoluto dentro do limite de comprimento de caminho do
sistema, e cada um dos seus componentes tem de ser um nome de diretório
verdadeiro — `.` e `..` não o são, pelo que uma escrita nunca pode
significar outro lugar que não aquele que se lê. Um diretório que
infrinja alguma dessas regras, ou que o utilizador que a lançou não
possa listar, é recusado com a razão no fluxo de erro padrão e a janela
abre-se antes na pasta `UserFiles`, para que um argumento errado nunca
deixe o utilizador sem janela. Um segundo operando é recusado de todo
em vez de ignorado.

## OPTIONS

- `--desktop` — executar como o componente de gestor de ficheiros do próprio
  ambiente de trabalho: um lugar permanente na barra de ícones que oferece os
  seus locais e os volumes montados, nenhuma janela até que uma seja pedida,
  e nenhuma forma de sair. A sessão do ambiente de trabalho passa esta opção
  no arranque; nomear um `directory` junto dela é recusado, porque um componente
  não abre nenhuma janela onde o pôr.
- `-h, -?` — mostrar a ajuda curta deste próprio comando e sair.

## EXIT STATUS

Zero após um fecho limpo, ou depois de mostrada a ajuda curta; `2`
quando a linha de comandos não foi compreendida; caso contrário,
diferente de zero quando o canal de janela, a região de fotogramas
partilhada ou a listagem inicial do diretório foi recusada (o motivo é
indicado no fluxo de erro padrão).
