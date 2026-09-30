## NAME

TextEdit — editor gráfico de texto y hexadecimal

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Edita cualquier archivo en una ventana de escritorio: texto, código fuente,
los archivos de ajustes del sistema o bytes sin procesar. Iniciado con un
documento — desde el gestor de archivos, desde el escritorio o al soltar un
archivo sobre su icono en la barra de iconos — abre una ventana sobre ese
archivo. Iniciado por sí solo abre una ventana vacía. Cada documento es una
ventana del único editor; al cerrar la última permanece en la barra de
iconos, y la fila «Salir» del menú de su icono lo termina.

Nada de lo que contiene un archivo queda oculto. Un byte de control se
muestra como `[x03]`, un byte que no es UTF-8 válido como `[xC3]`, y un
carácter invisible o que cambia la dirección de escritura como `[U+202E]`,
cada uno en su propio color y cada uno un solo paso del cursor. Un archivo
que parece contener datos binarios se abre en la vista hexadecimal, que
muestra cada byte como dos dígitos hexadecimales junto a su carácter y edita
los mismos bytes que la vista de texto.

El código fuente se colorea: HTML, XML y SVG, CSS, JavaScript, JSON, YAML,
TOML, Markdown, Rust, C, Java, Python y scripts de shell. Los archivos de
ajustes del sistema — ajustes de aplicaciones, la biblioteca de programas,
la configuración del sistema y de la red, las sustituciones de servicios,
las bases de datos de usuarios y de grupos, y los manifiestos de familias
tipográficas — también se colorean y se comprueban con el analizador con el
que el sistema los lee: un problema se marca en el margen junto a su línea y
se indica en la línea de estado. El formato se elige por el nombre del
archivo y, si no, por sus primeros bytes; un formato elegido en el menú Ver
o en la línea de estado siempre prevalece.

El editor no posee ninguna capacidad sobre el sistema de archivos. Solo edita
el archivo que se le entregó. Un archivo que el usuario puede modificar se
entrega con permiso de escritura, y Guardar lo escribe de vuelta; cualquier
otro es de solo lectura, y Guardar pregunta dónde guardar una copia. El
coloreado, la detección del formato y la comprobación se ejecutan en un
proceso de trabajo aparte sin ningún acceso, de modo que un archivo hostil no
puede alcanzar nada de lo que alcanza el editor.

Al pulsar el botón secundario (derecho) del ratón en cualquier lugar de la
ventana se abre su menú: Cortar, Copiar, Pegar y Seleccionar todo, y después
Archivo, Edición, Buscar y Ver, cada uno de los cuales abre su propio
submenú. La ventana no tiene barra de menús.

La línea de estado muestra la línea y la columna del cursor, lo que encontró
la comprobación y, como campos que abren un menú al pulsarlos: el formato,
texto o hexadecimal, los finales de línea y la sangría. Cerrar una ventana o
salir con cambios sin guardar pregunta antes.

* `Ctrl+N` — una ventana nueva
* `Ctrl+O` — abrir un archivo
* `Ctrl+S` — guardar; `Ctrl+Shift+S` — guardar como
* `Ctrl+W` — cerrar la ventana
* `Ctrl+Z` — deshacer; `Ctrl+Shift+Z` o `Ctrl+Y` — rehacer
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — cortar, copiar, pegar
* `Ctrl+A` — seleccionar todo
* `Ctrl+F` — buscar; `Ctrl+H` — reemplazar
* `F3` / `Shift+F3` — la coincidencia siguiente o anterior
* `Ctrl+L` — ir a una línea
* `F8` — el siguiente problema que encontró la comprobación
* `Ctrl+]` / `Ctrl+[` — aumentar o reducir la sangría de las líneas seleccionadas
* `Ctrl+/` — convertir en comentario las líneas seleccionadas o deshacerlo
* `Ctrl+Shift+H` — alternar entre la vista de texto y la hexadecimal
* `Insert` — alternar entre insertar y sobrescribir
* `Tab` — en la vista hexadecimal, pasar entre las columnas hexadecimal y de caracteres

## OPTIONS

`-h`, `-?`, `--help`
: Escribir esta ayuda en la salida estándar y terminar.

## EXIT STATUS

Cero tras Salir. Distinto de cero cuando se rechazó el canal de ventana, el
buzón de eventos o la sesión de escritorio; el motivo se declara en la salida
de error estándar.
