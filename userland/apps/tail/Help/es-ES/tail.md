## NAME

tail — mostrar la última parte de los archivos

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Muestra las últimas 10 líneas de cada `file` en la salida estándar. Con
más de un `file`, cada parte va precedida de un encabezado
`==> file <==`. Sin `file`, o cuando `file` es `-`, se lee la entrada
estándar.

`-n` y `-c` cambian cuánto se muestra: un recuento simple (o escrito con
un `-` inicial) muestra las últimas `num` líneas o bytes; un recuento
escrito con un `+` inicial muestra todo **desde** la línea o el byte
`num` (contando desde 1) hasta el final. Un recuento puede llevar un
sufijo multiplicador: `b` (512), `kB` (1000), `K` (1024), `MB`, `M`,
`GB`, `G`, y así para `T`, `P`, `E`, `Z`, `Y`, `R`, `Q` (una letra sola
multiplica por potencias de 1024; con `B` por potencias de 1000; con
`iB` por potencias de 1024).

La forma histórica de primer argumento `tail -num` / `tail +num` (con una
letra final `b`/`c`/`l` opcional) se acepta, como en la herramienta GNU.

El modo de seguimiento mantiene cada archivo abierto e imprime los datos
nuevos a medida que el archivo crece. `-f` sigue el archivo abierto por
su descriptor, de modo que tras un cambio de nombre o un traslado sigue
el mismo archivo. `-F` sigue el *nombre*: cuando el archivo se rota (se
renombra y se pone un archivo nuevo en su lugar) reabre el archivo
nuevo, y `--retry` sigue intentándolo con un nombre que aún no existe.
Un seguimiento se bloquea hasta que el archivo cambia de verdad; nunca
hace girar la CPU en vacío. `--pid=PID` termina el seguimiento en cuanto
el proceso `PID` finaliza, comprobado cada `--sleep-interval` (segundos,
por defecto 1). `--max-unchanged-stats=N` controla cuántos ciclos sin
cambios pasan antes de que `-F` vuelva a comprobar si el nombre se ha
rotado (por defecto 5). Un truncamiento (el archivo encoge) se informa
y el archivo se vuelve a seguir desde su nuevo inicio.

Cuando no se muestra contenido inicial, se escribe un registro
informativo en el flujo de información estándar (fd 3); nunca cambia la
salida ni el estado de salida. Un archivo que no se puede leer se informa
en la salida de error y la ejecución continúa con el siguiente archivo.

En un terminal, cada nombre que muestra la salida se entrecomilla donde
un shell lo necesitaría, y cualquier carácter de control u otro carácter
invisible que haya en él se escribe como secuencia de escape, de modo
que un nombre no puede controlar ni reordenar la pantalla; GNU lo
muestra sin cambios. Una tubería o un archivo recibe el nombre byte a
byte.

## OPTIONS

- `-c, --bytes <num>` — mostrar los últimos `num` bytes de cada archivo;
  con un `+` inicial, todo desde el byte `num`.
- `-n, --lines <num>` — mostrar las últimas `num` líneas de cada
  archivo; con un `+` inicial, todo desde la línea `num`.
- `-q, --quiet, --silent` — nunca mostrar los encabezados
  `==> file <==`.
- `-v, --verbose` — mostrar siempre los encabezados `==> file <==`.
- `-z, --zero-terminated` — las líneas se delimitan con NUL en lugar de
  salto de línea.
- `-f, --follow[=descriptor]` — mantener el archivo abierto e imprimir
  los datos a medida que se añaden, siguiéndolo por descriptor.
- `-F` — seguir por nombre (`--follow=name --retry`): reabrir el archivo
  cuando se rota.
- `--follow=name` — seguir el nombre en lugar del descriptor.
- `--retry` — seguir intentando abrir un archivo que aún no es
  accesible.
- `--pid <PID>` — en un seguimiento, detenerse cuando muere el proceso
  `PID`.
- `--sleep-interval <N>` — segundos entre las comprobaciones de
  `--pid`/rotación (se admiten fracciones; por defecto 1).
- `--max-unchanged-stats <N>` — ciclos sin cambios antes de que `-F`
  vuelva a comprobar si el nombre se ha rotado (por defecto 5).
- `-h, -?` — mostrar la ayuda breve de esta orden.

## EXAMPLES

- `tail log.txt` — mostrar las últimas 10 líneas de `log.txt`.
- `tail -n 3 a b` — mostrar las últimas 3 líneas de `a` y de `b`, cada
  una bajo su encabezado.
- `tail -c 1K image` — mostrar los últimos 1024 bytes de `image`.
- `tail -n +5 notes` — mostrar `notes` desde su 5.ª línea hasta el final.
- `tail -f log.txt` — mostrar las últimas 10 líneas y luego las líneas
  nuevas a medida que se escriben.
- `tail -F /System/Logs/system` — seguir el registro del sistema a
  través de las rotaciones.
- `tail -f --pid 1234 out` — seguir `out` hasta que termine el proceso
  1234.

## EXIT STATUS

- `0` — se mostró cada archivo (o se escribió la ayuda breve).
- `1` — no se pudo leer un archivo, o no se pudo entregar la salida.
  Una línea de órdenes no entendida también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda breve (una
  etiqueta BCP-47 como `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
