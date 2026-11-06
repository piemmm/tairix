## NAME

fstree — el gestor de archivos en árbol a pantalla completa

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Recorre el sistema de archivos en una sesión a pantalla completa guiada
por teclado, al estilo de XTree Gold: una cabecera de estadísticas de
disco arriba, una ventana de árbol de directorios sobre una ventana de
archivos que lista las entradas del directorio resaltado con sus tamaños
y fechas de modificación. La sesión comienza en `directory` (la vista
raíz `/` por omisión).

La ventana de árbol es la principal: la barra de resaltado se mueve por
ella y la ventana de archivos muestra siempre el directorio resaltado.
`Enter` (o `Tab`) pasa a la ventana de archivos para actuar sobre los
archivos; `Esc` (o `Tab`) vuelve al árbol: la navegación siempre empieza
en el árbol.

El árbol se lee de forma perezosa: el contenido de un directorio solo se
obtiene cuando se muestra o se despliega por primera vez, de modo que
recorrer un volumen enorme solo cuesta los directorios realmente
abiertos. Un directorio que el llamante no puede listar se rechaza en el
sitio: el error aparece en la línea de mensajes y la vista anterior se
conserva; nada se fabrica.

Teclas:

- `Up`/`Down` o `k`/`j` — mover el resaltado de la ventana activa;
  `PageUp`/`PageDown` lo mueven una pantalla, `Home`/`End` saltan a la
  primera o a la última fila. Mover el resaltado del árbol lista el
  directorio recién resaltado en la ventana de archivos.
- `Right`/`l` — en el árbol, desplegar la rama resaltada (leída de forma
  perezosa); en la ventana de archivos, descender al directorio
  resaltado.
- `Left`/`h` — en el árbol, plegar la rama resaltada o — cuando ya está
  plegada o no tiene subdirectorios — subir el resaltado al directorio
  padre; en la ventana de archivos, volver al árbol.
- `+`/`-` — desplegar / plegar la rama resaltada del árbol.
- `Enter` — en el árbol, pasar a la ventana de archivos; en la ventana de
  archivos, descender al directorio resaltado (ambas ventanas lo siguen)
  o abrir el archivo resaltado en un visor a pantalla completa: el visor
  de desensamblado para un ejecutable reconocido, el paginador de texto
  cuando los primeros bytes del archivo se leen como texto, el volcado
  hexadecimal en otro caso (los visores se describen más abajo).
- `Esc` — cancelar un cálculo de uso de disco en curso (conservando sus
  cifras); si no, volver de la ventana de archivos a la ventana de árbol.
- `o` — abrir el archivo resaltado en el visor que elija: `t` texto,
  `x` hexadecimal o `d` desensamblado.
- `Tab` — cambiar entre la ventana de árbol y la ventana de archivos.
- `s` — abrir el menú de ordenación: `n` nombre, `e` extensión,
  `s` tamaño, `m` fecha de modificación, `r` invertir el sentido, `Esc`
  cancela. Los directorios siempre se agrupan antes que los archivos.
- `c` — copiar la entrada seleccionada: una línea pide el destino. Un
  destino relativo cae en el directorio listado; un destino que es un
  directorio existente recibe la copia dentro, bajo el nombre del
  origen. `Tab` completa la ruta escrita: una coincidencia única se
  rellena (un directorio queda abierto con su `/`), varias coincidencias
  se amplían hasta su raíz común o se listan. Un directorio se copia con
  todo su contenido. Copiar una entrada sobre sí misma o un directorio
  dentro de su propio subárbol se rechaza antes de escribir nada.
- `m` — mover la entrada seleccionada, con la misma pregunta de
  destino. Dentro de un mismo volumen el movimiento es un renombrado
  atómico; entre volúmenes se copia la entrada y después se elimina el
  origen.
- `r` — renombrar la entrada seleccionada en el sitio: la línea viene
  precargada con el nombre actual.
- `d` — borrar la entrada seleccionada tras una confirmación; solo `y`
  procede. Borrar un directorio elimina todo su contenido, y la
  confirmación lo dice. La confirmación puede desactivarse en el menú de
  ajustes (`S`).
