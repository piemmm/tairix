## NAME

files — navegador gráfico del sistema de archivos

## SYNOPSIS

`files [--desktop] [directorio] [-h | -?]`

## DESCRIPTION

Abre una ventana de escritorio que lista el sistema de archivos, empezando
por el `directorio` nombrado en la línea de órdenes o, si no se nombra
ninguno, por la carpeta `UserFiles` del usuario que lo inicia (su directorio
personal si esa carpeta no puede listarse). El título de la ventana nombra
el directorio actual; la ventana lista sus entradas, cada entrada
seleccionada resaltada con el color de acento del tema activo. Cada lectura
de un directorio es un listado ordinario, sujeto a permisos, bajo la
identidad del usuario que lo inicia: un directorio ilegible se rechaza,
nunca se adivina.

El escritorio inicia el explorador por usted y lo mantiene en la barra de
iconos: el menú de su ranura lista sus propios lugares y todo lo montado, y
elegir uno abre allí una ventana. Un clic en la ranura abre una en su
carpeta `UserFiles`. Pedir una carpeta que ya tiene ventana trae esa ventana
al frente en lugar de abrir otra. Esa copia no tiene fila *Salir*: forma
parte del escritorio, y cerrar sus ventanas simplemente la retira.

Iniciado por su nombre desde un shell (o abierto sobre una carpeta desde el
escritorio), es en cambio una aplicación ordinaria: una ventana, y termina
cuando usted la cierra. En cualquier caso requiere una sesión gráfica en
marcha: sin ella el canal de ventanas es inalcanzable, y el explorador
informa del rechazo en el flujo de error estándar y termina.

La ventana se maneja con el teclado: `Abajo` y `Arriba` mueven la selección,
`Intro` abre el directorio seleccionado y `Retroceso` sube al directorio
padre. `F5` vuelve a leer tanto el listado como la columna de lugares; un
volumen recién conectado aparece en ella por sí solo. `Ctrl+Shift+N` crea
una carpeta nueva.

Un listado se abre sin nada seleccionado. Un clic selecciona un elemento, un
clic con `Ctrl` añade o quita uno, y un clic con `Shift` selecciona la serie
desde el último elemento elegido; un clic en un espacio vacío borra la
selección. Arrastrar por un espacio vacío traza un recuadro que selecciona
todo lo que toca a medida que crece; sostenido en el borde superior o
inferior del listado, este se desplaza, y `Escape` deshace lo que el
recuadro seleccionó.

El espacio que rodea cada icono y su nombre cuenta como vacío, así que un
recuadro puede empezar en cualquier punto entre los elementos. `Ctrl+A`
selecciona todo lo del listado y `Ctrl+Shift+A` borra la selección; ambos
están también en el menú contextual.

El nombre de un elemento se muestra entero, en dos líneas cuando las
necesita; un nombre demasiado largo incluso para ellas conserva su principio
y su final, con `…` en medio, de modo que su extensión siempre se ve.

`F2` cambia el nombre del elemento seleccionado en su sitio, igual que hacer
clic en el nombre del único elemento seleccionado y esperar: el nombre se
abre para editarlo en cuanto el clic ya no puede ser un doble clic. Queda
seleccionada la parte anterior a la extensión, así que lo que se escribe
sustituye al nombre y conserva la extensión. `Intro`, o un clic fuera del
nombre, conserva el nombre nuevo —un nombre que el volumen rechaza sigue
abierto con el motivo— y `Escape` lo descarta.

Arrastrar los elementos seleccionados a otra ventana del gestor de archivos,
a una carpeta de ella o al escritorio los copia allí; manteniendo Mayús, en
cambio, los mueve. El puntero muestra un signo más mientras soltar copiaría
y una flecha mientras movería, y la carpeta donde caería lo soltado aparece
resaltada. Un solo archivo arrastrado a la ranura de una aplicación en la
barra de iconos se abre allí.

El submenú *Nuevo* del menú contextual crea una carpeta, o un documento
vacío de cada tipo que escribe un editor instalado, y abre su nombre para
editarlo.

`Alt+Enter` abre una ventana de *Propiedades* del elemento seleccionado,
igual que la fila *Propiedades* del menú contextual. Es una ventana propia,
así que pueden abrirse varias a la vez y el listado sigue usable mientras
tanto: muestra qué es el elemento, su tamaño, sus marcas de tiempo, adónde
apunta un alias, sus permisos y su propietario, y los atributos extendidos
que el volumen guarda para él. Permisos, propietario y atributos pueden
cambiarse allí, cada uno como una escritura ordinaria, sujeta a permisos,
bajo su propia identidad: un rechazo dice por qué y no cambia nada.
Reasignar un propietario requiere la capacidad `CAP_FS_CHOWN`; una sesión
sin ella ve el propietario y el grupo marcados con un candado y una línea
que explica por qué.

`Izquierda` y `Derecha` se mueven entre las secciones de la ventana. En
*Permisos*, `Abajo` o `Tab` entra en sus controles: las flechas se mueven
entre ellos, `Space` activa o desactiva un permiso o abre el propietario o
el grupo para editarlo, y `Tab` o `Escape` vuelve a las secciones.

El operando `directorio` se trata como entrada no confiable: debe ser
una ruta absoluta dentro del límite de longitud de ruta del sistema, y
cada uno de sus componentes debe ser un nombre de directorio real —
`.` y `..` no lo son, de modo que una escritura nunca puede significar
un lugar distinto del que se lee. Un directorio que incumpla alguna de
esas reglas, o que el usuario que lo lanzó no pueda listar, se rechaza
con el motivo por el flujo de error estándar y la ventana se abre en la
carpeta `UserFiles`, de modo que un argumento incorrecto nunca deja al
usuario sin ventana. Un segundo operando se rechaza de plano en lugar
de ignorarse.

## OPTIONS

- `--desktop` — ejecutarse como el componente de gestor de archivos propio
  del escritorio: una ranura permanente en la barra de iconos que ofrece sus
  lugares y los volúmenes montados, ninguna ventana hasta que se pida una, y
  ninguna forma de salir. La sesión de escritorio pasa esta opción al
  arrancar; nombrar un `directorio` junto a ella se rechaza, porque un componente
  no abre ninguna ventana en la que ponerlo.
- `-h, -?` — mostrar la ayuda corta de esta orden y salir.

## EXIT STATUS

Cero tras un cierre limpio, o tras mostrarse la ayuda corta; `2` cuando
la línea de órdenes no se entendió; por lo demás, distinto de cero
cuando el canal de ventana, la región de fotogramas compartida o el
listado inicial del directorio fue rechazado (el motivo se indica por
el flujo de error estándar).
