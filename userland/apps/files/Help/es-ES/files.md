## NAME

files — navegador gráfico del sistema de archivos

## SYNOPSIS

`files [--desktop] [ruta] [-h | -?]`

## DESCRIPTION

Abre una ventana de escritorio que lista el sistema de archivos, empezando
por la carpeta que `ruta` nombra en la línea de órdenes — o, si nombra un
archivo, por la carpeta que contiene ese archivo, con el archivo
seleccionado — o, si no se nombra ninguna, por la carpeta que elija la
configuración: la carpeta `Home` del usuario que lo inicia, salvo que este
elija su carpeta `Desktop` o los volúmenes conectados (su directorio
personal si esa carpeta no puede listarse). El título de la ventana nombra
el directorio actual; la ventana lista sus entradas, cada entrada
seleccionada resaltada con el color de acento del tema activo. Cada lectura
de un directorio es un listado ordinario, sujeto a permisos, bajo la
identidad del usuario que lo inicia: un directorio ilegible se rechaza,
nunca se adivina.

El escritorio inicia el explorador por usted y lo mantiene en la barra de
iconos: el menú de su ranura lista sus propios lugares y todo lo montado, y
elegir uno abre allí una ventana; *Red* y *Configuración…* van al final. Un
clic en la ranura abre una donde indique la configuración. Pedir una carpeta
que ya tiene ventana trae esa ventana al frente en lugar de abrir otra. Esa
copia no tiene fila *Salir*: forma parte del escritorio, y cerrar sus
ventanas simplemente la retira.

*Red*, en el menú de la ranura justo encima de *Configuración…*, abre una
ventana con los ordenadores de la red local que comparten archivos. Cada uno
aparece con su nombre, la dirección en la que se le alcanza, su grupo de
trabajo o dominio y lo que es, y la lista se completa a medida que responden
los ordenadores. La red solo se explora mientras una ventana la muestra. Un
ordenador que ha dejado de responder aparece atenuado hasta que se olvida;
un nombre que mezcla alfabetos como lo necesita la imitación de otro nombre
lleva una señal de advertencia en lugar de su imagen; y cuando un ordenador
ofrece más que unas pocas entradas, el resto se cuenta en una fila propia en
vez de desplazar a los demás. Los nombres de la red los elige quien gestiona
cada ordenador, así que un nombre nunca prueba qué ordenador está mirando.
Conectarse a los recursos compartidos de un ordenador no está disponible en
este sistema, y abrir uno lo indica.

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
selección. Un doble clic abre un elemento; manteniendo `Shift`, lista el
contenido de una aplicación en lugar de iniciarla, y deja sin seleccionar
lo que abra en otro lugar. Arrastrar por un espacio vacío traza un recuadro
que selecciona todo lo que toca a medida que crece; sostenido en el borde
superior o inferior del listado, este se desplaza, y `Escape` deshace lo
que el recuadro seleccionó.

El espacio que rodea cada icono y su nombre cuenta como vacío, así que un
recuadro puede empezar en cualquier punto entre los elementos. `Ctrl+A`
selecciona todo lo del listado y `Ctrl+Shift+A` borra la selección; ambos
están también en el menú contextual.

El nombre de un elemento se muestra entero, en dos líneas cuando las
necesita; un nombre demasiado largo incluso para ellas conserva su principio
y su final, con `…` en medio, de modo que su extensión siempre se ve. Una
aplicación se muestra sin su sufijo `.app`, y la configuración puede
mostrarlo, o bien ocultar también los sufijos de los tipos de archivo que el
sistema conoce. Los elementos cuyo nombre empieza por un punto solo se
listan cuando la configuración lo pide.

`F2` cambia el nombre del elemento seleccionado en su sitio, igual que hacer
clic en el nombre del único elemento seleccionado y esperar: el nombre se
abre para editarlo en cuanto el clic ya no puede ser un doble clic. Queda
seleccionada la parte anterior a la extensión, así que lo que se escribe
sustituye al nombre y conserva la extensión; un sufijo que la ventana no
muestra también se conserva. `Intro`, o un clic fuera del nombre, conserva
el nombre nuevo —un nombre que el volumen rechaza sigue abierto con el
motivo— y `Escape` lo descarta. Un clic que ha cambiado el nombre del
elemento no hace nada más, ya que la carpeta puede haberse reordenado bajo
el puntero.

