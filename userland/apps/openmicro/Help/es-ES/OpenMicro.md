## NAME

OpenMicro — Open Micro Emulator, emulador de microordenadores de 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Ejecuta microordenadores de 8 bits en ventanas del escritorio —el BBC Micro
Model B, el Acorn Electron y el Commodore 64— ciclo a ciclo a su propia
velocidad, cada uno equipado con su propio sistema operativo y su BASIC,
escritos en sala limpia, el Model B además con un sistema de archivos en disco
y el Commodore 64 con una unidad de disco 1541 dotada de su propio DOS, de modo
que una máquina arranca en el indicador de BASIC. La imagen es la de un
televisor, cuatro unidades de ancho por cada tres de alto, y el sonido suena
por el audio del escritorio.

La ventana de la biblioteca muestra cada máquina como una ficha con su última
pantalla o, hasta que haya funcionado, con la pantalla de arranque de su
modelo. Nueva crea una máquina, con el nombre pedido: un BBC Micro Model B, con
RAM lateral en las ranuras 4 a 7 y un disco nuevo en blanco en la unidad 0 o
sin disco; un Acorn Electron, con o sin un Plus 1 instalado; o un Commodore 64,
PAL o NTSC, con un SID 6581 o 8580 y una unidad 1541, dos o ninguna. Iniciar,
un doble clic o la tecla Intro inicia la máquina elegida en una ventana propia,
o trae su ventana al frente; se conservan hasta veinte máquinas, y varias
funcionan a la vez. Duplicar copia una máquina con su estado guardado y su
carpeta, y Eliminar elimina una máquina y su estado guardado una vez
confirmado, dejando su carpeta. Hacer clic con el botón derecho en la ficha de
una máquina ofrece Iniciar, Ajustes, Duplicar, Eliminar y Mostrar carpeta para
esa máquina y, mientras funciona, también el menú de la máquina. La primera vez
que se inicia, el emulador crea un BBC Micro Model B con un disco en blanco.

Ajustes cambia el nombre de una máquina, su volumen, la carga rápida de cinta,
que hace correr la máquina tan deprisa como puede mientras gira el motor del
magnetófono, y en un Commodore 64 el puerto de control en el que está su
joystick de teclado numérico. Lo que tiene instalado solo cambia mientras está
apagada: la ROM de cada zócalo —el OS y las dieciséis ranuras laterales del
Model B, el OS y el BASIC del Electron, la ROM y los cartuchos de su Plus 1, y
el KERNAL, el BASIC, el juego de caracteres y el DOS de cada unidad del
Commodore 64—, sea una ROM incluida, RAM lateral en una ranura del Model B,
nada en una ranura o un cartucho, o una imagen de ROM propia, puesta en el
almacén del emulador con Instalar una imagen; el Plus 1 del Electron; la norma
de televisión, el SID y las unidades del Commodore 64; y los puentes del
teclado del Model B, que fijan su modo de pantalla al arrancar, el arranque
desde disco y los tiempos de disco.

Un Model B tiene además VIM, un editor al estilo de vi, instalado en la
ranura 13. En el indicador de la máquina, `*VIM` seguido del nombre de un
archivo edita ese archivo, y BREAK en VIM conserva el texto que contiene. VIM
edita texto, como los archivos `!BOOT` que crea `*BUILD`, y programas en BASIC,
que lista como texto y vuelve a guardar tokenizados, coloreando cada uno según
lo permitan los colores del modo de pantalla; `:dis` seguido del nombre de un
archivo recorre página a página el código máquina de un programa. En VIM,
`:help` enumera sus órdenes y los modos de pantalla con el espacio que deja
cada uno, `:mode` seguido de un número cambia el modo, y `:q` vuelve a BASIC.
Donde un banco de RAM lateral no contiene ninguna ROM, VIM guarda allí lo que
copia o borra, en el banco más alto de esos, de modo que no ocupa nada de la
memoria que tiene el texto.

Cerrar la ventana de una máquina la suspende: al arrancarla de nuevo, sigue
donde se detuvo. Apagar, en su menú, saca sus soportes y la apaga, y conserva
solo su última pantalla.

Cada máquina tiene una carpeta propia, Open Micro y luego el nombre de la
máquina, en su carpeta Documents. Nuevo disco, también en un Commodore 64, y
Nueva cinta crean ahí un disco o una cinta en blanco y lo insertan, Desde la
carpeta ofrece los discos y las cintas que ya están ahí, y Guardar instantánea
guarda ahí la máquina entera como un archivo `.snapshot`. Mostrar carpeta abre
la carpeta en Files, de modo que lo que la máquina guarda ahí puede copiarse a
otros soportes o a otro ordenador. Lo que sus unidades y su magnetófono tienen
de su carpeta se queda con la máquina: apagada, lo vuelve a cargar al
arrancar; reanudada, solo vuelve a escribir en ello allí donde ningún otro
programa lo haya cambiado desde que se suspendió.