- `M` — crear un directorio en el directorio listado; se pide su nombre.
- `a` — abrir los atributos de la entrada seleccionada: sus bits de
  permiso y sus atributos extendidos (pares `namespace.key = value`, por
  ejemplo `user.comment`). Dentro de la vista, las flechas recorren los
  atributos, `m` edita el modo (una línea octal precargada con los bits
  actuales; Intro aplica — solo el propietario de la entrada puede
  cambiarlo, el núcleo rechaza a cualquier otro), `n` añade un atributo
  como una línea `key=value`, Intro edita el seleccionado (precargado;
  con un valor binario solo se precarga la clave), `d` lo elimina y Esc
  sale. El núcleo decide cada cambio: la lectura y la escritura siguen
  los permisos propios de la entrada, los espacios de nombres
  `system.`/`trusted.` están reservados y un rechazo se muestra tal
  cual. Un sistema de archivos cuyo formato no almacena atributos
  (FAT32, ext4) lo indica en la vista; el editor de modo sigue
  funcionando allí.
- `t` — marcar o desmarcar la entrada seleccionada del panel de
  archivos y bajar una fila; pulsaciones repetidas marcan una serie.
  Las entradas marcadas llevan un `*`.
- `T` — marcar por patrón: un glob (`*`, `?`, `[...]`) comparado con
  los nombres visibles, o un intervalo — `size:MIN..MAX` (bytes, sufijos
  `K`/`M`/`G`/`T`, cualquiera de los límites puede quedar abierto:
  `size:1M..`, `size:..64K`) o `date:YYYY-MM-DD..YYYY-MM-DD` (fecha de
  modificación, fecha final incluida; una entrada cuyo almacenamiento no
  guarda fecha nunca coincide por fecha). Cada coincidencia se añade al
  conjunto marcado; un patrón o intervalo mal formado no marca nada y
  dice por qué.
- `i` — invertir las marcas sobre las entradas visibles.
- `C` — borrar todas las marcas.
- `f` — filtrar el panel de archivos por un glob de nombre de archivo,
  aplicado en vivo mientras se escribe. `Enter` conserva el filtro
  (mostrado en la línea de estado), `Esc` restaura el filtro tal como
  estaba. Un patrón que no compila no oculta nada: la línea de estado lo
  marca en cambio con `(bad pattern)`. Vaciar el patrón quita el filtro.
- `/` — buscar en la rama bajo el directorio enfocado por glob de nombre
  de archivo, comparado con la ruta de cada archivo relativa a la rama.
  Los resultados llegan a la vista aplanada a medida que se encuentran.
- `F` — buscar un texto literal en el contenido de los archivos, sin
  distinguir mayúsculas de minúsculas y leído en ventanas acotadas (una
  coincidencia que cruza un límite de lectura se encuentra igualmente).
  Con entradas marcadas, la búsqueda abarca el conjunto marcado (los
  directorios marcados, de forma recursiva); si no, abarca la rama
  enfocada. Cada fila de resultado lleva su número de coincidencias; un
  archivo cuyo contenido parece binario se informa como coincidencia
  binaria, y sus bytes nunca se muestran. Un archivo que no se deja leer
  figura en el informe del recorrido, nunca se omite en silencio.
- `u` — contar el uso de disco bajo el directorio enfocado: archivos,
  bytes y directorios, recorridos de forma incremental en segundo
  plano. `Esc` cancela conservando las cifras contadas hasta entonces.
- `v` — aplanar la rama bajo el directorio enfocado: una lista de cada
  archivo debajo, llenada página a página (`Space` carga la siguiente).
  Dentro de la vista, `t`/`T`/`i`/`C` marcan sus filas, `c`/`m`/`d`
  ejecutan operaciones por lotes sobre el conjunto marcado, `Enter` salta
  al directorio de la fila seleccionada en los paneles (dejando el cursor
  sobre su entrada) y `Esc` vuelve a los paneles. Mientras un recorrido o
  una búsqueda siguen en curso, `Esc` primero los detiene, conservando
  las filas encontradas hasta entonces. Las filas se nombran relativas a
  la rama aplanada. Los resultados de búsqueda (`/`, `F`) llenan esta
  misma vista, de modo que sus filas se pueden marcar y operar
  exactamente igual que un listado aplanado.
- `H` — mostrar/ocultar las entradas ocultas (nombres con punto) en ambos
  paneles.
- `.` — repetir la última operación de archivo sobre la selección
  actual: una copia o un movimiento va de nuevo al mismo directorio de
  destino, un borrado vuelve a preguntar según el ajuste de
  confirmación. Si aún no hay ninguna operación, la tecla lo dice.
