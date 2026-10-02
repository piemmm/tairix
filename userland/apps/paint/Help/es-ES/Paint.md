## NAME

Paint — editor gráfico de imágenes y sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Pinta y edita imágenes en una ventana de escritorio, píxel a píxel o con
pinceles y formas. Iniciado con un documento — desde el gestor de archivos,
desde el escritorio o soltando un archivo sobre su icono en la barra de
iconos — abre una ventana sobre él. Iniciado por sí solo, abre una imagen
nueva en blanco. Cada documento es una ventana del único programa; cerrar
la última lo deja en la barra de iconos, y la fila Salir de su menú de
icono lo termina.

Abre todos los formatos de imagen que lee el sistema: PNG, JPEG, GIF, BMP,
TIFF, WebP, iconos de Windows y archivos de sprites de RISC OS. Escribe
PNG, JPEG y archivos de sprites; una imagen leída de cualquier otro formato
se guarda como un archivo nuevo. Un PNG conserva su paleta, y un JPEG se
escribe con la calidad fijada en Calidad JPEG del menú Archivo.

Un archivo de sprites contiene cualquier número de sprites, cada uno con su
nombre, modo de pantalla, paleta y máscara. Cada profundidad se edita tal
como se almacena: 2, 4, 16 y 256 colores y millones de colores. Un sprite
sin paleta propia muestra los colores del escritorio de RISC OS — con 16
colores, el color n es el color Wimp n; con 2 colores, los colores Wimp 0
y 7; con 4 colores, los colores Wimp 0, 2, 4 y 7; con 256 colores, la
disposición de tintes de RISC OS — nunca una paleta de PC. Un sprite cuyos
píxeles son más altos que anchos, como en el modo 12, se muestra así. Un
sprite que este editor no sabe leer, como uno CMYK, se conserva tal cual y
se vuelve a guardar sin cambios. El menú Sprites va a un sprite, añade,
copia, cambia el nombre, borra y reordena sprites.

El botón principal (izquierdo) pinta con el color principal y el botón
central con el color secundario; manteniendo Alt se toma un color en su
lugar. Las herramientas son selección, lápiz, pincel, aerógrafo, goma,
relleno, cuentagotas, línea, rectángulo y elipse; el panel junto a la
imagen contiene los dos colores, la paleta de la imagen o los colores del
escritorio, y los ajustes de la herramienta en uso. Pulsar un pozo de color
edita ese color; hacer doble clic en un color de la paleta edita la
paleta. Manteniendo Mayús se dibuja un cuadrado, un círculo o una línea en
múltiplos de 45 grados.

Con la herramienta de selección, arrastre para marcar parte de la imagen y
luego arrastre la selección para moverla; flota hasta que se deposita, y
moverla es un único cambio que deshacer. Las imágenes copiadas viajan por
el portapapeles como PNG, y lo pegado flota hasta que se deposita.

El programa no tiene ninguna capacidad sobre el sistema de archivos. Solo
edita el archivo que se le entregó. Un archivo que el usuario puede cambiar
se entrega con permiso de escritura, y Guardar lo reescribe; cualquier otro
es de solo lectura, y Guardar pregunta dónde guardar una copia. Las
imágenes, también las pegadas desde el portapapeles, se decodifican en un
proceso de trabajo aparte sin ningún alcance, y cada documento recibe uno
nuevo: un archivo hostil no puede tocar nada de lo que el programa puede.

Pulsar el botón secundario (derecho) del ratón en cualquier parte de la
ventana abre su menú: Cortar, Copiar, Pegar, Seleccionar todo y
Deseleccionar, y luego Archivo, Edición, Imagen, Colores, Sprites, Vista y
Herramientas, cada uno con su submenú. La ventana no tiene barra de menús.
Cerrar una ventana o salir con cambios sin guardar pregunta primero.

* `Ctrl+N` — una imagen nueva; `Ctrl+O` — abrir un archivo
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — cerrar la ventana
* `Ctrl+Z` — deshacer; `Ctrl+Shift+Z` o `Ctrl+Y` — rehacer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, pegar
* `Ctrl+A` — seleccionarlo todo; `Ctrl+D` — deseleccionar
* `Enter` — depositar una selección flotante; `Escape` — devolverla
* `Delete` — borrar la selección
* `Ctrl+Shift+X` — recortar a la selección
* `Ctrl+R` — cambiar el tamaño; `Ctrl+Shift+R` — tamaño del lienzo
* `Ctrl+[` / `Ctrl+]` — girar a la izquierda o a la derecha
* `Ctrl+I` — invertir los colores
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — las herramientas, en orden
* `X` — intercambiar los colores principal y secundario
* `+` / `-` — acercar o alejar; `1` — tamaño real; `Ctrl+0` — ajustar
* `Ctrl` + rueda — acercar o alejar en torno al puntero
* Pellizcar con dos dedos — acercar o alejar con suavidad; en una pantalla táctil la imagen sigue a los dedos
* `G` — mostrar u ocultar la cuadrícula entre píxeles
* `Page Up` / `Page Down` — el sprite anterior o siguiente
* teclas de flecha — mover una selección flotante un píxel; con `Shift`, diez

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero tras Salir. Distinto de cero cuando se rechazó el canal de ventana,
el buzón de eventos o la sesión de escritorio; el motivo se indica en la
salida de error estándar.
