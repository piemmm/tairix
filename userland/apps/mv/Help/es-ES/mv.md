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
misma promesa: la copia se construye junto al destino, se escribe en el
disco y se renombra a su lugar en un solo paso, y solo entonces se
elimina el origen — así el destino conserva lo que contenía hasta que el
movimiento está completo, y ni un fallo ni una caída pierden nada. En
ambos casos, un directorio solo reemplaza a un directorio vacío, nada
más reemplaza a un directorio y ningún directorio se mueve dentro de su
propio subárbol.

Un destino existente se sobrescribe por defecto, se omite con `-n` y
se pregunta por la salida de error estándar con `-i` (una pregunta
rechazada omite ese movimiento sin error; una respuesta ilegible
nunca se considera consentimiento). El primer fallo detiene la
ejecución antes de cualquier operando posterior. `--` termina el
análisis de opciones: cada argumento posterior es una ruta.

La copia conserva los huecos de cada archivo, sus atributos extendidos —
un tipo de archivo de RISC OS, un tipo y un creador de Mac —, sus
permisos y su propietario allí donde el destino los guarde, y mueve un
enlace simbólico como un enlace con el mismo destino, nunca aquello a lo
que apunta. Un archivo que quien lo mueve no puede devolver a su
propietario pasa a ser suyo, sin sus bits set-ID. Las marcas de tiempo
no se conservan: ninguna llamada de este sistema las establece. Los
atributos que el destino no puede guardar se notifican en una única
advertencia al terminar la ejecución, y en un registro `omission` en el
flujo de información estándar, antes de que los orígenes desaparezcan
para siempre.

## OPTIONS

- `-f, --force` — no preguntar nunca antes de sobrescribir. Prevalece el
  último de `-f`, `-i` y `-n`.
- `-i, --interactive` — preguntar antes de sobrescribir un destino
  existente; solo consiente una respuesta que empiece por `y`/`Y`.
- `-n, --no-clobber` — no sobrescribir nunca un destino existente.
- `-v, --verbose` — informar de cada movimiento como
  `renamed 'source' -> 'dest'`, y de un movimiento entre volúmenes como
  `copied 'source' -> 'dest'` y después `removed 'source'`.
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
- `mv -n new.cfg current.cfg` — instalar un archivo solo si el
  destino no existe ya.

## EXIT STATUS

- `0` — todos los movimientos tuvieron éxito (una omisión por `-n` y
  una pregunta `-i` rechazada no son fallos).
- `1` — un fallo del sistema de archivos, de la pregunta o de la
  salida; el motivo se imprime en la salida de error estándar.
- `2` — la línea de órdenes no se entendió.

## ENVIRONMENT

- `LANG` — la configuración regional preferida para la ayuda corta
  (una etiqueta BCP-47 como `es-ES`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