- `V` — listar los volúmenes montados: cada fila muestra el punto de
  montaje, el tipo de sistema de archivos y los bytes libres/totales
  cuando el volumen los informa. `Enter` cambia la raíz de toda la sesión
  al volumen elegido; `Esc` cierra la lista. Cuando no se informa ningún
  volumen, la tecla lo dice.
- `S` — el menú de ajustes: `1` activa o desactiva la confirmación del
  borrado individual, `2` la del borrado por lotes. Los cambios se
  guardan en los ajustes propios de esta aplicación, que solo ella puede
  leer, y se cargan en el siguiente inicio; cuando no se puede contactar
  con el servicio de configuración, duran solo la sesión y el menú dice
  por qué.
- `?` — mostrar esta ayuda sobre los paneles; cualquier tecla la cierra.
- `q` — salir restaurando el terminal.

Mientras haya entradas marcadas, `c`, `m` y `d` actúan sobre todo el
conjunto marcado en lugar de la selección: `c`/`m` piden un directorio
de destino existente donde caen las entradas (`Tab` lo completa), y `d`
confirma el borrado por lotes (salvo que esa confirmación esté
desactivada en el menú de ajustes). Las entradas se procesan en orden de
marcado; un fallo nunca detiene al resto, el informe final cuenta lo que
tuvo éxito y una pantalla de informe nombra cada fallo — un lote nunca
es parcial en silencio. Las entradas con éxito se desmarcan; los fallos
siguen marcados para reintentar.

Cuando una copia o un movimiento sobrescribiría un archivo existente,
la sesión pregunta por archivo: `o` sobrescribe, `s` lo salta (un
origen saltado queda en su sitio) y `c` cancela los pasos restantes —
en un lote, cancelar abandona todas las entradas restantes —
lo ya aplicado permanece, y el informe final dice qué ocurrió. Un
fallo a mitad de copia elimina el destino a medio escribir y muestra
el error del núcleo; nada se hace pasar jamás por una copia completa.
Cada operación la autoriza el núcleo — un rechazo aparece tal cual en
la línea de mensajes sin que nada cambie.
Una copia conserva los huecos de un archivo y sus atributos
extendidos; cualquier atributo que el destino no pueda conservar se
indica en la línea de mensajes al terminar la operación.

Un directorio copiado o movido sobre un directorio de su mismo nombre se
combina con él: lo que contiene pasa al directorio que ya existe, y por cada
uno de sus archivos que ya esté allí se pregunta de la misma manera.

La cabecera de estadísticas de disco de arriba muestra la ruta listada,
los bytes libres/totales del volumen subyacente (cuando el servicio de
información del sistema puede informarlos), el número de elementos
visibles y — mientras haya algo marcado — el número de entradas
marcadas. La banda de estado bajo las ventanas muestra la ventana activa
(Tree o Files), el orden de clasificación, si se muestran las entradas
ocultas, el filtro de nombre de archivo activo, y el número de entradas
marcadas con su total de bytes. Un archivo cuyo formato de
almacenamiento no guarda fecha de modificación muestra `-` en la columna
de fecha.

Cuando el panel de archivos omite entradas ocultas, la sesión también
anota la omisión en el flujo de información estándar (fd 3): un registro
informativo por cada cambio, que se puede capturar con
`fstree 3>info.jsonl`; la sesión en sí no se ve afectada.

Los visores: `Enter` sobre un archivo regular lo abre en solo lectura en
un visor a pantalla completa. Un ejecutable reconocido — una imagen
`rxe`, un ELF de 64 bits, un módulo wasm o un manifiesto firmado
independiente — se abre en el **visor de desensamblado**; un archivo
cuyos primeros bytes no contienen NUL y son UTF-8 válido se abre en el
**paginador de texto**; todo lo demás se abre en el **volcado
hexadecimal**. `o` anula esa elección. `x` cambia el visor abierto al
volcado hexadecimal en el mismo lugar, `t` al paginador de texto
(ajustado al principio de la línea que contiene el desplazamiento
mostrado) y `d` al visor de desensamblado (un archivo que ningún formato
de contenedor reclama pide un juego de instrucciones y se descodifica
como un fragmento en bruto desde el lugar actual); `q` o `Esc` vuelve a
los paneles.

Los visores recorren el archivo en ventanas acotadas — el archivo nunca
se mantiene entero en memoria, así que un archivo de cualquier tamaño
(mucho más allá de 4 GiB) se pagina correctamente — y comparten las
teclas:

