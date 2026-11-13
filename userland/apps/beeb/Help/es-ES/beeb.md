## NAME

beeb — emulador del BBC Micro modelo B

## SYNOPSIS

`beeb`

## DESCRIPTION

Ejecuta un BBC Micro modelo B en una ventana del escritorio, ciclo a ciclo a
su propia velocidad, con su propio MOS, BBC BASIC y DFS escritos en sala
limpia, de modo que arranca en el indicador de BASIC con un sistema de
archivos en disco. La imagen es la de un televisor, cuatro unidades de ancho
por cada tres de alto, y el sonido suena por el audio del escritorio.

Una imagen de disco (`.ssd`, `.dsd`) o de cinta (`.uef`, `.csw`) abierta con
el emulador, soltada en su ventana o elegida con Insertar disco o Insertar
cinta en su menú entra en la unidad o en el magnetófono. Lo que la máquina
escribe en un disco vuelve al archivo del disco mientras ese archivo pueda
escribirse; un disco cuyo archivo no puede escribirse queda protegido contra
escritura. Expulsar saca un disco, y Guardar disco como guarda una copia
donde se quiera. Una cinta en la que se ha grabado se guarda al sacarla.
Guardar instantánea guarda la máquina entera en un archivo, y Restaurar
instantánea la recupera, con sus discos y su cinta.

El menú, abierto con un clic derecho sobre la imagen o desde el icono del
emulador en la barra de iconos, ofrece además BREAK, Reiniciar, Pausa y la
velocidad; pantalla completa, escalado nítido o suave y líneas de barrido; la
correspondencia del teclado; y Pegar, que teclea el texto del portapapeles.
Por carácter, las teclas escriben lo que muestran, y el CAPS LOCK de la
máquina decide si una letra es mayúscula. Por posición, cada tecla es la
tecla del BBC en su lugar: F1 a F9 son f1 a f9, F10 es f0, F12 es BREAK, Fin
es COPY e Inicio es SHIFT LOCK.

Ajustes elige lo que contienen el zócalo del sistema y las dieciséis ranuras
laterales —las ROM incluidas, RAM lateral o imágenes de ROM propias,
instaladas en el almacén del emulador—, el modo de pantalla, el arranque y
los tiempos de disco de los puentes del teclado, la imagen, las teclas, el
volumen y la carga rápida de cinta, que hace correr la máquina tan deprisa
como puede mientras gira el motor del magnetófono. Los cambios en las ROM y
los puentes surten efecto en el siguiente reinicio.

Unas luces al pie de la ventana muestran CAPS LOCK, SHIFT LOCK, el motor del
magnetófono y cada unidad. La máquina se ejecuta en un proceso auxiliar sin
autoridad alguna, así que no se confía ni en su software ni en sus soportes;
un proceso que falla se sustituye, la máquina vuelve a arrancar como al
encenderse y se muestra el motivo.

## OPTIONS

`-h`, `-?`, `--help`
: Escribe esta ayuda en la salida estándar y termina.

## EXIT STATUS

Cero una vez cerrada su ventana o elegido Salir en el menú de su icono de la barra de iconos. Distinto de cero cuando se rechazó el canal de ventanas, el buzón de eventos o la sesión de escritorio; el motivo se indica en la salida de error estándar.
