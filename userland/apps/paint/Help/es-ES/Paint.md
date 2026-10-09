## NAME

Paint — editor gráfico de imágenes y sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Pinta y edita imágenes en una ventana del escritorio, píxel a píxel o con
pinceles y formas. Lanzado con un documento — desde el gestor de archivos,
desde el escritorio o soltando un archivo sobre su icono en la barra de
iconos — abre una ventana sobre él. Lanzado solo, abre una imagen nueva en
blanco. Cada documento es una ventana del único programa; cerrar la última
lo deja en la barra de iconos, y la fila Salir de su menú de icono lo
termina.

Abre todos los formatos de imagen que lee el sistema: PNG, JPEG, GIF, BMP,
TIFF, WebP, iconos de Windows, archivos de sprites de RISC OS y OpenRaster.
Escribe PNG, JPEG, GIF, BMP, TIFF, archivos de sprites y OpenRaster; una
imagen leída de cualquier otro formato, o de un archivo que guarda más que
su imagen, como un perfil de color, se guarda como archivo nuevo. Imagen
nueva pregunta primero para qué formato es la imagen y ofrece los colores
que ese formato admite. Guardar como pregunta el formato y sus propios
ajustes — la calidad de un JPEG, si un GIF va entrelazado, la compresión de
un TIFF — y dice lo que el formato no puede conservar, antes de preguntar
dónde. Una paleta se conserva allí donde el formato la admite, y también la
densidad de una imagen.

Una imagen puede estar hecha de capas, la de abajo primero, cada una con un
nombre, una opacidad y si se muestra; se pinta en una capa cada vez, y la
ventana las muestra superpuestas. OpenRaster conserva las capas; cualquier
otro formato las recibe superpuestas. El menú Capas añade, copia, elimina,
sube y baja capas, combina una con la de debajo y acopla la imagen, y sus
Propiedades de capa cambian el nombre de una capa y fijan cuánto se ve de
ella. Los ajustes, rellenos y trazos cambian la capa en que se pinta; los
giros, volteos, cambios de tamaño y recortes cambian todas las capas. Una
imagen con paleta tiene una sola capa.

Un archivo de sprites guarda cualquier número de sprites, cada uno con su
nombre, modo de pantalla, paleta y máscara. Cada profundidad se edita tal
como se guarda: 2, 4, 16 y 256 colores y millones de colores. Un sprite sin
paleta propia muestra los colores del escritorio de RISC OS — con 16
colores, el color n es el color Wimp n; con 2 colores, los colores Wimp 0 y
7; con 4 colores, los colores Wimp 0, 2, 4 y 7; con 256 colores, la
disposición de tintes de RISC OS — nunca una paleta de PC. Un sprite cuyos
píxeles son más altos que anchos, como en el modo 12, se muestra así. Un
sprite que este editor no puede leer, como uno CMYK, se conserva tal cual y
se vuelve a escribir sin cambios. El menú Sprites va a sprites, los añade,
copia, renombra, elimina y reordena. Un TIFF guarda cualquier número de
páginas, y para él el menú Páginas va a páginas, las añade, copia, elimina y
reordena.

El botón principal (izquierdo) pinta con el color principal y el botón
central con el color secundario; manteniendo Alt se toma un color en su
lugar, tal como lo muestran las capas. La caja de herramientas, en el panel
Herramientas, reúne las herramientas en dos columnas: selección, lápiz,
pincel, aerógrafo, goma, clonar, relleno, degradado, cuentagotas, texto,
línea, rectángulo, elipse, polígono, recortar, mano y zoom. La barra de
arriba nombra la herramienta en uso y guarda sus ajustes — el tamaño, la
dureza, la opacidad, el flujo y el espaciado de un pincel, la tolerancia de
un relleno, la forma de un degradado, el tamaño del texto, las esquinas de un
rectángulo — tecleados o ajustados con las flechas, y los botones que amplían
y muestran la cuadrícula de píxeles; la tira de paleta bajo la imagen guarda
la paleta de la imagen, o los colores del escritorio. El aerógrafo sigue
rociando mientras se mantiene quieto. Manteniendo Mayús se dibuja un
cuadrado, un círculo o una línea en un múltiplo de 45 grados.

