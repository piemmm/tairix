## NAME

play — reproducir archivos de sonido

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Reproduce cada archivo por turno en una salida. Cada archivo se descodifica en
un proceso aislado que no tiene autoridad alguna, de modo que un archivo hostil
solo puede, como mucho, terminar su propia descodificación: se omite indicando
el motivo y se reproduce el resto de la lista. Se leen archivos AU, WAV y FLAC, este
último nativo o en Ogg.

Los archivos consecutivos con la misma frecuencia, el mismo formato de muestra
y la misma disposición de canales se reproducen sin pausa, en un solo flujo. Un
archivo de otra forma espera a que suene lo que está en cola y luego abre su
propio flujo.

Cuando la entrada estándar es una terminal, `play` dibuja una interfaz a
pantalla completa: el archivo que suena, la posición, el nivel y un medidor por
canal, y la lista. La reproducción no depende de ella. Enviado a segundo plano,
`play` sigue sonando y devuelve la terminal; traído a primer plano, vuelve a
dibujarse. Sin interfaz, informa del progreso en una línea en la salida de
errores de una terminal.

La interfaz admite estas teclas: Espacio o `p` pausa y reanuda; las flechas
izquierda y derecha saltan diez segundos; `n` o `>` pasa al archivo siguiente;
`b` o `<` vuelve al principio de este archivo, o al anterior durante sus tres
primeros segundos; `+`, `=` o la flecha arriba sube tres decibelios, y `-`, `_`
o la flecha abajo los baja; `q` o Ctrl-C detiene; Ctrl-Z suspende, tras pausar
el flujo.

El nivel de un flujo es una atenuación: un flujo no puede superar la escala
completa, así que `--gain` y las teclas de nivel no pasan de 0 dB. Para sonar
más alto, suba el volumen de la salida.

Un tiempo se escribe `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` o
`12.5`.

En la información estándar (fd 3), `play` escribe un registro `schema` por cada
archivo que reproduce, un registro `omission` por cada archivo omitido o
cortado, y un registro `summary` al terminar.

## OPTIONS

- `-q, --quiet` — sin interfaz ni línea de progreso.
- `-v, --verbose` — el formato y la duración de cada archivo en la salida de errores.
- `--ui, --no-ui` — dibujar la interfaz, o nunca; `--ui` sin terminal se
  rechaza.
- `-d, --device <sink>` — la salida: `audio:sink/default`,
  `audio:sink/<id>` para este arranque, o `audio:sink/<location>` esté
  donde esté el dispositivo, tal como las nombra `--list-devices`.
- `-g, --gain <dB>` — el nivel del flujo, 0 o menos, a la centésima.
- `-s, --start <time>` — empezar cada archivo en este tiempo.
- `-t, --duration <time>` — reproducir esta duración de cada archivo.
- `-l, --loop[=N]` — reproducir la lista N veces en total, o sin N para siempre.
- `--list-devices` — nombrar las salidas de esta sesión, cada una por su
  identificador y su ubicación, y salir.
- `-h, -?, --help` — mostrar la ayuda breve de esta orden.
- `--version` — mostrar la versión y salir.

## EXAMPLES

- `play song.wav` — reproducir un archivo, con la interfaz en una terminal.
- `play -q intro.au song.wav &` — reproducir una lista en segundo plano.
- `play -s 1:30 -t 20 song.wav` — reproducir veinte segundos desde el minuto y medio.
- `play -l3 -g -6 loop.wav` — reproducir un archivo tres veces, seis decibelios más bajo.
- `play --list-devices` — ver las salidas.

## EXIT STATUS

- `0` — se reprodujo cada archivo, o se detuvo la reproducción sin omitir ninguno.
- `1` — un archivo no pudo reproducirse, o la reproducción no pudo continuar.
- `2` — no se entendió la línea de órdenes.

## ENVIRONMENT

- `TERM` — la terminal para la que dibuja la interfaz.
- `LANG` — el idioma preferido de la ayuda breve (una etiqueta BCP-47 como
  `fr-FR`).

## SEE ALSO

- `man`