Una imagen de disco (`.ssd`, `.dsd`) o de cinta (`.uef`, `.csw`), o para un
Commodore 64 una de disco (`.d64`, `.g64`), de cinta (`.tap`), de cartucho
(`.crt`) o de programa (`.prg`, `.t64`), abierta con el emulador va a la
máquina en marcha cuya ventana fue la última activa, o a otra máquina en marcha
que la acepte, o si no a la primera máquina de la biblioteca que la acepte, que
se inicia para ello, y la ventana de esa máquina pasa al frente. Abrir un
disco de BBC Micro en Files muestra los archivos que contiene en lugar de
iniciar una máquina. Una elegida con Insertar disco o Insertar cinta entra en
esa unidad o en el magnetófono, y una soltada en la ventana de una máquina
entra en su primera unidad o en el magnetófono, protegida contra escritura, y
la ventana toma el teclado. Lo que la máquina escribe en un disco vuelve al
archivo del disco mientras ese archivo pueda escribirse, y mientras el disco
está en la unidad ni un montaje ni otra máquina pueden escribir en ese
archivo; un disco cuyo archivo no puede escribirse, o que otro programa está
usando, queda protegido contra escritura, y la ventana indica el motivo. Si un
disco `.d64` ha llegado a necesitar un `.g64`, se ofrece guardarlo como tal.
Expulsar saca un disco, y Guardar disco como guarda una copia donde se quiera.
Una cinta en la que se ha grabado se guarda al sacarla. En un Commodore 64,
Reproducir, Grabar, Detener y Rebobinar, del menú Cinta, manejan su datasette;
Insertar cartucho conecta un cartucho y Quitar cartucho lo retira, y cada uno
reinicia la máquina; y Cargar programa pone un programa en la memoria, como
haría LOAD, para que RUN lo inicie. Guardar instantánea guarda la máquina
entera en un archivo —en su carpeta o, antes de que tenga una, donde se
elija—, y Restaurar instantánea recupera una máquina así guardada, con sus
soportes.

El menú, abierto con un clic derecho sobre la imagen, ofrece cada unidad de un
Model B o de un Commodore 64 y la cinta, y el cartucho y Cargar programa de un
Commodore 64; BREAK, o RESTORE en un Commodore 64; en un Model B y en un
Electron, SHIFT-BREAK, que pulsa BREAK manteniendo pulsado SHIFT para arrancar
desde el disco o la cinta; Reiniciar, Pausa y la velocidad; pantalla completa,
escalado nítido o suave y líneas de barrido; la correspondencia del teclado;
Pegar, que teclea el texto del portapapeles, cada letra en mayúscula o
minúscula según la muestre la máquina; las instantáneas; Mostrar carpeta;
Ajustes; Biblioteca de máquinas; y Apagar. Por carácter, las teclas escriben
lo que muestran, y el CAPS LOCK de la máquina, o Mayús en un Commodore 64,
decide si una letra es mayúscula. Por posición, cada tecla es la tecla de la
máquina en su lugar. En un Model B, F1 a F9 son f1 a f9, F10 es f0, Inicio es
SHIFT LOCK e Insert es la tecla de barra invertida. En un Electron, F1 a F10
son FUNC con 1 a 9 y 0, Alt es FUNC y Bloq Mayús es CAPS LK. En ambos, F12 es
BREAK, Fin es COPY y las flechas son las teclas del cursor. En un Commodore
64, F1, F3, F5 y F7 son sus teclas de función y F2, F4, F6 y F8 las mismas con
SHIFT, Escape es RUN/STOP, Alt es la tecla Commodore, Ctrl es CTRL, Inicio es
CLR/HOME, F12 o RePág es RESTORE, y las flechas son las teclas del cursor; con
su joystick de teclado numérico activado, las flechas del teclado numérico
mueven el joystick y 0 y 5 disparan.

Unas luces al pie de la ventana de una máquina muestran CAPS LOCK y el motor
del magnetófono y, en un Model B, SHIFT LOCK y cada unidad; en un Commodore 64
muestran el motor del datasette y cada unidad. En un Model B y en un Electron,
el botón SHIFT-BREAK, a la derecha del pie, hace lo que hace la fila del menú.
Cada máquina se ejecuta en un proceso auxiliar propio sin autoridad alguna,
así que no se confía ni en su software ni en sus soportes; un proceso que
falla se sustituye, la máquina vuelve a arrancar como al encenderse y se
muestra el motivo.

Cerrar la ventana de la biblioteca deja funcionando las máquinas en marcha, y
cerrar todas las ventanas deja el emulador en la barra de iconos: un clic en su
icono, o Biblioteca de máquinas en su menú, siempre trae la biblioteca al
frente, y Salir en ese menú suspende todas las máquinas en marcha y termina el
emulador.

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero una vez elegido Salir en el menú de su icono de la barra de iconos.
Distinto de cero cuando se rechazó el canal de ventanas, el buzón de eventos o
la sesión de escritorio; el motivo se indica en la salida de error estándar.
