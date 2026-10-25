## NAME

rm — eliminar archivos y directorios

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Elimina cada operando de archivo, en orden. Un operando que no es un
directorio se desenlaza —se elimina el propio enlace simbólico,
nunca lo que nombra—; un operando de directorio solo se elimina con
`-r` (que elimina su contenido en profundidad primero y luego el
directorio mismo) o, cuando está vacío, con `-d`. En ambos casos se
rechaza un operando cuyo último componente es `.` o `..`, como exige
POSIX.

Con `-f`, un operando que no existe se omite en silencio y nunca se
hace una pregunta. `-i` pregunta por la salida de error estándar
antes de cada eliminación, nombrando lo que eliminaría
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), y
antes de descender a un directorio que contenga algo; `-I` pregunta
una sola vez antes de eliminar más de tres operandos o antes de una
eliminación recursiva. Una pregunta rechazada conserva el objeto (u
omite toda la ejecución, con `-I`) sin error; una respuesta ilegible
nunca se considera consentimiento. El último de `-f`, `-i` e `-I`
gana.

Sin `-f`, cuando la entrada estándar es una terminal, se pregunta por un
archivo en el que el llamante no puede escribir antes de eliminarlo
(`remove write-protected regular file 'x'?`), y solo se elimina con una
respuesta que empiece por `y` o `Y`; nunca se pregunta por un enlace
simbólico. Con `-i`, la pregunta también nombra la protección.

Una ruta que no puede inspeccionarse, leerse o eliminarse se
notifica y las demás se eliminan de todos modos; un directorio que
sigue conteniendo algo se conserva sin decir nada. Una eliminación
recursiva de `/`, se escriba como se escriba, se rechaza bajo
`--preserve-root`, el comportamiento predeterminado. `--` termina el
análisis de opciones: cada argumento posterior es una ruta.

## OPTIONS

- `-r, -R, --recursive` — eliminar directorios y su contenido.
- `-f, --force` — ignorar los operandos que no existen; no preguntar
  nunca.
- `-d, --dir` — eliminar directorios vacíos.
- `-i, --interactive` — preguntar antes de cada eliminación; solo
  consiente una respuesta que empiece por `y`/`Y`.
- `-I` — preguntar una sola vez antes de eliminar más de tres
  operandos, o antes de una eliminación recursiva.
- `-v, --verbose` — informar de cada eliminación como
  `removed 'file'`.
- `--preserve-root` — negarse a eliminar `/` recursivamente (el
  comportamiento predeterminado).
- `--no-preserve-root` — permitir eliminar `/`. Solo se acepta sin
  abreviar.
- `-h, -?, --help` — mostrar la ayuda corta de esta orden.

## EXAMPLES

- `rm notes.txt` — eliminar un archivo.
- `rm -r Scratch` — eliminar el árbol `Scratch` y todo su contenido.
- `rm -I a b c d` — preguntar una vez y, con `y`, eliminar los cuatro
  archivos.

## EXIT STATUS

- `0` — todas las eliminaciones tuvieron éxito (una pregunta
  rechazada y una omisión por `-f` no son fallos).
- `1` — una ruta no pudo eliminarse, o falló una pregunta o la
  salida; cada motivo se imprime en la salida de error estándar. Una
  línea de órdenes no entendida también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
