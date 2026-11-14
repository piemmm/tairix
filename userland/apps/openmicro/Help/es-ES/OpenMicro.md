## NAME

OpenMicro — Open Micro Emulator, emulador de microordenadores de 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Ejecuta microordenadores de 8 bits en ventanas del escritorio —el BBC Micro
Model B y el Acorn Electron— ciclo a ciclo a su propia velocidad, cada uno
equipado con su propio sistema operativo y su BASIC, escritos en sala limpia, y
el Model B además con un sistema de archivos en disco, de modo que una máquina
arranca en el indicador de BASIC. La imagen es la de un televisor, cuatro
unidades de ancho por cada tres de alto, y el sonido suena por el audio del
escritorio.

La ventana de la biblioteca muestra cada máquina como una ficha con su última
pantalla. Nueva crea una máquina, con el nombre pedido: un BBC Micro Model B,
con un disco nuevo en blanco en la unidad 0 o sin disco, o un Acorn Electron,
con o sin un Plus 1 instalado. Iniciar, un doble clic o la tecla Intro inicia
la máquina elegida en una ventana propia, o trae su ventana al frente; se
conservan hasta veinte máquinas, y varias funcionan a la vez. Duplicar copia
una máquina con su estado guardado y su carpeta, y Eliminar elimina una máquina
y su estado guardado una vez confirmado, dejando su carpeta. Hacer clic con el
botón derecho en la ficha de una máquina ofrece Iniciar, Ajustes, Duplicar y
Eliminar para esa máquina y, mientras funciona, también el menú de la máquina.
La primera vez que se inicia, el emulador crea un BBC Micro Model B con un
disco en blanco.

Ajustes cambia el nombre de una máquina, su volumen y la carga rápida de cinta,
que hace correr la máquina tan deprisa como puede mientras gira el motor del
magnetófono. Lo que tiene instalado solo cambia mientras está apagada: la ROM
de cada zócalo —el OS y las dieciséis ranuras laterales del Model B, el OS y el
BASIC del Electron, y la ROM y los cartuchos de su Plus 1—, sea una ROM
incluida, RAM lateral en una ranura del Model B, nada en una ranura o un
cartucho, o una imagen de ROM propia, puesta en el almacén del emulador con
Instalar una imagen; el Plus 1 del Electron; y los puentes del teclado del
Model B, que fijan su modo de pantalla al arrancar, el arranque desde disco y
los tiempos de disco.

Cerrar la ventana de una máquina la suspende: al arrancarla de nuevo, sigue
donde se detuvo. Apagar, en su menú, saca sus discos y su cinta y la apaga, y
conserva solo su última pantalla.

Cada máquina tiene una carpeta propia, Open Micro y luego el nombre de la
máquina, en su carpeta Documents. Nuevo disco y Nueva cinta crean ahí un disco
o una cinta en blanco y lo insertan, y Desde la carpeta ofrece los discos y las
cintas que ya están ahí. Lo que sus unidades y su magnetófono tienen de su
carpeta se queda con la máquina: apagada, lo vuelve a cargar al arrancar;
reanudada, solo vuelve a escribir en ello allí donde ningún otro programa lo
haya cambiado desde que se suspendió.

Una imagen de disco (`.ssd`, `.dsd`) o de cinta (`.uef`, `.csw`) abierta con el
emulador va a la máquina en marcha cuya ventana fue la última activa, o a otra
máquina en marcha que la acepte, o si no a la primera máquina de la biblioteca
que la acepte, que se inicia para ello. Una elegida con Insertar disco o
Insertar cinta entra en esa unidad o en el magnetófono, y una soltada en la
ventana de una máquina entra en la unidad 0 o en el magnetófono, protegida
contra escritura. Lo que la máquina escribe en un disco vuelve al archivo del
disco mientras ese archivo pueda escribirse; un disco cuyo archivo no puede
escribirse queda protegido contra escritura. Expulsar saca un disco, y Guardar
disco como guarda una copia donde se quiera. Una cinta en la que se ha grabado
se guarda al sacarla. Guardar instantánea guarda la máquina entera en un
archivo, y Restaurar instantánea la recupera, con sus discos y su cinta.

El menú, abierto con un clic derecho sobre la imagen, ofrece cada unidad de un
Model B y la cinta; BREAK, Reiniciar, Pausa y la velocidad; pantalla completa,
escalado nítido o suave y líneas de barrido; la correspondencia del teclado;
Pegar, que teclea el texto del portapapeles; las instantáneas; Ajustes;
Biblioteca de máquinas; y Apagar. Por carácter, las teclas escriben lo que
muestran, y el CAPS LOCK de la máquina decide si una letra es mayúscula. Por
posición, cada tecla es la tecla de la máquina en su lugar. En un Model B, F1 a
F9 son f1 a f9, F10 es f0, Inicio es SHIFT LOCK e Insert es la tecla de barra
invertida. En un Electron, F1 a F10 son FUNC con 1 a 9 y 0, Alt es FUNC y Bloq
Mayús es CAPS LK. En ambos, F12 es BREAK, Fin es COPY y las flechas son las
teclas del cursor.

Unas luces al pie de la ventana de una máquina muestran CAPS LOCK y el motor
del magnetófono y, en un Model B, SHIFT LOCK y cada unidad. Cada máquina se
ejecuta en un proceso auxiliar propio sin autoridad alguna, así que no se
confía ni en su software ni en sus soportes; un proceso que falla se sustituye,
la máquina vuelve a arrancar como al encenderse y se muestra el motivo.

Cerrar la ventana de la biblioteca deja funcionando las máquinas en marcha, y
cerrar todas las ventanas deja el emulador en la barra de iconos: un clic en su
icono, o Biblioteca de máquinas en su menú, vuelve a abrir la biblioteca, y
Salir en ese menú suspende todas las máquinas en marcha y termina el emulador.

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero una vez elegido Salir en el menú de su icono de la barra de iconos.
Distinto de cero cuando se rechazó el canal de ventanas, el buzón de eventos o
la sesión de escritorio; el motivo se indica en la salida de error estándar.
