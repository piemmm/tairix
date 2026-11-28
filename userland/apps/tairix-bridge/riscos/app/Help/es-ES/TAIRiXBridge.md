## NAME

TAIRiXBridge — TAIRiX Bridge, programas de RISC OS en el escritorio de TAIRiX

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Ejecuta programas de RISC OS —programas BASIC, herramientas de línea de
órdenes y juegos de una sola tarea— en el escritorio de TAIRiX, cada uno con
su pantalla en una ventana propia. El RISC OS en el que se ejecutan es el de
TAIRiX Bridge, escrito a partir de documentación publicada; no contiene código
de RISC OS.

Los programas se ejecutan en sesiones. La sesión compartida es la máquina
RISC OS que muestra la ventana TAIRiX Bridge: su disco propio es la carpeta
RISC OS de sus Documentos, su secuencia de arranque se ejecuta al iniciarse y
un programa de su propio almacenamiento se ejecuta en ella. Un programa de
cualquier otro lugar —soportes extraíbles, una imagen de disco, un recurso
compartido de red— o de origen desconocido se ejecuta en una sesión aislada
propia, que nunca ve su carpeta RISC OS y termina con su último programa.
Ejecutar en la sesión compartida ejecuta un archivo en la sesión compartida,
venga de donde venga.

La ventana de una sesión enumera sus tareas, cada una con su memoria y si está
en pausa u ocupada, y lo que ocupan las áreas compartidas de la sesión. Ejecutar elige
un archivo para ejecutar; un archivo soltado en la ventana, o abierto con
TAIRiX Bridge, también se ejecuta. Elegir una tarea y luego Pausa o Salir la
detiene o la termina.

La pantalla de cada programa es una ventana propia, con su imagen escalada a
la ventana en las proporciones de su modo. El botón Pantalla abre el menú de
la ventana: Pantalla completa, escalado nítido o suave, Pausa y Reiniciar, que
termina el programa y lo vuelve a ejecutar en la misma ventana. La pantalla
toma todo el teclado, cada tecla con el número que le da RISC OS. Select, Menu
y Adjust son los botones principal, secundario y central. En cuanto un
programa mueve el ratón por sí mismo, un clic captura el ratón para él, y una
sola pulsación de la tecla Meta lo libera.

Ajustes elige el orden de los botones, la memoria que puede tomar cada sesión,
si cada programa se aísla y cómo se escalan las pantallas. El proceso de
trabajo de una sesión no tiene ninguna autoridad: los archivos que lee y
escribe se le transmiten, y un proceso que falla termina su sesión, con el
motivo mostrado en la ventana de la sesión.

## OPTIONS

`-h`, `-?`, `--help`
: Escribir esta ayuda en la salida estándar y salir.

## EXIT STATUS

Cero una vez cerrada la ventana de la última sesión. Distinto de cero cuando se
rechazó el canal de ventanas, el buzón de eventos o la sesión de escritorio;
el motivo se indica en el flujo de error estándar.
