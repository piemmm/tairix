## NAME

PCEmulator — emulador de máquinas IBM PC y de la clase XT

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Ejecuta PC de la época del 8088 al 80186 en ventanas del escritorio: de cada
máquina, su procesador, su tarjeta de vídeo, sus discos, su sonido y sus
puertos, a la velocidad que tenía la máquina.

La ventana de la biblioteca muestra cada máquina como una ficha con su última
pantalla. Nueva crea una máquina a partir de uno de cuatro modelos —el IBM PC,
el IBM PC XT, un XT turbo y una estación de trabajo 80186— con el nombre
pedido y, en un modelo con controladora de disco duro, con un disco duro de
30 MB que arranca TAIRiX DOS. Iniciar, o un doble clic, inicia la máquina
elegida en una ventana propia, o trae su ventana al frente; varias máquinas
funcionan a la vez. Ajustes cambia el hardware de una máquina mientras está
apagada, Duplicar copia una máquina con sus discos, y Eliminar elimina una
con sus discos una vez confirmado. La primera vez que se inicia, PCEmulator
crea un XT turbo. Lo que una máquina escribe en sus discos, y la hora a la que
está puesto su reloj, se conservan de una ejecución a la siguiente.

El XT turbo y la estación de trabajo 80186 tienen un coprocesador numérico
8087, y Ajustes pone uno en cualquier máquina o lo quita. Un programa que
espera al coprocesador sin comprobar antes que lo hay se detiene en una
máquina que no lo tiene, como ocurría en el hardware real.

La imagen es la del monitor, cuatro unidades de ancho por cada tres de alto, y
cada punto de la tarjeta de vídeo conserva sus bordes nítidos sea cual sea el
tamaño de la ventana. Cerrar la ventana de una máquina la suspende: al
arrancarla de nuevo, sigue donde se detuvo, con los disquetes fuera de sus
unidades y el reloj adelantado el tiempo que estuvo ausente. El hardware de
una máquina suspendida se cambia una vez arrancada y apagada.

Al pie de la ventana de una máquina, cada una de sus unidades tiene un
piloto que se enciende mientras la unidad trabaja. Pulsar ahí una
disquetera ofrece Insertar disco, que elige una imagen de disquete para la
unidad, y Expulsar. Una imagen de disquete soltada sobre la ventana entra en
la unidad sobre la que se suelta, o si no en la primera, y allí no se puede
escribir.

Al final de esa barra, el menú de la máquina la pausa, la reinicia, le envía
Ctrl+Alt+Supr y fija la velocidad de su procesador durante la sesión.
Visualización enciende y apaga las líneas de barrido, el resplandor, la
persistencia del fósforo, el ruido y la ondulación de la imagen, y la máquina
conserva lo elegido. Hacer captura guarda la pantalla de la máquina como
imagen PNG donde indica el panel Guardar. Pantalla completa llena la pantalla
con la ventana y deja la barra en su sitio, donde el mismo menú la devuelve.
Apagar apaga la máquina y conserva solo su última pantalla.

Copiar el texto de la pantalla pone en el portapapeles el texto que muestra la
pantalla de la máquina, mientras muestra texto. Pegar escribe el texto del
portapapeles con el teclado de la máquina, cada carácter una vez que el
programa ha leído el anterior. Lo que la máquina no puede escribir tal cual —
las caras y las flechas del PC, o una letra que no conoce — se omite, y la
ventana lo dice. Dejar de pegar, o una tecla pulsada en la ventana de la
máquina, detiene la escritura.

Compartir una carpeta presta a la máquina una carpeta suya, elegida en el
selector, como una unidad propia: la primera letra que dejan libre sus discos,
una vez arrancado TAIRiX DOS. Un nombre que DOS no puede contener se acorta,
como Windows acorta un nombre largo, y una carpeta compartida solo para
lectura, o un archivo que usted no puede cambiar, no puede cambiarse desde la
máquina. Cada carpeta compartida tiene un sitio en la barra que nombra su
unidad; pulsarlo ofrece Dejar de compartir. Una máquina comparte hasta cuatro
carpetas, y ninguna una vez suspendida o apagada.

Depurador, en el menú de la máquina, abre una ventana sobre la máquina: sus
registros, su 8087, su código y su memoria tal como estaban al detenerse, y
sus puntos de interrupción. Interrumpir detiene la máquina donde está;
Continuar, Paso a paso, Saltar paso y Salir del procedimiento la ponen de
nuevo en marcha, como F5, F11, F10 y Mayús+F11, y F5 también detiene una
máquina en marcha. Pulsar junto a una línea de código pone ahí un punto de
interrupción o lo quita, como hace F9 con la línea actual, y Añadir punto de
interrupción detiene en cambio la máquina cuando escribe o lee la memoria, lee
o escribe un puerto o entra en una interrupción. El tiempo de la máquina se
queda quieto mientras está detenida, y cerrar el depurador la deja seguir.

Mientras la ventana de una máquina tiene el teclado, cada tecla llega a la
máquina como una tecla de su propio teclado, incluidas Ctrl, Alt y las teclas
de bloqueo; el escritorio se reserva Ctrl+Alt+Supr, sus atajos de captura y la
tecla Meta. Un programa que usa el ratón a través del controlador de ratón de
DOS sigue el puntero del escritorio dondequiera que esté sobre la imagen. Un
programa que lee el ratón por sí mismo lo toma al hacer clic en la imagen y lo
devuelve al pulsar brevemente la tecla Meta.

Cerrar la ventana de la biblioteca deja funcionando las máquinas en marcha, y
Biblioteca de máquinas, en el menú de la barra de iconos, la vuelve a abrir.
Cada máquina se ejecuta en un proceso de trabajo propio que no tiene ninguna
autoridad: nunca se confía en sus programas ni en sus discos.

## OPTIONS

`-h`, `-?`, `--help`
: Escribir esta ayuda en la salida estándar y salir.

## EXIT STATUS

Cero una vez cerrada su última ventana. Distinto de cero cuando se rechazó el canal
de ventana, el buzón de eventos o la sesión de escritorio; el motivo se indica
en la salida de error estándar.
