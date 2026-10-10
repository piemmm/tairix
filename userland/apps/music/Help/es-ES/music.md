## NAME

music — el reproductor de música del escritorio

## SYNOPSIS

`music`

## DESCRIPTION

Reproduce una lista de pistas en una ventana. Abra archivos o una carpeta
entera con el selector de archivos del escritorio, o abra una pista desde el
gestor de archivos: se une a la lista del reproductor que ya está abierto. Las
pistas de la misma frecuencia, el mismo formato de muestra y la misma
disposición de canales se suceden sin pausa.

El reproductor no tiene ninguna capacidad sobre el sistema de archivos. La
sesión del escritorio explora por él y le delega, una sola vez y solo para
lectura, exactamente los archivos que el usuario elige — para una carpeta, los
archivos que contiene y que este reproductor abre. Ningún archivo se decodifica
dentro del reproductor: el sonido y la carátula los decodifica cada uno un
proceso aparte sin alcance alguno, de modo que un archivo mal formado u hostil
no puede llegar a nada de lo que llega el reproductor.

La parte superior de la ventana muestra lo que suena: la carátula, el título,
el artista y el álbum, el formato, la posición y un medidor de nivel por canal.
Debajo están el transporte, los controles de aleatorio y repetición y el
volumen, y debajo la lista. Arrastre el control de posición para saltar y el
de volumen para fijar el nivel; ambos actúan donde los suelta. Haga doble clic
en una pista para reproducirla. Una pulsación secundaria en la lista abre el
menú del reproductor, que también elige la salida y si las pistas se igualan
por la sonoridad que indican sus propias etiquetas.

Un archivo que el reproductor no puede leer se deja fuera, con su motivo en la
línea de estado.

* `Space` — reproducir o pausar
* `Enter` — reproducir la pista seleccionada
* `Left` / `Right` — diez segundos atrás o adelante
* `Ctrl` + `Left` / `Right` — la pista anterior o la siguiente
* `Up` / `Down` — seleccionar la pista de arriba o de abajo
* `Alt` + `Up` / `Down` — mover la pista seleccionada
* `Delete` — quitar de la lista la pista seleccionada
* `+` / `-` — tres decibelios más alto o más bajo
* `S` — aleatorio o en orden
* `R` — no repetir nada, repetir la lista o la pista
* `Ctrl` + `O` — abrir archivos
* `Ctrl` + `Shift` + `O` — abrir una carpeta

## OPTIONS

`-h`, `-?`, `--help`
: Escribir esta ayuda en la salida estándar y salir.

## EXIT STATUS

Cero tras cerrar la ventana o elegir Salir. Distinto de cero cuando se
rechazó el canal de ventana, la región de imagen compartida o la sesión del
escritorio; el motivo se indica en la salida de error estándar.