A ambos lados de la ventana corren paneles: de forma predeterminada el panel
Herramientas a la izquierda y el panel Color a la derecha, con el panel Ajuste
debajo en cuanto se abre un ajuste. Cada uno lleva arriba una banda fina que
lo nombra, con un control que lo enrolla sobre su banda y una marca que lo
cierra; Ver ▸ Paneles vuelve a mostrar un panel cerrado, y Restablecer paneles
devuelve cada panel a como lo tiene una ventana nueva. Arrastrar una banda
mueve su panel en su lado o al otro, marcando por el camino dónde caerá;
soltado lejos de ambos lados, o arrastrado fuera de la ventana, el panel flota
en una ventanita propia, que se mueve por su banda, se mantiene sobre la
imagen y se cierra con su marca; arrastrado de nuevo sobre un lado, vuelve a
acoplarse allí.

La herramienta de selección marca un rectángulo, una elipse, un lazo a mano
alzada, un polígono pulsado esquina a esquina o, con la varita mágica, los
píxeles unidos a uno por colores parecidos. Mayús añade a la selección, Alt
le quita, y ambas conservan solo lo que comparten; Difuminar suaviza su
borde. Mientras hay una selección, toda herramienta, relleno y ajuste se
limita a ella. Arrastrar dentro la levanta y la mueve: flota hasta que se
deposita, y moverla es un solo cambio que deshacer. Las imágenes copiadas
viajan por el portapapeles como PNG, y lo que se pega flota hasta que se
deposita.

La herramienta de clonar pinta lo que hay en otro lugar de la imagen: Alt y
clic donde copiar, y luego pintar. La herramienta de degradado funde el
color principal en el secundario a lo largo de un arrastre, en bandas o en
anillos. La herramienta de texto pone las palabras tecleadas donde se pulsa;
Intro empieza una línea nueva, otro clic u otra herramienta las deposita, y
Escape las descarta. Las esquinas de la herramienta de polígono se pulsan
por turno, y un clic en la primera o Intro lo cierra. La herramienta de
recortar marca la parte que queda, sus tiradores mueven sus bordes, e Intro
recorta. La mano arrastra la imagen por la ventana, como Espacio con
cualquier herramienta; el zoom amplía con un clic, o con Alt reduce, y un
recuadro arrastrado llena la ventana.

El menú Ajustes abre un ajuste en el panel Ajuste, donde cualquier otra
herramienta, panel y menú sigue a mano: brillo y contraste, tono y saturación,
equilibrio de color, niveles, curvas, balance de blancos, posterizar, umbral,
desenfocar, enfocar, pixelar y añadir ruido; desaturar y buscar bordes, que no
tienen ajustes, se aplican al momento. La imagen muestra el ajuste a medida
que se mueven sus valores, Vista previa lo apaga y lo enciende para comparar,
Restablecer devuelve sus valores y Aplicar lo conserva como un solo cambio que
se puede deshacer; pintar, rellenar o elegir otro ajuste lo aplica antes. Los
niveles fijan los puntos negro, gris y blanco sobre un histograma de la capa,
para todos los canales juntos o cada uno por separado, con cuentagotas que los
toman de la imagen y Auto; las curvas doblan los tonos de un canal mediante
puntos arrastrados sobre su histograma; el balance de blancos fija la
temperatura y el matiz de la luz, a partir de un píxel neutro elegido o con
Auto; tono y saturación giran, refuerzan y aclaran todos los colores o una
gama de ellos; el equilibrio de color lleva las sombras, los medios tonos y
las luces hacia el rojo, el verde o el azul, conservando su luminosidad si se
pide. Todo queda limitado a la selección. En una imagen con paleta, un ajuste
cambia su paleta, y los que necesitan colores vecinos no se ofrecen.

El panel Color guarda los colores principal y secundario y un selector de
color para el que esté elegido: un clic en un color lo elige, y luego se toma
en un cuadrado de saturación y valor junto a una tira de tonos, en una rueda
de tonos alrededor de un triángulo, o en un deslizador por canal, y se teclea
en RGB, HSV, HSL, CMYK, Lab, LCh o como gris, o por su escritura hexadecimal
y, donde la imagen admite transparencia, su opacidad. Un color Lab o LCh que
la pantalla no puede mostrar se muestra lo más cerca posible, y se marca.
Intercambiar cambia entre sí los dos colores, Restablecer los vuelve negro y
blanco, y Tomar coge el siguiente color en el que se haga clic en la imagen.
El color que tenía queda al lado, un clic lo recupera, y los últimos colores
elegidos esperan debajo para elegirse de nuevo. En una imagen con paleta, los
colores son sus entradas, así que el selector edita la paleta, y cada edición
es un solo cambio que se puede deshacer.

