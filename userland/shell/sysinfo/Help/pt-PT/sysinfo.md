## NAME

sysinfo — consultar informação do sistema

## SYNOPSIS

`sysinfo <query>`

## DESCRIPTION

Emite uma consulta tipada à API de Informação do Sistema e apresenta a
resposta. O TAIRiX não tem `/proc` nem `/sys`: este comando é a face de
terminal da mesma API versionada e verificada por capacidades que todos
os programas usam, e nenhum caminho contorna a verificação de
capacidade.

As consultas:

- `processes`, `ps` — listar processos, uma fila por processo.
- `memory`, `mem` — estatísticas de memória do núcleo (precisa de
  `CAP_SYSINFO_KERNEL`).
- `hardware`, `hw` — a árvore de hardware detetada (precisa de
  `CAP_SYSINFO_HW`).
- `identity`, `id` — identidade da máquina e versão do SO.
- `uptime` — tempo desde o arranque e a hora de relógio do arranque.
- `limits`, `rlimits` — os seus limites de recursos efetivos e o uso ao
  vivo.
- `seats` — o inventário de assentos: o dono de cada ecrã e a sua
  consola em primeiro plano (necessita de `CAP_SYSINFO_HW`).
- `pressure` — o indicador de pressão de memória em direto: banda,
  limiares e contadores de transição (necessita `CAP_SYSINFO_KERNEL`).
- `reclaim` — o registo das caches recuperáveis, uma linha por classe
  (necessita `CAP_SYSINFO_KERNEL`).
- `ramzip` — os contadores do nível de memória comprimida (necessita
  `CAP_SYSINFO_KERNEL`).
- `cpu` — profundidade da fila, mudanças de contexto e preempções por
  CPU (necessita `CAP_SYSINFO_KERNEL`).
- `cpuinfo` — o relatório do processador por CPU (um superconjunto de
  `/proc/cpuinfo`): modelo/fabricante, classe de desempenho, sinalizadores
  de extensões ISA, o registo de identidade em bruto, a frequência de
  relógio do núcleo medida em direto (em MHz — ou um honesto «unknown»
  onde não existe contador de relógio do núcleo) e a frequência fixa de
  referência ou base de tempo. Factos públicos do hardware, não requer
  nenhuma capacidade.
- `irq`, `irqs` — a tabela de IRQ do núcleo: uma linha por cada linha de
  interrupção associada — o seu id, a tarefa do controlador proprietária,
  o número de interrupções desde o arranque e se a linha está em
  quarentena (necessita `CAP_SYSINFO_HW`).
- `frames` — o que custaram os fotogramas compostos de cada sessão do
  ambiente de trabalho: uma linha por cada sessão que publica — os pixels
  que os seus fotogramas recompuseram e a média por fotograma, o múltiplo
  de sobredesenho (contribuições de camadas por pixel danificado), a
  fração resolvida pela cópia de um troço opaco, os pixels que um fundo em
  vidro fosco recalculado voltou a desfocar, os pixels convertidos para a
  saída de varrimento, o pior fotograma individual e a sua fração do ecrã,
  as chamadas ao controlador que os apresentaram, e os acertos e falhas da
  cache da decoração das janelas. São os números do próprio ambiente de
  trabalho — só um processo que detenha um compositor pode contar
  pixels —, pelo que cada linha nomeia quem a publicou, tal como o serviço
  o atestou, e uma sessão que nada publicou não imprime nenhuma linha
  (necessita `CAP_SYSINFO_GLOBAL`).
- `storage`, `io` — três tabelas por volume, com uma linha em cada por
  cada volume suportado por blocos e ciente de falhas, identificada por
  um prefixo do identificador durável do volume e com o nome do
  dispositivo que o serve. Essa identidade é o ponto final do serviço de
  blocos que o serve quando um controlador em espaço de utilizador serve
  o dispositivo, e uma identidade reservada gerida pelo núcleo quando é o
  próprio núcleo a geri-lo — o disco a partir do qual a máquina arrancou,
  que não tem nenhum ponto final de serviço. **Serviço** nomeia ainda o
  dispositivo: o nome curto que o seu próprio controlador declara
  (`virtio-blk`, `emmc2`, `usb-msd`), ou `-` quando não declara nenhum.
  Transporta os bytes acumulados, os pedidos concluídos e o tempo de
  ocupação do dispositivo que um leitor converte, por diferença, em
  débito, IOPS, utilização e espera — nada é pré-calculado, pelo que dois
  leitores nunca herdam a mesma janela de média (não requer nenhuma
  capacidade). **Fila** transporta o que está pendente agora, os
  acumuladores de cuja diferença sai uma profundidade média, e a
  profundidade e o prazo que a própria classe do dispositivo permite.
  **Saúde** transporta a disponibilidade atual do volume
  (available/degraded/recovering/lost) e os contadores cumulativos de
  resultados (conclusões, reinícios, expirações, erros de suporte,
  reemissões) em que um disco a falhar ou instável se torna visível. As
  duas últimas necessitam de `CAP_SYSINFO_KERNEL`; a tabela de serviço é
  impressa primeiro, pelo que um chamador sem essa capacidade continua a
  ler os números de utilização antes de a recusa ser comunicada.
