## NAME

fstree — o gestor de ficheiros em árvore de ecrã inteiro

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Percorre o sistema de ficheiros numa sessão de ecrã inteiro guiada pelo
teclado, ao estilo do XTree Gold: um cabeçalho de estatísticas de disco
no topo, uma janela com a árvore de diretórios por cima de uma janela de
ficheiros que lista as entradas do diretório realçado com os seus
tamanhos e datas de modificação. A sessão começa em `directory` (a vista
raiz `/` por omissão).

A janela da árvore é a principal: a barra de realce move-se nela e a
janela de ficheiros mostra sempre o diretório realçado. `Enter` (ou
`Tab`) passa à janela de ficheiros para atuar sobre ficheiros; `Esc` (ou
`Tab`) regressa à árvore — a navegação começa sempre na árvore.

A árvore é lida preguiçosamente: o conteúdo de um diretório só é obtido
quando é mostrado ou expandido pela primeira vez, pelo que percorrer um
volume enorme custa apenas os diretórios realmente abertos. Um diretório
que o chamador não pode listar é recusado no local — o erro aparece na
linha de mensagens e a vista anterior mantém-se; nada é fabricado.

Teclas:

- `Up`/`Down` ou `k`/`j` — mover o realce da janela ativa;
  `PageUp`/`PageDown` movem-no um ecrã, `Home`/`End` saltam para a
  primeira ou a última linha. Mover o realce da árvore lista o diretório
  recém-realçado na janela de ficheiros.
- `Right`/`l` — na árvore, expandir o ramo realçado (lido
  preguiçosamente); na janela de ficheiros, descer ao diretório
  realçado.
- `Left`/`h` — na árvore, recolher o ramo realçado ou — quando já está
  recolhido ou sem subdiretórios — subir o realce ao diretório pai; na
  janela de ficheiros, voltar à árvore.
- `+`/`-` — expandir / recolher o ramo realçado da árvore.
- `Enter` — na árvore, passar à janela de ficheiros; na janela de
  ficheiros, descer ao diretório realçado (ambas as janelas acompanham)
  ou abrir o ficheiro realçado num visualizador de ecrã inteiro: o
  visualizador de desassemblagem para um executável reconhecido, o
  paginador de texto quando os primeiros bytes do ficheiro se leem como
  texto, o despejo hexadecimal nos restantes casos (os visualizadores são
  descritos mais abaixo).
- `Esc` — cancelar um cálculo de uso do disco em curso (mantendo os seus
  números); caso contrário, voltar da janela de ficheiros à janela da
  árvore.
- `o` — abrir o ficheiro realçado num visualizador à escolha: `t` texto,
  `x` hexadecimal ou `d` desassemblagem.
- `Tab` — alternar entre a janela da árvore e a janela de ficheiros.
- `s` — abrir o menu de ordenação: `n` nome, `e` extensão, `s` tamanho,
  `m` data de modificação, `r` inverter o sentido, `Esc` cancela. Os
  diretórios agrupam-se sempre antes dos ficheiros.
- `c` — copiar a entrada selecionada: uma linha pede o destino. Um
  destino relativo cai no diretório listado; um destino que é um
  diretório existente recebe a cópia lá dentro, com o nome da origem.
  `Tab` completa o caminho escrito — uma correspondência única é
  preenchida (um diretório fica aberto com a sua `/`), várias
  correspondências estendem-se até à raiz comum ou são listadas. Um
  diretório é copiado com todo o seu conteúdo. Copiar uma entrada sobre
  si própria ou um diretório para dentro da sua própria subárvore é
  recusado antes de qualquer escrita.
- `m` — mover a entrada selecionada, com a mesma pergunta de destino.
  Dentro do mesmo volume a mudança é uma renomeação atómica; entre
  volumes a entrada é copiada e a origem depois removida.
- `r` — renomear a entrada selecionada no local: a linha vem
  pré-preenchida com o nome atual.
- `d` — apagar a entrada selecionada após uma confirmação; só `y`
  prossegue. Apagar um diretório remove todo o seu conteúdo, e a
  confirmação di-lo. A confirmação pode ser desligada no menu de
  configurações (`S`).