Ajustes, en el menú de la barra de iconos, abre la ventana de ajustes de
Paint: la herramienta con la que empieza una ventana nueva y si una imagen se
abre ajustada a la ventana o a tamaño real; el tamaño, el formato, los colores
y el fondo que ofrece Imagen nueva; el espaciado, el desplazamiento, el color,
la opacidad y el estilo de la cuadrícula — líneas, guiones, puntos o cruces —,
si una ventana nueva la muestra, el ajuste a ella y el zoom desde el que se
muestra la cuadrícula entre píxeles; el tamaño y los tonos del damero y lo que
rodea la imagen; y los paneles con los que se abre una ventana nueva. Un cambio
se aplica al momento a cada ventana y se guarda para la próxima vez;
Restablecer valores predeterminados los devuelve todos. Ver ▸ Cuadrícula
muestra la cuadrícula de una ventana. Mientras se ve y el ajuste está activo,
las formas, las selecciones y los marcos de recorte cubren celdas enteras, los
extremos de una línea y de un degradado y las esquinas de un polígono caen en
sus cruces, y una selección arrastrada cae con su esquina en uno.

El programa no tiene ningún permiso sobre el sistema de archivos. Solo
edita el archivo que se le entregó. Un archivo que el usuario puede cambiar
se entrega con escritura, y Guardar lo reescribe; cualquier otro es de solo
lectura, y Guardar pregunta dónde guardar una copia. Las imágenes, y las
pegadas desde el portapapeles, se descodifican en un proceso de trabajo
aparte sin ningún alcance, y cada documento recibe uno nuevo: un archivo
hostil no puede tocar nada de lo que alcanza el programa.

Pulsar el botón secundario (derecho) del ratón en cualquier parte de la
ventana abre su menú: Cortar, Copiar, Pegar, Seleccionar todo y
Deseleccionar, y después Archivo, Edición, Imagen, Capas, Colores, Ajustes,
Sprites o Páginas, Ver y Herramientas, cada uno con su propio submenú. La
ventana no tiene barra de menús. Cerrar una ventana o salir con cambios sin
guardar pregunta primero.

* `Ctrl+N` — una imagen nueva; `Ctrl+O` — abrir un archivo
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — cerrar la ventana
* `Ctrl+Z` — deshacer; `Ctrl+Shift+Z` o `Ctrl+Y` — rehacer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, pegar
* `Ctrl+A` — seleccionar todo; `Ctrl+D` — deseleccionar
* `Enter` — depositar una selección flotante, cerrar un polígono o recortar; `Escape` — echarse atrás
* `Delete` — borrar la selección; `Alt+Backspace` — rellenarla con el color principal
* `Ctrl+Shift+X` — recortar a la selección
* `Ctrl+R` — cambiar el tamaño; `Ctrl+Shift+R` — tamaño del lienzo
* `Ctrl+[` / `Ctrl+]` — girar a la izquierda o a la derecha
* `Ctrl+I` — invertir los colores
* `Ctrl+Shift+N` — una capa nueva; `Ctrl+E` — combinar hacia abajo; `Ctrl+Shift+E` — acoplar
* `Ctrl+Page Up` / `Ctrl+Page Down` — pintar en la capa de arriba o de abajo
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — subir o bajar la capa
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — las herramientas, en orden
* `Space` — mantenido, arrastrar la imagen con cualquier herramienta
* `X` — intercambiar los colores principal y secundario
* `Tab` — por los ajustes de la herramienta, la tira de paleta y el panel de color; `Shift+Tab` — hacia atrás; `Escape` — de vuelta a la imagen
* `+` / `-` — acercar o alejar; `1` — tamaño real; `Ctrl+0` — ajustar
* `Ctrl` + rueda — acercar o alejar en torno al puntero
* Pellizcar con dos dedos — acercar o alejar con suavidad; en una pantalla táctil la imagen sigue a los dedos
* `G` — mostrar u ocultar la cuadrícula entre píxeles
* `Ctrl+'` — mostrar u ocultar la cuadrícula
* `Page Up` / `Page Down` — el sprite o la página anterior o siguiente
* teclas de flecha — mover una selección flotante un píxel; con `Shift`, diez; en la tira de paleta, recorrer sus colores

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero tras Salir. Distinto de cero cuando se rechazó el canal de ventana, el
buzón de eventos o la sesión del escritorio; el motivo se indica en la
salida de error estándar.
