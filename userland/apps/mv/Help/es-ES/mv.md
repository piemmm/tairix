## NAME

mv — mover (renombrar) archivos y directorios

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Mueve cada operando de origen a un destino. Con un único origen y un
destino que no nombra un directorio, el origen se renombra a esa ruta
exacta. Cuando el destino nombra un directorio existente — y siempre
que haya más de un origen — cada origen se mueve *dentro* de ese
directorio bajo su propio nombre base.

Un movimiento dentro de un volumen es un cambio de nombre atómico que
conserva la identidad del nodo. Un movimiento entre volúmenes cumple la
misma promesa: la copia se construye junto al destino, legible solo por
su propietario hasta que está completa, se escribe en el disco y se
renombra a su lugar en un solo paso, y solo entonces se elimina lo
copiado — así el destino conserva lo que contenía hasta que el
movimiento está completo, y ni un fallo ni una caída pierden nada. Un
archivo añadido al origen, o modificado, mientras se copiaba se conserva
donde estaba y se notifica, junto con el directorio que lo contiene; en
un volumen que no lleva un contador de cambios, o que guarda tiempos
poco precisos (los dos segundos de FAT), una reescritura de la misma
longitud hecha dentro de ese margen puede pasar inadvertida. En ambos
casos, un directorio solo reemplaza a un directorio vacío, nada más
reemplaza a un directorio y ningún directorio se mueve dentro de su
propio subárbol.

Un destino existente se sobrescribe por defecto, se conserva con `-n`
(que avisa con `not replacing` y hace fallar la ejecución, como en GNU)
y se pregunta por la salida de error estándar con `-i` (una pregunta
rechazada omite ese movimiento y hace fallar la ejecución; una respuesta
ilegible nunca se considera consentimiento). Un operando que no puede
moverse se notifica, y el siguiente se mueve de todos modos. `--`
termina el análisis de opciones: cada argumento posterior es una ruta.

La copia conserva los huecos de cada archivo, sus atributos extendidos —
un tipo de archivo de RISC OS, un tipo y un creador de Mac —, sus
permisos y su propietario allí donde el destino los guarde, y mueve un
enlace simbólico como un enlace con el mismo destino, nunca aquello a lo
que apunta. Un archivo devuelto a otro propietario llega sin sus bits
set-ID, que el sistema borra al cambiar el propietario; uno que quien lo
mueve no puede devolver pasa a ser suyo, en su propio grupo allí donde
quien lo mueve puede conservarlo, y sin los bits set-ID de un
propietario o un grupo que no conservó. Las marcas de tiempo no se
conservan: ninguna llamada de este sistema las establece. Los atributos
que el destino no puede guardar se notifican en una única advertencia al
terminar la ejecución, y en un registro `omission` en el flujo de
información estándar, antes de que los orígenes desaparezcan para
siempre.

## OPTIONS

- `-f, --force` — no preguntar nunca antes de sobrescribir. Prevalece el
  último de `-f`, `-i` y `-n`.
- `-i, --interactive` — preguntar antes de sobrescribir un destino
  existente; solo consiente una respuesta que empiece por `y`/`Y`.
- `-n, --no-clobber` — no sobrescribir nunca un destino existente; cada
  uno que se conserva se notifica como `not replacing 'dest'`.
- `-v, --verbose` — informar de cada movimiento como
  `renamed 'source' -> 'dest'`, y de un movimiento entre volúmenes
  entrada a entrada, como hace GNU: `created directory 'dest'` por cada
  directorio y `copied 'source' -> 'dest'` por todo lo demás a medida
  que se copia, y después `removed 'source'` o
  `removed directory 'source'` por cada uno a medida que desaparece el
  origen.
- `-t dir, --target-directory=dir` — mover cada origen dentro de
  `dir`, que debe ser un directorio existente. El valor sigue adjunto
  (`-tdir`, `--target-directory=dir`) o como argumento siguiente.
- `-T, --no-target-directory` — tratar el destino como un archivo
  normal; se permite exactamente un origen. No puede combinarse con
  `-t`.
- `-h, -?, --help` — mostrar la ayuda corta de esta orden.

## EXAMPLES

- `mv draft.txt final.txt` — renombrar un archivo.
- `mv -v a.txt b.txt Archive` — mover ambos archivos a `Archive`,
  informando de cada movimiento.
- `mv -n new.cfg current.cfg` — instalar un archivo solo si el destino
  no existe ya (avisando con `not replacing` si existe).

## EXIT STATUS

- `0` — todos los movimientos tuvieron éxito.
- `1` — un operando no pudo moverse, `-n` conservó un destino, se
  rechazó una pregunta `-i`, algo de un origen cambió mientras se movía
  y se conservó, o falló una pregunta o la salida; el motivo de cada
  fallo se imprime en la salida de error estándar. Una línea de órdenes
  no entendida también termina con `1`.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
