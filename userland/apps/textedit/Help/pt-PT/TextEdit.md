## NAME

TextEdit — editor gráfico de texto e hexadecimal

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Edita qualquer ficheiro numa janela do ambiente de trabalho: texto, código
fonte, os ficheiros de definições do sistema ou bytes em bruto. Iniciado com
um documento — a partir do gestor de ficheiros, do ambiente de trabalho ou ao
largar um ficheiro sobre o seu ícone na barra de ícones — abre uma janela
sobre esse ficheiro. Iniciado sozinho abre uma janela vazia. Cada documento é
uma janela do único editor; fechar a última deixa-o na barra de ícones, e a
linha «Sair» do menu do seu ícone termina-o.

Nada do que um ficheiro contém fica escondido. Um byte de controlo aparece
como `[x03]`, um byte que não é UTF-8 válido como `[xC3]`, e um carácter
invisível ou que muda a direção da escrita como `[U+202E]`, cada um na sua
própria cor e cada um um só passo do cursor. Um ficheiro que parece conter
dados binários abre na vista hexadecimal, que mostra cada byte como dois
dígitos hexadecimais ao lado do seu carácter e edita os mesmos bytes que a
vista de texto.

O código fonte é colorido: HTML, XML e SVG, CSS, JavaScript, JSON, YAML, TOML,
Markdown, Rust, C, Java, Python e scripts de shell. Os ficheiros de definições
do sistema — definições de aplicações, a biblioteca de programas, a
configuração do sistema e da rede, as substituições de serviços, as bases de
dados de utilizadores e de grupos e os manifestos de famílias de tipos de
letra — também são coloridos e verificados com o analisador com que o sistema
os lê: um problema é assinalado na margem ao lado da sua linha e indicado na
linha de estado. O formato é escolhido pelo nome do ficheiro e, se não, pelos
seus primeiros bytes; um formato escolhido no menu Ver ou na linha de estado
prevalece sempre.

O editor não possui qualquer capacidade sobre o sistema de ficheiros. Edita
apenas o ficheiro que lhe foi entregue. Um ficheiro que o utilizador pode
alterar é entregue com permissão de escrita, e Guardar escreve-o de volta;
qualquer outro é só de leitura, e Guardar pergunta onde guardar uma cópia. A
coloração, a deteção do formato e a verificação correm num processo de
trabalho à parte sem qualquer acesso, pelo que um ficheiro hostil não
consegue alcançar nada do que o editor alcança.

Ao premir o botão secundário (direito) do rato em qualquer lugar da janela
abre-se o seu menu: Cortar, Copiar, Colar e Selecionar tudo, e depois
Ficheiro, Editar, Procurar e Ver, cada um dos quais abre o seu próprio
submenu. A janela não tem barra de menus.

A linha de estado mostra a linha e a coluna do cursor, o que a verificação
encontrou e, como campos que abrem um menu ao clicar: o formato, texto ou
hexadecimal, os finais de linha e a indentação. Fechar uma janela ou sair
com alterações por guardar pergunta primeiro.

* `Ctrl+N` — uma janela nova
* `Ctrl+O` — abrir um ficheiro
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — fechar a janela
* `Ctrl+Z` — anular; `Ctrl+Shift+Z` ou `Ctrl+Y` — refazer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, colar
* `Ctrl+A` — selecionar tudo
* `Ctrl+F` — procurar; `Ctrl+H` — substituir
* `F3` / `Shift+F3` — a ocorrência seguinte ou anterior
* `Ctrl+L` — ir para uma linha
* `F8` — o problema seguinte que a verificação encontrou
* `Ctrl+]` / `Ctrl+[` — aumentar ou reduzir a indentação das linhas selecionadas
* `Ctrl+/` — comentar as linhas selecionadas ou descomentá-las
* `Ctrl+Shift+H` — alternar entre a vista de texto e a hexadecimal
* `Insert` — alternar entre inserir e sobrescrever
* `Tab` — na vista hexadecimal, passar entre as colunas hexadecimal e de caracteres

## OPTIONS

`-h`, `-?`, `--help`
: Escrever esta ajuda na saída padrão e terminar.

## EXIT STATUS

Zero após Sair. Diferente de zero quando o canal de janela, a caixa de
eventos ou a sessão foi recusada; o motivo é declarado no fluxo de erro
padrão.
