## NAME

settings — configurar o ambiente de trabalho e esta máquina

## SYNOPSIS

`settings`

## DESCRIPTION

Abre uma janela do ambiente de trabalho que lista cada categoria de definições
deste sistema: o que a máquina é, o aspeto do ambiente de trabalho, o ecrã, a
rede, os dispositivos com que é conduzida, as contas que a usam e os volumes
que contém. Escolher uma categoria na barra lateral mostra o seu painel. Uma
categoria que contém vários painéis abre e fecha antes a sua lista no próprio
lugar, e qualquer número dessas listas pode ficar aberto ao mesmo tempo.

O Settings não detém autoridade própria. Cada alteração é ou um pedido à sessão
do ambiente de trabalho, dona das definições do utilizador, ou uma execução
reautenticada do comando que já escreve esse armazém; nada aqui pode elevar um
privilégio.

Uma categoria que este sistema não consegue servir di-lo claramente e nomeia o
que teria de existir para o conseguir. Um controlo que nada alteraria nunca é
mostrado.

Escreva no campo de pesquisa acima da barra lateral para a filtrar às
categorias e definições que uma palavra alcança. `Tab` e `Shift+Tab` movem o
foco entre o campo de pesquisa, o percurso, a barra lateral e o painel; `Up` e
`Down` percorrem a barra lateral e `Enter` abre a linha. `Right` e `Left` abrem
e fecham a lista da categoria sob o cursor. Uma janela demasiado estreita
abandona a barra lateral, e a primeira migalha do percurso lista então as
categorias.

É lançado a partir da linha *Settings…* do menu de sistema do ambiente de
trabalho, ou a partir de uma shell pelo seu caminho,
`/System/Components/settings.app`. Exige uma sessão gráfica em execução: sem
ela o canal de janela é inalcançável e comunica a recusa no fluxo de erro
padrão e termina.

Som mostra cada saída e entrada com a sua escolha predefinida, o seu nível e o
seu silêncio, e o que está a gravar: as suas próprias gravações pelo nome e
quantas fazem os outros. Os controlos mudam o dispositivo de imediato, e só
enquanto a sua sessão ocupa a sala que ele serve; um dispositivo da sala de
outra sessão é mostrado mas não pode ser alterado aqui. O seu ambiente de
trabalho recorda o que define.

**Raspberry Pi** só aparece quando a máquina é um. Define a ventoinha do
processador, comutada através de uma linha do conector de 40 pinos: *Off*, *On*
sempre ou *Automatic*, que liga a ventoinha quando o processador atinge a
temperatura escolhida e só a desliga depois de ele arrefecer a margem escolhida,
para que não ligue e desligue em torno de um só grau. A linha de controlo
escolhe-se pelo número GPIO e pelo pino, bem como o nível, alto ou baixo, em que
a ventoinha funciona; uma linha já usada por outra coisa é indicada em vez de
oferecida. Uma linha do conector não consegue alimentar uma ventoinha sozinha,
por isso ligue-a através de um transístor. O painel mostra também a temperatura
do processador e se a ventoinha está a funcionar. Quando *Automatic* não
consegue ler a temperatura, a ventoinha funciona até conseguir. Tal como as
outras definições da máquina, uma alteração espera por *Apply*, que pede uma
conta autorizada a alterá-las.

**Storage** lista cada volume montado, exceto os que outro utilizador mantém
privados: onde está montado, o seu sistema de ficheiros, o seu dispositivo e o
seu suporte, quão cheio está e se está em bom estado. É um relatório, não um
controlo — montar e desmontar cabem ao gestor de ficheiros e ao comando
`mount` — e um volume cujo formato não tem uma capacidade fixa di-lo em vez de
mostrar uma barra. O `df` reporta os mesmos números numa shell.

**Files** define como funciona o gestor de ficheiros: onde se abre uma
janela nova e como mostra e ordena o que lista, se mostra a coluna de locais e
a barra de ferramentas, se as pastas ficam no topo, que extensões de nomes e
que itens ficam ocultos, se uma imagem mostra o seu próprio conteúdo e uma
pasta o que contém, se mover itens para o Lixo pergunta primeiro e se uma
imagem de disco ou um arquivo que abriu é desmontado depois de fechadas as suas
janelas. Uma linha surte efeito assim que é escolhida: onde se abre uma janela
nova e se mostra a coluna de locais e a barra de ferramentas aplicam-se à
próxima janela que abrir, e tudo o resto chega a todas as janelas do gestor de
ficheiros de uma vez, embora uma janela mantenha uma visualização ou uma ordem
que tenha escolhido nela pessoalmente. *Settings…* no menu do próprio gestor de
ficheiros abre este painel.

## EXIT STATUS

Zero após um fecho limpo; diferente de zero quando o canal de janela ou a
região de fotogramas partilhada foi recusada (o motivo é indicado no fluxo de
erro padrão).