Arrastrar los elementos seleccionados a otra ventana del gestor de archivos,
a una carpeta de ella o al escritorio los copia allí; manteniendo Mayús, en
cambio, los mueve. El puntero muestra un signo más mientras soltar copiaría
y una flecha mientras movería, y la carpeta donde caería lo soltado aparece
resaltada. Un solo archivo arrastrado a la ranura de una aplicación en la
barra de iconos se abre allí.
Copiar o mover conserva los huecos de cada archivo y sus atributos
extendidos, como un tipo de archivo de RISC OS; cualquier atributo que el
destino no pueda conservar se indica en un aviso al terminar la operación.

El menú contextual empieza por *Mostrar*. Debajo, un submenú nombra aquello
sobre lo que actúa el menú — *Archivo «notes.txt»*, con un nombre largo
acortado por el medio para que su extensión siga viéndose, o *Selección*
cuando hay varios elementos seleccionados — y contiene *Abrir*, *Abrir con…*,
*Obtener información…*, *Renombrar*, *Cortar*, *Copiar* y *Eliminar*; una
fila que no puede actuar ahí aparece atenuada y dice por qué. Le siguen
*Nuevo*, luego *Seleccionar todo* y *Borrar selección*, y por último *Pegar*.
Un clic derecho en un espacio vacío borra la selección, así que el submenú
del elemento aparece atenuado.

El submenú *Nuevo* del menú contextual crea una carpeta, o un documento
vacío de cada tipo que escribe un editor instalado, y abre su nombre para
editarlo.

El submenú *Mostrar* del menú contextual presenta los elementos como iconos
grandes, la opción predeterminada; como iconos pequeños, cada nombre en una
línea junto a su imagen; o como lista, una fila por elemento con su imagen,
su tamaño, la fecha de su último cambio y su tipo. Una unidad que aparezca
entre ellos muestra en su lugar su espacio libre, su sistema de archivos y
lo bien que funciona. Debajo de las vistas, ordena la ventana por nombre,
tipo, tamaño o fecha del último cambio de cada elemento — o, en una lista de
unidades, por nombre, espacio libre, sistema de archivos o estado — de forma
ascendente o descendente. Un clic en el título de una columna también ordena
la lista por ella, y un segundo clic invierte el orden. Las carpetas quedan
arriba sea cual sea el orden elegido, salvo que la configuración las coloque
entre los archivos.

*Configuración…* en el menú de la ranura de la barra de iconos, o `Ctrl+,`,
abre la configuración del explorador: dónde se abre una ventana nueva, cómo
muestra y ordena sus elementos, y si muestra la columna de lugares y la
barra de herramientas; si las carpetas quedan arriba; qué sufijos de nombre
y qué elementos se ocultan; si una imagen muestra su propio contenido y una
carpeta lo que contiene; y si mover elementos a la Papelera pide
confirmación antes. *Restaurar valores predeterminados* restablece todos los
ajustes. Un cambio se aplica a todas las ventanas a la vez y se recuerda;
una ventana ya abierta conserva la vista y el orden que tiene.

`Alt+Enter` abre la ventana de *información* del elemento seleccionado,
igual que *Obtener información…* en su submenú del menú contextual. Es una ventana propia,
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

El operando `ruta` se trata como entrada no confiable: debe ser una ruta
absoluta dentro del límite de longitud de ruta del sistema, y cada uno de
sus componentes debe ser un nombre real — `.` y `..` no lo son, de modo
que una escritura nunca puede significar un lugar distinto del que se
lee. Una ruta que incumpla alguna de esas reglas, o que el usuario que lo
lanzó no pueda listar, se rechaza con el motivo por el flujo de error
estándar y la ventana se abre en la carpeta `Home`, de modo que un
argumento incorrecto nunca deja al usuario sin ventana. Un segundo
operando se rechaza de plano en lugar de ignorarse.

## OPTIONS

- `--desktop` — ejecutarse como el componente de gestor de archivos propio
  del escritorio: una ranura permanente en la barra de iconos que ofrece sus
  lugares y los volúmenes montados, ninguna ventana hasta que se pida una, y
  ninguna forma de salir. La sesión de escritorio pasa esta opción al
  arrancar; nombrar una `ruta` junto a ella se rechaza, porque un componente
  no abre ninguna ventana en la que ponerla.
- `-h, -?` — mostrar la ayuda corta de esta orden y salir.

## EXIT STATUS

Cero tras un cierre limpio, o tras mostrarse la ayuda corta; `2` cuando
la línea de órdenes no se entendió; por lo demás, distinto de cero
cuando el canal de ventana, la región de fotogramas compartida o el
listado inicial del directorio fue rechazado (el motivo se indica por
el flujo de error estándar).