- `M` — criar um diretório no diretório listado; o nome é pedido.
- `a` — abrir os atributos da entrada selecionada: os seus bits de
  permissão e os seus atributos estendidos (pares `namespace.key = value`,
  por exemplo `user.comment`). Na vista, as setas percorrem os atributos,
  `m` edita o modo (uma linha octal pré-preenchida com os bits atuais;
  Enter aplica — só o proprietário da entrada o pode alterar, o núcleo
  recusa qualquer outro), `n` acrescenta um atributo como uma linha
  `key=value`, Enter edita o selecionado (pré-preenchido; com um valor
  binário só a chave é pré-preenchida), `d` remove-o e Esc sai. O núcleo
  decide cada alteração — a leitura e a escrita seguem as permissões
  próprias da entrada, os espaços de nomes `system.`/`trusted.` estão
  reservados e uma recusa é mostrada tal e qual. Um sistema de ficheiros
  cujo formato não guarda atributos (FAT32, ext4) indica-o na vista; o
  editor de modo continua a funcionar aí.
- `t` — marcar ou desmarcar a entrada selecionada do painel de
  ficheiros e descer uma linha; pressionar repetidamente marca uma
  série. As entradas marcadas trazem um `*`.
- `T` — marcar por padrão: um glob (`*`, `?`, `[...]`) comparado com
  os nomes visíveis, ou um intervalo — `size:MIN..MAX` (bytes, sufixos
  `K`/`M`/`G`/`T`, qualquer dos limites pode ficar em aberto:
  `size:1M..`, `size:..64K`) ou `date:YYYY-MM-DD..YYYY-MM-DD` (data de
  modificação, data final incluída; uma entrada cujo suporte não guarda
  data nunca corresponde a uma data). Cada correspondência junta-se ao
  conjunto marcado; um padrão ou intervalo mal formado não marca nada e
  diz porquê.
- `i` — inverter as marcas sobre as entradas visíveis.
- `C` — limpar todas as marcas.
- `f` — filtrar o painel de ficheiros por um glob de nome de ficheiro,
  aplicado em direto à medida que é escrito. `Enter` mantém o filtro
  (mostrado na linha de estado), `Esc` repõe o filtro tal como estava. Um
  padrão que não compila não oculta nada — a linha de estado assinala-o
  antes com `(bad pattern)`. Esvaziar o padrão retira o filtro.
- `/` — pesquisar o ramo sob o diretório focado por glob de nome de
  ficheiro, comparado com o caminho de cada ficheiro relativo ao ramo. Os
  resultados chegam à vista aplanada à medida que são encontrados.
- `F` — pesquisar um texto literal no conteúdo dos ficheiros, sem
  distinguir maiúsculas de minúsculas e lido em janelas limitadas (uma
  correspondência que atravessa um limite de leitura é encontrada na
  mesma). Com entradas marcadas, a pesquisa abrange o conjunto marcado
  (os diretórios marcados, recursivamente); caso contrário, abrange o
  ramo focado. Cada linha de resultado indica o seu número de
  correspondências; um ficheiro cujo conteúdo parece binário é reportado
  como correspondência binária — os seus bytes nunca são mostrados. Um
  ficheiro que não se deixa ler é listado no relatório do percurso, nunca
  descartado em silêncio.
- `u` — contar o uso do disco sob o diretório focado: ficheiros,
  bytes e diretórios, percorridos de forma incremental em segundo
  plano. `Esc` cancela, mantendo os números contados até aí.
- `v` — aplanar o ramo sob o diretório focado: uma lista de cada
  ficheiro abaixo dele, preenchida página a página (`Space` carrega
  a seguinte). Na vista, `t`/`T`/`i`/`C` marcam as suas linhas,
  `c`/`m`/`d` executam operações em lote sobre o conjunto marcado,
  `Enter` salta para o diretório da linha selecionada nos painéis (com o
  cursor sobre a sua entrada) e `Esc` regressa aos painéis. Enquanto um
  percurso ou uma pesquisa ainda decorre, `Esc` primeiro interrompe-o, mantendo
  as linhas encontradas até aí. As linhas são nomeadas relativamente ao
  ramo aplanado. Os resultados de pesquisa (`/`, `F`) preenchem esta
  mesma vista, pelo que as suas linhas se marcam e operam exatamente como
  uma listagem aplanada.
- `H` — mostrar/ocultar as entradas ocultas (nomes com ponto) em ambos os
  painéis.
- `.` — repetir a última operação sobre ficheiros na seleção atual: uma
  cópia ou mudança vai de novo para o mesmo diretório de destino, uma
  eliminação volta a perguntar conforme a configuração de confirmação.
  Sem qualquer operação anterior, a tecla di-lo.