- `raid`, `arrays` — os conjuntos RAID compostos e os dispositivos que o
  compositor de conjuntos detém: uma linha por conjunto — um prefixo da
  sua identidade, o seu nível, a sua saúde
  (optimal/degraded/recovering/failed), o número de membros sincronizados
  e definidos, a sua unidade de faixa, o seu número de blocos e qualquer
  reconstrução ou verificação em curso — depois uma linha por dispositivo
  — o seu nó da árvore de hardware, o conjunto a que pertence (um travessão
  para um candidato não afiliado), a sua ranhura, o seu papel
  (candidate/held/in-sync/resyncing/faulted), o seu tamanho e a geração de
  metadados que carrega (necessita `CAP_SYSINFO_HW`).
- `dma`, `iommu` — as unidades de tradução DMA: uma linha por unidade — o
  seu nó na árvore de hardware, a sua família, se traduz ou porque não, como
  assinala as suas falhas, onde residem as suas traduções, os proprietários
  e fluxos do firmware que detém, e as suas falhas registadas e descartadas,
  e os fluxos que uma tempestade silenciou, desde o arranque — depois uma
  linha por grupo de isolamento que um proprietário detém — a sua unidade, o
  nó e a geração do detentor e o estado desse proprietário — depois uma
  linha por nó que uma unidade traduz para um proprietário — a sua unidade e
  o seu grupo, a geração e o estado do seu proprietário, os seus fluxos, e
  os mapeamentos do seu domínio e os bytes que abrangem (requer
  `CAP_SYSINFO_HW`).
- `audio`, `sound` — os dispositivos de som: uma linha por saída e por
  entrada — o seu identificador, se é a predefinida do seu sentido, o nível
  e o silêncio, a frequência medida do seu relógio durante a reprodução, as
  tramas perdidas, a localização e o nome — e depois uma linha por cada
  fluxo de som seu — o dispositivo, o sentido, a função e o estado, a
  posição, as falhas e quem o detém.
- `show <resource-ref>` — lê uma referência de recurso
  `info:`/`state:`/`stats:` e imprime o seu valor. Esses espaços de nomes
  servem valores tipados através desta API, nunca fluxos de bytes: o `cat`
  não os consegue abrir. Uma recusa nomeia a capacidade necessária.
- `describe <resource-ref>` — imprime o envelope da resposta em vez do
  valor: o produtor, a autorização com que foi servida e os metadados da
  carga — para uma métrica o género, a unidade, o comportamento de
  reposição e a janela de amostragem; para um facto o tipo e a
  sensibilidade.
- `help` — a ajuda curta deste próprio comando.

Sem consulta, mostra-se a ajuda curta.

## OPTIONS

- `--all, -a` — com `processes`: listar todos os processos do sistema
  em vez de apenas os seus; o serviço só concede esta vista a um
  chamador que detenha `CAP_SYSINFO_GLOBAL`.
  Com `audio`, todos os fluxos de som da máquina, com a mesma
  capacidade.
- `-h, -?` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `sysinfo identity` — imprimir a identidade da máquina e a versão do
  SO.
- `sysinfo ps --all` — listar todos os processos do sistema.
- `sysinfo frames` — mostrar quanto custam os fotogramas do ambiente de
  trabalho.

## EXIT STATUS

- `0` — a consulta foi respondida e apresentada.
- `1` — o serviço recusou ou falhou, ou o resultado não pôde ser
  entregue. Uma linha de comandos não compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `pt-PT`).

## SEE ALSO

- `man`
- `ps`
- `top`