- `Up`/`Down` o `k`/`j` — una fila; `PageUp`/`PageDown`, `b`/`Space` —
  una página; `Home`/`End` — el principio / la última página.
- `g` — ir a un lugar: un número de línea contado desde 1 en el
  paginador de texto, un desplazamiento en bytes (decimal o hexadecimal
  con `0x`) en el volcado hexadecimal, una dirección en el visor de
  desensamblado.
- `/` — buscar hacia delante desde el lugar actual; `n` repite la
  última búsqueda más allá de su coincidencia anterior. La exploración
  se ejecuta en segundo plano en pasos acotados: la sesión sigue
  respondiendo con un archivo enorme, la línea de estado dice
  `searching…` y `Esc` la detiene donde está.
- `?` — esta ayuda; cualquier tecla la cierra.

El **paginador de texto** descodifica UTF-8 con un reemplazo visible para
los bytes no válidos, expande los tabuladores y muestra cualquier otro
byte de control como un `·` visible: el contenido de los archivos nunca
se envía al terminal como secuencias de escape en bruto. Las líneas
largas se ajustan por omisión; `w` activa o desactiva el ajuste (sin él,
la cola se recorta en el borde derecho). `/` busca texto literal, sin
distinguir mayúsculas de minúsculas, a través de los límites de lectura.
La línea de estado muestra el lugar como un número de línea; tras un
salto desde el volcado hexadecimal, el número de línea es desconocido
(no se pagó el coste de contar) hasta que un salto con `g` lo vuelve a
anclar.

El **volcado hexadecimal** muestra la disposición clásica: columna de
desplazamientos, dieciséis bytes como pares hexadecimales y una columna
ASCII con los bytes no imprimibles como `.`. La columna de
desplazamientos es tan ancha como el archivo necesita (al menos ocho
dígitos hexadecimales). `/` busca texto literal (sin distinguir
mayúsculas de minúsculas) o, escrito como `0x` seguido de pares de bytes
hexadecimales (`0xdeadbeef`), una secuencia exacta de bytes.

El **visor de desensamblado** se abre en una página de resumen: el
formato del contenedor, su juego de instrucciones y su punto de entrada,
sus regiones (dirección, extensión en el archivo, tamaño en memoria,
permisos) y su número de símbolos; un manifiesto firmado independiente
lista en cambio su versión de ABI y las capacidades que solicita. Una
imagen `rxe` lleva su manifiesto al lado, nunca incrustado, y el resumen
lo indica. `Up`/`Down` recorren las filas de regiones y `Enter` abre el
desensamblado de la región de código seleccionada (una región de datos
se muestra en hexadecimal sobre sus bytes del archivo). Un contenedor
que no nombra ningún juego de instrucciones — una imagen `rxe` se
ejecuta en cualquier máquina que la cargue — pregunta una vez:
`x` x86-64, `a` aarch64, `r` riscv64, `w` wasm.

El panel de código muestra una instrucción por fila — dirección, bytes
de codificación, mnemónico, operandos — con líneas de etiqueta
`<symbol>:` y destinos de salto simbolizados (`<main+0x8>`) donde el
contenedor nombra símbolos. Las instrucciones se descodifican por
pantallas, nunca todo el binario por adelantado. `g` salta a una
dirección, `/` busca en el texto de las instrucciones, `End` avanza hasta
el final de la región en segundo plano (`Esc` detiene el avance) e `I`
vuelve a descodificar con otro juego de instrucciones. `Esc` vuelve a la
página de resumen.

Toda descodificación de contenedores e instrucciones se ejecuta en un
proceso auxiliar blindado (el entorno aislado de análisis), nunca en el
propio gestor de archivos: un ejecutable malicioso puede hacer fallar al
auxiliar, no a la sesión. Un archivo que el descodificador rechaza — mal
formado, o demasiado grande para entregarlo al entorno aislado — recurre
al volcado hexadecimal con un aviso de una línea.

Una lectura que el núcleo rechaza en mitad de la visualización cierra el
visor y muestra el error en la línea de mensajes: el contenido obsoleto
nunca se presenta como actual.

## OPTIONS

- `directory` — el directorio en el que comienza la sesión; el valor por
  omisión es la vista raíz `/`.
- `-h`, `-?` — imprimir la forma corta de este documento y salir.

## EXIT STATUS

- `0` — la sesión terminó con la `q` del usuario.
- `1` — el directorio inicial no pudo listarse, o falló la vía del
  terminal. Una línea de órdenes no entendida también termina con `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