- `V` — listar os volumes montados: cada linha mostra o ponto de
  montagem, o tipo de sistema de ficheiros e os bytes livres/totais
  quando o volume os reporta. `Enter` muda a raiz de toda a sessão para
  o volume escolhido; `Esc` fecha a lista. Quando nenhum volume é
  reportado, a tecla di-lo.
- `S` — o menu de configurações: `1` liga ou desliga a confirmação da
  eliminação individual, `2` a da eliminação em lote. As alterações são
  guardadas nas configurações próprias desta aplicação, que só ela pode
  ler, e carregadas no arranque seguinte; quando o serviço de
  configurações está inacessível, valem apenas para a sessão e o menu
  explica porquê.
- `?` — mostrar esta ajuda sobre os painéis; qualquer tecla fecha-a.
- `q` — sair, restaurando o terminal.

Enquanto houver entradas marcadas, `c`, `m` e `d` atuam sobre todo o
conjunto marcado em vez da seleção: `c`/`m` pedem um diretório de
destino existente onde as entradas caem (`Tab` completa-o), e `d`
confirma a eliminação em lote (a menos que essa confirmação esteja
desligada no menu de configurações). As entradas são processadas por
ordem de marcação; uma falha nunca trava as restantes, o relatório final
conta o que teve sucesso e um ecrã de relatório nomeia cada falha — um
lote nunca fica silenciosamente parcial. As entradas com sucesso são
desmarcadas; as falhas ficam marcadas para nova tentativa.

Quando uma cópia ou mudança iria sobrescrever um ficheiro existente, a
sessão pergunta por ficheiro: `o` sobrescreve, `s` salta (uma origem
saltada fica no seu lugar) e `c` cancela os passos restantes — num
lote, cancelar abandona todas as entradas restantes — o que
já foi aplicado permanece, e o relatório final diz o que aconteceu. Uma
falha a meio da cópia remove o destino meio escrito e mostra o erro do
núcleo; nada se faz passar por uma cópia completa. Cada operação é
autorizada pelo núcleo — uma recusa aparece tal e qual na linha de
mensagens sem que nada mude.
Uma cópia mantém os buracos de um ficheiro e os seus atributos
estendidos; qualquer atributo que o destino não consiga guardar é
indicado na linha de mensagens no fim da operação.

Um diretório copiado ou movido para um diretório com o mesmo nome é juntado
a ele: o que contém passa para o diretório que já lá está, e pergunta-se da
mesma forma por cada um dos seus ficheiros que já lá esteja.

O cabeçalho de estatísticas de disco no topo mostra o caminho listado, os
bytes livres/totais do volume subjacente (quando o serviço de informação
do sistema os pode reportar), o número de itens visíveis e — enquanto
algo estiver marcado — o número de entradas marcadas. A faixa de estado
por baixo das janelas mostra a janela ativa (Tree ou Files), a ordem de
ordenação, se as entradas ocultas estão visíveis, o filtro de nome de
ficheiro ativo, e o número de entradas marcadas com o seu total de
bytes. Um ficheiro cujo formato de armazenamento não guarda data de
modificação mostra `-` na coluna da data.

Quando o painel de ficheiros omite entradas ocultas, a sessão também
anota a omissão no fluxo de informação padrão (fd 3) — um registo
informativo por cada alteração, que pode ser capturado com
`fstree 3>info.jsonl`; a própria sessão não é afetada.

Os visualizadores: `Enter` sobre um ficheiro regular abre-o só para
leitura num visualizador de ecrã inteiro. Um executável reconhecido — uma
imagem `rxe`, um ELF de 64 bits, um módulo wasm ou um manifesto assinado
autónomo — abre no **visualizador de desassemblagem**; um ficheiro cujos
primeiros bytes não contêm NUL e são UTF-8 válido abre no **paginador de
texto**; tudo o resto abre no **despejo hexadecimal**. `o` sobrepõe-se a
esta escolha. `x` muda o visualizador aberto para o despejo hexadecimal
no mesmo ponto, `t` para o paginador de texto (ajustado ao início da
linha que contém o deslocamento mostrado) e `d` para o visualizador de
desassemblagem (um ficheiro que nenhum formato de contentor reivindica
pede um conjunto de instruções e é descodificado como um fragmento em
bruto a partir do ponto atual); `q` ou `Esc` regressa aos painéis.

Os visualizadores percorrem o ficheiro em janelas limitadas — o ficheiro
nunca é mantido inteiro em memória, pelo que um ficheiro de qualquer
tamanho (muito para lá de 4 GiB) é paginado corretamente — e partilham as
teclas:

- `Up`/`Down` ou `k`/`j` — uma linha; `PageUp`/`PageDown`, `b`/`Space` —
  uma página; `Home`/`End` — o início / a última página.
- `g` — ir para um ponto: um número de linha contado a partir de 1 no
  paginador de texto, um deslocamento em bytes (decimal ou hexadecimal
  com `0x`) no despejo hexadecimal, um endereço no visualizador de
  desassemblagem.
- `/` — pesquisar para a frente a partir do ponto atual; `n` repete a
  última pesquisa para lá da correspondência anterior. A pesquisa corre
  em segundo plano em passos limitados — a sessão continua a responder
  num ficheiro enorme, a linha de estado diz `searching…` e `Esc`
  interrompe-a onde está.
- `?` — esta ajuda; qualquer tecla fecha-a.

O **paginador de texto** descodifica UTF-8 com uma substituição visível
para bytes inválidos, expande as tabulações e mostra qualquer outro byte
de controlo como um `·` visível — o conteúdo dos ficheiros nunca é
enviado ao terminal como sequências de escape em bruto. As linhas longas
são quebradas por omissão; `w` liga ou desliga a quebra (sem ela, o resto
é cortado na margem direita). `/` pesquisa texto literal, sem distinguir
maiúsculas de minúsculas, através dos limites de leitura. A linha de
estado mostra o ponto como um número de linha; após um salto a partir do
despejo hexadecimal, o número de linha é desconhecido (poupou-se o custo
de contar) até que um salto com `g` o volte a ancorar.

O **despejo hexadecimal** mostra a disposição clássica — coluna de
deslocamentos, dezasseis bytes em pares hexadecimais e uma coluna ASCII
com os bytes não imprimíveis como `.`. A coluna de deslocamentos é tão
larga quanto o ficheiro exige (pelo menos oito dígitos hexadecimais). `/`
pesquisa texto literal (sem distinguir maiúsculas de minúsculas) ou,
escrito como `0x` seguido de pares de bytes hexadecimais (`0xdeadbeef`),
uma sequência exata de bytes.

O **visualizador de desassemblagem** abre numa página de resumo: o
formato do contentor, o seu conjunto de instruções e o seu ponto de
entrada, as suas regiões (endereço, extensão no ficheiro, tamanho em
memória, permissões) e o seu número de símbolos; um manifesto assinado
autónomo lista antes a sua versão de ABI e as capacidades que pede. Uma
imagem `rxe` traz o seu manifesto ao lado, nunca embutido, e o resumo
di-lo. `Up`/`Down` percorrem as linhas de regiões e `Enter` abre a
desassemblagem da região de código selecionada (uma região de dados é
mostrada em hexadecimal sobre os seus bytes do ficheiro). Um contentor
que não indica nenhum conjunto de instruções — uma imagem `rxe` corre em
qualquer máquina que a carregue — pergunta uma vez: `x` x86-64,
`a` aarch64, `r` riscv64, `w` wasm.

O painel de código mostra uma instrução por linha — endereço, bytes de
codificação, mnemónica, operandos — com linhas de etiqueta `<symbol>:` e
destinos de salto simbolizados (`<main+0x8>`) onde o contentor indica
símbolos. As instruções são descodificadas ecrã a ecrã, nunca o binário
inteiro de antemão. `g` salta para um endereço, `/` pesquisa o texto das
instruções, `End` avança até ao fim da região em segundo plano (`Esc`
interrompe o avanço) e `I` volta a descodificar com outro conjunto de
instruções. `Esc` regressa à página de resumo.

Toda a descodificação de contentores e de instruções corre num processo
auxiliar blindado (a caixa de areia dos analisadores), nunca no próprio
gestor de ficheiros: um executável malicioso pode fazer falhar o
auxiliar, não a sessão. Um ficheiro que o descodificador recusa — mal
formado, ou demasiado grande para entregar à caixa de areia — recorre ao
despejo hexadecimal com um aviso de uma linha.

Uma leitura que o núcleo recusa a meio da visualização fecha o
visualizador e mostra o erro na linha de mensagens — conteúdo
desatualizado nunca é apresentado como atual.

## OPTIONS

- `directory` — o diretório onde a sessão começa; o valor por omissão é a
  vista raiz `/`.
- `-h`, `-?` — imprimir a forma curta deste documento e sair.

## EXIT STATUS

- `0` — a sessão terminou com o `q` do utilizador.
- `1` — o diretório inicial não pôde ser listado, ou o caminho do
  terminal falhou. Uma linha de comandos não compreendida também termina
  com `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
